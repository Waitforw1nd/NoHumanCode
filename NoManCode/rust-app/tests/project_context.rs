use axum::{Router, routing::post};
use peachsh::{
    domain::*, engine::Engine, network_authorization::*, project_config::*, store::Store,
};
use rusqlite::Connection;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn settings(root: &Path, base: &str) -> Settings {
    Settings {
        workspace: root.to_string_lossy().into(),
        max_concurrency: 4,
        routes: vec![Route {
            id: "route".into(),
            name: "Route".into(),
            base_url: base.into(),
            model: "mock".into(),
            max_tokens: 128,
            parallel_limit: 4,
            key_env: None,
        }],
        newapi: None,
    }
}
fn register(store: &Store, root: &Path, name: &str) -> Project {
    store
        .register_project(RegisterProject {
            name: name.into(),
            root_path: root.to_string_lossy().into(),
            idempotency_key: id(),
        })
        .unwrap()
}
fn configure(store: &Store, project: &ProjectId) {
    let old = store.project_config(project).unwrap();
    store
        .configure_project(ConfigureProject {
            project_id: project.clone(),
            expected_revision: old.revision,
            config: ProjectConfig {
                allowed_route_ids: vec!["route".into()],
                default_route_id: Some("route".into()),
                tools: true,
                write_scopes: vec!["src".into()],
                allow_commands: false,
            },
        })
        .unwrap();
}
fn subject(project: &ProjectId) -> NetworkSubject {
    NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(project.clone()),
    }
}
fn grant(store: &Store, project: &ProjectId) -> NetworkAuthorization {
    let preview = store.network_preview(subject(project)).unwrap();
    store
        .network_confirm(
            subject(project),
            ConfirmRequest {
                config_revision: preview.config_revision,
                credential_revision: preview.credential_revision,
                binding_digest: preview.binding_digest,
                purposes: vec![NetworkPurpose::Models, NetworkPurpose::Completion],
            },
            &id(),
        )
        .unwrap()
        .authorization
}
fn request() -> RunRequest {
    RunRequest {
        title: "project proof".into(),
        kind: SessionKind::Chat,
        tasks: vec![TaskSpec {
            name: "worker".into(),
            role: "developer".into(),
            route_id: "route".into(),
            prompt: "hello".into(),
            depends_on: vec![],
            write_scopes: vec!["src".into()],
            tools: true,
            allow_commands: false,
            max_rounds: 2,
        }],
    }
}
fn code(error: anyhow::Error, expected: &str) {
    assert_eq!(
        error.downcast_ref::<ProjectError>().unwrap().code(),
        expected
    );
}

#[cfg(windows)]
#[tokio::test]
async fn ordinary_root_search_rejects_case_sensitive_descendant() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("case");
    std::fs::create_dir(&child).unwrap();
    let output = std::process::Command::new("fsutil.exe")
        .args(["file", "setCaseSensitiveInfo"])
        .arg(&child)
        .arg("enable")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "case-sensitive fixture must actually exist"
    );
    std::fs::write(child.join("secret.txt"), "unique-sensitive-sentinel").unwrap();
    let task = Task {
        id: id(),
        run_id: id(),
        spec: request().tasks.remove(0),
        route: settings(temp.path(), "http://127.0.0.1:1").routes.remove(0),
        workspace: temp.path().to_string_lossy().into(),
        status: "running".into(),
        output: String::new(),
        error: None,
        messages: vec![],
        usage: serde_json::Value::Null,
        created_at: 1,
        updated_at: 1,
    };
    let error = peachsh::workspace::execute(
        &task,
        "search_files",
        &serde_json::json!({"path":".","query":"unique-sensitive-sentinel"}),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<peachsh::workspace_identity::RootIdentityError>(),
        Some(peachsh::workspace_identity::RootIdentityError::CaseSensitiveDirectory)
    ));
}
async fn settled(engine: &Engine, task: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.store.task(task).unwrap().status, "completed");
}

