//! End-to-end integration tests for the zero-ceremony CLI AI Coding Workbench (`coda init`, `coda ask`, `@file`, `--continue`).
use axum::{
    Json, Router,
    extract::{RawQuery, State},
    http::{HeaderMap, header},
    response::IntoResponse,
    routing::post,
};
use serde_json::{Value, json};
use std::{
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone)]
struct MockLlm {
    turn: Arc<AtomicUsize>,
}

async fn completions(State(state): State<MockLlm>, Json(body): Json<Value>) -> impl IntoResponse {
    let step = state.turn.fetch_add(1, Ordering::SeqCst);
    // Verify the upgraded system prompt and modern tools are sent to the model!
    let tools = body["tools"].as_array().cloned().unwrap_or_default();
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert!(tool_names.contains(&"edit_file"));
    assert!(tool_names.contains(&"glob_files"));
    assert!(!tool_names.contains(&"run_wasm"));

    let sse = match step {
        0 => {
            let chunk = json!({
                "choices": [{
                    "delta": {
                        "reasoning_content": "Inspecting files first...",
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-read-1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": "{\"path\":\"src/lib.rs\",\"start_line\":1,\"end_line\":10}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 50, "completion_tokens": 20, "total_tokens": 85 }
            });
            format!("data: {chunk}\n\ndata: [DONE]\n\n")
        }
        1 => {
            let args = json!({
                "path": "src/lib.rs",
                "old_string": "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}",
                "new_string": "pub fn add(a: i32, b: i32) -> i32 {\n    let cancel_token = \"Bearer ok\";\n    let _ = cancel_token;\n    a + b\n}"
            })
            .to_string();
            let chunk = json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-edit-2",
                            "type": "function",
                            "function": {
                                "name": "edit_file",
                                "arguments": args
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 80, "completion_tokens": 30, "total_tokens": 110 }
            });
            format!("data: {chunk}\n\ndata: [DONE]\n\n")
        }
        _ => {
            let chunk = json!({
                "choices": [{
                    "delta": {
                        "content": "Fixed `add` in `src/lib.rs` using surgical `edit_file` (preserving `cancel_token` and `Bearer ok`)."
                    },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 100, "completion_tokens": 25, "total_tokens": 125 }
            });
            format!("data: {chunk}\n\ndata: [DONE]\n\n")
        }
    };
    ([("content-type", "text/event-stream")], sse)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_ceremony_init_and_ask_completes_surgical_code_edit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .with_state(MockLlm {
            turn: Arc::new(AtomicUsize::new(0)),
        });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("coda-data");
    let proj_dir = temp.path().join("pat_demo_project");
    std::fs::create_dir_all(proj_dir.join("src")).unwrap();
    std::fs::write(
        proj_dir.join("AGENTS.md"),
        "# Project Rules\nAlways prefer surgical edits.",
    )
    .unwrap();
    std::fs::write(
        proj_dir.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_coda");

    // Step 1: 1-command `coda init` with piped API key (zero revision/hash copy-paste!)
    let mut init_child = Command::new(bin)
        .args([
            "init",
            "--data-dir",
            &data_dir.to_string_lossy(),
            "--base-url",
            &format!("http://127.0.0.1:{port}/v1"),
            "--model",
            "gpt-test-coder",
            "--api-key-stdin",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        init_child
            .stdin
            .take()
            .unwrap()
            .write_all(b"sk-test-secret-12345\n")
            .unwrap();
    }
    let init_out = init_child.wait_with_output().unwrap();
    assert!(
        init_out.status.success(),
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&init_out.stdout),
        String::from_utf8_lossy(&init_out.stderr)
    );

    // Step 2: 1-command `coda ask -y` inside `pat_demo_project`!
    let ask_out = Command::new(bin)
        .args([
            "ask",
            "-y",
            "--data-dir",
            &data_dir.to_string_lossy(),
            "--dir",
            &proj_dir.to_string_lossy(),
            "Fix the bug in src/lib.rs add()",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&ask_out.stdout);
    let stderr = String::from_utf8_lossy(&ask_out.stderr);
    assert!(
        ask_out.status.success(),
        "ask failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("Fixed `add`"));
    assert!(!stdout.contains("Inspecting files first..."));
    assert!(stderr.contains("💭"));
    assert!(stderr.contains("Inspecting files first..."));
    assert!(!stdout.contains("sk-test-secret-12345"));
    assert!(!stderr.contains("sk-test-secret-12345"));

    let updated = std::fs::read_to_string(proj_dir.join("src/lib.rs")).unwrap();
    assert!(updated.contains("a + b"));
    assert!(updated.contains("let cancel_token = \"Bearer ok\";"));

    server.abort();
}

#[allow(clippy::type_complexity)]
#[derive(Clone)]
struct RecordingMockLlm {
    requests: Arc<Mutex<Vec<(Option<String>, Value)>>>,
    replies: Arc<Vec<(String, (u64, u64, u64))>>,
}

async fn recording_completions(
    State(state): State<RecordingMockLlm>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let idx = {
        let mut guard = state.requests.lock().unwrap();
        let current = guard.len();
        guard.push((auth, body));
        current
    };
    let (reply_text, (prompt_t, completion_t, total_t)) = state
        .replies
        .get(idx)
        .or_else(|| state.replies.last())
        .cloned()
        .unwrap_or_else(|| ("OK".to_owned(), (20, 10, 30)));
    let chunk = json!({
        "choices": [{
            "delta": { "content": reply_text },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": prompt_t,
            "completion_tokens": completion_t,
            "total_tokens": total_t
        }
    });
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_init_env_vars_mode_runs_ask_without_init() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/v1/chat/completions", post(recording_completions))
        .with_state(RecordingMockLlm {
            requests: requests.clone(),
            replies: Arc::new(vec![(
                "Zero-init environment mode succeeded!".into(),
                (42, 12, 54),
            )]),
        });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("fresh-env-data");
    let proj_dir = temp.path().join("env_project");
    std::fs::create_dir_all(&proj_dir).unwrap();
    std::fs::write(proj_dir.join("README.md"), "# Env Mode Demo\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_coda");
    let ask_out = Command::new(bin)
        .env("OPENAI_API_KEY", "sk-zero-init-env-secret-999")
        .env("OPENAI_BASE_URL", format!("http://127.0.0.1:{port}/v1"))
        .env("CODA_MODEL", "gpt-env-zero-init")
        .args([
            "ask",
            "-y",
            "--data-dir",
            &data_dir.to_string_lossy(),
            "--dir",
            &proj_dir.to_string_lossy(),
            "Verify zero-init startup",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&ask_out.stdout);
    let stderr = String::from_utf8_lossy(&ask_out.stderr);
    assert!(
        ask_out.status.success(),
        "zero-init ask failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("Zero-init environment mode succeeded!"));
    assert!(stderr.contains("[tokens: prompt=42, completion=12, total=54]"));
    assert!(!stdout.contains("sk-zero-init-env-secret-999"));
    assert!(!stderr.contains("sk-zero-init-env-secret-999"));

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].0.as_deref(),
        Some("Bearer sk-zero-init-env-secret-999")
    );
    assert_eq!(recorded[0].1["model"], "gpt-env-zero-init");

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_file_mention_attaches_numbered_line_range_to_prompt() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/v1/chat/completions", post(recording_completions))
        .with_state(RecordingMockLlm {
            requests: requests.clone(),
            replies: Arc::new(vec![(
                "Lines 1-3 define `compute_score`.".into(),
                (60, 15, 75),
            )]),
        });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("mention-data");
    let proj_dir = temp.path().join("mention_project");
    std::fs::create_dir_all(proj_dir.join("src")).unwrap();
    std::fs::write(
        proj_dir.join("src/lib.rs"),
        "pub fn compute_score(x: i32) -> i32 {\n    x * 2\n}\npub fn hidden_outside_range() {}\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_coda");
    let ask_out = Command::new(bin)
        .env("OPENAI_API_KEY", "sk-mention-test-123")
        .env("OPENAI_BASE_URL", format!("http://127.0.0.1:{port}/v1"))
        .env("CODA_MODEL", "gpt-mention-coder")
        .args([
            "ask",
            "-y",
            "--data-dir",
            &data_dir.to_string_lossy(),
            "--dir",
            &proj_dir.to_string_lossy(),
            "Explain @src/lib.rs:1-3 (cc foo@bar.com and @src/nonexistent.rs)",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&ask_out.stdout);
    let stderr = String::from_utf8_lossy(&ask_out.stderr);
    assert!(
        ask_out.status.success(),
        "mention ask failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("Lines 1-3 define `compute_score`."));
    assert!(stderr.contains("📎 Attached src/lib.rs:1-3"));

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let messages = recorded[0].1["messages"].as_array().unwrap();
    let user_msg = messages
        .iter()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .expect("missing user message");
    assert!(user_msg.contains("<context_files>"));
    assert!(user_msg.contains("1: pub fn compute_score(x: i32) -> i32 {"));
    assert!(user_msg.contains("2:     x * 2"));
    assert!(user_msg.contains("3: }"));
    assert!(!user_msg.contains("hidden_outside_range"));

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continue_flag_persists_multi_turn_conversation_across_invocations() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/v1/chat/completions", post(recording_completions))
        .with_state(RecordingMockLlm {
            requests: requests.clone(),
            replies: Arc::new(vec![
                (
                    "Understood, remembering code ALPHA-42.".into(),
                    (30, 10, 40),
                ),
                (
                    "You asked me to remember code ALPHA-42.".into(),
                    (55, 12, 67),
                ),
            ]),
        });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("continue-data");
    let proj_dir = temp.path().join("continue_project");
    std::fs::create_dir_all(&proj_dir).unwrap();

    let bin = env!("CARGO_BIN_EXE_coda");
    let base_url = format!("http://127.0.0.1:{port}/v1");

    // Turn 1: initial `coda ask`
    let turn1_out = Command::new(bin)
        .env("OPENAI_API_KEY", "sk-continue-test-456")
        .env("OPENAI_BASE_URL", &base_url)
        .env("CODA_MODEL", "gpt-session-coder")
        .args([
            "ask",
            "--data-dir",
            &data_dir.to_string_lossy(),
            "--dir",
            &proj_dir.to_string_lossy(),
            "Remember code ALPHA-42",
        ])
        .output()
        .unwrap();
    assert!(
        turn1_out.status.success(),
        "turn 1 failed: stdout={} stderr={}",
        String::from_utf8_lossy(&turn1_out.stdout),
        String::from_utf8_lossy(&turn1_out.stderr)
    );

    // Turn 2: `coda ask -c` continues the exact same session in a new turn!
    let turn2_out = Command::new(bin)
        .env("OPENAI_API_KEY", "sk-continue-test-456")
        .env("OPENAI_BASE_URL", &base_url)
        .env("CODA_MODEL", "gpt-session-coder")
        .args([
            "ask",
            "-c",
            "--data-dir",
            &data_dir.to_string_lossy(),
            "--dir",
            &proj_dir.to_string_lossy(),
            "What code did I ask you to remember?",
        ])
        .output()
        .unwrap();
    let stdout2 = String::from_utf8_lossy(&turn2_out.stdout);
    let stderr2 = String::from_utf8_lossy(&turn2_out.stderr);
    assert!(
        turn2_out.status.success(),
        "turn 2 (-c) failed: stdout={stdout2} stderr={stderr2}"
    );
    assert!(stdout2.contains("ALPHA-42"));

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    let second_messages = recorded[1].1["messages"].as_array().unwrap();
    let has_turn1_prompt = second_messages.iter().any(|m| {
        m["role"] == "user"
            && m["content"]
                .as_str()
                .is_some_and(|text| text.contains("ALPHA-42"))
    });
    let has_turn2_prompt = second_messages.iter().any(|m| {
        m["role"] == "user"
            && m["content"]
                .as_str()
                .is_some_and(|text| text.contains("What code did I ask you to remember?"))
    });
    assert!(
        has_turn1_prompt,
        "expected Turn 1 ALPHA-42 message in Turn 2 LLM history: {second_messages:?}"
    );
    assert!(
        has_turn2_prompt,
        "expected Turn 2 prompt in Turn 2 LLM history: {second_messages:?}"
    );

    server.abort();
}

async fn mock_anthropic_messages(headers: HeaderMap, Json(body): Json<Value>) -> impl IntoResponse {
    assert_eq!(
        headers.get("x-api-key").and_then(|v| v.to_str().ok()),
        Some("sk-ant-zero-init-secret-777")
    );
    assert_eq!(
        headers
            .get("anthropic-version")
            .and_then(|v| v.to_str().ok()),
        Some("2023-06-01")
    );
    assert_eq!(body["model"], "claude-sonnet-4-20250514");
    assert_eq!(body["stream"], true);

    let sse = [
        format!(
            "event: message_start\ndata: {}\n\n",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_zero_ant",
                    "role": "assistant",
                    "usage": { "input_tokens": 30, "output_tokens": 1 }
                }
            })
        ),
        format!(
            "event: content_block_start\ndata: {}\n\n",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "thinking", "thinking": "" }
            })
        ),
        format!(
            "event: content_block_delta\ndata: {}\n\n",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "thinking_delta", "thinking": "Analyzing Anthropic request..." }
            })
        ),
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n"
            .to_owned(),
        format!(
            "event: content_block_start\ndata: {}\n\n",
            json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": { "type": "text", "text": "" }
            })
        ),
        format!(
            "event: content_block_delta\ndata: {}\n\n",
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": { "type": "text_delta", "text": "Anthropic zero-init ready!" }
            })
        ),
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n"
            .to_owned(),
        format!(
            "event: message_delta\ndata: {}\n\n",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": 14 }
            })
        ),
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_owned(),
    ]
    .concat();

    ([("content-type", "text/event-stream")], sse)
}

