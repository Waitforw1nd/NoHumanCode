use crate::{
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
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

const SENTINEL: &str = "nhc-codec-private-panic-sentinel";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    Encode,
    Decoder,
    Feed,
    Finish,
    EncodePanic,
    DecoderPanic,
    FeedPanic,
    DonePanic,
    FinishPanic,
    Alter,
    AlterMessages,
    AlterMax,
    AlterStream,
    AlterUsage,
    AlterTools,
    ExtraField,
}

struct TestCodec {
    fault: Fault,
    encoded: Arc<AtomicUsize>,
    decoded: Arc<AtomicUsize>,
}
impl CompletionCodec for TestCodec {
    fn encode(&self, input: &CompletionInput<'_>) -> Result<Value, CodecError> {
        self.encoded.fetch_add(1, Ordering::SeqCst);
        if self.fault == Fault::Encode {
            return Err(CodecError::Failed);
        }
        if self.fault == Fault::EncodePanic {
            panic!("{SENTINEL}");
        }
        let mut body = json!({"model":input.model,"messages":input.messages,"max_tokens":input.max_tokens,"stream":true,"stream_options":{"include_usage":true}});
        if !input.tools.is_empty() {
            body["tools"] = json!(input.tools);
            body["tool_choice"] = json!("auto");
        }
        match self.fault {
            Fault::Alter => body["model"] = json!("unapproved"),
            Fault::AlterMessages => body["messages"] = json!([]),
            Fault::AlterMax => body["max_tokens"] = json!(129),
            Fault::AlterStream => body["stream"] = json!(false),
            Fault::AlterUsage => body["stream_options"]["include_usage"] = json!(false),
            Fault::AlterTools => body["tools"] = json!([]),
            Fault::ExtraField => body["unapproved"] = json!(true),
            _ => {}
        }
        Ok(body)
    }
    fn decoder(&self) -> Result<Box<dyn CompletionDecoder>, CodecError> {
        self.decoded.fetch_add(1, Ordering::SeqCst);
        if self.fault == Fault::Decoder {
            return Err(CodecError::Failed);
        }
        if self.fault == Fault::DecoderPanic {
            panic!("{SENTINEL}");
        }
        Ok(Box::new(TestDecoder {
            fault: self.fault,
            seen: false,
        }))
    }
}
struct TestDecoder {
    fault: Fault,
    seen: bool,
}
impl CompletionDecoder for TestDecoder {
    fn feed(&mut self, _bytes: &[u8], _emit: &mut Emit<'_>) -> Result<(), CodecError> {
        if self.fault == Fault::Feed {
            return Err(CodecError::Failed);
        }
        if self.fault == Fault::FeedPanic {
            panic!("{SENTINEL}");
        }
        self.seen = true;
        Ok(())
    }
    fn is_done(&self) -> bool {
        if self.fault == Fault::DonePanic {
            panic!("{SENTINEL}");
        }
        self.seen
    }
    fn finish(self: Box<Self>, _emit: &mut Emit<'_>) -> Result<CompletionOutput, CodecError> {
        if self.fault == Fault::Finish {
            return Err(CodecError::Failed);
        }
        if self.fault == Fault::FinishPanic {
            panic!("{SENTINEL}");
        }
        Ok((
            json!({"role":"assistant","content":"codec-marker"}),
            json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}),
        ))
    }
}

