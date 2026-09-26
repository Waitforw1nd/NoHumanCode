use axum::{Json, Router, extract::State, routing::post};
use peachsh::checkpoint::CheckpointError;
use peachsh::{
    domain::{Route, RunRequest, SessionKind, Settings, TaskSpec},
    engine::Engine,
    store::Store,
    workspace_changes::RestoreStatus,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

type Script = Arc<Mutex<VecDeque<Value>>>;

async fn response(
    State(script): State<Script>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let delta = script
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| json!({"content":"done"}));
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

async fn server(path: &str, content: &str) -> (String, tokio::task::JoinHandle<()>) {
    let mut replies = VecDeque::new();
    if !path.is_empty() {
        replies.push_back(json!({"tool_calls":[{"index":0,"id":"write-1","type":"function","function":{"name":"write_file","arguments":json!({"path":path,"content":content}).to_string()}}]}));
    }
    replies.push_back(json!({"content":"done"}));
    let script = Arc::new(Mutex::new(replies));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(response))
        .with_state(script);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, handle)
}

async fn scripted_server(replies: Vec<Value>) -> (String, tokio::task::JoinHandle<()>) {
    let script = Arc::new(Mutex::new(VecDeque::from(replies)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(response))
        .with_state(script);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, handle)
}

fn write_delta(id: &str, path: &str, content: &str) -> Value {
    json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":"write_file","arguments":json!({"path":path,"content":content}).to_string()}}]})
}

