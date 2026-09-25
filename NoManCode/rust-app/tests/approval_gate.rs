use axum::{Json, Router, extract::State, routing::post};
use peachsh::{
    approval::{self, ApprovalError, ApprovalRecord, ApprovalStatus, ExecutionState},
    domain::*,
    engine::Engine,
    store::Store,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Barrier, Mutex},
    time::Duration,
};

type Script = Arc<Mutex<VecDeque<Vec<Value>>>>;

fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn write(id: &str, path: &str) -> Value {
    call(
        id,
        "write_file",
        json!({"path":path,"content":"approval-body"}),
    )
}
fn args(call: &Value) -> Value {
    serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap()
}

async fn response(
    State(script): State<Script>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let group = script.lock().unwrap().pop_front().unwrap_or_default();
    let delta = if group.is_empty() {
        json!({"content":"done"})
    } else {
        let group: Vec<_> = group
            .into_iter()
            .enumerate()
            .map(|(index, mut c)| {
                c["index"] = json!(index);
                c
            })
            .collect();
        json!({"tool_calls":group})
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

async fn server(groups: Vec<Vec<Value>>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(response))
        .with_state(Arc::new(Mutex::new(VecDeque::from(groups))));
    (
        base,
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }),
    )
}

fn route(base: &str) -> Route {
    Route {
        id: "route".into(),
        name: "Route".into(),
        base_url: base.into(),
        model: "mock".into(),
        max_tokens: 128,
        parallel_limit: 4,
        key_env: None,
    }
}
fn spec(commands: bool) -> TaskSpec {
    TaskSpec {
        name: "worker".into(),
        role: "worker".into(),
        route_id: "route".into(),
        prompt: "begin".into(),
        depends_on: vec![],
        write_scopes: vec!["src".into()],
        tools: true,
        allow_commands: commands,
        max_rounds: 8,
    }
}
fn setup(path: &Path, base: &str) -> Arc<Engine> {
    let store = Arc::new(Store::open(&path.join("approval.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: path.to_string_lossy().into(),
            max_concurrency: 4,
            routes: vec![route(base)],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "fake-key-approval").unwrap();
    Engine::new(store, 4).unwrap()
}
async fn start(e: &Arc<Engine>, commands: bool) -> Task {
    e.start(RunRequest {
        title: "approval".into(),
        kind: SessionKind::Team,
        tasks: vec![spec(commands)],
    })
    .await
    .unwrap()
    .tasks
    .remove(0)
}
async fn until<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if let Some(v) = probe() {
                return v;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded durable observation")
}
async fn pending(e: &Engine, id: &str) -> ApprovalRecord {
    until(|| {
        e.store
            .pending_approvals_for_task(id)
            .unwrap()
            .into_iter()
            .next()
    })
    .await
}
async fn settled(e: &Engine, id: &str) -> Task {
    until(|| {
        let t = e.store.task(id).unwrap();
        (!e.is_busy() && !matches!(t.status.as_str(), "queued" | "running")).then_some(t)
    })
    .await
}
fn event_count(e: &Engine, t: &Task, kind: &str) -> usize {
    e.store
        .events(&t.run_id, 0)
        .unwrap()
        .iter()
        .filter(|v| v.kind == kind)
        .count()
}
fn db(path: &Path) -> Connection {
    let c = Connection::open(path.join("approval.db")).unwrap();
    c.busy_timeout(Duration::from_secs(5)).unwrap();
    c
}
fn error_kind(error: &anyhow::Error) -> &ApprovalError {
    error
        .downcast_ref::<ApprovalError>()
        .expect("typed approval error chain")
}
fn seed(store: &Store, root: &Path, id: &str, calls: Vec<Value>, commands: bool) -> Task {
    let task = Task {
        id: id.into(),
        run_id: format!("run-{id}"),
        spec: spec(commands),
        route: route("http://127.0.0.1:1/v1"),
        workspace: root.to_string_lossy().into(),
        status: "running".into(),
        output: String::new(),
        error: None,
        messages: vec![
            json!({"role":"user","content":"begin"}),
            json!({"role":"assistant","tool_calls":calls}),
        ],
        usage: Value::Null,
        created_at: 1,
        updated_at: 1,
    };
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

#[tokio::test]
async fn ap01_ap02_pending_then_approved_executes_once() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _server) = server(vec![vec![write("w1", "src/a.txt")]]).await;
    let e = setup(temp.path(), &base);
    let t = start(&e, false).await;
    let card = pending(&e, &t.id).await;
    assert!(!temp.path().join("src/a.txt").exists());
    assert_eq!(event_count(&e, &t, "tool_start"), 0);
    assert_eq!(event_count(&e, &t, "tool_result"), 0);
    assert_eq!(event_count(&e, &t, "file_backup"), 0);
    assert_eq!(card.execution_state, ExecutionState::NotStarted);
    assert_eq!(event_count(&e, &t, "approval.requested"), 1);
    let approved = e.decide_approval(&card.id, true, None).unwrap();
    assert_eq!(approved.decided_by.as_deref(), Some("user"));
    assert!(approved.decided_at.is_some());
    let done = settled(&e, &t.id).await;
    assert_eq!(done.status, "completed");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "approval-body"
    );
    assert_eq!(event_count(&e, &t, "tool_start"), 1);
    assert_eq!(event_count(&e, &t, "tool_result"), 1);
    assert_eq!(event_count(&e, &t, "approval.resolved"), 1);
    assert_eq!(
        e.store.approval(&card.id).unwrap().execution_state,
        ExecutionState::Finished
    );
    let events = serde_json::to_string(&e.store.events(&t.run_id, 0).unwrap()).unwrap();
    assert!(!events.contains("approval-body"));
    assert!(!events.contains("arguments"));
}

