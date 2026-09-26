use axum::{Json, Router, extract::State, routing::post};
use peachsh::domain::{LifecycleStatus, TaskId, Turn, TurnId, TurnTask, send_chat_turn_hash};
use peachsh::{
    approval::ExecutionState,
    domain::{
        ChatTurnError, Route, RunRequest, SendChatTurn, SessionKind, Settings, Task, TaskSpec,
    },
    engine::Engine,
    store::Store,
    workspace_changes::RestoreStatus,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Default)]
struct Provider {
    replies: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}
async fn respond(
    State(provider): State<Provider>,
    Json(body): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    provider.requests.lock().unwrap().push(body);
    let delta = provider
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| json!({"content":"done"}));
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}
fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
}
struct Harness {
    dir: tempfile::TempDir,
    engine: Arc<Engine>,
    provider: Provider,
    server: tokio::task::JoinHandle<()>,
    run: String,
    first: String,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Harness {
    fn db(&self) -> Connection {
        Connection::open(self.dir.path().join("tools.db")).unwrap()
    }
    fn queue(&self, replies: Vec<Value>) {
        self.provider.replies.lock().unwrap().extend(replies);
    }
    fn command(&self, key: &str) -> SendChatTurn {
        let context = self.engine.store.run_context(&self.run).unwrap();
        SendChatTurn {
            session_id: context.session.id,
            agent_id: context.tasks[0].agent_id.clone(),
            expected_last_turn_id: context.latest_turn.id,
            message: "continue".into(),
            idempotency_key: key.into(),
        }
    }
    fn count(&self, table: &str) -> i64 {
        self.db()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }
    async fn settle(&self, id: &str, approve: bool) -> Task {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                for approval in self.engine.store.pending_approvals_for_task(id).unwrap() {
                    self.engine
                        .decide_approval(&approval.id, approve, Some("test"))
                        .unwrap();
                }
                let task = self.engine.store.task(id).unwrap();
                if !matches!(task.status.as_str(), "queued" | "running") && !self.engine.is_busy() {
                    return task;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("task settles")
    }
    async fn pending(&self, id: &str) -> peachsh::approval::ApprovalRecord {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(record) = self
                    .engine
                    .store
                    .pending_approvals_for_task(id)
                    .unwrap()
                    .into_iter()
                    .next()
                {
                    return record;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("approval becomes pending")
    }
}
async fn start(replies: Vec<Value>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.txt"), "before").unwrap();
    let provider = Provider::default();
    provider.replies.lock().unwrap().extend(replies);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(respond))
        .with_state(provider.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = Arc::new(Store::open(&dir.path().join("tools.db")).unwrap());
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
    store.put_secret("route", "fake-tool-session-key").unwrap();
    let engine = Engine::new(store, 2).unwrap();
    let run = engine
        .start_idempotent(
            RunRequest {
                title: "tool chat".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "worker".into(),
                    role: "worker".into(),
                    route_id: "route".into(),
                    prompt: "start".into(),
                    depends_on: vec![],
                    write_scopes: vec!["src".into()],
                    tools: true,
                    allow_commands: false,
                    max_rounds: 8,
                }],
            },
            "start",
        )
        .await
        .unwrap();
    Harness {
        dir,
        engine,
        provider,
        server,
        run: run.id,
        first: run.tasks[0].id.clone(),
    }
}
fn write(id: &str, content: &str) -> Value {
    call(
        id,
        "write_file",
        json!({"path":"src/a.txt","content":content}),
    )
}

#[tokio::test]
async fn n03_closed_history_inherits_permissions_and_creates_fresh_approval() {
    let h = start(vec![
        call("read-1", "read_file", json!({"path":"src/a.txt"})),
        write("write-1", "first"),
        json!({"content":"done"}),
    ])
    .await;
    let original = h.settle(&h.first, true).await;
    assert_eq!(original.status, "completed");
    let old_approval = h
        .engine
        .store
        .approvals_for_task(&h.first)
        .unwrap()
        .remove(0);
    h.queue(vec![write("write-2", "second"), json!({"content":"done"})]);
    let cmd = h.command("next");
    let next = h.engine.send_chat_turn(cmd.clone()).await.unwrap();
    let approval = h.pending(&next.task.legacy_task_id).await;
    assert_ne!(approval.id, old_approval.id);
    assert_eq!(
        std::fs::read_to_string(h.dir.path().join("src/a.txt")).unwrap(),
        "first"
    );
    let done = h.settle(&next.task.legacy_task_id, true).await;
    assert_eq!(done.status, "completed");
    assert!(done.messages.starts_with(&original.messages));
    assert_eq!(done.spec.write_scopes, original.spec.write_scopes);
    assert_eq!(done.spec.tools, original.spec.tools);
    assert_eq!(done.spec.allow_commands, original.spec.allow_commands);
    assert_eq!(done.workspace, original.workspace);
    assert_eq!(
        h.engine.store.approvals_for_task(&done.id).unwrap().len(),
        1
    );
    assert_eq!(h.count("workspace_changes"), 2);
    let calls_before = h.provider.requests.lock().unwrap().len();
    let replay = h.engine.send_chat_turn(cmd).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.task.id, next.task.id);
    assert_eq!(h.provider.requests.lock().unwrap().len(), calls_before);
    h.queue(vec![write("write-1", "must-not-run")]);
    let reused = h
        .engine
        .send_chat_turn(h.command("reused-id"))
        .await
        .unwrap();
    assert_eq!(
        h.settle(&reused.task.legacy_task_id, true).await.status,
        "failed"
    );
    assert_eq!(
        std::fs::read_to_string(h.dir.path().join("src/a.txt")).unwrap(),
        "second"
    );
    assert_eq!(h.count("workspace_changes"), 2);
}

