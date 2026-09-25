//! NEXT-04A approval CLI contract tests. Every CLI assertion executes the real binary.

use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use peachsh::{
    approval::{ApprovalStatus, ExecutionState},
    domain::*,
    engine::Engine,
    server::{self, App},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_peachsh");

fn cli(port: u16, arguments: &[&str]) -> Output {
    let mut command = Command::new(BIN);
    command
        .arg("--port")
        .arg(port.to_string())
        .args(arguments)
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "");
    command.output().unwrap()
}

fn cli_in(port: u16, arguments: &[&str], directory: &Path) -> Output {
    Command::new(BIN)
        .current_dir(directory)
        .arg("--port")
        .arg(port.to_string())
        .args(arguments)
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn port(origin: &str) -> u16 {
    origin.rsplit(':').next().unwrap().parse().unwrap()
}

struct Harness {
    _dir: TempDir,
    engine: Arc<Engine>,
    origin: String,
    provider_calls: Arc<AtomicUsize>,
    _server: tokio::task::JoinHandle<()>,
    _provider: Option<tokio::task::JoinHandle<()>>,
}

fn task(id: &str, root: &Path) -> Task {
    Task {
        id: id.into(),
        run_id: format!("run-{id}"),
        spec: TaskSpec {
            name: "worker".into(),
            role: "worker".into(),
            route_id: "route".into(),
            prompt: "begin".into(),
            depends_on: vec![],
            write_scopes: vec!["src".into()],
            tools: true,
            allow_commands: false,
            max_rounds: 4,
        },
        route: Route {
            id: "route".into(),
            name: "route".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "mock".into(),
            max_tokens: 64,
            parallel_limit: 1,
            key_env: None,
        },
        workspace: root.to_string_lossy().into(),
        status: "running".into(),
        output: String::new(),
        error: None,
        messages: vec![json!({"role":"user","content":"begin"})],
        usage: Value::Null,
        created_at: 1,
        updated_at: 1,
    }
}

fn seed_task(store: &Store, root: &Path, id: &str) -> Task {
    let task = task(id, root);
    store
        .create_run(&Run {
            id: task.run_id.clone(),
            title: "seed".into(),
            kind: SessionKind::Team,
            created_at: 1,
            tasks: vec![task.clone()],
        })
        .unwrap();
    task
}

fn card(h: &Harness, task: &mut Task, call: &str, content: &str) -> String {
    let args = json!({"path":format!("src/{call}.txt"),"content":content});
    task.messages.push(json!({
        "role":"assistant",
        "tool_calls":[{"id":call,"type":"function","function":{"name":"write_file","arguments":args.to_string()}}]
    }));
    h.engine.store.save_task(task).unwrap();
    h.engine
        .store
        .ensure_approval(task, call, "write_file", &args)
        .unwrap()
        .0
        .id
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("cli.db")).unwrap());
    let engine = Engine::new(store, 2).unwrap();
    serve(dir, engine, Arc::new(AtomicUsize::new(0)), None).await
}

