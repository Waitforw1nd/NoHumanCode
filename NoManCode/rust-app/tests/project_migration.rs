use sha2::{Digest, Sha256};
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
use peachsh::{network_authorization::*, project_config::ProjectError, secrets, store::Store};
use rusqlite::{Connection, params, types::Value};
use serde_json::json;
#[path = "support/project_fixture.rs"]
#[allow(dead_code)]
mod project_fixture;

fn rows(db: &Connection, table: &str) -> Vec<Vec<Value>> {
    let mut s = db
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    s.query_map([], |r| {
        (0..r.as_ref().column_count()).map(|i| r.get(i)).collect()
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

#[test]
fn schema11_preserves_v1_network_ciphertext_digest_grant_and_historical_confirm_replay() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    drop(Store::open(&path).unwrap());
    let db = Connection::open(&path).unwrap();
    project_fixture::drop_project_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE IF EXISTS plugin_management_requests; DROP TABLE IF EXISTS plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1');").unwrap();
    db.pragma_update(None, "user_version", 10).unwrap();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: None,
    };
    let mut snapshot = json!({"subject":subject,"config_revision":1,"credential_revision":1,"origin":"http://127.0.0.1:1","base_url":"http://127.0.0.1:1","identity":"mock","credential_ref":"stored/route/route"});
    let mut digest_value = snapshot.clone();
    digest_value["version"] = json!(1);
    let binding = digest(&serde_json::to_vec(&digest_value).unwrap());
    snapshot["binding_digest"] = json!(binding);
    let grant = peachsh::domain::id();
    let call = peachsh::domain::id();
    let request = ConfirmRequest {
        config_revision: 1,
        credential_revision: 1,
        binding_digest: binding.clone(),
        purposes: vec![NetworkPurpose::Completion],
    };
    let receipt = CallReceipt {
        call_id: call.clone(),
        authorization_id: grant.clone(),
        subject: subject.clone(),
        task_id: None,
        purpose: NetworkPurpose::Completion,
        method: "POST".into(),
        origin: "http://127.0.0.1:1".into(),
        endpoint_digest: "a".repeat(64),
        state: CallState::Unknown,
        claimed_at: 1,
        finished_at: Some(2),
        success: None,
        http_status: None,
        result: Some(CallResult::Interrupted),
    };
    db.execute(
        "INSERT INTO network_subjects VALUES ('route:route',?1,?2)",
        params![
            serde_json::to_string(&snapshot).unwrap(),
            secrets::seal(b"historical-fixture-key").unwrap()
        ],
    )
    .unwrap();
    db.execute("INSERT INTO network_authorizations VALUES (?1,'route:route',1,1,?2,'[\"completion\"]','active',1)",params![grant,binding]).unwrap();
    db.execute(
        "INSERT INTO network_confirmations VALUES ('route:route','old-confirm',?1,?2)",
        params![digest(&serde_json::to_vec(&request).unwrap()), grant],
    )
    .unwrap();
    db.execute(
        "INSERT INTO network_calls VALUES (?1,?2,NULL,'unknown',?3)",
        params![call, grant, serde_json::to_string(&receipt).unwrap()],
    )
    .unwrap();
    let tables = [
        "network_subjects",
        "network_authorizations",
        "network_confirmations",
        "network_calls",
        "network_events",
        "command_network_ack",
        "projects",
        "sessions",
        "turns",
        "tasks",
        "checkpoints",
    ];
    let before: Vec<_> = tables.iter().map(|t| rows(&db, t)).collect();
    drop(db);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 15);
    assert!(
        store
            .network_confirm(subject.clone(), request.clone(), "old-confirm")
            .unwrap()
            .replayed
    );
    assert_eq!(store.network_call(&call).unwrap(), receipt);
    assert_eq!(
        store
            .network_preview(subject.clone())
            .unwrap_err()
            .kind
            .project_code(),
        Some(ProjectError::SelectionRequired.code())
    );
    assert_eq!(
        store
            .network_confirm(subject, request, "new-confirm")
            .unwrap_err()
            .kind
            .project_code(),
        Some(ProjectError::SelectionRequired.code())
    );
    drop(store);
    drop(Store::open(&path).unwrap());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        tables.iter().map(|t| rows(&db, t)).collect::<Vec<_>>(),
        before
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM task_project_bindings", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn schema11_rejects_forged_nullable_type_literal_fk_and_primary_key() {
    for mutation in ["nullable", "literal", "foreign", "primary"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        drop(Store::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        let table = if mutation == "literal" {
            "task_project_bindings"
        } else {
            "project_configs"
        };
        let sql: String = db
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        let forged = match mutation {
            "nullable" => sql.replace("value TEXT NOT NULL", "value TEXTNOT NULL"),
            "literal" => sql.replace("'native_v1'", "'native_ v1'"),
            "foreign" => sql.replace("REFERENCES projects(id)", ""),
            _ => sql.replace("PRIMARY KEY", "UNIQUE"),
        };
        assert_ne!(sql, forged);
        db.execute_batch(&format!("DROP TABLE {table};{forged};"))
            .unwrap();
        if mutation == "nullable" {
            db.execute(
                "INSERT INTO projects VALUES ('nullable','fixture','not-opened',1,1)",
                [],
            )
            .unwrap();
            db.execute(
                "INSERT INTO project_configs VALUES ('nullable',1,NULL,'digest')",
                [],
            )
            .unwrap();
            let value: Option<String> = db
                .query_row("SELECT value FROM project_configs", [], |r| r.get(0))
                .unwrap();
            assert!(value.is_none());
        }
        drop(db);
        let error = Store::open(&path)
            .err()
            .expect("forged project schema accepted");
        assert!(error.to_string().contains("project"), "{mutation}: {error}");
    }
}

#[test]
fn failed_schema11_transaction_keeps_schema10_version_and_marker() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    drop(Store::open(&path).unwrap());
    let db = Connection::open(&path).unwrap();
    project_fixture::drop_project_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE IF EXISTS plugin_management_requests; DROP TABLE IF EXISTS plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1');").unwrap();
    db.pragma_update(None, "user_version", 10).unwrap();
    db.execute_batch("CREATE TRIGGER block_project_marker BEFORE INSERT ON schema_migrations WHEN NEW.id='project-config-root-binding-v1' BEGIN SELECT RAISE(ABORT,'blocked marker'); END;").unwrap();
    drop(db);
    assert!(Store::open(&path).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        10
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='project_configs'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER block_project_marker")
        .unwrap();
    drop(db);
    assert_eq!(Store::open(&path).unwrap().schema_version().unwrap(), 15);
}
