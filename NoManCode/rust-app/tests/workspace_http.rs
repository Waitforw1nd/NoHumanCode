use axum::{Json, Router, extract::State, routing::post};
use peachsh::{
    domain::{Route, RunRequest, SessionKind, Settings, TaskSpec},
    engine::Engine,
    server::{self, App},
    store::Store,
};
use reqwest::{Client, Response, StatusCode};
use rusqlite::{Connection, ErrorCode};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tempfile::TempDir;

const TOKEN: &str = "workspace-http-test-token";
const BEFORE_SECRET: &str = "sk-before-http-secret";

type Script = Arc<Mutex<VecDeque<Value>>>;

async fn provider(State(script): State<Script>, Json(_): Json<Value>) -> String {
    let delta = script
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

    fn changes_url(&self, id: &str) -> String {
        format!("{}/api/tasks/{id}/changes", self.origin)
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
    let provider_app = Router::new()
        .route("/v1/chat/completions", post(provider))
        .with_state(script);
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
            kind: SessionKind::Team,
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

fn rows(db: &Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn private_absent(text: &str, root: &Path, sealed: &[u8]) {
    let path = root.to_string_lossy();
    let hex = sealed
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert!(!text.contains(BEFORE_SECRET));
    assert!(!text.contains(path.as_ref()));
    assert!(!text.contains(&path.replace('\\', "\\\\")));
    assert!(!text.contains(&hex));
    assert!(
        !text
            .as_bytes()
            .windows(sealed.len())
            .any(|part| part == sealed)
    );
}

#[tokio::test]
async fn normal_dto_auth_and_empty_task_are_distinct_and_safe() {
    let h = harness(vec![
        write_call("w1", "src/a.txt", "after"),
        json!({"content":"done"}),
    ])
    .await;
    std::fs::write(h.file("a.txt"), BEFORE_SECRET).unwrap();
    let task = start_task(&h, &[("src/a.txt", "after")]).await;
    let c = client();
    let db = h.db();
    let baseline = (
        rows(&db, "workspace_changes"),
        rows(&db, "workspace_restores"),
    );
    let sealed: Vec<u8> = db
        .query_row(
            "SELECT before_blob FROM workspace_changes WHERE task_id=?1",
            [&task],
            |row| row.get(0),
        )
        .unwrap();
    for response in [
        c.get(h.changes_url(&task))
            .header("host", "invalid.local")
            .send()
            .await
            .unwrap(),
        c.get(h.changes_url(&task))
            .header("origin", "http://invalid.local")
            .send()
            .await
            .unwrap(),
        c.post(h.restore_url(&task)).send().await.unwrap(),
        c.post(h.restore_url(&task))
            .header("x-peachsh-token", "wrong")
            .send()
            .await
            .unwrap(),
        c.post(h.restore_url(&task))
            .header("origin", "http://invalid.local")
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap(),
        c.post(h.restore_url(&task))
            .header("host", "invalid.local")
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap(),
    ] {
        let rejected = body(response, StatusCode::FORBIDDEN).await;
        assert_eq!(rejected["code"], "forbidden");
        assert_eq!(rows(&db, "workspace_changes"), baseline.0);
        assert_eq!(rows(&db, "workspace_restores"), baseline.1);
        assert_eq!(std::fs::read_to_string(h.file("a.txt")).unwrap(), "after");
    }
    let invalid = body(
        c.get(h.changes_url("%0A")).send().await.unwrap(),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(invalid["code"], "request_failed");
    let unknown = body(
        c.get(h.changes_url("unknown-task-id"))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(unknown["code"], "not_found");
    let invalid_post = body(
        c.post(h.restore_url("%0A"))
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(invalid_post["code"], "request_failed");
    let unknown_post = body(
        c.post(h.restore_url("unknown-task-id"))
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(unknown_post["code"], "not_found");
    let changes = body(
        c.get(h.changes_url(&task)).send().await.unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(changes["changes"].as_array().unwrap().len(), 1);
    assert_eq!(changes["changes"][0]["path"], "src/a.txt");
    assert_eq!(changes["changes"][0]["kind"], "modified");
    assert_eq!(changes["changes"][0]["state"], "finished");
    assert_eq!(changes["changes"][0]["restorable"], true);
    assert_eq!(
        changes["changes"][0]["before_digest"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        changes["changes"][0]["after_digest"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert!(changes["restore"].is_null());
    private_absent(&changes.to_string(), h.dir.path(), &sealed);
    let restored = body(
        c.post(h.restore_url(&task))
            .header("x-peachsh-token", TOKEN)
            .header("origin", &h.origin)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(restored["ok"], true);
    assert_eq!(restored["receipt"]["status"], "complete");
    assert_eq!(restored["receipt"]["restored"], 1);
    assert_eq!(restored["receipt"]["outcomes"][0]["status"], "complete");
    private_absent(&restored.to_string(), h.dir.path(), &sealed);
    let restore_id = restored["receipt"]["restore_id"].as_str().unwrap();
    assert!(!restore_id.is_empty());
    assert_eq!(
        std::fs::read_to_string(h.file("a.txt")).unwrap(),
        BEFORE_SECRET
    );
    let after = body(
        c.get(h.changes_url(&task)).send().await.unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(after["restore"], restored["receipt"]);
    assert_eq!(after["changes"][0]["restore_state"], "restored");
    private_absent(&after.to_string(), h.dir.path(), &sealed);
    let run_id = h.engine.store.task(&task).unwrap().run_id;
    let persisted = h.engine.store.events(&run_id, 0).unwrap();
    let events = serde_json::to_string(&persisted).unwrap();
    private_absent(&events, h.dir.path(), &sealed);
    let mut stream = c
        .get(format!("{}/api/runs/{run_id}/events", h.origin))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    security(&stream);
    let mut frames = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while frames.matches("data:").count() < persisted.len() {
            let chunk = stream.chunk().await.unwrap().unwrap();
            frames.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    })
    .await
    .unwrap();
    private_absent(&frames, h.dir.path(), &sealed);
    let empty = start_task(&h, &[]).await;
    let noop = body(
        c.post(h.restore_url(&empty))
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(noop["receipt"]["restore_id"], Value::Null);
    assert_eq!(noop["receipt"]["restored"], 0);
    assert_eq!(rows(&db, "workspace_restores"), baseline.1 + 1);
}

#[tokio::test]
async fn internal_error_and_unknown_receipt_hide_database_details() {
    let h = harness(vec![
        write_call("w1", "src/a.txt", "after"),
        json!({"content":"done"}),
    ])
    .await;
    std::fs::write(h.file("a.txt"), BEFORE_SECRET).unwrap();
    let task = start_task(&h, &[("src/a.txt", "after")]).await;
    let db = h.db();
    db.execute_batch("CREATE TRIGGER fail_outcome BEFORE UPDATE OF status ON workspace_restore_outcomes WHEN NEW.status='complete' BEGIN SELECT RAISE(ABORT,'SQL-private-trigger-secret'); END;").unwrap();
    let c = client();
    let first = body(
        c.post(h.restore_url(&task))
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap(),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(first["ok"], false);
    assert_eq!(first["receipt"]["status"], "unknown");
    assert_eq!(first["receipt"]["outcomes"][0]["status"], "unknown");
    let restore_id = first["receipt"]["restore_id"].as_str().unwrap().to_owned();
    assert!(!restore_id.is_empty());
    for text in [BEFORE_SECRET, "SQL-private-trigger-secret"] {
        assert!(!first.to_string().contains(text));
    }
    let retry = body(
        c.post(h.restore_url(&task))
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap(),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(retry["receipt"]["restore_id"], restore_id);
    assert_eq!(
        std::fs::read_to_string(h.file("a.txt")).unwrap(),
        BEFORE_SECRET
    );
    db.execute_batch("DROP TRIGGER fail_outcome; ALTER TABLE workspace_changes RENAME TO workspace_changes_broken;")
        .unwrap();
    let failure = body(
        c.get(h.changes_url(&task)).send().await.unwrap(),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    assert_eq!(failure["code"], "internal");
    assert_eq!(failure["message"], "文件变更服务失败");
    assert_eq!(failure["retryable"], false);
    for text in [
        BEFORE_SECRET,
        "workspace_changes",
        "SQL-private-trigger-secret",
        &h.dir.path().to_string_lossy(),
    ] {
        assert!(!failure.to_string().contains(text));
    }
}

#[cfg(windows)]
fn writer_active(db_path: &Path) -> bool {
    let db = Connection::open(db_path).unwrap();
    db.busy_timeout(Duration::ZERO).unwrap();
    match db.execute_batch("BEGIN IMMEDIATE; ROLLBACK;") {
        Ok(()) => false,
        Err(rusqlite::Error::SqliteFailure(error, _)) => error.code == ErrorCode::DatabaseBusy,
        Err(error) => panic!("unexpected DB probe: {error}"),
    }
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_restore_reports_completed_and_unattempted_paths() {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};

    let h = harness(vec![
        write_call("w1", "src/a.txt", "a-after"),
        write_call("w2", "src/b.txt", "b-after"),
        json!({"content":"done"}),
    ])
    .await;
    std::fs::write(h.file("a.txt"), "a-before-secret").unwrap();
    std::fs::write(h.file("b.txt"), "b-before-secret").unwrap();
    let task = start_task(&h, &[("src/a.txt", "a-after"), ("src/b.txt", "b-after")]).await;
    let db_path = h.dir.path().join("workspace.db");
    h.db().execute_batch("CREATE TRIGGER pause_first_outcome BEFORE UPDATE OF status ON workspace_restore_outcomes WHEN NEW.status='complete' AND NEW.path='src/a.txt' BEGIN SELECT sum(x) FROM (WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x<10000000) SELECT x FROM seq); END;").unwrap();
    let c = client();
    let url = h.restore_url(&task);
    let request = tokio::spawn(async move {
        c.post(url)
            .header("x-peachsh-token", TOKEN)
            .send()
            .await
            .unwrap()
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let first_restored = std::fs::read(h.file("a.txt")).unwrap() == b"a-before-secret";
        if first_restored && writer_active(&db_path) {
            break;
        }
        assert!(!request.is_finished(), "restore finished before barrier");
        assert!(
            Instant::now() < deadline,
            "first outcome barrier not reached"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(h.file("b.txt"))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(h.file("a.txt")).unwrap(),
        "a-before-secret"
    );
    let response = tokio::time::timeout(Duration::from_secs(20), request)
        .await
        .unwrap()
        .unwrap();
    let partial = body(response, StatusCode::CONFLICT).await;
    assert_eq!(partial["receipt"]["status"], "partial");
    assert_eq!(partial["receipt"]["restored"], 1);
    assert_eq!(partial["receipt"]["outcomes"].as_array().unwrap().len(), 2);
    assert_eq!(partial["receipt"]["outcomes"][0]["status"], "complete");
    assert_eq!(partial["receipt"]["outcomes"][1]["status"], "unknown");
    assert_eq!(partial["ok"], false);
    assert!(!partial.to_string().contains("secret"));
    drop(lock);
    assert_eq!(std::fs::read_to_string(h.file("b.txt")).unwrap(), "b-after");
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
    assert_eq!(std::fs::read_to_string(h.file("b.txt")).unwrap(), "b-after");
}
