//! NEXT-07: real CLI processes against a bounded local Host response fixture.
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    process::Output,
    sync::{Arc, Mutex},
    time::Duration,
};

type RecordedRequest = (String, String, Value, bool, Option<String>);

#[derive(Clone)]
struct Mock {
    replies: Arc<Mutex<VecDeque<(u16, Value)>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}
struct Host {
    port: u16,
    state: Mock,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn reply(
    State(mock): State<Mock>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if uri.path() == "/api/bootstrap" {
        return (
            StatusCode::OK,
            axum::Json(json!({"token":"local-test-token"})),
        );
    }
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    mock.requests.lock().unwrap().push((
        method.to_string(),
        uri.path().into(),
        body,
        headers
            .get("x-peachsh-token")
            .is_some_and(|v| v == "local-test-token"),
        headers
            .get("idempotency-key")
            .map(|v| v.to_str().unwrap().to_owned()),
    ));
    let (status, body) = mock
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected CLI request");
    (StatusCode::from_u16(status).unwrap(), axum::Json(body))
}
impl Host {
    async fn new(replies: Vec<(u16, Value)>) -> Self {
        let state = Mock {
            replies: Arc::new(Mutex::new(replies.into())),
            requests: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().fallback(reply).with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { port, state, task }
    }
    async fn cli(&self, args: &[&str]) -> Output {
        tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
                .arg("--port")
                .arg(self.port.to_string())
                .arg("--json")
                .args(args)
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("HTTPS_PROXY", "http://127.0.0.1:1")
                .env("ALL_PROXY", "http://127.0.0.1:1")
                .env("NO_PROXY", "")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap()
    }
}
fn ok(output: Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn error(output: Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).unwrap()
}

fn snapshot(revision: u64) -> Value {
    json!({"project_id":"project-a","revision":revision,"config":if revision == 0 { Value::Null } else { json!({"limit_tokens":100,"per_call_reservation_tokens":10}) }})
}
fn status() -> Value {
    json!({"project_id":"project-a","snapshot":snapshot(1),"mode":"configured","spent_tokens":0,"held_tokens":0,"available_tokens":100,"claimed_count":0,"unresolved_count":0,"overrun_count":0,"blocked_reason":null,"coverage":"since_configuration","coverage_start_call_rowid":0})
}
fn call(state: &str) -> Value {
    let finished = matches!(
        state,
        "settled" | "overrun" | "usage_missing" | "usage_invalid"
    );
    let network_state = if finished { "finished" } else { state };
    let reason = match state {
        "usage_missing" => json!("usage_missing"),
        "usage_invalid" => json!("usage_invalid"),
        "overrun" => json!("reservation_overrun"),
        "unknown" => json!("transport_unknown"),
        "not_sent" => json!("preflight_rejected"),
        _ => Value::Null,
    };
    let result = match state {
        "unknown" => json!("transport_unknown"),
        "not_sent" => json!("preflight_rejected"),
        "claimed" => Value::Null,
        _ => json!("success"),
    };
    json!({"project_id":"project-a","call_id":"call-a","managed":true,"budget_revision":1,"reserved_tokens":10,"state":state,"usage":if state=="settled" { json!({"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}) } else if state=="overrun" { json!({"prompt_tokens":8,"completion_tokens":3,"total_tokens":11}) } else { Value::Null },"reason":reason,"call":{"call_id":"call-a","authorization_id":"auth-a","subject":{"kind":"route","id":"route-a","project_id":"project-a"},"task_id":null,"purpose":"completion","method":"POST","origin":"https://example.com:443","endpoint_digest":"a".repeat(64),"state":network_state,"claimed_at":1,"finished_at":if state=="claimed" {Value::Null} else {json!(2)},"success":if finished {json!(true)} else {Value::Null},"http_status":if finished {json!(200)} else {Value::Null},"result":result}})
}
const SET: &[&str] = &[
    "project",
    "budget",
    "config",
    "set",
    "--id",
    "project-a",
    "--expected-revision",
    "0",
    "--limit-tokens",
    "100",
    "--per-call-reservation-tokens",
    "10",
    "--key",
    "budget-1",
];
const STATUS: &[&str] = &["project", "budget", "status", "--id", "project-a"];
const CALL: &[&str] = &[
    "project",
    "budget",
    "call",
    "--id",
    "project-a",
    "--call-id",
    "call-a",
];
#[tokio::test]
async fn budget_cli_config_exact_put_and_replayed_original_snapshot() {
    let h = Host::new(vec![
        (200, snapshot(0)),
        (200, json!({"snapshot":snapshot(1),"replayed":false})),
        (200, json!({"snapshot":snapshot(1),"replayed":true})),
    ])
    .await;
    assert_eq!(
        ok(
            h.cli(&["project", "budget", "config", "get", "--id", "project-a"])
                .await
        ),
        snapshot(0)
    );
    assert_eq!(ok(h.cli(SET).await)["replayed"], false);
    assert_eq!(ok(h.cli(SET).await)["replayed"], true);
    let requests = h.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].0, "GET");
    for request in &requests[1..] {
        assert_eq!(request.0, "PUT");
        assert_eq!(request.1, "/api/projects/project-a/budget");
        assert!(request.3);
        assert!(request.4.is_none());
        assert_eq!(
            request.2,
            json!({"expected_revision":0,"idempotency_key":"budget-1","config":{"limit_tokens":100,"per_call_reservation_tokens":10}})
        );
    }
}
#[tokio::test]
async fn budget_cli_status_blocked_is_success_and_diagnosis_is_read_only() {
    let mut blocked = status();
    blocked["held_tokens"] = json!(10);
    blocked["available_tokens"] = json!(90);
    blocked["unresolved_count"] = json!(1);
    blocked["blocked_reason"] = json!("unresolved");
    let h = Host::new(vec![(200, blocked.clone())]).await;
    assert_eq!(ok(h.cli(STATUS).await), blocked);
    let requests = h.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "GET");
    assert!(requests[0].3);
}
#[tokio::test]
async fn budget_cli_accepts_every_legal_call_state_and_unmanaged_history() {
    let mut states = [
        "claimed",
        "settled",
        "not_sent",
        "usage_missing",
        "usage_invalid",
        "unknown",
        "overrun",
    ]
    .map(call)
    .to_vec();
    let mut unmanaged = call("unknown");
    unmanaged["managed"] = json!(false);
    for field in [
        "budget_revision",
        "reserved_tokens",
        "state",
        "usage",
        "reason",
    ] {
        unmanaged[field] = Value::Null;
    }
    states.push(unmanaged);
    let h = Host::new(states.iter().cloned().map(|v| (200, v)).collect()).await;
    for expected in states {
        assert_eq!(ok(h.cli(CALL).await), expected);
    }
    assert!(
        h.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.0 == "GET" && r.1.ends_with("/budget/calls/call-a") && r.3)
    );
}
#[tokio::test]
async fn budget_cli_rejects_inconsistent_status_and_call_shapes() {
    let mut invalid = Vec::new();
    for (field, value) in [
        ("available_tokens", json!(99)),
        ("project_id", json!("wrong")),
        ("held_tokens", json!(1)),
        ("coverage", json!("all_history")),
        ("blocked_reason", json!("exceeded")),
        ("claimed_count", json!(9223372036854775808u64)),
    ] {
        let mut s = status();
        s[field] = value;
        invalid.push(s);
    }
    let mut missing = status();
    missing.as_object_mut().unwrap().remove("blocked_reason");
    invalid.push(missing);
    for value in invalid {
        let h = Host::new(vec![(200, value)]).await;
        assert_eq!(error(h.cli(STATUS).await)["code"], "internal");
    }
    let mut invalid = Vec::new();
    for (field, value) in [
        ("call_id", json!("other")),
        ("managed", json!(false)),
        ("reason", json!("transport_unknown")),
        ("reserved_tokens", json!(1)),
    ] {
        let mut c = call("settled");
        c[field] = value;
        invalid.push(c);
    }
    let mut mismatch = call("unknown");
    mismatch["call"]["subject"]["project_id"] = json!("project-b");
    invalid.push(mismatch);
    let mut mismatch = call("settled");
    mismatch["usage"]["total_tokens"] = json!(6);
    invalid.push(mismatch);
    let mut missing = call("claimed");
    missing.as_object_mut().unwrap().remove("usage");
    invalid.push(missing);
    for value in invalid {
        let h = Host::new(vec![(200, value)]).await;
        assert_eq!(error(h.cli(CALL).await)["code"], "internal");
    }
}
#[tokio::test]
async fn budget_cli_budget_errors_are_typed_and_writes_are_single_attempt() {
    for (code, http) in [
        ("budget_invalid", 400),
        ("budget_invalid", 413),
        ("budget_invalid", 415),
        ("budget_not_found", 404),
        ("budget_revision_conflict", 409),
        ("budget_conflict", 409),
        ("budget_busy", 409),
        ("budget_exceeded", 409),
        ("budget_unresolved", 409),
        ("budget_overrun", 409),
        ("budget_corrupt", 500),
        ("budget_internal", 500),
    ] {
        let h=Host::new(vec![(http,json!({"error":{"code":code,"message":"预算操作被拒绝","dispatch_state":"not_sent","calls":[],"retryable":false}}))]).await;
        let result = error(h.cli(SET).await);
        assert_eq!(result["code"], code);
        assert_eq!(result["retryable"], false);
        assert_eq!(h.state.requests.lock().unwrap().len(), 1);
    }
    for value in [
        json!({"snapshot":snapshot(1),"replayed":false,"unexpected":true}),
        json!({"snapshot":snapshot(2),"replayed":false}),
        json!({"snapshot":snapshot(1),"replayed":"no"}),
        json!({"snapshot":snapshot(1),"replayed":false,"echo":"local-test-token"}),
    ] {
        let h = Host::new(vec![(200, value)]).await;
        let result = error(h.cli(SET).await);
        assert_eq!(result["code"], "operation_result_unknown");
        assert_eq!(result["retryable"], false);
        assert!(!result.to_string().contains("local-test-token"));
        assert_eq!(h.state.requests.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn budget_cli_human_explains_limits_and_invalid_arguments_send_nothing() {
    let h = Host::new(vec![(200, snapshot(0))]).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
        .args([
            "--port",
            &h.port.to_string(),
            "project",
            "budget",
            "config",
            "get",
            "--id",
            "project-a",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("unconfigured"));
    assert!(text.contains("硬上限"));
    assert!(text.contains("历史未覆盖"));
    for args in [
        vec!["project", "budget", "status", "--id", ".."],
        vec![
            "project",
            "budget",
            "config",
            "set",
            "--id",
            "project-a",
            "--expected-revision",
            "0",
            "--limit-tokens",
            "1000000000001",
            "--per-call-reservation-tokens",
            "10",
            "--key",
            "x",
        ],
        vec![
            "project",
            "budget",
            "config",
            "set",
            "--id",
            "project-a",
            "--expected-revision",
            "0",
            "--limit-tokens",
            "100",
            "--per-call-reservation-tokens",
            "0",
            "--key",
            "x",
        ],
    ] {
        assert_eq!(h.cli(&args).await.status.code(), Some(2));
    }
    assert_eq!(h.state.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn budget_cli_closed_stdout_is_write_unknown_without_repeat() {
    let h = Host::new(vec![(
        200,
        json!({"snapshot":snapshot(1),"replayed":false}),
    )])
    .await;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
        .args(["--port", &h.port.to_string(), "--json"])
        .args(SET)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let failure = error(output);
    assert_eq!(failure["code"], "operation_result_unknown");
    assert_eq!(failure["retryable"], false);
    assert!(failure.get("calls").is_none());
    assert_eq!(h.state.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn budget_cli_raw_duplicate_and_broken_wire_are_not_accepted_or_retried() {
    for payload in [
        "{",
        "{\"snapshot\":{\"project_id\":\"project-a\",\"revision\":1,\"config\":{\"limit_tokens\":100,\"per_call_reservation_tokens\":10}},\"replayed\":false,\"replayed\":false}",
        "{\"error\":{\"code\":\"budget_invalid\",\"message\":\"safe\",\"dispatch_state\":\"not_sent\",\"calls\":[],\"retryable\":false,\"retryable\":false}}",
    ] {
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = requests.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().fallback(move |uri: Uri| {
            let counter = counter.clone();
            async move {
                if uri.path() == "/api/bootstrap" {
                    return (StatusCode::OK, "{\"token\":\"local-test-token\"}").into_response();
                }
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (
                    if payload.contains("error") {
                        StatusCode::BAD_REQUEST
                    } else {
                        StatusCode::OK
                    },
                    payload,
                )
                    .into_response()
            }
        });
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
            .args(["--port", &port.to_string(), "--json"])
            .args(SET)
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert_eq!(error(output)["code"], "operation_result_unknown");
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        task.abort();
        let _ = task.await;
    }
}
#[tokio::test]
async fn budget_cli_received_put_then_real_transport_truncation_is_unknown_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for partial_response in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let puts = Arc::new(AtomicUsize::new(0));
        let count = puts.clone();
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = tokio::select! { result=listener.accept()=>result.unwrap(), _=&mut done_rx=>break };
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 1024];
                let header_end = loop {
                    let read = socket.read(&mut chunk).await.unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break index + 4;
                    }
                    assert!(bytes.len() < 8192);
                };
                let header = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let read = socket.read(&mut chunk).await.unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&chunk[..read]);
                }
                if header.starts_with("GET /api/bootstrap ") {
                    let body = "{\"token\":\"local-test-token\"}";
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                } else {
                    assert!(header.starts_with("PUT /api/projects/project-a/budget "));
                    let body: Value =
                        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                    assert_eq!(
                        body,
                        json!({"expected_revision":0,"idempotency_key":"budget-1","config":{"limit_tokens":100,"per_call_reservation_tokens":10}})
                    );
                    count.fetch_add(1, Ordering::SeqCst);
                    if partial_response {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 500\r\nConnection: close\r\n\r\n{\"snapshot\":").await.unwrap();
                    }
                }
                socket.shutdown().await.unwrap();
            }
        });
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
                .args(["--port", &port.to_string(), "--json"])
                .args(SET)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        let failure = error(output);
        assert_eq!(failure["code"], "operation_result_unknown");
        assert_eq!(failure["retryable"], false);
        assert!(failure.get("calls").is_none());
        done_tx.send(()).unwrap();
        server.await.unwrap();
        assert_eq!(puts.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn budget_cli_route_check_accepts_budget_rejection_with_finished_models_receipt() {
    let mut receipt = call("settled")["call"].clone();
    receipt["purpose"] = json!("models");
    receipt["method"] = json!("GET");
    let h=Host::new(vec![(409,json!({"error":{"code":"budget_exceeded","message":"预算不足","dispatch_state":"not_sent","calls":[receipt.clone()],"retryable":false}}))]).await;
    let failure = error(
        h.cli(&["route", "check", "route-a", "--project", "project-a"])
            .await,
    );
    assert_eq!(failure["code"], "budget_exceeded");
    assert_eq!(failure["dispatch_state"], "not_sent");
    assert_eq!(failure["calls"], json!([receipt]));
    assert_eq!(failure["retryable"], false);
    let requests = h.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "POST");
    assert_eq!(requests[0].1, "/api/routes/route-a/check");
    assert!(requests[0].3);
}
