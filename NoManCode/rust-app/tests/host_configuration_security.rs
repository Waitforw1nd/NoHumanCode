//! D I04/I08: real Host + CLI and bounded hostile-response intermediaries.
//! Fixtures contain only synthetic credentials and remain under TMP for review.
#![cfg(windows)]

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::{self, Next},
    routing::{get, post},
};
use peachsh::{
    budget::*,
    domain::*,
    engine::Engine,
    host_configuration::*,
    network_authorization::*,
    project_config::*,
    secrets,
    server::{self, App},
    store::{Store, fresh_settings},
};
use reqwest::Client;
use rusqlite::{Connection, OpenFlags, types::Value as SqlValue};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const BIN: &str = env!("CARGO_BIN_EXE_peachsh");
const ENV_CHILD: &str = "NHC_D_SECURITY_ENV_CHILD";
const ENV_KEY: &str = "NHC_D_SECURITY_SYNTHETIC_KEY";
const ROUTE: &str = "route";

fn synthetic() -> String {
    format!("synthetic-{}", uuid::Uuid::new_v4())
}
fn client() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(12))
        .build()
        .unwrap()
}
fn fixture(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("nhc-security-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    eprintln!("D security retained fixture: {}", root.display());
    root
}
fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && bytes.windows(needle.len()).any(|part| part == needle)
}
fn secret_free(bytes: &[u8], forbidden: &[String]) {
    for value in forbidden {
        assert!(!contains(bytes, value.as_bytes()), "secret/token leaked");
    }
}
fn record_output(root: &Path, label: &str, output: &Output, forbidden: &[String]) {
    // Preserve actual captured output even on a failed assertion; synthetic only.
    fs::write(root.join(format!("{label}.stdout.log")), &output.stdout).unwrap();
    fs::write(root.join(format!("{label}.stderr.log")), &output.stderr).unwrap();
    secret_free(&output.stdout, forbidden);
    secret_free(&output.stderr, forbidden);
}
type Rows = BTreeMap<String, Vec<Vec<SqlValue>>>;
fn rows(root: &Path) -> Rows {
    let mut db =
        Connection::open_with_flags(root.join("host.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let tx = db.transaction().unwrap();
    let names = tx
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let mut result = BTreeMap::new();
    for name in names {
        let escaped = name.replace('"', "\"\"");
        let mut stmt = tx
            .prepare(&format!("SELECT rowid,* FROM \"{escaped}\" ORDER BY rowid"))
            .unwrap();
        let data = stmt
            .query_map([], |r| {
                (0..r.as_ref().column_count()).map(|i| r.get(i)).collect()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<Vec<SqlValue>>>>()
            .unwrap();
        result.insert(name, data);
    }
    tx.commit().unwrap();
    result
}
fn scan_persisted(root: &Path, forbidden: &[String]) {
    // Host has stopped and all external SQLite handles are closed here. Capture
    // actual DB/WAL/SHM before the scanner opens its own read-only connection.
    let artifacts: Vec<Vec<u8>> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| fs::read(p).unwrap())
        .collect();
    let mut needles: Vec<Vec<u8>> = forbidden.iter().map(|s| s.as_bytes().to_vec()).collect();
    for s in forbidden {
        let hash = Sha256::digest(s.as_bytes());
        needles.push(hash.to_vec());
        needles.push(format!("{hash:x}").into_bytes());
        needles.push(format!("{hash:X}").into_bytes());
    }
    let db = Connection::open_with_flags(root.join("host.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let mut stmt = db
        .prepare("SELECT request_fingerprint_blob FROM host_config_requests")
        .unwrap();
    for blob in stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)).unwrap() {
        let digest = secrets::open(&blob.unwrap()).expect("DPAPI request fingerprint");
        assert_eq!(digest.len(), 32);
        let hex: String = digest.iter().map(|v| format!("{v:02x}")).collect();
        needles.push(hex.to_uppercase().into_bytes());
        needles.push(hex.into_bytes());
        needles.push(digest);
    }
    for bytes in artifacts {
        for needle in &needles {
            assert!(
                !contains(&bytes, needle),
                "plaintext/digest in database or captured log"
            );
        }
    }
}

struct Host {
    origin: String,
    token: String,
    store: Arc<Store>,
    engine: Arc<Engine>,
    puts: Arc<AtomicUsize>,
    stopping: CancellationToken,
    task: JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.stopping.cancel();
        self.task.abort();
    }
}
impl Host {
    async fn start(root: &Path) -> Self {
        let store = Arc::new(Store::open(&root.join("host.db")).unwrap());
        let settings = fresh_settings(&store, &root.join("workspace")).unwrap();
        store.recover().unwrap();
        let engine = Engine::new(store.clone(), settings.max_concurrency).unwrap();
        let token = synthetic();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let puts = Arc::new(AtomicUsize::new(0));
        let counted = puts.clone();
        let app = server::router(App {
            engine: engine.clone(),
            token: token.clone(),
            origin: origin.clone(),
        })
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            let counted = counted.clone();
            async move {
                if request.method() == Method::PUT {
                    counted.fetch_add(1, Ordering::SeqCst);
                }
                next.run(request).await
            }
        }));
        let stopping = CancellationToken::new();
        let stopped = stopping.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
                .unwrap();
        });
        let boot: Value = client()
            .get(format!("{origin}/api/bootstrap"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(boot["token"] == token, "real Host bootstrap");
        Self {
            origin,
            token,
            store,
            engine,
            puts,
            stopping,
            task,
        }
    }
    async fn stop(mut self) {
        // Aborting only the accept task can leave connection tasks owning the Router.
        self.stopping.cancel();
        tokio::time::timeout(Duration::from_secs(10), &mut self.task)
            .await
            .expect("Host connections did not finish")
            .expect("Host server failed");
        let report = self.engine.shutdown(Duration::from_secs(5)).await;
        assert!(report.settled(), "{report:?}");
        let engine = Arc::downgrade(&self.engine);
        let store = Arc::downgrade(&self.store);
        drop(self);
        assert!(engine.upgrade().is_none(), "Host still owns the Engine");
        assert!(store.upgrade().is_none(), "Host still owns the Store");
    }
    fn revision(&self) -> u64 {
        self.store.host_config().unwrap().revision
    }
    async fn put(
        &self,
        id: &str,
        key: &str,
        payload: &Value,
        forbidden: &[String],
    ) -> (StatusCode, Value) {
        let response = client()
            .put(format!("{}/api/host/routes/{id}", self.origin))
            .header("x-peachsh-token", &self.token)
            .header("idempotency-key", key)
            .json(payload)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        secret_free(&bytes, forbidden);
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}
fn route(base: &str) -> Value {
    json!({"name":"Local fixture","base_url":base,"model":"mock","max_tokens":128,"parallel_limit":2})
}
fn request(revision: u64, base: &str, secret: Option<&str>) -> Value {
    json!({"expected_revision":revision,"route":route(base),"credential":match secret {
        Some(value)=>json!({"mode":"replace","value":value}),None=>json!({"mode":"keep"})}})
}
fn set_args(revision: u64, base: &str, key: &str, mode: &str) -> Vec<String> {
    [
        "host",
        "route",
        "set",
        ROUTE,
        "--expected-revision",
        &revision.to_string(),
        "--name",
        "Local fixture",
        "--base-url",
        base,
        "--model",
        "mock",
        "--max-tokens",
        "128",
        "--parallel-limit",
        "2",
        "--key",
        key,
        mode,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
async fn cli(
    origin: &str,
    args: &[String],
    input: Option<&[u8]>,
    json_mode: bool,
    close_stdout: bool,
) -> Output {
    let port = reqwest::Url::parse(origin).unwrap().port().unwrap();
    let mut command = tokio::process::Command::new(BIN);
    // Successful Prompt has separate private-console coverage. These CLI
    // children must never attach to a real user's console or read real keys.
    command
        .as_std_mut()
        .creation_flags(if args.iter().any(|arg| arg == "--credential-prompt") {
            0x0000_0008 // DETACHED_PROCESS: no console, unlike a hidden console.
        } else {
            0x0800_0000
        });
    command.arg("--port").arg(port.to_string());
    if json_mode {
        command.arg("--json");
    }
    command
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "");
    let mut child = command.spawn().unwrap();
    if close_stdout {
        drop(child.stdout.take());
    }
    if let Some(input) = input {
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(input).await;
        drop(stdin);
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take().unwrap();
    let read_stdout = tokio::spawn(async move {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stdout {
            stream.read_to_end(&mut bytes).await.unwrap();
        }
        bytes
    });
    let read_stderr = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let mut stream = stderr;
        stream.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let status = match tokio::time::timeout(Duration::from_secs(20), child.wait()).await {
        Ok(status) => status.unwrap(),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let _ = read_stdout.await;
            let _ = read_stderr.await;
            panic!("CLI timed out; child explicitly killed and reaped");
        }
    };
    Output {
        status,
        stdout: read_stdout.await.unwrap(),
        stderr: read_stderr.await.unwrap(),
    }
}
fn unknown(output: &Output, json_mode: bool) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    if json_mode {
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["code"], "operation_result_unknown");
        assert_eq!(error["retryable"], false);
    } else {
        assert!(String::from_utf8_lossy(&output.stderr).contains("operation_result_unknown"));
    }
}

