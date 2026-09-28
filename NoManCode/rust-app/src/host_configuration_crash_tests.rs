//! I09: real library-test Host processes, authenticated HTTP, kill + wait.
//! Register in lib.rs only as cfg(all(test, windows)); registration belongs to B.
//! Fixture roots and logs are retained under TMP, including on failure.
#![cfg(all(test, windows))]

use crate::{
    engine::Engine,
    host_configuration::{
        CredentialSource, HostConfigSnapshot, RouteConfigReceipt, RouteConfigResult,
    },
    secrets,
    server::{self, App},
    store::{Store, fresh_settings},
};
use axum::{Router, http::StatusCode, routing::get};
use reqwest::{Client, Response};
use rusqlite::{Connection, OpenFlags, OptionalExtension, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const CHILD_ROOT: &str = "NHC_D_HOST_CRASH_ROOT";
const CHILD_RUN: &str = "NHC_D_HOST_CRASH_RUN";
const ROUTE: &str = "i09-route";
const OTHER_ROUTE: &str = "i09-preserved";
const OLD_KEY: &str = "i09-seed-target";
const OTHER_KEY: &str = "i09-seed-other";
const LOST_KEY: &str = "i09-interrupted-change";
const WAIT_LIMIT: Duration = Duration::from_secs(25);

// Values, ciphertext and request digests are never included in assertion output.
type Rows = Vec<Vec<SqlValue>>;
#[derive(PartialEq)]
struct DbSnapshot(BTreeMap<&'static str, Rows>);

const TABLES: &[&str] = &[
    "config",
    "secrets",
    "host_config_state",
    "host_config_requests",
    "network_subjects",
    "network_authorizations",
    "network_confirmations",
    "network_calls",
    "network_events",
    "task_network_bindings",
    "project_budgets",
    "budget_requests",
    "budget_calls",
];

fn read_db(root: &Path) -> Connection {
    Connection::open_with_flags(root.join("host.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|_| panic!("read-only fixture database unavailable"))
}

fn db_snapshot(root: &Path) -> DbSnapshot {
    let mut db = read_db(root);
    let tx = db.transaction().expect("consistent read transaction");
    let mut tables = BTreeMap::new();
    for &table in TABLES {
        // Names are a fixed schema allowlist, never request data.
        let mut statement = tx
            .prepare(&format!("SELECT rowid,* FROM {table} ORDER BY rowid"))
            .expect("snapshot table exists");
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, SqlValue>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .expect("snapshot query")
            .collect::<rusqlite::Result<Rows>>()
            .expect("snapshot rows");
        tables.insert(table, rows);
    }
    tx.commit().expect("end read transaction");
    DbSnapshot(tables)
}

fn event(root: &Path, value: Value) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("lifecycle.jsonl"))
        .expect("lifecycle log");
    let record = json!({"unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(), "event":value});
    serde_json::to_writer(&mut file, &record).expect("public lifecycle record");
    writeln!(file).expect("lifecycle newline");
    file.flush().expect("flush lifecycle record");
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    pid: u32,
    run: String,
    origin: String,
}

struct HostProcess {
    child: Option<Child>,
    root: PathBuf,
    run: &'static str,
    pid: u32,
}

impl HostProcess {
    fn spawn(root: &Path, run: &'static str, phase: Option<&str>) -> Self {
        assert!(
            !root.join(format!("{run}.listening.json")).exists(),
            "fresh readiness marker required"
        );
        let child_test = format!(
            "{}::host_process_child",
            module_path!().split_once("::").unwrap().1
        );
        let mut command = Command::new(std::env::current_exe().expect("current test binary"));
        command
            .args(["--exact", &child_test, "--nocapture", "--test-threads=1"])
            .current_dir(root)
            .env(CHILD_ROOT, root)
            .env(CHILD_RUN, run)
            .env_remove("NHC_HOST_CONFIG_TEST_ROOT")
            .env_remove("NHC_HOST_CONFIG_TEST_PHASE")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                File::create(root.join(format!("{run}.stdout.log"))).unwrap(),
            ))
            .stderr(Stdio::from(
                File::create(root.join(format!("{run}.stderr.log"))).unwrap(),
            ))
            .creation_flags(0x08000000);
        if let Some(phase) = phase {
            assert!(matches!(phase, "before_commit" | "after_commit"));
            assert!(
                !root.join(format!("{phase}.ready")).exists(),
                "fresh phase marker required"
            );
            command
                .env("NHC_HOST_CONFIG_TEST_ROOT", root)
                .env("NHC_HOST_CONFIG_TEST_PHASE", phase);
        }
        let child = command.spawn().expect("spawn this library test binary");
        let pid = child.id();
        let process = Self {
            child: Some(child),
            root: root.to_owned(),
            run,
            pid,
        };
        event(
            root,
            json!({"kind":"spawn", "run":run,"pid":pid,"phase":phase}),
        );
        process
    }

    fn assert_running(&mut self) {
        assert!(
            self.child
                .as_mut()
                .unwrap()
                .try_wait()
                .expect("poll child")
                .is_none(),
            "Host child exited before the expected observation; retained logs identify the run"
        );
    }

    fn kill_and_wait(&mut self, reason: &str) {
        self.assert_running();
        let child = self.child.as_mut().unwrap();
        let started = Instant::now();
        child.kill().expect("kill original Host child handle");
        let status = child.wait().expect("wait for killed Host process");
        self.child.take();
        event(
            &self.root,
            json!({"kind":"kill_wait","run":self.run,"pid":self.pid,
            "reason":reason,"exit_code":status.code(),"elapsed_ms":started.elapsed().as_millis(),"reaped":true}),
        );
        assert!(
            !status.success(),
            "the selected Host must actually be terminated"
        );
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            // Also reap the original handle on assertions, timeouts or HTTP errors.
            let kill_ok = child.kill().is_ok();
            let status = child.wait();
            let record = json!({"kind":"failure_cleanup_wait","run":self.run,"pid":self.pid,
                "kill_ok":kill_ok,"wait_ok":status.is_ok(),"exit_code":status.ok().and_then(|s|s.code())});
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.root.join("lifecycle.jsonl"))
            {
                let _ = serde_json::to_writer(&mut file, &record);
                let _ = writeln!(file);
            }
        }
    }
}

