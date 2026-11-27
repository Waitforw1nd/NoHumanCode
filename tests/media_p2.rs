use axum::{
    Router,
    body::Bytes as AxumBytes,
    extract::Path as AxumPath,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use coda::domain::{Route, Settings};
use coda::store::Store;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn write_mock_exec(dir: &Path, stem: &str, cmd_body: &str, sh_body: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let _ = sh_body;
        let p = dir.join(format!("{stem}.cmd"));
        std::fs::write(&p, format!("@echo off\r\n{cmd_body}\r\n")).unwrap();
        p
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = cmd_body;
        let p = dir.join(stem);
        std::fs::write(&p, format!("#!/bin/sh\n{sh_body}\n")).unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&p, perms).unwrap();
        p
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_media_tts_writes_workspace_artifacts_for_gemini_elevenlabs_azure_minimax() {
    let app = Router::new()
        .route(
            "/v1beta/models/{action}",
            post(|headers: HeaderMap| async move {
                assert_eq!(
                    headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                    Some("gem-cli-key")
                );
                let pcm = vec![0x0Au8, 0x0B, 0x0C, 0x0D];
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "candidates": [{
                            "content": {
                                "parts": [{
                                    "inlineData": {
                                        "mimeType": "audio/L16;codec=pcm;rate=24000",
                                        "data": STANDARD.encode(&pcm),
                                    }
                                }]
                            },
                            "finishReason": "STOP"
                        }]
                    })),
                )
            }),
        )
        .route(
            "/v1/text-to-speech/{voice}",
            post(
                |AxumPath(voice): AxumPath<String>, headers: HeaderMap| async move {
                    assert_eq!(voice, "voice-abc");
                    assert_eq!(
                        headers.get("xi-api-key").and_then(|v| v.to_str().ok()),
                        Some("xi-cli-key")
                    );
                    let mut h = HeaderMap::new();
                    h.insert("content-type", "audio/mpeg".parse().unwrap());
                    (StatusCode::OK, h, AxumBytes::from_static(b"ID3-EL-CLI"))
                },
            ),
        )
        .route(
            "/cognitiveservices/v1",
            post(|headers: HeaderMap, body: AxumBytes| async move {
                assert_eq!(
                    headers
                        .get("ocp-apim-subscription-key")
                        .and_then(|v| v.to_str().ok()),
                    Some("az-cli-key")
                );
                assert_eq!(
                    headers.get("content-type").and_then(|v| v.to_str().ok()),
                    Some("application/ssml+xml")
                );
                assert!(headers.get("user-agent").is_some());
                assert!(String::from_utf8_lossy(&body).contains("<speak"));
                (StatusCode::OK, AxumBytes::from_static(b"ID3-AZ-CLI"))
            }),
        )
        .route(
            "/v1/t2a_v2",
            post(|headers: HeaderMap| async move {
                assert_eq!(
                    headers.get("authorization").and_then(|v| v.to_str().ok()),
                    Some("Bearer mm-cli-key")
                );
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "base_resp": { "status_code": 0 },
                        "data": { "audio": "4944332d4d4d" }
                    })),
                )
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");

    let ws = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_coda");

    // 1. Gemini TTS -> 44-byte RIFF WAV on disk
    let out_gem = Command::new(bin)
        .args([
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "tts",
            "--vendor",
            "gemini",
            "--model",
            "gemini-2.5-flash-preview-tts",
            "--text",
            "hello",
            "--voice",
            "Kore",
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_GEM_KEY",
        ])
        .env("CLI_GEM_KEY", "gem-cli-key")
        .output()
        .unwrap();
    assert!(
        out_gem.status.success(),
        "gemini tts failed: {}",
        String::from_utf8_lossy(&out_gem.stderr)
    );
    let gem_json: Value = serde_json::from_slice(&out_gem.stdout).unwrap();
    let gem_path = ws.path().join(gem_json["audio"]["path"].as_str().unwrap());
    let gem_bytes = std::fs::read(&gem_path).unwrap();
    assert_eq!(&gem_bytes[0..4], b"RIFF");
    assert_eq!(&gem_bytes[8..12], b"WAVE");
    assert_eq!(gem_bytes.len(), 48);

    // 2. ElevenLabs TTS
    let out_el = Command::new(bin)
        .args([
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "tts",
            "--vendor",
            "elevenlabs",
            "--model",
            "eleven_multilingual_v2",
            "--text",
            "hello",
            "--voice",
            "voice-abc",
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_EL_KEY",
        ])
        .env("CLI_EL_KEY", "xi-cli-key")
        .output()
        .unwrap();
    assert!(
        out_el.status.success(),
        "elevenlabs tts failed: {}",
        String::from_utf8_lossy(&out_el.stderr)
    );
    let el_json: Value = serde_json::from_slice(&out_el.stdout).unwrap();
    let el_bytes =
        std::fs::read(ws.path().join(el_json["audio"]["path"].as_str().unwrap())).unwrap();
    assert_eq!(&el_bytes, b"ID3-EL-CLI");

    // 3. Azure TTS
    let out_az = Command::new(bin)
        .args([
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "tts",
            "--vendor",
            "azure",
            "--text",
            "hello azure",
            "--voice",
            "en-US-JennyNeural",
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_AZ_KEY",
        ])
        .env("CLI_AZ_KEY", "az-cli-key")
        .output()
        .unwrap();
    assert!(
        out_az.status.success(),
        "azure tts failed: {}",
        String::from_utf8_lossy(&out_az.stderr)
    );
    let az_json: Value = serde_json::from_slice(&out_az.stdout).unwrap();
    let az_bytes =
        std::fs::read(ws.path().join(az_json["audio"]["path"].as_str().unwrap())).unwrap();
    assert_eq!(&az_bytes, b"ID3-AZ-CLI");

    // 4. MiniMax TTS (hex)
    let out_mm = Command::new(bin)
        .args([
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "tts",
            "--vendor",
            "minimax",
            "--model",
            "speech-2.8-hd",
            "--text",
            "hello minimax",
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_MM_KEY",
        ])
        .env("CLI_MM_KEY", "mm-cli-key")
        .output()
        .unwrap();
    assert!(
        out_mm.status.success(),
        "minimax tts failed: {}",
        String::from_utf8_lossy(&out_mm.stderr)
    );
    let mm_json: Value = serde_json::from_slice(&out_mm.stdout).unwrap();
    let mm_bytes =
        std::fs::read(ws.path().join(mm_json["audio"]["path"].as_str().unwrap())).unwrap();
    assert_eq!(&mm_bytes, b"ID3-MM");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_media_stt_openai_whisper_mock_and_top_data_dir_route() {
    let app = Router::new()
        .route(
            "/v1/audio/transcriptions",
            post(|headers: HeaderMap| async move {
                assert_eq!(
                    headers.get("authorization").and_then(|v| v.to_str().ok()),
                    Some("Bearer oai-stt-tok")
                );
                (StatusCode::OK, r#"{"text":"cli openai stt"}"#)
            }),
        )
        .route(
            "/v1/audio/speech",
            post(|headers: HeaderMap| async move {
                assert_eq!(
                    headers.get("authorization").and_then(|v| v.to_str().ok()),
                    Some("Bearer route-secret-tts")
                );
                let mut h = HeaderMap::new();
                h.insert("content-type", "audio/mpeg".parse().unwrap());
                (StatusCode::OK, h, AxumBytes::from_static(b"ID3-ROUTE-TTS"))
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");

    let ws = tempfile::tempdir().unwrap();
    let audio_file = ws.path().join("speech.mp3");
    std::fs::write(&audio_file, b"ID3-AUDIO").unwrap();
    let bin = env!("CARGO_BIN_EXE_coda");

    // 1. OpenAI STT CLI
    let out_oai = Command::new(bin)
        .args([
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "stt",
            "--vendor",
            "openai",
            "--model",
            "whisper-1",
            "--file",
            audio_file.to_str().unwrap(),
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_OAI_STT",
        ])
        .env("CLI_OAI_STT", "oai-stt-tok")
        .output()
        .unwrap();
    assert!(
        out_oai.status.success(),
        "openai stt failed: {}",
        String::from_utf8_lossy(&out_oai.stderr)
    );
    let oai_json: Value = serde_json::from_slice(&out_oai.stdout).unwrap();
    assert_eq!(oai_json["text"], "cli openai stt");

    // 2. Local whisper.cpp CLI with mock executables
    let mock_dir = tempfile::tempdir().unwrap();
    let model_path = mock_dir.path().join("ggml-small-q5_1.bin");
    std::fs::write(&model_path, b"MODEL").unwrap();
    let ffprobe = write_mock_exec(
        mock_dir.path(),
        "cli_ffprobe",
        "echo {\"format\":{\"duration\":\"4.5\"}}",
        "echo '{\"format\":{\"duration\":\"4.5\"}}'",
    );
    let ffmpeg = write_mock_exec(
        mock_dir.path(),
        "cli_ffmpeg",
        "for %%A in (%*) do set LAST=%%A\r\necho RIFF> \"%LAST%\"",
        "for arg in \"$@\"; do last=\"$arg\"; done\nprintf 'RIFF' > \"$last\"",
    );
    let whisper = write_mock_exec(
        mock_dir.path(),
        "cli_whisper",
        ":loop\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"--output-file\" set OUT=%~2\r\nshift\r\ngoto loop\r\n:done\r\necho cli mock whisper text> \"%OUT%.txt\"",
        "while [ $# -gt 0 ]; do\n  if [ \"$1\" = \"--output-file\" ]; then OUT=\"$2\"; shift 2; continue; fi\n  shift\ndone\nprintf 'cli mock whisper text' > \"${OUT}.txt\"",
    );

    let out_w = Command::new(bin)
        .args([
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "stt",
            "--vendor",
            "whisper",
            "--file",
            audio_file.to_str().unwrap(),
            "--language",
            "en-US",
            "--whisper-bin",
            whisper.to_str().unwrap(),
            "--whisper-model",
            model_path.to_str().unwrap(),
            "--ffmpeg-bin",
            ffmpeg.to_str().unwrap(),
            "--ffprobe-bin",
            ffprobe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out_w.status.success(),
        "whisper cli stt failed: {}",
        String::from_utf8_lossy(&out_w.stderr)
    );
    let w_json: Value = serde_json::from_slice(&out_w.stdout).unwrap();
    assert_eq!(w_json["vendor"], "whisper");
    assert_eq!(w_json["text"], "cli mock whisper text");

    // 3. Top-level --data-dir + --route resolution
    let data_dir = tempfile::tempdir().unwrap();
    let store = Store::open(&data_dir.path().join("coda.sqlite3")).unwrap();
    store
        .save_settings(&Settings {
            workspace: ws.path().to_string_lossy().into_owned(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "tts-route".into(),
                name: "TTS Route".into(),
                base_url: base.clone(),
                model: "gpt-4o-mini-tts".into(),
                max_tokens: 4096,
                parallel_limit: 1,
                key_env: None,
                protocol: None,
            }],
            newapi: None,
        })
        .unwrap();
    store
        .put_secret("route::tts-route", "route-secret-tts")
        .unwrap();
    drop(store);

    let out_route = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "tts",
            "--vendor",
            "openai",
            "--text",
            "via route",
            "--route",
            "tts-route",
        ])
        .output()
        .unwrap();
    assert!(
        out_route.status.success(),
        "route tts failed: {}",
        String::from_utf8_lossy(&out_route.stderr)
    );
    let r_json: Value = serde_json::from_slice(&out_route.stdout).unwrap();
    let r_bytes = std::fs::read(ws.path().join(r_json["audio"]["path"].as_str().unwrap())).unwrap();
    assert_eq!(&r_bytes, b"ID3-ROUTE-TTS");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_media_task_no_wait_status_poll_resume_and_cancel() {
    let poll_calls = Arc::new(AtomicUsize::new(0));
    let poll_c = poll_calls.clone();
    let remote_cancelled = Arc::new(AtomicBool::new(false));
    let rc = remote_cancelled.clone();

    let app = Router::new()
        .route(
            "/v1/tts/create",
            post(|| async {
                let mut h = HeaderMap::new();
                h.insert("content-type", "application/json".parse().unwrap());
                (
                    StatusCode::OK,
                    h,
                    AxumBytes::from_static(br#"{"taskICode":"icode-cli-1"}"#),
                )
            }),
        )
        .route(
            "/v1/tts/getAudioStatus",
            post(move |axum::Json(body): axum::Json<Value>| {
                let n = poll_c.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert_eq!(body["taskICode"], "icode-cli-1");
                    if n == 0 {
                        (StatusCode::OK, r#"{"status":"processing"}"#.to_owned())
                    } else {
                        let data_uri = format!(
                            "data:audio/mpeg;base64,{}",
                            STANDARD.encode(b"ID3-CLI-POLL")
                        );
                        (
                            StatusCode::OK,
                            format!(r#"{{"status":"success","data":{{"data":"{data_uri}"}}}}"#),
                        )
                    }
                }
            }),
        )
        .route(
            "/api/v1/services/audio/asr/transcription",
            post(|| async {
                (
                    StatusCode::OK,
                    r#"{"output":{"task_id":"ds-cli-cancel-1"}}"#,
                )
            }),
        )
        .route(
            "/api/v1/tasks/{id}/cancel",
            post(move |AxumPath(id): AxumPath<String>| {
                rc.store(true, Ordering::SeqCst);
                async move {
                    assert_eq!(id, "ds-cli-cancel-1");
                    (StatusCode::NO_CONTENT, "")
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");

    let ws = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_coda");

    // 1. Submit partner-seed-audio with --no-wait
    let sub_tts = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "tts",
            "--vendor",
            "partner-seed-audio",
            "--model",
            "seed-audio-1",
            "--text",
            "async cli",
            "--no-wait",
            "--idempotency-key",
            "cli-tts-1",
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_PSA_KEY",
        ])
        .env("CLI_PSA_KEY", "psa-tok")
        .output()
        .unwrap();
    assert!(
        sub_tts.status.success(),
        "submit tts --no-wait failed: {}",
        String::from_utf8_lossy(&sub_tts.stderr)
    );
    let sub_rec: Value = serde_json::from_slice(&sub_tts.stdout).unwrap();
    assert_eq!(sub_rec["status"], "submitted");
    let task_id = sub_rec["id"].as_str().unwrap().to_owned();

    // 2. Inspect via `coda media task status`
    let st_out = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--json",
            "media",
            "task",
            "status",
            "--id",
            &task_id,
        ])
        .output()
        .unwrap();
    assert!(st_out.status.success());
    let st_rec: Value = serde_json::from_slice(&st_out.stdout).unwrap();
    assert_eq!(st_rec["id"], task_id);
    assert_eq!(st_rec["remote_task_id"], "icode-cli-1");

    // 3. Poll via `coda media task poll` -> completes and writes artifact
    let poll_out = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "task",
            "poll",
            "--id",
            &task_id,
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_PSA_KEY",
        ])
        .env("CLI_PSA_KEY", "psa-tok")
        .output()
        .unwrap();
    assert!(
        poll_out.status.success(),
        "task poll failed: {}",
        String::from_utf8_lossy(&poll_out.stderr)
    );
    let done_rec: Value = serde_json::from_slice(&poll_out.stdout).unwrap();
    assert_eq!(done_rec["status"], "succeeded");
    let art_path = done_rec["artifacts"][0]["path"].as_str().unwrap();
    assert_eq!(
        std::fs::read(ws.path().join(art_path)).unwrap(),
        b"ID3-CLI-POLL"
    );

    // 4. Submit DashScope Paraformer --no-wait and cancel via `coda media task cancel`
    let sub_stt = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "stt",
            "--vendor",
            "dashscope",
            "--model",
            "paraformer-v2",
            "--audio-url",
            "https://example.com/a.wav",
            "--transport",
            "async",
            "--no-wait",
            "--idempotency-key",
            "cli-stt-cancel-1",
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_DS_KEY",
        ])
        .env("CLI_DS_KEY", "ds-tok")
        .output()
        .unwrap();
    assert!(
        sub_stt.status.success(),
        "submit stt --no-wait failed: {}",
        String::from_utf8_lossy(&sub_stt.stderr)
    );
    let stt_rec: Value = serde_json::from_slice(&sub_stt.stdout).unwrap();
    let stt_id = stt_rec["id"].as_str().unwrap();

    let cancel_out = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--json",
            "media",
            "task",
            "cancel",
            "--id",
            stt_id,
            "--reason",
            "user stop",
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_DS_KEY",
        ])
        .env("CLI_DS_KEY", "ds-tok")
        .output()
        .unwrap();
    assert!(
        cancel_out.status.success(),
        "task cancel failed: {}",
        String::from_utf8_lossy(&cancel_out.stderr)
    );
    let cancel_rec: Value = serde_json::from_slice(&cancel_out.stdout).unwrap();
    assert_eq!(cancel_rec["status"], "cancelled");
    assert!(remote_cancelled.load(Ordering::SeqCst));

    // 5. Submit another partner-seed-audio --no-wait and complete via `coda media task resume`
    let sub_resume = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "tts",
            "--vendor",
            "partner-seed-audio",
            "--model",
            "seed-audio-1",
            "--text",
            "resume cli",
            "--no-wait",
            "--session-id",
            "sess-res-1",
            "--idempotency-key",
            "cli-tts-res-1",
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_PSA_KEY",
        ])
        .env("CLI_PSA_KEY", "psa-tok")
        .output()
        .unwrap();
    assert!(sub_resume.status.success());

    let res_out = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "task",
            "resume",
            "--vendor",
            "partner-seed-audio",
            "--kind",
            "tts",
            "--session-id",
            "sess-res-1",
            "--base-url",
            &base,
            "--api-key-env",
            "CLI_PSA_KEY",
        ])
        .env("CLI_PSA_KEY", "psa-tok")
        .output()
        .unwrap();
    assert!(
        res_out.status.success(),
        "task resume failed: {}",
        String::from_utf8_lossy(&res_out.stderr)
    );
    let res_arr: Value = serde_json::from_slice(&res_out.stdout).unwrap();
    assert_eq!(res_arr.as_array().unwrap().len(), 1);
    assert_eq!(res_arr[0]["ok"], true);
    assert_eq!(res_arr[0]["task"]["status"], "succeeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_media_stt_idempotency_replay_paraformer_json_and_polish() {
    let para_submits = Arc::new(AtomicUsize::new(0));
    let para_sub_c = para_submits.clone();
    let volc_submits = Arc::new(AtomicUsize::new(0));
    let volc_sub_c = volc_submits.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let app = Router::new()
        .route(
            "/api/v1/services/audio/asr/transcription",
            post(move || {
                para_sub_c.fetch_add(1, Ordering::SeqCst);
                async { (StatusCode::OK, r#"{"output":{"task_id":"ds-p2-1"}}"#) }
            }),
        )
        .route(
            "/api/v1/tasks/{id}",
            get(move |AxumPath(id): AxumPath<String>| async move {
                assert_eq!(id, "ds-p2-1");
                (
                    StatusCode::OK,
                    format!(
                        r#"{{"output":{{"task_status":"SUCCEEDED","results":[{{"subtask_status":"SUCCEEDED","transcription_url":"http://127.0.0.1:{}/dl/para-p2.json"}}]}}}}"#,
                        addr.port()
                    ),
                )
            }),
        )
        .route(
            "/dl/para-p2.json",
            get(|| async {
                let mut h = HeaderMap::new();
                h.insert("content-type", "application/json".parse().unwrap());
                (
                    StatusCode::OK,
                    h,
                    r#"{"properties":{"original_duration_in_milliseconds":1500},"transcripts":[{"text":"para json replay ok"}]}"#,
                )
            }),
        )
        .route(
            "/api/v3/auc/bigmodel/submit",
            post(move |headers: HeaderMap| {
                volc_sub_c.fetch_add(1, Ordering::SeqCst);
                async move {
                    let req_id = headers
                        .get("x-api-request-id")
                        .and_then(|v| v.to_str().ok())
                        .unwrap()
                        .to_owned();
                    let mut h = HeaderMap::new();
                    h.insert("x-api-status-code", "20000000".parse().unwrap());
                    h.insert("x-api-request-id", req_id.parse().unwrap());
                    (StatusCode::OK, h, "")
                }
            }),
        )
        .route(
            "/api/v3/auc/bigmodel/query",
            post(|| async {
                let mut h = HeaderMap::new();
                h.insert("x-api-status-code", "20000000".parse().unwrap());
                (
                    StatusCode::OK,
                    h,
                    r#"{"result":{"text":"volc cli replay ok"}}"#,
                )
            }),
        )
        .route(
            "/v1/t2a_async_v2",
            post(|axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(body["audio_setting"]["format"], "wav");
                assert_eq!(body["voice_setting"]["speed"], 1.25);
                (StatusCode::OK, r#"{"base_resp":{"status_code":0},"task_id":"mm-cn-1"}"#)
            }),
        )
        .route(
            "/v1/query/t2a_async_query_v2",
            get(|| async {
                (
                    StatusCode::OK,
                    r#"{"base_resp":{"status_code":0},"status":"Success","file_id":"mm-file-1"}"#,
                )
            }),
        )
        .route(
            "/v1/files/retrieve",
            get(move || async move {
                (
                    StatusCode::OK,
                    format!(
                        r#"{{"base_resp":{{"status_code":0}},"file":{{"download_url":"http://127.0.0.1:{}/dl/mm.wav"}}}}"#,
                        addr.port()
                    ),
                )
            }),
        )
        .route(
            "/dl/mm.wav",
            get(|| async { (StatusCode::OK, AxumBytes::from_static(b"RIFF-CN-WAV")) }),
        );

    let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");
    let ws = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_coda");

    // 1. DashScope Paraformer async STT with Content-Type: application/json + idempotency replay
    for run in 0..2 {
        let out = Command::new(bin)
            .args([
                "--data-dir",
                data_dir.path().to_str().unwrap(),
                "--workspace",
                ws.path().to_str().unwrap(),
                "--json",
                "media",
                "stt",
                "--vendor",
                "dashscope",
                "--model",
                "paraformer-v2",
                "--transport",
                "async",
                "--audio-url",
                "https://example.com/a.wav",
                "--idempotency-key",
                "p2-para-idem-1",
                "--allow-loopback-download",
                "--base-url",
                &base,
                "--api-key-env",
                "P2_DS_KEY",
            ])
            .env("P2_DS_KEY", "ds-p2-tok")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "paraformer run {run} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tr: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(tr["text"], "para json replay ok");
        assert!(tr["rawArtifact"]["path"].as_str().is_some());
    }
    assert_eq!(para_submits.load(Ordering::SeqCst), 1);

    // 2. Volcengine async STT idempotency replay
    for run in 0..2 {
        let out = Command::new(bin)
            .args([
                "--data-dir",
                data_dir.path().to_str().unwrap(),
                "--workspace",
                ws.path().to_str().unwrap(),
                "--json",
                "media",
                "stt",
                "--vendor",
                "volcengine",
                "--model",
                "bigmodel",
                "--transport",
                "async",
                "--audio-url",
                "https://example.com/v.wav",
                "--idempotency-key",
                "p2-volc-idem-1",
                "--base-url",
                &base,
                "--api-key-env",
                "P2_VOLC_KEY",
            ])
            .env("P2_VOLC_KEY", "volc-p2-tok")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "volcengine run {run} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tr: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(tr["text"], "volc cli replay ok");
        assert!(tr["rawArtifact"]["path"].as_str().is_some());
    }
    assert_eq!(volc_submits.load(Ordering::SeqCst), 1);

    // 3. MiniMax-CN async --no-wait preserves vendor + full payload, and poll infers wav format
    let mm_sub = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "tts",
            "--vendor",
            "minimax-cn",
            "--transport",
            "async",
            "--model",
            "speech-2.8-hd",
            "--text",
            "ni hao",
            "--response-format",
            "wav",
            "--speed",
            "1.25",
            "--no-wait",
            "--idempotency-key",
            "p2-mmcn-1",
            "--base-url",
            &base,
            "--api-key-env",
            "P2_MM_KEY",
        ])
        .env("P2_MM_KEY", "mm-cn-tok")
        .output()
        .unwrap();
    assert!(
        mm_sub.status.success(),
        "minimax-cn submit failed: {}",
        String::from_utf8_lossy(&mm_sub.stderr)
    );
    let mm_rec: Value = serde_json::from_slice(&mm_sub.stdout).unwrap();
    assert_eq!(mm_rec["vendor"], "minimax-cn");
    let mm_id = mm_rec["id"].as_str().unwrap();

    let mm_poll = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "task",
            "poll",
            "--id",
            mm_id,
            "--base-url",
            &base,
            "--api-key-env",
            "P2_MM_KEY",
        ])
        .env("P2_MM_KEY", "mm-cn-tok")
        .output()
        .unwrap();
    assert!(
        mm_poll.status.success(),
        "minimax-cn poll failed: {}",
        String::from_utf8_lossy(&mm_poll.stderr)
    );
    let mm_done: Value = serde_json::from_slice(&mm_poll.stdout).unwrap();
    assert_eq!(mm_done["status"], "succeeded");
    assert_eq!(mm_done["artifacts"][0]["mimeType"], "audio/wav");

    // 4. --route + --auth-mode app-access without --secondary-key-env is rejected cleanly
    let store = Store::open(&data_dir.path().join("coda.sqlite3")).unwrap();
    store
        .save_settings(&Settings {
            workspace: ws.path().to_string_lossy().into_owned(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "volc-route".into(),
                name: "Volc Route".into(),
                base_url: base.clone(),
                model: "bigmodel".into(),
                max_tokens: 4096,
                parallel_limit: 1,
                key_env: None,
                protocol: None,
            }],
            newapi: None,
        })
        .unwrap();
    store
        .put_secret("route::volc-route", "route-app-key")
        .unwrap();
    drop(store);

    let bad_auth = Command::new(bin)
        .args([
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "media",
            "stt",
            "--vendor",
            "volcengine",
            "--transport",
            "http",
            "--audio-url",
            "https://example.com/v.wav",
            "--route",
            "volc-route",
            "--auth-mode",
            "app-access",
        ])
        .output()
        .unwrap();
    assert!(!bad_auth.status.success());
    let err_txt = String::from_utf8_lossy(&bad_auth.stderr);
    assert!(err_txt.contains("invalid_request"));
}
