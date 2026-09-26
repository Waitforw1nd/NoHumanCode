use axum::{
    Json, Router,
    body::Body,
    extract::State,
    response::{IntoResponse, Response},
    routing::{get, post},
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn n01_n02_n04_real_cli_host_tool_restore_new_turn_and_restart() {
    let mut h=host(vec![
        call("read-1","read_file",json!({"path":"src/a.txt"})),
        write_call("deny-1","src/denied.txt","denied"),
        write_call("write-1","src/a.txt","after"),
        call("command-1","run_command",json!({"command":"if ((Get-Content -Raw 'src/a.txt') -ne 'after') { exit 7 }; Set-Content -NoNewline 'src/test-ok.txt' 'ok'"})),
        json!({"content":"first complete"}),
        write_call("write-2","src/a.txt","continued"),json!({"content":"second complete"}),
        json!({"content":"after restart"}),
    ]).await;
    std::fs::write(h.workspace().join("src/a.txt"), "before").unwrap();
    assert_eq!(h.ok(&["project", "list"]).await, json!([]));
    let run = start(&h, "workflow-start", true, true).await;
    let run_id = run["run_id"].as_str().unwrap();
    let task = run["tasks"][0]["id"].as_str().unwrap();
    assert!(run["tasks"][0].get("messages").is_none());
    let context = h.context(run_id).await;
    let session = context["session"]["id"].as_str().unwrap();
    assert_ne!(run_id, session);
    assert_eq!(context["tasks"][0]["legacy_task_id"], task);
    assert_eq!(context["project"]["id"], context["session"]["project_id"]);
    assert_eq!(
        h.ok(&["session", "status", session]).await["legacy_run_id"],
        run_id
    );
    assert_eq!(
        h.ok(&["project", "list"]).await.as_array().unwrap().len(),
        1
    );
    for (call_id, decision) in [
        ("deny-1", "deny"),
        ("write-1", "approve"),
        ("command-1", "approve"),
    ] {
        let card = h.pending(task).await;
        assert_eq!(card["tool_call_id"], call_id);
        let id = card["id"].as_str().unwrap();
        assert_eq!(h.ok(&["approval", "get", id]).await["id"], id);
        h.ok(&["approval", decision, id]).await;
    }
    let done = h.done(run_id, task).await;
    assert_eq!(done["allow_commands"], true);
    assert!(!h.workspace().join("src/denied.txt").exists());
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"after"
    );
    assert_eq!(
        std::fs::read(h.workspace().join("src/test-ok.txt")).unwrap(),
        b"ok"
    );
    assert_eq!(h.script.requests.lock().unwrap().len(), 5);
    assert_eq!(
        start(&h, "workflow-start", true, true).await["run_id"],
        run_id
    );
    assert_eq!(h.script.requests.lock().unwrap().len(), 5);
    let changes = h.ok(&["task", "changes", task]).await;
    assert_eq!(changes["changes"].as_array().unwrap().len(), 1);
    let restored = h.ok(&["task", "restore", task]).await;
    assert_eq!(restored["status"], "complete");
    assert_eq!(restored["restored"], 1);
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"before"
    );
    assert_eq!(h.ok(&["task", "restore", task]).await, restored);
    let agent = context["tasks"][0]["agent_id"].as_str().unwrap();
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
    assert_eq!(sent["task"]["agent_id"], agent);
    let card = h.pending(second).await;
    assert_eq!(card["tool_call_id"], "write-2");
    h.ok(&["approval", "approve", card["id"].as_str().unwrap()])
        .await;
    h.done(run_id, second).await;
    let second_changes = h.ok(&["task", "changes", second]).await;
    assert_eq!(
        second_changes["changes"][0]["before_digest"],
        changes["changes"][0]["before_digest"]
    );
    assert_eq!(h.script.requests.lock().unwrap().len(), 7);
    let original_approvals = h.ok(&["task", "approvals", task]).await;
    assert_eq!(original_approvals["approvals"].as_array().unwrap().len(), 3);
    let latest = h.context(run_id).await;
    assert_eq!(latest["tasks"][0]["legacy_task_id"], second);
    assert_eq!(
        h.ok(&["session", "turns", session])
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // The new request includes an explicit Host restore fact, without rewriting prior results.
    let requests = h.script.requests.lock().unwrap().clone();
    let next_messages = requests[5]["messages"].as_array().unwrap();
    assert!(
        next_messages
            .iter()
            .any(|m| m["content"].as_str().is_some_and(
                |text| text.contains("恢复") || text.to_ascii_lowercase().contains("restore")
            ))
    );
    let observed = h
        .command(&["run", "events", run_id, "--after", "0", "--limit", "2"])
        .await;
    assert_eq!(observed.status.code(), Some(0));
    let events: Vec<Value> = String::from_utf8(observed.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert!(events[1]["seq"].as_i64() > events[0]["seq"].as_i64());
    let cursor = events[1]["seq"].to_string();
    let resumed = h
        .command(&[
            "session", "events", session, "--after", &cursor, "--limit", "1",
        ])
        .await;
    assert_eq!(resumed.status.code(), Some(0));
    let event: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert!(event["seq"].as_i64() > events[1]["seq"].as_i64());
    h.stop().await;
    h.restart().await;
    assert_eq!(h.context(run_id).await, latest);
    assert_eq!(h.ok(&["task", "approvals", task]).await, original_approvals);
    assert_eq!(h.ok(&["task", "changes", second]).await, second_changes);
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
            "after restart",
            "--key",
            "workflow-third",
        ])
        .await;
    h.done(run_id, sent["task"]["legacy_task_id"].as_str().unwrap())
        .await;
    assert_eq!(h.script.requests.lock().unwrap().len(), 8);
    assert_eq!(
        std::fs::read_dir(h.cwd()).unwrap().count(),
        0,
        "CLI must not initialize a DB"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn n12_host_kill_wait_recovery_and_observer_disconnect_do_not_reexecute() {
    let mut h = host(vec![
        json!({"hold":true}),
        json!({"content":"explicit resume"}),
    ])
    .await;
    let run = start(&h, "interrupted-start", false, false).await;
    let run_id = run["id"].as_str().unwrap();
    let task = run["tasks"][0]["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(8), h.script.entered.notified())
        .await
        .unwrap();
    let context = h.context(run_id).await;
    let observed = h
        .command(&["run", "events", run_id, "--after", "0", "--limit", "1"])
        .await;
    assert!(observed.status.success());
    assert_eq!(
        h.ok(&["run", "status", run_id]).await["tasks"][0]["status"],
        "running"
    );
    h.stop().await;
    h.restart().await;
    let recovered = h.context(run_id).await;
    assert_eq!(recovered["session"], context["session"]);
    assert_eq!(recovered["latest_turn"]["id"], context["latest_turn"]["id"]);
    assert_eq!(
        h.ok(&["run", "status", run_id]).await["tasks"][0]["status"],
        "interrupted"
    );
    assert_eq!(h.script.requests.lock().unwrap().len(), 1);
    assert_eq!(
        start(&h, "interrupted-start", false, false).await["id"],
        run_id
    );
    assert_eq!(h.script.requests.lock().unwrap().len(), 1);
    h.ok(&["task", "resume", task, "--message", "explicit resume"])
        .await;
    h.done(run_id, task).await;
    assert_eq!(h.script.requests.lock().unwrap().len(), 2);
    h.stop().await;
}

fn event(seq: i64) -> Value {
    json!({"schema_version":1,"seq":seq,"cursor":seq.to_string(),"session_id":"s","turn_id":"t","task_id":"task","kind":"delta","data":{"text":"中文"},"at":1})
}
fn frame(seq: i64) -> Vec<u8> {
    format!("id: {seq}\r\nevent: delta\r\ndata: {}\r\n\r\n", event(seq)).into_bytes()
}
async fn stream_server(bytes: Vec<u8>) -> (u16, tokio::task::JoinHandle<()>) {
    let app=Router::new().route("/api/runs/r/events",get(move || {
        let bytes=bytes.clone(); async move {
            let stream=async_stream::stream! { for chunk in bytes.chunks(if bytes.len()>10000 {4096} else {1}) { yield Ok::<_,Infallible>(chunk.to_vec()); tokio::task::yield_now().await; } };
            ([("content-type","text/event-stream")],Body::from_stream(stream))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (
        port,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
#[tokio::test(flavor = "multi_thread")]
async fn n11_sse_split_utf8_crlf_comments_multiline_limit_and_failure_cursor() {
    let cwd = tempfile::tempdir().unwrap();
    let mut bytes = b": keepalive\r\n\r\n".to_vec();
    let pretty = serde_json::to_string_pretty(&event(1)).unwrap();
    bytes.extend_from_slice(b"id: 1\nevent: delta\n");
    for line in pretty.lines() {
        bytes.extend_from_slice(format!("data: {line}\n").as_bytes());
    }
    bytes.push(b'\n');
    bytes.extend(frame(2));
    let (port, server) = stream_server(bytes).await;
    let output = cli(port, cwd.path(), &["run", "events", "r", "--limit", "2"]).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines, vec![event(1), event(2)]);
    server.abort();
    for tail in [
        vec![],
        b"event: error\ndata: {\"code\":\"internal\",\"message\":\"storage failed\",\"retryable\":false,\"after\":1}\n\n".to_vec(),
        b"id: 2\nevent: delta\ndata: {broken}\n\n".to_vec(),
        b"id: -1\nevent: delta\ndata: {}\n\n".to_vec(),
        {let mut text=frame(2); let pos=text.iter().position(|b|*b==b'{').unwrap(); text[pos]=0xff; text},
        {let mut text=b"id: 2\nevent: delta\ndata: ".to_vec(); text.extend(vec![b'x';1_048_577]);text},
        format!("id: 3\nevent: delta\ndata: {}\n\n",event(2)).into_bytes(),
    ] {
        let mut bytes=frame(1);bytes.extend(tail);
        let (port,server)=stream_server(bytes).await;
        let output=cli(port,cwd.path(),&["run","events","r"]).await;
        assert_eq!(output.status.code(),Some(1),"{}",String::from_utf8_lossy(&output.stderr));
        assert_eq!(serde_json::from_slice::<Value>(&output.stdout).unwrap(),event(1));
        let error:Value=serde_json::from_slice(&output.stderr).unwrap(); assert_eq!(error["after"],1);
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn n13_workflow_parse_gates_and_unknown_post_are_safe_and_single_attempt() {
    let cwd = tempfile::tempdir().unwrap();
    for args in [
        vec![
            "run",
            "start",
            "--title",
            "t",
            "--route",
            "r",
            "--message",
            "m",
            "--key",
            "k",
            "--allow-commands",
        ],
        vec![
            "run",
            "start",
            "--title",
            "t",
            "--route",
            "r",
            "--message",
            "m",
            "--key",
            "k",
            "--scope",
            "src",
        ],
        vec!["run", "events", "r", "--after", "-1"],
        vec!["run", "events", "r", "--limit", "0"],
        vec![
            "session",
            "send",
            "s",
            "--agent",
            "a",
            "--after-turn",
            "t",
            "--message",
            "m",
            "--key",
            "bad,key",
        ],
    ] {
        let output = cli(1, cwd.path(), &args).await;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = requests.clone();
    let app = Router::new()
        .route(
            "/api/bootstrap",
            get(|| async { Json(json!({"token":"workflow-token"})) }),
        )
        .fallback(move |uri: axum::extract::OriginalUri| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(uri.to_string());
                (axum::http::StatusCode::OK, "invalid reply")
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for args in [
        vec![
            "run",
            "start",
            "--title",
            "t",
            "--route",
            "r",
            "--message",
            "m",
            "--key",
            "k",
        ],
        vec![
            "session",
            "send",
            "s",
            "--agent",
            "a",
            "--after-turn",
            "t",
            "--message",
            "m",
            "--key",
            "k",
        ],
        vec!["task", "resume", "t", "--message", "m"],
        vec!["task", "cancel", "t"],
        vec!["task", "restore", "t"],
    ] {
        let error = failure(&cli(port, cwd.path(), &args).await);
        assert_eq!(error["code"], "operation_result_unknown");
        assert_eq!(error["retryable"], false);
        assert!(!error.to_string().contains("workflow-token"));
    }
    assert_eq!(requests.lock().unwrap().len(), 5);
    assert_eq!(std::fs::read_dir(cwd.path()).unwrap().count(), 0);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn n10_cli_unknown_receipt_survives_real_host_restart_without_repeating_effects() {
    let mut h = host(vec![
        write_call("w1", "src/a.txt", "after"),
        json!({"content":"done"}),
    ])
    .await;
    std::fs::write(h.workspace().join("src/a.txt"), "before-private-value").unwrap();
    let run = start(&h, "unknown-restore", true, false).await;
    let task = run["tasks"][0]["id"].as_str().unwrap();
    let run_id = run["id"].as_str().unwrap();
    let pending = h.pending(task).await;
    h.ok(&["approval", "approve", pending["id"].as_str().unwrap()])
        .await;
    h.done(run_id, task).await;
    rusqlite::Connection::open(h.db()).unwrap().execute_batch("CREATE TRIGGER fail_outcome BEFORE UPDATE OF status ON workspace_restore_outcomes WHEN NEW.status='complete' BEGIN SELECT RAISE(ABORT,'SQL-private-value'); END;").unwrap();
    let unknown = failure(&h.command(&["task", "restore", task]).await);
    assert_eq!(unknown["status"], "unknown");
    assert_eq!(unknown["receipt"]["status"], "unknown");
    assert_eq!(unknown["receipt"]["outcomes"][0]["status"], "unknown");
    assert_eq!(unknown["retryable"], false);
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"before-private-value"
    );
    for forbidden in ["before-private-value", "SQL-private-value", "before_blob"] {
        assert!(!unknown.to_string().contains(forbidden));
    }
    let context = h.context(run_id).await;
    h.stop().await;
    h.restart().await;
    assert_eq!(h.context(run_id).await, context);
    assert_eq!(
        h.ok(&["task", "changes", task]).await["restore"],
        unknown["receipt"]
    );
    std::fs::write(h.workspace().join("src/a.txt"), "later external edit").unwrap();
    let repeated = failure(&h.command(&["task", "restore", task]).await);
    assert_eq!(repeated["receipt"], unknown["receipt"]);
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"later external edit"
    );
    assert_eq!(h.script.requests.lock().unwrap().len(), 2);
    h.stop().await;
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn n10_cli_real_partial_receipt_preserves_both_path_facts() {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt, time::Instant};
    let mut h = host(vec![
        write_call("w1", "src/a.txt", "a-after"),
        write_call("w2", "src/b.txt", "b-after"),
        json!({"content":"done"}),
    ])
    .await;
    std::fs::write(h.workspace().join("src/a.txt"), "a-before").unwrap();
    std::fs::write(h.workspace().join("src/b.txt"), "b-before").unwrap();
    let run = start(&h, "partial-restore", true, false).await;
    let task = run["tasks"][0]["id"].as_str().unwrap();
    let run_id = run["id"].as_str().unwrap();
    for id in ["w1", "w2"] {
        let pending = h.pending(task).await;
        assert_eq!(pending["tool_call_id"], id);
        h.ok(&["approval", "approve", pending["id"].as_str().unwrap()])
            .await;
    }
    h.done(run_id, task).await;
    let db = rusqlite::Connection::open(h.db()).unwrap();
    db.execute_batch("CREATE TRIGGER pause_first_outcome BEFORE UPDATE OF status ON workspace_restore_outcomes WHEN NEW.status='complete' AND NEW.path='src/a.txt' BEGIN SELECT sum(x) FROM (WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x<10000000) SELECT x FROM seq); END;").unwrap();
    let port = h.port;
    let cwd = h.cwd();
    let id = task.to_owned();
    let request = tokio::spawn(async move { cli(port, &cwd, &["task", "restore", &id]).await });
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let probe = rusqlite::Connection::open(h.db()).unwrap();
        probe.busy_timeout(Duration::ZERO).unwrap();
        let busy = matches!(probe.execute_batch("BEGIN IMMEDIATE; ROLLBACK;"),Err(rusqlite::Error::SqliteFailure(error,_)) if error.code==rusqlite::ErrorCode::DatabaseBusy);
        if busy && std::fs::read(h.workspace().join("src/a.txt")).unwrap() == b"a-before" {
            break;
        }
        assert!(
            !request.is_finished(),
            "restore finished before first path commit barrier"
        );
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(h.workspace().join("src/b.txt"))
        .unwrap();
    let partial = failure(&request.await.unwrap());
    assert_eq!(partial["status"], "partial");
    assert_eq!(partial["restored"], 1);
    assert_eq!(partial["receipt"]["outcomes"][0]["status"], "complete");
    assert_eq!(partial["receipt"]["outcomes"][1]["status"], "unknown");
    drop(lock);
    assert_eq!(
        std::fs::read(h.workspace().join("src/b.txt")).unwrap(),
        b"b-after"
    );
    assert_eq!(
        failure(&h.command(&["task", "restore", task]).await)["receipt"],
        partial["receipt"]
    );
    assert_eq!(
        std::fs::read(h.workspace().join("src/b.txt")).unwrap(),
        b"b-after"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn n01_context_routes_classify_absent_legacy_and_corrupt_without_identity_invention() {
    let mut h = host(vec![json!({"content":"done"})]).await;
    let run = start(&h, "context-cases", false, false).await;
    let run_id = run["id"].as_str().unwrap();
    let task = run["tasks"][0]["id"].as_str().unwrap();
    h.done(run_id, task).await;
    let context = h.context(run_id).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for (id, status, code) in [
        ("absent", 404, "not_found"),
        ("bad%20id", 400, "request_failed"),
    ] {
        let response = client
            .get(format!("http://127.0.0.1:{}/api/runs/{id}/context", h.port))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.json::<Value>().await.unwrap()["code"], code);
    }
    let db = rusqlite::Connection::open(h.db()).unwrap();
    db.execute(
        "INSERT INTO runs(id,title,kind,created_at) VALUES ('legacy','legacy','chat',1)",
        [],
    )
    .unwrap();
    let legacy = failure(&h.command(&["run", "context", "legacy"]).await);
    assert_eq!(legacy["code"], "conflict");
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sessions WHERE legacy_run_id='legacy'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    db.pragma_update(None, "foreign_keys", false).unwrap();
    db.execute(
        "UPDATE sessions SET project_id='private-missing-project' WHERE id=?1",
        [context["session"]["id"].as_str().unwrap()],
    )
    .unwrap();
    let corrupt = failure(&h.command(&["run", "context", run_id]).await);
    assert_eq!(corrupt["code"], "internal");
    assert!(!corrupt.to_string().contains("private"));
    let denied = client
        .get(format!(
            "http://127.0.0.1:{}/api/runs/{run_id}/context",
            h.port
        ))
        .header("Origin", "http://outside.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status().as_u16(), 403);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn n11_sse_keepalive_exceeds_regular_timeout_and_ignores_proxy() {
    let app = Router::new().route(
        "/api/runs/r/events",
        get(|| async {
            let stream = async_stream::stream! {
                for _ in 0..5 {
                    yield Ok::<_,Infallible>(b": keepalive\n\n".to_vec());
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                yield Ok(frame(1));
            };
            (
                [("content-type", "text/event-stream")],
                Body::from_stream(stream),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cwd = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let mut command = Command::new(BIN);
    command
        .args([
            "--port",
            &port.to_string(),
            "--json",
            "run",
            "events",
            "r",
            "--limit",
            "1",
        ])
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .current_dir(cwd.path())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(12), command.output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stdout(&output), event(1));
    assert!(started.elapsed() >= Duration::from_secs(5));
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn n13_write_reply_transport_oversize_and_escaped_token_never_retry() {
    let cwd = tempfile::tempdir().unwrap();
    let attempts = Arc::new(Mutex::new(0usize));
    let current = Arc::new(Mutex::new(0usize));
    let state = (attempts.clone(), current.clone());
    let app = Router::new()
        .route(
            "/api/bootstrap",
            get(|| async { Json(json!({"token":"workflow-private-token"})) }),
        )
        .route(
            "/api/tasks/t/cancel",
            post(move || {
                let (attempts, current) = state.clone();
                async move {
                    *attempts.lock().unwrap() += 1;
                    match *current.lock().unwrap() {
                        0 => {
                            let stream = async_stream::stream! {
                                yield Ok::<_,std::io::Error>(b"{\"ok\":".to_vec());
                                tokio::task::yield_now().await;
                                yield Err(std::io::Error::other("private transport reason"));
                            };
                            Body::from_stream(stream).into_response()
                        }
                        1 => (
                            [("content-length", "1048577")],
                            Body::from(vec![b'x'; 1_048_577]),
                        )
                            .into_response(),
                        _ => (
                            [("content-type", "application/json")],
                            r#"{"ok":true,"extra":"workflow-private-\u0074oken"}"#,
                        )
                            .into_response(),
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for mode in 0..3 {
        *current.lock().unwrap() = mode;
        let error = failure(&cli(port, cwd.path(), &["task", "cancel", "t"]).await);
        assert_eq!(error["code"], "operation_result_unknown");
        assert_eq!(error["retryable"], false);
        assert!(!error.to_string().contains("private"));
        assert_eq!(*attempts.lock().unwrap(), mode + 1);
    }
    server.abort();
}