async fn mock_gemini_stream(
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    assert_eq!(query.as_deref(), Some("alt=sse"));
    assert_eq!(
        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
        Some("AIzaSyZeroInitGeminiSecret888")
    );
    assert!(body.get("model").is_none());

    let sse = [format!(
        "data: {}\n\n",
        json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        { "thought": true, "text": "Analyzing Gemini request..." },
                        { "text": "Gemini zero-init ready!" }
                    ]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 25,
                "candidatesTokenCount": 11,
                "totalTokenCount": 40
            }
        })
    )]
    .concat();

    ([("content-type", "text/event-stream")], sse)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_init_anthropic_and_gemini_env_vars_work_without_init() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/anthropic/v1/messages", post(mock_anthropic_messages))
        .route(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent",
            post(mock_gemini_stream),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    let proj_dir = temp.path().join("multi_provider_proj");
    std::fs::create_dir_all(&proj_dir).unwrap();
    let bin = env!("CARGO_BIN_EXE_coda");

    // 1. Anthropic zero-init without `coda init`
    let ant_data = temp.path().join("ant-zero-data");
    let ant_out = Command::new(bin)
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GEMINI_BASE_URL")
        .env_remove("CODA_MODEL")
        .env("ANTHROPIC_API_KEY", "sk-ant-zero-init-secret-777")
        .env(
            "ANTHROPIC_BASE_URL",
            format!("http://127.0.0.1:{port}/anthropic/v1"),
        )
        .args([
            "ask",
            "-y",
            "--data-dir",
            &ant_data.to_string_lossy(),
            "--dir",
            &proj_dir.to_string_lossy(),
            "Verify Anthropic zero-init",
        ])
        .output()
        .unwrap();
    let ant_stdout = String::from_utf8_lossy(&ant_out.stdout);
    let ant_stderr = String::from_utf8_lossy(&ant_out.stderr);
    assert!(
        ant_out.status.success(),
        "Anthropic zero-init failed: stdout={ant_stdout} stderr={ant_stderr}"
    );
    assert!(ant_stdout.contains("Anthropic zero-init ready!"));
    assert!(!ant_stdout.contains("Analyzing Anthropic request..."));
    assert!(ant_stderr.contains("💭"));
    assert!(ant_stderr.contains("Analyzing Anthropic request..."));
    assert!(ant_stderr.contains("[tokens: prompt=30, completion=14, total=44]"));
    assert!(!ant_stdout.contains("sk-ant-zero-init-secret-777"));
    assert!(!ant_stderr.contains("sk-ant-zero-init-secret-777"));

    // 2. Gemini zero-init without `coda init`
    let gem_data = temp.path().join("gem-zero-data");
    let gem_out = Command::new(bin)
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("CODA_MODEL")
        .env("GEMINI_API_KEY", "AIzaSyZeroInitGeminiSecret888")
        .env("GEMINI_BASE_URL", format!("http://127.0.0.1:{port}/v1beta"))
        .args([
            "ask",
            "-y",
            "--data-dir",
            &gem_data.to_string_lossy(),
            "--dir",
            &proj_dir.to_string_lossy(),
            "Verify Gemini zero-init",
        ])
        .output()
        .unwrap();
    let gem_stdout = String::from_utf8_lossy(&gem_out.stdout);
    let gem_stderr = String::from_utf8_lossy(&gem_out.stderr);
    assert!(
        gem_out.status.success(),
        "Gemini zero-init failed: stdout={gem_stdout} stderr={gem_stderr}"
    );
    assert!(gem_stdout.contains("Gemini zero-init ready!"));
    assert!(!gem_stdout.contains("Analyzing Gemini request..."));
    assert!(gem_stderr.contains("💭"));
    assert!(gem_stderr.contains("Analyzing Gemini request..."));
    assert!(gem_stderr.contains("[tokens: prompt=25, completion=11, total=40]"));
    assert!(!gem_stdout.contains("AIzaSyZeroInitGeminiSecret888"));
    assert!(!gem_stderr.contains("AIzaSyZeroInitGeminiSecret888"));

    // 3. `coda init --provider anthropic --json` populates Anthropic defaults cleanly
    let init_ant_data = temp.path().join("init-ant-data");
    let mut init_child = Command::new(bin)
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("CODA_MODEL")
        .args([
            "--json",
            "init",
            "--provider",
            "anthropic",
            "--data-dir",
            &init_ant_data.to_string_lossy(),
            "--api-key-stdin",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        init_child
            .stdin
            .take()
            .unwrap()
            .write_all(b"sk-ant-init-secret-321\n")
            .unwrap();
    }
    let init_out = init_child.wait_with_output().unwrap();
    assert!(
        init_out.status.success(),
        "init --provider anthropic failed: stdout={} stderr={}",
        String::from_utf8_lossy(&init_out.stdout),
        String::from_utf8_lossy(&init_out.stderr)
    );
    let init_json: Value = serde_json::from_slice(&init_out.stdout).unwrap();
    assert_eq!(init_json["ok"], true);
    assert_eq!(init_json["base_url"], "https://api.anthropic.com:443/v1");
    assert_eq!(init_json["model"], "claude-sonnet-4-20250514");

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continue_with_model_override_switches_model_preserving_session_context() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/v1/chat/completions", post(recording_completions))
        .with_state(RecordingMockLlm {
            requests: requests.clone(),
            replies: Arc::new(vec![
                ("Stored passphrase ORBIT-731.".into(), (32, 10, 42)),
                (
                    "Your passphrase is ORBIT-731 (answered by v2).".into(),
                    (58, 14, 72),
                ),
            ]),
        });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("switch-model-data");
    let proj_dir = temp.path().join("switch_model_project");
    std::fs::create_dir_all(&proj_dir).unwrap();

    let bin = env!("CARGO_BIN_EXE_coda");
    let base_url = format!("http://127.0.0.1:{port}/v1");

    // Turn 1: initial turn on `gpt-coder-v1`
    let turn1_out = Command::new(bin)
        .env("OPENAI_API_KEY", "sk-model-switch-test-555")
        .env("OPENAI_BASE_URL", &base_url)
        .args([
            "ask",
            "--model",
            "gpt-coder-v1",
            "--data-dir",
            &data_dir.to_string_lossy(),
            "--dir",
            &proj_dir.to_string_lossy(),
            "Remember passphrase ORBIT-731",
        ])
        .output()
        .unwrap();
    assert!(
        turn1_out.status.success(),
        "turn 1 failed: stdout={} stderr={}",
        String::from_utf8_lossy(&turn1_out.stdout),
        String::from_utf8_lossy(&turn1_out.stderr)
    );

    // Turn 2: `-c --model gpt-coder-v2` switches model AND preserves Turn 1 context!
    let turn2_out = Command::new(bin)
        .env("OPENAI_API_KEY", "sk-model-switch-test-555")
        .env("OPENAI_BASE_URL", &base_url)
        .args([
            "ask",
            "-c",
            "--model",
            "gpt-coder-v2",
            "--data-dir",
            &data_dir.to_string_lossy(),
            "--dir",
            &proj_dir.to_string_lossy(),
            "What passphrase did I ask you to remember?",
        ])
        .output()
        .unwrap();
    let stdout2 = String::from_utf8_lossy(&turn2_out.stdout);
    let stderr2 = String::from_utf8_lossy(&turn2_out.stderr);
    assert!(
        turn2_out.status.success(),
        "turn 2 (-c --model) failed: stdout={stdout2} stderr={stderr2}"
    );
    assert!(stdout2.contains("ORBIT-731"));

    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].1["model"], "gpt-coder-v1");
    assert_eq!(recorded[1].1["model"], "gpt-coder-v2");

    let second_messages = recorded[1].1["messages"].as_array().unwrap();
    let has_turn1_context = second_messages.iter().any(|m| {
        m["role"] == "user"
            && m["content"]
                .as_str()
                .is_some_and(|text| text.contains("ORBIT-731"))
    });
    let has_turn2_prompt = second_messages.iter().any(|m| {
        m["role"] == "user"
            && m["content"]
                .as_str()
                .is_some_and(|text| text.contains("What passphrase did I ask you to remember?"))
    });
    assert!(
        has_turn1_context,
        "expected Turn 1 ORBIT-731 context in Turn 2 messages: {second_messages:?}"
    );
    assert!(
        has_turn2_prompt,
        "expected Turn 2 prompt in Turn 2 messages: {second_messages:?}"
    );

    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "provider credentials require Windows DPAPI")]
