#[allow(dead_code)]
#[path = "support/network_fixture.rs"]
mod network_fixture;
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

#[tokio::test]
async fn shutdown_rejects_new_checkpoint_without_changing_records_or_files() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "initial").unwrap();
    let (base, server) = scripted_server(vec![
        write_delta("u18-first", "src/a.txt", "first"),
        json!({"content":"done"}),
        write_delta("u18-second", "src/a.txt", "second"),
        json!({"content":"done"}),
    ])
    .await;
    let engine = setup(temp.path(), &base);
    let first = write_once(&engine).await;
    let checkpoint = engine
        .create_checkpoint(&first, "u18-positive")
        .await
        .unwrap()
        .checkpoint;
    let second = write_once(&engine).await;
    let db = Connection::open(temp.path().join("workspace.db")).unwrap();
    let before: i64 = db
        .query_row("SELECT count(*) FROM checkpoints", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, 1);
    let file_before = std::fs::read(temp.path().join("src/a.txt")).unwrap();
    assert_eq!(file_before, b"second");
    assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
    assert!(
        engine
            .create_checkpoint(&second, "u18-closed")
            .await
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM checkpoints", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        engine.checkpoint(&checkpoint.checkpoint_id).unwrap(),
        checkpoint
    );
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        file_before
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn audit_reversed_clock_keeps_three_writes_and_checkpoint_causal() {
    use sha2::Digest;
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "first").unwrap();
    let (base, server) = scripted_server(vec![
        write_delta("audit-w1", "src/a.txt", "second"),
        write_delta("audit-w2", "src/a.txt", "third"),
        write_delta("audit-w3", "src/a.txt", "fourth"),
        json!({"content":"done"}),
    ])
    .await;
    let engine = setup(temp.path(), &base);
    let db = Connection::open(temp.path().join("workspace.db")).unwrap();
    // Model a backwards wall clock at the actual durable prepare boundary;
    // all writes still execute through approvals and the real file service.
    db.execute_batch("CREATE TRIGGER audit_reverse_clock AFTER INSERT ON workspace_changes BEGIN UPDATE workspace_changes SET created_at=1000-(SELECT count(*) FROM workspace_changes) WHERE id=NEW.id; END;").unwrap();
    let task = complete_script(&engine, 3).await;
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"fourth"
    );
    let checkpoint = engine
        .create_checkpoint(&task, "audit-clock")
        .await
        .unwrap()
        .checkpoint;
    assert_eq!(checkpoint.entries.len(), 1);
    assert_eq!(
        checkpoint.entries[0].before_digest,
        Some(format!("{:x}", sha2::Sha256::digest(b"first")))
    );
    assert_eq!(
        checkpoint.entries[0].after_digest,
        format!("{:x}", sha2::Sha256::digest(b"fourth"))
    );
    drop(db);
    drop(engine);
    let store = Arc::new(Store::open(&temp.path().join("workspace.db")).unwrap());
    let reopened = Engine::new(store, 2).unwrap();
    assert_eq!(
        reopened.checkpoint(&checkpoint.checkpoint_id).unwrap(),
        checkpoint
    );
    assert_eq!(
        reopened
            .restore_checkpoint(&checkpoint.checkpoint_id)
            .await
            .unwrap()
            .status,
        RestoreStatus::Complete
    );
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"first"
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cross_project_restore_keeps_original_root_and_waits_for_host_command() {
    use peachsh::workspace_changes::WorkspaceChangeError;
    use peachsh::{
        domain::{RunRequest, SessionKind, TaskSpec, id},
        network_authorization::*,
        project_config::*,
    };
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    std::fs::create_dir(a.path().join("src")).unwrap();
    std::fs::create_dir(b.path().join("src")).unwrap();
    std::fs::write(a.path().join("src/a.txt"), "original-a").unwrap();
    std::fs::write(b.path().join("src/a.txt"), "b-sentinel").unwrap();
    let command = "Set-Content -LiteralPath command-started.txt -Value running; while (-not (Test-Path -LiteralPath command-release.txt)) { Start-Sleep -Milliseconds 10 }; Write-Output done";
    let (base,server)=scripted_server(vec![write_delta("a-write","src/a.txt","after-a"),json!({"content":"done"}),json!({"tool_calls":[{"index":0,"id":"command","type":"function","function":{"name":"run_command","arguments":json!({"command":command}).to_string()}}]}),json!({"content":"done"})]).await;
    let engine = setup(a.path(), &base);
    let task = write_once(&engine).await;
    let cp = engine
        .create_checkpoint(&task, "a-checkpoint")
        .await
        .unwrap()
        .checkpoint;
    let project = engine
        .register_project(RegisterProject {
            name: "B".into(),
            root_path: b.path().to_string_lossy().into(),
            idempotency_key: id(),
        })
        .await
        .unwrap();
    engine
        .configure_project(ConfigureProject {
            project_id: project.id.clone(),
            expected_revision: 1,
            config: ProjectConfig {
                allowed_route_ids: vec!["route".into()],
                default_route_id: Some("route".into()),
                tools: true,
                write_scopes: vec!["src".into()],
                allow_commands: true,
            },
        })
        .await
        .unwrap();
    let selection = engine.current_project().unwrap();
    engine
        .select_project(SelectProject {
            project_id: project.id.clone(),
            expected_revision: selection.revision,
        })
        .await
        .unwrap();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(project.id.clone()),
    };
    let preview = engine.network_preview(subject.clone()).await.unwrap();
    engine
        .network_confirm(
            subject,
            ConfirmRequest {
                config_revision: preview.config_revision,
                credential_revision: preview.credential_revision,
                binding_digest: preview.binding_digest,
                purposes: vec![NetworkPurpose::Completion],
            },
            &id(),
        )
        .await
        .unwrap();
    let run = engine
        .start_in_project(StartInProject {
            project_id: project.id,
            expected_config_revision: 2,
            idempotency_key: id(),
            request: RunRequest {
                title: "command B".into(),
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "command-worker".into(),
                    role: "worker".into(),
                    route_id: "route".into(),
                    prompt: "run".into(),
                    depends_on: vec![],
                    write_scopes: vec!["src".into()],
                    tools: true,
                    allow_commands: true,
                    max_rounds: 3,
                }],
            },
        })
        .await
        .unwrap();
    let pending = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(card) = engine
                .store
                .pending_approvals_for_task(&run.tasks[0].id)
                .unwrap()
                .into_iter()
                .next()
            {
                break card;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    engine
        .decide_approval_with_network_ack(&pending.id, true, None, true)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while !b.path().join("command-started.txt").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for result in [
        engine.restore(&task).await,
        engine.restore_checkpoint(&cp.checkpoint_id).await,
    ] {
        assert!(matches!(
            result.unwrap_err().downcast_ref::<WorkspaceChangeError>(),
            Some(WorkspaceChangeError::Active)
        ));
    }
    assert!(engine.latest_restore(&task).unwrap().is_none());
    assert_eq!(
        std::fs::read(a.path().join("src/a.txt")).unwrap(),
        b"after-a"
    );
    assert_eq!(
        std::fs::read(b.path().join("src/a.txt")).unwrap(),
        b"b-sentinel"
    );
    std::fs::write(b.path().join("command-release.txt"), "release").unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        engine.store.task(&run.tasks[0].id).unwrap().status,
        "completed"
    );
    let receipt = engine.restore_checkpoint(&cp.checkpoint_id).await.unwrap();
    assert_eq!(receipt.status, RestoreStatus::Complete);
    assert_eq!(
        engine.restore(&task).await.unwrap().restore_id,
        receipt.restore_id
    );
    assert_eq!(
        std::fs::read(a.path().join("src/a.txt")).unwrap(),
        b"original-a"
    );
    assert_eq!(
        std::fs::read(b.path().join("src/a.txt")).unwrap(),
        b"b-sentinel"
    );
    server.abort();
    let _ = server.await;
}

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
    network_fixture::authorize_routes(&store);
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
        .decide_approval_with_network_ack(
            &approval.id,
            true,
            Some("test"),
            approval.tool_name == "run_command",
        )
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
            .decide_approval_with_network_ack(
                &pending.id,
                true,
                Some("test"),
                pending.tool_name == "run_command",
            )
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
        network_fixture::drop_network_schema(&db);
        db.execute_batch("DROP TABLE IF EXISTS host_config_requests; DROP TABLE IF EXISTS host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE IF EXISTS plugin_management_requests; DROP TABLE IF EXISTS plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1','builtin-runtime-management-v1');").unwrap();
        db.execute_batch("INSERT INTO config VALUES ('sentinel','original-bytes'); DROP TABLE checkpoints; DELETE FROM schema_migrations WHERE id='task-before-checkpoint-repository';").unwrap();
        if version < 8 {
            db.execute_batch("DROP TABLE workspace_restore_outcomes; DROP TABLE workspace_restores; DROP TABLE workspace_changes; DELETE FROM schema_migrations WHERE id='workspace-change-repository';").unwrap();
        }
        if version < 7 {
            db.execute_batch("DROP TABLE approvals; DELETE FROM schema_migrations WHERE id='tool-call-approval-repository';").unwrap();
        }
        db.execute_batch("DROP TABLE IF EXISTS execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; ").unwrap();
        db.pragma_update(None, "user_version", version).unwrap();
        drop(db);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 18);
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

