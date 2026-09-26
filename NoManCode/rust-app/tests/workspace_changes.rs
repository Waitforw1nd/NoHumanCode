use axum::{Json, Router, extract::State, routing::post};
use peachsh::{
    domain::{Route, RunRequest, SessionKind, Settings, Task, TaskSpec},
    engine::Engine,
    server::{self as host_server, App},
    store::Store,
    workspace,
    workspace_changes::{
        ChangeKind, ChangeState, CurrentState, RestoreStatus, WorkspaceChangeError,
    },
};
use reqwest::StatusCode;
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

#[tokio::test]
async fn approved_write_has_typed_metadata_and_restores_exact_bytes() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), b"before\r\nbytes").unwrap();
    let (base, _server) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    let changes = engine.changes(&task).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].current, CurrentState::Matches);
    assert!(changes[0].restorable);
    let serialized = serde_json::to_string(&changes).unwrap();
    assert!(!serialized.contains("before\\r\\nbytes"));
    let receipt = engine.restore(&task).await.unwrap();
    assert_eq!(receipt.status, RestoreStatus::Complete);
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before\r\nbytes"
    );
    assert_eq!(engine.restore(&task).await.unwrap(), receipt);
}

#[tokio::test]
async fn external_edit_conflicts_without_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, _server) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    std::fs::write(temp.path().join("src/a.txt"), "external").unwrap();
    let error = engine.restore(&task).await.unwrap_err();
    assert!(error.chain().any(|cause| matches!(
        cause.downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Conflict { .. })
    )));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "external"
    );
}

#[tokio::test]
async fn created_file_restore_deletes_only_unchanged_task_output() {
    for external_edit in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        let target = temp.path().join("src/new.txt");
        assert!(!target.exists());
        let (base, _provider) = server("src/new.txt", "created by task\r\n").await;
        let engine = setup(temp.path(), &base);
        let task_id = write_once(&engine).await;
        assert_eq!(std::fs::read(&target).unwrap(), b"created by task\r\n");
        let changes = engine.changes(&task_id).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Created);
        assert_eq!(changes[0].before_digest, None);
        assert_eq!(changes[0].state, ChangeState::Finished);
        assert!(changes[0].restorable);
        if external_edit {
            std::fs::write(&target, "external edit").unwrap();
            assert!(matches!(
                engine
                    .restore(&task_id)
                    .await
                    .unwrap_err()
                    .downcast_ref::<WorkspaceChangeError>(),
                Some(WorkspaceChangeError::Conflict { .. })
            ));
            assert_eq!(std::fs::read(&target).unwrap(), b"external edit");
            assert!(engine.latest_restore(&task_id).unwrap().is_none());
            assert_eq!(
                engine.changes(&task_id).unwrap()[0].current,
                CurrentState::Diverged
            );
        } else {
            let receipt = engine.restore(&task_id).await.unwrap();
            assert_eq!(receipt.status, RestoreStatus::Complete);
            assert_eq!(receipt.restored, 1);
            assert_eq!(receipt.outcomes.len(), 1);
            assert_eq!(receipt.outcomes[0].status, RestoreStatus::Complete);
            assert!(!target.exists());
            assert_eq!(engine.restore(&task_id).await.unwrap(), receipt);
            assert!(!target.exists());
        }
    }
}

