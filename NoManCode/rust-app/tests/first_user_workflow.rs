//! P07: one real Host/CLI user story, starting with an absent database.
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
    sync::{Arc, Mutex},
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
    entered: Arc<Notify>,
    release: Arc<Notify>,
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
fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
}
fn write_call(id: &str, path: &str, content: &str) -> Value {
    call(id, "write_file", json!({"path":path,"content":content}))
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
    token: String,
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
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(provider))
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
        "--key",
        "setup-network",
    ])
    .await;
    assert!(h.script.requests.lock().unwrap().is_empty());
    h
}

async fn start(h: &Host, key: &str) -> Value {
    h.ok(&[
        "run",
        "start",
        "--title",
        "First user workflow",
        "--route",
        "local",
        "--message",
        "perform requested work",
        "--key",
        key,
        "--tools",
        "--scope",
        "src",
        "--allow-commands",
    ])
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_database_public_cli_development_restore_and_restart() {
    let mut h = host(vec![
        call("read-1", "read_file", json!({"path":"src/a.txt"})),
        call("list-1", "list_files", json!({"path":"src"})),
        call("search-1", "search_files", json!({"path":"src","query":"needle"})),
        write_call("deny-1", "src/denied.txt", "denied"),
        write_call("write-1", "src/a.txt", "after needle\n"),
        call("command-1", "run_command", json!({"command":"if ((Get-Content -Raw 'src/a.txt') -ne \"after needle`n\") { exit 7 }; Add-Content -LiteralPath 'shell-effect.txt' -Value 'once' -ErrorAction Stop"})),
        json!({"content":"first complete"}),
        json!({"content":"continued after restore"}),
        json!({"content":"continued after restart"}),
    ]).await;
    let route_receipt = h
        .ok(&["host", "config", "operation", "--key", "setup-route"])
        .await;
    let project = h.ok(&["project", "current"]).await;
    let project_id = project["project_id"].as_str().unwrap();
    let run = start(&h, "workflow-start").await;
    let run_id = run["run_id"].as_str().unwrap();
    let task = run["tasks"][0]["id"].as_str().unwrap();
    let context = h.context(run_id).await;
    let session = context["session"]["id"].as_str().unwrap();
    let agent = context["tasks"][0]["agent_id"].as_str().unwrap();
    for (call_id, decision) in [
        ("deny-1", "deny"),
        ("write-1", "approve"),
        ("command-1", "approve"),
    ] {
        let card = h.pending(task).await;
        assert_eq!(card["tool_call_id"], call_id);
        assert_eq!(
            h.ok(&["approval", "get", card["id"].as_str().unwrap()])
                .await["id"],
            card["id"]
        );
        if call_id == "deny-1" {
            assert!(!h.workspace().join("src/denied.txt").exists());
        }
        if call_id == "write-1" {
            assert_eq!(
                std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
                b"before needle\n"
            );
        }
        if call_id == "command-1" {
            assert!(!h.workspace().join("shell-effect.txt").exists());
        }
        let id = card["id"].as_str().unwrap();
        if call_id == "command-1" {
            h.ok(&["approval", decision, id, "--acknowledge-command-network"])
                .await;
        } else {
            h.ok(&["approval", decision, id]).await;
        }
    }
    h.done(run_id, task).await;
    assert!(!h.workspace().join("src/denied.txt").exists());
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"after needle\n"
    );
    let shell_effect = std::fs::read(h.workspace().join("shell-effect.txt")).unwrap();
    assert_eq!(String::from_utf8_lossy(&shell_effect).trim(), "once");
    let requests = h.script.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 7);
    let results = requests[6]["messages"].as_array().unwrap();
    for (id, needle) in [
        ("read-1", "before needle"),
        ("list-1", "a.txt"),
        ("search-1", "needle"),
    ] {
        let msg = results
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == id)
            .unwrap();
        assert!(msg["content"].as_str().unwrap().contains(needle));
    }
    let command = results
        .iter()
        .find(|m| m["role"] == "tool" && m["tool_call_id"] == "command-1")
        .unwrap();
    let command_result: Value = serde_json::from_str(command["content"].as_str().unwrap()).unwrap();
    assert_eq!(command_result["exit_code"], 0);
    assert_eq!(start(&h, "workflow-start").await["run_id"], run_id);
    assert_eq!(h.script.requests.lock().unwrap().len(), 7);
    let budget = h
        .ok(&["project", "budget", "status", "--id", project_id])
        .await;
    assert_eq!(budget["spent_tokens"], 35);
    assert_eq!(budget["held_tokens"], 0);
    let calls = h.ok(&["task", "network-calls", task]).await;
    assert_eq!(calls["calls"].as_array().unwrap().len(), 7);
    let changes = h.ok(&["task", "changes", task]).await;
    assert_eq!(changes["changes"].as_array().unwrap().len(), 1);
    let diff = h
        .ok(&[
            "task",
            "diff",
            task,
            "--path",
            "src/a.txt",
            "--view",
            "head",
        ])
        .await;
    assert_eq!(diff["diff"]["status"], "text");
    let patch = diff["diff"]["patch"].as_str().unwrap();
    assert!(patch.contains("-before needle") && patch.contains("+after needle"));
    let checkpoint = h
        .ok(&["task", "checkpoint", task, "--key", "first-checkpoint"])
        .await;
    let checkpoint_id = checkpoint["checkpoint"]["checkpoint_id"].as_str().unwrap();
    assert_eq!(
        checkpoint["checkpoint"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(checkpoint["checkpoint"]["entries"][0]["path"], "src/a.txt");
    assert_eq!(
        h.ok(&["checkpoint", "get", checkpoint_id]).await["checkpoint"],
        checkpoint["checkpoint"]
    );
    std::fs::write(h.workspace().join("src/a.txt"), "external\n").unwrap();
    let conflict = failure(&h.command(&["checkpoint", "restore", checkpoint_id]).await);
    assert_eq!(conflict["code"], "conflict");
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"external\n"
    );
    std::fs::write(h.workspace().join("src/a.txt"), "after needle\n").unwrap();
    let restored = h.ok(&["checkpoint", "restore", checkpoint_id]).await;
    assert_eq!(restored["status"], "complete");
    assert_eq!(restored["restored"], 1);
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"before needle\n"
    );
    assert_eq!(
        std::fs::read(h.workspace().join("shell-effect.txt")).unwrap(),
        shell_effect
    );
    assert_eq!(
        h.ok(&["checkpoint", "restore", checkpoint_id]).await,
        restored
    );
    let sealed = failure(
        &h.command(&["task", "resume", task, "--message", "must stay sealed"])
            .await,
    );
    assert_eq!(sealed["code"], "conflict");
    let previous = context["latest_turn"]["id"].as_str().unwrap();
    let sent = h
        .ok(&[
            "session",
            "send",
            session,
            "--agent",
            agent,
            "--after-turn",
            previous,
            "--message",
            "continue after restore",
            "--key",
            "workflow-next",
        ])
        .await;
    let second = sent["task"]["legacy_task_id"].as_str().unwrap();
    assert_ne!(second, task);
    h.done(run_id, second).await;
    let observed = h
        .command(&["run", "events", run_id, "--after", "0", "--limit", "2"])
        .await;
    assert_eq!(observed.status.code(), Some(0));
    let events: Vec<Value> = String::from_utf8(observed.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    let cursor = events[1]["seq"].to_string();
    let resumed = h
        .command(&[
            "session", "events", session, "--after", &cursor, "--limit", "1",
        ])
        .await;
    assert_eq!(resumed.status.code(), Some(0));
    let next_event: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert!(next_event["seq"].as_i64() > events[1]["seq"].as_i64());
    let latest = h.context(run_id).await;
    let budget_before_restart = h
        .ok(&["project", "budget", "status", "--id", project_id])
        .await;
    assert_eq!(h.script.requests.lock().unwrap().len(), 8);
    h.stop().await;
    h.restart().await;
    assert_eq!(
        h.ok(&["host", "config", "operation", "--key", "setup-route"])
            .await,
        route_receipt
    );
    assert_eq!(h.ok(&["host", "config", "get"]).await["revision"], 1);
    assert_eq!(
        h.ok(&["project", "budget", "status", "--id", project_id])
            .await,
        budget_before_restart
    );
    assert_eq!(h.context(run_id).await, latest);
    let after_restart = h
        .command(&[
            "session", "events", session, "--after", &cursor, "--limit", "1",
        ])
        .await;
    assert_eq!(after_restart.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&after_restart.stdout).unwrap(),
        next_event
    );
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"before needle\n"
    );
    assert_eq!(
        std::fs::read(h.workspace().join("shell-effect.txt")).unwrap(),
        shell_effect
    );
    assert_eq!(h.script.requests.lock().unwrap().len(), 8);
    let sent = h
        .ok(&[
            "session",
            "send",
            session,
            "--agent",
            agent,
            "--after-turn",
            latest["latest_turn"]["id"].as_str().unwrap(),
            "--message",
            "continue after restart",
            "--key",
            "workflow-third",
        ])
        .await;
    h.done(run_id, sent["task"]["legacy_task_id"].as_str().unwrap())
        .await;
    assert_eq!(h.script.requests.lock().unwrap().len(), 9);
    assert_eq!(
        std::fs::read(h.workspace().join("shell-effect.txt")).unwrap(),
        shell_effect
    );
    assert_eq!(std::fs::read_dir(h.cwd()).unwrap().count(), 0);
    h.stop().await;
}