#[test]
fn safe_defaults_cas_restart_and_overlap_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    std::fs::create_dir(&a).unwrap();
    let db = temp.path().join("db");
    let store = Store::open(&db).unwrap();
    store
        .save_settings(&settings(temp.path(), "http://127.0.0.1:1"))
        .unwrap();
    let project = register(&store, &a, "A");
    let initial = store.project_config(&project.id).unwrap();
    assert_eq!(initial.revision, 1);
    assert_eq!(initial.config, ProjectConfig::default());
    assert!(store.current_project().unwrap().project_id.is_none());
    configure(&store, &project.id);
    code(
        store
            .configure_project(ConfigureProject {
                project_id: project.id.clone(),
                expected_revision: 1,
                config: ProjectConfig::default(),
            })
            .unwrap_err(),
        "project_revision_conflict",
    );
    let selected = store
        .select_project(SelectProject {
            project_id: project.id.clone(),
            expected_revision: 0,
        })
        .unwrap();
    assert_eq!(selected.revision, 1);
    code(
        store
            .select_project(SelectProject {
                project_id: project.id.clone(),
                expected_revision: 0,
            })
            .unwrap_err(),
        "project_revision_conflict",
    );
    code(
        store
            .register_project(RegisterProject {
                name: "other".into(),
                root_path: a.to_string_lossy().into(),
                idempotency_key: id(),
            })
            .unwrap_err(),
        "project_duplicate_root",
    );
    let nested = a.join("nested");
    std::fs::create_dir(&nested).unwrap();
    code(
        store
            .register_project(RegisterProject {
                name: "nested".into(),
                root_path: nested.to_string_lossy().into(),
                idempotency_key: id(),
            })
            .unwrap_err(),
        "project_overlapping_root",
    );
    drop(store);
    let store = Store::open(&db).unwrap();
    assert_eq!(store.schema_version().unwrap(), 17);
    assert_eq!(
        store.current_project().unwrap().project_id,
        Some(project.id.clone())
    );
    assert_eq!(store.project_config(&project.id).unwrap().revision, 2);
}

