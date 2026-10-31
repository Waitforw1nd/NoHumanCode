//! NEXT-20: real CLI with adversarial composition responses.
#![cfg(windows)]
use peachsh::{engine::Engine, store::Store};
use serde_json::{Value, json};
use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const BIN: &str = env!("CARGO_BIN_EXE_peachsh");
const TOKEN: &str = "composition-bootstrap-test-token";
const SECRET: &str = "sk-synthetic-composition-secret";

fn wire(status: &str, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).into_bytes()
}
fn actual_snapshot() -> Value {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(
        Arc::new(Store::open(&dir.path().join("host.db")).unwrap()),
        1,
    )
    .unwrap();
    serde_json::to_value(engine.composition().unwrap()).unwrap()
}
async fn run(
    reply: Option<Vec<u8>>,
    json_mode: bool,
) -> (std::process::Output, Vec<(String, String, bool)>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..n]);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                assert!(bytes.len() < 65536);
            }
            let header = String::from_utf8(bytes).unwrap();
            let mut first = header.lines().next().unwrap().split_whitespace();
            let method = first.next().unwrap().to_owned();
            let path = first.next().unwrap().to_owned();
            if path == "/api/bootstrap" {
                let _ = socket
                    .write_all(&wire("200 OK", &json!({"token":TOKEN}).to_string()))
                    .await;
            } else {
                let authenticated = header.lines().any(|line| {
                    line.split_once(':').is_some_and(|(name, value)| {
                        name.eq_ignore_ascii_case("x-peachsh-token") && value.trim() == TOKEN
                    })
                });
                recorded.lock().unwrap().push((method, path, authenticated));
                if let Some(bytes) = &reply {
                    if bytes == b"__timeout__" {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    } else {
                        let _ = socket.write_all(bytes).await;
                    }
                }
            }
            let _ = socket.shutdown().await;
        }
    });
    let mut command = tokio::process::Command::new(BIN);
    command
        .arg("--port")
        .arg(port.to_string())
        .args(if json_mode { vec!["--json"] } else { vec![] })
        .args(["host", "composition"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(12), command.output())
        .await
        .unwrap()
        .unwrap();
    server.abort();
    let observed = requests.lock().unwrap().clone();
    assert_eq!(
        observed,
        vec![("GET".into(), "/api/host/composition".into(), true)]
    );
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains(TOKEN) && !text.contains(SECRET));
    }
    (output, observed)
}
fn failure(output: &std::process::Output, code: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["code"], code);
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(!text.contains("SELECT") && !text.contains("PUT") && !text.contains("unknown"));
}
#[tokio::test]
async fn composition_cli_real_transport_success_and_adversarial_responses() {
    let valid = actual_snapshot();
    for mode in [true, false] {
        let (output, _) = run(Some(wire("200 OK", &valid.to_string())), mode).await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if mode {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                valid
            );
        } else {
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(
                text.contains("host_required")
                    && text.contains("per_component")
                    && text.contains("授权")
            );
        }
    }
    let mut valid_version = valid.clone();
    valid_version["capabilities"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["id"] == "provider-xpeach")
        .unwrap()["provider_version"] = json!("7.6.5");
    assert!(
        run(Some(wire("200 OK", &valid_version.to_string())), true)
            .await
            .0
            .status
            .success()
    );
    let mut invalid = Vec::new();
    let mut v = valid.clone();
    v["capabilities"].as_array_mut().unwrap().pop();
    invalid.push(v);
    let mut v = valid.clone();
    v["capabilities"][0] = v["capabilities"][1].clone();
    invalid.push(v);
    let mut v = valid.clone();
    v["extra"] = json!(1);
    invalid.push(v);
    let mut v = valid.clone();
    v["version"] = json!(2);
    invalid.push(v);
    let mut v = valid.clone();
    v["capabilities"][0]["dependencies"] = json!(["workspace"]);
    invalid.push(v);
    let mut v = valid.clone();
    v["capabilities"][0]["interfaces"][0]["version"] = json!("2.0.0");
    invalid.push(v);
    let mut v = valid.clone();
    v["capabilities"][0]["provider_version"] = json!("1.02.0");
    invalid.push(v);
    let mut v = valid.clone();
    v["plugins"][0]["desired"]["unexpected"] = json!(true);
    invalid.push(v);
    let mut v = valid.clone();
    v["runtimes"][0]["runtime"]["unexpected"] = json!(true);
    invalid.push(v);
    let mut v = valid.clone();
    v["capabilities"][0]["provider_id"] = json!(SECRET);
    invalid.push(v);
    let mut v = valid.clone();
    v["capabilities"][0]["provider_id"] = json!(TOKEN);
    invalid.push(v);
    for v in invalid {
        let (output, _) = run(Some(wire("200 OK", &v.to_string())), true).await;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let duplicate = format!(
        "{{\"version\":1,{}",
        valid.to_string().trim_start_matches('{')
    );
    let base = valid.to_string();
    let padded = format!("{base}{}", " ".repeat(1_048_576 - base.len()));
    assert!(
        run(Some(wire("200 OK", &padded)), true)
            .await
            .0
            .status
            .success()
    );
    for response in [
        wire("200 OK", &duplicate),
        wire("200 OK", "not-json"),
        wire("200 OK", &(padded + " ")),
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 200\r\nConnection: close\r\n\r\n{".to_vec(),
    ] {
        let (output, _) = run(Some(response), true).await;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let redirected = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let location = format!("http://{}/followed", redirected.local_addr().unwrap());
    let response = format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes();
    let (output, _) = run(Some(response), true).await;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(200), redirected.accept())
            .await
            .is_err()
    );
    for (status, body, code) in [
        (
            "403 Forbidden",
            json!({"error":{"code":"forbidden","message":"SELECT private","retryable":false}}),
            "forbidden",
        ),
        (
            "404 Not Found",
            json!({"code":"not_found","message":"SELECT private","error":"SELECT private","retryable":false}),
            "not_found",
        ),
        (
            "500 Internal Server Error",
            json!({"code":"internal","message":"SELECT private","error":"SELECT private","retryable":true}),
            "internal",
        ),
    ] {
        let (output, _) = run(Some(wire(status, &body.to_string())), true).await;
        failure(&output, code);
    }
    let (output, _) = run(None, true).await;
    failure(&output, "unavailable");
    let (output, _) = run(Some(b"__timeout__".to_vec()), true).await;
    failure(&output, "unavailable");
}