#[tokio::test]
async fn unsupported_new_content_and_before_bytes_have_no_file_effect() {
    let oversized = "word ".repeat(52_429);
    assert_eq!(oversized.len(), 262_145);
    for (before, after, expect_empty) in [
        (b"original".to_vec(), oversized.clone(), false),
        (oversized.into_bytes(), "ordinary after".into(), true),
        (vec![0xff, 0xfe, 0x80], "ordinary after".into(), true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        let target = temp.path().join("src/a.txt");
        std::fs::write(&target, &before).unwrap();
        let (base, _provider) = server("src/a.txt", &after).await;
        let engine = setup(temp.path(), &base);
        let task_id = write_once(&engine).await;
        assert_eq!(std::fs::read(&target).unwrap(), before);
        let changes = engine.changes(&task_id).unwrap();
        assert!(
            changes
                .iter()
                .all(|change| change.state == ChangeState::Failed)
        );
        let db = Connection::open(temp.path().join("workspace.db")).unwrap();
        let states: Vec<String> = db
            .prepare("SELECT state FROM workspace_changes WHERE task_id=?1")
            .unwrap()
            .query_map([&task_id], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        if expect_empty {
            assert!(changes.is_empty());
            assert!(states.is_empty());
        } else {
            assert_eq!(states, vec!["failed"]);
        }
        let task = engine.store.task(&task_id).unwrap();
        assert_eq!(task.status, "completed");
        assert!(task.messages.iter().any(|message| {
            message["role"] == "tool"
                && serde_json::from_str::<Value>(message["content"].as_str().unwrap())
                    .unwrap()
                    .get("error")
                    .is_some()
        }));
        let approvals = engine.store.approvals_for_task(&task_id).unwrap();
        assert_eq!(approvals.len(), 1);
        assert_eq!(
            approvals[0].status,
            peachsh::approval::ApprovalStatus::Approved
        );
        assert_eq!(
            approvals[0].execution_state,
            peachsh::approval::ExecutionState::Finished
        );
        assert!(engine.latest_restore(&task_id).unwrap().is_none());
        assert_eq!(
            std::fs::read_dir(temp.path().join("src")).unwrap().count(),
            1
        );
    }
}

#[tokio::test]
async fn changed_host_workspace_or_task_scopes_reject_restore_before_claim() {
    for change_host in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let alternate = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::create_dir(alternate.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
        std::fs::write(alternate.path().join("src/a.txt"), "alternate sentinel").unwrap();
        let (base, _provider) = server("src/a.txt", "after").await;
        let engine = setup(temp.path(), &base);
        let task_id = write_once(&engine).await;
        let changes_before = engine.changes(&task_id).unwrap();
        if change_host {
            let mut settings = engine.store.settings().unwrap().unwrap();
            settings.workspace = alternate.path().to_string_lossy().into();
            engine.configure(settings).await.unwrap();
            assert_eq!(
                engine.store.settings().unwrap().unwrap().workspace,
                alternate.path().to_string_lossy()
            );
        } else {
            let mut task = engine.store.task(&task_id).unwrap();
            task.spec.write_scopes.push("extra".into());
            // The ordinary Store API enforces immutable scopes; simulate persisted tampering
            // to prove restore also checks the original successful change's permission snapshot.
            assert!(engine.store.save_task(&task).is_err());
            let db = Connection::open(temp.path().join("workspace.db")).unwrap();
            assert_eq!(
                db.execute(
                    "UPDATE tasks SET value=?1 WHERE id=?2",
                    rusqlite::params![serde_json::to_string(&task).unwrap(), task_id]
                )
                .unwrap(),
                1
            );
            assert_eq!(
                engine.store.task(&task_id).unwrap().spec.write_scopes,
                vec!["src", "extra"]
            );
        }
        assert!(matches!(
            engine
                .restore(&task_id)
                .await
                .unwrap_err()
                .downcast_ref::<WorkspaceChangeError>(),
            Some(WorkspaceChangeError::Conflict { .. })
        ));
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            b"after"
        );
        assert_eq!(
            std::fs::read(alternate.path().join("src/a.txt")).unwrap(),
            b"alternate sentinel"
        );
        assert!(engine.latest_restore(&task_id).unwrap().is_none());
        let changes_after = engine.changes(&task_id).unwrap();
        assert_eq!(changes_after[0].change_id, changes_before[0].change_id);
        assert_eq!(changes_after[0].state, ChangeState::Finished);
        assert_eq!(changes_after[0].restore_state, "pending");
    }
}

#[tokio::test]
async fn another_task_edit_blocks_resumed_write_without_absorbing_external_bytes() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    let target = temp.path().join("src/a.txt");
    std::fs::write(&target, "original").unwrap();
    let (base, _provider) = scripted_server(vec![
        write_delta("a-first", "src/a.txt", "task a"),
        json!({"content":"done"}),
        write_delta("b-first", "src/a.txt", "task b"),
        json!({"content":"done"}),
        write_delta("a-second", "src/a.txt", "overwrite b"),
        json!({"content":"done"}),
    ])
    .await;
    let engine = setup(temp.path(), &base);
    let first = write_once(&engine).await;
    let first_changes = engine.changes(&first).unwrap();
    let second = write_once(&engine).await;
    assert_eq!(std::fs::read(&target).unwrap(), b"task b");
    engine.resume(&first, "write again").await.unwrap();
    let pending = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(approval) = engine
                .store
                .pending_approvals_for_task(&first)
                .unwrap()
                .into_iter()
                .next()
            {
                break approval;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(pending.tool_call_id, "a-second");
    engine
        .decide_approval(&pending.id, true, Some("test"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"task b");
    let changes = engine.changes(&first).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].change_id, first_changes[0].change_id);
    assert_eq!(changes[0].before_digest, first_changes[0].before_digest);
    assert_eq!(changes[0].after_digest, first_changes[0].after_digest);
    let task = engine.store.task(&first).unwrap();
    assert_eq!(task.status, "completed");
    let answer = task
        .messages
        .iter()
        .find(|message| message["tool_call_id"] == "a-second")
        .unwrap();
    assert!(
        serde_json::from_str::<Value>(answer["content"].as_str().unwrap())
            .unwrap()
            .get("error")
            .is_some()
    );
    assert_eq!(
        engine.store.approval(&pending.id).unwrap().execution_state,
        peachsh::approval::ExecutionState::Finished
    );
    assert!(engine.latest_restore(&first).unwrap().is_none());
    assert!(matches!(
        engine
            .restore(&first)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Conflict { .. })
    ));
    assert_eq!(
        engine.restore(&second).await.unwrap().status,
        RestoreStatus::Complete
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"task a");
    assert_eq!(
        engine.restore(&first).await.unwrap().status,
        RestoreStatus::Complete
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"original");
}

