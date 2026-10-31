use crate::{
    tool_call_application::ToolCallApplication,
    tool_result_tests::{DENIED, fixture, snapshot},
    tool_results::Origin,
    tool_runtime::ToolRuntime,
};
use std::sync::{Arc, Mutex};

#[test]
fn t03_service_updates_caller_only_after_commit() {
    let dir = tempfile::tempdir().unwrap();
    let (store, mut task) = fixture(&dir.path().join("service.db"));
    let store = Arc::new(store);
    let service = ToolCallApplication::new(
        store.clone(),
        Arc::new(ToolRuntime::builtin().unwrap()),
        Arc::new(tokio::sync::Mutex::new(())),
        Arc::new(Mutex::new(None)),
    );
    let before = snapshot(&store);
    let messages = task.messages.clone();
    store.db.lock().unwrap().execute_batch("CREATE TRIGGER reject_result BEFORE INSERT ON events WHEN NEW.kind='tool_result' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let error = service
        .record_tool_result(
            &mut task,
            "call",
            "write_file",
            DENIED,
            "unused-secret",
            Origin::AgentDenied,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("injected"));
    assert_eq!(task.messages, messages);
    assert_eq!(snapshot(&store), before);
    store
        .db
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_result;")
        .unwrap();
    store.db.lock().unwrap().execute_batch("CREATE TABLE commit_parent(id INTEGER PRIMARY KEY); CREATE TABLE commit_child(id INTEGER REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER reject_commit AFTER INSERT ON events WHEN NEW.kind='tool_result' BEGIN INSERT INTO commit_child VALUES(42); END;").unwrap();
    let error = service
        .record_tool_result(
            &mut task,
            "call",
            "write_file",
            DENIED,
            "unused-secret",
            Origin::AgentDenied,
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("FOREIGN KEY constraint failed"));
    assert_eq!(task.messages, messages);
    assert_eq!(snapshot(&store), before);
    store
        .db
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_commit;")
        .unwrap();
    service
        .record_tool_result(
            &mut task,
            "call",
            "write_file",
            DENIED,
            "unused-secret",
            Origin::AgentDenied,
        )
        .unwrap();
    assert_eq!(task.messages, store.task(&task.id).unwrap().messages);
    assert_eq!(task.messages.len(), messages.len() + 1);
    assert_eq!(snapshot(&store).3, 1);
}

#[tokio::test]
async fn t07_orphan_write_and_command_events_do_not_turn_into_interrupted_success() {
    use crate::tool_result_tests::fixture_call;
    use serde_json::json;
    for (name, args) in [
        ("write_file", json!({"path":"a","content":"never"})),
        ("run_command", json!({"command":"echo never"})),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (store, mut task) = fixture_call(&dir.path().join("orphan.db"), name, args);
        let store = Arc::new(store);
        store
            .event(
                &task.id,
                "tool_result",
                json!({"name":name,"tool_call_id":"call","result":{"ok":true}}),
            )
            .unwrap();
        let before = snapshot(&store);
        let messages = task.messages.clone();
        let service = ToolCallApplication::new(
            store.clone(),
            Arc::new(ToolRuntime::builtin().unwrap()),
            Arc::new(tokio::sync::Mutex::new(())),
            Arc::new(Mutex::new(None)),
        );
        assert!(
            service
                .reconcile_open_tools(
                    &mut task,
                    &tokio_util::sync::CancellationToken::new(),
                    "unused-secret"
                )
                .await
                .is_err(),
            "{name}"
        );
        assert_eq!(task.messages, messages);
        assert_eq!(snapshot(&store), before);
        assert!(store.approvals_for_task(&task.id).unwrap().is_empty());
        assert!(!dir.path().join("a").exists());
    }
}
