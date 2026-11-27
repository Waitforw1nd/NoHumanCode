//! D.6 Music adapters across Engines 2, 3, and 4:
//! 1. MiniMax Music (Engine 2 sync / SSE ONLY: `POST /v1/music_generation`, NO `/v1/query/music_generation`)
//! 2. ElevenLabs Music (Engine 2: `/v1/music`, `/v1/music/stream`, `/v1/music/detailed`, `/v1/music/plan`)
//! 3. Google Lyria (3 interfaces):
//!    - Lyria 3 Pro/Clip (Engine 2: `POST /v1beta/interactions` / Vertex `.../interactions`, `store: false`)
//!    - Lyria 2 (`lyria-002`, Engine 2: Vertex `POST .../publishers/google/models/lyria-002:predict`)
//!    - Lyria RealTime (`lyria-realtime-exp`, Engine 4 WS: `BidiGenerateMusic`, 48kHz stereo PCM -> WAV)
//! 4. Suno (New API proxy, Engine 3, explicitly disabled by default behind `allow_suno_proxy`)
//! 5. Coda / TreeAPI Unified Music Gateway (Engine 3, strictly isolated from `new-api`)

use super::engine2::{
    Engine2Request, MEDIA_DEFAULT_ARTIFACT_LIMIT, MEDIA_MAX_DOWNLOAD_REDIRECTS, MediaHttpClient,
    StreamControl, check_standard_business_status, decode_base64_bytes, decode_hex,
    parse_multipart_mixed, pcm16_to_wav, validate_download_hop_url,
};
use super::engine3::{
    Engine3Adapter, Engine3Completion, Engine3Runner, Engine3SubmitInput, MediaOutputSource,
    PollOutcome, SubmitOutcome,
};
use super::tts::{build_minimax_url, ensure_auth_kind, header_request_id};
use super::ws::{WsConnectConfig, WsMessage, connect_ws};
use super::{
    MediaArtifactRef, MediaArtifactStore, MediaAuthKind, MediaCredential, MediaEndpointConfig,
    MediaError, MediaErrorCode, MediaKind, MediaTaskRecord, MediaTaskRepository, MediaTaskStatus,
    http_url_to_ws_url, is_loopback_host, normalize_gemini_model_id, validate_media_url,
    validate_path_segment,
};
use crate::redaction::CredentialSet;
use bytes::Bytes;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MusicArtifact {
    pub artifact: MediaArtifactRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MusicGenerationResponse {
    pub vendor: String,
    pub model: String,
    pub tracks: Vec<MusicArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

fn detect_music_audio_mime_and_ext(
    bytes: &[u8],
    fallback: (&'static str, &'static str),
) -> (&'static str, &'static str) {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        ("audio/wav", "wav")
    } else if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0)
    {
        if bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xF6) == 0xF0 {
            ("audio/aac", "aac")
        } else {
            ("audio/mpeg", "mp3")
        }
    } else if bytes.starts_with(b"OggS") {
        ("audio/ogg", "ogg")
    } else if bytes.starts_with(b"fLaC") {
        ("audio/flac", "flac")
    } else {
        fallback
    }
}

fn mime_to_ext(mime: &str) -> &'static str {
    let base = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/flac" => "flac",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/aac" => "aac",
        "audio/mp4" | "audio/m4a" => "m4a",
        "audio/pcm" | "audio/l16" => "pcm",
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        _ => "bin",
    }
}

// ============================================================================
// 1. MiniMax Music (Engine 2 sync / SSE ONLY — NO /v1/query/music_generation)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiniMaxMusicFormat {
    Mp3,
    Wav,
    Pcm,
}

impl MiniMaxMusicFormat {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mp3" => Some(Self::Mp3),
            "wav" => Some(Self::Wav),
            "pcm" => Some(Self::Pcm),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Pcm => "pcm",
        }
    }

    pub const fn mime_and_ext(self) -> (&'static str, &'static str) {
        match self {
            Self::Mp3 => ("audio/mpeg", "mp3"),
            Self::Wav => ("audio/wav", "wav"),
            Self::Pcm => ("audio/pcm", "pcm"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiniMaxMusicOutputFormat {
    Hex,
    Url,
}

#[derive(Clone, Debug, Default)]
pub struct MiniMaxMusicRequest {
    pub prompt: Option<String>,
    pub lyrics: Option<String>,
    pub is_instrumental: Option<bool>,
    pub lyrics_optimizer: Option<bool>,
    pub sample_rate: Option<u32>,
    pub bitrate: Option<u32>,
    pub format: Option<MiniMaxMusicFormat>,
    pub stream: bool,
    pub output_format: Option<MiniMaxMusicOutputFormat>,
    pub allow_http_loopback_download: bool,
}

pub fn build_minimax_music_payload(
    model: &str,
    req: &MiniMaxMusicRequest,
) -> Result<Value, MediaError> {
    if model.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "MiniMax Music requires a non-empty model",
        ));
    }
    let has_prompt = req.prompt.as_deref().is_some_and(|s| !s.trim().is_empty());
    let has_lyrics = req.lyrics.as_deref().is_some_and(|s| !s.trim().is_empty());
    let instrumental = req.is_instrumental == Some(true);
    let optimizer = req.lyrics_optimizer == Some(true);

    if !has_prompt && !has_lyrics && !(instrumental && optimizer) {
        return Err(MediaError::invalid_request(
            "MiniMax Music requires prompt or lyrics",
        ));
    }
    if req.stream && req.output_format == Some(MiniMaxMusicOutputFormat::Url) {
        return Err(MediaError::invalid_request(
            "MiniMax Music stream=true requires output_format=\"hex\"",
        ));
    }

    let output_format = if req.stream {
        "hex"
    } else {
        match req.output_format.unwrap_or(MiniMaxMusicOutputFormat::Hex) {
            MiniMaxMusicOutputFormat::Hex => "hex",
            MiniMaxMusicOutputFormat::Url => "url",
        }
    };
    let format = req.format.unwrap_or(MiniMaxMusicFormat::Mp3);
    let mut body = json!({
        "model": model,
        "stream": req.stream,
        "output_format": output_format,
        "audio_setting": {
            "sample_rate": req.sample_rate.unwrap_or(44_100),
            "bitrate": req.bitrate.unwrap_or(256_000),
            "format": format.as_str(),
        }
    });
    if let Some(prompt) = req
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["prompt"] = json!(prompt);
    }
    if let Some(lyrics) = req
        .lyrics
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["lyrics"] = json!(lyrics);
    }
    if let Some(inst) = req.is_instrumental {
        body["is_instrumental"] = json!(inst);
    }
    if let Some(opt) = req.lyrics_optimizer {
        body["lyrics_optimizer"] = json!(opt);
    }
    Ok(body)
}

/// Engine 2 MiniMax Music (`POST /v1/music_generation`, sync JSON or SSE stream).
pub async fn minimax_music<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &MiniMaxMusicRequest,
    cancel: &CancellationToken,
) -> Result<MusicGenerationResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "MiniMax Music",
    )?;
    let model = if endpoint.model.trim().is_empty() {
        "music-2.5"
    } else {
        endpoint.model.trim()
    };
    let url = build_minimax_url(
        &endpoint.base_url,
        "/music_generation",
        endpoint.embedded_proxy,
    )?;
    let (headers, sensitive, keys) = cred.materialize_headers(super::engine2::current_unix_i64())?;
    let payload = build_minimax_music_payload(model, req)?;
    let format = req.format.unwrap_or(MiniMaxMusicFormat::Mp3);
    let vendor_tag = if endpoint.vendor.eq_ignore_ascii_case("minimax-cn") {
        "minimax-cn"
    } else {
        "minimax"
    };

    if !req.stream {
        let http_req = Engine2Request::post_json(url, payload).with_headers(headers, sensitive);
        let resp = http
            .execute_json(&http_req, &keys, check_standard_business_status, cancel)
            .await?;

        if let Some(status) = resp.body.pointer("/data/status").and_then(Value::as_i64)
            && status != 2
        {
            return Err(MediaError::business(
                Some(status.to_string()),
                format!("MiniMax Music returned unexpected data.status={status} (expected 2)"),
                false,
            ));
        }
        let audio_val = resp
            .body
            .pointer("/data/audio")
            .or_else(|| resp.body.pointer("/data/audio_url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "MiniMax Music response missing data.audio",
                )
            })?;

        let raw_bytes = if audio_val.starts_with("http://") || audio_val.starts_with("https://") {
            let allow_loopback = req.allow_http_loopback_download
                || url::Url::parse(&endpoint.base_url)
                    .ok()
                    .is_some_and(|u| is_loopback_host(&u));
            let dl_url = validate_download_hop_url(audio_val, allow_loopback)
                .map_err(|e| e.scrubbed(&keys))?;
            let dl_req = Engine2Request::get(dl_url.as_str())
                .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT)
                .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS);
            http.execute_bytes_with_controlled_redirects(&dl_req, allow_loopback, &keys, cancel)
                .await?
                .bytes
                .to_vec()
        } else {
            decode_hex(audio_val)?
        };

        let (mime, ext) = detect_music_audio_mime_and_ext(&raw_bytes, format.mime_and_ext());
        let artifact = artifacts
            .write_artifact(owner_id, ext, mime, &raw_bytes)
            .await?;
        let duration_ms = resp
            .body
            .pointer("/extra_info/music_duration")
            .or_else(|| resp.body.pointer("/extra_info/audio_length"))
            .and_then(Value::as_u64);
        let request_id = resp
            .body
            .get("trace_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| header_request_id(&resp.headers));
        return Ok(MusicGenerationResponse {
            vendor: vendor_tag.to_owned(),
            model: model.to_owned(),
            tracks: vec![MusicArtifact {
                artifact,
                media_type: Some("audio".to_owned()),
                clip_index: Some(0),
                title: None,
                tags: None,
                lyrics: None,
                duration_secs: duration_ms.map(|ms| ms / 1000),
                stem_name: None,
            }],
            text: None,
            metadata: resp.body.get("extra_info").cloned(),
            request_id,
            task_id: None,
        });
    }

    // SSE stream (`stream: true`, `output_format: "hex"`).
    // Track `status == 1` chunks separately from `status == 2` terminal frame to avoid duplication.
    let http_req = Engine2Request::post_json(url, payload).with_headers(headers, sensitive);
    let mut chunks_status1: Vec<u8> = Vec::new();
    let mut final_status2: Option<Vec<u8>> = None;
    let mut saw_status2 = false;
    let mut trace_id: Option<String> = None;
    let mut extra_info: Option<Value> = None;

    http.execute_sse_stream(
        &http_req,
        &keys,
        |ev| {
            let trimmed = ev.data.trim();
            if trimmed.is_empty() || trimmed == "[DONE]" {
                return Ok(StreamControl::Continue);
            }
            let val: Value = serde_json::from_str(trimmed).map_err(|e| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    format!("invalid MiniMax Music SSE JSON: {e}"),
                )
            })?;
            check_standard_business_status(&HeaderMap::new(), &val)?;
            if trace_id.is_none() {
                trace_id = val
                    .get("trace_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
            if extra_info.is_none() && val.get("extra_info").is_some() {
                extra_info = val.get("extra_info").cloned();
            }
            let status = val.pointer("/data/status").and_then(Value::as_i64);
            let hex_audio = val
                .pointer("/data/audio")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            let decoded = if hex_audio.is_empty() {
                Vec::new()
            } else {
                decode_hex(hex_audio)?
            };
            if status == Some(2) {
                saw_status2 = true;
                if !decoded.is_empty() {
                    final_status2 = Some(decoded);
                }
                return Ok(StreamControl::Done);
            }
            if !decoded.is_empty() {
                chunks_status1.extend_from_slice(&decoded);
            }
            Ok(StreamControl::Continue)
        },
        cancel,
    )
    .await?;

    if !saw_status2 {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "MiniMax Music SSE stream ended without terminal data.status == 2 frame",
        ));
    }
    let raw_bytes = if !chunks_status1.is_empty() {
        chunks_status1
    } else {
        final_status2.unwrap_or_default()
    };
    if raw_bytes.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "MiniMax Music SSE stream yielded empty audio",
        ));
    }

    let (mime, ext) = detect_music_audio_mime_and_ext(&raw_bytes, format.mime_and_ext());
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &raw_bytes)
        .await?;
    let duration_secs = extra_info
        .as_ref()
        .and_then(|v| v.get("music_duration").or_else(|| v.get("audio_length")))
        .and_then(Value::as_u64)
        .map(|ms| ms / 1000);

    Ok(MusicGenerationResponse {
        vendor: vendor_tag.to_owned(),
        model: model.to_owned(),
        tracks: vec![MusicArtifact {
            artifact,
            media_type: Some("audio".to_owned()),
            clip_index: Some(0),
            title: None,
            tags: None,
            lyrics: None,
            duration_secs,
            stem_name: None,
        }],
        text: None,
        metadata: extra_info,
        request_id: trace_id,
        task_id: None,
    })
}