/// No dedicated child environment: harmless helper, not substantive coverage.
#[test]
fn host_process_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let run = std::env::var(CHILD_RUN).expect("fixture run selector");
    assert!(matches!(
        run.as_str(),
        "seed" | "at-stop" | "restart-1" | "restart-2"
    ));
    assert_eq!(
        fs::read(root.join("fixture-owner")).expect("private fixture marker"),
        b"D-R13-I09"
    );
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("real Host runtime")
        .block_on(async {
            let store = Arc::new(Store::open(&root.join("host.db")).expect("Host database"));
            let settings = fresh_settings(&store, &root.join("workspace"))
                .expect("fresh or existing settings");
            store.recover().expect("Host recovery");
            let engine = Engine::new(store, settings.max_concurrency).expect("real Engine");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("Host listener");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let app = server::router(App {
                engine,
                token: uuid::Uuid::new_v4().to_string(),
                origin: origin.clone(),
            });
            let ready = Ready {
                pid: std::process::id(),
                run: run.clone(),
                origin,
            };
            fs::write(
                root.join(format!("{run}.listening.json")),
                serde_json::to_vec(&ready).unwrap(),
            )
            .expect("publish public listener readiness");
            // The parent additionally waits for authenticated HTTP before writing.
            axum::serve(listener, app)
                .await
                .expect("real Host HTTP runtime");
            panic!("Host child must remain running until the parent kills and waits");
        });
}

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(40))
        .build()
        .unwrap()
}

async fn bytes(mut response: Response) -> Vec<u8> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.expect("HTTP body transport") {
        assert!(body.len() + chunk.len() <= 65536, "bounded public response");
        body.extend_from_slice(&chunk);
    }
    body
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && bytes.windows(needle.len()).any(|part| part == needle)
}