fn expected_after_worktree_migration(
    mut old: Vec<(String, Vec<Vec<rusqlite::types::Value>>)>,
) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
    for name in [
        "host_config_requests",
        "project_budgets",
        "budget_requests",
        "budget_calls",
        "worktree_operations",
        "worktree_project_reservations",
        "project_worktree_origins",
    ] {
        assert!(!old.iter().any(|(table, _)| table == name));
        old.push((name.into(), vec![]));
    }
    assert!(!old.iter().any(|(name, _)| name == "host_config_state"));
    old.push((
        "host_config_state".into(),
        vec![vec![
            rusqlite::types::Value::Integer(1),
            rusqlite::types::Value::Integer(0),
        ]],
    ));
    old.sort_by(|a, b| a.0.cmp(&b.0));
    old
}

fn all_schema10_rows(db: &Connection) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
    let mut q=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT IN ('execution_owner','schema_migrations','project_configs','project_root_bindings','project_selection','task_project_bindings','project_registration_keys','plugin_desired','plugin_management_requests') ORDER BY name").unwrap();
    let names = q
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let mut q = db
                .prepare(&format!("SELECT * FROM {name} ORDER BY rowid"))
                .unwrap();
            let rows = q
                .query_map([], |r| {
                    (0..r.as_ref().column_count()).map(|i| r.get(i)).collect()
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (name, rows)
        })
        .collect()
}

