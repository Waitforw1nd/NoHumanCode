//! NEXT-08 real Host project admission, CAS, isolation and CLI integration.
use axum::{
    routing::{get, post},
    Json, Router,
};
use peachsh::{
    domain::{Route, Settings},
    engine::Engine,
    server::{self, App},
    store::Store,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
struct Host {
    origin: String,
    engine: Arc<Engine>,
    calls: Arc<AtomicUsize>,
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
    let actual = response.status().as_u16();
    let value: Value = response.json().await.unwrap();
    assert_eq!(actual, status, "{value}");
    assert!(!value.to_string().contains("synthetic-network-key"));
    value
}

impl Host {
    async fn register(&self, name: &str) -> Value {
        let path = self._dir.path().join(name);
        std::fs::create_dir(&path).unwrap();
        body(
            self.request(reqwest::Method::POST, "/api/projects")
                .header("Idempotency-Key", format!("register-{name}"))
                .json(&json!({"name":name,"root_path":path}))
                .send()
                .await
                .unwrap(),
            201,
        )
        .await
    }
    async fn get(&self, path: &str) -> Value {
        body(
            self.request(reqwest::Method::GET, path)
                .send()
                .await
                .unwrap(),
            200,
        )
        .await
    }
    async fn put(&self, path: &str, value: Value, status: u16) -> Value {
        body(
            self.request(reqwest::Method::PUT, path)
                .json(&value)
                .send()
                .await
                .unwrap(),
            status,
        )
        .await
    }
    async fn configure(&self, id: &str) -> Value {
        self.put(&format!("/api/projects/{id}/config"),json!({"expected_revision":1,"config":{"allowed_route_ids":["route-a"],"default_route_id":"route-a","tools":false,"write_scopes":[],"allow_commands":false}}),200).await
    }
    async fn grant(&self, id: &str) {
        let preview = self
            .preview(json!({"kind":"route","id":"route-a","project_id":id}))
            .await;
        body(
            self.authorize(
                &preview,
                json!(["models", "completion"]),
                &format!("grant-{id}"),
            )
            .await,
            201,
        )
        .await;
    }
    async fn run(&self, id: &str, key: &str) -> Value {
        body(
            self.request(reqwest::Method::POST, &format!("/api/projects/{id}/runs"))
                .header("Idempotency-Key", key)
                .json(&json!({"expected_config_revision":2,"request":request()}))
                .send()
                .await
                .unwrap(),
            200,
        )
        .await
    }
    async fn settle(&self, id: &str) -> Value {
        for _ in 0..100 {
            let run = self.get(&format!("/api/runs/{id}")).await;
            if run["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| !matches!(t["status"].as_str(), Some("running" | "queued")))
            {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("run did not settle")
    }
    async fn cli(&self, args: &[&str]) -> std::process::Output {
        tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
                .args(["--port", self.origin.rsplit(':').next().unwrap(), "--json"])
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap()
    }
}
fn request() -> Value {
    json!({"title":"project chat","kind":"chat","tasks":[{"name":"agent","role":"test","route_id":"route-a","prompt":"hello","depends_on":[],"write_scopes":[],"tools":false,"allow_commands":false,"max_rounds":3}]})
}
#[tokio::test]
async fn p01_project_management_cas_strict_json_and_zero_network() {
    let h = Host::new().await;
    let absent = body(h.post("/api/runs", request()).await, 409).await;
    assert_eq!(absent["code"], "project_selection_required");
    let a = h.register("A").await;
    let id = a["id"].as_str().unwrap();
    let config = h.get(&format!("/api/projects/{id}/config")).await;
    assert_eq!(config["revision"], 1);
    assert_eq!(
        config["config"],
        json!({"allowed_route_ids":[],"default_route_id":null,"tools":false,"write_scopes":[],"allow_commands":false})
    );
    assert_eq!(h.get(&format!("/api/projects/{id}")).await, a);
    assert_eq!(
        h.get("/api/project-selection").await,
        json!({"project_id":null,"revision":0})
    );
    let selected = h
        .put(
            "/api/project-selection",
            json!({"project_id":id,"expected_revision":0}),
            200,
        )
        .await;
    assert_eq!(selected["revision"], 1);
    assert_eq!(
        h.put(
            "/api/project-selection",
            json!({"project_id":id,"expected_revision":0}),
            409
        )
        .await["code"],
        "project_revision_conflict"
    );
    h.configure(id).await;
    assert_eq!(
        h.put(
            &format!("/api/projects/{id}/config"),
            json!({"expected_revision":1,"config":config["config"]}),
            409
        )
        .await["code"],
        "project_revision_conflict"
    );
    for invalid in [
        json!({"expected_revision":2,"tools":true}),
        json!({"expected_revision":2,"config":{"allowed_route_ids":[],"default_route_id":null,"tools":[],"write_scopes":[],"allow_commands":false}}),
        json!({"expected_revision":2,"config":{"allowed_route_ids":[],"default_route_id":null,"tools":false,"write_scopes":[],"allow_commands":false,"extra":1}}),
    ] {
        h.put(&format!("/api/projects/{id}/config"), invalid, 400)
            .await;
    }
    body(
        h.request(reqwest::Method::PUT, "/api/project-selection")
            .header("content-type", "application/json")
            .body(format!(
                r#"{{"project_id":"{id}","expected_revision":1,"expected_revision":1}}"#
            ))
            .send()
            .await
            .unwrap(),
        400,
    )
    .await;
    for path in [
        "/api/projects",
        "/api/project-selection",
        &format!("/api/projects/{id}/config"),
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
    let settings = h.get("/api/settings").await;
    let mut changed = settings["settings"].clone();
    changed["workspace"] = json!(h._dir.path().join("A"));
    assert_eq!(
        h.put("/api/settings", json!({"settings":changed,"keys":{}}), 409)
            .await["code"],
        "workspace_selection_managed"
    );
    assert_eq!(h.get("/api/settings").await, settings);
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        body(
            h.post(
                "/api/wasm/run",
                json!({"plugin":"x","input":{},"project_id":id})
            )
            .await,
            409
        )
        .await["code"],
        "project_operation_unsupported"
    );
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn p02_project_route_isolation_old_replay_and_session_turn() {
    let h = Host::new().await;
    let a = h.register("A").await;
    let b = h.register("B").await;
    let a = a["id"].as_str().unwrap();
    let b = b["id"].as_str().unwrap();
    h.configure(a).await;
    h.configure(b).await;
    h.grant(a).await;
    let denied = body(
        h.post("/api/routes/route-a/models", json!({"project_id":b}))
            .await,
        409,
    )
    .await;
    assert_eq!(denied["error"]["code"], "required");
    assert_eq!(denied["error"]["calls"], json!([]));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    h.put(
        "/api/project-selection",
        json!({"project_id":a,"expected_revision":0}),
        200,
    )
    .await;
    let old = body(
        h.request(reqwest::Method::POST, "/api/runs")
            .header("Idempotency-Key", "default-run")
            .json(&request())
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    let id = old["id"].as_str().unwrap();
    h.settle(id).await;
    h.put(
        "/api/project-selection",
        json!({"project_id":b,"expected_revision":1}),
        200,
    )
    .await;
    let before = h.calls.load(Ordering::SeqCst);
    let replay = body(
        h.request(reqwest::Method::POST, "/api/runs")
            .header("Idempotency-Key", "default-run")
            .json(&request())
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(replay["id"], old["id"]);
    assert_eq!(h.calls.load(Ordering::SeqCst), before);
    let context = h.get(&format!("/api/runs/{id}/context")).await;
    assert_eq!(context["project"]["id"], a);
    let binding = h
        .get(&format!(
            "/api/tasks/{}/project",
            context["tasks"][0]["legacy_task_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(binding["binding"]["project_id"], a);
    let turn=body(h.request(reqwest::Method::POST,&format!("/api/sessions/{}/turns",context["session"]["id"].as_str().unwrap())).header("Idempotency-Key","followup-a").json(&json!({"agent_id":context["tasks"][0]["agent_id"],"expected_last_turn_id":context["latest_turn"]["id"],"message":"again"})).send().await.unwrap(),201).await;
    assert_eq!(turn["turn"]["project_id"], a);
    h.settle(id).await;
    assert_eq!(h.get(&format!("/api/runs?project_id={b}")).await, json!([]));
    let history = h.get(&format!("/api/runs?project_id={a}")).await;
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0].as_object().unwrap().len(), 4);
    h.grant(b).await;
    let explicit = h.run(b, "explicit-b").await;
    h.settle(explicit["id"].as_str().unwrap()).await;
    assert_eq!(
        body(
            h.request(reqwest::Method::POST, &format!("/api/projects/{a}/runs"))
                .header("Idempotency-Key", "explicit-b")
                .json(&json!({"expected_config_revision":2,"request":request()}))
                .send()
                .await
                .unwrap(),
            409
        )
        .await["code"],
        "conflict"
    );
}
#[tokio::test]
async fn p09_real_cli_against_project_host() {
    let h = Host::new().await;
    let a = h.register("cli-project").await;
    let id = a["id"].as_str().unwrap();
    h.configure(id).await;
    h.grant(id).await;
    for args in [
        vec!["project", "get", id],
        vec!["project", "config", "get", id],
        vec!["project", "select", id, "--expected-revision", "0"],
        vec!["project", "current"],
    ] {
        let out = h.cli(&args).await;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = h
        .cli(&[
            "run",
            "start",
            "--project",
            id,
            "--expected-config-revision",
            "2",
            "--title",
            "cli",
            "--route",
            "route-a",
            "--message",
            "hello",
            "--key",
            "cli-run",
        ])
        .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run: Value = serde_json::from_slice(&out.stdout).unwrap();
    h.settle(run["id"].as_str().unwrap()).await;
    let out = h.cli(&["run", "list", "--project", id]).await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn p09_nested_json_rejected_before_any_project_write_or_network() {
    let h = Host::new().await;
    let a = h.register("strict").await;
    let id = a["id"].as_str().unwrap();
    h.configure(id).await;
    h.grant(id).await;
    let config_path = format!("/api/projects/{id}/config");
    let run_path = format!("/api/projects/{id}/runs");
    let before = h.get(&config_path).await;
    for config in [
        json!([["route-a"], "route-a", false, [], false]),
        json!({"allowed_route_ids":["route-a"],"tools":false,"write_scopes":[],"allow_commands":false}),
    ] {
        h.put(
            &config_path,
            json!({"expected_revision":2,"config":config}),
            400,
        )
        .await;
        assert_eq!(h.get(&config_path).await, before);
    }
    let mut unknown_project = request();
    unknown_project["project_id"] = json!("other");
    let mut unknown_task = request();
    unknown_task["tasks"][0]["project_id"] = json!("other");
    let mut array_task = request();
    array_task["tasks"][0] = json!(["agent", "test", "route-a", "hello", [], [], false, false, 3]);
    for (index, request) in [
        json!(["project chat", "chat", []]),
        unknown_project,
        unknown_task,
        array_task,
    ]
    .into_iter()
    .enumerate()
    {
        body(
            h.request(reqwest::Method::POST, &run_path)
                .header("Idempotency-Key", format!("strict-{index}"))
                .json(&json!({"expected_config_revision":2,"request":request}))
                .send()
                .await
                .unwrap(),
            400,
        )
        .await;
        assert_eq!(h.get("/api/runs").await, json!([]));
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    }
    let duplicate = format!(
        r#"{{"expected_revision":2,"config":{{"allowed_route_ids":[],"default_route_id":null,"tools":false,"tools":true,"write_scopes":[],"allow_commands":false}}}}"#
    );
    body(
        h.request(reqwest::Method::PUT, &config_path)
            .header("content-type", "application/json")
            .body(duplicate)
            .send()
            .await
            .unwrap(),
        400,
    )
    .await;
    let duplicate = r#"{"expected_config_revision":2,"request":{"title":"strict","kind":"chat","tasks":[{"name":"agent","role":"test","route_id":"route-a","prompt":"one","prompt":"two"}]}}"#;
    body(
        h.request(reqwest::Method::POST, &run_path)
            .header("Idempotency-Key", "duplicate-task")
            .header("content-type", "application/json")
            .body(duplicate)
            .send()
            .await
            .unwrap(),
        400,
    )
    .await;
    assert_eq!(h.get(&config_path).await, before);
    assert_eq!(h.get("/api/runs").await, json!([]));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.engine.store.projects().unwrap().len(), 1);
}

#[tokio::test]
async fn p09_wasm_http_list_and_run_are_explicitly_unsupported() {
    let h = Host::new().await;
    let project = h.register("wasm").await;
    let id = project["id"].as_str().unwrap();
    let plugins = h._dir.path().join("wasm/.peachsh/plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let manifest = plugins.join("probe.json");
    let bytes = b"invalid manifest: must never be loaded";
    std::fs::write(&manifest, bytes).unwrap();
    for path in [
        "/api/wasm/plugins".to_owned(),
        format!("/api/wasm/plugins?project_id={id}"),
        "/api/wasm/plugins?project_id=missing".into(),
    ] {
        assert_eq!(
            body(
                h.request(reqwest::Method::GET, &path).send().await.unwrap(),
                409
            )
            .await["code"],
            "project_operation_unsupported"
        );
    }
    for value in [
        json!({"plugin":"probe","input":{}}),
        json!({"plugin":"probe","input":{},"project_id":id}),
    ] {
        assert_eq!(
            body(h.post("/api/wasm/run", value).await, 409).await["code"],
            "project_operation_unsupported"
        );
    }
    assert_eq!(std::fs::read(manifest).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(plugins).unwrap().count(), 1);
    assert_eq!(h.get("/api/runs").await, json!([]));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}
