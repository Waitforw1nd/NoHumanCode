//! NEXT-19 T13: public CLI approvals, continuation and real Host restart.
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
const SENSITIVE: &str = "sk-synthetic-denied-private-value";
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
        assert!(!String::from_utf8_lossy(bytes).contains(SENSITIVE));
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
    dir: std::path::PathBuf,
    port: u16,
    child: Child,
    script: Script,
    provider: tokio::task::JoinHandle<()>,
    token: String,
    retired_tokens: Vec<String>,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.provider.abort();
    }
}
impl Host {
    fn workspace(&self) -> std::path::PathBuf {
        self.dir.join("workspace")
    }
    fn cwd(&self) -> std::path::PathBuf {
        self.dir.join("client")
    }
    async fn command(&self, args: &[&str]) -> Output {
        let output = cli(self.port, &self.cwd(), args, None).await;
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&self.token));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&self.token));
        for token in &self.retired_tokens {
            assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
            assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
        }
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
        let status = self.child.wait().await.unwrap();
        assert!(!status.success());
        use std::io::Write as _;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("host-stops.jsonl"))
            .unwrap();
        writeln!(
            log,
            "{}",
            json!({"pid":pid,"waited":true,"exit_code":status.code(),"success":status.success()})
        )
        .unwrap();
        println!("T13 Host kill+wait pid={pid}");
    }
    async fn restart(&mut self) {
        self.child = launch(&self.dir, self.port).await;
        let new_token = bootstrap(self.port).await;
        assert_ne!(new_token, self.token);
        self.retired_tokens.push(self.token.clone());
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
        .stdout(Stdio::from(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("host.stdout.log"))
                .unwrap(),
        ))
        .stderr(Stdio::from(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("host.stderr.log"))
                .unwrap(),
        ))
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
    let dir = tempfile::tempdir().unwrap().keep();
    println!("T13 retained fixture={}", dir.display());
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::create_dir(dir.join("client")).unwrap();
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
    assert!(!dir.join("data/peachsh.sqlite3").exists());
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
    let child = launch(&dir, port).await;
    let token = bootstrap(port).await;
    let h = Host {
        dir,
        port,
        child,
        script,
        provider,
        token,
        retired_tokens: Vec::new(),
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
    let config = h.dir.join("project-config.json");
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

fn tool_message<'a>(messages: &'a [Value], id: &str) -> &'a Value {
    let found: Vec<_> = messages
        .iter()
        .filter(|m| m["role"] == "tool" && m["tool_call_id"] == id)
        .collect();
    assert_eq!(found.len(), 1, "tool result cardinality for {id}");
    found[0]
}

async fn read_events(h: &Host, kind: &str, id: &str, after: i64, limit: usize) -> Vec<Value> {
    let output = h
        .command(&[
            kind,
            "events",
            id,
            "--after",
            &after.to_string(),
            "--limit",
            &limit.to_string(),
        ])
        .await;
    assert!(output.status.success(), "event read failed");
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), limit);
    events
}

