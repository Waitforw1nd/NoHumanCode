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

#[tokio::test]
async fn runtime_state_relations_accept_real_lifecycle_and_reject_impossible_transition() {
    let mut draining = current(false, 1);
    draining["runtime"]["active"] = json!(true);
    draining["runtime"]["in_flight"] = json!(u64::MAX);
    draining["runtime"]["generation"] = json!(u64::MAX);
    draining["runtime"]["draining"] = json!(true);
    draining["runtime"]["effects"] = json!(1);
    draining["converged"] = json!(false);
    let provider = json!({"id":"builtin.provider-xpeach","capability":"provider.completion-codec@1.0.0","desired":{"scope":"host","enabled":false,"revision":1,"updated_at":1},"runtime":{"kind":"completion_provider","scope":"process","active":false,"admission_open":false,"in_flight":u64::MAX,"generation":u64::MAX,"transitioning":false,"effects":0,"recovery":0,"lifecycle_failed":false},"converged":true});
    let mut transition = current(false, 1);
    transition["runtime"]["transitioning"] = json!(true);
    transition["runtime"]["effects"] = json!(1);
    transition["converged"] = json!(false);
    for value in [draining, provider, transition.clone()] {
        let id = value["id"].as_str().unwrap();
        let (port, _, task) = server(value.to_string(), axum::http::StatusCode::OK).await;
        let out = cli(port, &["runtime", "component", "status", id]).await;
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), value);
        task.abort();
    }
    transition["runtime"]["admission_open"] = json!(true);
    let (port, _, task) = server(transition.to_string(), axum::http::StatusCode::OK).await;
    let out = cli(
        port,
        &["runtime", "component", "status", "builtin.chat-agent"],
    )
    .await;
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    task.abort();
    let body = json!({"receipt":{"plugin_id":"builtin.chat-agent","enabled":false,"previous_revision":0,"revision":1,"committed_at":1},"replayed":false,"current":transition});
    let (port, _, task) = server(body.to_string(), axum::http::StatusCode::OK).await;
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
            "bad-state",
        ],
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stderr).unwrap()["code"],
        "operation_result_unknown"
    );
    assert!(out.stdout.is_empty());
    task.abort();
}

#[tokio::test]
async fn malformed_nested_duplicate_oversize_and_error_mapping_are_rejected() {
    let valid = current(true, 0).to_string();
    let nested = valid.replacen(
        "\"scope\":\"process\"",
        "\"scope\":\"process\",\"scope\":\"process\"",
        1,
    );
    assert_ne!(nested, valid);
    let cases = [
        (nested, axum::http::StatusCode::OK),
        ("x".repeat(256 * 1024 + 1), axum::http::StatusCode::OK),
        (json!({"code":"runtime_busy","message":"Runtime transition busy","error":"Runtime transition busy","retryable":false}).to_string(), axum::http::StatusCode::BAD_REQUEST),
        (json!({"code":"runtime_busy","message":"Runtime transition busy","error":"Runtime transition busy","retryable":"false"}).to_string(), axum::http::StatusCode::CONFLICT),
    ];
    for (body, status) in cases {
        let (port, calls, task) = server(body, status).await;
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
                "boundary",
            ],
        )
        .await;
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stderr).unwrap()["code"],
            "operation_result_unknown"
        );
        assert!(out.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&out.stderr).contains(TOKEN));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort();
    }
}
#[tokio::test]
async fn human_output_and_argument_exit_codes() {
    let value = current(true, 0);
    let (port, _, task) = server(value.to_string(), axum::http::StatusCode::OK).await;
    let out = tokio::time::timeout(
        Duration::from_secs(12),
        Command::new(BIN)
            .arg("--port")
            .arg(port.to_string())
            .args(["runtime", "component", "status", "builtin.chat-agent"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), value);
    let invalid = cli(
        port,
        &[
            "runtime",
            "component",
            "set-enabled",
            "builtin.chat-agent",
            "--expected-revision",
            "0",
            "--key",
            "missing-enabled",
        ],
    )
    .await;
    assert_eq!(invalid.status.code(), Some(2));
    task.abort();
}
