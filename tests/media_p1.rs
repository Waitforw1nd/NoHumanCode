//! CLI integration smoke tests for `coda media {embed, rerank, image}` against loopback mock upstreams.

use axum::{
    Router,
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::process::{Command, Stdio};

const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x01, 0x02,
];

async fn spawn_mock_upstream() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route(
            "/v1/embeddings",
            post(|headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(
                    headers.get("authorization").and_then(|v| v.to_str().ok()),
                    Some("Bearer cli-oai-key")
                );
                assert_eq!(body["model"], "text-embedding-3-small");
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "model": "text-embedding-3-small",
                        "data": [{"index": 0, "embedding": [0.5, -0.25]}]
                    })),
                )
            }),
        )
        .route(
            "/v1/rerank",
            post(|headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(
                    headers.get("authorization").and_then(|v| v.to_str().ok()),
                    Some("Bearer cli-jina-key")
                );
                assert_eq!(body["query"], "rust");
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "model": "jina-reranker-v2-base-multilingual",
                        "results": [
                            {"index": 1, "relevance_score": 0.95, "document": "rust cargo"},
                            {"index": 0, "relevance_score": 0.12, "document": "other"}
                        ]
                    })),
                )
            }),
        )
        .route(
            "/v1/images/generations",
            post(|headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(
                    headers.get("authorization").and_then(|v| v.to_str().ok()),
                    Some("Bearer cli-img-key")
                );
                assert!(
                    body.get("response_format").is_none(),
                    "gpt-image-1 must omit response_format"
                );
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "data": [{"b64_json": STANDARD.encode(TINY_PNG), "revised_prompt": "ok"}]
                    })),
                )
            }),
        )
        .route(
            "/v1/images/edits",
            post(|headers: HeaderMap, body: Bytes| async move {
                let ct = headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap();
                assert!(ct.starts_with("multipart/form-data; boundary="));
                let raw = String::from_utf8_lossy(&body);
                assert_eq!(raw.matches("name=\"image[]\"").count(), 2);
                assert!(raw.contains("name=\"mask\""));
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "data": [{"b64_json": STANDARD.encode(TINY_PNG)}]
                    })),
                )
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

fn run_coda_cli(args: &[&str], stdin_key: &str) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_coda"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn coda binary");
    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "{stdin_key}").expect("write stdin key");
    }
    let output = child.wait_with_output().expect("wait coda output");
    assert!(
        output.status.success(),
        "coda failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("valid JSON stdout")
}

#[tokio::test]
async fn cli_media_embed_rerank_and_image_write_workspace_artifacts() {
    let (base_url, _server) = spawn_mock_upstream().await;
    let ws = tempfile::tempdir().unwrap();
    let ws_str = ws.path().to_str().unwrap();

    // 1. coda media embed
    let embed_out = tokio::task::spawn_blocking({
        let base_url = base_url.clone();
        move || {
            run_coda_cli(
                &[
                    "--json",
                    "media",
                    "embed",
                    "--vendor",
                    "openai",
                    "--base-url",
                    &base_url,
                    "--model",
                    "text-embedding-3-small",
                    "--input",
                    "hello world",
                    "--api-key-stdin",
                ],
                "cli-oai-key",
            )
        }
    })
    .await
    .unwrap();
    assert_eq!(embed_out["embeddings"][0]["values"], json!([0.5, -0.25]));

    // 2. coda media rerank
    let rerank_out = tokio::task::spawn_blocking({
        let base_url = base_url.clone();
        move || {
            run_coda_cli(
                &[
                    "--json",
                    "media",
                    "rerank",
                    "--base-url",
                    &base_url,
                    "--model",
                    "jina-reranker-v2-base-multilingual",
                    "--query",
                    "rust",
                    "--doc",
                    "other",
                    "--doc",
                    "rust cargo",
                    "--top-n",
                    "2",
                    "--api-key-stdin",
                ],
                "cli-jina-key",
            )
        }
    })
    .await
    .unwrap();
    assert_eq!(rerank_out["results"][0]["index"], 1);

    // 3. coda media image generate (persists artifact under workspace/.coda/artifacts)
    let img_out = tokio::task::spawn_blocking({
        let base_url = base_url.clone();
        let ws_owned = ws_str.to_owned();
        move || {
            run_coda_cli(
                &[
                    "--json",
                    "--workspace",
                    &ws_owned,
                    "media",
                    "image",
                    "generate",
                    "--vendor",
                    "openai",
                    "--base-url",
                    &base_url,
                    "--model",
                    "gpt-image-1",
                    "--prompt",
                    "a tiny icon",
                    "--output-format",
                    "png",
                    "--api-key-stdin",
                ],
                "cli-img-key",
            )
        }
    })
    .await
    .unwrap();
    let artifact_path = img_out["images"][0]["artifact"]["path"]
        .as_str()
        .expect("artifact path");
    assert!(artifact_path.starts_with(".coda/artifacts/"));
    let expected_sha = format!("{:x}", Sha256::digest(TINY_PNG));
    assert_eq!(img_out["images"][0]["artifact"]["sha256"], expected_sha);
    let saved = std::fs::read(ws.path().join(artifact_path)).unwrap();
    assert_eq!(saved, TINY_PNG);

    // 4. coda media image edit (multipart repeated image[] + PNG mask)
    let img1 = ws.path().join("in1.png");
    let img2 = ws.path().join("in2.png");
    let mask = ws.path().join("mask.png");
    std::fs::write(&img1, TINY_PNG).unwrap();
    std::fs::write(&img2, TINY_PNG).unwrap();
    std::fs::write(&mask, TINY_PNG).unwrap();

    let edit_out = tokio::task::spawn_blocking({
        let base_url = base_url.clone();
        let ws_owned = ws_str.to_owned();
        let i1 = img1.to_str().unwrap().to_owned();
        let i2 = img2.to_str().unwrap().to_owned();
        let m = mask.to_str().unwrap().to_owned();
        move || {
            run_coda_cli(
                &[
                    "--json",
                    "--workspace",
                    &ws_owned,
                    "media",
                    "image",
                    "edit",
                    "--vendor",
                    "openai",
                    "--base-url",
                    &base_url,
                    "--model",
                    "gpt-image-1",
                    "--prompt",
                    "combine",
                    "--image",
                    &i1,
                    "--image",
                    &i2,
                    "--mask",
                    &m,
                    "--api-key-stdin",
                ],
                "cli-img-key",
            )
        }
    })
    .await
    .unwrap();
    let edit_path = edit_out["images"][0]["artifact"]["path"]
        .as_str()
        .expect("edit artifact path");
    assert_eq!(std::fs::read(ws.path().join(edit_path)).unwrap(), TINY_PNG);
}

