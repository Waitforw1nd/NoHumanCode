//! NEXT-13 real Host worktree HTTP, independent grants and durable diagnostics.
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
   .route("/v1/chat/completions",post(move|Json(body):Json<Value>|{let calls=complete.clone();async move{
 calls.fetch_add(1,Ordering::SeqCst);
 let messages=body["messages"].as_array().unwrap();
 let write=messages.iter().any(|m|m["content"].as_str().is_some_and(|s|s.contains("write isolated")));
 let finished=messages.iter().any(|m|m["role"]=="tool");
 let chunk=if write && !finished {json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"isolated-write","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"src/output.txt\",\"content\":\"isolated-result\"}"}}]},"finish_reason":"tool_calls"}]})}else{json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]})};
 ([("content-type","text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n"))
}}))
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

async fn git(root: &std::path::Path, args: &[&str]) -> String {
    let o = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("git")
            .args(["-c", "core.autocrlf=false", "-c", "core.hooksPath=NUL"])
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap().trim().into()
}
async fn source(h: &Host) -> (Value, std::path::PathBuf, String) {
    let project = h.register("source").await;
    let root = h._dir.path().join("source");
    git(&root, &["init", "--initial-branch=main"]).await;
    std::fs::write(root.join("tracked.txt"), "original\n").unwrap();
    git(&root, &["add", "tracked.txt"]).await;
    git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "commit",
            "-m",
            "fixture",
        ],
    )
    .await;
    let head = git(&root, &["rev-parse", "HEAD"]).await;
    (project, root, head)
}
fn rows(path: &std::path::Path) -> Vec<String> {
    let db = rusqlite::Connection::open(path).unwrap();
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|name| {
            let mut s = db
                .prepare(&format!(
                    "SELECT rowid,* FROM \"{}\" ORDER BY rowid",
                    name.replace('"', "\"\"")
                ))
                .unwrap();
            let n = s.column_count();
            let v = s
                .query_map([], |r| {
                    Ok((0..n)
                        .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                        .collect::<Vec<_>>())
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            format!("{name}:{v:?}")
        })
        .collect()
}
fn create_body(h: &Host, head: &str, revision: u64) -> Value {
    json!({"name":"isolated","parent_path":h._dir.path(),"expected_config_revision":revision,"expected_head":head})
}
#[tokio::test]
async fn w01_w02_worktree_project_has_independent_authorization_and_original_session_root() {
    let h = Host::new().await;
    let (p, root, head) = source(&h).await;
    let source_id = p["id"].as_str().unwrap();
    h.configure(source_id).await;
    h.grant(source_id).await;
    let shared = h.run(source_id, "shared-first").await;
    h.settle(shared["id"].as_str().unwrap()).await;
    let context = h
        .get(&format!(
            "/api/runs/{}/context",
            shared["id"].as_str().unwrap()
        ))
        .await;
    let session = context["session"]["id"].as_str().unwrap();
    let turn = context["latest_turn"]["id"].as_str().unwrap();
    let agent = context["tasks"][0]["agent_id"].as_str().unwrap();
    let old_binding = h
        .get(&format!(
            "/api/tasks/{}/project",
            shared["tasks"][0]["id"].as_str().unwrap()
        ))
        .await;
    let old_grant = h
        .preview(json!({"kind":"route","id":"route-a","project_id":source_id}))
        .await;
    let index = std::fs::read(root.join(".git/index")).unwrap();
    let tracked = std::fs::read(root.join("tracked.txt")).unwrap();
    let calls = h.calls.load(Ordering::SeqCst);
    let endpoint = format!("/api/projects/{source_id}/worktrees");
    let receipt = body(
        h.request(reqwest::Method::POST, &endpoint)
            .header("Idempotency-Key", "tree-create")
            .json(&create_body(&h, &head, 2))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(receipt["state"], "succeeded");
    let target_id = receipt["project_id"].as_str().unwrap();
    assert_ne!(target_id, source_id);
    let target = std::path::PathBuf::from(receipt["target_path"].as_str().unwrap());
    let op = receipt["operation_id"].as_str().unwrap();
    assert_eq!(h.calls.load(Ordering::SeqCst), calls);
    assert_eq!(git(&root, &["rev-parse", "HEAD"]).await, head);
    assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
    assert_eq!(std::fs::read(root.join("tracked.txt")).unwrap(), tracked);
    assert_eq!(git(&target, &["rev-parse", "HEAD"]).await, head);
    assert_eq!(
        git(&target, &["branch", "--show-current"]).await,
        format!("codex/workspace/{op}")
    );
    assert!(target.join(".git").is_file());
    assert_eq!(
        git(
            &root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        )
        .await,
        git(
            &target,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        )
        .await
    );
    let config = h.get(&format!("/api/projects/{target_id}/config")).await;
    assert_eq!(config["revision"], 1);
    assert_eq!(
        config["config"],
        json!({"allowed_route_ids":[],"default_route_id":null,"tools":false,"write_scopes":[],"allow_commands":false})
    );
    h.configure(target_id).await;
    let denied = h
        .request(
            reqwest::Method::POST,
            &format!("/api/projects/{target_id}/runs"),
        )
        .header("Idempotency-Key", "no-grant")
        .json(&json!({"expected_config_revision":2,"request":request()}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 409);
    assert_eq!(h.calls.load(Ordering::SeqCst), calls);
    h.put(&format!("/api/projects/{target_id}/config"),json!({"expected_revision":2,"config":{"allowed_route_ids":["route-a"],"default_route_id":"route-a","tools":true,"write_scopes":["src"],"allow_commands":false}}),200).await;
    h.grant(target_id).await;
    let mut write = request();
    write["tasks"][0]["prompt"] = json!("write isolated");
    write["tasks"][0]["tools"] = json!(true);
    write["tasks"][0]["write_scopes"] = json!(["src"]);
    let isolated = body(
        h.request(
            reqwest::Method::POST,
            &format!("/api/projects/{target_id}/runs"),
        )
        .header("Idempotency-Key", "isolated-first")
        .json(&json!({"expected_config_revision":3,"request":write}))
        .send()
        .await
        .unwrap(),
        200,
    )
    .await;
    let task = isolated["tasks"][0]["id"].as_str().unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let approvals = h.get(&format!("/api/tasks/{task}/approvals")).await;
            if let Some(a) = approvals["approvals"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["status"] == "pending")
            {
                break a.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    body(
        h.post(
            &format!(
                "/api/approvals/{}/decision",
                approval["id"].as_str().unwrap()
            ),
            json!({"decision":"approve","acknowledge_command_network":false}),
        )
        .await,
        200,
    )
    .await;
    let done = h.settle(isolated["id"].as_str().unwrap()).await;
    assert_eq!(done["tasks"][0]["workspace"], receipt["target_path"]);
    assert_eq!(
        std::fs::read(target.join("src/output.txt")).unwrap(),
        b"isolated-result"
    );
    assert!(!root.join("src/output.txt").exists());
    assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
    let continued = body(
        h.request(
            reqwest::Method::POST,
            &format!("/api/sessions/{session}/turns"),
        )
        .header("Idempotency-Key", "source-next")
        .json(&json!({"agent_id":agent,"expected_last_turn_id":turn,"message":"continue original"}))
        .send()
        .await
        .unwrap(),
        201,
    )
    .await;
    assert_eq!(continued["turn"]["project_id"], source_id);
    let source_done = h.settle(shared["id"].as_str().unwrap()).await;
    assert_eq!(
        source_done["tasks"][0]["workspace"],
        shared["tasks"][0]["workspace"]
    );
    assert_eq!(
        h.get(&format!(
            "/api/tasks/{}/project",
            shared["tasks"][0]["id"].as_str().unwrap()
        ))
        .await,
        old_binding
    );
    assert_eq!(
        h.preview(json!({"kind":"route","id":"route-a","project_id":source_id}))
            .await,
        old_grant
    );
    let db = h._dir.path().join("host.sqlite3");
    let before = rows(&db);
    for _ in 0..3 {
        assert_eq!(h.get(&format!("{endpoint}/{op}")).await, receipt);
        assert_eq!(
            body(
                h.post(
                    &format!("{endpoint}/lookup"),
                    json!({"idempotency_key":"tree-create"})
                )
                .await,
                200
            )
            .await,
            receipt
        );
        let inspect = body(
            h.post(&format!("{endpoint}/{op}/inspect"), json!({})).await,
            200,
        )
        .await;
        assert_eq!(inspect["receipt"], receipt);
        assert_eq!(inspect["observation"]["state"], "present_matching");
    }
    assert_eq!(rows(&db), before);
    assert_eq!(
        body(
            h.request(reqwest::Method::POST, &endpoint)
                .header("Idempotency-Key", "tree-create")
                .json(&create_body(&h, &head, 2))
                .send()
                .await
                .unwrap(),
            200
        )
        .await,
        receipt
    );
    assert_eq!(rows(&db), before);
    assert_eq!(h.engine.store.projects().unwrap().len(), 2);
    let cli = h
        .cli(&[
            "project",
            "worktree",
            "status",
            "--source-project",
            source_id,
            "--key",
            "tree-create",
        ])
        .await;
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&cli.stdout).unwrap(),
        receipt
    );
}
#[tokio::test]
async fn w09_http_strict_dtos_auth_and_cross_source_lookup_have_no_side_effects() {
    let h = Host::new().await;
    let (p, _root, head) = source(&h).await;
    let id = p["id"].as_str().unwrap();
    let endpoint = format!("/api/projects/{id}/worktrees");
    let valid = create_body(&h, &head, 1);
    let db = h._dir.path().join("host.sqlite3");
    let before = rows(&db);
    for header in ["token", "host", "origin", "fetch"] {
        let mut r = client()
            .post(format!("{}{endpoint}", h.origin))
            .header(
                "x-peachsh-token",
                if header == "token" {
                    "wrong"
                } else {
                    "host-token"
                },
            )
            .header("Idempotency-Key", "auth")
            .json(&valid);
        r = match header {
            "token" => r,
            "host" => r.header("host", "evil.invalid"),
            "origin" => r.header("origin", "http://evil.invalid"),
            _ => r.header("sec-fetch-site", "cross-site"),
        };
        assert_eq!(r.send().await.unwrap().status(), 403);
    }
    let op = "12345678-1234-4234-8234-123456789abc";
    for path in [
        format!("{endpoint}/{op}"),
        format!("{endpoint}/lookup"),
        format!("{endpoint}/{op}/inspect"),
    ] {
        let method = if path.ends_with(op) {
            reqwest::Method::GET
        } else {
            reqwest::Method::POST
        };
        let mut r = client().request(method, format!("{}{path}", h.origin));
        if !path.ends_with(op) {
            r = r.json(&json!({}));
        }
        assert_eq!(r.send().await.unwrap().status(), 403);
    }
    for raw in [
        "{}".to_owned(),
        "[]".into(),
        valid
            .to_string()
            .replacen("{", "{\"name\":\"duplicate\",", 1),
        {
            let mut v = valid.clone();
            v["branch"] = json!("main");
            v.to_string()
        },
        {
            let mut v = valid.clone();
            v["expected_config_revision"] = json!(-1);
            v.to_string()
        },
    ] {
        body(
            h.request(reqwest::Method::POST, &endpoint)
                .header("Idempotency-Key", "invalid")
                .header("content-type", "application/json")
                .body(raw)
                .send()
                .await
                .unwrap(),
            400,
        )
        .await;
    }
    let huge = format!("{{\"name\":\"{}\"}}", "x".repeat(1_048_577));
    assert_eq!(
        h.request(reqwest::Method::POST, &endpoint)
            .header("Idempotency-Key", "huge")
            .header("content-type", "application/json")
            .body(huge)
            .send()
            .await
            .unwrap()
            .status(),
        413
    );
    for (path, raw) in [
        (
            format!("{endpoint}/lookup"),
            "{\"idempotency_key\":\"a\",\"idempotency_key\":\"b\"}",
        ),
        (format!("{endpoint}/{op}/inspect"), "{\"force\":true}"),
    ] {
        body(
            h.request(reqwest::Method::POST, &path)
                .header("content-type", "application/json")
                .body(raw)
                .send()
                .await
                .unwrap(),
            400,
        )
        .await;
    }
    assert_eq!(rows(&db), before);
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    let receipt = body(
        h.request(reqwest::Method::POST, &endpoint)
            .header("Idempotency-Key", "good")
            .json(&valid)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    let other = receipt["project_id"].as_str().unwrap();
    let op = receipt["operation_id"].as_str().unwrap();
    let before = rows(&db);
    assert_eq!(
        body(
            h.post(
                &format!("/api/projects/{other}/worktrees/lookup"),
                json!({"idempotency_key":"good"})
            )
            .await,
            404
        )
        .await["code"],
        "worktree_not_found"
    );
    for path in [
        format!("/api/projects/{other}/worktrees/{op}"),
        format!("/api/projects/{other}/worktrees/{op}/inspect"),
    ] {
        let r = if path.ends_with("inspect") {
            h.post(&path, json!({})).await
        } else {
            h.request(reqwest::Method::GET, &path).send().await.unwrap()
        };
        assert_eq!(body(r, 404).await["code"], "worktree_not_found");
    }
    assert_eq!(rows(&db), before);
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}
