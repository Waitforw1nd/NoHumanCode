//! I08 transport tests: real CLI processes, adversarial local TCP responses.
//! This response fixture is not a Host/Store or first-use acceptance substitute.
use serde_json::{Value, json};
use std::{
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "synthetic-cli-bootstrap-boundary";
const SECRET: &str = "synthetic-cli-input-credential";
const BIN: &str = env!("CARGO_BIN_EXE_peachsh");
const SET: &[&str] = &[
    "host",
    "route",
    "set",
    "local",
    "--expected-revision",
    "0",
    "--name",
    "Local",
    "--base-url",
    "https://example.invalid/v1",
    "--model",
    "mock-model",
    "--max-tokens",
    "128",
    "--parallel-limit",
    "1",
    "--key",
    "original-operation",
];
struct Record {
    method: String,
    path: String,
    body: Value,
    authenticated: bool,
    original_key: bool,
}
struct Fixture {
    port: u16,
    requests: Arc<Mutex<Vec<Record>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn wire(status: &str, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).into_bytes()
}
fn route() -> Value {
    json!({"id":"local","name":"Local","base_url":"https://example.invalid:443/v1","model":"mock-model","max_tokens":128,"parallel_limit":1,"key_env":null,"credential_source":"stored"})
}
fn result(replayed: bool) -> Value {
    json!({"receipt":{"operation_id":"e69d52f6-bb7f-4854-b404-e525204a9092","route_id":"local","revision":1,"route":route(),"changed":true,"committed_at":1},"replayed":replayed})
}
impl Fixture {
    async fn new(reply: Option<Vec<u8>>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (header_end, length) = loop {
                    let mut chunk = [0; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break (0, 0);
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let header = std::str::from_utf8(&bytes[..end]).unwrap();
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                    .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                    assert!(bytes.len() < 65536, "bounded headers");
                };
                if header_end == 0 {
                    continue;
                }
                while bytes.len() < header_end + length {
                    let mut chunk = [0; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert_ne!(n, 0, "full request body arrives before response failure");
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let header = std::str::from_utf8(&bytes[..header_end]).unwrap();
                let mut first = header.lines().next().unwrap().split_whitespace();
                let method = first.next().unwrap().to_owned();
                let path = first.next().unwrap().to_owned();
                if path == "/api/bootstrap" {
                    socket
                        .write_all(&wire("200 OK", &json!({"token":TOKEN}).to_string()))
                        .await
                        .unwrap();
                } else {
                    let body = if length == 0 {
                        Value::Null
                    } else {
                        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
                    };
                    let matches_header = |name: &str, expected: &str| {
                        header.lines().any(|line| {
                            line.split_once(':').is_some_and(|(key, v)| {
                                key.eq_ignore_ascii_case(name) && v.trim() == expected
                            })
                        })
                    };
                    recorded.lock().unwrap().push(Record {
                        method,
                        path,
                        body,
                        authenticated: matches_header("x-peachsh-token", TOKEN),
                        original_key: matches_header("idempotency-key", "original-operation"),
                    });
                    if let Some(reply) = &reply {
                        let _ = socket.write_all(reply).await;
                    }
                }
                let _ = socket.shutdown().await;
            }
        });
        Self {
            port,
            requests,
            task,
        }
    }
    async fn cli(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        json: bool,
        break_stdout: bool,
    ) -> Output {
        let mut command = tokio::process::Command::new(BIN);
        command
            .arg("--port")
            .arg(self.port.to_string())
            .args(if json { vec!["--json"] } else { vec![] })
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env("NO_PROXY", "")
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        if break_stdout {
            drop(child.stdout.take());
        }
        if let Some(input) = input {
            let mut stdin = child.stdin.take().unwrap();
            let _ = stdin.write_all(input).await;
            drop(stdin);
        }
        tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
            .await
            .unwrap()
            .unwrap()
    }
    fn one_put(&self) {
        let requests = self.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "one request only; no retry/lookup/alternate key"
        );
        let request = &requests[0];
        assert_eq!(request.method, "PUT");
        assert_eq!(request.path, "/api/host/routes/local");
        assert!(request.authenticated && request.original_key);
    }
}
fn secret_free(output: &Output) {
    for stream in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(stream);
        assert!(!text.contains(SECRET), "credential must never be output");
        assert!(
            !text.contains(TOKEN),
            "bootstrap token must never be output"
        );
    }
}
fn failed(output: Output, code: &str) {
    secret_free(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], code);
    assert_eq!(error["retryable"], false);
}
fn set(mode: &str) -> Vec<&str> {
    let mut args = SET.to_vec();
    args.push(mode);
    args
}

