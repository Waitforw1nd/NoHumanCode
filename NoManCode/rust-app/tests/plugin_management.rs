use peachsh::{
    engine::Engine, plugin_management::*, store::Store, tool_runtime::FilesRuntimeState,
};
use std::sync::Arc;
fn engine(path: &std::path::Path) -> Arc<Engine> {
    Engine::new(Arc::new(Store::open(path).unwrap()), 2).unwrap()
}
fn request(enabled: bool, expected_revision: u64) -> SetPluginEnabled {
    SetPluginEnabled {
        enabled,
        expected_revision,
    }
}
#[test]
fn management_status_replay_and_explicit_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let engine = engine(&path);
    let first = engine.plugin_status(BUILTIN_FILES_ID).unwrap();
    assert!(first.desired.enabled && first.converged && first.runtime.admission_open);
    assert_eq!(first.runtime.providers.len(), 4);
    assert_eq!(first.runtime.effects_count, 4);
    assert_eq!(first.version, "1.2.0");
    assert_eq!(
        first.affected_tools,
        ["read_file", "write_file", "list_files", "search_files"]
    );
    assert_eq!(first.compat_tools, ["run_wasm"]);
    assert_eq!(
        engine.plugins().unwrap(),
        vec![
            first.clone(),
            engine.plugin_status(BUILTIN_SHELL_ID).unwrap()
        ]
    );
    let disabled = engine
        .set_plugin_enabled(BUILTIN_FILES_ID, request(false, 0), "disable")
        .unwrap();
    assert!(disabled.current.converged);
    assert_eq!(disabled.current.runtime.state, FilesRuntimeState::Stopped);
    assert_eq!(
        disabled.current.blocked_reason,
        Some(PluginBlockedReason::Disabled)
    );
    assert_eq!(disabled.current.runtime.effects_count, 0);
    let enabled = engine
        .set_plugin_enabled(BUILTIN_FILES_ID, request(true, 1), "enable")
        .unwrap();
    assert!(enabled.current.converged);
    engine.disable_builtin_files().unwrap();
    let stopped = engine.plugin_status(BUILTIN_FILES_ID).unwrap();
    assert!(stopped.desired.enabled);
    assert!(!stopped.converged);
    assert_eq!(
        stopped.blocked_reason,
        Some(PluginBlockedReason::RuntimeUnavailable)
    );
    let replay = engine
        .set_plugin_enabled(BUILTIN_FILES_ID, request(false, 0), "disable")
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.receipt, disabled.receipt);
    assert_eq!(replay.current, stopped); // GET and replay do not reconcile
    let fresh = engine
        .set_plugin_enabled(BUILTIN_FILES_ID, request(true, 2), "reconcile")
        .unwrap();
    assert_eq!(fresh.receipt.revision, 3);
    assert!(fresh.current.converged && fresh.current.runtime.admission_open);
}
#[test]
fn disabled_restart_and_runtime_failure_before_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let first = engine(&path);
    let before = first.plugin_status(BUILTIN_FILES_ID).unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TRIGGER fail_management BEFORE INSERT ON plugin_management_requests BEGIN SELECT RAISE(ABORT,'sensitive-path-must-not-be-public'); END;").unwrap();
    let error = first
        .set_plugin_enabled(BUILTIN_FILES_ID, request(false, 0), "disable")
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<PluginManagementError>(),
        Some(&PluginManagementError::Internal)
    );
    assert!(!error.to_string().contains("sensitive-path"));
    assert_eq!(first.plugin_status(BUILTIN_FILES_ID).unwrap(), before);
    raw.execute_batch("DROP TRIGGER fail_management").unwrap();
    let disabled = first
        .set_plugin_enabled(BUILTIN_FILES_ID, request(false, 0), "disable")
        .unwrap();
    first.unload_builtin_files().unwrap();
    drop(first);
    let reopened = engine(&path);
    let status = reopened.plugin_status(BUILTIN_FILES_ID).unwrap();
    assert!(!status.desired.enabled);
    assert!(status.converged);
    assert_eq!(status.runtime.state, FilesRuntimeState::Unloaded);
    assert_eq!(status.runtime.effects_count, 0);
    assert_eq!(status.runtime.in_flight, 0);
    assert!(status.runtime.providers.is_empty());
    let replayed = reopened
        .set_plugin_enabled(BUILTIN_FILES_ID, request(false, 0), "disable")
        .unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.receipt, disabled.receipt);
    assert_eq!(replayed.current, status);
}
#[test]
fn strict_request_and_safe_typed_errors() {
    for bad in [
        r#"{"enabled":true,"expected_revision":0,"extra":1}"#,
        r#"{"enabled":true,"enabled":false,"expected_revision":0}"#,
        r#"{"enabled":true,"expected_revision":-1}"#,
        r#"{"enabled":true,"expected_revision":0.0}"#,
        r#"{"enabled":null,"expected_revision":0}"#,
    ] {
        assert!(
            serde_json::from_str::<SetPluginEnabled>(bad).is_err(),
            "{bad}"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(&dir.path().join("state.sqlite"));
    assert_eq!(
        engine
            .plugin_status("unknown")
            .unwrap_err()
            .downcast_ref::<PluginManagementError>(),
        Some(&PluginManagementError::NotFound)
    );
    for (request, key, expected) in [
        (request(false, 0), "", PluginManagementError::Invalid),
        (
            request(false, u64::MAX),
            "range",
            PluginManagementError::Invalid,
        ),
        (
            request(false, 1),
            "stale",
            PluginManagementError::RevisionConflict,
        ),
    ] {
        assert_eq!(
            engine
                .set_plugin_enabled(BUILTIN_FILES_ID, request, key)
                .unwrap_err()
                .downcast_ref::<PluginManagementError>(),
            Some(&expected)
        );
    }
    assert_eq!(
        engine
            .plugin_status(BUILTIN_FILES_ID)
            .unwrap()
            .desired
            .revision,
        0
    );
}
#[test]
fn shell_state_is_independent_and_both_disabled_restart_without_activation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let first = engine(&path);
    let files = first.plugin_status(BUILTIN_FILES_ID).unwrap();
    let shell = first.plugin_status(BUILTIN_SHELL_ID).unwrap();
    assert_eq!(shell.version, "1.0.0");
    assert_eq!(shell.affected_tools, ["run_command"]);
    assert_eq!(shell.compat_tools, ["run_wasm"]);
    assert_eq!(shell.affected_dependents, ["builtin.engine-shell-tools"]);
    assert_eq!(shell.runtime.instances.len(), 2);
    assert_eq!(shell.runtime.effects_count, 1);
    assert!(shell.converged);
    first.disable_builtin_shell().unwrap();
    assert_eq!(first.plugin_status(BUILTIN_FILES_ID).unwrap(), files);
    let off = first
        .set_plugin_enabled(BUILTIN_SHELL_ID, request(false, 0), "shell-off")
        .unwrap();
    assert!(off.current.converged);
    let conflict = first
        .set_plugin_enabled(BUILTIN_FILES_ID, request(false, 0), "shell-off")
        .unwrap_err();
    assert_eq!(
        conflict.downcast_ref::<PluginManagementError>(),
        Some(&PluginManagementError::IdempotencyConflict)
    );
    assert_eq!(first.plugin_status(BUILTIN_FILES_ID).unwrap(), files);
    first
        .set_plugin_enabled(BUILTIN_FILES_ID, request(false, 0), "files-off")
        .unwrap();
    drop(first);
    let restarted = engine(&path);
    for state in restarted.plugins().unwrap() {
        assert!(!state.desired.enabled);
        assert!(state.converged);
        assert_eq!(state.runtime.effects_count, 0);
        assert_eq!(state.runtime.in_flight, 0);
        assert!(!state.runtime.admission_open);
        assert!(state.runtime.providers.is_empty());
    }
    let enabled = restarted
        .set_plugin_enabled(BUILTIN_SHELL_ID, request(true, 1), "shell-on")
        .unwrap();
    assert!(enabled.current.converged);
    assert_eq!(enabled.current.runtime.effects_count, 1);
    let files = restarted.plugin_status(BUILTIN_FILES_ID).unwrap();
    assert!(!files.desired.enabled && files.converged);
    restarted.unload_builtin_shell().unwrap();
    let stopped = restarted.plugin_status(BUILTIN_SHELL_ID).unwrap();
    let replay = restarted
        .set_plugin_enabled(BUILTIN_SHELL_ID, request(true, 1), "shell-on")
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.current, stopped);
    assert_eq!(restarted.plugin_status(BUILTIN_FILES_ID).unwrap(), files);
}
