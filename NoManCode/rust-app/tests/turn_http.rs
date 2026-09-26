//! NEXT-01 joint acceptance for `POST /api/sessions/{id}/turns`.
//!
//! Every case runs a real loopback HTTP server backed by a temporary SQLite
//! database and a local mock provider. The first turn of every chat fixture is
//! created through a real `POST /api/runs` request and awaited to completion;
//! no direct Engine call stands in for the new endpoint under test.

use axum::{Json, Router, body::Body, extract::State, response::IntoResponse, routing::post};
use peachsh::{
    domain::*,
    engine::Engine,
    server::{self, App},
    store::Store,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
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
    release: Arc<tokio::sync::Notify>,
}

/// Mock chat-completion endpoint. A message starting with `hold:` streams one
/// persisted delta and then parks until `probe.release` fires, giving tests a
/// genuinely running task without sleeps. Anything else answers in one chunk.
async fn chat(State(probe): State<Probe>, Json(body): Json<Value>) -> axum::response::Response {
    probe.calls.fetch_add(1, Ordering::SeqCst);
    let last = body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .cloned()
        .unwrap_or(Value::Null);
    let last_content = last["content"].as_str().unwrap_or_default().to_owned();
    probe.bodies.lock().unwrap().push(body);
    if let Some(tag) = last_content.strip_prefix("hold:") {
        let release = probe.release.clone();
        let chunk = json!({"choices":[{"delta":{"content":format!("partial-{tag}")}}]});
        let stream = async_stream::stream! {
            yield Ok::<_, std::convert::Infallible>(format!("data: {chunk}\n\n").into_bytes());
            release.notified().await;
            let done = json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}});
            yield Ok(format!("data: {done}\n\ndata: [DONE]\n\n").into_bytes());
        };
        return (
            [("content-type", "text/event-stream")],
            Body::from_stream(stream),
        )
            .into_response();
    }
    let chunk = json!({"choices":[{"delta":{"content":"reply"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
        .into_response()
}

async fn mock_provider() -> (String, Probe) {
    let probe = Probe {
        calls: Arc::new(AtomicUsize::new(0)),
        bodies: Arc::new(std::sync::Mutex::new(Vec::new())),
        release: Arc::new(tokio::sync::Notify::new()),
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

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}

fn security_headers(response: &reqwest::Response) {
    let headers = response.headers();
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(
        headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'self'")
    );
}

async fn error_of(response: reqwest::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    security_headers(&response);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json"),
        "structured errors must be JSON, not plain text"
    );
    let body: Value = response.json().await.unwrap();
    assert!(body["code"].is_string(), "{body}");
    assert_eq!(body["error"], body["message"]);
    assert!(body["retryable"].is_boolean());
    (status, body)
}

/// The eleven tables a rejected command must never touch, plus `events` whose
/// row set and max seq must also stay put.
const WATCHED_TABLES: &[&str] = &[
    "projects",
    "sessions",
    "turns",
    "agents",
    "turn_tasks",
    "turn_task_dependencies",
    "idempotency_records",
    "runs",
    "tasks",
    "idempotency",
    "events",
];

fn open_db(path: &Path) -> Connection {
    let db = Connection::open(path).unwrap();
    db.pragma_update(None, "busy_timeout", 5000).unwrap();
    db
}

/// Full content snapshot: row counts alone cannot prove old rows were not
/// rewritten, so every cell is captured in rowid order.
fn snapshot(path: &Path) -> Vec<(String, Vec<Vec<String>>)> {
    let db = open_db(path);
    WATCHED_TABLES
        .iter()
        .map(|table| {
            let mut stmt = db
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    Ok((0..columns)
                        .map(|index| {
                            format!("{:?}", row.get::<_, rusqlite::types::Value>(index).unwrap())
                        })
                        .collect::<Vec<String>>())
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (table.to_string(), rows)
        })
        .collect()
}

fn table<'a>(snapshot: &'a [(String, Vec<Vec<String>>)], name: &str) -> &'a [Vec<String>] {
    &snapshot
        .iter()
        .find(|(table, _)| table == name)
        .unwrap_or_else(|| panic!("missing table {name}"))
        .1
}

/// Old rows must be an exact prefix of the new state for append-only tables.
fn assert_grew_only(
    before: &[(String, Vec<Vec<String>>)],
    after: &[(String, Vec<Vec<String>>)],
    name: &str,
    added: usize,
) {
    let old = table(before, name);
    let new = table(after, name);
    assert_eq!(new.len(), old.len() + added, "{name} row count");
    assert!(
        new.iter().take(old.len()).eq(old.iter()),
        "{name} existing rows were rewritten"
    );
}

fn assert_static(
    before: &[(String, Vec<Vec<String>>)],
    after: &[(String, Vec<Vec<String>>)],
    name: &str,
) {
    assert_eq!(table(before, name), table(after, name), "{name} changed");
}

fn calls(h: &Harness) -> usize {
    h.probe.calls.load(Ordering::SeqCst)
}

/// Provider calls whose last user message is exactly `message`.
fn calls_for(h: &Harness, message: &str) -> usize {
    h.probe
        .bodies
        .lock()
        .unwrap()
        .iter()
        .filter(|body| {
            body["messages"]
                .as_array()
                .and_then(|messages| messages.last())
                .is_some_and(|message_value| message_value["content"] == message)
        })
        .count()
}

fn last_message_for(h: &Harness, message: &str) -> Option<Value> {
    h.probe
        .bodies
        .lock()
        .unwrap()
        .iter()
        .find(|body| {
            body["messages"]
                .as_array()
                .and_then(|messages| messages.last())
                .is_some_and(|last| last["content"] == message)
        })
        .and_then(|body| body["messages"].as_array().and_then(|m| m.last()).cloned())
}

struct Harness {
    _dir: TempDir,
    db_path: PathBuf,
    origin: String,
    token: String,
    engine: Arc<Engine>,
    probe: Probe,
    _http: tokio::task::JoinHandle<()>,
}

async fn serve(engine: Arc<Engine>, token: &str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = server::router(App {
        engine,
        token: token.into(),
        origin: origin.clone(),
    });
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (origin, task)
}

fn configure(store: &Store, dir: &TempDir, base: &str) {
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into(),
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
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let (base, probe) = mock_provider().await;
    let db_path = dir.path().join("turns.db");
    let store = Arc::new(Store::open(&db_path).unwrap());
    configure(&store, &dir, &base);
    let engine = Engine::new(store, 2).unwrap();
    let (origin, http) = serve(engine.clone(), "turn-token").await;
    Harness {
        _dir: dir,
        db_path,
        origin,
        token: "turn-token".into(),
        engine,
        probe,
        _http: http,
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

struct ChatIds {
    run: Run,
    session: Session,
    agent: Agent,
    turn: Turn,
    task: TurnTask,
}

/// First turn of a chat via the real `POST /api/runs`, waited to completion.
async fn spawn_chat(h: &Harness, title: &str, prompt: &str, key: &str, kind: &str) -> ChatIds {
    let http = client();
    let response = http
        .post(format!("{}/api/runs", h.origin))
        .header("x-peachsh-token", &h.token)
        .header("idempotency-key", key)
        .json(&json!({
            "kind": kind,
            "title": title,
            "tasks": [{
                "name": "chat-worker",
                "role": "对话助手",
                "route_id": "chat-route",
                "prompt": prompt,
                "depends_on": [],
                "write_scopes": [],
                "tools": false,
                "allow_commands": false,
                "max_rounds": 2
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let run: Run = response.json().await.unwrap();
    let done = wait_task(&h.engine, &run.tasks[0].id).await;
    assert_eq!(done.status, "completed");
    let db = open_db(&h.db_path);
    let session_id: String = db
        .query_row(
            "SELECT id FROM sessions WHERE legacy_run_id=?1",
            [&run.id],
            |row| row.get(0),
        )
        .unwrap();
    let agent_id: String = db
        .query_row(
            "SELECT id FROM agents WHERE session_id=?1",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap();
    let turn_id: String = db
        .query_row(
            "SELECT id FROM turns WHERE session_id=?1",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap();
    drop(db);
    let session = h.engine.store.session(&SessionId(session_id)).unwrap();
    let agent = h.engine.store.agent(&AgentId(agent_id)).unwrap();
    let turn = h.engine.store.turn(&TurnId(turn_id)).unwrap();
    let task = h.engine.store.turn_tasks(&turn.id).unwrap().remove(0);
    ChatIds {
        run,
        session,
        agent,
        turn,
        task,
    }
}

struct Fixture {
    h: Harness,
    chat: ChatIds,
}

async fn started() -> Fixture {
    let h = harness().await;
    let chat = spawn_chat(&h, "连续对话", "第一轮", "run-create", "chat").await;
    Fixture { h, chat }
}

fn turn_post<'a>(
    http: &'a reqwest::Client,
    h: &'a Harness,
    session: &str,
    agent: &str,
    previous: &str,
    message: &str,
    key: &str,
) -> reqwest::RequestBuilder {
    http.post(format!("{}/api/sessions/{session}/turns", h.origin))
        .header("x-peachsh-token", &h.token)
        .header("idempotency-key", key)
        .json(&json!({
            "agent_id": agent,
            "expected_last_turn_id": previous,
            "message": message,
        }))
}

/// The receipt contract: exactly `turn`, `task`, `replayed` with the identity
/// relationships the contract fixes.
fn receipt(body: &Value, fixture: &Fixture, replayed: bool) -> (Turn, TurnTask) {
    let object = body.as_object().unwrap();
    assert_eq!(
        object.len(),
        3,
        "receipt must carry exactly turn/task/replayed: {body}"
    );
    let turn: Turn = serde_json::from_value(body["turn"].clone()).unwrap();
    let task: TurnTask = serde_json::from_value(body["task"].clone()).unwrap();
    assert_eq!(body["replayed"], replayed);
    assert_eq!(task.turn_id, turn.id);
    assert_eq!(turn.session_id, fixture.chat.session.id);
    assert_eq!(task.session_id, fixture.chat.session.id);
    assert_eq!(task.agent_id, fixture.chat.agent.id);
    assert_ne!(
        task.legacy_task_id, fixture.chat.run.tasks[0].id,
        "the run's first task must never be returned as a new turn's task"
    );
    (turn, task)
}

fn expected_hash(fixture: &Fixture, previous: &str, message: &str, key: &str) -> String {
    send_chat_turn_hash(&SendChatTurn {
        session_id: fixture.chat.session.id.clone(),
        agent_id: fixture.chat.agent.id.clone(),
        expected_last_turn_id: TurnId(previous.into()),
        message: message.into(),
        idempotency_key: key.into(),
    })
    .unwrap()
}

#[derive(Debug)]
struct Frame {
    id: Option<String>,
    event: Option<String>,
    data: String,
}

struct LiveSse {
    response: reqwest::Response,
    pending: Vec<u8>,
}

impl LiveSse {
    fn new(response: reqwest::Response) -> Self {
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        security_headers(&response);
        Self {
            response,
            pending: Vec::new(),
        }
    }

    async fn next_data(&mut self, timeout: Duration) -> Option<Frame> {
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(frame) = pop_data_frame(&mut self.pending) {
                    return Some(frame);
                }
                let chunk = self.response.chunk().await.unwrap()?;
                self.pending.extend_from_slice(&chunk);
            }
        })
        .await
        .expect("SSE frame timed out")
    }

    /// Like `next_data`, but a quiet stream returns `None` instead of panicking.
    /// Reconnect assertions use this to prove no older frame is still pending.
    async fn try_next_data(&mut self, timeout: Duration) -> Option<Frame> {
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(frame) = pop_data_frame(&mut self.pending) {
                    return Some(frame);
                }
                let chunk = self.response.chunk().await.unwrap()?;
                self.pending.extend_from_slice(&chunk);
            }
        })
        .await
        .unwrap_or_default()
    }
}

fn pop_data_frame(pending: &mut Vec<u8>) -> Option<Frame> {
    let end = pending.windows(2).position(|pair| pair == b"\n\n")?;
    let raw = std::str::from_utf8(&pending[..end]).unwrap().to_owned();
    pending.drain(..end + 2);
    if raw.is_empty()
        || raw
            .lines()
            .all(|line| line.starts_with(':') || line.is_empty())
    {
        return None;
    }
    let mut frame = Frame {
        id: None,
        event: None,
        data: String::new(),
    };
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("id: ") {
            frame.id = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("event: ") {
            frame.event = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("data: ") {
            if !frame.data.is_empty() {
                frame.data.push('\n');
            }
            frame.data.push_str(value);
        }
    }
    if frame.data.is_empty() {
        None
    } else {
        Some(frame)
    }
}

/// Read frames until `stop` matches or the frame budget is exhausted.
async fn read_until(
    stream: &mut LiveSse,
    timeout: Duration,
    budget: usize,
    stop: impl Fn(&Value) -> bool,
) -> Vec<(Frame, Value)> {
    let mut frames = Vec::new();
    for _ in 0..budget {
        let Some(frame) = stream.next_data(timeout).await else {
            break;
        };
        let data: Value = serde_json::from_str(&frame.data).unwrap();
        let done = stop(&data);
        frames.push((frame, data));
        if done {
            break;
        }
    }
    frames
}

fn scalar(path: &Path, sql: &str, param: &str) -> String {
    open_db(path)
        .query_row(sql, [param], |row| row.get::<_, String>(0))
        .unwrap()
}

fn count_where(path: &Path, sql: &str, params: impl rusqlite::Params) -> i64 {
    let db = open_db(path);
    let mut stmt = db.prepare(sql).unwrap();
    stmt.query_row(params, |row| row.get(0)).unwrap()
}

fn max_seq(path: &Path) -> i64 {
    open_db(path)
        .query_row("SELECT coalesce(max(seq),0) FROM events", [], |row| {
            row.get(0)
        })
        .unwrap()
}

async fn wait_delta(path: &Path, task_id: &str, text: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let hit = count_where(
                path,
                "SELECT count(*) FROM events WHERE task_id=?1 AND kind='delta' AND data LIKE ?2",
                [task_id, &format!("%{text}%")],
            );
            if hit > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("delta was not persisted")
}

async fn append(
    http: &reqwest::Client,
    fixture: &Fixture,
    previous: &str,
    message: &str,
    key: &str,
) -> reqwest::Response {
    turn_post(
        http,
        &fixture.h,
        &fixture.chat.session.id.0,
        &fixture.chat.agent.id.0,
        previous,
        message,
        key,
    )
    .send()
    .await
    .unwrap()
}

fn assert_code(status: u16, body: &Value, code: &str, retryable: bool) {
    assert_eq!(body["code"], code, "{body}");
    assert_eq!(body["retryable"], retryable, "{body}");
    assert!(body["message"].is_string(), "{body}");
    let _ = status;
}

async fn expect_status(
    response: reqwest::Response,
    status: u16,
    code: &str,
    retryable: bool,
) -> Value {
    let (got, body) = error_of(response).await;
    assert_eq!(got, status, "{body}");
    assert_code(got, &body, code, retryable);
    body
}

fn hides(body: &Value, needles: &[&str]) {
    let text = body.to_string();
    for needle in needles {
        assert!(!text.contains(needle), "response leaked {needle}: {text}");
    }
}

/// Snapshot taken after the fault injection, so the injection itself is not a side effect.
fn assert_untouched(before: &[(String, Vec<Vec<String>>)], path: &Path) {
    let after = snapshot(path);
    for name in WATCHED_TABLES {
        assert_static(before, &after, name);
    }
}

#[tokio::test]
async fn n01_append_and_chain() {
    let fixture = started().await;
    let http = client();
    let before = snapshot(&fixture.h.db_path);
    let first_value = scalar(
        &fixture.h.db_path,
        "SELECT value FROM tasks WHERE id=?1",
        &fixture.chat.task.legacy_task_id,
    );
    let response = append(&http, &fixture, &fixture.chat.turn.id.0, "第二轮", "turn-2").await;
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    security_headers(&response);
    let body: Value = response.json().await.unwrap();
    let (turn2, task2) = receipt(&body, &fixture, false);
    assert_eq!(turn2.status, LifecycleStatus::Queued);
    assert_eq!(turn2.idempotency_key.as_deref(), Some("turn-2"));
    assert_eq!(
        turn2.request_hash,
        expected_hash(&fixture, &fixture.chat.turn.id.0, "第二轮", "turn-2")
    );
    let done2 = wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    assert_eq!(done2.status, "completed");

    let response = append(&http, &fixture, &turn2.id.0, "第三轮", "turn-3").await;
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let body: Value = response.json().await.unwrap();
    let (turn3, task3) = receipt(&body, &fixture, false);
    let done3 = wait_task(&fixture.h.engine, &task3.legacy_task_id).await;
    assert_eq!(done3.status, "completed");

    let after = snapshot(&fixture.h.db_path);
    for name in ["projects", "sessions", "agents", "runs"] {
        assert_static(&before, &after, name);
    }
    for name in [
        "turns",
        "turn_tasks",
        "tasks",
        "idempotency_records",
        "idempotency",
    ] {
        assert_grew_only(&before, &after, name, 2);
    }
    assert_static(&before, &after, "turn_task_dependencies");
    let old_events = table(&before, "events");
    let new_events = table(&after, "events");
    assert!(
        new_events
            .iter()
            .take(old_events.len())
            .eq(old_events.iter()),
        "events old rows must stay a prefix"
    );
    assert!(new_events.len() > old_events.len());

    let hash2 = expected_hash(&fixture, &fixture.chat.turn.id.0, "第二轮", "turn-2");
    let record = open_db(&fixture.h.db_path);
    let (request_hash, turn_id, session_id, run_id): (String, String, String, String) = record
        .query_row(
            "SELECT request_hash,turn_id,session_id,legacy_run_id FROM idempotency_records WHERE key='turn-2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(request_hash, hash2);
    assert_eq!(turn_id, turn2.id.0);
    assert_eq!(session_id, fixture.chat.session.id.0);
    assert_eq!(run_id, fixture.chat.session.legacy_run_id);
    let (legacy_run, legacy_hash): (String, String) = record
        .query_row(
            "SELECT run_id,request_hash FROM idempotency WHERE key='turn-2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(legacy_run, fixture.chat.session.legacy_run_id);
    assert_eq!(legacy_hash, hash2);
    drop(record);

    assert_eq!(calls_for(&fixture.h, "第二轮"), 1);
    assert_eq!(calls_for(&fixture.h, "第三轮"), 1);
    assert_eq!(
        last_message_for(&fixture.h, "第二轮").unwrap()["content"],
        "第二轮"
    );
    assert_eq!(
        last_message_for(&fixture.h, "第三轮").unwrap()["content"],
        "第三轮"
    );
    assert_eq!(
        scalar(
            &fixture.h.db_path,
            "SELECT value FROM tasks WHERE id=?1",
            &fixture.chat.task.legacy_task_id
        ),
        first_value,
        "the first task JSON must not be rewritten"
    );
    assert_ne!(turn3.id, turn2.id);
}

#[tokio::test]
async fn n02_session_sse_stream() {
    let fixture = started().await;
    let http = client();
    let baseline = max_seq(&fixture.h.db_path);
    let mut stream = LiveSse::new(
        http.get(format!(
            "{}/api/sessions/{}/events?after={baseline}",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .send()
        .await
        .unwrap(),
    );
    let response = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "hold:sse",
        "turn-sse",
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let body: Value = response.json().await.unwrap();
    let (turn2, task2) = receipt(&body, &fixture, false);
    let frames = read_until(&mut stream, Duration::from_secs(5), 16, |data| {
        data["kind"] == "delta" && data["data"]["text"] == "partial-sse"
    })
    .await;
    assert!(!frames.is_empty(), "delta frame never arrived");
    let mut previous = baseline;
    for (frame, data) in &frames {
        let seq = data["seq"].as_i64().unwrap();
        assert_eq!(frame.id.as_deref(), Some(seq.to_string().as_str()));
        assert_eq!(frame.event.as_deref(), data["kind"].as_str());
        assert_eq!(data["session_id"], fixture.chat.session.id.0);
        assert_eq!(data["turn_id"], turn2.id.0);
        assert_eq!(data["task_id"], task2.legacy_task_id);
        assert!(seq > previous, "frame seq must strictly increase");
        previous = seq;
        assert!(data.get("at").is_some());
        assert!(data.get("kind").is_some());
    }
    let consumed = previous;

    let listed: Vec<Turn> = http
        .get(format!(
            "{}/api/sessions/{}/turns",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed.iter().any(|turn| turn.id == turn2.id));
    let fetched: Turn = http
        .get(format!("{}/api/turns/{}", fixture.h.origin, turn2.id.0))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched.id, turn2.id);
    assert_eq!(fetched.session_id, turn2.session_id);
    assert_eq!(fetched.project_id, turn2.project_id);
    assert_eq!(fetched.request_hash, turn2.request_hash);
    assert_eq!(fetched.idempotency_key, turn2.idempotency_key);

    drop(stream);
    let mut again = LiveSse::new(
        http.get(format!(
            "{}/api/sessions/{}/events?after={consumed}",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .send()
        .await
        .unwrap(),
    );
    // Quiet window: nothing <= consumed may still be sitting in the buffer.
    assert!(
        again
            .try_next_data(Duration::from_millis(200))
            .await
            .is_none(),
        "reconnect returned a frame before release"
    );
    fixture.h.probe.release.notify_waiters();
    let later = again.next_data(Duration::from_secs(5)).await.unwrap();
    let later_data: Value = serde_json::from_str(&later.data).unwrap();
    assert!(later_data["seq"].as_i64().unwrap() > consumed);
    assert_eq!(later_data["turn_id"], turn2.id.0);
    let done = wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    assert_eq!(done.status, "completed");
}

#[tokio::test]
async fn n03_replay_variants() {
    let fixture = started().await;
    let http = client();
    let before = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let response = append(&http, &fixture, &fixture.chat.turn.id.0, "hold:run", "rk").await;
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let created: Value = response.json().await.unwrap();
    let (turn2, task2) = receipt(&created, &fixture, false);
    wait_delta(&fixture.h.db_path, &task2.legacy_task_id, "partial-run").await;
    assert_eq!(calls_for(&fixture.h, "hold:run"), 1);

    let replay = append(&http, &fixture, &fixture.chat.turn.id.0, "hold:run", "rk").await;
    assert_eq!(replay.status(), reqwest::StatusCode::OK);
    let replayed: Value = replay.json().await.unwrap();
    let (same, same_task) = receipt(&replayed, &fixture, true);
    assert_eq!(same.id, turn2.id);
    assert_eq!(same_task.id, task2.id);
    assert_eq!(calls_for(&fixture.h, "hold:run"), 1);

    fixture.h.probe.release.notify_waiters();
    let done = wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    assert_eq!(done.status, "completed");
    let after_done = append(&http, &fixture, &fixture.chat.turn.id.0, "hold:run", "rk").await;
    assert_eq!(after_done.status(), reqwest::StatusCode::OK);
    let after_body: Value = after_done.json().await.unwrap();
    assert_eq!(after_body["turn"]["id"], turn2.id.0);
    assert_eq!(after_body["replayed"], true);

    let raw = format!(
        r#"{{"message":"\u0068old:run","expected_last_turn_id":"{}","agent_id":"{}"}}"#,
        fixture.chat.turn.id.0, fixture.chat.agent.id.0
    );
    // Field order and \u escapes must hash the same as the original JSON
    // object: this raw body reorders fields and writes `h` as `\u0068`.
    let escaped = http
        .post(format!(
            "{}/api/sessions/{}/turns",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .header("x-peachsh-token", &fixture.h.token)
        .header("idempotency-key", "rk")
        .header("content-type", "application/json")
        .body(raw)
        .send()
        .await
        .unwrap();
    assert_eq!(escaped.status(), reqwest::StatusCode::OK);
    let escaped_body: Value = escaped.json().await.unwrap();
    assert_eq!(escaped_body["turn"]["id"], turn2.id.0);
    assert_eq!(escaped_body["replayed"], true);

    let third = append(&http, &fixture, &turn2.id.0, "第三轮", "turn-3").await;
    assert_eq!(third.status(), reqwest::StatusCode::CREATED);
    let third_body: Value = third.json().await.unwrap();
    let (turn3, task3) = receipt(&third_body, &fixture, false);
    let done3 = wait_task(&fixture.h.engine, &task3.legacy_task_id).await;
    assert_eq!(done3.status, "completed");
    let historical = append(&http, &fixture, &fixture.chat.turn.id.0, "hold:run", "rk").await;
    assert_eq!(historical.status(), reqwest::StatusCode::OK);
    let historical_body: Value = historical.json().await.unwrap();
    assert_eq!(historical_body["turn"]["id"], turn2.id.0);

    let after = snapshot(&fixture.h.db_path);
    // The two new turns (hold + third) are the only growth; every replay added nothing.
    for name in [
        "turns",
        "turn_tasks",
        "tasks",
        "idempotency_records",
        "idempotency",
    ] {
        assert_grew_only(&before, &after, name, 2);
    }
    assert_eq!(calls_for(&fixture.h, "hold:run"), 1);
    assert_eq!(calls(&fixture.h), calls_before + 2);
    assert_ne!(turn3.id, turn2.id);
}

#[tokio::test]
async fn n04_key_binding_conflict() {
    let fixture = started().await;
    let http = client();
    let created = append(&http, &fixture, &fixture.chat.turn.id.0, "绑定", "bound").await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: Value = created.json().await.unwrap();
    let (turn2, task2) = receipt(&body, &fixture, false);
    let done = wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    assert_eq!(done.status, "completed");
    let other = spawn_chat(&fixture.h, "另一会话", "另一轮", "run-other", "chat").await;
    let calls_before = calls(&fixture.h);
    let before = snapshot(&fixture.h.db_path);

    let cases = [
        (
            fixture.chat.session.id.0.clone(),
            fixture.chat.agent.id.0.clone(),
            turn2.id.0.clone(),
            "改了消息",
        ),
        (
            fixture.chat.session.id.0.clone(),
            other.agent.id.0.clone(),
            turn2.id.0.clone(),
            "绑定",
        ),
        (
            other.session.id.0.clone(),
            fixture.chat.agent.id.0.clone(),
            turn2.id.0.clone(),
            "绑定",
        ),
        (
            fixture.chat.session.id.0.clone(),
            fixture.chat.agent.id.0.clone(),
            fixture.chat.turn.id.0.clone(),
            "改了前序",
        ),
    ];
    for (session, agent, previous, message) in cases {
        let response = turn_post(
            &http, &fixture.h, &session, &agent, &previous, message, "bound",
        )
        .send()
        .await
        .unwrap();
        let body = expect_status(response, 409, "conflict", false).await;
        hides(&body, &[&agent]);
    }
    let reused = turn_post(
        &http,
        &fixture.h,
        &fixture.chat.session.id.0,
        &fixture.chat.agent.id.0,
        &turn2.id.0,
        "复用首轮 key",
        "run-create",
    )
    .send()
    .await
    .unwrap();
    let body = expect_status(reused, 409, "conflict", false).await;
    hides(&body, &["复用首轮 key"]);
    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);
}

fn post_raw(
    http: &reqwest::Client,
    h: &Harness,
    session: &str,
    key: Option<&str>,
    body: String,
) -> reqwest::RequestBuilder {
    let mut request = http
        .post(format!("{}/api/sessions/{session}/turns", h.origin))
        .header("x-peachsh-token", &h.token)
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    request.body(body)
}

#[tokio::test]
async fn n05_body_and_id_validation() {
    let fixture = started().await;
    let http = client();
    let calls_before = calls(&fixture.h);
    let before = snapshot(&fixture.h.db_path);
    let session = fixture.chat.session.id.0.clone();
    let agent = fixture.chat.agent.id.0.clone();
    let previous = fixture.chat.turn.id.0.clone();
    let legal = format!(
        r#"{{"agent_id":"{agent}","expected_last_turn_id":"{previous}","message":"合法"}}"#
    );
    let mut rejected = Vec::new();
    let missing = [
        format!(r#"{{"expected_last_turn_id":"{previous}","message":"缺agent"}}"#),
        format!(r#"{{"agent_id":"{agent}","message":"缺expected"}}"#),
        format!(r#"{{"agent_id":"{agent}","expected_last_turn_id":"{previous}"}}"#),
    ];
    for body in missing {
        rejected.push(body);
    }
    for field in ["agent_id", "expected_last_turn_id", "message"] {
        let mut value: Value = serde_json::from_str(&legal).unwrap();
        value[field] = Value::Null;
        rejected.push(value.to_string());
        value[field] = json!(1);
        rejected.push(value.to_string());
    }
    rejected.push(format!(
        r#"{{"agent_id":"{agent}","agent_id":"{agent}","expected_last_turn_id":"{previous}","message":"重复"}}"#
    ));
    rejected.push(format!(
        r#"{{"agent_id":"{agent}","expected_last_turn_id":"{previous}","message":"未知","extra":"nope"}}"#
    ));
    rejected.push(format!(
        r#"{{"agent_id":"{agent}","expected_last_turn_id":"{previous}","message":"塞了session","session_id":"{session}"}}"#
    ));
    rejected.push(format!(
        r#"{{"agent_id":"{agent}","expected_last_turn_id":"{previous}","message":"塞了key","idempotency_key":"from-body"}}"#
    ));
    rejected.push("[]".into());
    rejected.push(r#""text""#.into());
    for body in &rejected {
        let response = post_raw(&http, &fixture.h, &session, Some("bad-body"), body.clone())
            .send()
            .await
            .unwrap();
        let error = expect_status(response, 400, "request_failed", false).await;
        hides(&error, &["sk-testtoken123", "from-body"]);
    }

    // `%FF` is valid percent-encoding syntactically but decodes to invalid
    // UTF-8; `%20` decodes to whitespace. Construction succeeds for both —
    // the 400 comes from the server's path extraction / id validation. If a
    // client layer ever refused these before send, the unwrap would surface
    // it as a client-boundary result instead of server evidence.
    for path_id in ["%FF", "%20"] {
        let request = http
            .post(format!("{}/api/sessions/{path_id}/turns", fixture.h.origin))
            .header("x-peachsh-token", &fixture.h.token)
            .header("idempotency-key", "bad-path")
            .header("content-type", "application/json")
            .body(legal.clone())
            .build()
            .unwrap_or_else(|error| panic!("{path_id} rejected at client build: {error}"));
        let response = http.execute(request).await.unwrap();
        let error = expect_status(response, 400, "request_failed", false).await;
        hides(&error, &["sk-testtoken123"]);
    }

    let long = "x".repeat(129);
    let wide = "界".repeat(43);
    let secret = "sk-testtoken123";
    for bad in [" ", "a\tb", long.as_str(), wide.as_str(), secret] {
        for field in ["agent_id", "expected_last_turn_id"] {
            let mut value: Value = serde_json::from_str(&legal).unwrap();
            value[field] = json!(bad);
            let response = post_raw(
                &http,
                &fixture.h,
                &session,
                Some("bad-id"),
                value.to_string(),
            )
            .send()
            .await
            .unwrap();
            let error = expect_status(response, 400, "request_failed", false).await;
            hides(&error, &[secret, bad]);
        }
    }

    let missing_wide = "界".repeat(40);
    let positives = [
        (missing_wide.as_str(), agent.as_str(), "不存在的宽字符前序"),
        (
            "not-a-uuid-legal",
            previous.as_str(),
            "合法但不是 uuid 的 agent",
        ),
        (agent.as_str(), "missing-turn", "不存在的前序"),
    ];
    // The first positive uses the wide id as expected_last_turn_id.
    let response = post_raw(
        &http,
        &fixture.h,
        &session,
        Some("missing-wide"),
        json!({
            "agent_id": agent,
            "expected_last_turn_id": missing_wide,
            "message": "宽字符但不存在"
        })
        .to_string(),
    )
    .send()
    .await
    .unwrap();
    expect_status(response, 404, "not_found", false).await;
    let response = post_raw(
        &http,
        &fixture.h,
        &session,
        Some("missing-agent"),
        json!({
            "agent_id": "not-a-uuid-legal",
            "expected_last_turn_id": previous,
            "message": "agent 不存在"
        })
        .to_string(),
    )
    .send()
    .await
    .unwrap();
    expect_status(response, 404, "not_found", false).await;
    let response = post_raw(
        &http,
        &fixture.h,
        &session,
        Some("missing-turn"),
        json!({
            "agent_id": agent,
            "expected_last_turn_id": "missing-turn",
            "message": "前序不存在"
        })
        .to_string(),
    )
    .send()
    .await
    .unwrap();
    expect_status(response, 404, "not_found", false).await;
    let _ = positives;

    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);
}

#[tokio::test]
async fn n06_idempotency_header() {
    let fixture = started().await;
    let http = client();
    let calls_before = calls(&fixture.h);
    let before = snapshot(&fixture.h.db_path);
    let session = &fixture.chat.session.id.0;
    let body = json!({
        "agent_id": fixture.chat.agent.id.0,
        "expected_last_turn_id": fixture.chat.turn.id.0,
        "message": "头校验"
    })
    .to_string();
    let response = http
        .post(format!("{}/api/sessions/{session}/turns", fixture.h.origin))
        .header("x-peachsh-token", &fixture.h.token)
        .header("content-type", "application/json")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    expect_status(response, 400, "request_failed", false).await;

    for (left, right) in [("same", "same"), ("left", "right")] {
        let response = http
            .post(format!("{}/api/sessions/{session}/turns", fixture.h.origin))
            .header("x-peachsh-token", &fixture.h.token)
            .header("content-type", "application/json")
            .header("idempotency-key", left)
            .header("idempotency-key", right)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        expect_status(response, 400, "request_failed", false).await;
    }
    for bad in [
        "key-a,key-b",
        " ",
        "a b",
        &"k".repeat(201),
        "sk-testtoken123",
    ] {
        let response = post_raw(&http, &fixture.h, session, Some(bad), body.clone())
            .send()
            .await
            .unwrap();
        let error = expect_status(response, 400, "request_failed", false).await;
        hides(&error, &["sk-testtoken123"]);
    }
    // obs-text bytes (>=0x80) are legal in HeaderValue, so these values are
    // constructed and sent; the server rejects them because `to_str` only
    // accepts visible ASCII. If `from_bytes` or send ever rejected them the
    // unwraps would surface a client-boundary result instead of server 400s.
    for bytes in ["kéy".as_bytes(), &[0x80u8][..]] {
        let value = reqwest::header::HeaderValue::from_bytes(bytes)
            .unwrap_or_else(|_| panic!("{bytes:?} rejected at client construction"));
        let response = http
            .post(format!("{}/api/sessions/{session}/turns", fixture.h.origin))
            .header("x-peachsh-token", &fixture.h.token)
            .header("content-type", "application/json")
            .header("idempotency-key", value)
            .body(body.clone())
            .send()
            .await
            .unwrap_or_else(|error| panic!("{bytes:?} rejected at client send: {error}"));
        expect_status(response, 400, "request_failed", false).await;
    }
    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);

    let long = "k".repeat(200);
    let created = append(&http, &fixture, &fixture.chat.turn.id.0, "长 key", &long).await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_body: Value = created.json().await.unwrap();
    let (turn2, task2) = receipt(&created_body, &fixture, false);
    let again = append(&http, &fixture, &fixture.chat.turn.id.0, "长 key", &long).await;
    assert_eq!(again.status(), reqwest::StatusCode::OK);
    let again_body: Value = again.json().await.unwrap();
    assert_eq!(again_body["turn"]["id"], turn2.id.0);
    let done = wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    assert_eq!(done.status, "completed");

    let upper = http
        .post(format!(
            "{}/api/sessions/{}/turns",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .header("x-peachsh-token", &fixture.h.token)
        .header("IDEMPOTENCY-KEY", "mixed-case")
        .json(&json!({
            "agent_id": fixture.chat.agent.id.0,
            "expected_last_turn_id": turn2.id.0,
            "message": "大小写头"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(upper.status(), reqwest::StatusCode::CREATED);
    let upper_body: Value = upper.json().await.unwrap();
    let (_, upper_task) = receipt(&upper_body, &fixture, false);
    wait_task(&fixture.h.engine, &upper_task.legacy_task_id).await;

    let encoded = append(
        &http,
        &fixture,
        upper_body["turn"]["id"].as_str().unwrap(),
        "不解码",
        "a%20b",
    )
    .await;
    assert_eq!(encoded.status(), reqwest::StatusCode::CREATED);
    let stored = scalar(
        &fixture.h.db_path,
        "SELECT key FROM idempotency_records WHERE key=?1",
        "a%20b",
    );
    assert_eq!(stored, "a%20b");

    let query = http
        .post(format!(
            "{}/api/sessions/{}/turns?idempotency_key=x",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .header("x-peachsh-token", &fixture.h.token)
        .json(&json!({
            "agent_id": fixture.chat.agent.id.0,
            "expected_last_turn_id": fixture.chat.turn.id.0,
            "message": "query 不能代替头"
        }))
        .send()
        .await
        .unwrap();
    expect_status(query, 400, "request_failed", false).await;
}

#[tokio::test]
async fn n07_message_bounds_and_body_limit() {
    let fixture = started().await;
    let http = client();
    for blank in ["   ", ""] {
        let response = append(&http, &fixture, &fixture.chat.turn.id.0, blank, "blank").await;
        expect_status(response, 400, "request_failed", false).await;
    }
    let exact = "x".repeat(100_000);
    let created = append(&http, &fixture, &fixture.chat.turn.id.0, &exact, "exact-x").await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_body: Value = created.json().await.unwrap();
    let (turn2, task2) = receipt(&created_body, &fixture, false);
    wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    let over = append(&http, &fixture, &turn2.id.0, &"x".repeat(100_001), "over-x").await;
    expect_status(over, 400, "request_failed", false).await;

    let wide = format!("{}a", "界".repeat(33_333));
    assert_eq!(wide.len(), 100_000);
    let wide_ok = append(&http, &fixture, &turn2.id.0, &wide, "exact-wide").await;
    assert_eq!(wide_ok.status(), reqwest::StatusCode::CREATED);
    let wide_body: Value = wide_ok.json().await.unwrap();
    let (turn3, task3) = receipt(&wide_body, &fixture, false);
    wait_task(&fixture.h.engine, &task3.legacy_task_id).await;
    let wide_over = "界".repeat(33_334);
    assert_eq!(wide_over.len(), 100_002);
    let wide_bad = append(&http, &fixture, &turn3.id.0, &wide_over, "over-wide").await;
    expect_status(wide_bad, 400, "request_failed", false).await;

    let baseline = append(&http, &fixture, &turn3.id.0, "基准可追加", "before-fat").await;
    assert_eq!(baseline.status(), reqwest::StatusCode::CREATED);
    let baseline_body: Value = baseline.json().await.unwrap();
    let (turn4, task4) = receipt(&baseline_body, &fixture, false);
    wait_task(&fixture.h.engine, &task4.legacy_task_id).await;
    // The engine rejects when `serde_json::to_vec(previous.messages + new user
    // message)` exceeds 1_500_000 bytes. Compute the real candidate length
    // from the predecessor's stored messages instead of guessing a pad size.
    let saved_value = scalar(
        &fixture.h.db_path,
        "SELECT value FROM tasks WHERE id=?1",
        &task4.legacy_task_id,
    );
    let base_messages = serde_json::from_str::<Value>(&saved_value).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let new_message = "再加一条";
    let candidate_len = |pad: usize| -> usize {
        let mut messages = base_messages.clone();
        messages.push(json!({"role":"user","content":"y".repeat(pad)}));
        messages.push(json!({"role":"user","content":new_message}));
        serde_json::to_vec(&messages).unwrap().len()
    };
    let fixed = candidate_len(0);
    assert!(
        fixed < 1_500_000,
        "fixture overhead alone exceeds the threshold"
    );
    let inject_fat = |pad: usize| {
        open_db(&fixture.h.db_path)
            .execute(
                "UPDATE tasks SET value=json_set(value, '$.messages[#]', json(?1)) WHERE id=?2",
                params![
                    json!({"role":"user","content":"y".repeat(pad)}).to_string(),
                    task4.legacy_task_id
                ],
            )
            .unwrap();
    };
    // Over-limit: pad so the serialized candidate is exactly 1 byte too long.
    let over_pad = 1_500_001 - fixed;
    assert!(candidate_len(over_pad) > 1_500_000);
    eprintln!(
        "n07 cumulative: fixed={fixed} over_pad={over_pad} over_len={} boundary_pad={} boundary_len={}",
        candidate_len(over_pad),
        1_500_000 - fixed,
        candidate_len(1_500_000 - fixed),
    );
    inject_fat(over_pad);
    let injected = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let too_big = append(&http, &fixture, &turn4.id.0, new_message, "after-fat").await;
    expect_status(too_big, 400, "request_failed", false).await;
    assert_untouched(&injected, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);
    // Boundary positive: restore the predecessor row, then a fat history that
    // lands exactly on the threshold must still append and replay.
    open_db(&fixture.h.db_path)
        .execute(
            "UPDATE tasks SET value=?1 WHERE id=?2",
            params![saved_value, task4.legacy_task_id],
        )
        .unwrap();
    let boundary_pad = 1_500_000 - fixed;
    assert!(candidate_len(boundary_pad) <= 1_500_000);
    inject_fat(boundary_pad);
    let boundary_ok = append(&http, &fixture, &turn4.id.0, new_message, "fat-boundary").await;
    assert_eq!(boundary_ok.status(), reqwest::StatusCode::CREATED);
    let boundary_body: Value = boundary_ok.json().await.unwrap();
    let (turn5, task5) = receipt(&boundary_body, &fixture, false);
    wait_task(&fixture.h.engine, &task5.legacy_task_id).await;
    let boundary_replay = append(&http, &fixture, &turn4.id.0, new_message, "fat-boundary").await;
    assert_eq!(boundary_replay.status(), reqwest::StatusCode::OK);
    let replay_body: Value = boundary_replay.json().await.unwrap();
    assert_eq!(replay_body["replayed"], true);
    assert_eq!(replay_body["turn"]["id"], turn5.id.0);

    let no_type = http
        .post(format!(
            "{}/api/sessions/{}/turns",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .header("x-peachsh-token", &fixture.h.token)
        .header("idempotency-key", "no-type")
        .body(r#"{"agent_id":"a","expected_last_turn_id":"b","message":"c"}"#)
        .send()
        .await
        .unwrap();
    expect_status(no_type, 415, "request_failed", false).await;
    let plain = http
        .post(format!(
            "{}/api/sessions/{}/turns",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .header("x-peachsh-token", &fixture.h.token)
        .header("idempotency-key", "plain")
        .header("content-type", "text/plain")
        .body("not-json")
        .send()
        .await
        .unwrap();
    expect_status(plain, 415, "request_failed", false).await;
    let broken = post_raw(
        &http,
        &fixture.h,
        &fixture.chat.session.id.0,
        Some("broken"),
        "{".into(),
    )
    .send()
    .await
    .unwrap();
    expect_status(broken, 400, "request_failed", false).await;
    let huge = "z".repeat(1_048_577);
    let limited = post_raw(
        &http,
        &fixture.h,
        &fixture.chat.session.id.0,
        Some("huge"),
        huge,
    )
    .send()
    .await
    .unwrap();
    expect_status(limited, 413, "request_failed", false).await;

    let clean = started().await;
    let padded = append(
        &http,
        &clean,
        &clean.chat.turn.id.0,
        "  padded-message  ",
        "padded",
    )
    .await;
    assert_eq!(padded.status(), reqwest::StatusCode::CREATED);
    let padded_body: Value = padded.json().await.unwrap();
    let (padded_turn, _) = receipt(&padded_body, &clean, false);
    assert_eq!(
        padded_turn.request_hash,
        expected_hash(
            &clean,
            &clean.chat.turn.id.0,
            "  padded-message  ",
            "padded"
        )
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if last_message_for(&clean.h, "  padded-message  ").is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("padded message was not forwarded");
    assert_eq!(
        last_message_for(&clean.h, "  padded-message  ").unwrap()["content"],
        "  padded-message  "
    );
}

#[tokio::test]
async fn n08_write_guard() {
    let fixture = started().await;
    let http = client();
    let calls_before = calls(&fixture.h);
    let before = snapshot(&fixture.h.db_path);
    let body = json!({
        "agent_id": fixture.chat.agent.id.0,
        "expected_last_turn_id": fixture.chat.turn.id.0,
        "message": "应被守卫拒绝"
    });
    let url = format!(
        "{}/api/sessions/{}/turns",
        fixture.h.origin, fixture.chat.session.id.0
    );
    let mut wrong_host = http
        .post(&url)
        .header("host", "evil.example")
        .header("x-peachsh-token", &fixture.h.token)
        .json(&body);
    let _ = &mut wrong_host;
    for response in [
        wrong_host.send().await.unwrap(),
        http.post(&url)
            .header("origin", "http://evil.example")
            .header("x-peachsh-token", &fixture.h.token)
            .json(&body)
            .send()
            .await
            .unwrap(),
        http.post(&url)
            .header("sec-fetch-site", "cross-site")
            .header("x-peachsh-token", &fixture.h.token)
            .json(&body)
            .send()
            .await
            .unwrap(),
        http.post(&url).json(&body).send().await.unwrap(),
        http.post(&url)
            .header("x-peachsh-token", "wrong-token")
            .json(&body)
            .send()
            .await
            .unwrap(),
    ] {
        let error = expect_status(response, 403, "forbidden", false).await;
        hides(&error, &["应被守卫拒绝", "wrong-token"]);
    }
    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);

    let created = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "守卫放行",
        "guard-ok",
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    security_headers(&created);
}

#[tokio::test]
async fn n09_session_shape_and_404() {
    let fixture = started().await;
    let http = client();
    for (session, agent, previous, key) in [
        (
            "missing-session",
            fixture.chat.agent.id.0.as_str(),
            fixture.chat.turn.id.0.as_str(),
            "missing-session",
        ),
        (
            fixture.chat.session.id.0.as_str(),
            "missing-agent",
            fixture.chat.turn.id.0.as_str(),
            "missing-agent",
        ),
        (
            fixture.chat.session.id.0.as_str(),
            fixture.chat.agent.id.0.as_str(),
            "missing-turn",
            "missing-turn",
        ),
    ] {
        let before = snapshot(&fixture.h.db_path);
        let calls_before = calls(&fixture.h);
        let response = turn_post(&http, &fixture.h, session, agent, previous, "找不到", key)
            .send()
            .await
            .unwrap();
        expect_status(response, 404, "not_found", false).await;
        assert_untouched(&before, &fixture.h.db_path);
        assert_eq!(calls(&fixture.h), calls_before);
    }

    let other = spawn_chat(&fixture.h, "第二会话", "另一轮", "shape-other", "chat").await;
    let proved = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "形状基准",
        "shape-ok",
    )
    .await;
    assert_eq!(proved.status(), reqwest::StatusCode::CREATED);
    let proved_body: Value = proved.json().await.unwrap();
    let (_, proved_task) = receipt(&proved_body, &fixture, false);
    wait_task(&fixture.h.engine, &proved_task.legacy_task_id).await;

    // Each rejection below takes its pre-request snapshot after every fixture
    // mutation settles, so a write by the request itself would be caught.
    let before = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let foreign_agent = turn_post(
        &http,
        &fixture.h,
        &fixture.chat.session.id.0,
        &other.agent.id.0,
        proved_body["turn"]["id"].as_str().unwrap(),
        "别人的 agent",
        "foreign-agent",
    )
    .send()
    .await
    .unwrap();
    expect_status(foreign_agent, 400, "request_failed", false).await;
    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);

    let before = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let foreign_turn = turn_post(
        &http,
        &fixture.h,
        &fixture.chat.session.id.0,
        &fixture.chat.agent.id.0,
        &other.turn.id.0,
        "别人的前序",
        "foreign-turn",
    )
    .send()
    .await
    .unwrap();
    expect_status(foreign_turn, 400, "request_failed", false).await;
    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);

    let team = spawn_chat(&fixture.h, "团队", "团队首轮", "team-create", "team").await;
    let before = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let team_post = turn_post(
        &http,
        &fixture.h,
        &team.session.id.0,
        &team.agent.id.0,
        &team.turn.id.0,
        "团队不能追加",
        "team-turn",
    )
    .send()
    .await
    .unwrap();
    expect_status(team_post, 400, "request_failed", false).await;
    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);

    let created = now();
    fixture
        .h
        .engine
        .store
        .insert_agent(&Agent {
            id: AgentId("second-agent".into()),
            session_id: fixture.chat.session.id.clone(),
            display_name: "第二个".into(),
            role: "旁听".into(),
            created_at: created,
        })
        .unwrap();
    let injected = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let second = append(
        &http,
        &fixture,
        proved_body["turn"]["id"].as_str().unwrap(),
        "双 agent",
        "two-agents",
    )
    .await;
    expect_status(second, 400, "request_failed", false).await;
    assert_untouched(&injected, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);
    open_db(&fixture.h.db_path)
        .execute("DELETE FROM agents WHERE id='second-agent'", [])
        .unwrap();

    // Dependency/history faults: restore the predecessor task to its exact saved
    // baseline before each injection, so every case is triggered by that
    // fault alone and never by residue from the previous case. Mutate this
    // task's final assistant message; inherited prefix mutations are corruption.
    let latest = proved_task.legacy_task_id.clone();
    let original_value = scalar(
        &fixture.h.db_path,
        "SELECT value FROM tasks WHERE id=?1",
        &latest,
    );
    let mutations = [
        (
            "UPDATE tasks SET value=json_set(value, '$.spec.depends_on', json('[\"foreign-task\"]')) WHERE id=?1",
            "dependency-present",
        ),
        (
            "UPDATE tasks SET value=json_set(value, '$.messages[#-1].role', 'tool') WHERE id=?1",
            "role-tool",
        ),
        (
            "UPDATE tasks SET value=json_set(value, '$.messages[#-1].tool_calls', json('[{\"id\":\"c\"}]')) WHERE id=?1",
            "calls-present",
        ),
        (
            "UPDATE tasks SET value=json_set(value, '$.messages[#-1].tool_calls', json('\"yes\"')) WHERE id=?1",
            "calls-text",
        ),
        (
            "UPDATE tasks SET value=json_insert(value, '$.messages[#]', json('{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[{\"id\":\"forged\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"src/a.txt\\\"}\"}}]}'), '$.messages[#]', json('{\"role\":\"tool\",\"tool_call_id\":\"forged\",\"content\":\"{}\"}')) WHERE id=?1",
            "closed-calls-without-source-event",
        ),
    ];
    for (sql, key) in mutations {
        open_db(&fixture.h.db_path)
            .execute(
                "UPDATE tasks SET value=?1 WHERE id=?2",
                params![original_value, latest],
            )
            .unwrap();
        open_db(&fixture.h.db_path).execute(sql, [&latest]).unwrap();
        let injected = snapshot(&fixture.h.db_path);
        let calls_before = calls(&fixture.h);
        let response = append(
            &http,
            &fixture,
            proved_body["turn"]["id"].as_str().unwrap(),
            key,
            key,
        )
        .await;
        expect_status(response, 400, "request_failed", false).await;
        assert_untouched(&injected, &fixture.h.db_path);
        assert_eq!(calls(&fixture.h), calls_before);
    }
    // Inherited transcript is immutable even if the new message itself is valid.
    open_db(&fixture.h.db_path)
        .execute(
            "UPDATE tasks SET value=?1 WHERE id=?2",
            params![original_value, latest],
        )
        .unwrap();
    open_db(&fixture.h.db_path)
        .execute(
            "UPDATE tasks SET value=json_set(value, '$.messages[0].role', 'tool') WHERE id=?1",
            [&latest],
        )
        .unwrap();
    let injected = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let inherited = append(
        &http,
        &fixture,
        proved_body["turn"]["id"].as_str().unwrap(),
        "damaged inherited prefix",
        "inherited-prefix-corrupt",
    )
    .await;
    expect_status(inherited, 500, "internal", false).await;
    assert_untouched(&injected, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);

    // Positive: the same restored clean baseline (tools=false, valid message roles, no
    // tool_calls residue) plus only an empty tool_calls array
    // must still append.
    open_db(&fixture.h.db_path)
        .execute(
            "UPDATE tasks SET value=?1 WHERE id=?2",
            params![original_value, latest],
        )
        .unwrap();
    open_db(&fixture.h.db_path)
        .execute(
            "UPDATE tasks SET value=json_set(value, '$.messages[#-1].tool_calls', json('[]')) WHERE id=?1",
            [&latest],
        )
        .unwrap();
    let empty_calls = append(
        &http,
        &fixture,
        proved_body["turn"]["id"].as_str().unwrap(),
        "空工具调用",
        "empty-calls",
    )
    .await;
    assert_eq!(empty_calls.status(), reqwest::StatusCode::CREATED);
}

#[tokio::test]
async fn n10_predecessor_states() {
    let fixture = started().await;
    let http = client();
    let created = append(&http, &fixture, &fixture.chat.turn.id.0, "第二轮", "pred-2").await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: Value = created.json().await.unwrap();
    let (turn2, task2) = receipt(&body, &fixture, false);
    wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    let stale_before = snapshot(&fixture.h.db_path);
    let stale_calls = calls(&fixture.h);
    let stale = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "陈旧前序",
        "stale",
    )
    .await;
    expect_status(stale, 409, "conflict", false).await;
    assert_untouched(&stale_before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), stale_calls);

    let held = append(&http, &fixture, &turn2.id.0, "hold:busy", "busy-key").await;
    assert_eq!(held.status(), reqwest::StatusCode::CREATED);
    let held_body: Value = held.json().await.unwrap();
    let (running, running_task) = receipt(&held_body, &fixture, false);
    wait_delta(
        &fixture.h.db_path,
        &running_task.legacy_task_id,
        "partial-busy",
    )
    .await;
    // The mock is parked on `hold:` now, so no further events or provider
    // calls can appear; the snapshot below captures a stable busy baseline.
    let busy_before = snapshot(&fixture.h.db_path);
    let busy_calls = calls(&fixture.h);
    let on_running = append(&http, &fixture, &running.id.0, "顶着运行中", "on-running").await;
    expect_status(on_running, 409, "conflict", false).await;
    assert_untouched(&busy_before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), busy_calls);
    let behind_before = snapshot(&fixture.h.db_path);
    let behind = append(&http, &fixture, &turn2.id.0, "顶着旧前序", "behind").await;
    expect_status(behind, 409, "conflict", false).await;
    assert_untouched(&behind_before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), busy_calls);
    // Release only after every busy-state assertion has completed.
    fixture.h.probe.release.notify_waiters();
    let done = wait_task(&fixture.h.engine, &running_task.legacy_task_id).await;
    assert_eq!(done.status, "completed");

    for status in ["cancelled", "interrupted"] {
        let fresh = started().await;
        let created = append(&http, &fresh, &fresh.chat.turn.id.0, "可改状态", "seed").await;
        assert_eq!(created.status(), reqwest::StatusCode::CREATED);
        let body: Value = created.json().await.unwrap();
        let (turn2, task2) = receipt(&body, &fresh, false);
        wait_task(&fresh.h.engine, &task2.legacy_task_id).await;
        let db = open_db(&fresh.h.db_path);
        db.execute(
            "UPDATE turns SET status=?1 WHERE id=?2",
            params![status, turn2.id.0],
        )
        .unwrap();
        db.execute(
            "UPDATE turn_tasks SET status=?1 WHERE turn_id=?2",
            params![status, turn2.id.0],
        )
        .unwrap();
        db.execute(
            "UPDATE tasks SET value=json_set(value, '$.status', ?1) WHERE id=?2",
            params![status, task2.legacy_task_id],
        )
        .unwrap();
        drop(db);
        let injected = snapshot(&fresh.h.db_path);
        let calls_before = calls(&fresh.h);
        let blocked = append(&http, &fresh, &turn2.id.0, "终态不能追加", "blocked").await;
        expect_status(blocked, 409, "conflict", false).await;
        assert_untouched(&injected, &fresh.h.db_path);
        assert_eq!(calls(&fresh.h), calls_before);
    }
    // PredecessorChanged cannot be forced through HTTP: the candidate is built
    // from the predecessor inside the same locked command, so the snapshot cannot
    // drift between build and commit. Its 409 mapping is covered by
    // `turn_command_error_maps_every_stable_variant` and the store-layer regression.
}

#[tokio::test]
async fn n11_same_key_concurrency() {
    let fixture = started().await;
    let http = client();
    let before = snapshot(&fixture.h.db_path);
    let left = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "并发同 key",
        "race-key",
    );
    let right = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "并发同 key",
        "race-key",
    );
    let (left, right) = tokio::join!(left, right);
    let mut created = 0;
    let mut replayed = 0;
    let mut turn_id = None;
    for response in [left, right] {
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        match status.as_u16() {
            201 => {
                created += 1;
                let (turn, _) = receipt(&body, &fixture, false);
                turn_id = Some(turn.id.0);
            }
            200 => {
                replayed += 1;
                assert_eq!(body["replayed"], true);
                turn_id = Some(body["turn"]["id"].as_str().unwrap().to_owned());
            }
            other => panic!("unexpected status {other}: {body}"),
        }
    }
    assert_eq!((created, replayed), (1, 1));
    // Wait for the provider call to be observable (persisted delta, then a
    // settled task) before reading the background call counter — the HTTP
    // 201 returns before the launched task has necessarily been scheduled.
    let legacy = scalar(
        &fixture.h.db_path,
        "SELECT legacy_task_id FROM turn_tasks WHERE turn_id=?1",
        turn_id.as_deref().unwrap(),
    );
    wait_delta(&fixture.h.db_path, &legacy, "reply").await;
    let done = wait_task(&fixture.h.engine, &legacy).await;
    assert_eq!(done.status, "completed");
    assert_eq!(calls_for(&fixture.h, "并发同 key"), 1);
    let after = snapshot(&fixture.h.db_path);
    for name in [
        "turns",
        "turn_tasks",
        "tasks",
        "idempotency_records",
        "idempotency",
    ] {
        assert_grew_only(&before, &after, name, 1);
    }
    let queued = count_where(
        &fixture.h.db_path,
        "SELECT count(*) FROM events WHERE turn_id=?1 AND kind='status' AND data LIKE '%queued%'",
        [turn_id.as_deref().unwrap()],
    );
    assert_eq!(queued, 1);

    let second = started().await;
    let before = snapshot(&second.h.db_path);
    let store = Arc::new(Store::open(&second.h.db_path).unwrap());
    let engine = Engine::new(store, 2).unwrap();
    let (origin, _http) = serve(engine, &second.h.token).await;
    let peer = Harness {
        _dir: tempfile::tempdir().unwrap(),
        db_path: second.h.db_path.clone(),
        origin,
        token: second.h.token.clone(),
        engine: second.h.engine.clone(),
        probe: second.h.probe.clone(),
        _http,
    };
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let left_barrier = barrier.clone();
    let right_barrier = barrier.clone();
    let left_http = client();
    let right_http = client();
    let left = async {
        left_barrier.wait().await;
        append(
            &left_http,
            &second,
            &second.chat.turn.id.0,
            "双引擎",
            "dual-key",
        )
        .await
    };
    let right = async {
        right_barrier.wait().await;
        turn_post(
            &right_http,
            &peer,
            &second.chat.session.id.0,
            &second.chat.agent.id.0,
            &second.chat.turn.id.0,
            "双引擎",
            "dual-key",
        )
        .send()
        .await
        .unwrap()
    };
    let (left, right) = tokio::join!(left, right);
    let mut created = 0;
    let mut replayed = 0;
    let mut winner_legacy = None;
    for response in [left, right] {
        match response.status().as_u16() {
            201 => {
                created += 1;
                let body: Value = response.json().await.unwrap();
                winner_legacy = Some(body["task"]["legacy_task_id"].as_str().unwrap().to_owned());
            }
            200 => {
                replayed += 1;
                let body: Value = response.json().await.unwrap();
                assert_eq!(body["replayed"], true);
            }
            other => panic!("unexpected dual-engine status {other}"),
        }
    }
    assert_eq!((created, replayed), (1, 1));
    // The winner may run on the peer engine; its writes land in the shared
    // database, so observe the persisted delta and settled row there before
    // reading the background call counter.
    let legacy = winner_legacy.expect("one request must create");
    wait_delta(&second.h.db_path, &legacy, "reply").await;
    let done = wait_task(&second.h.engine, &legacy).await;
    assert_eq!(done.status, "completed");
    assert_eq!(calls_for(&second.h, "双引擎"), 1);
    let after = snapshot(&second.h.db_path);
    for name in [
        "turns",
        "turn_tasks",
        "tasks",
        "idempotency_records",
        "idempotency",
    ] {
        assert_grew_only(&before, &after, name, 1);
    }
}

#[tokio::test]
async fn n12_different_key_race() {
    let fixture = started().await;
    // Baseline before the barrier: every later write is attributable to the
    // race winner or the loser's separate retry, never to the rejected 409.
    let before = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let seq_before = max_seq(&fixture.h.db_path);
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let left_barrier = barrier.clone();
    let right_barrier = barrier;
    let left_http = client();
    let right_http = client();
    let left = async {
        left_barrier.wait().await;
        append(
            &left_http,
            &fixture,
            &fixture.chat.turn.id.0,
            "左 key",
            "left-key",
        )
        .await
    };
    let right = async {
        right_barrier.wait().await;
        append(
            &right_http,
            &fixture,
            &fixture.chat.turn.id.0,
            "右 key",
            "right-key",
        )
        .await
    };
    let (left, right) = tokio::join!(left, right);
    let mut winner = None;
    let mut loser = None;
    for (response, key, message) in [(left, "left-key", "左 key"), (right, "right-key", "右 key")]
    {
        match response.status().as_u16() {
            201 => {
                let body: Value = response.json().await.unwrap();
                let (turn, task) = receipt(&body, &fixture, false);
                winner = Some((turn, task, key, message));
            }
            409 => {
                let body = expect_status(response, 409, "conflict", false).await;
                hides(&body, &["左 key", "右 key"]);
                loser = Some((key, message));
            }
            other => panic!("unexpected race status {other}"),
        }
    }
    let (turn, task, _winner_key, winner_message) = winner.expect("one request must win");
    let (loser_key, loser_message) = loser.expect("one request must lose");
    wait_delta(&fixture.h.db_path, &task.legacy_task_id, "reply").await;
    let done = wait_task(&fixture.h.engine, &task.legacy_task_id).await;
    assert_eq!(done.status, "completed");
    let after = snapshot(&fixture.h.db_path);
    // Exactly one new object group belongs to the winner; the loser's 409
    // request added nothing and never reached the provider.
    for name in [
        "turns",
        "turn_tasks",
        "tasks",
        "idempotency_records",
        "idempotency",
    ] {
        assert_grew_only(&before, &after, name, 1);
    }
    let new_events = count_where(
        &fixture.h.db_path,
        "SELECT count(*) FROM events WHERE seq>?1",
        [seq_before],
    );
    let winner_events = count_where(
        &fixture.h.db_path,
        "SELECT count(*) FROM events WHERE seq>?1 AND turn_id=?2 AND task_id=?3",
        params![seq_before, turn.id.0, task.legacy_task_id],
    );
    assert!(new_events > 0, "winner produced no events");
    assert_eq!(new_events, winner_events, "events from a non-winner leaked");
    assert_eq!(calls_for(&fixture.h, winner_message), 1);
    assert_eq!(calls_for(&fixture.h, loser_message), 0);
    assert_eq!(calls(&fixture.h), calls_before + 1);

    // The losing key's retry is a separate new command on the latest turn;
    // its own before/after snapshots keep it out of the race accounting.
    let retry_before = snapshot(&fixture.h.db_path);
    let retry_seq = max_seq(&fixture.h.db_path);
    let retry_calls = calls(&fixture.h);
    let retry = append(&client(), &fixture, &turn.id.0, loser_message, loser_key).await;
    assert_eq!(retry.status(), reqwest::StatusCode::CREATED);
    let retry_body: Value = retry.json().await.unwrap();
    let (retry_turn, retry_task) = receipt(&retry_body, &fixture, false);
    assert_ne!(retry_turn.id, turn.id);
    wait_delta(&fixture.h.db_path, &retry_task.legacy_task_id, "reply").await;
    let retry_done = wait_task(&fixture.h.engine, &retry_task.legacy_task_id).await;
    assert_eq!(retry_done.status, "completed");
    let retry_after = snapshot(&fixture.h.db_path);
    for name in [
        "turns",
        "turn_tasks",
        "tasks",
        "idempotency_records",
        "idempotency",
    ] {
        assert_grew_only(&retry_before, &retry_after, name, 1);
    }
    let retry_events = count_where(
        &fixture.h.db_path,
        "SELECT count(*) FROM events WHERE seq>?1 AND turn_id=?2",
        params![retry_seq, retry_turn.id.0],
    );
    let any_new_events = count_where(
        &fixture.h.db_path,
        "SELECT count(*) FROM events WHERE seq>?1",
        [retry_seq],
    );
    assert_eq!(any_new_events, retry_events);
    assert_eq!(calls_for(&fixture.h, loser_message), 1);
    assert_eq!(calls(&fixture.h), retry_calls + 1);
}

async fn prove_then_fault(label: &str, fault: impl FnOnce(&Path, &Fixture, &Turn, &TurnTask)) {
    let fixture = started().await;
    let http = client();
    let created = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "故障基准",
        "fault-base",
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: Value = created.json().await.unwrap();
    let (turn2, task2) = receipt(&body, &fixture, false);
    wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    fault(&fixture.h.db_path, &fixture, &turn2, &task2);
    let injected = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let response = append(&http, &fixture, &turn2.id.0, "故障后追加", "fault-next").await;
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(!text.contains("SENTINEL-XYZ"), "{text}");
    assert!(!text.to_ascii_lowercase().contains("denied"), "{text}");
    assert!(
        !text.contains(&fixture.h.db_path.display().to_string()),
        "{text}"
    );
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        status,
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "{label}: {body}"
    );
    assert_eq!(body["code"], "internal");
    assert_eq!(body["retryable"], false);
    assert_eq!(body["error"], body["message"]);
    assert_untouched(&injected, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);
}

#[tokio::test]
async fn n13_storage_faults_safe_500() {
    // `created_at=-1` is a negative epoch second. session/turn lookup reads the
    // column as u64, so rusqlite raises OutOfRange; classify_row wraps that as
    // CorruptState and the HTTP layer maps it to 500. agent_lookup reads i64 and
    // treats `< 0` as CorruptState directly.
    prove_then_fault("session", |path, fixture, _, _| {
        open_db(path)
            .execute(
                "UPDATE sessions SET created_at=-1 WHERE id=?1",
                [&fixture.chat.session.id.0],
            )
            .unwrap();
    })
    .await;
    prove_then_fault("agent", |path, fixture, _, _| {
        open_db(path)
            .execute(
                "UPDATE agents SET created_at=-1 WHERE id=?1",
                [&fixture.chat.agent.id.0],
            )
            .unwrap();
    })
    .await;
    prove_then_fault("turn", |path, _, turn2, _| {
        open_db(path)
            .execute("UPDATE turns SET created_at=-1 WHERE id=?1", [&turn2.id.0])
            .unwrap();
    })
    .await;
    prove_then_fault("events", |path, fixture, _, _| {
        open_db(path)
            .execute(
                "DELETE FROM events WHERE turn_id=?1",
                [&fixture.chat.turn.id.0],
            )
            .unwrap();
    })
    .await;
    prove_then_fault("json", |path, _, _, task2| {
        // The predecessor task is loaded from this row inside the append command.
        open_db(path)
            .execute(
                "UPDATE tasks SET value='{\"SENTINEL-XYZ\":true' WHERE id=?1",
                [&task2.legacy_task_id],
            )
            .unwrap();
    })
    .await;
    let fixture = started().await;
    let http = client();
    let created = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "触发器基准",
        "fault-base",
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: Value = created.json().await.unwrap();
    let (_, task2) = receipt(&body, &fixture, false);
    wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    open_db(&fixture.h.db_path)
        .execute_batch(
            "CREATE TRIGGER deny_turn_events BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'denied'); END",
        )
        .unwrap();
    let injected = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let response = append(
        &http,
        &fixture,
        body["turn"]["id"].as_str().unwrap(),
        "触发器后",
        "fault-next",
    )
    .await;
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(!text.to_ascii_lowercase().contains("denied"), "{text}");
    assert!(
        !text.contains(&fixture.h.db_path.display().to_string()),
        "{text}"
    );
    let error: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        status,
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "{error}"
    );
    assert_eq!(error["code"], "internal");
    assert_eq!(error["retryable"], false);
    assert_untouched(&injected, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);
    assert_eq!(
        count_where(
            &fixture.h.db_path,
            "SELECT count(*) FROM idempotency_records WHERE key=?1",
            ["fault-next"]
        ),
        0,
        "a trigger abort must roll the whole append back"
    );
}

#[tokio::test]
async fn n14_config_faults_500_and_replay_first() {
    let fixture = started().await;
    let http = client();
    let created = append(&http, &fixture, &fixture.chat.turn.id.0, "配置基准", "cfg").await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: Value = created.json().await.unwrap();
    let (turn2, task2) = receipt(&body, &fixture, false);
    wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    let mut settings = fixture.h.engine.store.settings().unwrap().unwrap();
    let saved = settings.clone();
    settings.routes.clear();
    fixture.h.engine.store.save_settings(&settings).unwrap();
    let before = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let missing_route = append(&http, &fixture, &turn2.id.0, "路由已删", "cfg-next").await;
    expect_status(missing_route, 500, "internal", false).await;
    // Replay-first: the committed key answers 200 even with the route gone.
    let replay = append(&http, &fixture, &fixture.chat.turn.id.0, "配置基准", "cfg").await;
    assert_eq!(replay.status(), reqwest::StatusCode::OK);
    let replay_body: Value = replay.json().await.unwrap();
    assert_eq!(replay_body["replayed"], true);
    assert_eq!(replay_body["turn"]["id"], turn2.id.0);
    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);

    fixture.h.engine.store.save_settings(&saved).unwrap();
    open_db(&fixture.h.db_path)
        .execute("DELETE FROM secrets WHERE id='route::chat-route'", [])
        .unwrap();
    let before = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let missing_secret = append(&http, &fixture, &turn2.id.0, "凭据已删", "cfg-secret").await;
    expect_status(missing_secret, 500, "internal", false).await;
    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);

    fixture
        .h
        .engine
        .store
        .put_secret("route::chat-route", "synthetic-chat-key")
        .unwrap();
    let mut moved = saved.clone();
    moved.routes[0].base_url = "http://127.0.0.1:9/v1".into();
    fixture.h.engine.store.save_settings(&moved).unwrap();
    let before = snapshot(&fixture.h.db_path);
    let calls_before = calls(&fixture.h);
    let moved_host = append(&http, &fixture, &turn2.id.0, "地址已改", "cfg-url").await;
    expect_status(moved_host, 500, "internal", false).await;
    assert_untouched(&before, &fixture.h.db_path);
    assert_eq!(calls(&fixture.h), calls_before);

    fixture.h.engine.store.save_settings(&saved).unwrap();
    let restored = append(&http, &fixture, &turn2.id.0, "地址恢复", "cfg-ok").await;
    assert_eq!(restored.status(), reqwest::StatusCode::CREATED);
}

#[tokio::test]
async fn n15_disconnect_mid_run() {
    let fixture = started().await;
    let http = client();
    let baseline = max_seq(&fixture.h.db_path);
    let mut stream = LiveSse::new(
        http.get(format!(
            "{}/api/sessions/{}/events?after={baseline}",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .send()
        .await
        .unwrap(),
    );
    let created = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "hold:disconnect",
        "dc",
    )
    .await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: Value = created.json().await.unwrap();
    let (turn2, task2) = receipt(&body, &fixture, false);
    let frames = read_until(&mut stream, Duration::from_secs(5), 16, |data| {
        data["kind"] == "delta" && data["data"]["text"] == "partial-disconnect"
    })
    .await;
    let consumed = frames.last().unwrap().1["seq"].as_i64().unwrap();
    drop(stream);
    assert!(fixture.h.engine.is_busy());
    assert_eq!(calls_for(&fixture.h, "hold:disconnect"), 1);
    fixture.h.probe.release.notify_waiters();
    let done = wait_task(&fixture.h.engine, &task2.legacy_task_id).await;
    assert_eq!(done.status, "completed");
    let replay = append(
        &http,
        &fixture,
        &fixture.chat.turn.id.0,
        "hold:disconnect",
        "dc",
    )
    .await;
    assert_eq!(replay.status(), reqwest::StatusCode::OK);
    let replay_body: Value = replay.json().await.unwrap();
    assert_eq!(replay_body["turn"]["id"], turn2.id.0);
    assert_eq!(replay_body["replayed"], true);
    let mut again = LiveSse::new(
        http.get(format!(
            "{}/api/sessions/{}/events?after={consumed}",
            fixture.h.origin, fixture.chat.session.id.0
        ))
        .send()
        .await
        .unwrap(),
    );
    let mut saw = 0;
    while let Some(frame) = again.try_next_data(Duration::from_millis(400)).await {
        let data: Value = serde_json::from_str(&frame.data).unwrap();
        assert!(data["seq"].as_i64().unwrap() > consumed);
        assert_eq!(data["turn_id"], turn2.id.0);
        saw += 1;
    }
    assert!(
        saw > 0,
        "reconnect should still deliver the completion frames"
    );
}

struct RestartIds {
    session: String,
    agent: String,
    turn1: String,
    turn2: String,
    legacy2: String,
    run_id: String,
}

#[tokio::test]
async fn n16_real_restart_partial_replay_resume() {
    let (base, probe) = mock_provider().await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("turns.db");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<RestartIds>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let worker_base = base.clone();
    let worker_db = db_path.clone();
    let worker_dir = dir.path().to_path_buf();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ids = runtime.block_on(async {
            let store = Arc::new(Store::open(&worker_db).unwrap());
            store
                .save_settings(&Settings {
                    workspace: worker_dir.to_string_lossy().into(),
                    max_concurrency: 2,
                    routes: vec![Route {
                        id: "chat-route".into(),
                        name: "chat".into(),
                        base_url: worker_base,
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
            let engine = Engine::new(store, 2).unwrap();
            let (origin, _http) = serve(engine.clone(), "restart-token").await;
            let http = client();
            let response = http
                .post(format!("{origin}/api/runs"))
                .header("x-peachsh-token", "restart-token")
                .header("idempotency-key", "run-create")
                .json(&json!({
                    "kind": "chat",
                    "title": "崩溃恢复",
                    "tasks": [{
                        "name": "chat-worker",
                        "role": "对话助手",
                        "route_id": "chat-route",
                        "prompt": "第一轮",
                        "depends_on": [],
                        "write_scopes": [],
                        "tools": false,
                        "allow_commands": false,
                        "max_rounds": 2
                    }]
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            let run: Run = response.json().await.unwrap();
            let done = wait_task(&engine, &run.tasks[0].id).await;
            assert_eq!(done.status, "completed");
            let db = open_db(&worker_db);
            let session: String = db
                .query_row(
                    "SELECT id FROM sessions WHERE legacy_run_id=?1",
                    [&run.id],
                    |row| row.get(0),
                )
                .unwrap();
            let agent: String = db
                .query_row(
                    "SELECT id FROM agents WHERE session_id=?1",
                    [&session],
                    |row| row.get(0),
                )
                .unwrap();
            let turn1: String = db
                .query_row(
                    "SELECT id FROM turns WHERE session_id=?1",
                    [&session],
                    |row| row.get(0),
                )
                .unwrap();
            drop(db);
            let turn_response = http
                .post(format!("{origin}/api/sessions/{session}/turns"))
                .header("x-peachsh-token", "restart-token")
                .header("idempotency-key", "partial-key")
                .json(&json!({
                    "agent_id": agent,
                    "expected_last_turn_id": turn1,
                    "message": "hold:restart"
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(turn_response.status(), reqwest::StatusCode::CREATED);
            let receipt: Value = turn_response.json().await.unwrap();
            RestartIds {
                session,
                agent,
                turn1,
                turn2: receipt["turn"]["id"].as_str().unwrap().to_owned(),
                legacy2: receipt["task"]["legacy_task_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                run_id: run.id,
            }
        });
        ready_tx.send(ids).unwrap();
        let _ = stop_rx.recv();
        drop(runtime);
    });
    let ids = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(ids) = ready_rx.try_recv() {
                break ids;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("restart worker did not publish ids");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let hit = count_where(
                &db_path,
                "SELECT count(*) FROM events WHERE turn_id=?1 AND kind='delta' AND data LIKE '%partial-restart%'",
                [&ids.turn2],
            );
            if hit > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("partial-restart delta was not persisted");
    let calls_after = probe.calls.load(Ordering::SeqCst);
    tokio::task::spawn_blocking(move || {
        stop_tx.send(()).unwrap();
        worker.join().unwrap();
    })
    .await
    .unwrap();

    let recovered = Store::open(&db_path).unwrap();
    assert!(recovered.recover().unwrap() >= 1);
    let interrupted = recovered.task(&ids.legacy2).unwrap();
    assert_eq!(interrupted.status, "interrupted");
    assert!(interrupted.output.contains("partial-restart"));
    let turn = recovered.turn(&TurnId(ids.turn2.clone())).unwrap();
    assert_eq!(turn.status, LifecycleStatus::Interrupted);
    let engine = Engine::new(Arc::new(recovered), 2).unwrap();
    let (origin, _http) = serve(engine.clone(), "restart-token").await;
    let http = client();
    // Post-recovery baseline: the replay and the interrupted-predecessor
    // rejection below must each leave every watched table cell identical.
    let replay_before = snapshot(&db_path);
    let replay_calls = probe.calls.load(Ordering::SeqCst);
    assert_eq!(
        replay_calls, calls_after,
        "restart itself called no provider"
    );
    let replay = http
        .post(format!("{origin}/api/sessions/{}/turns", ids.session))
        .header("x-peachsh-token", "restart-token")
        .header("idempotency-key", "partial-key")
        .json(&json!({
            "agent_id": ids.agent,
            "expected_last_turn_id": ids.turn1,
            "message": "hold:restart"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), reqwest::StatusCode::OK);
    let replay_body: Value = replay.json().await.unwrap();
    assert_eq!(replay_body["replayed"], true);
    assert_eq!(replay_body["turn"]["id"], ids.turn2);
    assert_eq!(replay_body["turn"]["status"], "interrupted");
    assert!(!engine.is_busy());
    assert_untouched(&replay_before, &db_path);
    assert_eq!(probe.calls.load(Ordering::SeqCst), replay_calls);

    let blocked_before = snapshot(&db_path);
    let blocked_calls = probe.calls.load(Ordering::SeqCst);
    let blocked = http
        .post(format!("{origin}/api/sessions/{}/turns", ids.session))
        .header("x-peachsh-token", "restart-token")
        .header("idempotency-key", "after-interrupt")
        .json(&json!({
            "agent_id": ids.agent,
            "expected_last_turn_id": ids.turn2,
            "message": "中断后不能直接追加"
        }))
        .send()
        .await
        .unwrap();
    expect_status(blocked, 409, "conflict", false).await;
    assert_untouched(&blocked_before, &db_path);
    assert_eq!(probe.calls.load(Ordering::SeqCst), blocked_calls);
    let resumed = http
        .post(format!("{origin}/api/tasks/{}/resume", ids.legacy2))
        .header("x-peachsh-token", "restart-token")
        .json(&json!({"message":"恢复"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resumed.status(), reqwest::StatusCode::OK);
    let resumed_task: Task = resumed.json().await.unwrap();
    let done = wait_task(&engine, &resumed_task.id).await;
    assert_eq!(done.status, "completed");
    let appended = http
        .post(format!("{origin}/api/sessions/{}/turns", ids.session))
        .header("x-peachsh-token", "restart-token")
        .header("idempotency-key", "after-resume")
        .json(&json!({
            "agent_id": ids.agent,
            "expected_last_turn_id": ids.turn2,
            "message": "恢复后追加"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(appended.status(), reqwest::StatusCode::CREATED);
    let _ = ids.run_id;
}

#[tokio::test]
async fn n17_regression_surface() {
    let fixture = started().await;
    let http = client();
    for url in [
        format!(
            "{}/api/sessions/{}",
            fixture.h.origin, fixture.chat.session.id.0
        ),
        format!(
            "{}/api/sessions/{}/turns",
            fixture.h.origin, fixture.chat.session.id.0
        ),
        format!("{}/api/turns/{}", fixture.h.origin, fixture.chat.turn.id.0),
        format!("{}/api/runs", fixture.h.origin),
    ] {
        let response = http.get(url).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }
    // The old run endpoint still treats Idempotency-Key as optional.
    let response = http
        .post(format!("{}/api/runs", fixture.h.origin))
        .header("x-peachsh-token", &fixture.h.token)
        .json(&json!({
            "kind": "chat",
            "title": "无 key",
            "tasks": [{
                "name": "chat-worker",
                "role": "对话助手",
                "route_id": "chat-route",
                "prompt": "回归",
                "depends_on": [],
                "write_scopes": [],
                "tools": false,
                "allow_commands": false,
                "max_rounds": 2
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    // ChatTurnError's exhaustive status mapping stays in the four server.rs unit
    // tests (`turn_command_error_maps_every_stable_variant` and its siblings).
    // N13 and N14 are the real HTTP-layer checks for storage and config faults.
}
