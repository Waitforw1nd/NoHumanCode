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

    let _ = engine.shutdown(Duration::from_secs(3)).await;
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

    let _ = engine.shutdown(Duration::from_secs(3)).await;
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
