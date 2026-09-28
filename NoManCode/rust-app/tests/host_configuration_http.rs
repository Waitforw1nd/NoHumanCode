//! I07: real Router + Engine + fresh Store; routes are written only through HTTP.
#![cfg(windows)]
use axum::{Json, Router, routing::any};
use peachsh::{
    engine::Engine,
    server::{self, App},
    store::{Store, fresh_settings},
};
use reqwest::{
    Client, Method,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const TOKEN: &str = "initialization-http-host-boundary";
const SECRET: &str = "synthetic-initialization-http-credential";
struct Host {
    origin: String,
    base: String,
    engine: Arc<Engine>,
    client: Client,
    calls: Arc<AtomicUsize>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}
impl Drop for Host {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Host {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("host.sqlite3")).unwrap());
        fresh_settings(&store, dir.path()).unwrap();
        let engine = Engine::new(store, 3).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let provider = Router::new().fallback(any(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Json(json!({"unexpected":true}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let provider = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = server::router(App {
            engine: engine.clone(),
            token: TOKEN.into(),
            origin: origin.clone(),
        });
        let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            origin,
            base,
            engine,
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(8))
                .build()
                .unwrap(),
            calls,
            tasks: vec![provider, http],
            _dir: dir,
        }
    }
    fn body(&self, revision: u64) -> Value {
        json!({"expected_revision":revision,"route":{"name":"Local","base_url":self.base,"model":"mock-model","max_tokens":128,"parallel_limit":1},"credential":{"mode":"replace","value":SECRET}})
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
        headers: HeaderMap,
    ) -> (u16, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .headers(headers);
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let status = response.status().as_u16();
        let bytes = response.bytes().await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            !text.contains(SECRET) && !text.contains(TOKEN),
            "secret-free response"
        );
        (status, serde_json::from_str(&text).unwrap())
    }
    async fn put(&self, body: Value, key: &str) -> (u16, Value) {
        self.request(
            Method::PUT,
            "/api/host/routes/local",
            Some(body.to_string()),
            headers(Some(key)),
        )
        .await
    }
    fn unchanged(&self) {
        let state = self.engine.host_config().unwrap();
        assert_eq!(state.revision, 0);
        assert!(state.routes.is_empty());
        let db = rusqlite::Connection::open_with_flags(
            self._dir.path().join("host.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        for table in ["secrets", "host_config_requests"] {
            let count: i64 = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "no partial write: {table}");
        }
        assert_eq!(self.calls.load(Ordering::SeqCst), 0);
    }
}
fn headers(key: Option<&str>) -> HeaderMap {
    let mut result = HeaderMap::new();
    result.insert("x-peachsh-token", HeaderValue::from_static(TOKEN));
    result.insert("content-type", HeaderValue::from_static("application/json"));
    if let Some(key) = key {
        result.insert("idempotency-key", HeaderValue::from_str(key).unwrap());
    }
    result
}
fn error(response: (u16, Value), status: u16, code: &str) {
    assert_eq!(response.0, status);
    let body = response.1;
    assert_eq!(body.as_object().unwrap().len(), 1);
    assert_eq!(body["error"].as_object().unwrap().len(), 3);
    assert_eq!(body["error"]["code"], code);
    assert_eq!(body["error"]["retryable"], false);
    assert!(body["error"]["message"].is_string());
}

