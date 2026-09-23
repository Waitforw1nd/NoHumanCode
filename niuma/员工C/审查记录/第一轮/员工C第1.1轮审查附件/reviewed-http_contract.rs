//! HTTP and SSE transport contract.
//!
//! These tests use a temporary database and a localhost mock. They do not call
//! a running user instance or a paid provider.

use peachsh::{
    domain::*,
    engine::Engine,
    server::{self, App},
    store::{Store, TurnBundle},
};
use peachsh_protocol::{
    AgentId, LifecycleStatus, ProjectId, SessionId, SessionKind, TaskId, TurnId,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Harness {
    origin: String,
    token: String,
    engine: Arc<Engine>,
    db_path: std::path::PathBuf,
    calls: Arc<AtomicUsize>,
    _provider: tokio::task::JoinHandle<()>,
    _http: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let provider = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                (
                    [("content-type", "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                )
            }
        }),
    );
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_origin = format!("http://{}", provider_listener.local_addr().unwrap());
    let provider_task = tokio::spawn(async move {
        axum::serve(provider_listener, provider).await.unwrap();
    });

    let db_path = dir.path().join("http-contract.sqlite3");
    let store = Arc::new(Store::open(&db_path).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into(),
            max_concurrency: 1,
            routes: vec![Route {
                id: "route-1".into(),
                name: "Route 1".into(),
                base_url: format!("{provider_origin}/v1"),
                model: "model-a".into(),
                max_tokens: 128,
                parallel_limit: 1,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store
        .put_secret("route::route-1", "synthetic-test-key")
        .unwrap();
    let engine = Engine::new(store, 1).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let token = "http-contract-token".to_owned();
    let app = server::router(App {
        engine: engine.clone(),
        token: token.clone(),
        origin: origin.clone(),
    });
    let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Harness {
        origin,
        token,
        engine,
        db_path,
        calls,
        _provider: provider_task,
        _http: http,
        _dir: dir,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(4))
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

fn counts(path: &std::path::Path) -> (i64, i64, i64, i64) {
    let db = rusqlite::Connection::open(path).unwrap();
    let one = |sql: &str| db.query_row(sql, [], |row| row.get(0)).unwrap();
    (
        one("SELECT count(*) FROM runs"),
        one("SELECT count(*) FROM tasks"),
        one("SELECT count(*) FROM events"),
        one("SELECT count(*) FROM sessions"),
    )
}

fn route() -> Route {
    Route {
        id: "route-1".into(),
        name: "Route 1".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "model-a".into(),
        max_tokens: 128,
        parallel_limit: 1,
        key_env: None,
    }
}

fn spec(name: &str, prompt: &str) -> TaskSpec {
    TaskSpec {
        name: name.into(),
        role: "worker".into(),
        route_id: "route-1".into(),
        prompt: prompt.into(),
        depends_on: vec![],
        write_scopes: vec![],
        tools: false,
        allow_commands: false,
        max_rounds: 1,
    }
}

struct Seed {
    project: Project,
    session: Session,
    turns: Vec<Turn>,
    tasks: Vec<TurnTask>,
}

fn seed_named(store: &Store, label: &str, visible: &str, prompts: &[&str]) -> Seed {
    let stamp = 1_700_000_000;
    let project = Project {
        id: ProjectId(format!("project-{label}")),
        name: format!("Project {label}"),
        root_path: format!("C:/workspace/{label}"),
        created_at: stamp,
        updated_at: stamp,
    };
    let session = Session {
        id: SessionId(format!("session-{label}")),
        project_id: project.id.clone(),
        kind: SessionKind::Chat,
        title: visible.to_owned(),
        legacy_run_id: format!("run-{label}"),
        created_at: stamp,
        updated_at: stamp,
    };
    let agent = Agent {
        id: AgentId(format!("agent-{label}")),
        session_id: session.id.clone(),
        display_name: visible.to_owned(),
        role: "worker".into(),
        created_at: stamp,
    };
    let mut turns = Vec::new();
    let mut tasks = Vec::new();
    for (index, prompt) in prompts.iter().enumerate() {
        let n = index + 1;
        let turn = Turn {
            id: TurnId(format!("turn-{label}-{n}")),
            session_id: session.id.clone(),
            project_id: project.id.clone(),
            status: LifecycleStatus::Completed,
            request_hash: format!("hash-{label}-{n}"),
            idempotency_key: Some(format!("key-{label}-{n}")),
            created_at: stamp,
            updated_at: stamp,
        };
        let task = TurnTask {
            id: TaskId(format!("task-{label}-{n}")),
            turn_id: turn.id.clone(),
            session_id: session.id.clone(),
            agent_id: agent.id.clone(),
            legacy_task_id: format!("legacy-{label}-{n}"),
            depends_on: vec![],
            status: LifecycleStatus::Completed,
            created_at: stamp,
            updated_at: stamp,
        };
        let legacy = Task {
            id: task.legacy_task_id.clone(),
            run_id: session.legacy_run_id.clone(),
            spec: spec(&format!("worker-{label}-{n}"), prompt),
            route: route(),
            workspace: project.root_path.clone(),
            status: "completed".into(),
            output: String::new(),
            error: None,
            messages: vec![json!({"role":"user","content":prompt})],
            usage: json!({}),
            created_at: stamp,
            updated_at: stamp,
        };
        if index == 0 {
            store
                .commit_turn_bundle(
                    TurnBundle {
                        project: &project,
                        session: &session,
                        agents: std::slice::from_ref(&agent),
                        turn: &turn,
                        tasks: std::slice::from_ref(&task),
                        legacy_tasks: std::slice::from_ref(&legacy),
                    },
                    Some(&format!("key-{label}-1")),
                )
                .unwrap();
        } else {
            store
                .commit_turn(
                    &turn,
                    std::slice::from_ref(&task),
                    Some(&format!("key-{label}-{n}")),
                    std::slice::from_ref(&legacy),
                )
                .unwrap();
        }
        turns.push(turn);
        tasks.push(task);
    }
    Seed {
        project,
        session,
        turns,
        tasks,
    }
}

fn seed_chat(store: &Store, label: &str, prompts: &[&str]) -> Seed {
    seed_named(store, label, &format!("Chat {label}"), prompts)
}

fn append_named(store: &Store, seed: &Seed, visible: &str, prompt: &str) -> Turn {
    let n = seed.turns.len() + 1;
    let turn = Turn {
        id: TurnId(format!("turn-{}-extra-{n}", seed.session.id.0)),
        session_id: seed.session.id.clone(),
        project_id: seed.project.id.clone(),
        status: LifecycleStatus::Completed,
        request_hash: format!("hash-extra-{n}-{}", seed.session.id.0),
        idempotency_key: Some(format!("key-extra-{n}-{}", seed.session.id.0)),
        created_at: 1,
        updated_at: 1,
    };
    let task = TurnTask {
        id: TaskId(format!("task-extra-{n}-{}", seed.session.id.0)),
        turn_id: turn.id.clone(),
        session_id: seed.session.id.clone(),
        agent_id: seed.tasks[0].agent_id.clone(),
        legacy_task_id: format!("legacy-extra-{n}-{}", seed.session.id.0),
        depends_on: vec![],
        status: LifecycleStatus::Completed,
        created_at: 1,
        updated_at: 1,
    };
    let legacy = Task {
        id: task.legacy_task_id.clone(),
        run_id: seed.session.legacy_run_id.clone(),
        spec: spec(visible, prompt),
        route: route(),
        workspace: seed.project.root_path.clone(),
        status: "completed".into(),
        output: String::new(),
        error: None,
        messages: vec![],
        usage: json!({}),
        created_at: 1,
        updated_at: 1,
    };
    store
        .commit_turn(
            &turn,
            std::slice::from_ref(&task),
            turn.idempotency_key.as_deref(),
            std::slice::from_ref(&legacy),
        )
        .unwrap();
    turn
}

#[derive(Debug)]
struct Frame {
    id: Option<String>,
    event: Option<String>,
    data: String,
    comment: bool,
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
        comment: false,
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

async fn read_frames(response: reqwest::Response, limit: usize, timeout: Duration) -> Vec<Frame> {
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    security_headers(&response);
    let mut response = response;
    let frames = tokio::time::timeout(timeout, async {
        let mut bytes = Vec::new();
        let mut frames = Vec::new();
        while frames.len() < limit {
            let Some(chunk) = response.chunk().await.unwrap() else {
                break;
            };
            bytes.extend_from_slice(&chunk);
            while let Some(end) = bytes.windows(2).position(|pair| pair == b"\n\n") {
                let raw = std::str::from_utf8(&bytes[..end]).unwrap().to_owned();
                bytes.drain(..end + 2);
                if raw.is_empty() {
                    continue;
                }
                let mut frame = Frame {
                    id: None,
                    event: None,
                    data: String::new(),
                    comment: raw
                        .lines()
                        .all(|line| line.starts_with(':') || line.is_empty()),
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
                if !frame.comment {
                    frames.push(frame);
                }
            }
        }
        frames
    })
    .await
    .expect("SSE frame timed out");
    drop(response);
    frames
}

#[tokio::test]
async fn h1_read_shapes_and_security_headers_stay() {
    let harness = harness().await;
    let seed = seed_chat(&harness.engine.store, "alpha", &["first"]);
    let http = client();

    let health = http
        .get(format!("{}/api/health", harness.origin))
        .send()
        .await
        .unwrap();
    security_headers(&health);
    let health: Value = health.json().await.unwrap();
    assert_eq!(health["ok"], true);
    assert_eq!(health["runtime"], "rust");
    assert_eq!(health["schema_version"], 6);
    assert_ne!(health["schema_version"], 0);

    let projects: Vec<Project> = http
        .get(format!("{}/api/projects", harness.origin))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(projects[0].id, seed.project.id);

    let session: Session = http
        .get(format!(
            "{}/api/sessions/{}",
            harness.origin, seed.session.id.0
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session.legacy_run_id, seed.session.legacy_run_id);
    assert_ne!(session.id.0, session.legacy_run_id);

    let turns: Vec<Turn> = http
        .get(format!(
            "{}/api/sessions/{}/turns",
            harness.origin, seed.session.id.0
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(turns.len(), 1);
    let turn: Turn = http
        .get(format!("{}/api/turns/{}", harness.origin, turns[0].id.0))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(turn.session_id, seed.session.id);

    let runs: Vec<Value> = http
        .get(format!("{}/api/runs", harness.origin))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs[0]["id"], seed.session.legacy_run_id);
    let run: Run = http
        .get(format!(
            "{}/api/runs/{}",
            harness.origin, seed.session.legacy_run_id
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(run.id, seed.session.legacy_run_id);
    assert_eq!(run.tasks[0].id, seed.tasks[0].legacy_task_id);
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn h2_missing_is_404_and_corruption_is_500() {
    let harness = harness().await;
    let seed = seed_chat(&harness.engine.store, "beta", &["first"]);
    let http = client();
    let before = counts(&harness.db_path);

    let missing = http
        .get(format!("{}/api/sessions/missing-session", harness.origin))
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(missing).await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "not_found");
    assert_eq!(body["retryable"], false);

    let missing_turn = http
        .get(format!("{}/api/turns/missing-turn", harness.origin))
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(missing_turn).await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "not_found");

    let missing_run = http
        .get(format!("{}/api/runs/missing-run", harness.origin))
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(missing_run).await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "not_found");

    {
        let db = rusqlite::Connection::open(&harness.db_path).unwrap();
        db.execute(
            "UPDATE sessions SET kind='not-a-kind' WHERE id=?1",
            [seed.session.id.0.as_str()],
        )
        .unwrap();
    }
    let corrupt = http
        .get(format!(
            "{}/api/sessions/{}",
            harness.origin, seed.session.id.0
        ))
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(corrupt).await;
    assert_eq!(status, 500);
    assert_eq!(body["code"], "internal");
    assert_eq!(body["retryable"], false);
    assert_ne!(body["code"], "not_found");
    assert!(!body["message"].as_str().unwrap().contains("not-a-kind"));

    {
        let db = rusqlite::Connection::open(&harness.db_path).unwrap();
        db.execute(
            "UPDATE tasks SET value=json_set(value,'$.id','different-id') WHERE id=?1",
            [seed.tasks[0].legacy_task_id.as_str()],
        )
        .unwrap();
    }
    let identity = http
        .get(format!(
            "{}/api/runs/{}",
            harness.origin, seed.session.legacy_run_id
        ))
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(identity).await;
    assert_eq!(status, 500);
    assert_eq!(body["code"], "internal");
    assert_eq!(body["retryable"], false);
    assert!(!body["message"].as_str().unwrap().contains("different-id"));

    {
        let db = rusqlite::Connection::open(&harness.db_path).unwrap();
        db.execute_batch("PRAGMA user_version=-1;").unwrap();
    }
    assert!(harness.engine.store.schema_version().is_err());
    let health = http
        .get(format!("{}/api/health", harness.origin))
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(health).await;
    assert_eq!(status, 500);
    assert_eq!(body["code"], "internal");
    assert_eq!(body["retryable"], false);
    assert!(body.get("ok").is_none());
    assert!(body.get("schema_version").is_none());

    assert_eq!(counts(&harness.db_path).0, before.0);
}

#[tokio::test]
async fn h3_guard_rejections_are_structured_and_do_not_execute() {
    let harness = harness().await;
    let http = client();
    let before = counts(&harness.db_path);
    let calls = harness.calls.load(Ordering::SeqCst);
    let payload = json!({"title":"blocked","tasks":[{"name":"worker","role":"worker","route_id":"route-1","prompt":"hello"}]});

    let mut wrong_host = http
        .post(format!("{}/api/runs", harness.origin))
        .header("host", "evil.example")
        .header("x-peachsh-token", &harness.token)
        .json(&payload);
    let _ = &mut wrong_host;
    let response = wrong_host.send().await.unwrap();
    let (status, body) = error_of(response).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["code"], "forbidden");

    let response = http
        .post(format!("{}/api/runs", harness.origin))
        .header("origin", "http://evil.example")
        .header("x-peachsh-token", &harness.token)
        .json(&payload)
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(response).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["code"], "forbidden");

    let response = http
        .post(format!("{}/api/runs", harness.origin))
        .header("sec-fetch-site", "cross-site")
        .header("x-peachsh-token", &harness.token)
        .json(&payload)
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(response).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["code"], "forbidden");

    let response = http
        .post(format!("{}/api/runs", harness.origin))
        .json(&payload)
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(response).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["code"], "forbidden");

    let response = http
        .post(format!("{}/api/runs", harness.origin))
        .header("x-peachsh-token", "wrong-token")
        .json(&payload)
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(response).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["code"], "forbidden");
    assert_eq!(body["retryable"], false);
    assert_eq!(counts(&harness.db_path), before);
    assert_eq!(harness.calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn h4_json_input_failures_do_not_enter_engine() {
    let harness = harness().await;
    let http = client();
    let before = counts(&harness.db_path);
    let calls = harness.calls.load(Ordering::SeqCst);
    let url = format!("{}/api/runs", harness.origin);

    let syntax = http
        .post(&url)
        .header("x-peachsh-token", &harness.token)
        .header("content-type", "application/json")
        .body("{")
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(syntax).await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "request_failed");
    assert!(!body["message"].as_str().unwrap().contains("{"));

    let typed = http
        .post(&url)
        .header("x-peachsh-token", &harness.token)
        .json(&json!({"title":"bad","tasks":"not-an-array"}))
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(typed).await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "request_failed");

    let missing_type = http
        .post(&url)
        .header("x-peachsh-token", &harness.token)
        .body(r#"{"title":"x","tasks":[]}"#)
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(missing_type).await;
    assert_eq!(status, 415);
    assert_eq!(body["code"], "request_failed");

    let text_type = http
        .post(&url)
        .header("x-peachsh-token", &harness.token)
        .header("content-type", "text/plain")
        .body(r#"{"title":"x","tasks":[]}"#)
        .send()
        .await
        .unwrap();
    let (status, _body) = error_of(text_type).await;
    assert_eq!(status, 415);

    let huge = "x".repeat(1_048_577);
    let too_large = http
        .post(&url)
        .header("x-peachsh-token", &harness.token)
        .header("content-type", "application/json")
        .body(huge)
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(too_large).await;
    assert_eq!(status, 413);
    assert_eq!(body["code"], "request_failed");
    assert_eq!(body["retryable"], false);
    assert_eq!(counts(&harness.db_path), before);
    assert_eq!(harness.calls.load(Ordering::SeqCst), calls);

    for path in [
        "/api/sessions/%FF",
        "/api/turns/%FF",
        "/api/runs/%FF",
        "/api/sessions/%FF/events",
        "/api/runs/%FF/events",
    ] {
        let response = http
            .get(format!("{}{path}", harness.origin))
            .send()
            .await
            .unwrap();
        let (status, body) = error_of(response).await;
        assert_eq!(status, 400, "{path} -> {body}");
        assert_eq!(body["code"], "request_failed");
        assert!(!body["message"].as_str().unwrap().contains("%FF"));
    }
}

#[tokio::test]
async fn h5_cursor_inputs_reject_ambiguity() {
    let harness = harness().await;
    let seed = seed_chat(&harness.engine.store, "cursor", &["first"]);
    let http = client();
    let url = format!(
        "{}/api/sessions/{}/events",
        harness.origin, seed.session.id.0
    );

    let ok = http.get(format!("{url}?after=0")).send().await.unwrap();
    assert_eq!(ok.status(), reqwest::StatusCode::OK);
    drop(ok);
    let header_only = http
        .get(&url)
        .header("last-event-id", "0")
        .send()
        .await
        .unwrap();
    assert_eq!(header_only.status(), reqwest::StatusCode::OK);
    drop(header_only);

    let rejected = [
        format!("{url}?after=-1"),
        format!("{url}?after=+1"),
        format!("{url}?after=%20"),
        format!("{url}?after=1%201"),
        format!("{url}?after=abc"),
        format!("{url}?after="),
        format!("{url}?after=9223372036854775808"),
        format!("{url}?after=1,2"),
        format!("{url}?after=1&after=1"),
        format!("{url}?after=1&after=2"),
    ];
    for target in rejected {
        let response = http.get(target).send().await.unwrap();
        let (status, body) = error_of(response).await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["code"], "request_failed");
        assert!(
            response_content_type_json(&body),
            "error body must be structured"
        );
    }
    let duplicate_header = http
        .get(&url)
        .header("last-event-id", "1")
        .header("last-event-id", "1")
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(duplicate_header).await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "request_failed");

    let bad_header_with_good_query = http
        .get(format!("{url}?after=0"))
        .header("last-event-id", "-1")
        .send()
        .await
        .unwrap();
    let (status, _) = error_of(bad_header_with_good_query).await;
    assert_eq!(status, 400);
    let bad_query_with_good_header = http
        .get(format!("{url}?after=-1"))
        .header("last-event-id", "0")
        .send()
        .await
        .unwrap();
    let (status, _) = error_of(bad_query_with_good_header).await;
    assert_eq!(status, 400);

    let encoded = [
        format!("{url}?after=0&%61fter=1"),
        format!("{url}?%61fter=1&%61fter=1"),
        format!("{url}?%61fter=-1"),
        format!("{url}?%2575fter=1"),
    ];
    for target in encoded {
        let response = http.get(&target).send().await.unwrap();
        let (status, body) = error_of(response).await;
        assert_eq!(status, 400, "{target} -> {body}");
        assert_eq!(body["code"], "request_failed");
        assert_eq!(body["code"], "request_failed");
    }
    let encoded_value = http.get(format!("{url}?%61fter=0")).send().await.unwrap();
    assert_eq!(encoded_value.status(), reqwest::StatusCode::OK);
    drop(encoded_value);
    let run_url = format!(
        "{}/api/runs/{}/events",
        harness.origin, seed.session.legacy_run_id
    );
    let encoded_invalid = http
        .get(format!("{run_url}?%61fter=-1"))
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(encoded_invalid).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["code"], "request_failed");
    let encoded_with_header = http
        .get(format!("{run_url}?%61fter=-1"))
        .header("last-event-id", "0")
        .send()
        .await
        .unwrap();
    let (status, _) = error_of(encoded_with_header).await;
    assert_eq!(status, 400);

    let known = harness
        .engine
        .store
        .events(&seed.session.legacy_run_id, 0)
        .unwrap();
    let ahead = known.last().unwrap().seq + 20;
    let header_wins = http
        .get(format!("{url}?after=0"))
        .header("last-event-id", ahead.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(header_wins.status(), reqwest::StatusCode::OK);
    let mut header_wins = header_wins;
    let early = tokio::time::timeout(Duration::from_millis(350), header_wins.chunk()).await;
    if let Ok(chunk) = early {
        let bytes = chunk.unwrap().unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("data: "),
            "header cursor lost to query: {text}"
        );
    }
}

fn response_content_type_json(body: &Value) -> bool {
    body["code"].is_string() && body["message"].is_string() && body["error"] == body["message"]
}

#[tokio::test]
async fn h6_sse_frame_matches_persisted_event() {
    let harness = harness().await;
    let seed = seed_chat(&harness.engine.store, "frame", &["first"]);
    let http = client();
    let response = http
        .get(format!(
            "{}/api/sessions/{}/events",
            harness.origin, seed.session.id.0
        ))
        .send()
        .await
        .unwrap();
    let frames = read_frames(response, 1, Duration::from_secs(3)).await;
    let event: Value = serde_json::from_str(&frames[0].data).unwrap();
    assert_eq!(
        frames[0].id.as_deref(),
        Some(event["seq"].as_i64().unwrap().to_string().as_str())
    );
    assert_eq!(frames[0].event.as_deref(), event["kind"].as_str());
    assert_eq!(event["cursor"], event["seq"].as_i64().unwrap().to_string());
    assert_eq!(event["session_id"], seed.session.id.0);
    assert_eq!(event["turn_id"], seed.turns[0].id.0);
    assert_eq!(event["task_id"], seed.tasks[0].legacy_task_id);
    assert!(event.get("at").is_some());
}

#[tokio::test]
async fn h7_disconnect_does_not_cancel_running_task() {
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = release.clone();
    let started = entered.clone();
    let counted = calls.clone();
    let provider = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move || {
            let gate = gate.clone();
            let started = started.clone();
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                started.notify_waiters();
                gate.notified().await;
                (
                    [("content-type", "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"content\":\"held\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                )
            }
        }),
    );
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_origin = format!("http://{}", provider_listener.local_addr().unwrap());
    let provider_task = tokio::spawn(async move {
        axum::serve(provider_listener, provider).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("held.sqlite3");
    let store = Arc::new(Store::open(&db_path).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into(),
            max_concurrency: 1,
            routes: vec![Route {
                id: "route-1".into(),
                name: "Route 1".into(),
                base_url: format!("{provider_origin}/v1"),
                model: "model-a".into(),
                max_tokens: 128,
                parallel_limit: 1,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store
        .put_secret("route::route-1", "synthetic-test-key")
        .unwrap();
    let engine = Engine::new(store, 1).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = server::router(App {
        engine: engine.clone(),
        token: "held-token".into(),
        origin: origin.clone(),
    });
    let http_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = client();
    let started_run = http
        .post(format!("{origin}/api/runs"))
        .header("x-peachsh-token", "held-token")
        .json(&json!({
            "kind": "chat",
            "title": "held",
            "tasks": [{
                "name": "worker",
                "role": "worker",
                "route_id": "route-1",
                "prompt": "hold",
                "depends_on": [],
                "write_scopes": [],
                "tools": false,
                "allow_commands": false,
                "max_rounds": 1
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(started_run.status(), reqwest::StatusCode::OK);
    let run: Run = started_run.json().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    assert!(engine.is_busy());
    let session_id = engine.store.events(&run.id, 0).unwrap()[0]
        .session_id
        .clone()
        .unwrap();
    let mut stream = LiveSse::new(
        http.get(format!("{origin}/api/sessions/{session_id}/events"))
            .send()
            .await
            .unwrap(),
    );
    let first = stream.next_data(Duration::from_secs(3)).await.unwrap();
    let consumed: Value = serde_json::from_str(&first.data).unwrap();
    drop(stream);
    assert!(
        engine.is_busy(),
        "closing the SSE client must not cancel the task"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.notify_waiters();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = engine.store.run(&run.id).unwrap();
            if current.tasks[0].status == "completed" && !engine.is_busy() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut resumed = LiveSse::new(
        http.get(format!("{origin}/api/sessions/{session_id}/events"))
            .header(
                "last-event-id",
                consumed["seq"].as_i64().unwrap().to_string(),
            )
            .send()
            .await
            .unwrap(),
    );
    let next = resumed.next_data(Duration::from_secs(3)).await.unwrap();
    let event: Value = serde_json::from_str(&next.data).unwrap();
    assert!(event["seq"].as_i64().unwrap() > consumed["seq"].as_i64().unwrap());
    http_server.abort();
    provider_task.abort();
}

#[tokio::test]
async fn h8_sessions_do_not_cross_and_seq_may_gap() {
    let harness = harness().await;
    let left = seed_named(&harness.engine.store, "left", "same-visible", &["one"]);
    let right = seed_named(&harness.engine.store, "right", "same-visible", &["two"]);
    let left_again = append_named(&harness.engine.store, &left, "same-visible", "three");
    let http = client();
    let left_frames = read_frames(
        http.get(format!(
            "{}/api/sessions/{}/events",
            harness.origin, left.session.id.0
        ))
        .send()
        .await
        .unwrap(),
        2,
        Duration::from_secs(3),
    )
    .await;
    let right_frames = read_frames(
        http.get(format!(
            "{}/api/sessions/{}/events",
            harness.origin, right.session.id.0
        ))
        .send()
        .await
        .unwrap(),
        1,
        Duration::from_secs(3),
    )
    .await;
    let left_events: Vec<Value> = left_frames
        .iter()
        .map(|frame| serde_json::from_str(&frame.data).unwrap())
        .collect();
    let right_event: Value = serde_json::from_str(&right_frames[0].data).unwrap();
    assert!(
        left_events
            .iter()
            .all(|event| event["session_id"] == left.session.id.0)
    );
    assert_eq!(right_event["session_id"], right.session.id.0);
    assert_eq!(left.session.title, right.session.title);
    assert_ne!(left.session.id, right.session.id);
    let left_ids: Vec<_> = left_events
        .iter()
        .map(|event| event["turn_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        left_ids,
        vec![left.turns[0].id.0.as_str(), left_again.id.0.as_str()]
    );
    let seqs: Vec<i64> = left_events
        .iter()
        .map(|event| event["seq"].as_i64().unwrap())
        .collect();
    assert!(seqs[1] > seqs[0]);
    assert!(right_event["seq"].as_i64().unwrap() > seqs[0]);
    assert!(right_event["seq"].as_i64().unwrap() < seqs[1]);
}

#[tokio::test]
async fn h9_idle_and_future_cursor_do_not_replay() {
    let harness = harness().await;
    let seed = seed_chat(&harness.engine.store, "future", &["first"]);
    let known = harness
        .engine
        .store
        .events(&seed.session.legacy_run_id, 0)
        .unwrap();
    let max_seq = known.last().unwrap().seq;
    let future = max_seq + 1;
    let http = client();
    let response = http
        .get(format!(
            "{}/api/sessions/{}/events?after={future}",
            harness.origin, seed.session.id.0
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let mut response = response;
    let early = tokio::time::timeout(Duration::from_millis(350), response.chunk()).await;
    if let Ok(chunk) = early {
        let bytes = chunk.unwrap().unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("data: "),
            "future cursor replayed history: {text}"
        );
    }
    let later = Turn {
        id: TurnId("turn-future-2".into()),
        session_id: seed.session.id.clone(),
        project_id: seed.project.id.clone(),
        status: LifecycleStatus::Completed,
        request_hash: "hash-future-2".into(),
        idempotency_key: Some("key-future-2".into()),
        created_at: 1,
        updated_at: 1,
    };
    let task = TurnTask {
        id: TaskId("task-future-2".into()),
        turn_id: later.id.clone(),
        session_id: seed.session.id.clone(),
        agent_id: seed.tasks[0].agent_id.clone(),
        legacy_task_id: "legacy-future-2".into(),
        depends_on: vec![],
        status: LifecycleStatus::Completed,
        created_at: 1,
        updated_at: 1,
    };
    let legacy = Task {
        id: task.legacy_task_id.clone(),
        run_id: seed.session.legacy_run_id.clone(),
        spec: spec("worker-future-2", "later"),
        route: route(),
        workspace: seed.project.root_path.clone(),
        status: "completed".into(),
        output: String::new(),
        error: None,
        messages: vec![],
        usage: json!({}),
        created_at: 1,
        updated_at: 1,
    };
    harness
        .engine
        .store
        .commit_turn(
            &later,
            std::slice::from_ref(&task),
            Some("key-future-2"),
            &[legacy],
        )
        .unwrap();
    let below = harness
        .engine
        .store
        .events(&seed.session.legacy_run_id, future)
        .unwrap();
    assert!(
        below.is_empty(),
        "an event at or below the future cursor must not be returned"
    );
    let crossed = append_named(&harness.engine.store, &seed, "future-cross", "cross");
    let arrived = harness
        .engine
        .store
        .events(&seed.session.legacy_run_id, future)
        .unwrap();
    assert_eq!(arrived.len(), 1);
    assert!(arrived[0].seq > future);
    assert_eq!(arrived[0].turn_id.as_deref(), Some(crossed.id.0.as_str()));
    let frames = read_frames(response, 1, Duration::from_secs(4)).await;
    let event: Value = serde_json::from_str(&frames[0].data).unwrap();
    assert!(event["seq"].as_i64().unwrap() > future);
    assert_eq!(event["turn_id"], crossed.id.0);
}

async fn fault_after_first_frame(corrupt_kind: bool) {
    let harness = harness().await;
    let seed = seed_chat(&harness.engine.store, "fault", &["first"]);
    let http = client();
    let mut stream = LiveSse::new(
        http.get(format!(
            "{}/api/sessions/{}/events",
            harness.origin, seed.session.id.0
        ))
        .send()
        .await
        .unwrap(),
    );
    let first = stream.next_data(Duration::from_secs(3)).await.unwrap();
    let first_event: Value = serde_json::from_str(&first.data).unwrap();
    let first_seq = first_event["seq"].as_i64().unwrap();
    assert!(first.id.is_some());
    harness
        .engine
        .store
        .event(&seed.tasks[0].legacy_task_id, "delta", json!({"value": 1}))
        .unwrap();
    let db = rusqlite::Connection::open(&harness.db_path).unwrap();
    if corrupt_kind {
        db.execute(
            "UPDATE events SET kind=?1 WHERE session_id=?2 AND seq>?3",
            rusqlite::params!["bad\nkind", seed.session.id.0, first_seq],
        )
        .unwrap();
    } else {
        db.execute(
            "UPDATE events SET data=?1 WHERE session_id=?2 AND seq>?3",
            rusqlite::params!["{broken", seed.session.id.0, first_seq],
        )
        .unwrap();
    }
    let before = counts(&harness.db_path);
    let error = stream.next_data(Duration::from_secs(3)).await.unwrap();
    assert_eq!(error.event.as_deref(), Some("error"));
    assert!(error.id.is_none());
    let body: Value = serde_json::from_str(&error.data).unwrap();
    assert_eq!(body["code"], "internal");
    assert_eq!(body["retryable"], false);
    assert_eq!(body["after"], first_seq);
    assert!(stream.next_data(Duration::from_secs(2)).await.is_none());
    assert_eq!(counts(&harness.db_path), before);
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn h10_stream_failure_keeps_last_sent_cursor_and_writes_nothing() {
    fault_after_first_frame(false).await;
    fault_after_first_frame(true).await;
}

#[tokio::test]
async fn h11_existing_run_idempotency_and_bad_key_stay() {
    let harness = harness().await;
    let http = client();
    let calls = harness.calls.load(Ordering::SeqCst);
    let payload = json!({
        "kind": "chat",
        "title": "idempotent",
        "tasks": [{
            "name": "worker",
            "role": "worker",
            "route_id": "route-1",
            "prompt": "hello",
            "depends_on": [],
            "write_scopes": [],
            "tools": false,
            "allow_commands": false,
            "max_rounds": 1
        }]
    });
    let first = http
        .post(format!("{}/api/runs", harness.origin))
        .header("x-peachsh-token", &harness.token)
        .header("idempotency-key", "contract-key")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let run: Run = first.json().await.unwrap();
    let replay = http
        .post(format!("{}/api/runs", harness.origin))
        .header("x-peachsh-token", &harness.token)
        .header("idempotency-key", "contract-key")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), reqwest::StatusCode::OK);
    let replayed: Run = replay.json().await.unwrap();
    assert_eq!(replayed.id, run.id);

    let mut changed = payload.clone();
    changed["title"] = json!("different");
    let conflict = http
        .post(format!("{}/api/runs", harness.origin))
        .header("x-peachsh-token", &harness.token)
        .header("idempotency-key", "contract-key")
        .json(&changed)
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(conflict).await;
    assert_eq!(status, 409);
    assert_eq!(body["code"], "conflict");
    assert_eq!(body["retryable"], false);

    let before = counts(&harness.db_path);
    let bad_key = http
        .post(format!("{}/api/runs", harness.origin))
        .header("x-peachsh-token", &harness.token)
        .header("idempotency-key", "bad key")
        .json(&payload)
        .send()
        .await
        .unwrap();
    let (status, body) = error_of(bad_key).await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "request_failed");
    assert_eq!(counts(&harness.db_path), before);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let run = harness.engine.store.run(&run.id).unwrap();
            if run
                .tasks
                .iter()
                .all(|task| task.status != "queued" && task.status != "running")
                && !harness.engine.is_busy()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(harness.calls.load(Ordering::SeqCst), calls + 1);
}