#[tokio::test]
async fn n04_restore_then_new_task_uses_restored_bytes_and_host_fact() {
    let h = start(vec![write("write-1", "first"), json!({"content":"done"})]).await;
    let original = h.settle(&h.first, true).await;
    let receipt = h.engine.restore(&h.first).await.unwrap();
    assert_eq!(receipt.status, RestoreStatus::Complete);
    h.queue(vec![write("write-2", "second"), json!({"content":"done"})]);
    let cmd = h.command("after-restore");
    let next = h.engine.send_chat_turn(cmd.clone()).await.unwrap();
    let done = h.settle(&next.task.legacy_task_id, true).await;
    assert_eq!(done.status, "completed");
    assert!(done.messages.starts_with(&original.messages));
    let fact = &done.messages[original.messages.len()];
    assert_eq!(fact["role"], "user");
    assert!(
        fact["content"]
            .as_str()
            .unwrap()
            .contains(receipt.restore_id.as_ref().unwrap())
    );
    let before: String = h
        .db()
        .query_row(
            "SELECT before_digest FROM workspace_changes WHERE task_id=?1",
            [&done.id],
            |row| row.get(0),
        )
        .unwrap();
    use sha2::Digest;
    assert_eq!(before, format!("{:x}", sha2::Sha256::digest(b"before")));
    assert_eq!(
        h.engine.restore(&done.id).await.unwrap().status,
        RestoreStatus::Complete
    );
    assert_eq!(
        std::fs::read(h.dir.path().join("src/a.txt")).unwrap(),
        b"before"
    );
    assert!(
        h.engine
            .resume(&h.first, "old task must stay sealed")
            .await
            .is_err()
    );
    assert!(h.engine.send_chat_turn(cmd).await.unwrap().replayed);
    assert_eq!(
        h.engine.store.task(&done.id).unwrap().messages,
        done.messages
    );
}

