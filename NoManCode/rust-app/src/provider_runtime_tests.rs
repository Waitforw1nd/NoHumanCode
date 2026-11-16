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
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

const SENTINEL: &str = "nhc-codec-private-panic-sentinel";

#[derive(Clone, Copy, PartialEq, Eq)]
enum RedactionEnding {
    Clean,
    FeedError,
    FinishError,
    Cancel,
}
struct RedactionCodec(RedactionEnding);
struct RedactionDecoder {
    ending: RedactionEnding,
    fed: bool,
}
const REDACTION_PARTS: &[&str] = &[
    "普通🦀 ",
    "s",
    "k-",
    "synthetic123",
    "; ",
    "password",
    "=",
    "\"",
    "synthetic",
    "456",
    "\"",
    "; Authorization: Ba",
    "sic ",
    "synthetic789",
    "; empty_password=\"",
    "\"",
    "\nend s",
];
impl CompletionCodec for RedactionCodec {
    fn encode(&self, input: &CompletionInput<'_>) -> Result<Value, CodecError> {
        Ok(crate::provider_runtime::expected_body(input))
    }
    fn decoder(&self) -> Result<Box<dyn CompletionDecoder>, CodecError> {
        Ok(Box::new(RedactionDecoder {
            ending: self.0,
            fed: false,
        }))
    }
}
impl CompletionDecoder for RedactionDecoder {
    fn feed(&mut self, _: &[u8], emit: &mut Emit<'_>) -> Result<(), CodecError> {
        if !self.fed {
            self.fed = true;
            for part in REDACTION_PARTS {
                emit(part).map_err(|_| CodecError::Failed)?;
            }
        }
        if self.ending == RedactionEnding::FeedError {
            Err(CodecError::Failed)
        } else {
            Ok(())
        }
    }
    fn is_done(&self) -> bool {
        self.fed && self.ending != RedactionEnding::Cancel
    }
    fn finish(self: Box<Self>, emit: &mut Emit<'_>) -> Result<CompletionOutput, CodecError> {
        // A codec may emit in finish, and a secret may cross feed -> finish.
        emit("k-syntheticFinish; done").map_err(|_| CodecError::Failed)?;
        if self.ending == RedactionEnding::FinishError {
            return Err(CodecError::Failed);
        }
        Ok((
            json!({"role":"assistant", "content":format!("{}k-syntheticFinish; done", REDACTION_PARTS.join(""))}),
            json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}),
        ))
    }
}

#[tokio::test]
async fn stream_redaction_replacement_codec_reaches_store_and_sse_safely() {
    use tower::ServiceExt;
    let f = fixture_with_codec(
        Fault::None,
        false,
        Some(Arc::new(RedactionCodec(RedactionEnding::Clean))),
    )
    .await;
    let run = f.engine.start(request()).await.unwrap();
    let task = settled(&f.engine, &run.tasks[0].id).await;
    assert_eq!(task.status, "completed");
    let expected = "普通🦀 [redacted]; password=\"[redacted]\"; Authorization: [redacted]; empty_password=\"\"\nend [redacted]; done";
    assert_eq!(task.output, expected);
    assert_eq!(task.messages.last().unwrap()["content"], expected);
    let events = f.engine.store.events(&run.id, 0).unwrap();
    let delta: String = events
        .iter()
        .filter(|e| e.kind == "delta")
        .map(|e| e.data["text"].as_str().unwrap())
        .collect();
    assert_eq!(delta, expected);
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("synthetic")
    );

    let app = crate::server::router(crate::server::App {
        engine: f.engine.clone(),
        token: "fixture-token".into(),
        origin: "http://127.0.0.1:12345".into(),
    });
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/runs/{}/events", run.id))
                .header("host", "127.0.0.1:12345")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let mut wire = String::new();
    let mut frames = 0;
    while frames < events.len() {
        let chunk = tokio::time::timeout(Duration::from_secs(3), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        wire.push_str(std::str::from_utf8(&chunk).unwrap());
        frames = wire.lines().filter(|l| l.starts_with("data:")).count();
    }
    let replay: String = wire
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|l| serde_json::from_str::<Value>(l.trim_start()).unwrap())
        .filter(|v| v["kind"] == "delta")
        .map(|v| v["data"]["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(replay, expected);
    assert!(!wire.contains("synthetic"));
}

#[tokio::test]
async fn stream_redaction_codec_errors_and_future_cancel_never_flush_raw_tail() {
    for ending in [
        RedactionEnding::FeedError,
        RedactionEnding::FinishError,
        RedactionEnding::Cancel,
    ] {
        let f =
            fixture_with_codec(Fault::None, false, Some(Arc::new(RedactionCodec(ending)))).await;
        if ending == RedactionEnding::Cancel {
            f.mock.stream_hold.store(true, Ordering::SeqCst);
        }
        let run = f.engine.start(request()).await.unwrap();
        if ending == RedactionEnding::Cancel {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let events = f.engine.store.events(&run.id, 0).unwrap();
                    if events.iter().any(|e| {
                        e.kind == "delta"
                            && e.data["text"].as_str().is_some_and(|s| s.contains("end "))
                    }) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            f.engine.cancel(&run.tasks[0].id).unwrap();
        }
        let task = settled(&f.engine, &run.tasks[0].id).await;
        assert_eq!(
            task.status,
            if ending == RedactionEnding::Cancel {
                "cancelled"
            } else {
                "failed"
            }
        );
        let events = f.engine.store.events(&run.id, 0).unwrap();
        let delta: String = events
            .iter()
            .filter(|e| e.kind == "delta")
            .map(|e| e.data["text"].as_str().unwrap())
            .collect();
        assert!(!delta.contains("synthetic"));
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("synthetic")
        );
        if ending != RedactionEnding::FinishError {
            assert!(delta.ends_with("end "));
        }
        assert_eq!(task.output, delta);
    }
}

