use crate::{
    domain::{Route, RunRequest, SessionKind, Settings, TaskSpec},
    engine::Engine,
    server::{self, App},
    store::Store,
};
use axum::{Json, Router, extract::State, routing::post};
use reqwest::{Client, Response, StatusCode};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tempfile::TempDir;

const TOKEN: &str = "workspace-http-test-token";

type Script = (
    Arc<Mutex<VecDeque<Value>>>,
    Arc<std::sync::atomic::AtomicUsize>,
);

async fn provider(State(script): State<Script>, Json(_): Json<Value>) -> String {
    script.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let delta = script
        .0
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| json!({"content":"done"}));
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn write_call(id: &str, path: &str, content: &str) -> Value {
    json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{
        "name":"write_file","arguments":json!({"path":path,"content":content}).to_string()
    }}]})
}

struct Harness {
    dir: TempDir,
    engine: Arc<Engine>,
    origin: String,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    _provider: tokio::task::JoinHandle<()>,
    _host: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn db(&self) -> Connection {
        Connection::open(self.dir.path().join("workspace.db")).unwrap()
    }

    fn file(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join("src").join(name)
    }

    fn restore_url(&self, id: &str) -> String {
        format!("{}/api/tasks/{id}/restore", self.origin)
    }
}

async fn harness(replies: Vec<Value>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let script = Arc::new(Mutex::new(VecDeque::from(replies)));
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", provider_listener.local_addr().unwrap());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider_app = Router::new()
        .route("/v1/chat/completions", post(provider))
        .with_state((script, calls.clone()));
    let provider_server = tokio::spawn(async move {
        axum::serve(provider_listener, provider_app).await.unwrap();
    });
    let store = Arc::new(Store::open(&dir.path().join("workspace.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: dir.path().to_string_lossy().into_owned(),
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
    store.put_secret("route", "fake-http-key").unwrap();
    authorize_routes(&store);
    let engine = Engine::new(store, 2).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = server::router(App {
        engine: engine.clone(),
        token: TOKEN.into(),
        origin: origin.clone(),
    });
    let host = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Harness {
        dir,
        engine,
        origin,
        calls,
        _provider: provider_server,
        _host: host,
    }
}

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

fn security(response: &Response) {
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'self'")
    );
}

async fn body(response: Response, status: StatusCode) -> Value {
    assert_eq!(response.status(), status);
    security(&response);
    response.json().await.unwrap()
}

async fn start_task(h: &Harness, paths: &[(&str, &str)]) -> String {
    let task = h
        .engine
        .start(RunRequest {
            title: "workspace-http".into(),
            kind: SessionKind::Chat,
            tasks: vec![TaskSpec {
                name: "worker".into(),
                role: "worker".into(),
                route_id: "route".into(),
                prompt: "write".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: !paths.is_empty(),
                allow_commands: false,
                max_rounds: 5,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    let deadline = Instant::now() + Duration::from_secs(15);
    for _ in paths {
        loop {
            if let Some(approval) = h
                .engine
                .store
                .pending_approvals_for_task(&task.id)
                .unwrap()
                .into_iter()
                .next()
            {
                h.engine
                    .decide_approval(&approval.id, true, Some("test"))
                    .unwrap();
                break;
            }
            assert!(Instant::now() < deadline, "approval not reached");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    loop {
        if !h.engine.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "task not settled");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for (path, content) in paths {
        assert_eq!(
            std::fs::read_to_string(h.dir.path().join(path)).unwrap(),
            *content
        );
    }
    task.id
}

fn authorize_routes(store: &Store) {
    use crate::{network_authorization::*, project_config::*};
    let settings = store.settings().unwrap().unwrap();
    let project = store
        .register_project(RegisterProject {
            name: "restore-http".into(),
            root_path: settings.workspace,
            idempotency_key: "restore-project".into(),
        })
        .unwrap();
    store
        .select_project(SelectProject {
            project_id: project.id.clone(),
            expected_revision: store.current_project().unwrap().revision,
        })
        .unwrap();
    store
        .configure_project(ConfigureProject {
            project_id: project.id.clone(),
            expected_revision: store.project_config(&project.id).unwrap().revision,
            config: ProjectConfig {
                allowed_route_ids: vec!["route".into()],
                default_route_id: Some("route".into()),
                tools: true,
                write_scopes: vec!["src".into()],
                allow_commands: false,
            },
        })
        .unwrap();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(project.id),
    };
    let preview = store.network_preview(subject.clone()).unwrap();
    store
        .network_confirm(
            subject,
            ConfirmRequest {
                config_revision: preview.config_revision,
                credential_revision: preview.credential_revision,
                binding_digest: preview.binding_digest,
                purposes: vec![NetworkPurpose::Completion],
            },
            "restore-grant",
        )
        .unwrap();
}

// Drop releases a blocked callback even if an assertion unwinds before continue.
struct ProbeGuard {
    engine: Arc<Engine>,
    release: Option<std::sync::mpsc::Sender<()>>,
}
impl Drop for ProbeGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        self.engine.clear_restore_probe();
    }
}

fn outcome_rows(db: &Connection) -> Vec<(String, String)> {
    db.prepare("SELECT path,status FROM workspace_restore_outcomes ORDER BY rowid")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn assert_files(root: &Path) {
    for (name, expected) in [("a", "a-before-secret"), ("b", "b-after"), ("c", "c-after")] {
        assert_eq!(
            std::fs::read_to_string(root.join(format!("src/{name}.txt"))).unwrap(),
            expected
        );
    }
}

fn durable_counts(db: &Connection) -> (i64, i64, i64) {
    db.query_row("SELECT (SELECT count(*) FROM tasks),(SELECT count(*) FROM turns),(SELECT count(*) FROM network_calls)", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
}

async fn assert_sealed(
    engine: &Arc<Engine>,
    task: &str,
    db: &Connection,
    calls: &std::sync::atomic::AtomicUsize,
) {
    use crate::{
        domain::{ChatTurnError, SendChatTurn},
        workspace_changes::WorkspaceChangeError,
    };
    let counts = durable_counts(db);
    let previous = serde_json::to_value(engine.store.task(task).unwrap()).unwrap();
    let call_count = calls.load(std::sync::atomic::Ordering::SeqCst);
    let resume = engine.resume(task, "must not retry").await.unwrap_err();
    assert!(
        matches!(
            resume.downcast_ref::<WorkspaceChangeError>(),
            Some(WorkspaceChangeError::Unknown { .. } | WorkspaceChangeError::Unrestorable)
        ),
        "resume must fail for restore sealing, got {resume:?}"
    );
    let context = engine
        .store
        .run_context(&engine.store.task(task).unwrap().run_id)
        .unwrap();
    let turn = engine
        .send_chat_turn(SendChatTurn {
            session_id: context.session.id,
            agent_id: context.tasks[0].agent_id.clone(),
            expected_last_turn_id: context.latest_turn.id,
            message: "must not bypass partial".into(),
            idempotency_key: "partial-sealed-turn".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(
            turn.downcast_ref::<ChatTurnError>(),
            Some(ChatTurnError::UnresolvedEffects)
        ),
        "new turn must fail for unresolved effects, got {turn:?}"
    );
    assert_eq!(durable_counts(db), counts);
    assert_eq!(
        serde_json::to_value(engine.store.task(task).unwrap()).unwrap(),
        previous
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), call_count);
    assert!(!engine.is_busy());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partial_restore_committed_barrier_http_reopen_no_retry() {
    use crate::engine::restore_probe::Stage;
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    let h = harness(vec![
        write_call("w1", "src/a.txt", "a-after"),
        write_call("w2", "src/b.txt", "b-after"),
        write_call("w3", "src/c.txt", "c-after"),
        json!({"content":"done"}),
    ])
    .await;
    for name in ["a", "b", "c"] {
        std::fs::write(
            h.file(&format!("{name}.txt")),
            format!("{name}-before-secret"),
        )
        .unwrap();
    }
    let task = start_task(
        &h,
        &[
            ("src/a.txt", "a-after"),
            ("src/b.txt", "b-after"),
            ("src/c.txt", "c-after"),
        ],
    )
    .await;
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let ready_tx = Mutex::new(Some(ready_tx));
    let (continue_tx, continue_rx) = std::sync::mpsc::channel();
    let continue_rx = Mutex::new(continue_rx);
    h.engine.set_restore_probe(
        task.clone(),
        Arc::new(move |event| {
            let pause = event.path == "src/a.txt" && matches!(event.stage, Stage::OutcomeCommitted);
            captured.lock().unwrap().push(event);
            if pause {
                if let Some(ready) = ready_tx.lock().unwrap().take() {
                    let _ = ready.send(());
                }
                continue_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(15))
                    .expect("bounded continue barrier");
            }
        }),
    );
    let mut guard = ProbeGuard {
        engine: h.engine.clone(),
        release: Some(continue_tx),
    };
    let url = h.restore_url(&task);
    let request = tokio::spawn(async move {
        client()
            .post(url)
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(15), ready_rx)
        .await
        .expect("bounded ready barrier")
        .unwrap();
    // Independent connection sees committed A while callback is paused.
    let db = h.db();
    assert_eq!(
        outcome_rows(&db),
        vec![
            ("src/a.txt".into(), "complete".into()),
            ("src/b.txt".into(), "claimed".into()),
            ("src/c.txt".into(), "claimed".into())
        ]
    );
    assert_files(h.dir.path());
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(h.file("b.txt"))
        .unwrap();
    guard.release.take().unwrap().send(()).unwrap();
    let response = tokio::time::timeout(Duration::from_secs(20), request)
        .await
        .unwrap()
        .unwrap();
    let partial = body(response, StatusCode::CONFLICT).await;
    assert_eq!(partial["ok"], false);
    assert_eq!(partial["receipt"]["status"], "partial");
    assert_eq!(partial["receipt"]["restored"], 1);
    let outcomes = partial["receipt"]["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 3);
    for (index, (path, status)) in [
        ("src/a.txt", "complete"),
        ("src/b.txt", "unknown"),
        ("src/c.txt", "unknown"),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(outcomes[index]["path"], path);
        assert_eq!(outcomes[index]["status"], status);
    }
    {
        let events = events.lock().unwrap();
        assert!(events.iter().all(|e| e.task_id == task));
        assert!(events.iter().any(|e| e.path == "src/b.txt"
            && matches!(e.stage, Stage::ReadCurrent)
            && e.error.as_ref().is_some_and(|codes| codes.io.is_some())));
        let committed = events
            .iter()
            .position(|e| e.path == "src/a.txt" && matches!(e.stage, Stage::OutcomeCommitted))
            .expect("A committed event");
        assert!(
            !events[committed + 1..]
                .iter()
                .any(|e| e.path == "src/c.txt")
        );
        assert!(!events.iter().any(|e| e.path == "src/c.txt"
            && matches!(
                e.stage,
                Stage::FileAction | Stage::OutcomePersist | Stage::OutcomeCommitted
            )));
    }
    for secret in [
        "before-secret",
        "fake-http-key",
        TOKEN,
        "sqlite",
        "os error",
    ] {
        assert!(!partial.to_string().contains(secret));
    }
    assert!(
        !partial
            .to_string()
            .contains(h.dir.path().to_string_lossy().as_ref())
    );
    drop(lock);
    drop(guard);
    assert_files(h.dir.path());
    let committed = outcome_rows(&db);
    assert_eq!(
        committed,
        vec![
            ("src/a.txt".into(), "complete".into()),
            ("src/b.txt".into(), "unknown".into()),
            ("src/c.txt".into(), "unknown".into())
        ]
    );
    let retry = body(
        client()
            .post(h.restore_url(&task))
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap(),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(retry["receipt"], partial["receipt"]);
    assert_sealed(&h.engine, &task, &db, &h.calls).await;
    assert_files(h.dir.path());
    assert_eq!(outcome_rows(&db), committed);
    // Stop and join the HTTP servers; reopen actual durable Store/Engine.
    let calls = h.calls.clone();
    let Harness {
        dir,
        engine,
        _host: host,
        _provider: provider,
        ..
    } = h;
    host.abort();
    assert!(host.await.unwrap_err().is_cancelled());
    provider.abort();
    assert!(provider.await.unwrap_err().is_cancelled());
    drop(engine);
    drop(db);
    let store = Arc::new(Store::open(&dir.path().join("workspace.db")).unwrap());
    store.recover().unwrap();
    let engine = Engine::new(store, 2).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = server::router(App {
        engine: engine.clone(),
        token: TOKEN.into(),
        origin: origin.clone(),
    });
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let reopened = body(
        client()
            .post(format!("{origin}/api/tasks/{task}/restore"))
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap(),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(reopened["receipt"], partial["receipt"]);
    let reopened_db = Connection::open(dir.path().join("workspace.db")).unwrap();
    assert_sealed(&engine, &task, &reopened_db, &calls).await;
    assert_files(dir.path());
    let db = Connection::open(dir.path().join("workspace.db")).unwrap();
    assert_eq!(outcome_rows(&db), committed);
    assert_eq!(
        db.query_row("SELECT count(*) FROM workspace_restores", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
