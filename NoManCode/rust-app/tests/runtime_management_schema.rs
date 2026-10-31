#![cfg(windows)]
#[path = "support/schema_fixture.rs"]
mod schema_fixture;

use peachsh::store::Store;
use rusqlite::{Connection, types::Value};
use std::{
    path::Path,
    sync::{Arc, Barrier},
};

fn historical16(path: &Path) {
    drop(Store::open(path).unwrap());
    let db = Connection::open(path).unwrap();
    schema_fixture::restore_two_plugin_schema(&db);
    db.execute_batch("PRAGMA user_version=16;
        UPDATE plugin_desired SET rowid=31,enabled=0,revision=7,updated_at=-12 WHERE plugin_id='builtin.files';
        UPDATE plugin_desired SET rowid=42,enabled=1,revision=3,updated_at=14 WHERE plugin_id='builtin.shell';
        INSERT INTO plugin_management_requests(rowid,key,plugin_id,enabled,expected_revision,result_revision,committed_at) VALUES(71,'old-files','builtin.files',0,6,7,-12),(72,'old-shell','builtin.shell',1,2,3,14);
        INSERT INTO secrets(rowid,id,value) VALUES(97,'opaque-next18',X'0001FFFE');").unwrap();
}

fn rows(db: &Connection, sql: &str) -> Vec<Vec<Value>> {
    db.prepare(sql)
        .unwrap()
        .query_map([], |row| {
            (0..row.as_ref().column_count())
                .map(|i| row.get(i))
                .collect()
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn migration_keeps_old_rows_and_receipts_and_opaque_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime-schema.db");
    historical16(&path);
    let db = Connection::open(&path).unwrap();
    let desired = rows(&db, "SELECT rowid,* FROM plugin_desired ORDER BY rowid");
    let receipts = rows(
        &db,
        "SELECT rowid,* FROM plugin_management_requests ORDER BY rowid",
    );
    let secrets = rows(&db, "SELECT rowid,* FROM secrets ORDER BY rowid");
    for _ in 0..2 {
        assert_eq!(Store::open(&path).unwrap().schema_version().unwrap(), 17);
        assert_eq!(
            rows(
                &db,
                "SELECT rowid,* FROM plugin_desired WHERE plugin_id IN ('builtin.files','builtin.shell') ORDER BY rowid"
            ),
            desired
        );
        assert_eq!(
            rows(
                &db,
                "SELECT rowid,* FROM plugin_management_requests ORDER BY rowid"
            ),
            receipts
        );
        assert_eq!(
            rows(&db, "SELECT rowid,* FROM secrets ORDER BY rowid"),
            secrets
        );
        let added = rows(
            &db,
            "SELECT plugin_id,enabled,revision,updated_at FROM plugin_desired WHERE plugin_id IN ('builtin.chat-agent','builtin.provider-xpeach') ORDER BY plugin_id",
        );
        assert_eq!(
            added,
            vec![
                vec![
                    Value::Text("builtin.chat-agent".into()),
                    Value::Integer(1),
                    Value::Integer(0),
                    Value::Integer(0)
                ],
                vec![
                    Value::Text("builtin.provider-xpeach".into()),
                    Value::Integer(1),
                    Value::Integer(0),
                    Value::Integer(0)
                ]
            ]
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
}

#[test]
fn independent_connections_can_migrate_same_historical_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime-schema.db");
    historical16(&path);
    let gate = Arc::new(Barrier::new(3));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let gate = gate.clone();
            let path = path.clone();
            std::thread::spawn(move || {
                gate.wait();
                Store::open(&path).unwrap().schema_version().unwrap()
            })
        })
        .collect();
    gate.wait();
    for worker in workers {
        assert_eq!(worker.join().unwrap(), 17);
    }
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM schema_migrations WHERE id='builtin-runtime-management-v1'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM plugin_desired", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn marker_and_schema_shape_mismatches_are_rejected() {
    for sql in [
        "DELETE FROM schema_migrations WHERE id='builtin-runtime-management-v1'",
        "PRAGMA user_version=16",
        "DELETE FROM plugin_desired WHERE plugin_id='builtin.chat-agent'",
        "DROP TABLE plugin_management_requests; CREATE TABLE plugin_management_requests(key TEXT PRIMARY KEY,plugin_id TEXT,enabled INTEGER,expected_revision INTEGER,result_revision INTEGER,committed_at INTEGER)",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-schema.db");
        drop(Store::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        db.execute_batch(sql).unwrap();
        assert!(Store::open(&path).is_err(), "must reject {sql}");
    }
}

#[test]
fn migration_failure_at_marker_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime-schema.db");
    historical16(&path);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_runtime_marker BEFORE INSERT ON schema_migrations WHEN NEW.id='builtin-runtime-management-v1' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let desired = rows(&db, "SELECT rowid,* FROM plugin_desired ORDER BY rowid");
    let receipts = rows(
        &db,
        "SELECT rowid,* FROM plugin_management_requests ORDER BY rowid",
    );
    let error = Store::open(&path)
        .err()
        .expect("marker must reject migration");
    assert!(
        format!("{error:#}").contains("injected"),
        "wrong failure: {error:#}"
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        16
    );
    assert_eq!(
        rows(&db, "SELECT rowid,* FROM plugin_desired ORDER BY rowid"),
        desired
    );
    assert_eq!(
        rows(
            &db,
            "SELECT rowid,* FROM plugin_management_requests ORDER BY rowid"
        ),
        receipts
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM schema_migrations WHERE id='builtin-runtime-management-v1'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER fail_runtime_marker")
        .unwrap();
    assert_eq!(Store::open(&path).unwrap().schema_version().unwrap(), 17);
    assert_eq!(
        rows(
            &db,
            "SELECT rowid,* FROM plugin_desired WHERE plugin_id IN ('builtin.files','builtin.shell') ORDER BY rowid"
        ),
        desired
    );
    assert_eq!(
        rows(
            &db,
            "SELECT rowid,* FROM plugin_management_requests ORDER BY rowid"
        ),
        receipts
    );
}
#[test]
fn historical_versions_12_through_16_upgrade_to_17() {
    for version in 12..=16 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-schema.db");
        historical16(&path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        if version < 16 {
            db.execute_batch("DROP TABLE host_config_requests; DROP TABLE host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1';").unwrap();
        }
        if version < 15 {
            db.execute_batch("DROP TABLE budget_calls; DROP TABLE budget_requests; DROP TABLE project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1';").unwrap();
        }
        if version < 14 {
            db.execute_batch("DROP TABLE project_worktree_origins; DROP TABLE worktree_project_reservations; DROP TABLE worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1';").unwrap();
        }
        if version == 12 {
            let desired: String = db
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name='plugin_desired'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let requests: String = db.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='plugin_management_requests'", [], |r| r.get(0)).unwrap();
            let old = "plugin_id IN ('builtin.files','builtin.shell')";
            assert!(desired.contains(old) && requests.contains(old));
            db.execute_batch("BEGIN IMMEDIATE; CREATE TEMP TABLE saved_desired AS SELECT rowid AS saved_rowid,* FROM plugin_desired WHERE plugin_id='builtin.files'; CREATE TEMP TABLE saved_requests AS SELECT rowid AS saved_rowid,* FROM plugin_management_requests WHERE plugin_id='builtin.files'; DROP TABLE plugin_management_requests; DROP TABLE plugin_desired;").unwrap();
            db.execute_batch(&desired.replace(old, "plugin_id='builtin.files'"))
                .unwrap();
            db.execute_batch(&requests.replace(old, "plugin_id='builtin.files'"))
                .unwrap();
            db.execute_batch("INSERT INTO plugin_desired(rowid,plugin_id,enabled,revision,updated_at) SELECT saved_rowid,plugin_id,enabled,revision,updated_at FROM saved_desired; INSERT INTO plugin_management_requests(rowid,key,plugin_id,enabled,expected_revision,result_revision,committed_at) SELECT saved_rowid,key,plugin_id,enabled,expected_revision,result_revision,committed_at FROM saved_requests; DROP TABLE temp.saved_desired; DROP TABLE temp.saved_requests; DELETE FROM schema_migrations WHERE id='builtin-shell-management-v1'; COMMIT;").unwrap();
        }
        db.pragma_update(None, "user_version", version).unwrap();
        drop(db);
        for _ in 0..2 {
            assert_eq!(
                Store::open(&path).unwrap().schema_version().unwrap(),
                17,
                "historical {version}"
            );
            let db = Connection::open(&path).unwrap();
            assert_eq!(db.query_row("SELECT rowid FROM plugin_desired WHERE plugin_id='builtin.files' AND enabled=0 AND revision=7 AND updated_at=-12",[],|r|r.get::<_,i64>(0)).unwrap(),31);
            assert_eq!(db.query_row("SELECT rowid FROM plugin_management_requests WHERE key='old-files' AND committed_at=-12",[],|r|r.get::<_,i64>(0)).unwrap(),71);
        }
    }
}
#[test]
fn historical_corruption_and_unsupported_objects_fail_closed() {
    let control = tempfile::tempdir().unwrap();
    let control_path = control.path().join("control.db");
    historical16(&control_path);
    assert_eq!(
        Store::open(&control_path)
            .unwrap()
            .schema_version()
            .unwrap(),
        17
    );
    for (sql, reason) in [
        (
            "INSERT INTO schema_migrations(id,applied_at) VALUES('builtin-runtime-management-v1',1)",
            "schema 17 marker/version mismatch",
        ),
        (
            "DELETE FROM schema_migrations WHERE id='builtin-shell-management-v1'",
            "schema 13 marker/version mismatch",
        ),
        (
            "DELETE FROM plugin_desired WHERE plugin_id='builtin.shell'",
            "plugin desired state invalid",
        ),
        (
            "CREATE INDEX unexpected_plugin_index ON plugin_desired(enabled)",
            "unsupported plugin schema objects",
        ),
        (
            "CREATE TRIGGER unexpected_plugin_trigger AFTER UPDATE ON plugin_desired BEGIN SELECT 1; END",
            "unsupported plugin schema objects",
        ),
        (
            "CREATE TABLE external_reference(plugin_id TEXT REFERENCES plugin_desired(plugin_id))",
            "unsupported plugin foreign key dependencies",
        ),
        (
            "CREATE TABLE nhc_runtime17_desired(collision INTEGER)",
            "staging name collision",
        ),
        (
            "PRAGMA ignore_check_constraints=ON; UPDATE plugin_desired SET enabled=3 WHERE plugin_id='builtin.files'; PRAGMA ignore_check_constraints=OFF",
            "plugin desired state invalid",
        ),
        (
            "PRAGMA ignore_check_constraints=ON; UPDATE plugin_management_requests SET result_revision=42 WHERE key='old-files'; PRAGMA ignore_check_constraints=OFF",
            "plugin receipt invalid",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-schema.db");
        historical16(&path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch(sql).unwrap();
        let error = Store::open(&path).err().expect("damaged fixture must fail");
        assert!(
            format!("{error:#}").contains(reason),
            "wrong rejection for {sql}: {error:#}"
        );
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            16
        );
    }
}

#[test]
fn maximum_historical_revision_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime-schema.db");
    historical16(&path);
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE plugin_desired SET revision=?1 WHERE plugin_id='builtin.files'",
        [i64::MAX],
    )
    .unwrap();
    assert_eq!(Store::open(&path).unwrap().schema_version().unwrap(), 17);
    assert_eq!(
        db.query_row(
            "SELECT revision FROM plugin_desired WHERE plugin_id='builtin.files'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        i64::MAX
    );
}
