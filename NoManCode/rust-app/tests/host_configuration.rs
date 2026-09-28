#![cfg(windows)]
use peachsh::{budget::*, network_authorization::*, project_config::*};
use peachsh::{
    domain::*,
    engine::Engine,
    host_configuration::*,
    store::{Store, fresh_settings},
};
use rusqlite::{Connection, types::Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
};

fn grant_project(store: &Store, root: &std::path::Path) -> (ProjectId, NetworkAuthorization) {
    std::fs::create_dir_all(root).unwrap();
    let project = store
        .register_project(RegisterProject {
            name: "Fixture project".into(),
            root_path: root.to_string_lossy().into_owned(),
            idempotency_key: id(),
        })
        .unwrap()
        .id;
    let before = store.project_config(&project).unwrap();
    store
        .configure_project(ConfigureProject {
            project_id: project.clone(),
            expected_revision: before.revision,
            config: ProjectConfig {
                allowed_route_ids: vec!["route".into()],
                default_route_id: Some("route".into()),
                tools: false,
                write_scopes: vec![],
                allow_commands: false,
            },
        })
        .unwrap();
    store
        .configure_budget(ConfigureBudget {
            project_id: project.clone(),
            expected_revision: 0,
            idempotency_key: id(),
            config: BudgetConfig {
                limit_tokens: 1000,
                per_call_reservation_tokens: 100,
            },
        })
        .unwrap();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: Some(project.clone()),
    };
    let p = store.network_preview(subject.clone()).unwrap();
    let grant = store
        .network_confirm(
            subject,
            ConfirmRequest {
                config_revision: p.config_revision,
                credential_revision: p.credential_revision,
                binding_digest: p.binding_digest,
                purposes: vec![NetworkPurpose::Models, NetworkPurpose::Completion],
            },
            &id(),
        )
        .unwrap()
        .authorization;
    (project, grant)
}

