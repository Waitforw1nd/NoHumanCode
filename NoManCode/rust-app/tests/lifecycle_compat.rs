//! Public cancellation/closing compatibility. No paid provider or shell command.
//! These assertions cover receipts and admission, not OS process-tree cleanup.
#![cfg(windows)]

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Request as HttpRequest, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use peachsh::{
    approval::{ApprovalStatus, ExecutionState},
    domain::{Route, Run, SessionKind, Task, TaskSpec},
    engine::Engine,
    server::{self, App},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    fs::File,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

const TOKEN: &str = "synthetic-lifecycle-token";
const HOST: &str = "127.0.0.1:18327";

fn evidence_root() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".local/lifecycle-compat");
    std::fs::create_dir_all(&base).unwrap();
    let base = base.canonicalize().unwrap();
    assert!(
        base.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_ascii_lowercase()
            .starts_with("e:\\"),
        "fixture output must remain on E"
    );
    // Retain evidence, also on an assertion failure; no auto-delete of a scene.
    tempfile::tempdir_in(base).unwrap().keep()
}

fn fixture() -> (Arc<Engine>, Task, String) {
    let root = evidence_root();
    let store = Arc::new(Store::open(&root.join("compat.sqlite3")).unwrap());
    let args = json!({"path":"note.txt","content":"never execute"});
    let task = Task {
        id: "compat-task".into(),
        run_id: "compat-run".into(),
        spec: TaskSpec {
            name: "compat".into(),
            role: "worker".into(),
            route_id: "route".into(),
            prompt: "compatibility".into(),
            depends_on: vec![],
            write_scopes: vec!["*".into()],
            tools: true,
            allow_commands: false,
            max_rounds: 1,
        },
        route: Route {
            id: "route".into(),
            name: "fixture".into(),
            base_url: "https://example.invalid/v1".into(),
            model: "fixture".into(),
            max_tokens: 128,
            parallel_limit: 1,
            key_env: None,
        },
        workspace: root.to_string_lossy().into_owned(),
        status: "running".into(),
        output: String::new(),
        error: None,
        messages: vec![
            json!({"role":"user","content":"compatibility"}),
            json!({
                "role":"assistant", "tool_calls":[{"id":"compat-call","type":"function",
                    "function":{"name":"write_file","arguments":args.to_string()}}]
            }),
        ],
        usage: Value::Null,
        created_at: 1,
        updated_at: 1,
    };
    store
        .create_run(&Run {
            id: task.run_id.clone(),
            title: "compat".into(),
            kind: SessionKind::Team,
            created_at: 1,
            tasks: vec![task.clone()],
        })
        .unwrap();
    let engine = Engine::new(store, 1).unwrap();
    let approval = engine
        .store
        .ensure_approval(&task, "compat-call", "write_file", &args)
        .unwrap()
        .0;
    (engine, task, approval.id)
}

fn router(engine: Arc<Engine>) -> Router {
    server::router(App {
        engine,
        token: TOKEN.into(),
        origin: format!("http://{HOST}"),
    })
}

