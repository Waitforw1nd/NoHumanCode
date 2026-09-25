use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    routing::{get, post},
};
use peachsh::{
    domain::*,
    engine::Engine,
    server::{self, App},
    store::Store,
    wasm, workspace,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

#[derive(Default)]
struct Counters {
    active: usize,
    max: usize,
    by_key: HashMap<String, usize>,
    max_key: HashMap<String, usize>,
    calls: Vec<Value>,
}
#[derive(Clone, Default)]
struct Mock {
    inner: Arc<Mutex<Counters>>,
}
async fn chat(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    assert_eq!(body["stream"], true);
    let key = headers["authorization"].to_str().unwrap().to_string();
    {
        let mut s = mock.inner.lock().unwrap();
        s.active += 1;
        s.max = s.max.max(s.active);
        let n = s.by_key.entry(key.clone()).or_default();
        *n += 1;
        let n = *n;
        let max = s.max_key.entry(key.clone()).or_default();
        *max = (*max).max(n);
        s.calls.push(json!({"key":key,"body":body}));
    }
    tokio::time::sleep(Duration::from_millis(60)).await;
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    let chunk = if last["content"] == "write-test" {
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"write-1","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"src/result.txt\",\"content\":\"rust-native-ok\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})
    } else if last["content"] == "escape-test" {
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"write-escape","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"../outside.txt\",\"content\":\"bad\"}"}}]},"finish_reason":"tool_calls"}]})
    } else {
        json!({"choices":[{"delta":{"content":"323"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1,"total_tokens":11}})
    };
    {
        let mut s = mock.inner.lock().unwrap();
        s.active -= 1;
        *s.by_key.get_mut(&key).unwrap() -= 1;
    }
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}
async fn mock_server() -> (String, Mock, tokio::task::JoinHandle<()>) {
    let mock = Mock::default();
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .route(
            "/v1/models",
            get(|| async { Json(json!({"data":[{"id":"model-a"},{"id":"model-b"}]})) }),
        )
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/v1"), mock, handle)
}
fn setup(dir: &std::path::Path, base: &str) -> Arc<Engine> {
    let store = Arc::new(Store::open(&dir.join("test.db")).unwrap());
    let routes = (1..=3)
        .map(|n| Route {
            id: format!("key-{n}"),
            name: format!("Key {n}"),
            base_url: base.into(),
            model: if n == 2 { "model-b" } else { "model-a" }.into(),
            max_tokens: 128,
            parallel_limit: 1,
            key_env: None,
        })
        .collect();
    store
        .save_settings(&Settings {
            workspace: dir.to_string_lossy().into(),
            max_concurrency: 2,
            routes,
            newapi: None,
        })
        .unwrap();
    for n in 1..=3 {
        store
            .put_secret(
                &format!("key-{n}"),
                if n == 2 { "fake-key-b" } else { "fake-key-a" },
            )
            .unwrap();
    }
    Engine::new(store, 2).unwrap()
}
fn task(name: &str, route: &str, prompt: &str, scopes: &[&str]) -> TaskSpec {
    TaskSpec {
        name: name.into(),
        role: format!("role-{name}"),
        route_id: route.into(),
        prompt: prompt.into(),
        depends_on: vec![],
        write_scopes: scopes.iter().map(|s| s.to_string()).collect(),
        tools: !scopes.is_empty(),
        allow_commands: false,
        max_rounds: 4,
    }
}
async fn settled(engine: &Engine, id: &str) -> Run {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let r = engine.store.run(id).unwrap();
            if r.tasks
                .iter()
                .all(|t| !matches!(t.status.as_str(), "queued" | "running"))
                && !engine.is_busy()
            {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn approve_pending_write(engine: &Engine, task_id: &str) {
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pending = engine.store.pending_approvals_for_task(task_id).unwrap();
            if !pending.is_empty() {
                assert_eq!(
                    pending.len(),
                    1,
                    "expected one approval for the known tool call"
                );
                break pending.into_iter().next().unwrap();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("known write approval did not become pending");
    assert_eq!(approval.tool_name, "write_file");
    engine
        .decide_approval(&approval.id, true, Some("runtime-test"))
        .unwrap();
}

async fn first_http_event(mut response: reqwest::Response) -> Value {
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        loop {
            let chunk = response.chunk().await.unwrap().expect("SSE ended early");
            bytes.extend_from_slice(&chunk);
            if let Some(end) = bytes.windows(2).position(|pair| pair == b"\n\n") {
                let frame = std::str::from_utf8(&bytes[..end]).unwrap();
                let data = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .expect("SSE event must have JSON data");
                let event: Value = serde_json::from_str(data).unwrap();
                let event_id = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("id: "))
                    .expect("SSE event must have a cursor");
                assert_eq!(event_id, event["cursor"].as_str().unwrap());
                assert_eq!(event_id, event["seq"].as_i64().unwrap().to_string());
                return event;
            }
        }
    })
    .await
    .expect("SSE event timed out")
}

#[tokio::test]
async fn http_run_exposes_domain_and_session_events_with_conflict_replay() {
    let temp = tempfile::tempdir().unwrap();
    let (base, mock, provider_server) = mock_server().await;
    let engine = setup(temp.path(), &base);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = server::router(App {
        engine: engine.clone(),
        token: "csrf-http-domain".into(),
        origin: origin.clone(),
    });
    let http_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let payload = serde_json::to_value(RunRequest {
        kind: SessionKind::Chat,
        title: "领域 HTTP 测试".into(),
        tasks: vec![task("domain-worker", "key-1", "hello", &[])],
    })
    .unwrap();
    let start = client
        .post(format!("{origin}/api/runs"))
        .header("x-peachsh-token", "csrf-http-domain")
        .header("idempotency-key", "http-domain-replay")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), reqwest::StatusCode::OK);
    let run: Run = start.json().await.unwrap();
    let run = settled(&engine, &run.id).await;
    assert_eq!(run.tasks[0].status, "completed");
    let first = first_http_event(
        client
            .get(format!("{origin}/api/runs/{}/events", run.id))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let session_id = first["session_id"].as_str().unwrap();
    let turn_id = first["turn_id"].as_str().unwrap();
    let session: Session = client
        .get(format!("{origin}/api/sessions/{session_id}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session.id.0, session_id);
    assert_eq!(session.legacy_run_id, run.id);
    assert_eq!(session.kind, SessionKind::Chat);
    let projects: Vec<Project> = client
        .get(format!("{origin}/api/projects"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, session.project_id);
    let turns: Vec<Turn> = client
        .get(format!("{origin}/api/sessions/{session_id}/turns"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id.0, turn_id);
    assert_eq!(turns[0].status, LifecycleStatus::Completed);
    let turn: Turn = client
        .get(format!("{origin}/api/turns/{turn_id}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(turn.id, turns[0].id);
    assert_eq!(turn.session_id, session.id);
    let session_first = first_http_event(
        client
            .get(format!("{origin}/api/sessions/{session_id}/events"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(session_first, first);
    let resumed_event = first_http_event(
        client
            .get(format!("{origin}/api/sessions/{session_id}/events?after=0"))
            .header("last-event-id", first["seq"].as_i64().unwrap().to_string())
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(resumed_event["seq"].as_i64().unwrap() > first["seq"].as_i64().unwrap());
    assert_eq!(resumed_event["session_id"], session_id);
    assert_eq!(resumed_event["turn_id"], turn_id);

    let replay: Run = client
        .post(format!("{origin}/api/runs"))
        .header("x-peachsh-token", "csrf-http-domain")
        .header("idempotency-key", "http-domain-replay")
        .json(&payload)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay.id, run.id);
    assert_eq!(replay.tasks.len(), 1);
    assert_eq!(replay.tasks[0].id, run.tasks[0].id);
    let mut changed = payload;
    changed["title"] = json!("different request");
    let conflict = client
        .post(format!("{origin}/api/runs"))
        .header("x-peachsh-token", "csrf-http-domain")
        .header("idempotency-key", "http-domain-replay")
        .json(&changed)
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(conflict.json::<Value>().await.unwrap()["code"], "conflict");
    assert_eq!(mock.inner.lock().unwrap().calls.len(), 1);
    for path in [
        "/api/sessions/missing-session",
        "/api/sessions/missing-session/turns",
        "/api/sessions/missing-session/events",
        "/api/turns/missing-turn",
    ] {
        let response = client.get(format!("{origin}{path}")).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(response.json::<Value>().await.unwrap()["code"], "not_found");
    }
    http_server.abort();
    provider_server.abort();
}

#[tokio::test]
async fn multiple_keys_limits_identity_and_resume() {
    let temp = tempfile::tempdir().unwrap();
    let (base, mock, server) = mock_server().await;
    let e = setup(temp.path(), &base);
    let r = e
        .start(RunRequest {
            kind: SessionKind::Team,
            title: "parallel".into(),
            tasks: vec![
                task("planner", "key-1", "hello", &[]),
                task("developer", "key-2", "hello", &[]),
                task("reviewer", "key-3", "hello", &[]),
            ],
        })
        .await
        .unwrap();
    let r = settled(&e, &r.id).await;
    assert!(
        r.tasks
            .iter()
            .all(|t| t.status == "completed" && t.output == "323")
    );
    {
        let m = mock.inner.lock().unwrap();
        assert_eq!(m.max, 2);
        assert_eq!(m.max_key["Bearer fake-key-a"], 1);
        assert_eq!(m.max_key["Bearer fake-key-b"], 1);
        assert!(
            m.calls
                .iter()
                .any(|c| c["body"]["model"] == "model-b" && c["key"] == "Bearer fake-key-b")
        );
    }
    let mut settings = e.store.settings().unwrap().unwrap();
    settings.routes[0].model = "changed-after-run".into();
    e.configure(settings).await.unwrap();
    let before = &r.tasks[0];
    e.resume(&before.id, "continue").await.unwrap();
    let after = settled(&e, &r.id).await;
    let after = &after.tasks[0];
    assert_eq!(before.id, after.id);
    assert_eq!(before.spec.role, after.spec.role);
    assert_eq!(after.route.model, "model-a");
    assert_eq!(after.usage["total_tokens"], 22);
    let mut changed = e.store.settings().unwrap().unwrap();
    changed.routes[0].base_url = "https://changed.invalid/v1".into();
    e.configure(changed).await.unwrap();
    assert!(
        e.resume(&before.id, "must not send the new key to the old host")
            .await
            .is_err()
    );
    let events = e.store.events(&r.id, 0).unwrap();
    let seq = events.last().unwrap().seq;
    assert!(e.store.events(&r.id, seq).unwrap().is_empty());
    server.abort();
}
#[tokio::test]
async fn explicit_session_kind_survives_arbitrary_member_names() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _, server) = mock_server().await;
    let e = setup(temp.path(), &base);

    let chat = e
        .start(RunRequest {
            kind: SessionKind::Chat,
            title: "explicit chat".into(),
            tasks: vec![task("developer", "key-1", "hello", &[])],
        })
        .await
        .unwrap();
    assert_eq!(chat.kind, SessionKind::Chat);
    let chat = settled(&e, &chat.id).await;
    assert_eq!(chat.tasks[0].spec.name, "developer");

    let team = e
        .start(RunRequest {
            kind: SessionKind::Team,
            title: "explicit team".into(),
            tasks: vec![task("chat-session", "key-1", "hello", &[])],
        })
        .await
        .unwrap();
    assert_eq!(team.kind, SessionKind::Team);
    let _ = settled(&e, &team.id).await;
    server.abort();
}

#[tokio::test]
async fn tool_execution_and_path_rejection() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _, server) = mock_server().await;
    let e = setup(temp.path(), &base);
    let r = e
        .start(RunRequest {
            kind: SessionKind::Team,
            title: "tools".into(),
            tasks: vec![task("writer", "key-1", "write-test", &["src"])],
        })
        .await
        .unwrap();
    approve_pending_write(&e, &r.tasks[0].id).await;
    let r = settled(&e, &r.id).await;
    assert_eq!(r.tasks[0].status, "completed");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/result.txt")).unwrap(),
        "rust-native-ok"
    );
    assert_eq!(r.tasks[0].usage["total_tokens"], 26);
    let events = e.store.events(&r.id, 0).unwrap();
    assert!(events.iter().any(|v| v.kind == "file_backup"));
    let r = e
        .start(RunRequest {
            kind: SessionKind::Team,
            title: "scope".into(),
            tasks: vec![task("writer", "key-1", "escape-test", &["src"])],
        })
        .await
        .unwrap();
    approve_pending_write(&e, &r.tasks[0].id).await;
    let r = settled(&e, &r.id).await;
    assert_eq!(r.tasks[0].status, "completed");
    assert!(r.tasks[0].messages.iter().any(|m| {
        m["role"] == "tool"
            && m["content"]
                .as_str()
                .and_then(|content| serde_json::from_str::<Value>(content).ok())
                .is_some_and(|content| content["error"].is_string())
    }));
    server.abort();
}
#[tokio::test]
async fn conflicts_cancel_and_configuration_lock() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _, server) = mock_server().await;
    let e = setup(temp.path(), &base);
    let invalid = RunRequest {
        kind: SessionKind::Team,
        title: "overlap".into(),
        tasks: vec![
            task("one", "key-1", "hello", &["src"]),
            task("two", "key-2", "hello", &["src/api"]),
        ],
    };
    assert!(e.start(invalid).await.is_err());
    let r = e
        .start(RunRequest {
            kind: SessionKind::Team,
            title: "active".into(),
            tasks: vec![
                task("one", "key-1", "hello", &["src"]),
                task("two", "key-3", "hello", &["tests"]),
            ],
        })
        .await
        .unwrap();
    assert!(
        e.configure(e.store.settings().unwrap().unwrap())
            .await
            .is_err()
    );
    assert!(
        e.start(RunRequest {
            kind: SessionKind::Team,
            title: "conflict".into(),
            tasks: vec![task("third", "key-2", "hello", &["SRC/api"])]
        })
        .await
        .is_err()
    );
    e.cancel(&r.tasks[1].id).unwrap();
    let r = settled(&e, &r.id).await;
    assert_eq!(r.tasks[1].status, "cancelled");
    server.abort();
}
#[tokio::test]
async fn restart_and_http_security() {
    let temp = tempfile::tempdir().unwrap();
    let e = setup(temp.path(), "http://127.0.0.1:1/v1");
    let route = e.store.settings().unwrap().unwrap().routes[0].clone();
    let durable_task = Task {
        id: "durable-id".into(),
        run_id: "durable-run".into(),
        spec: task("worker", "key-1", "hello", &[]),
        route,
        workspace: temp.path().to_string_lossy().into(),
        status: "running".into(),
        output: "partial".into(),
        error: None,
        messages: vec![],
        usage: Value::Null,
        created_at: now(),
        updated_at: now(),
    };
    e.store
        .create_run(&Run {
            id: "durable-run".into(),
            title: "recovery".into(),
            kind: SessionKind::Team,
            created_at: now(),
            tasks: vec![durable_task],
        })
        .unwrap();
    e.store
        .event(
            "durable-id",
            "delta",
            json!({"text":"partial recovered from event log"}),
        )
        .unwrap();
    let store = Store::open(&temp.path().join("test.db")).unwrap();
    assert_eq!(store.recover().unwrap(), 1);
    assert_eq!(
        store.events("durable-run", 0).unwrap().last().unwrap().data["status"],
        "interrupted"
    );
    assert_eq!(store.task("durable-id").unwrap().status, "interrupted");
    assert_eq!(
        store.task("durable-id").unwrap().output,
        "partial recovered from event log"
    );
    let app = server::router(App {
        engine: e,
        token: "csrf-test".into(),
        origin: "http://127.0.0.1:3090".into(),
    });
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .header("host", "evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .header("host", "127.0.0.1:3090")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("host", "127.0.0.1:3090")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let payload = serde_json::to_vec(&RunRequest {
        kind: SessionKind::Team,
        title: "idempotent".into(),
        tasks: vec![task("idempotent-worker", "key-1", "hello", &[])],
    })
    .unwrap();
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("host", "127.0.0.1:3090")
                .header("x-peachsh-token", "csrf-test")
                .header("idempotency-key", "runtime-idempotency")
                .header("content-type", "application/json")
                .body(Body::from(payload.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let first: Run =
        serde_json::from_slice(&to_bytes(r.into_body(), 1_000_000).await.unwrap()).unwrap();
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("host", "127.0.0.1:3090")
                .header("x-peachsh-token", "csrf-test")
                .header("idempotency-key", "runtime-idempotency")
                .header("content-type", "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let second: Run =
        serde_json::from_slice(&to_bytes(r.into_body(), 1_000_000).await.unwrap()).unwrap();
    assert_eq!(first.id, second.id);
    let r = app
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .header("host", "127.0.0.1:3090")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let b = to_bytes(r.into_body(), 1_000_000).await.unwrap();
    let s = String::from_utf8(b.to_vec()).unwrap();
    assert!(!s.contains("fake-key-a"));
    assert!(!s.contains("fake-key-b"));
}

#[tokio::test]
async fn dependency_order_summary_and_cycle_rejection() {
    let temp = tempfile::tempdir().unwrap();
    let (base, mock, server) = mock_server().await;
    let e = setup(temp.path(), &base);
    let first = task("developer", "key-1", "write-test", &["src"]);
    let mut second = task("reviewer", "key-2", "review predecessor", &["src"]);
    second.depends_on = vec!["developer".into()];
    let mut third = task("summary", "key-3", "summarize", &[]);
    third.depends_on = vec!["developer".into(), "reviewer".into()];
    let r = e
        .start(RunRequest {
            kind: SessionKind::Team,
            title: "pipeline".into(),
            tasks: vec![first.clone(), second, third],
        })
        .await
        .unwrap();
    approve_pending_write(&e, &r.tasks[0].id).await;
    let r = settled(&e, &r.id).await;
    assert!(r.tasks.iter().all(|t| t.status == "completed"));
    assert!(r.tasks[2].messages.iter().any(|m| {
        m["content"]
            .as_str()
            .is_some_and(|s| s.contains("Completed predecessor results") && s.contains("reviewer"))
    }));
    {
        let c = mock.inner.lock().unwrap();
        assert_eq!(c.max, 1);
        assert_eq!(c.calls[2]["body"]["model"], "model-b");
    }
    let mut a = first;
    a.depends_on = vec!["loop".into()];
    let mut b = task("loop", "key-2", "hello", &[]);
    b.depends_on = vec!["developer".into()];
    assert!(
        e.start(RunRequest {
            kind: SessionKind::Team,
            title: "cycle".into(),
            tasks: vec![a, b]
        })
        .await
        .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn replace_existing_file_preserves_hardlink_and_command_permission() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _, server) = mock_server().await;
    let e = setup(temp.path(), &base);
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("original.txt"), "original").unwrap();
    std::fs::hard_link(
        temp.path().join("original.txt"),
        temp.path().join("src/result.txt"),
    )
    .unwrap();
    let r = e
        .start(RunRequest {
            kind: SessionKind::Team,
            title: "replace".into(),
            tasks: vec![task("writer", "key-1", "write-test", &["src"])],
        })
        .await
        .unwrap();
    approve_pending_write(&e, &r.tasks[0].id).await;
    let r = settled(&e, &r.id).await;
    assert_eq!(r.tasks[0].status, "completed");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("original.txt")).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/result.txt")).unwrap(),
        "rust-native-ok"
    );
    assert!(
        e.store
            .events(&r.id, 0)
            .unwrap()
            .iter()
            .any(|e| e.kind == "file_backup"
                && e.data["path"] == "src/result.txt"
                && e.data["change_id"].is_string()
                && e.data.get("previous").is_none())
    );
    let mut command_task = r.tasks[0].clone();
    let args = json!({"command":"Write-Output 'native-command-ok'"});
    assert!(
        peachsh::workspace::execute(&command_task, "run_command", &args)
            .await
            .is_err()
    );
    command_task.spec.allow_commands = true;
    let result = peachsh::workspace::execute(&command_task, "run_command", &args)
        .await
        .unwrap();
    assert_eq!(result["exit_code"], 0);
    assert!(
        result["stdout"]
            .as_str()
            .unwrap()
            .contains("native-command-ok")
    );
    server.abort();
}

#[tokio::test]
async fn wasm_plugin_runs_through_agent_tool_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _, server) = mock_server().await;
    let engine = setup(temp.path(), &base);
    let plugin_dir = temp.path().join(".peachsh/plugins/echo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let wat = r#"(module (memory (export "memory") 1 1) (func (export "alloc") (param i32) (result i32) (i32.const 0)) (func (export "run_json") (param i32 i32) (result i64) (i64.extend_i32_u (local.get 1))))"#;
    let wasm_bytes = wat::parse_str(wat).unwrap();
    let manifest = wasm::Manifest {
        abi: wasm::ABI_VERSION.into(),
        id: "echo".into(),
        name: "Echo".into(),
        version: "1".into(),
        capabilities: vec!["json".into()],
        fuel: wasm::DEFAULT_FUEL,
        memory_pages: 1,
        sha256: wasm::sha256_hex(&wasm_bytes),
    };
    std::fs::write(plugin_dir.join("plugin.wasm"), &wasm_bytes).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let mut spec = task("wasm-worker", "key-1", "hello", &[]);
    spec.tools = true;
    let task = Task {
        id: "wasm-task".into(),
        run_id: "wasm-run".into(),
        spec,
        route: engine.store.settings().unwrap().unwrap().routes[0].clone(),
        workspace: temp.path().to_string_lossy().into(),
        status: "queued".into(),
        output: String::new(),
        error: None,
        messages: vec![],
        usage: Value::Null,
        created_at: now(),
        updated_at: now(),
    };
    let result = workspace::execute(
        &task,
        "run_wasm",
        &json!({"plugin":"echo","input":{"message":"边界测试"}}),
    )
    .await
    .unwrap();
    assert_eq!(result["output"]["message"], "边界测试");
    assert_eq!(
        workspace::definitions(&task)
            .iter()
            .filter_map(|v| v.pointer("/function/name").and_then(Value::as_str))
            .filter(|name| *name == "run_wasm")
            .count(),
        1
    );
    server.abort();
}