#[tokio::test]
async fn empty_restore_is_noop_without_operation() {
    let temp = tempfile::tempdir().unwrap();
    let (base, _server) = server("", "").await;
    let engine = setup(temp.path(), &base);
    let task = engine
        .start(RunRequest {
            title: "empty".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "reader".into(),
                role: "reader".into(),
                route_id: "route".into(),
                prompt: "done".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: false,
                allow_commands: false,
                max_rounds: 1,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if !engine.is_busy() {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    let receipt = engine.restore(&task.id).await.unwrap();
    assert_eq!(receipt.status, RestoreStatus::Complete);
    assert_eq!(receipt.restore_id, None);
    assert!(engine.latest_restore(&task.id).unwrap().is_none());
}

#[test]
fn schema7_migrates_and_forged_schema8_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("workspace.db");
    let store = Store::open(&path).unwrap();
    store
        .save_settings(&Settings {
            workspace: temp.path().to_string_lossy().into(),
            max_concurrency: 1,
            routes: vec![],
            newapi: None,
        })
        .unwrap();
    drop(store);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE workspace_restore_outcomes; DROP TABLE workspace_restores; DROP TABLE workspace_changes; DELETE FROM schema_migrations WHERE id='workspace-change-repository'; PRAGMA user_version=7;").unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 8);
    assert!(store.settings().unwrap().is_some());
    drop(store);
    db.execute_batch("DROP INDEX workspace_changes_task_path; CREATE INDEX workspace_changes_task_path ON workspace_changes(path_key,task_id,created_at);").unwrap();
    assert!(Store::open(&path).is_err());

    for (needle, forgery) in [
        ("'created','modified'", "'CREATED','MODIFIED'"),
        ("'created','modified'", "'cre ated','modified'"),
    ] {
        let forged_path = temp.path().join(format!("forged-{}.db", forgery.len()));
        let store = Store::open(&forged_path).unwrap();
        drop(store);
        let forged_db = Connection::open(&forged_path).unwrap();
        let schema = [
            "workspace_changes",
            "workspace_restores",
            "workspace_restore_outcomes",
            "workspace_changes_task_path",
            "workspace_restores_task_complete",
            "workspace_restore_outcomes_restore",
        ]
        .map(|name| {
            forged_db
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE name=?1",
                    [name],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
        });
        let replacement = schema[0].replace(needle, forgery);
        assert_ne!(replacement, schema[0]);
        forged_db.execute_batch("DROP TABLE workspace_restore_outcomes; DROP TABLE workspace_restores; DROP TABLE workspace_changes;").unwrap();
        forged_db.execute_batch(&replacement).unwrap();
        for ddl in &schema[1..] {
            forged_db.execute_batch(ddl).unwrap();
        }
        let error = Store::open(&forged_path)
            .err()
            .expect("forged constraint must fail");
        assert!(
            error.to_string().contains("workspace_changes") && error.to_string().contains("约束"),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn http_unknown_receipt_is_safe_after_outcome_persist_failure() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before-http-secret").unwrap();
    let (base, _provider) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task = write_once(&engine).await;
    Connection::open(temp.path().join("workspace.db")).unwrap().execute_batch("CREATE TRIGGER fail_restore_outcome BEFORE UPDATE ON workspace_restore_outcomes BEGIN SELECT RAISE(ABORT,'SQL-private-trigger'); END;").unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let token = "workspace-http-token".to_owned();
    let app = host_server::router(App {
        engine,
        token: token.clone(),
        origin: origin.clone(),
    });
    let _host = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .post(format!("{origin}/api/tasks/{task}/restore"))
        .header("x-peachsh-token", &token)
        .header("origin", &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "unknown");
    assert!(body.pointer("/receipt/restore_id").unwrap().is_string());
    let text = body.to_string();
    for forbidden in ["SQL-private-trigger", "before-http-secret", "workspace.db"] {
        assert!(!text.contains(forbidden));
    }

    let invalid = client
        .get(format!("{origin}/api/tasks/bad%20id/changes"))
        .header("x-peachsh-token", &token)
        .header("origin", &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(!invalid.text().await.unwrap().contains("bad id"));
}

#[tokio::test]
async fn failed_atomic_replace_cleans_plaintext_temp() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("src/target")).unwrap();
    let task = Task {
        id: "task".into(),
        run_id: "run".into(),
        spec: TaskSpec {
            name: "writer".into(),
            role: "writer".into(),
            route_id: "route".into(),
            prompt: "write".into(),
            depends_on: vec![],
            write_scopes: vec!["src".into()],
            tools: true,
            allow_commands: false,
            max_rounds: 1,
        },
        route: Route {
            id: "route".into(),
            name: "route".into(),
            base_url: "http://127.0.0.1".into(),
            model: "mock".into(),
            max_tokens: 1,
            parallel_limit: 1,
            key_env: None,
        },
        workspace: temp.path().to_string_lossy().into(),
        status: "running".into(),
        output: String::new(),
        error: None,
        messages: vec![],
        usage: Value::Null,
        created_at: 0,
        updated_at: 0,
    };
    assert!(
        workspace::execute(
            &task,
            "write_file",
            &json!({"path":"src/target","content":"plaintext-sentinel"})
        )
        .await
        .is_err()
    );
    let names: Vec<_> = std::fs::read_dir(temp.path().join("src"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with(".peachsh-") && name.ends_with(".tmp"))
    );
}

#[tokio::test]
async fn repeated_path_restores_earliest_before_and_legacy_mix_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "first").unwrap();
    let (base, _server) = scripted_server(vec![
        write_delta("w1", "src/a.txt", "second"),
        write_delta("w2", "src/a.txt", "third"),
        json!({"content":"done"}),
    ])
    .await;
    let engine = setup(temp.path(), &base);
    let task = engine
        .start(RunRequest {
            title: "repeat".into(),
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
                max_rounds: 5,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    for _ in 0..2 {
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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if !engine.is_busy() {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.changes(&task.id).unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "third"
    );
    let receipt = engine.restore(&task.id).await.unwrap();
    assert_eq!(receipt.restored, 1);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "first"
    );

    let temp2 = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp2.path().join("src")).unwrap();
    std::fs::write(temp2.path().join("src/a.txt"), "before").unwrap();
    let (base2, _server2) = server("src/a.txt", "after").await;
    let engine2 = setup(temp2.path(), &base2);
    let task2 = write_once(&engine2).await;
    engine2
        .store
        .event(
            &task2,
            "file_backup",
            json!({"path":"src/a.txt","previous":"legacy-placeholder"}),
        )
        .unwrap();
    assert!(
        engine2
            .restore(&task2)
            .await
            .unwrap_err()
            .chain()
            .any(|cause| matches!(
                cause.downcast_ref::<WorkspaceChangeError>(),
                Some(WorkspaceChangeError::Unrestorable)
            ))
    );
    assert_eq!(
        std::fs::read_to_string(temp2.path().join("src/a.txt")).unwrap(),
        "after"
    );
}

#[tokio::test]
async fn restore_rejects_tampered_call_path_content_and_binding() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, _provider) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task_id = write_once(&engine).await;
    let db = Connection::open(temp.path().join("workspace.db")).unwrap();
    let original: String = db
        .query_row("SELECT value FROM tasks WHERE id=?1", [&task_id], |row| {
            row.get(0)
        })
        .unwrap();

    db.execute(
        "UPDATE workspace_changes SET path='src/other.txt' WHERE task_id=?1",
        [&task_id],
    )
    .unwrap();
    let error = engine.restore(&task_id).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<WorkspaceChangeError>(),
            Some(WorkspaceChangeError::Corrupt)
        ),
        "{error:?}"
    );
    db.execute(
        "UPDATE workspace_changes SET path='src/a.txt',binding_digest='forged' WHERE task_id=?1",
        [&task_id],
    )
    .unwrap();
    assert!(engine.restore(&task_id).await.is_err());
    let binding: String = db
        .query_row(
            "SELECT binding_digest FROM approvals WHERE task_id=?1",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE workspace_changes SET binding_digest=?1 WHERE task_id=?2",
        (&binding, &task_id),
    )
    .unwrap();

    let mut value: Value = serde_json::from_str(&original).unwrap();
    for message in value["messages"].as_array_mut().unwrap() {
        if let Some(calls) = message["tool_calls"].as_array_mut() {
            for call in calls {
                if call["id"] == "write-1" {
                    call["function"]["arguments"] = json!({"path":"src/a.txt","content":"forged"})
                        .to_string()
                        .into();
                }
            }
        }
    }
    db.execute(
        "UPDATE tasks SET value=?1 WHERE id=?2",
        (value.to_string(), &task_id),
    )
    .unwrap();
    let error = engine.restore(&task_id).await.unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<WorkspaceChangeError>(),
            Some(WorkspaceChangeError::Corrupt)
        ),
        "{error:?}"
    );
    db.execute(
        "UPDATE tasks SET value=?1 WHERE id=?2",
        (&original, &task_id),
    )
    .unwrap();
    db.execute(
        "UPDATE workspace_changes SET tool_call_id='forged-call' WHERE task_id=?1",
        [&task_id],
    )
    .unwrap();
    assert!(matches!(
        engine
            .restore(&task_id)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Corrupt)
    ));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "after"
    );
    assert!(engine.latest_restore(&task_id).unwrap().is_none());
}