#[tokio::test]
async fn i07_all_endpoints_require_host_origin_site_and_token() {
    let host = Host::new().await;
    for (method, path, body) in [
        (Method::GET, "/api/host/config", None),
        (
            Method::PUT,
            "/api/host/routes/local",
            Some(host.body(0).to_string()),
        ),
        (
            Method::POST,
            "/api/host/config/lookup",
            Some(json!({"key":"first"}).to_string()),
        ),
    ] {
        for violation in ["missing-token", "wrong-token", "host", "origin", "site"] {
            let mut h = headers(Some("first"));
            match violation {
                "missing-token" => {
                    h.remove("x-peachsh-token");
                }
                "wrong-token" => {
                    h.insert("x-peachsh-token", HeaderValue::from_static("wrong"));
                }
                "host" => {
                    h.insert("host", HeaderValue::from_static("example.invalid"));
                }
                "origin" => {
                    h.insert("origin", HeaderValue::from_static("http://example.invalid"));
                }
                _ => {
                    h.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
                }
            }
            error(
                host.request(method.clone(), path, body.clone(), h).await,
                403,
                "forbidden",
            );
            host.unchanged();
        }
    }
    let (status, value) = host
        .request(Method::GET, "/api/host/config", None, headers(None))
        .await;
    assert_eq!(status, 200);
    assert_eq!(value, json!({"revision":0,"routes":[]}));
}

#[tokio::test]
async fn i07_nested_json_types_keys_limits_and_unsafe_targets_are_atomic_rejections() {
    let host = Host::new().await;
    let good = host.body(0);
    let raw = good.to_string();
    let mut invalid = vec![
        "null".to_owned(),
        "[]".into(),
        "{".into(),
        raw.replacen(
            "\"expected_revision\":0",
            "\"expected_revision\":0,\"expected_revision\":0",
            1,
        ),
        raw.replacen(
            "\"name\":\"Local\"",
            "\"name\":\"Local\",\"name\":\"Local\"",
            1,
        ),
        raw.replacen(
            "\"mode\":\"replace\"",
            "\"mode\":\"replace\",\"mode\":\"keep\"",
            1,
        ),
    ];
    for (pointer, value) in [
        ("/expected_revision", json!(null)),
        ("/expected_revision", json!(-1)),
        ("/expected_revision", json!(0.0)),
        ("/expected_revision", json!(9223372036854775808_u64)),
        ("/route", json!(null)),
        ("/route/name", json!(null)),
        ("/route/model", json!("")),
        ("/route/max_tokens", json!(15)),
        ("/route/max_tokens", json!(65537)),
        ("/route/max_tokens", json!(16.0)),
        ("/route/parallel_limit", json!(0)),
        ("/route/parallel_limit", json!(9)),
        ("/route/parallel_limit", json!("1")),
        ("/credential", json!(null)),
        ("/credential", json!({"mode":"keep","value":null})),
        ("/credential", json!({"mode":"replace","value":""})),
        ("/credential", json!({"mode":"replace","value":null})),
        ("/credential", json!({"mode":"replace"})),
        ("/credential", json!({"mode":"other"})),
    ] {
        let mut body = good.clone();
        *body.pointer_mut(pointer).unwrap() = value;
        invalid.push(body.to_string());
    }
    for pointer in ["", "/route", "/credential"] {
        let mut body = good.clone();
        body.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!(1));
        invalid.push(body.to_string());
    }
    for url in [
        "http://localhost/v1",
        "http://example.invalid/v1",
        "http://127.1/v1",
        "http://2130706433/v1",
        "https://user:pass@example.invalid/v1",
        "https://example.invalid/v1?x=1",
        "https://example.invalid/v1#x",
        "https://example.invalid/a/%2f/b",
        "https://example.invalid/a/../v1",
        "https://example.invalid/a/%2e%2e/v1",
    ] {
        let mut body = good.clone();
        body["route"]["base_url"] = json!(url);
        invalid.push(body.to_string());
    }
    for body in invalid {
        error(
            host.request(
                Method::PUT,
                "/api/host/routes/local",
                Some(body),
                headers(Some("invalid")),
            )
            .await,
            400,
            "host_config_invalid",
        );
        host.unchanged();
    }
    for id in ["newapi-account", "UPPER", "bad_id"] {
        error(
            host.request(
                Method::PUT,
                &format!("/api/host/routes/{id}"),
                Some(raw.clone()),
                headers(Some("invalid")),
            )
            .await,
            400,
            "host_config_invalid",
        );
    }
    for key in [None, Some("a,b"), Some(" ")] {
        error(
            host.request(
                Method::PUT,
                "/api/host/routes/local",
                Some(raw.clone()),
                headers(key),
            )
            .await,
            400,
            "host_config_invalid",
        );
    }
    let mut duplicate = headers(Some("same"));
    duplicate.append("idempotency-key", HeaderValue::from_static("same"));
    error(
        host.request(
            Method::PUT,
            "/api/host/routes/local",
            Some(raw.clone()),
            duplicate,
        )
        .await,
        400,
        "host_config_invalid",
    );
    let mut wrong_type = headers(Some("first"));
    wrong_type.insert("content-type", HeaderValue::from_static("text/plain"));
    error(
        host.request(Method::PUT, "/api/host/routes/local", Some(raw), wrong_type)
            .await,
        415,
        "host_config_invalid",
    );
    error(
        host.request(
            Method::PUT,
            "/api/host/routes/local",
            Some(" ".repeat(1_048_577)),
            headers(Some("large")),
        )
        .await,
        413,
        "host_config_invalid",
    );
    for body in [
        r#"{"key":"a","key":"b"}"#,
        r#"{"key":"a","extra":1}"#,
        r#"{"key":null}"#,
    ] {
        error(
            host.request(
                Method::POST,
                "/api/host/config/lookup",
                Some(body.into()),
                headers(None),
            )
            .await,
            400,
            "host_config_invalid",
        );
    }
    host.unchanged();
}

