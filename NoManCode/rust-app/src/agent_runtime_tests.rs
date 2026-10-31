use crate::{
    agent_runtime::{
        AgentRuntime, AgentStrategy, CompletionKind, StrategyAction, StrategyError, StrategyInput,
        ToolClass, ToolVote,
    },
    budget::{BudgetConfig, ConfigureBudget},
    domain::*,
    engine::Engine,
    network_authorization::*,
    project_config::*,
    provider_runtime::{
        CodecError, CompletionCodec, CompletionDecoder, CompletionInput, CompletionOutput, Emit,
        ProviderRuntime,
    },
    store::Store,
};
use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const SECRET: &str = "nhc-agent-callback-sentinel";

fn bounded_child(test_name: &str, env_name: &str) -> std::process::Output {
    bounded_child_with_value(test_name, env_name, "1")
}
fn bounded_child_with_value(
    test_name: &str,
    env_name: &str,
    env_value: &str,
) -> std::process::Output {
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env(env_name, env_value)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bounded child timed out: {test_name}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
struct OwnedChild(std::process::Child);
impl std::ops::Deref for OwnedChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
#[derive(Clone, Copy, Debug)]
enum Mode {
    All,
    ReadOnly,
    Deny,
    Duplicate,
    Reverse,
    OutOfRange,
    BadBefore,
    BadAfter,
    BadLength,
    StopBefore,
    StopAfter,
    PanicBefore,
    ErrorBefore,
}
#[derive(Default)]
struct Trace {
    before: AtomicUsize,
    after: AtomicUsize,
    seen: Mutex<Vec<(usize, usize, usize)>>,
}
struct Strategy {
    mode: Mode,
    trace: Arc<Trace>,
}
impl AgentStrategy for Strategy {
    fn decide(
        &self,
        input: StrategyInput<'_>,
        messages: &[Value],
        round: usize,
        max_rounds: usize,
    ) -> Result<StrategyAction, StrategyError> {
        self.trace
            .seen
            .lock()
            .unwrap()
            .push((round, max_rounds, messages.len()));
        match input {
            StrategyInput::BeforeCompletion { tools } => {
                self.trace.before.fetch_add(1, Ordering::SeqCst);
                match self.mode {
                    Mode::PanicBefore => panic!("{SECRET}"),
                    Mode::ErrorBefore => Err(StrategyError::Failed),
                    Mode::StopBefore => Ok(StrategyAction::Stop),
                    Mode::Duplicate => Ok(StrategyAction::UseTools(vec![0, 0])),
                    Mode::Reverse => Ok(StrategyAction::UseTools(vec![1, 0])),
                    Mode::OutOfRange => Ok(StrategyAction::UseTools(vec![tools.len()])),
                    Mode::BadBefore => Ok(StrategyAction::Finish),
                    Mode::ReadOnly => {
                        let wants_list = messages
                            .iter()
                            .any(|m| m["role"] == "user" && m["content"] == "list-request");
                        Ok(StrategyAction::UseTools(
                            tools
                                .iter()
                                .enumerate()
                                .filter_map(|(i, t)| {
                                    (if wants_list {
                                        matches!(t, ToolClass::List)
                                    } else {
                                        matches!(t, ToolClass::Read)
                                    })
                                    .then_some(i)
                                })
                                .collect(),
                        ))
                    }
                    _ => Ok(StrategyAction::UseTools((0..tools.len()).collect())),
                }
            }
            StrategyInput::AfterCompletion { outcome } => {
                self.trace.after.fetch_add(1, Ordering::SeqCst);
                if matches!(self.mode, Mode::StopAfter) {
                    return Ok(StrategyAction::Stop);
                }
                match outcome {
                    CompletionKind::NoToolCalls => {
                        if matches!(self.mode, Mode::BadAfter) {
                            Ok(StrategyAction::UseTools(vec![]))
                        } else {
                            Ok(StrategyAction::Finish)
                        }
                    }
                    CompletionKind::ToolGroup(tools) => {
                        if matches!(self.mode, Mode::BadAfter) {
                            Ok(StrategyAction::Finish)
                        } else if matches!(self.mode, Mode::BadLength) {
                            Ok(StrategyAction::ToolVotes(vec![
                                ToolVote::Deny;
                                tools.len() + 1
                            ]))
                        } else {
                            Ok(StrategyAction::ToolVotes(
                                (0..tools.len())
                                    .map(|_| {
                                        if matches!(self.mode, Mode::Deny) {
                                            ToolVote::Deny
                                        } else {
                                            ToolVote::ApplyHostPolicy
                                        }
                                    })
                                    .collect(),
                            ))
                        }
                    }
                }
            }
        }
    }
}
#[derive(Clone, Default)]
struct Mock {
    responses: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    hits: Arc<AtomicUsize>,
    hold: Arc<std::sync::atomic::AtomicBool>,
    arrived: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
async fn completion(State(m): State<Mock>, Json(body): Json<Value>) -> String {
    m.hits.fetch_add(1, Ordering::SeqCst);
    m.requests.lock().unwrap().push(body);
    if m.hold.load(Ordering::SeqCst) {
        m.arrived.notify_one();
        m.release.notified().await;
    }
    let delta = m
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected completion");
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}
fn call(id: &str, name: &str, args: Value, index: usize) -> Value {
    json!({"index":index,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn preserve_a10(root: &std::path::Path, phase: &str) {
    let cwd = std::env::current_dir().unwrap();
    let workspace = cwd.parent().unwrap().parent().unwrap();
    let relative = std::path::PathBuf::from(".local")
        .join("next17-a10-evidence")
        .join(uuid::Uuid::new_v4().to_string());
    let destination = workspace.join(&relative);
    std::fs::create_dir_all(&destination).unwrap();
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "armed"
            || name == "crash-signal"
            || name.starts_with("crash-child.")
            || name.starts_with("recovery-")
        {
            std::fs::copy(entry.path(), destination.join(name.as_ref())).unwrap();
        }
    }
    let db = rusqlite::Connection::open(root.join("agent.db")).unwrap();
    let snapshot = destination.join("agent-snapshot.db");
    db.execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])
        .unwrap();
    eprintln!(
        "a10-evidence {}",
        json!({"phase":phase,"directory":relative.to_string_lossy(),"snapshot":"agent-snapshot.db"})
    );
}
fn group(calls: Vec<Value>) -> Value {
    json!({"tool_calls":calls})
}
struct Fixture {
    dir: tempfile::TempDir,
    engine: Arc<Engine>,
    project: ProjectId,
    mock: Mock,
    trace: Arc<Trace>,
    server: tokio::task::JoinHandle<()>,
}
struct FixtureCodec {
    messages: Arc<Mutex<VecDeque<Value>>>,
}
struct FixtureDecoder {
    messages: Arc<Mutex<VecDeque<Value>>>,
    seen: bool,
}
impl CompletionCodec for FixtureCodec {
    fn encode(&self, input: &CompletionInput<'_>) -> Result<Value, CodecError> {
        let mut body = json!({"model":input.model,"messages":input.messages,"max_tokens":input.max_tokens,"stream":true,"stream_options":{"include_usage":true}});
        if !input.tools.is_empty() {
            body["tools"] = json!(input.tools);
            body["tool_choice"] = json!("auto");
        }
        Ok(body)
    }
    fn decoder(&self) -> Result<Box<dyn CompletionDecoder>, CodecError> {
        Ok(Box::new(FixtureDecoder {
            messages: self.messages.clone(),
            seen: false,
        }))
    }
}
impl CompletionDecoder for FixtureDecoder {
    fn feed(&mut self, _: &[u8], _: &mut Emit<'_>) -> Result<(), CodecError> {
        self.seen = true;
        Ok(())
    }
    fn is_done(&self) -> bool {
        self.seen
    }
    fn finish(self: Box<Self>, _: &mut Emit<'_>) -> Result<CompletionOutput, CodecError> {
        let message = self.messages.lock().unwrap().pop_front().unwrap();
        Ok((
            message,
            json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}),
        ))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture(mode: Mode, responses: Vec<Value>) -> Fixture {
    fixture_with_provider(mode, responses, None).await
}
async fn fixture_with_provider(
    mode: Mode,
    responses: Vec<Value>,
    provider_messages: Option<Vec<Value>>,
) -> Fixture {
    fixture_with_runtimes(mode, responses, provider_messages, None).await
}
async fn fixture_with_runtimes(
    mode: Mode,
    responses: Vec<Value>,
    provider_messages: Option<Vec<Value>>,
    agent_override: Option<AgentRuntime>,
) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.txt"), "fixture-content").unwrap();
    let mock = Mock {
        responses: Arc::new(Mutex::new(responses.into())),
        ..Mock::default()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/chat/completions", post(completion))
        .with_state(mock.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = Arc::new(Store::open(&dir.path().join("agent.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "route".into(),
                base_url: base,
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store
        .put_secret("route::route", "agent-fixture-secret")
        .unwrap();
    let project = store
        .register_project(RegisterProject {
            name: "fixture".into(),
            root_path: dir.path().to_string_lossy().into(),
            idempotency_key: "project".into(),
        })
        .unwrap();
    let current = store.current_project().unwrap();
    store
        .select_project(SelectProject {
            project_id: project.id.clone(),
            expected_revision: current.revision,
        })
        .unwrap();
    let config = store.project_config(&project.id).unwrap();
    store
        .configure_project(ConfigureProject {
            project_id: project.id.clone(),
            expected_revision: config.revision,
            config: ProjectConfig {
                allowed_route_ids: vec!["route".into()],
                default_route_id: Some("route".into()),
                tools: true,
                write_scopes: vec!["*".into()],
                allow_commands: true,
            },
        })
        .unwrap();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(project.id.clone()),
    };
    let p = store.network_preview(subject.clone()).unwrap();
    store
        .network_confirm(
            subject,
            ConfirmRequest {
                config_revision: p.config_revision,
                credential_revision: p.credential_revision,
                binding_digest: p.binding_digest,
                purposes: vec![NetworkPurpose::Completion],
            },
            "grant",
        )
        .unwrap();
    let trace = Arc::new(Trace::default());
    let runtime = agent_override.unwrap_or_else(|| {
        AgentRuntime::with_strategy_for_test(Arc::new(Strategy {
            mode,
            trace: trace.clone(),
        }))
        .unwrap()
    });
    let engine = if let Some(messages) = provider_messages {
        let provider = ProviderRuntime::with_codec_for_test(Arc::new(FixtureCodec {
            messages: Arc::new(Mutex::new(messages.into())),
        }))
        .unwrap();
        Engine::new_with_runtimes_for_test(store, 2, runtime, provider).unwrap()
    } else {
        Engine::new_with_agent_runtime_for_test(store, 2, runtime).unwrap()
    };
    engine
        .configure_budget(ConfigureBudget {
            project_id: project.id.clone(),
            expected_revision: 0,
            idempotency_key: "budget".into(),
            config: BudgetConfig {
                limit_tokens: 1000,
                per_call_reservation_tokens: 100,
            },
        })
        .unwrap();
    Fixture {
        dir,
        engine,
        project: project.id,
        mock,
        trace,
        server,
    }
}
fn request(rounds: usize) -> RunRequest {
    RunRequest {
        title: "agent-runtime".into(),
        kind: SessionKind::Chat,
        tasks: vec![TaskSpec {
            name: "worker".into(),
            role: "worker".into(),
            route_id: "route".into(),
            prompt: "read then respond".into(),
            depends_on: vec![],
            write_scopes: vec!["*".into()],
            tools: true,
            allow_commands: true,
            max_rounds: rounds,
        }],
    }
}
async fn settled(engine: &Engine, id: &str) -> Task {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let task = engine.store.task(id).unwrap();
            if !engine.is_busy() && !matches!(task.status.as_str(), "queued" | "running") {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a02_subset_changes_real_provider_request_without_modifying_definitions() {
    let baseline = fixture(Mode::All, vec![json!({"content":"done"})]).await;
    let run = baseline.engine.start(request(2)).await.unwrap();
    assert_eq!(
        settled(&baseline.engine, &run.tasks[0].id).await.status,
        "completed"
    );
    let selected = fixture(Mode::ReadOnly, vec![json!({"content":"done"})]).await;
    let run = selected.engine.start(request(2)).await.unwrap();
    assert_eq!(
        settled(&selected.engine, &run.tasks[0].id).await.status,
        "completed"
    );
    let all = baseline.mock.requests.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .clone();
    let actual = selected.mock.requests.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .clone();
    let expected: Vec<Value> = all
        .iter()
        .filter(|v| v["function"]["name"] == "read_file")
        .cloned()
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(actual, expected);
    assert_eq!(
        (
            selected.trace.before.load(Ordering::SeqCst),
            selected.trace.after.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
    let listing = fixture(Mode::ReadOnly, vec![json!({"content":"done"})]).await;
    let mut list_request = request(2);
    list_request.tasks[0].prompt = "list-request".into();
    let run = listing.engine.start(list_request).await.unwrap();
    assert_eq!(
        settled(&listing.engine, &run.tasks[0].id).await.status,
        "completed"
    );
    let listed = listing.mock.requests.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .clone();
    let expected_list: Vec<Value> = all
        .into_iter()
        .filter(|v| v["function"]["name"] == "list_files")
        .collect();
    assert!(!expected_list.is_empty());
    assert_eq!(listed, expected_list);
    assert_ne!(actual, listed);
}
#[tokio::test]
async fn a03_deny_pairs_host_result_without_approval_or_write_then_continues() {
    let f = fixture(
        Mode::Deny,
        vec![
            group(vec![call(
                "w1",
                "write_file",
                json!({"path":"result.txt","content":"bad"}),
                0,
            )]),
            json!({"content":"done"}),
        ],
    )
    .await;
    let run = f.engine.start(request(3)).await.unwrap();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "completed");
    assert!(!f.dir.path().join("result.txt").exists());
    assert!(
        f.engine
            .store
            .approvals_for_task(&task.id)
            .unwrap()
            .is_empty()
    );
    let results: Vec<_> = task
        .messages
        .iter()
        .filter(|m| m["role"] == "tool" && m["tool_call_id"] == "w1")
        .collect();
    assert_eq!(results.len(), 1);
    let body: Value = serde_json::from_str(results[0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(body, json!({"error":"blocked_by_agent_policy"}));
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        (
            f.trace.before.load(Ordering::SeqCst),
            f.trace.after.load(Ordering::SeqCst)
        ),
        (2, 2)
    );
}
#[tokio::test]
async fn a04_invalid_before_or_stop_fails_without_provider_or_approval() {
    for mode in [
        Mode::Duplicate,
        Mode::Reverse,
        Mode::OutOfRange,
        Mode::BadBefore,
        Mode::StopBefore,
        Mode::PanicBefore,
        Mode::ErrorBefore,
    ] {
        let f = fixture(mode, vec![]).await;
        let run = f.engine.start(request(2)).await.unwrap();
        let task = settled(&f.engine, &run.tasks[0].id).await;
        assert_eq!(task.status, "failed", "{mode:?}");
        assert_eq!(f.mock.hits.load(Ordering::SeqCst), 0, "{mode:?}");
        assert!(
            f.engine
                .store
                .approvals_for_task(&task.id)
                .unwrap()
                .is_empty()
        );
        assert!(!task.error.unwrap_or_default().contains(SECRET));
    }
}
#[tokio::test]
async fn a04_invalid_after_does_not_create_approval_or_false_completion() {
    for mode in [Mode::BadAfter, Mode::BadLength, Mode::StopAfter] {
        let f = fixture(
            mode,
            vec![group(vec![call(
                "w1",
                "write_file",
                json!({"path":"result.txt","content":"bad"}),
                0,
            )])],
        )
        .await;
        let run = f.engine.start(request(2)).await.unwrap();
        let task = settled(&f.engine, &run.tasks[0].id).await;
        assert_eq!(task.status, "failed", "{mode:?}");
        assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
        assert!(!f.dir.path().join("result.txt").exists());
        assert!(
            f.engine
                .store
                .approvals_for_task(&task.id)
                .unwrap()
                .is_empty()
        );
        assert!(
            task.messages
                .iter()
                .any(|m| m["role"] == "assistant" && m["tool_calls"][0]["id"] == "w1")
        );
    }
}

#[tokio::test]
async fn a06_disable_drains_existing_task_refuses_new_and_enable_until_release() {
    let f = fixture(Mode::ReadOnly, vec![json!({"content":"old-finished"})]).await;
    f.mock.hold.store(true, Ordering::SeqCst);
    let mut read_request = request(2);
    read_request.tasks[0].allow_commands = false;
    read_request.tasks[0].write_scopes.clear();
    let run = f.engine.start(read_request.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), f.mock.arrived.notified())
        .await
        .unwrap();
    let initial = f.engine.builtin_agent_status().unwrap();
    assert_eq!(initial.in_flight, 1);
    f.engine.disable_builtin_agent().unwrap();
    let draining = f.engine.builtin_agent_status().unwrap();
    assert!(draining.active && draining.draining && !draining.admission_open);
    assert_eq!(draining.in_flight, 1);
    assert!(f.engine.enable_builtin_agent().is_err());
    let second = f.engine.start(read_request).await.unwrap();
    let second_id = second.tasks[0].id.clone();
    let second_task = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let t = f.engine.store.task(&second_id).unwrap();
            if t.status == "failed" {
                break t;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(second_task.status, "failed");
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
    f.mock.release.notify_one();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "completed");
    assert!(task.messages.iter().any(|m| m["content"] == "old-finished"));
    let drained = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let s = f.engine.builtin_agent_status().unwrap();
            if !s.draining && !s.transitioning {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(drained.in_flight, 0);
    assert!(!drained.active && !drained.admission_open);
    assert_eq!(drained.effects, 0);
    f.engine.enable_builtin_agent().unwrap();
    let enabled = f.engine.builtin_agent_status().unwrap();
    assert!(enabled.active && enabled.admission_open && enabled.generation != initial.generation);
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a08_cancel_during_provider_call_keeps_unknown_and_releases_task_lease() {
    let f = fixture(Mode::All, vec![json!({"content":"late"})]).await;
    f.mock.hold.store(true, Ordering::SeqCst);
    let run = f.engine.start(request(2)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), f.mock.arrived.notified())
        .await
        .unwrap();
    f.engine.cancel(&run.tasks[0].id).unwrap();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "cancelled");
    let calls = f.engine.network_calls_for_task(&task.id).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].state, CallState::Unknown);
    let budget = f.engine.budget_status(&f.project).unwrap();
    assert_eq!((budget.held_tokens, budget.unresolved_count), (100, 1));
    assert_eq!(f.engine.builtin_agent_status().unwrap().in_flight, 0);
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
    f.mock.release.notify_one();
}

#[tokio::test]
async fn a01_empty_tool_group_continues_and_consumes_round() {
    let f = fixture_with_provider(
        Mode::All,
        vec![json!({"content":"wire"}), json!({"content":"wire"})],
        Some(vec![
            json!({"role":"assistant","content":"","tool_calls":[]}),
            json!({"role":"assistant","content":"done"}),
        ]),
    )
    .await;
    let run = f.engine.start(request(2)).await.unwrap();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "completed");
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 2);
    assert!(task.messages.iter().any(|m| {
        m["role"] == "assistant"
            && m.get("tool_calls")
                .is_some_and(|c| c.as_array().is_some_and(Vec::is_empty))
    }));
    let seen = f.trace.seen.lock().unwrap().clone();
    assert_eq!(
        seen.iter().map(|x| x.0).collect::<Vec<_>>(),
        vec![0, 0, 1, 1]
    );
    assert_eq!(task.usage["total_tokens"], 10);
}
#[tokio::test]
async fn a04_finish_on_empty_group_is_rejected_after_message_save() {
    let f = fixture_with_provider(
        Mode::BadAfter,
        vec![json!({"content":"wire"})],
        Some(vec![
            json!({"role":"assistant","content":"","tool_calls":[]}),
        ]),
    )
    .await;
    let run = f.engine.start(request(2)).await.unwrap();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "failed");
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
    assert!(task.messages.iter().any(|m| {
        m["role"] == "assistant"
            && m.get("tool_calls")
                .is_some_and(|c| c.as_array().is_some_and(Vec::is_empty))
    }));
}
#[tokio::test]
async fn a03_malformed_later_call_keeps_valid_prefix_and_fails_in_order() {
    let calls = vec![
        json!({"id":"r1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"sample.txt\"}"}}),
        json!({"id":"w2","type":"function","function":{"name":"write_file","arguments":"{"}}),
    ];
    let f = fixture_with_provider(
        Mode::All,
        vec![json!({"content":"wire"})],
        Some(vec![
            json!({"role":"assistant","content":"","tool_calls":calls}),
        ]),
    )
    .await;
    let run = f.engine.start(request(2)).await.unwrap();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "failed");
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
    assert!(task.messages.iter().any(|m| {
        m["role"] == "tool"
            && m["tool_call_id"] == "r1"
            && m["content"]
                .as_str()
                .is_some_and(|s| s.contains("fixture-content"))
    }));
    assert!(
        !task
            .messages
            .iter()
            .any(|m| m["role"] == "tool" && m["tool_call_id"] == "w2")
    );
    assert!(
        f.engine
            .store
            .approvals_for_task(&task.id)
            .unwrap()
            .is_empty()
    );
}

fn agent_key() -> crate::plugin_catalog::InterfaceKey {
    use crate::plugin_catalog::*;
    InterfaceKey {
        kind: InterfaceKind::Service,
        name: "agent.strategy".into(),
        version: ExactVersion {
            major: 1,
            minor: 0,
            patch: 0,
        },
    }
}
fn agent_manifest(
    id: &str,
    provides: Vec<crate::plugin_catalog::InterfaceKey>,
    requires: Vec<crate::plugin_catalog::InterfaceKey>,
) -> crate::plugin_catalog::CatalogManifestV1 {
    use crate::plugin_catalog::*;
    CatalogManifestV1 {
        manifest_version: 1,
        id: id.into(),
        display_name: id.into(),
        version: ExactVersion {
            major: 1,
            minor: 0,
            patch: 0,
        },
        scope: ScopeKind::Host,
        runtime: RuntimeKind::Builtin,
        lifecycle: LifecycleKind::HostManaged,
        surfaces: vec![Surface::Headless],
        permissions: vec![],
        config_schema: json!({}),
        provides,
        requires,
    }
}
struct Consumer;
impl crate::plugin_host::BuiltinPlugin for Consumer {
    fn activate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        Ok(())
    }
    fn deactivate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        Ok(())
    }
}
struct BlockingLifecycle {
    entered: std::sync::mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
}
impl crate::plugin_host::BuiltinPlugin for BlockingLifecycle {
    fn activate(
        &mut self,
        ctx: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        let strategy: Arc<dyn AgentStrategy> = Arc::new(Strategy {
            mode: Mode::All,
            trace: Arc::new(Trace::default()),
        });
        ctx.register_effect(&agent_key(), Box::new(strategy))
            .unwrap();
        Ok(())
    }
    fn deactivate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        self.entered.send(()).unwrap();
        let (lock, ready) = &*self.release;
        let (released, _) = ready
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(15), |v| !*v)
            .unwrap();
        if !*released {
            return Err(crate::plugin_host::PluginError::new("fixture timeout"));
        }
        Ok(())
    }
}
fn blocking_runtime(
    entered: std::sync::mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
) -> Arc<AgentRuntime> {
    use crate::plugin_catalog::ScopeKey;
    let mut host = crate::plugin_host::PluginHost::new(ScopeKey::Host).unwrap();
    host.catalog_mut()
        .register(agent_manifest(
            "builtin.engine-chat-agent",
            vec![],
            vec![agent_key()],
        ))
        .unwrap();
    host.with_factory("builtin.engine-chat-agent", || Box::new(Consumer));
    host.catalog_mut()
        .register(agent_manifest(
            "builtin.chat-agent",
            vec![agent_key()],
            vec![],
        ))
        .unwrap();
    host.with_factory("builtin.chat-agent", move || {
        Box::new(BlockingLifecycle {
            entered: entered.clone(),
            release: release.clone(),
        })
    });
    Arc::new(AgentRuntime::from_host_for_test(host).unwrap())
}
#[test]
fn a07_enable_cannot_race_reserved_stop_on_disable_or_last_drop() {
    for with_lease in [false, true] {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let runtime = blocking_runtime(entered_tx, release.clone());
        let old_generation = runtime.status().unwrap().generation;
        let lease = with_lease.then(|| runtime.acquire().unwrap());
        if with_lease {
            runtime.enable().unwrap();
            assert_eq!(runtime.status().unwrap().generation, old_generation);
            runtime.disable().unwrap();
            assert!(runtime.status().unwrap().draining);
        }
        let worker = {
            let r = runtime.clone();
            std::thread::spawn(move || {
                if let Some(lease) = lease {
                    drop(lease)
                } else {
                    r.disable().unwrap()
                }
            })
        };
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("deactivate did not start");
        let during = runtime.status().unwrap();
        assert!(during.transitioning && !during.admission_open);
        assert!(matches!(
            runtime.enable(),
            Err(crate::agent_runtime::AgentError::Busy)
        ));
        assert!(runtime.acquire().is_err());
        *release.0.lock().unwrap() = true;
        release.1.notify_one();
        worker.join().unwrap();
        let stopped = runtime.status().unwrap();
        assert!(!stopped.active && !stopped.transitioning && !stopped.admission_open);
        assert_eq!(stopped.in_flight, 0);
        assert_eq!(stopped.effects, 0);
        runtime.enable().unwrap();
        let enabled = runtime.status().unwrap();
        assert!(enabled.active && enabled.admission_open && enabled.generation != old_generation);
    }
}

