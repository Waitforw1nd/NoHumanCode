//! NEXT-10 real HTTP contracts, durable CAS and process restart evidence.
use peachsh::{
    engine::Engine,
    server::{self, App},
    store::Store,
};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
struct Host {
    origin: String,
    engine: Arc<Engine>,
    db: PathBuf,
    task: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Host {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("host.sqlite3");
        let engine = Engine::new(Arc::new(Store::open(&db).unwrap()), 1).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = server::router(App {
            engine: engine.clone(),
            token: "plugin-test-token".into(),
            origin: origin.clone(),
        });
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            origin,
            engine,
            db,
            task,
            _dir: dir,
        }
    }
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        client()
            .request(method, format!("{}{path}", self.origin))
            .header("x-peachsh-token", "plugin-test-token")
    }
    async fn status(&self) -> Value {
        body(
            self.request(Method::GET, "/api/plugins/builtin.files")
                .send()
                .await
                .unwrap(),
            200,
        )
        .await["plugin"]
            .clone()
    }
    async fn set(&self, enabled: bool, revision: u64, key: &str) -> reqwest::Response {
        self.request(Method::PUT, "/api/plugins/builtin.files/enabled")
            .header("Idempotency-Key", key)
            .json(&json!({"enabled":enabled,"expected_revision":revision}))
            .send()
            .await
            .unwrap()
    }
    fn snapshot(&self) -> Vec<String> {
        snapshot(&self.db)
    }
}
fn snapshot(db: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut result = Vec::new();
    for table in tables {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
            .unwrap();
        let n = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            values.push(
                (0..n)
                    .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"),
            );
        }
        values.sort();
        result.push(format!("{table}:{values:?}"));
    }
    result
}
async fn body(response: reqwest::Response, status: u16) -> Value {
    assert_eq!(response.status().as_u16(), status);
    let value: Value = response.json().await.unwrap();
    let text = value.to_string();
    for secret in ["plugin-test-token", "sqlite3", "SELECT ", "INSERT "] {
        assert!(!text.contains(secret), "{value}");
    }
    // Project registration legitimately returns its canonical root. Only plugin
    // diagnostics and error payloads promise to contain no local paths.
    if value.get("plugin").is_some()
        || value.get("plugins").is_some()
        || value.get("current").is_some()
        || value.get("error").is_some()
    {
        for path in ["E:\\", "D:\\"] {
            assert!(!text.contains(path), "{value}");
        }
    }
    value
}
fn error(value: &Value, code: &str) {
    assert_eq!(value["code"], code);
    assert_eq!(value["message"], value["error"]);
    assert_eq!(value["retryable"], false);
    assert!(value.get("ok").is_none());
}

#[tokio::test]
async fn f02_status_is_read_only_and_reports_actual_runtime() {
    let h = Host::new().await;
    let before = h.snapshot();
    let status = h.status().await;
    assert_eq!(status["plugin_id"], "builtin.files");
    assert_eq!(status["version"], "1.2.0");
    assert_eq!(status["scope"], "host");
    assert_eq!(status["desired"]["enabled"], true);
    assert_eq!(status["desired"]["revision"], 0);
    assert_eq!(status["runtime"]["state"], "active");
    assert_eq!(status["runtime"]["admission_open"], true);
    assert_eq!(status["converged"], true);
    assert_eq!(
        status["affected_tools"],
        json!(["read_file", "write_file", "list_files", "search_files"])
    );
    assert_eq!(status["compat_tools"], json!(["run_wasm"]));
    assert_eq!(
        status["affected_dependents"],
        json!(["builtin.engine-tools"])
    );
    assert_eq!(status["runtime"]["providers"].as_array().unwrap().len(), 4);
    assert_eq!(status["runtime"]["effects_count"], 4);
    assert_eq!(status["runtime"]["subscriptions_count"], 0);
    assert_eq!(status["runtime"]["instances"].as_array().unwrap().len(), 2);
    let mut names = vec![];
    for provider in status["runtime"]["providers"].as_array().unwrap() {
        assert_eq!(provider["provider_id"], "builtin.files");
        assert_eq!(
            provider["key"]["version"],
            json!({"major":1,"minor":0,"patch":0})
        );
        names.push(provider["key"]["name"].as_str().unwrap());
    }
    names.sort();
    assert_eq!(
        names,
        vec![
            "tool.list_files",
            "tool.read_file",
            "tool.search_files",
            "tool.write_file"
        ]
    );
    let list = body(
        h.request(Method::GET, "/api/plugins").send().await.unwrap(),
        200,
    )
    .await;
    assert_eq!(list["plugins"].as_array().unwrap().len(), 2);
    assert_eq!(list["plugins"][0], status);
    assert_eq!(list["plugins"][1]["plugin_id"], "builtin.shell");
    assert_eq!(list["plugins"][1]["affected_tools"], json!(["run_command"]));
    assert_eq!(h.snapshot(), before);
    h.engine.disable_builtin_files().unwrap();
    let before = h.snapshot();
    let stopped = h.status().await;
    assert_eq!(stopped["desired"]["enabled"], true);
    assert_eq!(stopped["runtime"]["admission_open"], false);
    assert_eq!(stopped["converged"], false);
    assert_eq!(h.snapshot(), before);
}

