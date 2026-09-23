use anyhow::Result;
use axum::{Json, Router, extract::State, routing::post};
use peachsh::{
    domain::*,
    engine::Engine,
    store::{IdempotencyConflict, Store},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;

#[derive(Clone)]
struct Probe {
    calls: Arc<AtomicUsize>,
    bodies: Arc<std::sync::Mutex<Vec<Value>>>,
}

async fn chat(State(probe): State<Probe>, Json(body): Json<Value>) -> String {
    probe.calls.fetch_add(1, Ordering::SeqCst);
    if body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .is_some_and(|message| message["content"] == "hold")
    {
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    probe.bodies.lock().unwrap().push(body);
    let chunk = json!({"choices":[{"delta":{"content":"reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

async fn mock_provider() -> (String, Probe) {
    let probe = Probe {
        calls: Arc::new(AtomicUsize::new(0)),
        bodies: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(probe.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/v1"), probe)
}

fn engine_at(dir: &Path, base: &str) -> Arc<Engine> {
    let store = Arc::new(Store::open(&dir.join("turns.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "chat-route".into(),
                name: "chat".into(),
                base_url: base.into(),
                model: "chat-model".into(),
                max_tokens: 128,
                parallel_limit: 1,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store
        .put_secret("route::chat-route", "synthetic-chat-key")
        .unwrap();
    Engine::new(store, 2).unwrap()
}

fn chat_request(prompt: &str) -> RunRequest {
    RunRequest {
        title: "连续对话".into(),
        kind: SessionKind::Chat,
        tasks: vec![TaskSpec {
            name: "chat-worker".into(),
            role: "对话助手".into(),
            route_id: "chat-route".into(),
            prompt: prompt.into(),
            depends_on: vec![],
            write_scopes: vec![],
            tools: false,
            allow_commands: false,
            max_rounds: 2,
        }],
    }
}

fn command(
    session: &SessionId,
    agent: &AgentId,
    previous: &TurnId,
    message: &str,
    key: &str,
) -> SendChatTurn {
    SendChatTurn {
        session_id: session.clone(),
        agent_id: agent.clone(),
        expected_last_turn_id: previous.clone(),
        message: message.into(),
        idempotency_key: key.into(),
    }
}

async fn wait_task(engine: &Engine, id: &str) -> Task {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let task = engine.store.task(id).unwrap();
            if !matches!(task.status.as_str(), "queued" | "running") && !engine.is_busy() {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("task did not settle")
}

struct Harness {
    _dir: TempDir,
    engine: Arc<Engine>,
    probe: Probe,
    session: Session,
    agent: Agent,
    first: ChatTurnReceipt,
}

async fn started() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let (base, probe) = mock_provider().await;
    let engine = engine_at(dir.path(), &base);
    let run = engine
        .start_idempotent(chat_request("第一轮"), "create-chat")
        .await
        .unwrap();
    let done = wait_task(&engine, &run.tasks[0].id).await;
    assert_eq!(done.status, "completed");
    let session = Connection::open(dir.path().join("turns.db")).unwrap();
    let session_id: String = session
        .query_row("SELECT id FROM sessions", [], |row| row.get(0))
        .unwrap();
    let agent_id: String = session
        .query_row("SELECT id FROM agents", [], |row| row.get(0))
        .unwrap();
    let turn_id: String = session
        .query_row("SELECT id FROM turns", [], |row| row.get(0))
        .unwrap();
    let domain_session = engine.store.session(&SessionId(session_id)).unwrap();
    let agent = engine.store.agent(&AgentId(agent_id)).unwrap();
    let turn = engine.store.turn(&TurnId(turn_id)).unwrap();
    let task = engine.store.turn_tasks(&turn.id).unwrap().remove(0);
    Harness {
        _dir: dir,
        engine,
        probe,
        session: domain_session,
        agent,
        first: ChatTurnReceipt {
            turn,
            task,
            replayed: false,
        },
    }
}

fn open_db(harness: &Harness) -> Connection {
    Connection::open(harness._dir.path().join("turns.db")).unwrap()
}

fn table_counts(harness: &Harness) -> (i64, i64, i64, i64, i64, i64) {
    let db = open_db(harness);
    let one = |sql: &str| db.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap();
    (
        one("SELECT count(*) FROM projects"),
        one("SELECT count(*) FROM sessions"),
        one("SELECT count(*) FROM agents"),
        one("SELECT count(*) FROM runs"),
        one("SELECT count(*) FROM turns"),
        one("SELECT count(*) FROM tasks"),
    )
}

fn event_count(harness: &Harness) -> i64 {
    open_db(harness)
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap()
}

fn calls(harness: &Harness) -> usize {
    harness.probe.calls.load(Ordering::SeqCst)
}

fn error_of<T: std::fmt::Debug>(result: Result<T>) -> ChatTurnError {
    result
        .expect_err("expected a classified chat error")
        .downcast::<ChatTurnError>()
        .expect("error must be ChatTurnError, not an unrelated failure")
}

#[tokio::test]
async fn t1_t2_three_turns_keep_identity_and_independent_results() {
    let harness = started().await;
    let before = table_counts(&harness);
    let first_task = harness
        .engine
        .store
        .task(&harness.first.task.legacy_task_id)
        .unwrap();
    let mut previous = harness.first.turn.id.clone();
    let mut receipts = vec![harness.first.clone()];
    for (index, message) in ["第二轮", "第三轮"].into_iter().enumerate() {
        let receipt = harness
            .engine
            .send_chat_turn(command(
                &harness.session.id,
                &harness.agent.id,
                &previous,
                message,
                &format!("turn-{}", index + 2),
            ))
            .await
            .unwrap();
        assert!(!receipt.replayed);
        let task = wait_task(&harness.engine, &receipt.task.legacy_task_id).await;
        assert_eq!(task.status, "completed");
        assert_eq!(task.output, "reply");
        assert_eq!(task.usage["total_tokens"], 4);
        previous = receipt.turn.id.clone();
        receipts.push(receipt);
    }
    assert_eq!(table_counts(&harness).0, before.0);
    assert_eq!(table_counts(&harness).1, before.1);
    assert_eq!(table_counts(&harness).2, before.2);
    assert_eq!(table_counts(&harness).3, before.3);
    assert_eq!(table_counts(&harness).4, 3);
    assert_eq!(table_counts(&harness).5, 3);
    let unchanged = harness.engine.store.task(&first_task.id).unwrap();
    assert_eq!(unchanged.output, first_task.output);
    assert_eq!(unchanged.usage, first_task.usage);
    assert_eq!(unchanged.status, "completed");
    let bodies = harness.probe.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    let third = bodies[2]["messages"].as_array().unwrap();
    let users: Vec<_> = third
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].as_str().unwrap())
        .collect();
    assert_eq!(users, ["第一轮", "第二轮", "第三轮"]);
    assert!(
        third
            .iter()
            .all(|message| message.get("tool_calls").is_none())
    );
    assert!(bodies[2].get("tools").is_none() || bodies[2]["tools"].as_array().unwrap().is_empty());
    assert_eq!(receipts[2].turn.session_id, harness.session.id);
    assert_eq!(receipts[2].task.agent_id, harness.agent.id);
    assert!(
        receipts
            .iter()
            .map(|receipt| receipt.turn.id.0.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len()
            == 3
    );
}

#[tokio::test]
async fn t3_same_key_replay_does_not_relaunch_or_grow() {
    let harness = started().await;
    let created = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "第二轮",
            "same-key",
        ))
        .await
        .unwrap();
    wait_task(&harness.engine, &created.task.legacy_task_id).await;
    let calls_after = calls(&harness);
    let events_after = event_count(&harness);
    let objects_after = table_counts(&harness);
    let replay = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "第二轮",
            "same-key",
        ))
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.turn.id, created.turn.id);
    assert_eq!(replay.task.id, created.task.id);
    assert_eq!(calls(&harness), calls_after);
    assert_eq!(event_count(&harness), events_after);
    assert_eq!(table_counts(&harness), objects_after);

    let third = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &created.turn.id,
            "第三轮",
            "later-key",
        ))
        .await
        .unwrap();
    wait_task(&harness.engine, &third.task.legacy_task_id).await;
    let calls_later = calls(&harness);
    let historical = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "第二轮",
            "same-key",
        ))
        .await
        .unwrap();
    assert!(historical.replayed);
    assert_eq!(historical.turn.id, created.turn.id);
    assert_ne!(historical.turn.id, third.turn.id);
    assert_eq!(calls(&harness), calls_later);

    let previous = harness
        .engine
        .store
        .task(&third.task.legacy_task_id)
        .unwrap();
    let hash = send_chat_turn_hash(&command(
        &harness.session.id,
        &harness.agent.id,
        &third.turn.id,
        "hold",
        "held-key",
    ))
    .unwrap();
    let make_same = |suffix: &str| {
        let id = format!("same-{suffix}");
        let turn = Turn {
            id: TurnId(format!("same-turn-{suffix}")),
            session_id: harness.session.id.clone(),
            project_id: harness.session.project_id.clone(),
            status: LifecycleStatus::Queued,
            request_hash: hash.clone(),
            idempotency_key: Some("held-key".into()),
            created_at: previous.created_at,
            updated_at: previous.created_at,
        };
        let task = TurnTask {
            id: TaskId(id.clone()),
            turn_id: turn.id.clone(),
            session_id: harness.session.id.clone(),
            agent_id: harness.agent.id.clone(),
            legacy_task_id: id.clone(),
            depends_on: vec![],
            status: LifecycleStatus::Queued,
            created_at: previous.created_at,
            updated_at: previous.created_at,
        };
        let mut legacy = previous.clone();
        legacy.id = id;
        legacy.status = "queued".into();
        legacy.output.clear();
        legacy.usage = Value::Null;
        legacy.spec.prompt = "hold".into();
        (turn, task, legacy)
    };
    let (turn_a, task_a, legacy_a) = make_same("a");
    let (turn_b, task_b, legacy_b) = make_same("b");
    let other = Store::open(&harness._dir.path().join("turns.db")).unwrap();
    let left = Arc::clone(&harness.engine.store);
    let predecessor = third.turn.id.clone();
    let first = std::thread::spawn(move || {
        left.append_chat_turn(&turn_a, &task_a, &legacy_a, "held-key", &predecessor)
    });
    let predecessor = third.turn.id.clone();
    let second = std::thread::spawn(move || {
        other.append_chat_turn(&turn_b, &task_b, &legacy_b, "held-key", &predecessor)
    });
    let first = first.join().unwrap().unwrap();
    let second = second.join().unwrap().unwrap();
    assert!(first.replayed ^ second.replayed);
    let created = if first.replayed { &second } else { &first };
    let replayed = if first.replayed { &first } else { &second };
    assert_eq!(replayed.turn.id, created.turn.id);
    assert_eq!(replayed.task.id, created.task.id);
    assert_eq!(table_counts(&harness).4, 4);
}

#[tokio::test]
async fn t4_changed_key_inputs_and_legacy_key_conflict() {
    let harness = started().await;
    let before_calls = calls(&harness);
    let before_events = event_count(&harness);
    let before_objects = table_counts(&harness);
    let base = command(
        &harness.session.id,
        &harness.agent.id,
        &harness.first.turn.id,
        "第二轮",
        "bound-key",
    );
    let created = harness.engine.send_chat_turn(base.clone()).await.unwrap();
    wait_task(&harness.engine, &created.task.legacy_task_id).await;
    let settled_calls = calls(&harness);
    let settled_events = event_count(&harness);
    let settled_objects = table_counts(&harness);
    let mut changed = base.clone();
    changed.message = "改过的消息".into();
    assert!(
        harness
            .engine
            .send_chat_turn(changed)
            .await
            .unwrap_err()
            .downcast_ref::<IdempotencyConflict>()
            .is_some()
    );
    let mut other_turn = base.clone();
    other_turn.expected_last_turn_id = TurnId("not-the-predecessor".into());
    assert!(
        harness
            .engine
            .send_chat_turn(other_turn)
            .await
            .unwrap_err()
            .downcast_ref::<IdempotencyConflict>()
            .is_some()
    );
    assert_eq!(calls(&harness), settled_calls);
    assert_eq!(event_count(&harness), settled_events);
    assert_eq!(table_counts(&harness), settled_objects);

    let db = open_db(&harness);
    db.execute(
        "INSERT INTO idempotency(key,run_id,created_at,request_hash) VALUES (?1,?2,?3,?4)",
        params![
            "legacy-only",
            harness.session.legacy_run_id,
            1_u64,
            "legacy-hash"
        ],
    )
    .unwrap();
    drop(db);
    let legacy = command(
        &harness.session.id,
        &harness.agent.id,
        &harness.first.turn.id,
        "不该创建",
        "legacy-only",
    );
    assert!(
        harness
            .engine
            .send_chat_turn(legacy)
            .await
            .unwrap_err()
            .downcast_ref::<IdempotencyConflict>()
            .is_some()
    );
    assert_eq!(calls(&harness), settled_calls);
    assert_eq!(table_counts(&harness), settled_objects);
    assert!(before_calls < settled_calls);
    assert!(before_events < settled_events);
    let _ = before_objects;
}

#[tokio::test]
async fn t5_two_store_connections_allow_only_one_successor() {
    let harness = started().await;
    let previous = harness
        .engine
        .store
        .task(&harness.first.task.legacy_task_id)
        .unwrap();
    let other = Store::open(&harness._dir.path().join("turns.db")).unwrap();
    let make = |suffix: &str| {
        let id = format!("concurrent-{suffix}");
        let turn = Turn {
            id: TurnId(format!("turn-{suffix}")),
            session_id: harness.session.id.clone(),
            project_id: harness.session.project_id.clone(),
            status: LifecycleStatus::Queued,
            request_hash: format!("hash-{suffix}"),
            idempotency_key: Some(format!("key-{suffix}")),
            created_at: previous.created_at,
            updated_at: previous.created_at,
        };
        let task = TurnTask {
            id: TaskId(id.clone()),
            turn_id: turn.id.clone(),
            session_id: harness.session.id.clone(),
            agent_id: harness.agent.id.clone(),
            legacy_task_id: id.clone(),
            depends_on: vec![],
            status: LifecycleStatus::Queued,
            created_at: previous.created_at,
            updated_at: previous.created_at,
        };
        let mut legacy = previous.clone();
        legacy.id = id;
        legacy.status = "queued".into();
        legacy.output.clear();
        legacy.usage = Value::Null;
        legacy.spec.prompt = suffix.into();
        (turn, task, legacy)
    };
    let (turn_a, task_a, legacy_a) = make("a");
    let (turn_b, task_b, legacy_b) = make("b");
    let store = Arc::clone(&harness.engine.store);
    let predecessor = harness.first.turn.id.clone();
    let first = std::thread::spawn(move || {
        store.append_chat_turn(&turn_a, &task_a, &legacy_a, "key-a", &predecessor)
    });
    let predecessor = harness.first.turn.id.clone();
    let second = std::thread::spawn(move || {
        other.append_chat_turn(&turn_b, &task_b, &legacy_b, "key-b", &predecessor)
    });
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    let successes = usize::from(first.is_ok()) + usize::from(second.is_ok());
    assert_eq!(successes, 1, "two connections must not both append");
    assert_eq!(table_counts(&harness).4, 2);
    let failure = if first.is_err() { first } else { second };
    assert_eq!(
        *failure
            .unwrap_err()
            .downcast_ref::<ChatTurnError>()
            .unwrap(),
        ChatTurnError::StalePredecessor
    );
}

#[tokio::test]
async fn t6_same_second_uuid_order_does_not_decide_latest() {
    let harness = started().await;
    let created = harness.first.turn.created_at;
    let db = open_db(&harness);
    db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    db.execute(
        "UPDATE turn_tasks SET turn_id='zzzz-later-looking' WHERE turn_id=?1",
        [&harness.first.turn.id.0],
    )
    .unwrap();
    db.execute(
        "UPDATE turns SET id='zzzz-later-looking', created_at=?2 WHERE id=?1",
        params![harness.first.turn.id.0, created],
    )
    .unwrap();
    db.execute(
        "UPDATE events SET turn_id='zzzz-later-looking' WHERE turn_id=?1",
        [&harness.first.turn.id.0],
    )
    .unwrap();
    db.execute(
        "UPDATE idempotency_records SET turn_id='zzzz-later-looking' WHERE turn_id=?1",
        [&harness.first.turn.id.0],
    )
    .unwrap();
    drop(db);
    let real_latest = TurnId("zzzz-later-looking".into());
    let stale = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &TurnId("aaaa-earlier-looking".into()),
            "陈旧前序",
            "stale-key",
        ))
        .await;
    assert_eq!(error_of(stale), ChatTurnError::NotFound);
    let accepted = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &real_latest,
            "真实最新",
            "fresh-key",
        ))
        .await
        .unwrap();
    assert!(!accepted.replayed);
    assert_ne!(accepted.turn.id, real_latest);
}