// ============================================================================
// 2. ElevenLabs Music (Engine 2: /v1/music, /stream, /detailed, /plan)
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ElevenLabsMusicMode {
    #[default]
    Compose,
    Stream,
    Detailed,
}

#[derive(Clone, Debug, Default)]
pub struct ElevenLabsMusicRequest {
    pub prompt: Option<String>,
    pub composition_plan: Option<Value>,
    pub music_length_ms: Option<u32>,
    pub force_instrumental: Option<bool>,
    pub respect_sections_durations: Option<bool>,
    pub output_format: Option<String>,
    pub mode: ElevenLabsMusicMode,
}

#[derive(Clone, Debug, Default)]
pub struct ElevenLabsMusicPlanRequest {
    pub prompt: String,
    pub music_length_ms: Option<u32>,
}

pub fn build_elevenlabs_music_url(
    base_url: &str,
    suffix: &str,
    output_format: Option<&str>,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    let mut full = if embedded_proxy
        || url
            .path()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/v1")
    {
        format!("{trimmed}/music{suffix}")
    } else {
        format!("{trimmed}/v1/music{suffix}")
    };
    if let Some(fmt) = output_format.map(str::trim).filter(|s| !s.is_empty()) {
        let mut parsed = url::Url::parse(&full)
            .map_err(|_| MediaError::invalid_request("invalid ElevenLabs Music URL"))?;
        parsed.query_pairs_mut().append_pair("output_format", fmt);
        full = parsed.to_string();
    }
    Ok(full)
}

fn validate_elevenlabs_music_length(ms: u32) -> Result<(), MediaError> {
    if !(3_000..=600_000).contains(&ms) {
        return Err(MediaError::invalid_request(
            "ElevenLabs music_length_ms must be between 3000 and 600000",
        ));
    }
    Ok(())
}

pub fn build_elevenlabs_music_payload(
    model: &str,
    req: &ElevenLabsMusicRequest,
) -> Result<Value, MediaError> {
    let prompt = req
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_plan = req.composition_plan.is_some();
    match (prompt, has_plan) {
        (Some(_), true) => {
            return Err(MediaError::invalid_request(
                "ElevenLabs Music prompt and composition_plan are mutually exclusive",
            ));
        }
        (None, false) => {
            return Err(MediaError::invalid_request(
                "ElevenLabs Music requires either prompt or composition_plan",
            ));
        }
        _ => {}
    }
    if req.music_length_ms.is_some() && has_plan {
        return Err(MediaError::invalid_request(
            "ElevenLabs Music music_length_ms is only valid with prompt",
        ));
    }
    if let Some(ms) = req.music_length_ms {
        validate_elevenlabs_music_length(ms)?;
    }

    let model_id = if model.trim().is_empty() {
        "music_v1"
    } else {
        model.trim()
    };
    let mut body = json!({ "model_id": model_id });
    if let Some(p) = prompt {
        body["prompt"] = json!(p);
    }
    if let Some(plan) = &req.composition_plan {
        body["composition_plan"] = plan.clone();
    }
    if let Some(ms) = req.music_length_ms {
        body["music_length_ms"] = json!(ms);
    }
    if let Some(inst) = req.force_instrumental {
        body["force_instrumental"] = json!(inst);
    }
    if let Some(respect) = req.respect_sections_durations {
        body["respect_sections_durations"] = json!(respect);
    }
    Ok(body)
}

fn elevenlabs_music_mime_ext(
    output_format: Option<&str>,
    bytes: &[u8],
) -> (&'static str, &'static str) {
    if let Some(fmt) = output_format.map(|s| s.trim().to_ascii_lowercase()) {
        if fmt.starts_with("mp3_") {
            return ("audio/mpeg", "mp3");
        } else if fmt.starts_with("pcm_") {
            return ("audio/pcm", "pcm");
        } else if fmt.starts_with("opus_") {
            return ("audio/ogg", "opus");
        } else if fmt.starts_with("ulaw_") {
            return ("audio/basic", "ulaw");
        } else if fmt.starts_with("alaw_") {
            return ("audio/alaw", "alaw");
        }
    }
    detect_music_audio_mime_and_ext(bytes, ("audio/mpeg", "mp3"))
}

pub async fn elevenlabs_music<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &ElevenLabsMusicRequest,
    cancel: &CancellationToken,
) -> Result<MusicGenerationResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::XiApiKey, MediaAuthKind::ProxyToken],
        "ElevenLabs Music",
    )?;
    let suffix = match req.mode {
        ElevenLabsMusicMode::Compose => "",
        ElevenLabsMusicMode::Stream => "/stream",
        ElevenLabsMusicMode::Detailed => "/detailed",
    };
    let url = build_elevenlabs_music_url(
        &endpoint.base_url,
        suffix,
        req.output_format.as_deref(),
        endpoint.embedded_proxy,
    )?;
    let model = if endpoint.model.trim().is_empty() {
        "music_v1"
    } else {
        endpoint.model.trim()
    };
    let payload = build_elevenlabs_music_payload(model, req)?;
    let (headers, sensitive, keys) = cred.materialize_headers(super::engine2::current_unix_i64())?;

    if req.mode != ElevenLabsMusicMode::Detailed {
        let http_req = Engine2Request::post_json(url, payload)
            .with_headers(headers, sensitive)
            .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
        let resp = http.execute_bytes(&http_req, &keys, cancel).await?;
        let (mime, ext) = elevenlabs_music_mime_ext(req.output_format.as_deref(), &resp.bytes);
        let artifact = artifacts
            .write_artifact(owner_id, ext, mime, &resp.bytes)
            .await?;
        return Ok(MusicGenerationResponse {
            vendor: "elevenlabs".to_owned(),
            model: model.to_owned(),
            tracks: vec![MusicArtifact {
                artifact,
                media_type: Some("audio".to_owned()),
                clip_index: Some(0),
                title: None,
                tags: None,
                lyrics: None,
                duration_secs: req.music_length_ms.map(|ms| u64::from(ms / 1000)),
                stem_name: None,
            }],
            text: None,
            metadata: None,
            request_id: header_request_id(&resp.headers),
            task_id: None,
        });
    }

    // `/v1/music/detailed`: multipart/mixed (JSON metadata + audio), with raw audio/JSON fallbacks.
    let http_req = Engine2Request::post_json(url, payload)
        .with_headers(headers, sensitive)
        .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT)
        .with_allow_json_bytes(true);
    let resp = http.execute_bytes(&http_req, &keys, cancel).await?;
    let ct = resp
        .headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let ct_lower = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    let (audio_bytes, audio_ct, metadata): (Vec<u8>, Option<String>, Option<Value>) =
        if ct_lower.starts_with("multipart/") {
            let parts = parse_multipart_mixed(&ct, &resp.bytes)?;
            let mut meta = None;
            let mut audio_part: Option<(Vec<u8>, String)> = None;
            for part in parts {
                let p_ct = part
                    .content_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if (p_ct == "application/json" || p_ct.ends_with("+json")) && meta.is_none() {
                    let parsed: Value = serde_json::from_slice(&part.body).map_err(|e| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            format!("invalid ElevenLabs detailed JSON part: {e}"),
                        )
                    })?;
                    meta = Some(parsed);
                } else if (p_ct.starts_with("audio/") || p_ct == "application/octet-stream")
                    && audio_part.is_none()
                {
                    audio_part = Some((part.body.to_vec(), part.content_type));
                }
            }
            let (bytes, part_ct) = audio_part.ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "ElevenLabs /v1/music/detailed multipart response missing audio part",
                )
            })?;
            (bytes, Some(part_ct), meta)
        } else if ct_lower == "application/json" || ct_lower.ends_with("+json") {
            let parsed: Value = serde_json::from_slice(&resp.bytes).map_err(|e| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    format!("invalid ElevenLabs detailed JSON response: {e}"),
                )
            })?;
            check_standard_business_status(&resp.headers, &parsed)?;
            if let Some(err_val) = parsed.get("error").or_else(|| parsed.get("detail")) {
                let msg = err_val
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| err_val.as_str())
                    .unwrap_or("ElevenLabs Music business error");
                return Err(MediaError::business(None, msg, false));
            }
            let b64 = parsed
                .get("audio")
                .or_else(|| parsed.get("audio_base64"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        "ElevenLabs /v1/music/detailed JSON response missing audio payload",
                    )
                })?;
            let decoded = decode_base64_bytes(b64)?;
            (decoded, None, Some(parsed))
        } else {
            (resp.bytes.to_vec(), Some(ct), None)
        };

    let (mime, ext) = if let Some(ref act) = audio_ct {
        let base_mime = act.split(';').next().unwrap_or("").trim();
        if base_mime.starts_with("audio/") {
            let ext = mime_to_ext(base_mime);
            if ext != "bin" {
                (base_mime.to_owned(), ext)
            } else {
                let (m, e) = elevenlabs_music_mime_ext(req.output_format.as_deref(), &audio_bytes);
                (m.to_owned(), e)
            }
        } else {
            let (m, e) = elevenlabs_music_mime_ext(req.output_format.as_deref(), &audio_bytes);
            (m.to_owned(), e)
        }
    } else {
        let (m, e) = elevenlabs_music_mime_ext(req.output_format.as_deref(), &audio_bytes);
        (m.to_owned(), e)
    };

    let artifact = artifacts
        .write_artifact(owner_id, ext, &mime, &audio_bytes)
        .await?;
    Ok(MusicGenerationResponse {
        vendor: "elevenlabs".to_owned(),
        model: model.to_owned(),
        tracks: vec![MusicArtifact {
            artifact,
            media_type: Some("audio".to_owned()),
            clip_index: Some(0),
            title: metadata
                .as_ref()
                .and_then(|m| m.pointer("/song_metadata/title"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            tags: None,
            lyrics: None,
            duration_secs: req.music_length_ms.map(|ms| u64::from(ms / 1000)),
            stem_name: None,
        }],
        text: None,
        metadata,
        request_id: header_request_id(&resp.headers),
        task_id: None,
    })
}

/// ElevenLabs Music Plan (`POST /v1/music/plan`, returns composition plan JSON).
pub async fn elevenlabs_music_plan(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &ElevenLabsMusicPlanRequest,
    cancel: &CancellationToken,
) -> Result<Value, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::XiApiKey, MediaAuthKind::ProxyToken],
        "ElevenLabs Music Plan",
    )?;
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(MediaError::invalid_request(
            "ElevenLabs Music plan requires a non-empty prompt",
        ));
    }
    if let Some(ms) = req.music_length_ms {
        validate_elevenlabs_music_length(ms)?;
    }
    let url =
        build_elevenlabs_music_url(&endpoint.base_url, "/plan", None, endpoint.embedded_proxy)?;
    let model = if endpoint.model.trim().is_empty() {
        "music_v1"
    } else {
        endpoint.model.trim()
    };
    let mut body = json!({
        "prompt": prompt,
        "model_id": model,
    });
    if let Some(ms) = req.music_length_ms {
        body["music_length_ms"] = json!(ms);
    }
    let (headers, sensitive, keys) = cred.materialize_headers(super::engine2::current_unix_i64())?;
    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;
    Ok(resp.body)
}