async fn exploration_progress_preserves_piped_parent_text() {
    const EXCERPT: &str = "private file contents";
    const THOUGHT: &str = "private child reasoning";
    const SUMMARY: &str = "summary for parent only";

    let requests = RecordingMockLlm {
        requests: Arc::default(),
        replies: Arc::default(),
    };
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|State(state): State<RecordingMockLlm>, Json(body): Json<Value>| async move {
                let mut calls = state.requests.lock().unwrap();
                let (delta, finish) = match calls.len() {
                    0 => {
                        let args = json!({"question":"What does this file do?","paths":["sample.txt"]});
                        (json!({"content":"Before ", "tool_calls":[{
                            "index":0,"id":"explore-1","type":"function",
                            "function":{"name":"explore_files","arguments":args.to_string()}
                        }]}), "tool_calls")
                    }
                    1 => (json!({"content":SUMMARY,"reasoning_content":THOUGHT}), "stop"),
                    _ => (json!({"content":"after."}), "stop"),
                };
                calls.push((None, body));
                let frame = json!({"choices":[{"delta":delta,"finish_reason":finish}],
                    "usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}});
                ([("content-type","text/event-stream")], format!("data: {frame}\n\ndata: [DONE]\n\n"))
            }),
        )
        .with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("sample.txt"), EXCERPT).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_coda"))
        .env("OPENAI_API_KEY", "sk-exploration-test-only")
        .env("OPENAI_BASE_URL", base_url)
        .env("CODA_MODEL", "gpt-explore")
        .args(["ask", "--data-dir"])
        .arg(temp.path().join("data"))
        .arg("--dir")
        .arg(project)
        .arg("Explore the sample file")
        .output()
        .unwrap();
    server.abort();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "ask failed: {stderr}");
    assert_eq!(stdout.trim_end_matches(['\r', '\n']), "Before after.");
    assert!(
        stderr
            .lines()
            .any(|s| s.contains(r#"[tool] explore_files Exploring "sample.txt""#))
    );
    assert!(
        stderr
            .lines()
            .any(|s| s.contains("[explore_files] Summary ready"))
    );
    for private in [EXCERPT, THOUGHT, SUMMARY] {
        assert!(!stdout.contains(private) && !stderr.contains(private));
    }
    let requests = requests.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].1["messages"].to_string().contains(EXCERPT));
    assert!(requests[2].1["messages"].to_string().contains(SUMMARY));
}

#[path = "support/http_mcp_workbench.rs"]
mod http_mcp_workbench;

#[path = "support/key_pool_workbench.rs"]
mod key_pool_workbench;

#[path = "support/oauth_workbench.rs"]
mod oauth_workbench;

#[path = "support/parallel_workbench.rs"]
mod parallel_workbench;

#[path = "support/durable_workbench.rs"]
mod durable_workbench;

#[test]
fn computer_watch_without_registration_reports_configuration_error() {
    let workspace = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_coda");
    let group = Command::new(bin)
        .args(["computer", "--help"])
        .output()
        .unwrap();
    assert!(group.status.success());
    let group_help = String::from_utf8_lossy(&group.stdout);
    assert!(group_help.contains("Windows Computer Use"));
    assert!(group_help.contains("does not click, type, or save files"));
    let help = Command::new(bin)
        .args(["computer", "watch", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--timeout-ms"));

    let output = Command::new(bin)
        .env_remove("CODA_COMPUTER_TARGET")
        .args(["--json", "--workspace"])
        .arg(workspace.path())
        .args(["computer", "watch", "--timeout-ms", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "computer_watch_error");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("no computer target registered")
    );
}