#[tokio::test]
async fn parent_commit_failure_returns_unknown_receipt_and_seals_change() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, _provider) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task_id = write_once(&engine).await;
    let db = Connection::open(temp.path().join("workspace.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_complete BEFORE UPDATE ON workspace_restores WHEN NEW.status='complete' BEGIN SELECT RAISE(ABORT,'private-parent-error'); END;").unwrap();
    let error = engine.restore(&task_id).await.unwrap_err();
    let Some(WorkspaceChangeError::Unknown {
        receipt: Some(receipt),
    }) = error.downcast_ref::<WorkspaceChangeError>()
    else {
        panic!("expected unknown receipt: {error}")
    };
    assert_eq!(receipt.status, RestoreStatus::Unknown);
    assert_eq!(receipt.restored, 1);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "before"
    );
    assert_eq!(
        engine.latest_restore(&task_id).unwrap().unwrap().status,
        RestoreStatus::Unknown
    );
    assert!(!engine.changes(&task_id).unwrap()[0].restorable);
}

#[tokio::test]
async fn redacted_write_arguments_do_not_leave_an_unrestorable_success() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, _provider) = server("src/a.txt", "sk-workspace-test-secret").await;
    let engine = setup(temp.path(), &base);
    let task_id = write_once(&engine).await;
    let task = engine.store.task(&task_id).unwrap();
    let changes = engine.changes(&task_id).unwrap();
    assert_eq!(task.status, "completed");
    assert!(changes.is_empty());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "before"
    );
    assert!(task.messages.iter().any(|message| {
        message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("error"))
    }));
}

