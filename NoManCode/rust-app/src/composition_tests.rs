use super::*;
use crate::{plugin_management::SetPluginEnabled, runtime_management::RuntimeId};
use std::time::Duration;

fn task(workspace: &std::path::Path) -> Task {
    Task {
        id: "composition-task".into(),
        run_id: "composition-run".into(),
        spec: TaskSpec {
            name: "worker".into(),
            role: "worker".into(),
            route_id: "route".into(),
            prompt: "read".into(),
            depends_on: vec![],
            write_scopes: vec![],
            tools: false,
            allow_commands: false,
            max_rounds: 2,
        },
        route: Route {
            id: "route".into(),
            name: "route".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "mock".into(),
            max_tokens: 64,
            parallel_limit: 1,
            key_env: None,
        },
        workspace: workspace.to_string_lossy().into_owned(),
        status: "running".into(),
        output: String::new(),
        error: None,
        messages: vec![json!({"role":"user","content":"read"})],
        usage: Value::Null,
        created_at: 1,
        updated_at: 1,
    }
}

#[test]
fn persisted_four_domains_remain_disabled_after_reconstruction() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("host.db");
    let engine = Engine::new(Arc::new(Store::open(&db).unwrap()), 1).unwrap();
    for (id, key) in [
        ("builtin.files", "disable-files"),
        ("builtin.shell", "disable-shell"),
    ] {
        engine
            .set_plugin_enabled(
                id,
                SetPluginEnabled {
                    enabled: false,
                    expected_revision: 0,
                },
                key,
            )
            .unwrap();
    }
    for (id, key) in [
        (RuntimeId::ChatAgent, "disable-agent"),
        (RuntimeId::CompletionProvider, "disable-provider"),
    ] {
        engine
            .set_runtime_enabled(
                id,
                SetPluginEnabled {
                    enabled: false,
                    expected_revision: 0,
                },
                key,
            )
            .unwrap();
    }
    drop(engine);
    let engine = Engine::new(Arc::new(Store::open(&db).unwrap()), 1).unwrap();
    let snapshot = serde_json::to_value(engine.composition().unwrap()).unwrap();
    for entry in snapshot["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .chain(snapshot["runtimes"].as_array().unwrap())
    {
        assert_eq!(entry["desired"]["enabled"], false, "{entry}");
        assert_eq!(entry["desired"]["revision"], 1, "{entry}");
        assert_eq!(entry["runtime"]["admission_open"], false, "{entry}");
        assert_eq!(entry["converged"], true, "{entry}");
    }
}

#[test]
fn runtime_state_read_failure_fails_composition_query_closed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("host.db");
    let engine = Engine::new(Arc::new(Store::open(&db).unwrap()), 1).unwrap();
    engine.composition().unwrap();
    assert_eq!(
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "DELETE FROM plugin_desired WHERE plugin_id='builtin.chat-agent'",
                []
            )
            .unwrap(),
        1
    );
    assert!(engine.composition().is_err());
}

#[test]
fn missing_injected_provider_binding_does_not_use_default_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("host.db")).unwrap());
    let host = crate::plugin_host::PluginHost::new(crate::plugin_catalog::ScopeKey::Host).unwrap();
    let runtime =
        crate::provider_runtime::ProviderRuntime::from_host_with_state_for_test(host, false)
            .unwrap();
    let engine = Engine::new_with_provider_runtime_for_test(store, 1, runtime).unwrap();
    let error = match engine.composition() {
        Ok(_) => panic!("missing provider binding unexpectedly yielded a composition"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("provider binding missing"));
}

#[test]
fn ordinary_constructor_does_not_recover_but_recovered_constructor_does() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("host.db");
    let store = Arc::new(Store::open(&db).unwrap());
    let task = task(dir.path());
    store
        .create_run(&Run {
            id: task.run_id.clone(),
            title: "recovery".into(),
            kind: SessionKind::Team,
            created_at: 1,
            tasks: vec![task.clone()],
        })
        .unwrap();
    store
        .event(
            &task.id,
            "tool_result",
            json!({"name":"read_file","tool_call_id":"old","result":{"content":"preserved"}}),
        )
        .unwrap();
    let original = serde_json::to_value(
        store
            .events(&task.run_id, 0)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "tool_result")
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let engine = Engine::new(store.clone(), 1).unwrap();
    assert_eq!(engine.store.task(&task.id).unwrap().status, "running");
    drop(engine);
    let (engine, recovered) = Engine::new_recovered(store.clone(), 1).unwrap();
    assert_eq!(recovered, 1);
    assert_eq!(engine.store.task(&task.id).unwrap().status, "interrupted");
    let events = store.events(&task.run_id, 0).unwrap();
    assert_eq!(
        serde_json::to_value(
            events
                .iter()
                .filter(|e| e.kind == "tool_result")
                .collect::<Vec<_>>()
        )
        .unwrap(),
        original
    );
    assert!(!events.iter().any(|e| e.kind == "tool_start"));
    drop(engine);
    let (_, recovered_again) = Engine::new_recovered(store, 1).unwrap();
    assert_eq!(recovered_again, 0);
}