fn business_rows(db: &Connection) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
    let mut names=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT IN ('execution_owner','checkpoints','schema_migrations','sqlite_sequence','network_subjects','network_authorizations','network_confirmations','task_network_bindings','network_calls','command_network_ack','network_events','project_configs','project_root_bindings','project_selection','task_project_bindings','project_registration_keys','plugin_desired','plugin_management_requests') ORDER BY name").unwrap();
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
    network_fixture::drop_network_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS host_config_requests; DROP TABLE IF EXISTS host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE IF EXISTS plugin_management_requests; DROP TABLE IF EXISTS plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1','builtin-runtime-management-v1');").unwrap();
    db.execute_batch("DROP TABLE checkpoints; DELETE FROM schema_migrations WHERE id='task-before-checkpoint-repository'; DROP TABLE IF EXISTS execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; PRAGMA user_version=8;").unwrap();
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
    assert_eq!(
        business_rows(&db),
        expected_after_worktree_migration(old_rows)
    );
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
    network_fixture::drop_network_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS host_config_requests; DROP TABLE IF EXISTS host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE IF EXISTS plugin_management_requests; DROP TABLE IF EXISTS plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1','builtin-runtime-management-v1');").unwrap();
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
    let original = engine.git_diff(&task, request).await.unwrap();
    assert!(original.patch.unwrap().contains("disk"));
    server.abort();
}