#[tokio::test]
async fn finished_change_without_after_digest_is_corrupt_not_empty_restore() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, _provider) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task_id = write_once(&engine).await;
    let db = Connection::open(temp.path().join("workspace.db")).unwrap();
    db.execute(
        "UPDATE workspace_changes SET after_digest=NULL WHERE task_id=?1",
        [&task_id],
    )
    .unwrap();
    assert!(matches!(
        engine
            .restore(&task_id)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Corrupt)
    ));
    assert!(matches!(
        engine
            .changes(&task_id)
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Corrupt)
    ));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "after"
    );
    assert!(engine.latest_restore(&task_id).unwrap().is_none());
}

#[tokio::test]
async fn another_task_write_blocks_old_restore_and_hardlink_bytes_stay_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("outside.txt"), "original").unwrap();
    std::fs::hard_link(
        temp.path().join("outside.txt"),
        temp.path().join("src/a.txt"),
    )
    .unwrap();
    let (base, _provider) = scripted_server(vec![
        write_delta("w1", "src/a.txt", "first"),
        json!({"content":"done"}),
        write_delta("w2", "src/a.txt", "second"),
        json!({"content":"done"}),
    ])
    .await;
    let engine = setup(temp.path(), &base);
    let first = write_once(&engine).await;
    assert_eq!(
        std::fs::read_to_string(temp.path().join("outside.txt")).unwrap(),
        "original"
    );
    let second = write_once(&engine).await;
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "second"
    );
    assert!(matches!(
        engine
            .restore(&first)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Conflict { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "second"
    );
    assert_eq!(
        engine.restore(&second).await.unwrap().status,
        RestoreStatus::Complete
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "first"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("outside.txt")).unwrap(),
        "original"
    );
}