fn setup(root: &Path, base: &str) -> Arc<Engine> {
    let store = Arc::new(Store::open(&root.join("workspace.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: root.to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "Route".into(),
                base_url: base.into(),
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "fake-workspace-key").unwrap();
    Engine::new(store, 2).unwrap()
}

async fn write_once(engine: &Arc<Engine>) -> String {
    let task = engine
        .start(RunRequest {
            title: "changes".into(),
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
    let approval = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(value) = engine
                .store
                .pending_approvals_for_task(&task.id)
                .unwrap()
                .into_iter()
                .next()
            {
                break value;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    engine
        .decide_approval(&approval.id, true, Some("test"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let value = engine.store.task(&task.id).unwrap();
            if !matches!(value.status.as_str(), "queued" | "running") && !engine.is_busy() {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    task.id
}

async fn complete_script(engine: &Arc<Engine>, count: usize) -> String {
    let task = engine
        .start(RunRequest {
            title: "checkpoint".into(),
            kind: SessionKind::Chat,
            tasks: vec![TaskSpec {
                name: "writer".into(),
                role: "writer".into(),
                route_id: "route".into(),
                prompt: "write".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 30,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    for _ in 0..count {
        let pending = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(a) = engine
                    .store
                    .pending_approvals_for_task(&task.id)
                    .unwrap()
                    .into_iter()
                    .next()
                {
                    break a;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        engine
            .decide_approval(&pending.id, true, Some("test"))
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.store.task(&task.id).unwrap().status, "completed");
    task.id
}

#[tokio::test]
async fn k01_exact_task_before_survives_reopen_and_restore_converges() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), b"original\r\nno newline").unwrap();
    std::fs::write(temp.path().join("outside.txt"), b"untouched").unwrap();
    git(temp.path(), &["init", "-q"]);
    git(temp.path(), &["add", "--", "outside.txt"]);
    let git_index = std::fs::read(temp.path().join(".git/index")).unwrap();
    let git_head = std::fs::read(temp.path().join(".git/HEAD")).unwrap();
    let (base, server) = scripted_server(vec![
        write_delta("w1", "src/a.txt", "middle"),
        write_delta("w2", "src/new.txt", "created"),
        write_delta("w3", "src/a.txt", "final"),
        json!({"content":"done"}),
    ])
    .await;
    let engine = setup(temp.path(), &base);
    let task = complete_script(&engine, 3).await;
    let created = engine
        .create_checkpoint(&task, "checkpoint-one")
        .await
        .unwrap();
    assert!(!created.replayed);
    assert_eq!(created.checkpoint.kind, "task_before");
    assert_eq!(created.checkpoint.entries.len(), 2);
    assert_eq!(
        engine
            .create_checkpoint(&task, "checkpoint-one")
            .await
            .unwrap()
            .checkpoint,
        created.checkpoint
    );
    assert!(matches!(
        engine
            .create_checkpoint(&task, "different")
            .await
            .unwrap_err()
            .downcast_ref(),
        Some(CheckpointError::Conflict)
    ));
    assert!(engine.resume(&task, "must not resume").await.is_err());
    drop(engine);
    let engine = Engine::new(
        Arc::new(Store::open(&temp.path().join("workspace.db")).unwrap()),
        2,
    )
    .unwrap();
    assert_eq!(
        engine
            .checkpoint(&created.checkpoint.checkpoint_id)
            .unwrap(),
        created.checkpoint
    );
    let (a, b) = tokio::join!(
        engine.restore(&task),
        engine.restore_checkpoint(&created.checkpoint.checkpoint_id)
    );
    let receipt = a.unwrap();
    assert_eq!(receipt, b.unwrap());
    assert_eq!(receipt.status, RestoreStatus::Complete);
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"original\r\nno newline"
    );
    assert!(!temp.path().join("src/new.txt").exists());
    assert_eq!(
        std::fs::read(temp.path().join("outside.txt")).unwrap(),
        b"untouched"
    );
    std::fs::write(temp.path().join("src/a.txt"), "later user edit").unwrap();
    assert_eq!(
        engine
            .restore_checkpoint(&created.checkpoint.checkpoint_id)
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "later user edit"
    );
    assert!(
        engine
            .create_checkpoint(&task, "checkpoint-one")
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(
        std::fs::read(temp.path().join(".git/index")).unwrap(),
        git_index
    );
    assert_eq!(
        std::fs::read(temp.path().join(".git/HEAD")).unwrap(),
        git_head
    );
    let db = Connection::open(temp.path().join("workspace.db")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM events WHERE kind='checkpoint.created'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM workspace_restores", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn k02_concurrent_creation_transaction_failure_and_no_secret_disclosure() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(
        temp.path().join("src/a.txt"),
        "api_key=hidden-before-secret",
    )
    .unwrap();
    let (base, server) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    let db = Connection::open(temp.path().join("workspace.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_checkpoint BEFORE INSERT ON checkpoints BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    assert!(engine.create_checkpoint(&task, "key-one").await.is_err());
    assert_eq!(
        db.query_row("SELECT count(*) FROM checkpoints", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM events WHERE kind='checkpoint.created'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TRIGGER reject_checkpoint;").unwrap();
    let (a, b) = tokio::join!(
        engine.create_checkpoint(&task, "key-one"),
        engine.create_checkpoint(&task, "key-one")
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.replayed, b.replayed);
    assert_eq!(a.checkpoint, b.checkpoint);
    assert!(!serde_json::to_string(&a).unwrap().contains("hidden-before"));
    let events: String = db
        .query_row("SELECT group_concat(data) FROM events", [], |r| r.get(0))
        .unwrap();
    assert!(!events.contains("hidden-before"));
    server.abort();
}

#[tokio::test]
async fn k03_external_edits_corrupt_sources_and_manifest_fail_closed() {
    for mutation in [
        "external", "blob", "manifest", "source", "scope", "pending", "legacy",
    ] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
        let (base, server) = server("src/a.txt", "after").await;
        let engine = setup(temp.path(), &base);
        let task = write_once(&engine).await;
        let db = Connection::open(temp.path().join("workspace.db")).unwrap();
        let checkpoint = if matches!(mutation, "manifest" | "source" | "scope") {
            Some(
                engine
                    .create_checkpoint(&task, "key")
                    .await
                    .unwrap()
                    .checkpoint,
            )
        } else {
            None
        };
        match mutation {
            "external" => std::fs::write(temp.path().join("src/a.txt"), "external").unwrap(),
            "blob" => {
                db.execute("UPDATE workspace_changes SET before_blob=x'00'", [])
                    .unwrap();
            }
            "manifest" => {
                db.execute("UPDATE checkpoints SET manifest='{}'", [])
                    .unwrap();
            }
            "source" => {
                db.execute(
                    "UPDATE workspace_changes SET after_digest=?1",
                    ["0".repeat(64)],
                )
                .unwrap();
            }
            "scope" => {
                db.execute("UPDATE tasks SET value=json_set(value,'$.spec.write_scopes',json('[\"other\"]'))",[]).unwrap();
            }
            "pending" => {
                db.execute("UPDATE workspace_changes SET state='unknown'", [])
                    .unwrap();
            }
            "legacy" => {
                db.execute(
                    "INSERT INTO events(task_id,kind,data,at) VALUES (?1,'file_backup','{}',1)",
                    [&task],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let before = std::fs::read(temp.path().join("src/a.txt")).unwrap();
        if let Some(cp) = checkpoint {
            assert_eq!(
                engine
                    .checkpoint(&cp.checkpoint_id)
                    .unwrap_err()
                    .downcast_ref::<CheckpointError>(),
                Some(&CheckpointError::Corrupt),
                "{mutation}"
            );
            assert_eq!(
                engine
                    .restore_checkpoint(&cp.checkpoint_id)
                    .await
                    .unwrap_err()
                    .downcast_ref::<CheckpointError>(),
                Some(&CheckpointError::Corrupt),
                "{mutation}"
            );
            assert_eq!(
                engine
                    .restore(&task)
                    .await
                    .unwrap_err()
                    .downcast_ref::<CheckpointError>(),
                Some(&CheckpointError::Corrupt),
                "compatibility restore: {mutation}"
            );
        } else {
            let expected = match mutation {
                "external" => CheckpointError::Conflict,
                "blob" => CheckpointError::Corrupt,
                _ => CheckpointError::Unrestorable,
            };
            assert_eq!(
                engine
                    .create_checkpoint(&task, "key")
                    .await
                    .unwrap_err()
                    .downcast_ref::<CheckpointError>(),
                Some(&expected),
                "{mutation}"
            );
        }
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            before
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM workspace_restores", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        server.abort();
    }
}

#[test]
fn k05_schema_6_7_8_migration_preserves_rows_and_rejects_fake_constraints() {
    for version in [6, 7, 8] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db.sqlite");
        drop(Store::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        db.execute_batch("INSERT INTO config VALUES ('sentinel','original-bytes'); DROP TABLE checkpoints; DELETE FROM schema_migrations WHERE id='task-before-checkpoint-repository';").unwrap();
        if version < 8 {
            db.execute_batch("DROP TABLE workspace_restore_outcomes; DROP TABLE workspace_restores; DROP TABLE workspace_changes; DELETE FROM schema_migrations WHERE id='workspace-change-repository';").unwrap();
        }
        if version < 7 {
            db.execute_batch("DROP TABLE approvals; DELETE FROM schema_migrations WHERE id='tool-call-approval-repository';").unwrap();
        }
        db.pragma_update(None, "user_version", version).unwrap();
        drop(db);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 9);
        drop(store);
        drop(Store::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT value FROM config WHERE id='sentinel'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "original-bytes"
        );
        db.execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_master SET sql=replace(sql,'CHECK (generation=1)','CHECK (generation>=1)') WHERE name='checkpoints'; PRAGMA writable_schema=OFF;").unwrap();
        drop(db);
        assert!(Store::open(&path).is_err());
    }
}

#[tokio::test]
async fn k02_distinct_tasks_can_reuse_creation_key() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    let (base, server) = scripted_server(vec![
        write_delta("w1", "src/a.txt", "first"),
        json!({"content":"done"}),
        write_delta("w2", "src/b.txt", "second"),
        json!({"content":"done"}),
    ])
    .await;
    let engine = setup(temp.path(), &base);
    let first = write_once(&engine).await;
    let a = engine.create_checkpoint(&first, "same-key").await.unwrap();
    let second = write_once(&engine).await;
    let b = engine.create_checkpoint(&second, "same-key").await.unwrap();
    assert_ne!(a.checkpoint.task_id, b.checkpoint.task_id);
    assert_ne!(a.checkpoint.checkpoint_id, b.checkpoint.checkpoint_id);
    server.abort();
}

#[test]
fn k05_fake_literal_and_missing_constraint_are_rejected() {
    for (original, replacement) in [
        ("kind='task_before'", "kind='task_ before'"),
        (" CHECK (generation=1)", ""),
        (" UNIQUE REFERENCES tasks(id)", " REFERENCES tasks(id)"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fake.db");
        drop(Store::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA writable_schema=ON;").unwrap();
        assert_eq!(
            db.execute(
                "UPDATE sqlite_master SET sql=replace(sql,?1,?2) WHERE name='checkpoints'",
                rusqlite::params![original, replacement]
            )
            .unwrap(),
            1
        );
        drop(db);
        assert!(Store::open(&path).is_err(), "{original}");
    }
}

fn business_rows(db: &Connection) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
    let mut names=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT IN ('checkpoints','schema_migrations','sqlite_sequence') ORDER BY name").unwrap();
    names
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|name| {
            let name = name.unwrap();
            let mut stmt = db
                .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))
                .unwrap();
            let count = stmt.column_count();
            let values = stmt
                .query_map([], |r| {
                    (0..count)
                        .map(|i| r.get(i))
                        .collect::<rusqlite::Result<Vec<rusqlite::types::Value>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (name, values)
        })
        .collect()
}

#[tokio::test]
async fn k05_real_schema8_migration_preserves_every_business_value_and_ciphertext() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "sensitive original bytes").unwrap();
    let (base, server) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    let receipt = engine.restore(&task).await.unwrap();
    assert_eq!(receipt.status, RestoreStatus::Complete);
    drop(engine);
    let path = temp.path().join("workspace.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE checkpoints; DELETE FROM schema_migrations WHERE id='task-before-checkpoint-repository'; PRAGMA user_version=8;").unwrap();
    let old_rows = business_rows(&db);
    let before_blob: Vec<u8> = db
        .query_row("SELECT before_blob FROM workspace_changes", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!before_blob.is_empty());
    drop(db);
    drop(Store::open(&path).unwrap());
    drop(Store::open(&path).unwrap());
    let db = Connection::open(&path).unwrap();
    assert_eq!(business_rows(&db), old_rows);
    assert_eq!(
        db.query_row("SELECT before_blob FROM workspace_changes", [], |r| r
            .get::<_, Vec<u8>>(0))
            .unwrap(),
        before_blob
    );
    server.abort();
}

#[test]
fn k05_version9_without_marker_is_not_silently_repaired() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("fake.db");
    drop(Store::open(&path).unwrap());
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE checkpoints; DELETE FROM schema_migrations WHERE id='task-before-checkpoint-repository';").unwrap();
    drop(db);
    assert!(Store::open(&path).is_err());
}

#[tokio::test]
async fn k03_path_and_before_total_limits_leave_zero_new_facts() {
    for (count, before_size) in [(129, 0), (33, 262144)] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        let calls:Vec<_>=(0..count).map(|i| {
            let path=format!("src/file-{i}.txt");
            if before_size>0 {std::fs::write(temp.path().join(&path),vec![b'x';before_size]).unwrap();}
            json!({"index":i,"id":format!("write-{i}"),"type":"function","function":{"name":"write_file","arguments":json!({"path":path,"content":"after"}).to_string()}})
        }).collect();
        let mut replies: Vec<_> = calls
            .chunks(32)
            .map(|chunk| {
                let calls: Vec<_> = chunk
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let mut c = c.clone();
                        c["index"] = json!(i);
                        c
                    })
                    .collect();
                json!({"tool_calls":calls})
            })
            .collect();
        replies.push(json!({"content":"done"}));
        let (base, server) = scripted_server(replies).await;
        let engine = setup(temp.path(), &base);
        let task = complete_script(&engine, count).await;
        let db = Connection::open(temp.path().join("workspace.db")).unwrap();
        let events: i64 = db
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            engine
                .create_checkpoint(&task, "limits")
                .await
                .unwrap_err()
                .downcast_ref::<CheckpointError>(),
            Some(&CheckpointError::Unrestorable)
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            events
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM checkpoints", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        server.abort();
    }
}

#[tokio::test]
async fn k04_multiple_paths_preflight_fails_before_any_write() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    for p in ["a", "b"] {
        std::fs::write(
            temp.path().join(format!("src/{p}.txt")),
            format!("before-{p}"),
        )
        .unwrap();
    }
    let (base, server) = scripted_server(vec![
        write_delta("a", "src/a.txt", "after-a"),
        write_delta("b", "src/b.txt", "after-b"),
        json!({"content":"done"}),
    ])
    .await;
    let engine = setup(temp.path(), &base);
    let task = complete_script(&engine, 2).await;
    let cp = engine
        .create_checkpoint(&task, "multi")
        .await
        .unwrap()
        .checkpoint;
    std::fs::write(temp.path().join("src/b.txt"), "external").unwrap();
    assert!(matches!(
        engine
            .restore_checkpoint(&cp.checkpoint_id)
            .await
            .unwrap_err()
            .downcast_ref::<peachsh::workspace_changes::WorkspaceChangeError>(),
        Some(peachsh::workspace_changes::WorkspaceChangeError::Conflict { receipt: None })
    ));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "after-a"
    );
    assert!(engine.latest_restore(&task).unwrap().is_none());
    server.abort();
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git fixture failed");
}

#[tokio::test]
async fn engine_git_diff_uses_real_index_without_tool_changes_and_checks_task_binding() {
    use peachsh::git_diff::{GitDiffRequest, GitDiffStatus, GitDiffView};
    use peachsh::workspace_changes::WorkspaceChangeError;
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    git(temp.path(), &["init", "-q"]);
    std::fs::write(temp.path().join("src/review.txt"), "index\n").unwrap();
    git(temp.path(), &["add", "--", "src/review.txt"]);
    std::fs::write(temp.path().join("src/review.txt"), "disk\n").unwrap();
    let (base, server) = scripted_server(vec![json!({"content":"done"})]).await;
    let engine = setup(temp.path(), &base);
    let task = complete_script(&engine, 0).await;
    assert!(engine.changes(&task).unwrap().is_empty());
    for (view, needle) in [
        (GitDiffView::Staged, "+index"),
        (GitDiffView::Unstaged, "-index"),
        (GitDiffView::Head, "+disk"),
    ] {
        let diff = engine
            .git_diff(
                &task,
                GitDiffRequest {
                    path: "src/review.txt".into(),
                    view,
                },
            )
            .await
            .unwrap();
        assert_eq!(diff.status, GitDiffStatus::Text);
        assert!(diff.patch.unwrap().contains(needle));
    }
    let request = GitDiffRequest {
        path: "src/review.txt".into(),
        view: GitDiffView::Head,
    };
    assert!(matches!(
        engine
            .git_diff("missing", request.clone())
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::NotFound)
    ));
    assert_eq!(
        engine
            .create_checkpoint(&task, "empty")
            .await
            .unwrap_err()
            .downcast_ref::<CheckpointError>(),
        Some(&CheckpointError::Unrestorable)
    );
    assert_eq!(
        engine
            .create_checkpoint(&task, "bad key")
            .await
            .unwrap_err()
            .downcast_ref::<CheckpointError>(),
        Some(&CheckpointError::Invalid)
    );
    let other = tempfile::tempdir().unwrap();
    let mut settings = engine.store.settings().unwrap().unwrap();
    settings.workspace = other.path().to_string_lossy().into();
    engine.store.save_settings(&settings).unwrap();
    assert!(matches!(
        engine
            .git_diff(&task, request)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Conflict { receipt: None })
    ));
    server.abort();
}
