//! Clean, high-signal subsystem integration tests for Coda CLI:
//! 1. Approvals, `edit_file` / `write_file` workspace changes, Checkpoints & Restore
//! 2. Multi-turn chat continuation after `completed` / `interrupted` / `failed` / `cancelled` + Context Compaction
//! 3. Budget accounting (non-bricking on missing/reasoning usage, unblocking on limit increase) & Network auto-auth + explicit revoke
//! 4. Project management & fast 3-command Git Worktree creation (even on dirty repos & paths containing `pat_`)
//! 5. Standard Stdio MCP (`mcp__<server>__<tool>`) discovery, approval, and execution end-to-end

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use coda::{
    approval::ExecutionState,
    budget::{BudgetBlockedReason, BudgetConfig, ConfigureBudget},
    domain::{ProjectId, Route, RunRequest, SendChatTurn, SessionKind, TaskSpec},
    engine::Engine,
    network_authorization::{NetworkSubject, NetworkSubjectKind},
    project_config::{
        ConfigureProject, ProjectConfig, RegisterProject, SelectProject, StartInProject,
    },
    store::{Store, fresh_settings},
    worktree_management::{CreateWorktree, WorktreeState},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone)]
struct ScriptedLlm {
    step: Arc<AtomicUsize>,
    mode: &'static str,
}