#[test]
fn schema_v1_migrates_to_current_version() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("legacy.db");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE config (id TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE secrets (id TEXT PRIMARY KEY, value BLOB NOT NULL);
             CREATE TABLE runs (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE tasks (id TEXT PRIMARY KEY, run_id TEXT NOT NULL, value TEXT NOT NULL);
             CREATE TABLE events (seq INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);
             PRAGMA user_version=1;",
        )
        .unwrap();
        db.execute(
            "INSERT INTO runs(id,title,created_at) VALUES ('legacy-chat','旧对话',1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO tasks(id,run_id,value) VALUES ('legacy-task','legacy-chat',?1)",
            [json!({"spec":{"name":"chat-session"}}).to_string()],
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.schema_version().unwrap(),
        peachsh::store::SCHEMA_VERSION
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    let exists: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='idempotency'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(exists, 1);
    let migration: i64 = db
        .query_row(
            "SELECT count(*) FROM schema_migrations WHERE id='session-kind-explicit'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(migration, 1);
    let db = rusqlite::Connection::open(&path).unwrap();
    let kind: String = db
        .query_row("SELECT kind FROM runs WHERE id='legacy-chat'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(kind, "chat");
    drop(db);
    // Opening the already migrated database is idempotent and does not
    // duplicate the migration marker.
    let _again = Store::open(&path).unwrap();
}