// ============================================================================
// 3. Google Lyria (Lyria 3 Interactions, Lyria 2 Predict, Lyria RealTime WS)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct Lyria3Request {
    pub prompt: String,
    pub response_modalities: Vec<String>,
    pub generation_config: Option<Value>,
}

pub fn build_lyria3_interactions_url(
    base_url: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    let path_lower = url.path().trim_end_matches('/').to_ascii_lowercase();
    if embedded_proxy {
        if path_lower.ends_with("/gemini/interactions") || path_lower.ends_with("/interactions") {
            Ok(trimmed.to_owned())
        } else if path_lower.ends_with("/gemini") {
            Ok(format!("{trimmed}/interactions"))
        } else {
            Ok(format!("{trimmed}/gemini/interactions"))
        }
    } else if path_lower.ends_with("/interactions") {
        Ok(trimmed.to_owned())
    } else if (path_lower.contains("/projects/") && path_lower.contains("/locations/"))
        || path_lower.ends_with("/v1beta")
        || path_lower.ends_with("/v1beta1")
    {
        Ok(format!("{trimmed}/interactions"))
    } else {
        Ok(format!("{trimmed}/v1beta/interactions"))
    }
}

/// 3a. Lyria 3 Pro / Clip (Engine 2 sync: `POST /v1beta/interactions` with mandatory `store: false`).
pub async fn lyria3_generate<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &Lyria3Request,
    cancel: &CancellationToken,
) -> Result<MusicGenerationResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[
            MediaAuthKind::XGoogApiKey,
            MediaAuthKind::Bearer,
            MediaAuthKind::ProxyToken,
        ],
        "Google Lyria 3",
    )?;
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(MediaError::invalid_request(
            "Lyria 3 requires a non-empty prompt",
        ));
    }
    let model = if endpoint.model.trim().is_empty() {
        "lyria-3-pro-preview"
    } else {
        normalize_gemini_model_id(endpoint.model.trim())?
    };
    let url = build_lyria3_interactions_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let modalities = if req.response_modalities.is_empty() {
        vec!["AUDIO".to_owned()]
    } else {
        req.response_modalities.clone()
    };
    let mut payload = json!({
        "model": model,
        "input": prompt,
        "response_modalities": modalities,
        "store": false,
    });
    if let Some(cfg) = &req.generation_config {
        payload["generation_config"] = cfg.clone();
    }

    let (headers, sensitive, keys) = cred.materialize_headers(super::engine2::current_unix_i64())?;
    let http_req = Engine2Request::post_json(url, payload).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    if let Some(err_obj) = resp.body.get("error").filter(|v| !v.is_null()) {
        let code = err_obj.get("code").and_then(|v| {
            v.as_str()
                .map(ToOwned::to_owned)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        });
        let msg = err_obj
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Lyria 3 interaction error");
        return Err(MediaError::business(code, msg, false));
    }
    if let Some(status) = resp.body.get("status").and_then(Value::as_str)
        && matches!(status.to_ascii_lowercase().as_str(), "failed" | "cancelled")
    {
        return Err(MediaError::business(
            Some(status.to_owned()),
            format!("Lyria 3 interaction ended with status '{status}'"),
            false,
        ));
    }

    let mut content_items: Vec<&Value> = Vec::new();
    if let Some(outputs) = resp.body.get("outputs").and_then(Value::as_array) {
        content_items.extend(outputs.iter());
    } else if let Some(steps) = resp
        .body
        .pointer("/model_output/steps")
        .and_then(Value::as_array)
    {
        for step in steps {
            if let Some(contents) = step.get("content").and_then(Value::as_array) {
                content_items.extend(contents.iter());
            }
        }
    }

    let mut tracks = Vec::new();
    let mut texts = Vec::new();
    for item in content_items {
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if item_type == "text"
            && let Some(t) = item.get("text").and_then(Value::as_str)
        {
            texts.push(t.to_owned());
        } else if item_type == "audio" || item.get("audio").is_some() {
            let b64 = item
                .get("data")
                .or_else(|| item.pointer("/audio/data"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        "Lyria 3 audio output missing base64 data",
                    )
                })?;
            let mime = item
                .get("mime_type")
                .or_else(|| item.get("mimeType"))
                .or_else(|| item.pointer("/audio/mime_type"))
                .or_else(|| item.pointer("/audio/mimeType"))
                .and_then(Value::as_str)
                .unwrap_or("audio/mp3");
            let bytes = decode_base64_bytes(b64)?;
            let ext = mime_to_ext(mime);
            let ext = if ext == "bin" { "mp3" } else { ext };
            let artifact = artifacts
                .write_artifact(owner_id, ext, mime, &bytes)
                .await?;
            let idx = tracks.len();
            tracks.push(MusicArtifact {
                artifact,
                media_type: Some("audio".to_owned()),
                clip_index: Some(idx),
                title: None,
                tags: None,
                lyrics: None,
                duration_secs: None,
                stem_name: None,
            });
        }
    }

    if tracks.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "Lyria 3 response contained no audio outputs",
        ));
    }

    Ok(MusicGenerationResponse {
        vendor: "lyria3".to_owned(),
        model: model.to_owned(),
        tracks,
        text: if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n"))
        },
        metadata: None,
        request_id: header_request_id(&resp.headers),
        task_id: resp
            .body
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

#[derive(Clone, Debug, Default)]
pub struct Lyria2Request {
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub seed: Option<i64>,
    pub sample_count: Option<u32>,
}

pub fn build_lyria2_predict_url(
    base_url: &str,
    model: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let model_id = if model.trim().is_empty() {
        "lyria-002"
    } else {
        normalize_gemini_model_id(model.trim())?
    };
    let trimmed = url.as_str().trim_end_matches('/');
    let path_trimmed = url.path().trim_end_matches('/');
    if path_trimmed.ends_with(":predict") {
        Ok(trimmed.to_owned())
    } else if embedded_proxy {
        if path_trimmed.to_ascii_lowercase().ends_with("/gemini") {
            Ok(format!(
                "{trimmed}/publishers/google/models/{model_id}:predict"
            ))
        } else {
            Ok(format!(
                "{trimmed}/gemini/publishers/google/models/{model_id}:predict"
            ))
        }
    } else if path_trimmed.is_empty() || path_trimmed == "/" {
        Ok(format!(
            "{trimmed}/v1/publishers/google/models/{model_id}:predict"
        ))
    } else {
        Ok(format!(
            "{trimmed}/publishers/google/models/{model_id}:predict"
        ))
    }
}

/// 3b. Lyria 2 (`lyria-002`, Vertex AI Engine 2 sync: `POST .../publishers/google/models/lyria-002:predict`).
pub async fn lyria2_predict<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &Lyria2Request,
    cancel: &CancellationToken,
) -> Result<MusicGenerationResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "Google Lyria 2 (Vertex AI)",
    )?;
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(MediaError::invalid_request(
            "Lyria 2 requires a non-empty prompt",
        ));
    }
    if req.seed.is_some() && req.sample_count.is_some() {
        return Err(MediaError::invalid_request(
            "Lyria 2 seed and sample_count are mutually exclusive",
        ));
    }
    if req.sample_count == Some(0) {
        return Err(MediaError::invalid_request(
            "Lyria 2 sample_count must be >= 1",
        ));
    }

    let model = if endpoint.model.trim().is_empty() {
        "lyria-002"
    } else {
        normalize_gemini_model_id(endpoint.model.trim())?
    };
    let url = build_lyria2_predict_url(&endpoint.base_url, model, endpoint.embedded_proxy)?;

    let mut instance = json!({ "prompt": prompt });
    if let Some(neg) = req
        .negative_prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        instance["negative_prompt"] = json!(neg);
    }
    if let Some(seed) = req.seed {
        instance["seed"] = json!(seed);
    }
    let mut payload = json!({ "instances": [instance] });
    if let Some(count) = req.sample_count {
        payload["parameters"] = json!({ "sample_count": count });
    }

    let (headers, sensitive, keys) = cred.materialize_headers(super::engine2::current_unix_i64())?;
    let http_req = Engine2Request::post_json(url, payload).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    let predictions = resp
        .body
        .get("predictions")
        .or_else(|| resp.body.pointer("/response/predictions"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "Lyria 2 response missing predictions array",
            )
        })?;

    let mut tracks = Vec::with_capacity(predictions.len());
    for (idx, pred) in predictions.iter().enumerate() {
        let b64 = pred
            .get("bytesBase64Encoded")
            .or_else(|| pred.get("audioContent"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Lyria 2 prediction missing bytesBase64Encoded",
                )
            })?;
        let mime = pred
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("audio/wav");
        let bytes = decode_base64_bytes(b64)?;
        let ext = mime_to_ext(mime);
        let ext = if ext == "bin" { "wav" } else { ext };
        let artifact = artifacts
            .write_artifact(owner_id, ext, mime, &bytes)
            .await?;
        tracks.push(MusicArtifact {
            artifact,
            media_type: Some("audio".to_owned()),
            clip_index: Some(idx),
            title: None,
            tags: None,
            lyrics: None,
            duration_secs: None,
            stem_name: None,
        });
    }

    if tracks.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "Lyria 2 response contained zero predictions",
        ));
    }

    Ok(MusicGenerationResponse {
        vendor: "lyria2".to_owned(),
        model: model.to_owned(),
        tracks,
        text: None,
        metadata: None,
        request_id: header_request_id(&resp.headers),
        task_id: None,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LyriaWeightedPrompt {
    pub text: String,
    pub weight: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyriaMusicGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute_bass: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute_drums: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only_bass_and_drums: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub music_generation_mode: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LyriaRealTimeRequest {
    pub prompts: Vec<LyriaWeightedPrompt>,
    pub config: Option<LyriaMusicGenerationConfig>,
    pub duration_secs: Option<u32>,
    pub max_audio_bytes: Option<usize>,
    pub wrap_wav: bool,
}

impl Default for LyriaRealTimeRequest {
    fn default() -> Self {
        Self {
            prompts: Vec::new(),
            config: None,
            duration_secs: None,
            max_audio_bytes: None,
            wrap_wav: true,
        }
    }
}

pub fn build_lyria_realtime_ws_url(
    base_url: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let mut url = http_url_to_ws_url(base_url, embedded_proxy)?;
    let path = url.path().trim_end_matches('/').to_owned();
    const BIDI_MUSIC_SUFFIX: &str =
        "/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateMusic";
    if path.contains("BidiGenerateMusic") {
        return Ok(url.to_string());
    }
    let new_path = if embedded_proxy && !path.is_empty() && path != "/" {
        if path.to_ascii_lowercase().ends_with("/v1beta") {
            let prefix = &path[..path.len() - 7];
            format!("{prefix}{BIDI_MUSIC_SUFFIX}")
        } else {
            format!("{path}{BIDI_MUSIC_SUFFIX}")
        }
    } else if path.to_ascii_lowercase().ends_with("/v1beta") {
        let prefix = &path[..path.len() - 7];
        format!("{prefix}{BIDI_MUSIC_SUFFIX}")
    } else if path.is_empty() || path == "/" {
        BIDI_MUSIC_SUFFIX.to_owned()
    } else {
        format!("{path}{BIDI_MUSIC_SUFFIX}")
    };
    url.set_path(&new_path);
    Ok(url.to_string())
}

/// 3c. Lyria RealTime (`lyria-realtime-exp`, Engine 4 WS `BidiGenerateMusic`, 48kHz stereo PCM).
pub async fn lyria_realtime_ws<A: MediaArtifactStore>(
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &LyriaRealTimeRequest,
    cancel: &CancellationToken,
) -> Result<MusicGenerationResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[
            MediaAuthKind::XGoogApiKey,
            MediaAuthKind::Bearer,
            MediaAuthKind::ProxyToken,
        ],
        "Google Lyria RealTime",
    )?;
    if req.prompts.is_empty()
        || req
            .prompts
            .iter()
            .any(|p| p.text.trim().is_empty() || !p.weight.is_finite())
    {
        return Err(MediaError::invalid_request(
            "Lyria RealTime requires at least one non-empty prompt with finite weight",
        ));
    }

    let model = if endpoint.model.trim().is_empty() {
        "lyria-realtime-exp"
    } else {
        normalize_gemini_model_id(endpoint.model.trim())?
    };
    let ws_url = build_lyria_realtime_ws_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(super::engine2::current_unix_i64())?;
    let max_bytes = req
        .max_audio_bytes
        .unwrap_or_else(|| (req.duration_secs.unwrap_or(10) as usize).saturating_mul(192_000))
        .min(MEDIA_DEFAULT_ARTIFACT_LIMIT);
    if max_bytes == 0 {
        return Err(MediaError::invalid_request(
            "Lyria RealTime max_audio_bytes / duration must be > 0",
        ));
    }

    let (writer, mut reader) = connect_ws(
        &ws_url,
        WsConnectConfig {
            headers,
            sensitive_headers: sensitive,
            ..Default::default()
        },
        &keys,
        cancel,
    )
    .await?;

    writer
        .send_json(&json!({
            "setup": {
                "model": format!("models/{model}")
            }
        }))
        .await?;

    let turn_timeout = Duration::from_millis(endpoint.timeout_ms.unwrap_or(60_000));
    let deadline = tokio::time::Instant::now() + turn_timeout;

    // Wait for setupComplete before emitting weightedPrompts / PLAY.
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = writer.close(1000, "cancelled").await;
                return Err(MediaError::new(MediaErrorCode::Cancelled, "Lyria RealTime cancelled"));
            }
            _ = tokio::time::sleep_until(deadline) => {
                let _ = writer.close(1000, "timeout").await;
                return Err(MediaError::new(MediaErrorCode::Timeout, "Lyria RealTime timed out awaiting setupComplete"));
            }
            msg = reader.recv() => msg,
        };
        let Some(msg_res) = next else {
            return Err(MediaError::new(
                MediaErrorCode::Transport,
                "Lyria RealTime WebSocket closed before setupComplete",
            ));
        };
        match msg_res.map_err(|e| e.scrubbed(&keys))? {
            WsMessage::Text(txt) => {
                let val: Value = serde_json::from_str(&txt).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("invalid Lyria RealTime setup JSON: {e}"),
                    )
                    .scrubbed(&keys)
                })?;
                if let Some(err_obj) = val.get("error").filter(|v| !v.is_null()) {
                    let msg = err_obj
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Lyria RealTime setup error");
                    return Err(MediaError::business(None, msg, false).scrubbed(&keys));
                }
                if val.get("setupComplete").is_some() || val.get("setup_complete").is_some() {
                    break;
                }
            }
            WsMessage::Binary(bin) => {
                if let Ok(val) = serde_json::from_slice::<Value>(&bin)
                    && (val.get("setupComplete").is_some() || val.get("setup_complete").is_some())
                {
                    break;
                }
            }
            WsMessage::Close(frame) => {
                return Err(map_ws_close_error(frame, "Lyria RealTime", &keys));
            }
            WsMessage::Pong(_) => {}
        }
    }

    writer
        .send_json(&json!({
            "clientContent": {
                "weightedPrompts": req.prompts,
            }
        }))
        .await?;
    if let Some(cfg) = &req.config {
        writer
            .send_json(&json!({
                "musicGenerationConfig": cfg
            }))
            .await?;
    }
    writer
        .send_json(&json!({ "playbackControl": "PLAY" }))
        .await?;

    let mut pcm_buf: Vec<u8> = Vec::new();
    while pcm_buf.len() < max_bytes {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = writer.send_json(&json!({ "playbackControl": "STOP" })).await;
                let _ = writer.close(1000, "cancelled").await;
                return Err(MediaError::new(MediaErrorCode::Cancelled, "Lyria RealTime cancelled"));
            }
            _ = tokio::time::sleep_until(deadline) => {
                let _ = writer.send_json(&json!({ "playbackControl": "STOP" })).await;
                let _ = writer.close(1000, "timeout").await;
                if pcm_buf.is_empty() {
                    return Err(MediaError::new(MediaErrorCode::Timeout, "Lyria RealTime timed out before receiving audio"));
                }
                break;
            }
            msg = reader.recv() => msg,
        };
        let Some(msg_res) = next else {
            break;
        };
        match msg_res.map_err(|e| e.scrubbed(&keys))? {
            WsMessage::Text(txt) => {
                let val: Value = serde_json::from_str(&txt).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("invalid Lyria RealTime audio JSON: {e}"),
                    )
                    .scrubbed(&keys)
                })?;
                if let Some(err_obj) = val.get("error").filter(|v| !v.is_null()) {
                    let msg = err_obj
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Lyria RealTime stream error");
                    return Err(MediaError::business(None, msg, false).scrubbed(&keys));
                }
                let chunks = val
                    .pointer("/serverContent/audioChunks")
                    .or_else(|| val.pointer("/server_content/audio_chunks"))
                    .and_then(Value::as_array);
                if let Some(chunks) = chunks {
                    for chunk in chunks {
                        if let Some(b64) = chunk.get("data").and_then(Value::as_str) {
                            let decoded = decode_base64_bytes(b64)?;
                            let remaining = max_bytes.saturating_sub(pcm_buf.len());
                            if remaining == 0 {
                                break;
                            }
                            let take = decoded.len().min(remaining);
                            pcm_buf.extend_from_slice(&decoded[..take]);
                        }
                    }
                }
                let turn_done = val
                    .pointer("/serverContent/turnComplete")
                    .or_else(|| val.pointer("/server_content/turn_complete"))
                    .and_then(Value::as_bool)
                    == Some(true);
                if turn_done {
                    break;
                }
            }
            WsMessage::Binary(bin) => {
                if let Ok(val) = serde_json::from_slice::<Value>(&bin) {
                    if let Some(chunks) = val
                        .pointer("/serverContent/audioChunks")
                        .or_else(|| val.pointer("/server_content/audio_chunks"))
                        .and_then(Value::as_array)
                    {
                        for chunk in chunks {
                            if let Some(b64) = chunk.get("data").and_then(Value::as_str) {
                                let decoded = decode_base64_bytes(b64)?;
                                let remaining = max_bytes.saturating_sub(pcm_buf.len());
                                let take = decoded.len().min(remaining);
                                pcm_buf.extend_from_slice(&decoded[..take]);
                            }
                        }
                    }
                } else {
                    let remaining = max_bytes.saturating_sub(pcm_buf.len());
                    let take = bin.len().min(remaining);
                    pcm_buf.extend_from_slice(&bin[..take]);
                }
            }
            WsMessage::Close(frame) => {
                if let Some(ref f) = frame
                    && f.code != 1000
                {
                    return Err(map_ws_close_error(frame, "Lyria RealTime", &keys));
                }
                break;
            }
            WsMessage::Pong(_) => {}
        }
    }

    let _ = writer
        .send_json(&json!({ "playbackControl": "STOP" }))
        .await;
    let _ = writer.close(1000, "done").await;

    if pcm_buf.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "Lyria RealTime session produced zero audio bytes",
        ));
    }

    let (final_bytes, mime, ext) = if req.wrap_wav {
        let already_wav =
            pcm_buf.len() >= 12 && &pcm_buf[0..4] == b"RIFF" && &pcm_buf[8..12] == b"WAVE";
        let wav = if already_wav {
            pcm_buf
        } else {
            pcm16_to_wav(&pcm_buf, 48_000, 2)?
        };
        (wav, "audio/wav", "wav")
    } else {
        (pcm_buf, "audio/pcm;rate=48000;channels=2", "pcm")
    };

    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &final_bytes)
        .await?;
    Ok(MusicGenerationResponse {
        vendor: "lyria-realtime".to_owned(),
        model: model.to_owned(),
        tracks: vec![MusicArtifact {
            artifact,
            media_type: Some("audio".to_owned()),
            clip_index: Some(0),
            title: None,
            tags: None,
            lyrics: None,
            duration_secs: req.duration_secs.map(u64::from),
            stem_name: None,
        }],
        text: None,
        metadata: None,
        request_id: None,
        task_id: None,
    })
}