#[tokio::test]
async fn ap03_ap09_denied_and_decision_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _server) = server(vec![vec![write("w1", "src/a.txt")]]).await;
    let e = setup(temp.path(), &base);
    let t = start(&e, false).await;
    let card = pending(&e, &t.id).await;
    assert!(matches!(
        error_kind(&e.decide_approval("missing", true, None).unwrap_err()),
        ApprovalError::NotFound { .. }
    ));
    e.decide_approval(&card.id, false, None).unwrap();
    assert!(matches!(
        error_kind(&e.decide_approval(&card.id, true, None).unwrap_err()),
        ApprovalError::Conflict { .. }
    ));
    let done = settled(&e, &t.id).await;
    assert_eq!(done.status, "completed");
    assert!(!temp.path().join("src/a.txt").exists());
    assert!(done.messages.iter().any(
        |m| m["role"] == "tool" && m["content"] == approval::tool_error(ApprovalStatus::Denied)
    ));
    assert_eq!(event_count(&e, &t, "approval.resolved"), 1);
    assert_eq!(event_count(&e, &t, "tool_start"), 0);
}

#[tokio::test]
async fn ap04_read_tools_pass_through() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("input.txt"), "hello").unwrap();
    let (base, _server) = server(vec![vec![
        call("r1", "list_files", json!({"path":"."})),
        call("r2", "read_file", json!({"path":"input.txt"})),
        call("r3", "search_files", json!({"query":"hello"})),
    ]])
    .await;
    let e = setup(temp.path(), &base);
    let t = start(&e, false).await;
    let done = settled(&e, &t.id).await;
    assert_eq!(done.status, "completed");
    assert!(e.store.approvals_for_task(&t.id).unwrap().is_empty());
    assert_eq!(event_count(&e, &t, "tool_result"), 3);
}

#[tokio::test]
async fn ap05_command_waits_and_runs_once() {
    for approved in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let (base, _server) = server(vec![vec![call(
            "cmd",
            "run_command",
            json!({"command":"Add-Content -LiteralPath counter.txt -Value one"}),
        )]])
        .await;
        let e = setup(temp.path(), &base);
        let t = start(&e, true).await;
        let card = pending(&e, &t.id).await;
        assert!(!temp.path().join("counter.txt").exists());
        e.decide_approval(&card.id, approved, None).unwrap();
        assert_eq!(settled(&e, &t.id).await.status, "completed");
        if approved {
            assert_eq!(
                std::fs::read_to_string(temp.path().join("counter.txt"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
        } else {
            assert!(!temp.path().join("counter.txt").exists());
            assert_eq!(event_count(&e, &t, "tool_start"), 0);
        }
    }
}

#[tokio::test]
async fn ap08_cancel_pending_is_durable() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _server) = server(vec![vec![write("w1", "src/a.txt")]]).await;
    let e = setup(temp.path(), &base);
    let t = start(&e, false).await;
    let card = pending(&e, &t.id).await;
    e.cancel(&t.id).unwrap();
    assert_eq!(settled(&e, &t.id).await.status, "cancelled");
    assert_eq!(
        e.store.approval(&card.id).unwrap().status,
        ApprovalStatus::Cancelled
    );
    assert!(!temp.path().join("src/a.txt").exists());
    assert_eq!(event_count(&e, &t, "approval.resolved"), 1);
    assert!(matches!(
        error_kind(&e.decide_approval(&card.id, true, None).unwrap_err()),
        ApprovalError::Conflict { .. }
    ));
}

#[test]
fn ap08_restart_public_cancel_is_idempotent_without_active_runtime() {
    for approve_before_restart in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (task, card) = runtime.block_on(async {
            let (base, _server) = server(vec![vec![write("w", "src/a.txt")]]).await;
            let e = setup(temp.path(), &base);
            let task = start(&e, false).await;
            let card = pending(&e, &task.id).await;
            if approve_before_restart {
                e.decide_approval(&card.id, true, None).unwrap();
                assert_eq!(
                    e.store.approval(&card.id).unwrap().execution_state,
                    ExecutionState::NotStarted
                );
            }
            (task, card)
        });
        runtime.shutdown_timeout(Duration::from_secs(2));

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (base, _server) = server(vec![]).await;
            let e = setup(temp.path(), &base);
            e.store.recover().unwrap();
            assert_eq!(e.store.task(&task.id).unwrap().status, "interrupted");

            e.cancel(&task.id).unwrap();
            e.cancel(&task.id).unwrap();
            assert!(e.cancel("missing-task").is_err());

            let cancelled = e.store.approval(&card.id).unwrap();
            assert_eq!(
                cancelled.status,
                if approve_before_restart {
                    ApprovalStatus::Approved
                } else {
                    ApprovalStatus::Cancelled
                }
            );
            assert_eq!(cancelled.execution_state, ExecutionState::Cancelled);
            assert_eq!(e.store.task(&task.id).unwrap().status, "interrupted");
            assert_eq!(event_count(&e, &task, "approval.resolved"), 1);
            assert_eq!(event_count(&e, &task, "tool_start"), 0);
            assert!(!temp.path().join("src/a.txt").exists());

            let mut persisted = e.store.task(&task.id).unwrap();
            persisted.route.base_url = base;
            db(temp.path())
                .execute(
                    "UPDATE tasks SET value=?2 WHERE id=?1",
                    params![persisted.id, serde_json::to_string(&persisted).unwrap()],
                )
                .unwrap();
            e.resume(&task.id, "continue").await.unwrap();
            let done = settled(&e, &task.id).await;
            assert_eq!(done.status, "completed");
            assert!(done.messages.iter().any(|message| {
                message["role"] == "tool"
                    && message["content"] == approval::tool_error(ApprovalStatus::Cancelled)
            }));
            assert_eq!(event_count(&e, &task, "tool_start"), 0);
            assert_eq!(event_count(&e, &task, "approval.resolved"), 1);
        });
        runtime.shutdown_timeout(Duration::from_secs(2));
    }
}

