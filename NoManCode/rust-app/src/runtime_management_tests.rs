use crate::{
    engine::Engine,
    plugin_management::SetPluginEnabled,
    runtime_management::{RuntimeId, RuntimeManagementError},
    store::Store,
};
use serde_json::Value;
use std::sync::{Arc, Barrier};

fn write(enabled: bool, expected_revision: u64) -> SetPluginEnabled {
    SetPluginEnabled {
        enabled,
        expected_revision,
    }
}
fn status(engine: &Engine, id: RuntimeId) -> Value {
    serde_json::to_value(engine.runtime_component_status(id).unwrap()).unwrap()
}

#[test]
fn persistent_disable_survives_restart_and_old_local_enable_cannot_bypass_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime.db");
    let engine = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
    for (id, key) in [
        (RuntimeId::ChatAgent, "agent-off"),
        (RuntimeId::CompletionProvider, "provider-off"),
    ] {
        let first = serde_json::to_value(
            engine
                .set_runtime_enabled(id, write(false, 0), key)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(first["receipt"]["revision"], 1);
        assert_eq!(first["replayed"], false);
        let replay = serde_json::to_value(
            engine
                .set_runtime_enabled(id, write(false, 0), key)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(replay["receipt"], first["receipt"]);
        assert_eq!(replay["replayed"], true);
    }
    assert!(engine.enable_builtin_agent().is_err());
    assert!(engine.enable_builtin_provider().is_err());
    drop(engine);
    let restarted = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
    for id in [RuntimeId::ChatAgent, RuntimeId::CompletionProvider] {
        let state = status(&restarted, id);
        assert_eq!(state["desired"]["enabled"], false);
        assert_eq!(state["desired"]["revision"], 1);
        assert_eq!(state["runtime"]["active"], false);
        assert_eq!(state["runtime"]["admission_open"], false);
        assert_eq!(state["runtime"]["generation"], 1);
        assert_eq!(state["converged"], true);
    }
    assert!(restarted.enable_builtin_agent().is_err());
    assert!(restarted.enable_builtin_provider().is_err());
}

#[test]
fn global_key_conflict_and_independent_store_cas() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime.db");
    let first = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
    let second = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
    first
        .set_runtime_enabled(RuntimeId::ChatAgent, write(false, 0), "shared")
        .unwrap();
    assert!(matches!(
        second.set_runtime_enabled(RuntimeId::CompletionProvider, write(false, 0), "shared"),
        Err(RuntimeManagementError::IdempotencyConflict)
    ));
    let gate = Arc::new(Barrier::new(3));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .enumerate()
        .map(|(i, engine)| {
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                engine.set_runtime_enabled(
                    RuntimeId::CompletionProvider,
                    write(false, 0),
                    if i == 0 { "race-a" } else { "race-b" },
                )
            })
        })
        .collect();
    gate.wait();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(RuntimeManagementError::RevisionConflict)))
            .count(),
        1
    );
}
#[test]
fn local_suppression_requires_a_new_persistent_intent_to_restore_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime.db");
    let engine = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
    for (id, key) in [
        (RuntimeId::ChatAgent, "agent-on"),
        (RuntimeId::CompletionProvider, "provider-on"),
    ] {
        engine.set_runtime_enabled(id, write(true, 0), key).unwrap();
    }
    engine.disable_builtin_agent().unwrap();
    engine.disable_builtin_provider().unwrap();
    for (id, key, next_key) in [
        (RuntimeId::ChatAgent, "agent-on", "agent-on-again"),
        (
            RuntimeId::CompletionProvider,
            "provider-on",
            "provider-on-again",
        ),
    ] {
        let suppressed = status(&engine, id);
        assert_eq!(suppressed["desired"]["enabled"], true);
        assert_eq!(suppressed["desired"]["revision"], 1);
        assert_eq!(suppressed["runtime"]["admission_open"], false);
        let replay =
            serde_json::to_value(engine.set_runtime_enabled(id, write(true, 0), key).unwrap())
                .unwrap();
        assert_eq!(replay["replayed"], true);
        assert_eq!(status(&engine, id)["runtime"]["admission_open"], false);
        let applied = serde_json::to_value(
            engine
                .set_runtime_enabled(id, write(true, 1), next_key)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(applied["receipt"]["revision"], 2);
        assert_eq!(status(&engine, id)["runtime"]["admission_open"], true);
    }
}