fn map_ws_close_error(
    frame: Option<super::ws::WsCloseFrame>,
    label: &str,
    keys: &CredentialSet,
) -> MediaError {
    match frame {
        Some(f) => MediaError::new(
            MediaErrorCode::Transport,
            format!("{label} WebSocket closed ({}): {}", f.code, f.reason),
        )
        .scrubbed(keys),
        None => MediaError::new(
            MediaErrorCode::Transport,
            format!("{label} WebSocket closed unexpectedly"),
        ),
    }
}

// ============================================================================
// 4. Suno (New API proxy, Engine 3 — EXPLICITLY DISABLED BY DEFAULT)
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SunoAction {
    #[default]
    Music,
    Lyrics,
    Concat,
}

impl SunoAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Music => "music",
            Self::Lyrics => "lyrics",
            Self::Concat => "concat",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "music" | "generate" => Some(Self::Music),
            "lyrics" => Some(Self::Lyrics),
            "concat" => Some(Self::Concat),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SunoMusicRequest {
    pub action: SunoAction,
    pub prompt: Option<String>,
    pub tags: Option<String>,
    pub title: Option<String>,
    pub mv: Option<String>,
    pub make_instrumental: Option<bool>,
    pub gpt_description_prompt: Option<String>,
    pub continue_clip_id: Option<String>,
    pub continue_at: Option<f64>,
    pub clip_id: Option<String>,
    pub use_post_fetch: bool,
    /// Opt-in flag required to invoke Suno New API proxy (defaults to `false`).
    pub allow_suno_proxy: bool,
}

fn ensure_suno_enabled(allow_suno_proxy: bool) -> Result<(), MediaError> {
    if !allow_suno_proxy {
        return Err(MediaError::new(
            MediaErrorCode::Forbidden,
            "Suno New API proxy is disabled by default; set allow_suno_proxy=true (--allow-suno-proxy) to enable",
        ));
    }
    Ok(())
}

