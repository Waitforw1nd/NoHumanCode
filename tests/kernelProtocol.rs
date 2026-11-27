#![allow(non_snake_case)]

use axum::{
    Json, Router,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode, header},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::{Child, ChildStdin, Command},
    task::JoinHandle,
};
use uuid::Uuid;

#[derive(Clone, Debug)]
struct RecordedUpstreamReq {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Value,
}

#[derive(Clone, Debug)]
enum MockReply {
    Sse(Vec<String>),
    ErrorJson { status: u16, body: Value },
}

#[derive(Default)]
struct MockUpstreamState {
    queues: HashMap<String, VecDeque<MockReply>>,
    recorded: Vec<RecordedUpstreamReq>,
}

struct MockUpstream {
    port: u16,
    state: Arc<Mutex<MockUpstreamState>>,
    task: JoinHandle<()>,
}

impl MockUpstream {
    async fn start() -> Self {
        let state = Arc::new(Mutex::new(MockUpstreamState::default()));
        let app = Router::new()
            .route("/m/{run_key}/responses", post(handle_mock_responses))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { port, state, task }
    }

    fn enqueue(&self, run_key: &str, reply: MockReply) {
        self.state
            .lock()
            .unwrap()
            .queues
            .entry(run_key.to_owned())
            .or_default()
            .push_back(reply);
    }

    fn recorded_for(&self, path_suffix: &str) -> Vec<RecordedUpstreamReq> {
        self.state
            .lock()
            .unwrap()
            .recorded
            .iter()
            .filter(|r| r.path == path_suffix)
            .cloned()
            .collect()
    }

    fn clear_recorded(&self) {
        self.state.lock().unwrap().recorded.clear();
    }
}

impl Drop for MockUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_mock_responses(
    State(state): State<Arc<Mutex<MockUpstreamState>>>,
    AxumPath(run_key): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let path = format!("/m/{run_key}/responses");
    let reply = {
        let mut st = state.lock().unwrap();
        st.recorded.push(RecordedUpstreamReq {
            method: "POST".into(),
            path: path.clone(),
            authorization: auth,
            body,
        });
        st.queues
            .get_mut(&run_key)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| {
                panic!("unexpected upstream request to {path} with empty mock queue")
            })
    };

    match reply {
        MockReply::Sse(frames) => {
            let payload = frames.join("");
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from(payload),
            )
                .into_response()
        }
        MockReply::ErrorJson { status, body } => {
            let sc = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (sc, Json(body)).into_response()
        }
    }
}

fn sse_responses_text(
    response_id: &str,
    thinking_chunks: &[&str],
    encrypted_content: Option<&str>,
    text_chunks: &[&str],
    usage: Value,
) -> MockReply {
    let mut frames = vec![format!(
        "event: response.created\ndata: {}\n\n",
        json!({
            "type": "response.created",
            "response": { "id": response_id, "status": "in_progress" }
        })
    )];

    if !thinking_chunks.is_empty() || encrypted_content.is_some() {
        let mut summary_full = String::new();
        for chunk in thinking_chunks {
            summary_full.push_str(chunk);
            frames.push(format!(
                "event: response.reasoning_summary_text.delta\ndata: {}\n\n",
                json!({
                    "type": "response.reasoning_summary_text.delta",
                    "item_id": "rs_1",
                    "output_index": 0,
                    "delta": chunk,
                })
            ));
        }
        let mut item = json!({
            "id": "rs_1",
            "type": "reasoning",
            "summary": [{ "type": "summary_text", "text": summary_full }],
        });
        if let Some(enc) = encrypted_content {
            item["encrypted_content"] = json!(enc);
        }
        frames.push(format!(
            "event: response.output_item.done\ndata: {}\n\n",
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": item,
            })
        ));
    }

    for chunk in text_chunks {
        frames.push(format!(
            "event: response.output_text.delta\ndata: {}\n\n",
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": 1,
                "delta": chunk,
            })
        ));
    }

    frames.push(format!(
        "event: response.completed\ndata: {}\n\n",
        json!({
            "type": "response.completed",
            "response": {
                "id": response_id,
                "status": "completed",
                "usage": usage,
            }
        })
    ));
    frames.push("data: [DONE]\n\n".into());
    MockReply::Sse(frames)
}

fn sse_responses_tool_calls(
    response_id: &str,
    text: Option<&str>,
    calls: &[(&str, &str, Value)],
    usage: Value,
) -> MockReply {
    let mut frames = vec![format!(
        "event: response.created\ndata: {}\n\n",
        json!({
            "type": "response.created",
            "response": { "id": response_id, "status": "in_progress" }
        })
    )];
    let mut next_idx = 0usize;
    if let Some(t) = text {
        frames.push(format!(
            "event: response.output_text.delta\ndata: {}\n\n",
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": next_idx,
                "delta": t,
            })
        ));
        next_idx += 1;
    }
    for (i, (call_id, name, args)) in calls.iter().enumerate() {
        frames.push(format!(
            "event: response.output_item.done\ndata: {}\n\n",
            json!({
                "type": "response.output_item.done",
                "output_index": next_idx + i,
                "item": {
                    "id": format!("fc_{i}"),
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": args.to_string(),
                }
            })
        ));
    }
    frames.push(format!(
        "event: response.completed\ndata: {}\n\n",
        json!({
            "type": "response.completed",
            "response": {
                "id": response_id,
                "status": "completed",
                "usage": usage,
            }
        })
    ));
    frames.push("data: [DONE]\n\n".into());
    MockReply::Sse(frames)
}