fn snapshot_after_stop(root: &Path, pid: u32) {
    use sha2::{Digest, Sha256};
    let source = root.join("data");
    let raw = root.join("stopped-raw");
    let query = root.join("stopped-query");
    std::fs::create_dir(&raw).unwrap();
    std::fs::create_dir(&query).unwrap();
    let mut files = Vec::new();
    for name in [
        "peachsh.sqlite3",
        "peachsh.sqlite3-wal",
        "peachsh.sqlite3-shm",
    ] {
        let file = source.join(name);
        if file.exists() {
            let bytes = std::fs::read(&file).unwrap();
            std::fs::write(raw.join(name), &bytes).unwrap();
            std::fs::write(query.join(name), &bytes).unwrap();
            files.push(json!({"name":name,"present":true,"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))}));
        } else {
            files.push(json!({"name":name,"present":false}));
        }
    }
    assert_eq!(files[0]["present"], true);
    std::fs::write(root.join("stopped-metadata.json"), serde_json::to_vec_pretty(&json!({"host_pid":pid,"killed_and_waited":true,"files":files,"query_is_separate_copy":true})).unwrap()).unwrap();
}

fn verify_sealed_raw(root: &Path) {
    use sha2::{Digest, Sha256};
    let metadata: Value =
        serde_json::from_slice(&std::fs::read(root.join("stopped-metadata.json")).unwrap())
            .unwrap();
    for file in metadata["files"].as_array().unwrap() {
        let path = root
            .join("stopped-raw")
            .join(file["name"].as_str().unwrap());
        if file["present"] == true {
            let bytes = std::fs::read(path).unwrap();
            assert_eq!(file["bytes"], bytes.len());
            assert_eq!(file["sha256"], format!("{:x}", Sha256::digest(&bytes)));
        } else {
            assert!(!path.exists());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_tool_approval_turn_restart_and_event_continuation() {
    let denied_command =
        "Add-Content -LiteralPath 'denied-shell.txt' -Value 'denied' -ErrorAction Stop";
    let approved_command = "if ((Get-Content -Raw 'src/a.txt') -ne \"approved needle`n\") { exit 7 }; Add-Content -LiteralPath 'approved-shell.txt' -Value 'once' -ErrorAction Stop";
    let mut h = host(vec![
        call("t13-read", "read_file", json!({"path":"src/a.txt"})),
        write_call("t13-deny-write", "src/denied.txt", SENSITIVE),
        call(
            "t13-deny-command",
            "run_command",
            json!({"command":denied_command}),
        ),
        json!({"content":"first turn complete"}),
        write_call("t13-write", "src/a.txt", "approved needle\n"),
        call(
            "t13-command",
            "run_command",
            json!({"command":approved_command}),
        ),
        json!({"content":"second turn complete"}),
        json!({"content":"after restart complete"}),
    ])
    .await;
    let run = start(&h, "t13-start").await;
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    let first_task = run["tasks"][0]["id"].as_str().unwrap().to_owned();
    let first_context = h.context(&run_id).await;
    let session = first_context["session"]["id"].as_str().unwrap().to_owned();
    let agent = first_context["tasks"][0]["agent_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_turn = first_context["latest_turn"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let denied_file = h.workspace().join("src/denied.txt");
    let denied_effect = h.workspace().join("denied-shell.txt");
    let approved_effect = h.workspace().join("approved-shell.txt");
    let mut cards = Vec::new();
    for id in ["t13-deny-write", "t13-deny-command"] {
        let card = h.pending(&first_task).await;
        assert_eq!(card["tool_call_id"], id);
        assert_eq!(card["task_id"], first_task);
        assert_eq!(card["session_id"], session);
        assert_eq!(card["turn_id"], first_turn);
        assert!(!denied_file.exists() && !denied_effect.exists());
        assert_eq!(
            std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
            b"before needle\n"
        );
        let approval_id = card["id"].as_str().unwrap().to_owned();
        assert_eq!(
            h.ok(&["approval", "get", &approval_id]).await["id"],
            approval_id
        );
        h.ok(&["approval", "deny", &approval_id]).await;
        cards.push((approval_id, first_task.clone(), first_turn.clone(), id));
        assert!(!denied_file.exists() && !denied_effect.exists());
    }
    h.done(&run_id, &first_task).await;
    assert!(!denied_file.exists() && !denied_effect.exists());
    {
        let requests = h.script.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        let messages = requests[3]["messages"].as_array().unwrap();
        assert!(
            tool_message(messages, "t13-read")["content"]
                .as_str()
                .unwrap()
                .contains("before needle")
        );
        for id in ["t13-deny-write", "t13-deny-command"] {
            assert_eq!(
                tool_message(messages, id)["content"],
                "审批已拒绝，工具未执行。"
            );
        }
    }
    let sent = h
        .ok(&[
            "session",
            "send",
            &session,
            "--agent",
            &agent,
            "--after-turn",
            &first_turn,
            "--message",
            "perform approved work",
            "--key",
            "t13-next",
        ])
        .await;
    let second_task = sent["task"]["legacy_task_id"].as_str().unwrap().to_owned();
    let second_turn = sent["turn"]["id"].as_str().unwrap().to_owned();
    assert_ne!(second_task, first_task);
    assert_ne!(second_turn, first_turn);
    assert_eq!(sent["turn"]["session_id"], session);
    for id in ["t13-write", "t13-command"] {
        let card = h.pending(&second_task).await;
        assert_eq!(card["tool_call_id"], id);
        assert_eq!(card["task_id"], second_task);
        assert_eq!(card["session_id"], session);
        assert_eq!(card["turn_id"], second_turn);
        let approval_id = card["id"].as_str().unwrap().to_owned();
        if id == "t13-write" {
            assert_eq!(
                std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
                b"before needle\n"
            );
            h.ok(&["approval", "approve", &approval_id]).await;
        } else {
            assert_eq!(
                std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
                b"approved needle\n"
            );
            assert!(!approved_effect.exists());
            assert_eq!(card["network_notice"]["may_access_network"], true);
            h.ok(&[
                "approval",
                "approve",
                &approval_id,
                "--acknowledge-command-network",
            ])
            .await;
        }
        cards.push((approval_id, second_task.clone(), second_turn.clone(), id));
    }
    h.done(&run_id, &second_task).await;
    assert!(!denied_file.exists() && !denied_effect.exists());
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"approved needle\n"
    );
    let effect = std::fs::read(&approved_effect).unwrap();
    assert_eq!(String::from_utf8_lossy(&effect).trim(), "once");
    {
        let requests = h.script.requests.lock().unwrap();
        assert_eq!(requests.len(), 7);
        let messages = requests[6]["messages"].as_array().unwrap();
        for id in [
            "t13-read",
            "t13-deny-write",
            "t13-deny-command",
            "t13-write",
            "t13-command",
        ] {
            tool_message(messages, id);
        }
        let result: Value = serde_json::from_str(
            tool_message(messages, "t13-command")["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["exit_code"], 0);
    }
    let mut events = read_events(&h, "run", &run_id, 0, 2).await;
    let first_cursor = events.last().unwrap()["seq"].as_i64().unwrap();
    let mut cursor = first_cursor;
    for _ in 0..200 {
        let next = read_events(&h, "session", &session, cursor, 1)
            .await
            .pop()
            .unwrap();
        let seq = next["seq"].as_i64().unwrap();
        assert!(seq > cursor);
        assert!(!events.iter().any(|e| e["seq"] == next["seq"]));
        cursor = seq;
        events.push(next);
        let enough = [
            "t13-read",
            "t13-deny-write",
            "t13-deny-command",
            "t13-write",
            "t13-command",
        ]
        .iter()
        .all(|id| {
            events
                .iter()
                .any(|e| e["kind"] == "tool_result" && e["data"]["tool_call_id"] == *id)
        }) && cards.iter().all(|(id, _, _, _)| {
            events
                .iter()
                .any(|e| e["kind"] == "approval.resolved" && e["data"]["approval_id"] == *id)
        }) && events.iter().any(|e| {
            e["kind"] == "status"
                && e["task_id"] == second_task
                && e["data"]["status"] == "completed"
        });
        if enough {
            break;
        }
    }
    for (id, task, turn, call_id) in &cards {
        for kind in ["approval.requested", "approval.resolved"] {
            let matching: Vec<_> = events
                .iter()
                .filter(|e| e["kind"] == kind && e["data"]["approval_id"] == *id)
                .collect();
            assert_eq!(matching.len(), 1);
            assert_eq!(matching[0]["session_id"], session);
            assert_eq!(matching[0]["task_id"], *task);
            assert_eq!(matching[0]["turn_id"], *turn);
        }
        let matching: Vec<_> = events
            .iter()
            .filter(|e| e["kind"] == "tool_result" && e["data"]["tool_call_id"] == *call_id)
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0]["task_id"], *task);
        assert_eq!(matching[0]["turn_id"], *turn);
        assert_eq!(matching[0]["session_id"], session);
    }
    let reads: Vec<_> = events
        .iter()
        .filter(|e| e["kind"] == "tool_result" && e["data"]["tool_call_id"] == "t13-read")
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0]["task_id"], first_task);
    assert_eq!(reads[0]["turn_id"], first_turn);
    assert_eq!(reads[0]["session_id"], session);
    assert!(events.iter().any(|e| e["kind"] == "status"
        && e["task_id"] == second_task
        && e["data"]["status"] == "completed"));
    for id in ["t13-write", "t13-command"] {
        assert_eq!(
            events
                .iter()
                .filter(|e| e["kind"] == "tool_start" && e["data"]["tool_call_id"] == id)
                .count(),
            1
        );
    }
    for id in ["t13-deny-write", "t13-deny-command"] {
        let result = events
            .iter()
            .find(|e| e["kind"] == "tool_result" && e["data"]["tool_call_id"] == id)
            .unwrap();
        assert_eq!(result["data"]["origin"], "approval_denied");
        assert_eq!(result["data"]["result"], "审批已拒绝，工具未执行。");
        assert!(
            !events
                .iter()
                .any(|e| e["kind"] == "tool_start" && e["data"]["tool_call_id"] == id)
        );
    }
    let before_restart = h.context(&run_id).await;
    let approvals_first = h.ok(&["task", "approvals", &first_task]).await;
    let approvals_second = h.ok(&["task", "approvals", &second_task]).await;
    let pid = h.child.id().unwrap();
    h.stop().await;
    snapshot_after_stop(&h.dir, pid);
    h.restart().await;
    assert_eq!(h.context(&run_id).await, before_restart);
    assert_eq!(
        h.ok(&["task", "approvals", &first_task]).await,
        approvals_first
    );
    assert_eq!(
        h.ok(&["task", "approvals", &second_task]).await,
        approvals_second
    );
    let resumed = read_events(&h, "session", &session, first_cursor, 1).await;
    assert_eq!(resumed[0], events[2]);
    assert_eq!(h.script.requests.lock().unwrap().len(), 7);
    assert_eq!(std::fs::read(&approved_effect).unwrap(), effect);
    assert!(!denied_file.exists() && !denied_effect.exists());
    let third = h
        .ok(&[
            "session",
            "send",
            &session,
            "--agent",
            &agent,
            "--after-turn",
            &second_turn,
            "--message",
            "continue after restart",
            "--key",
            "t13-after-restart",
        ])
        .await;
    assert_eq!(third["turn"]["session_id"], session);
    assert_ne!(third["turn"]["id"], second_turn);
    let third_task = third["task"]["legacy_task_id"].as_str().unwrap();
    assert_ne!(third_task, second_task);
    h.done(&run_id, third_task).await;
    {
        let requests = h.script.requests.lock().unwrap();
        assert_eq!(requests.len(), 8);
        let messages = requests[7]["messages"].as_array().unwrap();
        for id in [
            "t13-read",
            "t13-deny-write",
            "t13-deny-command",
            "t13-write",
            "t13-command",
        ] {
            tool_message(messages, id);
        }
    }
    assert_eq!(std::fs::read(&approved_effect).unwrap(), effect);
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"approved needle\n"
    );
    h.stop().await;
    verify_sealed_raw(&h.dir);
    let stops = std::fs::read_to_string(h.dir.join("host-stops.jsonl")).unwrap();
    let stopped: Vec<Value> = stops
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(stopped.len(), 2);
    assert_eq!(stopped[0]["pid"], pid);
    assert!(stopped[1]["pid"].as_u64().is_some_and(|value| value > 0));
    assert!(
        stopped
            .iter()
            .all(|entry| entry["waited"] == true && entry["success"] == false)
    );
    for name in ["host.stdout.log", "host.stderr.log"] {
        let output = std::fs::read_to_string(h.dir.join(name)).unwrap();
        assert!(
            !output.contains(SECRET) && !output.contains(SENSITIVE) && !output.contains(&h.token)
        );
        for token in &h.retired_tokens {
            assert!(!output.contains(token));
        }
    }
    assert_eq!(std::fs::read_dir(h.cwd()).unwrap().count(), 0);
}