#[tokio::test]
async fn i08_replace_stdin_and_keep_send_controlled_bodies_and_safe_output() {
    for json in [true, false] {
        for keep in [true, false] {
            let host = Fixture::new(Some(wire("200 OK", &result(false).to_string()))).await;
            let input = format!("{SECRET}\r\n");
            let output = host
                .cli(
                    &set(if keep {
                        "--keep-credential"
                    } else {
                        "--credential-stdin"
                    }),
                    if keep { None } else { Some(input.as_bytes()) },
                    json,
                    false,
                )
                .await;
            secret_free(&output);
            assert_eq!(output.status.code(), Some(0));
            assert!(output.stderr.is_empty());
            host.one_put();
            let records = host.requests.lock().unwrap();
            let request = &records[0].body;
            assert_eq!(request["expected_revision"], 0);
            assert_eq!(
                request["route"]["base_url"],
                "https://example.invalid:443/v1"
            );
            if keep {
                assert_eq!(request["credential"], json!({"mode":"keep"}));
            } else {
                assert!(request["credential"]["value"] == SECRET);
                assert_eq!(request["credential"]["mode"], "replace");
            }
            if json {
                assert_eq!(
                    serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                    result(false)
                );
            } else {
                assert!(
                    String::from_utf8(output.stdout)
                        .unwrap()
                        .contains("未授权联网")
                );
            }
        }
    }
}

#[tokio::test]
async fn i08_complete_put_then_disconnect_truncate_redirect_oversize_and_malicious_success_are_unknown()
 {
    let valid = result(false).to_string();
    let mut replies=vec![None,Some(wire("200 OK","{")),
        Some(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{valid}",valid.len()+20).into_bytes()),
        Some(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /api/host/routes/local\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()),
        Some(wire("200 OK",&" ".repeat(1_048_577))),
        Some(wire("201 Created",&valid)),
        Some(wire("200 OK",&valid.replacen("\"replayed\":false","\"replayed\":false,\"replayed\":true",1))),
        Some(wire("200 OK",&valid.replacen("\"name\":\"Local\"","\"name\":\"Local\",\"name\":\"Local\"",1)))];
    for (pointer, value) in [
        ("/receipt/route_id", json!("other")),
        ("/receipt/route/id", json!("other")),
        ("/receipt/revision", json!(2)),
        ("/receipt/revision", json!(1.0)),
        ("/receipt/operation_id", json!("not-a-uuid")),
        ("/receipt/committed_at", json!(null)),
        (
            "/receipt/route/base_url",
            json!("https://different.invalid:443/v1"),
        ),
        ("/receipt/route/model", json!("other-model")),
        ("/receipt/route/key_env", json!("ENV_KEY")),
        ("/receipt/route/credential_source", json!("missing")),
        ("/receipt/route/name", json!(SECRET)),
        ("/receipt/route/name", json!(TOKEN)),
    ] {
        let mut body = result(false);
        *body.pointer_mut(pointer).unwrap() = value;
        replies.push(Some(wire("200 OK", &body.to_string())));
    }
    for pointer in ["", "/receipt", "/receipt/route"] {
        let mut body = result(false);
        body.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), json!("bad"));
        replies.push(Some(wire("200 OK", &body.to_string())));
    }
    for reply in replies {
        let host = Fixture::new(reply).await;
        failed(
            host.cli(
                &set("--credential-stdin"),
                Some(SECRET.as_bytes()),
                true,
                false,
            )
            .await,
            "operation_result_unknown",
        );
        host.one_put();
    }
    let host = Fixture::new(Some(wire("200 OK", &valid))).await;
    failed(
        host.cli(
            &set("--credential-stdin"),
            Some(SECRET.as_bytes()),
            true,
            true,
        )
        .await,
        "operation_result_unknown",
    );
    host.one_put();
}