fn reject_secret_bytes(body: &[u8], forbidden: &[String]) {
    for value in forbidden {
        assert!(
            !contains(body, value.as_bytes()),
            "credential or Host token leaked into output"
        );
    }
}

async fn public_json<T: serde::de::DeserializeOwned>(
    response: Response,
    forbidden: &[String],
) -> T {
    let body = bytes(response).await;
    reject_secret_bytes(&body, forbidden);
    serde_json::from_slice(&body).unwrap_or_else(|_| panic!("invalid public response shape"))
}

struct LiveHost {
    process: HostProcess,
    origin: String,
    token: String,
}

async fn start_host(
    root: &Path,
    run: &'static str,
    phase: Option<&str>,
    client: &Client,
    forbidden: &[String],
) -> LiveHost {
    let mut process = HostProcess::spawn(root, run, phase);
    let deadline = Instant::now() + WAIT_LIMIT;
    let ready: Ready = loop {
        process.assert_running();
        if let Ok(raw) = fs::read(root.join(format!("{run}.listening.json")))
            && let Ok(ready) = serde_json::from_slice::<Ready>(&raw)
        {
            break ready;
        }
        assert!(
            Instant::now() < deadline,
            "Host listener readiness timed out"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(ready.pid, process.pid);
    assert_eq!(ready.run, run);
    let origin_url = reqwest::Url::parse(&ready.origin).unwrap();
    assert_eq!(origin_url.scheme(), "http");
    assert_eq!(origin_url.host_str(), Some("127.0.0.1"));
    let response = client
        .get(format!("{}/api/bootstrap", ready.origin))
        .send()
        .await
        .expect("real bootstrap HTTP");
    assert_eq!(response.status(), StatusCode::OK);
    let bootstrap: Value = serde_json::from_slice(&bytes(response).await)
        .unwrap_or_else(|_| panic!("bootstrap shape"));
    let token = bootstrap["token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("bootstrap token")
        .to_owned();
    let host = LiveHost {
        process,
        origin: ready.origin,
        token,
    };
    let authenticated = client
        .get(format!("{}/api/host/config", host.origin))
        .header("x-peachsh-token", &host.token)
        .send()
        .await
        .expect("authenticated Host readiness");
    assert_eq!(authenticated.status(), StatusCode::OK);
    let mut all_forbidden = forbidden.to_vec();
    all_forbidden.push(host.token.clone());
    let _: HostConfigSnapshot = public_json(authenticated, &all_forbidden).await;
    event(
        root,
        json!({"kind":"authenticated_http_ready","run":run,"pid":host.process.pid,"origin":host.origin}),
    );
    host
}

async fn snapshot(client: &Client, host: &LiveHost, forbidden: &[String]) -> HostConfigSnapshot {
    let response = client
        .get(format!("{}/api/host/config", host.origin))
        .header("x-peachsh-token", &host.token)
        .send()
        .await
        .expect("Host config HTTP");
    assert_eq!(response.status(), StatusCode::OK);
    public_json(response, forbidden).await
}

fn request(revision: u64, base: &str, name: &str, secret: &str) -> Value {
    json!({"expected_revision":revision,"route":{"name":name,"base_url":base,"model":name,
        "max_tokens":128,"parallel_limit":1},"credential":{"mode":"replace","value":secret}})
}

async fn put(
    client: &Client,
    host: &LiveHost,
    id: &str,
    key: &str,
    body: &Value,
    forbidden: &[String],
) -> RouteConfigResult {
    let response = client
        .put(format!("{}/api/host/routes/{id}", host.origin))
        .header("x-peachsh-token", &host.token)
        .header("idempotency-key", key)
        .json(body)
        .send()
        .await
        .expect("Host route HTTP transport");
    assert_eq!(response.status(), StatusCode::OK);
    public_json(response, forbidden).await
}

async fn lookup(
    client: &Client,
    host: &LiveHost,
    key: &str,
    forbidden: &[String],
) -> Option<RouteConfigReceipt> {
    let response = client
        .post(format!("{}/api/host/config/lookup", host.origin))
        .header("x-peachsh-token", &host.token)
        .json(&json!({"key":key}))
        .send()
        .await
        .expect("Host operation HTTP");
    if response.status() == StatusCode::NOT_FOUND {
        let error: Value = public_json(response, forbidden).await;
        assert_eq!(error["error"]["code"], "host_config_not_found");
        assert_eq!(error["error"]["retryable"], false);
        None
    } else {
        assert_eq!(response.status(), StatusCode::OK);
        let result: RouteConfigResult = public_json(response, forbidden).await;
        assert!(result.replayed);
        Some(result.receipt)
    }
}

fn assert_secret(root: &Path, id: &str, expected: &str) {
    let blob: Vec<u8> = read_db(root)
        .query_row(
            "SELECT value FROM secrets WHERE id=?1",
            [format!("route::{id}")],
            |r| r.get(0),
        )
        .expect("stored route credential");
    let mut plaintext =
        secrets::open(&blob).unwrap_or_else(|_| panic!("DPAPI fixture credential unavailable"));
    assert!(
        plaintext.as_slice() == expected.as_bytes(),
        "wrong persisted credential after crash"
    );
    plaintext.fill(0);
}

fn database_receipt(root: &Path) -> Option<RouteConfigReceipt> {
    let raw: Option<String> = read_db(root)
        .query_row(
            "SELECT receipt FROM host_config_requests WHERE key=?1",
            [LOST_KEY],
            |r| r.get(0),
        )
        .optional()
        .expect("durable operation query");
    raw.map(|s| serde_json::from_str(&s).unwrap_or_else(|_| panic!("durable receipt shape")))
}

fn assert_no_plaintext_files(root: &Path, forbidden: &[String]) {
    let db = read_db(root);
    let mut needles: Vec<Vec<u8>> = forbidden.iter().map(|s| s.as_bytes().to_vec()).collect();
    for secret in forbidden {
        let digest = Sha256::digest(secret.as_bytes());
        needles.push(digest.to_vec());
        needles.push(format!("{digest:x}").into_bytes());
        needles.push(format!("{digest:X}").into_bytes());
    }
    let mut stmt = db
        .prepare("SELECT request_fingerprint_blob FROM host_config_requests")
        .unwrap();
    let blobs = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)).unwrap();
    for blob in blobs {
        let digest =
            secrets::open(&blob.unwrap()).unwrap_or_else(|_| panic!("request fingerprint DPAPI"));
        assert_eq!(digest.len(), 32);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        needles.push(hex.to_uppercase().into_bytes());
        needles.push(hex.into_bytes());
        needles.push(digest);
    }
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let contents = fs::read(path).expect("retained fixture artifact");
            for needle in &needles {
                assert!(
                    !contains(&contents, needle),
                    "plaintext credential/token/fingerprint persisted in fixture artifacts"
                );
            }
        }
    }
}

