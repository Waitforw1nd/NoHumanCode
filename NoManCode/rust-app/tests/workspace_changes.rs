use axum::{Json, Router, extract::State, routing::post};
use peachsh::{
    domain::{Route, RunRequest, SessionKind, Settings, TaskSpec},
    engine::Engine,
    store::Store,
    workspace_changes::{CurrentState, RestoreStatus, WorkspaceChangeError},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

type Script = Arc<Mutex<VecDeque<Value>>>;

async fn response(
    State(script): State<Script>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let delta = script
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| json!({"content":"done"}));
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

async fn server(path: &str, content: &str) -> (String, tokio::task::JoinHandle<()>) {
    let mut replies = VecDeque::new();
    if !path.is_empty() {
        replies.push_back(json!({"tool_calls":[{"index":0,"id":"write-1","type":"function","function":{"name":"write_file","arguments":json!({"path":path,"content":content}).to_string()}}]}));
    }
    replies.push_back(json!({"content":"done"}));
    let script = Arc::new(Mutex::new(replies));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(response))
        .with_state(script);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, handle)
}

fn setup(root: &Path, base: &str) -> Arc<Engine> {
    let store = Arc::new(Store::open(&root.join("workspace.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: root.to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "Route".into(),
                base_url: base.into(),
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "fake-workspace-key").unwrap();
    Engine::new(store, 2).unwrap()
}

async fn write_once(engine: &Arc<Engine>) -> String {
    let task = engine
        .start(RunRequest {
            title: "changes".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "worker".into(),
                role: "worker".into(),
                route_id: "route".into(),
                prompt: "write".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 4,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    let approval = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(value) = engine
                .store
                .pending_approvals_for_task(&task.id)
                .unwrap()
                .into_iter()
                .next()
            {
                break value;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    engine
        .decide_approval(&approval.id, true, Some("test"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let value = engine.store.task(&task.id).unwrap();
            if !matches!(value.status.as_str(), "queued" | "running") && !engine.is_busy() {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    task.id
}

#[tokio::test]
async fn approved_write_has_typed_metadata_and_restores_exact_bytes() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), b"before\r\nbytes").unwrap();
    let (base, _server) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    let changes = engine.changes(&task).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].current, CurrentState::Matches);
    assert!(changes[0].restorable);
    let serialized = serde_json::to_string(&changes).unwrap();
    assert!(!serialized.contains("before\\r\\nbytes"));
    let receipt = engine.restore(&task).await.unwrap();
    assert_eq!(receipt.status, RestoreStatus::Complete);
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before\r\nbytes"
    );
    assert_eq!(engine.restore(&task).await.unwrap(), receipt);
}

#[tokio::test]
async fn external_edit_conflicts_without_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, _server) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    std::fs::write(temp.path().join("src/a.txt"), "external").unwrap();
    let error = engine.restore(&task).await.unwrap_err();
    assert!(error.chain().any(|cause| matches!(
        cause.downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Conflict { .. })
    )));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "external"
    );
}

#[tokio::test]
async fn empty_restore_is_noop_without_operation() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _server) = server("", "").await;
    let engine = setup(temp.path(), &base);
    let task = engine
        .start(RunRequest {
            title: "empty".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "reader".into(),
                role: "reader".into(),
                route_id: "route".into(),
                prompt: "done".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: false,
                allow_commands: false,
                max_rounds: 1,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if !engine.is_busy() {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    let receipt = engine.restore(&task.id).await.unwrap();
    assert_eq!(receipt.status, RestoreStatus::Complete);
    assert_eq!(receipt.restore_id, None);
    assert!(engine.latest_restore(&task.id).unwrap().is_none());
}

#[test]
fn schema7_migrates_and_forged_schema8_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("workspace.db");
    let store = Store::open(&path).unwrap();
    store
        .save_settings(&Settings {
            workspace: temp.path().to_string_lossy().into(),
            max_concurrency: 1,
            routes: vec![],
            newapi: None,
        })
        .unwrap();
    drop(store);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE workspace_restore_outcomes; DROP TABLE workspace_restores; DROP TABLE workspace_changes; DELETE FROM schema_migrations WHERE id='workspace-change-repository'; PRAGMA user_version=7;").unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 8);
    assert!(store.settings().unwrap().is_some());
    drop(store);
    db.execute_batch("DROP INDEX workspace_changes_task_path; CREATE INDEX workspace_changes_task_path ON workspace_changes(path_key,task_id,created_at);").unwrap();
    assert!(Store::open(&path).is_err());
}
