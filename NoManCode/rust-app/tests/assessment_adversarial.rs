//! Explicit security and reliability probes. The repaired probes run in routine CI.
use peachsh::{domain::*, engine::Engine, store::Store, wasm};
use std::sync::Arc;

fn setup(dir: &std::path::Path) -> Arc<Engine> {
    let store = Arc::new(Store::open(&dir.join("test.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.to_string_lossy().into(),
            max_concurrency: 1,
            newapi: None,
            routes: vec![Route {
                id: "test".into(),
                name: "test".into(),
                base_url: "http://127.0.0.1:1/v1".into(),
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 1,
                key_env: None,
            }],
        })
        .unwrap();
    store.put_secret("test", "fake-only").unwrap();
    Engine::new(store, 1).unwrap()
}
fn request(title: &str) -> RunRequest {
    RunRequest {
        kind: SessionKind::Team,
        title: title.into(),
        tasks: vec![TaskSpec {
            name: "worker".into(),
            role: "tester".into(),
            route_id: "test".into(),
            prompt: title.into(),
            depends_on: vec![],
            write_scopes: vec![],
            tools: false,
            allow_commands: false,
            max_rounds: 1,
        }],
    }
}

#[tokio::test]
async fn idempotency_rejects_different_payload() {
    let temp = tempfile::tempdir().unwrap();
    let engine = setup(temp.path());
    engine
        .start_idempotent(request("original"), "repeat")
        .await
        .unwrap();
    assert!(
        engine
            .start_idempotent(request("different"), "repeat")
            .await
            .is_err(),
        "different request with same key must be rejected, not silently mapped to old run"
    );
}

#[tokio::test]
async fn idempotency_storage_failure_leaves_no_orphan_run() {
    let temp = tempfile::tempdir().unwrap();
    let engine = setup(temp.path());
    let db = rusqlite::Connection::open(temp.path().join("test.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_binding BEFORE INSERT ON idempotency BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
    assert!(
        engine
            .start_idempotent(request("storage failure"), "repeat")
            .await
            .is_err()
    );
    assert!(
        engine.store.runs().unwrap().is_empty(),
        "failed atomic creation left a queued run behind"
    );
}

#[tokio::test]
async fn inference_route_cannot_read_account_secret() {
    let temp = tempfile::tempdir().unwrap();
    let engine = setup(temp.path());
    engine
        .store
        .put_secret("newapi-account", "fake-account-assessment")
        .unwrap();
    let mut settings = engine.store.settings().unwrap().unwrap();
    settings.routes[0].id = "newapi-account".into();
    let route = settings.routes[0].clone();
    let configured = engine.configure(settings).await;
    assert!(
        configured.is_err() || engine.key(&route).is_err(),
        "inference route resolved the account credential"
    );
}

#[test]
fn wasm_rejects_unbounded_large_table() {
    // Only about 8 MiB of references: bounded probe, not an exhaustion attack.
    let module = wat::parse_str(
        r#"(module
        (memory (export "memory") 1 1)
        (table 1048576 funcref)
        (func (export "alloc") (param i32) (result i32) (i32.const 0))
        (func (export "run_json") (param i32 i32) (result i64) (i64.extend_i32_u (local.get 1)))
    )"#,
    )
    .unwrap();
    assert!(
        wasm::validate_module(&module, 1).is_err(),
        "unbounded table exceeding the 4 MiB host-memory goal was accepted"
    );
}