struct EmbeddedKernelProc {
    child: Child,
    stdin: Option<ChildStdin>,
    port: u16,
    pid: u32,
    version: String,
    token: String,
    data_dir: tempfile::TempDir,
    stderr_buf: Arc<Mutex<Vec<String>>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl EmbeddedKernelProc {
    async fn spawn() -> Self {
        let data_dir = tempfile::tempdir().unwrap();
        let mut raw = [0u8; 24];
        raw[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        raw[16..].copy_from_slice(&Uuid::new_v4().as_bytes()[..8]);
        let token = URL_SAFE_NO_PAD.encode(raw);

        let bin = env!("CARGO_BIN_EXE_coda");
        let mut cmd = Command::new(bin);
        cmd.args([
            "kernel",
            "serve",
            "--embedded",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--token-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);

        let mut child = cmd.spawn().expect("spawn coda kernel serve --embedded");
        let pid = child.id().expect("child pid");
        let mut stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");

        let stderr_buf = Arc::new(Mutex::new(Vec::<String>::new()));
        let stderr_buf_c = stderr_buf.clone();
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                stderr_buf_c.lock().unwrap().push(line);
            }
        });

        let handshake_line = json!({ "token": token }).to_string() + "\n";
        stdin
            .write_all(handshake_line.as_bytes())
            .await
            .expect("write token handshake");
        stdin.flush().await.expect("flush token handshake");

        let mut stdout_reader = BufReader::new(stdout).lines();
        let ready_line = tokio::time::timeout(Duration::from_secs(10), stdout_reader.next_line())
            .await
            .expect("timeout waiting for ready line")
            .expect("read ready line")
            .expect("unexpected stdout EOF before ready");

        let ready_json: Value =
            serde_json::from_str(&ready_line).expect("ready line must be valid JSON");
        assert_eq!(ready_json["type"], "ready");
        assert_eq!(ready_json["protocol"], "codaKernel/1");
        assert_eq!(ready_json["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(ready_json["pid"], json!(pid));
        let port = ready_json["port"].as_u64().expect("ready port") as u16;
        assert!(port > 0);
        let version = ready_json["version"].as_str().unwrap().to_owned();

        Self {
            child,
            stdin: Some(stdin),
            port,
            pid,
            version,
            token,
            data_dir,
            stderr_buf,
            stderr_task: Some(stderr_task),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn assert_no_sqlite_or_locks(&self) {
        let entries: Vec<PathBuf> = std::fs::read_dir(self.data_dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|de| de.path()))
            .collect();
        for p in &entries {
            let name = p.file_name().unwrap().to_string_lossy().to_lowercase();
            assert!(
                !name.contains("sqlite") && !name.ends_with(".lock") && !name.ends_with(".db"),
                "embedded mode must not create SQLite or lock files in data-dir, found: {}",
                p.display()
            );
        }
    }

    fn assert_no_secret_leaks(&self, secrets: &[&str]) {
        let lines = self.stderr_buf.lock().unwrap();
        for line in lines.iter() {
            assert!(
                !line.contains(&self.token),
                "stderr leaked kernel token: {line}"
            );
            for sec in secrets {
                assert!(
                    !line.contains(sec),
                    "stderr leaked secret token {sec}: {line}"
                );
            }
        }
    }

    async fn close_stdin_and_wait_exit(mut self, max_ms: u64, secrets: &[&str]) {
        let t0 = Instant::now();
        drop(self.stdin.take());
        let status = tokio::time::timeout(Duration::from_millis(max_ms), self.child.wait())
            .await
            .expect("child did not exit within deadline after stdin EOF")
            .expect("wait child failed");
        let elapsed = t0.elapsed();
        assert!(
            status.success(),
            "coda exited with non-zero status {status:?} after {elapsed:?}; stderr: {:?}",
            self.stderr_buf.lock().unwrap()
        );
        assert!(
            elapsed <= Duration::from_millis(max_ms),
            "exit took {elapsed:?} > {max_ms}ms"
        );
        if let Some(t) = self.stderr_task.take() {
            let _ = tokio::time::timeout(Duration::from_secs(1), t).await;
        }
        self.assert_no_sqlite_or_locks();
        self.assert_no_secret_leaks(secrets);
    }
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("codaKernel")
        .join(name)
}

fn load_fixture(name: &str) -> Value {
    let raw = std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("invalid JSON in fixture {name}: {e}"))
}

#[derive(Default)]
struct MatcherState {
    token: String,
    port: u16,
    pid: u32,
    version: String,
    mock_port: u16,
    bindings: HashMap<String, String>,
    proxy_tokens: HashMap<String, String>,
    run_scoped_bindings: HashMap<String, String>,
}

impl MatcherState {
    fn new(proc: &EmbeddedKernelProc, mock_port: u16) -> Self {
        Self {
            token: proc.token.clone(),
            port: proc.port,
            pid: proc.pid,
            version: proc.version.clone(),
            mock_port,
            bindings: HashMap::new(),
            proxy_tokens: HashMap::new(),
            run_scoped_bindings: HashMap::new(),
        }
    }

    fn reset_run_scoped(&mut self) {
        self.run_scoped_bindings.clear();
    }

    fn proxy_token(&mut self, placeholder: &str) -> String {
        self.proxy_tokens
            .entry(placeholder.to_owned())
            .or_insert_with(|| {
                format!(
                    "mock-{}-{}",
                    placeholder.trim_matches(['<', '>']),
                    Uuid::new_v4()
                )
            })
            .clone()
    }

    fn substitute_str(&mut self, s: &str) -> String {
        let mut out = s
            .replace("<TOKEN>", &self.token)
            .replace("<PORT>", &self.port.to_string())
            .replace("<VERSION>", &self.version)
            .replace(
                "http://127.0.0.1:61234",
                &format!("http://127.0.0.1:{}", self.mock_port),
            );

        for tag in [
            "<PROXY_TOKEN>",
            "<PROXY_TOKEN_1>",
            "<PROXY_TOKEN_2>",
            "<PROXY_TOKEN_BAD>",
            "<PROXY_TOKEN_OTHER>",
            "<PROXY_TOKEN_409>",
            "<PROXY_TOKEN_401>",
        ] {
            if out.contains(tag) {
                let tok = self.proxy_token(tag);
                out = out.replace(tag, &tok);
            }
        }

        let mut search_from = 0usize;
        while let Some(rel_start) = out[search_from..].find("<GATE_ID:") {
            let start = search_from + rel_start;
            if let Some(rel_end) = out[start..].find('>') {
                let end = start + rel_end + 1;
                let key = &out[start..end];
                if let Some(val) = self.bindings.get(key) {
                    let val = val.clone();
                    out.replace_range(start..end, &val);
                    search_from = start + val.len();
                    continue;
                }
            }
            break;
        }
        let mut search_from = 0usize;
        while let Some(rel_start) = out[search_from..].find("<ENTRY_ID:") {
            let start = search_from + rel_start;
            if let Some(rel_end) = out[start..].find('>') {
                let end = start + rel_end + 1;
                let key = &out[start..end];
                if let Some(val) = self.bindings.get(key) {
                    let val = val.clone();
                    out.replace_range(start..end, &val);
                    search_from = start + val.len();
                    continue;
                }
            }
            break;
        }
        out
    }

    fn substitute_value(&mut self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.substitute_str(s)),
            Value::Array(arr) => {
                Value::Array(arr.iter().map(|x| self.substitute_value(x)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, val)| (k.clone(), self.substitute_value(val)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn all_proxy_tokens(&self) -> Vec<String> {
        self.proxy_tokens.values().cloned().collect()
    }
}

fn is_valid_iso8601_offset(s: &str) -> bool {
    // e.g. 2026-11-26T21:55:30.123Z or 2026-11-27T01:00:00+08:00
    let Some((date, rest)) = s.split_once('T') else {
        return false;
    };
    let dparts: Vec<&str> = date.split('-').collect();
    if dparts.len() != 3 || dparts[0].len() != 4 || dparts[1].len() != 2 || dparts[2].len() != 2 {
        return false;
    }
    if !dparts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
        return false;
    }
    let has_offset = rest.ends_with('Z')
        || (rest.len() >= 6 && {
            let tail = &rest[rest.len() - 6..];
            (tail.starts_with('+') || tail.starts_with('-'))
                && tail.as_bytes()[3] == b':'
                && tail[1..3].chars().all(|c| c.is_ascii_digit())
                && tail[4..6].chars().all(|c| c.is_ascii_digit())
        });
    has_offset && rest.contains(':')
}

#[derive(Clone, Copy, Default)]
struct MatchOpts {
    ignore_token_estimates: bool,
    usage_openai_cached_math: bool,
}

fn bind_or_check(map: &mut HashMap<String, String>, key: &str, actual: &str, path: &str) {
    assert!(
        !actual.is_empty(),
        "{path}: placeholder {key} matched empty string"
    );
    if let Some(prev) = map.get(key) {
        assert_eq!(
            prev, actual,
            "{path}: placeholder {key} bound to '{prev}', got '{actual}'"
        );
    } else {
        map.insert(key.to_owned(), actual.to_owned());
    }
}

fn assert_matches_fixture(
    actual: &Value,
    expected: &Value,
    state: &mut MatcherState,
    path: &str,
    opts: MatchOpts,
) {
    match expected {
        Value::String(exp_s) => {
            if exp_s == "<VERSION>" {
                let act_s = actual
                    .as_str()
                    .unwrap_or_else(|| panic!("{path}: expected string <VERSION>, got {actual}"));
                assert_eq!(act_s, state.version, "{path}: version mismatch");
                return;
            }
            if exp_s == "<PID>" {
                let act_n = actual
                    .as_u64()
                    .unwrap_or_else(|| panic!("{path}: expected integer <PID>, got {actual}"));
                assert_eq!(act_n, u64::from(state.pid), "{path}: pid mismatch");
                return;
            }
            if exp_s == "<PORT>" {
                let act_n = actual
                    .as_u64()
                    .unwrap_or_else(|| panic!("{path}: expected integer <PORT>, got {actual}"));
                assert_eq!(act_n, u64::from(state.port), "{path}: port mismatch");
                return;
            }
            if exp_s == "<TS>" {
                let act_s = actual.as_str().unwrap_or_else(|| {
                    panic!("{path}: expected timestamp string <TS>, got {actual}")
                });
                assert!(
                    is_valid_iso8601_offset(act_s),
                    "{path}: invalid ISO-8601 offset timestamp '{act_s}'"
                );
                return;
            }
            if exp_s == "<ANY>" {
                let act_s = actual.as_str().unwrap_or_else(|| {
                    panic!("{path}: expected non-empty string <ANY>, got {actual}")
                });
                assert!(!act_s.is_empty(), "{path}: <ANY> string cannot be empty");
                return;
            }
            if exp_s == "<OPAQUE>" {
                let act_s = actual.as_str().unwrap_or_else(|| {
                    panic!("{path}: expected non-empty string <OPAQUE>, got {actual}")
                });
                assert!(!act_s.is_empty(), "{path}: <OPAQUE> string cannot be empty");
                return;
            }
            if (exp_s.starts_with("<ENTRY_ID:") || exp_s.starts_with("<GATE_ID:"))
                && exp_s.ends_with('>')
            {
                let act_s = actual
                    .as_str()
                    .unwrap_or_else(|| panic!("{path}: expected string for {exp_s}, got {actual}"));
                bind_or_check(&mut state.bindings, exp_s, act_s, path);
                return;
            }
            if (path.ends_with(".blockId") && exp_s.starts_with("b-"))
                || (path.ends_with(".batchId") && exp_s.starts_with("batch-"))
            {
                let act_s = actual
                    .as_str()
                    .unwrap_or_else(|| panic!("{path}: expected string for {exp_s}, got {actual}"));
                let scope_key = if path.ends_with(".blockId") {
                    format!("blockId:{exp_s}")
                } else {
                    format!("batchId:{exp_s}")
                };
                bind_or_check(&mut state.run_scoped_bindings, &scope_key, act_s, path);
                return;
            }
            assert_eq!(actual, expected, "{path}: string mismatch");
        }
        Value::Array(exp_arr) => {
            let act_arr = actual
                .as_array()
                .unwrap_or_else(|| panic!("{path}: expected array, got {actual}"));
            assert_eq!(
                act_arr.len(),
                exp_arr.len(),
                "{path}: array length mismatch (actual {}, expected {})\nactual: {actual}\nexpected: {expected}",
                act_arr.len(),
                exp_arr.len()
            );
            for (i, (a, e)) in act_arr.iter().zip(exp_arr.iter()).enumerate() {
                assert_matches_fixture(a, e, state, &format!("{path}[{i}]"), opts);
            }
        }
        Value::Object(exp_obj) => {
            let act_obj = actual
                .as_object()
                .unwrap_or_else(|| panic!("{path}: expected object, got {actual}"));

            if opts.usage_openai_cached_math
                && (path.ends_with(".usage") || path == "usage")
                && exp_obj.contains_key("input")
                && exp_obj.contains_key("output")
            {
                let act_in = act_obj.get("input").and_then(Value::as_u64).unwrap_or(0);
                let act_cr = act_obj
                    .get("cacheRead")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let exp_in = exp_obj.get("input").and_then(Value::as_u64).unwrap_or(0);
                let exp_cr = exp_obj
                    .get("cacheRead")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                assert_eq!(act_cr, exp_cr, "{path}.cacheRead mismatch");
                assert!(
                    act_in == exp_in || (exp_cr > 0 && act_in + act_cr == exp_in),
                    "{path}.input mismatch: actual {act_in} (cacheRead {act_cr}), expected {exp_in}"
                );
                for (k, exp_v) in exp_obj {
                    if k == "input" || k == "cacheRead" {
                        continue;
                    }
                    let act_v = act_obj
                        .get(k)
                        .unwrap_or_else(|| panic!("{path}: missing key '{k}' in {actual}"));
                    assert_matches_fixture(act_v, exp_v, state, &format!("{path}.{k}"), opts);
                }
                return;
            }

            for (k, exp_v) in exp_obj {
                let child_path = format!("{path}.{k}");
                let act_v = act_obj
                    .get(k)
                    .unwrap_or_else(|| panic!("{path}: missing key '{k}' in {actual}"));
                if opts.ignore_token_estimates && (k == "tokens" || k == "tokensBefore") {
                    assert!(
                        act_v.as_u64().is_some(),
                        "{child_path}: expected non-negative integer token estimate, got {act_v}"
                    );
                    continue;
                }
                assert_matches_fixture(act_v, exp_v, state, &child_path, opts);
            }
            for k in act_obj.keys() {
                assert!(
                    exp_obj.contains_key(k),
                    "{path}: unexpected extra key '{k}' in actual object: {actual}"
                );
            }
        }
        _ => {
            if opts.ignore_token_estimates
                && (path.ends_with(".tokens") || path.ends_with(".tokensBefore"))
            {
                assert!(
                    actual.as_u64().is_some(),
                    "{path}: expected non-negative integer token estimate, got {actual}"
                );
                return;
            }
            assert_eq!(actual, expected, "{path}: value mismatch");
        }
    }
}

async fn send_raw_http_tcp(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&str>,
) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect raw tcp");
    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    let mut has_conn = false;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("connection") {
            has_conn = true;
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_conn {
        req.push_str("Connection: close\r\n");
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        req.push_str("Content-Type: application/json\r\n");
        req.push_str("\r\n");
        req.push_str(b);
    } else {
        req.push_str("\r\n");
    }
    stream.write_all(req.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("raw HTTP response timeout")
        .expect("read raw HTTP response");
    let text = String::from_utf8(raw).expect("utf8 HTTP response");
    let (head, body_raw) = text.split_once("\r\n\r\n").expect("HTTP header/body split");
    let status_line = head.lines().next().expect("HTTP status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("HTTP status code")
        .parse()
        .unwrap();

    let parsed_body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut decoded = String::new();
        let mut rem = body_raw;
        while let Some((sz_line, rest)) = rem.split_once("\r\n") {
            let sz = usize::from_str_radix(sz_line.trim(), 16).unwrap_or(0);
            if sz == 0 {
                break;
            }
            decoded.push_str(&rest[..sz]);
            rem = &rest[sz + 2..];
        }
        serde_json::from_str(&decoded).unwrap_or(Value::Null)
    } else if body_raw.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body_raw).unwrap_or(Value::Null)
    };
    (status, parsed_body)
}

async fn send_substituted_json_http(
    client: &reqwest::Client,
    base_url: &str,
    method_str: &str,
    path: &str,
    headers: &[(String, String)],
    body: Option<&Value>,
) -> (u16, Value) {
    let method = reqwest::Method::from_bytes(method_str.as_bytes()).unwrap();
    let url = format!("{base_url}{path}");
    let mut builder = client.request(method, url);
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("host") {
            continue;
        }
        builder = builder.header(k.as_str(), v.as_str());
    }
    if let Some(b) = body {
        builder = builder.json(b);
    }
    let resp = builder.send().await.expect("HTTP request failed");
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.unwrap();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "invalid JSON response ({status}) from {path}: {e}; raw={}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

async fn send_json_http(
    client: &reqwest::Client,
    base_url: &str,
    req_spec: &Value,
    state: &mut MatcherState,
) -> (u16, Value) {
    let method_str = req_spec["method"].as_str().unwrap();
    let path = state.substitute_str(req_spec["path"].as_str().unwrap());
    let mut headers = Vec::new();
    if let Some(hdrs) = req_spec["headers"].as_object() {
        for (k, v) in hdrs {
            headers.push((k.clone(), state.substitute_str(v.as_str().unwrap())));
        }
    }
    let body = req_spec.get("body").map(|b| state.substitute_value(b));
    send_substituted_json_http(client, base_url, method_str, &path, &headers, body.as_ref()).await
}

fn coalesce_sse_events(raw_events: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for env in raw_events {
        let ev = &env["event"];
        let kind = ev["type"].as_str().unwrap_or("");
        if kind == "thinking" || kind == "text" {
            let block_id = ev["blockId"].as_str().unwrap_or("");
            let delta = ev["delta"].as_str().unwrap_or("");
            let done = ev.get("done").and_then(Value::as_bool).unwrap_or(false);
            let redacted = ev.get("redacted").and_then(Value::as_bool);
            if let Some(last) = out.last_mut()
                && last["event"]["type"] == kind
                && last["event"]["blockId"] == block_id
                && !last["event"]
                    .get("done")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                let prev_delta = last["event"]["delta"].as_str().unwrap_or("").to_owned();
                last["event"]["delta"] = Value::String(prev_delta + delta);
                if done {
                    last["event"]["done"] = json!(true);
                }
                if let Some(r) = redacted {
                    last["event"]["redacted"] = json!(r);
                }
                continue;
            }
        }
        out.push(env.clone());
    }
    out
}

fn logical_step_delta(open_blocks: &mut HashMap<String, bool>, env: &Value) -> u64 {
    let ev = &env["event"];
    let kind = ev["type"].as_str().unwrap_or("");
    if kind == "thinking" || kind == "text" {
        let block_id = ev["blockId"].as_str().unwrap_or("");
        let done = ev.get("done").and_then(Value::as_bool).unwrap_or(false);
        let key = format!("{kind}:{block_id}");
        if let Some(is_open) = open_blocks.get_mut(&key)
            && *is_open
        {
            if done {
                *is_open = false;
            }
            return 0;
        }
        open_blocks.insert(key, !done);
    }
    1
}

async fn read_sse_stream_simple(
    mut resp: reqwest::Response,
    expected_after_start: u64,
) -> Vec<Value> {
    let mut raw_events = Vec::new();
    let mut buffer = Vec::<u8>::new();
    let mut next_expected_seq = expected_after_start;
    let mut saw_done = false;

    loop {
        let chunk_opt = tokio::time::timeout(Duration::from_secs(15), resp.chunk())
            .await
            .expect("SSE chunk timeout")
            .expect("SSE chunk error");
        let Some(chunk) = chunk_opt else {
            break;
        };
        buffer.extend_from_slice(&chunk);

        while let Some(pos) = find_double_newline(&buffer) {
            let frame_bytes: Vec<u8> = buffer.drain(..pos).collect();
            if buffer.starts_with(b"\r\n\r\n") {
                buffer.drain(..4);
            } else if buffer.starts_with(b"\n\n") {
                buffer.drain(..2);
            }
            let frame_str = String::from_utf8(frame_bytes).expect("utf8 SSE frame");
            let mut id_val: Option<String> = None;
            let mut event_val: Option<String> = None;
            let mut data_lines: Vec<String> = Vec::new();
            for line in frame_str.lines() {
                let line = line.trim_end_matches('\r');
                if line.starts_with(':') || line.is_empty() {
                    continue;
                }
                if let Some(rest) = line.strip_prefix("id:") {
                    id_val = Some(rest.trim().to_owned());
                } else if let Some(rest) = line.strip_prefix("event:") {
                    event_val = Some(rest.trim().to_owned());
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.trim_start().to_owned());
                }
            }
            if data_lines.is_empty() {
                continue;
            }
            let data_raw = data_lines.join("\n");
            let env: Value = serde_json::from_str(&data_raw)
                .unwrap_or_else(|e| panic!("invalid SSE JSON: {e}; raw={data_raw}"));
            let seq = env["seq"].as_u64().expect("SSE seq");
            assert_eq!(
                seq, next_expected_seq,
                "SSE seq gap: expected {next_expected_seq}, got {seq} ({env})"
            );
            next_expected_seq += 1;
            assert_eq!(
                id_val.as_deref(),
                Some(seq.to_string().as_str()),
                "SSE id header must equal seq"
            );
            let ev_type = env["event"]["type"].as_str().expect("SSE event.type");
            assert_eq!(
                event_val.as_deref(),
                Some(ev_type),
                "SSE event header must equal event.type"
            );
            assert!(!saw_done, "received SSE event after done: {env}");
            if ev_type == "done" {
                saw_done = true;
            }
            raw_events.push(env);
        }
    }
    assert!(saw_done, "reconnect SSE stream ended without done");
    raw_events
}

async fn read_sse_stream_with_interactions(
    mut resp: reqwest::Response,
    expected_after_start: u64,
    interactions: &[Value],
    client: &reqwest::Client,
    base_url: &str,
    shared_state: &Arc<Mutex<MatcherState>>,
) -> (Vec<Value>, Vec<JoinHandle<()>>) {
    let mut raw_events = Vec::new();
    let mut buffer = Vec::<u8>::new();
    let mut next_expected_seq = expected_after_start;
    let mut logical_seq = expected_after_start.saturating_sub(1);
    let mut open_blocks = HashMap::<String, bool>::new();
    let mut triggered = vec![false; interactions.len()];
    let mut spawned_sse = Vec::<JoinHandle<()>>::new();
    let mut saw_done = false;

    loop {
        let chunk_opt = tokio::time::timeout(Duration::from_secs(15), resp.chunk())
            .await
            .expect("SSE chunk timeout")
            .expect("SSE chunk error");
        let Some(chunk) = chunk_opt else {
            break;
        };
        buffer.extend_from_slice(&chunk);

        while let Some(pos) = find_double_newline(&buffer) {
            let frame_bytes: Vec<u8> = buffer.drain(..pos).collect();
            // drain separator (\r\n\r\n or \n\n)
            if buffer.starts_with(b"\r\n\r\n") {
                buffer.drain(..4);
            } else if buffer.starts_with(b"\n\n") {
                buffer.drain(..2);
            }
            let frame_str = String::from_utf8(frame_bytes).expect("utf8 SSE frame");
            let mut id_val: Option<String> = None;
            let mut event_val: Option<String> = None;
            let mut data_lines: Vec<String> = Vec::new();
            for line in frame_str.lines() {
                let line = line.trim_end_matches('\r');
                if line.starts_with(':') || line.is_empty() {
                    continue;
                }
                if let Some(rest) = line.strip_prefix("id:") {
                    id_val = Some(rest.trim().to_owned());
                } else if let Some(rest) = line.strip_prefix("event:") {
                    event_val = Some(rest.trim().to_owned());
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.trim_start().to_owned());
                }
            }
            if data_lines.is_empty() {
                continue;
            }
            let data_raw = data_lines.join("\n");
            let env: Value = serde_json::from_str(&data_raw)
                .unwrap_or_else(|e| panic!("invalid SSE JSON: {e}; raw={data_raw}"));
            let seq = env["seq"].as_u64().expect("SSE seq");
            assert_eq!(
                seq, next_expected_seq,
                "SSE seq gap: expected {next_expected_seq}, got {seq} ({env})"
            );
            next_expected_seq += 1;
            assert_eq!(
                id_val.as_deref(),
                Some(seq.to_string().as_str()),
                "SSE id header must equal seq"
            );
            let ev_type = env["event"]["type"].as_str().expect("SSE event.type");
            assert_eq!(
                event_val.as_deref(),
                Some(ev_type),
                "SSE event header must equal event.type"
            );
            assert!(!saw_done, "received SSE event after done: {env}");
            if ev_type == "done" {
                saw_done = true;
            }

            // Capture any <GATE_ID:*> / <ENTRY_ID:*> needed for upcoming interactions!
            if ev_type == "gate"
                && let Some(gid) = env["event"]["gateId"].as_str()
            {
                let mut st = shared_state.lock().unwrap();
                // Bind next unbound <GATE_ID:*> in interactions order when gate arrives.
                for inter in interactions {
                    if let Some(p) = inter["request"]["path"].as_str()
                        && let Some(start) = p.find("<GATE_ID:")
                        && let Some(rel_end) = p[start..].find('>')
                    {
                        let key = &p[start..start + rel_end + 1];
                        if !st.bindings.contains_key(key) {
                            st.bindings.insert(key.to_owned(), gid.to_owned());
                            break;
                        }
                    }
                }
            }

            logical_seq += logical_step_delta(&mut open_blocks, &env);
            raw_events.push(env);

            for (idx, inter) in interactions.iter().enumerate() {
                if triggered[idx] {
                    continue;
                }
                let after_seq = inter["afterSeq"].as_u64().unwrap();
                if logical_seq >= after_seq {
                    triggered[idx] = true;
                    let inter_clone = inter.clone();
                    if inter_clone["response"].get("sse").is_some() {
                        let client_c = client.clone();
                        let base_c = base_url.to_owned();
                        let st_c = shared_state.clone();
                        let handle = tokio::spawn(async move {
                            let req_spec = &inter_clone["request"];
                            let (url, hdrs) = {
                                let mut st = st_c.lock().unwrap();
                                let path = st.substitute_str(req_spec["path"].as_str().unwrap());
                                let mut h = Vec::new();
                                if let Some(obj) = req_spec["headers"].as_object() {
                                    for (k, v) in obj {
                                        h.push((k.clone(), st.substitute_str(v.as_str().unwrap())));
                                    }
                                }
                                (format!("{base_c}{path}"), h)
                            };
                            let mut b = client_c.get(url);
                            for (k, v) in hdrs {
                                b = b.header(&k, v);
                            }
                            let r = b.send().await.expect("reconnect SSE request failed");
                            assert_eq!(
                                r.status().as_u16(),
                                inter_clone["response"]["status"].as_u64().unwrap() as u16
                            );
                            let expected_sse = inter_clone["response"]["sse"].as_array().unwrap();
                            let start_after = expected_sse[0]["seq"].as_u64().unwrap();
                            let recon_raw = read_sse_stream_simple(r, start_after).await;
                            let recon_norm = coalesce_sse_events(&recon_raw);
                            let exp_norm = coalesce_sse_events(expected_sse);
                            let mut st = st_c.lock().unwrap();
                            assert_eq!(
                                recon_norm.len(),
                                exp_norm.len(),
                                "reconnect SSE length mismatch"
                            );
                            for (i, (a, e)) in recon_norm.iter().zip(exp_norm.iter()).enumerate() {
                                let mut a_no_seq = a.clone();
                                let mut e_no_seq = e.clone();
                                if let Some(m) = a_no_seq.as_object_mut() {
                                    m.remove("seq");
                                }
                                if let Some(m) = e_no_seq.as_object_mut() {
                                    m.remove("seq");
                                }
                                let opts = MatchOpts {
                                    ignore_token_estimates: true,
                                    usage_openai_cached_math: true,
                                };
                                assert_matches_fixture(
                                    &a_no_seq,
                                    &e_no_seq,
                                    &mut st,
                                    &format!("reconnect.sse[{i}]"),
                                    opts,
                                );
                            }
                        });
                        spawned_sse.push(handle);
                    } else {
                        let (method_str, path, headers, body) = {
                            let mut st = shared_state.lock().unwrap();
                            let req_spec = &inter_clone["request"];
                            let method_str = req_spec["method"].as_str().unwrap().to_owned();
                            let path = st.substitute_str(req_spec["path"].as_str().unwrap());
                            let mut headers = Vec::new();
                            if let Some(hdrs) = req_spec["headers"].as_object() {
                                for (k, v) in hdrs {
                                    headers
                                        .push((k.clone(), st.substitute_str(v.as_str().unwrap())));
                                }
                            }
                            let body = req_spec.get("body").map(|b| st.substitute_value(b));
                            (method_str, path, headers, body)
                        };
                        let (status, actual_body) = send_substituted_json_http(
                            client,
                            base_url,
                            &method_str,
                            &path,
                            &headers,
                            body.as_ref(),
                        )
                        .await;
                        let exp_status = inter_clone["response"]["status"].as_u64().unwrap() as u16;
                        assert_eq!(
                            status, exp_status,
                            "interaction {} status mismatch: body={actual_body}",
                            inter_clone["request"]["path"]
                        );
                        if let Some(exp_body) = inter_clone["response"].get("body") {
                            let mut st = shared_state.lock().unwrap();
                            assert_matches_fixture(
                                &actual_body,
                                exp_body,
                                &mut st,
                                "interaction.body",
                                MatchOpts::default(),
                            );
                        }
                    }
                }
            }
        }
    }