#[tokio::test]
async fn f09_strict_inputs_and_auth_have_no_mutations() {
    let h = Host::new().await;
    for path in [
        "/api/plugins",
        "/api/plugins/builtin.files",
        "/api/plugins/builtin.files/enabled",
    ] {
        let before = h.snapshot();
        let method = if path.ends_with("enabled") {
            Method::PUT
        } else {
            Method::GET
        };
        let r = client()
            .request(method, format!("{}{path}", h.origin))
            .json(&json!({"enabled":false,"expected_revision":0}))
            .send()
            .await
            .unwrap();
        error(&body(r, 403).await, "forbidden");
        assert_eq!(h.snapshot(), before);
    }
    for header in ["origin", "host", "sec-fetch-site"] {
        let before = h.snapshot();
        let value = if header == "sec-fetch-site" {
            "cross-site"
        } else {
            "http://untrusted.invalid"
        };
        let r = h
            .request(Method::GET, "/api/plugins")
            .header(header, value)
            .send()
            .await
            .unwrap();
        error(&body(r, 403).await, "forbidden");
        assert_eq!(h.snapshot(), before);
    }
    for raw in [
        r#"{}"#,
        r#"{"enabled":false}"#,
        r#"{"enabled":false,"expected_revision":null}"#,
        r#"{"enabled":false,"expected_revision":-1}"#,
        r#"{"enabled":false,"expected_revision":0.0}"#,
        r#"{"enabled":"false","expected_revision":0}"#,
        r#"{"enabled":false,"expected_revision":0,"extra":1}"#,
        r#"{"enabled":false,"enabled":true,"expected_revision":0}"#,
        r#"{"enabled":false,"expected_revision":0,"expected_revision":1}"#,
        r#"null"#,
        r#"[]"#,
        r#"{"#,
    ] {
        let before = h.snapshot();
        let r = h
            .request(Method::PUT, "/api/plugins/builtin.files/enabled")
            .header("Idempotency-Key", "strict")
            .header("content-type", "application/json")
            .body(raw)
            .send()
            .await
            .unwrap();
        error(&body(r, 400).await, "request_failed");
        assert_eq!(h.snapshot(), before);
    }
    for mode in 0..4 {
        let before = h.snapshot();
        let mut r = h
            .request(Method::PUT, "/api/plugins/builtin.files/enabled")
            .json(&json!({"enabled":false,"expected_revision":0}));
        match mode {
            0 => {}
            1 => {
                r = r
                    .header("Idempotency-Key", "a")
                    .header("Idempotency-Key", "b");
            }
            2 => {
                r = r.header("Idempotency-Key", "a,b");
            }
            _ => {
                r = r.header("Idempotency-Key", "has space");
            }
        }
        error(&body(r.send().await.unwrap(), 400).await, "plugin_invalid");
        assert_eq!(h.snapshot(), before);
    }
    for (content_type, raw, expected) in [
        ("text/plain", "{}".to_owned(), 415),
        ("application/json", " ".repeat(1_048_577), 413),
    ] {
        let before = h.snapshot();
        let r = h
            .request(Method::PUT, "/api/plugins/builtin.files/enabled")
            .header("Idempotency-Key", "size")
            .header("content-type", content_type)
            .body(raw)
            .send()
            .await
            .unwrap();
        error(&body(r, expected).await, "request_failed");
        assert_eq!(h.snapshot(), before);
    }
    let before = h.snapshot();
    error(
        &body(
            h.request(Method::GET, "/api/plugins/unknown")
                .send()
                .await
                .unwrap(),
            404,
        )
        .await,
        "plugin_not_found",
    );
    assert_eq!(h.snapshot(), before);
    let before = h.snapshot();
    error(
        &body(h.set(false, i64::MAX as u64 + 1, "range").await, 400).await,
        "plugin_invalid",
    );
    assert_eq!(h.snapshot(), before);
}

