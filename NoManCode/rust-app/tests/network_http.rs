//! NEXT-07 real Host authorization admission and transport contracts; localhost only.
use axum::{
    Json, Router,
    routing::{get, post},
};
use peachsh::{
    domain::{Account, Route, Settings},
    engine::Engine,
    server::{self, App},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Host {
    origin: String,
    engine: Arc<Engine>,
    calls: Arc<AtomicUsize>,
    provider_origin: String,
    http: tokio::task::JoinHandle<()>,
    provider: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.http.abort();
        self.provider.abort();
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(6))
        .build()
        .unwrap()
}
impl Host {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let models = calls.clone();
        let complete = calls.clone();
        let balance = calls.clone();
        let provider=Router::new().route("/v1/models",get(move||{let calls=models.clone();async move{calls.fetch_add(1,Ordering::SeqCst);Json(json!({"data":[{"id":"mock-model"}]}))}}))
   .route("/v1/chat/completions",post(move||{let calls=complete.clone();async move{calls.fetch_add(1,Ordering::SeqCst);([("content-type","text/event-stream")],"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")}}))
   .route("/api/user/self",get(move||{let calls=balance.clone();async move{calls.fetch_add(1,Ordering::SeqCst);Json(json!({"success":true,"data":{"id":7,"username":"test","display_name":"Test","group":"default","quota":500000,"used_quota":0}}))}}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider_origin = format!("http://{}", listener.local_addr().unwrap());
        let provider = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
        let engine = Engine::new(
            Arc::new(Store::open(&dir.path().join("host.sqlite3")).unwrap()),
            1,
        )
        .unwrap();
        engine
            .configure_with_keys(
                Settings {
                    workspace: dir.path().to_string_lossy().into(),
                    max_concurrency: 1,
                    routes: vec![Route {
                        id: "route-a".into(),
                        name: "Route".into(),
                        base_url: format!("{provider_origin}/v1"),
                        model: "mock-model".into(),
                        max_tokens: 128,
                        parallel_limit: 1,
                        key_env: None,
                    }],
                    newapi: None,
                },
                HashMap::from([("route-a".into(), "synthetic-network-key".into())]),
            )
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = server::router(App {
            engine: engine.clone(),
            token: "host-token".into(),
            origin: origin.clone(),
        });
        let http = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            origin,
            engine,
            calls,
            provider_origin,
            http,
            provider,
            _dir: dir,
        }
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        client()
            .request(method, format!("{}{path}", self.origin))
            .header("x-peachsh-token", "host-token")
    }
    async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        self.request(reqwest::Method::POST, path)
            .json(&body)
            .send()
            .await
            .unwrap()
    }
    async fn preview(&self, subject: Value) -> Value {
        let r = self.post("/api/network/preview", subject).await;
        assert_eq!(r.status(), 200);
        r.json().await.unwrap()
    }
    async fn authorize(&self, preview: &Value, purposes: Value, key: &str) -> reqwest::Response {
        self.request(reqwest::Method::POST,"/api/network/authorizations").header("Idempotency-Key",key).json(&json!({"subject":preview["subject"],"config_revision":preview["config_revision"],"credential_revision":preview["credential_revision"],"binding_digest":preview["binding_digest"],"purposes":purposes})).send().await.unwrap()
    }
}
async fn body(response: reqwest::Response, status: u16) -> Value {
    assert_eq!(response.status().as_u16(), status);
    let value: Value = response.json().await.unwrap();
    assert!(!value.to_string().contains("synthetic-network-key"));
    value
}
#[tokio::test]
async fn network_http_local_save_preview_grant_replay_and_explicit_post() {
    let h = Host::new().await;
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    for path in ["/api/routes/route-a/models", "/api/newapi/balance"] {
        let r = h.request(reqwest::Method::GET, path).send().await.unwrap();
        assert_eq!(r.status(), 405);
        assert_eq!(r.headers()["allow"], "POST");
        body(r, 405).await;
    }
    let required = body(h.post("/api/routes/route-a/models", json!({})).await, 409).await;
    assert_eq!(required["error"]["code"], "required");
    assert_eq!(required["error"]["dispatch_state"], "not_sent");
    assert_eq!(required["error"]["calls"], json!([]));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    let preview = h.preview(json!({"kind":"route","id":"route-a"})).await;
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    let created = body(
        h.authorize(
            &preview,
            json!(["models", "completion", "models"]),
            "grant-1",
        )
        .await,
        201,
    )
    .await;
    assert_eq!(created["replayed"], false);
    let replay = body(
        h.authorize(&preview, json!(["completion", "models"]), "grant-1")
            .await,
        200,
    )
    .await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["authorization"], created["authorization"]);
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    let response = body(h.post("/api/routes/route-a/models", json!({})).await, 200).await;
    assert_eq!(response["result"], json!(["mock-model"]));
    assert_eq!(response["calls"].as_array().unwrap().len(), 1);
    assert_eq!(response["calls"][0]["state"], "finished");
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    let call_id = response["calls"][0]["call_id"].as_str().unwrap();
    let fetched = body(
        h.request(
            reqwest::Method::GET,
            &format!("/api/network/calls/{call_id}"),
        )
        .send()
        .await
        .unwrap(),
        200,
    )
    .await;
    assert_eq!(fetched, response["calls"][0]);
    let checked = body(h.post("/api/routes/route-a/check", json!({})).await, 200).await;
    assert_eq!(checked["calls"].as_array().unwrap().len(), 2);
    assert_eq!(checked["calls"][0]["purpose"], "models");
    assert_eq!(checked["calls"][1]["purpose"], "completion");
    assert_eq!(h.calls.load(Ordering::SeqCst), 3);
    let id = created["authorization"]["authorization_id"]
        .as_str()
        .unwrap();
    body(
        h.post(
            &format!("/api/network/authorizations/{id}/revoke"),
            json!({}),
        )
        .await,
        200,
    )
    .await;
    let revoked = body(h.post("/api/routes/route-a/models", json!({})).await, 409).await;
    assert_eq!(revoked["error"]["code"], "revoked");
    assert_eq!(h.calls.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn network_http_strict_json_auth_reads_and_invalid_requests_never_dispatch() {
    let h = Host::new().await;
    for path in [
        "/api/network/authorizations/auth-a",
        "/api/network/calls/call-a",
        "/api/tasks/task-a/network-calls",
    ] {
        let r = client()
            .get(format!("{}{path}", h.origin))
            .send()
            .await
            .unwrap();
        let v = body(r, 403).await;
        assert_eq!(v["code"], "forbidden");
        assert!(v["error"].is_string());
    }
    for (path, request) in [
        (
            "/api/network/preview",
            r#"{"kind":"route","kind":"account","id":"route-a"}"#,
        ),
        ("/api/network/preview", r#"["route","route-a"]"#),
        (
            "/api/network/preview",
            r#"{"kind":"route","id":"route-a","extra":1}"#,
        ),
        (
            "/api/network/authorizations",
            r#"{"subject":{"kind":"route","id":"route-a","id":"other"},"config_revision":1,"credential_revision":1,"binding_digest":"a","purposes":["models"]}"#,
        ),
        ("/api/routes/route-a/models", r#"{"unexpected":true}"#),
        ("/api/routes/route-a/check", r#"[]"#),
        ("/api/newapi/balance", r#"null"#),
        (
            "/api/network/authorizations/auth-a/revoke",
            r#"{"id":"other"}"#,
        ),
    ] {
        let response = h
            .request(reqwest::Method::POST, path)
            .header("content-type", "application/json")
            .body(request)
            .send()
            .await
            .unwrap();
        let v = body(response, 400).await;
        assert!(v["error"].is_string());
    }
    let preview = h.preview(json!({"kind":"route","id":"route-a"})).await;
    let no_key=h.request(reqwest::Method::POST,"/api/network/authorizations").json(&json!({"subject":preview["subject"],"config_revision":preview["config_revision"],"credential_revision":preview["credential_revision"],"binding_digest":preview["binding_digest"],"purposes":["models"]})).send().await.unwrap();
    body(no_key, 400).await;
    let absent = body(
        h.request(reqwest::Method::GET, "/api/tasks/absent/network-calls")
            .send()
            .await
            .unwrap(),
        404,
    )
    .await;
    assert_eq!(absent["error"]["calls"], json!([]));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    // Reachable positive control uses this exact provider and request after explicit authorization.
    body(
        h.authorize(&preview, json!(["models"]), "positive").await,
        201,
    )
    .await;
    body(h.post("/api/routes/route-a/models", json!({})).await, 200).await;
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn network_http_account_save_is_local_and_revision_change_requires_new_preview() {
    let h = Host::new().await;
    let account = Account {
        base_url: h.provider_origin.clone(),
        user_id: "7".into(),
        quota_per_unit: 500000.0,
    };
    let saved = h
        .request(reqwest::Method::PUT, "/api/newapi")
        .json(&json!({"account":account,"token":"synthetic-account-token"}))
        .send()
        .await
        .unwrap();
    let saved = body(saved, 200).await;
    assert_eq!(saved["network_authorization_required"], true);
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    let required = body(h.post("/api/newapi/balance", json!({})).await, 409).await;
    assert_eq!(required["error"]["code"], "required");
    let preview = h.preview(json!({"kind":"account","id":"newapi"})).await;
    body(
        h.authorize(&preview, json!(["balance"]), "account-grant")
            .await,
        201,
    )
    .await;
    let balance = body(h.post("/api/newapi/balance", json!({})).await, 200).await;
    assert_eq!(balance["calls"][0]["purpose"], "balance");
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    let mut settings = h.engine.store.settings().unwrap().unwrap();
    settings.routes[0].model = "changed-model".into();
    h.engine
        .configure_with_keys(settings, HashMap::new())
        .await
        .unwrap();
    let route_preview = h.preview(json!({"kind":"route","id":"route-a"})).await;
    let mut settings = h.engine.store.settings().unwrap().unwrap();
    settings.routes[0].model = "another-model".into();
    h.engine
        .configure_with_keys(settings, HashMap::new())
        .await
        .unwrap();
    let stale = body(
        h.authorize(&route_preview, json!(["models"]), "stale-snapshot")
            .await,
        409,
    )
    .await;
    assert_eq!(stale["error"]["code"], "stale");
    assert_eq!(stale["error"]["calls"], json!([]));
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
}