    assert!(saw_done, "SSE stream ended without a `done` event");
    for (idx, did) in triggered.iter().enumerate() {
        assert!(
            *did,
            "interaction[{idx}] (afterSeq={}) was never triggered! logical_seq={logical_seq}",
            interactions[idx]["afterSeq"]
        );
    }
    (raw_events, spawned_sse)
}

fn find_double_newline(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len() {
        if i + 1 < buf.len() && buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i);
        }
        if i + 3 < buf.len()
            && buf[i] == b'\r'
            && buf[i + 1] == b'\n'
            && buf[i + 2] == b'\r'
            && buf[i + 3] == b'\n'
        {
            return Some(i);
        }
    }
    None
}

async fn run_sse_step(
    client: &reqwest::Client,
    base_url: &str,
    step: &Value,
    state: &mut MatcherState,
    opts: MatchOpts,
) -> Vec<Value> {
    state.reset_run_scoped();
    let req_spec = &step["request"];
    let path = state.substitute_str(req_spec["path"].as_str().unwrap());
    let body = state.substitute_value(&req_spec["body"]);
    let mut builder = client.post(format!("{base_url}{path}")).json(&body);
    if let Some(hdrs) = req_spec["headers"].as_object() {
        for (k, v) in hdrs {
            builder = builder.header(k.as_str(), state.substitute_str(v.as_str().unwrap()));
        }
    }
    let resp = builder.send().await.expect("POST /runs failed");
    let exp_status = step["response"]["status"].as_u64().unwrap() as u16;
    assert_eq!(resp.status().as_u16(), exp_status);

    let interactions = step["interactions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut temp_state = MatcherState::new_from_existing(state);
    let shared = Arc::new(Mutex::new(std::mem::replace(
        state,
        std::mem::take(&mut temp_state),
    )));
    let (raw_events, handles) =
        read_sse_stream_with_interactions(resp, 1, interactions, client, base_url, &shared).await;
    for h in handles {
        h.await.expect("spawned reconnect task panicked");
    }
    *state = Arc::try_unwrap(shared)
        .ok()
        .expect("unique state")
        .into_inner()
        .unwrap();

    // Verify all raw thinking/text blocks closed with `done: true`.
    let mut open = HashMap::<String, bool>::new();
    for ev in &raw_events {
        let kind = ev["event"]["type"].as_str().unwrap_or("");
        if kind == "thinking" || kind == "text" {
            let bid = ev["event"]["blockId"].as_str().unwrap();
            let done = ev["event"]
                .get("done")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            open.insert(format!("{kind}:{bid}"), !done);
        }
    }
    assert!(
        open.values().all(|still_open| !*still_open),
        "unclosed streaming block in SSE: {open:?}"
    );

    let actual_norm = coalesce_sse_events(&raw_events);
    let expected_norm = coalesce_sse_events(step["response"]["sse"].as_array().unwrap());
    assert_eq!(
        actual_norm.len(),
        expected_norm.len(),
        "coalesced SSE count mismatch (actual {}, expected {})\nactual: {}\nexpected: {}",
        actual_norm.len(),
        expected_norm.len(),
        serde_json::to_string_pretty(&actual_norm).unwrap(),
        serde_json::to_string_pretty(&expected_norm).unwrap()
    );
    for (i, (a, e)) in actual_norm.iter().zip(expected_norm.iter()).enumerate() {
        // Ignore raw `seq` mismatch caused by coalescing multiple deltas into one logical frame,
        // since raw sequential `1..=N` was already strictly verified during reading.
        let mut a_no_seq = a.clone();
        let mut e_no_seq = e.clone();
        if let Some(m) = a_no_seq.as_object_mut() {
            m.remove("seq");
        }
        if let Some(m) = e_no_seq.as_object_mut() {
            m.remove("seq");
        }
        assert_matches_fixture(&a_no_seq, &e_no_seq, state, &format!("sse[{i}]"), opts);
    }

    // Verify no secret leaked in any SSE frame.
    let all_secs = state.all_proxy_tokens();
    for ev in &raw_events {
        let s = ev.to_string();
        assert!(!s.contains(&state.token), "SSE leaked kernel token: {s}");
        for sec in &all_secs {
            assert!(!s.contains(sec), "SSE leaked proxy token {sec}: {s}");
        }
    }
    raw_events
}

impl MatcherState {
    fn new_from_existing(_other: &Self) -> Self {
        Self::default()
    }
}

fn new_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}