#[tokio::test]
async fn i07_original_receipt_replay_current_snapshot_and_fixed_errors() {
    let host = Host::new().await;
    let mut keep = host.body(0);
    keep["credential"] = json!({"mode":"keep"});
    error(
        host.put(keep, "keep-new").await,
        409,
        "host_config_credential_required",
    );
    host.unchanged();
    let (status, first) = host.put(host.body(0), "first").await;
    assert_eq!(status, 200);
    assert_eq!(first["replayed"], false);
    assert_eq!(first["receipt"]["route_id"], "local");
    assert_eq!(first["receipt"]["route"]["id"], "local");
    assert_eq!(first["receipt"]["revision"], 1);
    assert_eq!(first["receipt"]["changed"], true);
    assert_eq!(first["receipt"]["route"]["credential_source"], "stored");
    assert!(first["receipt"]["route"]["key_env"].is_null());
    let (_, replay) = host.put(host.body(0), "first").await;
    assert_eq!(replay["receipt"], first["receipt"]);
    assert_eq!(replay["replayed"], true);
    error(
        host.put(host.body(1), "first").await,
        409,
        "host_config_idempotency_conflict",
    );
    error(
        host.put(host.body(0), "stale").await,
        409,
        "host_config_revision_conflict",
    );
    let (status, no_op) = host.put(host.body(1), "no-op").await;
    assert_eq!(status, 200);
    assert_eq!(no_op["receipt"]["changed"], false);
    assert_eq!(no_op["receipt"]["revision"], 1);
    let mut update = host.body(1);
    update["route"]["model"] = json!("new-model");
    update["credential"] = json!({"mode":"keep"});
    let (status, second) = host.put(update, "second").await;
    assert_eq!(status, 200);
    assert_eq!(second["receipt"]["revision"], 2);
    let (status, old) = host
        .request(
            Method::POST,
            "/api/host/config/lookup",
            Some(json!({"key":"first"}).to_string()),
            headers(None),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(old["receipt"], first["receipt"]);
    assert_eq!(old["replayed"], true);
    let (_, current) = host
        .request(Method::GET, "/api/host/config", None, headers(None))
        .await;
    assert_eq!(current["revision"], 2);
    assert_eq!(current["routes"][0]["model"], "new-model");
    error(
        host.request(
            Method::POST,
            "/api/host/config/lookup",
            Some(json!({"key":"absent"}).to_string()),
            headers(None),
        )
        .await,
        404,
        "host_config_not_found",
    );
    assert_eq!(
        host.calls.load(Ordering::SeqCst),
        0,
        "configuration never calls Provider"
    );
}
