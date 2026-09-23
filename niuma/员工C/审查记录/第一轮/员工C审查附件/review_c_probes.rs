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

fn seed_chat(store: &Store, label: &str, prompts: &[&str]) -> Seed {
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
        title: format!("Chat {label}"),
        legacy_run_id: format!("run-{label}"),
        created_at: stamp,
        updated_at: stamp,
    };
    let agent = Agent {
        id: AgentId(format!("agent-{label}")),
        session_id: session.id.clone(),
        display_name: format!("same-display-{label}"),
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

fn corrupt_nth_event(path: &std::path::Path, session_id: &str, nth: i64) {
    let db = rusqlite::Connection::open(path).unwrap();
    let seq: i64 = db
        .query_row(
            "SELECT seq FROM events WHERE session_id=?1 ORDER BY seq LIMIT 1 OFFSET ?2",
            rusqlite::params![session_id, nth],
            |row| row.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE events SET data=?1 WHERE seq=?2",
        rusqlite::params!["{broken", seq],
    )
    .unwrap();
}

#[derive(Debug)]
struct Frame {
    id: Option<String>,
    event: Option<String>,
    data: String,
    comment: bool,
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
async fn h7_disconnect_reconnects_after_consumed_seq_without_cancelling() {
    let harness = harness().await;
    let seed = seed_chat(&harness.engine.store, "resume", &["first"]);
    let http = client();
    let response = http
        .get(format!(
            "{}/api/sessions/{}/events",
            harness.origin, seed.session.id.0
        ))
        .send()
        .await
        .unwrap();
    let first = read_frames(response, 1, Duration::from_secs(3)).await;
    let consumed: Value = serde_json::from_str(&first[0].data).unwrap();
    let consumed_seq = consumed["seq"].as_i64().unwrap();

    let later = Turn {
        id: TurnId("turn-resume-2".into()),
        session_id: seed.session.id.clone(),
        project_id: seed.project.id.clone(),
        status: LifecycleStatus::Completed,
        request_hash: "hash-resume-2".into(),
        idempotency_key: Some("key-resume-2".into()),
        created_at: 1_700_000_000,
        updated_at: 1_700_000_000,
    };
    let task = TurnTask {
        id: TaskId("task-resume-2".into()),
        turn_id: later.id.clone(),
        session_id: seed.session.id.clone(),
        agent_id: seed.tasks[0].agent_id.clone(),
        legacy_task_id: "legacy-resume-2".into(),
        depends_on: vec![],
        status: LifecycleStatus::Completed,
        created_at: 1_700_000_000,
        updated_at: 1_700_000_000,
    };
    let legacy = Task {
        id: task.legacy_task_id.clone(),
        run_id: seed.session.legacy_run_id.clone(),
        spec: spec("worker-resume-2", "second"),
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
            Some("key-resume-2"),
            &[legacy],
        )
        .unwrap();
    let active_before = harness.engine.is_busy();
    let response = http
        .get(format!(
            "{}/api/sessions/{}/events",
            harness.origin, seed.session.id.0
        ))
        .header("last-event-id", consumed_seq.to_string())
        .send()
        .await
        .unwrap();
    let next = read_frames(response, 1, Duration::from_secs(3)).await;
    let event: Value = serde_json::from_str(&next[0].data).unwrap();
    assert!(event["seq"].as_i64().unwrap() > consumed_seq);
    assert_eq!(event["turn_id"], later.id.0);
    assert_ne!(event["seq"], consumed_seq);
    assert_eq!(harness.engine.is_busy(), active_before);
    assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn h8_sessions_do_not_cross_and_seq_may_gap() {
    let harness = harness().await;
    let left = seed_chat(&harness.engine.store, "left", &["one", "three"]);
    let right = seed_chat(&harness.engine.store, "right", &["two"]);
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
    assert_ne!(left.session.title, right.session.title);
    let left_ids: Vec<_> = left_events
        .iter()
        .map(|event| event["turn_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        left_ids,
        vec![left.turns[0].id.0.as_str(), left.turns[1].id.0.as_str()]
    );
    let seqs: Vec<i64> = left_events
        .iter()
        .map(|event| event["seq"].as_i64().unwrap())
        .collect();
    assert!(seqs[1] > seqs[0]);
    assert!(right_event["seq"].as_i64().unwrap() > seqs[0]);
    assert_ne!(right_event["seq"].as_i64().unwrap(), seqs[0] + 1);
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
    let future = max_seq;
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
        .commit_turn(&later, &[task], Some("key-future-2"), &[legacy])
        .unwrap();
    let arrived = harness
        .engine
        .store
        .events(&seed.session.legacy_run_id, future)
        .unwrap();
    assert_eq!(
        arrived.len(),
        1,
        "later event must be visible above the future cursor"
    );
    assert!(arrived[0].seq > future);
    let frames = read_frames(response, 1, Duration::from_secs(4)).await;
    let event: Value = serde_json::from_str(&frames[0].data).unwrap();
    assert!(event["seq"].as_i64().unwrap() > future);
    assert_eq!(event["turn_id"], "turn-future-2");
}

#[tokio::test]
async fn h10_stream_failure_keeps_last_sent_cursor_and_writes_nothing() {
    let harness = harness().await;
    let seed = seed_chat(&harness.engine.store, "fault", &["first", "second"]);
    let before = counts(&harness.db_path);
    let first = harness
        .engine
        .store
        .events(&seed.session.legacy_run_id, 0)
        .unwrap();
    assert!(
        first.len() >= 2,
        "fixture must have a readable event before corruption"
    );
    let first_seq = first[0].seq;
    corrupt_nth_event(&harness.db_path, &seed.session.id.0, 1);
    let http = client();
    let response = http
        .get(format!(
            "{}/api/sessions/{}/events?after={first_seq}",
            harness.origin, seed.session.id.0
        ))
        .send()
        .await
        .unwrap();
    let frames = read_frames(response, 2, Duration::from_secs(3)).await;
    assert_eq!(frames.len(), 1, "stream must end after one error frame");
    assert_eq!(frames[0].event.as_deref(), Some("error"));
    assert!(
        frames[0].id.is_none(),
        "error frame must not allocate an event id"
    );
    let body: Value = serde_json::from_str(&frames[0].data).unwrap();
    assert_eq!(body["code"], "internal");
    assert_eq!(body["retryable"], false);
    assert_eq!(body["after"], first_seq);
    assert_eq!(counts(&harness.db_path), before);
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

// Review-only probes; appended to an unchanged copy of C's HTTP fixtures.

async fn review_next_frame(
    response: &mut reqwest::Response,
    pending: &mut Vec<u8>,
) -> std::result::Result<Option<String>, reqwest::Error> {
    loop {
        if let Some(end) = pending.windows(2).position(|pair| pair == b"\n\n") {
            let frame = String::from_utf8_lossy(&pending[..end]).into_owned();
            pending.drain(..end + 2);
            if frame.lines().any(|line| line.starts_with("data:")) {
                return Ok(Some(frame));
            }
            continue;
        }
        let Some(chunk) = response.chunk().await? else { return Ok(None); };
        pending.extend_from_slice(&chunk);
    }
}

fn review_frame_data(frame: &str) -> Value {
    serde_json::from_str(frame.lines().find_map(|line| line.strip_prefix("data: ")).unwrap()).unwrap()
}

#[tokio::test]
async fn review_corrupt_run_identity_is_internal() {
    let h = harness().await;
    let seed = seed_chat(&h.engine.store, "bad-identity", &["first"]);
    let db = rusqlite::Connection::open(&h.db_path).unwrap();
    db.execute("UPDATE tasks SET value=json_set(value,'$.id','different-id') WHERE id=?1", [&seed.tasks[0].legacy_task_id]).unwrap();
    let before = counts(&h.db_path);
    let response = client().get(format!("{}/api/runs/{}", h.origin, seed.session.legacy_run_id)).send().await.unwrap();
    let (status, body) = error_of(response).await;
    eprintln!("corrupt_identity: status={status}, code={}", body["code"]);
    assert_eq!(counts(&h.db_path), before);
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    assert_eq!((status, body["code"].as_str()), (500, Some("internal")));
}

#[tokio::test]
async fn review_encoded_duplicate_cursor_is_rejected() {
    let h = harness().await;
    let seed = seed_chat(&h.engine.store, "encoded-duplicate", &["first"]);
    let response = client().get(format!("{}/api/sessions/{}/events?after=0&%61fter=1", h.origin, seed.session.id.0)).send().await.unwrap();
    eprintln!("encoded_duplicate: status={}", response.status());
    assert_eq!(response.status().as_u16(), 400);
    let (_, body) = error_of(response).await;
    assert_eq!(body["code"], "request_failed");
}

#[tokio::test]
async fn review_encoded_invalid_cursor_must_not_replay() {
    let h = harness().await;
    let seed = seed_chat(&h.engine.store, "encoded-negative", &["first"]);
    let mut response = client().get(format!("{}/api/runs/{}/events?%61fter=-1", h.origin, seed.session.legacy_run_id)).send().await.unwrap();
    let status = response.status().as_u16();
    if status == 200 {
        let frame = tokio::time::timeout(Duration::from_secs(2), review_next_frame(&mut response, &mut Vec::new())).await.unwrap().unwrap().unwrap();
        eprintln!("encoded_negative: status=200, replayed_seq={}", review_frame_data(&frame)["seq"]);
    }
    assert_eq!(status, 400, "an encoded invalid cursor must not become zero");
}

#[tokio::test]
async fn review_invalid_path_is_structured_json() {
    let h = harness().await;
    let response = client().get(format!("{}/api/sessions/%FF", h.origin)).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 400);
    security_headers(&response);
    let content_type = response.headers()["content-type"].to_str().unwrap().to_owned();
    eprintln!("invalid_path: status=400, content_type={content_type}");
    assert!(content_type.starts_with("application/json"));
    let (_, body) = error_of(response).await;
    assert_eq!(body["code"], "request_failed");
}

#[tokio::test]
async fn review_health_schema_failure_is_safe() {
    let h = harness().await;
    let db = rusqlite::Connection::open(&h.db_path).unwrap();
    db.execute_batch("PRAGMA user_version=-1;").unwrap();
    assert!(h.engine.store.schema_version().is_err());
    let response = client().get(format!("{}/api/health", h.origin)).send().await.unwrap();
    let (status, body) = error_of(response).await;
    eprintln!("health_failure: status={status}, code={}", body["code"]);
    assert_eq!(status, 500);
    assert_eq!(body["code"], "internal");
    assert_eq!(body["retryable"], false);
    assert!(body.get("ok").is_none() && body.get("schema_version").is_none());
}

async fn review_after_first_frame(corrupt_kind: bool) {
    let h = harness().await;
    let seed = seed_chat(&h.engine.store, "after-frame", &["first"]);
    let mut response = client().get(format!("{}/api/sessions/{}/events", h.origin, seed.session.id.0)).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let mut pending = Vec::new();
    let first = tokio::time::timeout(Duration::from_secs(2), review_next_frame(&mut response, &mut pending)).await.unwrap().unwrap().unwrap();
    let first_seq = review_frame_data(&first)["seq"].as_i64().unwrap();
    assert!(first.lines().any(|line| line.starts_with("id: ")));
    // No await between insert and corruption: the current-thread HTTP task
    // cannot read the new row before this fault injection is complete.
    h.engine.store.event(&seed.tasks[0].legacy_task_id, "delta", json!({"value":1})).unwrap();
    let db = rusqlite::Connection::open(&h.db_path).unwrap();
    if corrupt_kind {
        db.execute("UPDATE events SET kind=?1 WHERE session_id=?2 AND seq>?3", rusqlite::params!["bad\nkind", seed.session.id.0, first_seq]).unwrap();
    } else {
        db.execute("UPDATE events SET data=?1 WHERE session_id=?2 AND seq>?3", rusqlite::params!["{broken", seed.session.id.0, first_seq]).unwrap();
    }
    let before = counts(&h.db_path);
    let next = tokio::time::timeout(Duration::from_secs(2), review_next_frame(&mut response, &mut pending)).await.unwrap();
    eprintln!("after_first_frame: corrupt_kind={corrupt_kind}, first_seq={first_seq}, next={next:?}");
    assert!(matches!(&next, Ok(Some(frame)) if frame.lines().any(|line| line == "event: error")), "fault must produce an error frame instead of breaking the HTTP body");
    let frame = next.unwrap().unwrap();
    let body = review_frame_data(&frame);
    assert_eq!(body["after"], first_seq);
    assert_eq!(body["code"], "internal");
    assert_eq!(body["retryable"], false);
    assert!(!frame.lines().any(|line| line.starts_with("id:")));
    assert!(tokio::time::timeout(Duration::from_secs(2), review_next_frame(&mut response, &mut pending)).await.unwrap().unwrap().is_none());
    assert_eq!(counts(&h.db_path), before);
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn review_read_failure_after_success_keeps_cursor() {
    review_after_first_frame(false).await;
}

#[tokio::test]
async fn review_bad_kind_after_success_returns_error_frame() {
    review_after_first_frame(true).await;
}