#[tokio::test]
async fn fixture_01_handshake() {
    let fixture = load_fixture("handshake.json");
    let steps = fixture["steps"].as_array().unwrap();
    let proc = EmbeddedKernelProc::spawn().await;
    let mut state = MatcherState::new(&proc, 0);

    // Step 0: stdio handshake checked in spawn; validate against fixture stdout spec.
    let ready_obj = json!({
        "type": "ready",
        "protocol": "codaKernel/1",
        "version": proc.version,
        "pid": proc.pid,
        "port": proc.port,
    });
    assert_matches_fixture(
        &ready_obj,
        &steps[0]["stdio"]["stdout"],
        &mut state,
        "handshake.stdout",
        MatchOpts::default(),
    );

    // Steps 1..=5: HTTP capability & security checks via raw TCP for exact Host / Origin wire fidelity.
    for (idx, step) in steps[1..=5].iter().enumerate() {
        let req = &step["request"];
        let method = req["method"].as_str().unwrap();
        let path = req["path"].as_str().unwrap();
        let mut hdrs = Vec::new();
        for (k, v) in req["headers"].as_object().unwrap() {
            hdrs.push((k.clone(), state.substitute_str(v.as_str().unwrap())));
        }
        let (status, body) = send_raw_http_tcp(proc.port, method, path, &hdrs, None).await;
        assert_eq!(
            status,
            step["response"]["status"].as_u64().unwrap() as u16,
            "handshake step {}",
            idx + 1
        );
        assert_matches_fixture(
            &body,
            &step["response"]["body"],
            &mut state,
            &format!("handshake.step[{}]", idx + 1),
            MatchOpts::default(),
        );
    }

    // Extra security check: sec-fetch-site: cross-site -> 403 FORBIDDEN.
    let (cs_status, cs_body) = send_raw_http_tcp(
        proc.port,
        "GET",
        "/api/kernel/capabilities",
        &[
            ("host".into(), format!("127.0.0.1:{}", proc.port)),
            ("x-coda-token".into(), proc.token.clone()),
            ("sec-fetch-site".into(), "cross-site".into()),
        ],
        None,
    )
    .await;
    assert_eq!(cs_status, 403);
    assert_eq!(cs_body["error"]["code"], "FORBIDDEN");

    // Step 6: stdin EOF exit within 5000ms + no SQLite/lock files.
    let max_ms = steps[6]["stdio"]["expectExitWithinMs"].as_u64().unwrap();
    proc.close_stdin_and_wait_exit(max_ms, &[]).await;
}