#[tokio::test]
async fn cli_media_embed_respects_top_level_data_dir_for_route() {
    let app = Router::new().route(
        "/v1/embeddings",
        post(
            |headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(
                    headers.get("authorization").and_then(|v| v.to_str().ok()),
                    Some("Bearer top-data-dir-route-secret")
                );
                assert_eq!(body["model"], "text-embedding-3-small");
                assert_eq!(body["input"], "hello top data dir");
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "model": "text-embedding-3-small",
                        "data": [{"index": 0, "embedding": [0.125, 0.875]}]
                    })),
                )
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base_url = format!("http://{addr}/v1");

    let custom_data_dir = tempfile::tempdir().unwrap();
    let fallback_env_dir = tempfile::tempdir().unwrap();
    let fallback_local_appdata = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();

    let store = coda::store::Store::open(&custom_data_dir.path().join("coda.sqlite3")).unwrap();
    let settings = coda::domain::Settings {
        workspace: ws.path().to_string_lossy().into_owned(),
        max_concurrency: 2,
        routes: vec![coda::domain::Route {
            id: "route-top-dd".into(),
            name: "Top Data Dir Route".into(),
            base_url,
            model: "text-embedding-3-small".into(),
            max_tokens: 4096,
            parallel_limit: 1,
            key_env: None,
            protocol: None,
        }],
        newapi: None,
    };
    store.save_settings(&settings).unwrap();
    store
        .put_secret("route::route-top-dd", "top-data-dir-route-secret")
        .unwrap();
    drop(store);

    let custom_dd_str = custom_data_dir.path().to_str().unwrap().to_owned();
    let ws_str = ws.path().to_str().unwrap().to_owned();
    let fallback_env_path = fallback_env_dir.path().to_path_buf();
    let fallback_local_path = fallback_local_appdata.path().to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_coda"))
            .env("CODA_DATA_DIR", &fallback_env_path)
            .env("LOCALAPPDATA", &fallback_local_path)
            .args([
                "--data-dir",
                &custom_dd_str,
                "--workspace",
                &ws_str,
                "--json",
                "media",
                "embed",
                "--route",
                "route-top-dd",
                "--vendor",
                "openai",
                "--model",
                "text-embedding-3-small",
                "--input",
                "hello top data dir",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run coda binary")
    })
    .await
    .unwrap();

    assert!(
        output.status.success(),
        "coda --data-dir ... media embed failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("valid JSON stdout");
    assert_eq!(parsed["embeddings"][0]["values"], json!([0.125, 0.875]));
    assert!(
        custom_data_dir.path().join("coda.sqlite3").exists(),
        "expected custom data_dir DB to exist"
    );
    assert!(
        !fallback_env_dir.path().join("coda.sqlite3").exists(),
        "CODA_DATA_DIR fallback DB must not be created when top-level --data-dir is set"
    );
    assert!(
        !fallback_local_appdata
            .path()
            .join("coda")
            .join("coda.sqlite3")
            .exists(),
        "LOCALAPPDATA fallback DB must not be created when top-level --data-dir is set"
    );
}
