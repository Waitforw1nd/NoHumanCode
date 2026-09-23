// Review-only probes. Reuse B's valid fixture; never compiled in the shared workspace.
include!("session_turns.rs");

#[tokio::test]
async fn review_old_event_must_not_reopen_historical_turn() {
    let h = started().await;
    let second = h.engine.send_chat_turn(command(&h.session.id, &h.agent.id, &h.first.turn.id, "second", "review-second")).await.unwrap();
    wait_task(&h.engine, &second.task.legacy_task_id).await;
    h.engine.store.event(&h.first.task.legacy_task_id, "status", json!({"status":"completed"})).unwrap();
    let latest = h.engine.store.latest_committed_turn(&h.session.id).unwrap().unwrap();
    let mut historical = h.engine.store.task(&h.first.task.legacy_task_id).unwrap();
    historical.status = "queued".into();
    let resumed = h.engine.store.resume_task(&historical, "must reject");
    eprintln!("latest_is_old={}, historical_resume_accepted={}", latest.id == h.first.turn.id, resumed.is_ok());
    assert!(latest.id == second.turn.id && resumed.is_err(), "old status event changed commit order and allowed historical resume");
}

#[tokio::test]
async fn review_stale_predecessor_snapshot_must_be_rejected_or_refreshed() {
    let h = started().await;
    let previous = h.engine.store.task(&h.first.task.legacy_task_id).unwrap();
    let cmd = command(&h.session.id, &h.agent.id, &h.first.turn.id, "after-resume", "snapshot-race");
    let mut legacy = previous.clone();
    legacy.id = "snapshot-task".into();
    legacy.status = "queued".into();
    legacy.spec.prompt = cmd.message.clone();
    legacy.output.clear();
    legacy.usage = Value::Null;
    legacy.messages.push(json!({"role":"user","content":cmd.message}));
    let turn = Turn { id: TurnId("snapshot-turn".into()), session_id:h.session.id.clone(), project_id:h.session.project_id.clone(), status:LifecycleStatus::Queued, request_hash:send_chat_turn_hash(&cmd).unwrap(), idempotency_key:Some(cmd.idempotency_key.clone()), created_at:now(), updated_at:now() };
    let task = TurnTask { id:TaskId(legacy.id.clone()), turn_id:turn.id.clone(), session_id:h.session.id.clone(), agent_id:h.agent.id.clone(), legacy_task_id:legacy.id.clone(), depends_on:vec![], status:LifecycleStatus::Queued, created_at:now(), updated_at:now() };
    h.engine.resume(&previous.id, "intervening-resume-message").await.unwrap();
    let current = wait_task(&h.engine, &previous.id).await;
    assert!(current.messages.iter().any(|m| m["content"] == "intervening-resume-message"));
    let other = Store::open(&h._dir.path().join("turns.db")).unwrap();
    let accepted = other.append_chat_turn(&turn, &task, &legacy, &cmd.idempotency_key, &cmd.expected_last_turn_id);
    if let Err(error) = accepted { assert_eq!(error.downcast_ref::<ChatTurnError>(), Some(&ChatTurnError::PredecessorChanged)); return; }
    let saved = other.task(&legacy.id).unwrap();
    let preserved = saved.messages.iter().any(|m| m["content"] == "intervening-resume-message");
    eprintln!("stale_snapshot_accepted=true, intervening_history_preserved={preserved}");
    assert!(preserved, "append silently lost a legal resume that completed after candidate construction");
}

#[tokio::test]
async fn review_multiple_agents_must_be_refused() {
    let h = started().await;
    h.engine.store.insert_agent(&Agent { id:AgentId("extra-agent".into()), session_id:h.session.id.clone(), display_name:"extra".into(), role:"review".into(), created_at:now() }).unwrap();
    let before = table_counts(&h);
    let result = h.engine.send_chat_turn(command(&h.session.id, &h.agent.id, &h.first.turn.id, "must reject multi-agent", "extra-agent-key")).await;
    if let Ok(receipt) = &result { wait_task(&h.engine, &receipt.task.legacy_task_id).await; }
    eprintln!("multi_agent_accepted={}, turns_before={}, turns_after={}", result.is_ok(), before.4, table_counts(&h).4);
    assert!(result.is_err() && table_counts(&h) == before, "multi-agent Chat accepted");
}

#[tokio::test]
async fn review_malformed_message_must_fail_before_commit_and_provider() {
    let h = started().await;
    let mut previous = h.engine.store.task(&h.first.task.legacy_task_id).unwrap();
    previous.messages = vec![json!({"role":"assistant","tool_calls":{"id":"bad"}})];
    open_db(&h).execute("UPDATE tasks SET value=?2 WHERE id=?1", params![previous.id, serde_json::to_string(&previous).unwrap()]).unwrap();
    let before = table_counts(&h);
    let before_calls = calls(&h);
    let result = h.engine.send_chat_turn(command(&h.session.id, &h.agent.id, &h.first.turn.id, "valid short message", "bad-prefix-key")).await;
    if let Ok(receipt) = &result { wait_task(&h.engine, &receipt.task.legacy_task_id).await; }
    eprintln!("malformed_prefix_accepted={}, provider_call_delta={}", result.is_ok(), calls(&h)-before_calls);
    assert!(result.is_err() && table_counts(&h) == before && calls(&h) == before_calls, "malformed prefix persisted and dispatched");
}

