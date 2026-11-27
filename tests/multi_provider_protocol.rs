#[allow(dead_code)]
#[path = "support/network_fixture.rs"]
mod network_fixture;

use axum::{
    Json, Router,
    body::Body,
    extract::{RawQuery, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use coda::{
    domain::{Route, Run, RunRequest, SessionKind, Settings, TaskSpec},
    engine::Engine,
    provider::{ProviderProtocol, encode_protocol_body},
    provider_transport::{NetworkPurpose, validate_target},
    provider_usage::{TokenUsage, UsageObservation, classify_usage},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};

const ANTHROPIC_KEY: &str = "sk-ant-synthetic-test-key";
const GEMINI_KEY: &str = "AIzaSySyntheticGeminiTestKey";
const OPENAI_KEY: &str = "sk-openai-synthetic-test-key";

#[derive(Clone, Default)]
struct RecordedCalls {
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn wait_for_run(engine: &Engine, run_id: &str) -> Run {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let run = engine.store.run(run_id).unwrap();
            if run
                .tasks
                .iter()
                .all(|t| !matches!(t.status.as_str(), "queued" | "running"))
                && !engine.is_busy()
            {
                break run;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("timed out waiting for multi-provider task to finish")
}

fn assert_persisted_streams(
    db_path: &std::path::Path,
    run_id: &str,
    expected_reasoning: &str,
    expected_output: &str,
) {
    // Read through a fresh Store after Engine shutdown, not a decoder callback or CLI output.
    let store = Store::open(db_path).unwrap();
    let run = store.run(run_id).unwrap();
    let task = &run.tasks[0];
    assert_eq!(task.status, "completed");
    assert_eq!(task.output, expected_output);

    let events = store.events(run_id, 0).unwrap();
    let mut reasoning = String::new();
    let mut output = String::new();
    for event in &events {
        let target = match event.kind.as_str() {
            "reasoning_delta" => &mut reasoning,
            "delta" => &mut output,
            _ => continue,
        };
        assert_eq!(event.task_id, task.id);
        let text = event.data["text"].as_str().expect("data.text must be text");
        assert!(!text.is_empty());
        assert_eq!(event.data, json!({"text": text}));
        target.push_str(text);
    }
    assert!(!reasoning.is_empty(), "missing persisted reasoning_delta");
    assert_eq!(reasoning, expected_reasoning);
    assert_eq!(output, expected_output);
}

async fn openai_reasoning(
    State(state): State<RecordedCalls>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(
        headers["authorization"].to_str().unwrap(),
        format!("Bearer {OPENAI_KEY}")
    );
    assert_eq!(body["model"], "openai-reasoning-test");
    assert_eq!(body["stream"], true);
    state.requests.lock().unwrap().push(body);

    let (key_prefix, key_suffix) = OPENAI_KEY.split_at(12);
    let deltas = [
        json!({"reasoning_content": "Plan: use sk-learn; Bearer auth; let password = \"example\"; key="}),
        json!({"reasoning_content": key_prefix}),
        json!({"reasoning": key_suffix}),
        json!({"reasoning": format!("; repeat={OPENAI_KEY}; tail sk-openai-")}),
        json!({"content": "Answer: 42."}),
    ];
    let stream = async_stream::stream! {
        for delta in deltas {
            let frame = format!("data: {}\n\n", json!({"choices": [{"delta": delta}]}));
            yield Ok::<_, Infallible>(frame.into_bytes());
        }
        let finish = json!({"choices": [{"delta": {}, "finish_reason": "stop"}]});
        yield Ok::<_, Infallible>(format!("data: {finish}\n\ndata: [DONE]\n\n").into_bytes());
    };
    (
        [("content-type", "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

#[tokio::test]
async fn openai_reasoning_events_persist_scrub_known_key_and_keep_body_separate() {
    let state = RecordedCalls::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(openai_reasoning))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("openai.db");
    let store = Arc::new(Store::open(&db_path).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into(),
            max_concurrency: 1,
            routes: vec![Route {
                id: "openai".into(),
                name: "OpenAI Native".into(),
                base_url: format!("http://{addr}/v1"),
                model: "openai-reasoning-test".into(),
                protocol: None,
                max_tokens: 1024,
                parallel_limit: 1,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route::openai", OPENAI_KEY).unwrap();
    network_fixture::authorize_routes(&store);
    let engine = Engine::new(store, 1).unwrap();
    let started = engine
        .start(RunRequest {
            title: "OpenAI persisted reasoning test".into(),
            kind: SessionKind::Chat,
            tasks: vec![TaskSpec {
                name: "openai-worker".into(),
                role: "protocol verifier".into(),
                route_id: "openai".into(),
                prompt: "Think, then answer.".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: false,
                allow_commands: false,
                max_rounds: 1,
            }],
        })
        .await
        .unwrap();
    let finished = wait_for_run(&engine, &started.id).await;
    assert_eq!(finished.tasks[0].status, "completed");
    assert_eq!(state.requests.lock().unwrap().len(), 1);
    assert!(engine.shutdown(Duration::from_secs(3)).await.settled());
    drop(engine);

    assert_persisted_streams(
        &db_path,
        &started.id,
        "Plan: use sk-learn; Bearer auth; let password = \"example\"; key=[redacted]; repeat=[redacted]; tail sk-openai-",
        "Answer: 42.",
    );
    server_handle.abort();
}

async fn anthropic_models(headers: HeaderMap) -> Json<Value> {
    assert_eq!(
        headers.get("x-api-key").and_then(|v| v.to_str().ok()),
        Some(ANTHROPIC_KEY)
    );
    assert_eq!(
        headers
            .get("anthropic-version")
            .and_then(|v| v.to_str().ok()),
        Some("2023-06-01")
    );
    assert!(headers.get("authorization").is_none());
    Json(json!({
        "data": [
            {"id": "claude-sonnet-4-20250514", "type": "model"}
        ]
    }))
}

async fn anthropic_messages(
    State(state): State<RecordedCalls>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(
        headers.get("x-api-key").and_then(|v| v.to_str().ok()),
        Some(ANTHROPIC_KEY)
    );
    assert_eq!(
        headers
            .get("anthropic-version")
            .and_then(|v| v.to_str().ok()),
        Some("2023-06-01")
    );
    assert!(headers.get("authorization").is_none());
    assert_eq!(body["model"], "claude-sonnet-4-20250514");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 1024);
    assert!(body["system"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "system")
    );
    assert!(
        body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "read_file" && t["input_schema"]["type"] == "object")
    );

    let round = {
        let mut guard = state.requests.lock().unwrap();
        guard.push(body);
        guard.len()
    };

    let frames: Vec<String> = if round == 1 {
        vec![
            format!(
                "event: message_start\ndata: {}\n\n",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_01",
                        "role": "assistant",
                        "usage": {"input_tokens": 120, "output_tokens": 1}
                    }
                })
            ),
            format!(
                "event: content_block_start\ndata: {}\n\n",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "thinking", "thinking": ""}
                })
            ),
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "thinking_delta", "thinking": "Inspecting notes.txt first."}
                })
            ),
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "signature_delta", "signature": "sig-ant-001"}
                })
            ),
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"
                .to_owned(),
            format!(
                "event: content_block_start\ndata: {}\n\n",
                json!({
                    "type": "content_block_start",
                    "index": 1,
                    "content_block": {"type": "text", "text": ""}
                })
            ),
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                json!({
                    "type": "content_block_delta",
                    "index": 1,
                    "delta": {"type": "text_delta", "text": "Checking notes... "}
                })
            ),
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n"
                .to_owned(),
            format!(
                "event: content_block_start\ndata: {}\n\n",
                json!({
                    "type": "content_block_start",
                    "index": 2,
                    "content_block": {
                        "type": "tool_use",
                        "id": "toolu_notes_1",
                        "name": "read_file",
                        "input": {}
                    }
                })
            ),
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                json!({
                    "type": "content_block_delta",
                    "index": 2,
                    "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}
                })
            ),
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                json!({
                    "type": "content_block_delta",
                    "index": 2,
                    "delta": {"type": "input_json_delta", "partial_json": "\"notes.txt\"}"}
                })
            ),
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n"
                .to_owned(),
            format!(
                "event: message_delta\ndata: {}\n\n",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "tool_use"},
                    "usage": {"output_tokens": 30}
                })
            ),
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_owned(),
        ]
    } else {
        vec![
            format!(
                "event: message_start\ndata: {}\n\n",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_02",
                        "role": "assistant",
                        "usage": {"input_tokens": 180, "output_tokens": 1}
                    }
                })
            ),
            format!(
                "event: content_block_start\ndata: {}\n\n",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "text", "text": ""}
                })
            ),
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": "Secret is anthropic-payload-42."}
                })
            ),
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"
                .to_owned(),
            format!(
                "event: message_delta\ndata: {}\n\n",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn"},
                    "usage": {"output_tokens": 20}
                })
            ),
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_owned(),
        ]
    };

    let stream = async_stream::stream! {
        for frame in frames {
            yield Ok::<_, Infallible>(frame.into_bytes());
        }
    };
    (
        [("content-type", "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

#[tokio::test]
async fn anthropic_native_messages_tool_roundtrip_and_usage() {
    let state = RecordedCalls::default();
    let app = Router::new()
        .route("/anthropic/v1/models", get(anthropic_models))
        .route("/anthropic/v1/messages", post(anthropic_messages))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "anthropic-payload-42\n").unwrap();

    let store = Arc::new(Store::open(&dir.path().join("anthropic.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "anthropic".into(),
                name: "Anthropic Native".into(),
                base_url: format!("http://{addr}/anthropic/v1"),
                model: "claude-sonnet-4-20250514".into(),
                protocol: None,
                max_tokens: 1024,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route::anthropic", ANTHROPIC_KEY).unwrap();
    network_fixture::authorize_routes(&store);

    let engine = Engine::new(store, 2).unwrap();
    let models = engine.route_models("anthropic").await.unwrap();
    assert_eq!(models.result, vec!["claude-sonnet-4-20250514".to_owned()]);

    let started = engine
        .start(RunRequest {
            title: "Anthropic tool test".into(),
            kind: SessionKind::Chat,
            tasks: vec![TaskSpec {
                name: "claude-worker".into(),
                role: "protocol verifier".into(),
                route_id: "anthropic".into(),
                prompt: "Read notes.txt and report its payload.".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: true,
                allow_commands: false,
                max_rounds: 4,
            }],
        })
        .await
        .unwrap();

    let finished = wait_for_run(&engine, &started.id).await;
    let task = &finished.tasks[0];
    assert_eq!(task.status, "completed", "task error: {:?}", task.error);
    assert_eq!(
        task.output,
        "Checking notes... Secret is anthropic-payload-42."
    );
    assert_eq!(
        classify_usage(&task.usage),
        UsageObservation::Valid(TokenUsage {
            prompt_tokens: 300,
            completion_tokens: 50,
            total_tokens: 350,
        })
    );

    let requests = state.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let round2_msgs = requests[1]["messages"].as_array().unwrap();
    assert_eq!(round2_msgs.len(), 3);
    assert_eq!(round2_msgs[1]["role"], "assistant");
    let assistant_blocks = round2_msgs[1]["content"].as_array().unwrap();
    assert_eq!(assistant_blocks[0]["type"], "thinking");
    assert_eq!(
        assistant_blocks[0]["thinking"],
        "Inspecting notes.txt first."
    );
    assert_eq!(assistant_blocks[0]["signature"], "sig-ant-001");
    assert_eq!(assistant_blocks[1]["type"], "text");
    assert_eq!(assistant_blocks[1]["text"], "Checking notes... ");
    assert_eq!(assistant_blocks[2]["type"], "tool_use");
    assert_eq!(assistant_blocks[2]["id"], "toolu_notes_1");
    assert_eq!(assistant_blocks[2]["name"], "read_file");
    assert_eq!(assistant_blocks[2]["input"], json!({"path": "notes.txt"}));

    assert_eq!(round2_msgs[2]["role"], "user");
    let user_blocks = round2_msgs[2]["content"].as_array().unwrap();
    assert_eq!(user_blocks[0]["type"], "tool_result");
    assert_eq!(user_blocks[0]["tool_use_id"], "toolu_notes_1");
    assert!(
        user_blocks[0]["content"]
            .as_str()
            .unwrap()
            .contains("anthropic-payload-42")
    );

    assert!(engine.shutdown(Duration::from_secs(3)).await.settled());
    drop(engine);
    assert_persisted_streams(
        &dir.path().join("anthropic.db"),
        &started.id,
        "Inspecting notes.txt first.",
        "Checking notes... Secret is anthropic-payload-42.",
    );
    server_handle.abort();
}

async fn gemini_models(headers: HeaderMap) -> Json<Value> {
    assert_eq!(
        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
        Some(GEMINI_KEY)
    );
    assert!(headers.get("authorization").is_none());
    Json(json!({
        "models": [
            {"name": "models/gemini-2.5-pro", "displayName": "Gemini 2.5 Pro"}
        ]
    }))
}

async fn gemini_stream_generate_content(
    State(state): State<RecordedCalls>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(query.as_deref(), Some("alt=sse"));
    assert_eq!(
        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
        Some(GEMINI_KEY)
    );
    assert!(headers.get("authorization").is_none());
    assert!(body.get("model").is_none());
    assert!(
        body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 2048);

    let decls = body["tools"][0]["functionDeclarations"].as_array().unwrap();
    let read_decl = decls.iter().find(|d| d["name"] == "read_file").unwrap();
    assert!(
        read_decl["parameters"]
            .get("additionalProperties")
            .is_none()
    );

    let round = {
        let mut guard = state.requests.lock().unwrap();
        guard.push(body);
        guard.len()
    };

    let frames: Vec<String> = if round == 1 {
        vec![
            format!(
                "data: {}\r\n\r\n",
                json!({
                    "candidates": [{
                        "content": {
                            "role": "model",
                            "parts": [
                                {"thought": true, "text": "Need to inspect config.json"},
                                {"text": "Reading config... "}
                            ]
                        }
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 90,
                        "candidatesTokenCount": 4,
                        "totalTokenCount": 100
                    }
                })
            ),
            format!(
                "data: {}\r\n\r\n",
                json!({
                    "candidates": [{
                        "content": {
                            "role": "model",
                            "parts": [{
                                "functionCall": {
                                    "name": "read_file",
                                    "args": {"path": "config.json"}
                                }
                            }]
                        },
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 90,
                        "candidatesTokenCount": 12,
                        "totalTokenCount": 110
                    }
                })
            ),
        ]
    } else {
        vec![
            format!(
                "data: {}\n\n",
                json!({
                    "candidates": [{
                        "content": {
                            "role": "model",
                            "parts": [{"text": "Port is "}]
                        }
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 140,
                        "candidatesTokenCount": 3,
                        "totalTokenCount": 143
                    }
                })
            ),
            format!(
                "data: {}\n\n",
                json!({
                    "candidates": [{
                        "content": {
                            "role": "model",
                            "parts": [{"text": "8080."}]
                        },
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 140,
                        "candidatesTokenCount": 8,
                        "totalTokenCount": 148
                    }
                })
            ),
        ]
    };

    let stream = async_stream::stream! {
        for frame in frames {
            yield Ok::<_, Infallible>(frame.into_bytes());
        }
    };
    (
        [("content-type", "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

#[tokio::test]
async fn gemini_native_stream_generate_content_tool_roundtrip_and_usage() {
    let state = RecordedCalls::default();
    let app = Router::new()
        .route("/v1beta/models", get(gemini_models))
        .route(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent",
            post(gemini_stream_generate_content),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.json"), "{\"port\":8080}\n").unwrap();

    let store = Arc::new(Store::open(&dir.path().join("gemini.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "gemini".into(),
                name: "Google Gemini Native".into(),
                base_url: format!("http://{addr}/v1beta"),
                model: "gemini-2.5-pro".into(),
                protocol: None,
                max_tokens: 2048,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route::gemini", GEMINI_KEY).unwrap();
    network_fixture::authorize_routes(&store);

    let engine = Engine::new(store, 2).unwrap();
    let models = engine.route_models("gemini").await.unwrap();
    assert_eq!(models.result, vec!["gemini-2.5-pro".to_owned()]);

    let started = engine
        .start(RunRequest {
            title: "Gemini tool test".into(),
            kind: SessionKind::Chat,
            tasks: vec![TaskSpec {
                name: "gemini-worker".into(),
                role: "protocol verifier".into(),
                route_id: "gemini".into(),
                prompt: "Read config.json and report the port.".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: true,
                allow_commands: false,
                max_rounds: 4,
            }],
        })
        .await
        .unwrap();

    let finished = wait_for_run(&engine, &started.id).await;
    let task = &finished.tasks[0];
    assert_eq!(task.status, "completed", "task error: {:?}", task.error);
    assert_eq!(task.output, "Reading config... Port is 8080.");
    assert_eq!(
        classify_usage(&task.usage),
        UsageObservation::Valid(TokenUsage {
            prompt_tokens: 230,
            completion_tokens: 20,
            total_tokens: 258,
        })
    );

    let requests = state.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    let contents = requests[1]["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 3);
    assert_eq!(contents[1]["role"], "model");
    let model_parts = contents[1]["parts"].as_array().unwrap();
    assert_eq!(model_parts[0], json!({"text": "Reading config... "}));
    assert_eq!(
        model_parts[1],
        json!({
            "functionCall": {
                "name": "read_file",
                "args": {"path": "config.json"}
            }
        })
    );

    assert_eq!(contents[2]["role"], "user");
    let fn_resp = &contents[2]["parts"][0]["functionResponse"];
    assert_eq!(fn_resp["name"], "read_file");
    assert!(fn_resp["response"].is_object());
    assert!(fn_resp["response"].to_string().contains("8080"));

    assert!(engine.shutdown(Duration::from_secs(3)).await.settled());
    drop(engine);
    assert_persisted_streams(
        &dir.path().join("gemini.db"),
        &started.id,
        "Need to inspect config.json",
        "Reading config... Port is 8080.",
    );
    server_handle.abort();
}

#[test]
fn protocol_detection_and_grouped_parallel_tool_results() {
    assert_eq!(
        ProviderProtocol::detect("https://api.anthropic.com"),
        ProviderProtocol::Anthropic
    );
    assert_eq!(
        ProviderProtocol::detect("https://api.anthropic.com/v1"),
        ProviderProtocol::Anthropic
    );
    assert_eq!(
        ProviderProtocol::detect("http://127.0.0.1:9000/anthropic/v1"),
        ProviderProtocol::Anthropic
    );
    assert_eq!(
        ProviderProtocol::detect("https://generativelanguage.googleapis.com/v1beta"),
        ProviderProtocol::Gemini
    );
    assert_eq!(
        ProviderProtocol::detect("http://127.0.0.1:9000/gemini"),
        ProviderProtocol::Gemini
    );
    assert_eq!(
        ProviderProtocol::detect("https://api.openai.com/v1"),
        ProviderProtocol::OpenAi
    );
    assert_eq!(
        ProviderProtocol::detect("http://127.0.0.1:9000/v1"),
        ProviderProtocol::OpenAi
    );

    let ant_target =
        validate_target("https://api.anthropic.com/v1", NetworkPurpose::Completion).unwrap();
    assert_eq!(
        ant_target.endpoint(),
        "https://api.anthropic.com:443/v1/messages"
    );

    let openai_body = json!({
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 512,
        "messages": [
            {"role": "system", "content": "Be concise."},
            {"role": "user", "content": "Read both files."},
            {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {
                        "id": "call_a",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\":\"a.txt\"}"}
                    },
                    {
                        "id": "call_b",
                        "type": "function",
                        "function": {"name": "list_files", "arguments": "{\"path\":\".\"}"}
                    }
                ]
            },
            {"role": "tool", "tool_call_id": "call_a", "content": "{\"lines\":\"a\"}"},
            {"role": "tool", "tool_call_id": "call_b", "content": "{\"entries\":[\"a.txt\"]}"}
        ]
    });

    let anthropic_wire = encode_protocol_body(ProviderProtocol::Anthropic, &openai_body).unwrap();
    assert_eq!(anthropic_wire["system"], "Be concise.");
    let ant_msgs = anthropic_wire["messages"].as_array().unwrap();
    assert_eq!(ant_msgs.len(), 3);
    assert_eq!(ant_msgs[2]["role"], "user");
    let ant_tool_results = ant_msgs[2]["content"].as_array().unwrap();
    assert_eq!(ant_tool_results.len(), 2);
    assert_eq!(ant_tool_results[0]["tool_use_id"], "call_a");
    assert_eq!(ant_tool_results[1]["tool_use_id"], "call_b");

    let gemini_wire = encode_protocol_body(ProviderProtocol::Gemini, &openai_body).unwrap();
    let gem_contents = gemini_wire["contents"].as_array().unwrap();
    assert_eq!(gem_contents.len(), 3);
    assert_eq!(gem_contents[2]["role"], "user");
    let gem_parts = gem_contents[2]["parts"].as_array().unwrap();
    assert_eq!(gem_parts.len(), 2);
    assert_eq!(gem_parts[0]["functionResponse"]["name"], "read_file");
    assert_eq!(gem_parts[1]["functionResponse"]["name"], "list_files");
}

#[test]
fn multi_provider_decoders_stream_live_reasoning_separately_from_text() {
    use coda::provider::{AnthropicDecoder, Decoder, GeminiDecoder};
    use std::cell::RefCell;

    let mut openai = Decoder::default();
    let openai_events = RefCell::new(Vec::new());
    let openai_sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"plan A -> \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"plan B\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"final answer\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    openai
        .feed_with_reasoning(
            openai_sse.as_bytes(),
            &mut |t| {
                openai_events.borrow_mut().push(("text", t.to_owned()));
                Ok(())
            },
            &mut |r| {
                openai_events.borrow_mut().push(("reasoning", r.to_owned()));
                Ok(())
            },
        )
        .unwrap();
    let openai_msg = openai.finish(&mut |_| Ok(())).unwrap();
    assert_eq!(
        openai_events.into_inner(),
        vec![
            ("reasoning", "plan A -> ".to_owned()),
            ("reasoning", "plan B".to_owned()),
            ("text", "final answer".to_owned()),
        ]
    );
    assert_eq!(openai_msg["content"], "final answer");
    assert_eq!(openai_msg["reasoning_content"], "plan A -> plan B");

    let mut anthropic = AnthropicDecoder::default();
    let anthropic_events = RefCell::new(Vec::new());
    let anthropic_sse = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"checking tree...\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"done.\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    anthropic
        .feed_with_reasoning(
            anthropic_sse.as_bytes(),
            &mut |t| {
                anthropic_events.borrow_mut().push(("text", t.to_owned()));
                Ok(())
            },
            &mut |r| {
                anthropic_events
                    .borrow_mut()
                    .push(("reasoning", r.to_owned()));
                Ok(())
            },
        )
        .unwrap();
    let anthropic_msg = anthropic.finish(&mut |_| Ok(())).unwrap();
    assert_eq!(
        anthropic_events.into_inner(),
        vec![
            ("reasoning", "checking tree...".to_owned()),
            ("text", "done.".to_owned()),
        ]
    );
    assert_eq!(anthropic_msg["content"], "done.");
    assert_eq!(anthropic_msg["reasoning_content"], "checking tree...");

    let mut gemini = GeminiDecoder::default();
    let gemini_events = RefCell::new(Vec::new());
    let gemini_sse = concat!(
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"thought\":true,\"text\":\"evaluating options\"},{\"text\":\"selected 1\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"!\"}]},\"finishReason\":\"STOP\"}]}\n\n"
    );
    gemini
        .feed_with_reasoning(
            gemini_sse.as_bytes(),
            &mut |t| {
                gemini_events.borrow_mut().push(("text", t.to_owned()));
                Ok(())
            },
            &mut |r| {
                gemini_events.borrow_mut().push(("reasoning", r.to_owned()));
                Ok(())
            },
        )
        .unwrap();
    let gemini_msg = gemini.finish(&mut |_| Ok(())).unwrap();
    assert_eq!(
        gemini_events.into_inner(),
        vec![
            ("reasoning", "evaluating options".to_owned()),
            ("text", "selected 1".to_owned()),
            ("text", "!".to_owned()),
        ]
    );
    assert_eq!(gemini_msg["content"], "selected 1!");
    assert_eq!(gemini_msg["reasoning_content"], "evaluating options");
}

mod retry {
    use super::*;
    use axum::http::StatusCode;
    use coda::{
        budget::{BudgetCallState, BudgetConfig, ConfigureBudget},
        domain::ProjectId,
        network_authorization::{CallReceipt, CallResult},
    };
    use std::{collections::VecDeque, time::Instant};

    const PARTIAL: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"once\"}}]}\n\n";
    const SUCCESS: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"once\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
        "data: [DONE]\n\n"
    );

    struct Fixture {
        _dir: tempfile::TempDir,
        engine: Arc<Engine>,
        project: ProjectId,
        arrivals: Arc<Mutex<Vec<Instant>>>,
        server: tokio::task::JoinHandle<()>,
        run: Run,
    }

    impl Fixture {
        async fn start(script: &[(u16, Option<&str>, &'static str)]) -> Self {
            let arrivals = Arc::new(Mutex::new(Vec::new()));
            let recorded = arrivals.clone();
            let script = Arc::new(Mutex::new(
                script
                    .iter()
                    .map(|&(status, hint, body)| (status, hint.map(str::to_owned), body))
                    .collect::<VecDeque<_>>(),
            ));
            let handler = move || {
                let recorded = recorded.clone();
                let script = script.clone();
                async move {
                    recorded.lock().unwrap().push(Instant::now());
                    let (status, hint, body) = script
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or((500, None, ""));
                    let mut response =
                        Response::builder().status(StatusCode::from_u16(status).unwrap());
                    if let Some(hint) = hint {
                        response = response.header("retry-after", hint);
                    }
                    response.body(Body::from(body)).unwrap()
                }
            };
            let app = Router::new()
                .route("/chat/completions", post(handler.clone()))
                .route("/models", get(handler));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(Store::open(&dir.path().join("retry.db")).unwrap());
            store
                .save_settings(&Settings {
                    workspace: dir.path().to_string_lossy().into(),
                    max_concurrency: 1,
                    routes: vec![Route {
                        id: "test".into(),
                        name: "test".into(),
                        base_url,
                        model: "retry".into(),
                        protocol: None,
                        key_env: None,
                        max_tokens: 32,
                        parallel_limit: 1,
                    }],
                    newapi: None,
                })
                .unwrap();
            store.put_secret("route::test", OPENAI_KEY).unwrap();
            network_fixture::authorize_routes(&store);
            let project = store.current_project().unwrap().project_id.unwrap();
            store
                .configure_budget(ConfigureBudget {
                    project_id: project.clone(),
                    expected_revision: 0,
                    idempotency_key: "initial".into(),
                    config: BudgetConfig {
                        limit_tokens: 100,
                        per_call_reservation_tokens: 10,
                    },
                })
                .unwrap();
            let engine = Engine::new(store, 1).unwrap();
            let run = engine
                .start(RunRequest {
                    title: "retry".into(),
                    kind: SessionKind::Chat,
                    tasks: vec![TaskSpec {
                        name: "worker".into(),
                        role: "worker".into(),
                        route_id: "test".into(),
                        prompt: "reply".into(),
                        depends_on: vec![],
                        write_scopes: vec![],
                        tools: false,
                        allow_commands: false,
                        max_rounds: 1,
                    }],
                })
                .await
                .unwrap();
            Self {
                _dir: dir,
                engine,
                project,
                arrivals,
                server,
                run,
            }
        }

        fn calls(&self) -> Vec<CallReceipt> {
            self.engine
                .network_calls_for_task(&self.run.tasks[0].id)
                .unwrap()
        }

        async fn first_failure(&self) -> CallReceipt {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Some(call) = self
                        .calls()
                        .into_iter()
                        .find(|call| call.result == Some(CallResult::HttpFailure))
                    {
                        break call;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap()
        }

        async fn finish(&self, status: &str, attempts: usize) -> Run {
            let run = wait_for_run(&self.engine, &self.run.id).await;
            assert_eq!(run.tasks[0].status, status, "{:?}", run.tasks[0].error);
            assert_eq!(self.arrivals.lock().unwrap().len(), attempts);
            assert_eq!(self.calls().len(), attempts);
            let shutdown = self.engine.shutdown(Duration::from_secs(3)).await;
            let unknown = self
                .calls()
                .iter()
                .any(|call| call.result == Some(CallResult::TransportUnknown));
            assert_eq!(shutdown.external_outcome_unknown, unknown);
            assert_eq!(shutdown.remaining_workers, 0);
            assert!(!shutdown.persistence_or_worker_failure && !shutdown.timed_out);
            run
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    #[tokio::test]
    #[cfg_attr(
        not(windows),
        ignore = "persistent network authorization requires Windows DPAPI"
    )]
    async fn transient_statuses_back_off_and_account_for_each_attempt() {
        for pair in [[429, 502], [503, 504]] {
            let hint = (pair[0] == 429).then_some("1");
            let f = Fixture::start(&[
                (pair[0], hint, ""),
                (pair[1], None, ""),
                (200, None, SUCCESS),
            ])
            .await;
            let run = f.finish("completed", 3).await;
            assert_eq!(run.tasks[0].output, "once");
            assert_eq!(run.tasks[0].usage["total_tokens"], 3);
            let calls = f.calls();
            for (i, call) in calls.iter().enumerate() {
                let budget = f.engine.budget_call(&f.project, &call.call_id).unwrap();
                assert_eq!(budget.call, *call);
                if i < 2 {
                    assert_eq!(call.http_status, Some(pair[i]));
                    assert_eq!(budget.state, Some(BudgetCallState::Unknown));
                } else {
                    assert_eq!(budget.state, Some(BudgetCallState::Settled));
                }
            }
            let status = f.engine.budget_status(&f.project).unwrap();
            assert_eq!((status.spent_tokens, status.held_tokens), (3, 0));
            let arrivals = f.arrivals.lock().unwrap();
            assert!(
                arrivals[1] - arrivals[0]
                    >= Duration::from_millis(if hint.is_some() { 1000 } else { 250 })
            );
            assert!(arrivals[2] - arrivals[1] >= Duration::from_millis(500));
        }
    }

    #[tokio::test]
    #[cfg_attr(
        not(windows),
        ignore = "persistent network authorization requires Windows DPAPI"
    )]
    async fn stops_after_limit_permanent_failure_or_partial_success() {
        let f = Fixture::start(&[(503, None, ""); 3]).await;
        f.finish("failed", 3).await;
        for (status, hint, body) in [
            (401, None, ""),
            (500, None, ""),
            (429, Some("3600"), ""),
            (200, None, PARTIAL),
        ] {
            let f = Fixture::start(&[(status, hint, body)]).await;
            let run = f.finish("failed", 1).await;
            if status == 200 {
                let events = f.engine.store.events(&run.id, 0).unwrap();
                let deltas: Vec<_> = events
                    .iter()
                    .filter(|e| e.kind == "delta")
                    .map(|e| e.data["text"].as_str().unwrap())
                    .collect();
                assert_eq!(deltas.concat(), "once");
                assert_eq!(f.calls()[0].result, Some(CallResult::TransportUnknown));
            }
        }
    }

    #[tokio::test]
    #[cfg_attr(
        not(windows),
        ignore = "persistent network authorization requires Windows DPAPI"
    )]
    async fn backoff_honors_cancel_and_rechecks_revocation_and_budget() {
        for stop in ["cancel", "revoke", "budget"] {
            let f =
                Fixture::start(&[(503, Some(if stop == "cancel" { "10" } else { "1" }), "")]).await;
            let call = f.first_failure().await;
            match stop {
                "cancel" => f.engine.cancel(&f.run.tasks[0].id).unwrap(),
                "revoke" => {
                    f.engine
                        .store
                        .network_revoke(&call.authorization_id)
                        .unwrap();
                }
                _ => {
                    f.engine
                        .store
                        .configure_budget(ConfigureBudget {
                            project_id: f.project.clone(),
                            expected_revision: 1,
                            idempotency_key: "lower".into(),
                            config: BudgetConfig {
                                limit_tokens: 0,
                                per_call_reservation_tokens: 10,
                            },
                        })
                        .unwrap();
                }
            }
            if stop == "cancel" {
                tokio::time::timeout(Duration::from_secs(2), f.finish("cancelled", 1))
                    .await
                    .unwrap();
            } else {
                f.finish("failed", 1).await;
            }
        }
    }
    #[tokio::test]
    #[cfg_attr(
        not(windows),
        ignore = "persistent network authorization requires Windows DPAPI"
    )]
    async fn route_check_returns_ordered_receipts_across_both_requests() {
        let f = Fixture::start(&[
            (200, None, SUCCESS),
            (503, None, ""),
            (200, None, r#"{"data":[{"id":"retry"}]}"#),
            (429, None, ""),
            (200, None, SUCCESS),
        ])
        .await;
        assert_eq!(
            wait_for_run(&f.engine, &f.run.id).await.tasks[0].status,
            "completed"
        );
        let response = f.engine.route_check("test").await.unwrap();
        let observations: Vec<_> = response
            .calls
            .into_iter()
            .map(|call| {
                assert_eq!(f.engine.network_call(&call.call_id).unwrap(), call);
                (call.purpose, call.http_status)
            })
            .collect();
        assert_eq!(
            observations,
            [
                (NetworkPurpose::Models, Some(503)),
                (NetworkPurpose::Models, None),
                (NetworkPurpose::Completion, Some(429)),
                (NetworkPurpose::Completion, None),
            ]
        );
        assert_eq!(response.result["answer"], "once");
        assert_eq!(f.arrivals.lock().unwrap().len(), 5);
        assert!(f.engine.shutdown(Duration::from_secs(3)).await.settled());
    }
}

mod exploration {
    use super::*;
    use coda::{
        budget::{BudgetCallState, BudgetConfig, ConfigureBudget},
        network_authorization::CallResult,
    };

    const PARENT: &str = "parent-only conversation marker";
    const QUESTION: &str = "What does sample.txt define?";
    const EXCERPT: &str = "unique source excerpt marker";
    const OUTSIDE: &str = "outside workspace marker";
    const PRIVATE_THOUGHT: &str = "private child reasoning marker";
    const SUMMARY: &str = "The sample defines a counter.";

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Scenario {
        Success,
        Escape,
        BudgetDenied,
        Cancel,
    }

    async fn exercise(scenario: Scenario) {
        let recorded = RecordedCalls::default();
        let child_entered = Arc::new(tokio::sync::Notify::new());
        let app = Router::new().route(
            "/chat/completions",
            post({
                let recorded = recorded.clone();
                let child_entered = child_entered.clone();
                move |Json(request): Json<Value>| {
                    let recorded = recorded.clone();
                    let child_entered = child_entered.clone();
                    async move {
                        let index = {
                            let mut requests = recorded.requests.lock().unwrap();
                            requests.push(request);
                            requests.len() - 1
                        };
                        let (delta, finish) = match index {
                            0 => {
                                let paths = if scenario == Scenario::Escape {
                                    vec!["sample.txt", "../outside.txt"]
                                } else {
                                    vec!["sample.txt"]
                                };
                                let arguments = json!({"question":QUESTION,"paths":paths});
                                (json!({"tool_calls":[{"index":0,"id":"exploration-call","type":"function",
                                    "function":{"name":"explore_files","arguments":arguments.to_string()}}]}),"tool_calls")
                            }
                            1 if scenario != Scenario::Escape => {
                                child_entered.notify_one();
                                if scenario == Scenario::Cancel {
                                    tokio::time::sleep(Duration::from_secs(20)).await;
                                }
                                (json!({"content":SUMMARY,"reasoning_content":PRIVATE_THOUGHT}), "stop")
                            }
                            1 | 2 => (json!({"content":"parent done"}), "stop"),
                            _ => return axum::http::StatusCode::BAD_REQUEST.into_response(),
                        };
                        let frame = json!({"choices":[{"delta":delta,"finish_reason":finish}],
                            "usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}});
                        ([("content-type","text/event-stream")],format!("data: {frame}\n\ndata: [DONE]\n\n")).into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("sample.txt"), EXCERPT).unwrap();
        std::fs::write(dir.path().join("outside.txt"), OUTSIDE).unwrap();
        let db_path = dir.path().join("exploration.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        store
            .save_settings(&Settings {
                workspace: workspace.to_string_lossy().into_owned(),
                max_concurrency: 1,
                routes: vec![Route {
                    id: "local".into(),
                    name: "local".into(),
                    base_url,
                    model: "explore-test".into(),
                    protocol: None,
                    key_env: Some("CARGO_PKG_NAME".into()),
                    max_tokens: 128,
                    parallel_limit: 1,
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
                idempotency_key: "exploration".into(),
                config: BudgetConfig {
                    limit_tokens: if scenario == Scenario::BudgetDenied {
                        3
                    } else {
                        100
                    },
                    per_call_reservation_tokens: 3,
                },
            })
            .unwrap();
        let engine = Engine::new(store, 1).unwrap();
        let started = engine
            .start(RunRequest {
                title: "exploration".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "main".into(),
                    role: "coding".into(),
                    route_id: "local".into(),
                    prompt: PARENT.into(),
                    depends_on: vec![],
                    write_scopes: vec![],
                    tools: true,
                    allow_commands: false,
                    max_rounds: 3,
                }],
            })
            .await
            .unwrap();
        let task_id = &started.tasks[0].id;
        if scenario == Scenario::Cancel {
            tokio::time::timeout(Duration::from_secs(8), child_entered.notified())
                .await
                .unwrap();
            engine.cancel(task_id).unwrap();
        }
        wait_for_run(&engine, &started.id).await;
        // Cancellation may publish the terminal task state before the worker settles its receipt.
        let report = engine.shutdown(Duration::from_secs(5)).await;
        assert!(!report.timed_out);
        assert_eq!(report.remaining_workers, 0);
        let run = engine.store.run(&started.id).unwrap();
        let task = &run.tasks[0];
        let expected_calls = match scenario {
            Scenario::Success => 3,
            Scenario::Escape | Scenario::Cancel => 2,
            Scenario::BudgetDenied => 1,
        };
        let requests = recorded.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), expected_calls, "{:?}", task.error);
        assert!(
            requests[0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "explore_files")
        );
        assert!(requests[0]["messages"].to_string().contains(PARENT));
        assert!(
            requests
                .iter()
                .all(|r| r["model"] == "explore-test" && !r.to_string().contains(OUTSIDE))
        );

        let calls = engine.network_calls_for_task(task_id).unwrap();
        assert_eq!(calls.len(), expected_calls);
        for call in &calls {
            assert_eq!(call.task_id.as_ref(), Some(task_id));
            assert_eq!(call.authorization_id, calls[0].authorization_id);
            let budget = engine.budget_call(&project, &call.call_id).unwrap();
            assert_eq!(budget.call, *call);
            if scenario == Scenario::Cancel && call.call_id == calls[1].call_id {
                assert_eq!(call.result, Some(CallResult::TransportUnknown));
                assert_eq!(budget.state, Some(BudgetCallState::Unknown));
            } else {
                assert_eq!(budget.state, Some(BudgetCallState::Settled));
            }
        }
        if matches!(scenario, Scenario::Success | Scenario::Cancel) {
            let child = &requests[1];
            assert!(child.get("tools").is_none() || child["tools"] == json!([]));
            let messages = child["messages"].to_string();
            assert!(messages.contains(QUESTION) && messages.contains(EXCERPT));
            assert!(!messages.contains(PARENT));
        }
        if scenario == Scenario::Cancel {
            assert_eq!(task.status, "cancelled");
        } else if scenario == Scenario::BudgetDenied {
            assert_eq!(task.status, "failed");
            let code = coda::budget::BudgetErrorKind::Exceeded.code();
            assert!(
                task.error
                    .as_deref()
                    .is_some_and(|error| error.contains(code)),
                "{:?}",
                task.error
            );
            assert_eq!(task.usage["total_tokens"], 3);
            let status = engine.budget_status(&project).unwrap();
            assert_eq!(
                (
                    status.spent_tokens,
                    status.held_tokens,
                    status.available_tokens
                ),
                (3, 0, Some(0))
            );
        } else {
            assert_eq!(task.status, "completed", "{:?}", task.error);
            assert_eq!(task.output, "parent done");
            let total = (expected_calls * 3) as u64;
            assert_eq!(task.usage["total_tokens"], total);
            assert_eq!(engine.budget_status(&project).unwrap().spent_tokens, total);
            let history = &requests.last().unwrap()["messages"];
            let tool = history
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["role"] == "tool")
                .unwrap();
            assert_eq!(tool["tool_call_id"], "exploration-call");
            let result: Value = serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
            if scenario == Scenario::Success {
                assert_eq!(result["summary"], SUMMARY);
            } else {
                assert!(result["error"].is_string(), "{result}");
            }
            assert!(!history.to_string().contains(EXCERPT));
        }
        drop(engine);
        server.abort();
        let reopened = Store::open(&db_path).unwrap();
        let persisted = reopened.run(&started.id).unwrap();
        let messages = json!(persisted.tasks[0].messages).to_string();
        let events = json!(reopened.events(&started.id, 0).unwrap()).to_string();
        for marker in [PRIVATE_THOUGHT, EXCERPT, OUTSIDE] {
            assert!(!messages.contains(marker) && !events.contains(marker));
        }
        if scenario == Scenario::Success {
            assert!(messages.contains(SUMMARY));
        }
        assert_eq!(
            std::fs::read_to_string(workspace.join("sample.txt")).unwrap(),
            EXCERPT
        );
    }

    #[tokio::test]
    #[cfg_attr(
        not(windows),
        ignore = "persistent network authorization requires Windows DPAPI"
    )]
    async fn single_slot_child_is_isolated_and_charged_to_parent() {
        exercise(Scenario::Success).await;
    }

    #[tokio::test]
    #[cfg_attr(
        not(windows),
        ignore = "persistent network authorization requires Windows DPAPI"
    )]
    async fn traversal_rejects_the_batch_before_sending_child_context() {
        exercise(Scenario::Escape).await;
    }

    #[tokio::test]
    #[cfg_attr(
        not(windows),
        ignore = "persistent network authorization requires Windows DPAPI"
    )]
    async fn child_budget_denial_sends_and_charges_only_the_parent_request() {
        exercise(Scenario::BudgetDenied).await;
    }

    #[tokio::test]
    #[cfg_attr(
        not(windows),
        ignore = "persistent network authorization requires Windows DPAPI"
    )]
    async fn cancel_in_flight_child_finishes_receipt_without_resuming_parent() {
        exercise(Scenario::Cancel).await;
    }
}

#[path = "support/parallel_exploration.rs"]
mod parallel_exploration;

#[path = "support/durable_exploration.rs"]
mod durable_exploration;