#[tokio::test]
async fn real_engine_busy_two_projects_old_task_and_unknown_budget_are_preserved() {
    use axum::{
        Json, Router,
        extract::State,
        routing::{get, post},
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::Notify;
    #[derive(Clone)]
    struct Mock {
        hits: Arc<AtomicUsize>,
        hold: Arc<AtomicBool>,
        arrived: Arc<Notify>,
        release: Arc<Notify>,
    }
    async fn complete(State(m): State<Mock>) -> String {
        m.hits.fetch_add(1, Ordering::SeqCst);
        m.arrived.notify_one();
        if m.hold.load(Ordering::SeqCst) {
            m.release.notified().await;
        }
        let chunk = serde_json::json!({"choices":[{"delta":{"content":"323"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}});
        format!("data: {chunk}\n\ndata: [DONE]\n\n")
    }
    let mock = Mock {
        hits: Arc::new(AtomicUsize::new(0)),
        hold: Arc::new(AtomicBool::new(true)),
        arrived: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/chat/completions", post(complete))
        .route(
            "/models",
            get(|State(m): State<Mock>| async move {
                m.hits.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({"data":[{"id":"mock"}]}))
            }),
        )
        .with_state(mock.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (dir, store) = fixture();
    let engine = Engine::new(store.clone(), 3).unwrap();
    let make = |revision| {
        let mut r = request(revision);
        r.route.base_url = base.clone();
        r
    };
    let first = engine
        .configure_host_route("route", make(0), "initial")
        .await
        .unwrap();
    assert_eq!(mock.hits.load(Ordering::SeqCst), 0);
    assert!(store.projects().unwrap().is_empty());
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    for table in [
        "network_authorizations",
        "network_calls",
        "budget_calls",
        "project_budgets",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
    let (p1, g1) = grant_project(&store, &dir.path().join("p1"));
    let (p2, g2) = grant_project(&store, &dir.path().join("p2"));
    store
        .select_project(SelectProject {
            project_id: p1.clone(),
            expected_revision: store.current_project().unwrap().revision,
        })
        .unwrap();
    let run = engine
        .start(RunRequest {
            title: "Configuration fixture".into(),
            kind: SessionKind::Chat,
            tasks: vec![TaskSpec {
                name: "agent".into(),
                role: "developer".into(),
                route_id: "route".into(),
                prompt: "reply".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: false,
                allow_commands: false,
                max_rounds: 1,
            }],
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), mock.arrived.notified())
        .await
        .unwrap();
    assert!(engine.is_busy());
    let before = rows(&db);
    assert_eq!(
        engine
            .configure_host_route("route", make(1), "busy-task")
            .await
            .unwrap_err(),
        HostConfigError::Busy
    );
    assert_eq!(
        engine
            .configure_host_route("route", make(0), "initial")
            .await
            .unwrap()
            .receipt,
        first.receipt
    );
    assert_eq!(rows(&db), before);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 1);
    mock.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while engine.is_busy() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let old_task: String = db
        .query_row("SELECT value FROM tasks WHERE run_id=?1", [&run.id], |r| {
            r.get(0)
        })
        .unwrap();
    let task: Task = serde_json::from_str(&old_task).unwrap();
    assert_eq!(task.status, "completed");
    let e = engine.clone();
    let p = p1.clone();
    let pending = tokio::spawn(async move { e.project_route_check(&p, "route").await });
    tokio::time::timeout(std::time::Duration::from_secs(10), mock.arrived.notified())
        .await
        .unwrap();
    assert!(!engine.is_busy());
    let before = rows(&db);
    assert_eq!(
        engine
            .configure_host_route("route", make(1), "busy-slot")
            .await
            .unwrap_err(),
        HostConfigError::Busy
    );
    assert!(
        engine
            .configure_host_route("route", make(0), "initial")
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(rows(&db), before);
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    store.recover().unwrap();
    mock.release.notify_one();
    let status = store.budget_status(&p1).unwrap();
    assert_eq!(status.unresolved_count, 1);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM budget_calls WHERE state='unknown'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(status.held_tokens, 100);
    assert_eq!(status.spent_tokens, 5);
    let before = rows(&db);
    let mut metadata = make(1);
    metadata.credential = CredentialUpdate::Keep;
    metadata.route.name = "Renamed route".into();
    engine
        .configure_host_route("route", metadata, "metadata")
        .await
        .unwrap();
    assert_eq!(
        store.network_authorization(&g1.authorization_id).unwrap(),
        g1
    );
    assert_eq!(
        store.network_authorization(&g2.authorization_id).unwrap(),
        g2
    );
    let mut rotated = make(2);
    rotated.credential = CredentialUpdate::Replace {
        value: "rotated-fixture".into(),
    };
    db.execute_batch("CREATE TRIGGER fail_atomic_receipt BEFORE INSERT ON host_config_requests BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
    let rollback = rows(&db);
    assert_eq!(
        engine
            .configure_host_route("route", rotated, "rollback")
            .await
            .unwrap_err(),
        HostConfigError::Internal
    );
    assert_eq!(rows(&db), rollback);
    db.execute_batch("DROP TRIGGER fail_atomic_receipt")
        .unwrap();
    let mut rotated = make(2);
    rotated.credential = CredentialUpdate::Replace {
        value: "rotated-fixture".into(),
    };
    engine
        .configure_host_route("route", rotated, "rotate")
        .await
        .unwrap();
    for grant in [g1, g2] {
        let after = store
            .network_authorization(&grant.authorization_id)
            .unwrap();
        let mut expected = grant;
        expected.state = AuthorizationState::Stale;
        assert_eq!(after, expected);
    }
    let after = rows(&db);
    for (table, original) in before {
        if ![
            "config",
            "secrets",
            "host_config_state",
            "host_config_requests",
            "network_authorizations",
            "network_subjects",
        ]
        .contains(&table.as_str())
        {
            assert_eq!(after[&table], original, "preserved {table}");
        }
    }
    assert_eq!(
        db.query_row("SELECT value FROM tasks WHERE run_id=?1", [&run.id], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        old_task
    );
    assert_eq!(store.budget_status(&p1).unwrap().unresolved_count, 1);
    assert_eq!(store.budget_status(&p1).unwrap().held_tokens, 100);
    assert_eq!(store.budget_status(&p2).unwrap().spent_tokens, 0);
    assert_eq!(mock.hits.load(Ordering::SeqCst), 3);
    assert_eq!(
        engine.host_config_operation("initial").unwrap().receipt,
        first.receipt
    );
    server.abort();
    drop(engine);
    drop(store);
    // A controlled, nonempty schema15 fixture, not a claim about an old binary.
    db.execute_batch("DROP TABLE host_config_requests; DROP TABLE host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; PRAGMA user_version=15").unwrap();
    let legacy = rows(&db);
    for _ in 0..2 {
        let reopened = Store::open(&dir.path().join("host.db")).unwrap();
        assert_eq!(reopened.host_config().unwrap().revision, 0);
        assert_eq!(reopened.budget_status(&p1).unwrap().held_tokens, 100);
        let mut migrated = rows(&db);
        migrated.remove("host_config_state");
        migrated.remove("host_config_requests");
        migrated
            .get_mut("schema_migrations")
            .unwrap()
            .retain(|r| r[1] != Value::Text("host-route-configuration-v1".into()));
        assert_eq!(migrated, legacy);
    }
}

#[test]
fn environment_change_does_not_bump_host_revision_but_noop_rechecks_binding() {
    let (dir, store) = fixture();
    let env = format!("NHC_N15_{}", id().replace('-', "_"));
    unsafe {
        std::env::set_var(&env, "env-fixture-before");
    }
    let mut settings = store.settings().unwrap().unwrap();
    settings.routes.push(Route {
        id: "route".into(),
        name: "Local mock".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "mock".into(),
        max_tokens: 128,
        parallel_limit: 2,
        key_env: Some(env.clone()),
    });
    store.save_settings(&settings).unwrap();
    let (_, grant) = grant_project(&store, &dir.path().join("project"));
    let revision = store.host_config().unwrap().revision;
    unsafe {
        std::env::set_var(&env, "env-fixture-after");
    }
    assert_eq!(store.host_config().unwrap().revision, revision);
    let result = store
        .configure_host_route("route", keep(revision), "environment-noop")
        .unwrap();
    assert!(!result.receipt.changed);
    assert_eq!(result.receipt.revision, revision);
    assert_eq!(
        store
            .network_authorization(&grant.authorization_id)
            .unwrap()
            .state,
        AuthorizationState::Stale
    );
    unsafe {
        std::env::remove_var(&env);
    }
}

#[test]
fn legacy_credential_keep_preserves_source_namespace_and_ciphertext() {
    let (dir, store) = fixture();
    let mut settings = store.settings().unwrap().unwrap();
    settings.routes.push(Route {
        id: "route".into(),
        name: "Local mock".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "mock".into(),
        max_tokens: 128,
        parallel_limit: 2,
        key_env: None,
    });
    store.save_settings(&settings).unwrap();
    store.put_secret("route", "legacy-only-fixture").unwrap();
    let revision = store.host_config().unwrap().revision;
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let before = rows(&db)["secrets"].clone();
    let result = store
        .configure_host_route("route", keep(revision), "legacy-keep")
        .unwrap();
    assert!(!result.receipt.changed);
    assert_eq!(
        result.receipt.route.credential_source,
        CredentialSource::Legacy
    );
    assert_eq!(rows(&db)["secrets"], before);
}

#[tokio::test]
async fn legacy_engine_configuration_and_account_binding_each_advance_once() {
    let (_dir, store) = fixture();
    let engine = Engine::new(store.clone(), 3).unwrap();
    engine
        .configure_host_route("route", request(0), "initial")
        .await
        .unwrap();
    let mut settings = store.settings().unwrap().unwrap();
    settings.max_concurrency = 2;
    engine
        .configure_with_keys(
            settings.clone(),
            std::collections::HashMap::from([("route".into(), "ui-fixture".into())]),
        )
        .await
        .unwrap();
    assert_eq!(store.host_config().unwrap().revision, 2);
    engine
        .configure_with_keys(
            settings,
            std::collections::HashMap::from([("route".into(), "ui-fixture".into())]),
        )
        .await
        .unwrap();
    assert_eq!(store.host_config().unwrap().revision, 2);
    engine
        .bind_account(
            Account {
                base_url: "http://127.0.0.1:9".into(),
                user_id: "1".into(),
                quota_per_unit: 500_000.0,
            },
            Some("account-fixture".into()),
        )
        .await
        .unwrap();
    assert_eq!(store.host_config().unwrap().revision, 3);
    assert_eq!(
        engine
            .configure_host_route("route", request(2), "old-cas")
            .await
            .unwrap_err(),
        HostConfigError::RevisionConflict
    );
    let preserved = store.settings().unwrap().unwrap();
    engine
        .configure_host_route("other", request(3), "new-route")
        .await
        .unwrap();
    let after = store.settings().unwrap().unwrap();
    assert_eq!(after.newapi, preserved.newapi);
    assert_eq!(after.workspace, preserved.workspace);
    assert_eq!(after.max_concurrency, preserved.max_concurrency);
    assert_eq!(
        serde_json::to_value(&after.routes[0]).unwrap(),
        serde_json::to_value(&preserved.routes[0]).unwrap()
    );
}

fn fixture() -> (tempfile::TempDir, Arc<Store>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("host.db")).unwrap());
    fresh_settings(&store, dir.path()).unwrap();
    (dir, store)
}
#[test]
fn missing_settings_with_project_history_is_not_a_fresh_host() {
    let (dir, store) = fixture();
    store
        .register_project(RegisterProject {
            name: "Existing project".into(),
            root_path: dir.path().to_string_lossy().into_owned(),
            idempotency_key: "existing".into(),
        })
        .unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    db.execute("DELETE FROM config WHERE id='settings'", [])
        .unwrap();
    let before = rows(&db);
    assert_eq!(store.host_config().unwrap_err(), HostConfigError::Corrupt);
    assert!(fresh_settings(&store, dir.path()).is_err());
    assert_eq!(rows(&db), before);
}
#[test]
fn missing_receipt_field_and_wrong_length_sealed_digest_are_corrupt() {
    let (dir, store) = fixture();
    let result = store
        .configure_host_route("route", request(0), "first")
        .unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let mut receipt = serde_json::to_value(&result.receipt).unwrap();
    receipt["route"].as_object_mut().unwrap().remove("key_env");
    db.execute(
        "UPDATE host_config_requests SET receipt=?1",
        [receipt.to_string()],
    )
    .unwrap();
    assert_eq!(
        store.host_config_operation("first").unwrap_err(),
        HostConfigError::Corrupt
    );
    db.execute(
        "UPDATE host_config_requests SET receipt=?1, request_fingerprint_blob=?2",
        rusqlite::params![
            serde_json::to_string(&result.receipt).unwrap(),
            peachsh::secrets::seal(b"wrong-size").unwrap()
        ],
    )
    .unwrap();
    assert_eq!(
        store.host_config_operation("first").unwrap_err(),
        HostConfigError::Corrupt
    );
}
fn request(revision: u64) -> ConfigureHostRoute {
    ConfigureHostRoute {
        expected_revision: revision,
        route: RouteInput {
            name: "Local mock".into(),
            base_url: "http://127.0.0.1:9/v1/".into(),
            model: "mock".into(),
            max_tokens: 128,
            parallel_limit: 2,
        },
        credential: CredentialUpdate::Replace {
            value: "fixture-value-n15".into(),
        },
    }
}
fn keep(revision: u64) -> ConfigureHostRoute {
    let mut r = request(revision);
    r.credential = CredentialUpdate::Keep;
    r
}
fn rows(db: &Connection) -> BTreeMap<String, Vec<Vec<Value>>> {
    let names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let values = db
                .prepare(&format!("SELECT rowid,* FROM \"{name}\" ORDER BY rowid"))
                .unwrap()
                .query_map([], |r| {
                    (0..r.as_ref().column_count()).map(|i| r.get(i)).collect()
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (name, values)
        })
        .collect()
}

#[test]
fn fresh_initialization_is_empty_revision_zero_and_cannot_overwrite_history() {
    let (dir, store) = fixture();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    assert_eq!(
        store.host_config().unwrap(),
        HostConfigSnapshot {
            revision: 0,
            routes: vec![]
        }
    );
    assert_eq!(store.settings().unwrap().unwrap().max_concurrency, 3);
    let before = rows(&db);
    fresh_settings(&store, &dir.path().join("another")).unwrap();
    assert_eq!(rows(&db), before);
    assert!(!dir.path().join("another").exists());
    store
        .configure_host_route("route", request(0), "first")
        .unwrap();
    db.execute("DELETE FROM config WHERE id='settings'", [])
        .unwrap();
    let before = rows(&db);
    assert!(fresh_settings(&store, dir.path()).is_err());
    assert_eq!(rows(&db), before);
}
#[test]
fn durable_replay_lookup_and_noop_preserve_original_receipt() {
    let (dir, store) = fixture();
    let first = store
        .configure_host_route("route", request(0), "operation-a")
        .unwrap();
    assert_eq!(first.receipt.revision, 1);
    assert!(first.receipt.changed);
    assert!(!first.replayed);
    assert_eq!(
        first.receipt.route.credential_source,
        CredentialSource::Stored
    );
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let before = rows(&db);
    let again = store
        .configure_host_route("route", request(0), "operation-a")
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.receipt, first.receipt);
    assert_eq!(rows(&db), before);
    let noop = store
        .configure_host_route("route", request(1), "operation-b")
        .unwrap();
    assert!(!noop.receipt.changed);
    assert_eq!(noop.receipt.revision, 1);
    let mut change = keep(1);
    change.route.name = "Updated display".into();
    store
        .configure_host_route("route", change, "operation-c")
        .unwrap();
    assert_eq!(store.host_config().unwrap().revision, 2);
    drop(store);
    for _ in 0..2 {
        let reopened = Store::open(&dir.path().join("host.db")).unwrap();
        assert_eq!(
            reopened
                .host_config_operation("operation-a")
                .unwrap()
                .receipt,
            first.receipt
        );
        assert!(
            reopened
                .host_config_operation("operation-b")
                .unwrap()
                .replayed
        );
        assert_eq!(reopened.host_config().unwrap().revision, 2);
    }
}
#[test]
fn idempotency_conflicts_cover_secret_route_expected_and_mode_without_writes() {
    let (dir, store) = fixture();
    store
        .configure_host_route("route", request(0), "same-key")
        .unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let before = rows(&db);
    let mut secret = request(0);
    secret.credential = CredentialUpdate::Replace {
        value: "different-fixture".into(),
    };
    let mut model = request(0);
    model.route.model = "other".into();
    for (id, r) in [
        ("route", secret),
        ("other-route", request(0)),
        ("route", request(1)),
        ("route", keep(0)),
        ("route", model),
    ] {
        assert_eq!(
            store.configure_host_route(id, r, "same-key").unwrap_err(),
            HostConfigError::IdempotencyConflict
        );
        assert_eq!(rows(&db), before);
    }
    assert_eq!(
        store.host_config_operation("not-seen").unwrap_err(),
        HostConfigError::NotFound
    );
    assert_eq!(rows(&db), before);
}
#[test]
fn two_stores_same_key_converge_and_different_keys_cas_atomically() {
    for same in [true, false] {
        let (dir, a) = fixture();
        let b = Arc::new(Store::open(&dir.path().join("host.db")).unwrap());
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = [a.clone(), b]
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    s.configure_host_route(
                        "route",
                        request(0),
                        if same || i == 0 { "a" } else { "b" },
                    )
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(a.host_config().unwrap().revision, 1);
        if same {
            assert_eq!(
                results[0].as_ref().unwrap().receipt,
                results[1].as_ref().unwrap().receipt
            );
            assert_eq!(
                results
                    .iter()
                    .filter(|r| r.as_ref().unwrap().replayed)
                    .count(),
                1
            );
        } else {
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
            assert_eq!(
                *results.iter().find_map(|r| r.as_ref().err()).unwrap(),
                HostConfigError::RevisionConflict
            );
        }
        let db = Connection::open(dir.path().join("host.db")).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM host_config_requests", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
    }
}
#[test]
fn same_key_different_secret_race_has_one_winner_and_two_noops_can_coexist() {
    let (dir, a) = fixture();
    let b = Arc::new(Store::open(&dir.path().join("host.db")).unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = [a.clone(), b.clone()]
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut r = request(0);
                r.credential = CredentialUpdate::Replace {
                    value: format!("fixture-{i}"),
                };
                barrier.wait();
                s.configure_host_route("route", r, "race")
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        *results.iter().find_map(|r| r.as_ref().err()).unwrap(),
        HostConfigError::IdempotencyConflict
    );
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = [a.clone(), b]
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                s.configure_host_route("route", keep(1), &format!("noop-{i}"))
            })
        })
        .collect();
    barrier.wait();
    for h in handles {
        let result = h.join().unwrap().unwrap();
        assert!(!result.receipt.changed);
        assert_eq!(result.receipt.revision, 1);
    }
}
#[test]
fn strict_request_and_target_validation_is_shared_by_store() {
    let (_dir, store) = fixture();
    for url in [
        "http://localhost/v1",
        "http://example.invalid/v1",
        "http://127.1/v1",
        "https://user:pass@example.invalid",
        "https://example.invalid/a/%2e%2e/x",
        "https://example.invalid/a%2fb",
        "https://example.invalid?q=1",
        "https://example.invalid#fragment",
    ] {
        let mut r = request(0);
        r.route.base_url = url.into();
        assert_eq!(
            store
                .configure_host_route("route", r, "invalid")
                .unwrap_err(),
            HostConfigError::Invalid
        );
    }
    for value in ["", " space", "line\nline", "tab\tvalue"] {
        let mut r = request(0);
        r.credential = CredentialUpdate::Replace {
            value: value.into(),
        };
        assert_eq!(
            store
                .configure_host_route("route", r, "invalid")
                .unwrap_err(),
            HostConfigError::Invalid
        );
    }
    for raw in [
        r#"{"mode":"keep","value":null}"#,
        r#"{"mode":"keep","value":"x"}"#,
        r#"{"mode":"replace"}"#,
        r#"{"mode":"replace","value":"x","extra":1}"#,
    ] {
        assert!(
            serde_json::from_str::<CredentialUpdate>(raw).is_err(),
            "strict tagged union"
        );
    }
    assert_eq!(
        store
            .configure_host_route("newapi-account", request(0), "invalid")
            .unwrap_err(),
        HostConfigError::Invalid
    );
    assert_eq!(store.host_config().unwrap().revision, 0);
    let mut r = request(0);
    r.route.base_url = "http://[::1]:123/v1/".into();
    assert_eq!(
        r.validate("route", "valid").unwrap().base_url,
        "http://[::1]:123/v1"
    );
}
#[test]
fn keep_requires_existing_available_credential_and_same_api_base() {
    let (_dir, store) = fixture();
    assert_eq!(
        store
            .configure_host_route("route", keep(0), "keep-new")
            .unwrap_err(),
        HostConfigError::CredentialRequired
    );
    store
        .configure_host_route("route", request(0), "create")
        .unwrap();
    let mut r = keep(1);
    r.route.base_url = "https://different.invalid/v1".into();
    assert_eq!(
        store
            .configure_host_route("route", r, "keep-moved")
            .unwrap_err(),
        HostConfigError::CredentialRequired
    );
    let mut r = request(1);
    r.route.base_url = "https://different.invalid/v1".into();
    assert_eq!(
        store
            .configure_host_route("route", r, "replace-moved")
            .unwrap()
            .receipt
            .revision,
        2
    );
}
#[test]
fn replacement_clears_env_and_keeps_legacy_ciphertext() {
    let (dir, store) = fixture();
    let name = format!("NHC_N15_{}", id().replace('-', "_"));
    // Unique name belongs solely to this test; no other task reads or writes it.
    unsafe {
        std::env::set_var(&name, "synthetic-environment-value");
    }
    let mut settings = store.settings().unwrap().unwrap();
    settings.routes.push(Route {
        id: "route".into(),
        name: "Local mock".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "mock".into(),
        max_tokens: 128,
        parallel_limit: 2,
        key_env: Some(name.clone()),
    });
    store.save_settings(&settings).unwrap();
    store.put_secret("route", "legacy-fixture").unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let blob: Vec<u8> = db
        .query_row("SELECT value FROM secrets WHERE id='route'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        store.host_config().unwrap().routes[0].credential_source,
        CredentialSource::Environment
    );
    let revision = store.host_config().unwrap().revision;
    assert_eq!(
        store
            .configure_host_route("route", keep(revision), "keep-env")
            .unwrap()
            .receipt
            .route
            .key_env,
        Some(name.clone())
    );
    let revision = store.host_config().unwrap().revision;
    let result = store
        .configure_host_route("route", request(revision), "replace-env")
        .unwrap();
    assert_eq!(result.receipt.route.key_env, None);
    assert_eq!(
        result.receipt.route.credential_source,
        CredentialSource::Stored
    );
    assert_eq!(
        Engine::new(store.clone(), 3)
            .unwrap()
            .key(&store.settings().unwrap().unwrap().routes[0])
            .unwrap(),
        "fixture-value-n15"
    );
    assert_eq!(
        db.query_row("SELECT value FROM secrets WHERE id='route'", [], |r| r
            .get::<_, Vec<u8>>(0))
            .unwrap(),
        blob
    );
    unsafe {
        std::env::remove_var(&name);
    }
}
#[test]
fn unavailable_secret_and_damaged_receipt_fail_closed() {
    let (dir, store) = fixture();
    store
        .configure_host_route("route", request(0), "first")
        .unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    db.execute(
        "UPDATE secrets SET value=X'000102' WHERE id='route::route'",
        [],
    )
    .unwrap();
    assert_eq!(
        store.host_config().unwrap().routes[0].credential_source,
        CredentialSource::Unavailable
    );
    let before = rows(&db);
    assert_eq!(
        store
            .configure_host_route("route", keep(1), "keep-bad")
            .unwrap_err(),
        HostConfigError::Unavailable
    );
    assert_eq!(
        store
            .configure_host_route("route", request(1), "replace-bad")
            .unwrap_err(),
        HostConfigError::Unavailable
    );
    assert_eq!(rows(&db), before);
    db.execute(
        "UPDATE host_config_requests SET request_fingerprint_blob=X'010203'",
        [],
    )
    .unwrap();
    assert_eq!(
        store.host_config_operation("first").unwrap_err(),
        HostConfigError::Unavailable
    );
    assert_eq!(
        store
            .configure_host_route("route", request(0), "first")
            .unwrap_err(),
        HostConfigError::Unavailable
    );
    assert!(Store::open(&dir.path().join("host.db")).is_err());
}
#[test]
fn compatibility_writes_invalidate_cas_without_receipts_and_noop_keeps_revision() {
    let (_dir, store) = fixture();
    let mut settings = store.settings().unwrap().unwrap();
    settings.max_concurrency = 2;
    store.save_settings(&settings).unwrap();
    assert_eq!(store.host_config().unwrap().revision, 1);
    store.save_settings(&settings).unwrap();
    assert_eq!(store.host_config().unwrap().revision, 1);
    assert_eq!(
        store
            .configure_host_route("route", request(0), "stale")
            .unwrap_err(),
        HostConfigError::RevisionConflict
    );
    store
        .put_secret("newapi-account", "account-fixture")
        .unwrap();
    assert_eq!(store.host_config().unwrap().revision, 2);
    store
        .put_secret("newapi-account", "account-fixture")
        .unwrap();
    assert_eq!(store.host_config().unwrap().revision, 2);
    assert_eq!(
        store.host_config_operation("stale").unwrap_err(),
        HostConfigError::NotFound
    );
}
#[test]
fn receipt_insert_failure_and_revision_overflow_roll_back_every_row() {
    let (dir, store) = fixture();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON host_config_requests BEGIN SELECT RAISE(ABORT,'synthetic failure'); END").unwrap();
    let before = rows(&db);
    assert_eq!(
        store
            .configure_host_route("route", request(0), "fail")
            .unwrap_err(),
        HostConfigError::Internal
    );
    assert_eq!(rows(&db), before);
    db.execute_batch(
        "DROP TRIGGER fail_receipt; UPDATE host_config_state SET revision=9223372036854775807",
    )
    .unwrap();
    let before = rows(&db);
    assert_eq!(
        store
            .configure_host_route("route", request(i64::MAX as u64), "overflow")
            .unwrap_err(),
        HostConfigError::Corrupt
    );
    assert_eq!(rows(&db), before);
}
#[test]
fn stored_request_contains_only_sealed_digest_and_public_receipt() {
    use sha2::{Digest, Sha256};
    let (dir, store) = fixture();
    let result = store
        .configure_host_route("route", request(0), "safe-operation")
        .unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let (sealed, receipt): (Vec<u8>, String) = db
        .query_row(
            "SELECT request_fingerprint_blob,receipt FROM host_config_requests",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_ne!(sealed.len(), 32);
    assert_eq!(peachsh::secrets::open(&sealed).unwrap().len(), 32);
    let output = serde_json::to_string(&result).unwrap();
    for text in [&receipt, &output] {
        assert!(!text.contains("fixture-value-n15"));
        assert!(!text.contains("safe-operation"));
        assert!(!text.contains("fingerprint"));
    }
    let raw_digest = Sha256::digest(b"fixture-value-n15");
    drop(db);
    drop(store);
    for suffix in ["host.db", "host.db-wal"] {
        if let Ok(bytes) = std::fs::read(dir.path().join(suffix)) {
            assert!(
                !bytes
                    .windows(b"fixture-value-n15".len())
                    .any(|w| w == b"fixture-value-n15")
            );
            assert!(!bytes.windows(32).any(|w| w == raw_digest.as_slice()));
        }
    }
}
