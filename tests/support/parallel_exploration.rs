use super::*;
use coda::budget::{BudgetCallState, BudgetConfig, ConfigureBudget};
use tokio::sync::{mpsc, oneshot};

const PROMPT: &str = "PARENT_ONLY_MARKER";
const NAMES: [&str; 3] = ["alpha", "beta", "gamma"];
const CALL_ID: &str = "parallel-call";

pub(super) struct Request {
    pub(super) body: Value,
    response: oneshot::Sender<(Value, &'static str)>,
}

impl Request {
    pub(super) fn reply(self, delta: Value, reason: &'static str) {
        self.response
            .send((delta, reason))
            .expect("request was cancelled");
    }

    pub(super) fn answer(self, text: &str) {
        self.reply(json!({"content":text}), "stop");
    }

    fn child(&self) -> usize {
        let messages = self.body["messages"].to_string();
        let index = NAMES
            .iter()
            .position(|name| messages.contains(&format!("question-{name}")))
            .unwrap();
        assert!(!messages.contains(PROMPT));
        assert!(self.body.get("tools").is_none() || self.body["tools"] == json!([]));
        for (i, name) in NAMES.iter().enumerate() {
            assert_eq!(
                messages.contains(&format!("source-only-{name}")),
                i == index
            );
            assert_eq!(messages.contains(&format!("question-{name}")), i == index);
        }
        index
    }