struct AcrossRoundsCodec {
    parts: [&'static str; 2],
    ending: RedactionEnding,
    streamed: bool,
    tool_group: bool,
    decoded: AtomicUsize,
}
struct AcrossRoundsDecoder {
    part: &'static str,
    first: bool,
    ending: RedactionEnding,
    streamed: bool,
    fed: bool,
}
impl CompletionCodec for AcrossRoundsCodec {
    fn encode(&self, input: &CompletionInput<'_>) -> Result<Value, CodecError> {
        Ok(crate::provider_runtime::expected_body(input))
    }
    fn decoder(&self) -> Result<Box<dyn CompletionDecoder>, CodecError> {
        let round = self.decoded.fetch_add(1, Ordering::SeqCst);
        let part = *self.parts.get(round).ok_or(CodecError::Failed)?;
        Ok(Box::new(AcrossRoundsDecoder {
            part,
            first: round == 0 && self.tool_group,
            ending: if round == 0 {
                RedactionEnding::Clean
            } else {
                self.ending
            },
            streamed: self.streamed,
            fed: false,
        }))
    }
}
impl CompletionDecoder for AcrossRoundsDecoder {
    fn feed(&mut self, _: &[u8], emit: &mut Emit<'_>) -> Result<(), CodecError> {
        if !self.fed {
            self.fed = true;
            if self.streamed {
                emit(self.part).map_err(|_| CodecError::Failed)?;
            }
        }
        if self.ending == RedactionEnding::FeedError {
            Err(CodecError::Failed)
        } else {
            Ok(())
        }
    }
    fn is_done(&self) -> bool {
        self.fed && self.ending != RedactionEnding::Cancel
    }
    fn finish(self: Box<Self>, _: &mut Emit<'_>) -> Result<CompletionOutput, CodecError> {
        if self.ending == RedactionEnding::FinishError {
            return Err(CodecError::Failed);
        }
        let mut message = json!({"role":"assistant", "content":self.part});
        if self.first {
            // A real read-only tool group makes Engine request a second completion.
            message["tool_calls"] = json!([{
                "id":"audit-round-list", "type":"function",
                "function":{"name":"list_files", "arguments":"{\"path\":\".\"}"}
            }]);
        }
        Ok((
            message,
            json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}),
        ))
    }
}

