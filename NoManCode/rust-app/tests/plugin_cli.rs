//! NEXT-10 actual CLI processes: authenticated wire, safe output and no implicit retry.
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use peachsh::{engine::Engine, store::Store};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    process::Output,
    sync::{Arc, Mutex},
    time::Duration,
};
type Request = (String, String, Value, bool, Option<String>);
#[derive(Clone)]
struct Mock {
    replies: Arc<Mutex<VecDeque<(u16, Value)>>>,
    requests: Arc<Mutex<Vec<Request>>>,
}
struct Host {
    port: u16,
    state: Mock,
    task: tokio::task::JoinHandle<()>,
    cwd: tempfile::TempDir,
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
) -> Response {
    if uri.path() == "/api/bootstrap" {
        return axum::Json(json!({"token":"plugin-cli-token"})).into_response();
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
            .is_some_and(|v| v == "plugin-cli-token"),
        headers
            .get("idempotency-key")
            .map(|v| v.to_str().unwrap().to_owned()),
    ));
    let (status, value) = mock
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("no automatic retry is permitted");
    if status == 0 {
        let stream = futures_util::stream::iter(vec![Err::<Bytes, _>(std::io::Error::other(
            "response lost",
        ))]);
        return Response::builder()
            .header("content-type", "application/json")
            .body(Body::from_stream(stream))
            .unwrap();
    }
    (StatusCode::from_u16(status).unwrap(), axum::Json(value)).into_response()
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
        Self {
            port,
            state,
            task,
            cwd: tempfile::tempdir().unwrap(),
        }
    }
    async fn cli(&self, args: &[&str]) -> Output {
        tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
                .arg("--port")
                .arg(self.port.to_string())
                .arg("--json")
                .args(args)
                .current_dir(self.cwd.path())
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
    fn no_files(&self) {
        assert_eq!(std::fs::read_dir(self.cwd.path()).unwrap().count(), 0);
    }
}
fn status() -> Value {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(
        Arc::new(Store::open(&dir.path().join("host.sqlite3")).unwrap()),
        1,
    )
    .unwrap();
    serde_json::to_value(engine.plugin_status("builtin.files").unwrap()).unwrap()
}
fn result(enabled: bool) -> Value {
    let mut current = status();
    current["desired"]["enabled"] = json!(enabled);
    current["desired"]["revision"] = json!(1);
    if !enabled {
        current["runtime"]["state"] = json!("stopped");
        current["runtime"]["admission_open"] = json!(false);
        current["runtime"]["providers"] = json!([]);
        current["runtime"]["effects_count"] = json!(0);
        current["blocked_reason"] = json!("disabled");
    }
    json!({"receipt":{"plugin_id":"builtin.files","enabled":enabled,"previous_revision":0,"revision":1,"committed_at":123},"replayed":false,"current":current})
}
fn stdout(output: &Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn failure(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).unwrap()
}
const SET: [&str; 9] = [
    "plugin",
    "set-enabled",
    "builtin.files",
    "--enabled",
    "false",
    "--expected-revision",
    "0",
    "--key",
    "original-key",
];
#[tokio::test]
async fn f09_actual_cli_uses_authenticated_get_put_and_preserves_response() {
    let s = status();
    let r = result(false);
    let h = Host::new(vec![
        (200, json!({"plugins":[s.clone()]})),
        (200, json!({"plugin":s.clone()})),
        (200, r.clone()),
    ])
    .await;
    assert_eq!(
        stdout(&h.cli(&["plugin", "list"]).await, 0),
        json!({"plugins":[s.clone()]})
    );
    assert_eq!(
        stdout(&h.cli(&["plugin", "status", "builtin.files"]).await, 0),
        json!({"plugin":s})
    );
    assert_eq!(stdout(&h.cli(&SET).await, 0), r);
    let requests = h.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|r| r.3));
    assert_eq!(requests[0].0, "GET");
    assert_eq!(requests[0].1, "/api/plugins");
    assert_eq!(requests[1].1, "/api/plugins/builtin.files");
    assert_eq!(requests[2].0, "PUT");
    assert_eq!(requests[2].1, "/api/plugins/builtin.files/enabled");
    assert_eq!(
        requests[2].2,
        json!({"enabled":false,"expected_revision":0})
    );
    assert_eq!(requests[2].4.as_deref(), Some("original-key"));
    h.no_files();
}
#[tokio::test]
async fn f09_unconverged_accepted_json_is_complete_and_exit_one() {
    let mut r = result(false);
    r["current"]["converged"] = json!(false);
    r["current"]["runtime"]["state"] = json!("failed");
    r["current"]["blocked_reason"] = json!("runtime_failed");
    let h = Host::new(vec![(200, r.clone())]).await;
    assert_eq!(stdout(&h.cli(&SET).await, 1), r);
    assert_eq!(h.state.requests.lock().unwrap().len(), 1);
    h.no_files();
}
#[tokio::test]
async fn f09_lost_response_requires_manual_same_key_and_parameters() {
    let mut r = result(false);
    r["replayed"] = json!(true);
    let h = Host::new(vec![(0, Value::Null), (200, r.clone())]).await;
    let e = failure(&h.cli(&SET).await);
    assert_eq!(e["code"], "operation_result_unknown");
    assert_eq!(e["retryable"], false);
    assert!(e["message"].as_str().unwrap().contains("原 --key"));
    assert_eq!(h.state.requests.lock().unwrap().len(), 1);
    assert_eq!(stdout(&h.cli(&SET).await, 0), r);
    let requests = h.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    h.no_files();
}
#[tokio::test]
async fn f09_cli_rejects_invalid_input_and_safe_errors_remain_structured() {
    let h = Host::new(vec![]).await;
    for args in [
        vec![
            "plugin",
            "set-enabled",
            "builtin.files",
            "--enabled",
            "maybe",
            "--expected-revision",
            "0",
            "--key",
            "x",
        ],
        vec![
            "plugin",
            "set-enabled",
            "builtin.files",
            "--enabled",
            "false",
            "--expected-revision",
            "-1",
            "--key",
            "x",
        ],
        vec![
            "plugin",
            "set-enabled",
            "builtin.files",
            "--enabled",
            "false",
            "--expected-revision",
            "0",
        ],
        vec![
            "plugin",
            "set-enabled",
            "builtin.files",
            "--enabled",
            "false",
            "--expected-revision",
            "0",
            "--key",
            "a,b",
        ],
        vec!["--data-dir", "local-db", "plugin", "list"],
    ] {
        assert_eq!(h.cli(&args).await.status.code(), Some(2));
    }
    assert!(h.state.requests.lock().unwrap().is_empty());
    h.no_files();
    for (code, status) in [
        ("plugin_invalid", 400),
        ("plugin_not_found", 404),
        ("plugin_revision_conflict", 409),
        ("plugin_idempotency_conflict", 409),
        ("plugin_unavailable", 503),
    ] {
        let h = Host::new(vec![(
            status,
            json!({"code":code,"message":"固定安全错误","error":"固定安全错误","retryable":false}),
        )])
        .await;
        let e = failure(&h.cli(&SET).await);
        assert_eq!(e["code"], code);
        assert_eq!(e["retryable"], false);
        h.no_files();
    }
}
#[tokio::test]
async fn f09_hostile_output_is_not_echoed_or_retried() {
    for payload in [
        json!({"plugin":{"token":"plugin-cli-token"}}),
        json!({"unexpected":"shape"}),
    ] {
        let h = Host::new(vec![(200, payload)]).await;
        let output = h.cli(&SET).await;
        let e = failure(&output);
        assert_eq!(e["code"], "operation_result_unknown");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("plugin-cli-token"));
        assert_eq!(h.state.requests.lock().unwrap().len(), 1);
        h.no_files();
    }
}