    fn tool_result(&self) -> Value {
        let messages = self.body["messages"].as_array().unwrap();
        let results: Vec<_> = messages
            .iter()
            .filter(|m| m["tool_call_id"] == CALL_ID)
            .collect();
        assert_eq!(results.len(), 1);
        assert!(!self.body["messages"].to_string().contains("source-only-"));
        serde_json::from_str(results[0]["content"].as_str().unwrap()).unwrap()
    }
}

pub(super) struct Fixture {
    dir: tempfile::TempDir,
    _server: Server,
    pub(super) requests: mpsc::UnboundedReceiver<Request>,
    pub(super) engine: Arc<Engine>,
    pub(super) run: Run,
    pub(super) project: coda::domain::ProjectId,
}

struct Server(tokio::task::JoinHandle<()>);

impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Fixture {
    pub(super) async fn start_parent(global: usize, key: usize, budget: u64) -> Self {
        let (send, requests) = mpsc::unbounded_channel();
        let app = Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<Value>| {
                let send = send.clone();
                async move {
                    let (response, rx) = oneshot::channel();
                    if send.send(Request { body, response }).is_err() {
                        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let Ok((delta, reason)) = rx.await else {
                        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                    };
                    let frame = json!({"choices":[{"delta":delta,"finish_reason":reason}],
                    "usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}});
                    (
                        [("content-type", "text/event-stream")],
                        format!("data: {frame}\n\ndata: [DONE]\n\n"),
                    )
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        for name in NAMES {
            let branch = workspace.join(name);
            std::fs::create_dir(&branch).unwrap();
            std::fs::write(
                branch.join("sample.txt"),
                format!("{}search-marker source-only-{name}\n", "line\n".repeat(810)),
            )
            .unwrap();
        }
        let store = Arc::new(Store::open(&dir.path().join("state.db")).unwrap());
        store
            .save_settings(&Settings {
                workspace: workspace.to_string_lossy().into_owned(),
                max_concurrency: global,
                routes: vec![Route {
                    id: "local".into(),
                    name: "local".into(),
                    base_url,
                    model: "explore-test".into(),
                    protocol: None,
                    key_env: Some("CARGO_PKG_NAME".into()),
                    max_tokens: 128,
                    parallel_limit: key,
                }],
                newapi: None,
            })
            .unwrap();
        network_fixture::authorize_routes(&store);
        let project = store.current_project().unwrap().project_id.unwrap();
        store
            .configure_budget(ConfigureBudget {
                project_id: project.clone(),
                expected_revision: 0,
                idempotency_key: "parallel".into(),
                config: BudgetConfig {
                    limit_tokens: budget,
                    per_call_reservation_tokens: 3,
                },
            })
            .unwrap();
        let engine = Engine::new(store, global).unwrap();
        let run = engine
            .start(RunRequest {
                title: "parallel".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "main".into(),
                    role: "coding".into(),
                    route_id: "local".into(),
                    prompt: PROMPT.into(),
                    depends_on: vec![],
                    write_scopes: vec![],
                    tools: true,
                    allow_commands: false,
                    max_rounds: 3,
                }],
            })
            .await
            .unwrap();
        Self {
            dir,
            _server: Server(server),
            requests,
            engine,
            run,
            project,
        }
    }

    async fn start(global: usize, key: usize, budget: u64, last_path: &str) -> Self {
        let mut fixture = Self::start_parent(global, key, budget).await;
        let first = fixture.next().await;
        assert!(first.body["messages"].to_string().contains(PROMPT));
        assert!(
            first.body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "explore_parallel")
        );
        let mut branches: Vec<_> = NAMES.iter().map(|name| json!({"question":format!("question-{name}"), "path":name,"query":"search-marker"})).collect();
        branches.push(json!({"question":"empty","path":last_path,"query":"absent-query"}));
        first.reply(json!({"tool_calls":[{"index":0,"id":CALL_ID,"type":"function","function":{"name":"explore_parallel","arguments":json!({"branches":branches}).to_string()}}]}), "tool_calls");
        fixture
    }

    pub(super) async fn next(&mut self) -> Request {
        tokio::time::timeout(Duration::from_secs(10), self.requests.recv())
            .await
            .expect("provider request timed out")
            .unwrap()
    }

    async fn settled(&self, status: &str, sends: usize, spent: u64) {
        let run = wait_for_run(&self.engine, &self.run.id).await;
        let task = &run.tasks[0];
        assert_eq!(task.status, status, "{:?}", task.error);
        assert_eq!(task.usage["total_tokens"], spent);
        let calls = self.engine.network_calls_for_task(&task.id).unwrap();
        assert_eq!(calls.len(), sends);
        for call in calls {
            let receipt = self
                .engine
                .budget_call(&self.project, &call.call_id)
                .unwrap();
            assert_eq!(receipt.state, Some(BudgetCallState::Settled));
            assert_eq!(receipt.call, call);
        }
        let budget = self.engine.budget_status(&self.project).unwrap();
        assert_eq!((budget.spent_tokens, budget.held_tokens), (spent, 0));
    }

    async fn finish_success(&mut self) {
        let parent = self.next().await;
        let result = parent.tool_result();
        let branches = result["branches"].as_array().unwrap();
        assert_eq!(branches.len(), 4);
        for (i, branch) in branches.iter().enumerate() {
            assert_eq!(branch["index"], i + 1);
            if let Some(name) = NAMES.get(i) {
                assert_eq!(branch["status"], "ok");
                assert_eq!(branch["summary"], format!("summary-{name}"));
                let evidence = branch["evidence"].as_array().unwrap();
                assert_eq!(evidence.len(), 1);
                let item = &evidence[0];
                assert_eq!(item["path"], format!("{name}/sample.txt"));
                assert!(item["start_line"].as_u64().unwrap() <= 811);
                assert!(item["end_line"].as_u64().unwrap() >= 811);
            } else {
                assert_eq!(branch["status"], "no_matches");
                assert_eq!(branch["evidence"], json!([]));
            }
        }
        parent.answer("parent done");
        self.settled("completed", 5, 15).await;
    }
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn simultaneous_children_return_ordered_results_and_exact_usage() {
    let mut f = Fixture::start(2, 2, 100, "alpha").await;
    let first = f.next().await;
    let second = f.next().await;
    let (alpha, beta) = if first.child() == 0 {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!((alpha.child(), beta.child()), (0, 1));
    beta.answer("summary-beta");
    // Await the next dispatch before releasing alpha, proving beta finished first.
    let gamma = f.next().await;
    assert_eq!(gamma.child(), 2);
    gamma.answer("summary-gamma");
    alpha.answer("summary-alpha");
    f.finish_success().await;
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn single_global_or_credential_slot_serves_the_whole_queue() {
    for (global, key) in [(1, 2), (2, 1)] {
        let mut f = Fixture::start(global, key, 100, "alpha").await;
        let mut seen = Vec::new();
        for turn in 0..NAMES.len() {
            let child = f.next().await;
            let i = child.child();
            seen.push(i);
            if turn == 0 {
                // Hold the first wire response while a competing send could reach the same server.
                assert!(
                    tokio::time::timeout(Duration::from_secs(1), f.requests.recv())
                        .await
                        .is_err(),
                    "a second child reached the provider with only one slot"
                );
            }
            child.answer(&format!("summary-{}", NAMES[i]));
        }
        seen.sort();
        assert_eq!(seen, [0, 1, 2]);
        f.finish_success().await;
    }
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn denial_after_one_child_stops_queued_sends() {
    let mut f = Fixture::start(1, 2, 6, "alpha").await;
    let child = f.next().await;
    assert_eq!(child.child(), 0);
    child.answer("summary-alpha");
    f.settled("failed", 2, 6).await;
    let task = f.engine.store.task(&f.run.tasks[0].id).unwrap();
    assert!(
        task.error
            .unwrap()
            .contains(coda::budget::BudgetErrorKind::Exceeded.code())
    );
    assert!(f.requests.try_recv().is_err());
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn invalid_last_directory_rejects_batch_before_first_child() {
    let mut f = Fixture::start(2, 2, 100, "../").await;
    let parent = f.next().await;
    assert!(parent.tool_result()["error"].is_string());
    parent.answer("parent done");
    f.settled("completed", 2, 6).await;
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn cancellation_settles_inflight_and_resume_after_restart_skips_batch() {
    let mut f = Fixture::start(2, 2, 100, "alpha").await;
    let first = f.next().await;
    let second = f.next().await;
    assert_ne!(first.child(), second.child());
    let id = f.run.tasks[0].id.clone();
    f.engine.cancel(&id).unwrap();
    assert!(!f.engine.shutdown(Duration::from_secs(10)).await.timed_out);
    drop((first, second));
    assert_eq!(f.engine.store.task(&id).unwrap().status, "cancelled");
    let calls = f.engine.network_calls_for_task(&id).unwrap();
    assert_eq!(calls.len(), 3);
    let unknown = calls
        .iter()
        .filter(|c| c.result == Some(coda::network_authorization::CallResult::TransportUnknown))
        .count();
    assert_eq!(unknown, 2);
    for call in &calls {
        let expected =
            if call.result == Some(coda::network_authorization::CallResult::TransportUnknown) {
                BudgetCallState::Unknown
            } else {
                BudgetCallState::Settled
            };
        assert_eq!(
            f.engine
                .budget_call(&f.project, &call.call_id)
                .unwrap()
                .state,
            Some(expected)
        );
    }
    drop(f.engine);
    let store = Arc::new(Store::open(&f.dir.path().join("state.db")).unwrap());
    f.engine = Engine::new(store, 2).unwrap();
    f.engine
        .resume(&id, "continue with available results")
        .await
        .unwrap();
    let next = f.next().await;
    let messages = next.body["messages"].as_array().unwrap();
    let tool = messages
        .iter()
        .find(|m| m["tool_call_id"] == CALL_ID)
        .unwrap();
    assert!(tool["content"].as_str().unwrap().contains("usage"));
    next.answer("continued");
    let resumed = wait_for_run(&f.engine, &f.run.id).await;
    assert_eq!(resumed.tasks[0].status, "completed");
    assert!(resumed.tasks[0].output.ends_with("continued"));
    assert_eq!(f.engine.network_calls_for_task(&id).unwrap().len(), 4);
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn exhausted_budget_drains_the_already_dispatched_sibling() {
    // Only the initial parent and one child fit. Either of the first two branches can win.
    let mut f = Fixture::start(2, 2, 6, "alpha").await;
    let inflight = f.next().await;
    let i = inflight.child();
    assert!(i < 2);
    let budget = f.engine.budget_status(&f.project).unwrap();
    assert_eq!((budget.spent_tokens, budget.held_tokens), (3, 3));
    assert_eq!(budget.available_tokens, Some(0));
    // Keep the upstream response pending while the competing child seeks admission.
    assert!(
        tokio::time::timeout(Duration::from_secs(1), f.requests.recv())
            .await
            .is_err()
    );
    inflight.answer(&format!("summary-{}", NAMES[i]));
    f.settled("failed", 2, 6).await;
    let task = f.engine.store.task(&f.run.tasks[0].id).unwrap();
    assert!(
        task.error
            .unwrap()
            .contains(coda::budget::BudgetErrorKind::Exceeded.code())
    );
    assert!(f.requests.try_recv().is_err());
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn reasoning_without_summary_is_a_charged_branch_error_and_preserves_siblings() {
    let mut f = Fixture::start(2, 2, 100, "alpha").await;
    for _ in NAMES {
        let child = f.next().await;
        let i = child.child();
        if i == 1 {
            child.reply(
                json!({"reasoning_content":"private-child-reasoning"}),
                "stop",
            );
        } else {
            child.answer(&format!("summary-{}", NAMES[i]));
        }
    }
    let parent = f.next().await;
    let result = parent.tool_result();
    let branches = result["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 4);
    assert_eq!(branches[0]["summary"], "summary-alpha");
    assert!(!result.to_string().contains("private-child-reasoning"));
    let error = &branches[1];
    assert_eq!(error["status"], "error");
    assert!(error["error"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(error["evidence"][0]["path"], "beta/sample.txt");
    assert_eq!(branches[2]["summary"], "summary-gamma");
    assert_eq!(branches[3]["status"], "no_matches");
    parent.answer("parent done");
    f.settled("completed", 5, 15).await;
}

#[tokio::test]
#[cfg_attr(
    not(windows),
    ignore = "persistent network authorization requires Windows DPAPI"
)]
async fn revocation_while_credential_is_busy_rejects_waiting_and_queued_children() {
    let mut f = Fixture::start(2, 1, 100, "alpha").await;
    let inflight = f.next().await;
    let i = inflight.child();
    assert!(i < 2);
    let id = &f.run.tasks[0].id;
    let before = f.engine.network_calls_for_task(id).unwrap();
    assert_eq!(before.len(), 2);
    let child = before.iter().find(|c| c.result.is_none()).unwrap();
    assert_eq!(child.subject.project_id.as_ref(), Some(&f.project));
    f.engine
        .network_revoke(&child.authorization_id)
        .await
        .unwrap();

    inflight.answer(&format!("summary-{}", NAMES[i]));
    f.settled("failed", 2, 6).await;
    assert!(f.requests.try_recv().is_err());
    let after = f.engine.network_calls_for_task(id).unwrap();
    for sent in before {
        let receipt = after.iter().find(|c| c.call_id == sent.call_id).unwrap();
        assert_eq!(
            receipt.result,
            Some(coda::network_authorization::CallResult::Success)
        );
    }
}