#[tokio::test]
async fn review_wrong_workspace_must_not_be_silently_replaced() {
    let h = started().await;
    let mut previous = h.engine.store.task(&h.first.task.legacy_task_id).unwrap();
    previous.workspace = "X:/unrelated-project".into();
    open_db(&h).execute("UPDATE tasks SET value=?2 WHERE id=?1", params![previous.id, serde_json::to_string(&previous).unwrap()]).unwrap();
    let result = h.engine.send_chat_turn(command(&h.session.id, &h.agent.id, &h.first.turn.id, "workspace mismatch", "workspace-mismatch-key")).await;
    if let Ok(receipt) = &result { wait_task(&h.engine, &receipt.task.legacy_task_id).await; }
    eprintln!("wrong_predecessor_workspace_accepted={}", result.is_ok());
    assert!(result.is_err(), "wrong workspace was silently replaced with Project.root_path");
}

#[tokio::test]
async fn review_database_failure_must_not_be_classified_as_not_found() {
    let h = started().await;
    open_db(&h).execute("UPDATE agents SET created_at=-1 WHERE id=?1", params![h.agent.id.0]).unwrap();
    let error = h.engine.send_chat_turn(command(&h.session.id, &h.agent.id, &h.first.turn.id, "db broken", "db-failure-key")).await.unwrap_err();
    eprintln!("database_error_classification={:?}", error.downcast_ref::<ChatTurnError>());
    assert_ne!(error.downcast_ref::<ChatTurnError>(), Some(&ChatTurnError::NotFound), "persisted row decoding failure was discarded and mislabeled missing object");
}



#[tokio::test]
async fn review_b11_incomplete_function_must_not_dispatch() {
    let h = started().await;
    let mut previous = h.engine.store.task(&h.first.task.legacy_task_id).unwrap();
    assert!(previous.messages.iter().any(|m| m["role"] == "user"));
    assert_eq!(previous.messages.last().unwrap()["role"], "assistant");
    previous.messages.last_mut().unwrap()["tool_calls"] = json!([
        {"id":"incomplete-call","type":"function","function":{}}
    ]);
    previous.messages.push(json!({"role":"tool","tool_call_id":"incomplete-call","content":"result"}));
    previous.messages.push(json!({"role":"assistant","content":"finished"}));
    open_db(&h).execute("UPDATE tasks SET value=?2 WHERE id=?1", params![previous.id, serde_json::to_string(&previous).unwrap()]).unwrap();
    let before = table_counts(&h);
    let calls_before = calls(&h);
    let result = h.engine.send_chat_turn(command(&h.session.id, &h.agent.id, &h.first.turn.id, "short valid message", "incomplete-function-key")).await;
    if let Ok(receipt) = &result { wait_task(&h.engine, &receipt.task.legacy_task_id).await; }
    eprintln!("incomplete_function_accepted={}, provider_call_delta={}, turn_delta={}", result.is_ok(), calls(&h)-calls_before, table_counts(&h).4-before.4);
    assert!(result.is_err() && table_counts(&h) == before && calls(&h) == calls_before, "a paired call with no function name or arguments was persisted and dispatched");
}

#[tokio::test]
async fn review_b11_session_and_turn_decode_failures_are_not_missing() {
    let mut observed = Vec::new();
    for entity in ["session", "turn"] {
        let h = started().await;
        if entity == "session" {
            open_db(&h).execute("UPDATE sessions SET created_at=-1 WHERE id=?1", [&h.session.id.0]).unwrap();
        } else {
            open_db(&h).execute("UPDATE turns SET created_at=-1 WHERE id=?1", [&h.first.turn.id.0]).unwrap();
        }
        let error = h.engine.send_chat_turn(command(&h.session.id, &h.agent.id, &h.first.turn.id, "decode failure", "decode-key")).await.unwrap_err();
        let classified = error.downcast_ref::<ChatTurnError>().copied();
        eprintln!("{entity}_decode_classification={classified:?}");
        observed.push(classified);
    }
    assert!(observed.iter().all(|error| *error == Some(ChatTurnError::CorruptState)), "diagnostic promise not met for session/turn");
}

#[tokio::test]
async fn review_b11_partial_fixture_must_have_its_own_delta() {
    let h = started().await;
    let before_calls = calls(&h);
    let second = h.engine.send_chat_turn(command(&h.session.id, &h.agent.id, &h.first.turn.id, "stream-partial", "review-partial-key")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if h.engine.store.task(&second.task.legacy_task_id).unwrap().status == "running" && calls(&h) > before_calls { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let events = h.engine.store.events(&h.session.legacy_run_id, 0).unwrap();
    let all_deltas = events.iter().filter(|event| event.kind == "delta").count();
    let current_deltas = events.iter().filter(|event| event.kind == "delta" && event.task_id == second.task.legacy_task_id).count();
    let weak_engine = Arc::downgrade(&h.engine);
    drop(h);
    let old_executor_alive = weak_engine.upgrade().is_some_and(|engine| engine.is_busy());
    eprintln!("partial_fixture: run_deltas={all_deltas}, current_task_deltas={current_deltas}, old_executor_alive_after_drop={old_executor_alive}");
    assert!(current_deltas > 0 && !old_executor_alive, "the claimed restart fixture has no current-task partial and did not stop the old executor");
}

