//! NEXT-02D approval HTTP contract tests.

use axum::{Json, Router, extract::State, routing::post};
use peachsh::{
    approval::{ApprovalStatus, ExecutionState},
    domain::*,
    engine::Engine,
    server::{self, App},
    store::Store,
};
use reqwest::{Client, Response, StatusCode};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, VecDeque},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;

struct Harness {
    _dir: TempDir,
    db_path: std::path::PathBuf,
    engine: Arc<Engine>,
    origin: String,
    token: String,
    provider_calls: Arc<AtomicUsize>,
    _server: tokio::task::JoinHandle<()>,
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
            allow_commands: true,
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

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("approval-http.db");
    let store = Arc::new(Store::open(&db_path).unwrap());
    let engine = Engine::new(store, 2).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let token = "approval-http-token".to_owned();
    let app = server::router(App {
        engine: engine.clone(),
        token: token.clone(),
        origin: origin.clone(),
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Harness {
        _dir: dir,
        db_path,
        engine,
        origin,
        token,
        provider_calls: Arc::new(AtomicUsize::new(0)),
        _server: server,
    }
}

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(12))
        .build()
        .unwrap()
}

fn assert_security(response: &Response) {
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'self'")
    );
}

async fn error(response: Response, status: StatusCode, retryable: bool) -> Value {
    assert_eq!(response.status(), status);
    assert_security(&response);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], body["message"]);
    assert_eq!(body["retryable"], retryable);
    body
}

struct SseFrame {
    id: i64,
    event: String,
    data: Value,
}

async fn next_sse(response: &mut Response, buffer: &mut String) -> SseFrame {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            while let Some(end) = buffer.find("\n\n") {
                let block = buffer[..end].replace("\r", "");
                buffer.drain(..end + 2);
                if block
                    .lines()
                    .all(|line| line.starts_with(':') || line.is_empty())
                {
                    continue;
                }
                let mut id = None;
                let mut event = None;
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(value) = line.strip_prefix("id:") {
                        id = Some(value.trim().parse::<i64>().unwrap());
                    } else if let Some(value) = line.strip_prefix("event:") {
                        event = Some(value.trim().to_owned());
                    } else if let Some(value) = line.strip_prefix("data:") {
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(value.trim_start());
                    }
                }
                if let (Some(id), Some(event)) = (id, event) {
                    return SseFrame {
                        id,
                        event,
                        data: serde_json::from_str(&data).unwrap(),
                    };
                }
            }
            let chunk = response.chunk().await.unwrap().expect("SSE ended");
            buffer.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    })
    .await
    .expect("SSE frame timeout")
}

fn card(h: &Harness, task: &mut Task, call: &str, name: &str, args: Value) -> String {
    task.messages.push(json!({
        "role":"assistant",
        "tool_calls":[{"id":call,"type":"function","function":{"name":name,"arguments":args.to_string()}}]
    }));
    h.engine.store.save_task(task).unwrap();
    h.engine
        .store
        .ensure_approval(task, call, name, &args)
        .unwrap()
        .0
        .id
}

fn event_count(h: &Harness, task: &Task, kind: &str) -> usize {
    h.engine
        .store
        .events(&task.run_id, 0)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == kind)
        .count()
}

#[derive(Clone)]
struct Script {
    groups: Arc<Mutex<VecDeque<Vec<Value>>>>,
    calls: Arc<AtomicUsize>,
}

fn tool_call(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}

async fn provider(
    State(script): State<Script>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
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
            call["index"] = json!(index);
            call
        }).collect::<Vec<_>>()})
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

async fn provider_server(
    groups: Vec<Vec<Value>>,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/v1/chat/completions", post(provider))
        .with_state(Script {
            groups: Arc::new(Mutex::new(VecDeque::from(groups))),
            calls: calls.clone(),
        });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, calls, server)
}