#[derive(Clone, Default)]
struct Mock {
    models: Arc<AtomicUsize>,
    completions: Arc<AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<Value>>>,
    hold: Arc<AtomicBool>,
    arrived: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
async fn models(State(m): State<Mock>) -> Json<Value> {
    m.models.fetch_add(1, Ordering::SeqCst);
    Json(json!({"data":[{"id":"mock"}]}))
}
async fn completion(State(m): State<Mock>, Json(body): Json<Value>) -> &'static str {
    m.completions.fetch_add(1, Ordering::SeqCst);
    m.requests.lock().unwrap().push(body);
    if m.hold.load(Ordering::SeqCst) {
        m.arrived.notify_one();
        m.release.notified().await;
    }
    "data: {\"choices\":[{\"delta\":{\"content\":\"wire-marker\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\ndata: [DONE]\n\n"
}
struct Fixture {
    _dir: tempfile::TempDir,
    engine: Arc<Engine>,
    project: ProjectId,
    mock: Mock,
    server: tokio::task::JoinHandle<()>,
    encoded: Arc<AtomicUsize>,
    decoded: Arc<AtomicUsize>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture(fault: Fault) -> Fixture {
    fixture_mode(fault, false).await
}
async fn fixture_mode(fault: Fault, builtin: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/models", get(models))
        .route("/chat/completions", post(completion))
        .with_state(mock.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = Arc::new(Store::open(&dir.path().join("provider-test.db")).unwrap());
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
    store.put_secret("route::route", "fixture-secret").unwrap();
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
    let before = store.project_config(&project.id).unwrap();
    store
        .configure_project(ConfigureProject {
            project_id: project.id.clone(),
            expected_revision: before.revision,
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
    let preview = store.network_preview(subject.clone()).unwrap();
    store
        .network_confirm(
            subject,
            ConfirmRequest {
                config_revision: preview.config_revision,
                credential_revision: preview.credential_revision,
                binding_digest: preview.binding_digest,
                purposes: vec![NetworkPurpose::Models, NetworkPurpose::Completion],
            },
            "grant",
        )
        .unwrap();
    let encoded = Arc::new(AtomicUsize::new(0));
    let decoded = Arc::new(AtomicUsize::new(0));
    let runtime = ProviderRuntime::with_codec_for_test(Arc::new(TestCodec {
        fault,
        encoded: encoded.clone(),
        decoded: decoded.clone(),
    }))
    .unwrap();
    let engine = if builtin {
        Engine::new(store, 2).unwrap()
    } else {
        Engine::new_with_provider_runtime_for_test(store, 2, runtime).unwrap()
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
        _dir: dir,
        engine,
        project: project.id,
        mock,
        server,
        encoded,
        decoded,
    }
}
fn request() -> RunRequest {
    RunRequest {
        title: "provider-runtime".into(),
        kind: SessionKind::Chat,
        tasks: vec![TaskSpec {
            name: "agent".into(),
            role: "worker".into(),
            route_id: "route".into(),
            prompt: "reply".into(),
            depends_on: vec![],
            write_scopes: vec![],
            tools: false,
            allow_commands: false,
            max_rounds: 1,
        }],
    }
}
async fn settled(engine: &Engine, id: &str) -> Task {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task = engine.store.task(id).unwrap();
            if !engine.is_busy() {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn p03_both_engine_paths_consume_replacement_codec() {
    let f = fixture(Fault::None).await;
    let checked = f
        .engine
        .project_route_check(&f.project, "route")
        .await
        .unwrap();
    assert_eq!(checked.result["answer"], "codec-marker");
    assert_eq!(
        checked.calls.iter().map(|c| c.purpose).collect::<Vec<_>>(),
        vec![NetworkPurpose::Models, NetworkPurpose::Completion]
    );
    let run = f.engine.start(request()).await.unwrap();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "completed");
    assert!(task.messages.iter().any(|m| m["content"] == "codec-marker"));
    assert_eq!(
        (
            f.mock.models.load(Ordering::SeqCst),
            f.mock.completions.load(Ordering::SeqCst)
        ),
        (1, 2)
    );
    assert_eq!(
        (
            f.encoded.load(Ordering::SeqCst),
            f.decoded.load(Ordering::SeqCst)
        ),
        (2, 2)
    );
    assert_eq!(f.engine.budget_status(&f.project).unwrap().spent_tokens, 10);
}

#[tokio::test]
async fn p05_preclaim_failures_preserve_models_receipt_and_never_send_completion() {
    for fault in [
        Fault::Encode,
        Fault::Decoder,
        Fault::EncodePanic,
        Fault::DecoderPanic,
        Fault::Alter,
        Fault::AlterMessages,
        Fault::AlterMax,
        Fault::AlterStream,
        Fault::AlterUsage,
        Fault::AlterTools,
        Fault::ExtraField,
    ] {
        let f = fixture(fault).await;
        let err = f
            .engine
            .project_route_check(&f.project, "route")
            .await
            .unwrap_err();
        assert_eq!(err.dispatch_state, DispatchState::NotSent, "{fault:?}");
        assert_eq!(err.calls.len(), 1, "{fault:?}");
        assert_eq!(err.calls[0].purpose, NetworkPurpose::Models);
        assert!(!format!("{err:?}").contains(SENTINEL));
        assert_eq!(
            (
                f.mock.models.load(Ordering::SeqCst),
                f.mock.completions.load(Ordering::SeqCst)
            ),
            (1, 0)
        );
        let budget = f.engine.budget_status(&f.project).unwrap();
        assert_eq!((budget.held_tokens, budget.unresolved_count), (0, 0));
    }
}

#[tokio::test]
async fn p06_decoder_failures_after_send_are_unknown_and_hold_reservation() {
    for fault in [
        Fault::Feed,
        Fault::FeedPanic,
        Fault::DonePanic,
        Fault::Finish,
        Fault::FinishPanic,
    ] {
        let f = fixture(fault).await;
        let err = f
            .engine
            .project_route_check(&f.project, "route")
            .await
            .unwrap_err();
        assert_eq!(err.kind, NetworkErrorKind::Unknown, "{fault:?}");
        assert_eq!(err.calls.len(), 2, "{fault:?}");
        assert_eq!(err.calls[1].state, CallState::Unknown, "{fault:?}");
        assert!(!format!("{err:?}").contains(SENTINEL));
        assert_eq!(f.mock.completions.load(Ordering::SeqCst), 1);
        let budget = f.engine.budget_status(&f.project).unwrap();
        assert_eq!(
            (budget.held_tokens, budget.unresolved_count),
            (100, 1),
            "{fault:?}"
        );
    }
}

fn make_runtime() -> ProviderRuntime {
    ProviderRuntime::with_codec_for_test(Arc::new(TestCodec {
        fault: Fault::None,
        encoded: Arc::new(AtomicUsize::new(0)),
        decoded: Arc::new(AtomicUsize::new(0)),
    }))
    .unwrap()
}
fn input<'a>(messages: &'a [Value], tools: &'a [Value]) -> CompletionInput<'a> {
    CompletionInput {
        model: "mock",
        max_tokens: 128,
        messages,
        tools,
    }
}

#[test]
fn p04_disabled_first_and_old_generation_never_admit() {
    let runtime = make_runtime();
    let messages = vec![json!({"role":"user","content":"hi"})];
    let tools = vec![];
    let prepared = runtime.prepare(&input(&messages, &tools)).unwrap();
    let generation = runtime.status_for_test().unwrap().generation;
    runtime.disable_for_test().unwrap();
    assert!(!runtime.status_for_test().unwrap().admission_open);
    let called = AtomicUsize::new(0);
    assert!(
        runtime
            .acquire(prepared, || {
                called.fetch_add(1, Ordering::SeqCst);
                Ok::<_, NetworkError>(())
            })
            .is_err()
    );
    assert_eq!(called.load(Ordering::SeqCst), 0);
    runtime.enable_for_test().unwrap();
    let status = runtime.status_for_test().unwrap();
    assert!(status.admission_open && status.generation != generation);
    let stale = runtime.prepare(&input(&messages, &tools)).unwrap();
    runtime.disable_for_test().unwrap();
    runtime.enable_for_test().unwrap();
    assert!(
        runtime
            .acquire(stale, || {
                called.fetch_add(1, Ordering::SeqCst);
                Ok::<_, NetworkError>(())
            })
            .is_err()
    );
    assert_eq!(called.load(Ordering::SeqCst), 0);
    let fresh = runtime.prepare(&input(&messages, &tools)).unwrap();
    let (lease, ()) = runtime
        .acquire(fresh, || Ok::<_, NetworkError>(()))
        .unwrap();
    assert_eq!(runtime.status_for_test().unwrap().in_flight, 1);
    drop(lease);
    assert_eq!(runtime.status_for_test().unwrap().in_flight, 0);
}

#[test]
fn p04_admission_first_keeps_lease_through_disable() {
    use std::sync::{Barrier, mpsc};
    let runtime = Arc::new(make_runtime());
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (tx, rx) = mpsc::channel();
    let worker = {
        let runtime = runtime.clone();
        let entered = entered.clone();
        let release = release.clone();
        std::thread::spawn(move || {
            let messages = vec![json!({"role":"user","content":"hi"})];
            let tools = vec![];
            let prepared = runtime.prepare(&input(&messages, &tools)).unwrap();
            let (lease, ()) = runtime
                .acquire(prepared, || {
                    entered.wait();
                    release.wait();
                    Ok::<_, NetworkError>(())
                })
                .unwrap();
            tx.send(lease).unwrap();
        })
    };
    entered.wait();
    let (started_tx, started_rx) = mpsc::channel();
    let disable = {
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            runtime.disable_for_test().unwrap();
        })
    };
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release.wait();
    let lease = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.join().unwrap();
    disable.join().unwrap();
    let state = runtime.status_for_test().unwrap();
    assert!(!state.admission_open);
    assert_eq!(state.in_flight, 1);
    let messages = vec![json!({"role":"user","content":"again"})];
    let tools = vec![];
    assert!(runtime.prepare(&input(&messages, &tools)).is_err());
    drop(lease);
    assert_eq!(runtime.status_for_test().unwrap().in_flight, 0);
}

#[tokio::test]
async fn p06_cancel_after_send_finishes_unknown_without_resend() {
    let f = fixture(Fault::None).await;
    f.mock.hold.store(true, Ordering::SeqCst);
    let run = f.engine.start(request()).await.unwrap();
    let task_id = &run.tasks[0].id;
    tokio::time::timeout(Duration::from_secs(10), f.mock.arrived.notified())
        .await
        .unwrap();
    f.engine.cancel(task_id).unwrap();
    let task = settled(&f.engine, task_id).await;
    assert_ne!(task.status, "completed");
    let calls = f.engine.network_calls_for_task(task_id).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].state, CallState::Unknown);
    let budget = f.engine.budget_status(&f.project).unwrap();
    assert_eq!((budget.held_tokens, budget.unresolved_count), (100, 1));
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.engine
            .provider_runtime_for_test()
            .status_for_test()
            .unwrap()
            .in_flight,
        0
    );
    f.mock.release.notify_one();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p06_real_host_kill_and_recovery_preserve_claim_without_resend() {
    use std::process::{Command, Stdio};
    if let Ok(root) = std::env::var("NHC_P16_RECOVER_ROOT") {
        let root = std::path::PathBuf::from(root);
        let call_id = std::env::var("NHC_P16_RECOVER_CALL").unwrap();
        let output = std::env::var("NHC_P16_RECOVER_OUTPUT").unwrap();
        let store = Arc::new(Store::open(&root.join("provider-test.db")).unwrap());
        store.recover().unwrap();
        let project = store.current_project().unwrap().project_id.unwrap();
        let engine = Engine::new_with_provider_runtime_for_test(store, 2, make_runtime()).unwrap();
        let status = engine.budget_status(&project).unwrap();
        let call = engine.budget_call(&project, &call_id).unwrap();
        let snapshot = json!({
            "pid": std::process::id(),
            "state": {
                "call_id": call.call_id,
                "network_state": call.call.state,
                "network_result": call.call.result,
                "network_success": call.call.success,
                "budget_state": call.state,
                "budget_reason": call.reason,
                "reserved_tokens": call.reserved_tokens,
                "held_tokens": status.held_tokens,
                "unresolved_count": status.unresolved_count,
                "claimed_count": status.claimed_count
            }
        });
        std::fs::write(root.join(output), serde_json::to_vec(&snapshot).unwrap()).unwrap();
        return;
    }
    if let Ok(root) = std::env::var("NHC_P16_CRASH_ROOT") {
        let root = std::path::PathBuf::from(root);
        let phase = match std::env::var("NHC_P16_CRASH_PHASE").unwrap().as_str() {
            "budget-after-claim" => "budget-after-claim",
            "budget-before-finish" => "budget-before-finish",
            _ => panic!("invalid phase"),
        };
        let store = Arc::new(Store::open(&root.join("provider-test.db")).unwrap());
        let project = store.current_project().unwrap().project_id.unwrap();
        let subject = NetworkSubject {
            kind: NetworkSubjectKind::Route,
            id: "route".into(),
            project_id: Some(project.clone()),
        };
        crate::engine::crash_probe::arm(subject.key().unwrap(), phase, root.join("crash-signal"));
        let engine = Engine::new_with_provider_runtime_for_test(store, 2, make_runtime()).unwrap();
        let _ = engine.project_route_check(&project, "route").await;
        panic!("crash probe did not block");
    }
    for (phase, expected_hits) in [("budget-after-claim", 0), ("budget-before-finish", 1)] {
        let f = fixture(Fault::None).await;
        let root = f._dir.path();
        let mut child=Command::new(std::env::current_exe().unwrap())
            .args(["--exact","provider_runtime_tests::p06_real_host_kill_and_recovery_preserve_claim_without_resend","--nocapture"])
            .env("NHC_P16_CRASH_ROOT",root).env("NHC_P16_CRASH_PHASE",phase)
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let signal = loop {
            if let Ok(s) = std::fs::read_to_string(root.join("crash-signal")) {
                let lines: Vec<_> = s.lines().collect();
                if lines.len() == 2 && uuid::Uuid::parse_str(lines[1]).is_ok() {
                    break s;
                }
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("child exited early {phase}: {status}");
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("missing crash signal: {phase}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        let call_id = signal.lines().nth(1).unwrap();
        let mut states = Vec::new();
        for run in 1..=2 {
            let output = format!("recovery-{run}.json");
            let mut recovery = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "provider_runtime_tests::p06_real_host_kill_and_recovery_preserve_claim_without_resend",
                    "--nocapture",
                ])
                .env("NHC_P16_RECOVER_ROOT", root)
                .env("NHC_P16_RECOVER_CALL", call_id)
                .env("NHC_P16_RECOVER_OUTPUT", &output)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let spawned_pid = recovery.id();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = recovery.try_wait().unwrap() {
                    assert!(status.success(), "recovery {run} failed: {status}");
                    assert!(recovery.wait().unwrap().success());
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    let _ = recovery.kill();
                    let _ = recovery.wait();
                    panic!("recovery {run} timed out: {phase}");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let snapshot: Value =
                serde_json::from_slice(&std::fs::read(root.join(output)).unwrap()).unwrap();
            assert_eq!(snapshot["pid"], spawned_pid);
            assert_ne!(spawned_pid, std::process::id());
            let state = snapshot["state"].clone();
            assert_eq!(state["call_id"], call_id);
            assert_eq!(state["network_state"], "unknown");
            assert_eq!(state["reserved_tokens"], 100);
            assert_eq!(state["held_tokens"], 100);
            assert_eq!(state["unresolved_count"], 1);
            assert_eq!(state["claimed_count"], 0);
            assert_eq!(f.mock.completions.load(Ordering::SeqCst), expected_hits);
            states.push(state);
        }
        assert_eq!(states[0], states[1]);
    }
}

#[tokio::test]
async fn p05_panic_payload_is_not_written_to_process_output() {
    const ORDINARY_PANIC: &str = "nhc-ordinary-host-panic-control";
    if let Ok(name) = std::env::var("NHC_P16_PANIC_PHASE") {
        let fault = match name.as_str() {
            "encode" => Fault::EncodePanic,
            "decoder" => Fault::DecoderPanic,
            "feed" => Fault::FeedPanic,
            "done" => Fault::DonePanic,
            "finish" => Fault::FinishPanic,
            "ordinary" => Fault::EncodePanic,
            _ => panic!("unknown phase"),
        };
        let f = fixture(fault).await;
        let err = f
            .engine
            .project_route_check(&f.project, "route")
            .await
            .unwrap_err();
        assert!(!format!("{err:?}").contains(SENTINEL));
        if name == "ordinary" {
            assert!(std::panic::catch_unwind(|| panic!("{ORDINARY_PANIC}")).is_err());
        }
        return;
    }
    for phase in ["encode", "decoder", "feed", "done", "finish", "ordinary"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "provider_runtime_tests::p05_panic_payload_is_not_written_to_process_output",
                "--nocapture",
            ])
            .env("NHC_P16_PANIC_PHASE", phase)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "phase={phase} status={}",
            output.status
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(SENTINEL),
            "stdout leaked {phase}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains(SENTINEL),
            "stderr leaked {phase}"
        );
        if phase == "ordinary" {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(ORDINARY_PANIC),
                "ordinary panic hook was silenced"
            );
        }
    }
}