#[test]
fn unavailable_legacy_root_does_not_block_registration_or_change_old_row() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let store = Store::open(&path).unwrap();
    let raw = Connection::open(&path).unwrap();
    let missing = temp
        .path()
        .join("deleted-history")
        .to_string_lossy()
        .into_owned();
    raw.execute("INSERT INTO projects(id,name,root_path,created_at,updated_at) VALUES ('old','historic',?1,1,2)",[&missing]).unwrap();
    let fresh = temp.path().join("fresh");
    std::fs::create_dir(&fresh).unwrap();
    let project = register(&store, &fresh, "Fresh");
    assert_ne!(project.id.0, "old");
    let old = store.project(&ProjectId("old".into())).unwrap();
    assert_eq!(old.name, "historic");
    assert_eq!(old.root_path, missing);
    assert_eq!((old.created_at, old.updated_at), (1, 2));
    assert_eq!(
        raw.query_row(
            "SELECT count(*) FROM project_root_bindings WHERE project_id='old'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn register_replay_precedes_root_access_and_failure_rolls_back_all_tables() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let store = Store::open(&path).unwrap();
    let root = temp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let command = RegisterProject {
        name: "root".into(),
        root_path: root.to_string_lossy().into(),
        idempotency_key: "register-once".into(),
    };
    let first = store.register_project(command.clone()).unwrap();
    std::fs::remove_dir(&root).unwrap();
    assert_eq!(store.register_project(command).unwrap(), first);
    // A fresh registration fails after inserting the domain row. The immediate transaction undoes it.
    let other = temp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    drop(store);
    let path = temp.path().join("rollback-db");
    let store = Store::open(&path).unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TRIGGER stop_config BEFORE INSERT ON project_configs BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;").unwrap();
    let error = store
        .register_project(RegisterProject {
            name: "other".into(),
            root_path: other.to_string_lossy().into(),
            idempotency_key: id(),
        })
        .unwrap_err();
    assert!(error.to_string().contains("fixture rollback"));
    for table in [
        "projects",
        "project_configs",
        "project_root_bindings",
        "project_registration_keys",
    ] {
        assert_eq!(
            raw.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn separate_grants_selection_and_old_session_follow_original_project() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    std::fs::create_dir_all(a.join("src")).unwrap();
    std::fs::create_dir_all(b.join("src")).unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let app=Router::new().route("/chat/completions",post(move || {let count=count.clone();async move {count.fetch_add(1,Ordering::SeqCst);"data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = Arc::new(Store::open(&temp.path().join("db")).unwrap());
    store.save_settings(&settings(temp.path(), &base)).unwrap();
    store.put_secret("route::route", "fixture-key").unwrap();
    let engine = Engine::new(store.clone(), 4).unwrap();
    let a = register(&store, &a, "A");
    let b = register(&store, &b, "B");
    configure(&store, &a.id);
    configure(&store, &b.id);
    grant(&store, &a.id);
    let denied = engine
        .start_in_project(StartInProject {
            project_id: b.id.clone(),
            expected_config_revision: 2,
            request: request(),
            idempotency_key: "b-first".into(),
        })
        .await
        .err()
        .unwrap();
    assert_eq!(
        denied.downcast_ref::<NetworkError>().unwrap().kind,
        NetworkErrorKind::Required
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert!(store.project_runs(&b.id).unwrap().is_empty());
    let run = engine
        .start_in_project(StartInProject {
            project_id: a.id.clone(),
            expected_config_revision: 2,
            request: request(),
            idempotency_key: "a-first".into(),
        })
        .await
        .unwrap();
    settled(&engine, &run.tasks[0].id).await;
    store
        .select_project(SelectProject {
            project_id: b.id.clone(),
            expected_revision: 0,
        })
        .unwrap();
    grant(&store, &b.id);
    let second = engine
        .start_idempotent(request(), "default-b")
        .await
        .unwrap();
    settled(&engine, &second.tasks[0].id).await;
    assert_eq!(
        store
            .task_project_binding(&second.tasks[0].id)
            .unwrap()
            .unwrap()
            .project_id,
        b.id
    );
    let raw = Connection::open(temp.path().join("db")).unwrap();
    let (session,agent,turn):(String,String,String)=raw.query_row("SELECT tt.session_id,tt.agent_id,tt.turn_id FROM turn_tasks tt WHERE legacy_task_id=?1",[&run.tasks[0].id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    let next = engine
        .send_chat_turn(SendChatTurn {
            session_id: SessionId(session),
            agent_id: AgentId(agent),
            expected_last_turn_id: TurnId(turn),
            message: "continue".into(),
            idempotency_key: "a-next".into(),
        })
        .await
        .unwrap();
    settled(&engine, &next.task.legacy_task_id).await;
    assert_eq!(
        store
            .task_project_binding(&next.task.legacy_task_id)
            .unwrap()
            .unwrap()
            .project_id,
        a.id
    );
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    let old_binding: String = raw
        .query_row(
            "SELECT authorization_id FROM task_network_bindings WHERE task_id=?1",
            [&run.tasks[0].id],
            |r| r.get(0),
        )
        .unwrap();
    let new_binding: String = raw
        .query_row(
            "SELECT authorization_id FROM task_network_bindings WHERE task_id=?1",
            [&next.task.legacy_task_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old_binding, new_binding);
    // Expansion changes the ceiling, never the frozen Task or approval permission digest.
    let frozen = store
        .task_project_binding(&next.task.legacy_task_id)
        .unwrap()
        .unwrap();
    let mut expanded = store.project_config(&a.id).unwrap();
    expanded.config.allow_commands = true;
    expanded.config.write_scopes = vec!["*".into()];
    store
        .configure_project(ConfigureProject {
            project_id: a.id.clone(),
            expected_revision: expanded.revision,
            config: expanded.config,
        })
        .unwrap();
    let unchanged = store.task(&next.task.legacy_task_id).unwrap();
    assert!(!unchanged.spec.allow_commands);
    assert_eq!(unchanged.spec.write_scopes, vec!["src"]);
    assert_eq!(
        store
            .task_project_binding(&unchanged.id)
            .unwrap()
            .unwrap()
            .permission_digest,
        frozen.permission_digest
    );
    let original = std::path::PathBuf::from(&a.root_path);
    let moved = temp.path().join("a-moved");
    std::fs::rename(&original, &moved).unwrap();
    code(
        engine.changes(&unchanged.id).unwrap_err(),
        "project_root_unavailable",
    );
    std::fs::create_dir(&original).unwrap();
    code(
        engine.changes(&unchanged.id).unwrap_err(),
        "project_root_changed",
    );
    std::fs::remove_dir(&original).unwrap();
    std::fs::rename(&moved, &original).unwrap();
    let config = store.project_config(&a.id).unwrap();
    store
        .configure_project(ConfigureProject {
            project_id: a.id.clone(),
            expected_revision: config.revision,
            config: ProjectConfig::default(),
        })
        .unwrap();
    let replay = engine
        .start_in_project(StartInProject {
            project_id: a.id.clone(),
            expected_config_revision: 2,
            request: request(),
            idempotency_key: "a-first".into(),
        })
        .await
        .unwrap();
    assert_eq!(replay.id, run.id);
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    code(
        engine
            .resume(&next.task.legacy_task_id, "retry")
            .await
            .unwrap_err(),
        "project_permission_revoked",
    );
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn safe_default_admission_and_task_binding_failure_leave_no_execution_facts() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("db");
    let store = Arc::new(Store::open(&path).unwrap());
    store
        .save_settings(&settings(temp.path(), "http://127.0.0.1:1"))
        .unwrap();
    store.put_secret("route", "fixture-key").unwrap();
    let engine = Engine::new(store.clone(), 4).unwrap();
    code(
        engine.start(request()).await.unwrap_err(),
        "project_selection_required",
    );
    let project = register(&store, temp.path(), "safe");
    store
        .select_project(SelectProject {
            project_id: project.id.clone(),
            expected_revision: 0,
        })
        .unwrap();
    code(
        engine.start(request()).await.unwrap_err(),
        "project_permission_revoked",
    );
    configure(&store, &project.id);
    grant(&store, &project.id);
    let raw = Connection::open(&path).unwrap();
    let tables = [
        "runs",
        "sessions",
        "agents",
        "turns",
        "turn_tasks",
        "tasks",
        "task_project_bindings",
        "task_network_bindings",
        "network_calls",
        "events",
    ];
    for target in ["task_project_bindings", "task_network_bindings"] {
        let before: Vec<u64> = tables
            .iter()
            .map(|t| {
                raw.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
                    .unwrap()
            })
            .collect();
        raw.execute_batch(&format!("CREATE TRIGGER stop_task_binding BEFORE INSERT ON {target} BEGIN SELECT RAISE(ABORT,'fixture task rollback'); END;")).unwrap();
        assert!(
            engine
                .start(request())
                .await
                .unwrap_err()
                .to_string()
                .contains("fixture task rollback")
        );
        let after: Vec<u64> = tables
            .iter()
            .map(|t| {
                raw.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0))
                    .unwrap()
            })
            .collect();
        assert_eq!(after, before, "{target}");
        raw.execute_batch("DROP TRIGGER stop_task_binding").unwrap();
    }
    assert!(!engine.is_busy());
}

#[test]
fn project_config_change_and_explicit_start_are_atomic_across_stores() {
    for _ in 0..8 {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db");
        let store = Arc::new(Store::open(&path).unwrap());
        store
            .save_settings(&settings(temp.path(), "http://127.0.0.1:1"))
            .unwrap();
        store.put_secret("route", "fixture-key").unwrap();
        let project = register(&store, temp.path(), "race");
        configure(&store, &project.id);
        let grant = grant(&store, &project.id);
        let config = store.project_config(&project.id).unwrap();
        let engine = Engine::new(store.clone(), 4).unwrap();
        let other = Store::open(&path).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let start_barrier = barrier.clone();
        let command = StartInProject {
            project_id: project.id.clone(),
            expected_config_revision: config.revision,
            request: request(),
            idempotency_key: id(),
        };
        let started = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            start_barrier.wait();
            let result = runtime.block_on(engine.start_in_project(command));
            // Do not poll the queued worker: this test observes the atomic admission only.
            drop(runtime);
            result
        });
        let revoked = std::thread::spawn(move || {
            barrier.wait();
            other.configure_project(ConfigureProject {
                project_id: project.id,
                expected_revision: config.revision,
                config: ProjectConfig::default(),
            })
        });
        let result = started.join().unwrap();
        revoked.join().unwrap().unwrap();
        let raw = Connection::open(&path).unwrap();
        match result {
            Ok(run) => {
                assert_eq!(run.tasks.len(), 1);
                let task = &run.tasks[0];
                let binding = store.task_project_binding(&task.id).unwrap().unwrap();
                assert_eq!(binding.config_revision, 2);
                assert!(task.spec.tools);
                assert_eq!(task.spec.write_scopes, vec!["src"]);
                let actual: String = raw
                    .query_row(
                        "SELECT authorization_id FROM task_network_bindings WHERE task_id=?1",
                        [&task.id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(actual, grant.authorization_id);
                let mut resumed = task.clone();
                resumed.status = "queued".into();
                assert!(store.resume_task(&resumed, "retry").is_err());
            }
            Err(error) => {
                assert!(
                    matches!(
                        error.downcast_ref::<ProjectError>(),
                        Some(ProjectError::RevisionConflict | ProjectError::PermissionRevoked)
                    ),
                    "{error:#}"
                );
                for table in [
                    "runs",
                    "sessions",
                    "turns",
                    "turn_tasks",
                    "tasks",
                    "task_project_bindings",
                    "task_network_bindings",
                ] {
                    let count: u64 = raw
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                        .unwrap();
                    assert_eq!(count, 0, "{table}");
                }
            }
        }
        assert_eq!(
            raw.query_row("SELECT count(*) FROM network_calls", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
}
