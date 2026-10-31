//! A11: actual CLI/Host diagnostics and adversarial local transport responses.
use axum::{
    Json, Router,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::process::{Child, Command};

const BIN: &str = env!("CARGO_BIN_EXE_peachsh");
const TOKEN: &str = "synthetic-agent-diagnostics-token";
const PRIVATE: &str = "synthetic-agent-private-payload";
fn valid() -> Value {
    json!({"plugin_id":"builtin.chat-agent","service":"agent.strategy","interface_version":"1.0.0","scope":"process","active":true,"admission_open":true,"in_flight":0,"generation":1,"draining":false,"transitioning":false,"effects":1,"recovery":0,"last_failure":null})
}
#[derive(Clone)]
struct Reply {
    body: String,
    status: axum::http::StatusCode,
    calls: Arc<Mutex<Vec<(String, bool)>>>,
}
async fn bootstrap() -> Json<Value> {
    Json(json!({"token":TOKEN}))
}
async fn fake_status(State(state): State<Reply>, request: Request) -> Response {
    let authenticated = request
        .headers()
        .get("x-peachsh-token")
        .and_then(|v| v.to_str().ok())
        == Some(TOKEN);
    state
        .calls
        .lock()
        .unwrap()
        .push((request.uri().path().to_owned(), authenticated));
    (
        state.status,
        [("content-type", "application/json")],
        state.body,
    )
        .into_response()
}
async fn fake(
    body: String,
    status: axum::http::StatusCode,
) -> (
    u16,
    Arc<Mutex<Vec<(String, bool)>>>,
    tokio::task::JoinHandle<()>,
) {
    let calls = Arc::new(Mutex::new(vec![]));
    let app = Router::new()
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/runtime/agents/chat", get(fake_status))
        .with_state(Reply {
            body,
            status,
            calls: calls.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (port, calls, task)
}
async fn cli(port: u16, json_output: bool) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.arg("--port").arg(port.to_string());
    if json_output {
        cmd.arg("--json");
    }
    cmd.args(["runtime", "agent", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(12), cmd.output())
        .await
        .unwrap()
        .unwrap()
}
fn output_safe(out: &Output) {
    for b in [&out.stdout, &out.stderr] {
        let text = String::from_utf8_lossy(b);
        assert!(!text.contains(TOKEN));
        assert!(!text.contains(PRIVATE));
    }
}
#[tokio::test]
async fn cli_validates_closed_status_and_never_prints_hostile_data() {
    let (port, calls, server) = fake(valid().to_string(), axum::http::StatusCode::OK).await;
    for json_output in [true, false] {
        let out = cli(port, json_output).await;
        output_safe(&out);
        assert_eq!(out.status.code(), Some(0));
        assert!(out.stderr.is_empty());
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value, valid());
    }
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[
            ("/api/runtime/agents/chat".to_owned(), true),
            ("/api/runtime/agents/chat".to_owned(), true)
        ]
    );
    server.abort();
    let mut draining = valid();
    draining["admission_open"] = json!(false);
    draining["in_flight"] = json!(1);
    draining["draining"] = json!(true);
    let mut transitioning = valid();
    transitioning["active"] = json!(false);
    transitioning["admission_open"] = json!(false);
    transitioning["transitioning"] = json!(true);
    let mut closed_on_failure = valid();
    closed_on_failure["admission_open"] = json!(false);
    closed_on_failure["last_failure"] = json!("internal");
    for value in [draining, transitioning, closed_on_failure] {
        let (port, _, server) = fake(value.to_string(), axum::http::StatusCode::OK).await;
        let out = cli(port, true).await;
        output_safe(&out);
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), value);
        server.abort();
    }
    let mut invalid = Vec::new();
    for (key, value) in [
        ("plugin_id", json!(PRIVATE)),
        ("service", json!("wrong")),
        ("interface_version", json!("2.0.0")),
        ("scope", json!("persistent")),
        ("active", json!(false)),
        ("generation", json!(0)),
        ("draining", json!(true)),
        ("transitioning", json!(true)),
        ("effects", json!(0)),
        ("last_failure", json!(PRIVATE)),
        ("in_flight", json!(-1)),
    ] {
        let mut v = valid();
        v[key] = value;
        invalid.push(v.to_string());
    }
    let mut extra = valid();
    extra["extra"] = json!(PRIVATE);
    invalid.push(extra.to_string());
    let mut missing = valid();
    missing.as_object_mut().unwrap().remove("last_failure");
    invalid.push(missing.to_string());
    invalid.push(valid().to_string().replacen(
        "\"active\":true",
        "\"active\":true,\"active\":true",
        1,
    ));
    for body in invalid {
        let (port, calls, server) = fake(body, axum::http::StatusCode::OK).await;
        let out = cli(port, true).await;
        output_safe(&out);
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert!(calls.lock().unwrap()[0].1);
        server.abort();
    }
    let error = json!({"code":"agent_runtime_unavailable","message":"Agent运行态不可用","error":"Agent运行态不可用","retryable":false});
    let (port, _, server) = fake(
        error.to_string(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    let out = cli(port, true).await;
    output_safe(&out);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stderr).unwrap()["code"],
        "agent_runtime_unavailable"
    );
    server.abort();
    let error = json!({"code":"agent_runtime_unavailable","message":PRIVATE,"error":PRIVATE,"retryable":false});
    let (port, _, server) = fake(
        error.to_string(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    let out = cli(port, true).await;
    output_safe(&out);
    assert_eq!(out.status.code(), Some(1));
    server.abort();
}

async fn launch(root: &Path, port: u16) -> Child {
    Command::new(BIN)
        .arg("--port")
        .arg(port.to_string())
        .arg("--data-dir")
        .arg(root.join("data"))
        .arg("--workspace")
        .arg(root.join("workspace"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}
async fn ready(client: &reqwest::Client, origin: &str, child: &mut Child) -> String {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            assert!(child.try_wait().unwrap().is_none());
            if let Ok(response) = client.get(format!("{origin}/api/bootstrap")).send().await
                && response.status().is_success()
            {
                let v: Value = response.json().await.unwrap();
                break v["token"].as_str().unwrap().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn real_host_auth_query_method_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("workspace")).unwrap();
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let mut host = launch(dir.path(), port).await;
    let origin = format!("http://127.0.0.1:{port}");
    let path = format!("{origin}/api/runtime/agents/chat");
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let token = ready(&client, &origin, &mut host).await;
    let reply = client
        .get(&path)
        .header("x-peachsh-token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
    assert_eq!(reply.headers()["cache-control"], "no-store");
    let status: Value = reply.json().await.unwrap();
    assert_eq!(status["plugin_id"], "builtin.chat-agent");
    assert_eq!(status["service"], "agent.strategy");
    assert_eq!(status["scope"], "process");
    assert_eq!(client.get(&path).send().await.unwrap().status(), 403);
    assert_eq!(
        client
            .get(&path)
            .header("x-peachsh-token", "wrong")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(format!("{path}?unexpected=1"))
            .header("x-peachsh-token", &token)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .get(&path)
            .header("host", "evil.invalid")
            .header("x-peachsh-token", &token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(&path)
            .header("origin", "http://evil.invalid")
            .header("x-peachsh-token", &token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(&path)
            .header("sec-fetch-site", "cross-site")
            .header("x-peachsh-token", &token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(&path)
            .header("x-peachsh-token", &token)
            .send()
            .await
            .unwrap()
            .status(),
        405
    );
    assert_eq!(
        client
            .head(&path)
            .header("x-peachsh-token", &token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let out = cli(port, true).await;
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&token));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(&token));
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        status
    );
    let pid = host.id().unwrap();
    host.kill().await.unwrap();
    assert!(!host.wait().await.unwrap().success());
    println!("agent diagnostic Host kill+wait pid={pid}");
    let mut host = launch(dir.path(), port).await;
    let new_token = ready(&client, &origin, &mut host).await;
    assert_ne!(token, new_token);
    assert_eq!(
        client
            .get(&path)
            .header("x-peachsh-token", &token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let out = cli(port, true).await;
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&new_token));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(&new_token));
    assert_eq!(out.status.code(), Some(0));
    let restarted: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(restarted["scope"], "process");
    assert_eq!(restarted["admission_open"], true);
    let pid = host.id().unwrap();
    host.kill().await.unwrap();
    assert!(!host.wait().await.unwrap().success());
    println!("agent diagnostic Host kill+wait pid={pid}");
}