#[derive(Clone)]
struct ProbeState {
    calls: Arc<AtomicUsize>,
    models: Arc<AtomicUsize>,
    completions: Arc<AtomicUsize>,
    hold: Arc<AtomicBool>,
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}
struct Probe {
    origin: String,
    state: ProbeState,
    task: JoinHandle<()>,
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Probe {
    async fn start() -> Self {
        let state = ProbeState {
            calls: Arc::new(AtomicUsize::new(0)),
            models: Arc::new(AtomicUsize::new(0)),
            completions: Arc::new(AtomicUsize::new(0)),
            hold: Arc::new(AtomicBool::new(false)),
            arrived: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        async fn complete(State(s): State<ProbeState>) -> String {
            s.calls.fetch_add(1, Ordering::SeqCst);
            s.completions.fetch_add(1, Ordering::SeqCst);
            s.arrived.notify_one();
            if s.hold.load(Ordering::SeqCst) {
                s.release.notified().await;
            }
            let chunk = json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}});
            format!("data: {chunk}\n\ndata: [DONE]\n\n")
        }
        let app = Router::new()
            .route("/health", get(|| async { StatusCode::NO_CONTENT }))
            .route("/chat/completions", post(complete))
            .route(
                "/models",
                get(|State(s): State<ProbeState>| async move {
                    s.calls.fetch_add(1, Ordering::SeqCst);
                    s.models.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"data":[{"id":"mock"}]}))
                }),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        assert_eq!(
            client()
                .get(format!("{origin}/health"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        Self {
            origin,
            state,
            task,
        }
    }
}

#[test]
fn i04_environment_namespace_dpapi_and_http_in_isolated_process() {
    let root = fixture("environment-parent");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "environment_host_child", "--nocapture"])
        .env(ENV_CHILD, &root)
        .env(ENV_KEY, synthetic())
        .stdin(Stdio::null())
        .stdout(fs::File::create(root.join("child.stdout.log")).unwrap())
        .stderr(fs::File::create(root.join("child.stderr.log")).unwrap())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "isolated security helper failed; retained logs"
            );
            break;
        }
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("security helper timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Auxiliary child entry, not an independent substantive test.
#[test]
fn environment_host_child() {
    let Some(parent_root) = std::env::var_os(ENV_CHILD) else {
        return;
    };
    let root = PathBuf::from(parent_root).join("host");
    fs::create_dir(&root).unwrap();
    let environment = std::env::var(ENV_KEY).expect("only the synthetic child variable");
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let probe = Probe::start().await;
            let host = Host::start(&root).await;
            let legacy = synthetic();
            let replacement = synthetic();
            let account = synthetic();
            let forbidden = vec![
                environment.clone(),
                legacy.clone(),
                replacement.clone(),
                account.clone(),
                host.token.clone(),
            ];
            let mut settings = host.store.settings().unwrap().unwrap();
            settings.routes.push(Route {
                id: ROUTE.into(),
                name: "Local fixture".into(),
                base_url: probe.origin.clone(),
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 2,
                key_env: Some(ENV_KEY.into()),
            });
            host.store.save_settings(&settings).unwrap();
            host.store.put_secret(ROUTE, &legacy).unwrap();
            host.store.put_secret("newapi-account", &account).unwrap();
            let before = rows(&root);
            assert_eq!(
                host.store.host_config().unwrap().routes[0].credential_source,
                CredentialSource::Environment
            );
            assert!(
                host.engine
                    .key(&host.store.settings().unwrap().unwrap().routes[0])
                    .unwrap()
                    == environment
            );
            let keep = cli(
                &host.origin,
                &set_args(
                    host.revision(),
                    &probe.origin,
                    "keep-env",
                    "--keep-credential",
                ),
                None,
                false,
                false,
            )
            .await;
            record_output(&root, "keep-env", &keep, &forbidden);
            assert!(keep.status.success());
            assert!(keep.stderr.is_empty());
            assert_eq!(
                host.store.host_config().unwrap().routes[0]
                    .key_env
                    .as_deref(),
                Some(ENV_KEY)
            );
            let output = cli(
                &host.origin,
                &set_args(
                    host.revision(),
                    &probe.origin,
                    "replace-env",
                    "--credential-stdin",
                ),
                Some(replacement.as_bytes()),
                true,
                false,
            )
            .await;
            record_output(&root, "replace-env", &output, &forbidden);
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            let snapshot = host.store.host_config().unwrap();
            assert!(snapshot.routes[0].key_env.is_none());
            assert_eq!(
                snapshot.routes[0].credential_source,
                CredentialSource::Stored
            );
            assert!(
                std::env::var(ENV_KEY).unwrap() == environment,
                "environment still exists after replace"
            );
            assert!(
                host.engine
                    .key(&host.store.settings().unwrap().unwrap().routes[0])
                    .unwrap()
                    == replacement
            );
            let db = Connection::open(root.join("host.db")).unwrap();
            let sealed: Vec<u8> = db
                .query_row(
                    "SELECT value FROM secrets WHERE id='route::route'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let mut opened = secrets::open(&sealed).unwrap();
            assert!(opened == replacement.as_bytes());
            opened.fill(0);
            let after = rows(&root);
            for previous in &before["secrets"] {
                assert!(
                    after["secrets"].contains(previous),
                    "legacy/account ciphertext and rowid preserved"
                );
            }
            let before_invalid = rows(&root);
            for (id, body, expected) in [
                (
                    "fresh",
                    request(host.revision(), &probe.origin, None),
                    "host_config_credential_required",
                ),
                (
                    ROUTE,
                    request(host.revision(), "https://different.invalid/v1", None),
                    "host_config_credential_required",
                ),
                (
                    "newapi-account",
                    request(host.revision(), &probe.origin, Some(&replacement)),
                    "host_config_invalid",
                ),
            ] {
                let (status, error) = host.put(id, "invalid-case", &body, &forbidden).await;
                assert!(!status.is_success());
                assert_eq!(error["error"]["code"], expected);
                assert!(rows(&root) == before_invalid);
            }
            db.execute(
                "UPDATE secrets SET value=X'000102' WHERE id='route::route'",
                [],
            )
            .unwrap();
            let damaged = rows(&root);
            assert_eq!(
                host.store.host_config().unwrap().routes[0].credential_source,
                CredentialSource::Unavailable
            );
            for credential in [None, Some(replacement.as_str())] {
                let (status, error) = host
                    .put(
                        ROUTE,
                        "damaged",
                        &request(host.revision(), &probe.origin, credential),
                        &forbidden,
                    )
                    .await;
                assert!(!status.is_success());
                assert_eq!(error["error"]["code"], "host_config_unavailable");
                assert!(rows(&root) == damaged);
            }
            db.execute(
                "UPDATE secrets SET value=?1 WHERE id='route::route'",
                [&sealed],
            )
            .unwrap();
            assert_eq!(probe.state.calls.load(Ordering::SeqCst), 0);
            drop(db);
            host.stop().await;
            scan_persisted(&root, &forbidden);
        });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i04_rejected_pipe_inputs_make_no_put_or_db_change() {
    let root = fixture("invalid-input");
    let host = Host::start(&root).await;
    let probe = Probe::start().await;
    let before = rows(&root);
    let marker = synthetic();
    let forbidden = vec![host.token.clone(), marker.clone()];
    let invalid = vec![
        vec![],
        b"\n".to_vec(),
        format!(" {marker}").into_bytes(),
        format!("{marker} ").into_bytes(),
        format!("{marker}\nsecond\n").into_bytes(),
        format!("{marker}\r\nsecond").into_bytes(),
        format!("{marker}\tvalue").into_bytes(),
        vec![3],
        vec![0],
        vec![0xff, 0xfe],
        vec![b'x'; 4096],
    ];
    for (case, input) in invalid.iter().enumerate() {
        for json_mode in [true, false] {
            let output = cli(
                &host.origin,
                &set_args(0, &probe.origin, "invalid-input", "--credential-stdin"),
                Some(input),
                json_mode,
                false,
            )
            .await;
            record_output(
                &root,
                &format!("invalid-{case}-{json_mode}"),
                &output,
                &forbidden,
            );
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert_eq!(host.puts.load(Ordering::SeqCst), 0);
            assert!(rows(&root) == before);
        }
    }
    assert_eq!(probe.state.calls.load(Ordering::SeqCst), 0);
    host.stop().await;
    scan_persisted(&root, &forbidden);
}

#[derive(Clone, Copy)]
enum Reply {
    Disconnect,
    Truncate,
    Oversize,
    Redirect,
    WrongReceipt,
    SecretEcho,
    TokenEcho,
    Duplicate,
    Malformed,
    SecretError,
    Valid,
    HoldThenDisconnect,
}
struct Relay {
    origin: String,
    puts: Arc<AtomicUsize>,
    committed: Arc<AtomicUsize>,
    arrived: Arc<Notify>,
    release: Arc<Notify>,
    task: JoinHandle<()>,
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn wire(status: u16, body: &[u8]) -> Vec<u8> {
    let mut out=format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}
impl Relay {
    async fn stop(mut self) {
        self.task.abort();
        if let Err(error) = (&mut self.task).await {
            assert!(error.is_cancelled(), "relay failed: {error}");
        }
    }
    async fn start(host: &Host, reply: Reply) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let upstream = host.origin.clone();
        let token = host.token.clone();
        let puts = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let arrived = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (p, c, a, r) = (
            puts.clone(),
            completed.clone(),
            arrived.clone(),
            release.clone(),
        );
        let task = tokio::spawn(async move {
            let client = client();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (end, length) = loop {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break (0, 0);
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    assert!(bytes.len() < 65536, "bounded relay headers");
                    if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&bytes[..i]).unwrap();
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                    .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        assert!(length < 65536, "bounded relay request body");
                        break (i + 4, length);
                    }
                };
                if end == 0 {
                    continue;
                }
                while bytes.len() < end + length {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0, "full PUT must arrive before induced failure");
                    bytes.extend_from_slice(&buf[..n]);
                }
                let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                let mut first = headers.lines().next().unwrap().split_whitespace();
                let method = first.next().unwrap();
                let path = first.next().unwrap();
                assert!(path.starts_with("/api/"));
                let is_put = method == "PUT";
                let header = |name: &str| {
                    headers.lines().find_map(|l| {
                        l.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
                            .map(|(_, v)| v.trim().to_owned())
                    })
                };
                if is_put {
                    assert!(header("x-peachsh-token").as_deref() == Some(token.as_str()));
                    assert!(header("idempotency-key").is_some());
                    p.fetch_add(1, Ordering::SeqCst);
                    a.notify_one();
                    if matches!(reply, Reply::HoldThenDisconnect) {
                        r.notified().await;
                    }
                }
                let body = &bytes[end..end + length];
                let mut request = client.request(
                    method.parse::<Method>().unwrap(),
                    format!("{upstream}{path}"),
                );
                for name in ["x-peachsh-token", "idempotency-key", "content-type"] {
                    if let Some(value) = header(name) {
                        request = request.header(name, value);
                    }
                }
                let response = request.body(body.to_vec()).send().await.unwrap();
                let status = response.status();
                let raw = response.bytes().await.unwrap();
                if !is_put {
                    let _ = socket.write_all(&wire(status.as_u16(), &raw)).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "upstream real Host must commit successfully"
                );
                let mut result: Value = serde_json::from_slice(&raw).unwrap();
                assert_eq!(result["receipt"]["revision"], 1);
                c.fetch_add(1, Ordering::SeqCst);
                let poisoned=match reply {
                    Reply::Disconnect|Reply::HoldThenDisconnect=>None,
                    Reply::Truncate=>Some(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{",raw.len()+100).into_bytes()),
                    Reply::Oversize=>Some(wire(200,&vec![b' ';1_048_577])),
                    Reply::Redirect=>Some(format!("HTTP/1.1 307 Fixture\r\nLocation: {upstream}{path}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()),
                    Reply::WrongReceipt=>{result["receipt"]["route_id"]=json!("wrong-route");Some(wire(200,&serde_json::to_vec(&result).unwrap()))},
                    Reply::SecretEcho=>{let payload:Value=serde_json::from_slice(body).unwrap();result["receipt"]["route"]["name"]=payload["credential"]["value"].clone();Some(wire(200,&serde_json::to_vec(&result).unwrap()))},
                    Reply::TokenEcho=>{result["receipt"]["route"]["name"]=json!(token);Some(wire(200,&serde_json::to_vec(&result).unwrap()))},
                    Reply::Duplicate=>{let original=std::str::from_utf8(&raw).unwrap();assert!(original.contains("\"replayed\":false"));Some(wire(200,original.replacen("\"replayed\":false","\"replayed\":false,\"replayed\":true",1).as_bytes()))},
                    Reply::Malformed=>Some(wire(200,b"{")),
                    Reply::SecretError=>{let payload:Value=serde_json::from_slice(body).unwrap();let secret=payload["credential"]["value"].as_str().unwrap();Some(wire(500,&serde_json::to_vec(&json!({"error":{"code":"host_config_internal","message":format!("{secret} {token}"),"retryable":false}})).unwrap()))},
                    Reply::Valid=>Some(wire(200,&raw)),
                };
                if let Some(response) = poisoned {
                    let _ = socket.write_all(&response).await;
                }
                let _ = socket.shutdown().await;
            }
        });
        Self {
            origin,
            puts,
            committed: completed,
            arrived,
            release,
            task,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i08_real_commits_then_hostile_responses_are_unknown_without_retries() {
    let cases = [
        Reply::Disconnect,
        Reply::Truncate,
        Reply::Oversize,
        Reply::Redirect,
        Reply::WrongReceipt,
        Reply::SecretEcho,
        Reply::TokenEcho,
        Reply::Duplicate,
        Reply::Malformed,
        Reply::SecretError,
        Reply::Valid,
    ];
    for (case, reply) in cases.into_iter().enumerate() {
        for json_mode in [true, false] {
            let root = fixture(&format!("hostile-{case}-{json_mode}"));
            let probe = Probe::start().await;
            let host = Host::start(&root).await;
            let secret = synthetic();
            let forbidden = vec![secret.clone(), host.token.clone()];
            let relay = Relay::start(&host, reply).await;
            let output = cli(
                &relay.origin,
                &set_args(
                    0,
                    &probe.origin,
                    "uncertain-operation",
                    "--credential-stdin",
                ),
                Some(secret.as_bytes()),
                json_mode,
                matches!(reply, Reply::Valid),
            )
            .await;
            record_output(&root, "uncertain", &output, &forbidden);
            unknown(&output, json_mode);
            assert_eq!(relay.puts.load(Ordering::SeqCst), 1);
            assert_eq!(relay.committed.load(Ordering::SeqCst), 1);
            assert_eq!(host.puts.load(Ordering::SeqCst), 1);
            assert_eq!(host.revision(), 1);
            let original = host
                .store
                .host_config_operation("uncertain-operation")
                .unwrap();
            assert_eq!(original.receipt.revision, 1);
            let lookup = cli(
                &host.origin,
                &[
                    "host",
                    "config",
                    "operation",
                    "--key",
                    "uncertain-operation",
                ]
                .map(str::to_owned),
                None,
                true,
                false,
            )
            .await;
            record_output(&root, "lookup", &lookup, &forbidden);
            assert!(lookup.status.success());
            let found: RouteConfigResult = serde_json::from_slice(&lookup.stdout).unwrap();
            assert!(found.receipt == original.receipt);
            let before = rows(&root);
            let replay = cli(
                &host.origin,
                &set_args(
                    0,
                    &probe.origin,
                    "uncertain-operation",
                    "--credential-stdin",
                ),
                Some(secret.as_bytes()),
                true,
                false,
            )
            .await;
            record_output(&root, "explicit-same-key", &replay, &forbidden);
            assert!(replay.status.success());
            let replayed: RouteConfigResult = serde_json::from_slice(&replay.stdout).unwrap();
            assert!(replayed.replayed);
            assert!(replayed.receipt == original.receipt);
            assert!(rows(&root) == before);
            assert_eq!(probe.state.calls.load(Ordering::SeqCst), 0);
            relay.stop().await;
            host.stop().await;
            scan_persisted(&root, &forbidden);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i08_lookup_404_during_received_put_does_not_resolve_unknown() {
    let root = fixture("inflight-404");
    let host = Host::start(&root).await;
    let probe = Probe::start().await;
    let secret = synthetic();
    let forbidden = vec![secret.clone(), host.token.clone()];
    let relay = Relay::start(&host, Reply::HoldThenDisconnect).await;
    let args = set_args(0, &probe.origin, "inflight-key", "--credential-stdin");
    let origin = relay.origin.clone();
    let pending =
        tokio::spawn(
            async move { cli(&origin, &args, Some(secret.as_bytes()), true, false).await },
        );
    tokio::time::timeout(Duration::from_secs(10), relay.arrived.notified())
        .await
        .unwrap();
    assert!(!pending.is_finished());
    assert_eq!(relay.puts.load(Ordering::SeqCst), 1);
    assert_eq!(host.puts.load(Ordering::SeqCst), 0);
    assert_eq!(
        host.store
            .host_config_operation("inflight-key")
            .unwrap_err(),
        HostConfigError::NotFound
    );
    let lookup = cli(
        &host.origin,
        &["host", "config", "operation", "--key", "inflight-key"].map(str::to_owned),
        None,
        true,
        false,
    )
    .await;
    record_output(&root, "lookup-during-inflight", &lookup, &forbidden);
    assert_eq!(lookup.status.code(), Some(1));
    assert!(lookup.stdout.is_empty());
    let lookup_error: Value = serde_json::from_slice(&lookup.stderr).unwrap();
    assert_eq!(lookup_error["code"], "host_config_not_found");
    assert_eq!(lookup_error["retryable"], false);
    let lookup_text = String::from_utf8_lossy(&lookup.stderr);
    assert!(lookup_text.contains("仍在途") && lookup_text.contains("不要因此换key"));
    assert_eq!(host.puts.load(Ordering::SeqCst), 0);
    assert!(!pending.is_finished());
    relay.release.notify_one();
    let output = tokio::time::timeout(Duration::from_secs(20), pending)
        .await
        .unwrap()
        .unwrap();
    record_output(&root, "inflight-put", &output, &forbidden);
    unknown(&output, true);
    assert_eq!(relay.puts.load(Ordering::SeqCst), 1);
    assert_eq!(relay.committed.load(Ordering::SeqCst), 1);
    assert_eq!(host.puts.load(Ordering::SeqCst), 1);
    assert_eq!(host.revision(), 1);
    assert!(host.store.host_config_operation("inflight-key").is_ok());
    assert_eq!(probe.state.calls.load(Ordering::SeqCst), 0);
    relay.stop().await;
    host.stop().await;
    scan_persisted(&root, &forbidden);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i04_legacy_keep_and_detached_prompt_preserve_namespace() {
    let root = fixture("legacy-and-prompt");
    let probe = Probe::start().await;
    let host = Host::start(&root).await;
    let secret = synthetic();
    let forbidden = vec![secret.clone(), host.token.clone()];
    let empty = rows(&root);
    for json_mode in [true, false] {
        let output = cli(
            &host.origin,
            &set_args(0, &probe.origin, "detached-prompt", "--credential-prompt"),
            Some(secret.as_bytes()),
            json_mode,
            false,
        )
        .await;
        record_output(
            &root,
            &format!("detached-prompt-{json_mode}"),
            &output,
            &forbidden,
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(host.puts.load(Ordering::SeqCst), 0);
        assert!(
            rows(&root) == empty,
            "Prompt must not fall back to echoed/pipe input"
        );
        if json_mode {
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["code"], "credential_unavailable");
            assert_eq!(error["retryable"], false);
        }
    }
    let mut settings = host.store.settings().unwrap().unwrap();
    settings.routes.push(Route {
        id: ROUTE.into(),
        name: "Local fixture".into(),
        base_url: probe.origin.clone(),
        model: "mock".into(),
        max_tokens: 128,
        parallel_limit: 2,
        key_env: None,
    });
    host.store.save_settings(&settings).unwrap();
    host.store.put_secret(ROUTE, &secret).unwrap();
    let revision = host.revision();
    let before = rows(&root);
    assert_eq!(
        host.store.host_config().unwrap().routes[0].credential_source,
        CredentialSource::Legacy
    );
    assert!(
        host.engine
            .key(&host.store.settings().unwrap().unwrap().routes[0])
            .unwrap()
            == secret
    );
    for json_mode in [true, false] {
        let output = cli(
            &host.origin,
            &set_args(
                revision,
                &probe.origin,
                &format!("legacy-{json_mode}"),
                "--keep-credential",
            ),
            None,
            json_mode,
            false,
        )
        .await;
        record_output(&root, &format!("legacy-{json_mode}"), &output, &forbidden);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_eq!(host.revision(), revision);
        let after = rows(&root);
        assert!(
            after["secrets"] == before["secrets"],
            "no namespace rewrite or DPAPI reseal on keep"
        );
        assert!(after["config"] == before["config"]);
        assert_eq!(
            host.store.host_config().unwrap().routes[0].credential_source,
            CredentialSource::Legacy
        );
        if json_mode {
            let result: RouteConfigResult = serde_json::from_slice(&output.stdout).unwrap();
            assert!(!result.receipt.changed);
            assert_eq!(result.receipt.revision, revision);
            assert_eq!(
                result.receipt.route.credential_source,
                CredentialSource::Legacy
            );
        }
    }
    assert_eq!(probe.state.calls.load(Ordering::SeqCst), 0);
    host.stop().await;
    scan_persisted(&root, &forbidden);
}

fn grant_project(store: &Store, root: &Path) -> (ProjectId, NetworkAuthorization) {
    fs::create_dir(root).unwrap();
    let project = store
        .register_project(RegisterProject {
            name: "Security fixture".into(),
            root_path: root.to_string_lossy().into_owned(),
            idempotency_key: id(),
        })
        .unwrap()
        .id;
    let before = store.project_config(&project).unwrap();
    store
        .configure_project(ConfigureProject {
            project_id: project.clone(),
            expected_revision: before.revision,
            config: ProjectConfig {
                allowed_route_ids: vec![ROUTE.into()],
                default_route_id: Some(ROUTE.into()),
                tools: false,
                write_scopes: vec![],
                allow_commands: false,
            },
        })
        .unwrap();
    store
        .configure_budget(ConfigureBudget {
            project_id: project.clone(),
            expected_revision: 0,
            idempotency_key: id(),
            config: BudgetConfig {
                limit_tokens: 1000,
                per_call_reservation_tokens: 100,
            },
        })
        .unwrap();
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: ROUTE.into(),
        project_id: Some(project.clone()),
    };
    let preview = store.network_preview(subject.clone()).unwrap();
    let grant = store
        .network_confirm(
            subject,
            ConfirmRequest {
                config_revision: preview.config_revision,
                credential_revision: preview.credential_revision,
                binding_digest: preview.binding_digest,
                purposes: vec![NetworkPurpose::Models, NetworkPurpose::Completion],
            },
            &id(),
        )
        .unwrap()
        .authorization;
    (project, grant)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_preserves_nonempty_unknown_budget_old_task_and_grants() {
    let root = fixture("nonempty-budget");
    let probe = Probe::start().await;
    let host = Host::start(&root).await;
    let old = synthetic();
    let rotated = synthetic();
    let mut forbidden = vec![old.clone(), rotated.clone(), host.token.clone()];
    let (status, _) = host
        .put(
            ROUTE,
            "seed",
            &request(0, &probe.origin, Some(&old)),
            &forbidden,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (p1, g1) = grant_project(&host.store, &root.join("project1"));
    let (p2, g2) = grant_project(&host.store, &root.join("project2"));
    host.store
        .select_project(SelectProject {
            project_id: p1.clone(),
            expected_revision: host.store.current_project().unwrap().revision,
        })
        .unwrap();
    let run = host
        .engine
        .start(RunRequest {
            title: "Retained task".into(),
            kind: SessionKind::Chat,
            tasks: vec![TaskSpec {
                name: "agent".into(),
                role: "developer".into(),
                route_id: ROUTE.into(),
                prompt: "reply".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: false,
                allow_commands: false,
                max_rounds: 1,
            }],
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while host.engine.is_busy() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let db = Connection::open(root.join("host.db")).unwrap();
    let old_task: String = db
        .query_row("SELECT value FROM tasks WHERE run_id=?1", [&run.id], |r| {
            r.get(0)
        })
        .unwrap();
    let task: Task = serde_json::from_str(&old_task).unwrap();
    assert_eq!(task.status, "completed");
    assert_eq!(host.store.budget_status(&p1).unwrap().spent_tokens, 5);
    // Consume the first call's notification before awaiting the new held call.
    tokio::time::timeout(Duration::from_secs(1), probe.state.arrived.notified())
        .await
        .unwrap();
    probe.state.hold.store(true, Ordering::SeqCst);
    let engine = host.engine.clone();
    let project = p1.clone();
    let pending = tokio::spawn(async move { engine.project_route_check(&project, ROUTE).await });
    tokio::time::timeout(Duration::from_secs(10), probe.state.arrived.notified())
        .await
        .unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    let before_recovery = rows(&root);
    assert!(host.store.recover().is_err());
    assert!(rows(&root) == before_recovery);
    host.stop().await;
    // A new Host recovers the original database after all old owners are gone.
    let host = Host::start(&root).await;
    forbidden.push(host.token.clone());
    probe.state.release.notify_one();
    let budget = host.store.budget_status(&p1).unwrap();
    assert_eq!(budget.unresolved_count, 1);
    assert_eq!(budget.held_tokens, 100);
    assert_eq!(budget.spent_tokens, 5);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM budget_calls WHERE state='unknown'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
    let calls = probe.state.calls.load(Ordering::SeqCst);
    assert_eq!(calls, 3);
    assert_eq!(probe.state.models.load(Ordering::SeqCst), 1);
    assert_eq!(probe.state.completions.load(Ordering::SeqCst), 2);
    let before = rows(&root);
    let mut metadata = request(host.revision(), &probe.origin, None);
    metadata["route"]["name"] = json!("Renamed fixture");
    assert_eq!(
        host.put(ROUTE, "metadata", &metadata, &forbidden).await.0,
        StatusCode::OK
    );
    assert!(
        host.store
            .network_authorization(&g1.authorization_id)
            .unwrap()
            == g1
    );
    assert!(
        host.store
            .network_authorization(&g2.authorization_id)
            .unwrap()
            == g2
    );
    assert_eq!(
        host.put(
            ROUTE,
            "rotate",
            &request(host.revision(), &probe.origin, Some(&rotated)),
            &forbidden
        )
        .await
        .0,
        StatusCode::OK
    );
    for mut grant in [g1, g2] {
        let current = host
            .store
            .network_authorization(&grant.authorization_id)
            .unwrap();
        grant.state = AuthorizationState::Stale;
        assert!(current == grant, "only grant state becomes stale");
    }
    let after = rows(&root);
    for (name, previous) in before {
        if ![
            "config",
            "secrets",
            "host_config_state",
            "host_config_requests",
            "network_subjects",
            "network_authorizations",
        ]
        .contains(&name.as_str())
        {
            assert!(
                after[&name] == previous,
                "configuration changed preserved table {name}"
            );
        }
    }
    assert!(
        db.query_row("SELECT value FROM tasks WHERE run_id=?1", [&run.id], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
            == old_task
    );
    let after_budget = host.store.budget_status(&p1).unwrap();
    assert_eq!(after_budget.unresolved_count, 1);
    assert_eq!(after_budget.held_tokens, 100);
    assert_eq!(after_budget.spent_tokens, 5);
    assert_eq!(host.store.budget_status(&p2).unwrap().spent_tokens, 0);
    assert_eq!(probe.state.calls.load(Ordering::SeqCst), calls);
    drop(db);
    host.stop().await;
    scan_persisted(&root, &forbidden);
}
