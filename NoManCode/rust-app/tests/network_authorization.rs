#[path = "support/project_fixture.rs"]
#[allow(dead_code)]
mod project_fixture;
use peachsh::{domain::*, engine::Engine, network_authorization::*, store::Store};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Mock {
    base: String,
    hits: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Mock {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let count = hits.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    loop {
                        let mut chunk = [0; 4096];
                        let n = stream.read(&mut chunk).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..n]);
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&bytes[..end]);
                            let size = header
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + size {
                                break;
                            }
                        }
                    }
                    let request = String::from_utf8_lossy(&bytes);
                    let body = if request.starts_with("GET /models ") {
                        r#"{"data":[{"id":"mock"}]}"#.to_string()
                    } else if request.starts_with("GET /api/user/self ") {
                        r#"{"success":true,"data":{"quota":100}}"#.to_string()
                    } else {
                        let chunk =
                            json!({"choices":[{"delta":{"content":"323"},"finish_reason":"stop"}]});
                        format!("data: {chunk}\n\ndata: [DONE]\n\n")
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        Self { base, hits, task }
    }
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn subject(engine: &Engine) -> NetworkSubject {
    NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: engine.current_project().unwrap().project_id,
    }
}
fn setup(root: &std::path::Path, base: &str) -> Arc<Engine> {
    let store = Arc::new(Store::open(&root.join("network.db")).unwrap());
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
    store
        .put_secret("route::route", "fixture-network-secret")
        .unwrap();
    project_fixture::configured_project(&store);
    Engine::new(store, 2).unwrap()
}
fn request() -> RunRequest {
    RunRequest {
        title: "network".into(),
        kind: SessionKind::Chat,
        tasks: vec![TaskSpec {
            name: "worker".into(),
            role: "worker".into(),
            route_id: "route".into(),
            prompt: "compute".into(),
            depends_on: vec![],
            write_scopes: vec!["src".into()],
            tools: false,
            allow_commands: false,
            max_rounds: 3,
        }],
    }
}
fn confirm_request(p: &NetworkPreview, purposes: Vec<NetworkPurpose>) -> ConfirmRequest {
    ConfirmRequest {
        config_revision: p.config_revision,
        credential_revision: p.credential_revision,
        binding_digest: p.binding_digest.clone(),
        purposes,
    }
}
async fn grant(e: &Engine, purposes: Vec<NetworkPurpose>) -> NetworkAuthorization {
    let p = e.network_preview(subject(&e)).await.unwrap();
    e.network_confirm(subject(&e), confirm_request(&p, purposes), &id())
        .await
        .unwrap()
        .authorization
}
async fn settled(e: &Engine, id: &str) -> Task {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let t = e.store.task(id).unwrap();
            if !e.is_busy() {
                return t;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
fn denied(error: NetworkError, kind: NetworkErrorKind) {
    assert_eq!(error.kind, kind);
    assert_eq!(error.dispatch_state, DispatchState::NotSent);
    assert!(error.calls.is_empty());
}

#[tokio::test]
async fn no_grant_purpose_or_revocation_has_zero_tcp_with_real_positive_control() {
    let mock = Mock::new().await;
    let other = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    assert!(e.store.secret("route::route").unwrap().is_some());
    denied(
        e.route_models("route").await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    denied(
        e.route_check("route").await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    let err = e.start(request()).await.unwrap_err();
    assert_eq!(
        err.downcast_ref::<NetworkError>().unwrap().kind,
        NetworkErrorKind::Required
    );
    assert_eq!((mock.hits(), other.hits()), (0, 0));
    let models = grant(&e, vec![NetworkPurpose::Models]).await;
    denied(
        e.route_check("route").await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    assert!(e.start(request()).await.is_err());
    assert_eq!(mock.hits(), 0);
    let response = e.route_models("route").await.unwrap();
    assert_eq!(response.result, vec!["mock"]);
    assert_eq!(response.calls[0].state, CallState::Finished);
    assert_eq!(mock.hits(), 1);
    e.network_revoke(&models.authorization_id).await.unwrap();
    assert!(e.route_models("route").await.is_err());
    assert_eq!(mock.hits(), 1);
    grant(&e, vec![NetworkPurpose::Models, NetworkPurpose::Completion]).await;
    let response = e.route_check("route").await.unwrap();
    assert_eq!(response.result["ok"], true);
    assert_eq!(
        response.calls.iter().map(|r| r.purpose).collect::<Vec<_>>(),
        vec![NetworkPurpose::Models, NetworkPurpose::Completion]
    );
    let task = e.start(request()).await.unwrap().tasks.remove(0);
    assert_eq!(settled(&e, &task.id).await.status, "completed");
    assert_eq!(e.network_calls_for_task(&task.id).unwrap().len(), 1);
    assert_eq!(mock.hits(), 4);
    assert_eq!(other.hits(), 0);
    let e2 = setup(&tempfile::tempdir().unwrap().keep(), &other.base);
    grant(&e2, vec![NetworkPurpose::Models]).await;
    e2.route_models("route").await.unwrap();
    assert_eq!(other.hits(), 1);
}
#[tokio::test]
async fn idempotency_preserves_revoked_fact_and_rejects_cross_subject_fk_corruption() {
    let mock = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    let p = e.network_preview(subject(&e)).await.unwrap();
    let req = confirm_request(&p, vec![NetworkPurpose::Models, NetworkPurpose::Models]);
    let a = e
        .network_confirm(subject(&e), req.clone(), "confirm-one")
        .await
        .unwrap();
    assert!(!a.replayed);
    let replay = e
        .network_confirm(subject(&e), req.clone(), "confirm-one")
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(a.authorization, replay.authorization);
    e.network_revoke(&a.authorization.authorization_id)
        .await
        .unwrap();
    let replay = e
        .network_confirm(subject(&e), req.clone(), "confirm-one")
        .await
        .unwrap();
    assert_eq!(replay.authorization.state, AuthorizationState::Revoked);
    let mut different = req.clone();
    different.purposes = vec![NetworkPurpose::Completion];
    denied(
        e.network_confirm(subject(&e), different, "confirm-one")
            .await
            .unwrap_err(),
        NetworkErrorKind::Conflict,
    );
    let mut settings = e.store.settings().unwrap().unwrap();
    let mut route = settings.routes[0].clone();
    route.id = "other".into();
    settings.routes.push(route);
    e.configure_with_keys(
        settings,
        std::collections::HashMap::from([("other".into(), "fixture-other-secret".into())]),
    )
    .await
    .unwrap();
    project_fixture::configured_project(&e.store);
    let other = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "other".into(),
        project_id: e.current_project().unwrap().project_id,
    };
    let p2 = e.network_preview(other.clone()).await.unwrap();
    let b = e
        .network_confirm(
            other,
            confirm_request(&p2, vec![NetworkPurpose::Models]),
            "other-confirm",
        )
        .await
        .unwrap();
    let db = Connection::open(dir.path().join("network.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    db.execute(
        "UPDATE network_confirmations SET authorization_id=?1 WHERE creation_key='confirm-one'",
        [&b.authorization.authorization_id],
    )
    .unwrap();
    denied(
        e.network_confirm(subject(&e), req, "confirm-one")
            .await
            .unwrap_err(),
        NetworkErrorKind::Corrupt,
    );
    assert_eq!(mock.hits(), 0);
}
#[tokio::test]
async fn raw_secret_and_settings_changes_cannot_bypass_bound_snapshot() {
    let mock = Mock::new().await;
    let other = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    let a = grant(&e, vec![NetworkPurpose::Models, NetworkPurpose::Completion]).await;
    e.store
        .put_secret("route::route", "fixture-changed-secret")
        .unwrap();
    denied(
        e.route_models("route").await.unwrap_err(),
        NetworkErrorKind::Stale,
    );
    assert_eq!(mock.hits(), 0);
    let p = e.network_preview(subject(&e)).await.unwrap();
    assert!(p.credential_revision > a.credential_revision);
    assert_eq!(
        e.network_authorization(&a.authorization_id).unwrap().state,
        AuthorizationState::Stale
    );
    grant(&e, vec![NetworkPurpose::Models]).await;
    e.route_models("route").await.unwrap();
    let mut settings = e.store.settings().unwrap().unwrap();
    settings.routes[0].base_url = other.base.clone();
    e.store.save_settings(&settings).unwrap();
    denied(
        e.route_models("route").await.unwrap_err(),
        NetworkErrorKind::Stale,
    );
    assert_eq!((mock.hits(), other.hits()), (1, 0));
    grant(&e, vec![NetworkPurpose::Models]).await;
    e.route_models("route").await.unwrap();
    assert_eq!(other.hits(), 1);
}
#[tokio::test]
async fn old_task_keeps_revoked_grant_while_new_task_binds_replacement_and_replay_is_local() {
    let mock = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    let a = grant(&e, vec![NetworkPurpose::Completion]).await;
    let run = e.start_idempotent(request(), "start-key").await.unwrap();
    let old = settled(&e, &run.tasks[0].id).await;
    assert_eq!(old.status, "completed");
    e.network_revoke(&a.authorization_id).await.unwrap();
    grant(&e, vec![NetworkPurpose::Completion]).await;
    let err = e.resume(&old.id, "continue").await.unwrap_err();
    assert_eq!(
        err.downcast_ref::<NetworkError>().unwrap().kind,
        NetworkErrorKind::Revoked
    );
    let replay = e.start_idempotent(request(), "start-key").await.unwrap();
    assert_eq!(replay.id, run.id);
    assert_eq!(mock.hits(), 1);
    let next = e.start(request()).await.unwrap().tasks.remove(0);
    assert_eq!(settled(&e, &next.id).await.status, "completed");
    assert_eq!(mock.hits(), 2);
    let db = Connection::open(dir.path().join("network.db")).unwrap();
    let binding: String = db
        .query_row(
            "SELECT authorization_id FROM task_network_bindings WHERE task_id=?1",
            [&old.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(binding, a.authorization_id);
}
#[tokio::test]
async fn binding_insert_failure_rolls_back_new_task_turn_and_idempotency() {
    let mock = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    grant(&e, vec![NetworkPurpose::Completion]).await;
    let db = Connection::open(dir.path().join("network.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_binding BEFORE INSERT ON task_network_bindings BEGIN SELECT RAISE(ABORT,'fixture binding failure'); END;").unwrap();
    assert!(e.start_idempotent(request(), "rollback-key").await.is_err());
    for table in [
        "tasks",
        "runs",
        "turns",
        "turn_tasks",
        "sessions",
        "agents",
        "idempotency",
    ] {
        let n: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    assert_eq!(mock.hits(), 0);
    db.execute_batch("DROP TRIGGER fail_binding").unwrap();
    let task = e
        .start_idempotent(request(), "rollback-key")
        .await
        .unwrap()
        .tasks
        .remove(0);
    assert_eq!(settled(&e, &task.id).await.status, "completed");
    assert_eq!(mock.hits(), 1);
}
#[tokio::test]
async fn account_save_is_local_and_requires_new_token_when_identity_changes() {
    let mock = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    let account = Account {
        base_url: mock.base.clone(),
        user_id: "1".into(),
        quota_per_unit: 10.0,
    };
    e.bind_account(account.clone(), Some("fixture-account-secret".into()))
        .await
        .unwrap();
    e.bind_account(account.clone(), None).await.unwrap();
    assert_eq!(mock.hits(), 0);
    denied(
        e.account_balance().await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    let mut changed = account.clone();
    changed.user_id = "2".into();
    denied(
        e.bind_account(changed, None).await.unwrap_err(),
        NetworkErrorKind::Invalid,
    );
    assert_eq!(e.store.settings().unwrap().unwrap().newapi, Some(account));
    let s = NetworkSubject {
        kind: NetworkSubjectKind::Account,
        id: "newapi".into(),
        project_id: None,
    };
    let p = e.network_preview(s.clone()).await.unwrap();
    e.network_confirm(
        s,
        confirm_request(&p, vec![NetworkPurpose::Balance]),
        "account-confirm",
    )
    .await
    .unwrap();
    assert_eq!(e.account_balance().await.unwrap().result["balance"], 10.0);
    assert_eq!(mock.hits(), 1);
}
#[test]
fn fake_unique_index_literal_is_rejected_and_active_uniqueness_is_real() {
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), "http://127.0.0.1:9");
    let p = e.store.network_preview(subject(&e)).unwrap();
    let a = e
        .store
        .network_confirm(
            subject(&e),
            confirm_request(&p, vec![NetworkPurpose::Models]),
            "one",
        )
        .unwrap();
    let path = dir.path().join("network.db");
    let db = Connection::open(&path).unwrap();
    assert!(db.execute("INSERT INTO network_authorizations SELECT ?1,subject,config_revision,credential_revision,binding_digest,purposes,state,created_at FROM network_authorizations WHERE id=?2",params![id(),a.authorization.authorization_id]).is_err());
    db.execute_batch("DROP INDEX network_one_active;CREATE UNIQUE INDEX network_one_active ON network_authorizations(subject) WHERE state='activeifnotexists';").unwrap();
    drop(db);
    drop(e);
    assert!(Store::open(&path).is_err());
}
#[tokio::test]
async fn confirm_failure_rolls_back_authorization_confirmation_and_event() {
    let mock = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    let p = e.network_preview(subject(&e)).await.unwrap();
    let db = Connection::open(dir.path().join("network.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_confirmation BEFORE INSERT ON network_confirmations BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        e.network_confirm(
            subject(&e),
            confirm_request(&p, vec![NetworkPurpose::Models]),
            "rollback"
        )
        .await
        .is_err()
    );
    for table in [
        "network_authorizations",
        "network_confirmations",
        "network_events",
    ] {
        let n: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
    db.execute_batch("DROP TRIGGER fail_confirmation").unwrap();
    e.network_confirm(
        subject(&e),
        confirm_request(&p, vec![NetworkPurpose::Models]),
        "rollback",
    )
    .await
    .unwrap();
    e.route_models("route").await.unwrap();
    assert_eq!(mock.hits(), 1);
}

#[tokio::test]
async fn receipt_contradictions_are_corrupt_and_result_commit_failure_keeps_same_unknown_id() {
    let mock = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    grant(&e, vec![NetworkPurpose::Models]).await;
    let first = e.route_models("route").await.unwrap().calls.remove(0);
    let db = Connection::open(dir.path().join("network.db")).unwrap();
    let original = serde_json::to_value(&first).unwrap();
    for (field, value) in [
        ("result", json!("http_failure")),
        ("method", json!("POST")),
        ("purpose", json!("balance")),
        ("http_status", json!(500)),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        db.execute(
            "UPDATE network_calls SET receipt=?2 WHERE id=?1",
            params![first.call_id, changed.to_string()],
        )
        .unwrap();
        assert_eq!(
            e.network_call(&first.call_id).unwrap_err().kind,
            NetworkErrorKind::Corrupt
        );
    }
    db.execute(
        "UPDATE network_calls SET receipt=?2 WHERE id=?1",
        params![first.call_id, original.to_string()],
    )
    .unwrap();
    db.execute_batch("CREATE TRIGGER fail_finish BEFORE UPDATE ON network_calls BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let error = e.route_models("route").await.unwrap_err();
    assert_eq!(error.kind, NetworkErrorKind::Unknown);
    assert_eq!(error.calls.len(), 1);
    let id = error.calls[0].call_id.clone();
    assert_eq!(error.calls[0].result, Some(CallResult::PersistenceUnknown));
    assert_eq!(e.network_call(&id).unwrap().state, CallState::Claimed);
    db.execute_batch("DROP TRIGGER fail_finish").unwrap();
    e.store.recover().unwrap();
    assert_eq!(e.network_call(&id).unwrap().state, CallState::Unknown);
    assert_eq!(mock.hits(), 2);
}
#[tokio::test]
async fn account_quota_change_is_local_and_preserves_identity_grant() {
    let mock = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    let mut account = Account {
        base_url: mock.base.clone(),
        user_id: "1".into(),
        quota_per_unit: 10.0,
    };
    e.bind_account(account.clone(), Some("fixture-account".into()))
        .await
        .unwrap();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Account,
        id: "newapi".into(),
        project_id: None,
    };
    let p = e.network_preview(subject.clone()).await.unwrap();
    let a = e
        .network_confirm(
            subject,
            confirm_request(&p, vec![NetworkPurpose::Balance]),
            "quota",
        )
        .await
        .unwrap();
    account.quota_per_unit = 20.0;
    e.bind_account(account, None).await.unwrap();
    assert_eq!(mock.hits(), 0);
    assert_eq!(
        e.network_authorization(&a.authorization.authorization_id)
            .unwrap()
            .state,
        AuthorizationState::Active
    );
    assert_eq!(e.account_balance().await.unwrap().result["balance"], 5.0);
}
#[tokio::test]
async fn command_requires_ack_then_really_contacts_listener_without_outbound_sandbox() {
    use axum::{Router, routing::post};
    let outbound = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let url = outbound.base.clone();
    let app=Router::new().route("/chat/completions",post(move ||{let count=count.clone();let url=url.clone();async move {let delta=if count.fetch_add(1,Ordering::SeqCst)==0{json!({"tool_calls":[{"index":0,"id":"network-command","type":"function","function":{"name":"run_command","arguments":json!({"command":format!("Invoke-WebRequest -Uri '{url}/models' -UseBasicParsing | Out-Null")}).to_string()}}]})}else{json!({"content":"done"})};let chunk=json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});format!("data: {chunk}\n\ndata: [DONE]\n\n")}}));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let e = setup(dir.path(), &base);
    grant(&e, vec![NetworkPurpose::Completion]).await;
    let mut req = request();
    req.tasks[0].tools = true;
    req.tasks[0].allow_commands = true;
    let task = e.start(req).await.unwrap().tasks.remove(0);
    let card = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(card) = e
                .store
                .pending_approvals_for_task(&task.id)
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
    assert_eq!(
        card.command_network_ack,
        peachsh::approval::CommandNetworkAck::Required
    );
    assert!(!card.network_notice.unwrap().outbound_sandbox);
    assert!(matches!(
        e.decide_approval(&card.id, true, None)
            .unwrap_err()
            .downcast_ref::<peachsh::approval::ApprovalError>(),
        Some(peachsh::approval::ApprovalError::NetworkAcknowledgementRequired { .. })
    ));
    assert_eq!(outbound.hits(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    e.decide_approval_with_network_ack(&card.id, true, None, true)
        .unwrap();
    assert_eq!(settled(&e, &task.id).await.status, "completed");
    assert_eq!(outbound.hits(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn environment_credential_reference_and_value_are_rechecked_across_real_processes() {
    if let Ok(mode) = std::env::var("NHC_NETWORK_ENV_CHILD") {
        let root = std::path::PathBuf::from(std::env::var("NHC_NETWORK_ENV_ROOT").unwrap());
        let store = Arc::new(Store::open(&root.join("network.db")).unwrap());
        let e = Engine::new(store, 2).unwrap();
        if mode == "confirm" {
            grant(&e, vec![NetworkPurpose::Models]).await;
        }
        let response = e.route_models("route").await;
        if mode == "reject" {
            assert_eq!(response.unwrap_err().kind, NetworkErrorKind::Stale);
        } else {
            assert_eq!(response.unwrap().result, vec!["mock"]);
        }
        return;
    }
    let mock = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    let variable = format!("NHC_NETWORK_KEY_{}", id().replace('-', "_"));
    let mut settings = e.store.settings().unwrap().unwrap();
    settings.routes[0].key_env = Some(variable.clone());
    e.configure(settings).await.unwrap();
    let original = grant(&e, vec![NetworkPurpose::Models]).await;
    assert!(
        e.network_preview(subject(&e))
            .await
            .unwrap()
            .credential_ref
            .starts_with("stored/")
    );
    for (mode, key) in [
        ("reject", "fixture-network-secret"),
        ("confirm", "fixture-network-secret"),
        ("send", "fixture-network-secret"),
        ("reject", "fixture-env-rotation"),
    ] {
        let root = dir.path().to_owned();
        let variable = variable.clone();
        let output=tokio::task::spawn_blocking(move ||std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","environment_credential_reference_and_value_are_rechecked_across_real_processes","--nocapture"]).env("NHC_NETWORK_ENV_CHILD",mode).env("NHC_NETWORK_ENV_ROOT",root).env(variable,key).output().unwrap()).await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert_eq!(mock.hits(), 2);
    assert_eq!(
        e.network_authorization(&original.authorization_id)
            .unwrap()
            .state,
        AuthorizationState::Stale
    );
}
#[tokio::test]
async fn admitted_call_finishes_after_revoke_while_config_is_rejected_and_next_admission_stops() {
    use axum::{Router, routing::get};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let arrived = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let a = arrived.clone();
    let r = release.clone();
    let app = Router::new().route(
        "/models",
        get(move || {
            let a = a.clone();
            let r = r.clone();
            async move {
                a.notify_one();
                r.notified().await;
                axum::Json(json!({"data":[{"id":"mock"}]}))
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &base);
    let mut settings = e.store.settings().unwrap().unwrap();
    settings.max_concurrency = 1;
    e.configure(settings.clone()).await.unwrap();
    let auth = grant(&e, vec![NetworkPurpose::Models]).await;
    let engine = e.clone();
    let call = tokio::spawn(async move { engine.route_models("route").await });
    arrived.notified().await;
    assert!(e.configure(settings.clone()).await.is_err());
    settings.max_concurrency = 2;
    assert!(e.configure(settings).await.is_err());
    tokio::time::timeout(
        Duration::from_secs(1),
        e.network_revoke(&auth.authorization_id),
    )
    .await
    .unwrap()
    .unwrap();
    release.notify_one();
    let receipt = call.await.unwrap().unwrap().calls.remove(0);
    assert_eq!(receipt.state, CallState::Finished);
    assert!(e.route_models("route").await.is_err());
    server.abort();
    let _ = server.await;
}

#[test]
fn schema10_rejects_nullable_column_disguised_as_fused_type_keyword() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake-type.db");
    drop(Store::open(&path).unwrap());
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF;DROP TABLE network_subjects;CREATE TABLE network_subjects(subject TEXT NOT NULL PRIMARY KEY,snapshot TEXTNOT NULL,credential_blob BLOB NOT NULL);INSERT INTO network_subjects VALUES ('route:fake',NULL,x'00');").unwrap();
    let is_null: bool = db
        .query_row("SELECT snapshot IS NULL FROM network_subjects", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(is_null);
    drop(db);
    assert!(Store::open(&path).is_err());
}

#[tokio::test]
async fn revoke_after_task_permit_allows_only_inflight_round_and_stops_next_model_round() {
    use axum::{Router, routing::post};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let arrived = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let count = Arc::new(AtomicUsize::new(0));
    let a = arrived.clone();
    let r = release.clone();
    let c = count.clone();
    let app=Router::new().route("/chat/completions",post(move ||{let a=a.clone();let r=r.clone();let c=c.clone();async move {c.fetch_add(1,Ordering::SeqCst);a.notify_one();r.notified().await;let chunk=json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"read","type":"function","function":{"name":"list_files","arguments":"{\"path\":\"src\"}"}}]},"finish_reason":"stop"}]});format!("data: {chunk}\n\ndata: [DONE]\n\n")}}));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let e = setup(dir.path(), &base);
    let auth = grant(&e, vec![NetworkPurpose::Completion]).await;
    let mut req = request();
    req.tasks[0].tools = true;
    let task = e.start(req).await.unwrap().tasks.remove(0);
    arrived.notified().await;
    tokio::time::timeout(
        Duration::from_secs(1),
        e.network_revoke(&auth.authorization_id),
    )
    .await
    .unwrap()
    .unwrap();
    release.notify_one();
    let task = settled(&e, &task.id).await;
    assert_eq!(task.status, "failed");
    let receipts = e.network_calls_for_task(&task.id).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].state, CallState::Finished);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn append_binding_failure_preserves_every_old_business_column_then_explicit_new_turn_succeeds()
 {
    fn rows(db: &Connection) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
        let names: Vec<String> = {
            let mut s = db
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            s.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        names
            .into_iter()
            .map(|name| {
                let mut s = db
                    .prepare(&format!("SELECT * FROM {name} ORDER BY rowid"))
                    .unwrap();
                let values = s
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
    let mock = Mock::new().await;
    let dir = tempfile::tempdir().unwrap();
    let e = setup(dir.path(), &mock.base);
    let old = grant(&e, vec![NetworkPurpose::Completion]).await;
    let task = e.start(request()).await.unwrap().tasks.remove(0);
    assert_eq!(settled(&e, &task.id).await.status, "completed");
    let db = Connection::open(dir.path().join("network.db")).unwrap();
    let (session, agent, turn): (String, String, String) = db
        .query_row(
            "SELECT session_id,agent_id,turn_id FROM turn_tasks WHERE legacy_task_id=?1",
            [&task.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    e.network_revoke(&old.authorization_id).await.unwrap();
    let replacement = grant(&e, vec![NetworkPurpose::Completion]).await;
    let command = SendChatTurn {
        session_id: SessionId(session),
        agent_id: AgentId(agent),
        expected_last_turn_id: TurnId(turn),
        message: "new turn".into(),
        idempotency_key: "append-rollback".into(),
    };
    db.execute_batch("CREATE TRIGGER fail_binding BEFORE INSERT ON task_network_bindings BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let before = rows(&db);
    assert!(e.send_chat_turn(command.clone()).await.is_err());
    assert_eq!(rows(&db), before);
    assert_eq!(mock.hits(), 1);
    db.execute_batch("DROP TRIGGER fail_binding").unwrap();
    let next = e.send_chat_turn(command.clone()).await.unwrap();
    assert_eq!(
        settled(&e, &next.task.legacy_task_id).await.status,
        "completed"
    );
    assert_ne!(next.task.legacy_task_id, task.id);
    let binding = |task_id: &str| {
        db.query_row(
            "SELECT authorization_id FROM task_network_bindings WHERE task_id=?1",
            [task_id],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
    };
    assert_eq!(binding(&task.id), old.authorization_id);
    assert_eq!(
        binding(&next.task.legacy_task_id),
        replacement.authorization_id
    );
    assert_eq!(
        e.resume(&task.id, "old retry")
            .await
            .unwrap_err()
            .downcast_ref::<NetworkError>()
            .unwrap()
            .kind,
        NetworkErrorKind::Revoked
    );
    assert_eq!(mock.hits(), 2);
    let replay = e.send_chat_turn(command).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(mock.hits(), 2);
}