fn configure_live(store: &Store, root: &Path, base: &str) {
    store
        .save_settings(&Settings {
            workspace: root.to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "route".into(),
                base_url: base.into(),
                model: "mock".into(),
                max_tokens: 64,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "synthetic-key").unwrap();
}

async fn live_harness(groups: Vec<Vec<Value>>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("approval-http.db");
    let (base, provider_calls, _provider) = provider_server(groups).await;
    let store = Arc::new(Store::open(&db_path).unwrap());
    configure_live(&store, dir.path(), &base);
    let engine = Engine::new(store, 2).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let token = "approval-http-token".to_owned();
    let app = server::router(App {
        engine: engine.clone(),
        token: token.clone(),
        origin: origin.clone(),
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Harness {
        _dir: dir,
        db_path,
        engine,
        origin,
        token,
        provider_calls,
        _server: server,
    }
}

async fn start_live(h: &Harness, commands: bool) -> Task {
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
                allow_commands: commands,
                max_rounds: 6,
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
    .expect("pending approval")
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
    .expect("settled task")
}

async fn http_decide(h: &Harness, id: &str, decision: &str) -> Response {
    client()
        .post(format!("{}/api/approvals/{id}/decision", h.origin))
        .header("x-peachsh-token", &h.token)
        .json(&json!({"decision":decision}))
        .send()
        .await
        .unwrap()
}

async fn serve_http_engine(engine: Arc<Engine>) -> (String, String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let token = "restart-token".to_owned();
    let app = server::router(App {
        engine,
        token: token.clone(),
        origin: origin.clone(),
    });
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (origin, token, handle)
}

async fn pending_engine(engine: &Engine, task_id: &str) -> peachsh::approval::ApprovalRecord {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(record) = engine
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
    .expect("pending approval")
}

async fn settled_engine(engine: &Engine, task_id: &str) -> Task {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let task = engine.store.task(task_id).unwrap();
            if !engine.is_busy() && !matches!(task.status.as_str(), "queued" | "running") {
                break task;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("settled task")
}

fn run_request(commands: bool) -> RunRequest {
    RunRequest {
        title: "restart".into(),
        kind: SessionKind::Team,
        tasks: vec![TaskSpec {
            name: "worker".into(),
            role: "worker".into(),
            route_id: "route".into(),
            prompt: "begin".into(),
            depends_on: vec![],
            write_scopes: vec!["src".into()],
            tools: true,
            allow_commands: commands,
            max_rounds: 6,
        }],
    }
}

#[tokio::test]
async fn ah02_ah03_ah04_real_tools_follow_http_decisions() {
    let h = live_harness(vec![
        vec![tool_call(
            "write",
            "write_file",
            json!({"path":"src/approved.txt","content":"approved"}),
        )],
        vec![],
    ])
    .await;
    let task = start_live(&h, false).await;
    let approval = pending(&h, &task.id).await;
    assert_eq!(h.provider_calls.load(Ordering::SeqCst), 1);
    assert!(!h._dir.path().join("src/approved.txt").exists());
    assert_eq!(event_count(&h, &task, "file_backup"), 0);
    assert_eq!(event_count(&h, &task, "tool_start"), 0);
    assert_eq!(event_count(&h, &task, "tool_result"), 0);
    let pending_list: Value = client()
        .get(format!("{}/api/tasks/{}/approvals", h.origin, task.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending_list["approvals"][0]["id"], approval.id);
    assert_eq!(pending_list["approvals"][0]["status"], "pending");
    let pending_single: Value = client()
        .get(format!("{}/api/approvals/{}", h.origin, approval.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending_single["execution_state"], "not_started");
    assert_eq!(
        http_decide(&h, &approval.id, "approve").await.status(),
        StatusCode::OK
    );
    let done = settled(&h, &task.id).await;
    assert_eq!(h.provider_calls.load(Ordering::SeqCst), 2);
    assert_eq!(done.status, "completed");
    assert_eq!(
        std::fs::read_to_string(h._dir.path().join("src/approved.txt")).unwrap(),
        "approved"
    );
    assert_eq!(
        h.engine
            .store
            .approval(&approval.id)
            .unwrap()
            .execution_state,
        ExecutionState::Finished
    );
    assert_eq!(event_count(&h, &task, "approval.resolved"), 1);
    assert_eq!(event_count(&h, &task, "tool_result"), 1);

    let denied = live_harness(vec![
        vec![tool_call(
            "write",
            "write_file",
            json!({"path":"src/denied.txt","content":"denied"}),
        )],
        vec![],
    ])
    .await;
    let denied_task = start_live(&denied, false).await;
    let denial = pending(&denied, &denied_task.id).await;
    assert_eq!(
        http_decide(&denied, &denial.id, "deny").await.status(),
        StatusCode::OK
    );
    assert_eq!(settled(&denied, &denied_task.id).await.status, "completed");
    assert!(!denied._dir.path().join("src/denied.txt").exists());
    assert_eq!(
        denied.engine.store.approval(&denial.id).unwrap().status,
        ApprovalStatus::Denied
    );

    let command = live_harness(vec![
        vec![tool_call(
            "cmd",
            "run_command",
            json!({"command":"Add-Content -LiteralPath counter.txt -Value one"}),
        )],
        vec![],
    ])
    .await;
    let command_task = start_live(&command, true).await;
    let command_card = pending(&command, &command_task.id).await;
    assert!(!command._dir.path().join("counter.txt").exists());
    assert_eq!(
        http_decide(&command, &command_card.id, "approve")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        settled(&command, &command_task.id).await.status,
        "completed"
    );
    assert_eq!(
        std::fs::read_to_string(command._dir.path().join("counter.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );

    let command_denied = live_harness(vec![
        vec![tool_call(
            "cmd",
            "run_command",
            json!({"command":"Add-Content -LiteralPath denied-counter.txt -Value one"}),
        )],
        vec![],
    ])
    .await;
    let denied_task = start_live(&command_denied, true).await;
    let denied_card = pending(&command_denied, &denied_task.id).await;
    assert_eq!(
        http_decide(&command_denied, &denied_card.id, "deny")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        settled(&command_denied, &denied_task.id).await.status,
        "completed"
    );
    assert!(
        !command_denied
            ._dir
            .path()
            .join("denied-counter.txt")
            .exists()
    );

    let read = live_harness(vec![
        vec![tool_call("read", "list_files", json!({"path":"."}))],
        vec![],
    ])
    .await;
    let read_task = start_live(&read, false).await;
    assert_eq!(settled(&read, &read_task.id).await.status, "completed");
    assert!(
        read.engine
            .store
            .approvals_for_task(&read_task.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(event_count(&read, &read_task, "approval.requested"), 0);
}

#[tokio::test]
async fn ah07_cancel_before_claim_and_after_real_command_claim() {
    let cancelled = live_harness(vec![vec![tool_call(
        "write",
        "write_file",
        json!({"path":"src/cancelled.txt","content":"cancelled"}),
    )]])
    .await;
    let task = start_live(&cancelled, false).await;
    let approval = pending(&cancelled, &task.id).await;
    let response = client()
        .post(format!("{}/api/tasks/{}/cancel", cancelled.origin, task.id))
        .header("x-peachsh-token", &cancelled.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let record = cancelled.engine.store.approval(&approval.id).unwrap();
    assert_eq!(record.status, ApprovalStatus::Cancelled);
    assert_eq!(record.execution_state, ExecutionState::Cancelled);
    error(
        http_decide(&cancelled, &approval.id, "approve").await,
        StatusCode::CONFLICT,
        false,
    )
    .await;
    assert_eq!(settled(&cancelled, &task.id).await.status, "cancelled");
    assert!(!cancelled._dir.path().join("src/cancelled.txt").exists());
    assert_eq!(event_count(&cancelled, &task, "approval.resolved"), 1);
    assert_eq!(event_count(&cancelled, &task, "tool_start"), 0);

    let claimed = live_harness(vec![vec![
        tool_call(
            "command",
            "run_command",
            json!({"command":"Add-Content -LiteralPath counter.txt -Value one; while (-not (Test-Path -LiteralPath release.txt)) { Start-Sleep -Milliseconds 10 }; Write-Output done"}),
        ),
        tool_call(
            "next",
            "write_file",
            json!({"path":"src/next.txt","content":"next"}),
        ),
    ]])
    .await;
    let task = start_live(&claimed, true).await;
    let approval = pending(&claimed, &task.id).await;
    assert_eq!(
        http_decide(&claimed, &approval.id, "approve")
            .await
            .status(),
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let count = std::fs::read_to_string(claimed._dir.path().join("counter.txt"))
                .ok()
                .map(|text| text.lines().count())
                .unwrap_or(0);
            let state = claimed
                .engine
                .store
                .approval(&approval.id)
                .unwrap()
                .execution_state;
            if count == 1 && state == ExecutionState::Claimed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("real command crossed claim and wrote once");
    let response = client()
        .post(format!("{}/api/tasks/{}/cancel", claimed.origin, task.id))
        .header("x-peachsh-token", &claimed.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        claimed.engine.store.approval(&approval.id).unwrap().status,
        ApprovalStatus::Approved
    );
    std::fs::write(claimed._dir.path().join("release.txt"), "release").unwrap();
    assert_eq!(settled(&claimed, &task.id).await.status, "cancelled");
    assert_eq!(
        claimed
            .engine
            .store
            .approval(&approval.id)
            .unwrap()
            .execution_state,
        ExecutionState::Finished
    );
    assert_eq!(
        std::fs::read_to_string(claimed._dir.path().join("counter.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(!claimed._dir.path().join("src/next.txt").exists());
    assert_eq!(event_count(&claimed, &task, "tool_result"), 1);
    assert_eq!(event_count(&claimed, &task, "approval.resolved"), 1);
}

#[tokio::test]
async fn ah07_barrier_cancel_and_http_decision_have_one_durable_resolution() {
    for round in 0..8 {
        let h = harness().await;
        let mut task = seed_task(&h.engine.store, h._dir.path(), &format!("race-{round}"));
        let id = card(
            &h,
            &mut task,
            "write",
            "write_file",
            json!({"path":"src/race.txt","content":"race"}),
        );
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let cancel_url = format!("{}/api/tasks/{}/cancel", h.origin, task.id);
        let decision_url = format!("{}/api/approvals/{id}/decision", h.origin);
        let cancel_token = h.token.clone();
        let decision_token = h.token.clone();
        let cancel_barrier = barrier.clone();
        let cancel = tokio::spawn(async move {
            cancel_barrier.wait().await;
            client()
                .post(cancel_url)
                .header("x-peachsh-token", cancel_token)
                .send()
                .await
                .unwrap()
                .status()
        });
        let decision_barrier = barrier.clone();
        let decision = tokio::spawn(async move {
            decision_barrier.wait().await;
            client()
                .post(decision_url)
                .header("x-peachsh-token", decision_token)
                .json(&json!({"decision":"approve"}))
                .send()
                .await
                .unwrap()
                .status()
        });
        barrier.wait().await;
        assert_eq!(cancel.await.unwrap(), StatusCode::OK);
        let decision_status = decision.await.unwrap();
        assert!(matches!(
            decision_status,
            StatusCode::OK | StatusCode::CONFLICT
        ));
        let record = h.engine.store.approval(&id).unwrap();
        match record.status {
            ApprovalStatus::Cancelled => {
                assert_eq!(decision_status, StatusCode::CONFLICT);
                assert_eq!(record.execution_state, ExecutionState::Cancelled);
            }
            ApprovalStatus::Approved => {
                assert_eq!(decision_status, StatusCode::OK);
                assert_eq!(record.execution_state, ExecutionState::Cancelled);
            }
            other => panic!("unexpected race result {other:?}"),
        }
        assert_eq!(event_count(&h, &task, "approval.resolved"), 1);
        assert_eq!(event_count(&h, &task, "tool_start"), 0);
        assert_eq!(event_count(&h, &task, "tool_result"), 0);
        assert!(!h._dir.path().join("src/race.txt").exists());
    }
}

#[tokio::test]
async fn ah10_real_workers_with_same_call_id_do_not_borrow_decisions() {
    let h = live_harness(vec![
        vec![tool_call(
            "shared",
            "write_file",
            json!({"path":"src/first.txt","content":"first"}),
        )],
        vec![],
        vec![tool_call(
            "shared",
            "write_file",
            json!({"path":"src/second.txt","content":"second"}),
        )],
        vec![],
    ])
    .await;
    let first = start_live(&h, false).await;
    let first_card = pending(&h, &first.id).await;
    assert_eq!(first_card.tool_call_id, "shared");
    assert_eq!(
        http_decide(&h, &first_card.id, "approve").await.status(),
        StatusCode::OK
    );
    assert_eq!(settled(&h, &first.id).await.status, "completed");
    let second = start_live(&h, false).await;
    let second_card = pending(&h, &second.id).await;
    assert_eq!(second_card.tool_call_id, "shared");
    assert_ne!(first_card.id, second_card.id);
    assert_eq!(
        http_decide(&h, &second_card.id, "deny").await.status(),
        StatusCode::OK
    );
    assert_eq!(settled(&h, &second.id).await.status, "completed");
    assert!(h._dir.path().join("src/first.txt").exists());
    assert!(!h._dir.path().join("src/second.txt").exists());
    let first_list: Value = client()
        .get(format!("{}/api/tasks/{}/approvals", h.origin, first.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_list: Value = client()
        .get(format!("{}/api/tasks/{}/approvals", h.origin, second.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first_list["approvals"][0]["id"], first_card.id);
    assert_eq!(second_list["approvals"][0]["id"], second_card.id);
}

#[tokio::test]
async fn ah13_run_and_session_sse_resume_from_last_consumed_cursor() {
    let h = live_harness(vec![
        vec![tool_call(
            "write",
            "write_file",
            json!({"path":"src/sse.txt","content":"sse"}),
        )],
        vec![],
    ])
    .await;
    let task = start_live(&h, false).await;
    let approval = pending(&h, &task.id).await;
    let session_id: String = Connection::open(&h.db_path)
        .unwrap()
        .query_row(
            "SELECT id FROM sessions WHERE legacy_run_id=?1",
            [&task.run_id],
            |row| row.get(0),
        )
        .unwrap();

    let mut run_stream = client()
        .get(format!("{}/api/runs/{}/events", h.origin, task.run_id))
        .send()
        .await
        .unwrap();
    let mut run_buffer = String::new();
    let run_requested = loop {
        let frame = next_sse(&mut run_stream, &mut run_buffer).await;
        if frame.event == "approval.requested" {
            break frame;
        }
    };
    assert_eq!(
        run_requested.id,
        run_requested.data["seq"].as_i64().unwrap()
    );
    assert_eq!(run_requested.data["cursor"], run_requested.id.to_string());
    assert_eq!(run_requested.data["task_id"], task.id);
    assert_eq!(
        run_requested.data["session_id"],
        json!(&approval.session_id)
    );
    assert_eq!(run_requested.data["turn_id"], json!(&approval.turn_id));
    assert_eq!(run_requested.data["data"]["approval_id"], approval.id);

    let mut session_stream = client()
        .get(format!("{}/api/sessions/{session_id}/events", h.origin))
        .send()
        .await
        .unwrap();
    let mut session_buffer = String::new();
    let session_requested = loop {
        let frame = next_sse(&mut session_stream, &mut session_buffer).await;
        if frame.event == "approval.requested" {
            break frame;
        }
    };
    assert_eq!(
        session_requested.id,
        session_requested.data["seq"].as_i64().unwrap()
    );
    assert_eq!(
        session_requested.data["cursor"],
        session_requested.id.to_string()
    );
    assert_eq!(session_requested.data["task_id"], task.id);
    assert_eq!(session_requested.data["session_id"], session_id);
    assert_eq!(session_requested.data["turn_id"], json!(&approval.turn_id));
    assert_eq!(session_requested.data["data"]["approval_id"], approval.id);
    drop(run_stream);
    drop(session_stream);
    assert_eq!(
        http_decide(&h, &approval.id, "approve").await.status(),
        StatusCode::OK
    );

    let mut run_resume = client()
        .get(format!(
            "{}/api/runs/{}/events?after={}",
            h.origin, task.run_id, run_requested.id
        ))
        .send()
        .await
        .unwrap();
    let mut run_resume_buffer = String::new();
    let run_resolved = loop {
        let frame = next_sse(&mut run_resume, &mut run_resume_buffer).await;
        assert!(
            frame.id > run_requested.id,
            "consumed requested event replayed"
        );
        if frame.event == "approval.resolved" {
            break frame;
        }
    };
    assert_eq!(run_resolved.id, run_resolved.data["seq"].as_i64().unwrap());
    assert_eq!(run_resolved.data["cursor"], run_resolved.id.to_string());
    assert_eq!(run_resolved.data["task_id"], task.id);
    assert_eq!(run_resolved.data["session_id"], json!(&approval.session_id));
    assert_eq!(run_resolved.data["turn_id"], json!(&approval.turn_id));
    assert_eq!(run_resolved.data["data"]["approval_id"], approval.id);

    let mut session_resume = client()
        .get(format!("{}/api/sessions/{session_id}/events", h.origin))
        .header("last-event-id", session_requested.id.to_string())
        .send()
        .await
        .unwrap();
    let mut session_resume_buffer = String::new();
    let session_resolved = loop {
        let frame = next_sse(&mut session_resume, &mut session_resume_buffer).await;
        assert!(
            frame.id > session_requested.id,
            "consumed session event replayed"
        );
        if frame.event == "approval.resolved" {
            break frame;
        }
    };
    assert_eq!(
        session_resolved.id,
        session_resolved.data["seq"].as_i64().unwrap()
    );
    assert_eq!(
        session_resolved.data["cursor"],
        session_resolved.id.to_string()
    );
    assert_eq!(session_resolved.data["task_id"], task.id);
    assert_eq!(session_resolved.data["session_id"], session_id);
    assert_eq!(session_resolved.data["turn_id"], json!(&approval.turn_id));
    assert_eq!(session_resolved.data["data"]["approval_id"], approval.id);
    assert_eq!(run_resolved.id, session_resolved.id);
    drop(run_resume);
    drop(session_resume);
    assert_eq!(settled(&h, &task.id).await.status, "completed");
    assert_eq!(
        std::fs::read_to_string(h._dir.path().join("src/sse.txt")).unwrap(),
        "sse"
    );
    assert_eq!(event_count(&h, &task, "approval.resolved"), 1);
}

#[tokio::test]
async fn ah01_ah09_ah10_safe_dto_order_and_task_isolation() {
    let h = harness().await;
    let mut a = seed_task(&h.engine.store, h._dir.path(), "task-a");
    let mut b = seed_task(&h.engine.store, h._dir.path(), "task-b");
    let secret = "mock-http-secret";
    let a1 = card(
        &h,
        &mut a,
        "same-call",
        "write_file",
        json!({"path":"src/a.txt","content":secret}),
    );
    let a2 = card(
        &h,
        &mut a,
        "command",
        "run_command",
        json!({"command":format!("Bearer {secret} {}", "界".repeat(200))}),
    );
    let b1 = card(
        &h,
        &mut b,
        "same-call",
        "write_file",
        json!({"path":"src/b.txt","content":"body"}),
    );

    let response = client()
        .get(format!("{}/api/tasks/{}/approvals", h.origin, a.id))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_security(&response);
    let body: Value = response.json().await.unwrap();
    let approvals = body["approvals"].as_array().unwrap();
    assert_eq!(approvals.len(), 2);
    let expected: Vec<_> = h
        .engine
        .store
        .approvals_for_task(&a.id)
        .unwrap()
        .into_iter()
        .map(|record| record.id)
        .collect();
    let actual: Vec<_> = approvals
        .iter()
        .map(|record| record["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        actual, expected,
        "HTTP must preserve Store (created_at,id) order"
    );
    let command = approvals.iter().find(|record| record["id"] == a2).unwrap();
    assert!(command["preview"].as_str().unwrap().chars().count() <= 160);
    let fields: BTreeSet<_> = approvals[0].as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        fields,
        [
            "created_at",
            "decided_at",
            "decided_by",
            "execution_state",
            "id",
            "preview",
            "session_id",
            "status",
            "task_id",
            "tool_call_id",
            "tool_name",
            "turn_id",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    assert!(!body.to_string().contains(secret));
    for forbidden in [
        "args_digest",
        "binding_digest",
        "workspace",
        "write_scopes",
        "allow_commands",
    ] {
        assert!(!body.to_string().contains(forbidden));
    }

    let single: Value = client()
        .get(format!("{}/api/approvals/{a1}", h.origin))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(single["task_id"], a.id);
    let other: Value = client()
        .get(format!("{}/api/tasks/{}/approvals", h.origin, b.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other["approvals"][0]["id"], b1);
    assert!(!other.to_string().contains(&a1));

    let empty = seed_task(&h.engine.store, h._dir.path(), "task-empty");
    let empty_body: Value = client()
        .get(format!("{}/api/tasks/{}/approvals", h.origin, empty.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty_body, json!({"approvals":[]}));
}

#[tokio::test]
async fn ah02_ah08_ah12_decision_snapshot_conflict_and_concurrency() {
    let h = harness().await;
    let mut task = seed_task(&h.engine.store, h._dir.path(), "decision-task");
    let id = card(
        &h,
        &mut task,
        "write",
        "write_file",
        json!({"path":"src/a","content":"x"}),
    );
    let url = format!("{}/api/approvals/{id}/decision", h.origin);
    let first = client()
        .post(&url)
        .header("x-peachsh-token", &h.token)
        .json(&json!({"decision":"approve"}))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let snapshot: Value = first.json().await.unwrap();
    assert_eq!(snapshot["status"], "approved");
    assert_eq!(snapshot["execution_state"], "not_started");
    assert_eq!(snapshot["decided_by"], "user");
    assert_eq!(event_count(&h, &task, "approval.resolved"), 1);
    assert!(!h._dir.path().join("src/a").exists());
    error(
        client()
            .post(&url)
            .header("x-peachsh-token", &h.token)
            .json(&json!({"decision":"deny"}))
            .send()
            .await
            .unwrap(),
        StatusCode::CONFLICT,
        false,
    )
    .await;
    error(
        client()
            .post(&url)
            .header("x-peachsh-token", &h.token)
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"decision":"approve","padding":"{}"}}"#,
                "x".repeat(1_100_000)
            ))
            .send()
            .await
            .unwrap(),
        StatusCode::PAYLOAD_TOO_LARGE,
        false,
    )
    .await;
    for (name, value) in [
        ("host", "evil.example"),
        ("origin", "http://evil.example"),
        ("sec-fetch-site", "cross-site"),
    ] {
        error(
            client()
                .post(&url)
                .header("x-peachsh-token", &h.token)
                .header(name, value)
                .json(&json!({"decision":"approve"}))
                .send()
                .await
                .unwrap(),
            StatusCode::FORBIDDEN,
            false,
        )
        .await;
    }
    assert_eq!(event_count(&h, &task, "approval.resolved"), 1);

    let race = card(
        &h,
        &mut task,
        "race",
        "write_file",
        json!({"path":"src/r","content":"r"}),
    );
    let race_url = format!("{}/api/approvals/{race}/decision", h.origin);
    let mut requests = tokio::task::JoinSet::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(9));
    for index in 0..8 {
        let url = race_url.clone();
        let token = h.token.clone();
        let barrier = barrier.clone();
        requests.spawn(async move {
            barrier.wait().await;
            client()
                .post(url)
                .header("x-peachsh-token", token)
                .json(&json!({"decision":if index % 2 == 0 {"approve"} else {"deny"}}))
                .send()
                .await
                .unwrap()
                .status()
        });
    }
    barrier.wait().await;
    let mut statuses = Vec::new();
    while let Some(status) = requests.join_next().await {
        statuses.push(status.unwrap());
    }
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        7
    );
    assert_eq!(event_count(&h, &task, "approval.resolved"), 2);
}

#[tokio::test]
async fn ah11_strict_inputs_guards_paths_and_safe_errors() {
    let h = harness().await;
    let mut task = seed_task(&h.engine.store, h._dir.path(), "input-task");
    let id = card(
        &h,
        &mut task,
        "write",
        "write_file",
        json!({"path":"src/a","content":"x"}),
    );
    let before = event_count(&h, &task, "approval.resolved");
    let url = format!("{}/api/approvals/{id}/decision", h.origin);
    for body in [
        "{}",
        r#"{"decision":"approve","decided_by":"attacker"}"#,
        r#"{"decision":"approve","decision":"deny"}"#,
        r#"{"decision":null}"#,
        r#"{"decision":3}"#,
        r#"{"decision":"later"}"#,
        "{",
    ] {
        error(
            client()
                .post(&url)
                .header("x-peachsh-token", &h.token)
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap(),
            StatusCode::BAD_REQUEST,
            false,
        )
        .await;
    }
    error(
        client()
            .post(&url)
            .header("x-peachsh-token", &h.token)
            .body(r#"{"decision":"approve"}"#)
            .send()
            .await
            .unwrap(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        false,
    )
    .await;
    error(
        client()
            .post(&url)
            .header("content-type", "application/json")
            .body(r#"{"decision":"approve"}"#)
            .send()
            .await
            .unwrap(),
        StatusCode::FORBIDDEN,
        false,
    )
    .await;
    error(
        client()
            .get(format!("{}/api/approvals/%00", h.origin))
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
        false,
    )
    .await;
    error(
        client()
            .get(format!("{}/api/approvals/missing", h.origin))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
        false,
    )
    .await;
    error(
        client()
            .get(format!("{}/api/tasks/missing/approvals", h.origin))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
        false,
    )
    .await;
    assert_eq!(event_count(&h, &task, "approval.resolved"), before);
    assert_eq!(
        h.engine.store.approval(&id).unwrap().status,
        ApprovalStatus::Pending
    );

    let damaged = seed_task(&h.engine.store, h._dir.path(), "damaged-task");
    Connection::open(&h.db_path)
        .unwrap()
        .execute(
            "UPDATE tasks SET value='{bad json' WHERE id=?1",
            [&damaged.id],
        )
        .unwrap();
    error(
        client()
            .get(format!("{}/api/tasks/{}/approvals", h.origin, damaged.id))
            .send()
            .await
            .unwrap(),
        StatusCode::INTERNAL_SERVER_ERROR,
        false,
    )
    .await;
}

#[tokio::test]
async fn ah11_real_busy_decision_is_retryable_and_corruption_is_not() {
    let h = harness().await;
    let mut task = seed_task(&h.engine.store, h._dir.path(), "fault-task");
    let busy_id = card(
        &h,
        &mut task,
        "busy",
        "write_file",
        json!({"path":"src/a","content":"x"}),
    );
    let lock = Connection::open(&h.db_path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let response = client()
        .post(format!("{}/api/approvals/{busy_id}/decision", h.origin))
        .header("x-peachsh-token", &h.token)
        .json(&json!({"decision":"approve"}))
        .send()
        .await
        .unwrap();
    error(response, StatusCode::INTERNAL_SERVER_ERROR, true).await;
    lock.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        h.engine.store.approval(&busy_id).unwrap().status,
        ApprovalStatus::Pending
    );

    let corrupt_id = card(
        &h,
        &mut task,
        "corrupt",
        "write_file",
        json!({"path":"src/b","content":"x"}),
    );
    let db = Connection::open(&h.db_path).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    db.execute(
        "UPDATE approvals SET task_id='missing-parent' WHERE id=?1",
        [&corrupt_id],
    )
    .unwrap();
    drop(db);
    error(
        client()
            .get(format!("{}/api/approvals/{corrupt_id}", h.origin))
            .send()
            .await
            .unwrap(),
        StatusCode::INTERNAL_SERVER_ERROR,
        false,
    )
    .await;
}

#[tokio::test]
async fn ah14_unknown_is_visible_and_decision_remains_conflict() {
    let h = harness().await;
    let mut task = seed_task(&h.engine.store, h._dir.path(), "unknown-task");
    let id = card(
        &h,
        &mut task,
        "command",
        "run_command",
        json!({"command":"exit 0"}),
    );
    h.engine.store.decide_approval(&id, true, "user").unwrap();
    h.engine
        .store
        .claim_approval(
            &task,
            "command",
            "run_command",
            &json!({"command":"exit 0"}),
        )
        .unwrap();
    h.engine.store.recover().unwrap();
    let body: Value = client()
        .get(format!("{}/api/approvals/{id}", h.origin))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["execution_state"], "unknown");
    error(
        client()
            .post(format!("{}/api/approvals/{id}/decision", h.origin))
            .header("x-peachsh-token", &h.token)
            .json(&json!({"decision":"approve"}))
            .send()
            .await
            .unwrap(),
        StatusCode::CONFLICT,
        false,
    )
    .await;
    assert_eq!(event_count(&h, &task, "approval.resolved"), 1);
    assert_eq!(
        h.engine.store.approval(&id).unwrap().execution_state,
        ExecutionState::Unknown
    );
}

#[tokio::test]
async fn ah14_http_decision_does_not_claim_binding_validation() {
    let h = live_harness(vec![vec![tool_call(
        "bound",
        "write_file",
        json!({"path":"src/bound.txt","content":"original"}),
    )]])
    .await;
    let task = start_live(&h, false).await;
    let approval = pending(&h, &task.id).await;
    let db = Connection::open(&h.db_path).unwrap();
    db.execute(
        "UPDATE approvals SET binding_digest='changed-binding' WHERE id=?1",
        [&approval.id],
    )
    .unwrap();
    drop(db);
    let response = http_decide(&h, &approval.id, "approve").await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "decision is only pending CAS"
    );
    let snapshot: Value = response.json().await.unwrap();
    assert_eq!(snapshot["status"], "approved");
    let done = settled(&h, &task.id).await;
    assert_eq!(done.status, "failed");
    assert!(!h._dir.path().join("src/bound.txt").exists());
    assert_eq!(event_count(&h, &task, "tool_start"), 0);
    assert_eq!(event_count(&h, &task, "tool_result"), 0);
}

#[test]
fn ah05_shutdown_reopen_http_decision_without_worker_then_resume_once() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("approval-http.db");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (task, approval) = runtime.block_on(async {
        let (base, _calls, _provider) = provider_server(vec![vec![tool_call(
            "write",
            "write_file",
            json!({"path":"src/restarted.txt","content":"once"}),
        )]])
        .await;
        let store = Arc::new(Store::open(&db_path).unwrap());
        configure_live(&store, temp.path(), &base);
        let engine = Engine::new(store, 2).unwrap();
        let task = engine
            .start(run_request(false))
            .await
            .unwrap()
            .tasks
            .remove(0);
        let approval = pending_engine(&engine, &task.id).await;
        (task, approval)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    assert!(!temp.path().join("src/restarted.txt").exists());

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (base, calls, _provider) = provider_server(vec![vec![]]).await;
        let store = Arc::new(Store::open(&db_path).unwrap());
        configure_live(&store, temp.path(), &base);
        let engine = Engine::new(store, 2).unwrap();
        engine.store.recover().unwrap();
        let (origin, token, _http) = serve_http_engine(engine.clone()).await;
        let response = client()
            .post(format!("{origin}/api/approvals/{}/decision", approval.id))
            .header("x-peachsh-token", token)
            .json(&json!({"decision":"approve"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "HTTP decision must not launch"
        );
        assert!(!temp.path().join("src/restarted.txt").exists());
        let mut persisted = engine.store.task(&task.id).unwrap();
        persisted.route.base_url = base;
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE tasks SET value=?2 WHERE id=?1",
                rusqlite::params![persisted.id, serde_json::to_string(&persisted).unwrap()],
            )
            .unwrap();
        engine.resume(&task.id, "continue").await.unwrap();
        assert_eq!(settled_engine(&engine, &task.id).await.status, "completed");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("src/restarted.txt")).unwrap(),
            "once"
        );
        assert_eq!(
            engine.store.approval(&approval.id).unwrap().execution_state,
            ExecutionState::Finished
        );
        assert_eq!(
            engine
                .store
                .events(&task.run_id, 0)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "tool_start")
                .count(),
            1
        );
    });
    runtime.shutdown_timeout(Duration::from_secs(2));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let store = Arc::new(Store::open(&db_path).unwrap());
        let engine = Engine::new(store, 2).unwrap();
        engine.store.recover().unwrap();
        assert_eq!(
            engine.store.approval(&approval.id).unwrap().execution_state,
            ExecutionState::Finished
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("src/restarted.txt")).unwrap(),
            "once"
        );
        assert_eq!(
            engine
                .store
                .events(&task.run_id, 0)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "tool_start")
                .count(),
            1
        );
        assert_eq!(engine.store.task(&task.id).unwrap().status, "completed");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("src/restarted.txt")).unwrap(),
            "once"
        );
        assert_eq!(
            engine
                .store
                .events(&task.run_id, 0)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "tool_start")
                .count(),
            1
        );
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[test]
fn ah06_real_command_side_effect_then_shutdown_recovers_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("approval-http.db");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (task, approval) = runtime.block_on(async {
        let (base, _calls, _provider) = provider_server(vec![vec![tool_call(
            "command",
            "run_command",
            json!({"command":"Add-Content -LiteralPath crash-counter.txt -Value one; while ($true) { Start-Sleep -Seconds 1 }"}),
        )]])
        .await;
        let store = Arc::new(Store::open(&db_path).unwrap());
        configure_live(&store, temp.path(), &base);
        let engine = Engine::new(store, 2).unwrap();
        let (origin, token, _http) = serve_http_engine(engine.clone()).await;
        let task = engine.start(run_request(true)).await.unwrap().tasks.remove(0);
        let approval = pending_engine(&engine, &task.id).await;
        let response = client()
            .post(format!("{origin}/api/approvals/{}/decision", approval.id))
            .header("x-peachsh-token", token)
            .json(&json!({"decision":"approve"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let count = std::fs::read_to_string(temp.path().join("crash-counter.txt"))
                    .ok().map(|text| text.lines().count()).unwrap_or(0);
                let record = engine.store.approval(&approval.id).unwrap();
                let results = engine.store.events(&task.run_id, 0).unwrap().into_iter().filter(|event| event.kind == "tool_result").count();
                if count == 1 && record.execution_state == ExecutionState::Claimed && results == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }).await.expect("real side effect before result commit");
        (task, approval)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let store = Arc::new(Store::open(&db_path).unwrap());
        let engine = Engine::new(store, 2).unwrap();
        engine.store.recover().unwrap();
        let (origin, token, _http) = serve_http_engine(engine.clone()).await;
        let response = client()
            .get(format!("{origin}/api/approvals/{}", approval.id))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["execution_state"], "unknown");
        error(
            client()
                .post(format!("{origin}/api/approvals/{}/decision", approval.id))
                .header("x-peachsh-token", token)
                .json(&json!({"decision":"approve"}))
                .send()
                .await
                .unwrap(),
            StatusCode::CONFLICT,
            false,
        )
        .await;
        assert!(engine.resume(&task.id, "continue").await.is_err());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("crash-counter.txt"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_ne!(engine.store.task(&task.id).unwrap().status, "completed");
        assert_eq!(
            engine
                .store
                .events(&task.run_id, 0)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "tool_start")
                .count(),
            1
        );
        assert_eq!(
            engine
                .store
                .events(&task.run_id, 0)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "tool_result")
                .count(),
            0
        );
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[test]
fn ah06_denied_http_decision_survives_real_runtime_restart_without_execution() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("approval-http.db");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (task, approval) = runtime.block_on(async {
        let (base, _calls, _provider) = provider_server(vec![
            vec![tool_call(
                "write",
                "write_file",
                json!({"path":"src/denied-restart.txt","content":"deny"}),
            )],
            vec![],
        ])
        .await;
        let store = Arc::new(Store::open(&db_path).unwrap());
        configure_live(&store, temp.path(), &base);
        let engine = Engine::new(store, 2).unwrap();
        let (origin, token, _http) = serve_http_engine(engine.clone()).await;
        let task = engine
            .start(run_request(false))
            .await
            .unwrap()
            .tasks
            .remove(0);
        let approval = pending_engine(&engine, &task.id).await;
        let response = client()
            .post(format!("{origin}/api/approvals/{}/decision", approval.id))
            .header("x-peachsh-token", token)
            .json(&json!({"decision":"deny"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(settled_engine(&engine, &task.id).await.status, "completed");
        assert!(!temp.path().join("src/denied-restart.txt").exists());
        (task, approval)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let store = Arc::new(Store::open(&db_path).unwrap());
        let engine = Engine::new(store, 2).unwrap();
        engine.store.recover().unwrap();
        let record = engine.store.approval(&approval.id).unwrap();
        assert_eq!(record.status, ApprovalStatus::Denied);
        assert_eq!(record.execution_state, ExecutionState::NotStarted);
        assert!(!temp.path().join("src/denied-restart.txt").exists());
        assert_eq!(
            engine
                .store
                .events(&task.run_id, 0)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "tool_start")
                .count(),
            0
        );
        assert_eq!(
            engine
                .store
                .events(&task.run_id, 0)
                .unwrap()
                .iter()
                .filter(|event| event.kind == "approval.resolved")
                .count(),
            1
        );
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}