#[tokio::test]
async fn later_path_conflict_prevents_all_restore_writes() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "a-before").unwrap();
    std::fs::write(temp.path().join("src/b.txt"), "b-before").unwrap();
    let (base, _provider) = scripted_server(vec![
        write_delta("w1", "src/a.txt", "a-after"),
        write_delta("w2", "src/b.txt", "b-after"),
        json!({"content":"done"}),
    ])
    .await;
    let engine = setup(temp.path(), &base);
    let task = engine
        .start(RunRequest {
            title: "two".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "writer".into(),
                role: "writer".into(),
                route_id: "route".into(),
                prompt: "write".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 5,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    for _ in 0..2 {
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
    }
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if !engine.is_busy() {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.changes(&task.id).unwrap().len(), 2);
    std::fs::write(temp.path().join("src/b.txt"), "external").unwrap();
    assert!(engine.restore(&task.id).await.is_err());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "a-after"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/b.txt")).unwrap(),
        "external"
    );
    assert!(engine.latest_restore(&task.id).unwrap().is_none());

    std::fs::write(temp.path().join("src/b.txt"), "b-after").unwrap();
    Connection::open(temp.path().join("workspace.db"))
        .unwrap()
        .execute(
            "UPDATE workspace_changes SET before_blob=x'00' WHERE task_id=?1 AND path='src/b.txt'",
            [&task.id],
        )
        .unwrap();
    assert!(matches!(
        engine
            .restore(&task.id)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Corrupt)
    ));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "a-after"
    );
    assert!(engine.latest_restore(&task.id).unwrap().is_none());
}

#[tokio::test]
async fn missing_non_utf8_and_link_are_distinct_current_states() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, _provider) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task_id = write_once(&engine).await;
    let path = temp.path().join("src/a.txt");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        engine.changes(&task_id).unwrap()[0].current,
        CurrentState::Missing
    );
    assert!(engine.restore(&task_id).await.is_err());
    std::fs::write(&path, [0xff, 0xfe]).unwrap();
    assert_eq!(
        engine.changes(&task_id).unwrap()[0].current,
        CurrentState::Unreadable
    );
    assert!(engine.restore(&task_id).await.is_err());
    std::fs::remove_file(&path).unwrap();
    let outside = temp.path().join("outside.txt");
    std::fs::write(&outside, "outside-sentinel").unwrap();
    #[cfg(windows)]
    let link_result = std::os::windows::fs::symlink_file(&outside, &path);
    #[cfg(unix)]
    let link_result = std::os::unix::fs::symlink(&outside, &path);
    if link_result.is_ok() {
        eprintln!("symlink creation succeeded");
        assert!(matches!(
            engine.changes(&task_id).unwrap()[0].current,
            CurrentState::Invalid | CurrentState::UnsafeLink
        ));
        assert!(engine.restore(&task_id).await.is_err());
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "outside-sentinel"
        );
    } else if let Err(error) = link_result {
        eprintln!("symlink creation unavailable: {error:?}");
    }
}

