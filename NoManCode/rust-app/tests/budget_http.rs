//! NEXT-14 real HTTP Router, durable budget and localhost Provider integration.
use axum::{
    Json, Router,
    routing::{get, post},
};
use peachsh::{
    domain::{Route, Settings},
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
    calls: Arc<AtomicUsize>,
    project_id: String,
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
    async fn new(usage: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let models = calls.clone();
        let complete = calls.clone();
        let completion = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}],"usage":usage})
        );
        let provider = Router::new()
            .route(
                "/v1/models",
                get(move || {
                    let calls = models.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"data":[{"id":"mock-model"}]}))
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(move || {
                    let calls = complete.clone();
                    let payload = completion.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        ([("content-type", "text/event-stream")], payload)
                    }
                }),
            );
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
        let project = engine
            .register_project(peachsh::project_config::RegisterProject {
                name: "network-fixture".into(),
                root_path: dir.path().to_string_lossy().into(),
                idempotency_key: "network-fixture-project".into(),
            })
            .await
            .unwrap();
        engine
            .configure_project(peachsh::project_config::ConfigureProject {
                project_id: project.id.clone(),
                expected_revision: 1,
                config: peachsh::project_config::ProjectConfig {
                    allowed_route_ids: vec!["route-a".into()],
                    default_route_id: Some("route-a".into()),
                    tools: true,
                    write_scopes: vec!["*".into()],
                    allow_commands: true,
                },
            })
            .await
            .unwrap();
        engine
            .select_project(peachsh::project_config::SelectProject {
                project_id: project.id.clone(),
                expected_revision: 0,
            })
            .await
            .unwrap();
        let project_id = project.id.0;
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
            calls,
            project_id,
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
    async fn preview(&self, mut subject: Value) -> Value {
        if subject["kind"] == "route" {
            subject["project_id"] = json!(self.project_id);
        }
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
fn valid_usage() -> Value {
    json!({"prompt_tokens":2,"completion_tokens":3,"total_tokens":5})
}
impl Host {
    fn budget(&self) -> String {
        format!("/api/projects/{}/budget", self.project_id)
    }
    async fn budget_set(
        &self,
        revision: u64,
        limit: u64,
        reservation: u64,
        key: &str,
    ) -> reqwest::Response {
        self.request(reqwest::Method::PUT,&self.budget()).json(&json!({"expected_revision":revision,"idempotency_key":key,"config":{"limit_tokens":limit,"per_call_reservation_tokens":reservation}})).send().await.unwrap()
    }
    async fn status(&self) -> Value {
        body(
            self.request(reqwest::Method::GET, &format!("{}/status", self.budget()))
                .send()
                .await
                .unwrap(),
            200,
        )
        .await
    }
    async fn grant(&self) {
        let preview = self.preview(json!({"kind":"route","id":"route-a"})).await;
        body(
            self.authorize(&preview, json!(["models", "completion"]), "budget-grant")
                .await,
            201,
        )
        .await;
    }
    async fn check(&self) -> reqwest::Response {
        self.post(
            "/api/routes/route-a/check",
            json!({"project_id":self.project_id}),
        )
        .await
    }
    async fn diagnostic(&self, id: &str) -> Value {
        body(
            self.request(
                reqwest::Method::GET,
                &format!("{}/calls/{id}", self.budget()),
            )
            .send()
            .await
            .unwrap(),
            200,
        )
        .await
    }
}
#[tokio::test]
async fn budget_http_config_cas_replay_forward_history_and_no_query_side_effects() {
    let h = Host::new(valid_usage()).await;
    let initial = h.status().await;
    assert_eq!(initial["mode"], "unconfigured");
    assert_eq!(initial["snapshot"]["revision"], 0);
    assert_eq!(initial["available_tokens"], Value::Null);
    h.grant().await;
    let historical = body(h.check().await, 200).await;
    let call_id = historical["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["purpose"] == "completion")
        .unwrap()["call_id"]
        .as_str()
        .unwrap();
    assert_eq!(h.diagnostic(call_id).await["managed"], false);
    let first = body(h.budget_set(0, 100, 10, "set-1").await, 200).await;
    assert_eq!(first["replayed"], false);
    body(h.budget_set(1, 200, 20, "set-2").await, 200).await;
    let replay = body(h.budget_set(0, 100, 10, "set-1").await, 200).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["snapshot"], first["snapshot"]);
    assert_eq!(
        body(h.budget_set(0, 100, 10, "set-stale").await, 409).await["error"]["code"],
        "budget_revision_conflict"
    );
    assert_eq!(
        body(h.budget_set(0, 101, 10, "set-1").await, 409).await["error"]["code"],
        "budget_conflict"
    );
    let before = h.calls.load(Ordering::SeqCst);
    let status = h.status().await;
    assert_eq!(status["snapshot"]["revision"], 2);
    assert_eq!(status["spent_tokens"], 0);
    assert_eq!(h.diagnostic(call_id).await["managed"], false);
    assert_eq!(h.status().await, status);
    assert_eq!(h.calls.load(Ordering::SeqCst), before);
}
#[tokio::test]
async fn budget_http_route_check_preserves_models_receipt_and_blocks_completion_before_claim() {
    let h = Host::new(valid_usage()).await;
    h.grant().await;
    body(h.budget_set(0, 0, 10, "zero").await, 200).await;
    let rejected = body(h.check().await, 409).await;
    assert_eq!(rejected["error"]["code"], "budget_exceeded");
    assert_eq!(rejected["error"]["dispatch_state"], "not_sent");
    assert_eq!(rejected["error"]["retryable"], false);
    assert_eq!(rejected["error"]["calls"].as_array().unwrap().len(), 1);
    assert_eq!(rejected["error"]["calls"][0]["purpose"], "models");
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    let status = h.status().await;
    assert_eq!(status["spent_tokens"], 0);
    assert_eq!(status["held_tokens"], 0);
    assert_eq!(status["claimed_count"], 0);
    assert_eq!(status["blocked_reason"], "exceeded");
    let models_id = rejected["error"]["calls"][0]["call_id"].as_str().unwrap();
    assert_eq!(
        body(
            h.request(
                reqwest::Method::GET,
                &format!("{}/calls/{models_id}", h.budget())
            )
            .send()
            .await
            .unwrap(),
            404
        )
        .await["error"]["code"],
        "budget_not_found"
    );
}
#[tokio::test]
async fn budget_http_valid_usage_settles_and_call_is_bound_to_project() {
    let h = Host::new(valid_usage()).await;
    h.grant().await;
    body(h.budget_set(0, 100, 10, "set").await, 200).await;
    let response = body(h.check().await, 200).await;
    let completion = response["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["purpose"] == "completion")
        .unwrap();
    let id = completion["call_id"].as_str().unwrap();
    let diagnosis = h.diagnostic(id).await;
    assert_eq!(diagnosis["state"], "settled");
    assert_eq!(diagnosis["usage"], valid_usage());
    assert_eq!(diagnosis["call"], *completion);
    let status = h.status().await;
    assert_eq!(status["spent_tokens"], 5);
    assert_eq!(status["held_tokens"], 0);
    assert_eq!(status["available_tokens"], 95);
    assert_eq!(
        body(
            h.request(
                reqwest::Method::GET,
                &format!("/api/projects/project-other/budget/calls/{id}")
            )
            .send()
            .await
            .unwrap(),
            404
        )
        .await["error"]["code"],
        "budget_not_found"
    );
    assert_eq!(h.status().await, status);
    assert_eq!(h.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn budget_http_missing_invalid_and_overrun_survive_config_increase() {
    for (usage, state, spent, held, code) in [
        (Value::Null, "usage_missing", 0, 10, "budget_unresolved"),
        (
            json!({"total_tokens":1}),
            "usage_invalid",
            0,
            10,
            "budget_unresolved",
        ),
        (
            json!({"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}),
            "overrun",
            11,
            0,
            "budget_overrun",
        ),
    ] {
        let h = Host::new(usage).await;
        h.grant().await;
        body(h.budget_set(0, 100, 10, "set").await, 200).await;
        let response = body(h.check().await, 200).await;
        let id = response["calls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["purpose"] == "completion")
            .unwrap()["call_id"]
            .as_str()
            .unwrap();
        let diagnosis = h.diagnostic(id).await;
        assert_eq!(diagnosis["state"], state);
        assert_eq!(diagnosis["call"]["success"], true);
        body(h.budget_set(1, 1000, 50, "increase").await, 200).await;
        assert_eq!(body(h.check().await, 409).await["error"]["code"], code);
        assert_eq!(h.calls.load(Ordering::SeqCst), 3);
        let status = h.status().await;
        assert_eq!(status["spent_tokens"], spent);
        assert_eq!(status["held_tokens"], held);
        assert_eq!(h.diagnostic(id).await, diagnosis);
    }
}
#[tokio::test]
async fn budget_http_auth_strict_body_and_safe_extractor_errors() {
    let h = Host::new(valid_usage()).await;
    for path in [
        h.budget(),
        format!("{}/status", h.budget()),
        format!("{}/calls/call-a", h.budget()),
    ] {
        assert_eq!(
            client()
                .get(format!("{}{path}", h.origin))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    let valid=json!({"expected_revision":0,"idempotency_key":"set","config":{"limit_tokens":100,"per_call_reservation_tokens":10}}).to_string();
    let invalid = [
        valid.replace(
            "\"limit_tokens\":100",
            "\"limit_tokens\":100,\"limit_tokens\":1",
        ),
        valid.replace("100", "-1"),
        valid.replace("100", "1.5"),
        valid.replace("100", "1000000000001"),
        valid.replace("\"config\":", "\"extra\":true,\"config\":"),
        valid.replace("\"expected_revision\":0,", ""),
    ];
    for payload in invalid {
        assert_ne!(payload, valid, "malformed fixture must actually differ");
        let response = h
            .request(reqwest::Method::PUT, &h.budget())
            .header("content-type", "application/json")
            .body(payload)
            .send()
            .await
            .unwrap();
        let value = body(response, 400).await;
        assert_eq!(value["error"]["code"], "budget_invalid");
        assert_eq!(value["error"]["calls"], json!([]));
    }
    for (content_type, payload, status) in [
        ("text/plain", valid, 415),
        ("application/json", " ".repeat(1_048_577), 413),
    ] {
        let response = h
            .request(reqwest::Method::PUT, &h.budget())
            .header("content-type", content_type)
            .body(payload)
            .send()
            .await
            .unwrap();
        let value = body(response, status).await;
        assert_eq!(value["error"]["code"], "budget_invalid");
        assert_eq!(value["error"]["dispatch_state"], "not_sent");
        assert_eq!(value["error"]["retryable"], false);
    }
    assert_eq!(h.status().await["mode"], "unconfigured");
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}
