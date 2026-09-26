//! Public Engine authorization paths; no test permit or transport bypass.
use peachsh::{
    domain::{Account, Route, Settings},
    engine::Engine,
    network_authorization::{
        AuthorizationState, CallResult, CallState, ConfirmRequest, DispatchState, NetworkError,
        NetworkErrorKind, NetworkPurpose, NetworkSubject, NetworkSubjectKind,
    },
    store::Store,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

struct Endpoint {
    base: String,
    accepts: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}
impl Endpoint {
    async fn start(fixed_response: Option<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let accepts = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let count = accepts.clone();
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                count.fetch_add(1, Ordering::SeqCst);
                let mut request = Vec::new();
                loop {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    assert!(request.len() < 1_000_000);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&request[..end]);
                        let length = header
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(request).unwrap();
                let body = if request.starts_with("POST ") {
                    "data: {\"choices\":[{\"delta\":{\"content\":\"323\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                } else if request.lines().next().unwrap().contains("/api/user/self ") {
                    "{\"success\":true,\"data\":{\"quota\":10}}"
                } else {
                    "{\"data\":[{\"id\":\"m\"}]}"
                };
                let response = fixed_response.clone().unwrap_or_else(|| {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .into_bytes()
                });
                captured.lock().unwrap().push(request);
                let _ = socket.write_all(&response).await;
            }
        });
        Self {
            base,
            accepts,
            requests,
            task,
        }
    }
    fn counts(&self) -> (usize, usize) {
        (
            self.accepts.load(Ordering::SeqCst),
            self.requests.lock().unwrap().len(),
        )
    }
    fn last(&self) -> String {
        self.requests.lock().unwrap().last().unwrap().clone()
    }
    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}
fn subject() -> NetworkSubject {
    NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
    }
}
fn account_subject() -> NetworkSubject {
    NetworkSubject {
        kind: NetworkSubjectKind::Account,
        id: "newapi".into(),
    }
}
fn settings(dir: &std::path::Path, base: &str) -> Settings {
    Settings {
        workspace: dir.to_string_lossy().into(),
        max_concurrency: 2,
        routes: vec![Route {
            id: "route".into(),
            name: "fixture".into(),
            base_url: base.into(),
            model: "m".into(),
            max_tokens: 128,
            parallel_limit: 1,
            key_env: None,
        }],
        newapi: None,
    }
}
async fn engine(dir: &std::path::Path, base: &str, key: &str) -> Arc<Engine> {
    let store = Arc::new(Store::open(&dir.join("test.db")).unwrap());
    let engine = Engine::new(store, 2).unwrap();
    engine
        .configure_with_keys(
            settings(dir, base),
            HashMap::from([("route".into(), key.into())]),
        )
        .await
        .unwrap();
    engine
}
async fn authorize(
    engine: &Engine,
    subject: NetworkSubject,
    purposes: Vec<NetworkPurpose>,
    key: &str,
) -> String {
    let preview = engine.network_preview(subject.clone()).await.unwrap();
    engine
        .network_confirm(
            subject,
            ConfirmRequest {
                config_revision: preview.config_revision,
                credential_revision: preview.credential_revision,
                binding_digest: preview.binding_digest,
                purposes,
            },
            key,
        )
        .await
        .unwrap()
        .authorization
        .authorization_id
}
fn not_sent(error: NetworkError, kind: NetworkErrorKind) {
    assert_eq!(error.kind, kind);
    assert_eq!(error.dispatch_state, DispatchState::NotSent);
    assert!(error.calls.is_empty());
}