pub fn build_suno_submit_payload(req: &SunoMusicRequest) -> Result<Value, MediaError> {
    ensure_suno_enabled(req.allow_suno_proxy)?;
    match req.action {
        SunoAction::Music => {
            let has_prompt = req.prompt.as_deref().is_some_and(|s| !s.trim().is_empty());
            let has_desc = req
                .gpt_description_prompt
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty());
            if !has_prompt && !has_desc {
                return Err(MediaError::invalid_request(
                    "Suno music submit requires prompt or gpt_description_prompt",
                ));
            }
            let mut body = json!({
                "_suno_action": "music",
            });
            if let Some(p) = req
                .prompt
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                body["prompt"] = json!(p);
            }
            if let Some(t) = req.tags.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                body["tags"] = json!(t);
            }
            if let Some(title) = req
                .title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                body["title"] = json!(title);
            }
            if let Some(mv) = req.mv.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                body["mv"] = json!(mv);
            }
            if let Some(inst) = req.make_instrumental {
                body["make_instrumental"] = json!(inst);
            }
            if let Some(desc) = req
                .gpt_description_prompt
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                body["gpt_description_prompt"] = json!(desc);
            }
            if let Some(cid) = req
                .continue_clip_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                body["continue_clip_id"] = json!(cid);
            }
            if let Some(at) = req.continue_at {
                body["continue_at"] = json!(at);
            }
            Ok(body)
        }
        SunoAction::Lyrics => {
            let prompt = req
                .prompt
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    MediaError::invalid_request("Suno lyrics submit requires a non-empty prompt")
                })?;
            Ok(json!({
                "_suno_action": "lyrics",
                "prompt": prompt,
            }))
        }
        SunoAction::Concat => {
            let clip_id = req
                .clip_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    MediaError::invalid_request("Suno concat submit requires clip_id")
                })?;
            Ok(json!({
                "_suno_action": "concat",
                "clip_id": clip_id,
            }))
        }
    }
}

pub struct SunoMusicAdapter {
    base_url: String,
    embedded_proxy: bool,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
    use_post_fetch: bool,
    allow_suno_proxy: bool,
}

impl SunoMusicAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        use_post_fetch: bool,
        allow_suno_proxy: bool,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_suno_proxy_and_auth(allow_suno_proxy, cred, endpoint)?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) =
            cred.materialize_headers(super::engine2::current_unix_i64())?;
        Ok((
            Self {
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                headers,
                sensitive,
                use_post_fetch,
                allow_suno_proxy,
            },
            keys,
        ))
    }

    fn build_url(&self, path: &str) -> Result<String, MediaError> {
        let url = validate_media_url(&self.base_url, self.embedded_proxy)?;
        let trimmed = url.as_str().trim_end_matches('/');
        if !self.embedded_proxy
            && url
                .path()
                .trim_end_matches('/')
                .to_ascii_lowercase()
                .ends_with("/v1")
        {
            let root = &trimmed[..trimmed.len() - 3];
            Ok(format!("{root}{path}"))
        } else {
            Ok(format!("{trimmed}{path}"))
        }
    }
}

fn ensure_suno_proxy_and_auth(
    allow_suno_proxy: bool,
    cred: &MediaCredential,
    endpoint: &MediaEndpointConfig,
) -> Result<(), MediaError> {
    ensure_suno_enabled(allow_suno_proxy)?;
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "Suno New API Proxy",
    )
}

impl Engine3Adapter for SunoMusicAdapter {
    fn vendor(&self) -> &str {
        "suno"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Music
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        ensure_suno_enabled(self.allow_suno_proxy)?;
        let mut body = input.payload.clone();
        let action = body
            .get("_suno_action")
            .and_then(Value::as_str)
            .and_then(SunoAction::parse)
            .unwrap_or(SunoAction::Music);
        if let Some(obj) = body.as_object_mut() {
            obj.remove("_suno_action");
        }
        let path = match action {
            SunoAction::Music => "/suno/submit/music",
            SunoAction::Lyrics => "/suno/submit/lyrics",
            SunoAction::Concat => "/suno/submit/concat",
        };
        let url = self.build_url(path)?;
        Ok(Engine2Request::post_json(url, body)
            .with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        ensure_suno_enabled(self.allow_suno_proxy)?;
        check_suno_envelope(body)?;
        let remote_task_id = body
            .get("data")
            .and_then(|d| {
                d.as_str().map(ToOwned::to_owned).or_else(|| {
                    d.get("task_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
            })
            .or_else(|| {
                body.get("task_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Suno submit response missing remote task id",
                )
            })?;
        Ok(SubmitOutcome::Accepted {
            remote_task_id,
            poll_cursor: Some(if self.use_post_fetch {
                "post_fetch".to_owned()
            } else {
                "get_fetch".to_owned()
            }),
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        ensure_suno_enabled(self.allow_suno_proxy)?;
        let remote_id = task
            .remote_task_id
            .as_deref()
            .ok_or_else(|| MediaError::invalid_request("missing remote_task_id for Suno poll"))?;
        let clean_id = validate_path_segment("Suno task_id", remote_id)?;
        let use_post = self.use_post_fetch || task.poll_cursor.as_deref() == Some("post_fetch");
        if use_post {
            let url = self.build_url("/suno/fetch")?;
            Ok(Engine2Request::post_json(url, json!({ "ids": [clean_id] }))
                .with_headers(self.headers.clone(), self.sensitive.clone()))
        } else {
            let url = self.build_url(&format!("/suno/fetch/{clean_id}"))?;
            Ok(Engine2Request::get(url).with_headers(self.headers.clone(), self.sensitive.clone()))
        }
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
        task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        ensure_suno_enabled(self.allow_suno_proxy)?;
        check_suno_envelope(body)?;
        let item = match body.get("data") {
            Some(Value::Array(arr)) => arr.first().unwrap_or(body),
            Some(Value::Object(_)) => &body["data"],
            _ => body,
        };
        let status_raw = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("PROCESSING")
            .trim()
            .to_ascii_uppercase();

        match status_raw.as_str() {
            "SUCCESS" | "SUCCEEDED" | "COMPLETED" => {
                let is_lyrics = task
                    .request_summary
                    .get("_suno_action")
                    .and_then(Value::as_str)
                    == Some("lyrics")
                    || item.get("action").and_then(Value::as_str) == Some("LYRICS");
                let mut outputs = Vec::new();
                if let Some(clips) = item.get("data").and_then(Value::as_array) {
                    for clip in clips {
                        if let Some(url) = clip
                            .get("audio_url")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            outputs.push(MediaOutputSource::DirectDownload {
                                request: Engine2Request::get(url)
                                    .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT)
                                    .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                                suggested_ext: "mp3".to_owned(),
                                mime_type: "audio/mpeg".to_owned(),
                            });
                        }
                        if let Some(img_url) = clip
                            .get("image_url")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            outputs.push(MediaOutputSource::DirectDownload {
                                request: Engine2Request::get(img_url)
                                    .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT)
                                    .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                                suggested_ext: "png".to_owned(),
                                mime_type: "image/png".to_owned(),
                            });
                        }
                    }
                }
                if outputs.is_empty()
                    && let Some(url) = item
                        .get("audio_url")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                {
                    outputs.push(MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(url)
                            .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT)
                            .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                        suggested_ext: "mp3".to_owned(),
                        mime_type: "audio/mpeg".to_owned(),
                    });
                }
                if outputs.is_empty() && is_lyrics {
                    let bytes = serde_json::to_vec(item)
                        .map_err(|e| MediaError::new(MediaErrorCode::DecodeError, e.to_string()))?;
                    outputs.push(MediaOutputSource::InlineBytes {
                        bytes: Bytes::from(bytes),
                        suggested_ext: "json".to_owned(),
                        mime_type: "application/json".to_owned(),
                    });
                }
                if outputs.is_empty() {
                    return Err(MediaError::new(
                        MediaErrorCode::DecodeError,
                        "Suno task succeeded without downloadable clips or lyrics",
                    ));
                }
                Ok(PollOutcome::Succeeded {
                    outputs,
                    poll_cursor: task.poll_cursor.clone(),
                })
            }
            "FAILED" | "FAILURE" | "ERROR" | "REJECTED" => {
                let msg = item
                    .get("fail_reason")
                    .or_else(|| item.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Suno music generation failed")
                    .to_owned();
                Ok(PollOutcome::Failed {
                    upstream_code: Some(status_raw),
                    message: msg,
                    retryable: false,
                })
            }
            "QUEUED" | "NOT_START" | "SUBMITTED" | "QUEUEING" => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Queued,
                poll_cursor: None,
                retry_after_hint: None,
            }),
            _ => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Running,
                poll_cursor: None,
                retry_after_hint: None,
            }),
        }
    }
}

fn check_suno_envelope(body: &Value) -> Result<(), MediaError> {
    if let Some(code) = body.get("code") {
        let is_ok = match code {
            Value::String(s) => {
                let l = s.trim().to_ascii_lowercase();
                l.is_empty() || l == "success" || l == "0"
            }
            Value::Number(n) => n.as_i64() == Some(0),
            Value::Null => true,
            _ => false,
        };
        if !is_ok {
            let msg = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Suno proxy business error");
            return Err(MediaError::business(Some(code.to_string()), msg, false));
        }
    }
    Ok(())
}

pub async fn suno_music_submit_and_wait<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &SunoMusicRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<Engine3Completion, MediaError> {
    let (adapter, keys) =
        SunoMusicAdapter::from_endpoint(endpoint, cred, req.use_post_fetch, req.allow_suno_proxy)?;
    let payload = build_suno_submit_payload(req)?;
    runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model: if endpoint.model.trim().is_empty() {
                    "suno".to_owned()
                } else {
                    endpoint.model.clone()
                },
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await
}

// ============================================================================
// 5. Coda / TreeAPI Unified Music Gateway (STRICTLY ISOLATED from new-api)
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodaMusicGatewayMode {
    #[default]
    CodaGateway,
    PartnerUpstream,
}

impl CodaMusicGatewayMode {
    pub fn as_cursor(self) -> &'static str {
        match self {
            Self::CodaGateway => "coda",
            Self::PartnerUpstream => "partner",
        }
    }

    pub fn from_cursor(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim) {
            Some("coda") => Some(Self::CodaGateway),
            Some("partner") => Some(Self::PartnerUpstream),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CodaMusicGatewayRequest {
    pub action: String,
    pub prompt: Option<String>,
    pub params: Option<Value>,
    pub source_upload: Option<String>,
    pub source_uploads: Vec<String>,
    pub source_artifact: Option<String>,
    pub source_artifacts: Vec<String>,
    pub mode: CodaMusicGatewayMode,
}

pub fn build_coda_music_payload(
    model: &str,
    req: &CodaMusicGatewayRequest,
) -> Result<Value, MediaError> {
    let action = req.action.trim();
    if action.is_empty() {
        return Err(MediaError::invalid_request(
            "Coda/TreeAPI music gateway requires a non-empty action",
        ));
    }
    let mut body = json!({
        "action": action,
    });
    if !model.trim().is_empty() {
        body["model"] = json!(model.trim());
    }
    if let Some(p) = req
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["prompt"] = json!(p);
    }
    if let Some(params) = &req.params {
        body["params"] = params.clone();
    }
    if let Some(su) = &req.source_upload {
        body["source_upload"] = json!(su);
    }
    if !req.source_uploads.is_empty() {
        body["source_uploads"] = json!(req.source_uploads);
    }
    if let Some(sa) = &req.source_artifact {
        body["source_artifact"] = json!(sa);
    }
    if !req.source_artifacts.is_empty() {
        body["source_artifacts"] = json!(req.source_artifacts);
    }
    Ok(body)
}