#[tokio::test]
async fn t7_unsupported_and_invalid_inputs_add_nothing() {
    let harness = started().await;
    let calls_before = calls(&harness);
    let objects_before = table_counts(&harness);
    let blank = command(
        &harness.session.id,
        &harness.agent.id,
        &harness.first.turn.id,
        "   ",
        "blank",
    );
    assert_eq!(
        error_of(harness.engine.send_chat_turn(blank).await.map(|_| ())),
        ChatTurnError::InvalidInput
    );
    let wrong_agent = command(
        &harness.session.id,
        &AgentId("other-agent".into()),
        &harness.first.turn.id,
        "消息",
        "wrong-agent",
    );
    assert_eq!(
        error_of(harness.engine.send_chat_turn(wrong_agent).await),
        ChatTurnError::NotFound
    );
    let db = open_db(&harness);
    db.execute(
        "UPDATE tasks SET value=json_set(value,'$.spec.tools',json('true')) WHERE id=?1",
        [&harness.first.task.legacy_task_id],
    )
    .unwrap();
    drop(db);
    let tooled = command(
        &harness.session.id,
        &harness.agent.id,
        &harness.first.turn.id,
        "带工具",
        "tooled",
    );
    assert_eq!(
        error_of(harness.engine.send_chat_turn(tooled).await.map(|_| ())),
        ChatTurnError::UnsupportedSession
    );
    assert_eq!(calls(&harness), calls_before);
    assert_eq!(table_counts(&harness), objects_before);
    let missing = command(
        &harness.session.id,
        &harness.agent.id,
        &TurnId("missing-turn".into()),
        "缺失前序",
        "missing-turn",
    );
    assert_eq!(
        error_of(harness.engine.send_chat_turn(missing).await.map(|_| ())),
        ChatTurnError::NotFound
    );
    assert_eq!(table_counts(&harness), objects_before);
}