// The shared test probe permits only one active barrier at a time.
pub(super) fn barrier_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
#[test]
fn exhausted_revision_rejects_new_intent_without_consuming_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime.db");
    drop(Store::open(&path).unwrap());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE plugin_desired SET revision=?1 WHERE plugin_id='builtin.chat-agent'",
        [i64::MAX],
    )
    .unwrap();
    let engine = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
    assert!(matches!(
        engine.set_runtime_enabled(
            RuntimeId::ChatAgent,
            write(true, i64::MAX as u64),
            "exhausted"
        ),
        Err(RuntimeManagementError::RevisionConflict)
    ));
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM plugin_management_requests WHERE key='exhausted'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        status(&engine, RuntimeId::ChatAgent)["desired"]["revision"],
        i64::MAX as u64
    );
}
#[test]
fn generation_exhaustion_does_not_leave_persistent_disable_open() {
    for id in [RuntimeId::ChatAgent, RuntimeId::CompletionProvider] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.db");
        let engine = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
        match id {
            RuntimeId::ChatAgent => engine
                .agent_runtime_for_test()
                .set_generation_for_test(u64::MAX)
                .unwrap(),
            RuntimeId::CompletionProvider => engine
                .provider_runtime_for_test()
                .set_generation_for_test(u64::MAX)
                .unwrap(),
        }
        let result = engine
            .set_runtime_enabled(id, write(false, 0), "exhaust-generation")
            .unwrap();
        assert_eq!(result.receipt.revision, 1);
        let state = status(&engine, id);
        assert_eq!(state["desired"]["enabled"], false);
        assert_eq!(state["runtime"]["admission_open"], false);
        assert_eq!(state["converged"], false);
    }
}
#[test]
fn intent_receipt_insert_failure_rolls_back_without_changing_local_runtime() {
    let _probe_guard = barrier_test_guard();
    for id in [RuntimeId::ChatAgent, RuntimeId::CompletionProvider] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.db");
        let engine = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TRIGGER n18_reject_receipt BEFORE INSERT ON plugin_management_requests WHEN NEW.key='write-failure' BEGIN SELECT RAISE(ABORT,'n18_receipt_injected'); END;").unwrap();
        let component = match id {
            RuntimeId::ChatAgent => "builtin.chat-agent",
            RuntimeId::CompletionProvider => "builtin.provider-xpeach",
        };
        let before = status(&engine, id);
        let probe=db.execute("INSERT INTO plugin_management_requests(key,plugin_id,enabled,expected_revision,result_revision,committed_at) VALUES('write-failure',?1,0,0,1,1)",[component]).unwrap_err();
        assert!(probe.to_string().contains("n18_receipt_injected"));
        let db_key: String = db
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name='main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let gate =
            crate::engine::runtime_barrier::arm(db_key, "runtime_intent_before_receipt_insert");
        let worker_engine = engine.clone();
        let worker = std::thread::spawn(move || {
            worker_engine.set_runtime_enabled(id, write(false, 0), "write-failure")
        });
        let arrived = gate.wait_arrived(std::time::Duration::from_secs(5));
        gate.release();
        assert!(
            arrived,
            "management transaction never reached target receipt insert"
        );
        assert!(matches!(
            worker.join().unwrap(),
            Err(RuntimeManagementError::Internal)
        ));
        assert_eq!(status(&engine, id), before);
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM plugin_management_requests WHERE key='write-failure'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT revision FROM plugin_desired WHERE plugin_id=?1",
                [component],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        db.execute_batch("DROP TRIGGER n18_reject_receipt").unwrap();
        let accepted = engine
            .set_runtime_enabled(id, write(false, 0), "write-failure")
            .unwrap();
        assert!(!accepted.replayed);
        assert_eq!(accepted.receipt.revision, 1);
        assert_eq!(status(&engine, id)["desired"]["enabled"], false);
    }
}
#[test]
fn sqlite_intent_commit_failure_rolls_back_before_runtime_changes() {
    for id in [RuntimeId::ChatAgent, RuntimeId::CompletionProvider] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.db");
        let engine = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        let db_key: String = db
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name='main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let component = match id {
            RuntimeId::ChatAgent => "builtin.chat-agent",
            RuntimeId::CompletionProvider => "builtin.provider-xpeach",
        };
        let before = status(&engine, id);
        crate::engine::runtime_barrier::fail_next_intent_commit(db_key.clone());
        assert!(matches!(
            engine.set_runtime_enabled(id, write(false, 0), "commit-failure"),
            Err(RuntimeManagementError::Internal)
        ));
        assert!(
            crate::engine::runtime_barrier::take_intent_commit_failure_observed(&db_key),
            "expected an actual deferred FK failure from SQLite COMMIT"
        );
        assert_eq!(status(&engine, id), before);
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM plugin_management_requests WHERE key='commit-failure'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT revision FROM plugin_desired WHERE plugin_id=?1",
                [component],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(Store::open(&path).unwrap());
        let accepted = engine
            .set_runtime_enabled(id, write(false, 0), "commit-failure")
            .unwrap();
        assert!(!accepted.replayed);
        assert_eq!(accepted.receipt.revision, 1);
        drop(engine);
        let restarted = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
        let after = status(&restarted, id);
        assert_eq!(after["desired"]["enabled"], false);
        assert_eq!(after["desired"]["revision"], 1);
        assert_eq!(after["runtime"]["admission_open"], false);
    }
}