pub struct CodaMusicGatewayAdapter {
    base_url: String,
    embedded_proxy: bool,
    mode: CodaMusicGatewayMode,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
}

impl CodaMusicGatewayAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        mode: CodaMusicGatewayMode,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "Coda / TreeAPI Music Gateway",
        )?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) =
            cred.materialize_headers(super::engine2::current_unix_i64())?;
        Ok((
            Self {
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                mode,
                headers,
                sensitive,
            },
            keys,
        ))
    }

    fn effective_mode(&self, task: &MediaTaskRecord) -> CodaMusicGatewayMode {
        CodaMusicGatewayMode::from_cursor(task.poll_cursor.as_deref()).unwrap_or(self.mode)
    }

    fn build_gateway_url(
        &self,
        mode: CodaMusicGatewayMode,
        suffix: &str,
    ) -> Result<String, MediaError> {
        let url = validate_media_url(&self.base_url, self.embedded_proxy)?;
        let trimmed = url.as_str().trim_end_matches('/');
        let path_lower = url.path().trim_end_matches('/').to_ascii_lowercase();
        let root = if !self.embedded_proxy && path_lower.ends_with("/v1") {
            &trimmed[..trimmed.len() - 3]
        } else {
            trimmed
        };
        let prefix = match mode {
            CodaMusicGatewayMode::CodaGateway => "/v1/music/tasks",
            CodaMusicGatewayMode::PartnerUpstream => "/music/v1/tasks",
        };
        Ok(format!("{root}{prefix}{suffix}"))
    }

    fn build_idempotency_lookup_url(
        &self,
        mode: CodaMusicGatewayMode,
        idempotency_key: &str,
    ) -> Result<String, MediaError> {
        let url = validate_media_url(&self.base_url, self.embedded_proxy)?;
        let trimmed = url.as_str().trim_end_matches('/');
        let path_lower = url.path().trim_end_matches('/').to_ascii_lowercase();
        let root = if !self.embedded_proxy && path_lower.ends_with("/v1") {
            &trimmed[..trimmed.len() - 3]
        } else {
            trimmed
        };
        match mode {
            CodaMusicGatewayMode::CodaGateway => {
                let clean = validate_path_segment("idempotency_key", idempotency_key)?;
                Ok(format!("{root}/v1/music/requests/{clean}"))
            }
            CodaMusicGatewayMode::PartnerUpstream => {
                let mut parsed = url::Url::parse(&format!("{root}/api/partner/coda/v1/tasks"))
                    .map_err(|_| MediaError::invalid_request("invalid partner lookup URL"))?;
                parsed
                    .query_pairs_mut()
                    .append_pair("idempotency_key", idempotency_key);
                Ok(parsed.to_string())
            }
        }
    }
}

impl Engine3Adapter for CodaMusicGatewayAdapter {
    fn vendor(&self) -> &str {
        "coda-music-gateway"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Music
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let url = self.build_gateway_url(self.mode, "")?;
        let mut req = Engine2Request::post_json(url, input.payload.clone())
            .with_headers(self.headers.clone(), self.sensitive.clone());
        if let Some(key) = input.idempotency_key.as_deref() {
            req.headers
                .push(("Idempotency-Key".to_owned(), key.to_owned()));
        }
        Ok(req)
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        check_standard_business_status(&HeaderMap::new(), body)?;
        let remote_task_id = body
            .get("task_id")
            .or_else(|| body.pointer("/data/task_id"))
            .or_else(|| body.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Coda/TreeAPI music submit response missing task_id",
                )
            })?
            .to_owned();
        Ok(SubmitOutcome::Accepted {
            remote_task_id,
            poll_cursor: Some(self.mode.as_cursor().to_owned()),
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("missing remote_task_id for Coda music poll")
        })?;
        let clean_id = validate_path_segment("Coda music task_id", remote_id)?;
        let mode = self.effective_mode(task);
        let url = self.build_gateway_url(mode, &format!("/{clean_id}"))?;
        Ok(Engine2Request::get(url).with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
        task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        check_standard_business_status(&HeaderMap::new(), body)?;
        if let Some(resp_tid) = body.get("task_id").and_then(Value::as_str)
            && let Some(expected) = task.remote_task_id.as_deref()
            && resp_tid != expected
        {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!("Coda music poll task_id mismatch: expected {expected}, got {resp_tid}"),
            ));
        }

        let status_raw = body
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("processing")
            .trim()
            .to_ascii_lowercase();

        match status_raw.as_str() {
            "succeeded" | "completed" | "success" => {
                let action = body
                    .get("action")
                    .or_else(|| task.request_summary.get("action"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if matches!(action.as_str(), "lyrics" | "boost_style") {
                    let result_obj = body.get("result").unwrap_or(body);
                    let has_lyrics = result_obj
                        .get("lyrics")
                        .and_then(Value::as_array)
                        .is_some_and(|a| !a.is_empty());
                    let has_text = result_obj
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.trim().is_empty());
                    if !has_lyrics && !has_text {
                        return Err(MediaError::business(
                            Some("E_RESULT_MISSING".to_owned()),
                            format!("Coda music '{action}' completed without lyrics/text result"),
                            false,
                        ));
                    }
                    let bytes = serde_json::to_vec(result_obj)
                        .map_err(|e| MediaError::new(MediaErrorCode::DecodeError, e.to_string()))?;
                    return Ok(PollOutcome::Succeeded {
                        outputs: vec![MediaOutputSource::InlineBytes {
                            bytes: Bytes::from(bytes),
                            suggested_ext: "json".to_owned(),
                            mime_type: "application/json".to_owned(),
                        }],
                        poll_cursor: task.poll_cursor.clone(),
                    });
                }

                let remote_id = task.remote_task_id.as_deref().unwrap_or("");
                let clean_id = validate_path_segment("Coda music task_id", remote_id)?;
                let mode = self.effective_mode(task);
                let artifacts_url =
                    self.build_gateway_url(mode, &format!("/{clean_id}/artifacts"))?;
                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::AdapterResolve {
                        resolve_request: Engine2Request::get(artifacts_url)
                            .with_headers(self.headers.clone(), self.sensitive.clone()),
                        context: body.clone(),
                    }],
                    poll_cursor: task.poll_cursor.clone(),
                })
            }
            "failed" | "failure" | "error" | "rejected" | "cancelled" | "canceled" | "expired"
            | "blocked" => {
                let code = body
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or(Some(status_raw.clone()));
                let msg = body
                    .pointer("/error/message")
                    .or_else(|| body.get("fail_reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("Coda/TreeAPI music task failed")
                    .to_owned();
                if matches!(status_raw.as_str(), "cancelled" | "canceled") {
                    Ok(PollOutcome::Cancelled { message: msg })
                } else {
                    Ok(PollOutcome::Failed {
                        upstream_code: code,
                        message: msg,
                        retryable: false,
                    })
                }
            }
            "queued" | "pending" | "not_start" | "queueing" => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Queued,
                poll_cursor: None,
                retry_after_hint: None,
            }),
            _ => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Running,
                poll_cursor: None,
                retry_after_hint: None,
            }),
        }
    }

    fn parse_resolved_outputs(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
        context: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<Vec<MediaOutputSource>, MediaError> {
        let artifacts_arr = body
            .get("artifacts")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Coda music /artifacts response missing artifacts array",
                )
            })?;
        if artifacts_arr.is_empty() {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "Coda music /artifacts response contained zero artifacts",
            ));
        }

        let mut seen_keys: HashSet<String> = HashSet::new();
        let mut by_key: HashMap<String, (String, String, String)> = HashMap::new();
        let mut outputs = Vec::with_capacity(artifacts_arr.len());

        for item in artifacts_arr {
            let key = item
                .get("key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        "Coda music artifact entry missing key",
                    )
                })?
                .to_owned();
            if !seen_keys.insert(key.clone()) {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    format!("duplicate artifact key '{key}' in Coda music /artifacts"),
                ));
            }
            let art_type = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("audio")
                .trim()
                .to_ascii_lowercase();
            let content_url = item
                .get("content_url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("Coda music artifact '{key}' missing content_url"),
                    )
                })?
                .to_owned();
            let mime_type = item
                .get("mime_type")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(match art_type.as_str() {
                    "image" => "image/png",
                    "video" => "video/mp4",
                    _ => "audio/mpeg",
                })
                .to_owned();
            let ext = mime_to_ext(&mime_type);
            let ext = if ext == "bin" {
                match art_type.as_str() {
                    "image" => "png",
                    "video" => "mp4",
                    _ => "mp3",
                }
            } else {
                ext
            };

            by_key.insert(key, (art_type, content_url.clone(), mime_type.clone()));
            outputs.push(MediaOutputSource::DirectDownload {
                request: Engine2Request::get(content_url)
                    .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT)
                    .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                suggested_ext: ext.to_owned(),
                mime_type,
            });
        }

        // Cross-check clip/file references declared in task poll result.
        if let Some(clips) = context.pointer("/result/clips").and_then(Value::as_array) {
            for clip in clips {
                for (field, expected_type) in [
                    ("audio_artifact", "audio"),
                    ("image_artifact", "image"),
                    ("video_artifact", "video"),
                ] {
                    if let Some(k) = clip.get(field).and_then(Value::as_str) {
                        let Some((actual_type, _, _)) = by_key.get(k) else {
                            return Err(MediaError::new(
                                MediaErrorCode::DecodeError,
                                format!(
                                    "Coda music missing referenced {field} '{k}' in /artifacts"
                                ),
                            ));
                        };
                        if actual_type != expected_type {
                            return Err(MediaError::new(
                                MediaErrorCode::DecodeError,
                                format!(
                                    "Coda music artifact '{k}' expected type '{expected_type}', got '{actual_type}'"
                                ),
                            ));
                        }
                    }
                }
            }
        }
        if let Some(files) = context.pointer("/result/files").and_then(Value::as_array) {
            for file in files {
                if let Some(k) = file.get("artifact").and_then(Value::as_str)
                    && !by_key.contains_key(k)
                {
                    return Err(MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("Coda music missing referenced file artifact '{k}' in /artifacts"),
                    ));
                }
            }
        }

        Ok(outputs)
    }
}

pub async fn recover_coda_music_by_idempotency_key(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    mode: CodaMusicGatewayMode,
    idempotency_key: &str,
    cancel: &CancellationToken,
) -> Result<Value, MediaError> {
    let (adapter, keys) = CodaMusicGatewayAdapter::from_endpoint(endpoint, cred, mode)?;
    let url = adapter.build_idempotency_lookup_url(mode, idempotency_key)?;
    let req = Engine2Request::get(url).with_headers(adapter.headers, adapter.sensitive);
    let resp = http
        .execute_json(&req, &keys, check_standard_business_status, cancel)
        .await?;
    Ok(resp.body)
}