#[tokio::test]
async fn t8_event_insert_failure_rolls_back_append() {
    let harness = started().await;
    let before = table_counts(&harness);
    let events = event_count(&harness);
    let calls_before = calls(&harness);
    let db = open_db(&harness);
    db.execute_batch(
        "CREATE TRIGGER deny_new_turn_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'denied'); END;",
    )
    .unwrap();
    drop(db);
    let failed = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "应回滚",
            "rollback-key",
        ))
        .await;
    assert!(failed.is_err());
    assert_eq!(table_counts(&harness), before);
    assert_eq!(event_count(&harness), events);
    assert_eq!(calls(&harness), calls_before);
    let idempotency: i64 = open_db(&harness)
        .query_row(
            "SELECT count(*) FROM idempotency_records WHERE key='rollback-key'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(idempotency, 0);
}

#[tokio::test]
async fn t9_cancel_restart_and_resume_boundaries() {
    let harness = started().await;
    let receipt = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "可取消",
            "cancel-key",
        ))
        .await
        .unwrap();
    harness.engine.cancel(&receipt.task.legacy_task_id).unwrap();
    let cancelled = wait_task(&harness.engine, &receipt.task.legacy_task_id).await;
    assert_eq!(cancelled.status, "cancelled");
    let calls_after_cancel = calls(&harness);
    let replay = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "可取消",
            "cancel-key",
        ))
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(calls(&harness), calls_after_cancel);
    let still = harness.engine.store.task(&cancelled.id).unwrap();
    assert_eq!(still.status, "cancelled");

    let db = open_db(&harness);
    let queued = cancelled.id.clone();
    db.execute(
        "UPDATE tasks SET value=json_set(value,'$.status','running','$.output','partial') WHERE id=?1",
        [&queued],
    )
    .unwrap();
    db.execute(
        "UPDATE turn_tasks SET status='running' WHERE legacy_task_id=?1",
        [&queued],
    )
    .unwrap();
    db.execute(
        "UPDATE turns SET status='running' WHERE id=?1",
        [&receipt.turn.id.0],
    )
    .unwrap();
    drop(db);
    assert!(harness.engine.store.recover().unwrap() >= 1);
    let interrupted = harness.engine.store.task(&queued).unwrap();
    assert_eq!(interrupted.status, "interrupted");
    assert_eq!(interrupted.output, "partial");
    assert_eq!(interrupted.route.model, cancelled.route.model);
    let calls_after_recover = calls(&harness);
    let replay_after = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "可取消",
            "cancel-key",
        ))
        .await
        .unwrap();
    assert!(replay_after.replayed);
    assert_eq!(calls(&harness), calls_after_recover);
    let resumed = harness.engine.resume(&queued, "显式继续").await.unwrap();
    let done = wait_task(&harness.engine, &resumed.id).await;
    assert_eq!(done.status, "completed");
    assert_eq!(done.id, queued);
    assert!(calls(&harness) > calls_after_recover);
}

