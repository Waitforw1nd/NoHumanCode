//! I01/I10. Real binary, initially absent database, all route writes through CLI.
//! Store is never used to inject settings, routes, credentials, grants or budgets.
#![cfg(windows)]
use axum::{
    Json, Router,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
};

const BIN: &str = env!("CARGO_BIN_EXE_peachsh");
const SECRET: &str = "synthetic-first-use-hidden-credential";
const LEGACY_SECRET: &str = "synthetic-legacy-import-credential";
struct Provider {
    base: String,
    completions: Arc<AtomicUsize>,
    models: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Provider {
    async fn new() -> Self {
        let completions = Arc::new(AtomicUsize::new(0));
        let models = Arc::new(AtomicUsize::new(0));
        let complete = completions.clone();
        let list = models.clone();
        let app=Router::new().route("/v1/models",get(move || { let list=list.clone(); async move { list.fetch_add(1,Ordering::SeqCst); Json(json!({"data":[{"id":"mock-model"}]})) } }))
            .route("/v1/chat/completions",post(move |headers:axum::http::HeaderMap,Json(body):Json<Value>| { let complete=complete.clone(); async move {
                assert!(headers.get("authorization").and_then(|v|v.to_str().ok())==Some(format!("Bearer {SECRET}").as_str()),"configured credential used");
                assert_eq!(body["model"],"mock-model");
                complete.fetch_add(1,Ordering::SeqCst);
                ([("content-type","text/event-stream")],format!("data: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"delta":{"content":"initialization completed"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}})))
            } }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            completions,
            models,
            task,
        }
    }
    fn count(&self, expected: usize) {
        assert_eq!(self.completions.load(Ordering::SeqCst), expected);
        assert_eq!(self.models.load(Ordering::SeqCst), 0);
    }
}
struct Host {
    root: PathBuf,
    data: PathBuf,
    port: u16,
    child: Child,
    token: String,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}
impl Host {
    async fn launch(root: &Path, data: &str, legacy: bool) -> Self {
        std::fs::create_dir_all(root.join("runner")).unwrap();
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let mut command = Command::new(BIN);
        command
            .arg("--port")
            .arg(port.to_string())
            .arg("--data-dir")
            .arg(root.join(data))
            .arg("--workspace")
            .arg(root.join("workspace"))
            .current_dir(root.join("runner"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .env(
                "PEACHSH_KEY_1",
                "synthetic-unrequested-environment-credential",
            )
            .env_remove("NHC_INIT_LEGACY_KEY");
        if legacy {
            command.arg("--legacy-data").arg(root.join("data"));
        }
        let mut child = command.spawn().unwrap();
        let http = client();
        let token = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "Host stays alive during startup"
                );
                if let Ok(response) = http
                    .get(format!("http://127.0.0.1:{port}/api/bootstrap"))
                    .send()
                    .await
                    && response.status() == 200
                {
                    let value: Value = response.json().await.unwrap();
                    break value["token"].as_str().unwrap().to_owned();
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        println!("I01/I10 real Host ready pid={}", child.id().unwrap());
        Self {
            root: root.to_owned(),
            data: root.join(data),
            port,
            child,
            token,
        }
    }
    async fn stop(&mut self) {
        let pid = self.child.id().unwrap();
        self.child.kill().await.unwrap();
        let status = self.child.wait().await.unwrap();
        assert!(!status.success());
        println!("I01/I10 kill+wait completed pid={pid}");
    }
    async fn command(&self, args: &[&str], input: Option<&[u8]>) -> Output {
        let mut child = Command::new(BIN)
            .arg("--port")
            .arg(self.port.to_string())
            .arg("--json")
            .args(args)
            .current_dir(self.root.join("runner"))
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        if let Some(input) = input {
            let mut pipe = child.stdin.take().unwrap();
            pipe.write_all(input).await.unwrap();
            drop(pipe);
        }
        let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        for bytes in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(bytes);
            assert!(
                !text.contains(SECRET)
                    && !text.contains(LEGACY_SECRET)
                    && !text.contains(&self.token),
                "CLI does not expose credentials or bootstrap token"
            );
        }
        output
    }
    async fn ok(&self, args: &[&str]) -> Value {
        self.ok_input(args, None).await
    }
    async fn ok_input(&self, args: &[&str], input: Option<&[u8]>) -> Value {
        let output = self.command(args, input).await;
        assert_eq!(
            output.status.code(),
            Some(0),
            "CLI command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.data.join("peachsh.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    fn no_permission_or_calls(&self) {
        let db = self.sql();
        for table in ["network_authorizations", "network_calls", "budget_calls"] {
            let count: i64 = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "configuration did not create {table}");
        }
    }
}
fn synthetic_legacy(root: &Path, base: &str) {
    std::fs::create_dir_all(root.join("data")).unwrap();
    let settings = json!({"llm-pi-ai":{"providers":{"legacy-route":{"displayName":"Legacy fixture","baseURL":base,"apiKeyEnv":"NHC_INIT_LEGACY_KEY","models":[{"id":"mock-model"}]}}}});
    // JSON is a YAML subset; the real legacy importer reads these exact filenames.
    std::fs::write(root.join("data/settings.yaml"), settings.to_string()).unwrap();
    std::fs::write(
        root.join("data/.credentials.yaml"),
        json!({"refs":{"NHC_INIT_LEGACY_KEY":LEGACY_SECRET}}).to_string(),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i01_safe_empty_start_explicit_legacy_and_existing_database_are_distinct() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new().await;
    synthetic_legacy(root.path(), &provider.base);
    assert!(!root.path().join("safe/peachsh.sqlite3").exists());
    let mut safe = Host::launch(root.path(), "safe", false).await;
    assert_eq!(
        safe.ok(&["host", "config", "get"]).await,
        json!({"revision":0,"routes":[]})
    );
    let settings: String = safe
        .sql()
        .query_row("SELECT value FROM config WHERE id='settings'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let settings: Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(settings["routes"], json!([]));
    assert!(settings["newapi"].is_null());
    assert_eq!(settings["max_concurrency"], 3);
    let count: i64 = safe
        .sql()
        .query_row("SELECT COUNT(*) FROM secrets", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    safe.no_permission_or_calls();
    provider.count(0);
    safe.stop().await;
    // Existing Settings suppress even an explicitly requested later import.
    let mut safe = Host::launch(root.path(), "safe", true).await;
    assert_eq!(
        safe.ok(&["host", "config", "get"]).await,
        json!({"revision":0,"routes":[]})
    );
    safe.stop().await;
    let mut legacy = Host::launch(root.path(), "legacy", true).await;
    let before = legacy.ok(&["host", "config", "get"]).await;
    assert_eq!(before["routes"].as_array().unwrap().len(), 1);
    assert_eq!(before["routes"][0]["id"], "legacy-route");
    assert_eq!(before["routes"][0]["credential_source"], "legacy");
    assert!(before["revision"].as_u64().unwrap() > 0);
    provider.count(0);
    legacy.stop().await;
    std::fs::write(root.path().join("data/settings.yaml"), "{}").unwrap();
    let mut legacy = Host::launch(root.path(), "legacy", true).await;
    assert_eq!(legacy.ok(&["host", "config", "get"]).await, before);
    provider.count(0);
    legacy.stop().await;
    let output = Command::new(BIN)
        .arg("--check")
        .arg("--data-dir")
        .arg(root.path().join("check-state"))
        .arg("--workspace")
        .arg(root.path().join("workspace"))
        .current_dir(root.path().join("runner"))
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(root.path().join("check-state/peachsh.sqlite3").exists());
    provider.count(0);
    let help = Command::new(BIN)
        .arg("--help")
        .current_dir(root.path())
        .output()
        .await
        .unwrap();
    let help = String::from_utf8(help.stdout)
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(help.contains("not a read-only check") && help.contains("--legacy-data"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i10_same_empty_database_cli_configuration_permissions_budget_completion_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new().await;
    synthetic_legacy(root.path(), &provider.base);
    assert!(!root.path().join("state/peachsh.sqlite3").exists());
    let mut host = Host::launch(root.path(), "state", false).await;
    assert_eq!(
        host.ok(&["host", "config", "get"]).await,
        json!({"revision":0,"routes":[]})
    );
    let credential = format!("{SECRET}\n");
    let first = host
        .ok_input(
            &[
                "host",
                "route",
                "set",
                "local",
                "--expected-revision",
                "0",
                "--name",
                "Local",
                "--base-url",
                &provider.base,
                "--model",
                "mock-model",
                "--max-tokens",
                "128",
                "--parallel-limit",
                "1",
                "--key",
                "first-route",
                "--credential-stdin",
            ],
            Some(credential.as_bytes()),
        )
        .await;
    assert_eq!(first["receipt"]["revision"], 1);
    assert_eq!(first["replayed"], false);
    let original = host
        .ok(&["host", "config", "operation", "--key", "first-route"])
        .await;
    assert_eq!(original["receipt"], first["receipt"]);
    assert_eq!(original["replayed"], true);
    assert_eq!(host.ok(&["project", "list"]).await, json!([]));
    host.no_permission_or_calls();
    provider.count(0);
    println!("I10 route configured by CLI; Provider=0 and authorization/call/budget-call rows=0");
    let workspace = root.path().join("workspace");
    let project = host
        .ok(&[
            "project",
            "create",
            "--name",
            "First use",
            "--root",
            workspace.to_str().unwrap(),
            "--key",
            "first-project",
        ])
        .await;
    let project_id = project["id"].as_str().unwrap();
    let initial = host.ok(&["project", "config", "get", project_id]).await;
    assert_eq!(initial["config"]["tools"], false);
    assert_eq!(initial["config"]["allow_commands"], false);
    assert_eq!(initial["config"]["write_scopes"], json!([]));
    let config = json!({"allowed_route_ids":["local"],"default_route_id":"local","tools":false,"write_scopes":[],"allow_commands":false});
    let file = root.path().join("project-config.json");
    std::fs::write(&file, config.to_string()).unwrap();
    let configured = host
        .ok(&[
            "project",
            "config",
            "set",
            project_id,
            "--expected-revision",
            &initial["revision"].to_string(),
            "--file",
            file.to_str().unwrap(),
        ])
        .await;
    assert_eq!(configured["config"], config);
    let selection = host.ok(&["project", "current"]).await;
    assert!(selection["project_id"].is_null());
    let selected = host
        .ok(&[
            "project",
            "select",
            project_id,
            "--expected-revision",
            &selection["revision"].to_string(),
        ])
        .await;
    assert_eq!(selected["project_id"], project_id);
    provider.count(0);
    let budget = host
        .ok(&["project", "budget", "config", "get", "--id", project_id])
        .await;
    assert_eq!(budget["revision"], 0);
    host.ok(&[
        "project",
        "budget",
        "config",
        "set",
        "--id",
        project_id,
        "--expected-revision",
        "0",
        "--limit-tokens",
        "100",
        "--per-call-reservation-tokens",
        "32",
        "--key",
        "first-budget",
    ])
    .await;
    host.no_permission_or_calls();
    provider.count(0);
    let preview = host
        .ok(&[
            "network",
            "preview",
            "--route",
            "local",
            "--project",
            project_id,
        ])
        .await;
    let authorization = host
        .ok(&[
            "network",
            "authorize",
            "--route",
            "local",
            "--project",
            project_id,
            "--config-revision",
            &preview["config_revision"].to_string(),
            "--credential-revision",
            &preview["credential_revision"].to_string(),
            "--binding",
            preview["binding_digest"].as_str().unwrap(),
            "--purpose",
            "completion",
            "--key",
            "first-network",
        ])
        .await;
    assert_eq!(authorization["purposes"], json!(["completion"]));
    provider.count(0);
    println!(
        "I10 explicit Project permissions/selection, budget and Completion authorization ready; Provider=0"
    );
    let run = host
        .ok(&[
            "run",
            "start",
            "--project",
            project_id,
            "--expected-config-revision",
            &configured["revision"].to_string(),
            "--title",
            "Initialization chain",
            "--route",
            "local",
            "--message",
            "reply once",
            "--key",
            "first-completion",
        ])
        .await;
    let run_id = run["run_id"].as_str().unwrap();
    let task = run["tasks"][0]["id"].as_str().unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let status = host.ok(&["run", "status", run_id]).await;
            let task = &status["tasks"][0];
            if task["status"] == "completed" {
                break status;
            }
            assert!(
                !matches!(
                    task["status"].as_str(),
                    Some("failed" | "interrupted" | "cancelled")
                ),
                "Completion remains healthy"
            );
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(finished["tasks"][0]["tools"], false);
    assert_eq!(finished["tasks"][0]["allow_commands"], false);
    provider.count(1);
    let calls = host.ok(&["task", "network-calls", task]).await;
    assert_eq!(calls["calls"].as_array().unwrap().len(), 1);
    assert_eq!(calls["calls"][0]["purpose"], "completion");
    let call_id = calls["calls"][0]["call_id"].as_str().unwrap();
    let charge = host
        .ok(&[
            "project",
            "budget",
            "call",
            "--id",
            project_id,
            "--call-id",
            call_id,
        ])
        .await;
    assert_eq!(charge["state"], "settled");
    assert_eq!(charge["usage"]["total_tokens"], 5);
    let budget_before = host
        .ok(&["project", "budget", "status", "--id", project_id])
        .await;
    assert_eq!(budget_before["spent_tokens"], 5);
    assert_eq!(budget_before["held_tokens"], 0);
    let changed = host
        .ok(&[
            "host",
            "route",
            "set",
            "local",
            "--expected-revision",
            "1",
            "--name",
            "Renamed local",
            "--base-url",
            &provider.base,
            "--model",
            "mock-model",
            "--max-tokens",
            "128",
            "--parallel-limit",
            "1",
            "--key",
            "rename-route",
            "--keep-credential",
        ])
        .await;
    assert_eq!(changed["receipt"]["revision"], 2);
    assert_eq!(
        host.ok(&["host", "config", "operation", "--key", "first-route"])
            .await,
        original
    );
    let current = host.ok(&["host", "config", "get"]).await;
    assert_eq!(current["revision"], 2);
    assert_eq!(current["routes"][0]["name"], "Renamed local");
    assert_eq!(
        host.ok(&["project", "budget", "status", "--id", project_id])
            .await,
        budget_before
    );
    provider.count(1);
    let old_token = host.token.clone();
    println!(
        "I10 one mock Completion settled; original receipt revision=1, current configuration revision=2"
    );
    host.stop().await;
    let mut restarted = Host::launch(root.path(), "state", false).await;
    assert_ne!(
        restarted.token, old_token,
        "restart issues a new bootstrap token"
    );
    assert_eq!(restarted.ok(&["host", "config", "get"]).await, current);
    assert_eq!(
        restarted
            .ok(&["host", "config", "operation", "--key", "first-route"])
            .await,
        original
    );
    assert_eq!(
        restarted
            .ok(&["project", "budget", "status", "--id", project_id])
            .await,
        budget_before
    );
    assert_eq!(restarted.ok(&["project", "current"]).await, selected);
    let grant = restarted
        .ok(&[
            "network",
            "get",
            authorization["authorization_id"].as_str().unwrap(),
        ])
        .await;
    assert_eq!(grant["state"], "active");
    assert_eq!(grant["binding_digest"], authorization["binding_digest"]);
    assert_eq!(
        restarted.ok(&["run", "status", run_id]).await["tasks"][0]["status"],
        "completed"
    );
    provider.count(1);
    restarted.stop().await;
    println!(
        "I10 restarted same database; receipt/configuration/selection/budget/grant preserved; Provider=1, no replay"
    );
}
