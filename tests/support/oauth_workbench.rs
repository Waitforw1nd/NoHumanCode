//! Browser, authorization server, MCP resource, and model are all local fixtures.
use super::*;
use axum::{
    Form,
    extract::Query,
    http::StatusCode,
    response::{Redirect, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    time::timeout,
};

const TOOL: &str = "mcp__demo__echo";
const ACCESS: [&str; 3] = ["qa-access-one", "qa-access-two", "qa-access-three"];
const REFRESH: [&str; 3] = ["qa-refresh-one", "qa-refresh-two", "qa-refresh-three"];
const CODE: &str = "qa-authorization-code";
type Fields = HashMap<String, String>;

#[derive(Default)]
struct Traffic {
    denied: bool,
    authorize: Fields,
    grants: Vec<Fields>,
    advertised: Vec<bool>,
    calls: Vec<String>,
    reject_call: bool,
    omit_expiry: bool,
    reject_discovery: Option<(&'static str, usize)>,
    rejected_discoveries: usize,
    invalid_grant: bool,
}

#[derive(Clone)]
struct Fixture {
    base: String,
    traffic: Arc<Mutex<Traffic>>,
}

async fn authorize(State(f): State<Fixture>, Query(query): Query<Fields>) -> Redirect {
    let mut url = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    let mut traffic = f.traffic.lock().unwrap();
    let field = if traffic.denied {
        ("error", "access_denied")
    } else {
        ("code", CODE)
    };
    url.query_pairs_mut()
        .append_pair("state", &query["state"])
        .append_pair(field.0, field.1);
    traffic.authorize = query;
    Redirect::temporary(url.as_str())
}

async fn token(State(f): State<Fixture>, Form(fields): Form<Fields>) -> Response {
    let mut t = f.traffic.lock().unwrap();
    let index = t.grants.len();
    t.grants.push(fields.clone());
    if index >= ACCESS.len()
        || fields["client_id"] != "coda-test"
        || fields["resource"] != format!("{}/mcp", f.base)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if index == 0 {
        assert_eq!(fields["grant_type"], "authorization_code");
        assert_eq!(fields["code"], CODE);
        assert_eq!(fields["redirect_uri"], t.authorize["redirect_uri"]);
        let verifier = &fields["code_verifier"];
        assert!((43..=128).contains(&verifier.len()));
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            t.authorize["code_challenge"]
        );
    } else {
        assert_eq!(fields["grant_type"], "refresh_token");
        assert_eq!(fields["refresh_token"], REFRESH[index - 1]);
        if t.invalid_grant {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant"})),
            )
                .into_response();
        }
    }
    let mut reply = json!({
        "access_token": ACCESS[index],
        "refresh_token": REFRESH[index],
        "token_type": "Bearer"
    });
    if !t.omit_expiry {
        reply["expires_in"] = json!(if index == 0 { 0 } else { 3600 });
    }
    Json(reply).into_response()
}

async fn mcp(State(f): State<Fixture>, headers: HeaderMap, Json(req): Json<Value>) -> Response {
    let mut t = f.traffic.lock().unwrap();
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let current = t.grants.len();
    if current == 0 || auth != format!("Bearer {}", ACCESS[current - 1]) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let method = req["method"].as_str().unwrap();
    if t.reject_discovery == Some((method, current)) {
        t.rejected_discoveries += 1;
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let result = match method {
        "initialize" => {
            json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"demo","version":"1"}})
        }
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            json!({"tools":[{"name":"echo","description":"Test tool","inputSchema":{"type":"object","properties":{}}}]})
        }
        "tools/call" => {
            assert_eq!(req["params"]["name"], "echo");
            t.calls.push(auth.to_owned());
            if t.reject_call {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            json!({"content":[{"type":"text","text":"echo worked"}]})
        }
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    Json(json!({"jsonrpc":"2.0","id":req["id"],"result":result})).into_response()
}