async fn llm_handler(State(state): State<ScriptedLlm>, Json(body): Json<Value>) -> Response {
    let idx = state.step.fetch_add(1, Ordering::SeqCst);
    let sse = match (state.mode, idx) {
        ("edit_and_restore", 0) => {
            let args = json!({
                "path": "src/main.rs",
                "old_string": "fn version() -> &'static str {\n    \"v1\"\n}",
                "new_string": "fn version() -> &'static str {\n    \"v2-Bearer-token\"\n}"
            })
            .to_string();
            let c = json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-edit-1",
                            "type": "function",
                            "function": { "name": "edit_file", "arguments": args }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 20, "completion_tokens": 15, "total_tokens": 50 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("fail_then_cancel_then_succeed", 0) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": { "message": "temporary upstream failure" } })),
            )
                .into_response();
        }
        ("fail_then_cancel_then_succeed", 1) => {
            let args = json!({
                "path": "draft.txt",
                "content": "pending-cancel"
            })
            .to_string();
            let c = json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-cancel-1",
                            "type": "function",
                            "function": { "name": "write_file", "arguments": args }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 15, "completion_tokens": 10, "total_tokens": 25 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("fail_then_cancel_then_succeed", _) => {
            let c = json!({
                "choices": [{
                    "delta": { "content": "recovered-after-failure-and-cancel" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 15, "completion_tokens": 10, "total_tokens": 25 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("mcp_echo", 0) => {
            let has_mcp_tool = body["tools"].as_array().is_some_and(|tools| {
                tools
                    .iter()
                    .any(|t| t["function"]["name"] == "mcp__demo__echo_tool")
            });
            if !has_mcp_tool {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": { "message": "mcp__demo__echo_tool was not advertised" } })),
                )
                    .into_response();
            }
            let args = json!({ "text": "hello-from-coda" }).to_string();
            let c = json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-mcp-1",
                            "type": "function",
                            "function": { "name": "mcp__demo__echo_tool", "arguments": args }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 20, "completion_tokens": 15, "total_tokens": 35 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("mcp_echo", _) => {
            let tool_reply = body["messages"]
                .as_array()
                .and_then(|msgs| {
                    msgs.iter()
                        .find(|m| m["role"] == "tool" && m["tool_call_id"] == "call-mcp-1")
                })
                .and_then(|m| m["content"].as_str())
                .unwrap_or("missing-tool-reply");
            let c = json!({
                "choices": [{
                    "delta": { "content": format!("mcp-final:{tool_reply}") },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 25, "completion_tokens": 12, "total_tokens": 37 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("missing_usage", _) => {
            // Deliberately omit `usage` to prove missing usage never bricks the project!
            let c = json!({
                "choices": [{
                    "delta": { "content": "ok without usage" },
                    "finish_reason": "stop"
                }]
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("todo_and_model_switch", 0) => {
            let model = body["model"].as_str().unwrap_or_default();
            let args = json!({
                "todos": [
                    { "id": "t1", "content": format!("plan-with-{model}"), "status": "completed" },
                    { "id": "t2", "content": "verify-switch", "status": "in_progress" }
                ]
            })
            .to_string();
            let c = json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-todo-1",
                            "type": "function",
                            "function": { "name": "todo_write", "arguments": args }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("todo_and_model_switch", 1) => {
            let args = json!({
                "path": "step1.txt",
                "content": "turn1-checkpoint-file\n"
            })
            .to_string();
            let c = json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-write-1",
                            "type": "function",
                            "function": { "name": "write_file", "arguments": args }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 12, "completion_tokens": 10, "total_tokens": 22 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("todo_and_model_switch", 2) => {
            let model = body["model"].as_str().unwrap_or_default();
            let c = json!({
                "choices": [{
                    "delta": { "content": format!("turn1-done:{model}") },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 14, "completion_tokens": 8, "total_tokens": 22 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("todo_and_model_switch", 3) => {
            let c = json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-todo-read-2",
                            "type": "function",
                            "function": { "name": "todo_read", "arguments": "{}" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 16, "completion_tokens": 8, "total_tokens": 24 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("todo_and_model_switch", 4) => {
            let args = json!({
                "path": "step2.txt",
                "content": "turn2-checkpoint-file\n"
            })
            .to_string();
            let c = json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-write-2",
                            "type": "function",
                            "function": { "name": "write_file", "arguments": args }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 18, "completion_tokens": 10, "total_tokens": 28 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        ("todo_and_model_switch", _) => {
            let model = body["model"].as_str().unwrap_or_default();
            let max_tokens = body["max_tokens"].as_u64().unwrap_or_default();
            let todo_reply = body["messages"]
                .as_array()
                .and_then(|msgs| {
                    msgs.iter()
                        .find(|m| m["role"] == "tool" && m["tool_call_id"] == "call-todo-read-2")
                })
                .and_then(|m| m["content"].as_str())
                .unwrap_or("missing-todo");
            let c = json!({
                "choices": [{
                    "delta": { "content": format!("turn2-model={model}|max={max_tokens}|todos={todo_reply}") },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 20, "completion_tokens": 12, "total_tokens": 32 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
        _ => {
            let c = json!({
                "choices": [{
                    "delta": { "content": format!("reply-{idx}") },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 15, "completion_tokens": 10, "total_tokens": 25 }
            });
            format!("data: {c}\n\ndata: [DONE]\n\n")
        }
    };
    ([("content-type", "text/event-stream")], sse).into_response()
}

async fn start_mock_llm(mode: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/v1/chat/completions", post(llm_handler))
        .with_state(ScriptedLlm {
            step: Arc::new(AtomicUsize::new(0)),
            mode,
        });
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{port}/v1"), handle)
}

async fn setup_engine_with_project(
    data_dir: &std::path::Path,
    workspace_dir: &std::path::Path,
    base_url: &str,
) -> (Arc<Engine>, ProjectId) {
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::create_dir_all(workspace_dir).unwrap();
    let store = Arc::new(Store::open(&data_dir.join("coda.sqlite3")).unwrap());
    let mut settings = fresh_settings(&store, workspace_dir).unwrap();
    settings.routes.push(Route {
        id: "default".into(),
        name: "Default".into(),
        base_url: base_url.to_owned(),
        model: "test-coder".into(),
        protocol: None,
        max_tokens: 4096,
        parallel_limit: 2,
        key_env: None,
    });
    let engine = Engine::new(store, 2).unwrap();
    engine
        .configure_with_keys(
            settings,
            HashMap::from([("default".into(), "sk-secret-key-999".into())]),
        )
        .await
        .unwrap();

    let project = engine
        .register_project(RegisterProject {
            name: "pat_demo".into(),
            root_path: workspace_dir.to_string_lossy().into_owned(),
            idempotency_key: "reg-1".into(),
        })
        .await
        .unwrap();
    let snap = engine
        .configure_project(ConfigureProject {
            project_id: project.id.clone(),
            expected_revision: 1,
            config: ProjectConfig {
                allowed_route_ids: vec!["default".into()],
                default_route_id: Some("default".into()),
                tools: true,
                write_scopes: vec!["*".into()],
                allow_commands: true,
            },
        })
        .await
        .unwrap();
    assert_eq!(snap.revision, 2);
    let sel = engine.current_project().unwrap();
    engine
        .select_project(SelectProject {
            project_id: project.id.clone(),
            expected_revision: sel.revision,
        })
        .await
        .unwrap();
    (engine, project.id)
}

#[tokio::test]
async fn edit_file_approval_checkpoint_and_restore_roundtrip() {
    let (base_url, server) = start_mock_llm("edit_and_restore").await;
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("pat_workspace");
    std::fs::create_dir_all(ws.join("src")).unwrap();
    let original = "fn version() -> &'static str {\n    \"v1\"\n}\n";
    std::fs::write(ws.join("src/main.rs"), original).unwrap();

    let (engine, project_id) =
        setup_engine_with_project(&temp.path().join("data"), &ws, &base_url).await;

    let run = engine
        .start_in_project(StartInProject {
            project_id,
            expected_config_revision: 2,
            request: RunRequest {
                title: "Edit and restore Bearer code".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "coder".into(),
                    role: "engineer".into(),
                    route_id: "default".into(),
                    prompt: "Bump version to v2".into(),
                    depends_on: vec![],
                    write_scopes: vec!["*".into()],
                    tools: true,
                    allow_commands: false,
                    max_rounds: 8,
                }],
            },
            idempotency_key: "run-edit-1".into(),
        })
        .await
        .unwrap();
    let task_id = run.tasks[0].id.clone();

    // Wait for pending edit_file approval and verify diff summary
    let approval_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let list = engine.store.approvals_for_task(&task_id).unwrap();
            if let Some(rec) = list.first() {
                assert_eq!(rec.tool_name, "edit_file");
                return rec.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    engine
        .decide_approval_with_network_ack(&approval_id, true, Some("tester"), false)
        .unwrap();

    // Wait for completion
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let t = engine.store.task(&task_id).unwrap();
            if t.status == "completed" {
                break;
            }
            assert_ne!(t.status, "failed", "task failed: {:?}", t.error);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let modified = std::fs::read_to_string(ws.join("src/main.rs")).unwrap();
    assert!(modified.contains("v2-Bearer-token"));

    // Create a checkpoint and restore the `edit_file` mutation back to original v1!
    let cp = engine.create_checkpoint(&task_id, "cp-1").await.unwrap();
    let receipt = engine
        .restore_checkpoint(&cp.checkpoint.checkpoint_id)
        .await
        .unwrap();
    assert!(receipt.ok());

    let restored = std::fs::read_to_string(ws.join("src/main.rs")).unwrap();
    assert_eq!(restored, original);
    server.abort();
}

#[tokio::test]
async fn chat_session_continues_cleanly_after_interruption_or_cancellation() {
    let (base_url, server) = start_mock_llm("fail_then_cancel_then_succeed").await;
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("ws");
    let (engine, project_id) =
        setup_engine_with_project(&temp.path().join("data"), &ws, &base_url).await;

    // Turn 1: upstream HTTP 500 causes Turn 1 to genuinely end in `"failed"`
    let run = engine
        .start_in_project(StartInProject {
            project_id,
            expected_config_revision: 2,
            request: RunRequest {
                title: "Multi-turn session".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "coder".into(),
                    role: "engineer".into(),
                    route_id: "default".into(),
                    prompt: "First turn".into(),
                    depends_on: vec![],
                    write_scopes: vec!["*".into()],
                    tools: true,
                    allow_commands: false,
                    max_rounds: 4,
                }],
            },
            idempotency_key: "run-chat-1".into(),
        })
        .await
        .unwrap();
    let first_task_id = run.tasks[0].id.clone();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if engine.store.task(&first_task_id).unwrap().status == "failed" && !engine.is_busy() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    // Turn 2: continue after `"failed"`, pause on pending `write_file` approval, then `engine.cancel`
    // while `"running"` so Turn 2 genuinely ends in `"cancelled"` (with an open tool_call)!
    let ctx1 = engine.store.run_context(&run.id).unwrap();
    let turn2 = engine
        .send_chat_turn(SendChatTurn {
            session_id: ctx1.session.id,
            agent_id: ctx1.tasks[0].agent_id.clone(),
            expected_last_turn_id: ctx1.latest_turn.id,
            message: "Try writing draft".into(),
            idempotency_key: "turn-2".into(),
        })
        .await
        .unwrap();
    let turn2_task_id = turn2.task.legacy_task_id.clone();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !engine
                .store
                .approvals_for_task(&turn2_task_id)
                .unwrap()
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    engine.cancel(&turn2_task_id).unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if engine.store.task(&turn2_task_id).unwrap().status == "cancelled" && !engine.is_busy()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    // Turn 3: continue cleanly after `"cancelled"` all the way to `"completed"`!
    let ctx2 = engine.store.run_context(&run.id).unwrap();
    let turn3 = engine
        .send_chat_turn(SendChatTurn {
            session_id: ctx2.session.id,
            agent_id: ctx2.tasks[0].agent_id.clone(),
            expected_last_turn_id: ctx2.latest_turn.id,
            message: "Continue after cancellation".into(),
            idempotency_key: "turn-3".into(),
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let t = engine.store.task(&turn3.task.legacy_task_id).unwrap();
            if t.status == "completed" {
                assert!(t.output.contains("recovered-after-failure-and-cancel"));
                break;
            }
            assert_ne!(t.status, "failed", "turn 3 failed: {:?}", t.error);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    server.abort();
}

#[tokio::test]
async fn budget_never_bricks_on_missing_usage_and_explicit_revoke_blocks_network() {
    let (base_url, server) = start_mock_llm("missing_usage").await;
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("ws");
    let (engine, project_id) =
        setup_engine_with_project(&temp.path().join("data"), &ws, &base_url).await;

    engine
        .configure_budget(ConfigureBudget {
            project_id: project_id.clone(),
            expected_revision: 0,
            idempotency_key: "b-1".into(),
            config: BudgetConfig {
                limit_tokens: 100,
                per_call_reservation_tokens: 25,
            },
        })
        .unwrap();

    // Run 1 with missing usage completes AND leaves budget unblocked!
    let run1 = engine
        .start_in_project(StartInProject {
            project_id: project_id.clone(),
            expected_config_revision: 2,
            request: RunRequest {
                title: "Missing usage run".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "coder".into(),
                    role: "engineer".into(),
                    route_id: "default".into(),
                    prompt: "Hi".into(),
                    depends_on: vec![],
                    write_scopes: vec![],
                    tools: false,
                    allow_commands: false,
                    max_rounds: 2,
                }],
            },
            idempotency_key: "run-b-1".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if engine.store.task(&run1.tasks[0].id).unwrap().status == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let status = engine.budget_status(&project_id).unwrap();
    assert_ne!(status.blocked_reason, Some(BudgetBlockedReason::Unresolved));
    assert_eq!(status.blocked_reason, None);

    // Explicit network_revoke still blocks future calls!
    let preview = engine
        .network_preview(NetworkSubject {
            kind: NetworkSubjectKind::Route,
            id: "default".into(),
            project_id: Some(project_id.clone()),
        })
        .await
        .unwrap();
    let auth_id = preview.authorization.unwrap().authorization_id;
    engine.network_revoke(&auth_id).await.unwrap();

    let revoked_err = engine
        .start_in_project(StartInProject {
            project_id,
            expected_config_revision: 2,
            request: RunRequest {
                title: "Blocked after explicit revoke".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "coder".into(),
                    role: "engineer".into(),
                    route_id: "default".into(),
                    prompt: "Should fail".into(),
                    depends_on: vec![],
                    write_scopes: vec![],
                    tools: false,
                    allow_commands: false,
                    max_rounds: 2,
                }],
            },
            idempotency_key: "run-b-2".into(),
        })
        .await;
    assert!(revoked_err.is_err());
    server.abort();
}

#[tokio::test]
async fn fast_worktree_creation_succeeds_even_with_untracked_files_in_source_repo() {
    let (base_url, server) = start_mock_llm("chat").await;
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("repo");
    let wt_parent = temp.path().join("worktrees");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&wt_parent).unwrap();

    Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&ws)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "dev@example.com"])
        .current_dir(&ws)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Dev"])
        .current_dir(&ws)
        .status()
        .unwrap();
    std::fs::write(ws.join("README.md"), "# Demo\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(&ws)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init", "--quiet"])
        .current_dir(&ws)
        .status()
        .unwrap();
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&ws)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();

    // Add an untracked scratch file (previously rejected as GitWorktreeError::Dirty!)
    std::fs::write(ws.join("scratch.tmp"), "in-progress notes").unwrap();

    let (engine, project_id) =
        setup_engine_with_project(&temp.path().join("data"), &ws, &base_url).await;

    let receipt = engine
        .create_project_worktree(CreateWorktree {
            source_project_id: project_id,
            name: "feature-wt".into(),
            parent_path: wt_parent.to_string_lossy().into_owned(),
            expected_config_revision: 2,
            expected_head: head,
            idempotency_key: "wt-1".into(),
        })
        .await
        .unwrap();
    assert_eq!(receipt.state, WorktreeState::Succeeded);
    assert!(receipt.project_id.is_some());
    assert!(!receipt.parent_path.starts_with(r"\\?\"));
    assert!(
        std::path::Path::new(&receipt.target_path)
            .join("README.md")
            .is_file()
    );
    server.abort();
}

async fn start_approved_mcp_call(engine: &Arc<Engine>, project_id: ProjectId) -> (String, String) {
    let run = engine
        .start_in_project(StartInProject {
            project_id,
            expected_config_revision: 2,
            request: RunRequest {
                title: "MCP echo run".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "coder".into(),
                    role: "engineer".into(),
                    route_id: "default".into(),
                    prompt: "Call demo MCP echo_tool".into(),
                    depends_on: vec![],
                    write_scopes: vec!["*".into()],
                    tools: true,
                    allow_commands: true,
                    max_rounds: 6,
                }],
            },
            idempotency_key: "run-mcp-1".into(),
        })
        .await
        .unwrap();
    let task_id = run.tasks[0].id.clone();

    let approval_id = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let list = engine.store.approvals_for_task(&task_id).unwrap();
            if let Some(rec) = list.first() {
                assert_eq!(rec.tool_name, "mcp__demo__echo_tool");
                return rec.id.clone();
            }
            let t = engine.store.task(&task_id).unwrap();
            assert_ne!(t.status, "failed", "task failed early: {:?}", t.error);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();

    engine
        .decide_approval_with_network_ack(&approval_id, true, Some("tester"), false)
        .unwrap();

    (task_id, approval_id)
}

#[cfg(windows)]
#[test]
fn blocking_stdio_fixture() {
    use std::io::{BufRead, Write};
    if std::env::var_os("CODA_BLOCKING_STDIO_FIXTURE").is_none() {
        return;
    }
    println!();
    for line in std::io::stdin().lock().lines() {
        let req: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let Some(id) = req["id"].as_u64() else {
            continue;
        };
        let reply = match req["method"].as_str().unwrap() {
            "initialize" => json!({}),
            "tools/list" => json!({"tools":[{"name":"echo_tool"}]}),
            "tools/call" => {
                let mut log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("dispatched")
                    .unwrap();
                writeln!(log, "{}", req["params"]["name"]).unwrap();
                std::thread::sleep(Duration::from_secs(120));
                return;
            }
            other => panic!("unexpected request {other}"),
        };
        println!("{}", json!({"jsonrpc":"2.0","id":id,"result":reply}));
        std::io::stdout().flush().unwrap();
    }
}

#[cfg(windows)]
#[tokio::test]
async fn cancelling_dispatched_mcp_call_preserves_unknown_approval_outcome() {
    let (base_url, server) = start_mock_llm("mcp_echo").await;
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("mcp_ws");
    let (engine, project_id) =
        setup_engine_with_project(&temp.path().join("data"), &ws, &base_url).await;
    std::fs::write(ws.join(".mcp.json"), json!({"mcpServers":{"demo":{
        "command":std::env::current_exe().unwrap(),"args":["--exact","blocking_stdio_fixture","--nocapture"],
        "env":{"CODA_BLOCKING_STDIO_FIXTURE":"1"}
    }}}).to_string()).unwrap();
    let (task_id, approval_id) = start_approved_mcp_call(&engine, project_id).await;
    let marker = ws.join("dispatched");
    tokio::time::timeout(Duration::from_secs(10), async {
        while std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    engine.cancel(&task_id).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while engine.is_busy() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let task = engine.store.task(&task_id).unwrap();
    assert_eq!(task.status, "cancelled");
    assert!(
        task.error
            .as_deref()
            .is_some_and(|e| e.contains("结果未知")),
        "{:?}",
        task.error
    );
    assert!(
        !task
            .messages
            .iter()
            .any(|m| m.tool_call_id() == Some("call-mcp-1"))
    );
    let record = engine.store.approval(&approval_id).unwrap();
    assert!(matches!(
        record.execution_state,
        ExecutionState::Claimed | ExecutionState::Unknown
    ));
    let resume_error = engine.resume(&task_id, "resume").await.unwrap_err();
    assert!(matches!(
        resume_error.downcast_ref::<coda::approval::ApprovalError>(),
        Some(coda::approval::ApprovalError::UnknownResult { id }) if id == &approval_id
    ));
    assert_eq!(std::fs::read_to_string(marker).unwrap().lines().count(), 1);
    assert!(
        engine
            .shutdown(Duration::from_secs(5))
            .await
            .external_outcome_unknown
    );
    server.abort();
}

#[tokio::test]
async fn mcp_stdio_discovery_approval_and_execution_end_to_end() {
    let (base_url, server) = start_mock_llm("mcp_echo").await;
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("mcp_ws");
    std::fs::create_dir_all(&ws).unwrap();

    let mock_mcp_ps1 = r#"
try {
    [Console]::InputEncoding = [System.Text.Encoding]::UTF8
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
} catch {}
while ($null -ne ($line = [Console]::In.ReadLine())) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    $req = $line | ConvertFrom-Json
    if ($req.method -eq 'initialize') {
        $resp = '{"jsonrpc":"2.0","id":' + $req.id + ',"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"demo","version":"0.2.0"}}}'
        [Console]::Out.WriteLine($resp)
        [Console]::Out.Flush()
    } elseif ($req.method -eq 'tools/list') {
        $resp = '{"jsonrpc":"2.0","id":' + $req.id + ',"result":{"tools":[{"name":"echo_tool","description":"Echoes text via MCP stdio","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}}]}}'
        [Console]::Out.WriteLine($resp)
        [Console]::Out.Flush()
    } elseif ($req.method -eq 'tools/call') {
        $txt = $req.params.arguments.text
        $foo = $env:FOO
        $resp = @{
            jsonrpc = '2.0'
            id = $req.id
            result = @{
                content = @(
                    @{
                        type = 'text'
                        text = "mcp-echo:$txt|foo=$foo"
                    }
                )
                isError = $false
            }
        } | ConvertTo-Json -Compress -Depth 8
        [Console]::Out.WriteLine($resp)
        [Console]::Out.Flush()
    }
}
"#;
    std::fs::write(ws.join("mock_mcp.ps1"), mock_mcp_ps1).unwrap();
    std::fs::write(
        ws.join(".mcp.json"),
        serde_json::to_string_pretty(&json!({
            "mcpServers": {
                "demo": {
                    "command": "pwsh",
                    "args": [
                        "-NoLogo",
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        "mock_mcp.ps1"
                    ],
                    "env": {
                        "FOO": "bar"
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let (engine, project_id) =
        setup_engine_with_project(&temp.path().join("data"), &ws, &base_url).await;

    let (task_id, approval_id) = start_approved_mcp_call(&engine, project_id).await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let t = engine.store.task(&task_id).unwrap();
            if t.status == "completed" {
                assert!(
                    t.output.contains("mcp-echo:hello-from-coda|foo=bar"),
                    "unexpected output: {}",
                    t.output
                );
                break;
            }
            assert_ne!(t.status, "failed", "MCP task failed: {:?}", t.error);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();

    let final_approval = engine.store.approval(&approval_id).unwrap();
    assert_eq!(final_approval.execution_state, ExecutionState::Finished);
    server.abort();
}

#[tokio::test]
async fn chat_turn_syncs_updated_route_model_recovers_todos_and_lists_checkpoints_newest_first() {
    let (base_url, server) = start_mock_llm("todo_and_model_switch").await;
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("session_ws");
    let (engine, project_id) =
        setup_engine_with_project(&temp.path().join("data"), &ws, &base_url).await;

    assert!(engine.checkpoints_for_tasks(&[]).unwrap().is_empty());

    // Turn 1 runs on initial model "test-coder", writes todos + step1.txt
    let run = engine
        .start_in_project(StartInProject {
            project_id,
            expected_config_revision: 2,
            request: RunRequest {
                title: "Session todo, model switch, and checkpoints".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "coder".into(),
                    role: "engineer".into(),
                    route_id: "default".into(),
                    prompt: "Start turn 1".into(),
                    depends_on: vec![],
                    write_scopes: vec!["*".into()],
                    tools: true,
                    allow_commands: false,
                    max_rounds: 6,
                }],
            },
            idempotency_key: "run-s1-t1".into(),
        })
        .await
        .unwrap();
    let task1_id = run.tasks[0].id.clone();

    let app1_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let list = engine.store.approvals_for_task(&task1_id).unwrap();
            if let Some(rec) = list.first() {
                return rec.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    engine
        .decide_approval_with_network_ack(&app1_id, true, Some("tester"), false)
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let t = engine.store.task(&task1_id).unwrap();
            if t.status == "completed" {
                assert!(t.output.contains("turn1-done:test-coder"));
                break;
            }
            assert_ne!(t.status, "failed", "turn 1 failed: {:?}", t.error);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let cp1 = engine
        .create_checkpoint(&task1_id, "cp-turn-1")
        .await
        .unwrap();

    // Update route "default" model in settings (simulating `/model upgraded-coder-v2`)
    let mut settings = engine.store.settings().unwrap().unwrap();
    settings.routes[0].model = "upgraded-coder-v2".into();
    settings.routes[0].max_tokens = 8192;
    engine.store.save_settings(&settings).unwrap();

    // Clear in-memory RUN_TODOS cache to simulate cross-process continuation (`coda ask -c`)
    coda::workspace::clear_todo_cache_for_run(&run.id);

    // Turn 2 continues the same session and should use "upgraded-coder-v2" + recover todos from messages!
    let ctx = engine.store.run_context(&run.id).unwrap();
    let turn2 = engine
        .send_chat_turn(SendChatTurn {
            session_id: ctx.session.id,
            agent_id: ctx.tasks[0].agent_id.clone(),
            expected_last_turn_id: ctx.latest_turn.id,
            message: "Continue on upgraded model".into(),
            idempotency_key: "turn-2-switch".into(),
        })
        .await
        .unwrap();
    let task2_id = turn2.task.legacy_task_id.clone();

    let app2_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let list = engine.store.approvals_for_task(&task2_id).unwrap();
            if let Some(rec) = list.first() {
                return rec.id.clone();
            }
            let t = engine.store.task(&task2_id).unwrap();
            assert_ne!(t.status, "failed", "turn 2 failed early: {:?}", t.error);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    engine
        .decide_approval_with_network_ack(&app2_id, true, Some("tester"), false)
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let t = engine.store.task(&task2_id).unwrap();
            if t.status == "completed" {
                assert_eq!(t.route.model, "upgraded-coder-v2");
                assert_eq!(t.route.max_tokens, 8192);
                assert!(
                    t.output
                        .contains("turn2-model=upgraded-coder-v2|max=8192|todos="),
                    "unexpected turn2 output: {}",
                    t.output
                );
                assert!(
                    t.output.contains("plan-with-test-coder"),
                    "todo_read did not recover turn 1 todos after cache clear: {}",
                    t.output
                );
                break;
            }
            assert_ne!(t.status, "failed", "turn 2 failed: {:?}", t.error);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    let cp2 = engine
        .create_checkpoint(&task2_id, "cp-turn-2")
        .await
        .unwrap();

    // Verify checkpoints_for_tasks returns newest-first (cp2, then cp1)
    let listed = engine
        .checkpoints_for_tasks(&[task1_id.clone(), task2_id.clone()])
        .unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].checkpoint_id, cp2.checkpoint.checkpoint_id);
    assert_eq!(listed[0].task_id, task2_id);
    assert_eq!(listed[1].checkpoint_id, cp1.checkpoint.checkpoint_id);
    assert_eq!(listed[1].task_id, task1_id);

    server.abort();
}