#[tokio::test]
async fn fixture_02_hydrate_session() {
    let fixture = load_fixture("hydrateSession.json");
    let steps = fixture["steps"].as_array().unwrap();
    let proc = EmbeddedKernelProc::spawn().await;
    let client = new_http_client();
    let mut state = MatcherState::new(&proc, 0);

    for (idx, step) in steps.iter().enumerate() {
        let (status, body) =
            send_json_http(&client, &proc.base_url(), &step["request"], &mut state).await;
        assert_eq!(
            status,
            step["response"]["status"].as_u64().unwrap() as u16,
            "hydrateSession step {idx}: {body}"
        );
        if let Some(exp_body) = step["response"].get("body") {
            assert_matches_fixture(
                &body,
                exp_body,
                &mut state,
                &format!("hydrateSession.step[{idx}]"),
                MatchOpts::default(),
            );
        }
    }

    proc.close_stdin_and_wait_exit(5000, &[]).await;
}

#[tokio::test]
async fn fixture_03_run_text() {
    let hydrate_fixture = load_fixture("hydrateSession.json");
    let fixture = load_fixture("runText.json");
    let step = &fixture["steps"][0];

    let proc = EmbeddedKernelProc::spawn().await;
    let mock = MockUpstream::start().await;
    let client = new_http_client();
    let mut state = MatcherState::new(&proc, mock.port);

    // Pre-hydrate s-1 with `gates: []` as documented in runText.json description.
    let mut hydrate_req = hydrate_fixture["steps"][0]["request"].clone();
    hydrate_req["body"]["gates"] = json!([]);
    let (h_status, _) = send_json_http(&client, &proc.base_url(), &hydrate_req, &mut state).await;
    assert_eq!(h_status, 201);

    mock.enqueue(
        "run-1",
        sse_responses_text(
            "resp_run_text_1",
            &["用户要一句话自我介绍。"],
            Some("enc_sig_text_1"),
            &["我是 Coda，", "运行在你电脑上的智能助手。"],
            json!({
                "input_tokens": 1830,
                "input_tokens_details": { "cached_tokens": 1024 },
                "output_tokens": 42,
                "output_tokens_details": { "reasoning_tokens": 12 },
                "total_tokens": 1872
            }),
        ),
    );

    let raw_sse = run_sse_step(
        &client,
        &proc.base_url(),
        step,
        &mut state,
        MatchOpts {
            ignore_token_estimates: true,
            usage_openai_cached_math: true,
        },
    )
    .await;

    // Assertions: runtimeContext never in any entry; upstream received runtimeContext after last user item & filtered tools.
    for ev in &raw_sse {
        if ev["event"]["type"] == "entry" {
            let s = ev["event"]["entry"].to_string();
            assert!(
                !s.contains("Working directory:"),
                "runtimeContext leaked into entry: {s}"
            );
        }
    }
    let reqs = mock.recorded_for("/m/run-1/responses");
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    let expected_proxy_tok = state.proxy_token("<PROXY_TOKEN>");
    assert_eq!(
        reqs[0].authorization.as_deref(),
        Some(format!("Bearer {expected_proxy_tok}").as_str())
    );
    assert_eq!(reqs[0].body["model"], "gpt-5.2");
    assert_eq!(reqs[0].body["stream"], true);
    let tool_names: Vec<&str> = reqs[0].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tool_names, vec!["read", "shell", "loadTools"]);
    let input_str = reqs[0].body["input"].to_string();
    assert!(input_str.contains("Working directory:"));

    let secs = state.all_proxy_tokens();
    let sec_refs: Vec<&str> = secs.iter().map(String::as_str).collect();
    proc.close_stdin_and_wait_exit(5000, &sec_refs).await;
}

#[tokio::test]
async fn fixture_04_run_tool_batch() {
    let fixture = load_fixture("runToolBatch.json");
    let steps = fixture["steps"].as_array().unwrap();
    let proc = EmbeddedKernelProc::spawn().await;
    let mock = MockUpstream::start().await;
    let client = new_http_client();
    let mut state = MatcherState::new(&proc, mock.port);

    // Step 0: PUT s-batch
    let (s0, b0) =
        send_json_http(&client, &proc.base_url(), &steps[0]["request"], &mut state).await;
    assert_eq!(s0, 201);
    assert_matches_fixture(
        &b0,
        &steps[0]["response"]["body"],
        &mut state,
        "step0",
        MatchOpts::default(),
    );

    // Step 1: 3-turn run
    mock.enqueue(
        "run-batch-1",
        sse_responses_tool_calls(
            "resp_batch_1",
            None,
            &[
                ("call_read_1", "read", json!({ "path": "package.json" })),
                (
                    "call_load_1",
                    "loadTools",
                    json!({ "names": ["messageBot"] }),
                ),
                (
                    "call_msg_1",
                    "messageBot",
                    json!({ "to": "helper", "text": "准备同步" }),
                ),
            ],
            json!({ "input_tokens": 1200, "output_tokens": 68, "total_tokens": 1268 }),
        ),
    );
    mock.enqueue(
        "run-batch-1",
        sse_responses_tool_calls(
            "resp_batch_2",
            None,
            &[
                ("call_shell_2", "shell", json!({ "command": "bun test" })),
                (
                    "call_msg_2",
                    "messageBot",
                    json!({ "to": "helper", "text": "package.json 版本为 1.0.0" }),
                ),
            ],
            json!({
                "input_tokens": 1620,
                "input_tokens_details": { "cached_tokens": 1024 },
                "output_tokens": 54,
                "total_tokens": 1674
            }),
        ),
    );
    mock.enqueue(
        "run-batch-1",
        sse_responses_text(
            "resp_batch_3",
            &[],
            None,
            &["已读取 package.json（demo 1.0.0）并同步给 helper；测试在 src/math.test.ts 失败（expected 3, got 2）。"],
            json!({
                "input_tokens": 1940,
                "input_tokens_details": { "cached_tokens": 1536 },
                "output_tokens": 38,
                "total_tokens": 1978
            }),
        ),
    );

    run_sse_step(
        &client,
        &proc.base_url(),
        &steps[1],
        &mut state,
        MatchOpts {
            ignore_token_estimates: true,
            usage_openai_cached_math: true,
        },
    )
    .await;

    let reqs = mock.recorded_for("/m/run-batch-1/responses");
    assert_eq!(reqs.len(), 3);
    // Turn 1 tools omit messageBot
    let t1_tools: Vec<&str> = reqs[0].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(t1_tools, vec!["read", "shell", "loadTools"]);
    // Turn 2 tools include messageBot; input preserves batchIndex order (call_read_1, call_load_1, call_msg_1)
    let t2_tools: Vec<&str> = reqs[1].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(t2_tools, vec!["read", "shell", "loadTools", "messageBot"]);
    let t2_outputs: Vec<&str> = reqs[1].body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|it| it["type"] == "function_call_output")
        .map(|it| it["call_id"].as_str().unwrap())
        .collect();
    assert_eq!(t2_outputs, vec!["call_read_1", "call_load_1", "call_msg_1"]);
    // Turn 3 includes modelNote on call_shell_2 and strips details
    let t3_shell_out = reqs[2].body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it["type"] == "function_call_output" && it["call_id"] == "call_shell_2")
        .unwrap();
    let t3_shell_str = t3_shell_out["output"].as_str().unwrap();
    assert!(
        t3_shell_str.ends_with("\n\n[进度注记] 仅 1 个单测失败，请向用户汇总失败位置"),
        "missing modelNote in Turn 3 input: {t3_shell_str}"
    );
    let t3_str = reqs[2].body["input"].to_string();
    assert!(!t3_str.contains("exitCode") && !t3_str.contains("delivered"));

    // Step 2: DELETE
    let (s2, _) = send_json_http(&client, &proc.base_url(), &steps[2]["request"], &mut state).await;
    assert_eq!(s2, 204);

    let secs = state.all_proxy_tokens();
    let sec_refs: Vec<&str> = secs.iter().map(String::as_str).collect();
    proc.close_stdin_and_wait_exit(5000, &sec_refs).await;
}