#[tokio::test]
async fn i08_invalid_secret_or_arguments_never_send_put_and_never_echo_inputs() {
    let host = Fixture::new(None).await;
    for input in [
        Vec::new(),
        b"a\nb\n".to_vec(),
        b" a".to_vec(),
        b"a ".to_vec(),
        b"a\0".to_vec(),
        vec![0xff],
        vec![b'a'; 4096],
    ] {
        let output = host
            .cli(&set("--credential-stdin"), Some(&input), true, false)
            .await;
        secret_free(&output);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let failure: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(matches!(
            failure["code"].as_str(),
            Some("credential_invalid" | "credential_cancelled")
        ));
        assert!(host.requests.lock().unwrap().is_empty());
    }
    for extra in [
        vec![],
        vec!["--credential-stdin", "--keep-credential"],
        vec!["--credential-stdin", "--api-key", SECRET],
        vec!["--credential-stdin", SECRET],
    ] {
        let mut args = SET.to_vec();
        args.extend(extra);
        let output = host.cli(&args, None, true, false).await;
        secret_free(&output);
        assert_eq!(output.status.code(), Some(2));
        assert!(host.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn i08_trusted_errors_remain_errors_and_untrusted_errors_are_unknown() {
    for (status, code) in [
        (400, "host_config_invalid"),
        (403, "forbidden"),
        (409, "host_config_revision_conflict"),
        (409, "host_config_idempotency_conflict"),
        (409, "host_config_busy"),
        (409, "host_config_credential_required"),
        (500, "host_config_corrupt"),
        (500, "host_config_unavailable"),
        (500, "host_config_internal"),
    ] {
        let body =
            json!({"error":{"code":code,"message":"fixed safe server error","retryable":false}});
        let host = Fixture::new(Some(wire(&format!("{status} Error"), &body.to_string()))).await;
        failed(
            host.cli(&set("--keep-credential"), None, true, false).await,
            code,
        );
        host.one_put();
    }
    for body in [
        json!({"error":{"code":"host_config_busy","message":SECRET,"retryable":false}}),
        json!({"error":{"code":"host_config_busy","message":"safe","retryable":true}}),
        json!({"error":{"code":"host_config_busy","message":"safe","retryable":false,"calls":[]}}),
    ] {
        let host = Fixture::new(Some(wire("409 Conflict", &body.to_string()))).await;
        failed(
            host.cli(
                &set("--credential-stdin"),
                Some(SECRET.as_bytes()),
                true,
                false,
            )
            .await,
            "operation_result_unknown",
        );
        host.one_put();
    }
}

#[tokio::test]
async fn i08_lookup_is_read_only_original_receipt_and_404_preserves_inflight_uncertainty() {
    let host = Fixture::new(Some(wire("200 OK", &result(true).to_string()))).await;
    let output = host
        .cli(
            &["host", "config", "operation", "--key", "original-operation"],
            None,
            true,
            false,
        )
        .await;
    secret_free(&output);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        result(true)
    );
    {
        let requests = host.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/host/config/lookup");
        assert_eq!(requests[0].body, json!({"key":"original-operation"}));
        assert!(requests[0].authenticated);
    }
    let body =
        json!({"error":{"code":"host_config_not_found","message":"not found","retryable":false}});
    let host = Fixture::new(Some(wire("404 Not Found", &body.to_string()))).await;
    let output = host
        .cli(
            &["host", "config", "operation", "--key", "original-operation"],
            None,
            true,
            false,
        )
        .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("仍在途") && stderr.contains("不要因此换key"));
    failed(output, "host_config_not_found");
    assert_eq!(host.requests.lock().unwrap().len(), 1);
    let snapshot = json!({"revision":8,"routes":[route()]});
    let host = Fixture::new(Some(wire("200 OK", &snapshot.to_string()))).await;
    let output = host
        .cli(&["host", "config", "get"], None, true, false)
        .await;
    secret_free(&output);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        snapshot
    );
    let requests = host.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert!(requests[0].authenticated);
}

#[tokio::test]
async fn i08_noop_replay_unavailable_and_human_failure_keep_distinct_meanings() {
    for replayed in [false, true] {
        let mut value = result(replayed);
        value["receipt"]["changed"] = json!(false);
        value["receipt"]["revision"] = json!(0);
        let host = Fixture::new(Some(wire("200 OK", &value.to_string()))).await;
        let output = host.cli(&set("--keep-credential"), None, true, false).await;
        secret_free(&output);
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            value
        );
        host.one_put();
    }
    let mut unavailable = route();
    unavailable["credential_source"] = json!("unavailable");
    let host = Fixture::new(Some(wire(
        "200 OK",
        &json!({"revision":3,"routes":[unavailable]}).to_string(),
    )))
    .await;
    let output = host
        .cli(&["host", "config", "get"], None, false, false)
        .await;
    secret_free(&output);
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).contains("不等于missing"));
    let mut value = result(false);
    value["receipt"]["route"]["name"] = json!(SECRET);
    let host = Fixture::new(Some(wire("200 OK", &value.to_string()))).await;
    let output = host
        .cli(
            &set("--credential-stdin"),
            Some(SECRET.as_bytes()),
            false,
            false,
        )
        .await;
    secret_free(&output);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("operation_result_unknown"));
    host.one_put();
}