#[tokio::test]
async fn f06_cas_replay_and_concurrent_conflicts() {
    let h = Host::new().await;
    let first = body(h.set(false, 0, "first").await, 200).await;
    assert_eq!(first["receipt"]["revision"], 1);
    assert_eq!(first["replayed"], false);
    assert_eq!(first["current"]["converged"], true);
    assert!(first.get("ok").is_none());
    let second = body(h.set(true, 1, "second").await, 200).await;
    assert_eq!(second["receipt"]["revision"], 2);
    h.engine.disable_builtin_files().unwrap();
    let before = h.snapshot();
    let replay = body(h.set(false, 0, "first").await, 200).await;
    assert_eq!(replay["receipt"], first["receipt"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["current"]["desired"]["revision"], 2);
    assert_eq!(replay["current"]["runtime"]["admission_open"], false);
    assert_eq!(h.snapshot(), before);
    for (enabled, revision, key, code) in [
        (true, 0, "first", "plugin_idempotency_conflict"),
        (false, 1, "first", "plugin_idempotency_conflict"),
        (false, 0, "stale", "plugin_revision_conflict"),
    ] {
        let before = h.snapshot();
        error(&body(h.set(enabled, revision, key).await, 409).await, code);
        assert_eq!(h.snapshot(), before);
    }
    let explicit = body(h.set(true, 2, "explicit").await, 200).await;
    assert_eq!(explicit["current"]["runtime"]["admission_open"], true);
    assert_eq!(explicit["receipt"]["revision"], 3);
    let (a, b) = tokio::join!(h.set(false, 3, "race-a"), h.set(false, 3, "race-b"));
    let statuses = [a.status().as_u16(), b.status().as_u16()];
    assert_eq!(statuses.iter().filter(|&&s| s == 200).count(), 1);
    assert_eq!(statuses.iter().filter(|&&s| s == 409).count(), 1);
    assert_eq!(h.status().await["desired"]["revision"], 4);
}

#[tokio::test]
async fn f07_database_rejection_does_not_change_runtime_or_consume_key() {
    let h = Host::new().await;
    let runtime = h.status().await["runtime"].clone();
    let conn = rusqlite::Connection::open(&h.db).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_plugin BEFORE UPDATE ON plugin_desired BEGIN SELECT RAISE(ABORT,'private-database-detail'); END;").unwrap();
    let before = h.snapshot();
    let e = body(h.set(false, 0, "db-fault").await, 500).await;
    error(&e, "plugin_internal");
    assert!(!e.to_string().contains("private-database-detail"));
    assert_eq!(h.snapshot(), before);
    assert_eq!(h.status().await["runtime"], runtime);
    conn.execute_batch("DROP TRIGGER reject_plugin").unwrap();
    let success = body(h.set(false, 0, "db-fault").await, 200).await;
    assert_eq!(success["replayed"], false);
    assert_eq!(success["receipt"]["revision"], 1);
}
async fn launch(root: &Path, port: u16) -> tokio::process::Child {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
        .arg("--port")
        .arg(port.to_string())
        .arg("--data-dir")
        .arg(root.join("data"))
        .arg("--legacy-data")
        .arg(root.join("absent-legacy"))
        .arg("--workspace")
        .arg(root.join("workspace"))
        .env("NHC_PLUGIN_TEST_KEY", "plugin-provider-key")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "Host exited before ready"
            );
            if client()
                .get(format!("http://127.0.0.1:{port}/api/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
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
async fn token(origin: &str) -> String {
    body(
        client()
            .get(format!("{origin}/api/bootstrap"))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await["token"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn request(origin: &str, token: &str, method: Method, path: &str) -> reqwest::RequestBuilder {
    client()
        .request(method, format!("{origin}{path}"))
        .header("x-peachsh-token", token)
}
async fn cli(port: u16, cwd: &Path, args: &[&str]) -> std::process::Output {
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"))
            .arg("--port")
            .arg(port.to_string())
            .arg("--json")
            .args(args)
            .current_dir(cwd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}
fn cli_ok(output: std::process::Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f05_real_host_kill_wait_restart_disabled_then_explicit_enable_admits_read() {
    real_host_file_service_case("read_file").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn l06_real_host_restart_list_and_search_remain_disabled_then_enable() {
    for tool in ["list_files", "search_files"] {
        real_host_file_service_case(tool).await;
    }
}

async fn real_host_file_service_case(tool: &'static str) {
    use axum::{Json, Router, routing::post};
    use peachsh::domain::{Route, Settings};
    use std::sync::Mutex;
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("workspace/src")).unwrap();
    std::fs::create_dir(root.path().join("client")).unwrap();
    std::fs::create_dir(root.path().join("data")).unwrap();
    std::fs::write(
        root.path().join("workspace/src/a.txt"),
        "read-positive-marker",
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let args = match tool {
        "read_file" => json!({"path":"src/a.txt"}),
        "list_files" => json!({"path":"src"}),
        "search_files" => json!({"path":"src","query":"read-positive-marker"}),
        _ => unreachable!(),
    };
    let provider=Router::new().route("/v1/chat/completions",post(move |Json(value):Json<Value>|{let captured=captured.clone();let args=args.clone();async move {let index={let mut requests=captured.lock().unwrap();let index=requests.len();requests.push(value);index};let delta=if index<2 {json!({"tool_calls":[{"index":0,"id":format!("file-{index}"),"type":"function","function":{"name":tool,"arguments":args.to_string()}}]})}else{json!({"content":"done"})};let chunk=json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});([("content-type","text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n"))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    {
        let store = Store::open(&root.path().join("data/peachsh.sqlite3")).unwrap();
        store
            .save_settings(&Settings {
                workspace: root.path().join("workspace").to_string_lossy().into(),
                max_concurrency: 1,
                routes: vec![Route {
                    id: "local".into(),
                    name: "Local".into(),
                    base_url: base,
                    model: "mock".into(),
                    max_tokens: 128,
                    parallel_limit: 1,
                    key_env: Some("NHC_PLUGIN_TEST_KEY".into()),
                }],
                newapi: None,
            })
            .unwrap();
    }
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let origin = format!("http://127.0.0.1:{port}");
    let mut child = launch(root.path(), port).await;
    let auth = token(&origin).await;
    let project = body(
        request(&origin, &auth, Method::POST, "/api/projects")
            .header("Idempotency-Key", "project")
            .json(&json!({"name":"plugin-project","root_path":root.path().join("workspace")}))
            .send()
            .await
            .unwrap(),
        201,
    )
    .await;
    let id = project["id"].as_str().unwrap();
    body(request(&origin,&auth,Method::PUT,&format!("/api/projects/{id}/config")).json(&json!({"expected_revision":1,"config":{"allowed_route_ids":["local"],"default_route_id":"local","tools":true,"write_scopes":["src"],"allow_commands":false}})).send().await.unwrap(),200).await;
    body(
        request(&origin, &auth, Method::PUT, "/api/project-selection")
            .json(&json!({"project_id":id,"expected_revision":0}))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    let preview = body(
        request(&origin, &auth, Method::POST, "/api/network/preview")
            .json(&json!({"kind":"route","id":"local","project_id":id}))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    body(request(&origin,&auth,Method::POST,"/api/network/authorizations").header("Idempotency-Key","grant").json(&json!({"subject":preview["subject"],"config_revision":preview["config_revision"],"credential_revision":preview["credential_revision"],"binding_digest":preview["binding_digest"],"purposes":["completion"]})).send().await.unwrap(),201).await;
    let disabled = cli_ok(
        cli(
            port,
            &root.path().join("client"),
            &[
                "plugin",
                "set-enabled",
                "builtin.files",
                "--enabled",
                "false",
                "--expected-revision",
                "0",
                "--key",
                "disable",
            ],
        )
        .await,
    );
    assert_eq!(disabled["current"]["converged"], true);
    let pid = child.id().unwrap();
    child.kill().await.unwrap();
    let killed = child.wait().await.unwrap();
    assert!(!killed.success());
    println!("F05 killed Host pid={pid}; wait={killed}");
    let before = snapshot(&root.path().join("data/peachsh.sqlite3"));
    child = launch(root.path(), port).await;
    let auth = token(&origin).await;
    let restarted = cli_ok(
        cli(
            port,
            &root.path().join("client"),
            &["plugin", "status", "builtin.files"],
        )
        .await,
    );
    let p = &restarted["plugin"];
    assert_eq!(p["desired"], disabled["current"]["desired"]);
    assert_eq!(p["runtime"]["admission_open"], false);
    assert_eq!(p["runtime"]["effects_count"], 0);
    assert_eq!(p["runtime"]["providers"], json!([]));
    assert_eq!(p["converged"], true);
    assert_eq!(snapshot(&root.path().join("data/peachsh.sqlite3")), before);
    assert!(requests.lock().unwrap().is_empty());
    for (index, key) in [(0, "disabled-read"), (1, "enabled-read")] {
        if index == 1 {
            let enabled = cli_ok(
                cli(
                    port,
                    &root.path().join("client"),
                    &[
                        "plugin",
                        "set-enabled",
                        "builtin.files",
                        "--enabled",
                        "true",
                        "--expected-revision",
                        "1",
                        "--key",
                        "enable",
                    ],
                )
                .await,
            );
            assert_eq!(enabled["current"]["runtime"]["admission_open"], true);
            assert_eq!(enabled["current"]["converged"], true);
        }
        let run = cli_ok(
            cli(
                port,
                &root.path().join("client"),
                &[
                    "run",
                    "start",
                    "--title",
                    "plugin-read",
                    "--route",
                    "local",
                    "--message",
                    "read file",
                    "--tools",
                    "--scope",
                    "src",
                    "--key",
                    key,
                ],
            )
            .await,
        );
        let run_id = run["run_id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let run = body(
                    request(&origin, &auth, Method::GET, &format!("/api/runs/{run_id}"))
                        .send()
                        .await
                        .unwrap(),
                    200,
                )
                .await;
                let status = run["tasks"][0]["status"].as_str().unwrap();
                if index == 0 && status == "failed" {
                    assert_eq!(run["tasks"][0]["error"], "模型请求了未授权的工具");
                    assert!(
                        !run["tasks"][0]["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|m| m["role"] == "tool")
                    );
                    break;
                }
                if index == 1 && status == "completed" {
                    break;
                }
                assert!(
                    index != 0 || status != "completed",
                    "disabled tool must not execute"
                );
                assert!(!matches!(status, "failed" | "cancelled"), "{run}");
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if index == 0 {
            let db = rusqlite::Connection::open(root.path().join("data/peachsh.sqlite3")).unwrap();
            let tool_events: u64 = db.query_row(
                "SELECT count(*) FROM events WHERE task_id=?1 AND kind IN ('tool_start','tool_result')",
                [run["tasks"][0]["id"].as_str().unwrap()],
                |row| row.get(0),
            ).unwrap();
            assert_eq!(tool_events, 0);
        }
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), if index == 0 { 1 } else { 3 });
        let first = &requests[index];
        let tools = first["tools"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            tools.iter().any(|v| v["function"]["name"] == tool),
            index == 1
        );
        if index == 1 {
            let messages = requests[2]["messages"].as_array().unwrap();
            let result = messages.iter().find(|v| v["role"] == "tool").unwrap()["content"]
                .as_str()
                .unwrap();
            let result: Value = serde_json::from_str(result).unwrap();
            match tool {
                "read_file" => assert_eq!(result["content"], "read-positive-marker"),
                "list_files" => {
                    assert_eq!(result["files"], json!([{"name":"a.txt","directory":false}]))
                }
                "search_files" => {
                    assert_eq!(result["matches"].as_array().unwrap().len(), 1);
                    assert_eq!(result["matches"][0]["path"], "src/a.txt");
                    assert_eq!(result["matches"][0]["line"], 1);
                    assert_eq!(result["matches"][0]["text"], "read-positive-marker");
                }
                _ => unreachable!(),
            }
        }
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("workspace/src/a.txt")).unwrap(),
        "read-positive-marker"
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("client"))
            .unwrap()
            .count(),
        0
    );
    child.kill().await.unwrap();
    assert!(!child.wait().await.unwrap().success());
    provider.abort();
}
#[tokio::test]
async fn f09_real_committed_response_loss_manual_cli_replay_keeps_receipt() {
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::Uri,
        response::{IntoResponse, Response},
    };
    use std::sync::Mutex;
    #[derive(Clone)]
    struct Relay {
        origin: String,
        committed: Arc<Mutex<Vec<Value>>>,
    }
    async fn relay(State(state): State<Relay>, uri: Uri, raw: Bytes) -> Response {
        if uri.path() == "/api/bootstrap" {
            return axum::Json(json!({"token":"plugin-test-token"})).into_response();
        }
        assert_eq!(uri.path(), "/api/plugins/builtin.files/enabled");
        let response = client()
            .put(format!("{}{}", state.origin, uri.path()))
            .header("x-peachsh-token", "plugin-test-token")
            .header("Idempotency-Key", "lost-original")
            .header("content-type", "application/json")
            .body(raw)
            .send()
            .await
            .unwrap();
        let committed = body(response, 200).await;
        state.committed.lock().unwrap().push(committed);
        let stream = futures_util::stream::iter(vec![Err::<Bytes, _>(std::io::Error::other(
            "response lost after commit",
        ))]);
        Response::builder()
            .header("content-type", "application/json")
            .body(Body::from_stream(stream))
            .unwrap()
    }
    let h = Host::new().await;
    let committed = Arc::new(Mutex::new(vec![]));
    let state = Relay {
        origin: h.origin.clone(),
        committed: committed.clone(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new().fallback(relay).with_state(state);
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cwd = tempfile::tempdir().unwrap();
    let args = [
        "plugin",
        "set-enabled",
        "builtin.files",
        "--enabled",
        "false",
        "--expected-revision",
        "0",
        "--key",
        "lost-original",
    ];
    let output = cli(port, cwd.path(), &args).await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(failure["code"], "operation_result_unknown");
    assert_eq!(committed.lock().unwrap().len(), 1);
    let original = committed.lock().unwrap()[0].clone();
    assert_eq!(original["receipt"]["revision"], 1);
    assert_eq!(h.status().await["desired"]["enabled"], false);
    let before = h.snapshot();
    let port = reqwest::Url::parse(&h.origin).unwrap().port().unwrap();
    let replay = cli_ok(cli(port, cwd.path(), &args).await);
    assert_eq!(replay["receipt"], original["receipt"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(h.snapshot(), before);
    assert_eq!(std::fs::read_dir(cwd.path()).unwrap().count(), 0);
    task.abort();
}

#[tokio::test]
async fn s10_shell_http_metadata_isolated_global_key_and_strict_contract() {
    let h = Host::new().await;
    let before = h.snapshot();
    let shell = body(
        h.request(Method::GET, "/api/plugins/builtin.shell")
            .send()
            .await
            .unwrap(),
        200,
    )
    .await["plugin"]
        .clone();
    assert_eq!(shell["plugin_id"], "builtin.shell");
    assert_eq!(shell["version"], "1.0.0");
    assert_eq!(shell["affected_tools"], json!(["run_command"]));
    assert_eq!(shell["compat_tools"], json!(["run_wasm"]));
    assert_eq!(
        shell["affected_dependents"],
        json!(["builtin.engine-shell-tools"])
    );
    assert_eq!(shell["runtime"]["effects_count"], 1);
    assert_eq!(shell["runtime"]["subscriptions_count"], 0);
    assert_eq!(shell["runtime"]["instances"].as_array().unwrap().len(), 2);
    assert_eq!(
        shell["runtime"]["providers"],
        json!([{"provider_id":"builtin.shell","key":{"kind":"service","name":"tool.run_command","version":{"major":1,"minor":0,"patch":0}}}])
    );
    assert_eq!(h.snapshot(), before);
    let files = h.status().await;
    for path in [
        "/api/plugins/builtin.shell",
        "/api/plugins/builtin.shell/enabled",
    ] {
        let before = h.snapshot();
        let method = if path.ends_with("enabled") {
            Method::PUT
        } else {
            Method::GET
        };
        error(
            &body(
                client()
                    .request(method, format!("{}{path}", h.origin))
                    .send()
                    .await
                    .unwrap(),
                403,
            )
            .await,
            "forbidden",
        );
        assert_eq!(h.snapshot(), before);
    }
    let bad = h
        .request(Method::PUT, "/api/plugins/builtin.shell/enabled")
        .header("Idempotency-Key", "bad")
        .header("content-type", "application/json")
        .body(r#"{"enabled":false,"enabled":true,"expected_revision":0}"#)
        .send()
        .await
        .unwrap();
    error(&body(bad, 400).await, "request_failed");
    assert_eq!(h.snapshot(), before);
    let first = body(h.set(false, 0, "global-shared-key").await, 200).await;
    let before = h.snapshot();
    let conflict = h
        .request(Method::PUT, "/api/plugins/builtin.shell/enabled")
        .header("Idempotency-Key", "global-shared-key")
        .json(&json!({"enabled":false,"expected_revision":0}))
        .send()
        .await
        .unwrap();
    error(&body(conflict, 409).await, "plugin_idempotency_conflict");
    assert_eq!(h.snapshot(), before);
    let shell_now = body(
        h.request(Method::GET, "/api/plugins/builtin.shell")
            .send()
            .await
            .unwrap(),
        200,
    )
    .await["plugin"]
        .clone();
    assert_eq!(shell_now, shell);
    let off = body(
        h.request(Method::PUT, "/api/plugins/builtin.shell/enabled")
            .header("Idempotency-Key", "shell-off")
            .json(&json!({"enabled":false,"expected_revision":0}))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(off["current"]["runtime"]["effects_count"], 0);
    assert_eq!(off["receipt"]["plugin_id"], "builtin.shell");
    assert_eq!(h.status().await, first["current"]);
    assert_eq!(files["desired"]["revision"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s09_shell_real_host_restart_disabled_then_approval_ack_command_and_files_positive() {
    use axum::{Json, Router, routing::post};
    use peachsh::domain::{Route, Settings};
    use std::sync::Mutex;
    let root = tempfile::tempdir().unwrap();
    for dir in ["workspace/src", "data", "client"] {
        std::fs::create_dir_all(root.path().join(dir)).unwrap();
    }
    std::fs::write(
        root.path().join("workspace/src/a.txt"),
        "files-stay-available",
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let provider=Router::new().route("/v1/chat/completions",post(move|Json(value):Json<Value>|{let captured=captured.clone();async move{let index={let mut r=captured.lock().unwrap();let n=r.len();r.push(value);n};let delta=match index {0|3=>json!({"tool_calls":[{"index":0,"id":format!("shell-{index}"),"type":"function","function":{"name":"run_command","arguments":json!({"command":"Set-Content -LiteralPath 'shell-marker.txt' -Value 'shell-once' -NoNewline; Write-Output 'shell-output'"}).to_string()}}]}),1=>json!({"tool_calls":[{"index":0,"id":"files-read","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/a.txt\"}"}},{"index":1,"id":"files-list","type":"function","function":{"name":"list_files","arguments":"{\"path\":\"src\"}"}},{"index":2,"id":"files-search","type":"function","function":{"name":"search_files","arguments":"{\"path\":\"src\",\"query\":\"files-stay-available\"}"}},{"index":3,"id":"files-write","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"src/written.txt\",\"content\":\"files-write-ok\"}"}}]}),2|4=>json!({"content":"done"}),_=>panic!("unexpected provider continuation")};let chunk=json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});([("content-type","text/event-stream")],format!("data: {chunk}\n\ndata: [DONE]\n\n"))}}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let provider = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    {
        let store = Store::open(&root.path().join("data/peachsh.sqlite3")).unwrap();
        store
            .save_settings(&Settings {
                workspace: root.path().join("workspace").to_string_lossy().into(),
                max_concurrency: 1,
                routes: vec![Route {
                    id: "local".into(),
                    name: "Local".into(),
                    base_url: base,
                    model: "mock".into(),
                    max_tokens: 128,
                    parallel_limit: 1,
                    key_env: Some("NHC_PLUGIN_TEST_KEY".into()),
                }],
                newapi: None,
            })
            .unwrap();
    }
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    let origin = format!("http://127.0.0.1:{port}");
    let mut child = launch(root.path(), port).await;
    let auth = token(&origin).await;
    let cwd = root.path().join("client");
    let project = body(
        request(&origin, &auth, Method::POST, "/api/projects")
            .header("Idempotency-Key", "project")
            .json(&json!({"name":"shell-project","root_path":root.path().join("workspace")}))
            .send()
            .await
            .unwrap(),
        201,
    )
    .await;
    let id = project["id"].as_str().unwrap();
    body(request(&origin,&auth,Method::PUT,&format!("/api/projects/{id}/config")).json(&json!({"expected_revision":1,"config":{"allowed_route_ids":["local"],"default_route_id":"local","tools":true,"write_scopes":["src"],"allow_commands":true}})).send().await.unwrap(),200).await;
    body(
        request(&origin, &auth, Method::PUT, "/api/project-selection")
            .json(&json!({"project_id":id,"expected_revision":0}))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    let preview = body(
        request(&origin, &auth, Method::POST, "/api/network/preview")
            .json(&json!({"kind":"route","id":"local","project_id":id}))
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    body(request(&origin,&auth,Method::POST,"/api/network/authorizations").header("Idempotency-Key","grant").json(&json!({"subject":preview["subject"],"config_revision":preview["config_revision"],"credential_revision":preview["credential_revision"],"binding_digest":preview["binding_digest"],"purposes":["completion"]})).send().await.unwrap(),201).await;
    let files = cli_ok(cli(port, &cwd, &["plugin", "status", "builtin.files"]).await);
    let off = cli_ok(
        cli(
            port,
            &cwd,
            &[
                "plugin",
                "set-enabled",
                "builtin.shell",
                "--enabled",
                "false",
                "--expected-revision",
                "0",
                "--key",
                "shell-off",
            ],
        )
        .await,
    );
    assert_eq!(off["current"]["converged"], true);
    let pid = child.id().unwrap();
    child.kill().await.unwrap();
    let exit = child.wait().await.unwrap();
    assert!(!exit.success());
    println!("S09 shell Host kill pid={pid}; wait={exit}");
    let before = snapshot(&root.path().join("data/peachsh.sqlite3"));
    child = launch(root.path(), port).await;
    let auth = token(&origin).await;
    let restarted = cli_ok(cli(port, &cwd, &["plugin", "status", "builtin.shell"]).await);
    assert_eq!(restarted["plugin"]["desired"], off["current"]["desired"]);
    assert_eq!(restarted["plugin"]["runtime"]["admission_open"], false);
    assert_eq!(restarted["plugin"]["runtime"]["effects_count"], 0);
    assert_eq!(restarted["plugin"]["runtime"]["in_flight"], 0);
    assert_eq!(snapshot(&root.path().join("data/peachsh.sqlite3")), before);
    assert!(requests.lock().unwrap().is_empty());
    assert_eq!(
        cli_ok(cli(port, &cwd, &["plugin", "status", "builtin.files"]).await),
        files
    );
    for stage in 0..3 {
        if stage == 2 {
            let enabled = cli_ok(
                cli(
                    port,
                    &cwd,
                    &[
                        "plugin",
                        "set-enabled",
                        "builtin.shell",
                        "--enabled",
                        "true",
                        "--expected-revision",
                        "1",
                        "--key",
                        "shell-on",
                    ],
                )
                .await,
            );
            assert_eq!(enabled["current"]["runtime"]["admission_open"], true);
        }
        let key = format!("shell-stage-{stage}");
        let run = cli_ok(
            cli(
                port,
                &cwd,
                &[
                    "run",
                    "start",
                    "--title",
                    "shell-test",
                    "--route",
                    "local",
                    "--message",
                    "perform tool",
                    "--tools",
                    "--scope",
                    "src",
                    "--allow-commands",
                    "--key",
                    &key,
                ],
            )
            .await,
        );
        let run_id = run["run_id"].as_str().unwrap();
        let task = run["tasks"][0]["id"].as_str().unwrap();
        if stage > 0 {
            let pending = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let v = body(
                        request(
                            &origin,
                            &auth,
                            Method::GET,
                            &format!("/api/tasks/{task}/approvals"),
                        )
                        .send()
                        .await
                        .unwrap(),
                        200,
                    )
                    .await;
                    if let Some(a) = v["approvals"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|a| a["status"] == "pending")
                    {
                        break a.clone();
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                pending["tool_name"],
                if stage == 1 {
                    "write_file"
                } else {
                    "run_command"
                }
            );
            assert!(!root.path().join("workspace/shell-marker.txt").exists());
            let approval = pending["id"].as_str().unwrap();
            if stage == 1 {
                cli_ok(cli(port, &cwd, &["approval", "approve", approval]).await);
            } else {
                let no_ack = cli(port, &cwd, &["approval", "approve", approval]).await;
                assert_eq!(no_ack.status.code(), Some(1));
                assert!(!root.path().join("workspace/shell-marker.txt").exists());
                cli_ok(
                    cli(
                        port,
                        &cwd,
                        &[
                            "approval",
                            "approve",
                            approval,
                            "--acknowledge-command-network",
                        ],
                    )
                    .await,
                );
            }
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let v = body(
                    request(&origin, &auth, Method::GET, &format!("/api/runs/{run_id}"))
                        .send()
                        .await
                        .unwrap(),
                    200,
                )
                .await;
                let status = v["tasks"][0]["status"].as_str().unwrap();
                if stage == 0 && status == "failed" {
                    assert_eq!(v["tasks"][0]["error"], "模型请求了未授权的工具");
                    break;
                }
                if stage > 0 && status == "completed" {
                    break;
                }
                assert!(
                    !matches!(status, "failed" | "cancelled" | "completed"),
                    "{v}"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if stage == 0 {
            assert!(!root.path().join("workspace/shell-marker.txt").exists());
            let approvals = body(
                request(
                    &origin,
                    &auth,
                    Method::GET,
                    &format!("/api/tasks/{task}/approvals"),
                )
                .send()
                .await
                .unwrap(),
                200,
            )
            .await;
            assert_eq!(approvals["approvals"], json!([]));
            let db = rusqlite::Connection::open(root.path().join("data/peachsh.sqlite3")).unwrap();
            let count:u64=db.query_row("SELECT count(*) FROM events WHERE task_id=?1 AND kind IN ('tool_start','tool_result')",[task],|r|r.get(0)).unwrap();
            assert_eq!(count, 0);
            assert_eq!(requests.lock().unwrap().len(), 1);
        }
        if stage == 1 {
            let r = requests.lock().unwrap();
            assert_eq!(r.len(), 3);
            let messages = r[2]["messages"].as_array().unwrap();
            for (id, field, expected) in [
                ("files-read", "content", json!("files-stay-available")),
                (
                    "files-list",
                    "files",
                    json!([{"name":"a.txt","directory":false}]),
                ),
                (
                    "files-search",
                    "matches",
                    json!([{"path":"src/a.txt","line":1,"text":"files-stay-available"}]),
                ),
                ("files-write", "written", json!("src/written.txt")),
            ] {
                let message = messages
                    .iter()
                    .find(|m| m["role"] == "tool" && m["tool_call_id"] == id)
                    .unwrap();
                let result: Value =
                    serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
                assert_eq!(result[field], expected);
            }
            assert_eq!(
                std::fs::read_to_string(root.path().join("workspace/src/written.txt")).unwrap(),
                "files-write-ok"
            );
            assert!(r[2]["messages"].as_array().unwrap().iter().any(|m| {
                m["role"] == "tool"
                    && m["content"]
                        .as_str()
                        .unwrap()
                        .contains("files-stay-available")
            }));
        }
        if stage == 2 {
            assert_eq!(
                std::fs::read_to_string(root.path().join("workspace/shell-marker.txt")).unwrap(),
                "shell-once"
            );
            {
                let r = requests.lock().unwrap();
                assert_eq!(r.len(), 5);
                let output = r[4]["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|m| m["role"] == "tool")
                    .unwrap()["content"]
                    .as_str()
                    .unwrap();
                let output: Value = serde_json::from_str(output).unwrap();
                assert_eq!(output["exit_code"], 0);
                assert!(output["stdout"].as_str().unwrap().contains("shell-output"));
            }
            let approvals = body(
                request(
                    &origin,
                    &auth,
                    Method::GET,
                    &format!("/api/tasks/{task}/approvals"),
                )
                .send()
                .await
                .unwrap(),
                200,
            )
            .await;
            assert_eq!(approvals["approvals"].as_array().unwrap().len(), 1);
            assert_eq!(approvals["approvals"][0]["status"], "approved");
            assert_eq!(approvals["approvals"][0]["execution_state"], "finished");
            assert_eq!(
                approvals["approvals"][0]["command_network_ack"],
                "acknowledged"
            );
        }
        assert_eq!(
            cli_ok(cli(port, &cwd, &["plugin", "status", "builtin.files"]).await),
            files
        );
    }
    child.kill().await.unwrap();
    assert!(!child.wait().await.unwrap().success());
    provider.abort();
    assert_eq!(std::fs::read_dir(cwd).unwrap().count(), 0);
}