#[tokio::test]
async fn n05_all_historical_unresolved_states_block_but_replay_survives() {
    let h = start(vec![write("write-1", "first"), json!({"content":"done"})]).await;
    assert_eq!(h.settle(&h.first, true).await.status, "completed");
    let cmd = h.command("settled-successor");
    let next = h.engine.send_chat_turn(cmd.clone()).await.unwrap();
    assert_eq!(
        h.settle(&next.task.legacy_task_id, true).await.status,
        "completed"
    );
    let baseline = h.count("turns");
    for (status, execution) in [
        ("pending", "not_started"),
        ("approved", "not_started"),
        ("approved", "claimed"),
        ("approved", "unknown"),
    ] {
        h.db()
            .execute(
                "UPDATE approvals SET status=?1,execution_state=?2 WHERE task_id=?3",
                params![status, execution, h.first],
            )
            .unwrap();
        let error = h
            .engine
            .send_chat_turn(h.command(&format!("blocked-{status}-{execution}")))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ChatTurnError>(),
            Some(&ChatTurnError::UnresolvedEffects)
        );
        assert!(h.engine.send_chat_turn(cmd.clone()).await.unwrap().replayed);
        assert_eq!(h.count("turns"), baseline);
    }
    h.db()
        .execute(
            "UPDATE approvals SET status='approved',execution_state='finished' WHERE task_id=?1",
            [&h.first],
        )
        .unwrap();
    for state in ["prepared", "unknown"] {
        h.db()
            .execute(
                "UPDATE workspace_changes SET state=?1 WHERE task_id=?2",
                params![state, h.first],
            )
            .unwrap();
        assert_eq!(
            h.engine
                .send_chat_turn(h.command(&format!("blocked-change-{state}")))
                .await
                .unwrap_err()
                .downcast_ref::<ChatTurnError>(),
            Some(&ChatTurnError::UnresolvedEffects)
        );
    }
    h.db()
        .execute(
            "UPDATE workspace_changes SET state='finished' WHERE task_id=?1",
            [&h.first],
        )
        .unwrap();
    for status in ["claimed", "partial", "unknown"] {
        h.db()
            .execute(
                "INSERT INTO workspace_restores(id,task_id,status,created_at) VALUES (?1,?2,?3,1)",
                params![status, h.first, status],
            )
            .unwrap();
        assert_eq!(
            h.engine
                .send_chat_turn(h.command(&format!("blocked-restore-{status}")))
                .await
                .unwrap_err()
                .downcast_ref::<ChatTurnError>(),
            Some(&ChatTurnError::UnresolvedEffects)
        );
        h.db()
            .execute("DELETE FROM workspace_restores WHERE id=?1", [status])
            .unwrap();
    }
    assert_eq!(h.count("turns"), baseline);
    let allowed = h
        .engine
        .send_chat_turn(h.command("allowed-again"))
        .await
        .unwrap();
    assert_eq!(
        h.settle(&allowed.task.legacy_task_id, true).await.status,
        "completed"
    );
}

#[tokio::test]
async fn n06_malformed_and_changed_origin_history_cannot_append() {
    let h = start(vec![
        call("read-1", "read_file", json!({"path":"src/a.txt"})),
        json!({"content":"done"}),
    ])
    .await;
    let original = h.settle(&h.first, true).await;
    let tool_index = original
        .messages
        .iter()
        .position(|message| message["role"] == "tool")
        .unwrap();
    let mut cases = Vec::new();
    let mut missing = original.messages.clone();
    missing.remove(tool_index);
    cases.push(missing);
    let mut duplicate = original.messages.clone();
    duplicate.insert(tool_index, duplicate[tool_index].clone());
    cases.push(duplicate);
    let mut orphan = original.messages.clone();
    orphan[tool_index]["tool_call_id"] = json!("orphan");
    cases.push(orphan);
    let mut changed = original.messages.clone();
    changed[tool_index]["content"] = json!("forged result");
    cases.push(changed);
    for (index, messages) in cases.into_iter().enumerate() {
        h.db()
            .execute(
                "UPDATE tasks SET value=json_set(value,'$.messages',json(?2)) WHERE id=?1",
                params![h.first, serde_json::to_string(&messages).unwrap()],
            )
            .unwrap();
        assert!(
            h.engine
                .send_chat_turn(h.command(&format!("malformed-{index}")))
                .await
                .is_err()
        );
        assert_eq!(h.count("turns"), 1);
    }
}

