//! Public cancellation/closing compatibility. No paid provider or shell command.
//! These assertions cover receipts and admission, not OS process-tree cleanup.
#![cfg(windows)]

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    fs::File,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

const TOKEN: &str = "synthetic-lifecycle-token";

#[derive(Clone)]
struct Reply {
    value: Value,
    status: StatusCode,
    truncate: bool,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

async fn cli_reply(State(reply): State<Reply>, request: Request) -> Response {
    let token = request
        .headers()
        .get("x-peachsh-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = to_bytes(request.into_body(), 4096).await.unwrap();
    reply
        .calls
        .lock()
        .unwrap()
        .push((token, serde_json::from_slice(&bytes).unwrap()));
    if reply.truncate {
        let stream = async_stream::stream! {
            yield Ok::<_, std::io::Error>(b"{".to_vec());
            yield Err(std::io::Error::other("synthetic response interruption"));
        };
        return (
            [(header::CONTENT_TYPE, "application/json")],
            Body::from_stream(stream),
        )
            .into_response();
    }
    (reply.status, Json(reply.value)).into_response()
}

async fn cli_cancel(reply: Reply) -> (i32, Vec<u8>, Vec<u8>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route(
            "/api/bootstrap",
            get(|| async { Json(json!({"token":TOKEN})) }),
        )
        .route("/api/tasks/compat-task/cancel", post(cli_reply))
        .with_state(reply);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let stdout = root.join("cli.stdout");
    let stderr = root.join("cli.stderr");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"));
    command
        .args([
            "--port",
            &port.to_string(),
            "--json",
            "task",
            "cancel",
            "compat-task",
        ])
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap())
        .creation_flags(0x0800_0000);
    let mut child = command.spawn().unwrap();
    let observed = tokio::time::timeout(Duration::from_secs(15), child.wait()).await;
    let (status, unknown) = match observed {
        Ok(Ok(status)) => (status, false),
        _ => {
            eprintln!(
                "NHC_TERMINAL_UNKNOWN CLI compatibility deadline/observation; holding original Child at {}",
                root.display()
            );
            // Keep the exact creation object. No name/PID kill or premature Drop.
            let status = loop {
                if let Ok(status) = child.wait().await {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            };
            (status, true)
        }
    };
    server.abort();
    let _ = server.await;
    std::fs::write(
        root.join("direct-cli-exit.json"),
        json!({"exit_code":status.code(),
        "terminal_unknown":unknown,"scope":"direct CLI process only, no tree claim"})
        .to_string(),
    )
    .unwrap();
    assert!(
        !unknown,
        "CLI exceeded its deadline; retained evidence is not passing"
    );
    (
        status.code().unwrap(),
        std::fs::read(stdout).unwrap(),
        std::fs::read(stderr).unwrap(),
    )
}

#[tokio::test]
async fn cli_cancel_ack_and_known_shutdown_refusal_are_single_requests() {
    for (value, status, expected_code) in [
        (json!({"ok":true}), StatusCode::OK, 0),
        (
            json!({"code":"request_failed","message":"Host 正在关闭，拒绝新工作",
                "error":"Host 正在关闭，拒绝新工作","retryable":false}),
            StatusCode::BAD_REQUEST,
            1,
        ),
    ] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (code, stdout, stderr) = cli_cancel(Reply {
            value,
            status,
            truncate: false,
            calls: calls.clone(),
        })
        .await;
        assert_eq!(code, expected_code);
        assert_eq!(*calls.lock().unwrap(), vec![(TOKEN.into(), json!({}))]);
        if code == 0 {
            assert!(stderr.is_empty());
            assert_eq!(
                serde_json::from_slice::<Value>(&stdout).unwrap(),
                json!({"ok":true})
            );
        } else {
            assert!(stdout.is_empty());
            let error: Value = serde_json::from_slice(&stderr).unwrap();
            assert_eq!(error["code"], "request_failed");
            assert_eq!(error["retryable"], false);
        }
    }
}

#[tokio::test]
async fn cli_cancel_lost_or_invalid_ack_is_unknown_without_automatic_retry() {
    for truncate in [false, true] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (code, stdout, stderr) = cli_cancel(Reply {
            value: json!({"ok":false}),
            status: StatusCode::OK,
            truncate,
            calls: calls.clone(),
        })
        .await;
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let error: Value = serde_json::from_slice(&stderr).unwrap();
        assert_eq!(error["code"], "operation_result_unknown");
        assert_eq!(error["retryable"], false);
        assert_eq!(*calls.lock().unwrap(), vec![(TOKEN.into(), json!({}))]);
    }
}