#[tokio::test]
async fn t10_historical_resume_loses_to_a_newer_turn() {
    let harness = started().await;
    let second = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "新轮次",
            "new-turn",
        ))
        .await
        .unwrap();
    wait_task(&harness.engine, &second.task.legacy_task_id).await;
    let historical = harness
        .engine
        .store
        .task(&harness.first.task.legacy_task_id)
        .unwrap();
    let mut retry = historical.clone();
    retry.status = "queued".into();
    let error = harness
        .engine
        .store
        .resume_task(&retry, "不能重开历史")
        .unwrap_err();
    assert_eq!(
        *error.downcast_ref::<ChatTurnError>().unwrap(),
        ChatTurnError::StalePredecessor
    );
    let unchanged = harness.engine.store.task(&historical.id).unwrap();
    assert_eq!(unchanged.status, "completed");
    assert_eq!(unchanged.output, historical.output);
}

#[tokio::test]
async fn t11_route_workspace_and_context_limits_fail_closed() {
    let harness = started().await;
    let calls_before = calls(&harness);
    let objects_before = table_counts(&harness);
    let mut settings = harness.engine.store.settings().unwrap().unwrap();
    settings.routes[0].base_url = "http://127.0.0.1:9/v1".into();
    harness.engine.store.save_settings(&settings).unwrap();
    let moved = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "地址变了",
            "moved-host",
        ))
        .await;
    assert!(moved.is_err());
    assert_eq!(calls(&harness), calls_before);
    settings.routes[0].base_url = harness
        .engine
        .store
        .task(&harness.first.task.legacy_task_id)
        .unwrap()
        .route
        .base_url;
    settings.workspace = harness
        ._dir
        .path()
        .join("elsewhere")
        .to_string_lossy()
        .into();
    std::fs::create_dir_all(&settings.workspace).unwrap();
    harness.engine.store.save_settings(&settings).unwrap();
    let kept = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "目录变了",
            "workspace-key",
        ))
        .await
        .unwrap();
    let task = harness
        .engine
        .store
        .task(&kept.task.legacy_task_id)
        .unwrap();
    assert_eq!(
        task.workspace,
        harness
            .engine
            .store
            .project(&harness.session.project_id)
            .unwrap()
            .root_path
    );
    assert_ne!(task.workspace, settings.workspace);
    let huge = "字".repeat(1_500_001);
    let oversized = command(
        &harness.session.id,
        &harness.agent.id,
        &harness.first.turn.id,
        &huge,
        "oversized",
    );
    assert_eq!(
        error_of(harness.engine.send_chat_turn(oversized).await.map(|_| ())),
        ChatTurnError::InvalidInput
    );
    assert_eq!(table_counts(&harness).4, objects_before.4 + 1);
    assert_eq!(calls(&harness), calls_before);
}

