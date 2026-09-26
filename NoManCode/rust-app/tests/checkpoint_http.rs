use axum::{
    Json, Router,
    body::Body,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};
use peachsh::{
    domain::{Route, Settings},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    convert::Infallible,
    path::Path,
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::Notify,
};

const BIN: &str = env!("CARGO_BIN_EXE_peachsh");
#[derive(Clone)]
struct Script {
    replies: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
async fn provider(State(script): State<Script>, Json(body): Json<Value>) -> Response {
    script.requests.lock().unwrap().push(body);
    let reply = script
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected provider execution");
    let hold = reply.get("hold") == Some(&json!(true));
    let delta = if hold {
        json!({"content":"working"})
    } else {
        reply
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":if hold {Value::Null} else {json!("stop")}}]});
    let stream = async_stream::stream! {
        yield Ok::<_,Infallible>(format!("data: {chunk}\n\n").into_bytes());
        if hold { script.entered.notify_one(); script.release.notified().await; }
        yield Ok(b"data: [DONE]\n\n".to_vec());
    };
    (
        [("content-type", "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}
fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
}
fn write_call(id: &str, path: &str, content: &str) -> Value {
    call(id, "write_file", json!({"path":path,"content":content}))
}
async fn cli(port: u16, cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(BIN);
    command
        .arg("--port")
        .arg(port.to_string())
        .arg("--json")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(12), command.output())
        .await
        .expect("CLI bounded completion")
        .unwrap()
}
fn stdout(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn failure(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    serde_json::from_slice(&output.stderr).unwrap()
}
struct Host {
    dir: tempfile::TempDir,
    port: u16,
    child: Child,
    script: Script,
    provider: tokio::task::JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.provider.abort();
    }
}
impl Host {
    fn workspace(&self) -> std::path::PathBuf {
        self.dir.path().join("workspace")
    }
    fn db(&self) -> std::path::PathBuf {
        self.dir.path().join("data/peachsh.sqlite3")
    }
    fn cwd(&self) -> std::path::PathBuf {
        self.dir.path().join("client")
    }
    async fn command(&self, args: &[&str]) -> Output {
        cli(self.port, &self.cwd(), args).await
    }
    async fn ok(&self, args: &[&str]) -> Value {
        stdout(&self.command(args).await)
    }
    async fn context(&self, run: &str) -> Value {
        self.ok(&["run", "context", run]).await
    }
    async fn pending(&self, task: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let list = self.ok(&["task", "approvals", task]).await;
                if let Some(card) = list["approvals"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|card| card["status"] == "pending")
                {
                    return card.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    async fn done(&self, run: &str, task: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let value = self.ok(&["run", "status", run]).await;
                let item = value["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["id"] == task)
                    .unwrap();
                if item["status"] == "completed" {
                    return item.clone();
                }
                assert!(
                    !matches!(
                        item["status"].as_str(),
                        Some("failed" | "cancelled" | "interrupted")
                    ),
                    "{item}"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    async fn stop(&mut self) {
        self.child.kill().await.unwrap();
        assert!(!self.child.wait().await.unwrap().success());
    }
    async fn restart(&mut self) {
        self.child = launch(self.dir.path(), self.port).await;
    }
}
async fn launch(root: &Path, port: u16) -> Child {
    let mut child = Command::new(BIN)
        .arg("--port")
        .arg(port.to_string())
        .arg("--data-dir")
        .arg(root.join("data"))
        .arg("--legacy-data")
        .arg(root.join("empty-legacy"))
        .arg("--workspace")
        .arg(root.join("workspace"))
        .env("NHC_WORKFLOW_TEST_KEY", "workflow-mock-key")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "Host exited before readiness"
            );
            if let Ok(response) = client
                .get(format!("http://127.0.0.1:{port}/api/health"))
                .send()
                .await
                && response.status().is_success()
                && response.json::<Value>().await.is_ok()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    child
}
async fn host(replies: Vec<Value>) -> Host {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("workspace/src")).unwrap();
    std::fs::create_dir(dir.path().join("client")).unwrap();
    std::fs::create_dir(dir.path().join("data")).unwrap();
    let script = Script {
        replies: Arc::new(Mutex::new(replies.into())),
        requests: Arc::new(Mutex::new(vec![])),
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(provider))
        .with_state(script.clone());
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    {
        let store = Store::open(&dir.path().join("data/peachsh.sqlite3")).unwrap();
        store
            .save_settings(&Settings {
                workspace: dir.path().join("workspace").to_string_lossy().into(),
                max_concurrency: 2,
                routes: vec![Route {
                    id: "local".into(),
                    name: "Local mock".into(),
                    base_url: base,
                    model: "mock".into(),
                    max_tokens: 128,
                    parallel_limit: 2,
                    key_env: Some("NHC_WORKFLOW_TEST_KEY".into()),
                }],
                newapi: None,
            })
            .unwrap();
    }
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let child = launch(dir.path(), port).await;
    Host {
        dir,
        port,
        child,
        script,
        provider,
    }
}
async fn start(h: &Host, key: &str, tools: bool, commands: bool) -> Value {
    let mut args = vec![
        "run",
        "start",
        "--title",
        "CLI workflow",
        "--route",
        "local",
        "--message",
        "perform requested work",
        "--key",
        key,
    ];
    if tools {
        args.extend(["--tools", "--scope", "src"]);
    }
    if commands {
        args.push("--allow-commands");
    }
    h.ok(&args).await
}

fn snapshot(h: &Host) -> Vec<(String, i64)> {
    let db = rusqlite::Connection::open(h.db()).unwrap();
    [
        "runs",
        "tasks",
        "events",
        "approvals",
        "workspace_changes",
        "workspace_restores",
        "checkpoints",
    ]
    .into_iter()
    .map(|table| {
        (
            table.into(),
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
        )
    })
    .collect()
}
async fn auth(h: &Host) -> (reqwest::Client, String, String) {
    let c = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let origin = format!("http://127.0.0.1:{}", h.port);
    let token = c
        .get(format!("{origin}/api/bootstrap"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    (c, origin, token)
}
async fn assert_error(response: reqwest::Response, status: u16, code: &str) -> Value {
    assert_eq!(response.status().as_u16(), status);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let value = response.json::<Value>().await.unwrap();
    assert_eq!(value["code"], code);
    assert_eq!(value["retryable"], false);
    assert!(value["message"].is_string());
    value
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t02_checkpoint_http_strict_security_zero_effects() {
    let mut h = host(vec![json!({"content":"done"})]).await;
    let run = start(&h, "http-start", false, false).await;
    let run_id = run["run_id"].as_str().unwrap();
    let task = run["tasks"][0]["id"].as_str().unwrap();
    h.done(run_id, task).await;
    let (c, origin, token) = auth(&h).await;
    let endpoints = [
        format!("/api/tasks/{task}/git-diff"),
        format!("/api/tasks/{task}/checkpoints"),
        "/api/checkpoints/nonexistent/restore".into(),
    ];
    for path in &endpoints {
        for mode in [
            "no-token",
            "bad-token",
            "bad-host",
            "bad-origin",
            "bad-json",
            "unknown-field",
            "bad-type",
            "large",
        ] {
            let before = snapshot(&h);
            let requests = h.script.requests.lock().unwrap().len();
            let mut request = c
                .post(format!("{origin}{path}"))
                .header("idempotency-key", "http-key");
            if mode != "no-token" {
                request = request.header(
                    "x-peachsh-token",
                    if mode == "bad-token" {
                        "incorrect"
                    } else {
                        &token
                    },
                );
            }
            if mode == "bad-host" {
                request = request.header("host", "example.invalid");
            }
            if mode == "bad-origin" {
                request = request.header("origin", "https://example.invalid");
            }
            let body = match mode {
                "bad-json" => "{".to_owned(),
                "unknown-field" => "{\"unexpected\":1}".into(),
                "bad-type" => "[]".into(),
                "large" => "x".repeat(1048577),
                _ => {
                    if path.ends_with("git-diff") {
                        json!({"path":"src/a.txt","view":"head"}).to_string()
                    } else {
                        "{}".into()
                    }
                }
            };
            let status = match mode {
                "no-token" | "bad-token" | "bad-host" | "bad-origin" => 403,
                "large" => 413,
                _ => 400,
            };
            let code = if status == 403 {
                "forbidden"
            } else {
                "request_failed"
            };
            assert_error(
                request
                    .header("content-type", "application/json")
                    .body(body)
                    .send()
                    .await
                    .unwrap(),
                status,
                code,
            )
            .await;
            assert_eq!(snapshot(&h), before, "{path} {mode}");
            assert_eq!(h.script.requests.lock().unwrap().len(), requests);
        }
        let before = snapshot(&h);
        assert_error(
            c.post(format!("{origin}{path}"))
                .header("x-peachsh-token", &token)
                .body("{}")
                .send()
                .await
                .unwrap(),
            415,
            "request_failed",
        )
        .await;
        assert_eq!(snapshot(&h), before);
    }
    let create = format!("{origin}/api/tasks/{task}/checkpoints");
    for key in [None, Some("bad,key"), Some("bad key")] {
        let before = snapshot(&h);
        let mut request = c
            .post(&create)
            .header("x-peachsh-token", &token)
            .json(&json!({}));
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        assert_error(request.send().await.unwrap(), 400, "request_failed").await;
        assert_eq!(snapshot(&h), before);
    }
    let before = snapshot(&h);
    assert_error(
        c.post(&create)
            .header("x-peachsh-token", &token)
            .header("idempotency-key", "a")
            .header("idempotency-key", "b")
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
        400,
        "request_failed",
    )
    .await;
    assert_eq!(snapshot(&h), before);
    assert_error(
        c.post(&create)
            .header("x-peachsh-token", &token)
            .header("idempotency-key", "valid")
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
        409,
        "conflict",
    )
    .await;
    assert_error(
        c.get(format!("{origin}/api/checkpoints/nonexistent"))
            .send()
            .await
            .unwrap(),
        404,
        "not_found",
    )
    .await;
    assert_error(
        c.post(format!("{origin}/api/tasks/nonexistent/checkpoints"))
            .header("x-peachsh-token", &token)
            .header("idempotency-key", "valid")
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
        404,
        "not_found",
    )
    .await;
    assert_error(
        c.post(format!("{origin}/api/tasks/{task}/git-diff"))
            .header("x-peachsh-token", &token)
            .json(&json!({"path":"src/a.txt","view":"head"}))
            .send()
            .await
            .unwrap(),
        409,
        "conflict",
    )
    .await;
    for path in ["../outside", ".git/config"] {
        assert_error(
            c.post(format!("{origin}/api/tasks/{task}/git-diff"))
                .header("x-peachsh-token", &token)
                .json(&json!({"path":path,"view":"head"}))
                .send()
                .await
                .unwrap(),
            400,
            "request_failed",
        )
        .await;
    }
    assert_eq!(snapshot(&h), before);
    h.stop().await;
}