#[test]
fn workspace_path_aliases_and_forbidden_spellings_are_rejected_or_coalesced() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "a").unwrap();
    let scopes = vec!["src".to_owned()];
    let lower = workspace::resolve(temp.path(), "src/a.txt", true, &scopes).unwrap();
    #[cfg(windows)]
    {
        let upper = workspace::resolve(temp.path(), "SRC/A.TXT", true, &scopes).unwrap();
        assert_eq!(lower.canonicalize().unwrap(), upper.canonicalize().unwrap());
    }
    assert!(lower.exists());
    for path in [
        "src/a.txt.",
        "src/a.txt ",
        "src/../a.txt",
        "src/.env",
        "src/CON",
    ] {
        assert!(
            workspace::resolve(temp.path(), path, true, &scopes).is_err(),
            "{path}"
        );
    }
}

#[tokio::test]
async fn dpapi_before_secret_roundtrips_without_plaintext_in_database() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    let before = b"sk-original-workspace-secret\r\n";
    std::fs::write(temp.path().join("src/a.txt"), before).unwrap();
    let (base, _provider) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task_id = write_once(&engine).await;
    let db = Connection::open(temp.path().join("workspace.db")).unwrap();
    let sealed: Vec<u8> = db
        .query_row(
            "SELECT before_blob FROM workspace_changes WHERE task_id=?1",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!sealed.windows(before.len()).any(|slice| slice == before));
    assert_eq!(
        engine.restore(&task_id).await.unwrap().status,
        RestoreStatus::Complete
    );
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        before
    );
}

#[tokio::test]
async fn schema7_to8_preserves_existing_task_approval_and_event_rows() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("src")).unwrap();
    std::fs::write(temp.path().join("src/a.txt"), "before").unwrap();
    let (base, _provider) = server("src/a.txt", "after").await;
    let engine = setup(temp.path(), &base);
    let task_id = write_once(&engine).await;
    let db_path = temp.path().join("workspace.db");
    drop(engine);
    let db = Connection::open(&db_path).unwrap();
    let task: String = db
        .query_row("SELECT value FROM tasks WHERE id=?1", [&task_id], |row| {
            row.get(0)
        })
        .unwrap();
    let approvals: Vec<(String, String)> = {
        let mut statement = db
            .prepare("SELECT id,status FROM approvals WHERE task_id=?1 ORDER BY id")
            .unwrap();
        statement
            .query_map([&task_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let events: Vec<(i64, String, String)> = {
        let mut statement = db
            .prepare("SELECT seq,kind,data FROM events WHERE task_id=?1 ORDER BY seq")
            .unwrap();
        statement
            .query_map([&task_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(!approvals.is_empty() && !events.is_empty());
    db.execute_batch("DROP TABLE workspace_restore_outcomes; DROP TABLE workspace_restores; DROP TABLE workspace_changes; DELETE FROM schema_migrations WHERE id='workspace-change-repository'; PRAGMA user_version=7;").unwrap();
    drop(db);
    let store = Store::open(&db_path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 8);
    drop(store);
    let db = Connection::open(&db_path).unwrap();
    let task_after: String = db
        .query_row("SELECT value FROM tasks WHERE id=?1", [&task_id], |row| {
            row.get(0)
        })
        .unwrap();
    let approvals_after: Vec<(String, String)> = {
        let mut statement = db
            .prepare("SELECT id,status FROM approvals WHERE task_id=?1 ORDER BY id")
            .unwrap();
        statement
            .query_map([&task_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let events_after: Vec<(i64, String, String)> = {
        let mut statement = db
            .prepare("SELECT seq,kind,data FROM events WHERE task_id=?1 ORDER BY seq")
            .unwrap();
        statement
            .query_map([&task_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(task_after, task);
    assert_eq!(approvals_after, approvals);
    assert_eq!(events_after, events);
    let store = Store::open(&db_path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 8);
}