#[tokio::test]
async fn fixture_05_run_steer_gate() {
    let fixture = load_fixture("runSteerGate.json");
    let steps = fixture["steps"].as_array().unwrap();
    let proc = EmbeddedKernelProc::spawn().await;
    let mock = MockUpstream::start().await;
    let client = new_http_client();
    let mut state = MatcherState::new(&proc, mock.port);

    let (s0, b0) =
        send_json_http(&client, &proc.base_url(), &steps[0]["request"], &mut state).await;
    assert_eq!(s0, 201);
    assert_matches_fixture(
        &b0,
        &steps[0]["response"]["body"],
        &mut state,
        "step0",
        MatchOpts::default(),
    );

    mock.enqueue(
        "run-steer-1",
        sse_responses_tool_calls(
            "resp_steer_1",
            None,
            &[("call_read_1", "read", json!({ "path": "src/api.ts" }))],
            json!({ "input_tokens": 1100, "output_tokens": 32, "total_tokens": 1132 }),
        ),
    );
    mock.enqueue(
        "run-steer-1",
        sse_responses_text(
            "resp_steer_2",
            &[],
            None,
            &["src/api.ts 中 ApiRes 定义完整，字段 ok 为 boolean。"],
            json!({
                "input_tokens": 1350,
                "input_tokens_details": { "cached_tokens": 1024 },
                "output_tokens": 26,
                "total_tokens": 1376
            }),
        ),
    );
    mock.enqueue(
        "run-steer-1",
        sse_responses_tool_calls(
            "resp_steer_3",
            None,
            &[("call_read_2", "read", json!({ "path": "src/api.ts" }))],
            json!({
                "input_tokens": 1520,
                "input_tokens_details": { "cached_tokens": 1280 },
                "output_tokens": 28,
                "total_tokens": 1548
            }),
        ),
    );

    run_sse_step(
        &client,
        &proc.base_url(),
        &steps[1],
        &mut state,
        MatchOpts {
            ignore_token_estimates: true,
            usage_openai_cached_math: true,
        },
    )
    .await;

    let reqs = mock.recorded_for("/m/run-steer-1/responses");
    assert_eq!(reqs.len(), 3);
    assert!(
        reqs[1].body["input"]
            .to_string()
            .contains("顺便检查下类型定义")
    );
    assert!(
        reqs[2].body["input"]
            .to_string()
            .contains("群成员补充：已更新接口")
    );

    let (s2, _) = send_json_http(&client, &proc.base_url(), &steps[2]["request"], &mut state).await;
    assert_eq!(s2, 204);

    let secs = state.all_proxy_tokens();
    let sec_refs: Vec<&str> = secs.iter().map(String::as_str).collect();
    proc.close_stdin_and_wait_exit(5000, &sec_refs).await;
}