#[tokio::test]
async fn management_requires_whole_check_scope_and_persists_real_calls() {
    let endpoint = Endpoint::start(None).await;
    let dir = tempfile::tempdir_in("target").unwrap();
    let engine = engine(dir.path(), &endpoint.base, "fixture-key-a").await;
    not_sent(
        engine.route_models("route").await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    assert_eq!(endpoint.counts(), (0, 0));
    let narrow = authorize(
        &engine,
        subject(),
        vec![NetworkPurpose::Models],
        "models-only",
    )
    .await;
    not_sent(
        engine.route_check("route").await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    assert_eq!(endpoint.counts(), (0, 0));
    let full = authorize(
        &engine,
        subject(),
        vec![NetworkPurpose::Models, NetworkPurpose::Completion],
        "check-both",
    )
    .await;
    assert_eq!(
        engine.network_authorization(&narrow).unwrap().state,
        AuthorizationState::Stale
    );
    let response = engine.route_check("route").await.unwrap();
    assert_eq!(response.result["ok"], true);
    assert_eq!(endpoint.counts(), (2, 2));
    assert_eq!(response.calls.len(), 2);
    for call in &response.calls {
        assert_eq!(call.authorization_id, full);
        assert_eq!(call.state, CallState::Finished);
        assert_eq!(call.success, Some(true));
        assert_eq!(engine.network_call(&call.call_id).unwrap(), *call);
        assert!(
            !serde_json::to_string(call)
                .unwrap()
                .contains("fixture-key-a")
        );
    }
    engine.network_revoke(&full).await.unwrap();
    not_sent(
        engine.route_models("route").await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    assert_eq!(endpoint.counts(), (2, 2));
    endpoint.stop().await;
}

#[tokio::test]
async fn target_path_model_and_raw_credential_changes_never_reuse_old_authorization() {
    let a = Endpoint::start(None).await;
    let b = Endpoint::start(None).await;
    let dir = tempfile::tempdir_in("target").unwrap();
    let engine = engine(dir.path(), &a.base, "fixture-key-a").await;
    authorize(&engine, subject(), vec![NetworkPurpose::Models], "a").await;
    assert_eq!(
        engine.route_models("route").await.unwrap().result,
        vec!["m"]
    );
    engine
        .configure_with_keys(
            settings(dir.path(), &format!("{}/v2", b.base)),
            HashMap::from([("route".into(), "fixture-key-b".into())]),
        )
        .await
        .unwrap();
    not_sent(
        engine.route_models("route").await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    assert_eq!(b.counts(), (0, 0));
    authorize(&engine, subject(), vec![NetworkPurpose::Models], "b").await;
    engine.route_models("route").await.unwrap();
    assert!(b.last().starts_with("GET /v2/models "));
    assert!(b.last().contains("Bearer fixture-key-b"));
    assert!(!b.last().contains("fixture-key-a"));
    let mut next = engine.store.settings().unwrap().unwrap();
    next.routes[0].model = "m2".into();
    next.routes[0].base_url = format!("{}/v3", b.base);
    engine.configure(next).await.unwrap();
    not_sent(
        engine.route_models("route").await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    assert_eq!(b.counts(), (1, 1));
    authorize(&engine, subject(), vec![NetworkPurpose::Models], "b-v3").await;
    engine.route_models("route").await.unwrap();
    assert!(b.last().starts_with("GET /v3/models "));
    // Raw secret storage is not authorization. This must be rejected at actual admission.
    engine
        .store
        .put_secret("route::route", "fixture-key-c")
        .unwrap();
    not_sent(
        engine.route_models("route").await.unwrap_err(),
        NetworkErrorKind::Stale,
    );
    assert_eq!(b.counts(), (2, 2));
    authorize(
        &engine,
        subject(),
        vec![NetworkPurpose::Models],
        "new-credential",
    )
    .await;
    engine.route_models("route").await.unwrap();
    assert!(b.last().contains("Bearer fixture-key-c"));
    assert_eq!(a.counts(), (1, 1));
    assert_eq!(b.counts(), (3, 3));
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn account_binding_is_local_and_identity_changes_need_separate_confirmation() {
    let endpoint = Endpoint::start(None).await;
    let dir = tempfile::tempdir_in("target").unwrap();
    let engine = engine(dir.path(), &endpoint.base, "fixture-route-key").await;
    authorize(
        &engine,
        subject(),
        vec![NetworkPurpose::Models],
        "route-only",
    )
    .await;
    let mut account = Account {
        base_url: endpoint.base.clone(),
        user_id: "42".into(),
        quota_per_unit: 2.0,
    };
    engine
        .bind_account(account.clone(), Some("fixture-account-key".into()))
        .await
        .unwrap();
    assert_eq!(endpoint.counts(), (0, 0));
    not_sent(
        engine.account_balance().await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    authorize(
        &engine,
        account_subject(),
        vec![NetworkPurpose::Balance],
        "account-42",
    )
    .await;
    let response = engine.account_balance().await.unwrap();
    assert_eq!(response.result["balance"], 5.0);
    assert!(
        endpoint
            .last()
            .to_ascii_lowercase()
            .contains("new-api-user: 42")
    );
    assert!(endpoint.last().contains("Bearer fixture-account-key"));
    assert!(!endpoint.last().contains("fixture-route-key"));
    account.user_id = "43".into();
    not_sent(
        engine
            .bind_account(account.clone(), None)
            .await
            .unwrap_err(),
        NetworkErrorKind::Invalid,
    );
    engine
        .bind_account(account, Some("fixture-account-next".into()))
        .await
        .unwrap();
    not_sent(
        engine.account_balance().await.unwrap_err(),
        NetworkErrorKind::Required,
    );
    assert_eq!(endpoint.counts(), (1, 1));
    authorize(
        &engine,
        account_subject(),
        vec![NetworkPurpose::Balance],
        "account-43",
    )
    .await;
    engine.account_balance().await.unwrap();
    assert_eq!(endpoint.counts(), (2, 2));
    assert!(
        endpoint
            .last()
            .to_ascii_lowercase()
            .contains("new-api-user: 43")
    );
    endpoint.stop().await;
}

#[tokio::test]
async fn redirect_and_truncated_responses_keep_receipts_and_never_replay_on_reopen() {
    let b = Endpoint::start(None).await;
    for (wire, state, result, kind) in [
        (format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {}/models\r\nContent-Length: 0\r\n\r\n", b.base).into_bytes(), CallState::Finished, CallResult::HttpFailure, NetworkErrorKind::Upstream),
        (b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{}".to_vec(), CallState::Unknown, CallResult::TransportUnknown, NetworkErrorKind::Unknown),
    ] {
        let a = Endpoint::start(Some(wire)).await; let dir = tempfile::tempdir_in("target").unwrap();
        let engine = engine(dir.path(), &a.base, "fixture-key").await;
        authorize(&engine, subject(), vec![NetworkPurpose::Models], "one-call").await;
        let error = engine.route_models("route").await.unwrap_err(); assert_eq!(error.kind, kind); assert_eq!(error.calls.len(), 1);
        let call = error.calls[0].clone(); assert_eq!(call.state, state); assert_eq!(call.result, Some(result));
        assert_eq!(a.counts(), (1, 1)); assert_eq!(b.counts(), (0, 0)); drop(engine);
        let reopened = Engine::new(Arc::new(Store::open(&dir.path().join("test.db")).unwrap()), 2).unwrap();
        assert_eq!(reopened.network_call(&call.call_id).unwrap(), call);
        assert_eq!(a.counts(), (1, 1)); assert_eq!(b.counts(), (0, 0)); a.stop().await;
    }
    let dir = tempfile::tempdir_in("target").unwrap();
    let engine = engine(dir.path(), &b.base, "fixture-b-key").await;
    authorize(
        &engine,
        subject(),
        vec![NetworkPurpose::Models],
        "reachable-b",
    )
    .await;
    engine.route_models("route").await.unwrap();
    assert_eq!(b.counts(), (1, 1));
    b.stop().await;
}