pub async fn coda_music_gateway_run<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &CodaMusicGatewayRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<Engine3Completion, MediaError> {
    let (adapter, keys) = CodaMusicGatewayAdapter::from_endpoint(endpoint, cred, req.mode)?;
    let payload = build_coda_music_payload(&endpoint.model, req)?;
    runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model: endpoint.model.clone(),
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::engine3::Engine3PollConfig;
    use crate::media::{InMemoryMediaArtifactStore, InMemoryMediaTaskRepository};
    use axum::{
        Router,
        extract::{Path as AxumPath, Query},
        http::{HeaderMap as AxumHeaders, StatusCode},
        routing::{get, post},
    };
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn minimax_music_sync_sse_dedup_and_host_lock() {
        let app = Router::new().route(
            "/v1/music_generation",
            post(|axum::Json(body): axum::Json<Value>| async move {
                let stream = body.get("stream").and_then(Value::as_bool) == Some(true);
                if body.get("prompt").and_then(Value::as_str) == Some("biz-err") {
                    return (
                        StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"base_resp":{"status_code":1004,"status_msg":"music quota exceeded"}}"#
                            .to_owned(),
                    );
                }
                if !stream {
                    let hex_mp3 = "4944330400"; // "ID3\x04\x00"
                    return (
                        StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        json!({
                            "base_resp": {"status_code": 0},
                            "data": {"status": 2, "audio": hex_mp3},
                            "extra_info": {"music_duration": 12000}
                        })
                        .to_string(),
                    );
                }
                assert_eq!(body["output_format"], "hex");
                if body.get("prompt").and_then(Value::as_str) == Some("missing-status2") {
                    let mut h = axum::http::HeaderMap::new();
                    h.insert("content-type", "text/event-stream".parse().unwrap());
                    return (
                        StatusCode::OK,
                        h,
                        "data: {\"base_resp\":{\"status_code\":0},\"data\":{\"status\":1,\"audio\":\"494433\"}}\n\ndata: [DONE]\n\n".to_owned(),
                    );
                }
                let mut h = axum::http::HeaderMap::new();
                h.insert("content-type", "text/event-stream".parse().unwrap());
                // status==1 emits "ID3", status==2 repeats cumulative "ID3XX" -> dedup must keep status==1 only!
                let sse = concat!(
                    "data: {\"base_resp\":{\"status_code\":0},\"data\":{\"status\":1,\"audio\":\"4944\"}}\n\n",
                    "data: {\"base_resp\":{\"status_code\":0},\"data\":{\"status\":1,\"audio\":\"33\"}}\n\n",
                    "data: {\"base_resp\":{\"status_code\":0},\"data\":{\"status\":2,\"audio\":\"4944335858\"}}\n\n",
                );
                (StatusCode::OK, h, sse.to_owned())
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = InMemoryMediaArtifactStore::new();
        let cancel = CancellationToken::new();
        let cred = MediaCredential::bearer("mm-secret").unwrap();
        let endpoint =
            MediaEndpointConfig::new("minimax-cn", format!("http://{addr}"), "music-2.5");

        // Sync hex
        let resp = minimax_music(
            &http,
            &artifacts,
            &endpoint,
            &cred,
            "u1",
            &MiniMaxMusicRequest {
                prompt: Some("ambient piano".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(resp.vendor, "minimax-cn");
        assert_eq!(
            artifacts
                .read_artifact(&resp.tracks[0].artifact)
                .await
                .unwrap(),
            b"\x49\x44\x33\x04\x00".as_slice()
        );

        // Stream=true with output_format=url rejected
        let url_err = minimax_music(
            &http,
            &artifacts,
            &endpoint,
            &cred,
            "u1",
            &MiniMaxMusicRequest {
                prompt: Some("rock".into()),
                stream: true,
                output_format: Some(MiniMaxMusicOutputFormat::Url),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(url_err.code, MediaErrorCode::InvalidRequest);

        // SSE deduplication
        let sse_resp = minimax_music(
            &http,
            &artifacts,
            &endpoint,
            &cred,
            "u1",
            &MiniMaxMusicRequest {
                prompt: Some("jazz".into()),
                stream: true,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            artifacts
                .read_artifact(&sse_resp.tracks[0].artifact)
                .await
                .unwrap(),
            b"ID3".as_slice()
        );

        // SSE missing status==2 rejected
        let no_s2 = minimax_music(
            &http,
            &artifacts,
            &endpoint,
            &cred,
            "u1",
            &MiniMaxMusicRequest {
                prompt: Some("missing-status2".into()),
                stream: true,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(no_s2.code, MediaErrorCode::DecodeError);

        // Business error
        let biz = minimax_music(
            &http,
            &artifacts,
            &endpoint,
            &cred,
            "u1",
            &MiniMaxMusicRequest {
                prompt: Some("biz-err".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(biz.code, MediaErrorCode::BusinessError);

        // Host lock rejects invalid minimax domain
        let bad_ep =
            MediaEndpointConfig::new("minimax", "https://evil.minimax.example.com", "music-2.5");
        let host_err = minimax_music(
            &http,
            &artifacts,
            &bad_ep,
            &cred,
            "u1",
            &MiniMaxMusicRequest {
                prompt: Some("ok".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(host_err.code, MediaErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn elevenlabs_music_modes_multipart_detailed_plan_and_validation() {
        let app = Router::new()
            .route(
                "/v1/music",
                post(|headers: AxumHeaders| async move {
                    assert_eq!(
                        headers.get("xi-api-key").and_then(|v| v.to_str().ok()),
                        Some("xi-key-1")
                    );
                    (StatusCode::OK, b"ID3COMPOSE".to_vec())
                }),
            )
            .route(
                "/v1/music/stream",
                post(|| async { (StatusCode::OK, b"ID3STREAM".to_vec()) }),
            )
            .route(
                "/v1/music/detailed",
                post(|| async {
                    let mut h = axum::http::HeaderMap::new();
                    h.insert(
                        "content-type",
                        "multipart/mixed; boundary=ELMIX".parse().unwrap(),
                    );
                    let body = b"--ELMIX\r\nContent-Type: application/json\r\n\r\n{\"song_metadata\":{\"title\":\"Sky\"}}\r\n--ELMIX\r\nContent-Type: audio/mpeg\r\n\r\nID3DETAILED\r\n--ELMIX--\r\n";
                    (StatusCode::OK, h, body.to_vec())
                }),
            )
            .route(
                "/v1/music/plan",
                post(|axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(body["prompt"], "epic trailer");
                    (StatusCode::OK, axum::Json(json!({"sections":[{"name":"intro"}]})))
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = InMemoryMediaArtifactStore::new();
        let cancel = CancellationToken::new();
        let endpoint = MediaEndpointConfig::new("elevenlabs", format!("http://{addr}"), "music_v1");
        let xi_cred = MediaCredential::xi_api_key("xi-key-1").unwrap();

        // Reject Bearer
        let bearer_err = elevenlabs_music(
            &http,
            &artifacts,
            &endpoint,
            &MediaCredential::bearer("bad").unwrap(),
            "u1",
            &ElevenLabsMusicRequest {
                prompt: Some("hi".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(bearer_err.code, MediaErrorCode::InvalidRequest);

        // Mutual exclusion & length validation
        let both_err = elevenlabs_music(
            &http,
            &artifacts,
            &endpoint,
            &xi_cred,
            "u1",
            &ElevenLabsMusicRequest {
                prompt: Some("a".into()),
                composition_plan: Some(json!({"sections":[]})),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(both_err.code, MediaErrorCode::InvalidRequest);

        let len_with_plan_err = elevenlabs_music(
            &http,
            &artifacts,
            &endpoint,
            &xi_cred,
            "u1",
            &ElevenLabsMusicRequest {
                composition_plan: Some(json!({"sections":[]})),
                music_length_ms: Some(10_000),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(len_with_plan_err.code, MediaErrorCode::InvalidRequest);

        // Compose & Detailed & Plan
        let c = elevenlabs_music(
            &http,
            &artifacts,
            &endpoint,
            &xi_cred,
            "u1",
            &ElevenLabsMusicRequest {
                prompt: Some("chill".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            artifacts
                .read_artifact(&c.tracks[0].artifact)
                .await
                .unwrap(),
            b"ID3COMPOSE".as_slice()
        );

        let d = elevenlabs_music(
            &http,
            &artifacts,
            &endpoint,
            &xi_cred,
            "u1",
            &ElevenLabsMusicRequest {
                prompt: Some("sky".into()),
                mode: ElevenLabsMusicMode::Detailed,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(d.tracks[0].title.as_deref(), Some("Sky"));
        assert_eq!(
            artifacts
                .read_artifact(&d.tracks[0].artifact)
                .await
                .unwrap(),
            b"ID3DETAILED".as_slice()
        );

        let plan = elevenlabs_music_plan(
            &http,
            &endpoint,
            &xi_cred,
            &ElevenLabsMusicPlanRequest {
                prompt: "epic trailer".into(),
                music_length_ms: Some(30_000),
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(plan["sections"][0]["name"], "intro");
    }

    #[tokio::test]
    async fn lyria3_lyria2_and_lyria_realtime_ws_protocols() {
        let app = Router::new()
            .route(
                "/v1beta/interactions",
                post(|headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(
                        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                        Some("goog-key")
                    );
                    assert_eq!(body["store"], false);
                    let b64 = STANDARD.encode(b"ID3LYRIA3");
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "id": "int-1",
                            "outputs": [
                                {"type": "text", "text": "Here is your track"},
                                {"type": "audio", "data": b64, "mime_type": "audio/mp3"}
                            ]
                        })),
                    )
                }),
            )
            .route(
                "/v1/publishers/google/models/lyria-002:predict",
                post(|headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(
                        headers.get("authorization").and_then(|v| v.to_str().ok()),
                        Some("Bearer vertex-oauth")
                    );
                    assert_eq!(body["instances"][0]["prompt"], "synthwave");
                    let b64 = STANDARD.encode(b"RIFFXXXXWAVEfmt ");
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "predictions": [{"bytesBase64Encoded": b64, "mimeType": "audio/wav"}]
                        })),
                    )
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = InMemoryMediaArtifactStore::new();
        let cancel = CancellationToken::new();

        // Lyria 3
        let l3 = lyria3_generate(
            &http,
            &artifacts,
            &MediaEndpointConfig::new("lyria3", format!("http://{addr}"), "lyria-3-pro-preview"),
            &MediaCredential::x_goog_api_key("goog-key").unwrap(),
            "u1",
            &Lyria3Request {
                prompt: "warm acoustic guitar".into(),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(l3.text.as_deref(), Some("Here is your track"));
        assert_eq!(
            artifacts
                .read_artifact(&l3.tracks[0].artifact)
                .await
                .unwrap(),
            b"ID3LYRIA3".as_slice()
        );

        // Lyria 2 rejects XGoogApiKey & seed+sample_count
        let l2_ep = MediaEndpointConfig::new("lyria2", format!("http://{addr}"), "lyria-002");
        assert!(
            lyria2_predict(
                &http,
                &artifacts,
                &l2_ep,
                &MediaCredential::x_goog_api_key("goog-key").unwrap(),
                "u1",
                &Lyria2Request {
                    prompt: "synthwave".into(),
                    ..Default::default()
                },
                &cancel,
            )
            .await
            .is_err()
        );
        let l2_cred = MediaCredential::bearer("vertex-oauth").unwrap();
        assert!(
            lyria2_predict(
                &http,
                &artifacts,
                &l2_ep,
                &l2_cred,
                "u1",
                &Lyria2Request {
                    prompt: "synthwave".into(),
                    seed: Some(42),
                    sample_count: Some(2),
                    ..Default::default()
                },
                &cancel,
            )
            .await
            .is_err()
        );
        let l2 = lyria2_predict(
            &http,
            &artifacts,
            &l2_ep,
            &l2_cred,
            "u1",
            &Lyria2Request {
                prompt: "synthwave".into(),
                sample_count: Some(1),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(l2.tracks.len(), 1);

        // Lyria RealTime WS loopback mock
        let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let pcm_sample = vec![0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let pcm_b64 = STANDARD.encode(&pcm_sample);
        tokio::spawn(async move {
            let (stream, _) = ws_listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut br = BufReader::new(r);
            let mut req_line = String::new();
            br.read_line(&mut req_line).await.unwrap();
            assert!(req_line.contains("BidiGenerateMusic"));
            let mut saw_key = false;
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
                    let mut sha1 = super::super::crypto::sha1_digest(accept_src.as_bytes());
                    let _ = &mut sha1;
                    let accept = STANDARD.encode(sha1);
                    w.write_all(
                        format!(
                            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                }
                if line.to_ascii_lowercase().starts_with("x-goog-api-key:")
                    && line.contains("rt-goog-key")
                {
                    saw_key = true;
                }
            }
            assert!(saw_key);
            // Expect setup frame
            let setup_txt = read_client_ws_text(&mut br).await;
            assert!(setup_txt.contains("\"setup\""));
            send_server_ws_text(&mut w, r#"{"setupComplete":{}}"#).await;
            // Expect weightedPrompts + PLAY
            let prompts_txt = read_client_ws_text(&mut br).await;
            assert!(prompts_txt.contains("weightedPrompts"));
            let play_txt = read_client_ws_text(&mut br).await;
            assert!(play_txt.contains("PLAY"));
            send_server_ws_text(
                &mut w,
                &json!({"serverContent":{"audioChunks":[{"data": pcm_b64}]}}).to_string(),
            )
            .await;
            let _stop = read_client_ws_text(&mut br).await;
        });

        let rt_ep = MediaEndpointConfig::new(
            "lyria-realtime",
            format!("http://{ws_addr}/v1beta"),
            "lyria-realtime-exp",
        );
        let rt = lyria_realtime_ws(
            &artifacts,
            &rt_ep,
            &MediaCredential::x_goog_api_key("rt-goog-key").unwrap(),
            "u1",
            &LyriaRealTimeRequest {
                prompts: vec![LyriaWeightedPrompt {
                    text: "minimal techno".into(),
                    weight: 1.0,
                }],
                max_audio_bytes: Some(8),
                wrap_wav: true,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        let saved = artifacts
            .read_artifact(&rt.tracks[0].artifact)
            .await
            .unwrap();
        assert_eq!(&saved[0..4], b"RIFF");
        assert_eq!(u16::from_le_bytes(saved[22..24].try_into().unwrap()), 2); // stereo
        assert_eq!(
            u32::from_le_bytes(saved[24..28].try_into().unwrap()),
            48_000
        );
        assert_eq!(&saved[44..], &pcm_sample[..]);
    }

    #[tokio::test]
    async fn suno_disabled_by_default_and_coda_gateway_multi_artifact_isolation() {
        let endpoint = MediaEndpointConfig::new("suno", "https://api.example.com", "chirp-v4");
        let cred = MediaCredential::bearer("tok-suno").unwrap();

        // Suno MUST fail with Forbidden when allow_suno_proxy=false
        let disabled_err = SunoMusicAdapter::from_endpoint(&endpoint, &cred, false, false)
            .err()
            .expect("Suno should be disabled by default");
        assert_eq!(disabled_err.code, MediaErrorCode::Forbidden);

        let suno_polls = Arc::new(AtomicUsize::new(0));
        let suno_polls_c = suno_polls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_srv = format!("http://{addr}");
        let base_c = base_srv.clone();

        let app = Router::new()
            .route(
                "/suno/submit/music",
                post(|| async { (StatusCode::OK, json!({"code":"success","data":"suno-1"}).to_string()) }),
            )
            .route(
                "/suno/fetch/{id}",
                get(move |AxumPath(_id): AxumPath<String>| {
                    let b = base_c.clone();
                    let n = suno_polls_c.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if n == 0 {
                            (StatusCode::OK, json!({"code":"success","data":{"status":"PROCESSING"}}).to_string())
                        } else {
                            (StatusCode::OK, json!({"code":"success","data":{"status":"SUCCESS","data":[{"audio_url":format!("{b}/dl/suno.mp3")}]}}).to_string())
                        }
                    }
                }),
            )
            .route(
                "/dl/suno.mp3",
                get(|| async { (StatusCode::OK, b"SUNOMP3".to_vec()) }),
            )
            .route(
                "/v1/music/tasks",
                post(|headers: AxumHeaders| async move {
                    assert_eq!(
                        headers.get("idempotency-key").and_then(|v| v.to_str().ok()),
                        Some("coda-idem-1")
                    );
                    (StatusCode::OK, json!({"task_id":"cm-1","status":"queued"}).to_string())
                }),
            )
            .route(
                "/v1/music/tasks/{id}",
                get(|| async {
                    (
                        StatusCode::OK,
                        json!({
                            "task_id": "cm-1",
                            "action": "generate",
                            "status": "succeeded",
                            "result": {
                                "clips": [{"audio_artifact": "a1", "image_artifact": "i1"}]
                            }
                        })
                        .to_string(),
                    )
                }),
            )
            .route(
                "/v1/music/tasks/{id}/artifacts",
                get({
                    let b = base_srv.clone();
                    move || {
                        let b2 = b.clone();
                        async move {
                            (
                                StatusCode::OK,
                                json!({
                                    "artifacts": [
                                        {"key": "a1", "type": "audio", "content_url": format!("{b2}/dl/coda.mp3"), "mime_type": "audio/mpeg"},
                                        {"key": "i1", "type": "image", "content_url": format!("{b2}/dl/cover.png"), "mime_type": "image/png"}
                                    ]
                                })
                                .to_string(),
                            )
                        }
                    }
                }),
            )
            .route(
                "/v1/music/requests/{key}",
                get(|AxumPath(key): AxumPath<String>| async move {
                    (StatusCode::OK, json!({"task_id":"cm-1","idempotency_key":key}).to_string())
                }),
            )
            .route(
                "/api/partner/coda/v1/tasks",
                get(|Query(q): Query<HashMap<String, String>>| async move {
                    (StatusCode::OK, json!({"task_id":"pm-1","idempotency_key":q.get("idempotency_key")}).to_string())
                }),
            )
            .route(
                "/dl/coda.mp3",
                get(|| async { (StatusCode::OK, b"CODAMP3".to_vec()) }),
            )
            .route(
                "/dl/cover.png",
                get(|| async { (StatusCode::OK, b"COVERPNG".to_vec()) }),
            );

        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let http = MediaHttpClient::new().unwrap();
        let runner = Engine3Runner::new(http.clone(), repo, artifacts.clone()).with_poll_config(
            Engine3PollConfig {
                initial_interval: Duration::from_millis(1),
                max_interval: Duration::from_millis(5),
                ..Default::default()
            },
        );
        let cancel = CancellationToken::new();

        // Suno enabled succeeds
        let suno_ep = MediaEndpointConfig::new("suno", &base_srv, "chirp-v4");
        let suno_res = suno_music_submit_and_wait(
            &runner,
            &suno_ep,
            &cred,
            &SunoMusicRequest {
                prompt: Some("lo-fi beats".into()),
                allow_suno_proxy: true,
                ..Default::default()
            },
            Some("suno-idem-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(suno_res.record.status, MediaTaskStatus::Succeeded);
        assert_eq!(&suno_res.payloads[0].1[..], b"SUNOMP3");

        // Coda Music Gateway multi-artifact + idempotency recovery
        let coda_ep = MediaEndpointConfig::new("coda-music-gateway", &base_srv, "treeapi-v1");
        let coda_res = coda_music_gateway_run(
            &runner,
            &coda_ep,
            &cred,
            &CodaMusicGatewayRequest {
                action: "generate".into(),
                prompt: Some("orchestral".into()),
                mode: CodaMusicGatewayMode::CodaGateway,
                ..Default::default()
            },
            Some("coda-idem-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(coda_res.record.status, MediaTaskStatus::Succeeded);
        assert_eq!(coda_res.payloads.len(), 2);
        assert_eq!(&coda_res.payloads[0].1[..], b"CODAMP3");
        assert_eq!(&coda_res.payloads[1].1[..], b"COVERPNG");

        let rec_coda = recover_coda_music_by_idempotency_key(
            &http,
            &coda_ep,
            &cred,
            CodaMusicGatewayMode::CodaGateway,
            "coda-idem-1",
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(rec_coda["task_id"], "cm-1");

        let rec_partner = recover_coda_music_by_idempotency_key(
            &http,
            &coda_ep,
            &cred,
            CodaMusicGatewayMode::PartnerUpstream,
            "partner-idem-1",
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(rec_partner["task_id"], "pm-1");
    }

    #[tokio::test]
    async fn minimax_music_audio_url_blocks_private_ip_and_redirect_ssrf() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let b1 = base_url.clone();

        let app = Router::new()
            .route(
                "/v1/music_generation",
                post(move |axum::Json(body): axum::Json<Value>| {
                    let b = b1.clone();
                    async move {
                        let prompt = body["prompt"].as_str().unwrap_or("");
                        let audio_url = if prompt == "metadata" {
                            "https://169.254.169.254/latest".to_owned()
                        } else if prompt == "v4compat" {
                            "https://[::10.0.0.1]/internal.mp3".to_owned()
                        } else {
                            format!("{b}/redir-private")
                        };
                        (
                            StatusCode::OK,
                            json!({
                                "base_resp": {"status_code": 0},
                                "data": {"status": 2, "audio_url": audio_url}
                            })
                            .to_string(),
                        )
                    }
                }),
            )
            .route(
                "/redir-private",
                get(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert(
                        "location",
                        "https://[::ffff:169.254.169.254]/meta".parse().unwrap(),
                    );
                    (StatusCode::FOUND, h)
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = InMemoryMediaArtifactStore::new();
        let cred = MediaCredential::bearer("mm-ssrf-secret").unwrap();
        let ep = MediaEndpointConfig::new("minimax", base_url, "music-2.5");
        let cancel = CancellationToken::new();

        for prompt in ["metadata", "v4compat", "redir"] {
            let err = minimax_music(
                &http,
                &artifacts,
                &ep,
                &cred,
                "u1",
                &MiniMaxMusicRequest {
                    prompt: Some(prompt.into()),
                    ..Default::default()
                },
                &cancel,
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, MediaErrorCode::InvalidRequest, "prompt={prompt}");
        }
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

    #[test]
    fn embedded_proxy_lyria_urls_target_gemini_proxy_paths() {
        assert_eq!(
            build_lyria3_interactions_url("http://127.0.0.1:4100/model-proxy/run-1/media", true)
                .unwrap(),
            "http://127.0.0.1:4100/model-proxy/run-1/media/gemini/interactions"
        );
        assert_eq!(
            build_lyria2_predict_url(
                "http://127.0.0.1:4100/model-proxy/run-1/media",
                "lyria-002",
                true
            )
            .unwrap(),
            "http://127.0.0.1:4100/model-proxy/run-1/media/gemini/publishers/google/models/lyria-002:predict"
        );
        assert_eq!(
            build_lyria3_interactions_url("https://generativelanguage.googleapis.com", false)
                .unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/interactions"
        );
        assert_eq!(
            build_lyria2_predict_url(
                "https://us-central1-aiplatform.googleapis.com",
                "lyria-002",
                false
            )
            .unwrap(),
            "https://us-central1-aiplatform.googleapis.com/v1/publishers/google/models/lyria-002:predict"
        );
    }
}
