use axum::{
    Router,
    extract::Path as AxumPath,
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

fn run_coda_with_io(
    workspace: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    remove_envs: &[&str],
    stdin_data: Option<&str>,
) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_coda"));
    cmd.current_dir(workspace);
    cmd.arg("--workspace").arg(workspace);
    cmd.arg("--json");
    cmd.args(args);
    for k in remove_envs {
        cmd.env_remove(k);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    if let Some(input) = stdin_data {
        use std::io::Write;
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("failed to spawn coda binary");
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().expect("failed to wait on coda");
        return (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        );
    }
    let out = cmd.output().expect("failed to execute coda binary");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_coda(workspace: &Path, args: &[&str], envs: &[(&str, &str)]) -> (bool, String, String) {
    run_coda_with_io(workspace, args, envs, &[], None)
}

async fn run_coda_with_io_async(
    workspace: std::path::PathBuf,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    remove_envs: Vec<String>,
    stdin_data: Option<String>,
) -> (bool, String, String) {
    tokio::task::spawn_blocking(move || {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let env_refs: Vec<(&str, &str)> =
            envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let rem_refs: Vec<&str> = remove_envs.iter().map(String::as_str).collect();
        run_coda_with_io(
            &workspace,
            &arg_refs,
            &env_refs,
            &rem_refs,
            stdin_data.as_deref(),
        )
    })
    .await
    .expect("spawn_blocking coda")
}

async fn run_coda_async(
    workspace: std::path::PathBuf,
    args: Vec<String>,
    envs: Vec<(String, String)>,
) -> (bool, String, String) {
    tokio::task::spawn_blocking(move || {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let env_refs: Vec<(&str, &str)> =
            envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        run_coda(&workspace, &arg_refs, &env_refs)
    })
    .await
    .expect("spawn_blocking coda")
}

async fn read_client_ws_text(br: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> String {
    let mut hdr = [0u8; 2];
    br.read_exact(&mut hdr).await.unwrap();
    let masked = (hdr[1] & 0x80) != 0;
    let len7 = (hdr[1] & 0x7F) as u64;
    let len = match len7 {
        126 => {
            let mut b = [0u8; 2];
            br.read_exact(&mut b).await.unwrap();
            u16::from_be_bytes(b) as usize
        }
        127 => {
            let mut b = [0u8; 8];
            br.read_exact(&mut b).await.unwrap();
            u64::from_be_bytes(b) as usize
        }
        n => n as usize,
    };
    let mut mask = [0u8; 4];
    if masked {
        br.read_exact(&mut mask).await.unwrap();
    }
    let mut payload = vec![0u8; len];
    br.read_exact(&mut payload).await.unwrap();
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    String::from_utf8(payload).unwrap()
}

async fn send_server_ws_text(w: &mut tokio::net::tcp::OwnedWriteHalf, text: &str) {
    let bytes = text.as_bytes();
    let mut frame = Vec::with_capacity(bytes.len() + 10);
    frame.push(0x81);
    if bytes.len() < 126 {
        frame.push(bytes.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(bytes);
    w.write_all(&frame).await.unwrap();
}

async fn accept_ws_and_upgrade(
    listener: &tokio::net::TcpListener,
) -> (
    String,
    Vec<String>,
    BufReader<tokio::net::tcp::OwnedReadHalf>,
    tokio::net::tcp::OwnedWriteHalf,
) {
    let (stream, _) = listener.accept().await.unwrap();
    let (r, mut w) = stream.into_split();
    let mut br = BufReader::new(r);
    let mut req_line = String::new();
    br.read_line(&mut req_line).await.unwrap();
    let mut hdrs = Vec::new();
    loop {
        let mut line = String::new();
        br.read_line(&mut line).await.unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(k) = line
            .strip_prefix("Sec-WebSocket-Key:")
            .or_else(|| line.strip_prefix("sec-websocket-key:"))
        {
            let accept_src = format!("{}258EAFA5-E914-47DA-95CA-C5AB0DC85B11", k.trim());
            let accept = STANDARD.encode(coda::media::crypto::sha1_digest(accept_src.as_bytes()));
            w.write_all(
                format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        }
        hdrs.push(line);
    }
    (req_line, hdrs, br, w)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_d6_music_matrix_and_suno_opt_in_and_coda_gateway() {
    let ws = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");
    let b1 = base_url.clone();
    let b2 = base_url.clone();

    let app = Router::new()
        .route(
            "/v1/music_generation",
            post(|axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(body["output_format"], "hex");
                (
                    StatusCode::OK,
                    json!({"base_resp":{"status_code":0},"data":{"status":2,"audio":"49443301"}})
                        .to_string(),
                )
            }),
        )
        .route(
            "/v1/music",
            post(|headers: HeaderMap| async move {
                assert_eq!(
                    headers.get("xi-api-key").and_then(|v| v.to_str().ok()),
                    Some("xi-cli-key")
                );
                (StatusCode::OK, b"ID3-EL".to_vec())
            }),
        )
        .route(
            "/v1/music/plan",
            post(|| async { (StatusCode::OK, json!({"sections":[{"name":"verse"}]}).to_string()) }),
        )
        .route(
            "/v1beta/interactions",
            post(|axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(body["store"], false);
                let b64 = STANDARD.encode(b"ID3-L3");
                (
                    StatusCode::OK,
                    json!({"outputs":[{"type":"audio","data":b64,"mime_type":"audio/mp3"}]})
                        .to_string(),
                )
            }),
        )
        .route(
            "/v1/publishers/google/models/lyria-002:predict",
            post(|| async {
                let b64 = STANDARD.encode(b"RIFFXXXXWAVEfmt ");
                (
                    StatusCode::OK,
                    json!({"predictions":[{"bytesBase64Encoded":b64,"mimeType":"audio/wav"}]})
                        .to_string(),
                )
            }),
        )
        .route(
            "/suno/submit/music",
            post(|| async { (StatusCode::OK, json!({"code":"success","data":"suno-cli-1"}).to_string()) }),
        )
        .route(
            "/suno/fetch/{id}",
            get(move || {
                let b = b1.clone();
                async move {
                    (
                        StatusCode::OK,
                        json!({"code":"success","data":{"status":"SUCCESS","data":[{"audio_url":format!("{b}/dl/s.mp3")}]}}).to_string(),
                    )
                }
            }),
        )
        .route(
            "/v1/music/tasks",
            post(|| async { (StatusCode::OK, json!({"task_id":"cm-cli-1"}).to_string()) }),
        )
        .route(
            "/v1/music/tasks/{id}",
            get(|| async {
                (
                    StatusCode::OK,
                    json!({"task_id":"cm-cli-1","action":"generate","status":"succeeded","result":{"clips":[{"audio_artifact":"a1"}]}}).to_string(),
                )
            }),
        )
        .route(
            "/v1/music/tasks/{id}/artifacts",
            get(move || {
                let b = b2.clone();
                async move {
                    (
                        StatusCode::OK,
                        json!({"artifacts":[{"key":"a1","type":"audio","content_url":format!("{b}/dl/c.mp3"),"mime_type":"audio/mpeg"}]}).to_string(),
                    )
                }
            }),
        )
        .route("/dl/{f}", get(|| async { (StatusCode::OK, b"ID3CLI".to_vec()) }));

    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let envs = vec![
        ("MINIMAX_API_KEY".into(), "mm-k".into()),
        ("ELEVENLABS_API_KEY".into(), "xi-cli-key".into()),
        ("GEMINI_API_KEY".into(), "gem-k".into()),
        ("VERTEX_ACCESS_TOKEN".into(), "vtx-k".into()),
        ("OPENAI_API_KEY".into(), "oa-k".into()),
    ];

    // 1. MiniMax
    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "media".into(),
            "music".into(),
            "--vendor".into(),
            "minimax".into(),
            "--model".into(),
            "music-2.5".into(),
            "--prompt".into(),
            "calm piano".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "minimax stderr: {stderr}");
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["vendor"], "minimax");

    // 2. ElevenLabs compose + plan
    let (ok, _, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "media".into(),
            "music".into(),
            "--vendor".into(),
            "elevenlabs".into(),
            "--prompt".into(),
            "synth".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "elevenlabs stderr: {stderr}");

    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "media".into(),
            "music".into(),
            "--vendor".into(),
            "elevenlabs".into(),
            "--transport".into(),
            "plan".into(),
            "--prompt".into(),
            "synth".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "elevenlabs plan stderr: {stderr}");
    assert!(stdout.contains("verse"));

    // 3. Lyria 3 + Lyria 2
    let (ok, _, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "media".into(),
            "music".into(),
            "--vendor".into(),
            "lyria3".into(),
            "--prompt".into(),
            "lofi".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "lyria3 stderr: {stderr}");

    let (ok, _, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "media".into(),
            "music".into(),
            "--vendor".into(),
            "lyria2".into(),
            "--prompt".into(),
            "orchestra".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "lyria2 stderr: {stderr}");

    // 4. Suno MUST fail by default without --allow-suno-proxy
    let (ok, _, _) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            data_dir.path().to_string_lossy().into_owned(),
            "media".into(),
            "music".into(),
            "--vendor".into(),
            "suno".into(),
            "--prompt".into(),
            "rock".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(!ok, "Suno must fail without --allow-suno-proxy");

    // Suno with --allow-suno-proxy succeeds
    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            data_dir.path().to_string_lossy().into_owned(),
            "media".into(),
            "music".into(),
            "--vendor".into(),
            "suno".into(),
            "--allow-suno-proxy".into(),
            "--prompt".into(),
            "rock".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "suno enabled stderr: {stderr}");
    let suno_rec: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(suno_rec["status"], "succeeded");

    // Suno --no-wait + task cancel (forbidden without --allow-suno-proxy, succeeds with it)
    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            data_dir.path().to_string_lossy().into_owned(),
            "media".into(),
            "music".into(),
            "--vendor".into(),
            "suno".into(),
            "--allow-suno-proxy".into(),
            "--no-wait".into(),
            "--prompt".into(),
            "jazz".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "suno no-wait stderr: {stderr}");
    let suno_pending: Value = serde_json::from_str(&stdout).unwrap();
    let suno_id = suno_pending["id"].as_str().unwrap().to_owned();

    let (ok_no_flag, _, _) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            data_dir.path().to_string_lossy().into_owned(),
            "media".into(),
            "task".into(),
            "cancel".into(),
            "--id".into(),
            suno_id.clone(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(
        !ok_no_flag,
        "Suno task cancel must fail without --allow-suno-proxy"
    );

    let (ok_cancel, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            data_dir.path().to_string_lossy().into_owned(),
            "media".into(),
            "task".into(),
            "cancel".into(),
            "--id".into(),
            suno_id,
            "--allow-suno-proxy".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok_cancel, "suno cancel stderr: {stderr}");
    let suno_cancelled: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(suno_cancelled["status"], "cancelled");

    // 5. Coda Music Gateway (--no-wait + task poll)
    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            data_dir.path().to_string_lossy().into_owned(),
            "media".into(),
            "music".into(),
            "--vendor".into(),
            "coda-gateway".into(),
            "--prompt".into(),
            "ambient".into(),
            "--no-wait".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "coda gateway no-wait stderr: {stderr}");
    let cm_rec: Value = serde_json::from_str(&stdout).unwrap();
    let cm_id = cm_rec["id"].as_str().unwrap().to_owned();

    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            data_dir.path().to_string_lossy().into_owned(),
            "media".into(),
            "task".into(),
            "poll".into(),
            "--id".into(),
            cm_id,
            "--base-url".into(),
            base_url,
        ],
        envs,
    )
    .await;
    assert!(ok, "coda gateway poll stderr: {stderr}");
    let polled: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(polled["status"], "succeeded");
    assert_eq!(polled["artifacts"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_d7_video_matrix_large_download_and_top_data_dir_precedence() {
    let ws = TempDir::new().unwrap();
    let top_dd = TempDir::new().unwrap();
    let sub_dd = TempDir::new().unwrap();

    let cdn_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cdn_addr = cdn_listener.local_addr().unwrap();
    let cdn_app = Router::new().route(
        "/veo-signed.mp4",
        get(|headers: HeaderMap| async move {
            assert!(!headers.contains_key("x-goog-api-key"));
            (StatusCode::OK, b"VEO-CLI-MP4".to_vec())
        }),
    );
    tokio::spawn(async move { axum::serve(cdn_listener, cdn_app).await.unwrap() });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");
    let b1 = base_url.clone();
    let b2 = base_url.clone();
    let b3 = base_url.clone();
    let b4 = base_url.clone();
    let runway_polls = Arc::new(AtomicUsize::new(0));
    let rp_c = runway_polls.clone();
    let sora_deleted = Arc::new(AtomicBool::new(false));
    let sd_c = sora_deleted.clone();

    let app = Router::new()
        .route(
            "/v1/videos",
            post(|axum::Json(body): axum::Json<Value>| async move {
                if body.get("prompt").and_then(Value::as_str) == Some("big-video") {
                    (StatusCode::OK, json!({"id":"big-1"}).to_string())
                } else {
                    (StatusCode::OK, json!({"id":"sora-cli-1"}).to_string())
                }
            }),
        )
        .route(
            "/v1/videos/{id}",
            get(|AxumPath(id): AxumPath<String>| async move {
                let _ = id;
                (StatusCode::OK, json!({"status":"completed"}).to_string())
            }),
        )
        .route(
            "/v1/videos/{id}",
            delete(move || {
                sd_c.store(true, Ordering::SeqCst);
                async { StatusCode::NO_CONTENT }
            }),
        )
        .route(
            "/v1/videos/{id}/content",
            get(|AxumPath(id): AxumPath<String>| async move {
                if id == "big-1" {
                    // > 32 MiB (33 MiB) proves MEDIA_DEFAULT_VIDEO_LIMIT works over 32 MiB JSON limit
                    (StatusCode::OK, vec![0x41u8; 33 * 1024 * 1024])
                } else {
                    (StatusCode::OK, b"SORA-CLI-MP4".to_vec())
                }
            }),
        )
        .route(
            "/v1beta/models/{ma}",
            post(|| async { (StatusCode::OK, json!({"name":"models/veo/operations/op1"}).to_string()) }),
        )
        .route(
            "/v1beta/models/{m}/operations/{op}",
            get(move || {
                let b = b1.clone();
                async move {
                    (
                        StatusCode::OK,
                        json!({"done":true,"response":{"generateVideoResponse":{"generatedSamples":[{"video":{"uri":format!("{b}/veo-hop")}}]}}}).to_string(),
                    )
                }
            }),
        )
        .route(
            "/veo-hop",
            get(move |headers: HeaderMap| async move {
                assert_eq!(
                    headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                    Some("veo-key")
                );
                let mut h = HeaderMap::new();
                h.insert(
                    "location",
                    format!("http://{cdn_addr}/veo-signed.mp4").parse().unwrap(),
                );
                (StatusCode::FOUND, h)
            }),
        )
        .route(
            "/v1/videos/text2video",
            post(|headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap();
                assert!(auth.starts_with("Bearer eyJ"));
                assert_eq!(body["duration"], "5");
                (StatusCode::OK, json!({"code":0,"data":{"task_id":"kl-cli-1"}}).to_string())
            }),
        )
        .route(
            "/v1/videos/text2video/{id}",
            get(move || {
                let b = b2.clone();
                async move {
                    (
                        StatusCode::OK,
                        json!({"code":0,"data":{"task_status":"succeed","task_result":{"videos":[{"url":format!("{b}/dl/kl.mp4")}]}}}).to_string(),
                    )
                }
            }),
        )
        .route(
            "/v1/text_to_video",
            post(|headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(
                    headers.get("x-runway-version").and_then(|v| v.to_str().ok()),
                    Some("2024-11-06")
                );
                assert_eq!(body["ratio"], "1280:720");
                (StatusCode::OK, json!({"id":"rw-cli-1"}).to_string())
            }),
        )
        .route(
            "/v1/tasks/{id}",
            get(move || {
                let n = rp_c.fetch_add(1, Ordering::SeqCst);
                let b = b3.clone();
                async move {
                    if n == 0 {
                        (StatusCode::OK, json!({"status":"THROTTLED"}).to_string())
                    } else {
                        (StatusCode::OK, json!({"status":"SUCCEEDED","output":[format!("{b}/dl/rw.mp4")]}).to_string())
                    }
                }
            }),
        )
        .route(
            "/v1/video/generations",
            post(|| async { (StatusCode::OK, json!({"task_id":"na-c-cli"}).to_string()) }),
        )
        .route(
            "/v1/video/generations/{id}",
            get(move || {
                let b = b4.clone();
                async move {
                    (StatusCode::OK, json!({"status":"succeeded","video_url":format!("{b}/dl/na.mp4")}).to_string())
                }
            }),
        )
        .route("/dl/{f}", get(|| async { (StatusCode::OK, b"VID-CLI".to_vec()) }));

    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let envs = vec![
        ("OPENAI_API_KEY".into(), "oa-vid-key".into()),
        ("GEMINI_API_KEY".into(), "veo-key".into()),
        ("KLING_ACCESS_KEY".into(), "ak-cli".into()),
        ("KLING_SECRET_KEY".into(), "sk-cli".into()),
        ("RUNWAYML_API_SECRET".into(), "rw-key".into()),
        ("NEW_API_KEY".into(), "na-key".into()),
    ];

    // 1. Sora --no-wait in top_data_dir vs sub_data_dir precedence
    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            top_dd.path().to_string_lossy().into_owned(),
            "media".into(),
            "video".into(),
            "generate".into(),
            "--vendor".into(),
            "sora".into(),
            "--prompt".into(),
            "cancel test".into(),
            "--duration".into(),
            "4".into(),
            "--no-wait".into(),
            "--data-dir".into(),
            sub_dd.path().to_string_lossy().into_owned(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "sora no-wait stderr: {stderr}");
    let rec: Value = serde_json::from_str(&stdout).unwrap();
    let tid = rec["id"].as_str().unwrap().to_owned();

    // Subcommand --data-dir overrides top --data-dir: status in top_dd must fail, status in sub_dd succeeds
    let (ok_top, _, _) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            top_dd.path().to_string_lossy().into_owned(),
            "media".into(),
            "task".into(),
            "status".into(),
            "--id".into(),
            tid.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(!ok_top);

    let (ok_cancel, _, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            top_dd.path().to_string_lossy().into_owned(),
            "media".into(),
            "task".into(),
            "cancel".into(),
            "--id".into(),
            tid,
            "--data-dir".into(),
            sub_dd.path().to_string_lossy().into_owned(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok_cancel, "sora cancel stderr: {stderr}");
    assert!(sora_deleted.load(Ordering::SeqCst));

    // 2. Veo Gemini with cross-origin redirect auth strip
    let (ok, _, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            top_dd.path().to_string_lossy().into_owned(),
            "media".into(),
            "video".into(),
            "generate".into(),
            "--vendor".into(),
            "veo".into(),
            "--prompt".into(),
            "ocean".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "veo stderr: {stderr}");

    // 3. Kling AI with Auto AK+SK env resolution
    let (ok, _, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            top_dd.path().to_string_lossy().into_owned(),
            "media".into(),
            "video".into(),
            "generate".into(),
            "--vendor".into(),
            "kling".into(),
            "--prompt".into(),
            "sunset".into(),
            "--duration".into(),
            "5".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "kling stderr: {stderr}");

    // 4. Runway THROTTLED -> SUCCEEDED
    let (ok, _, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            top_dd.path().to_string_lossy().into_owned(),
            "media".into(),
            "video".into(),
            "generate".into(),
            "--vendor".into(),
            "runway".into(),
            "--prompt".into(),
            "neon".into(),
            "--ratio".into(),
            "1280:720".into(),
            "--base-url".into(),
            base_url.clone(),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "runway stderr: {stderr}");
    assert_eq!(runway_polls.load(Ordering::SeqCst), 2);

    // 5. New API Video > 32 MiB download (33 MiB)
    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            top_dd.path().to_string_lossy().into_owned(),
            "media".into(),
            "video".into(),
            "generate".into(),
            "--vendor".into(),
            "newapi-video".into(),
            "--prompt".into(),
            "big-video".into(),
            "--base-url".into(),
            base_url,
        ],
        envs,
    )
    .await;
    assert!(ok, "newapi >32MiB stderr: {stderr}");
    let big_res: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        big_res["artifacts"][0]["sizeBytes"].as_u64(),
        Some(33 * 1024 * 1024)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_d8_realtime_openai_ga_and_gemini_live() {
    let ws = TempDir::new().unwrap();

    // OpenAI Realtime GA WS mock
    let oa_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let oa_addr = oa_listener.local_addr().unwrap();
    let oa_pcm = STANDARD.encode(b"\x01\x02\x03\x04");
    tokio::spawn(async move {
        let (req_line, hdrs, mut br, mut w) = accept_ws_and_upgrade(&oa_listener).await;
        assert!(req_line.contains("/v1/realtime?model=gpt-realtime"));
        assert!(
            !hdrs
                .iter()
                .any(|h| h.to_ascii_lowercase().starts_with("openai-beta:"))
        );
        send_server_ws_text(&mut w, r#"{"type":"session.created"}"#).await;
        let upd: Value = serde_json::from_str(&read_client_ws_text(&mut br).await).unwrap();
        assert_eq!(upd["session"]["type"], "realtime");
        send_server_ws_text(&mut w, r#"{"type":"session.updated"}"#).await;
        let _item = read_client_ws_text(&mut br).await;
        let _resp_create = read_client_ws_text(&mut br).await;
        send_server_ws_text(
            &mut w,
            &json!({"type":"response.output_audio.delta","delta":oa_pcm}).to_string(),
        )
        .await;
        send_server_ws_text(
            &mut w,
            r#"{"type":"response.output_text.delta","delta":"hello-oa"}"#,
        )
        .await;
        send_server_ws_text(
            &mut w,
            r#"{"type":"response.done","response":{"id":"r1","status":"completed"}}"#,
        )
        .await;
    });

    let envs = vec![
        ("OPENAI_API_KEY".into(), "oa-live-k".into()),
        ("GEMINI_API_KEY".into(), "gem-live-k".into()),
    ];

    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "media".into(),
            "realtime".into(),
            "--vendor".into(),
            "openai".into(),
            "--prompt".into(),
            "hi".into(),
            "--base-url".into(),
            format!("http://{oa_addr}"),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "openai realtime stderr: {stderr}");
    let oa_json: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(oa_json["text"], "hello-oa");
    assert!(oa_json["audio"]["path"].as_str().unwrap().ends_with(".wav"));

    // Gemini Live WS mock
    let gl_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gl_addr = gl_listener.local_addr().unwrap();
    let gl_pcm = STANDARD.encode(b"\x10\x20\x30\x40");
    tokio::spawn(async move {
        let (req_line, hdrs, mut br, mut w) = accept_ws_and_upgrade(&gl_listener).await;
        assert!(req_line.contains("BidiGenerateContent"));
        assert!(
            hdrs.iter()
                .any(|h| h.to_ascii_lowercase().starts_with("x-goog-api-key:")
                    && h.contains("gem-live-k"))
        );
        let setup = read_client_ws_text(&mut br).await;
        assert!(setup.contains("\"setup\""));
        send_server_ws_text(&mut w, r#"{"setupComplete":{}}"#).await;
        let ri = read_client_ws_text(&mut br).await;
        assert!(ri.contains("realtimeInput"));
        assert!(!ri.contains("mediaChunks"));
        send_server_ws_text(
            &mut w,
            &json!({
                "serverContent": {
                    "modelTurn": {
                        "parts": [
                            {"text":"hello-gem"},
                            {"inlineData":{"mimeType":"audio/pcm;rate=24000","data":gl_pcm}}
                        ]
                    },
                    "turnComplete": true
                }
            })
            .to_string(),
        )
        .await;
    });

    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "media".into(),
            "realtime".into(),
            "--vendor".into(),
            "gemini-live".into(),
            "--prompt".into(),
            "hi".into(),
            "--base-url".into(),
            format!("http://{gl_addr}/v1beta"),
        ],
        envs.clone(),
    )
    .await;
    assert!(ok, "gemini live stderr: {stderr}");
    let gl_json: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(gl_json["text"], "hello-gem");
    assert!(gl_json["audio"]["path"].as_str().unwrap().ends_with(".wav"));

    // Gemini Live Vertex (/v1 base URL + projects/.../models/... resource path)
    let vtx_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let vtx_addr = vtx_listener.local_addr().unwrap();
    let vtx_pcm = STANDARD.encode(b"\x20\x30\x40\x50");
    tokio::spawn(async move {
        let (req_line, hdrs, mut br, mut w) = accept_ws_and_upgrade(&vtx_listener).await;
        assert!(
            req_line.starts_with(
                "GET /ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent "
            ),
            "unexpected Vertex WS path: {req_line}"
        );
        assert!(hdrs.iter().any(|h| {
            h.to_ascii_lowercase()
                .starts_with("authorization: bearer vtx-live-k")
        }));
        let setup: Value = serde_json::from_str(&read_client_ws_text(&mut br).await).unwrap();
        assert_eq!(
            setup["setup"]["model"],
            "projects/my-proj/locations/us-central1/publishers/google/models/gemini-live-2.5-flash"
        );
        send_server_ws_text(&mut w, r#"{"setupComplete":{}}"#).await;
        let _ri = read_client_ws_text(&mut br).await;
        send_server_ws_text(
            &mut w,
            &json!({
                "serverContent": {
                    "modelTurn": {
                        "parts": [
                            {"text":"hello-vertex"},
                            {"inlineData":{"mimeType":"audio/pcm;rate=24000","data":vtx_pcm}}
                        ]
                    },
                    "outputTranscription": {"text": "vertex-spoken"},
                    "turnComplete": true
                }
            })
            .to_string(),
        )
        .await;
    });

    let mut vtx_envs = envs;
    vtx_envs.push(("VERTEX_ACCESS_TOKEN".into(), "vtx-live-k".into()));
    let (ok, stdout, stderr) = run_coda_async(
        ws.path().to_path_buf(),
        vec![
            "media".into(),
            "realtime".into(),
            "--vendor".into(),
            "gemini-live-vertex".into(),
            "--model".into(),
            "projects/my-proj/locations/us-central1/publishers/google/models/gemini-live-2.5-flash"
                .into(),
            "--prompt".into(),
            "hello vertex".into(),
            "--base-url".into(),
            format!("http://{vtx_addr}/v1"),
        ],
        vtx_envs,
    )
    .await;
    assert!(ok, "gemini live vertex stderr: {stderr}");
    let vtx_json: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(vtx_json["vendor"], "gemini-live-vertex");
    assert_eq!(vtx_json["text"], "hello-vertex");
    assert_eq!(vtx_json["audioTranscript"], "vertex-spoken");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_d7_kling_route_with_secondary_key_env_succeeds_without_global_sk() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let ws = TempDir::new().unwrap();
    let dd = TempDir::new().unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");
    let b1 = base_url.clone();

    let app = Router::new()
        .route(
            "/v1/videos/text2video",
            post(|headers: HeaderMap| async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap();
                let token = auth.strip_prefix("Bearer ").unwrap();
                let parts: Vec<&str> = token.split('.').collect();
                assert_eq!(parts.len(), 3);
                let claims_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
                let claims: Value = serde_json::from_slice(&claims_bytes).unwrap();
                assert_eq!(claims["iss"], "route-ak-777");
                let expected_sig = URL_SAFE_NO_PAD.encode(coda::media::crypto::hmac_sha256(
                    b"route-sk-777",
                    format!("{}.{}", parts[0], parts[1]).as_bytes(),
                ));
                assert_eq!(parts[2], expected_sig);
                (
                    StatusCode::OK,
                    json!({"code":0,"data":{"task_id":"kl-route-1"}}).to_string(),
                )
            }),
        )
        .route(
            "/v1/videos/text2video/{id}",
            get(move || {
                let b = b1.clone();
                async move {
                    (
                        StatusCode::OK,
                        json!({"code":0,"data":{"task_status":"succeed","task_result":{"videos":[{"url":format!("{b}/dl/kl-route.mp4")}]}}}).to_string(),
                    )
                }
            }),
        )
        .route(
            "/dl/{f}",
            get(|| async { (StatusCode::OK, b"KLING-ROUTE-MP4".to_vec()) }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (ok_init, _, err_init) = run_coda_with_io_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            dd.path().to_string_lossy().into_owned(),
            "init".into(),
            "--route".into(),
            "my-kling".into(),
            "--base-url".into(),
            base_url,
            "--model".into(),
            "kling-v1-6".into(),
            "--api-key-stdin".into(),
        ],
        vec![],
        vec![],
        Some("route-ak-777\n".into()),
    )
    .await;
    assert!(ok_init, "init stderr: {err_init}");

    let (ok, stdout, stderr) = run_coda_with_io_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            dd.path().to_string_lossy().into_owned(),
            "media".into(),
            "video".into(),
            "generate".into(),
            "--vendor".into(),
            "kling".into(),
            "--route".into(),
            "my-kling".into(),
            "--secondary-key-env".into(),
            "MY_ROUTE_SK".into(),
            "--prompt".into(),
            "route-sec".into(),
            "--auth-mode".into(),
            "auto".into(),
        ],
        vec![("MY_ROUTE_SK".into(), "route-sk-777".into())],
        vec![
            "KLING_SECRET_KEY".into(),
            "KLING_SK".into(),
            "KLING_ACCESS_KEY".into(),
            "KLING_AK".into(),
        ],
        None,
    )
    .await;
    assert!(ok, "kling route --secondary-key-env stderr: {stderr}");
    let rec: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rec["vendor"], "kling");
    assert_eq!(rec["model"], "kling-v1-6");
    assert_eq!(rec["task"]["status"], "succeeded");
    assert_eq!(rec["task"]["route_id"], "my-kling");
    assert_eq!(rec["artifacts"].as_array().unwrap().len(), 1);

    // Missing named --secondary-key-env MUST fail clearly
    let (ok_missing, out_missing, err_missing) = run_coda_with_io_async(
        ws.path().to_path_buf(),
        vec![
            "--data-dir".into(),
            dd.path().to_string_lossy().into_owned(),
            "media".into(),
            "video".into(),
            "generate".into(),
            "--vendor".into(),
            "kling".into(),
            "--route".into(),
            "my-kling".into(),
            "--secondary-key-env".into(),
            "NONEXISTENT_KLING_SK".into(),
            "--prompt".into(),
            "route-sec".into(),
            "--auth-mode".into(),
            "auto".into(),
        ],
        vec![],
        vec!["NONEXISTENT_KLING_SK".into()],
        None,
    )
    .await;
    assert!(!ok_missing);
    assert!(
        out_missing.contains("NONEXISTENT_KLING_SK")
            || err_missing.contains("NONEXISTENT_KLING_SK")
    );
}
