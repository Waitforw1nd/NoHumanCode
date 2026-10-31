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
