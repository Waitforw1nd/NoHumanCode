use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[tokio::test]
async fn kernel_serve_supports_token_env_with_closed_stdin_and_rejects_missing_env() {
    let bin = env!("CARGO_BIN_EXE_coda");

    // 1. Missing env var exits non-zero cleanly.
    let missing_out = Command::new(bin)
        .args([
            "kernel",
            "serve",
            "--embedded",
            "--listen",
            "127.0.0.1:0",
            "--token-env",
            "CODA_NONEXISTENT_KERNEL_TOKEN_VAR",
        ])
        .env_remove("CODA_NONEXISTENT_KERNEL_TOKEN_VAR")
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!missing_out.status.success());
    let stderr = String::from_utf8_lossy(&missing_out.stderr);
    assert!(stderr.contains("CODA_NONEXISTENT_KERNEL_TOKEN_VAR"));

    // 2. Valid env var emits ready line, stays alive with closed stdin, and authenticates HTTP requests.
    let token = "env-attached-kernel-token-abc123";
    let mut child = Command::new(bin)
        .args([
            "kernel",
            "serve",
            "--embedded",
            "--listen",
            "127.0.0.1:0",
            "--token-env",
            "CODA_TEST_KERNEL_TOKEN",
        ])
        .env("CODA_TEST_KERNEL_TOKEN", token)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut ready_line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut ready_line))
        .await
        .expect("timed out waiting for ready line")
        .unwrap();

    let ready: Value = serde_json::from_str(ready_line.trim()).unwrap();
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["protocol"], "codaKernel/1");
    let port = ready["port"].as_u64().unwrap();

    // Verify child stays alive despite closed stdin.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(child.try_wait().unwrap().is_none());

    let client = reqwest::Client::new();
    let unauth = client
        .get(format!("http://127.0.0.1:{port}/api/kernel/capabilities"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);

    let ok = client
        .get(format!("http://127.0.0.1:{port}/api/kernel/capabilities"))
        .header("x-coda-token", token)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);

    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
async fn kernel_serve_rejects_whitespace_only_token_env_before_ready() {
    let bin = env!("CARGO_BIN_EXE_coda");

    for (var_name, val) in [
        ("CODA_WS_TOK", "   "),
        ("CODA_TAB_TOK", "\t"),
        ("CODA_CRLF_WS_TOK", "\r\n "),
    ] {
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            Command::new(bin)
                .args([
                    "kernel",
                    "serve",
                    "--embedded",
                    "--listen",
                    "127.0.0.1:0",
                    "--token-env",
                    var_name,
                ])
                .env(var_name, val)
                .stdin(Stdio::null())
                .output(),
        )
        .await
        .expect("whitespace-only --token-env should exit immediately")
        .unwrap();

        assert!(
            !out.status.success(),
            "expected non-zero exit for {var_name}"
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("ready"),
            "unexpected ready output on stdout for {var_name}: {stdout}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(var_name),
            "expected stderr to mention {var_name}, got: {stderr}"
        );
    }
}
