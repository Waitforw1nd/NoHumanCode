//! M12: actual CLI against adversarial runtime management responses.
use axum::{
    Json, Router,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_peachsh");
const TOKEN: &str = "synthetic-runtime-token";
#[derive(Clone)]
struct Reply {
    body: String,
    status: axum::http::StatusCode,
    calls: Arc<AtomicUsize>,
}
async fn bootstrap() -> Json<Value> {
    Json(json!({"token":TOKEN}))
}
async fn reply(State(state): State<Reply>, request: Request) -> Response {
    assert_eq!(request.headers().get("x-peachsh-token").unwrap(), TOKEN);
    state.calls.fetch_add(1, Ordering::SeqCst);
    if state.body == "TRUNCATE" {
        return (
            state.status,
            [
                ("content-type", "application/json"),
                ("content-length", "100"),
            ],
            "{",
        )
            .into_response();
    }
    (
        state.status,
        [("content-type", "application/json")],
        state.body,
    )
        .into_response()
}
async fn server(
    body: String,
    status: axum::http::StatusCode,
) -> (u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/runtime/components/{id}", get(reply))
        .route(
            "/api/runtime/components/{id}/enabled",
            axum::routing::put(reply),
        )
        .with_state(Reply {
            body,
            status,
            calls: calls.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (
        port,
        calls,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
fn current(enabled: bool, revision: u64) -> Value {
    json!({"id":"builtin.chat-agent","capability":"agent.strategy@1.0.0", "desired":{"scope":"host","enabled":enabled,"revision":revision,"updated_at":0}, "runtime":{"kind":"agent","scope":"process","active":enabled,"admission_open":enabled,"in_flight":0,"generation":1,"draining":false,"transitioning":false,"effects":if enabled {1} else {0},"recovery":0,"last_failure":null},"converged":true})
}
async fn cli(port: u16, args: &[&str]) -> std::process::Output {
    tokio::time::timeout(
        Duration::from_secs(12),
        Command::new(BIN)
            .arg("--port")
            .arg(port.to_string())
            .arg("--json")
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}
#[tokio::test]
async fn validates_duplicate_fields_busy_and_replayed_current() {
    let id = "builtin.chat-agent";
    let args = [
        "runtime",
        "component",
        "set-enabled",
        id,
        "--enabled",
        "false",
        "--expected-revision",
        "0",
        "--key",
        "first",
    ];
    let value = json!({"receipt":{"plugin_id":id,"enabled":false,"previous_revision":0,"revision":1,"committed_at":1},"replayed":true,"current":current(true,2)});
    let (port, calls, task) = server(value.to_string(), axum::http::StatusCode::OK).await;
    let out = cli(port, &args).await;
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), value);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
    let duplicate = value.to_string().replacen(
        "\"replayed\":true",
        "\"replayed\":true,\"replayed\":true",
        1,
    );
    assert_ne!(duplicate, value.to_string());
    let (port, calls, task) = server(duplicate, axum::http::StatusCode::OK).await;
    let out = cli(port, &args).await;
    assert_eq!(out.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(error["code"], "operation_result_unknown");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
    let busy = json!({"code":"runtime_busy","message":"Runtime transition busy","error":"Runtime transition busy","retryable":false});
    let (port, calls, task) = server(busy.to_string(), axum::http::StatusCode::CONFLICT).await;
    let out = cli(port, &args).await;
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stderr).unwrap()["code"],
        "runtime_busy"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn truncated_write_is_unknown_and_sent_once() {
    let (port, calls, task) = server("TRUNCATE".to_owned(), axum::http::StatusCode::OK).await;
    let out = cli(
        port,
        &[
            "runtime",
            "component",
            "set-enabled",
            "builtin.chat-agent",
            "--enabled",
            "false",
            "--expected-revision",
            "0",
            "--key",
            "lost",
        ],
    )
    .await;
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stderr).unwrap()["code"],
        "operation_result_unknown"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!String::from_utf8_lossy(&out.stderr).contains(TOKEN));
    task.abort();
}

#[tokio::test]
async fn hostile_status_objects_never_reach_stdout() {
    let mut cases = Vec::new();
    let mut extra = current(true, 0);
    extra["runtime"]["secret"] = json!(TOKEN);
    cases.push(extra);
    let mut mismatch = current(true, 0);
    mismatch["capability"] = json!("provider.completion-codec@1.0.0");
    cases.push(mismatch);
    let mut generation = current(true, 0);
    generation["runtime"]["generation"] = json!(0);
    cases.push(generation);
    let mut invalid_failure = current(true, 0);
    invalid_failure["runtime"]["last_failure"] = json!("untrusted text");
    cases.push(invalid_failure);
    let mut wrong_converged = current(true, 0);
    wrong_converged["converged"] = json!(false);
    cases.push(wrong_converged);
    for value in cases {
        let (port, calls, task) = server(value.to_string(), axum::http::StatusCode::OK).await;
        let out = cli(
            port,
            &["runtime", "component", "status", "builtin.chat-agent"],
        )
        .await;
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains(TOKEN));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort();
    }
}
