use axum::{Json, Router, extract::State, routing::post};
use peachsh::{
    domain::{Route, RunRequest, SessionKind, Settings, TaskSpec},
    engine::Engine,
    server::{self, App},
    store::Store,
    workspace_changes::RestoreStatus,
};
use rusqlite::{Connection, ErrorCode};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const BEFORE: &str = "before-crash";
const AFTER: &str = "after-crash";
const TOKEN: &str = "workspace-crash-test-token";

type Script = Arc<Mutex<VecDeque<Value>>>;

async fn provider(State(script): State<Script>, Json(_): Json<Value>) -> String {
    let delta = script.lock().unwrap().pop_front().unwrap();
    if delta["stall"] == true {
        std::future::pending::<()>().await;
    }
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

async fn mock_provider(stall_after_write: bool) -> String {
    let script = Arc::new(Mutex::new(VecDeque::from([
        json!({"tool_calls":[{"index":0,"id":"crash-write","type":"function","function":{"name":"write_file","arguments":json!({"path":"src/a.txt","content":AFTER}).to_string()}}]}),
        if stall_after_write {
            json!({"stall":true})
        } else {
            json!({"content":"done"})
        },
    ])));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(provider))
                .with_state(script),
        )
        .await
        .unwrap();
    });
    base
}

fn open_engine(root: &Path) -> Arc<Engine> {
    let store = Arc::new(Store::open(&root.join("workspace.db")).unwrap());
    store.recover().unwrap();
    Engine::new(store, 2).unwrap()
}