struct ProviderProbe {
    origin: String,
    calls: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for ProviderProbe {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn provider_probe() -> ProviderProbe {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let app = Router::new()
        .route("/probe-health", get(|| async { StatusCode::NO_CONTENT }))
        .fallback(move || {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                StatusCode::INTERNAL_SERVER_ERROR
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    ProviderProbe {
        origin,
        calls,
        task,
    }
}

async fn assert_probe_live(client: &Client, probe: &ProviderProbe) {
    assert_eq!(
        client
            .get(format!("{}/probe-health", probe.origin))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        0,
        "configuration must not send Provider requests"
    );
}

async fn exercise_crash(phase: &'static str) {
    let root = std::env::temp_dir().join(format!("nhc-i09-{phase}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).expect("new retained crash fixture");
    fs::write(root.join("fixture-owner"), b"D-R13-I09").unwrap();
    eprintln!("I09 retained fixture: {}", root.display());
    event(
        &root,
        json!({"kind":"begin","phase":phase,"parent_pid":std::process::id()}),
    );
    let old = format!("fixture-{}", uuid::Uuid::new_v4());
    let new = format!("fixture-{}", uuid::Uuid::new_v4());
    let other = format!("fixture-{}", uuid::Uuid::new_v4());
    let mut forbidden = vec![old.clone(), new.clone(), other.clone()];
    let client = client();
    let probe = provider_probe().await;
    assert_probe_live(&client, &probe).await;
    let base = format!("{}/v1", probe.origin);

    let mut seed = start_host(&root, "seed", None, &client, &forbidden).await;
    forbidden.push(seed.token.clone());
    let initial = snapshot(&client, &seed, &forbidden).await;
    assert_eq!(initial.revision, 0);
    assert!(initial.routes.is_empty());
    let denied = client
        .get(format!("{}/api/host/config", seed.origin))
        .send()
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "prove this is the authenticated production route"
    );
    let _: Value = public_json(denied, &forbidden).await;
    let first = put(
        &client,
        &seed,
        ROUTE,
        OLD_KEY,
        &request(0, &base, "before", &old),
        &forbidden,
    )
    .await;
    assert!(!first.replayed && first.receipt.changed);
    assert_eq!(first.receipt.revision, 1);
    let second = put(
        &client,
        &seed,
        OTHER_ROUTE,
        OTHER_KEY,
        &request(1, &base, "preserved", &other),
        &forbidden,
    )
    .await;
    assert!(!second.replayed && second.receipt.changed);
    assert_eq!(second.receipt.revision, 2);
    let before = snapshot(&client, &seed, &forbidden).await;
    assert_eq!(before.revision, 2);
    assert_eq!(before.routes.len(), 2);
    for route in &before.routes {
        assert!(route.key_env.is_none());
        assert_eq!(route.credential_source, CredentialSource::Stored);
    }
    let before_db = db_snapshot(&root);
    assert_secret(&root, ROUTE, &old);
    assert_secret(&root, OTHER_ROUTE, &other);
    assert_no_plaintext_files(&root, &forbidden);
    seed.process.kill_and_wait("seed-complete");

    let mut stopped = start_host(&root, "at-stop", Some(phase), &client, &forbidden).await;
    forbidden.push(stopped.token.clone());
    assert!(snapshot(&client, &stopped, &forbidden).await == before);
    assert!(
        db_snapshot(&root) == before_db,
        "opening Host must preserve seeded state"
    );
    let payload = request(2, &base, "after", &new);
    let send_client = client.clone();
    let url = format!("{}/api/host/routes/{ROUTE}", stopped.origin);
    let token = stopped.token.clone();
    let body = payload.clone();
    event(
        &root,
        json!({"kind":"original_put_started","run":"at-stop","pid":stopped.process.pid,"phase":phase}),
    );
    let pending = tokio::spawn(async move {
        send_client
            .put(url)
            .header("x-peachsh-token", token)
            .header("idempotency-key", LOST_KEY)
            .json(&body)
            .send()
            .await
    });
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        stopped.process.assert_running();
        assert!(
            !pending.is_finished(),
            "HTTP response/transport completed before the true transaction stop"
        );
        if let Ok(text) = fs::read_to_string(root.join(format!("{phase}.ready")))
            && let Ok(pid) = text.parse::<u32>()
        {
            assert_eq!(
                pid, stopped.process.pid,
                "stop must belong to this original Host process"
            );
            break;
        }
        assert!(Instant::now() < deadline, "real transaction stop timed out");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    event(
        &root,
        json!({"kind":"transaction_stop_observed","phase":phase,"pid":stopped.process.pid,"http_response_finished":pending.is_finished()}),
    );
    let committed = phase == "after_commit";
    let at_stop = db_snapshot(&root);
    if !committed {
        assert!(
            at_stop == before_db,
            "before-commit readers must see all old state"
        );
    }
    let durable_receipt = database_receipt(&root);
    assert_eq!(durable_receipt.is_some(), committed);
    assert_secret(&root, ROUTE, if committed { &new } else { &old });
    assert_secret(&root, OTHER_ROUTE, &other);
    assert_no_plaintext_files(&root, &forbidden);
    assert!(
        !pending.is_finished(),
        "after-commit stop must still precede HTTP response"
    );
    stopped.process.kill_and_wait(phase);
    let interrupted = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("HTTP transport must finish after child kill")
        .expect("HTTP request task");
    assert!(
        interrupted.is_err(),
        "terminated Host must not deliver a successful response"
    );
    event(
        &root,
        json!({"kind":"original_put_transport_lost","phase":phase}),
    );

    let mut expected = before.clone();
    if committed {
        expected.revision = 3;
        let route = expected.routes.iter_mut().find(|r| r.id == ROUTE).unwrap();
        route.name = "after".into();
        route.model = "after".into();
        let receipt = durable_receipt.as_ref().unwrap();
        assert_eq!(receipt.revision, 3);
        assert_eq!(receipt.route_id, ROUTE);
        assert!(receipt.changed);
        assert!(receipt.route == *route);
        assert!(!receipt.operation_id.is_empty());
    }
    for run in ["restart-1", "restart-2"] {
        let mut host = start_host(&root, run, None, &client, &forbidden).await;
        forbidden.push(host.token.clone());
        assert!(
            snapshot(&client, &host, &forbidden).await == expected,
            "HTTP recovery state differs"
        );
        assert!(
            lookup(&client, &host, LOST_KEY, &forbidden).await == durable_receipt,
            "original durable receipt differs"
        );
        assert!(lookup(&client, &host, OLD_KEY, &forbidden).await == Some(first.receipt.clone()));
        assert!(
            lookup(&client, &host, OTHER_KEY, &forbidden).await == Some(second.receipt.clone())
        );
        if committed && run == "restart-1" {
            let replay = put(&client, &host, ROUTE, LOST_KEY, &payload, &forbidden).await;
            assert!(replay.replayed);
            assert!(Some(replay.receipt) == durable_receipt);
        }
        assert!(
            db_snapshot(&root) == at_stop,
            "restart/query/replay changed durable rows or ciphertext"
        );
        for table in [
            "network_authorizations",
            "network_confirmations",
            "network_calls",
            "network_events",
            "task_network_bindings",
            "project_budgets",
            "budget_requests",
            "budget_calls",
        ] {
            assert!(
                at_stop.0[table] == before_db.0[table],
                "configuration changed grant/call/budget state"
            );
        }
        let db = read_db(&root);
        let total: i64 = db
            .query_row("SELECT COUNT(*) FROM host_config_requests", [], |r| {
                r.get(0)
            })
            .unwrap();
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM host_config_requests WHERE key=?1",
                [LOST_KEY],
                |r| r.get(0),
            )
            .unwrap();
        let revision: i64 = db
            .query_row(
                "SELECT revision FROM host_config_state WHERE id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, if committed { 3 } else { 2 });
        assert_eq!(count, i64::from(committed));
        assert_eq!(revision, if committed { 3 } else { 2 });
        drop(db);
        assert_secret(&root, ROUTE, if committed { &new } else { &old });
        assert_secret(&root, OTHER_ROUTE, &other);
        assert_probe_live(&client, &probe).await;
        host.process.kill_and_wait("restart-observations-complete");
        assert_no_plaintext_files(&root, &forbidden);
        event(
            &root,
            json!({"kind":"restart_verified","run":run,"revision":expected.revision,
            "receipt_count":count,"provider_calls":probe.calls.load(Ordering::SeqCst)}),
        );
    }
    assert_probe_live(&client, &probe).await;
    event(
        &root,
        json!({"kind":"complete","phase":phase,"provider_calls":0,"restarts_verified":2}),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn before_commit_kill_wait_restores_all_old_state_twice() {
    exercise_crash("before_commit").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_commit_before_response_kill_wait_restores_one_receipt_twice() {
    exercise_crash("after_commit").await;
}