fn p01_key(minor: u32) -> crate::plugin_catalog::InterfaceKey {
    use crate::plugin_catalog::{ExactVersion, InterfaceKey, InterfaceKind};
    InterfaceKey {
        kind: InterfaceKind::Service,
        name: "provider.completion-codec".into(),
        version: ExactVersion {
            major: 1,
            minor,
            patch: 0,
        },
    }
}
fn p01_manifest(
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
struct P01Consumer;
impl crate::plugin_host::BuiltinPlugin for P01Consumer {
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
struct P01Provider {
    mode: u8,
}
impl crate::plugin_host::BuiltinPlugin for P01Provider {
    fn activate(
        &mut self,
        ctx: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        if self.mode == 2 {
            return Err(crate::plugin_host::PluginError::new("fixture failure"));
        }
        if self.mode == 1 {
            ctx.register_effect(&p01_key(0), Box::new(42u32)).unwrap();
        } else {
            let codec: Arc<dyn CompletionCodec> = Arc::new(TestCodec {
                fault: Fault::None,
                encoded: Arc::new(AtomicUsize::new(0)),
                decoded: Arc::new(AtomicUsize::new(0)),
            });
            ctx.register_effect(&p01_key(0), Box::new(codec)).unwrap();
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
fn p01_host(case: &str) -> crate::plugin_host::PluginHost {
    use crate::plugin_catalog::ScopeKey;
    let mut host = crate::plugin_host::PluginHost::new(ScopeKey::Host).unwrap();
    host.catalog_mut()
        .register(p01_manifest(
            "builtin.engine-provider",
            vec![],
            vec![p01_key(0)],
        ))
        .unwrap();
    host.with_factory("builtin.engine-provider", || Box::new(P01Consumer));
    if case != "missing" {
        let key = if case == "version" {
            p01_key(1)
        } else {
            p01_key(0)
        };
        host.catalog_mut()
            .register(p01_manifest("builtin.provider-xpeach", vec![key], vec![]))
            .unwrap();
        if case != "factory" {
            let mode = match case {
                "type" => 1,
                "activate" => 2,
                _ => 0,
            };
            host.with_factory("builtin.provider-xpeach", move || {
                Box::new(P01Provider { mode })
            });
        }
    }
    host
}
#[test]
fn p01_plugin_host_binding_fails_closed_for_missing_version_type_and_activation() {
    for case in ["missing", "version", "factory", "type", "activate"] {
        assert!(
            ProviderRuntime::from_host_for_test(p01_host(case)).is_err(),
            "case={case}"
        );
    }
    let runtime = ProviderRuntime::from_host_for_test(p01_host("valid")).unwrap();
    let status = runtime.status_for_test().unwrap();
    assert!(status.active && status.admission_open);
}

struct DropReentryCodec {
    runtime: Arc<std::sync::Mutex<Option<std::sync::Weak<ProviderRuntime>>>>,
    dropped: Arc<AtomicUsize>,
}
struct DropReentryDecoder {
    runtime: Arc<std::sync::Mutex<Option<std::sync::Weak<ProviderRuntime>>>>,
    dropped: Arc<AtomicUsize>,
}
impl CompletionCodec for DropReentryCodec {
    fn encode(&self, input: &CompletionInput<'_>) -> Result<Value, CodecError> {
        Ok(
            json!({"model":input.model,"messages":input.messages,"max_tokens":input.max_tokens,"stream":true,"stream_options":{"include_usage":true}}),
        )
    }
    fn decoder(&self) -> Result<Box<dyn CompletionDecoder>, CodecError> {
        Ok(Box::new(DropReentryDecoder {
            runtime: self.runtime.clone(),
            dropped: self.dropped.clone(),
        }))
    }
}
impl CompletionDecoder for DropReentryDecoder {
    fn feed(&mut self, _: &[u8], _: &mut Emit<'_>) -> Result<(), CodecError> {
        Ok(())
    }
    fn is_done(&self) -> bool {
        true
    }
    fn finish(self: Box<Self>, _: &mut Emit<'_>) -> Result<CompletionOutput, CodecError> {
        Ok((json!({}), json!({})))
    }
}
impl Drop for DropReentryDecoder {
    fn drop(&mut self) {
        if let Some(runtime) = self
            .runtime
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|w| w.upgrade())
        {
            let _ = runtime.status_for_test().unwrap();
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}
#[test]
fn p04_rejected_admit_drops_decoder_outside_runtime_lock() {
    let slot = Arc::new(std::sync::Mutex::new(None));
    let dropped = Arc::new(AtomicUsize::new(0));
    let runtime = Arc::new(
        ProviderRuntime::with_codec_for_test(Arc::new(DropReentryCodec {
            runtime: slot.clone(),
            dropped: dropped.clone(),
        }))
        .unwrap(),
    );
    *slot.lock().unwrap() = Some(Arc::downgrade(&runtime));
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let messages = vec![json!({"role":"user","content":"test"})];
        let tools = vec![];
        let prepared = runtime.prepare(&input(&messages, &tools)).unwrap();
        let result = runtime.acquire(prepared, || {
            Err::<(), _>(NetworkError::not_sent(NetworkErrorKind::Required))
        });
        tx.send(result.is_err()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("decoder drop reentered a held runtime lock")
    );
    worker.join().unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn p02_builtin_engine_preserves_wire_decode_and_usage() {
    let f = fixture_mode(Fault::None, true).await;
    let checked = f
        .engine
        .project_route_check(&f.project, "route")
        .await
        .unwrap();
    assert_eq!(checked.result["answer"], "wire-marker");
    assert_eq!(checked.result["usage"]["total_tokens"], 5);
    let body = f.mock.requests.lock().unwrap()[0].clone();
    assert_eq!(body["model"], "mock");
    assert_eq!(body["max_tokens"], 128);
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert!(body.get("tools").is_none());
    assert_eq!(f.engine.budget_status(&f.project).unwrap().spent_tokens, 5);
}

#[tokio::test]
async fn p04_engine_management_disables_new_completion_and_reenables() {
    let f = fixture(Fault::None).await;
    let initial = f.engine.builtin_provider_status().unwrap();
    f.engine.disable_builtin_provider().unwrap();
    let disabled = f.engine.builtin_provider_status().unwrap();
    assert!(!disabled.admission_open && !disabled.active);
    assert_eq!(disabled.in_flight, 0);
    assert!(disabled.generation != initial.generation);
    let err = f
        .engine
        .project_route_check(&f.project, "route")
        .await
        .unwrap_err();
    assert_eq!(err.dispatch_state, DispatchState::NotSent);
    assert_eq!(err.calls.len(), 1);
    assert_eq!(err.calls[0].purpose, NetworkPurpose::Models);
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 0);
    assert_eq!(f.engine.budget_status(&f.project).unwrap().held_tokens, 0);
    f.engine.enable_builtin_provider().unwrap();
    let enabled = f.engine.builtin_provider_status().unwrap();
    assert!(enabled.active && enabled.admission_open && enabled.generation != disabled.generation);
    let result = f
        .engine
        .project_route_check(&f.project, "route")
        .await
        .unwrap();
    assert_eq!(result.result["answer"], "codec-marker");
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 1);
}

#[test]
fn p02_original_decoder_fragmented_tool_usage_and_limit_controls() {
    let data = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.txt\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\ndata: [DONE]\n\n"
    );
    let mut decoder = crate::provider::Decoder::default();
    let mut text = String::new();
    for byte in data.as_bytes() {
        decoder
            .feed(&[*byte], &mut |delta| {
                text.push_str(delta);
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(text, "hello");
    assert_eq!(decoder.usage["total_tokens"], 5);
    let message = decoder.finish(&mut |_| Ok(())).unwrap();
    assert_eq!(message["tool_calls"][0]["function"]["name"], "read_file");
    assert_eq!(
        message["tool_calls"][0]["function"]["arguments"],
        "{\"path\":\"a.txt\"}"
    );
    let mut limited = crate::provider::Decoder::default();
    assert!(
        limited
            .feed(
                &vec![b' '; crate::provider_transport::RESPONSE_LIMIT + 1],
                &mut |_| Ok(())
            )
            .is_err()
    );
}

#[tokio::test]
async fn p04_disabling_during_real_completion_allows_existing_lease_to_finish() {
    let f = fixture(Fault::None).await;
    f.mock.hold.store(true, Ordering::SeqCst);
    let run = f.engine.start(request()).await.unwrap();
    let task_id = &run.tasks[0].id;
    tokio::time::timeout(Duration::from_secs(10), f.mock.arrived.notified())
        .await
        .unwrap();
    assert_eq!(f.engine.builtin_provider_status().unwrap().in_flight, 1);
    f.engine.disable_builtin_provider().unwrap();
    let disabled = f.engine.builtin_provider_status().unwrap();
    assert!(!disabled.admission_open && !disabled.active);
    assert_eq!(disabled.in_flight, 1);
    let err = f
        .engine
        .project_route_check(&f.project, "route")
        .await
        .unwrap_err();
    assert_eq!(err.dispatch_state, DispatchState::NotSent);
    assert_eq!(err.calls.len(), 1);
    assert_eq!(err.calls[0].purpose, NetworkPurpose::Models);
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 1);
    f.mock.release.notify_one();
    let task = settled(&f.engine, task_id).await;
    assert_eq!(task.status, "completed");
    assert!(task.messages.iter().any(|m| m["content"] == "codec-marker"));
    let calls = f.engine.network_calls_for_task(task_id).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].state, CallState::Finished);
    assert_eq!(calls[0].success, Some(true));
    let budget = f.engine.budget_status(&f.project).unwrap();
    assert_eq!(
        (
            budget.spent_tokens,
            budget.held_tokens,
            budget.unresolved_count
        ),
        (5, 0, 0)
    );
    assert_eq!(f.engine.builtin_provider_status().unwrap().in_flight, 0);
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 1);
}

struct LifecycleProbe {
    mode: &'static str,
    runtime: Arc<std::sync::Mutex<Option<std::sync::Weak<ProviderRuntime>>>>,
}
impl crate::plugin_host::BuiltinPlugin for LifecycleProbe {
    fn activate(
        &mut self,
        ctx: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        if self.mode == "activate" {
            panic!("{SENTINEL}");
        }
        let codec: Arc<dyn CompletionCodec> = Arc::new(TestCodec {
            fault: Fault::None,
            encoded: Arc::new(AtomicUsize::new(0)),
            decoded: Arc::new(AtomicUsize::new(0)),
        });
        ctx.register_effect(&p01_key(0), Box::new(codec)).unwrap();
        Ok(())
    }
    fn deactivate(
        &mut self,
        _: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        if self.mode == "deactivate" {
            panic!("{SENTINEL}");
        }
        if let Some(runtime) = self
            .runtime
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|weak| weak.upgrade())
        {
            let state = runtime.status_for_test().unwrap();
            assert!(state.transitioning && !state.admission_open);
        }
        Ok(())
    }
}
fn lifecycle_host(
    mode: &'static str,
    slot: Arc<std::sync::Mutex<Option<std::sync::Weak<ProviderRuntime>>>>,
) -> crate::plugin_host::PluginHost {
    use crate::plugin_catalog::ScopeKey;
    let mut host = crate::plugin_host::PluginHost::new(ScopeKey::Host).unwrap();
    host.catalog_mut()
        .register(p01_manifest(
            "builtin.engine-provider",
            vec![],
            vec![p01_key(0)],
        ))
        .unwrap();
    host.with_factory("builtin.engine-provider", || Box::new(P01Consumer));
    host.catalog_mut()
        .register(p01_manifest(
            "builtin.provider-xpeach",
            vec![p01_key(0)],
            vec![],
        ))
        .unwrap();
    host.with_factory("builtin.provider-xpeach", move || {
        if mode == "factory" {
            panic!("{SENTINEL}");
        }
        Box::new(LifecycleProbe {
            mode,
            runtime: slot.clone(),
        })
    });
    host
}

#[test]
fn p05_lifecycle_panics_are_safe_and_fail_closed() {
    if let Ok(name) = std::env::var("NHC_P16_LIFECYCLE_PHASE") {
        let mode = match name.as_str() {
            "factory" => "factory",
            "activate" => "activate",
            "deactivate" => "deactivate",
            _ => panic!("invalid lifecycle phase"),
        };
        let slot = Arc::new(std::sync::Mutex::new(None));
        let result = ProviderRuntime::from_host_for_test(lifecycle_host(mode, slot));
        if mode == "deactivate" {
            let runtime = result.unwrap();
            assert!(runtime.disable_for_test().is_err());
            let state = runtime.status_for_test().unwrap();
            assert!(!state.admission_open && state.lifecycle_failed);
        } else {
            assert!(result.is_err());
        }
        return;
    }
    for phase in ["factory", "activate", "deactivate"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "provider_runtime_tests::p05_lifecycle_panics_are_safe_and_fail_closed",
                "--nocapture",
            ])
            .env("NHC_P16_LIFECYCLE_PHASE", phase)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "phase={phase} status={}",
            output.status
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(SENTINEL),
            "stdout leaked {phase}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains(SENTINEL),
            "stderr leaked {phase}"
        );
    }
}

#[test]
fn p04_deactivate_callback_can_reenter_status_without_deadlock() {
    let slot = Arc::new(std::sync::Mutex::new(None));
    let runtime = Arc::new(
        ProviderRuntime::from_host_for_test(lifecycle_host("reenter", slot.clone())).unwrap(),
    );
    *slot.lock().unwrap() = Some(Arc::downgrade(&runtime));
    let (tx, rx) = std::sync::mpsc::channel();
    let other = runtime.clone();
    let worker = std::thread::spawn(move || {
        tx.send(other.disable_for_test().is_ok()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("deactivate reentered a held runtime lock")
    );
    worker.join().unwrap();
    let state = runtime.status_for_test().unwrap();
    assert!(!state.admission_open && !state.active && !state.transitioning);
}