async fn request(app: Router, path: &str, token: bool, body: Value) -> (StatusCode, Value) {
    let mut request = HttpRequest::builder()
        .method("POST")
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json");
    if token {
        request = request.header("x-peachsh-token", TOKEN);
    }
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        app.oneshot(request.body(Body::from(body.to_string())).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn http_cancel_keeps_auth_and_repeated_receipt_without_reexecuting_approval() {
    let (engine, task, approval_id) = fixture();
    let app = router(engine.clone());
    let path = format!("/api/tasks/{}/cancel", task.id);
    let before = serde_json::to_value(engine.store.events(&task.run_id, 0).unwrap()).unwrap();
    let (status, body) = request(app.clone(), &path, false, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["retryable"], false);
    assert_eq!(
        engine.store.approval(&approval_id).unwrap().status,
        ApprovalStatus::Pending
    );
    assert_eq!(
        serde_json::to_value(engine.store.events(&task.run_id, 0).unwrap()).unwrap(),
        before
    );

    assert_eq!(
        request(app.clone(), &path, true, json!({})).await,
        (StatusCode::OK, json!({"ok":true}))
    );
    let approval = engine.store.approval(&approval_id).unwrap();
    assert_eq!(approval.status, ApprovalStatus::Cancelled);
    assert_eq!(approval.execution_state, ExecutionState::Cancelled);
    let once = serde_json::to_value(engine.store.events(&task.run_id, 0).unwrap()).unwrap();
    assert_eq!(
        request(app, &path, true, json!({})).await,
        (StatusCode::OK, json!({"ok":true}))
    );
    assert_eq!(
        serde_json::to_value(engine.store.events(&task.run_id, 0).unwrap()).unwrap(),
        once
    );
    assert!(!PathBuf::from(&task.workspace).join("note.txt").exists());
    assert!(engine.shutdown(Duration::from_secs(2)).await.settled());
}

#[tokio::test]
async fn http_shutdown_rejects_resume_without_changing_task_or_queued_input() {
    let (engine, task, _) = fixture();
    engine.request_shutdown();
    let before_task = serde_json::to_value(engine.store.task(&task.id).unwrap()).unwrap();
    let before_events =
        serde_json::to_value(engine.store.events(&task.run_id, 0).unwrap()).unwrap();
    // Pending resume input is persisted in the event stream; comparing every
    // event also proves rejection did not enqueue a hidden follow-up input.
    let (status, body) = request(
        router(engine.clone()),
        &format!("/api/tasks/{}/resume", task.id),
        true,
        json!({"message":"do not queue after shutdown"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "request_failed");
    assert_eq!(body["retryable"], false);
    assert_eq!(body["error"], body["message"]);
    assert!(body["message"].as_str().unwrap().contains("Host 正在关闭"));
    assert_eq!(
        serde_json::to_value(engine.store.task(&task.id).unwrap()).unwrap(),
        before_task
    );
    assert_eq!(
        serde_json::to_value(engine.store.events(&task.run_id, 0).unwrap()).unwrap(),
        before_events
    );
    assert!(engine.shutdown(Duration::from_secs(2)).await.settled());
}

#[derive(Clone)]
struct Reply {
    value: Value,
    status: StatusCode,
    truncate: bool,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

async fn cli_reply(State(reply): State<Reply>, request: Request) -> Response {
    let token = request
        .headers()
        .get("x-peachsh-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = to_bytes(request.into_body(), 4096).await.unwrap();
    reply
        .calls
        .lock()
        .unwrap()
        .push((token, serde_json::from_slice(&bytes).unwrap()));
    if reply.truncate {
        let stream = async_stream::stream! {
            yield Ok::<_, std::io::Error>(b"{".to_vec());
            yield Err(std::io::Error::other("synthetic response interruption"));
        };
        return (
            [(header::CONTENT_TYPE, "application/json")],
            Body::from_stream(stream),
        )
            .into_response();
    }
    (reply.status, Json(reply.value)).into_response()
}

async fn cli_cancel(reply: Reply) -> (i32, Vec<u8>, Vec<u8>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route(
            "/api/bootstrap",
            get(|| async { Json(json!({"token":TOKEN})) }),
        )
        .route("/api/tasks/compat-task/cancel", post(cli_reply))
        .with_state(reply);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let root = evidence_root();
    let stdout = root.join("cli.stdout");
    let stderr = root.join("cli.stderr");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"));
    command
        .args([
            "--port",
            &port.to_string(),
            "--json",
            "task",
            "cancel",
            "compat-task",
        ])
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap())
        .creation_flags(0x0800_0000);
    let mut child = command.spawn().unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(15), child.wait()).await;
    let (status, unknown) = match observed {
        Ok(Ok(status)) => (status, false),
        _ => {
            eprintln!(
                "NHC_TERMINAL_UNKNOWN CLI compatibility deadline/observation; holding original Child at {}",
                root.display()
            );
            // Keep the exact creation object. No name/PID kill or premature Drop.
            let status = loop {
                if let Ok(status) = child.wait().await {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            };
            (status, true)
        }
    };
    server.abort();
    let _ = server.await;
    std::fs::write(
        root.join("direct-cli-exit.json"),
        json!({"exit_code":status.code(),
        "terminal_unknown":unknown,"scope":"direct CLI process only, no tree claim"})
        .to_string(),
    )
    .unwrap();
    assert!(
        !unknown,
        "CLI exceeded its deadline; retained evidence is not passing"
    );
    (
        status.code().unwrap(),
        std::fs::read(stdout).unwrap(),
        std::fs::read(stderr).unwrap(),
    )
}

#[tokio::test]
async fn cli_cancel_ack_and_known_shutdown_refusal_are_single_requests() {
    for (value, status, expected_code) in [
        (json!({"ok":true}), StatusCode::OK, 0),
        (
            json!({"code":"request_failed","message":"Host 正在关闭，拒绝新工作",
                "error":"Host 正在关闭，拒绝新工作","retryable":false}),
            StatusCode::BAD_REQUEST,
            1,
        ),
    ] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (code, stdout, stderr) = cli_cancel(Reply {
            value,
            status,
            truncate: false,
            calls: calls.clone(),
        })
        .await;
        assert_eq!(code, expected_code);
        assert_eq!(*calls.lock().unwrap(), vec![(TOKEN.into(), json!({}))]);
        if code == 0 {
            assert!(stderr.is_empty());
            assert_eq!(
                serde_json::from_slice::<Value>(&stdout).unwrap(),
                json!({"ok":true})
            );
        } else {
            assert!(stdout.is_empty());
            let error: Value = serde_json::from_slice(&stderr).unwrap();
            assert_eq!(error["code"], "request_failed");
            assert_eq!(error["retryable"], false);
        }
    }
}

#[tokio::test]
async fn cli_cancel_lost_or_invalid_ack_is_unknown_without_automatic_retry() {
    for truncate in [false, true] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (code, stdout, stderr) = cli_cancel(Reply {
            value: json!({"ok":false}),
            status: StatusCode::OK,
            truncate,
            calls: calls.clone(),
        })
        .await;
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let error: Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(error["code"], "operation_result_unknown");
        assert_eq!(error["retryable"], false);
        assert_eq!(*calls.lock().unwrap(), vec![(TOKEN.into(), json!({}))]);
    }
}
