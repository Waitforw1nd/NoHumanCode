#![cfg(windows)]
//! Schema 17 upgrade and original-child crash regression. Run serially in the
//! manager's real-process window; the child never owns external work.
#[allow(dead_code)]
#[path = "support/owned_child.rs"]
mod owned_child;

use peachsh::{engine::Engine, store::Store};
use rusqlite::{Connection, types::Value};
use std::{
    collections::BTreeMap,
    path::Path,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

type Rows = BTreeMap<String, Vec<Vec<Value>>>;

fn legacy_rows(db: &Connection) -> Rows {
    let names: Vec<String> = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name != 'execution_owner' ORDER BY name")
        .unwrap().query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    names.into_iter().map(|name| {
        let sql = if name == "schema_migrations" {
            "SELECT rowid,* FROM schema_migrations WHERE id != 'execution-owner-v18' ORDER BY rowid".to_owned()
        } else {
            format!("SELECT rowid,* FROM \"{}\" ORDER BY rowid", name.replace('"', "\"\""))
        };
        let mut statement = db.prepare(&sql).unwrap();
        let rows = statement.query_map([], |row| (0..row.as_ref().column_count()).map(|i| row.get(i)).collect())
            .unwrap().collect::<rusqlite::Result<Vec<Vec<Value>>>>().unwrap();
        (name, rows)
    }).collect()
}

// Restore the exact predecessor by removing only the v18 addition from a
// production-created database. No test-only adoption path is used on reopen.
fn schema17(path: &Path) -> Connection {
    drop(Store::open(path).unwrap());
    let db = Connection::open(path).unwrap();
    db.execute_batch("DROP TABLE execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; PRAGMA user_version=17;
        INSERT INTO config(rowid,id,value) VALUES(801,'u18-sentinel','legacy bytes');
        INSERT INTO secrets(rowid,id,value) VALUES(802,'u18-opaque',X'0001FFFE');
        INSERT INTO runs(rowid,id,title,created_at,kind) VALUES(803,'u18-run','legacy',11,'team');
        INSERT INTO tasks(rowid,id,run_id,value) VALUES(804,'u18-task','u18-run','{\"legacy\":true}');
        INSERT INTO events(seq,task_id,kind,data,at) VALUES(805,'u18-task','resume','{\"pending\":true}',12);").unwrap();
    db
}

#[test]
fn schema17_upgrade_preserves_every_old_row_and_is_repeatable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let db = schema17(&path);
    let before = legacy_rows(&db);
    for _ in 0..2 {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 18);
        assert_eq!(legacy_rows(&db), before);
        drop(store);
    }
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM schema_migrations WHERE id='execution-owner-v18'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn failed_schema18_marker_rolls_back_schema17_and_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let db = schema17(&path);
    db.execute_batch("CREATE TRIGGER reject_u18 BEFORE INSERT ON schema_migrations WHEN NEW.id='execution-owner-v18' BEGIN SELECT RAISE(ABORT,'injected migration failure'); END;").unwrap();
    let before = legacy_rows(&db);
    assert!(Store::open(&path).is_err());
    assert_eq!(legacy_rows(&db), before);
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='execution_owner'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER reject_u18").unwrap();
    assert_eq!(Store::open(&path).unwrap().schema_version().unwrap(), 18);
    assert_eq!(legacy_rows(&db), before);
}

#[test]
fn exhausted_epoch_or_corrupt_domain_rejects_controller_without_writes() {
    for mutation in [
        "UPDATE execution_owner SET epoch=9223372036854775807 WHERE id=1",
        "UPDATE execution_owner SET domain_identity='different-domain' WHERE id=1",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad-owner.db");
        let store = Arc::new(Store::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        db.execute(mutation, []).unwrap();
        let before_rows = legacy_rows(&db);
        let before_owner: (String, i64, String) = db
            .query_row(
                "SELECT domain_identity,epoch,phase FROM execution_owner WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(Engine::new(store, 1).is_err());
        let after_owner: (String, i64, String) = db
            .query_row(
                "SELECT domain_identity,epoch,phase FROM execution_owner WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(after_owner, before_owner);
        assert_eq!(legacy_rows(&db), before_rows);
    }
}

// Launched by the parent as this exact test. The barrier is emitted only after
// production controller acquisition; no PID is used for termination or waiting.
#[test]
fn crash_child_controller_barrier() {
    let Some(root) = std::env::var_os("NHC_U18_CRASH_CHILD") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let store = Arc::new(Store::open(&root.join("crash.db")).unwrap());
    let _engine = Engine::new(store, 1).unwrap();
    std::fs::write(root.join("controller-ready"), b"ready").unwrap();
    loop {
        std::thread::park_timeout(Duration::from_secs(60));
    }
}

#[tokio::test]
async fn killed_original_controller_is_waited_before_recovery() {
    let root = tempfile::Builder::new()
        .prefix("u18-crash-")
        .tempdir()
        .unwrap()
        .keep();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "crash_child_controller_barrier", "--nocapture"])
        .env("NHC_U18_CRASH_CHILD", &root);
    let mut child = owned_child::OwnedChild::spawn(&mut command, &root.join("evidence"));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !root.join("controller-ready").exists() {
        assert!(
            Instant::now() < deadline,
            "controller barrier was not reached"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.kill_wait();
    assert!(
        !status.success(),
        "child exited normally rather than being killed"
    );
    drop(child);
    let store = Arc::new(Store::open(&root.join("crash.db")).unwrap());
    let engine = Engine::new_recovered(store, 1).unwrap().0;
    assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
}