async fn http_host(engine: Arc<Engine>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = server::router(App {
        engine,
        token: TOKEN.into(),
        origin: origin.clone(),
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (origin, server)
}

async fn written_task(root: &Path) -> String {
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.txt"), BEFORE).unwrap();
    let base = mock_provider(false).await;
    let engine = open_engine(root);
    engine
        .store
        .save_settings(&Settings {
            workspace: root.to_string_lossy().into_owned(),
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
    engine.store.put_secret("route", "fake-test-key").unwrap();
    let task = engine
        .start(RunRequest {
            title: "crash".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "worker".into(),
                role: "worker".into(),
                route_id: "route".into(),
                prompt: "write".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 4,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    let deadline = Instant::now() + Duration::from_secs(15);
    let approval = loop {
        if let Some(value) = engine
            .store
            .pending_approvals_for_task(&task.id)
            .unwrap()
            .into_iter()
            .next()
        {
            break value;
        }
        assert!(Instant::now() < deadline, "approval was not reached");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    engine
        .decide_approval(&approval.id, true, Some("test"))
        .unwrap();
    loop {
        if !engine.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "write did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::read_to_string(root.join("src/a.txt")).unwrap(),
        AFTER
    );
    assert_eq!(engine.changes(&task.id).unwrap().len(), 1);
    task.id
}

fn install_outcome_barrier(db_path: &Path) {
    let db = Connection::open(db_path).unwrap();
    // This trigger exists only in the disposable test database. It keeps the
    // outcome UPDATE transaction open long enough to prove the crash window.
    db.execute_batch(
        "CREATE TRIGGER crash_outcome BEFORE UPDATE OF status ON workspace_restore_outcomes
         WHEN NEW.status='complete' BEGIN
           SELECT sum(x) FROM (WITH RECURSIVE seq(x) AS
             (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x<1000000000)
             SELECT x FROM seq);
         END;",
    )
    .unwrap();
}

fn install_write_finish_barrier(db_path: &Path) {
    let db = Connection::open(db_path).unwrap();
    db.execute_batch(
        "CREATE TRIGGER crash_write_finish BEFORE UPDATE OF state ON workspace_changes
         WHEN NEW.state='finished' BEGIN
           SELECT sum(x) FROM (WITH RECURSIVE seq(x) AS
             (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x<1000000000)
             SELECT x FROM seq);
         END;",
    )
    .unwrap();
}

fn writer_active(db_path: &Path) -> bool {
    let db = Connection::open(db_path).unwrap();
    db.busy_timeout(Duration::ZERO).unwrap();
    match db.execute_batch("BEGIN IMMEDIATE; ROLLBACK;") {
        Ok(()) => false,
        Err(rusqlite::Error::SqliteFailure(error, _)) => error.code == ErrorCode::DatabaseBusy,
        Err(error) => panic!("unexpected database probe failure: {error}"),
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_restore_child(root: &Path, task_id: &str, complete: bool) -> ChildGuard {
    let child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("crash_child_restore_http")
        .arg("--nocapture")
        .env("NHC_CRASH_ROOT", root)
        .env("NHC_CRASH_TASK", task_id)
        .env(
            "NHC_CRASH_RESTORE_COMPLETE",
            if complete { "1" } else { "0" },
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    ChildGuard(child)
}

fn spawn_write_child(root: &Path) -> ChildGuard {
    let child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("crash_child_write")
        .arg("--nocapture")
        .env("NHC_CRASH_WRITE_ROOT", root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    ChildGuard(child)
}

#[tokio::test]
async fn crash_child_write() {
    let Ok(root) = std::env::var("NHC_CRASH_WRITE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let base = mock_provider(true).await;
    let engine = open_engine(&root);
    engine
        .store
        .save_settings(&Settings {
            workspace: root.to_string_lossy().into_owned(),
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
    engine.store.put_secret("route", "fake-test-key").unwrap();
    let task = engine
        .start(RunRequest {
            title: "crash".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "worker".into(),
                role: "worker".into(),
                route_id: "route".into(),
                prompt: "write".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 4,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    std::fs::write(root.join("child-task-id"), &task.id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(approval) = engine
            .store
            .pending_approvals_for_task(&task.id)
            .unwrap()
            .into_iter()
            .next()
        {
            engine
                .decide_approval(&approval.id, true, Some("test"))
                .unwrap();
            break;
        }
        assert!(Instant::now() < deadline, "approval was not reached");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn crash_child_restore_http() {
    let Ok(root) = std::env::var("NHC_CRASH_ROOT") else {
        return;
    };
    let task_id = std::env::var("NHC_CRASH_TASK").unwrap();
    let root = PathBuf::from(root);
    let engine = open_engine(&root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = server::router(App {
        engine,
        token: TOKEN.into(),
        origin: origin.clone(),
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("{origin}/api/tasks/{task_id}/restore");
    let response = client
        .post(url)
        .header("x-peachsh-token", TOKEN)
        .header("origin", &origin)
        .send()
        .await;
    if std::env::var("NHC_CRASH_RESTORE_COMPLETE").as_deref() == Ok("1") {
        assert!(response.unwrap().status().is_success());
        std::fs::write(root.join("restore-http-received"), b"200").unwrap();
        std::future::pending::<()>().await;
        return;
    }
    panic!("HTTP restore returned before crash: {response:?}");
}

#[tokio::test]
async fn restore_http_crash_after_file_before_outcome_commit_is_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let task_id = written_task(root).await;
    let db_path = root.join("workspace.db");
    install_outcome_barrier(&db_path);
    let mut child = spawn_restore_child(root, &task_id, false);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let restored = std::fs::read(root.join("src/a.txt")).unwrap() == BEFORE.as_bytes();
        if restored && writer_active(&db_path) {
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "restore child exited early"
        );
        assert!(
            Instant::now() < deadline,
            "restore outcome window not reached"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let db = Connection::open(&db_path).unwrap();
    let (restore_id, status): (String, String) = db
        .query_row(
            "SELECT id,status FROM workspace_restores WHERE task_id=?1",
            [&task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "claimed");
    child.0.kill().unwrap();
    assert!(
        !child.0.wait().unwrap().success(),
        "child was not terminated"
    );
    let engine = open_engine(root);
    let receipt = engine.latest_restore(&task_id).unwrap().unwrap();
    assert_eq!(receipt.restore_id.as_deref(), Some(restore_id.as_str()));
    assert_eq!(receipt.status, RestoreStatus::Unknown);
    assert_eq!(receipt.restored, 0);
    assert_eq!(
        std::fs::read(root.join("src/a.txt")).unwrap(),
        BEFORE.as_bytes()
    );
    let (origin, server) = http_host(engine.clone()).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let get: Value = client
        .get(format!("{origin}/api/tasks/{task_id}/changes"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(get["restore"]["restore_id"], restore_id);
    assert_eq!(get["restore"]["status"], "unknown");
    let retry = client
        .post(format!("{origin}/api/tasks/{task_id}/restore"))
        .header("x-peachsh-token", TOKEN)
        .header("origin", &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(retry.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = retry.json().await.unwrap();
    assert_eq!(body["receipt"]["restore_id"], restore_id);
    assert_eq!(body["receipt"]["status"], "unknown");
    assert_eq!(body["ok"], false);
    assert!(!body.to_string().contains(BEFORE));
    server.abort();
    assert!(engine.restore(&task_id).await.is_err());
    assert_eq!(engine.latest_restore(&task_id).unwrap().unwrap(), receipt);
    assert_eq!(
        std::fs::read(root.join("src/a.txt")).unwrap(),
        BEFORE.as_bytes()
    );
}

#[tokio::test]
async fn restore_http_crash_after_complete_commit_keeps_stable_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let task_id = written_task(root).await;
    let db_path = root.join("workspace.db");
    let mut child = spawn_restore_child(root, &task_id, true);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let db = Connection::open(&db_path).unwrap();
        let complete: Option<String> = db
            .query_row(
                "SELECT id FROM workspace_restores WHERE task_id=?1 AND status='complete'",
                [&task_id],
                |row| row.get(0),
            )
            .ok();
        if complete.is_some() && root.join("restore-http-received").exists() {
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "restore child exited early"
        );
        assert!(
            Instant::now() < deadline,
            "complete restore window not reached"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    child.0.kill().unwrap();
    assert!(
        !child.0.wait().unwrap().success(),
        "child was not terminated"
    );
    let engine = open_engine(root);
    let receipt = engine.latest_restore(&task_id).unwrap().unwrap();
    assert_eq!(receipt.status, RestoreStatus::Complete);
    assert_eq!(receipt.restored, 1);
    assert!(receipt.restore_id.is_some());
    std::fs::write(root.join("src/a.txt"), "external-after-restore").unwrap();
    assert_eq!(engine.restore(&task_id).await.unwrap(), receipt);
    assert_eq!(
        std::fs::read_to_string(root.join("src/a.txt")).unwrap(),
        "external-after-restore"
    );
}

async fn write_crash_case(block_finish: bool) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.txt"), BEFORE).unwrap();
    let db_path = root.join("workspace.db");
    drop(Store::open(&db_path).unwrap());
    if block_finish {
        install_write_finish_barrier(&db_path);
    }
    let mut child = spawn_write_child(root);
    let deadline = Instant::now() + Duration::from_secs(20);
    let task_id = loop {
        if let Ok(id) = std::fs::read_to_string(root.join("child-task-id")) {
            break id;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "write child exited early"
        );
        assert!(Instant::now() < deadline, "child task ID not published");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    loop {
        let written = std::fs::read(root.join("src/a.txt")).unwrap() == AFTER.as_bytes();
        if written {
            let db = Connection::open(&db_path).unwrap();
            let state: Option<String> = db
                .query_row(
                    "SELECT state FROM workspace_changes WHERE task_id=?1",
                    [&task_id],
                    |row| row.get(0),
                )
                .ok();
            let at_boundary = if block_finish {
                state.as_deref() == Some("prepared") && writer_active(&db_path)
            } else {
                state.as_deref() == Some("finished")
            };
            if at_boundary {
                break;
            }
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "write child exited early"
        );
        assert!(Instant::now() < deadline, "write finish window not reached");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    child.0.kill().unwrap();
    assert!(
        !child.0.wait().unwrap().success(),
        "child was not terminated"
    );
    let engine = open_engine(root);
    let changes = engine.changes(&task_id).unwrap();
    assert_eq!(changes.len(), 1);
    let expected_state = if block_finish { "unknown" } else { "finished" };
    let db = Connection::open(&db_path).unwrap();
    let state: String = db
        .query_row(
            "SELECT state FROM workspace_changes WHERE task_id=?1",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, expected_state);
    assert_eq!(
        std::fs::read(root.join("src/a.txt")).unwrap(),
        AFTER.as_bytes()
    );
    if block_finish {
        assert!(!changes[0].restorable);
        assert!(engine.restore(&task_id).await.is_err());
    } else {
        assert!(changes[0].restorable);
    }
    assert_eq!(
        std::fs::read(root.join("src/a.txt")).unwrap(),
        AFTER.as_bytes()
    );
}

#[tokio::test]
async fn write_crash_after_file_before_finish_commit_is_unknown() {
    write_crash_case(true).await;
}

#[tokio::test]
async fn write_crash_after_finish_commit_stays_finished() {
    write_crash_case(false).await;
}