#[test]
fn redacted_command_result_stays_finished_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (task, card) = runtime.block_on(async {
        let command = "Add-Content -LiteralPath counter.txt -Value one; Write-Output 'bearer mock-redact-secret'; Write-Error 'ordinary failure' -ErrorAction Continue; exit 7";
        let (base, _server) = server(vec![vec![call(
            "cmd",
            "run_command",
            json!({"command":command}),
        )]])
        .await;
        let e = setup(temp.path(), &base);
        let task = start(&e, true).await;
        let card = pending(&e, &task.id).await;
        e.decide_approval(&card.id, true, None).unwrap();
        let done = settled(&e, &task.id).await;
        assert_eq!(done.status, "completed");
        assert_eq!(
            e.store.approval(&card.id).unwrap().execution_state,
            ExecutionState::Finished
        );
        let message = done
            .messages
            .iter()
            .find(|message| message["tool_call_id"] == "cmd")
            .unwrap();
        let result: Value = serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["exit_code"], 7);
        assert!(result["stdout"].as_str().unwrap().contains("bearer [redacted]"));
        assert!(result["stderr"].as_str().unwrap().contains("ordinary failure"));
        let event = e
            .store
            .events(&task.run_id, 0)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "tool_result")
            .unwrap();
        assert_eq!(event.data["result"], result);
        assert_eq!(event_count(&e, &task, "tool_start"), 1);
        (task, card)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));

    let raw = std::fs::read(temp.path().join("approval.db")).unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("mock-redact-secret"));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("counter.txt"))
            .unwrap()
            .lines()
            .count(),
        1
    );

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (base, _server) = server(vec![]).await;
        let e = setup(temp.path(), &base);
        e.store.recover().unwrap();
        assert_eq!(
            e.store.approval(&card.id).unwrap().execution_state,
            ExecutionState::Finished
        );
        let mut persisted = e.store.task(&task.id).unwrap();
        persisted.route.base_url = base;
        db(temp.path())
            .execute(
                "UPDATE tasks SET value=?2 WHERE id=?1",
                params![persisted.id, serde_json::to_string(&persisted).unwrap()],
            )
            .unwrap();
        e.resume(&task.id, "continue").await.unwrap();
        assert_eq!(settled(&e, &task.id).await.status, "completed");
        assert_eq!(
            std::fs::read_to_string(temp.path().join("counter.txt"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(event_count(&e, &task, "tool_start"), 1);
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[tokio::test]
async fn ap10_ap11_safe_previews_independent_calls() {
    let temp = tempfile::tempdir().unwrap();
    let secret = "Bearer mock-approval-secret";
    let c = call(
        "w1",
        "write_file",
        json!({"path":"src/a.txt","content":secret,"env":{"API_KEY":secret}}),
    );
    let (base, _server) = server(vec![vec![c, write("w2", "src/b.txt")]]).await;
    let e = setup(temp.path(), &base);
    let t = start(&e, false).await;
    let a = pending(&e, &t.id).await;
    assert!(
        !serde_json::to_string(&a)
            .unwrap()
            .contains("mock-approval-secret")
    );
    assert!(!a.preview.contains("Bearer"));
    e.decide_approval(&a.id, true, None).unwrap();
    let b = pending(&e, &t.id).await;
    assert_ne!(a.id, b.id);
    e.decide_approval(&b.id, false, None).unwrap();
    let done = settled(&e, &t.id).await;
    assert_eq!(done.status, "completed");
    assert!(temp.path().join("src/a.txt").exists());
    assert!(!temp.path().join("src/b.txt").exists());
    let events = serde_json::to_string(&e.store.events(&t.run_id, 0).unwrap()).unwrap();
    assert!(!events.contains("mock-approval-secret"));
    let tool_ids: Vec<_> = done
        .messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["tool_call_id"].as_str().unwrap())
        .collect();
    assert_eq!(tool_ids, vec!["w1", "w2"]);
    let preview = approval::preview(
        "run_command",
        &json!({"command":format!("Bearer mock-approval-secret {}", "界".repeat(200))}),
    );
    assert!(preview.chars().count() <= 160);
    assert!(!preview.contains("mock-approval-secret"));
}

#[tokio::test]
async fn ap13_duplicate_groups_have_no_effect() {
    for later in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let groups = if later {
            vec![
                vec![write("dup", "src/a.txt")],
                vec![write("fresh", "src/b.txt"), write("dup", "src/c.txt")],
            ]
        } else {
            vec![vec![write("dup", "src/a.txt"), write("dup", "src/b.txt")]]
        };
        let (base, _server) = server(groups).await;
        let e = setup(temp.path(), &base);
        let t = start(&e, false).await;
        if later {
            let card = pending(&e, &t.id).await;
            e.decide_approval(&card.id, true, None).unwrap();
        }
        assert_eq!(settled(&e, &t.id).await.status, "failed");
        let n = usize::from(later);
        assert_eq!(e.store.approvals_for_task(&t.id).unwrap().len(), n);
        assert_eq!(event_count(&e, &t, "tool_start"), n);
        assert_eq!(event_count(&e, &t, "approval.requested"), n);
        assert!(!temp.path().join("src/b.txt").exists());
        assert!(!temp.path().join("src/c.txt").exists());
    }
}

#[test]
fn ap13_cross_task_same_call_isolated() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(&temp.path().join("approval.db")).unwrap();
    let c = write("same", "src/a.txt");
    let a = seed(&s, temp.path(), "a", vec![c.clone()], false);
    let b = seed(&s, temp.path(), "b", vec![c.clone()], false);
    let (ar, _) = s
        .ensure_approval(&a, "same", "write_file", &args(&c))
        .unwrap();
    let (br, _) = s
        .ensure_approval(&b, "same", "write_file", &args(&c))
        .unwrap();
    assert_ne!(ar.id, br.id);
    s.decide_approval(&ar.id, true, "user").unwrap();
    assert_eq!(s.approval(&br.id).unwrap().status, ApprovalStatus::Pending);
    assert!(
        s.claim_approval(&b, "same", "write_file", &args(&c))
            .is_err()
    );
    assert_eq!(s.approvals_for_task("a").unwrap().len(), 1);
    assert_eq!(s.approvals_for_task("b").unwrap().len(), 1);
}