async fn across_rounds_fixture(
    parts: [&'static str; 2],
    ending: RedactionEnding,
    streamed: bool,
) -> Fixture {
    fixture_with_codec(
        Fault::None,
        false,
        Some(Arc::new(AcrossRoundsCodec {
            parts,
            ending,
            streamed,
            tool_group: true,
            decoded: AtomicUsize::new(0),
        })),
    )
    .await
}

fn across_rounds_request() -> RunRequest {
    let mut request = request();
    request.tasks[0].tools = true;
    request.tasks[0].max_rounds = 2;
    request
}

async fn assert_redaction_delta_replay(f: &Fixture, run_id: &str, expected: &str) {
    use tower::ServiceExt;
    let events = f.engine.store.events(run_id, 0).unwrap();
    let delta: String = events
        .iter()
        .filter(|e| e.kind == "delta")
        .map(|e| e.data["text"].as_str().unwrap())
        .collect();
    assert_eq!(delta, expected, "persisted delta concatenation");
    let app = crate::server::router(crate::server::App {
        engine: f.engine.clone(),
        token: "fixture-token".into(),
        origin: "http://127.0.0.1:12345".into(),
    });
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/runs/{run_id}/events"))
                .header("host", "127.0.0.1:12345")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let mut wire = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            wire.extend_from_slice(&body.next().await.unwrap().unwrap());
            if String::from_utf8_lossy(&wire)
                .lines()
                .filter(|l| l.starts_with("data:"))
                .count()
                >= events.len()
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let wire = String::from_utf8(wire).unwrap();
    let replay: String = wire
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|l| serde_json::from_str::<Value>(l.trim_start()).unwrap())
        .filter(|v| v["kind"] == "delta")
        .map(|v| v["data"]["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(replay, expected, "SSE delta concatenation");
    assert!(!replay.contains("synthetic") && !replay.contains("opaque123"));
}

#[tokio::test]
async fn stream_redaction_across_completions_keeps_raw_context_until_task_success() {
    for (parts, expected) in [
        (["s", "k-synthetic123"], "[redacted]"),
        (["password=", "opaque123"], "password=[redacted]"),
        (["Bearer ", "opaque123"], "bearer [redacted]"),
        // An inner, separately finished filter would lose the sensitive field
        // name before an outer filter ever sees the '='. One raw state is needed.
        (["sk-password", "=opaque123"], "[redacted]=[redacted]"),
        (["password=\"", "\""], "password=\"\""),
        (["普通🦀", "文本s"], "普通🦀文本s"),
    ] {
        for streamed in [true, false] {
            let f = across_rounds_fixture(parts, RedactionEnding::Clean, streamed).await;
            let run = f.engine.start(across_rounds_request()).await.unwrap();
            let task = settled(&f.engine, &run.tasks[0].id).await;
            assert_eq!(task.status, "completed", "parts: {parts:?}");
            assert_eq!(f.mock.completions.load(Ordering::SeqCst), 2);
            assert!(
                task.messages
                    .iter()
                    .any(|m| { m["role"] == "tool" && m["tool_call_id"] == "audit-round-list" })
            );
            assert_eq!(task.output, expected, "parts: {parts:?}");
            let persisted = serde_json::to_string(&task).unwrap();
            assert!(!persisted.contains("synthetic") && !persisted.contains("opaque123"));
            let assistant_text: String = task
                .messages
                .iter()
                .filter(|m| m["role"] == "assistant")
                .filter_map(|m| m["content"].as_str())
                .collect();
            assert_eq!(
                assistant_text, expected,
                "persisted assistant content: {parts:?}"
            );
            assert_redaction_delta_replay(&f, &run.id, expected).await;
        }
    }
}

#[tokio::test]
async fn stream_redaction_across_completions_failure_and_cancel_drop_tail() {
    for ending in [
        RedactionEnding::FeedError,
        RedactionEnding::FinishError,
        RedactionEnding::Cancel,
    ] {
        for (parts, expected) in [
            (["s", "k-synthetic123 done s"], "[redacted] done "),
            (
                ["sk-password", "=opaque123; done s"],
                "[redacted]=[redacted]; done ",
            ),
        ] {
            let f = across_rounds_fixture(parts, ending, true).await;
            if ending == RedactionEnding::Cancel {
                f.mock.stream_hold.store(true, Ordering::SeqCst);
            }
            let run = f.engine.start(across_rounds_request()).await.unwrap();
            if ending == RedactionEnding::Cancel {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let events = f.engine.store.events(&run.id, 0).unwrap();
                        if events.iter().any(|e| {
                            e.kind == "delta"
                                && e.data["text"].as_str().is_some_and(|s| s.contains("done "))
                        }) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                f.engine.cancel(&run.tasks[0].id).unwrap();
            }
            let task = settled(&f.engine, &run.tasks[0].id).await;
            assert_eq!(
                task.status,
                if ending == RedactionEnding::Cancel {
                    "cancelled"
                } else {
                    "failed"
                }
            );
            assert_eq!(f.mock.completions.load(Ordering::SeqCst), 2);
            assert_eq!(task.output, expected);
            let persisted = serde_json::to_string(&task).unwrap();
            assert!(!persisted.contains("synthetic") && !persisted.contains("opaque123"));
            assert_redaction_delta_replay(&f, &run.id, expected).await;
        }
    }
}

#[tokio::test]
async fn stream_redaction_content_only_and_streamed_messages_use_safe_projection() {
    let raw = "普通🦀 sk-synthetic123; password=opaque123; end s";
    let expected = "普通🦀 [redacted]; password=[redacted]; end s";
    for streamed in [true, false] {
        let f = fixture_with_codec(
            Fault::None,
            false,
            Some(Arc::new(AcrossRoundsCodec {
                parts: [raw, raw],
                ending: RedactionEnding::Clean,
                streamed,
                tool_group: false,
                decoded: AtomicUsize::new(0),
            })),
        )
        .await;
        // Route checks use the local execute wrapper; its clean tail must also
        // reach returned content. Task execution uses the shared-state API.
        let checked = f
            .engine
            .project_route_check(&f.project, "route")
            .await
            .unwrap();
        assert_eq!(checked.result["answer"], expected);
        let run = f.engine.start(request()).await.unwrap();
        let task = settled(&f.engine, &run.tasks[0].id).await;
        assert_eq!(task.status, "completed");
        assert_eq!(task.output, expected);
        assert_eq!(task.messages.last().unwrap()["content"], expected);
        let persisted = serde_json::to_string(&task).unwrap();
        assert!(!persisted.contains("synthetic") && !persisted.contains("opaque123"));
        assert_redaction_delta_replay(&f, &run.id, expected).await;
    }
}

#[tokio::test]
async fn stream_redaction_plain_token_fragment_preserves_output_events_and_sse() {
    // The second piece is token-shaped only when its prior raw context is
    // discarded. A Store that re-redacts each safe delta changes this output.
    for streamed in [true, false] {
        let f = across_rounds_fixture(["foo", "sk-plain"], RedactionEnding::Clean, streamed).await;
        let run = f.engine.start(across_rounds_request()).await.unwrap();
        let task = settled(&f.engine, &run.tasks[0].id).await;
        assert_eq!(task.status, "completed");
        assert_eq!(f.mock.completions.load(Ordering::SeqCst), 2);
        assert_eq!(task.output, "foosk-plain");
        assert_redaction_delta_replay(&f, &run.id, "foosk-plain").await;
        // Stored messages retain the independent safe_task_value policy. It may
        // conservatively redact a token-shaped piece in an individual message;
        // the three concatenated user-output projections must stay identical.
    }
}

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
    stream_hold: Arc<AtomicBool>,
}
async fn models(State(m): State<Mock>) -> Json<Value> {
    m.models.fetch_add(1, Ordering::SeqCst);
    Json(json!({"data":[{"id":"mock"}]}))
}
async fn completion(State(m): State<Mock>, Json(body): Json<Value>) -> axum::response::Response {
    use axum::response::IntoResponse;
    m.completions.fetch_add(1, Ordering::SeqCst);
    m.requests.lock().unwrap().push(body);
    if m.hold.load(Ordering::SeqCst) {
        m.arrived.notify_one();
        m.release.notified().await;
    }
    let wire = "data: {\"choices\":[{\"delta\":{\"content\":\"wire-marker\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\ndata: [DONE]\n\n";
    if m.stream_hold.load(Ordering::SeqCst) {
        let stream = futures_util::stream::once(async move {
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(wire.as_bytes()))
        })
        .chain(futures_util::stream::pending());
        axum::body::Body::from_stream(stream).into_response()
    } else {
        wire.into_response()
    }
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
    fixture_with_codec(fault, builtin, None).await
}
async fn fixture_with_codec(
    fault: Fault,
    builtin: bool,
    codec: Option<Arc<dyn CompletionCodec>>,
) -> Fixture {
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
    let codec = codec.unwrap_or_else(|| {
        Arc::new(TestCodec {
            fault,
            encoded: encoded.clone(),
            decoded: decoded.clone(),
        })
    });
    let runtime = ProviderRuntime::with_codec_for_test(codec).unwrap();
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
        let call_id = signal.lines().nth(1).unwrap();
        let killed_pid = child.id();
        child.kill().unwrap();
        let killed_status = child.wait().unwrap();
        assert!(!killed_status.success());
        eprintln!(
            "p06-recovery {}",
            json!({"phase":phase, "event":"killed-and-waited",
                   "pid":killed_pid, "exit_code":killed_status.code(),
                   "call_id":call_id, "mock_completions":f.mock.completions.load(Ordering::SeqCst)})
        );
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
            eprintln!(
                "p06-recovery {}",
                json!({"phase":phase, "event":"recovery-waited", "run":run,
                       "pid":spawned_pid, "exit_code":0, "call_id":call_id,
                       "network_state":state["network_state"],
                       "held_tokens":state["held_tokens"],
                       "unresolved_count":state["unresolved_count"],
                       "mock_completions":f.mock.completions.load(Ordering::SeqCst)})
            );
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
#[tokio::test]
async fn n18_cross_store_false_true_does_not_revive_old_prepared_completion() {
    use crate::{plugin_management::SetPluginEnabled, runtime_management::RuntimeId};
    let f = fixture(Fault::None).await;
    let other_store = Arc::new(Store::open(&f._dir.path().join("provider-test.db")).unwrap());
    let other = Engine::new(other_store, 2).unwrap();
    let runtime = make_runtime();
    let messages = vec![json!({"role":"user","content":"hi"})];
    let tools = vec![];
    let prepared = runtime
        .prepare_persisted(&f.engine.store, &input(&messages, &tools))
        .unwrap();
    other
        .set_runtime_enabled(
            RuntimeId::CompletionProvider,
            SetPluginEnabled {
                enabled: false,
                expected_revision: 0,
            },
            "p-off",
        )
        .unwrap();
    other
        .set_runtime_enabled(
            RuntimeId::CompletionProvider,
            SetPluginEnabled {
                enabled: true,
                expected_revision: 1,
            },
            "p-on",
        )
        .unwrap();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(f.project.clone()),
    };
    let db = rusqlite::Connection::open(f._dir.path().join("provider-test.db")).unwrap();
    let count = || {
        db.query_row("SELECT count(*) FROM network_calls", [], |r| {
            r.get::<_, u64>(0)
        })
        .unwrap()
    };
    let before = count();
    let budget_before = f.engine.budget_status(&f.project).unwrap();
    assert!(
        runtime
            .acquire_persisted(prepared, |revision| f
                .engine
                .store
                .network_admit_with_runtime_revision(
                    &subject,
                    NetworkPurpose::Completion,
                    None,
                    Some(revision)
                ))
            .is_err()
    );
    assert_eq!(count(), before);
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 0);
    let budget_after = f.engine.budget_status(&f.project).unwrap();
    assert_eq!(budget_after.held_tokens, budget_before.held_tokens);
    assert_eq!(
        budget_after.unresolved_count,
        budget_before.unresolved_count
    );
    assert_eq!(budget_after.spent_tokens, budget_before.spent_tokens);
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
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 1);
    assert_eq!(count(), before + 2);
    let settled = f.engine.budget_status(&f.project).unwrap();
    assert_eq!(settled.held_tokens, budget_before.held_tokens);
    assert_eq!(settled.unresolved_count, budget_before.unresolved_count);
    assert_eq!(settled.spent_tokens, budget_before.spent_tokens + 5);
}