#[tokio::test]
async fn a09_orphan_approval_blocks_new_assistant_and_terminal_save_cannot_write_it_back() {
    let args = json!({"path":"orphan.txt","content":"no-write"});
    let f = fixture(
        Mode::All,
        vec![group(vec![call(
            "orphan-a09",
            "write_file",
            args.clone(),
            0,
        )])],
    )
    .await;
    f.mock.hold.store(true, Ordering::SeqCst);
    let run = f.engine.start(request(2)).await.unwrap();
    let id = run.tasks[0].id.clone();
    tokio::time::timeout(Duration::from_secs(10), f.mock.arrived.notified())
        .await
        .unwrap();
    let store = f.engine.store.clone();
    let before = Arc::new(Mutex::new(None));
    let captured = before.clone();
    let approval_before = Arc::new(Mutex::new(None));
    let approval_captured = approval_before.clone();
    f.engine.set_agent_before_save_for_test(
        id.clone(),
        Arc::new(move |task, message| {
            *captured.lock().unwrap() = Some(task.messages.clone());
            let mut historic = task.clone();
            historic.messages.push(message.clone());
            store.save_task(&historic).unwrap();
            store
                .ensure_approval(&historic, "orphan-a09", "write_file", &args)
                .unwrap();
            *approval_captured.lock().unwrap() = Some(orphan_approval_snapshot(&store, &task.id));
            store.save_task(task).unwrap();
        }),
    );
    f.mock.release.notify_one();
    let task = settled(&f.engine, &id).await;
    assert_eq!(task.status, "failed");
    assert_eq!(
        task.messages,
        before.lock().unwrap().clone().expect("hook not invoked")
    );
    assert!(
        task.error
            .as_deref()
            .is_some_and(|e| e.contains("new tool call conflicts with existing approval"))
    );
    let count: u64 = f
        .engine
        .store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM approvals WHERE task_id=?1 AND tool_call_id=?2",
            rusqlite::params![&id, "orphan-a09"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        orphan_approval_snapshot(&f.engine.store, &id),
        approval_before.lock().unwrap().clone().unwrap()
    );
    let error = f
        .engine
        .store
        .approval_by_tool_call(&id, "orphan-a09")
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::approval::ApprovalError>(),
        Some(crate::approval::ApprovalError::CorruptState { .. })
    ));
    assert!(!f.dir.path().join("orphan.txt").exists());
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
}
fn orphan_approval_snapshot(store: &Store, task_id: &str) -> String {
    store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT json_array(id,tool_call_id,task_id,turn_id,session_id,tool_name,args_digest,binding_digest,workspace,write_scopes,allow_commands,preview,status,execution_state,created_at,decided_at,decided_by) FROM approvals WHERE task_id=?1 AND tool_call_id='orphan-a09'",
            [task_id],
            |row| row.get(0),
        )
        .unwrap()
}
#[tokio::test]
async fn a09_transaction_failure_rolls_back_group_and_terminal_save_uses_old_snapshot() {
    let f = fixture(
        Mode::All,
        vec![group(vec![call(
            "rollback-a09",
            "write_file",
            json!({"path":"rollback.txt","content":"no-write"}),
            0,
        )])],
    )
    .await;
    f.mock.hold.store(true, Ordering::SeqCst);
    let run = f.engine.start(request(2)).await.unwrap();
    let id = run.tasks[0].id.clone();
    tokio::time::timeout(Duration::from_secs(10), f.mock.arrived.notified())
        .await
        .unwrap();
    let store = f.engine.store.clone();
    let before = Arc::new(Mutex::new(None));
    let captured = before.clone();
    f.engine.set_agent_before_save_for_test(id.clone(),Arc::new(move |task,_message| {
        *captured.lock().unwrap()=Some(task.messages.clone());
        store.db.lock().unwrap().execute_batch("CREATE TEMP TRIGGER a09_fail BEFORE UPDATE ON tasks WHEN instr(NEW.value, 'rollback-a09') > 0 BEGIN SELECT RAISE(ABORT, 'a09 injected failure'); END;").unwrap();
    }));
    f.mock.release.notify_one();
    let task = settled(&f.engine, &id).await;
    assert_eq!(task.status, "failed");
    assert_eq!(
        task.messages,
        before.lock().unwrap().clone().expect("hook not invoked")
    );
    assert!(f.engine.store.approvals_for_task(&id).unwrap().is_empty());
    assert!(!f.dir.path().join("rollback.txt").exists());
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
    f.engine
        .store
        .db
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER a09_fail")
        .unwrap();
}

