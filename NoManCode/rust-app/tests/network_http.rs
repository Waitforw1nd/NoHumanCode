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
        let status = r.status();
        let value: Value = r.json().await.unwrap();
        assert_eq!(status, 200, "{value}");
        value
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
    assert_eq!(revoked["error"]["code"], "required");
    let revoked_grant = body(
        h.request(
            reqwest::Method::GET,
            &format!("/api/network/authorizations/{id}"),
        )
        .send()
        .await
        .unwrap(),
        200,
    )
    .await;
    assert_eq!(revoked_grant["state"], "revoked");
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
    for request in [
        r#"{"account":{"base_url":"https://example.com","user_id":"7","extra":true},"token":"synthetic"}"#,
        r#"{"account":{"base_url":"https://example.com","user_id":"7","user_id":"8"},"token":"synthetic"}"#,
    ] {
        let response = h
            .request(reqwest::Method::PUT, "/api/newapi")
            .header("content-type", "application/json")
            .body(request)
            .send()
            .await
            .unwrap();
        body(response, 400).await;
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

async fn launch_host(root: &std::path::Path, port: u16) -> tokio::process::Child {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
        .arg("--port")
        .arg(port.to_string())
        .arg("--data-dir")
        .arg(root.join("data"))
        .arg("--legacy-data")
        .arg(root.join("absent-legacy"))
        .arg("--workspace")
        .arg(root.join("workspace"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "Host exited before readiness"
            );
            if let Ok(response) = client()
                .get(format!("http://127.0.0.1:{port}/api/health"))
                .send()
                .await
                && response.status().is_success()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    child
}
async fn bootstrap(origin: &str) -> String {
    client()
        .get(format!("{origin}/api/bootstrap"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}
#[tokio::test]
async fn network_http_real_host_kill_after_send_retains_same_unknown_call_without_replay() {
    real_host_crash_case(false).await;
}
#[tokio::test]
async fn network_http_real_host_full_response_finish_db_failure_then_kill_recovers_same_call() {
    real_host_crash_case(true).await;
}
async fn real_host_crash_case(block_finish: bool) {
    use axum::{
        body::Body,
        response::{IntoResponse, Response},
    };
    use std::convert::Infallible;
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("workspace")).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (count, signal, barrier) = (calls.clone(), entered.clone(), release.clone());
    let provider=Router::new().route("/v1/models",get(move||{let (count,signal,barrier)=(count.clone(),signal.clone(),barrier.clone());async move {
  let attempt=count.fetch_add(1,Ordering::SeqCst);
  if attempt==0 {return Json(json!({"data":[{"id":"mock-model"}]})).into_response();}
  let stream=async_stream::stream! {yield Ok::<_,Infallible>(b"{\"data\":[".to_vec());signal.notify_one();barrier.notified().await;yield Ok(b"{\"id\":\"mock-model\"}]}".to_vec());};
  Response::builder().header("content-type","application/json").body(Body::from_stream(stream)).unwrap()
 }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_origin = format!("http://{}", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let origin = format!("http://127.0.0.1:{port}");
    let mut child = launch_host(root.path(), port).await;
    let token = bootstrap(&origin).await;
    let settings = Settings {
        workspace: root.path().join("workspace").to_string_lossy().into(),
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
    };
    body(
        client()
            .put(format!("{origin}/api/settings"))
            .header("x-peachsh-token", &token)
            .json(&json!({"settings":settings,"keys":{"route-a":"synthetic-network-key"}}))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    let preview = body(
        client()
            .post(format!("{origin}/api/network/preview"))
            .header("x-peachsh-token", &token)
            .json(&json!({"kind":"route","id":"route-a"}))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    body(client().post(format!("{origin}/api/network/authorizations")).header("x-peachsh-token",&token).header("Idempotency-Key","crash-grant").json(&json!({"subject":preview["subject"],"config_revision":preview["config_revision"],"credential_revision":preview["credential_revision"],"binding_digest":preview["binding_digest"],"purposes":["models"]})).send().await.unwrap(),201).await;
    // Reachable positive control on this exact server, credential, grant and endpoint.
    let positive = body(
        client()
            .post(format!("{origin}/api/routes/route-a/models"))
            .header("x-peachsh-token", &token)
            .json(&json!({}))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(positive["calls"][0]["state"], "finished");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let request = client()
        .post(format!("{origin}/api/routes/route-a/models"))
        .header("x-peachsh-token", &token)
        .json(&json!({}));
    let pending =
        tokio::spawn(async move { request.timeout(Duration::from_secs(12)).send().await });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let db_path = root.path().join("data/peachsh.sqlite3");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let (call_id, receipt): (String, String) = db
        .query_row(
            "SELECT id,receipt FROM network_calls WHERE state='claimed'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let receipt: Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["call_id"], call_id);
    assert_eq!(receipt["state"], "claimed");
    if block_finish {
        // The claim is already durable. Hold an external write transaction while
        // releasing the complete valid provider response. Host's typed
        // persistence_unknown receipt proves it reached finish and could not commit.
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        release.notify_one();
        let failure = body(pending.await.unwrap().unwrap(), 409).await;
        assert_eq!(failure["error"]["code"], "unknown");
        assert_eq!(failure["error"]["dispatch_state"], "unknown");
        assert_eq!(failure["error"]["calls"][0]["call_id"], call_id);
        assert_eq!(
            failure["error"]["calls"][0]["result"],
            "persistence_unknown"
        );
        let persisted: String = db
            .query_row(
                "SELECT state FROM network_calls WHERE id=?1",
                [&call_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(persisted, "claimed");
        child.kill().await.unwrap();
        assert!(!child.wait().await.unwrap().success());
        db.execute_batch("ROLLBACK").unwrap();
    } else {
        child.kill().await.unwrap();
        assert!(!child.wait().await.unwrap().success());
        assert!(pending.await.unwrap().is_err());
        release.notify_one();
    }
    drop(db);
    child = launch_host(root.path(), port).await;
    let token = bootstrap(&origin).await;
    let recovered = body(
        client()
            .get(format!("{origin}/api/network/calls/{call_id}"))
            .header("x-peachsh-token", &token)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(recovered["call_id"], call_id);
    assert_eq!(recovered["state"], "unknown");
    assert_eq!(recovered["success"], Value::Null);
    let db = rusqlite::Connection::open(db_path).unwrap();
    let rows: i64 = db
        .query_row("SELECT count(*) FROM network_calls", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 2, "restart must not manufacture another claim");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "restart and receipt reads must not resend"
    );
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    provider.abort();
}