#[test]
fn ap14_binding_changes_rejected_without_new_events() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(&temp.path().join("approval.db")).unwrap();
    let c = write("w", "src/a.txt");
    let t = seed(&s, temp.path(), "a", vec![c.clone()], false);
    let (card, _) = s.ensure_approval(&t, "w", "write_file", &args(&c)).unwrap();
    s.decide_approval(&card.id, true, "user").unwrap();
    let events = s.events(&t.run_id, 0).unwrap().len();
    for n in 0..4 {
        let mut task = t.clone();
        let mut a = args(&c);
        let mut name = "write_file";
        match n {
            0 => a["path"] = json!("src/b.txt"),
            1 => task.workspace.push_str("-other"),
            2 => task.spec.write_scopes = vec!["*".into()],
            _ => name = "run_command",
        }
        assert!(matches!(
            error_kind(&s.claim_approval(&task, "w", name, &a).unwrap_err()),
            ApprovalError::BindingConflict { .. }
        ));
        assert_eq!(s.approvals_for_task(&t.id).unwrap().len(), 1);
        assert_eq!(s.events(&t.run_id, 0).unwrap().len(), events);
    }
    let reordered: Value =
        serde_json::from_str("{ \"content\":\"approval-body\", \"path\":\"src/a.txt\" }").unwrap();
    assert_eq!(
        approval::args_digest(&args(&c)).unwrap(),
        approval::args_digest(&reordered).unwrap()
    );
    assert_eq!(
        s.claim_approval(&t, "w", "write_file", &reordered)
            .unwrap()
            .execution_state,
        ExecutionState::Claimed
    );
}

#[test]
fn ap17_multistore_decision_cancel_race() {
    for _ in 0..12 {
        let temp = tempfile::tempdir().unwrap();
        let s = Store::open(&temp.path().join("approval.db")).unwrap();
        let c = write("w", "src/a.txt");
        let t = seed(&s, temp.path(), "a", vec![c.clone()], false);
        let (card, _) = s.ensure_approval(&t, "w", "write_file", &args(&c)).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = vec![];
        for mode in 0..3 {
            let s = Store::open(&temp.path().join("approval.db")).unwrap();
            let barrier = barrier.clone();
            let id = card.id.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                if mode == 2 {
                    s.cancel_pending_approvals("a").map(|_| ())
                } else {
                    s.decide_approval(&id, mode == 0, "user").map(|_| ())
                }
            }));
        }
        for h in handles {
            let _ = h.join().unwrap();
        }
        assert_eq!(
            s.events(&t.run_id, 0)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "approval.resolved")
                .count(),
            1
        );
        assert_eq!(s.task("a").unwrap().status, "cancelled");
        assert!(s.claim_approval(&t, "w", "write_file", &args(&c)).is_err());
    }
}

#[test]
fn ap17_claim_cancel_order_and_stale_completed() {
    for claim_first in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let s = Store::open(&temp.path().join("approval.db")).unwrap();
        let c = write("w", "src/a.txt");
        let mut t = seed(&s, temp.path(), "a", vec![c.clone()], false);
        let (card, _) = s.ensure_approval(&t, "w", "write_file", &args(&c)).unwrap();
        s.decide_approval(&card.id, true, "user").unwrap();
        if claim_first {
            s.claim_approval(&t, "w", "write_file", &args(&c)).unwrap();
        }
        s.cancel_pending_approvals("a").unwrap();
        let record = s.approval(&card.id).unwrap();
        assert_eq!(record.status, ApprovalStatus::Approved);
        assert_eq!(
            record.execution_state,
            if claim_first {
                ExecutionState::Claimed
            } else {
                ExecutionState::Cancelled
            }
        );
        assert!(s.claim_approval(&t, "w", "write_file", &args(&c)).is_err());
        t.status = "completed".into();
        s.save_task(&t).unwrap();
        assert_eq!(s.task("a").unwrap().status, "cancelled");
        assert_eq!(
            s.events(&t.run_id, 0)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "approval.resolved")
                .count(),
            1
        );
    }
}

// A real runtime is destroyed between phases. No Arc-only restart fixture.
#[test]
fn ap06_ap07_ap18_runtime_restart_and_decide_before_waiter() {
    for approved in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (task, card) = runtime.block_on(async {
            let (base, _server) = server(vec![vec![write("w", "src/a.txt")]]).await;
            let e = setup(temp.path(), &base);
            let t = start(&e, false).await;
            let c = pending(&e, &t.id).await;
            (t, c)
        });
        runtime.shutdown_timeout(Duration::from_secs(2));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (base, _server) = server(vec![]).await;
            let e = setup(temp.path(), &base);
            e.store.recover().unwrap();
            // Preserve the same route endpoint in the task by updating the mock
            // fixture's persisted route (before any binding uses route metadata).
            let connection = db(temp.path());
            let mut t = e.store.task(&task.id).unwrap();
            t.route.base_url = base;
            connection
                .execute(
                    "UPDATE tasks SET value=?2 WHERE id=?1",
                    params![t.id, serde_json::to_string(&t).unwrap()],
                )
                .unwrap();
            assert_eq!(
                e.store.approval(&card.id).unwrap().status,
                ApprovalStatus::Pending
            );
            e.decide_approval(&card.id, approved, None).unwrap();
            assert!(!e.is_busy());
            assert!(!temp.path().join("src/a.txt").exists());
            e.resume(&task.id, "continue").await.unwrap();
            assert_eq!(settled(&e, &task.id).await.status, "completed");
            assert_eq!(temp.path().join("src/a.txt").exists(), approved);
            assert_eq!(event_count(&e, &task, "tool_start"), usize::from(approved));
            assert_eq!(event_count(&e, &task, "approval.requested"), 1);
        });
        runtime.shutdown_timeout(Duration::from_secs(2));
    }
}

