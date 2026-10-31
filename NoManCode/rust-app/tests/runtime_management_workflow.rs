//! M13: persistent runtime controls through a real Host and CLI.
#![cfg(windows)]
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    convert::Infallible,
    path::Path,
    process::{Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
    sync::Notify,
};

const BIN: &str = env!("CARGO_BIN_EXE_peachsh");
const SECRET: &str = "synthetic-first-user-key";
#[derive(Clone)]
struct Script {
    replies: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    models: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
async fn models(State(script): State<Script>) -> Json<Value> {
    script.models.fetch_add(1, Ordering::SeqCst);
    Json(json!({"data":[{"id":"mock-model"}]}))
}
async fn provider(
    State(script): State<Script>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some(format!("Bearer {SECRET}").as_str())
    );
    assert_eq!(body["model"], "mock-model");
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
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":if hold {Value::Null} else {json!("stop")}}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}});
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
async fn cli(port: u16, cwd: &Path, args: &[&str], input: Option<&[u8]>) -> Output {
    let mut child = Command::new(BIN)
        .arg("--port")
        .arg(port.to_string())
        .arg("--json")
        .args(args)
        .current_dir(cwd)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    if let Some(input) = input {
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(input).await.unwrap();
        drop(pipe);
    }
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains(SECRET));
    }
    output
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
struct Host {
    dir: tempfile::TempDir,
    port: u16,
    child: Child,
    script: Script,
    provider: tokio::task::JoinHandle<()>,
    token: String,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.provider.abort();
    }
}
impl Host {
    fn cwd(&self) -> std::path::PathBuf {
        self.dir.path().join("client")
    }
    async fn command(&self, args: &[&str]) -> Output {
        let output = cli(self.port, &self.cwd(), args, None).await;
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&self.token));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&self.token));
        output
    }
    async fn input(&self, args: &[&str], input: &[u8]) -> Value {
        let output = cli(self.port, &self.cwd(), args, Some(input)).await;
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&self.token));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&self.token));
        stdout(&output)
    }
    async fn ok(&self, args: &[&str]) -> Value {
        stdout(&self.command(args).await)
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
        let pid = self.child.id().unwrap();
        self.child.kill().await.unwrap();
        assert!(!self.child.wait().await.unwrap().success());
        println!("P07 Host kill+wait pid={pid}");
    }
    async fn restart(&mut self) {
        self.child = launch(self.dir.path(), self.port).await;
        let new_token = bootstrap(self.port).await;
        assert_ne!(new_token, self.token);
        self.token = new_token;
    }
}
async fn launch(root: &Path, port: u16) -> Child {
    let mut child = Command::new(BIN)
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
async fn bootstrap(port: u16) -> String {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response: Value = client
        .get(format!("http://127.0.0.1:{port}/api/bootstrap"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    response["token"].as_str().unwrap().to_owned()
}

async fn host(replies: Vec<Value>) -> Host {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::create_dir(dir.path().join("client")).unwrap();
    std::fs::write(workspace.join("src/a.txt"), "before needle\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "src/a.txt"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "core.hooksPath=",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&workspace)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(!dir.path().join("data/peachsh.sqlite3").exists());
    let script = Script {
        replies: Arc::new(Mutex::new(replies.into())),
        requests: Arc::new(Mutex::new(vec![])),
        models: Arc::new(AtomicUsize::new(0)),
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(provider))
        .route("/v1/models", axum::routing::get(models))
        .with_state(script.clone());
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let child = launch(dir.path(), port).await;
    let token = bootstrap(port).await;
    let h = Host {
        dir,
        port,
        child,
        script,
        provider,
        token,
    };
    assert_eq!(
        h.ok(&["host", "config", "get"]).await,
        json!({"revision":0,"routes":[]})
    );
    assert!(h.script.requests.lock().unwrap().is_empty());
    let configured = h
        .input(
            &[
                "host",
                "route",
                "set",
                "local",
                "--expected-revision",
                "0",
                "--name",
                "Local",
                "--base-url",
                &base,
                "--model",
                "mock-model",
                "--max-tokens",
                "128",
                "--parallel-limit",
                "1",
                "--key",
                "setup-route",
                "--credential-stdin",
            ],
            format!("{SECRET}\n").as_bytes(),
        )
        .await;
    assert_eq!(configured["receipt"]["revision"], 1);
    assert_eq!(configured["replayed"], false);
    let receipt = h
        .ok(&["host", "config", "operation", "--key", "setup-route"])
        .await;
    assert_eq!(receipt["receipt"], configured["receipt"]);
    assert_eq!(receipt["replayed"], true);
    assert_eq!(h.ok(&["host", "config", "get"]).await["revision"], 1);
    assert!(h.script.requests.lock().unwrap().is_empty());
    let project = h
        .ok(&[
            "project",
            "create",
            "--name",
            "First user",
            "--root",
            workspace.to_str().unwrap(),
            "--key",
            "setup-project",
        ])
        .await;
    let project_id = project["id"].as_str().unwrap();
    let initial = h.ok(&["project", "config", "get", project_id]).await;
    assert_eq!(initial["config"]["tools"], false);
    assert_eq!(initial["config"]["allow_commands"], false);
    assert_eq!(initial["config"]["write_scopes"], json!([]));
    let config = h.dir.path().join("project-config.json");
    std::fs::write(&config, serde_json::to_vec(&json!({"allowed_route_ids":["local"],"default_route_id":"local","tools":true,"write_scopes":["src"],"allow_commands":true})).unwrap()).unwrap();
    let project_revision = initial["revision"].to_string();
    h.ok(&[
        "project",
        "config",
        "set",
        project_id,
        "--expected-revision",
        &project_revision,
        "--file",
        config.to_str().unwrap(),
    ])
    .await;
    let current = h.ok(&["project", "current"]).await;
    assert!(current["project_id"].is_null());
    let selection_revision = current["revision"].to_string();
    h.ok(&[
        "project",
        "select",
        project_id,
        "--expected-revision",
        &selection_revision,
    ])
    .await;
    let budget = h
        .ok(&["project", "budget", "config", "get", "--id", project_id])
        .await;
    assert_eq!(budget["revision"], 0);
    h.ok(&[
        "project",
        "budget",
        "config",
        "set",
        "--id",
        project_id,
        "--expected-revision",
        "0",
        "--limit-tokens",
        "10000",
        "--per-call-reservation-tokens",
        "128",
        "--key",
        "setup-budget",
    ])
    .await;
    let preview = h
        .ok(&[
            "network",
            "preview",
            "--route",
            "local",
            "--project",
            project_id,
        ])
        .await;
    h.ok(&[
        "network",
        "authorize",
        "--route",
        "local",
        "--project",
        project_id,
        "--config-revision",
        &preview["config_revision"].to_string(),
        "--credential-revision",
        &preview["credential_revision"].to_string(),
        "--binding",
        preview["binding_digest"].as_str().unwrap(),
        "--purpose",
        "completion",
        "--purpose",
        "models",
        "--key",
        "setup-network",
    ])
    .await;
    assert!(h.script.requests.lock().unwrap().is_empty());
    h
}

async fn set(h: &Host, id: &str, enabled: &str, revision: &str, key: &str) -> Value {
    h.ok(&[
        "runtime",
        "component",
        "set-enabled",
        id,
        "--enabled",
        enabled,
        "--expected-revision",
        revision,
        "--key",
        key,
    ])
    .await
}

#[tokio::test]
async fn empty_database_disable_restart_enable_and_completion() {
    let mut h = host(vec![json!({"content":"no tools completion"})]).await;
    let agent = "builtin.chat-agent";
    let provider_id = "builtin.provider-xpeach";
    let initial = h.ok(&["runtime", "component", "list"]).await;
    assert_eq!(initial["components"].as_array().unwrap().len(), 2);
    for id in [agent, provider_id] {
        let state = h.ok(&["runtime", "component", "status", id]).await;
        assert_eq!(state["desired"]["enabled"], true);
        assert_eq!(state["desired"]["revision"], 0);
    }
    let old_plugins = h.ok(&["plugin", "list"]).await;
    assert_eq!(old_plugins["plugins"].as_array().unwrap().len(), 2);
    let agent_off = set(&h, agent, "false", "0", "disable-agent").await;
    let provider_off = set(&h, provider_id, "false", "0", "disable-provider").await;
    assert_eq!(agent_off["receipt"]["revision"], 1);
    assert_eq!(provider_off["receipt"]["revision"], 1);
    h.stop().await;
    h.restart().await;
    for id in [agent, provider_id] {
        let state = h.ok(&["runtime", "component", "status", id]).await;
        assert_eq!(state["desired"]["enabled"], false);
        assert_eq!(state["desired"]["revision"], 1);
        assert_eq!(state["runtime"]["active"], false);
        assert_eq!(state["runtime"]["effects"], 0);
        assert_eq!(state["converged"], true);
    }
    assert!(
        h.ok(&["runtime", "agent", "status"]).await["generation"]
            .as_u64()
            .unwrap()
            > 0
    );
    let plugins_after = h.ok(&["plugin", "list"]).await;
    assert_eq!(plugins_after["plugins"].as_array().unwrap().len(), 2);
    for i in 0..2 {
        assert_eq!(
            plugins_after["plugins"][i]["plugin_id"],
            old_plugins["plugins"][i]["plugin_id"]
        );
    }
    set(&h, agent, "true", "1", "enable-agent").await;
    set(&h, provider_id, "true", "1", "enable-provider").await;
    let replay = set(&h, agent, "false", "0", "disable-agent").await;
    assert_eq!(replay["receipt"], agent_off["receipt"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["current"]["desired"]["enabled"], true);
    for id in ["builtin.files", "builtin.shell"] {
        h.ok(&[
            "plugin",
            "set-enabled",
            id,
            "--enabled",
            "false",
            "--expected-revision",
            "0",
            "--key",
            if id == "builtin.files" {
                "files-off"
            } else {
                "shell-off"
            },
        ])
        .await;
    }
    let run = h
        .ok(&[
            "run",
            "start",
            "--title",
            "Runtime management",
            "--route",
            "local",
            "--message",
            "Say hello",
            "--key",
            "runtime-story",
        ])
        .await;
    let run_id = run["run_id"].as_str().unwrap();
    let task_id = run["tasks"][0]["id"].as_str().unwrap();
    h.done(run_id, task_id).await;
    assert_eq!(h.script.requests.lock().unwrap().len(), 1);
    h.stop().await;
}

#[tokio::test]
async fn disabled_completion_preserves_completed_models_receipt() {
    let mut h = host(vec![]).await;
    let project = h.ok(&["project", "current"]).await;
    let project_id = project["project_id"].as_str().unwrap();
    let before = h
        .ok(&["project", "budget", "status", "--id", project_id])
        .await;
    set(
        &h,
        "builtin.provider-xpeach",
        "false",
        "0",
        "provider-for-check",
    )
    .await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .post(format!(
            "http://127.0.0.1:{}/api/routes/local/check",
            h.port
        ))
        .header("x-peachsh-token", &h.token)
        .json(&json!({"project_id":project_id}))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    let body: Value = response.json().await.unwrap();
    let calls = body["error"]["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["purpose"], "models");
    assert_eq!(calls[0]["state"], "finished");
    assert_eq!(h.script.models.load(Ordering::SeqCst), 1);
    assert!(h.script.requests.lock().unwrap().is_empty());
    assert_eq!(
        h.ok(&["project", "budget", "status", "--id", project_id])
            .await,
        before
    );
    h.stop().await;
}
