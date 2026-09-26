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
fn subject() -> NetworkSubject {
    NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
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
    let p = e.network_preview(subject()).await.unwrap();
    e.network_confirm(subject(), confirm_request(&p, purposes), &id())
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
    let p = e.network_preview(subject()).await.unwrap();
    let req = confirm_request(&p, vec![NetworkPurpose::Models, NetworkPurpose::Models]);
    let a = e
        .network_confirm(subject(), req.clone(), "confirm-one")
        .await
        .unwrap();
    assert!(!a.replayed);
    let replay = e
        .network_confirm(subject(), req.clone(), "confirm-one")
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(a.authorization, replay.authorization);
    e.network_revoke(&a.authorization.authorization_id)
        .await
        .unwrap();
    let replay = e
        .network_confirm(subject(), req.clone(), "confirm-one")
        .await
        .unwrap();
    assert_eq!(replay.authorization.state, AuthorizationState::Revoked);
    let mut different = req.clone();
    different.purposes = vec![NetworkPurpose::Completion];
    denied(
        e.network_confirm(subject(), different, "confirm-one")
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
    let other = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "other".into(),
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
        e.network_confirm(subject(), req, "confirm-one")
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
    let p = e.network_preview(subject()).await.unwrap();
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
    let p = e.store.network_preview(subject()).unwrap();
    let a = e
        .store
        .network_confirm(
            subject(),
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
    let p = e.network_preview(subject()).await.unwrap();
    let db = Connection::open(dir.path().join("network.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_confirmation BEFORE INSERT ON network_confirmations BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        e.network_confirm(
            subject(),
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
        subject(),
        confirm_request(&p, vec![NetworkPurpose::Models]),
        "rollback",
    )
    .await
    .unwrap();
    e.route_models("route").await.unwrap();
    assert_eq!(mock.hits(), 1);
}
