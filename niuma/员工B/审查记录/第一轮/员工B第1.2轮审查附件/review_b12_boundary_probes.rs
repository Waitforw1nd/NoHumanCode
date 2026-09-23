// Isolated review evidence; never copied into the shared formal tests.
include!("session_turns.rs");

fn review_all_counts(h: &Harness) -> Vec<i64> {
    let db = open_db(h);
    [
        "projects", "sessions", "agents", "runs", "turns", "tasks",
        "turn_tasks", "turn_task_dependencies", "idempotency_records",
        "idempotency", "events",
    ].iter().map(|table| {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0)).unwrap()
    }).collect()
}

#[tokio::test]
async fn review_b12_tool_history_rejection_has_no_rows_at_either_boundary() {
    for function in [json!({}), json!({"name":"read_file","arguments":"{}"})] {
        let h = started().await;
        let mut previous = h.engine.store.task(&h.first.task.legacy_task_id).unwrap();
        previous.messages.last_mut().unwrap()["tool_calls"] = json!([
            {"id":"review-call","type":"function","function":function}
        ]);
        previous.messages.push(json!({"role":"tool","tool_call_id":"review-call","content":"result"}));
        previous.messages.push(json!({"role":"assistant","content":"finished"}));
        rewrite_messages(&h, previous.messages.clone());
        let before = review_all_counts(&h);
        let calls_before = calls(&h);
        let cmd = command(&h.session.id, &h.agent.id, &h.first.turn.id, "valid text", "boundary-engine");
        assert_eq!(error_of(h.engine.send_chat_turn(cmd).await), ChatTurnError::UnsupportedSession);
        tokio::task::yield_now().await;
        assert_eq!(review_all_counts(&h), before);
        assert_eq!(calls(&h), calls_before);
        assert!(!h.engine.is_busy());

        let cmd = command(&h.session.id, &h.agent.id, &h.first.turn.id, "valid text", "boundary-store");
        let mut candidate = previous.clone();
        candidate.id = "review-boundary-task".into();
        candidate.status = "queued".into();
        candidate.spec.prompt = cmd.message.clone();
        candidate.output.clear();
        candidate.error = None;
        candidate.usage = Value::Null;
        candidate.messages.push(json!({"role":"user","content":cmd.message}));
        let turn = Turn {
            id: TurnId("review-boundary-turn".into()),
            session_id: h.session.id.clone(), project_id: h.session.project_id.clone(),
            status: LifecycleStatus::Queued, request_hash: send_chat_turn_hash(&cmd).unwrap(),
            idempotency_key: Some(cmd.idempotency_key.clone()), created_at: now(), updated_at: now(),
        };
        let task = TurnTask {
            id: TaskId(candidate.id.clone()), turn_id: turn.id.clone(),
            session_id: h.session.id.clone(), agent_id: h.agent.id.clone(),
            legacy_task_id: candidate.id.clone(), depends_on: vec![], status: LifecycleStatus::Queued,
            created_at: now(), updated_at: now(),
        };
        let result = h.engine.store.append_chat_turn(&turn, &task, &candidate, &cmd.idempotency_key, &cmd.expected_last_turn_id);
        assert_eq!(error_of(result), ChatTurnError::UnsupportedSession);
        assert_eq!(review_all_counts(&h), before);
        assert_eq!(h.engine.store.task(&previous.id).unwrap().messages, previous.messages);
        assert_eq!(calls(&h), calls_before);
        assert!(!h.engine.is_busy());
    }
    eprintln!("paired malformed and complete tool histories rejected by Engine and Store; all 11 row counts unchanged");
}

#[tokio::test]
async fn review_b12_explicit_empty_tool_array_keeps_text_continuation() {
    let h = started().await;
    let mut messages = h.engine.store.task(&h.first.task.legacy_task_id).unwrap().messages;
    messages.last_mut().unwrap()["tool_calls"] = json!([]);
    rewrite_messages(&h, messages.clone());
    let calls_before = calls(&h);
    let receipt = h.engine.send_chat_turn(command(&h.session.id, &h.agent.id, &h.first.turn.id, "empty-array-compatible", "empty-array-key")).await.unwrap();
    let completed = wait_task(&h.engine, &receipt.task.legacy_task_id).await;
    assert_eq!(completed.status, "completed");
    assert!(!receipt.replayed);
    assert_eq!(receipt.turn.session_id, h.session.id);
    assert_eq!(receipt.task.agent_id, h.agent.id);
    assert_eq!(calls(&h), calls_before + 1);
    assert_eq!(&completed.messages[..messages.len()], messages.as_slice());
    eprintln!("explicit empty tool_calls array accepted; one new provider call; original prefix preserved");
}