#[test]
fn ap14_redacted_arguments_cannot_resume_as_substitute() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (t, card) = runtime.block_on(async {
        let c = call(
            "secret",
            "write_file",
            json!({"path":"src/a.txt","content":"Bearer mock-secret-value"}),
        );
        let (base, _server) = server(vec![vec![c]]).await;
        let e = setup(temp.path(), &base);
        let t = start(&e, false).await;
        let card = pending(&e, &t.id).await;
        (t, card)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let s = Arc::new(Store::open(&temp.path().join("approval.db")).unwrap());
        s.recover().unwrap();
        let e = Engine::new(s, 4).unwrap();
        e.decide_approval(&card.id, true, None).unwrap();
        assert!(matches!(
            error_kind(&e.resume(&t.id, "continue").await.unwrap_err()),
            ApprovalError::BindingConflict { .. }
        ));
        assert!(!temp.path().join("src/a.txt").exists());
        assert_eq!(event_count(&e, &t, "tool_start"), 0);
        assert_eq!(event_count(&e, &t, "approval.resolved"), 1);
        let raw: String = db(temp.path())
            .query_row("SELECT value FROM tasks WHERE id=?1", [&t.id], |r| r.get(0))
            .unwrap();
        assert!(!raw.contains("mock-secret-value"));
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[test]
fn ap15_claimed_before_execute_runtime_stops_then_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (t,card)=runtime.block_on(async {
        let (base,_server)=server(vec![vec![write("w","src/a.txt")]]).await; let e=setup(temp.path(),&base); let t=start(&e,false).await; let card=pending(&e,&t.id).await;
        db(temp.path()).execute_batch("CREATE TRIGGER stop_before_execute BEFORE INSERT ON events WHEN NEW.kind='file_backup' BEGIN SELECT RAISE(ABORT,'injected pre-execute stop'); END;").unwrap();
        e.decide_approval(&card.id,true,None).unwrap(); assert_eq!(settled(&e,&t.id).await.status,"failed"); assert_eq!(e.store.approval(&card.id).unwrap().execution_state,ExecutionState::Claimed); (t,card)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let s = Arc::new(Store::open(&temp.path().join("approval.db")).unwrap());
        s.recover().unwrap();
        let e = Engine::new(s, 4).unwrap();
        assert_eq!(
            e.store.approval(&card.id).unwrap().execution_state,
            ExecutionState::Unknown
        );
        assert!(matches!(
            error_kind(&e.resume(&t.id, "continue").await.unwrap_err()),
            ApprovalError::UnknownResult { .. }
        ));
        assert!(!temp.path().join("src/a.txt").exists());
        assert_eq!(event_count(&e, &t, "tool_result"), 0);
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[test]
fn ap15_real_command_effect_then_runtime_stop_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (t,card)=runtime.block_on(async {
        let c=call("cmd","run_command",json!({"command":"Add-Content -LiteralPath counter.txt -Value one; Start-Sleep -Seconds 120", "timeout_seconds":180}));
        let (base,_server)=server(vec![vec![c]]).await; let e=setup(temp.path(),&base); let t=start(&e,true).await; let card=pending(&e,&t.id).await; e.decide_approval(&card.id,true,None).unwrap();
        until(|| std::fs::read_to_string(temp.path().join("counter.txt")).ok().filter(|s| s.lines().count()==1)).await; assert_eq!(event_count(&e,&t,"tool_result"),0); (t,card)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let s = Arc::new(Store::open(&temp.path().join("approval.db")).unwrap());
        s.recover().unwrap();
        let e = Engine::new(s, 4).unwrap();
        assert_eq!(
            e.store.approval(&card.id).unwrap().execution_state,
            ExecutionState::Unknown
        );
        assert!(matches!(
            error_kind(&e.resume(&t.id, "continue").await.unwrap_err()),
            ApprovalError::UnknownResult { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(temp.path().join("counter.txt"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_ne!(e.store.task(&t.id).unwrap().status, "completed");
        assert_eq!(event_count(&e, &t, "tool_result"), 0);
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[test]
fn ap16_finish_transaction_fault_rolls_back() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (t,card)=runtime.block_on(async {
        let (base,_server)=server(vec![vec![write("w","src/a.txt")]]).await; let e=setup(temp.path(),&base); let t=start(&e,false).await; let card=pending(&e,&t.id).await;
        db(temp.path()).execute_batch("CREATE TRIGGER stop_finished BEFORE UPDATE OF execution_state ON approvals WHEN NEW.execution_state='finished' BEGIN SELECT RAISE(ABORT,'injected finish fault'); END;").unwrap();
        e.decide_approval(&card.id,true,None).unwrap(); let done=settled(&e,&t.id).await; assert_eq!(done.status,"failed"); assert!(temp.path().join("src/a.txt").exists()); assert_eq!(event_count(&e,&t,"tool_result"),0); assert!(!done.messages.iter().any(|m|m["role"]=="tool"));
        assert_eq!(e.store.approval(&card.id).unwrap().execution_state,ExecutionState::Claimed);
        let projected:String=db(temp.path()).query_row("SELECT status FROM turn_tasks WHERE legacy_task_id=?1",[&t.id],|r|r.get(0)).unwrap(); assert_eq!(projected,"failed"); (t,card)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    let s = Store::open(&temp.path().join("approval.db")).unwrap();
    s.recover().unwrap();
    assert_eq!(
        s.approval(&card.id).unwrap().execution_state,
        ExecutionState::Unknown
    );
    assert_eq!(
        s.events(&t.run_id, 0)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "tool_start")
            .count(),
        1
    );
}

#[test]
fn ap16_finished_restart_never_reexecutes_and_missing_result_is_corrupt() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (t, card) = runtime.block_on(async {
        let (base, _server) = server(vec![vec![write("w", "src/a.txt")]]).await;
        let e = setup(temp.path(), &base);
        let t = start(&e, false).await;
        let card = pending(&e, &t.id).await;
        e.decide_approval(&card.id, true, None).unwrap();
        settled(&e, &t.id).await;
        (t, card)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (base, _server) = server(vec![]).await;
        let e = setup(temp.path(), &base);
        e.store.recover().unwrap();
        let mut task = e.store.task(&t.id).unwrap();
        task.route.base_url = base;
        db(temp.path())
            .execute(
                "UPDATE tasks SET value=?2 WHERE id=?1",
                params![task.id, serde_json::to_string(&task).unwrap()],
            )
            .unwrap();
        std::fs::write(temp.path().join("src/a.txt"), "externally-updated").unwrap();
        e.resume(&t.id, "continue").await.unwrap();
        assert_eq!(settled(&e, &t.id).await.status, "completed");
        assert_eq!(event_count(&e, &t, "tool_start"), 1);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
            "externally-updated"
        );
        assert_eq!(
            e.store.approval(&card.id).unwrap().execution_state,
            ExecutionState::Finished
        );
        db(temp.path())
            .execute(
                "DELETE FROM events WHERE task_id=?1 AND kind='tool_result'",
                [&t.id],
            )
            .unwrap();
        assert!(matches!(
            error_kind(&e.store.approval(&card.id).unwrap_err()),
            ApprovalError::CorruptState { .. }
        ));
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[test]
fn ap19_schema6_preservation_atomic_migration_and_forged_constraints() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("approval.db");
    let s = Store::open(&path).unwrap();
    let c = write("w", "src/a.txt");
    let t = seed(&s, temp.path(), "a", vec![c], false);
    drop(s);
    // Restore exactly schema 6: the only removed objects are schema 7 objects.
    let conn = db(temp.path());
    conn.execute_batch("DROP TABLE workspace_restore_outcomes; DROP TABLE workspace_restores; DROP TABLE workspace_changes; DROP TABLE approvals; DELETE FROM schema_migrations WHERE id IN ('tool-call-approval-repository','workspace-change-repository'); PRAGMA user_version=6;").unwrap();
    let old: String = conn
        .query_row("SELECT value FROM tasks WHERE id='a'", [], |r| r.get(0))
        .unwrap();
    conn.execute_batch("CREATE TRIGGER migration_fault BEFORE INSERT ON schema_migrations WHEN NEW.id='tool-call-approval-repository' BEGIN SELECT RAISE(ABORT,'migration marker fault'); END;").unwrap();
    assert!(Store::open(&path).is_err());
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='approvals'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    conn.execute_batch("DROP TRIGGER migration_fault;").unwrap();
    let s = Store::open(&path).unwrap();
    assert_eq!(s.schema_version().unwrap(), 8);
    assert_eq!(s.task("a").unwrap().id, t.id);
    assert_eq!(
        conn.query_row("SELECT value FROM tasks WHERE id='a'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        old
    );
    drop(s);
    // Same columns/index names, missing CHECK: must fail without repairing it.
    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='approvals' AND type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute_batch("DROP TABLE approvals;").unwrap();
    conn.execute_batch(&ddl.replace(
        "CHECK (execution_state IN ('not_started','claimed','finished','unknown','cancelled'))",
        "",
    ))
    .unwrap();
    conn.execute_batch("CREATE UNIQUE INDEX approvals_task_call_unique ON approvals(task_id,tool_call_id); CREATE INDEX approvals_task_status ON approvals(task_id,status);").unwrap();
    assert!(Store::open(&path).is_err());
}

#[test]
fn ap20_public_query_order_and_corruption_distinct_from_missing() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(&temp.path().join("approval.db")).unwrap();
    let a = write("a", "src/a.txt");
    let b = write("b", "src/b.txt");
    let t = seed(&s, temp.path(), "task", vec![a.clone(), b.clone()], false);
    let (ar, _) = s.ensure_approval(&t, "a", "write_file", &args(&a)).unwrap();
    let (br, _) = s.ensure_approval(&t, "b", "write_file", &args(&b)).unwrap();
    let list = s.approvals_for_task("task").unwrap();
    let pairs: Vec<_> = list.iter().map(|v| (v.created_at, v.id.clone())).collect();
    let mut sorted = pairs.clone();
    sorted.sort();
    assert_eq!(pairs, sorted);
    assert_eq!(s.pending_approvals_for_task("task").unwrap().len(), 2);
    assert!(s.approvals_for_task("other").unwrap().is_empty());
    assert!(matches!(
        error_kind(&s.approval("missing").unwrap_err()),
        ApprovalError::NotFound { .. }
    ));
    let raw = serde_json::to_string(&list).unwrap();
    assert!(!raw.contains("approval-body"));
    assert!(!raw.contains("arguments"));
    let corrupt_db = db(temp.path());
    corrupt_db
        .execute_batch("PRAGMA foreign_keys=OFF;")
        .unwrap();
    corrupt_db
        .execute(
            "UPDATE approvals SET task_id='missing' WHERE id=?1",
            [&ar.id],
        )
        .unwrap();
    assert!(matches!(
        error_kind(&s.approval(&ar.id).unwrap_err()),
        ApprovalError::CorruptState { .. }
    ));
    assert!(matches!(
        error_kind(&s.decide_approval(&ar.id, true, "user").unwrap_err()),
        ApprovalError::CorruptState { .. }
    ));
    assert_eq!(s.approval(&br.id).unwrap().status, ApprovalStatus::Pending);
}

#[test]
fn ap15_approved_before_claim_stopped_runtime_executes_once() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (task, card) = runtime.block_on(async {
        let (base, _server) = server(vec![vec![write("w", "src/a.txt")]]).await;
        let e = setup(temp.path(), &base);
        let task = start(&e, false).await;
        let card = pending(&e, &task.id).await;
        // No await after decide: this single-thread runtime cannot poll the
        // worker between the committed approval and shutdown.
        e.decide_approval(&card.id, true, None).unwrap();
        assert_eq!(
            e.store.approval(&card.id).unwrap().execution_state,
            ExecutionState::NotStarted
        );
        (task, card)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    assert!(!temp.path().join("src/a.txt").exists());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (base, _server) = server(vec![]).await;
        let e = setup(temp.path(), &base);
        e.store.recover().unwrap();
        let mut t = e.store.task(&task.id).unwrap();
        t.route.base_url = base;
        db(temp.path())
            .execute(
                "UPDATE tasks SET value=?2 WHERE id=?1",
                params![t.id, serde_json::to_string(&t).unwrap()],
            )
            .unwrap();
        e.resume(&task.id, "continue").await.unwrap();
        assert_eq!(settled(&e, &task.id).await.status, "completed");
        assert_eq!(event_count(&e, &task, "tool_start"), 1);
        assert_eq!(
            e.store.approval(&card.id).unwrap().execution_state,
            ExecutionState::Finished
        );
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[tokio::test]
async fn ap18_decision_and_cancel_from_other_store_are_observed() {
    for approve in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let (base, _server) = server(vec![vec![write("w", "src/a.txt")]]).await;
        let e = setup(temp.path(), &base);
        let task = start(&e, false).await;
        let card = pending(&e, &task.id).await;
        // Public Store updates intentionally do not touch Engine's waiter map.
        let other = Store::open(&temp.path().join("approval.db")).unwrap();
        if approve {
            other.decide_approval(&card.id, true, "user").unwrap();
        } else {
            other.cancel_pending_approvals(&task.id).unwrap();
        }
        let done = settled(&e, &task.id).await;
        assert_eq!(done.status, if approve { "completed" } else { "cancelled" });
        assert_eq!(temp.path().join("src/a.txt").exists(), approve);
        assert_eq!(event_count(&e, &task, "approval.resolved"), 1);
    }
}

#[test]
fn ap18_cancel_before_new_waiter_resume_has_safe_cancel_result() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let task = runtime.block_on(async {
        let (base, _server) = server(vec![vec![write("w", "src/a.txt")]]).await;
        let e = setup(temp.path(), &base);
        let task = start(&e, false).await;
        pending(&e, &task.id).await;
        task
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (base, _server) = server(vec![]).await;
        let e = setup(temp.path(), &base);
        e.store.cancel_pending_approvals(&task.id).unwrap();
        let mut t = e.store.task(&task.id).unwrap();
        t.route.base_url = base;
        db(temp.path())
            .execute(
                "UPDATE tasks SET value=?2 WHERE id=?1",
                params![t.id, serde_json::to_string(&t).unwrap()],
            )
            .unwrap();
        e.resume(&task.id, "continue").await.unwrap();
        let done = settled(&e, &task.id).await;
        assert!(done.messages.iter().any(|m| m["role"] == "tool"
            && m["content"] == approval::tool_error(ApprovalStatus::Cancelled)));
        assert!(!temp.path().join("src/a.txt").exists());
        assert_eq!(event_count(&e, &task, "tool_start"), 0);
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[tokio::test]
async fn ap16_public_finish_failure_preserves_all_transaction_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let s = Store::open(&temp.path().join("approval.db")).unwrap();
    let c = write("w", "src/a.txt");
    let task = seed(&s, temp.path(), "a", vec![c.clone()], false);
    let (card, _) = s
        .ensure_approval(&task, "w", "write_file", &args(&c))
        .unwrap();
    s.decide_approval(&card.id, true, "user").unwrap();
    s.claim_approval(&task, "w", "write_file", &args(&c))
        .unwrap();
    let result = peachsh::workspace::execute(&task, "write_file", &args(&c))
        .await
        .unwrap();
    let conn = db(temp.path());
    let before: String = conn
        .query_row("SELECT value FROM tasks WHERE id='a'", [], |r| r.get(0))
        .unwrap();
    let before_events = s.events(&task.run_id, 0).unwrap().len();
    conn.execute_batch("CREATE TRIGGER finish_fault BEFORE UPDATE OF execution_state ON approvals WHEN NEW.execution_state='finished' BEGIN SELECT RAISE(ABORT,'finish fault'); END;").unwrap();
    assert!(
        s.finish_approval(&task, &card.id, &result.to_string(), "write_file", "w")
            .is_err()
    );
    assert_eq!(
        conn.query_row("SELECT value FROM tasks WHERE id='a'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        before
    );
    assert_eq!(s.events(&task.run_id, 0).unwrap().len(), before_events);
    assert_eq!(
        s.approval(&card.id).unwrap().execution_state,
        ExecutionState::Claimed
    );
    assert!(temp.path().join("src/a.txt").exists());
}

#[test]
fn ap19_constraints_enforced_and_partial_ddl_retry() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("approval.db");
    let s = Store::open(&path).unwrap();
    let c = write("w", "src/a.txt");
    let task = seed(&s, temp.path(), "a", vec![c.clone()], false);
    let (record, _) = s
        .ensure_approval(&task, "w", "write_file", &args(&c))
        .unwrap();
    let conn = db(temp.path());
    for sql in [
        "UPDATE approvals SET execution_state='fake'",
        "UPDATE approvals SET task_id=NULL",
        "UPDATE approvals SET tool_call_id=NULL",
        "UPDATE approvals SET task_id='missing'",
        "UPDATE approvals SET status='fake'",
    ] {
        assert!(
            matches!(
                conn.execute(sql, []),
                Err(rusqlite::Error::SqliteFailure(_, _))
            ),
            "{sql}"
        );
    }
    drop(s);
    conn.execute_batch("DELETE FROM schema_migrations WHERE id IN ('tool-call-approval-repository','workspace-change-repository'); PRAGMA user_version=6;").unwrap();
    let s = Store::open(&path).unwrap();
    assert_eq!(s.schema_version().unwrap(), 8);
    assert_eq!(s.approval(&record.id).unwrap(), record);
    drop(s);
    conn.execute_batch("DROP INDEX approvals_task_call_unique; CREATE INDEX approvals_task_call_unique ON approvals(task_id,tool_call_id);").unwrap();
    assert!(Store::open(&path).is_err());
}

#[test]
fn ap14_persisted_workspace_and_permission_changes_conflict() {
    for mode in 0..4 {
        let temp = tempfile::tempdir().unwrap();
        let s = Store::open(&temp.path().join("approval.db")).unwrap();
        let c = call(
            "cmd",
            "run_command",
            json!({"command":"Write-Output original"}),
        );
        let t = seed(&s, temp.path(), "a", vec![c.clone()], true);
        let (card, _) = s
            .ensure_approval(&t, "cmd", "run_command", &args(&c))
            .unwrap();
        s.decide_approval(&card.id, true, "user").unwrap();
        let mut changed = t.clone();
        match mode {
            0 => changed.workspace.push_str("/changed"),
            1 => changed.spec.allow_commands = false,
            2 => changed.spec.write_scopes = vec!["*".into()],
            _ => {
                changed.messages[1]["tool_calls"][0] = call(
                    "cmd",
                    "run_command",
                    json!({"command":"Write-Output changed"}),
                )
            }
        }
        db(temp.path())
            .execute(
                "UPDATE tasks SET value=?2 WHERE id=?1",
                params![changed.id, serde_json::to_string(&changed).unwrap()],
            )
            .unwrap();
        let count = s.events(&t.run_id, 0).unwrap().len();
        assert!(matches!(
            error_kind(
                &s.claim_approval(&t, "cmd", "run_command", &args(&c))
                    .unwrap_err()
            ),
            ApprovalError::BindingConflict { .. }
        ));
        assert_eq!(s.events(&t.run_id, 0).unwrap().len(), count);
        assert_eq!(s.approvals_for_task(&t.id).unwrap().len(), 1);
    }
}

#[tokio::test]
async fn ap17_cancel_after_real_claim_preserves_real_result_and_stops_next_call() {
    let temp = tempfile::tempdir().unwrap();
    let command = call(
        "cmd",
        "run_command",
        json!({"command":"Add-Content -LiteralPath counter.txt -Value one; while (-not (Test-Path -LiteralPath release.txt)) { Start-Sleep -Milliseconds 10 }; Write-Output done"}),
    );
    let (base, _server) = server(vec![vec![command, write("next", "src/next.txt")]]).await;
    let e = setup(temp.path(), &base);
    let task = start(&e, true).await;
    let card = pending(&e, &task.id).await;
    e.decide_approval(&card.id, true, None).unwrap();
    until(|| {
        std::fs::read_to_string(temp.path().join("counter.txt"))
            .ok()
            .filter(|s| s.lines().count() == 1)
    })
    .await;
    e.cancel(&task.id).unwrap();
    assert_eq!(
        e.store.approval(&card.id).unwrap().status,
        ApprovalStatus::Approved
    );
    std::fs::write(temp.path().join("release.txt"), "release").unwrap();
    let done = settled(&e, &task.id).await;
    assert_eq!(done.status, "cancelled");
    assert_eq!(
        e.store.approval(&card.id).unwrap().execution_state,
        ExecutionState::Finished
    );
    assert_eq!(event_count(&e, &task, "tool_result"), 1);
    assert_eq!(event_count(&e, &task, "approval.resolved"), 1);
    assert_eq!(e.store.approvals_for_task(&task.id).unwrap().len(), 1);
    assert!(!temp.path().join("src/next.txt").exists());
}

#[tokio::test]
async fn ap13_empty_id_and_ambiguous_history_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _server) = server(vec![vec![
        write("first", "src/a.txt"),
        write("", "src/b.txt"),
    ]])
    .await;
    let e = setup(temp.path(), &base);
    let task = start(&e, false).await;
    assert_eq!(settled(&e, &task.id).await.status, "failed");
    assert!(e.store.approvals_for_task(&task.id).unwrap().is_empty());
    assert_eq!(event_count(&e, &task, "tool_start"), 0);
    let mut damaged = e.store.task(&task.id).unwrap();
    damaged.messages.push(json!({"role":"assistant","tool_calls":[write("dup","src/a.txt"),write("dup","src/b.txt")]}));
    db(temp.path())
        .execute(
            "UPDATE tasks SET value=?2 WHERE id=?1",
            params![damaged.id, serde_json::to_string(&damaged).unwrap()],
        )
        .unwrap();
    assert!(matches!(
        error_kind(&e.resume(&task.id, "continue").await.unwrap_err()),
        ApprovalError::CorruptState { .. }
    ));
    assert_eq!(event_count(&e, &task, "tool_start"), 0);
}

#[test]
fn ap17_multistore_cancel_claim_barrier() {
    for _ in 0..12 {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("approval.db");
        let s = Store::open(&path).unwrap();
        let c = write("w", "src/a.txt");
        let task = seed(&s, temp.path(), "a", vec![c.clone()], false);
        let (card, _) = s
            .ensure_approval(&task, "w", "write_file", &args(&c))
            .unwrap();
        s.decide_approval(&card.id, true, "user").unwrap();
        let claim_store = Store::open(&path).unwrap();
        let cancel_store = Store::open(&path).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let b = barrier.clone();
        let a = args(&c);
        let t = task.clone();
        let claim = std::thread::spawn(move || {
            b.wait();
            claim_store.claim_approval(&t, "w", "write_file", &a)
        });
        let cancel = std::thread::spawn(move || {
            barrier.wait();
            cancel_store.cancel_pending_approvals("a")
        });
        let claimed = claim.join().unwrap().is_ok();
        cancel.join().unwrap().unwrap();
        let current = s.approval(&card.id).unwrap();
        assert_eq!(current.status, ApprovalStatus::Approved);
        assert_eq!(
            current.execution_state,
            if claimed {
                ExecutionState::Claimed
            } else {
                ExecutionState::Cancelled
            }
        );
        assert_eq!(s.task("a").unwrap().status, "cancelled");
        assert_eq!(
            s.events(&task.run_id, 0)
                .unwrap()
                .iter()
                .filter(|e| e.kind == "approval.resolved")
                .count(),
            1
        );
    }
}

#[test]
fn ap06_reconcile_preserves_order_with_pending_and_unapproved_calls() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (task, card) = runtime.block_on(async {
        let (base, _server) = server(vec![vec![
            write("first", "src/a.txt"),
            write("second", "src/b.txt"),
        ]])
        .await;
        let e = setup(temp.path(), &base);
        let task = start(&e, false).await;
        let card = pending(&e, &task.id).await;
        (task, card)
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (base, _server) = server(vec![]).await;
        let e = setup(temp.path(), &base);
        e.store.recover().unwrap();
        let mut t = e.store.task(&task.id).unwrap();
        t.route.base_url = base;
        db(temp.path())
            .execute(
                "UPDATE tasks SET value=?2 WHERE id=?1",
                params![t.id, serde_json::to_string(&t).unwrap()],
            )
            .unwrap();
        e.decide_approval(&card.id, true, None).unwrap();
        e.resume(&task.id, "continue").await.unwrap();
        let done = settled(&e, &task.id).await;
        assert_eq!(done.status, "completed");
        let ids: Vec<_> = done
            .messages
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["tool_call_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["first", "second"]);
        assert_eq!(event_count(&e, &task, "tool_start"), 1);
        assert!(!temp.path().join("src/b.txt").exists());
        assert_eq!(done.messages.last().unwrap()["role"], "assistant");
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
}
