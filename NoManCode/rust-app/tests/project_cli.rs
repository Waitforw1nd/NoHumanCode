//! NEXT-08: real CLI processes validate project wire requests and hostile responses.
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
    Router,
};
use serde_json::{json, Value};
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
        uri.to_string(),
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

fn project() -> Value {
    json!({"id":"project-a","name":"A","root_path":"C:/fixture","created_at":1,"updated_at":1})
}
fn config() -> Value {
    json!({"allowed_route_ids":["route-a"],"default_route_id":"route-a","tools":false,"write_scopes":[],"allow_commands":false})
}
fn snapshot(revision: u64) -> Value {
    json!({"project_id":"project-a","revision":revision,"config":config(),"permission_digest":"a".repeat(64)})
}
#[tokio::test]
async fn project_cli_management_uses_token_put_cas_and_nested_config() {
    let h = Host::new(vec![
        (201, project()),
        (200, json!([project()])),
        (200, project()),
        (200, json!({"project_id":null,"revision":0})),
        (200, json!({"project_id":"project-a","revision":1})),
        (200, snapshot(2)),
        (200, snapshot(3)),
    ])
    .await;
    ok(h.cli(&[
        "project",
        "create",
        "--name",
        "A",
        "--root",
        "C:/fixture",
        "--key",
        "register-a",
    ])
    .await);
    ok(h.cli(&["project", "list"]).await);
    ok(h.cli(&["project", "get", "project-a"]).await);
    ok(h.cli(&["project", "current"]).await);
    ok(
        h.cli(&["project", "select", "project-a", "--expected-revision", "0"])
            .await,
    );
    ok(h.cli(&["project", "config", "get", "project-a"]).await);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.json");
    std::fs::write(&file, serde_json::to_vec(&config()).unwrap()).unwrap();
    ok(h.cli(&[
        "project",
        "config",
        "set",
        "project-a",
        "--expected-revision",
        "2",
        "--file",
        file.to_str().unwrap(),
    ])
    .await);
    let requests = h.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 7);
    assert!(requests.iter().all(|r| r.3));
    assert_eq!(requests[0].0, "POST");
    assert_eq!(requests[0].4.as_deref(), Some("register-a"));
    assert_eq!(requests[4].0, "PUT");
    assert_eq!(
        requests[4].2,
        json!({"project_id":"project-a","expected_revision":0})
    );
    assert_eq!(requests[6].0, "PUT");
    assert_eq!(
        requests[6].2,
        json!({"expected_revision":2,"config":config()})
    );
}
#[tokio::test]
async fn project_cli_conflicts_and_forged_bindings_do_not_retry() {
    let h=Host::new(vec![(409,json!({"code":"project_revision_conflict","message":"conflict","error":"conflict","retryable":false})),(200,json!({"project_id":"project-b","revision":2})),(200,snapshot(2)),(200,json!([]))]).await;
    assert_eq!(
        error(
            h.cli(&["project", "select", "project-a", "--expected-revision", "1"])
                .await
        )["code"],
        "project_revision_conflict"
    );
    assert_eq!(
        error(
            h.cli(&["project", "select", "project-a", "--expected-revision", "1"])
                .await
        )["code"],
        "internal"
    );
    assert_eq!(
        error(h.cli(&["project", "config", "get", "project-b"]).await)["code"],
        "internal"
    );
    ok(h.cli(&["run", "list", "--project", "project-a"]).await);
    let requests = h.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[3].1, "/api/runs?project_id=project-a");
}
#[tokio::test]
async fn project_cli_config_arrays_missing_fields_duplicates_rejected_locally() {
    let h = Host::new(vec![]).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.json");
    for raw in [
        r#"[["route-a"],"route-a",false,[],false]"#,
        r#"{"allowed_route_ids":[],"tools":false,"write_scopes":[],"allow_commands":false}"#,
        r#"{"allowed_route_ids":[],"default_route_id":null,"tools":false,"tools":true,"write_scopes":[],"allow_commands":false}"#,
    ] {
        std::fs::write(&file, raw).unwrap();
        assert_eq!(
            error(
                h.cli(&[
                    "project",
                    "config",
                    "set",
                    "project-a",
                    "--expected-revision",
                    "1",
                    "--file",
                    file.to_str().unwrap()
                ])
                .await
            )["code"],
            "request_failed"
        );
    }
    assert!(h.state.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn project_cli_network_project_rejection_preserves_finished_call_receipt() {
    let call = json!({"call_id":"call-a","authorization_id":"grant-a","subject":{"kind":"route","id":"route-a","project_id":"project-a"},"task_id":null,"purpose":"models","method":"GET","origin":"http://127.0.0.1:9001","endpoint_digest":"a".repeat(64),"state":"finished","claimed_at":1,"finished_at":2,"success":true,"http_status":200,"result":"success"});
    let h=Host::new(vec![(409,json!({"error":{"code":"project_permission_revoked","message":"revoked","dispatch_state":"not_sent","calls":[call.clone()],"retryable":false}}))]).await;
    let result = error(
        h.cli(&["route", "check", "route-a", "--project", "project-a"])
            .await,
    );
    assert_eq!(result["code"], "project_permission_revoked");
    assert_eq!(result["calls"][0], call);
    let requests = h.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].2, json!({"project_id":"project-a"}));
}