#[tokio::test]
async fn schema9_to10_preserves_real_checkpoint_v2_ciphertext_and_restore_rows() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "protected-original").unwrap();
    let (base, server) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    engine
        .create_checkpoint(&task, "migration-proof")
        .await
        .unwrap();
    engine.restore(&task).await.unwrap();
    drop(engine);
    let path = temp.path().join("workspace.db");
    let db = Connection::open(&path).unwrap();
    network_fixture::drop_network_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS host_config_requests; DROP TABLE IF EXISTS host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE IF EXISTS plugin_management_requests; DROP TABLE IF EXISTS plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1','builtin-runtime-management-v1');").unwrap();
    db.execute_batch("DROP TABLE IF EXISTS execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; ").unwrap();
    db.pragma_update(None, "user_version", 9).unwrap();
    let before = business_rows(&db);
    let checkpoints: Vec<Vec<rusqlite::types::Value>> = {
        let mut s = db.prepare("SELECT * FROM checkpoints ORDER BY id").unwrap();
        s.query_map([], |r| {
            (0..r.as_ref().column_count()).map(|i| r.get(i)).collect()
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    };
    drop(db);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 18);
    assert_eq!(store.network_calls_for_task(&task).unwrap().len(), 0);
    drop(store);
    drop(Store::open(&path).unwrap());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        business_rows(&db),
        expected_after_worktree_migration(before)
    );
    let after: Vec<Vec<rusqlite::types::Value>> = {
        let mut s = db.prepare("SELECT * FROM checkpoints ORDER BY id").unwrap();
        s.query_map([], |r| {
            (0..r.as_ref().column_count()).map(|i| r.get(i)).collect()
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    };
    assert_eq!(after, checkpoints);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn completed_restore_replays_after_legacy_migration_and_original_root_is_offline() {
    use peachsh::project_config::*;
    let temp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, server) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    let checkpoint = engine
        .create_checkpoint(&task, "complete-replay")
        .await
        .unwrap()
        .checkpoint;
    let first = engine
        .restore_checkpoint(&checkpoint.checkpoint_id)
        .await
        .unwrap();
    assert_eq!(first.status, RestoreStatus::Complete);
    let b = engine
        .register_project(RegisterProject {
            name: "B".into(),
            root_path: other.path().to_string_lossy().into(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        })
        .await
        .unwrap();
    let selection = engine.current_project().unwrap();
    engine
        .select_project(SelectProject {
            project_id: b.id,
            expected_revision: selection.revision,
        })
        .await
        .unwrap();
    let raw = Connection::open(temp.path().join("workspace.db")).unwrap();
    network_fixture::project_fixture::drop_project_schema(&raw);
    raw.execute_batch("DROP TABLE IF EXISTS host_config_requests; DROP TABLE IF EXISTS host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE IF EXISTS plugin_management_requests; DROP TABLE IF EXISTS plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1','builtin-runtime-management-v1');").unwrap();
    raw.execute_batch("DROP TABLE IF EXISTS execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; ").unwrap();
    raw.pragma_update(None, "user_version", 10).unwrap();
    let all_before = all_schema10_rows(&raw);
    for table in [
        "projects",
        "sessions",
        "turns",
        "tasks",
        "approvals",
        "checkpoints",
        "workspace_changes",
        "workspace_restores",
        "workspace_restore_outcomes",
        "network_subjects",
        "network_authorizations",
        "network_calls",
    ] {
        assert!(
            !all_before
                .iter()
                .find(|(name, _)| name == table)
                .unwrap()
                .1
                .is_empty(),
            "{table} must be nonempty"
        );
    }
    drop(raw);
    drop(engine);
    server.abort();
    let _ = server.await;
    // Close every DB handle and move the complete directory. The original authenticated root is absent.
    let moved = other.path().join("old-root");
    std::fs::rename(temp.path(), &moved).unwrap();
    assert!(!temp.path().exists());
    let store = Arc::new(Store::open(&moved.join("workspace.db")).unwrap());
    let engine = Engine::new(store, 2).unwrap();
    let again = engine.restore(&task).await.unwrap();
    assert_eq!(again.restore_id, first.restore_id);
    assert_eq!(
        engine
            .restore_checkpoint(&checkpoint.checkpoint_id)
            .await
            .unwrap()
            .restore_id,
        first.restore_id
    );
    assert!(
        engine
            .create_checkpoint(&task, "complete-replay")
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(
        engine
            .changes(&task)
            .unwrap_err()
            .downcast_ref::<ProjectError>()
            .unwrap()
            .code(),
        "project_legacy_unbound"
    );
    assert_eq!(std::fs::read(moved.join("src/a.txt")).unwrap(), b"before");
    let raw = Connection::open(moved.join("workspace.db")).unwrap();
    assert_eq!(
        all_schema10_rows(&raw),
        expected_after_worktree_migration(all_before)
    );
    assert_eq!(engine.store.schema_version().unwrap(), 18);
}

#[tokio::test]
async fn schema10_unbound_checkpoint_is_history_only_and_cannot_restore() {
    use peachsh::project_config::ProjectError;
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, server) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    let checkpoint = engine
        .create_checkpoint(&task, "legacy-cp")
        .await
        .unwrap()
        .checkpoint;
    drop(engine);
    let raw = Connection::open(temp.path().join("workspace.db")).unwrap();
    network_fixture::project_fixture::drop_project_schema(&raw);
    raw.execute_batch("DROP TABLE IF EXISTS host_config_requests; DROP TABLE IF EXISTS host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE IF EXISTS plugin_management_requests; DROP TABLE IF EXISTS plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1','builtin-runtime-management-v1');").unwrap();
    raw.execute_batch("DROP TABLE IF EXISTS execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; ").unwrap();
    raw.pragma_update(None, "user_version", 10).unwrap();
    let before = business_rows(&raw);
    drop(raw);
    let store = Arc::new(Store::open(&temp.path().join("workspace.db")).unwrap());
    let engine = Engine::new(store, 2).unwrap();
    assert!(engine.task_project_binding(&task).unwrap().is_none());
    assert_eq!(
        engine
            .checkpoint(&checkpoint.checkpoint_id)
            .unwrap()
            .checkpoint_id,
        checkpoint.checkpoint_id
    );
    assert!(
        engine
            .create_checkpoint(&task, "legacy-cp")
            .await
            .unwrap()
            .replayed
    );
    for error in [
        engine.restore(&task).await.unwrap_err(),
        engine
            .restore_checkpoint(&checkpoint.checkpoint_id)
            .await
            .unwrap_err(),
        engine.changes(&task).unwrap_err(),
        engine.resume(&task, "resume").await.unwrap_err(),
    ] {
        assert_eq!(
            error.downcast_ref::<ProjectError>().unwrap().code(),
            "project_legacy_unbound"
        );
    }
    assert!(engine.latest_restore(&task).unwrap().is_none());
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"after"
    );
    let raw = Connection::open(temp.path().join("workspace.db")).unwrap();
    assert_eq!(
        business_rows(&raw),
        expected_after_worktree_migration(before)
    );
    server.abort();
    let _ = server.await;
}
