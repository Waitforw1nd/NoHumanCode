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

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t01_real_diff_checkpoint_restore_restart_new_turn() {
    let mut h = host(vec![
        write_call("write-1", "src/a.txt", "task-after\n"),
        write_call("write-2", "src/new.txt", "created"),
        json!({"content":"done"}),
        json!({"content":"new turn"}),
    ])
    .await;
    git(&h.workspace(), &["init"]);
    std::fs::write(h.workspace().join("src/a.txt"), "head\n").unwrap();
    git(&h.workspace(), &["add", "--", "src/a.txt"]);
    git(
        &h.workspace(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "base",
        ],
    );
    std::fs::write(h.workspace().join("src/a.txt"), "index\n").unwrap();
    git(&h.workspace(), &["add", "--", "src/a.txt"]);
    std::fs::write(h.workspace().join("src/a.txt"), "before\n").unwrap();
    std::fs::write(h.workspace().join("other.txt"), "unrelated").unwrap();
    let head = git(&h.workspace(), &["rev-parse", "HEAD"]);
    let index = std::fs::read(h.workspace().join(".git/index")).unwrap();
    let run = start(&h, "checkpoint-start", true, false).await;
    let run_id = run["run_id"].as_str().unwrap();
    let task = run["tasks"][0]["id"].as_str().unwrap();
    for call in ["write-1", "write-2"] {
        let card = h.pending(task).await;
        assert_eq!(card["tool_call_id"], call);
        h.ok(&["approval", "approve", card["id"].as_str().unwrap()])
            .await;
    }
    h.done(run_id, task).await;
    for (view, before, after) in [
        ("staged", "head", "index"),
        ("unstaged", "index", "task-after"),
        ("head", "head", "task-after"),
    ] {
        let diff = h
            .ok(&["task", "diff", task, "--path", "src/a.txt", "--view", view])
            .await;
        assert_eq!(diff["diff"]["status"], "text");
        assert_eq!(diff["diff"]["redacted"], false);
        let patch = diff["diff"]["patch"].as_str().unwrap();
        assert!(patch.contains(&format!("-{before}")), "{patch}");
        assert!(patch.contains(&format!("+{after}")), "{patch}");
    }
    let backslash = h.ok(&["task", "diff", task, "--path", r"src\a.txt"]).await;
    assert_eq!(backslash["diff"]["path"], "src/a.txt");
    assert_eq!(backslash["diff"]["view"], "head");
    let created = h
        .ok(&["task", "checkpoint", task, "--key", "checkpoint-key"])
        .await;
    assert_eq!(created["replayed"], false);
    let cp = created["checkpoint"]["checkpoint_id"].as_str().unwrap();
    assert_eq!(created["checkpoint"]["kind"], "task_before");
    assert_eq!(
        created["checkpoint"]["entries"].as_array().unwrap().len(),
        2
    );
    let replay = h
        .ok(&["task", "checkpoint", task, "--key", "checkpoint-key"])
        .await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["checkpoint"], created["checkpoint"]);
    assert_eq!(
        failure(
            &h.command(&["task", "checkpoint", task, "--key", "different"])
                .await
        )["code"],
        "conflict"
    );
    let snapshot = h.ok(&["checkpoint", "get", cp]).await;
    h.stop().await;
    h.restart().await;
    assert_eq!(h.ok(&["checkpoint", "get", cp]).await, snapshot);
    let restored = h.ok(&["checkpoint", "restore", cp]).await;
    assert_eq!(restored["status"], "complete");
    assert_eq!(restored["restored"], 2);
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"before\n"
    );
    assert!(!h.workspace().join("src/new.txt").exists());
    assert_eq!(
        std::fs::read(h.workspace().join("other.txt")).unwrap(),
        b"unrelated"
    );
    assert_eq!(git(&h.workspace(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read(h.workspace().join(".git/index")).unwrap(),
        index
    );
    h.stop().await;
    h.restart().await;
    std::fs::write(h.workspace().join("src/a.txt"), "external later").unwrap();
    assert_eq!(h.ok(&["checkpoint", "restore", cp]).await, restored);
    assert_eq!(h.ok(&["task", "restore", task]).await, restored);
    assert_eq!(
        std::fs::read(h.workspace().join("src/a.txt")).unwrap(),
        b"external later"
    );
    assert_eq!(
        h.ok(&["task", "checkpoint", task, "--key", "checkpoint-key"])
            .await,
        replay
    );
    let context = h.context(run_id).await;
    let sent = h
        .ok(&[
            "session",
            "send",
            context["session"]["id"].as_str().unwrap(),
            "--agent",
            context["tasks"][0]["agent_id"].as_str().unwrap(),
            "--after-turn",
            context["latest_turn"]["id"].as_str().unwrap(),
            "--message",
            "continue after checkpoint restore",
            "--key",
            "checkpoint-next",
        ])
        .await;
    let next = sent["task"]["legacy_task_id"].as_str().unwrap();
    assert_ne!(next, task);
    h.done(run_id, next).await;
    assert_eq!(h.ok(&["task", "changes", next]).await["changes"], json!([]));
    let external = h.ok(&["task", "diff", next, "--path", "other.txt"]).await;
    assert_eq!(external["diff"]["status"], "text");
    assert!(
        external["diff"]["patch"]
            .as_str()
            .unwrap()
            .contains("+unrelated")
    );
    assert_eq!(h.script.requests.lock().unwrap().len(), 4);
    assert!(!h.cwd().join("peachsh.sqlite3").exists());
    h.stop().await;
}
#[tokio::test]
async fn t02_review_bad_cli_arguments_exit_two() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["task", "diff", "task"],
        vec!["task", "diff", "task", "--path", "src/a", "--view", "bad"],
        vec!["task", "diff", "task", "--path", "../secret"],
        vec!["task", "checkpoint", "task"],
        vec!["task", "checkpoint", "task", "--key", "a,b"],
        vec!["checkpoint", "get", ".."],
        vec!["checkpoint", "restore", "bad id"],
    ] {
        let output = cli(1, dir.path(), &args).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t02_checkpoint_unknown_receipt_restart_no_retry() {
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
    let created = h
        .ok(&["task", "checkpoint", task, "--key", "restore-case"])
        .await;
    let checkpoint = created["checkpoint"]["checkpoint_id"].as_str().unwrap();
    rusqlite::Connection::open(h.db()).unwrap().execute_batch("CREATE TRIGGER fail_outcome BEFORE UPDATE OF status ON workspace_restore_outcomes WHEN NEW.status='complete' BEGIN SELECT RAISE(ABORT,'SQL-private-value'); END;").unwrap();
    let unknown = failure(&h.command(&["checkpoint", "restore", checkpoint]).await);
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
    let blocked = failure(
        &h.command(&[
            "session",
            "send",
            context["session"]["id"].as_str().unwrap(),
            "--agent",
            context["tasks"][0]["agent_id"].as_str().unwrap(),
            "--after-turn",
            context["latest_turn"]["id"].as_str().unwrap(),
            "--message",
            "must not continue unknown effects",
            "--key",
            "unknown-blocked",
        ])
        .await,
    );
    assert_eq!(blocked["code"], "conflict");
    assert_eq!(blocked["retryable"], false);
    assert_eq!(h.context(run_id).await, context);
    assert_eq!(
        h.ok(&["task", "changes", task]).await["restore"],
        unknown["receipt"]
    );
    std::fs::write(h.workspace().join("src/a.txt"), "later external edit").unwrap();
    let repeated = failure(&h.command(&["checkpoint", "restore", checkpoint]).await);
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
async fn t02_checkpoint_real_partial_receipt() {
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
    let created = h
        .ok(&["task", "checkpoint", task, "--key", "restore-case"])
        .await;
    let checkpoint = created["checkpoint"]["checkpoint_id"].as_str().unwrap();
    let db = rusqlite::Connection::open(h.db()).unwrap();
    db.execute_batch("CREATE TRIGGER pause_first_outcome BEFORE UPDATE OF status ON workspace_restore_outcomes WHEN NEW.status='complete' AND NEW.path='src/a.txt' BEGIN SELECT sum(x) FROM (WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x<10000000) SELECT x FROM seq); END;").unwrap();
    let port = h.port;
    let cwd = h.cwd();
    let id = checkpoint.to_owned();
    let request =
        tokio::spawn(async move { cli(port, &cwd, &["checkpoint", "restore", &id]).await });
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
        failure(&h.command(&["checkpoint", "restore", checkpoint]).await)["receipt"],
        partial["receipt"]
    );
    assert_eq!(
        std::fs::read(h.workspace().join("src/b.txt")).unwrap(),
        b"b-after"
    );
    h.stop().await;
}

fn checkpoint_wire() -> Value {
    json!({"checkpoint":{"checkpoint_id":"cp","task_id":"t","kind":"task_before","generation":1,"created_at":1,"manifest_digest":"a".repeat(64),"entries":[{"path":"src/a.txt","before_digest":null,"after_digest":"b".repeat(64)}]}})
}
#[tokio::test(flavor = "multi_thread")]
async fn t02_new_cli_malformed_oversize_token_redirect_single_post() {
    let cwd = tempfile::tempdir().unwrap();
    let attempts = Arc::new(Mutex::new(0usize));
    let mode = Arc::new(Mutex::new(0usize));
    let state = (attempts.clone(), mode.clone());
    let app=Router::new().route("/api/bootstrap",get(||async{Json(json!({"token":"review-private-token"}))})).route("/api/checkpoints/cp",get(||async{Json(checkpoint_wire())})).fallback(move |request:axum::extract::Request| {
        let (attempts,mode)=state.clone(); async move {
            assert_eq!(request.method(),axum::http::Method::POST); assert_eq!(request.headers()["x-peachsh-token"],"review-private-token");
            *attempts.lock().unwrap()+=1;
            match *mode.lock().unwrap() {
                0 => "not json".into_response(),
                1 => ([("content-length","1048577")],Body::from(vec![b'x';1_048_577])).into_response(),
                2 => ([("content-type","application/json")],r#"{"extra":"review-private-\u0074oken"}"#).into_response(),
                3 => (axum::http::StatusCode::TEMPORARY_REDIRECT,[("location","http://127.0.0.1:1/steal")],"redirect").into_response(),
                _ => {let stream=async_stream::stream!{yield Ok::<_,std::io::Error>(b"{".to_vec()); tokio::task::yield_now().await;yield Err(std::io::Error::other("private transport"));}; Body::from_stream(stream).into_response()},
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut count = 0;
    for current in 0..5 {
        *mode.lock().unwrap() = current;
        for args in [
            vec!["task", "checkpoint", "t", "--key", "original"],
            vec!["checkpoint", "restore", "cp"],
            vec!["task", "diff", "t", "--path", "src/a.txt"],
        ] {
            let error = failure(&cli(port, cwd.path(), &args).await);
            if args[1] != "diff" {
                assert_eq!(error["code"], "operation_result_unknown");
            } else {
                assert_ne!(error["code"], "operation_result_unknown");
            }
            assert!(!error.to_string().contains("private"));
            count += 1;
            assert_eq!(*attempts.lock().unwrap(), count);
        }
    }
    assert_eq!(std::fs::read_dir(cwd.path()).unwrap().count(), 0);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
#[tokio::test(flavor = "multi_thread")]
async fn t02_diff_status_projection_and_response_path_binding() {
    let mode = Arc::new(Mutex::new(0usize));
    let state = mode.clone();
    let app=Router::new().route("/api/bootstrap",get(||async{Json(json!({"token":"diff-token"}))})).route("/api/tasks/t/git-diff",post(move|Json(body):Json<Value>|{let mode=state.clone();async move{
        assert_eq!(body["path"],"src/a.txt"); assert_eq!(body["view"],"head");
        let n=*mode.lock().unwrap();let status=["binary","too_large","missing","unchanged","text","text"][n];
        Json(json!({"diff":{"path":if n==5{"src/other.txt"}else{"src/a.txt"},"view":"head","base_oid":null,"index_oid":null,"before_digest":null,"after_digest":null,"status":status,"patch":if status=="text"{json!("--- a/src/a.txt\n+++ b/src/a.txt\n@@ -0,0 +1 @@\n+[REDACTED]\n")}else{Value::Null},"redacted":n==4}}))
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cwd = tempfile::tempdir().unwrap();
    for n in 0..6 {
        *mode.lock().unwrap() = n;
        let output = cli(
            port,
            cwd.path(),
            &["task", "diff", "t", "--path", "src/a.txt"],
        )
        .await;
        if n == 5 {
            assert_eq!(failure(&output)["code"], "internal");
        } else {
            let value = stdout(&output);
            assert_eq!(
                value["diff"]["status"],
                ["binary", "too_large", "missing", "unchanged", "text"][n]
            );
        }
    }
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
