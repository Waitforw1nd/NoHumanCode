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

#[derive(Clone)]
struct Mock {
    replies: Arc<Mutex<VecDeque<(u16, Value)>>>,
    requests: Arc<Mutex<Vec<(String, String, Value, bool, Option<String>)>>>,
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
fn preview(base: &str, origin: &str) -> Value {
    json!({"subject":{"kind":"route","id":"route-a"},"config_revision":1,"credential_revision":2,"origin":origin,"base_url":base,"credential_ref":"route::route-a","binding_digest":"a".repeat(64),"purposes":["models","completion"],"authorization":null})
}
fn authorization() -> Value {
    json!({"authorization_id":"network-auth-a","subject":{"kind":"route","id":"route-a"},"config_revision":1,"credential_revision":2,"binding_digest":"a".repeat(64),"purposes":["models","completion"],"state":"active","created_at":1})
}
fn call(state: &str) -> Value {
    json!({"call_id":"network-call-a","authorization_id":"network-auth-a","subject":{"kind":"route","id":"route-a"},"task_id":null,"purpose":"models","method":"GET","origin":"https://example.com:443","endpoint_digest":"b".repeat(64),"state":state,"claimed_at":2,"finished_at":if state=="finished"{json!(3)}else{Value::Null},"success":if state=="finished"{json!(true)}else{Value::Null},"http_status":if state=="finished"{json!(200)}else{Value::Null},"result":if state=="finished"{json!("success")}else{json!("transport_unknown")}})
}
#[tokio::test]
async fn network_cli_default_ports_ipv6_and_case_follow_host_target_contract() {
    let variants = [
        ("http://EXAMPLE.COM/v1", "http://example.com:80"),
        ("https://EXAMPLE.COM:443/v1", "https://example.com:443"),
        ("HTTP://[::1]:80/v1", "http://[::1]:80"),
        ("https://[2001:db8::1]/v1", "https://[2001:db8::1]:443"),
        ("http://example.com:18080/v1", "http://example.com:18080"),
    ];
    let mut replies: Vec<_> = variants
        .iter()
        .map(|(base, origin)| (200, preview(base, origin)))
        .collect();
    replies.push((
        200,
        preview("https://example.com/v1", "https://other.example:443"),
    ));
    let host = Host::new(replies).await;
    for (_, origin) in variants {
        assert_eq!(
            ok(host
                .cli(&["network", "preview", "--route", "route-a"])
                .await)["origin"],
            origin
        );
    }
    assert_eq!(
        error(
            host.cli(&["network", "preview", "--route", "route-a"])
                .await
        )["code"],
        "internal"
    );
    let requests = host.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert!(
        requests
            .iter()
            .all(|r| r.0 == "POST" && r.1 == "/api/network/preview" && r.3)
    );
}
#[tokio::test]
async fn network_cli_authorize_uses_only_supplied_snapshot_and_deduplicates_purposes() {
    let host = Host::new(vec![(
        201,
        json!({"authorization":authorization(),"replayed":false}),
    )])
    .await;
    let result = ok(host
        .cli(&[
            "network",
            "authorize",
            "--route",
            "route-a",
            "--config-revision",
            "1",
            "--credential-revision",
            "2",
            "--binding",
            &"a".repeat(64),
            "--purpose",
            "completion",
            "--purpose",
            "models",
            "--purpose",
            "models",
            "--key",
            "authorization-key",
        ])
        .await);
    assert_eq!(
        result["authorization"]["authorization_id"],
        "network-auth-a"
    );
    let requests = host.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, "/api/network/authorizations");
    assert_eq!(requests[0].2["purposes"], json!(["completion", "models"]));
    assert_eq!(requests[0].4.as_deref(), Some("authorization-key"));
    assert!(requests[0].3);
}
#[tokio::test]
async fn network_cli_preserves_partial_finished_and_unknown_receipts_without_retry() {
    let finished = call("finished");
    let unknown = call("unknown");
    let host=Host::new(vec![(409,json!({"error":{"code":"stale","message":"网络授权已变化","dispatch_state":"not_sent","calls":[finished.clone()],"retryable":false}})),(409,json!({"error":{"code":"unknown","message":"网络调用结果未知","dispatch_state":"unknown","calls":[unknown.clone()],"retryable":false}}))]).await;
    let first = error(host.cli(&["route", "check", "route-a"]).await);
    assert_eq!(first["dispatch_state"], "not_sent");
    assert_eq!(first["calls"], json!([finished]));
    let second = error(host.cli(&["route", "models", "route-a"]).await);
    assert_eq!(second["dispatch_state"], "unknown");
    assert_eq!(second["calls"], json!([unknown]));
    let requests = host.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| r.0 == "POST" && r.2 == json!({}) && r.3)
    );
}
#[tokio::test]
async fn network_cli_authenticated_get_guard_failure_stays_read_only_and_rejects_token_echo() {
    let host=Host::new(vec![(403,json!({"code":"forbidden","message":"token required","error":"token required","retryable":false})),(200,json!({"local-test-token":"leak"}))]).await;
    let first = error(host.cli(&["network", "get", "network-auth-a"]).await);
    assert_eq!(first["code"], "forbidden");
    assert!(first.get("calls").is_none());
    let second = error(host.cli(&["network", "call", "network-call-a"]).await);
    assert_eq!(second["code"], "internal");
    assert!(!second.to_string().contains("local-test-token"));
    let requests = host.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| r.0 == "GET" && r.2.is_null() && r.3)
    );
}
#[tokio::test]
async fn network_cli_models_success_and_task_call_binding_are_validated() {
    let mut task_call = call("finished");
    task_call["task_id"] = json!("task-a");
    let host = Host::new(vec![
        (
            200,
            json!({"result":["mock-model"],"calls":[call("finished")]}),
        ),
        (200, json!({"calls":[task_call.clone()]})),
        (200, json!({"calls":[task_call]})),
    ])
    .await;
    assert_eq!(
        ok(host.cli(&["route", "models", "route-a"]).await)["result"],
        json!(["mock-model"])
    );
    assert_eq!(
        ok(host.cli(&["task", "network-calls", "task-a"]).await)["calls"][0]["task_id"],
        "task-a"
    );
    assert_eq!(
        error(host.cli(&["task", "network-calls", "task-b"]).await)["code"],
        "internal"
    );
}

#[tokio::test]
async fn network_cli_closed_stdout_keeps_known_call_receipt_on_stderr_once() {
    let receipt = call("finished");
    let host = Host::new(vec![(
        200,
        json!({"result":["mock-model"],"calls":[receipt.clone()]}),
    )])
    .await;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
        .arg("--port")
        .arg(host.port.to_string())
        .args(["--json", "route", "models", "route-a"])
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
    assert_eq!(failure["code"], "output_failed");
    assert_eq!(failure["dispatch_state"], "finished");
    assert_eq!(failure["calls"], json!([receipt]));
    let requests = host.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, "/api/routes/route-a/models");
}
