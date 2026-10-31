#[allow(dead_code)]
#[path = "support/network_fixture.rs"]
mod network_fixture;
use axum::{Json, Router, extract::State, routing::post};
use peachsh::{
    domain::{Route, RunRequest, SessionKind, Settings, TaskSpec},
    engine::Engine,
    server::{self, App},
    store::Store,
    workspace_changes::RestoreStatus,
};
use rusqlite::{Connection, ErrorCode, OptionalExtension};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs::File,
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
    network_fixture::authorize_routes(&engine.store);
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
        .decide_approval_with_network_ack(
            &approval.id,
            true,
            Some("test"),
            approval.tool_name == "run_command",
        )
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
    let stdout = File::create(root.join("write-child.stdout")).unwrap();
    let stderr = File::create(root.join("write-child.stderr")).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("crash_child_write")
        .arg("--nocapture")
        .env("NHC_CRASH_WRITE_ROOT", root)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();
    ChildGuard(child)
}

struct WriteEvidence {
    child: ChildGuard,
    root: PathBuf,
    task_id: Option<String>,
    target: &'static str,
    captured: bool,
}

impl WriteEvidence {
    fn capture(&mut self, phase: &str) -> anyhow::Result<PathBuf> {
        let pid = self.child.0.id();
        let task_id = self
            .task_id
            .clone()
            .or_else(|| std::fs::read_to_string(self.root.join("child-task-id")).ok());
        let evidence = self.root.join("write-evidence");
        let raw = evidence.join("raw");
        let query_source = evidence.join("query-source");
        let recovery = evidence.join("recovery");
        for path in [&raw, &query_source, &recovery] {
            std::fs::create_dir_all(path)?;
        }
        let mut process = json!({
            "phase": phase,
            "target": self.target,
            "pid": pid,
            "task_id": task_id.as_deref(),
            "fixture_root": &self.root,
            "try_wait": "not-attempted",
            "kill": "not-attempted",
            "wait": "not-attempted",
            "reaped": false,
        });
        let save_process = |record: &Value| -> anyhow::Result<()> {
            std::fs::write(
                evidence.join("process.json"),
                serde_json::to_vec_pretty(record)?,
            )?;
            Ok(())
        };
        save_process(&process)?;
        self.captured = true;
        let status = match self.child.0.try_wait() {
            Ok(Some(status)) => {
                process["try_wait"] = json!("already-exited");
                process["kill"] = json!("not-needed");
                process["wait"] = json!(format!("{status:?}"));
                process["reaped"] = json!(true);
                status
            }
            Ok(None) => {
                process["try_wait"] = json!("running");
                match self.child.0.kill() {
                    Ok(()) => process["kill"] = json!("killed"),
                    Err(error) => {
                        process["kill"] = json!(format!("error: {error}"));
                        save_process(&process)?;
                        return Err(error.into());
                    }
                }
                save_process(&process)?;
                match self.child.0.wait() {
                    Ok(status) => {
                        process["wait"] = json!(format!("{status:?}"));
                        process["reaped"] = json!(true);
                        status
                    }
                    Err(error) => {
                        process["wait"] = json!(format!("error: {error}"));
                        save_process(&process)?;
                        return Err(error.into());
                    }
                }
            }
            Err(error) => {
                process["try_wait"] = json!(format!("error: {error}"));
                save_process(&process)?;
                return Err(error.into());
            }
        };
        let task_id =
            task_id.or_else(|| std::fs::read_to_string(self.root.join("child-task-id")).ok());
        process["task_id"] = json!(task_id.as_deref());
        process["success"] = json!(status.success());
        save_process(&process)?;
        let mut raw_files = Vec::new();
        for name in ["workspace.db", "workspace.db-wal", "workspace.db-shm"] {
            let source = self.root.join(name);
            if source.exists() {
                let source_bytes = std::fs::read(&source)?;
                std::fs::copy(&source, raw.join(name))?;
                let bytes = std::fs::read(raw.join(name))?;
                anyhow::ensure!(bytes == source_bytes, "raw copy differs: {name}");
                use sha2::{Digest, Sha256};
                raw_files.push(json!({
                    "name": name,
                    "present": true,
                    "bytes": bytes.len(),
                    "sha256": format!("{:x}", Sha256::digest(&bytes)),
                }));
                std::fs::copy(raw.join(name), query_source.join(name))?;
                std::fs::copy(raw.join(name), recovery.join(name))?;
            } else {
                raw_files.push(json!({"name": name, "present": false}));
            }
        }
        std::fs::write(
            evidence.join("raw-manifest.json"),
            serde_json::to_vec_pretty(&raw_files)?,
        )?;
        anyhow::ensure!(raw.join("workspace.db").is_file(), "raw database missing");

        // SQLite may recover WAL when opened; only the copy is opened.
        let query_db = Connection::open(query_source.join("workspace.db"))?;
        let snapshot = evidence.join("queryable.db");
        query_db.execute("VACUUM INTO ?1", [snapshot.to_string_lossy().as_ref()])?;
        drop(query_db);
        let db =
            Connection::open_with_flags(&snapshot, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if let Some(id) = task_id.as_deref() {
            let task: Option<String> = db
                .query_row("SELECT value FROM tasks WHERE id=?1", [id], |r| r.get(0))
                .optional()?;
            let mut events =
                db.prepare("SELECT kind,data FROM events WHERE task_id=?1 ORDER BY seq")?;
            let events = events
                .query_map([id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut approvals = db.prepare(
                "SELECT id,tool_call_id,tool_name,status,execution_state FROM approvals WHERE task_id=?1 ORDER BY rowid",
            )?;
            let approvals = approvals
                .query_map([id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut changes = db.prepare(
                "SELECT id,tool_call_id,path,state,restore_state FROM workspace_changes WHERE task_id=?1 ORDER BY rowid",
            )?;
            let changes = changes
                .query_map([id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut calls = db.prepare(
                "SELECT id,state,receipt FROM network_calls WHERE task_id=?1 ORDER BY rowid",
            )?;
            let calls = calls
                .query_map([id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            std::fs::write(
                evidence.join("observed.json"),
                serde_json::to_vec_pretty(&json!({
                    "task_id": id,
                    "task_value": task,
                    "events": events,
                    "approvals": approvals,
                    "changes": changes,
                    "network_calls": calls,
                    "file": std::fs::read(self.root.join("src/a.txt")).ok(),
                }))?,
            )?;
        }
        println!(
            "workspace-write-evidence phase={phase} pid={pid} root={} task={}",
            self.root.display(),
            task_id.as_deref().unwrap_or("<unpublished>")
        );
        Ok(recovery)
    }
}

impl Drop for WriteEvidence {
    fn drop(&mut self) {
        if !self.captured
            && let Err(error) = self.capture("early-exit-or-timeout")
        {
            eprintln!(
                "workspace-write-evidence failed root={} error={error:#}",
                self.root.display()
            );
        }
    }
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
    network_fixture::authorize_routes(&engine.store);
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
    std::fs::write(root.join("child-task-id.tmp"), &task.id).unwrap();
    std::fs::rename(root.join("child-task-id.tmp"), root.join("child-task-id")).unwrap();
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
                .decide_approval_with_network_ack(
                    &approval.id,
                    true,
                    Some("test"),
                    approval.tool_name == "run_command",
                )
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
    assert_eq!(receipt.outcomes.len(), 1);
    assert_eq!(receipt.outcomes[0].status, RestoreStatus::Unknown);
    assert_eq!(
        std::fs::read(root.join("src/a.txt")).unwrap(),
        BEFORE.as_bytes()
    );
    let changes = engine.changes(&task_id).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].restore_state, "unknown");
    assert!(!changes[0].restorable);
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
    assert_eq!(receipt.outcomes.len(), 1);
    assert_eq!(receipt.outcomes[0].status, RestoreStatus::Complete);
    std::fs::write(root.join("src/a.txt"), "external-after-restore").unwrap();
    let (origin, server) = http_host(engine.clone()).await;
    let repeat = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{origin}/api/tasks/{task_id}/restore"))
        .header("x-peachsh-token", TOKEN)
        .header("origin", &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(repeat.status(), reqwest::StatusCode::OK);
    let repeated: Value = repeat.json().await.unwrap();
    assert_eq!(repeated["receipt"], serde_json::to_value(&receipt).unwrap());
    assert_eq!(repeated["ok"], true);
    server.abort();
    assert_eq!(
        std::fs::read_to_string(root.join("src/a.txt")).unwrap(),
        "external-after-restore"
    );
}

async fn write_crash_case(block_finish: bool) {
    let retained = tempfile::Builder::new()
        .prefix("workspace-write-crash-")
        .tempdir()
        .unwrap()
        .keep();
    let root = retained.as_path();
    println!(
        "workspace-write-fixture root={} block_finish={block_finish}",
        root.display()
    );
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.txt"), BEFORE).unwrap();
    let db_path = root.join("workspace.db");
    drop(Store::open(&db_path).unwrap());
    if block_finish {
        install_write_finish_barrier(&db_path);
    }
    let mut evidence = WriteEvidence {
        child: spawn_write_child(root),
        root: root.to_owned(),
        task_id: None,
        target: if block_finish {
            "file-before-finish"
        } else {
            "finished-after-commit"
        },
        captured: false,
    };
    println!(
        "workspace-write-child root={} pid={}",
        root.display(),
        evidence.child.0.id()
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let task_id = loop {
        if let Ok(id) = std::fs::read_to_string(root.join("child-task-id")) {
            let known = Connection::open(&db_path)
                .unwrap()
                .query_row("SELECT count(*) FROM tasks WHERE id=?1", [&id], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap_or(0);
            if !id.is_empty() && known == 1 {
                break id;
            }
        }
        assert!(
            evidence.child.0.try_wait().unwrap().is_none(),
            "write child exited early: {}",
            std::fs::read_to_string(root.join("write-child.stderr")).unwrap()
        );
        assert!(Instant::now() < deadline, "child task ID not published");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    println!(
        "workspace-write-task root={} task_id={task_id}",
        root.display()
    );
    evidence.task_id = Some(task_id.clone());
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
            evidence.child.0.try_wait().unwrap().is_none(),
            "write child exited early: {}",
            std::fs::read_to_string(root.join("write-child.stderr")).unwrap()
        );
        if Instant::now() >= deadline {
            let db = Connection::open(&db_path).unwrap();
            let state: Option<String> = db
                .query_row(
                    "SELECT state FROM workspace_changes WHERE task_id=?1",
                    [&task_id],
                    |row| row.get(0),
                )
                .ok();
            panic!(
                "write finish window not reached: file={:?}, state={state:?}, stderr={}",
                std::fs::read(root.join("src/a.txt")).unwrap(),
                std::fs::read_to_string(root.join("write-child.stderr")).unwrap()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let recovery = evidence.capture("target-boundary").unwrap();
    let process: Value =
        serde_json::from_slice(&std::fs::read(root.join("write-evidence/process.json")).unwrap())
            .unwrap();
    assert_eq!(process["kill"], "killed");
    assert_eq!(process["reaped"], true);
    assert_eq!(process["success"], false);
    let engine = open_engine(&recovery);
    let changes = engine.changes(&task_id).unwrap();
    assert_eq!(changes.len(), 1);
    let expected_state = if block_finish { "unknown" } else { "finished" };
    let db = Connection::open(recovery.join("workspace.db")).unwrap();
    let state: String = db
        .query_row(
            "SELECT state FROM workspace_changes WHERE task_id=?1",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, expected_state);
    let approval_state: String = db
        .query_row(
            "SELECT execution_state FROM approvals WHERE task_id=?1",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(approval_state, expected_state);
    let tool_messages = engine
        .store
        .task(&task_id)
        .unwrap()
        .messages
        .into_iter()
        .filter(|message| message["role"] == "tool")
        .count();
    let tool_results: i64 = db
        .query_row(
            "SELECT count(*) FROM events WHERE task_id=?1 AND kind='tool_result'",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    let finished_events = {
        let mut stmt = db
            .prepare("SELECT data FROM events WHERE task_id=?1 AND kind='file_backup'")
            .unwrap();
        stmt.query_map([&task_id], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|row| serde_json::from_str::<Value>(&row.unwrap()).unwrap())
            .filter(|event| event["state"] == "finished")
            .count()
    };
    let expected_count = usize::from(!block_finish);
    assert_eq!(tool_messages, expected_count);
    assert_eq!(tool_results, expected_count as i64);
    assert_eq!(finished_events, expected_count);
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