#[tokio::test]
async fn n18_provider_admission_and_remote_disable_have_a_transaction_order() {
    use crate::{
        engine::runtime_barrier, plugin_management::SetPluginEnabled, runtime_management::RuntimeId,
    };
    use std::sync::mpsc;
    for admission_first in [true, false] {
        let f = fixture(Fault::None).await;
        let _probe_guard = crate::runtime_management_tests::barrier_test_guard();
        let path = f._dir.path().join("provider-test.db");
        let store = f.engine.store.clone();
        let other = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        let key: String = db
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name='main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let runtime = Arc::new(make_runtime());
        let messages = vec![json!({"role":"user","content":"hi"})];
        let tools = vec![];
        let prepared = runtime
            .prepare_persisted(&store, &input(&messages, &tools))
            .unwrap();
        let subject = NetworkSubject {
            kind: NetworkSubjectKind::Route,
            id: "route".into(),
            project_id: Some(f.project.clone()),
        };
        let before: i64 = db
            .query_row("SELECT count(*) FROM network_calls", [], |r| r.get(0))
            .unwrap();
        let budget_before = f.engine.budget_status(&f.project).unwrap();
        let phase = if admission_first {
            "runtime_provider_admit_before_commit"
        } else {
            "runtime_intent_before_commit"
        };
        let barrier = runtime_barrier::arm(key, phase);
        let (started_tx, started_rx) = mpsc::channel();
        if admission_first {
            let r = runtime.clone();
            let s = store.clone();
            let admit = std::thread::spawn(move || {
                r.acquire_persisted(prepared, |revision| {
                    s.network_admit_with_runtime_revision(
                        &subject,
                        NetworkPurpose::Completion,
                        None,
                        Some(revision),
                    )
                })
            });
            let arrived = barrier.wait_arrived(Duration::from_secs(5));
            if !arrived {
                barrier.release();
                let _ = admit.join();
                panic!("provider admission barrier not reached");
            }
            let disable = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                other.set_runtime_enabled(
                    RuntimeId::CompletionProvider,
                    SetPluginEnabled {
                        enabled: false,
                        expected_revision: 0,
                    },
                    "provider-off",
                )
            });
            let started = started_rx.recv_timeout(Duration::from_secs(5));
            barrier.release();
            started.expect("disable worker did not start");
            let (lease, (_permit, receipt)) = admit.join().unwrap().unwrap();
            disable.join().unwrap().unwrap();
            assert_eq!(receipt.state, CallState::Claimed);
            let budget_call = f.engine.budget_call(&f.project, &receipt.call_id).unwrap();
            assert_eq!(
                budget_call.state,
                Some(crate::budget::BudgetCallState::Claimed)
            );
            assert_eq!(budget_call.reserved_tokens, Some(100));
            assert_eq!(budget_call.call, receipt);
            assert_eq!(runtime.status_for_test().unwrap().in_flight, 1);
            assert_eq!(
                db.query_row("SELECT count(*) FROM network_calls", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                before + 1
            );
            drop(lease);
        } else {
            let disable = std::thread::spawn(move || {
                other.set_runtime_enabled(
                    RuntimeId::CompletionProvider,
                    SetPluginEnabled {
                        enabled: false,
                        expected_revision: 0,
                    },
                    "provider-off",
                )
            });
            let arrived = barrier.wait_arrived(Duration::from_secs(5));
            if !arrived {
                barrier.release();
                let _ = disable.join();
                panic!("provider disable barrier not reached");
            }
            let r = runtime.clone();
            let s = store.clone();
            let admit = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                r.acquire_persisted(prepared, |revision| {
                    s.network_admit_with_runtime_revision(
                        &subject,
                        NetworkPurpose::Completion,
                        None,
                        Some(revision),
                    )
                })
            });
            let started = started_rx.recv_timeout(Duration::from_secs(5));
            barrier.release();
            started.expect("admission worker did not start");
            disable.join().unwrap().unwrap();
            assert!(admit.join().unwrap().is_err());
            assert_eq!(runtime.status_for_test().unwrap().in_flight, 0);
            assert_eq!(
                db.query_row("SELECT count(*) FROM network_calls", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                before
            );
        }
        let budget_after = f.engine.budget_status(&f.project).unwrap();
        assert_eq!(
            budget_after.held_tokens,
            budget_before.held_tokens + if admission_first { 100 } else { 0 }
        );
        assert_eq!(
            budget_after.claimed_count,
            budget_before.claimed_count + if admission_first { 1 } else { 0 }
        );
        assert_eq!(
            budget_after.unresolved_count,
            budget_before.unresolved_count
        );
        assert_eq!(f.mock.completions.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn n18_persisted_rejection_drops_reentrant_decoder_outside_lock() {
    use crate::{plugin_management::SetPluginEnabled, runtime_management::RuntimeId};
    let f = fixture(Fault::None).await;
    let other = Engine::new(
        Arc::new(Store::open(&f._dir.path().join("provider-test.db")).unwrap()),
        2,
    )
    .unwrap();
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
    let messages = vec![json!({"role":"user","content":"hi"})];
    let tools = vec![];
    let prepared = runtime
        .prepare_persisted(&f.engine.store, &input(&messages, &tools))
        .unwrap();
    other
        .set_runtime_enabled(
            RuntimeId::CompletionProvider,
            SetPluginEnabled {
                enabled: false,
                expected_revision: 0,
            },
            "provider-off",
        )
        .unwrap();
    let store = f.engine.store.clone();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(f.project.clone()),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = runtime.acquire_persisted(prepared, |revision| {
            store.network_admit_with_runtime_revision(
                &subject,
                NetworkPurpose::Completion,
                None,
                Some(revision),
            )
        });
        tx.send(result.is_err()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("persisted rejection dropped decoder while holding runtime lock")
    );
    worker.join().unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 0);
    let budget = f.engine.budget_status(&f.project).unwrap();
    assert_eq!(budget.held_tokens, 0);
    assert_eq!(budget.unresolved_count, 0);
}
#[tokio::test]
async fn n18_permit_failure_after_claim_compensates_without_completion_send() {
    let f = fixture(Fault::None).await;
    let path = f._dir.path().join("provider-test.db");
    let db = rusqlite::Connection::open(path).unwrap();
    let key: String = db
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    crate::engine::runtime_barrier::fail_next_permit(key);
    assert!(
        f.engine
            .project_route_check(&f.project, "route")
            .await
            .is_err()
    );
    assert_eq!(f.mock.models.load(Ordering::SeqCst), 1);
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 0);
    let receipts: Vec<CallReceipt> = db
        .prepare("SELECT receipt FROM network_calls ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].purpose, NetworkPurpose::Models);
    assert_eq!(receipts[0].state, CallState::Finished);
    assert_eq!(receipts[1].purpose, NetworkPurpose::Completion);
    assert_eq!(receipts[1].state, CallState::NotSent);
    let budget = f.engine.budget_status(&f.project).unwrap();
    assert_eq!(budget.held_tokens, 0);
    assert_eq!(budget.unresolved_count, 0);
    assert_eq!(budget.spent_tokens, 0);
}
struct N18CountedProvider {
    starts: Arc<AtomicUsize>,
}
impl crate::plugin_host::BuiltinPlugin for N18CountedProvider {
    fn activate(
        &mut self,
        ctx: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
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
        Ok(())
    }
}
fn n18_counted_provider_host(starts: Arc<AtomicUsize>) -> crate::plugin_host::PluginHost {
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
        Box::new(N18CountedProvider {
            starts: starts.clone(),
        })
    });
    host
}
#[test]
fn n18_provider_false_start_has_no_activation_or_effects() {
    let starts = Arc::new(AtomicUsize::new(0));
    let runtime = ProviderRuntime::from_host_with_state_for_test(
        n18_counted_provider_host(starts.clone()),
        false,
    )
    .unwrap();
    let state = runtime.status_for_test().unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(!state.active && !state.admission_open);
    assert_eq!(state.effects, 0);
    assert_eq!(state.generation, 1);
    drop(runtime);
    let enabled = ProviderRuntime::from_host_with_state_for_test(
        n18_counted_provider_host(starts.clone()),
        true,
    )
    .unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(enabled.status_for_test().unwrap().active);
}
#[tokio::test]
async fn n18_provider_store_failure_never_creates_claim_or_uses_cached_enabled() {
    let f = fixture(Fault::None).await;
    let runtime = make_runtime();
    let messages = vec![json!({"role":"user","content":"hi"})];
    let tools = vec![];
    let prepared = runtime
        .prepare_persisted(&f.engine.store, &input(&messages, &tools))
        .unwrap();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(f.project.clone()),
    };
    let db = rusqlite::Connection::open(f._dir.path().join("provider-test.db")).unwrap();
    let before: i64 = db
        .query_row("SELECT count(*) FROM network_calls", [], |r| r.get(0))
        .unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE plugin_desired;")
        .unwrap();
    assert!(
        runtime
            .acquire_persisted(prepared, |revision| f
                .engine
                .store
                .network_admit_with_runtime_revision(
                    &subject,
                    NetworkPurpose::Completion,
                    None,
                    Some(revision)
                ))
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM network_calls", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    let budget = f.engine.budget_status(&f.project).unwrap();
    assert_eq!(budget.held_tokens, 0);
    assert_eq!(budget.unresolved_count, 0);
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn n18_same_true_revision_change_invalidates_old_provider_preparation() {
    use crate::{plugin_management::SetPluginEnabled, runtime_management::RuntimeId};
    let f = fixture(Fault::None).await;
    let other = Engine::new(
        Arc::new(Store::open(&f._dir.path().join("provider-test.db")).unwrap()),
        2,
    )
    .unwrap();
    let runtime = make_runtime();
    let messages = vec![json!({"role":"user","content":"hi"})];
    let tools = vec![];
    let old = runtime
        .prepare_persisted(&f.engine.store, &input(&messages, &tools))
        .unwrap();
    let receipt = other
        .set_runtime_enabled(
            RuntimeId::CompletionProvider,
            SetPluginEnabled {
                enabled: true,
                expected_revision: 0,
            },
            "same-true",
        )
        .unwrap();
    assert_eq!(receipt.receipt.revision, 1);
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(f.project.clone()),
    };
    let db = rusqlite::Connection::open(f._dir.path().join("provider-test.db")).unwrap();
    let count = || {
        db.query_row("SELECT count(*) FROM network_calls", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };
    let before = count();
    let budget_before = f.engine.budget_status(&f.project).unwrap();
    assert!(
        runtime
            .acquire_persisted(old, |revision| f
                .engine
                .store
                .network_admit_with_runtime_revision(
                    &subject,
                    NetworkPurpose::Completion,
                    None,
                    Some(revision)
                ))
            .is_err()
    );
    assert_eq!(count(), before);
    let budget_after = f.engine.budget_status(&f.project).unwrap();
    assert_eq!(budget_after.held_tokens, budget_before.held_tokens);
    assert_eq!(
        budget_after.unresolved_count,
        budget_before.unresolved_count
    );
    assert_eq!(budget_after.spent_tokens, budget_before.spent_tokens);
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 0);
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
    assert_eq!(count(), before + 2);
    assert_eq!(f.mock.completions.load(Ordering::SeqCst), 1);
    let settled = f.engine.budget_status(&f.project).unwrap();
    assert_eq!(settled.held_tokens, budget_before.held_tokens);
    assert_eq!(settled.unresolved_count, budget_before.unresolved_count);
    assert_eq!(settled.spent_tokens, budget_before.spent_tokens + 5);
}
struct N18FailProviderStop {
    calls: Arc<AtomicUsize>,
    panic_on_stop: bool,
}
impl crate::plugin_host::BuiltinPlugin for N18FailProviderStop {
    fn activate(
        &mut self,
        ctx: &mut crate::plugin_host::PluginContext,
    ) -> Result<(), crate::plugin_host::PluginError> {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(!self.panic_on_stop, "n18 provider stop panic");
        Err(crate::plugin_host::PluginError::new(
            "n18 provider stop error",
        ))
    }
}
#[test]
fn n18_durable_provider_stop_failure_returns_receipt_and_replay_does_not_retry() {
    use crate::{
        plugin_catalog::ScopeKey,
        plugin_management::SetPluginEnabled,
        runtime_management::{RuntimeId, RuntimeManagementError},
    };
    for panic_on_stop in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider.db");
        let calls = Arc::new(AtomicUsize::new(0));
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
        let counted = calls.clone();
        host.with_factory("builtin.provider-xpeach", move || {
            Box::new(N18FailProviderStop {
                calls: counted.clone(),
                panic_on_stop,
            })
        });
        let runtime = ProviderRuntime::from_host_for_test(host).unwrap();
        let engine = Engine::new_with_provider_runtime_for_test(
            Arc::new(Store::open(&path).unwrap()),
            2,
            runtime,
        )
        .unwrap();
        let req = || SetPluginEnabled {
            enabled: false,
            expected_revision: 0,
        };
        let original = engine
            .set_runtime_enabled(RuntimeId::CompletionProvider, req(), "failing-stop")
            .unwrap();
        assert!(!original.replayed);
        assert_eq!(original.receipt.revision, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let status = serde_json::to_value(&original.current).unwrap();
        assert_eq!(status["desired"]["enabled"], false);
        assert_eq!(status["converged"], false);
        assert_eq!(status["runtime"]["admission_open"], false);
        assert_eq!(status["runtime"]["lifecycle_failed"], true);
        let replay = engine
            .set_runtime_enabled(RuntimeId::CompletionProvider, req(), "failing-stop")
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.receipt, original.receipt);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            engine.set_runtime_enabled(
                RuntimeId::CompletionProvider,
                SetPluginEnabled {
                    enabled: true,
                    expected_revision: 1
                },
                "enable-after-failure"
            ),
            Err(RuntimeManagementError::Unavailable)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let db = rusqlite::Connection::open(&path).unwrap();
        let persisted: (bool, i64) = db
            .query_row(
                "SELECT enabled,revision FROM plugin_desired WHERE plugin_id=?1",
                [original.receipt.plugin_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(persisted, (false, 1));
        let persisted_receipt:(String,bool,i64,i64)=db.query_row("SELECT plugin_id,enabled,expected_revision,result_revision FROM plugin_management_requests WHERE key='failing-stop'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(
            persisted_receipt,
            (original.receipt.plugin_id.clone(), false, 0, 1)
        );
        let after = serde_json::to_value(
            engine
                .runtime_component_status(RuntimeId::CompletionProvider)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(after["converged"], false);

        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM plugin_management_requests WHERE key='enable-after-failure'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