async fn model(State(f): State<Fixture>, Json(req): Json<Value>) -> Response {
    let advertised = req["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["function"]["name"] == TOOL);
    f.traffic.lock().unwrap().advertised.push(advertised);
    assert_private(&req.to_string());
    let (delta, finish) = if advertised
        && req["messages"].as_array().unwrap().last().unwrap()["role"] == "user"
    {
        (
            json!({"tool_calls":[{"index":0,"id":"echo1","type":"function","function":{"name":TOOL,"arguments":"{}"}}]}),
            "tool_calls",
        )
    } else {
        (json!({"content":"done"}), "stop")
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":finish}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
        .into_response()
}

fn assert_private(text: &str) {
    for secret in ACCESS.into_iter().chain(REFRESH).chain([CODE]) {
        assert!(!text.contains(secret), "OAuth secret in output");
    }
}

impl Fixture {
    fn command(&self, root: &Path, workspace: &str, args: &[&str]) -> Command {
        let entries: Vec<_> = ["project","neighbor"].iter().map(|name| json!({
            "workspace":root.join(name),"name":"demo","url":format!("{}/mcp", self.base),
            "oauth":{"clientId":"coda-test","issuer":format!("{}/issuer",self.base),"scopes":["tools"]}
        })).collect();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_coda"));
        cmd.kill_on_drop(true)
            .stdin(Stdio::null())
            .args(args)
            .arg("--dir")
            .arg(root.join(workspace))
            .arg("--data-dir")
            .arg(root.join("data"))
            .env("CODA_MCP_HTTP_SERVERS", json!(entries).to_string())
            .env("OPENAI_API_KEY", "sk-qa-model")
            .env("OPENAI_BASE_URL", format!("{}/v1", self.base))
            .env("CODA_MODEL", "gpt-test");
        cmd
    }

    async fn ask(&self, root: &Path, workspace: &str, success: bool) -> String {
        let output = timeout(
            Duration::from_secs(30),
            self.command(root, workspace, &["ask", "-y", "Echo"])
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        let logs = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_private(&logs);
        assert_eq!(output.status.success(), success, "{logs}");
        logs
    }

    async fn login(&self, root: &Path, denied: bool) {
        self.traffic.lock().unwrap().denied = denied;
        let child = self
            .command(root, "project", &["mcp", "login", "demo"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        self.complete_browser_login(child, denied).await;
    }

    async fn complete_browser_login(&self, mut child: Child, denied: bool) {
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let mut logs = String::new();
        let endpoint = format!("{}/authorize?", self.base);
        while let Some(line) = lines.next_line().await.unwrap() {
            logs.push_str(&line);
            if let Some(start) = line.find(&endpoint) {
                let response = reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .unwrap()
                    .get(&line[start..])
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status().is_success(), !denied);
                break;
            }
        }
        while let Some(line) = lines.next_line().await.unwrap() {
            logs.push_str(&line);
        }
        let out = child.wait_with_output().await.unwrap();
        assert_private(&logs);
        assert_private(&String::from_utf8_lossy(&out.stdout));
        assert_eq!(out.status.success(), !denied, "{logs}");
        let t = self.traffic.lock().unwrap();
        assert_eq!(t.authorize["client_id"], "coda-test");
        assert_eq!(t.authorize["response_type"], "code");
        assert_eq!(t.authorize["scope"], "tools");
        assert_eq!(t.authorize["resource"], format!("{}/mcp", self.base));
        assert!(!t.authorize["state"].is_empty());
        assert_eq!(t.authorize["code_challenge_method"], "S256");
    }
}

async fn start_server() -> (Fixture, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Fixture {
        base: format!("http://{}", listener.local_addr().unwrap()),
        traffic: Arc::default(),
    };
    let app = Router::new()
        .route(
            "/mcp",
            post(mcp)
                .get(|State(f): State<Fixture>| async move {
                    let challenge = format!("Bearer resource_metadata=\"{}/meta\"", f.base);
                    (
                        StatusCode::UNAUTHORIZED,
                        [(header::WWW_AUTHENTICATE, challenge)],
                    )
                })
                .delete(|| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/meta",
            get(|State(f): State<Fixture>| async move {
                Json(json!({
                    "resource": format!("{}/mcp", f.base),
                    "authorization_servers": [format!("{}/issuer", f.base)]
                }))
            }),
        )
        .route(
            "/.well-known/oauth-authorization-server/issuer",
            get(|State(f): State<Fixture>| async move {
                Json(json!({
                    "issuer": format!("{}/issuer", f.base),
                    "authorization_endpoint": format!("{}/authorize", f.base),
                    "token_endpoint": format!("{}/token", f.base),
                    "code_challenge_methods_supported": ["S256"],
                    "response_types_supported": ["code"]
                }))
            }),
        )
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/v1/chat/completions", post(model))
        .with_state(f.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (f, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "workspace credentials require Windows DPAPI")]
async fn oauth_login_refresh_rotation_denial_and_workspace_isolation() {
    let (f, server) = start_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for name in ["project", "neighbor"] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    assert!(
        f.ask(root, "project", true)
            .await
            .contains("mcp login demo")
    );
    assert_eq!(f.traffic.lock().unwrap().advertised, [false]);
    timeout(Duration::from_secs(30), f.login(root, true))
        .await
        .unwrap();
    assert!(f.traffic.lock().unwrap().grants.is_empty());
    assert!(f.traffic.lock().unwrap().calls.is_empty());
    timeout(Duration::from_secs(30), f.login(root, false))
        .await
        .unwrap();
    assert_eq!(f.traffic.lock().unwrap().grants.len(), 1);
    assert!(
        f.ask(root, "neighbor", true)
            .await
            .contains("mcp login demo")
    );
    assert_eq!(f.traffic.lock().unwrap().advertised, [false, false]);
    assert_eq!(f.traffic.lock().unwrap().grants.len(), 1);
    let logs = f.ask(root, "project", true).await;
    assert!(logs.contains(&format!("{}/mcp", f.base)), "{logs}");
    assert_eq!(
        f.traffic.lock().unwrap().calls,
        [format!("Bearer {}", ACCESS[1])]
    );
    f.traffic.lock().unwrap().reject_call = true;
    f.ask(root, "project", false).await;
    assert_eq!(f.traffic.lock().unwrap().calls.len(), 2);
    assert_eq!(f.traffic.lock().unwrap().grants.len(), 2);
    f.traffic.lock().unwrap().reject_call = false;
    f.ask(root, "project", true).await;
    let t = f.traffic.lock().unwrap();
    assert_eq!(t.grants.len(), 3);
    assert_eq!(
        t.calls,
        [
            format!("Bearer {}", ACCESS[1]),
            format!("Bearer {}", ACCESS[1]),
            format!("Bearer {}", ACCESS[2])
        ]
    );
    let database = root.join("data/coda.sqlite3");
    assert!(database.is_file());
    for file in std::fs::read_dir(database.parent().unwrap()).unwrap() {
        let file = file.unwrap();
        if file
            .file_name()
            .to_string_lossy()
            .starts_with("coda.sqlite3")
        {
            assert_private(&String::from_utf8_lossy(
                &std::fs::read(file.path()).unwrap(),
            ));
        }
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "workspace credentials require Windows DPAPI")]
async fn oauth_discovery_refreshes_once_and_invalid_grant_prompts_for_login() {
    let (f, server) = start_server().await;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for name in ["project", "neighbor"] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    {
        let mut t = f.traffic.lock().unwrap();
        t.omit_expiry = true;
        t.reject_discovery = Some(("tools/list", 1));
    }
    timeout(Duration::from_secs(30), f.login(root, false))
        .await
        .unwrap();
    f.ask(root, "project", true).await;
    {
        let mut t = f.traffic.lock().unwrap();
        assert_eq!(t.rejected_discoveries, 1);
        assert_eq!(t.grants.len(), 2);
        assert_eq!(t.advertised, [true, true]);
        assert_eq!(t.calls, [format!("Bearer {}", ACCESS[1])]);

        t.reject_discovery = Some(("initialize", 2));
        t.invalid_grant = true;
    }
    for expected_requests in [3, 4] {
        let logs = f.ask(root, "project", true).await;
        assert!(logs.contains("mcp login demo"), "{logs}");
        let t = f.traffic.lock().unwrap();
        assert_eq!(t.rejected_discoveries, 2);
        assert_eq!(t.grants.len(), 3);
        assert_eq!(t.advertised.len(), expected_requests);
        assert_eq!(t.advertised.last(), Some(&false));
        assert_eq!(t.calls.len(), 1);
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "workspace credentials require Windows DPAPI")]
async fn oauth_slash_login_makes_tool_available_in_the_current_chat() {
    let (f, server) = start_server().await;
    f.traffic.lock().unwrap().omit_expiry = true;
    let tmp = tempfile::tempdir().unwrap();
    for name in ["project", "neighbor"] {
        std::fs::create_dir(tmp.path().join(name)).unwrap();
    }
    let mut chat = f
        .command(tmp.path(), "project", &["chat", "-y"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    chat.stdin
        .take()
        .unwrap()
        .write_all(b"Before login\n/mcp login demo\nEcho\n/exit\n")
        .await
        .unwrap();
    timeout(
        Duration::from_secs(30),
        f.complete_browser_login(chat, false),
    )
    .await
    .unwrap();
    let t = f.traffic.lock().unwrap();
    assert_eq!(t.grants.len(), 1);
    assert_eq!(t.advertised, [false, true, true]);
    assert_eq!(t.calls, [format!("Bearer {}", ACCESS[0])]);
    server.abort();
}