#[tokio::test]
async fn n03_denied_write_history_can_continue_without_inheriting_approval() {
    let h = start(vec![write("denied-1", "denied"), json!({"content":"done"})]).await;
    assert_eq!(h.settle(&h.first, false).await.status, "completed");
    let next = h
        .engine
        .send_chat_turn(h.command("after-denial"))
        .await
        .unwrap();
    assert_eq!(
        h.settle(&next.task.legacy_task_id, true).await.status,
        "completed"
    );
    assert!(
        h.engine
            .store
            .approvals_for_task(&next.task.legacy_task_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(h.dir.path().join("src/a.txt")).unwrap(),
        "before"
    );
}

#[tokio::test]
async fn n09_stop_before_approval_claim_has_no_write_or_claim() {
    for unload in [false, true] {
        let h = start(vec![
            write("write-1", "not-written"),
            json!({"content":"done"}),
        ])
        .await;
        let pending = h.pending(&h.first).await;
        if unload {
            h.engine.unload_builtin_files().unwrap();
        } else {
            h.engine.disable_builtin_files().unwrap();
        }
        h.engine
            .decide_approval(&pending.id, true, Some("test"))
            .unwrap();
        assert_eq!(h.settle(&h.first, true).await.status, "failed");
        assert_eq!(
            h.engine
                .store
                .approval(&pending.id)
                .unwrap()
                .execution_state,
            ExecutionState::NotStarted
        );
        assert_eq!(h.count("workspace_changes"), 0);
        assert_eq!(
            std::fs::read_to_string(h.dir.path().join("src/a.txt")).unwrap(),
            "before"
        );
    }
}

#[tokio::test]
async fn n05_legacy_untracked_effect_is_not_cleared_by_interrupted_placeholder() {
    // Construct the persisted shape left by a pre-approval Engine, then by
    // legacy resume reconciliation. This does not run an old binary crash.
    for name in ["write_file", "run_command"] {
        let h = start(vec![json!({"content":"done"})]).await;
        let mut original = h.settle(&h.first, true).await;
        let content =
            "Execution was interrupted. Inspect files before repeating a write or command.";
        original.messages.push(json!({"role":"assistant","tool_calls":[{"id":"old-effect","type":"function","function":{"name":name,"arguments":"{}"}}]}));
        original
            .messages
            .push(json!({"role":"tool","tool_call_id":"old-effect","content":content}));
        original
            .messages
            .push(json!({"role":"assistant","content":"resumed and done"}));
        h.db()
            .execute(
                "UPDATE tasks SET value=?2 WHERE id=?1",
                params![h.first, serde_json::to_string(&original).unwrap()],
            )
            .unwrap();
        h.engine
            .store
            .event(
                &h.first,
                "tool_result",
                json!({"name":name,"result":content}),
            )
            .unwrap();
        let calls = h.provider.requests.lock().unwrap().len();
        assert_eq!(
            h.engine
                .send_chat_turn(h.command("legacy-effect"))
                .await
                .unwrap_err()
                .downcast_ref::<ChatTurnError>(),
            Some(&ChatTurnError::UnresolvedEffects)
        );
        assert_eq!(h.count("turns"), 1);
        assert_eq!(h.count("tasks"), 1);
        assert_eq!(h.provider.requests.lock().unwrap().len(), calls);
    }
}

#[tokio::test]
async fn n06_store_rechecks_every_inherited_permission_and_original_approval_owner() {
    let h = start(vec![write("write-1", "first"), json!({"content":"done"})]).await;
    let original = h.settle(&h.first, true).await;
    let cmd = h.command("store-tampering");
    let ctx = h.engine.store.run_context(&h.run).unwrap();
    let turn = Turn {
        id: TurnId("candidate-turn".into()),
        session_id: cmd.session_id.clone(),
        project_id: ctx.project.id,
        status: LifecycleStatus::Queued,
        request_hash: send_chat_turn_hash(&cmd).unwrap(),
        idempotency_key: Some(cmd.idempotency_key.clone()),
        created_at: original.created_at,
        updated_at: original.created_at,
    };
    let projected = TurnTask {
        id: TaskId("candidate-task".into()),
        turn_id: turn.id.clone(),
        session_id: cmd.session_id.clone(),
        agent_id: cmd.agent_id.clone(),
        legacy_task_id: "candidate-task".into(),
        depends_on: vec![],
        status: LifecycleStatus::Queued,
        created_at: original.created_at,
        updated_at: original.created_at,
    };
    let mut candidate = original.clone();
    candidate.id = projected.legacy_task_id.clone();
    candidate.status = "queued".into();
    candidate.output.clear();
    candidate.usage = Value::Null;
    candidate.error = None;
    candidate.spec.prompt = cmd.message.clone();
    candidate
        .messages
        .push(json!({"role":"user","content":cmd.message}));
    for field in ["route", "scopes", "tools", "commands", "workspace"] {
        let mut tampered = candidate.clone();
        match field {
            "route" => tampered.route.model = "other-model".into(),
            "scopes" => tampered.spec.write_scopes = vec!["*".into()],
            "tools" => tampered.spec.tools = false,
            "commands" => tampered.spec.allow_commands = true,
            "workspace" => tampered.workspace = h.dir.path().join("src").to_string_lossy().into(),
            _ => unreachable!(),
        }
        let error = h
            .engine
            .store
            .append_chat_turn(
                &turn,
                &projected,
                &tampered,
                &cmd.idempotency_key,
                &cmd.expected_last_turn_id,
            )
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ChatTurnError>(),
            Some(&ChatTurnError::PredecessorChanged),
            "{field}"
        );
        assert_eq!(h.count("turns"), 1);
        assert_eq!(h.count("tasks"), 1);
    }
    let next = h
        .engine
        .send_chat_turn(h.command("real-successor"))
        .await
        .unwrap();
    assert_eq!(
        h.settle(&next.task.legacy_task_id, true).await.status,
        "completed"
    );
    // A descendant's copied call cannot acquire a new authorization source.
    h.db().execute("INSERT INTO approvals(id,tool_call_id,task_id,turn_id,session_id,tool_name,args_digest,binding_digest,workspace,write_scopes,allow_commands,preview,status,execution_state,created_at,decided_at,decided_by) SELECT 'wrong-owner',tool_call_id,?1,?2,session_id,tool_name,args_digest,binding_digest,workspace,write_scopes,allow_commands,preview,'denied','not_started',created_at,decided_at,decided_by FROM approvals WHERE task_id=?3",params![next.task.legacy_task_id,next.turn.id.0,h.first]).unwrap();
    assert_eq!(
        h.engine
            .send_chat_turn(h.command("wrong-approval-owner"))
            .await
            .unwrap_err()
            .downcast_ref::<ChatTurnError>(),
        Some(&ChatTurnError::CorruptState)
    );
    assert_eq!(h.count("turns"), 2);
}