async fn serve(
    dir: TempDir,
    engine: Arc<Engine>,
    provider_calls: Arc<AtomicUsize>,
    provider: Option<tokio::task::JoinHandle<()>>,
) -> Harness {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = server::router(App {
        engine: engine.clone(),
        token: "cli-host-token".into(),
        origin: origin.clone(),
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Harness {
        _dir: dir,
        engine,
        origin,
        provider_calls,
        _server: server,
        _provider: provider,
    }
}

#[derive(Clone)]
struct Script {
    groups: Arc<Mutex<VecDeque<Vec<Value>>>>,
    calls: Arc<AtomicUsize>,
}

async fn provider(State(script): State<Script>, Json(_): Json<Value>) -> impl IntoResponse {
    script.calls.fetch_add(1, Ordering::SeqCst);
    let calls = script
        .groups
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_default();
    let delta = if calls.is_empty() {
        json!({"content":"done"})
    } else {
        json!({"tool_calls":calls.into_iter().enumerate().map(|(index, mut call)| {
            call["index"] = json!(index); call
        }).collect::<Vec<_>>()})
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

fn tool_call(id: &str, path: &str) -> Value {
    json!({"id":id,"type":"function","function":{"name":"write_file","arguments":json!({"path":path,"content":id}).to_string()}})
}

async fn live_harness(path: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(provider))
        .with_state(Script {
            groups: Arc::new(Mutex::new(VecDeque::from(vec![
                vec![tool_call("write", path)],
                vec![],
            ]))),
            calls: calls.clone(),
        });
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = Arc::new(Store::open(&dir.path().join("live.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into(),
            max_concurrency: 1,
            routes: vec![Route {
                id: "route".into(),
                name: "route".into(),
                base_url: base,
                model: "mock".into(),
                max_tokens: 64,
                parallel_limit: 1,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "synthetic").unwrap();
    let engine = Engine::new(store, 1).unwrap();
    serve(dir, engine, calls, Some(provider)).await
}

async fn start_live(h: &Harness) -> Task {
    h.engine
        .start(RunRequest {
            title: "live".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "worker".into(),
                role: "worker".into(),
                route_id: "route".into(),
                prompt: "begin".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 4,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0)
}

async fn pending(h: &Harness, task_id: &str) -> peachsh::approval::ApprovalRecord {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(record) = h
                .engine
                .store
                .pending_approvals_for_task(task_id)
                .unwrap()
                .into_iter()
                .next()
            {
                break record;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn settled(h: &Harness, task_id: &str) -> Task {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let task = h.engine.store.task(task_id).unwrap();
            if !h.engine.is_busy() && !matches!(task.status.as_str(), "queued" | "running") {
                break task;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

fn event_count(h: &Harness, task: &Task, kind: &str) -> usize {
    h.engine
        .store
        .events(&task.run_id, 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn cl01_list_get_human_json_and_empty() {
    let h = harness().await;
    let mut task = seed_task(&h.engine.store, h._dir.path(), "list-task");
    let id = card(&h, &mut task, "write", "secret-body");
    let p = port(&h.origin);
    let list = cli(p, &["--json", "task", "approvals", &task.id]);
    assert!(list.status.success(), "{}", stderr(&list));
    let value: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["approvals"][0]["id"], id);
    let list_human = cli(p, &["task", "approvals", &task.id]);
    assert_eq!(stdout(&list_human), format!("{id}\tpending\tnot_started\n"));
    let get = cli(p, &["approval", "get", &id]);
    assert!(get.status.success());
    assert_eq!(stdout(&get), format!("{id}\tpending\tnot_started\n"));
    let get_json = cli(p, &["--json", "approval", "get", &id]);
    assert!(get_json.status.success());
    let actual: Value = serde_json::from_slice(&get_json.stdout).unwrap();
    let expected: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("{}/api/approvals/{id}", h.origin))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(actual, expected);
    let empty = seed_task(&h.engine.store, h._dir.path(), "empty-task");
    let empty = cli(p, &["task", "approvals", &empty.id]);
    assert_eq!(stdout(&empty), "没有审批请求\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn cl02_real_cli_approve_executes_and_deny_does_not() {
    let approved = live_harness("src/approved.txt").await;
    let task = start_live(&approved).await;
    let card = pending(&approved, &task.id).await;
    let output = cli(port(&approved.origin), &["approval", "approve", &card.id]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("\tapproved\tnot_started\n"));
    assert_eq!(settled(&approved, &task.id).await.status, "completed");
    assert_eq!(
        std::fs::read_to_string(approved._dir.path().join("src/approved.txt")).unwrap(),
        "write"
    );
    assert_eq!(event_count(&approved, &task, "approval.resolved"), 1);
    assert_eq!(
        approved
            .engine
            .store
            .approval(&card.id)
            .unwrap()
            .execution_state,
        ExecutionState::Finished
    );

    let denied = live_harness("src/denied.txt").await;
    let task = start_live(&denied).await;
    let card = pending(&denied, &task.id).await;
    let output = cli(
        port(&denied.origin),
        &["--json", "approval", "deny", &card.id],
    );
    assert!(output.status.success());
    assert_eq!(settled(&denied, &task.id).await.status, "completed");
    assert!(!denied._dir.path().join("src/denied.txt").exists());
    assert_eq!(
        denied.engine.store.approval(&card.id).unwrap().status,
        ApprovalStatus::Denied
    );
    assert_eq!(event_count(&denied, &task, "approval.resolved"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn cl03_remote_cli_does_not_initialize_local_state_or_execute_without_waiter() {
    let h = harness().await;
    let mut task = seed_task(&h.engine.store, h._dir.path(), "remote-task");
    let id = card(&h, &mut task, "write", "remote");
    let local = tempfile::tempdir().unwrap();
    let cwd = local.path().join("cwd");
    std::fs::create_dir(&cwd).unwrap();
    let output = cli_in(port(&h.origin), &["approval", "approve", &id], &cwd);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!local.path().join("data-rust").exists());
    assert_eq!(h.provider_calls.load(Ordering::SeqCst), 0);
    assert!(!h._dir.path().join("src/write.txt").exists());
    assert_eq!(
        h.engine.store.approval(&id).unwrap().execution_state,
        ExecutionState::NotStarted
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cl04_http_errors_are_json_on_stderr_with_exit_one() {
    let h = harness().await;
    let unknown = cli(port(&h.origin), &["--json", "approval", "get", "missing"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(unknown.stdout.is_empty());
    let error: Value = serde_json::from_slice(&unknown.stderr).unwrap();
    assert_eq!(
        error,
        json!({"code":"not_found","message":"审批不存在","retryable":false,"error":"审批不存在"})
    );
    let missing_task = cli(port(&h.origin), &["--json", "task", "approvals", "missing"]);
    assert_eq!(missing_task.status.code(), Some(1));
    assert!(missing_task.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&missing_task.stderr).unwrap(),
        json!({"code":"not_found","message":"任务不存在","retryable":false,"error":"任务不存在"})
    );
    let mut task = seed_task(&h.engine.store, h._dir.path(), "conflict-task");
    let id = card(&h, &mut task, "write", "x");
    assert!(
        cli(port(&h.origin), &["approval", "approve", &id])
            .status
            .success()
    );
    let conflict = cli(port(&h.origin), &["--json", "approval", "deny", &id]);
    assert_eq!(conflict.status.code(), Some(1));
    assert!(conflict.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&conflict.stderr).unwrap(),
        json!({"code":"conflict","message":"审批当前状态不能执行此操作","retryable":false,"error":"审批当前状态不能执行此操作"})
    );
    assert_eq!(event_count(&h, &task, "approval.resolved"), 1);
    let injected = cli(
        port(&h.origin),
        &["--json", "approval", "get", "../bad?query=value"],
    );
    assert_eq!(injected.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&injected.stderr).unwrap()["code"],
        "not_found"
    );
    assert_eq!(event_count(&h, &task, "approval.resolved"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn cl05_parse_errors_exit_two_without_files() {
    let dir = tempfile::tempdir().unwrap().path().join("untouched");
    let too_long = "x".repeat(129);
    for args in [
        vec!["approval", "get", "."],
        vec!["approval", "get", ".."],
        vec!["approval", "get", "bad\ncontrol"],
        vec!["approval", "get", too_long.as_str()],
        vec![
            "--data-dir",
            dir.to_str().unwrap(),
            "--check",
            "approval",
            "get",
            "valid",
        ],
        vec!["--data-dir", dir.to_str().unwrap(), "--json"],
        vec!["--port", "0", "approval", "get", "valid"],
        vec![
            "--data-dir",
            dir.to_str().unwrap(),
            "approval",
            "get",
            "valid",
        ],
    ] {
        let output = cli(1, &args);
        assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
        assert!(output.stdout.is_empty());
        assert!(!dir.exists());
    }
    let sensitive = "sk-sensitive-cli-input-123456789";
    let output = cli(1, &["approval", "get", sensitive]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!stderr(&output).contains(sensitive));

    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = seen.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = listener.local_addr().unwrap().port();
    let app = Router::new().fallback(move |uri: OriginalUri| {
        let captured = captured.clone();
        async move {
            captured.lock().unwrap().push(uri.to_string());
            (
                StatusCode::NOT_FOUND,
                Json(json!({"code":"not_found","message":"审批不存在","retryable":false,"error":"审批不存在"})),
            )
        }
    });
    let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let boundary = "b".repeat(128);
    for id in [&boundary, "a/b?c#d%2f"] {
        let output = cli(p, &["--json", "approval", "get", id]);
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    }
    let seen = seen.lock().unwrap();
    assert!(seen[0].ends_with(&boundary));
    assert!(seen[1].contains("a%2Fb%3Fc%23d%252f"), "{}", seen[1]);
}

#[derive(Clone)]
struct MockState {
    bootstrap: Value,
    decision: Value,
    decision_status: StatusCode,
    bootstrap_calls: Arc<AtomicUsize>,
    decision_calls: Arc<AtomicUsize>,
    delay_ms: u64,
    decision_delay_ms: u64,
}

async fn mock_bootstrap(State(state): State<MockState>) -> impl IntoResponse {
    state.bootstrap_calls.fetch_add(1, Ordering::SeqCst);
    if state.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(state.delay_ms)).await;
    }
    Json(state.bootstrap)
}

async fn mock_decision(State(state): State<MockState>) -> impl IntoResponse {
    state.decision_calls.fetch_add(1, Ordering::SeqCst);
    if state.decision_delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(state.decision_delay_ms)).await;
    }
    (state.decision_status, Json(state.decision))
}

async fn mock(state: MockState) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/api/bootstrap", get(mock_bootstrap))
        .route("/api/approvals/{id}/decision", post(mock_decision))
        .with_state(state);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, server)
}

fn approval(id: &str, status: &str, execution: &str) -> Value {
    json!({
        "id":id,"task_id":"task-1","tool_call_id":"call-1","tool_name":"write_file",
        "preview":"write src/a","session_id":null,"turn_id":null,"decided_by":"user",
        "status":status,"execution_state":execution,"created_at":1,"decided_at":2
    })
}

fn assert_invalid_response(output: &Output) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "internal");
    assert_eq!(error["message"], "本机服务审批响应无效");
    assert_eq!(error["error"], error["message"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn cl01_command_shape_is_fixed_for_list_get_approve_and_deny() {
    let posts = Arc::new(AtomicUsize::new(0));
    let post_count = posts.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = listener.local_addr().unwrap().port();
    let single = approval("approval-shape", "pending", "not_started");
    let app = Router::new()
        .route(
            "/api/tasks/{id}/approvals",
            get({
                let single = single.clone();
                move || {
                    let single = single.clone();
                    async move { Json(single) }
                }
            }),
        )
        .route(
            "/api/approvals/{id}",
            get(|| async { Json(json!({"approvals":[]})) }),
        )
        .route(
            "/api/bootstrap",
            get(|| async { Json(json!({"token":"shape-token"})) }),
        )
        .route(
            "/api/approvals/{id}/decision",
            post(move || {
                let post_count = post_count.clone();
                async move {
                    post_count.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"approvals":[]}))
                }
            }),
        );
    let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    assert_invalid_response(&cli(p, &["--json", "task", "approvals", "task-shape"]));
    assert_invalid_response(&cli(p, &["--json", "approval", "get", "approval-shape"]));
    assert_invalid_response(&cli(
        p,
        &["--json", "approval", "approve", "approval-shape"],
    ));
    assert_invalid_response(&cli(p, &["--json", "approval", "deny", "approval-shape"]));
    assert_eq!(posts.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn cl09_escaped_token_is_rejected_in_success_and_error_fields() {
    let token = "quoted\"and\\backslash-token";
    let boot = Arc::new(AtomicUsize::new(0));
    let posts = Arc::new(AtomicUsize::new(0));
    let mut success = approval("approval-token", "approved", "unknown");
    success["preview"] = json!(format!("preview {token}"));
    let (p, _server) = mock(MockState {
        bootstrap: json!({"token":token}),
        decision: success,
        decision_status: StatusCode::OK,
        bootstrap_calls: boot,
        decision_calls: posts.clone(),
        delay_ms: 0,
        decision_delay_ms: 0,
    })
    .await;
    for mode in [
        vec!["approval", "approve", "approval-token"],
        vec!["--json", "approval", "approve", "approval-token"],
    ] {
        let output = cli(p, &mode);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!stderr(&output).contains(token));
    }
    assert_eq!(posts.load(Ordering::SeqCst), 2);

    let boot = Arc::new(AtomicUsize::new(0));
    let posts = Arc::new(AtomicUsize::new(0));
    let mut nested = serde_json::Map::new();
    nested.insert(token.to_owned(), json!({"nested":token}));
    let error = json!({
        "code":"conflict","message":"safe conflict","retryable":false,"error":"safe conflict",
        "extra": Value::Object(nested)
    });
    let (p, _server) = mock(MockState {
        bootstrap: json!({"token":token}),
        decision: error,
        decision_status: StatusCode::CONFLICT,
        bootstrap_calls: boot,
        decision_calls: posts.clone(),
        delay_ms: 0,
        decision_delay_ms: 0,
    })
    .await;
    for mode in [
        vec!["approval", "deny", "approval-token"],
        vec!["--json", "approval", "deny", "approval-token"],
    ] {
        let output = cli(p, &mode);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!stderr(&output).contains(token));
    }
    assert_eq!(posts.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn cl06_invalid_bootstrap_json_and_connection_are_safe_without_post_retry() {
    let boot = Arc::new(AtomicUsize::new(0));
    let post_calls = Arc::new(AtomicUsize::new(0));
    let (p, _server) = mock(MockState {
        bootstrap: json!({"wrong":"mock-secret-body"}),
        decision: approval("a", "approved", "unknown"),
        decision_status: StatusCode::OK,
        bootstrap_calls: boot.clone(),
        decision_calls: post_calls.clone(),
        delay_ms: 0,
        decision_delay_ms: 0,
    })
    .await;
    let invalid = cli(p, &["--json", "approval", "approve", "valid"]);
    assert_eq!(invalid.status.code(), Some(1));
    assert!(invalid.stdout.is_empty());
    assert!(!stderr(&invalid).contains("mock-secret-body"));
    assert_eq!(boot.load(Ordering::SeqCst), 1);
    assert_eq!(post_calls.load(Ordering::SeqCst), 0);
    let dead = cli(1, &["--json", "approval", "get", "valid"]);
    assert_eq!(dead.status.code(), Some(1));
    assert!(!stderr(&dead).contains("http://"));

    let boot = Arc::new(AtomicUsize::new(0));
    let post = Arc::new(AtomicUsize::new(0));
    let (p, _server) = mock(MockState {
        bootstrap: json!({"token":"timeout-token"}),
        decision: approval("valid", "approved", "not_started"),
        decision_status: StatusCode::OK,
        bootstrap_calls: boot,
        decision_calls: post.clone(),
        delay_ms: 0,
        decision_delay_ms: 5_000,
    })
    .await;
    let timeout = cli(p, &["--json", "approval", "approve", "valid"]);
    let error: Value = serde_json::from_slice(&timeout.stderr).unwrap();
    assert_eq!(error["code"], "decision_result_unknown");
    assert_eq!(error["retryable"], false);
    assert_eq!(post.load(Ordering::SeqCst), 1);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let invalid_port = listener.local_addr().unwrap().port();
    let app = Router::new().route(
        "/api/bootstrap",
        get(|| async {
            (
                [("content-type", "application/json")],
                "{mock-secret-invalid",
            )
        }),
    );
    let _invalid = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let invalid = cli(invalid_port, &["--json", "approval", "approve", "valid"]);
    assert_eq!(invalid.status.code(), Some(1));
    assert!(!stderr(&invalid).contains("mock-secret-invalid"));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let large_port = listener.local_addr().unwrap().port();
    let app = Router::new().route("/api/bootstrap", get(|| async { "x".repeat(1_048_577) }));
    let _large = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let large = cli(large_port, &["--json", "approval", "approve", "valid"]);
    assert_eq!(large.status.code(), Some(1));
    assert!(stderr(&large).contains("响应过大"));
}

#[tokio::test(flavor = "multi_thread")]
async fn cl07_redirects_proxy_and_token_echo_do_not_leak_or_retry() {
    let boot = Arc::new(AtomicUsize::new(0));
    let post_calls = Arc::new(AtomicUsize::new(0));
    let token = "bootstrap-super-secret";
    let (p, _server) = mock(MockState {
        bootstrap: json!({"token":token}),
        decision: json!({"code":"internal","message":token,"retryable":false,"error":token}),
        decision_status: StatusCode::INTERNAL_SERVER_ERROR,
        bootstrap_calls: boot.clone(),
        decision_calls: post_calls.clone(),
        delay_ms: 0,
        decision_delay_ms: 0,
    })
    .await;
    let output = cli(p, &["--json", "approval", "approve", "valid"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!stderr(&output).contains(token));
    assert_eq!(
        post_calls.load(Ordering::SeqCst),
        1,
        "decision POST is never retried"
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirect_port = listener.local_addr().unwrap().port();
    let app = Router::new().fallback(|| async {
        (
            StatusCode::FOUND,
            [("location", "http://127.0.0.1:1/stolen")],
        )
    });
    let _redirect = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let redirected = cli(redirect_port, &["--json", "approval", "approve", "valid"]);
    assert_eq!(redirected.status.code(), Some(1));
    assert!(!stderr(&redirected).contains("stolen"));

    let stolen = Arc::new(AtomicUsize::new(0));
    let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_port = target_listener.local_addr().unwrap().port();
    let target_count = stolen.clone();
    let target = Router::new().fallback(move || {
        let count = target_count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }
    });
    let _target = tokio::spawn(async move { axum::serve(target_listener, target).await.unwrap() });
    let redirect_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let post_redirect_port = redirect_listener.local_addr().unwrap().port();
    let location = format!("http://127.0.0.1:{target_port}/stolen");
    let app = Router::new()
        .route(
            "/api/bootstrap",
            get(|| async { Json(json!({"token":"redirect-token"})) }),
        )
        .route(
            "/api/approvals/{id}/decision",
            post(move || {
                let location = location.clone();
                async move { (StatusCode::TEMPORARY_REDIRECT, [("location", location)]) }
            }),
        );
    let _post_redirect =
        tokio::spawn(async move { axum::serve(redirect_listener, app).await.unwrap() });
    let redirected = cli(
        post_redirect_port,
        &["--json", "approval", "approve", "valid"],
    );
    assert_eq!(redirected.status.code(), Some(1));
    assert_eq!(stolen.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn cl08_check_and_server_startup_remain_compatible() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let legacy = dir.path().join("legacy");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let check = Command::new(BIN)
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "--legacy-data",
            legacy.to_str().unwrap(),
            "--workspace",
            workspace.to_str().unwrap(),
            "--check",
        ])
        .output()
        .unwrap();
    assert!(check.status.success(), "{}", stderr(&check));
    assert!(data.join("peachsh.sqlite3").exists());

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = listener.local_addr().unwrap().port();
    drop(listener);
    let server_data = dir.path().join("server-data");
    let prepare = Command::new(BIN)
        .args([
            "--data-dir",
            server_data.to_str().unwrap(),
            "--legacy-data",
            legacy.to_str().unwrap(),
            "--workspace",
            workspace.to_str().unwrap(),
            "--check",
        ])
        .output()
        .unwrap();
    assert!(prepare.status.success());
    let approval_id = {
        let store = Store::open(&server_data.join("peachsh.sqlite3")).unwrap();
        let mut task = seed_task(&store, &workspace, "locked-task");
        let args = json!({"path":"src/locked.txt","content":"locked"});
        task.messages.push(json!({
            "role":"assistant",
            "tool_calls":[{"id":"locked-call","type":"function","function":{"name":"write_file","arguments":args.to_string()}}]
        }));
        store.save_task(&task).unwrap();
        store
            .ensure_approval(&task, "locked-call", "write_file", &args)
            .unwrap()
            .0
            .id
    };
    let mut child = Command::new(BIN)
        .args([
            "--port",
            &p.to_string(),
            "--data-dir",
            server_data.to_str().unwrap(),
            "--legacy-data",
            legacy.to_str().unwrap(),
            "--workspace",
            workspace.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if client
                .get(format!("http://127.0.0.1:{p}/api/health"))
                .send()
                .await
                .is_ok()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let second_port = if p == u16::MAX { p - 1 } else { p + 1 };
    let second = Command::new(BIN)
        .args([
            "--port",
            &second_port.to_string(),
            "--data-dir",
            server_data.to_str().unwrap(),
            "--legacy-data",
            legacy.to_str().unwrap(),
            "--workspace",
            workspace.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(
        stderr(&second).contains("此数据目录已有一个 🍑sh 实例正在运行"),
        "{}",
        stderr(&second)
    );
    let connected = cli(p, &["task", "approvals", "locked-task"]);
    assert!(connected.status.success(), "{}", stderr(&connected));
    assert_eq!(
        stdout(&connected),
        format!("{approval_id}\tpending\tnot_started\n")
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cl09_unknown_state_is_truthful_and_help_has_no_secrets() {
    let boot = Arc::new(AtomicUsize::new(0));
    let post = Arc::new(AtomicUsize::new(0));
    let (p, _server) = mock(MockState {
        bootstrap: json!({"token":"hidden-token"}),
        decision: approval("approval-1", "approved", "unknown"),
        decision_status: StatusCode::OK,
        bootstrap_calls: boot,
        decision_calls: post,
        delay_ms: 0,
        decision_delay_ms: 0,
    })
    .await;
    let output = cli(p, &["approval", "approve", "approval-1"]);
    assert!(output.status.success());
    assert_eq!(stdout(&output), "approval-1\tapproved\tunknown\n");
    assert!(!stdout(&output).contains("成功执行"));
    assert!(!stdout(&output).contains("hidden-token"));
    let help = cli(p, &["--help"]);
    assert!(help.status.success());
    assert!(!stdout(&help).contains("hidden-token"));

    let boot = Arc::new(AtomicUsize::new(0));
    let post = Arc::new(AtomicUsize::new(0));
    let mut leaked = approval("approval-2", "approved", "unknown");
    leaked["preview"] = json!("hidden-success-token");
    let (p, _server) = mock(MockState {
        bootstrap: json!({"token":"hidden-success-token"}),
        decision: leaked,
        decision_status: StatusCode::OK,
        bootstrap_calls: boot,
        decision_calls: post,
        delay_ms: 0,
        decision_delay_ms: 0,
    })
    .await;
    let output = cli(p, &["--json", "approval", "approve", "approval-2"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!stderr(&output).contains("hidden-success-token"));

    let boot = Arc::new(AtomicUsize::new(0));
    let post = Arc::new(AtomicUsize::new(0));
    let mut extended = approval("approval-3", "approved", "unknown");
    extended["future_field"] = json!("benign");
    let (p, _server) = mock(MockState {
        bootstrap: json!({"token":"plain-token"}),
        decision: extended,
        decision_status: StatusCode::OK,
        bootstrap_calls: boot,
        decision_calls: post,
        delay_ms: 0,
        decision_delay_ms: 0,
    })
    .await;
    let output = cli(p, &["--json", "approval", "approve", "approval-3"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap()
            .get("future_field")
            .is_none()
    );
}
