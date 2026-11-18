//! Construct a genuine schema 13-16 plugin layout from a current test database.
use rusqlite::Connection;

pub fn restore_two_plugin_schema(db: &Connection) {
    let discarded: i64 = db.query_row("SELECT count(*) FROM plugin_management_requests WHERE plugin_id NOT IN ('builtin.files','builtin.shell')", [], |r| r.get(0)).unwrap();
    assert_eq!(discarded, 0, "fixture must not discard runtime receipts");
    let retained: i64 = db.query_row("SELECT count(*) FROM plugin_desired WHERE plugin_id IN ('builtin.files','builtin.shell')", [], |r| r.get(0)).unwrap();
    assert_eq!(retained, 2, "fixture requires both legacy desired rows");
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
         BEGIN IMMEDIATE;
         CREATE TEMP TABLE old_desired AS SELECT rowid AS saved_rowid,* FROM plugin_desired WHERE plugin_id IN ('builtin.files','builtin.shell');
         CREATE TEMP TABLE old_requests AS SELECT rowid AS saved_rowid,* FROM plugin_management_requests WHERE plugin_id IN ('builtin.files','builtin.shell');
         DROP TABLE plugin_management_requests;
         DROP TABLE plugin_desired;
         CREATE TABLE plugin_desired (plugin_id TEXT NOT NULL PRIMARY KEY CHECK(typeof(plugin_id)='text' AND plugin_id IN ('builtin.files','builtin.shell')), enabled INTEGER NOT NULL CHECK(typeof(enabled)='integer' AND enabled IN (0,1)), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0), updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer'));
         CREATE TABLE plugin_management_requests (key TEXT NOT NULL PRIMARY KEY CHECK(typeof(key)='text' AND length(key) BETWEEN 1 AND 200), plugin_id TEXT NOT NULL REFERENCES plugin_desired(plugin_id) CHECK(typeof(plugin_id)='text' AND plugin_id IN ('builtin.files','builtin.shell')), enabled INTEGER NOT NULL CHECK(typeof(enabled)='integer' AND enabled IN (0,1)), expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>=0 AND expected_revision<9223372036854775807), result_revision INTEGER NOT NULL CHECK(typeof(result_revision)='integer' AND result_revision=expected_revision+1), committed_at INTEGER NOT NULL CHECK(typeof(committed_at)='integer'));
         INSERT INTO plugin_desired(rowid,plugin_id,enabled,revision,updated_at) SELECT saved_rowid,plugin_id,enabled,revision,updated_at FROM old_desired;
         INSERT INTO plugin_management_requests(rowid,key,plugin_id,enabled,expected_revision,result_revision,committed_at) SELECT saved_rowid,key,plugin_id,enabled,expected_revision,result_revision,committed_at FROM old_requests;
         DROP TABLE temp.old_requests;
         DROP TABLE temp.old_desired;
         DELETE FROM schema_migrations WHERE id='builtin-runtime-management-v1';
         COMMIT;",
    ).unwrap();
}