#[tokio::test]
async fn fixture_06_run_abort() {
    let fixture = load_fixture("runAbort.json");
    let steps = fixture["steps"].as_array().unwrap();
    let proc = EmbeddedKernelProc::spawn().await;
    let mock = MockUpstream::start().await;
    let client = new_http_client();
    let mut state = MatcherState::new(&proc, mock.port);

    // Step 0: PUT s-abort
    let (s0, b0) =
        send_json_http(&client, &proc.base_url(), &steps[0]["request"], &mut state).await;
    assert_eq!(s0, 201);
    assert_matches_fixture(
        &b0,
        &steps[0]["response"]["body"],
        &mut state,
        "step0",
        MatchOpts::default(),
    );

    // Step 1: run-abort-1
    mock.enqueue(
        "run-abort-1",
        sse_responses_tool_calls(
            "resp_abort_1",
            Some("我先依次运行两个命令。"),
            &[
                ("call_1", "shell", json!({ "command": "echo step1 ok" })),
                ("call_2", "shell", json!({ "command": "sleep 60" })),
            ],
            json!({ "input_tokens": 950, "output_tokens": 46, "total_tokens": 996 }),
        ),
    );
    run_sse_step(
        &client,
        &proc.base_url(),
        &steps[1],
        &mut state,
        MatchOpts {
            ignore_token_estimates: true,
            usage_openai_cached_math: true,
        },
    )
    .await;

    // Step 2: GET s-abort
    let (s2, b2) =
        send_json_http(&client, &proc.base_url(), &steps[2]["request"], &mut state).await;
    assert_eq!(s2, 200);
    assert_matches_fixture(
        &b2,
        &steps[2]["response"]["body"],
        &mut state,
        "step2",
        MatchOpts::default(),
    );

    // Step 3: run-abort-2 with new modelProxy
    mock.enqueue(
        "run-abort-2",
        sse_responses_text(
            "resp_abort_2",
            &[],
            None,
            &["第一步命令已执行成功，输出为 step1 ok；第二步命令已按取消处理。"],
            json!({
                "input_tokens": 1120,
                "input_tokens_details": { "cached_tokens": 768 },
                "output_tokens": 24,
                "total_tokens": 1144
            }),
        ),
    );
    run_sse_step(
        &client,
        &proc.base_url(),
        &steps[3],
        &mut state,
        MatchOpts {
            ignore_token_estimates: true,
            usage_openai_cached_math: true,
        },
    )
    .await;

    let reqs2 = mock.recorded_for("/m/run-abort-2/responses");
    assert_eq!(reqs2.len(), 1);
    let tok2 = state.proxy_token("<PROXY_TOKEN_2>");
    assert_eq!(
        reqs2[0].authorization.as_deref(),
        Some(format!("Bearer {tok2}").as_str())
    );
    let outputs: Vec<(&str, &str)> = reqs2[0].body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|it| it["type"] == "function_call_output")
        .map(|it| {
            (
                it["call_id"].as_str().unwrap(),
                it["output"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(outputs, vec![("call_1", "step1 ok"), ("call_2", "aborted")]);

    // Step 4: DELETE s-abort
    let (s4, _) = send_json_http(&client, &proc.base_url(), &steps[4]["request"], &mut state).await;
    assert_eq!(s4, 204);

    let secs = state.all_proxy_tokens();
    let sec_refs: Vec<&str> = secs.iter().map(String::as_str).collect();
    proc.close_stdin_and_wait_exit(5000, &sec_refs).await;
}

#[tokio::test]
async fn fixture_07_compact() {
    let fixture = load_fixture("compact.json");
    let steps = fixture["steps"].as_array().unwrap();
    let proc = EmbeddedKernelProc::spawn().await;
    let mock = MockUpstream::start().await;
    let client = new_http_client();
    let mut state = MatcherState::new(&proc, mock.port);

    // Step 0: PUT s-compact (verbatim)
    let (s0, b0) =
        send_json_http(&client, &proc.base_url(), &steps[0]["request"], &mut state).await;
    assert_eq!(s0, 201);
    assert_matches_fixture(
        &b0,
        &steps[0]["response"]["body"],
        &mut state,
        "compact.step0",
        MatchOpts::default(),
    );

    // Verify 502 COMPACTION_EMPTY_SUMMARY does not mutate revision or history.
    mock.enqueue(
        "compact-empty",
        sse_responses_text(
            "resp_cmp_empty",
            &[],
            None,
            &["   "],
            json!({ "input_tokens": 100, "output_tokens": 1, "total_tokens": 101 }),
        ),
    );
    let empty_req = json!({
        "method": "POST",
        "path": "/api/kernel/sessions/s-compact/compact",
        "headers": { "x-coda-token": "<TOKEN>" },
        "body": {
            "baseRevision": 1,
            "keepRecentTokens": 0,
            "modelProxy": {
                "baseUrl": "http://127.0.0.1:61234/m/compact-empty",
                "token": "<PROXY_TOKEN_1>",
                "protocol": "openai-responses"
            }
        }
    });
    let (se, be) = send_json_http(&client, &proc.base_url(), &empty_req, &mut state).await;
    assert_eq!(se, 502);
    assert_eq!(be["error"]["code"], "COMPACTION_EMPTY_SUMMARY");

    // Step 1: Manual POST /compact (verbatim request)
    mock.enqueue(
        "compact-1",
        sse_responses_text(
            "resp_cmp_1",
            &[],
            None,
            &["已检查 src/config.ts，确认默认 PORT = 8080。"],
            json!({ "input_tokens": 640, "output_tokens": 28, "total_tokens": 668 }),
        ),
    );
    let (s1, b1) =
        send_json_http(&client, &proc.base_url(), &steps[1]["request"], &mut state).await;
    assert_eq!(s1, 200);
    assert_eq!(b1["revision"], 2);
    assert_eq!(b1["entry"]["type"], "compaction");
    assert_eq!(
        b1["entry"]["summary"],
        "已检查 src/config.ts，确认默认 PORT = 8080。"
    );
    assert_eq!(
        b1["result"]["summary"],
        "已检查 src/config.ts，确认默认 PORT = 8080。"
    );
    assert!(b1["result"]["usage"].is_object());
    let cmp1_id = b1["entry"]["id"].as_str().unwrap().to_owned();
    state.bindings.insert("<ENTRY_ID:cmp1>".into(), cmp1_id);

    let cmp_reqs = mock.recorded_for("/m/compact-1/responses");
    assert_eq!(cmp_reqs.len(), 1);
    assert_eq!(
        cmp_reqs[0].body["instructions"],
        "你是会话摘要生成器，请输出结构化检查点。"
    );
    assert!(
        cmp_reqs[0].body["input"]
            .to_string()
            .contains("保留所有文件路径与关键配置常量")
    );

    // Also verify §8.3 exclusions + atomic (e-0005+e-0006) cut point at e-0009 using calibrated keepRecentTokens: 14
    // on a copy of Step 0's history (e-0009 + e-0010 is ~25 chars/4 = ~7..15 tokens, keeping e-0009..e-0010).
    let mut s_excl_req = steps[0]["request"].clone();
    s_excl_req["path"] = json!("/api/kernel/sessions/s-compact-excl");
    let (sx, _) = send_json_http(&client, &proc.base_url(), &s_excl_req, &mut state).await;
    assert_eq!(sx, 201);
    mock.enqueue(
        "compact-excl",
        sse_responses_text(
            "resp_cmp_excl",
            &[],
            None,
            &["已检查 src/config.ts，确认默认 PORT = 8080。"],
            json!({ "input_tokens": 640, "output_tokens": 28, "total_tokens": 668 }),
        ),
    );
    let mut excl_cmp = steps[1]["request"].clone();
    excl_cmp["path"] = json!("/api/kernel/sessions/s-compact-excl/compact");
    excl_cmp["body"]["keepRecentTokens"] = json!(18);
    excl_cmp["body"]["modelProxy"]["baseUrl"] = json!("http://127.0.0.1:61234/m/compact-excl");
    let (sxc, bxc) = send_json_http(&client, &proc.base_url(), &excl_cmp, &mut state).await;
    assert_eq!(sxc, 200);
    assert_eq!(
        bxc["result"]["firstKeptEntryId"], "e-0009",
        "must cut at e-0009 without splitting e-0005+e-0006"
    );
    let excl_reqs = mock.recorded_for("/m/compact-excl/responses");
    let excl_input = excl_reqs[0].body["input"].to_string();
    assert!(
        !excl_input.contains("旧的临时试探提问"),
        "contextEdit(null) e-0001 leaked"
    );
    assert!(
        !excl_input.contains("Working directory:"),
        "codaRuntimeContext e-0003 leaked"
    );
    assert!(
        !excl_input.contains("UI 提示：仅展示给用户"),
        "includeInSummary:false e-0008 leaked"
    );
    assert!(
        excl_input.contains("call_cfg_1"),
        "atomic toolCall e-0005+e-0006 missing"
    );
    assert!(
        excl_input.contains("export const PORT = 8080;"),
        "atomic toolResult e-0006 missing"
    );

    // Step 2: PATCH s-compact (verbatim)
    let (s2, b2) =
        send_json_http(&client, &proc.base_url(), &steps[2]["request"], &mut state).await;
    assert_eq!(s2, 200);
    assert_matches_fixture(
        &b2,
        &steps[2]["response"]["body"],
        &mut state,
        "compact.step2",
        MatchOpts::default(),
    );

    // Step 3: verbatim run-compact-auto (short strings < chars/4 threshold -> runs directly)
    mock.enqueue(
        "run-compact-auto",
        sse_responses_text(
            "resp_auto_verbatim",
            &[],
            None,
            &["export const PORT = Number(process.env.PORT ?? 8080);"],
            json!({ "input_tokens": 320, "output_tokens": 18, "total_tokens": 338 }),
        ),
    );
    let mut verbatim_run = steps[3].clone();
    verbatim_run["interactions"] = json!([]);
    let verbatim_run_req = state.substitute_value(&verbatim_run["request"]["body"]);
    let r_v = client
        .post(format!(
            "{}/api/kernel/sessions/s-compact/runs",
            proc.base_url()
        ))
        .header("x-coda-token", &proc.token)
        .json(&verbatim_run_req)
        .send()
        .await
        .unwrap();
    assert_eq!(r_v.status().as_u16(), 200);
    let mut dummy_state = MatcherState::new(&proc, mock.port);
    let shared_v = Arc::new(Mutex::new(std::mem::replace(
        &mut state,
        std::mem::take(&mut dummy_state),
    )));
    let (v_raw, _) =
        read_sse_stream_with_interactions(r_v, 1, &[], &client, &proc.base_url(), &shared_v).await;
    state = Arc::try_unwrap(shared_v)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap();
    assert_eq!(v_raw.last().unwrap()["event"]["type"], "done");
    assert_eq!(v_raw.last().unwrap()["event"]["reason"], "completed");

    // Step 4: DELETE s-compact (verbatim)
    let (s4, _) = send_json_http(&client, &proc.base_url(), &steps[4]["request"], &mut state).await;
    assert_eq!(s4, 204);

    // Genuine end-to-end `run-compact-auto` `beforeCompact` `{ action: "replace" }` test
    // with history sized to exceed the `chars/4` `contextWindow: 2048` threshold (> 1332 tokens)
    // and `u2` > `keepRecentTokens: 60` (~280 chars = ~70 tokens).
    mock.clear_recorded();
    let pad_old = "A".repeat(3000); // ~750 tokens
    let pad_old2 = "B".repeat(3000); // ~750 tokens -> e-0009 + e-0010 = ~1500 tokens > 1332 threshold
    let u2_text = format!("请给出修改后的完整代码 {}", "C".repeat(280)); // ~75 tokens > keepRecentTokens: 60
    let mut auto_hydrate = json!({
        "method": "PUT",
        "path": "/api/kernel/sessions/s-compact-auto-e2e",
        "headers": { "x-coda-token": "<TOKEN>" },
        "body": {
            "cwd": "C:/Users/me/.coda/cd-1f2e",
            "systemPrompt": "你是 Coda。",
            "model": { "modelId": "gpt-5.2", "protocol": "openai-responses", "contextWindow": 2048, "maxOutputTokens": 512, "thinkingLevel": "medium" },
            "tools": [
                { "name": "read", "description": "读取工作区文件", "parameters": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }, "label": "读取文件", "executionMode": "parallel" }
            ],
            "activeToolNames": ["read"],
            "compaction": { "enabled": true, "keepRecentTokens": 60 },
            "steeringMode": "one-at-a-time",
            "gates": ["beforeCompact"],
            "history": {
                "leafId": "e-0010",
                "entries": [
                    { "id": "e-0009", "parentId": null, "timestamp": "2026-11-27T01:00:08+08:00", "type": "message", "role": "user", "content": [{ "type": "text", "text": pad_old }] },
                    { "id": "e-0010", "parentId": "e-0009", "timestamp": "2026-11-27T01:00:09+08:00", "type": "message", "role": "assistant", "content": [{ "type": "text", "text": pad_old2 }], "metadata": { "stopReason": "stop" } }
                ]
            }
        }
    });
    let (sa, _) = send_json_http(&client, &proc.base_url(), &auto_hydrate, &mut state).await;
    assert_eq!(sa, 201);
    auto_hydrate["body"]["history"]["entries"] = json!([]);

    let mut auto_step = steps[3].clone();
    auto_step["request"]["path"] = json!("/api/kernel/sessions/s-compact-auto-e2e/runs");
    auto_step["request"]["body"]["baseRevision"] = json!(1);
    auto_step["request"]["body"]["content"] = json!([{ "type": "text", "text": u2_text }]);
    auto_step["interactions"][0]["request"]["path"] = json!(
        "/api/kernel/sessions/s-compact-auto-e2e/runs/run-compact-auto/gates/<GATE_ID:gc_auto>"
    );

    // Adjust expected SSE for baseRevision=1 (`u2` rev 2, `cmp2` rev 3, `a2` rev 4, `usage2` rev 5)
    // and matching `<ENTRY_ID:*>` names so it validates every single event strictly!
    let mut expected_sse = auto_step["response"]["sse"].as_array().unwrap().clone();
    for ev in &mut expected_sse {
        ev["sessionId"] = json!("s-compact-auto-e2e");
    }
    expected_sse[1]["event"]["revision"] = json!(2);
    expected_sse[1]["event"]["entry"]["id"] = json!("<ENTRY_ID:u2_auto>");
    expected_sse[1]["event"]["entry"]["parentId"] = json!("e-0010");
    expected_sse[1]["event"]["entry"]["content"][0]["text"] = json!(u2_text);
    expected_sse[2]["event"]["entryId"] = json!("<ENTRY_ID:u2_auto>");
    expected_sse[5]["event"]["gateId"] = json!("<GATE_ID:gc_auto>");
    expected_sse[5]["event"]["preparation"]["firstKeptEntryId"] = json!("<ENTRY_ID:u2_auto>");
    expected_sse[7]["event"]["revision"] = json!(3);
    expected_sse[7]["event"]["entry"]["id"] = json!("<ENTRY_ID:cmp2_auto>");
    expected_sse[7]["event"]["entry"]["parentId"] = json!("<ENTRY_ID:u2_auto>");
    expected_sse[7]["event"]["entry"]["firstKeptEntryId"] = json!("<ENTRY_ID:u2_auto>");
    expected_sse[8]["event"]["entryId"] = json!("<ENTRY_ID:cmp2_auto>");
    expected_sse[8]["event"]["firstKeptEntryId"] = json!("<ENTRY_ID:u2_auto>");
    expected_sse[14]["event"]["revision"] = json!(4);
    expected_sse[14]["event"]["entry"]["id"] = json!("<ENTRY_ID:a2_auto>");
    expected_sse[14]["event"]["entry"]["parentId"] = json!("<ENTRY_ID:cmp2_auto>");
    expected_sse[15]["event"]["revision"] = json!(5);
    expected_sse[15]["event"]["entry"]["id"] = json!("<ENTRY_ID:usage2_auto>");
    expected_sse[15]["event"]["entry"]["parentId"] = json!("<ENTRY_ID:a2_auto>");
    expected_sse[16]["event"]["leafId"] = json!("<ENTRY_ID:usage2_auto>");
    expected_sse[16]["event"]["userEntryId"] = json!("<ENTRY_ID:u2_auto>");
    expected_sse[16]["event"]["assistantEntryId"] = json!("<ENTRY_ID:a2_auto>");
    expected_sse[16]["event"]["revision"] = json!(5);
    expected_sse[11]["event"]["entryId"] = json!("<ENTRY_ID:a2_auto>");
    auto_step["response"]["sse"] = Value::Array(expected_sse);

    mock.enqueue(
        "run-compact-auto",
        sse_responses_text(
            "resp_auto_e2e",
            &[],
            None,
            &["export const PORT = Number(process.env.PORT ?? 8080);"],
            json!({ "input_tokens": 320, "output_tokens": 18, "total_tokens": 338 }),
        ),
    );
    run_sse_step(
        &client,
        &proc.base_url(),
        &auto_step,
        &mut state,
        MatchOpts {
            ignore_token_estimates: true,
            usage_openai_cached_math: true,
        },
    )
    .await;

    let auto_reqs = mock.recorded_for("/m/run-compact-auto/responses");
    assert_eq!(
        auto_reqs.len(),
        1,
        "beforeCompact replace MUST NOT call summary model (expected 1 upstream call)"
    );
    assert!(auto_reqs[0].body["input"].to_string().contains(
        "【宿主接管摘要】已完成 src/config.ts 端口检查（8080），准备改为读取 process.env.PORT。"
    ));

    let secs = state.all_proxy_tokens();
    let sec_refs: Vec<&str> = secs.iter().map(String::as_str).collect();
    proc.close_stdin_and_wait_exit(5000, &sec_refs).await;
}

#[tokio::test]
async fn fixture_08_errors() {
    let fixture = load_fixture("errors.json");
    let steps = fixture["steps"].as_array().unwrap();
    let proc = EmbeddedKernelProc::spawn().await;
    let mock = MockUpstream::start().await;
    let client = new_http_client();
    let mut state = MatcherState::new(&proc, mock.port);

    for (idx, step) in steps.iter().enumerate() {
        if step["response"].get("sse").is_some() {
            let run_id = step["request"]["body"]["runId"].as_str().unwrap();
            match run_id {
                "run-err-1" => {
                    mock.enqueue(
                        "run-err-1",
                        sse_responses_tool_calls(
                            "resp_err_1a",
                            None,
                            &[("call_err_1", "read", json!({ "path": "README.md" }))],
                            json!({ "input_tokens": 1050, "output_tokens": 28, "total_tokens": 1078 }),
                        ),
                    );
                    mock.enqueue(
                        "run-err-1",
                        sse_responses_text(
                            "resp_err_1b",
                            &[],
                            None,
                            &["已读取 README.md。"],
                            json!({
                                "input_tokens": 1180,
                                "input_tokens_details": { "cached_tokens": 768 },
                                "output_tokens": 16,
                                "total_tokens": 1196
                            }),
                        ),
                    );
                }
                "run-err-proxy-409" => {
                    mock.enqueue(
                        "run-err-proxy-409",
                        MockReply::ErrorJson {
                            status: 409,
                            body: step["expectUpstream"][0]["mockResponse"]["body"].clone(),
                        },
                    );
                }
                "run-err-proxy-401" => {
                    mock.enqueue(
                        "run-err-proxy-401",
                        MockReply::ErrorJson {
                            status: 401,
                            body: step["expectUpstream"][0]["mockResponse"]["body"].clone(),
                        },
                    );
                }
                other => panic!("unexpected SSE run {other}"),
            }
            run_sse_step(
                &client,
                &proc.base_url(),
                step,
                &mut state,
                MatchOpts {
                    ignore_token_estimates: true,
                    usage_openai_cached_math: true,
                },
            )
            .await;

            if run_id == "run-err-proxy-409" {
                assert_eq!(mock.recorded_for("/m/run-err-proxy-409/responses").len(), 1);
            } else if run_id == "run-err-proxy-401" {
                assert_eq!(mock.recorded_for("/m/run-err-proxy-401/responses").len(), 1);
            }
        } else {
            let (status, body) =
                send_json_http(&client, &proc.base_url(), &step["request"], &mut state).await;
            assert_eq!(
                status,
                step["response"]["status"].as_u64().unwrap() as u16,
                "errors step {idx} ({:?}): {body}",
                step["note"]
            );
            if let Some(exp_body) = step["response"].get("body") {
                assert_matches_fixture(
                    &body,
                    exp_body,
                    &mut state,
                    &format!("errors.step[{idx}]"),
                    MatchOpts::default(),
                );
            }
        }

        // Before DELETE (step 13), verify unknown runId -> 404 RUN_NOT_FOUND (distinct from 410 EVENTS_EXPIRED).
        if idx == 12 {
            let r404 = client
                .get(format!(
                    "{}/api/kernel/sessions/s-err/runs/run-never/events?after=0",
                    proc.base_url()
                ))
                .header("x-coda-token", &proc.token)
                .send()
                .await
                .unwrap();
            assert_eq!(r404.status().as_u16(), 404);
            let b404: Value = r404.json().await.unwrap();
            assert_eq!(b404["error"]["code"], "RUN_NOT_FOUND");
        }
    }

    let secs = state.all_proxy_tokens();
    let sec_refs: Vec<&str> = secs.iter().map(String::as_str).collect();
    proc.close_stdin_and_wait_exit(5000, &sec_refs).await;
}

#[tokio::test]
async fn extra_finish_route_and_strict_run_model_rejection() {
    let proc = EmbeddedKernelProc::spawn().await;
    let mock = MockUpstream::start().await;
    let client = new_http_client();
    let mut state = MatcherState::new(&proc, mock.port);

    // Hydrate session with beforeModelTurn gate.
    let h = json!({
        "method": "PUT",
        "path": "/api/kernel/sessions/s-extra",
        "headers": { "x-coda-token": "<TOKEN>" },
        "body": {
            "cwd": ".",
            "systemPrompt": "sys",
            "model": { "modelId": "gpt-5.2", "protocol": "openai-responses" },
            "tools": [{ "name": "read", "description": "read", "parameters": { "type": "object" } }],
            "activeToolNames": ["read"],
            "gates": ["beforeModelTurn"],
            "history": { "leafId": null, "entries": [] }
        }
    });
    let (sh, _) = send_json_http(&client, &proc.base_url(), &h, &mut state).await;
    assert_eq!(sh, 201);

    // Verify `customProviderId` is strictly rejected on POST /runs and POST /compact.
    let bad_run = json!({
        "method": "POST",
        "path": "/api/kernel/sessions/s-extra/runs",
        "headers": { "x-coda-token": "<TOKEN>" },
        "body": {
            "runId": "run-bad-model",
            "baseRevision": 1,
            "model": { "modelId": "gpt-5.2", "customProviderId": "forbidden" },
            "content": [{ "type": "text", "text": "hi" }],
            "modelProxy": { "baseUrl": "http://127.0.0.1:61234/m/unused", "token": "<PROXY_TOKEN>", "protocol": "openai-responses" }
        }
    });
    let (sb1, bb1) = send_json_http(&client, &proc.base_url(), &bad_run, &mut state).await;
    assert_eq!(sb1, 400);
    assert_eq!(bb1["error"]["code"], "INVALID_REQUEST");

    let bad_compact = json!({
        "method": "POST",
        "path": "/api/kernel/sessions/s-extra/compact",
        "headers": { "x-coda-token": "<TOKEN>" },
        "body": {
            "baseRevision": 1,
            "model": { "modelId": "gpt-5.2", "customProviderId": "forbidden" },
            "modelProxy": { "baseUrl": "http://127.0.0.1:61234/m/unused", "token": "<PROXY_TOKEN>", "protocol": "openai-responses" }
        }
    });
    let (sb2, bb2) = send_json_http(&client, &proc.base_url(), &bad_compact, &mut state).await;
    assert_eq!(sb2, 400);
    assert_eq!(bb2["error"]["code"], "INVALID_REQUEST");

    // Exercise POST /runs/{runId}/finish + gate continue -> done(completed) with 0 model calls.
    let run_req = json!({
        "runId": "run-finish-1",
        "baseRevision": 1,
        "model": { "modelId": "gpt-5.2" },
        "content": [{ "type": "text", "text": "stop cleanly" }],
        "modelProxy": {
            "baseUrl": format!("http://127.0.0.1:{}/m/run-finish-1", mock.port),
            "token": state.proxy_token("<PROXY_TOKEN>"),
            "protocol": "openai-responses"
        }
    });
    let sse_resp = client
        .post(format!(
            "{}/api/kernel/sessions/s-extra/runs",
            proc.base_url()
        ))
        .header("x-coda-token", &proc.token)
        .json(&run_req)
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status().as_u16(), 200);

    let mut finish_step = json!({
        "interactions": [
            {
                "afterSeq": 5,
                "request": {
                    "method": "POST",
                    "path": "/api/kernel/sessions/s-extra/runs/run-finish-1/finish",
                    "headers": { "x-coda-token": "<TOKEN>" }
                },
                "response": { "status": 202 }
            },
            {
                "afterSeq": 5,
                "request": {
                    "method": "POST",
                    "path": "/api/kernel/sessions/s-extra/runs/run-finish-1/gates/<GATE_ID:gf1>",
                    "headers": { "x-coda-token": "<TOKEN>" },
                    "body": { "action": "continue" }
                },
                "response": { "status": 200, "body": { "accepted": true } }
            }
        ]
    });
    let interactions = finish_step["interactions"].as_array_mut().unwrap().clone();
    let mut dummy_state = MatcherState::new(&proc, mock.port);
    let shared = Arc::new(Mutex::new(std::mem::replace(
        &mut state,
        std::mem::take(&mut dummy_state),
    )));
    let (raw, _) = read_sse_stream_with_interactions(
        sse_resp,
        1,
        &interactions,
        &client,
        &proc.base_url(),
        &shared,
    )
    .await;
    state = Arc::try_unwrap(shared).ok().unwrap().into_inner().unwrap();
    assert_eq!(raw.last().unwrap()["event"]["type"], "done");
    assert_eq!(raw.last().unwrap()["event"]["reason"], "completed");
    assert_eq!(mock.recorded_for("/m/run-finish-1/responses").len(), 0);

    let secs = state.all_proxy_tokens();
    let sec_refs: Vec<&str> = secs.iter().map(String::as_str).collect();
    proc.close_stdin_and_wait_exit(5000, &sec_refs).await;
}