#[tokio::test]
async fn n07_tool_chat_same_key_race_and_active_scope_conflict() {
    let h = start(vec![write("write-1", "first"), json!({"content":"done"})]).await;
    let original = h.settle(&h.first, true).await;
    let cmd = h.command("concurrent");
    let (left, right) = tokio::join!(
        h.engine.send_chat_turn(cmd.clone()),
        h.engine.send_chat_turn(cmd)
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert!(left.replayed ^ right.replayed);
    assert_eq!(left.turn.id, right.turn.id);
    assert_eq!(
        h.settle(&left.task.legacy_task_id, true).await.status,
        "completed"
    );
    assert_eq!(h.count("turns"), 2);
    assert_eq!(h.count("workspace_changes"), 1);
    h.queue(vec![
        write("other-write", "blocked-scope"),
        json!({"content":"done"}),
    ]);
    let other = h
        .engine
        .start(RunRequest {
            title: "other writer".into(),
            kind: SessionKind::Team,
            tasks: vec![original.spec],
        })
        .await
        .unwrap();
    h.pending(&other.tasks[0].id).await;
    let turns = h.count("turns");
    let requests = h.provider.requests.lock().unwrap().len();
    let error = h
        .engine
        .send_chat_turn(h.command("scope-conflict"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("同一范围"));
    assert_eq!(h.count("turns"), turns);
    assert_eq!(h.provider.requests.lock().unwrap().len(), requests);
    h.engine.cancel(&other.tasks[0].id).unwrap();
    h.settle(&other.tasks[0].id, false).await;
}

#[tokio::test]
async fn n09_new_provider_definitions_and_calls_revoke_after_disable_and_unload() {
    for unload in [false, true] {
        let h = start(vec![json!({"content":"done"})]).await;
        assert_eq!(h.settle(&h.first, true).await.status, "completed");
        if unload {
            h.engine.unload_builtin_files().unwrap();
        } else {
            h.engine.disable_builtin_files().unwrap();
        }
        h.queue(vec![call(
            "revoked-read",
            "read_file",
            json!({"path":"src/a.txt"}),
        )]);
        let next = h
            .engine
            .send_chat_turn(h.command("after-stop"))
            .await
            .unwrap();
        assert_eq!(
            h.settle(&next.task.legacy_task_id, true).await.status,
            "failed"
        );
        let requests = h.provider.requests.lock().unwrap();
        let definitions = requests.last().unwrap()["tools"].as_array().unwrap();
        assert!(definitions.iter().all(|item| !matches!(
            item["function"]["name"].as_str(),
            Some("read_file" | "write_file")
        )));
        drop(requests);
        let events = h.engine.store.events(&h.run, 0).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "tool_start" || event.kind == "tool_result")
                .count(),
            0
        );
    }
}