#[tokio::test]
async fn a04_present_non_array_tool_calls_fails_after_settlement_without_finish() {
    for bad in [Value::Null, json!("bad"), json!({"id":"bad"})] {
        let f = fixture_with_provider(
            Mode::All,
            vec![json!({"content":"wire"})],
            Some(vec![
                json!({"role":"assistant","content":"not-a-finish","tool_calls":bad}),
            ]),
        )
        .await;
        let run = f.engine.start(request(2)).await.unwrap();
        let task = settled(&f.engine, &run.tasks[0].id).await;
        assert_eq!(task.status, "failed");
        assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
        let calls = f.engine.network_calls_for_task(&task.id).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].state, CallState::Finished);
        assert_eq!(f.engine.budget_status(&f.project).unwrap().spent_tokens, 5);
        assert!(
            f.engine
                .store
                .approvals_for_task(&task.id)
                .unwrap()
                .is_empty()
        );
        assert!(!task.messages.iter().any(|m| m["content"] == "not-a-finish"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a10_real_kill_recover_resume_never_replays_prior_tool_group() {
    use std::process::{Command, Stdio};
    if let Ok(root) = std::env::var("NHC_A17_CHILD_ROOT") {
        let root = std::path::PathBuf::from(root);
        let phase = match std::env::var("NHC_A17_PHASE").unwrap().as_str() {
            "agent_after_assistant_save" => "agent_after_assistant_save",
            "agent_before_denied_result" => "agent_before_denied_result",
            "agent_after_denied_result" => "agent_after_denied_result",
            "agent_after_applied_result" => "agent_after_applied_result",
            "write_pre_fs" => "write_pre_fs",
            _ => panic!("invalid crash phase"),
        };
        let store = Arc::new(Store::open(&root.join("agent.db")).unwrap());
        let mode = if matches!(phase, "agent_after_applied_result" | "write_pre_fs") {
            Mode::All
        } else {
            Mode::Deny
        };
        let runtime = AgentRuntime::with_strategy_for_test(Arc::new(Strategy {
            mode,
            trace: Arc::new(Trace::default()),
        }))
        .unwrap();
        let engine = Engine::new_with_agent_runtime_for_test(store, 2, runtime).unwrap();
        let run = engine.start(request(3)).await.unwrap();
        let task_id = run.tasks[0].id.clone();
        crate::engine::crash_probe::arm(task_id.clone(), phase, root.join("crash-signal"));
        std::fs::write(root.join("armed"), &task_id).unwrap();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    if let Ok(root) = std::env::var("NHC_A17_RECOVER_ROOT") {
        let root = std::path::PathBuf::from(root);
        let id = std::env::var("NHC_A17_RECOVER_TASK").unwrap();
        let run = std::env::var("NHC_A17_RECOVER_RUN").unwrap();
        let store = Arc::new(Store::open(&root.join("agent.db")).unwrap());
        store.recover().unwrap();
        let runtime = AgentRuntime::with_strategy_for_test(Arc::new(Strategy {
            mode: Mode::Deny,
            trace: Arc::new(Trace::default()),
        }))
        .unwrap();
        let engine = Engine::new_with_agent_runtime_for_test(store, 2, runtime).unwrap();
        let task = if run == "1" {
            engine.resume(&id, "resume after crash").await.unwrap();
            settled(&engine, &id).await
        } else if run.starts_with("unknown") {
            let error = engine
                .resume(&id, "do not replay unknown")
                .await
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<crate::approval::ApprovalError>(),
                Some(crate::approval::ApprovalError::UnknownResult { .. })
            ));
            engine.store.task(&id).unwrap()
        } else {
            engine.store.task(&id).unwrap()
        };
        let project = engine.store.current_project().unwrap().project_id.unwrap();
        let budget = engine.budget_status(&project).unwrap();
        let messages: Vec<Value> = task
            .messages
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| json!({"tool_call_id":m["tool_call_id"],"content":m["content"]}))
            .collect();
        let approvals = engine.store.approvals_for_task(&id).unwrap();
        let snapshot = json!({"pid":std::process::id(),"status":task.status,"messages":messages,"network_calls":engine.network_calls_for_task(&id).unwrap().len(),"held":budget.held_tokens,"unresolved":budget.unresolved_count,"approvals":approvals.len(),"approval_states":approvals.iter().map(|a| a.execution_state).collect::<Vec<_>>()});
        std::fs::write(
            root.join(format!("recovery-{run}.json")),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        return;
    }
    for phase in [
        "agent_after_assistant_save",
        "agent_before_denied_result",
        "agent_after_denied_result",
        "agent_after_applied_result",
    ] {
        let calls = if phase == "agent_after_applied_result" {
            vec![
                call("r1", "read_file", json!({"path":"sample.txt"}), 0),
                call(
                    "w2",
                    "write_file",
                    json!({"path":"forbidden.txt","content":"bad"}),
                    1,
                ),
            ]
        } else {
            vec![call(
                "w1",
                "write_file",
                json!({"path":"forbidden.txt","content":"bad"}),
                0,
            )]
        };
        let mut f = fixture(Mode::All, vec![group(calls), json!({"content":"done"})]).await;
        f.dir.disable_cleanup(true);
        let root = f.dir.path();
        eprintln!(
            "a10-fixture {}",
            json!({"phase":phase,"directory":root.to_string_lossy()})
        );
        f.mock.hold.store(true, Ordering::SeqCst);
        let mut child = OwnedChild(Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent_runtime_tests::a10_real_kill_recover_resume_never_replays_prior_tool_group",
                "--nocapture",
            ])
            .env("NHC_A17_CHILD_ROOT", root)
            .env("NHC_A17_PHASE", phase)
            .stdout(Stdio::from(std::fs::File::create(root.join("crash-child.stdout.log")).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(root.join("crash-child.stderr.log")).unwrap()))
            .spawn()
            .unwrap());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let task_id = loop {
            if let Ok(id) = std::fs::read_to_string(root.join("armed")) {
                break id;
            }
            if let Some(s) = child.try_wait().unwrap() {
                panic!("crash child exited early {phase}: {s}");
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("arming timeout {phase}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        f.mock.hold.store(false, Ordering::SeqCst);
        f.mock.release.notify_one();
        loop {
            if root.join("crash-signal").exists() {
                break;
            }
            if let Some(s) = child.try_wait().unwrap() {
                panic!("crash child exited before signal {phase}: {s}");
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("signal timeout {phase}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = child.id();
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        eprintln!(
            "a10-kill {}",
            json!({"phase":phase,"pid":pid,"exit_code":status.code(),"task_id":task_id})
        );
        preserve_a10(root, &format!("{phase}-killed"));
        let mut previous = None;
        for round in 1..=2 {
            let mut recovery=OwnedChild(Command::new(std::env::current_exe().unwrap()).args(["--exact","agent_runtime_tests::a10_real_kill_recover_resume_never_replays_prior_tool_group","--nocapture"]).env("NHC_A17_RECOVER_ROOT",root).env("NHC_A17_RECOVER_TASK",&task_id).env("NHC_A17_RECOVER_RUN",round.to_string()).stdout(Stdio::from(std::fs::File::create(root.join(format!("recovery-{round}.stdout.log"))).unwrap())).stderr(Stdio::from(std::fs::File::create(root.join(format!("recovery-{round}.stderr.log"))).unwrap())).spawn().unwrap());
            let recovery_pid = recovery.id();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(s) = recovery.try_wait().unwrap() {
                    assert!(s.success(), "recovery failed {phase} {round}: {s}");
                    assert!(recovery.wait().unwrap().success());
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    let _ = recovery.kill();
                    let _ = recovery.wait();
                    panic!("recovery timeout {phase} {round}");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let snapshot: Value = serde_json::from_slice(
                &std::fs::read(root.join(format!("recovery-{round}.json"))).unwrap(),
            )
            .unwrap();
            preserve_a10(root, &format!("{phase}-recovery-{round}"));
            assert_eq!(snapshot["pid"], recovery_pid);
            assert_eq!(snapshot["status"], "completed");
            assert_eq!(snapshot["network_calls"], 2);
            assert_eq!(snapshot["held"], 0);
            assert_eq!(snapshot["unresolved"], 0);
            assert_eq!(snapshot["approvals"], 0);
            assert!(!root.join("forbidden.txt").exists());
            assert_eq!(f.mock.hits.load(Ordering::SeqCst), 2);
            let messages = snapshot["messages"].clone();
            if let Some(ref old) = previous {
                assert_eq!(old, &messages);
            } else {
                previous = Some(messages);
            }
            eprintln!(
                "a10-recovery {}",
                json!({"phase":phase,"killed_pid":pid,"run":round,"recovery_pid":recovery_pid,"exit_code":0,"mock_completions":f.mock.hits.load(Ordering::SeqCst)})
            );
        }
        let tools = previous.unwrap();
        let items = tools.as_array().unwrap();
        let wanted = if phase == "agent_after_applied_result" {
            "w2"
        } else {
            "w1"
        };
        assert_eq!(
            items.iter().filter(|m| m["tool_call_id"] == wanted).count(),
            1
        );
        if phase == "agent_after_denied_result" {
            assert!(items.iter().any(|m| {
                m["tool_call_id"] == "w1"
                    && m["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("blocked_by_agent_policy"))
            }));
        }
        if phase == "agent_after_applied_result" {
            assert_eq!(
                items.iter().filter(|m| m["tool_call_id"] == "r1").count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn a07_callback_panic_is_redacted_but_ordinary_panic_hook_remains() {
    const NORMAL: &str = "nhc-a17-ordinary-panic-control";
    if std::env::var_os("NHC_A17_PANIC_CHILD").is_some() {
        let f = fixture(Mode::PanicBefore, vec![]).await;
        let run = f.engine.start(request(2)).await.unwrap();
        let task = settled(&f.engine, &run.tasks[0].id).await;
        assert_eq!(task.status, "failed");
        assert_eq!(f.mock.hits.load(Ordering::SeqCst), 0);
        assert!(!task.error.unwrap_or_default().contains(SECRET));
        assert!(std::panic::catch_unwind(|| panic!("{NORMAL}")).is_err());
        return;
    }
    let output = bounded_child(
        "agent_runtime_tests::a07_callback_panic_is_redacted_but_ordinary_panic_hook_remains",
        "NHC_A17_PANIC_CHILD",
    );
    assert!(
        output.status.success(),
        "panic subprocess status={}",
        output.status
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    assert!(String::from_utf8_lossy(&output.stderr).contains(NORMAL));
}

struct PanicOnDrop;
impl AgentStrategy for PanicOnDrop {
    fn decide(
        &self,
        _: StrategyInput<'_>,
        _: &[Value],
        _: usize,
        _: usize,
    ) -> Result<StrategyAction, StrategyError> {
        Ok(StrategyAction::Stop)
    }
}
impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("{SECRET}")
    }
}
struct DropProvider;
impl crate::plugin_host::BuiltinPlugin for DropProvider {
    fn activate(
        &mut self,
        ctx: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        let strategy: Arc<dyn AgentStrategy> = Arc::new(PanicOnDrop);
        ctx.register_effect(&agent_key(), Box::new(strategy))
            .unwrap();
        Ok(())
    }
    fn deactivate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        Ok(())
    }
}
#[test]
fn a07_panicking_strategy_drop_cannot_skip_count_or_draining_stop() {
    if std::env::var_os("NHC_A17_DROP_CHILD").is_some() {
        use crate::plugin_catalog::ScopeKey;
        let mut host = crate::plugin_host::PluginHost::new(ScopeKey::Host).unwrap();
        host.catalog_mut()
            .register(agent_manifest(
                "builtin.engine-chat-agent",
                vec![],
                vec![agent_key()],
            ))
            .unwrap();
        host.with_factory("builtin.engine-chat-agent", || Box::new(Consumer));
        host.catalog_mut()
            .register(agent_manifest(
                "builtin.chat-agent",
                vec![agent_key()],
                vec![],
            ))
            .unwrap();
        host.with_factory("builtin.chat-agent", || Box::new(DropProvider));
        let runtime = AgentRuntime::from_host_for_test(host).unwrap();
        let lease = runtime.acquire().unwrap();
        runtime.disable().unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(lease))).is_ok());
        let status = runtime.status().unwrap();
        assert_eq!(status.in_flight, 0);
        assert!(!status.admission_open);
        assert!(!status.draining && !status.transitioning);
        assert!(!status.active);
        assert_eq!(status.effects, 0);
        return;
    }
    let output = bounded_child(
        "agent_runtime_tests::a07_panicking_strategy_drop_cannot_skip_count_or_draining_stop",
        "NHC_A17_DROP_CHILD",
    );
    assert!(
        output.status.success(),
        "drop subprocess status={}",
        output.status
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
}

#[test]
fn a07_generation_overflow_failclosed_still_drains_and_revokes() {
    use crate::agent_runtime::AgentError;
    for with_lease in [false, true] {
        let runtime = AgentRuntime::with_strategy_for_test(Arc::new(Strategy {
            mode: Mode::All,
            trace: Arc::new(Trace::default()),
        }))
        .unwrap();
        runtime.set_generation_for_test(u64::MAX).unwrap();
        let lease = with_lease.then(|| runtime.acquire().unwrap());
        assert!(matches!(runtime.disable(), Err(AgentError::Internal)));
        let state = runtime.status().unwrap();
        assert_eq!(state.generation, u64::MAX);
        assert!(!state.admission_open);
        assert!(matches!(state.last_failure, Some(AgentError::Internal)));
        if with_lease {
            assert!(state.draining && state.active);
            assert!(matches!(runtime.enable(), Err(AgentError::Busy)));
        }
        drop(lease);
        let state = runtime.status().unwrap();
        assert_eq!(state.in_flight, 0);
        assert!(!state.draining && !state.transitioning && !state.active);
        assert_eq!(state.effects, 0);
        assert_eq!(state.generation, u64::MAX);
        assert!(matches!(runtime.enable(), Err(AgentError::Lifecycle)));
    }
}

#[tokio::test]
async fn a04_deny_repetition_still_exhausts_host_round_limit() {
    let group1 = group(vec![call(
        "w1",
        "write_file",
        json!({"path":"x.txt","content":"bad"}),
        0,
    )]);
    let group2 = group(vec![call(
        "w2",
        "write_file",
        json!({"path":"x.txt","content":"bad"}),
        0,
    )]);
    let f = fixture(Mode::Deny, vec![group1, group2]).await;
    let run = f.engine.start(request(2)).await.unwrap();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "failed");
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        (
            f.trace.before.load(Ordering::SeqCst),
            f.trace.after.load(Ordering::SeqCst)
        ),
        (2, 2)
    );
    assert_eq!(
        task.messages.iter().filter(|m| m["role"] == "tool").count(),
        2
    );
    assert!(
        f.engine
            .store
            .approvals_for_task(&task.id)
            .unwrap()
            .is_empty()
    );
    assert!(!f.dir.path().join("x.txt").exists());
}
#[tokio::test]
async fn a02_provider_cannot_execute_a_tool_removed_by_strategy() {
    let f = fixture(
        Mode::ReadOnly,
        vec![group(vec![call(
            "w1",
            "write_file",
            json!({"path":"x.txt","content":"bad"}),
            0,
        )])],
    )
    .await;
    let run = f.engine.start(request(2)).await.unwrap();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "failed");
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
    assert!(!f.dir.path().join("x.txt").exists());
    assert!(
        f.engine
            .store
            .approvals_for_task(&task.id)
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn a08_cancel_while_waiting_for_approval_releases_agent_lease() {
    let f = fixture(
        Mode::All,
        vec![group(vec![call(
            "w1",
            "write_file",
            json!({"path":"x.txt","content":"bad"}),
            0,
        )])],
    )
    .await;
    let run = f.engine.start(request(2)).await.unwrap();
    let id = &run.tasks[0].id;
    let approval = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(a) = f
                .engine
                .store
                .pending_approvals_for_task(id)
                .unwrap()
                .into_iter()
                .next()
            {
                break a;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.engine.builtin_agent_status().unwrap().in_flight, 1);
    f.engine.cancel(id).unwrap();
    let task = settled(&f.engine, id).await;
    assert_eq!(task.status, "cancelled");
    assert!(!f.dir.path().join("x.txt").exists());
    let approval = f.engine.store.approval(&approval.id).unwrap();
    assert_ne!(
        approval.execution_state,
        crate::approval::ExecutionState::Finished
    );
    assert_eq!(f.engine.builtin_agent_status().unwrap().in_flight, 0);
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
}

struct InvalidProvider {
    mode: u8,
}
impl crate::plugin_host::BuiltinPlugin for InvalidProvider {
    fn activate(
        &mut self,
        ctx: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        if self.mode == 2 {
            return Err(crate::plugin_host::PluginError::new(
                "fixture activation failure",
            ));
        }
        if self.mode == 1 {
            ctx.register_effect(&agent_key(), Box::new(42u32)).unwrap();
        } else {
            let strategy: Arc<dyn AgentStrategy> = Arc::new(Strategy {
                mode: Mode::All,
                trace: Arc::new(Trace::default()),
            });
            ctx.register_effect(&agent_key(), Box::new(strategy))
                .unwrap();
        }
        Ok(())
    }
    fn deactivate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        if self.mode == 3 {
            return Err(crate::plugin_host::PluginError::new(SECRET));
        }
        if self.mode == 4 {
            panic!("{SECRET}");
        }
        Ok(())
    }
}
#[test]
fn a05_plugin_host_missing_version_factory_type_and_activation_fail_closed() {
    use crate::plugin_catalog::ScopeKey;
    for case in [
        "missing",
        "version",
        "factory",
        "type",
        "activation",
        "valid",
    ] {
        let mut host = crate::plugin_host::PluginHost::new(ScopeKey::Host).unwrap();
        host.catalog_mut()
            .register(agent_manifest(
                "builtin.engine-chat-agent",
                vec![],
                vec![agent_key()],
            ))
            .unwrap();
        host.with_factory("builtin.engine-chat-agent", || Box::new(Consumer));
        if case != "missing" {
            let mut provided = agent_key();
            if case == "version" {
                provided.version.minor = 1;
            }
            host.catalog_mut()
                .register(agent_manifest("builtin.chat-agent", vec![provided], vec![]))
                .unwrap();
            if case != "factory" {
                let mode = match case {
                    "type" => 1,
                    "activation" => 2,
                    _ => 0,
                };
                host.with_factory("builtin.chat-agent", move || {
                    Box::new(InvalidProvider { mode })
                });
            }
        }
        let result = AgentRuntime::from_host_for_test(host);
        assert_eq!(result.is_ok(), case == "valid", "{case}");
        if let Ok(runtime) = result {
            let status = runtime.status().unwrap();
            assert!(status.active && status.admission_open);
        }
    }
}

#[test]
fn a07_deactivation_error_and_panic_close_admission_and_scrub_output() {
    if let Ok(mode) = std::env::var("NHC_A17_LIFECYCLE_MODE") {
        use crate::plugin_catalog::ScopeKey;
        let mut host = crate::plugin_host::PluginHost::new(ScopeKey::Host).unwrap();
        host.catalog_mut()
            .register(agent_manifest(
                "builtin.engine-chat-agent",
                vec![],
                vec![agent_key()],
            ))
            .unwrap();
        host.with_factory("builtin.engine-chat-agent", || Box::new(Consumer));
        host.catalog_mut()
            .register(agent_manifest(
                "builtin.chat-agent",
                vec![agent_key()],
                vec![],
            ))
            .unwrap();
        let value = if mode == "error" { 3 } else { 4 };
        host.with_factory("builtin.chat-agent", move || {
            Box::new(InvalidProvider { mode: value })
        });
        let runtime = AgentRuntime::from_host_for_test(host).unwrap();
        assert!(matches!(
            runtime.disable(),
            Err(crate::agent_runtime::AgentError::Lifecycle)
        ));
        let state = runtime.status().unwrap();
        assert!(!state.admission_open && !state.active && !state.transitioning);
        assert_eq!(state.in_flight, 0);
        assert_eq!(state.effects, 0);
        assert!(matches!(
            state.last_failure,
            Some(crate::agent_runtime::AgentError::Lifecycle)
        ));
        assert!(runtime.acquire().is_err());
        assert!(runtime.enable().is_err());
        return;
    }
    for mode in ["error", "panic"] {
        let output = bounded_child_with_value(
            "agent_runtime_tests::a07_deactivation_error_and_panic_close_admission_and_scrub_output",
            "NHC_A17_LIFECYCLE_MODE",
            mode,
        );
        assert!(
            output.status.success(),
            "mode={mode} status={}",
            output.status
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    }
}

struct FailConsumer;
impl crate::plugin_host::BuiltinPlugin for FailConsumer {
    fn activate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        Err(crate::plugin_host::PluginError::new(
            "fixture activation failure",
        ))
    }
    fn deactivate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        Ok(())
    }
}
struct FinalDropStrategy(Arc<AtomicUsize>);
impl AgentStrategy for FinalDropStrategy {
    fn decide(
        &self,
        _: StrategyInput<'_>,
        _: &[Value],
        _: usize,
        _: usize,
    ) -> Result<StrategyAction, StrategyError> {
        Ok(StrategyAction::Stop)
    }
}
impl Drop for FinalDropStrategy {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("{SECRET}");
    }
}
struct FinalDropProvider {
    drops: Arc<AtomicUsize>,
    wrong_type: bool,
}
impl crate::plugin_host::BuiltinPlugin for FinalDropProvider {
    fn activate(
        &mut self,
        ctx: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        if self.wrong_type {
            ctx.register_effect(
                &agent_key(),
                Box::new(FinalDropStrategy(self.drops.clone())),
            )
            .unwrap();
        } else {
            let strategy: Arc<dyn AgentStrategy> = Arc::new(FinalDropStrategy(self.drops.clone()));
            ctx.register_effect(&agent_key(), Box::new(strategy))
                .unwrap();
        }
        Ok(())
    }
    fn deactivate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        Ok(())
    }
}
#[test]
fn a07_runtime_final_drop_and_failed_binding_cannot_escape_guard() {
    if let Ok(mode) = std::env::var("NHC_A17_FINAL_DROP_MODE") {
        use crate::plugin_catalog::ScopeKey;
        let mut host = crate::plugin_host::PluginHost::new(ScopeKey::Host).unwrap();
        host.catalog_mut()
            .register(agent_manifest(
                "builtin.engine-chat-agent",
                vec![],
                vec![agent_key()],
            ))
            .unwrap();
        if mode == "start" {
            host.with_factory("builtin.engine-chat-agent", || Box::new(FailConsumer));
        } else {
            host.with_factory("builtin.engine-chat-agent", || Box::new(Consumer));
        }
        host.catalog_mut()
            .register(agent_manifest(
                "builtin.chat-agent",
                vec![agent_key()],
                vec![],
            ))
            .unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let count = drops.clone();
        let wrong_type = mode == "binding";
        host.with_factory("builtin.chat-agent", move || {
            Box::new(FinalDropProvider {
                drops: count.clone(),
                wrong_type,
            })
        });
        let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            AgentRuntime::from_host_for_test(host)
        }));
        assert!(guarded.is_ok(), "binding cleanup panicked");
        let result = guarded.unwrap();
        if mode == "binding" || mode == "start" {
            assert!(result.is_err());
        } else {
            let runtime = result.unwrap();
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(runtime))).is_ok(),
                "runtime cleanup panicked"
            );
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        return;
    }
    for mode in ["start", "binding", "runtime"] {
        let output = bounded_child_with_value(
            "agent_runtime_tests::a07_runtime_final_drop_and_failed_binding_cannot_escape_guard",
            "NHC_A17_FINAL_DROP_MODE",
            mode,
        );
        assert!(
            output.status.success(),
            "mode={mode} status={}",
            output.status
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a10_unknown_approved_write_survives_restarts_without_automatic_retry() {
    use std::process::{Command, Stdio};
    let mut f = fixture(
        Mode::All,
        vec![group(vec![call(
            "u1",
            "write_file",
            json!({"path":"unknown.txt","content":"do-not-repeat"}),
            0,
        )])],
    )
    .await;
    f.dir.disable_cleanup(true);
    let root = f.dir.path();
    eprintln!(
        "a10-fixture {}",
        json!({"phase":"write_pre_fs","directory":root.to_string_lossy()})
    );
    f.mock.hold.store(true, Ordering::SeqCst);
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent_runtime_tests::a10_real_kill_recover_resume_never_replays_prior_tool_group",
                "--nocapture",
            ])
            .env("NHC_A17_CHILD_ROOT", root)
            .env("NHC_A17_PHASE", "write_pre_fs")
            .stdout(Stdio::from(
                std::fs::File::create(root.join("crash-child.stdout.log")).unwrap(),
            ))
            .stderr(Stdio::from(
                std::fs::File::create(root.join("crash-child.stderr.log")).unwrap(),
            ))
            .spawn()
            .unwrap(),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let task_id = loop {
        if let Ok(id) = std::fs::read_to_string(root.join("armed")) {
            break id;
        }
        if let Some(s) = child.try_wait().unwrap() {
            panic!("unknown child exited: {s}");
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("unknown arm timeout");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    f.mock.hold.store(false, Ordering::SeqCst);
    f.mock.release.notify_one();
    let approval = loop {
        if let Some(a) = f
            .engine
            .store
            .pending_approvals_for_task(&task_id)
            .unwrap()
            .into_iter()
            .next()
        {
            break a;
        }
        if let Some(s) = child.try_wait().unwrap() {
            panic!("unknown child exited before approval: {s}");
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("approval timeout");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    f.engine.decide_approval(&approval.id, true, None).unwrap();
    loop {
        if root.join("crash-signal").exists() {
            break;
        }
        if let Some(s) = child.try_wait().unwrap() {
            panic!("unknown child exited before claim signal: {s}");
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("unknown claim timeout");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let child_pid = child.id();
    child.kill().unwrap();
    let child_status = child.wait().unwrap();
    assert!(!child_status.success());
    eprintln!(
        "a10-kill {}",
        json!({"phase":"write_pre_fs","pid":child_pid,"exit_code":child_status.code(),"task_id":task_id})
    );
    preserve_a10(root, "write_pre_fs-killed");
    for round in 1..=2 {
        let label = format!("unknown-{round}");
        let mut recovery = OwnedChild(Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent_runtime_tests::a10_real_kill_recover_resume_never_replays_prior_tool_group",
                "--nocapture",
            ])
            .env("NHC_A17_RECOVER_ROOT", root)
            .env("NHC_A17_RECOVER_TASK", &task_id)
            .env("NHC_A17_RECOVER_RUN", &label)
            .stdout(Stdio::from(std::fs::File::create(root.join(format!("recovery-{label}.stdout.log"))).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(root.join(format!("recovery-{label}.stderr.log"))).unwrap()))
            .spawn()
            .unwrap());
        let pid = recovery.id();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(s) = recovery.try_wait().unwrap() {
                assert!(s.success(), "unknown recovery failed: {s}");
                assert!(recovery.wait().unwrap().success());
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = recovery.kill();
                let _ = recovery.wait();
                panic!("unknown recovery timeout");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let snapshot: Value = serde_json::from_slice(
            &std::fs::read(root.join(format!("recovery-{label}.json"))).unwrap(),
        )
        .unwrap();
        preserve_a10(root, &format!("write_pre_fs-{label}"));
        assert_eq!(snapshot["pid"], pid);
        assert_eq!(snapshot["network_calls"], 1);
        assert_eq!(snapshot["approvals"], 1);
        assert_eq!(snapshot["approval_states"], json!(["unknown"]));
        assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
        assert!(!root.join("unknown.txt").exists());
        eprintln!(
            "a10-unknown {}",
            json!({"killed_pid":child_pid,"recovery_pid":pid,"run":round,"exit_code":0,"mock_completions":f.mock.hits.load(Ordering::SeqCst)})
        );
    }
}

struct CancelPhaseStrategy {
    after: bool,
    entered: std::sync::mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
}
impl AgentStrategy for CancelPhaseStrategy {
    fn decide(
        &self,
        input: StrategyInput<'_>,
        _: &[Value],
        _: usize,
        _: usize,
    ) -> Result<StrategyAction, StrategyError> {
        let action = match input {
            StrategyInput::BeforeCompletion { tools } => {
                StrategyAction::UseTools((0..tools.len()).collect())
            }
            StrategyInput::AfterCompletion {
                outcome: CompletionKind::NoToolCalls,
            } => StrategyAction::Finish,
            StrategyInput::AfterCompletion {
                outcome: CompletionKind::ToolGroup(tools),
            } => StrategyAction::ToolVotes(
                (0..tools.len())
                    .map(|_| ToolVote::ApplyHostPolicy)
                    .collect(),
            ),
        };
        let is_after = !matches!(&action, StrategyAction::UseTools(_));
        if is_after == self.after {
            self.entered.send(()).unwrap();
            let (lock, ready) = &*self.release;
            let (released, _) = ready
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(15), |v| !*v)
                .unwrap();
            if !*released {
                return Err(StrategyError::Failed);
            }
        }
        Ok(action)
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a08_cancel_during_before_and_after_callback_stops_next_host_action() {
    for after in [false, true] {
        let (tx, rx) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let runtime = AgentRuntime::with_strategy_for_test(Arc::new(CancelPhaseStrategy {
            after,
            entered: tx,
            release: release.clone(),
        }))
        .unwrap();
        let responses = if after {
            vec![group(vec![call(
                "w1",
                "write_file",
                json!({"path":"cancel.txt","content":"no"}),
                0,
            )])]
        } else {
            vec![]
        };
        let f = fixture_with_runtimes(Mode::All, responses, None, Some(runtime)).await;
        let run = f.engine.start(request(2)).await.unwrap();
        let id = &run.tasks[0].id;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if rx.try_recv().is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        f.engine.cancel(id).unwrap();
        *release.0.lock().unwrap() = true;
        release.1.notify_one();
        let task = settled(&f.engine, id).await;
        assert_eq!(task.status, "cancelled");
        assert_eq!(f.mock.hits.load(Ordering::SeqCst), usize::from(after));
        let calls = f.engine.network_calls_for_task(id).unwrap();
        assert_eq!(calls.len(), usize::from(after));
        if after {
            assert_eq!(calls[0].state, CallState::Finished);
            assert!(
                task.messages
                    .iter()
                    .any(|m| m["role"] == "assistant" && m["tool_calls"][0]["id"] == "w1")
            );
        }
        assert!(f.engine.store.approvals_for_task(id).unwrap().is_empty());
        assert!(!f.dir.path().join("cancel.txt").exists());
        assert_eq!(f.engine.builtin_agent_status().unwrap().in_flight, 0);
    }
}
#[tokio::test]
async fn a08_old_task_lease_cannot_bypass_current_project_permission_revocation() {
    let f = fixture(
        Mode::All,
        vec![group(vec![call(
            "w1",
            "write_file",
            json!({"path":"revoked.txt","content":"no"}),
            0,
        )])],
    )
    .await;
    f.mock.hold.store(true, Ordering::SeqCst);
    let run = f.engine.start(request(2)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), f.mock.arrived.notified())
        .await
        .unwrap();
    assert_eq!(f.engine.builtin_agent_status().unwrap().in_flight, 1);
    let old = f.engine.project_config(&f.project).unwrap();
    let mut config = old.config;
    config.tools = false;
    config.allow_commands = false;
    config.write_scopes.clear();
    f.engine
        .configure_project(ConfigureProject {
            project_id: f.project.clone(),
            expected_revision: old.revision,
            config,
        })
        .await
        .unwrap();
    f.mock.release.notify_one();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "failed");
    assert_eq!(f.mock.hits.load(Ordering::SeqCst), 1);
    assert!(!f.dir.path().join("revoked.txt").exists());
    assert!(
        f.engine
            .store
            .approvals_for_task(&task.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.engine.builtin_agent_status().unwrap().in_flight, 0);
}

#[tokio::test]
async fn a01_builtin_first_and_next_turn_preserve_messages_and_usage() {
    let builtin = AgentRuntime::builtin().unwrap();
    let f = fixture_with_runtimes(
        Mode::All,
        vec![
            json!({"content":"first-reply"}),
            json!({"content":"second-reply"}),
        ],
        None,
        Some(builtin),
    )
    .await;
    let run = f.engine.start(request(2)).await.unwrap();
    let first = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(first.status, "completed");
    assert_eq!(first.usage["total_tokens"], 5);
    let (session, agent, turn): (String, String, String) = f
        .engine
        .store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT session_id,agent_id,turn_id FROM turn_tasks WHERE legacy_task_id=?1",
            [&first.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let receipt = f
        .engine
        .send_chat_turn(SendChatTurn {
            session_id: SessionId(session),
            agent_id: AgentId(agent),
            expected_last_turn_id: TurnId(turn),
            message: "next-user".into(),
            idempotency_key: "a01-next".into(),
        })
        .await
        .unwrap();
    let second = settled(&f.engine, &receipt.task.legacy_task_id).await;
    assert_eq!(second.status, "completed");
    assert_eq!(second.usage["total_tokens"], 5);
    let requests = f.mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m["role"] == "assistant" && m["content"] == "first-reply")
    );
    assert!(
        messages
            .iter()
            .any(|m| m["role"] == "user" && m["content"] == "next-user")
    );
}
#[test]
fn n18_cross_store_disable_blocks_new_agent_lease_preserves_old_lease() {
    use crate::{plugin_management::SetPluginEnabled, runtime_management::RuntimeId};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-runtime.db");
    let first_store = Arc::new(Store::open(&path).unwrap());
    let second = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
    let runtime = AgentRuntime::builtin().unwrap();
    let old = runtime.acquire_persisted(&first_store).unwrap();
    assert_eq!(runtime.status().unwrap().in_flight, 1);
    second
        .set_runtime_enabled(
            RuntimeId::ChatAgent,
            SetPluginEnabled {
                enabled: false,
                expected_revision: 0,
            },
            "agent-off",
        )
        .unwrap();
    assert!(runtime.acquire_persisted(&first_store).is_err());
    assert_eq!(runtime.status().unwrap().in_flight, 1);
    assert!(matches!(
        old.decide(StrategyInput::BeforeCompletion { tools: &[] }, &[], 0, 1),
        Ok(StrategyAction::UseTools(_))
    ));
    drop(old);
    assert_eq!(runtime.status().unwrap().in_flight, 0);
    assert!(runtime.acquire_persisted(&first_store).is_err());
}
#[test]
fn n18_committed_replay_precedes_busy_during_agent_stop() {
    use crate::{
        plugin_management::SetPluginEnabled,
        runtime_management::{RuntimeId, RuntimeManagementError},
    };
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("runtime.db")).unwrap());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let runtime = Arc::try_unwrap(blocking_runtime(entered_tx, release.clone()))
        .ok()
        .unwrap();
    let engine = Engine::new_with_agent_runtime_for_test(store, 2, runtime).unwrap();
    let worker = {
        let engine = engine.clone();
        std::thread::spawn(move || {
            engine.set_runtime_enabled(
                RuntimeId::ChatAgent,
                SetPluginEnabled {
                    enabled: false,
                    expected_revision: 0,
                },
                "during-stop",
            )
        })
    };
    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("stop did not block");
    let replay = engine.set_runtime_enabled(
        RuntimeId::ChatAgent,
        SetPluginEnabled {
            enabled: false,
            expected_revision: 0,
        },
        "during-stop",
    );
    let new_key = engine.set_runtime_enabled(
        RuntimeId::ChatAgent,
        SetPluginEnabled {
            enabled: false,
            expected_revision: 1,
        },
        "new-during-stop",
    );
    *release.0.lock().unwrap() = true;
    release.1.notify_one();
    let original = worker.join().unwrap().unwrap();
    let replay = replay.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.receipt, original.receipt);
    assert!(matches!(new_key, Err(RuntimeManagementError::Busy)));
    assert!(
        engine
            .set_runtime_enabled(
                RuntimeId::ChatAgent,
                SetPluginEnabled {
                    enabled: false,
                    expected_revision: 1
                },
                "new-during-stop"
            )
            .is_ok()
    );
}
#[test]
fn n18_agent_admission_and_remote_disable_have_a_transaction_order() {
    let _probe_guard = crate::runtime_management_tests::barrier_test_guard();
    use crate::{
        engine::runtime_barrier, plugin_management::SetPluginEnabled, runtime_management::RuntimeId,
    };
    use std::sync::mpsc;
    for admission_first in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-runtime.db");
        let store = Arc::new(Store::open(&path).unwrap());
        let other = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        let key: String = db
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name='main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(db);
        let runtime = Arc::new(AgentRuntime::builtin().unwrap());
        let phase = if admission_first {
            "runtime_agent_admit_before_commit"
        } else {
            "runtime_intent_before_commit"
        };
        let barrier = runtime_barrier::arm(key, phase);
        let (started_tx, started_rx) = mpsc::channel();
        if admission_first {
            let r = runtime.clone();
            let s = store.clone();
            let admit = std::thread::spawn(move || r.acquire_persisted(&s));
            let arrived = barrier.wait_arrived(Duration::from_secs(5));
            if !arrived {
                barrier.release();
                let _ = admit.join();
                panic!("admission barrier not reached");
            }
            let disable = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                other.set_runtime_enabled(
                    RuntimeId::ChatAgent,
                    SetPluginEnabled {
                        enabled: false,
                        expected_revision: 0,
                    },
                    "remote-off",
                )
            });
            let started = started_rx.recv_timeout(Duration::from_secs(5));
            barrier.release();
            started.expect("disable worker did not start");
            let lease = admit.join().unwrap().unwrap();
            disable.join().unwrap().unwrap();
            assert_eq!(runtime.status().unwrap().in_flight, 1);
            assert!(runtime.acquire_persisted(&store).is_err());
            drop(lease);
        } else {
            let disable = std::thread::spawn(move || {
                other.set_runtime_enabled(
                    RuntimeId::ChatAgent,
                    SetPluginEnabled {
                        enabled: false,
                        expected_revision: 0,
                    },
                    "remote-off",
                )
            });
            let arrived = barrier.wait_arrived(Duration::from_secs(5));
            if !arrived {
                barrier.release();
                let _ = disable.join();
                panic!("disable barrier not reached");
            }
            let r = runtime.clone();
            let s = store.clone();
            let admit = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                r.acquire_persisted(&s)
            });
            let started = started_rx.recv_timeout(Duration::from_secs(5));
            barrier.release();
            started.expect("admission worker did not start");
            disable.join().unwrap().unwrap();
            assert!(admit.join().unwrap().is_err());
            assert_eq!(runtime.status().unwrap().in_flight, 0);
        }
    }
}
struct N18CountedAgent {
    starts: Arc<AtomicUsize>,
}
impl crate::plugin_host::BuiltinPlugin for N18CountedAgent {
    fn activate(
        &mut self,
        ctx: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let strategy: Arc<dyn AgentStrategy> = Arc::new(Strategy {
            mode: Mode::All,
            trace: Arc::new(Trace::default()),
        });
        ctx.register_effect(&agent_key(), Box::new(strategy))
            .unwrap();
        Ok(())
    }
    fn deactivate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        Ok(())
    }
}
fn n18_counted_agent_host(starts: Arc<AtomicUsize>) -> crate::plugin_host::PluginHost {
    use crate::plugin_catalog::ScopeKey;
    let mut host = crate::plugin_host::PluginHost::new(ScopeKey::Host).unwrap();
    host.catalog_mut()
        .register(agent_manifest(
            "builtin.engine-chat-agent",
            vec![],
            vec![agent_key()],
        ))
        .unwrap();
    host.with_factory("builtin.engine-chat-agent", || Box::new(Consumer));
    host.catalog_mut()
        .register(agent_manifest(
            "builtin.chat-agent",
            vec![agent_key()],
            vec![],
        ))
        .unwrap();
    host.with_factory("builtin.chat-agent", move || {
        Box::new(N18CountedAgent {
            starts: starts.clone(),
        })
    });
    host
}
#[test]
fn n18_agent_false_start_has_no_activation_or_effects() {
    let starts = Arc::new(AtomicUsize::new(0));
    let runtime =
        AgentRuntime::from_host_with_state_for_test(n18_counted_agent_host(starts.clone()), false)
            .unwrap();
    let state = runtime.status().unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(!state.active && !state.admission_open && !state.draining);
    assert_eq!(state.effects, 0);
    assert_eq!(state.generation, 1);
    drop(runtime);
    let enabled =
        AgentRuntime::from_host_with_state_for_test(n18_counted_agent_host(starts.clone()), true)
            .unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(enabled.status().unwrap().active);
}
#[test]
fn n18_agent_store_failure_never_uses_cached_enabled_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-runtime.db");
    let store = Store::open(&path).unwrap();
    let runtime = AgentRuntime::builtin().unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE plugin_desired;")
        .unwrap();
    assert!(runtime.acquire_persisted(&store).is_err());
    assert_eq!(runtime.status().unwrap().in_flight, 0);
}