#[tokio::test]
async fn ordinary_engine_review_uses_same_gate_as_composition() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(
        Arc::new(Store::open(&dir.path().join("host.db")).unwrap()),
        1,
    )
    .unwrap();
    assert!(Arc::ptr_eq(&engine.store, &engine.composition.store));
    assert!(Arc::ptr_eq(
        &engine.gate,
        &engine.composition.workspace.gate
    ));
    assert!(Arc::ptr_eq(&engine.active, &engine.composition.active));
    assert!(Arc::ptr_eq(&engine.tool_runtime, &engine.composition.tools));
    assert!(Arc::ptr_eq(
        &engine.provider_runtime,
        &engine.composition.provider
    ));
    assert!(Arc::ptr_eq(
        &engine.agent_runtime,
        &engine.composition.agent
    ));
    let guard = engine.gate.lock().await;
    let request = crate::git_diff::GitDiffRequest {
        path: "a.txt".into(),
        view: crate::git_diff::GitDiffView::Unstaged,
    };
    let call = engine.git_diff("missing-task", request);
    tokio::pin!(call);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut call)
            .await
            .is_err()
    );
    drop(guard);
    assert!(call.await.is_err());
}

#[tokio::test]
async fn review_and_restore_stay_available_with_tools_disabled() {
    use axum::{Json, Router, extract::State, routing::post};
    use std::sync::atomic::{AtomicUsize, Ordering};
    async fn reply(
        State(calls): State<Arc<AtomicUsize>>,
        Json(_): Json<Value>,
    ) -> ([(&'static str, &'static str); 1], String) {
        let delta = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            json!({"tool_calls":[{"index":0,"id":"write-one","type":"function","function":{"name":"write_file","arguments":json!({"path":"src/a.txt","content":"after"}).to_string()}}]})
        } else {
            json!({"content":"done"})
        };
        let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
        (
            [("content-type", "text/event-stream")],
            format!("data: {chunk}\n\ndata: [DONE]\n\n"),
        )
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.txt"), "before").unwrap();
    for args in [&["init", "-q"][..], &["add", "--", "src/a.txt"][..]] {
        assert!(
            std::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(reply))
        .with_state(Arc::new(AtomicUsize::new(0)));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = Arc::new(Store::open(&root.join("host.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: root.to_string_lossy().into_owned(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "route".into(),
                base_url: base,
                model: "mock".into(),
                max_tokens: 64,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "synthetic-key").unwrap();
    configure_fixture_project(&store);
    let subject = route_subject(&store, "route");
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
            &id(),
        )
        .unwrap();
    let engine = Engine::new(store, 2).unwrap();
    let run = engine
        .start(RunRequest {
            title: "review".into(),
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
                max_rounds: 3,
            }],
        })
        .await
        .unwrap();
    let task_id = &run.tasks[0].id;
    let pending = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(a) = engine
                .store
                .pending_approvals_for_task(task_id)
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
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.store.task(task_id).unwrap().status, "completed");
    assert_eq!(std::fs::read(root.join("src/a.txt")).unwrap(), b"after");
    engine.disable_builtin_files().unwrap();
    engine.disable_builtin_shell().unwrap();
    let request = crate::git_diff::GitDiffRequest {
        path: "src/a.txt".into(),
        view: crate::git_diff::GitDiffView::Unstaged,
    };
    let diff = engine.git_diff(task_id, request.clone()).await.unwrap();
    assert_eq!(diff.status, crate::git_diff::GitDiffStatus::Text);
    assert!(diff.patch.unwrap().contains("after"));
    engine.active.lock().unwrap().insert(
        task_id.clone(),
        Active {
            cancel: CancellationToken::new(),
            workspace: root.to_string_lossy().into_owned(),
            scopes: vec!["src".into()],
        },
    );
    assert!(matches!(
        engine
            .git_diff(task_id, request)
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Active)
    ));
    assert!(matches!(
        engine
            .create_checkpoint(task_id, "review-checkpoint")
            .await
            .unwrap_err()
            .downcast_ref::<crate::checkpoint::CheckpointError>(),
        Some(crate::checkpoint::CheckpointError::Conflict)
    ));
    engine.active.lock().unwrap().remove(task_id);
    let checkpoint = engine
        .create_checkpoint(task_id, "review-checkpoint")
        .await
        .unwrap()
        .checkpoint;
    assert_eq!(
        engine.checkpoint(&checkpoint.checkpoint_id).unwrap(),
        checkpoint
    );
    let receipt = engine
        .restore_checkpoint(&checkpoint.checkpoint_id)
        .await
        .unwrap();
    assert_eq!(receipt.status, RestoreStatus::Complete);
    assert_eq!(std::fs::read(root.join("src/a.txt")).unwrap(), b"before");
    server.abort();
}
