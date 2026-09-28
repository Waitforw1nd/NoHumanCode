#[path = "support/project_fixture.rs"]
#[allow(dead_code)]
mod project_fixture;
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use peachsh::{budget::*, domain::*, engine::Engine, network_authorization::*, store::Store};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Clone)]
struct Mock {
    responses: Arc<std::sync::Mutex<std::collections::VecDeque<Value>>>,
    hits: Arc<AtomicUsize>,
}
async fn reply(State(mock): State<Mock>) -> String {
    mock.hits.fetch_add(1, Ordering::SeqCst);
    let chunk = mock
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected completion");
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}
fn response(tool: bool, usage: Value) -> Value {
    json!({"choices":[{"delta":if tool{json!({"tool_calls":[{"index":0,"id":"read-1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"sample.txt\"}"}}]})}else{json!({"content":"323"})},"finish_reason":if tool{"tool_calls"}else{"stop"}}],"usage":usage})
}
fn usage() -> Value {
    json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5})
}
async fn setup(
    responses: Vec<Value>,
) -> (
    tempfile::TempDir,
    Arc<Engine>,
    ProjectId,
    Mock,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.txt"), "READ-MARKER").unwrap();
    let mock = Mock {
        responses: Arc::new(std::sync::Mutex::new(responses.into())),
        hits: Arc::new(AtomicUsize::new(0)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/chat/completions", post(reply))
        .route(
            "/models",
            get(|| async { Json(json!({"data":[{"id":"mock"}]})) }),
        )
        .with_state(mock.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = Arc::new(Store::open(&dir.path().join("budget.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "Route".into(),
                base_url: base,
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route::route", "budget-fixture").unwrap();
    let project = project_fixture::configured_project(&store);
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(project.clone()),
    };
    let p = store.network_preview(subject.clone()).unwrap();
    store
        .network_confirm(
            subject,
            ConfirmRequest {
                config_revision: p.config_revision,
                credential_revision: p.credential_revision,
                binding_digest: p.binding_digest,
                purposes: vec![NetworkPurpose::Models, NetworkPurpose::Completion],
            },
            "fixture-grant",
        )
        .unwrap();
    let engine = Engine::new(store, 2).unwrap();
    engine
        .configure_budget(ConfigureBudget {
            project_id: project.clone(),
            expected_revision: 0,
            idempotency_key: "budget".into(),
            config: BudgetConfig {
                limit_tokens: 1000,
                per_call_reservation_tokens: 100,
            },
        })
        .unwrap();
    (dir, engine, project, mock, server)
}
fn request(tools: bool, rounds: usize) -> RunRequest {
    RunRequest {
        title: "budget".into(),
        kind: SessionKind::Chat,
        tasks: vec![TaskSpec {
            name: "agent".into(),
            role: "developer".into(),
            route_id: "route".into(),
            prompt: "read or reply".into(),
            depends_on: vec![],
            write_scopes: vec!["*".into()],
            tools,
            allow_commands: false,
            max_rounds: rounds,
        }],
    }
}
async fn done(engine: &Engine, id: &str) -> Task {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let task = engine.store.task(id).unwrap();
            if !engine.is_busy() {
                return task;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn route_check_without_task_is_managed_and_models_never_spend() {
    let (_dir, e, p, m, s) = setup(vec![response(false, usage())]).await;
    let result = e.route_check("route").await.unwrap();
    assert_eq!(result.calls.len(), 2);
    let status = e.budget_status(&p).unwrap();
    assert_eq!((status.spent_tokens, status.held_tokens), (5, 0));
    assert_eq!(
        e.budget_call(&p, &result.calls[0].call_id)
            .unwrap_err()
            .kind,
        BudgetErrorKind::NotFound
    );
    let call = e.budget_call(&p, &result.calls[1].call_id).unwrap();
    assert!(call.call.task_id.is_none());
    assert!(call.managed);
    assert_eq!(m.hits.load(Ordering::SeqCst), 1);
    s.abort();
}
#[tokio::test]
async fn successful_missing_and_invalid_usage_allow_legal_tool_but_block_next_completion() {
    for bad in [Value::Null, json!({"total_tokens":1})] {
        let (_dir, e, p, m, s) = setup(vec![response(true, bad.clone())]).await;
        let run = e.start(request(true, 3)).await.unwrap();
        let task = done(&e, &run.tasks[0].id).await;
        assert_eq!(task.status, "failed");
        assert_eq!(m.hits.load(Ordering::SeqCst), 1);
        assert!(task.messages.iter().any(|v| v["role"] == "assistant"));
        assert!(task.messages.iter().any(|v| {
            v["role"] == "tool"
                && v["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("READ-MARKER"))
        }));
        let calls = e.network_calls_for_task(&task.id).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].state, CallState::Finished);
        let call = e.budget_call(&p, &calls[0].call_id).unwrap();
        assert_eq!(
            call.state,
            Some(if bad.is_null() {
                BudgetCallState::UsageMissing
            } else {
                BudgetCallState::UsageInvalid
            })
        );
        e.resume(&task.id, "continue").await.unwrap();
        done(&e, &task.id).await;
        assert_eq!(m.hits.load(Ordering::SeqCst), 1);
        assert_eq!(e.budget_status(&p).unwrap().held_tokens, 100);
        s.abort();
    }
}
#[tokio::test]
async fn agent_tool_rounds_resume_and_new_turn_each_create_distinct_budget_calls() {
    let (dir, e, p, m, s) = setup(vec![
        response(true, usage()),
        response(false, usage()),
        response(false, usage()),
    ])
    .await;
    let run = e.start(request(true, 1)).await.unwrap();
    let task = done(&e, &run.tasks[0].id).await;
    assert_eq!(task.status, "failed");
    assert!(task.messages.iter().any(|v| v["role"] == "tool"));
    e.resume(&task.id, "continue").await.unwrap();
    let task = done(&e, &task.id).await;
    assert_eq!(task.status, "completed");
    assert_eq!(e.network_calls_for_task(&task.id).unwrap().len(), 2);
    let db = rusqlite::Connection::open(dir.path().join("budget.db")).unwrap();
    let (session, agent, turn): (String, String, String) = db
        .query_row(
            "SELECT session_id,agent_id,turn_id FROM turn_tasks WHERE legacy_task_id=?1",
            [&task.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let command = SendChatTurn {
        session_id: SessionId(session),
        agent_id: AgentId(agent),
        expected_last_turn_id: TurnId(turn),
        message: "another turn".into(),
        idempotency_key: "next-budget-turn".into(),
    };
    let receipt = e.send_chat_turn(command.clone()).await.unwrap();
    let next = done(&e, &receipt.task.legacy_task_id).await;
    assert_eq!(next.status, "completed");
    assert!(e.send_chat_turn(command).await.unwrap().replayed);
    assert_eq!(m.hits.load(Ordering::SeqCst), 3);
    assert_eq!(e.budget_status(&p).unwrap().spent_tokens, 15);
    assert_eq!(e.network_calls_for_task(&next.id).unwrap().len(), 1);
    s.abort();
}