#[tokio::test]
async fn t12_session_events_stay_ordered_across_turns() {
    let harness = started().await;
    let second = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "事件归属",
            "event-key",
        ))
        .await
        .unwrap();
    wait_task(&harness.engine, &second.task.legacy_task_id).await;
    let events = harness
        .engine
        .store
        .events(&harness.session.legacy_run_id, 0)
        .unwrap();
    assert!(events.len() >= 2);
    let mut previous = 0;
    for event in &events {
        assert!(event.seq > previous);
        assert_eq!(
            event.session_id.as_deref(),
            Some(harness.session.id.0.as_str())
        );
        previous = event.seq;
    }
    let cursors: Vec<_> = events.iter().map(|event| event.seq).collect();
    let later = harness
        .engine
        .store
        .events(&harness.session.legacy_run_id, cursors[0])
        .unwrap();
    assert!(later.iter().all(|event| event.seq > cursors[0]));
    assert!(
        events
            .iter()
            .any(|event| event.turn_id.as_deref() == Some(harness.first.turn.id.0.as_str()))
    );
    assert!(
        events
            .iter()
            .any(|event| event.turn_id.as_deref() == Some(second.turn.id.0.as_str()))
    );
}

#[tokio::test]
async fn t7_busy_non_chat_corrupt_order_and_unpaired_tools_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (base, probe) = mock_provider().await;
    let engine = engine_at(dir.path(), &base);
    let team = RunRequest {
        title: "团队".into(),
        kind: SessionKind::Team,
        tasks: vec![TaskSpec {
            name: "member-a".into(),
            role: "成员".into(),
            route_id: "chat-route".into(),
            prompt: "执行".into(),
            depends_on: vec![],
            write_scopes: vec![],
            tools: false,
            allow_commands: false,
            max_rounds: 2,
        }],
    };
    let run = engine.start(team).await.unwrap();
    wait_task(&engine, &run.tasks[0].id).await;
    let db = Connection::open(dir.path().join("turns.db")).unwrap();
    let session_id: String = db
        .query_row("SELECT id FROM sessions", [], |row| row.get(0))
        .unwrap();
    let agent_id: String = db
        .query_row("SELECT id FROM agents", [], |row| row.get(0))
        .unwrap();
    let turn_id: String = db
        .query_row("SELECT id FROM turns", [], |row| row.get(0))
        .unwrap();
    drop(db);
    let before = Connection::open(dir.path().join("turns.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM turns", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let refused = engine
        .send_chat_turn(command(
            &SessionId(session_id),
            &AgentId(agent_id),
            &TurnId(turn_id),
            "不能追加团队",
            "team-key",
        ))
        .await;
    assert_eq!(error_of(refused), ChatTurnError::UnsupportedSession);
    assert_eq!(probe.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        Connection::open(dir.path().join("turns.db"))
            .unwrap()
            .query_row("SELECT count(*) FROM turns", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before
    );

    let harness = started().await;
    let calls_before = calls(&harness);
    let objects_before = table_counts(&harness);
    open_db(&harness)
        .execute(
            "UPDATE turns SET status='running' WHERE id=?1",
            [&harness.first.turn.id.0],
        )
        .unwrap();
    open_db(&harness)
        .execute(
            "UPDATE turn_tasks SET status='running' WHERE turn_id=?1",
            [&harness.first.turn.id.0],
        )
        .unwrap();
    open_db(&harness)
        .execute(
            "UPDATE tasks SET value=json_set(value,'$.status','running') WHERE id=?1",
            [&harness.first.task.legacy_task_id],
        )
        .unwrap();
    let busy = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "会话忙",
            "busy-key",
        ))
        .await;
    assert_eq!(error_of(busy), ChatTurnError::SessionBusy);

    open_db(&harness)
        .execute(
            "DELETE FROM events WHERE turn_id=?1",
            [&harness.first.turn.id.0],
        )
        .unwrap();
    let corrupt = harness
        .engine
        .send_chat_turn(command(
            &harness.session.id,
            &harness.agent.id,
            &harness.first.turn.id,
            "损坏顺序",
            "corrupt-key",
        ))
        .await;
    assert!(
        corrupt.is_err(),
        "a turn without its opening event cannot be ordered"
    );
    assert!(corrupt.unwrap_err().to_string().contains("首条事件"));

    let another = started().await;
    let value = another
        .engine
        .store
        .task(&another.first.task.legacy_task_id)
        .unwrap();
    let mut messages = value.messages.clone();
    messages.push(json!({"role":"assistant","tool_calls":[{"id":"dangling","type":"function"}]}));
    open_db(&another)
        .execute(
            "UPDATE tasks SET value=json_set(value,'$.messages',json(?2)) WHERE id=?1",
            params![value.id, serde_json::to_string(&messages).unwrap()],
        )
        .unwrap();
    let dangling = another
        .engine
        .send_chat_turn(command(
            &another.session.id,
            &another.agent.id,
            &another.first.turn.id,
            "坏上下文",
            "dangling-key",
        ))
        .await;
    assert_eq!(error_of(dangling), ChatTurnError::UnsupportedSession);
    assert_eq!(calls(&harness), calls_before);
    assert_eq!(table_counts(&harness), objects_before);
    assert_eq!(calls(&another), 1);
    assert_eq!(table_counts(&another).4, 1);
}
