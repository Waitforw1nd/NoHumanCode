#![cfg(windows)]
#[path = "support/schema_fixture.rs"]
mod schema_fixture;
use peachsh::{
    host_configuration::*,
    store::{Store, fresh_settings},
};
use rusqlite::{Connection, types::Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
};
fn rows(db: &Connection) -> BTreeMap<String, Vec<Vec<Value>>> {
    let names:Vec<String>=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT IN ('host_config_state','host_config_requests','execution_owner') ORDER BY name").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    names.into_iter().map(|name|{let query=if name=="schema_migrations" {"SELECT rowid,* FROM schema_migrations WHERE id NOT IN ('host-route-configuration-v1','builtin-runtime-management-v1','execution-owner-v18') ORDER BY rowid".into()}else if name=="plugin_desired" {"SELECT rowid,* FROM plugin_desired WHERE plugin_id IN ('builtin.files','builtin.shell') ORDER BY rowid".into()}else{format!("SELECT rowid,* FROM {name} ORDER BY rowid")};let values=db.prepare(&query).unwrap().query_map([],|r|(0..r.as_ref().column_count()).map(|i|r.get(i)).collect()).unwrap().collect::<rusqlite::Result<_>>().unwrap();(name,values)}).collect()
}
fn historical15() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("host.db")).unwrap();
    fresh_settings(&store, dir.path()).unwrap();
    drop(store);
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    schema_fixture::restore_two_plugin_schema(&db);
    db.execute_batch("DROP TABLE host_config_requests; DROP TABLE host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; PRAGMA user_version=15; INSERT INTO config(rowid,id,value) VALUES(88,'sentinel','exact old json bytes'); INSERT INTO secrets(rowid,id,value) VALUES(97,'opaque-legacy',X'0001FFFE'); UPDATE plugin_desired SET rowid=31,revision=7,updated_at=12 WHERE plugin_id='builtin.files';").unwrap();
    (dir, db)
}
#[test]
fn migration_keeps_all_legacy_rows_rowids_and_opaque_blobs_on_repeated_open() {
    let (dir, db) = historical15();
    let before = rows(&db);
    for _ in 0..2 {
        let store = Store::open(&dir.path().join("host.db")).unwrap();
        assert_eq!(store.schema_version().unwrap(), 18);
        assert_eq!(store.host_config().unwrap().revision, 0);
        assert_eq!(rows(&db), before);
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM host_config_requests", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn concurrent_migration_opens_converge_on_one_marker_and_singleton() {
    let (dir, db) = historical15();
    let before = rows(&db);
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = dir.path().join("host.db");
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                Store::open(&path)
            })
        })
        .collect();
    barrier.wait();
    for h in handles {
        assert_eq!(h.join().unwrap().unwrap().schema_version().unwrap(), 18);
    }
    assert_eq!(rows(&db), before);
    assert_eq!(
        db.query_row("SELECT count(*) FROM host_config_state", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM schema_migrations WHERE id='host-route-configuration-v1'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
}
#[test]
fn migration_failure_rolls_back_ddl_marker_and_version() {
    let (dir, db) = historical15();
    db.execute_batch("CREATE TRIGGER fail_host_marker BEFORE INSERT ON schema_migrations WHEN NEW.id='host-route-configuration-v1' BEGIN SELECT RAISE(ABORT,'test'); END").unwrap();
    let before = rows(&db);
    assert!(Store::open(&dir.path().join("host.db")).is_err());
    assert_eq!(rows(&db), before);
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        15
    );
    assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('host_config_state','host_config_requests','execution_owner')",[],|r|r.get::<_,u64>(0)).unwrap(),0);
}
#[test]
fn wrong_marker_version_singleton_and_constraint_shapes_are_rejected() {
    for sql in [
        "DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'",
        "PRAGMA user_version=15",
        "DELETE FROM host_config_state",
        "DROP TABLE host_config_state; CREATE TABLE host_config_state(id INTEGER PRIMARY KEY,revision INTEGER NOT NULL); INSERT INTO host_config_state VALUES(1,0)",
        "DROP TABLE host_config_requests; CREATE TABLE host_config_requests(key TEXT PRIMARY KEY, operation_id TEXT, route_id TEXT, expected_revision INTEGER,result_revision INTEGER,request_fingerprint_blob BLOB,receipt TEXT,committed_at INTEGER)",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("host.db")).unwrap();
        drop(store);
        let db = Connection::open(dir.path().join("host.db")).unwrap();
        db.execute_batch(sql).unwrap();
        assert!(
            Store::open(&dir.path().join("host.db")).is_err(),
            "must reject {sql}"
        );
    }
}
#[test]
fn actual_database_constraints_reject_invalid_values() {
    let dir = tempfile::tempdir().unwrap();
    Store::open(&dir.path().join("host.db")).unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    for sql in [
        "INSERT INTO host_config_state VALUES(2,0)",
        "UPDATE host_config_state SET revision=-1",
        "UPDATE host_config_state SET revision=1.5",
        "UPDATE host_config_state SET revision='oops'",
        "INSERT INTO host_config_requests VALUES('key','op','route',0,2,X'01','{}',1)",
        "INSERT INTO host_config_requests VALUES('key','op','route',0,1,'not-blob','{}',1)",
    ] {
        assert!(db.execute_batch(sql).is_err());
    }
}
#[test]
fn receipt_column_identity_and_revisions_are_validated_after_open() {
    for field in [
        "operation_id",
        "route_id",
        "revision",
        "committed_at",
        "changed",
        "extra",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("host.db")).unwrap();
        fresh_settings(&store, dir.path()).unwrap();
        let request = ConfigureHostRoute {
            expected_revision: 0,
            route: RouteInput {
                name: "Mock".into(),
                base_url: "https://example.invalid/v1".into(),
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 1,
            },
            credential: CredentialUpdate::Replace {
                value: "schema-fixture-value".into(),
            },
        };
        store.configure_host_route("route", request, "one").unwrap();
        let db = Connection::open(dir.path().join("host.db")).unwrap();
        let raw: String = db
            .query_row("SELECT receipt FROM host_config_requests", [], |r| r.get(0))
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value[field] = match field {
            "revision" | "committed_at" => serde_json::json!(999),
            "changed" => serde_json::json!(false),
            _ => serde_json::json!("damaged"),
        };
        db.execute(
            "UPDATE host_config_requests SET receipt=?1",
            [value.to_string()],
        )
        .unwrap();
        assert_eq!(
            store.host_config_operation("one").unwrap_err(),
            HostConfigError::Corrupt
        );
        drop(store);
        assert!(Store::open(&dir.path().join("host.db")).is_err());
    }
}
