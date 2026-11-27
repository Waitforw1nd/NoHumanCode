use coda::kernel_embedded::session_store::EmbeddedSessionStore;
use coda::kernel_server::{KernelAppState, kernel_router};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;

async fn spawn_kernel(
    media_proxy: bool,
) -> (
    String,
    Arc<EmbeddedSessionStore>,
    tokio::task::JoinHandle<()>,
) {
    let token = "test-kernel-v11-token";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let expected_host = format!("127.0.0.1:{}", addr.port());
    let state = KernelAppState::with_media_proxy(token.into(), expected_host, media_proxy);
    let store = state.store.clone();
    let app = kernel_router(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), store, handle)
}

#[tokio::test]
async fn kernel_1_0_default_disables_media_capability_and_rejects_media_blocks() {
    let (base, _store, srv) = spawn_kernel(false).await;
    let client = reqwest::Client::new();
    let tok = "test-kernel-v11-token";

    let caps: Value = client
        .get(format!("{base}/api/kernel/capabilities"))
        .header("x-coda-token", tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(caps["protocol"], "codaKernel/1");
    let features = caps["features"].as_array().unwrap();
    assert!(!features.iter().any(|f| f == "media"));
    assert!(caps.get("mediaProtocols").is_none());
    assert!(caps.get("mediaRoutes").is_none());

    // Hydrate containing an audio block must fail with 400 INVALID_REQUEST in 1.0 mode.
    let resp = client
        .put(format!("{base}/api/kernel/sessions/s-10"))
        .header("x-coda-token", tok)
        .json(&json!({
            "cwd": "E:/tmp",
            "systemPrompt": "hi",
            "model": {
                "modelId": "gpt-4o",
                "protocol": "openai-completions"
            },
            "tools": [],
            "activeToolNames": [],
            "compaction": { "enabled": false, "keepRecentTokens": 1000 },
            "steeringMode": "all",
            "gates": [],
            "history": {
                "entries": [
                    {
                        "type": "message",
                        "id": "e1",
                        "parentId": null,
                        "timestamp": "2026-11-27T00:00:00Z",
                        "role": "user",
                        "content": [
                            { "type": "audio", "path": "audio/clip.mp3", "mimeType": "audio/mpeg" }
                        ]
                    }
                ],
                "leafId": "e1"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err: Value = resp.json().await.unwrap();
    assert_eq!(err["error"]["code"], "INVALID_REQUEST");

    // Clean hydrate succeeds, then customMessage append & runtimeContext patch with media blocks are rejected.
    let ok_hydrate = client
        .put(format!("{base}/api/kernel/sessions/s-10"))
        .header("x-coda-token", tok)
        .json(&json!({
            "cwd": "E:/tmp",
            "systemPrompt": "hi",
            "model": {
                "modelId": "gpt-4o",
                "protocol": "openai-completions"
            },
            "tools": [],
            "activeToolNames": [],
            "compaction": { "enabled": false, "keepRecentTokens": 1000 },
            "steeringMode": "all",
            "gates": [],
            "history": { "entries": [], "leafId": null }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok_hydrate.status(), StatusCode::CREATED);

    let bad_append = client
        .post(format!("{base}/api/kernel/sessions/s-10/entries"))
        .header("x-coda-token", tok)
        .json(&json!({
            "baseRevision": 0,
            "deliverAs": "append",
            "entry": {
                "type": "customMessage",
                "name": "artifact",
                "display": true,
                "content": [
                    { "type": "resource", "path": "docs/report.pdf" }
                ]
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_append.status(), StatusCode::BAD_REQUEST);

    let bad_patch = client
        .patch(format!("{base}/api/kernel/sessions/s-10"))
        .header("x-coda-token", tok)
        .json(&json!({
            "runtimeContext": [
                { "type": "video", "path": "media/vid.mp4", "mimeType": "video/mp4" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_patch.status(), StatusCode::BAD_REQUEST);

    srv.abort();
}

#[tokio::test]
async fn kernel_1_1_advertises_media_capability_and_round_trips_media_blocks() {
    let (base, store, srv) = spawn_kernel(true).await;
    let client = reqwest::Client::new();
    let tok = "test-kernel-v11-token";

    let caps: Value = client
        .get(format!("{base}/api/kernel/capabilities"))
        .header("x-coda-token", tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caps["protocol"], "codaKernel/1");
    assert!(
        caps["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "media")
    );
    assert!(
        caps["mediaProtocols"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "openai-embeddings")
    );
    assert!(
        caps["mediaRoutes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "media/tasks")
    );

    // Invalid traversal, whitespace-only, and >4096-scalar paths still rejected in 1.1 mode.
    let over_limit_path = format!("{}.mp3", "a".repeat(4097));
    for bad_path in ["../escape.mp4", "   ", over_limit_path.as_str()] {
        let bad_hydrate = client
            .put(format!("{base}/api/kernel/sessions/s-11"))
            .header("x-coda-token", tok)
            .json(&json!({
                "cwd": "E:/tmp",
                "systemPrompt": "hi",
                "model": {
                    "modelId": "gpt-4o",
                    "protocol": "openai-completions"
                },
                "tools": [],
                "activeToolNames": [],
                "compaction": { "enabled": false, "keepRecentTokens": 1000 },
                "steeringMode": "all",
                "gates": [],
                "history": {
                    "entries": [
                        {
                            "type": "message",
                            "id": "e-bad",
                            "parentId": null,
                            "timestamp": "2026-11-27T00:00:00Z",
                            "role": "user",
                            "content": [
                                { "type": "video", "path": bad_path, "mimeType": "video/mp4" }
                            ]
                        }
                    ],
                    "leafId": "e-bad"
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            bad_hydrate.status(),
            StatusCode::BAD_REQUEST,
            "expected 400 for path len={}",
            bad_path.chars().count()
        );
        let bad_err: Value = bad_hydrate.json().await.unwrap();
        assert_eq!(bad_err["error"]["code"], "INVALID_REQUEST");
    }

    // Valid audio/video/resource hydrate + customMessage append round-trip identically.
    let ok_hydrate = client
        .put(format!("{base}/api/kernel/sessions/s-11"))
        .header("x-coda-token", tok)
        .json(&json!({
            "cwd": "E:/tmp",
            "systemPrompt": "hi",
            "model": {
                "modelId": "gpt-4o",
                "protocol": "openai-completions"
            },
            "tools": [],
            "activeToolNames": [],
            "compaction": { "enabled": false, "keepRecentTokens": 1000 },
            "steeringMode": "all",
            "gates": [],
            "history": {
                "entries": [
                    {
                        "type": "message",
                        "id": "e1",
                        "parentId": null,
                        "timestamp": "2026-11-27T00:00:00Z",
                        "role": "user",
                        "content": [
                            {
                                "type": "audio",
                                "path": "media/clip.mp3",
                                "mimeType": "audio/mpeg",
                                "bytes": 1234,
                                "durationMs": 500,
                                "sha256": "deadbeef",
                                "name": "clip.mp3"
                            },
                            {
                                "type": "video",
                                "path": "media/out.mp4",
                                "mimeType": "video/mp4"
                            }
                        ]
                    }
                ],
                "leafId": "e1"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok_hydrate.status(), StatusCode::CREATED);
    let hydrate_body: Value = ok_hydrate.json().await.unwrap();
    let rev = hydrate_body["revision"].as_u64().unwrap();

    let ok_append = client
        .post(format!("{base}/api/kernel/sessions/s-11/entries"))
        .header("x-coda-token", tok)
        .json(&json!({
            "baseRevision": rev,
            "deliverAs": "append",
            "entry": {
                "type": "customMessage",
                "name": "generated_asset",
                "display": true,
                "content": [
                    { "type": "resource", "path": "docs/output.pdf", "mimeType": "application/pdf", "bytes": 42 }
                ]
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok_append.status(), StatusCode::OK);
    let append_body: Value = ok_append.json().await.unwrap();
    assert_eq!(append_body["entry"]["content"][0]["type"], "resource");
    assert_eq!(
        append_body["entry"]["content"][0]["path"],
        "docs/output.pdf"
    );
    assert_eq!(append_body["entry"]["content"][0]["bytes"], 42);

    let ok_patch = client
        .patch(format!("{base}/api/kernel/sessions/s-11"))
        .header("x-coda-token", tok)
        .json(&json!({
            "runtimeContext": [
                { "type": "audio", "path": "media/bgm.wav", "mimeType": "audio/wav" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok_patch.status(), StatusCode::OK);

    assert!(store.media_enabled());

    srv.abort();
}
