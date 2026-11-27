//! D.5 Speech-to-Text (STT) adapters across Engines 2, 3, and 4 + pure `LocalAsrRunner` seam:
//! 1. OpenAI STT (`POST /v1/audio/transcriptions` & `/v1/audio/translations`, multipart + SSE)
//! 2. ElevenLabs Scribe (`POST /v1/speech-to-text`, `xi-api-key` multipart)
//! 3. Gemini Audio (`POST /v1beta/models/{model}:generateContent` + `/upload/v1beta/files` >20 MiB)
//! 4. DashScope Paraformer (Engine 3 `X-DashScope-Async: enable`) & Qwen-ASR (Engine 2) & Realtime WS (Engine 4)
//! 5. Volcengine ASR (Flash Engine 2, Async Engine 3 `POST /query` `{}` + `X-Api-Status-Code`, SAUC WS Engine 4)
//! 6. Local `whisper.cpp` (`LocalAsrRunner` trait + pure 11-demuxer/ffprobe/ffmpeg/whisper-cli helpers)

use super::engine2::{
    Engine2Request, MediaHttpClient, MultipartForm, StreamControl, check_standard_business_status,
    current_unix_i64,
};
use super::engine3::{
    Engine3Adapter, Engine3Completion, Engine3Runner, Engine3SubmitInput, MediaOutputSource,
    PollOutcome, SubmitOutcome,
};
use super::tts::{build_dashscope_url, volcengine_frame};
use super::vectors::build_gemini_v1beta_base;
use super::ws::{WsConnectConfig, WsMessage, connect_ws};
use super::{
    MediaArtifactRef, MediaArtifactStore, MediaAuthKind, MediaCredential, MediaEndpointConfig,
    MediaError, MediaErrorCode, MediaKind, MediaTaskRecord, MediaTaskRepository, MediaTaskStatus,
    http_url_to_ws_url, is_loopback_host, normalize_gemini_model_id, validate_media_download_url,
    validate_media_url,
};
use crate::redaction::CredentialSet;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub const OPENAI_STT_MAX_BYTES: usize = 25 * 1024 * 1024;
pub const GEMINI_INLINE_AUDIO_MAX_BYTES: usize = 20 * 1024 * 1024;
pub const DASHSCOPE_QWEN_ASR_MAX_DURATION_SECS: f64 = 300.0;

pub const LOCAL_ASR_MAX_SECONDS: f64 = 1200.0;
pub const LOCAL_ASR_MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub const LOCAL_ASR_TOTAL_TIMEOUT_MS: u64 = 600_000;
pub const LOCAL_ASR_FFPROBE_TIMEOUT_MS: u64 = 30_000;
pub const LOCAL_ASR_FFMPEG_TIMEOUT_MS: u64 = 120_000;

#[derive(Clone, Debug)]
pub enum SttAudioSource {
    Bytes {
        filename: String,
        mime_type: String,
        bytes: Bytes,
    },
    Url {
        url: String,
        mime_type: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SttSegment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SttWord {
    pub word: String,
    pub start_secs: f64,
    pub end_secs: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SttTranscript {
    pub vendor: String,
    pub model: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<SttSegment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<SttWord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_artifact: Option<MediaArtifactRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SttStreamEvent {
    Delta {
        text: String,
    },
    Segment {
        text: String,
        is_final: bool,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
    },
    Completed {
        text: String,
        usage: Option<Value>,
    },
}

fn ensure_stt_auth(
    cred: &MediaCredential,
    endpoint: &MediaEndpointConfig,
    allowed: &[MediaAuthKind],
    label: &str,
) -> Result<(), MediaError> {
    if endpoint.embedded_proxy && cred.kind == MediaAuthKind::ProxyToken {
        return Ok(());
    }
    if allowed.contains(&cred.kind) {
        return Ok(());
    }
    Err(MediaError::invalid_request(format!(
        "{label} requires compatible authentication kind (got {:?})",
        cred.kind
    )))
}

// ============================================================================
// 1. OpenAI STT (Engine 2: /v1/audio/transcriptions & /v1/audio/translations)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAiSttGranularity {
    Word,
    Segment,
}

impl OpenAiSttGranularity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Segment => "segment",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenAiSttRequest {
    pub source: Option<SttAudioSource>,
    pub translate: bool,
    pub language: Option<String>,
    pub prompt: Option<String>,
    pub response_format: Option<String>,
    pub temperature: Option<f64>,
    pub timestamp_granularities: Vec<OpenAiSttGranularity>,
    pub stream: bool,
}

pub fn build_openai_stt_url(
    base_url: &str,
    translate: bool,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    let suffix = if translate {
        "audio/translations"
    } else {
        "audio/transcriptions"
    };
    if embedded_proxy
        || url
            .path()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/v1")
    {
        Ok(format!("{trimmed}/{suffix}"))
    } else {
        Ok(format!("{trimmed}/v1/{suffix}"))
    }
}

fn is_gpt_transcribe_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower.starts_with("gpt-4o-transcribe") || lower.starts_with("gpt-transcribe")
}

pub async fn openai_stt(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &OpenAiSttRequest,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    ensure_stt_auth(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "OpenAI STT",
    )?;
    if endpoint.model.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "OpenAI STT model must not be empty",
        ));
    }
    let Some(SttAudioSource::Bytes {
        filename,
        mime_type,
        bytes,
    }) = &req.source
    else {
        return Err(MediaError::invalid_request(
            "OpenAI STT requires local audio file bytes (<= 25 MiB)",
        ));
    };
    if bytes.is_empty() || bytes.len() > OPENAI_STT_MAX_BYTES {
        return Err(MediaError::invalid_request(format!(
            "OpenAI STT audio bytes must be 1..={OPENAI_STT_MAX_BYTES} bytes"
        )));
    }
    if !req.timestamp_granularities.is_empty()
        && req.response_format.as_deref() != Some("verbose_json")
    {
        return Err(MediaError::invalid_request(
            "OpenAI STT timestamp_granularities[] requires response_format=verbose_json",
        ));
    }
    if req.stream {
        if req.translate {
            return Err(MediaError::invalid_request(
                "OpenAI STT streaming is not supported for /v1/audio/translations",
            ));
        }
        if !is_gpt_transcribe_model(&endpoint.model) {
            return Err(MediaError::invalid_request(
                "OpenAI STT stream=true is only supported on gpt-4o-transcribe* / gpt-transcribe* models",
            ));
        }
    }

    let url = build_openai_stt_url(&endpoint.base_url, req.translate, endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let mut form = MultipartForm::new()
        .file("file", filename.as_str(), mime_type.as_str(), bytes.clone())?
        .text("model", endpoint.model.as_str())?;
    if !req.translate
        && let Some(lang) = &req.language
        && !lang.trim().is_empty()
    {
        form = form.text("language", lang.trim())?;
    }
    if let Some(prompt) = &req.prompt
        && !prompt.trim().is_empty()
    {
        form = form.text("prompt", prompt.as_str())?;
    }
    if let Some(rf) = &req.response_format
        && !rf.trim().is_empty()
    {
        form = form.text("response_format", rf.trim())?;
    }
    if let Some(temp) = req.temperature {
        form = form.text("temperature", temp.to_string())?;
    }
    for g in &req.timestamp_granularities {
        form = form.text("timestamp_granularities[]", g.as_str())?;
    }
    if req.stream {
        form = form.text("stream", "true")?;
    }

    let http_req = Engine2Request::post_multipart(url, form).with_headers(headers, sensitive);

    if req.stream {
        let mut text_buf = String::new();
        let mut final_text: Option<String> = None;
        http.execute_sse_stream(
            &http_req,
            &keys,
            |ev| {
                let trimmed = ev.data.trim();
                if trimmed.is_empty() {
                    return Ok(StreamControl::Continue);
                }
                if trimmed == "[DONE]" {
                    return Ok(StreamControl::Done);
                }
                let val: Value = serde_json::from_str(trimmed).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("invalid OpenAI STT SSE JSON: {e}"),
                    )
                })?;
                if let Some(err_obj) = val.get("error") {
                    let msg = err_obj
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("OpenAI STT SSE error");
                    let code = err_obj
                        .get("code")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    return Err(MediaError::business(code, msg, false));
                }
                let ev_type = val
                    .get("type")
                    .and_then(Value::as_str)
                    .or(ev.event.as_deref())
                    .unwrap_or("");
                match ev_type {
                    "transcript.text.delta" => {
                        if let Some(delta) = val.get("delta").and_then(Value::as_str) {
                            text_buf.push_str(delta);
                        }
                    }
                    "transcript.text.done" => {
                        if let Some(t) = val.get("text").and_then(Value::as_str) {
                            final_text = Some(t.to_owned());
                        }
                        return Ok(StreamControl::Done);
                    }
                    _ => {
                        if let Some(delta) = val.get("delta").and_then(Value::as_str) {
                            text_buf.push_str(delta);
                        }
                    }
                }
                Ok(StreamControl::Continue)
            },
            cancel,
        )
        .await?;
        let text = final_text.unwrap_or(text_buf);
        return Ok(SttTranscript {
            vendor: "openai".into(),
            model: endpoint.model.clone(),
            text,
            language: req.language.clone(),
            duration_secs: None,
            segments: Vec::new(),
            words: Vec::new(),
            raw_artifact: None,
        });
    }

    let is_plain_text = matches!(
        req.response_format.as_deref().map(str::trim),
        Some("text" | "srt" | "vtt")
    );
    if is_plain_text {
        let resp = http.execute_bytes(&http_req, &keys, cancel).await?;
        let text = String::from_utf8(resp.bytes.to_vec()).map_err(|e| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                format!("OpenAI STT plain-text response is not UTF-8: {e}"),
            )
        })?;
        return Ok(SttTranscript {
            vendor: "openai".into(),
            model: endpoint.model.clone(),
            text,
            language: req.language.clone(),
            duration_secs: None,
            segments: Vec::new(),
            words: Vec::new(),
            raw_artifact: None,
        });
    }

    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;
    parse_openai_stt_json(&endpoint.model, &resp.body)
}

fn parse_openai_stt_json(model: &str, body: &Value) -> Result<SttTranscript, MediaError> {
    let text = body
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing text in OpenAI STT response",
            )
        })?
        .to_owned();
    let language = body
        .get("language")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let duration_secs = body.get("duration").and_then(Value::as_f64);

    let segments = body
        .get("segments")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    Some(SttSegment {
                        id: s.get("id").and_then(Value::as_u64),
                        start_secs: s.get("start").and_then(Value::as_f64)?,
                        end_secs: s.get("end").and_then(Value::as_f64)?,
                        text: s.get("text").and_then(Value::as_str)?.to_owned(),
                        speaker: s
                            .get("speaker")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let words = body
        .get("words")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|w| {
                    Some(SttWord {
                        word: w.get("word").and_then(Value::as_str)?.to_owned(),
                        start_secs: w.get("start").and_then(Value::as_f64)?,
                        end_secs: w.get("end").and_then(Value::as_f64)?,
                        speaker: w
                            .get("speaker")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        confidence: w.get("confidence").and_then(Value::as_f64),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(SttTranscript {
        vendor: "openai".into(),
        model: model.to_owned(),
        text,
        language,
        duration_secs,
        segments,
        words,
        raw_artifact: None,
    })
}

// ============================================================================
// 2. ElevenLabs Scribe (Engine 2: POST /v1/speech-to-text)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct ElevenLabsSttRequest {
    pub source: Option<SttAudioSource>,
    pub language_code: Option<String>,
    pub diarize: Option<bool>,
    pub num_speakers: Option<u32>,
    pub timestamps_granularity: Option<String>,
    pub tag_audio_events: Option<bool>,
    pub temperature: Option<f64>,
    pub keyterms: Vec<String>,
}

pub fn build_elevenlabs_stt_url(
    base_url: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy
        || url
            .path()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/v1")
    {
        Ok(format!("{trimmed}/speech-to-text"))
    } else {
        Ok(format!("{trimmed}/v1/speech-to-text"))
    }
}

pub async fn elevenlabs_stt(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &ElevenLabsSttRequest,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    ensure_stt_auth(
        cred,
        endpoint,
        &[MediaAuthKind::XiApiKey],
        "ElevenLabs Scribe STT",
    )?;
    let model = if endpoint.model.trim().is_empty() {
        "scribe_v1"
    } else {
        endpoint.model.trim()
    };
    if !matches!(
        model,
        "scribe_v1" | "scribe_v1_experimental" | "scribe_v2" | "scribe_v3"
    ) {
        return Err(MediaError::invalid_request(format!(
            "unsupported ElevenLabs Scribe model_id '{model}' (expected scribe_v1|v2|v3)"
        )));
    }

    let source = req.source.as_ref().ok_or_else(|| {
        MediaError::invalid_request(
            "ElevenLabs Scribe requires either file bytes or cloud_storage_url",
        )
    })?;
    let mut form = MultipartForm::new().text("model_id", model)?;
    match source {
        SttAudioSource::Bytes {
            filename,
            mime_type,
            bytes,
        } => {
            if bytes.is_empty() {
                return Err(MediaError::invalid_request(
                    "ElevenLabs Scribe audio bytes must not be empty",
                ));
            }
            form = form.file("file", filename.as_str(), mime_type.as_str(), bytes.clone())?;
        }
        SttAudioSource::Url { url, .. } => {
            let validated = validate_media_download_url(url, is_loopback_url_str(url))?;
            form = form.text("cloud_storage_url", validated.as_str())?;
        }
    }
    if let Some(lang) = &req.language_code
        && !lang.trim().is_empty()
    {
        form = form.text("language_code", lang.trim())?;
    }
    if let Some(diarize) = req.diarize {
        form = form.text("diarize", diarize.to_string())?;
    }
    if let Some(ns) = req.num_speakers {
        form = form.text("num_speakers", ns.to_string())?;
    }
    if let Some(gran) = &req.timestamps_granularity {
        form = form.text("timestamps_granularity", gran.trim())?;
    }
    if let Some(tag) = req.tag_audio_events {
        form = form.text("tag_audio_events", tag.to_string())?;
    }
    if let Some(temp) = req.temperature {
        form = form.text("temperature", temp.to_string())?;
    }
    for term in &req.keyterms {
        form = form.text("keyterms", term.as_str())?;
    }

    let url = build_elevenlabs_stt_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    let http_req = Engine2Request::post_multipart(url, form).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    let text = resp
        .body
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing text in ElevenLabs Scribe response",
            )
        })?
        .to_owned();
    let language = resp
        .body
        .get("language_code")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let words = resp
        .body
        .get("words")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|w| {
                    let word = w
                        .get("text")
                        .or_else(|| w.get("word"))
                        .and_then(Value::as_str)?;
                    Some(SttWord {
                        word: word.to_owned(),
                        start_secs: w.get("start").and_then(Value::as_f64).unwrap_or(0.0),
                        end_secs: w.get("end").and_then(Value::as_f64).unwrap_or(0.0),
                        speaker: w
                            .get("speaker_id")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        confidence: w.get("logprob").and_then(Value::as_f64),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(SttTranscript {
        vendor: "elevenlabs".into(),
        model: model.to_owned(),
        text,
        language,
        duration_secs: None,
        segments: Vec::new(),
        words,
        raw_artifact: None,
    })
}

// ============================================================================
// 3. Gemini Audio (Engine 2: inlineData <= 20 MiB | /upload/v1beta/files > 20 MiB)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct GeminiSttRequest {
    pub source: Option<SttAudioSource>,
    pub prompt: Option<String>,
    /// Override threshold for testing resumable upload without allocating > 20 MiB in unit tests.
    pub inline_threshold_bytes: Option<usize>,
    pub allow_http_loopback_upload: bool,
}

fn build_gemini_upload_url(base_url: &str, embedded_proxy: bool) -> Result<String, MediaError> {
    if embedded_proxy {
        return Err(MediaError::invalid_request(
            "Gemini resumable file upload (>20 MiB) is not supported in embedded proxy mode",
        ));
    }
    let url = validate_media_url(base_url, false)?;
    let trimmed = url.as_str().trim_end_matches('/');
    let root = if url
        .path()
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .ends_with("/v1beta")
    {
        &trimmed[..trimmed.len() - 7]
    } else {
        trimmed
    };
    Ok(format!("{root}/upload/v1beta/files"))
}

fn validate_gemini_resumable_upload_url(
    upload_start_url: &str,
    upload_url_raw: &str,
    allow_http_loopback: bool,
) -> Result<url::Url, MediaError> {
    let start_url = url::Url::parse(upload_start_url)
        .map_err(|_| MediaError::invalid_request("invalid Gemini upload start URL"))?;
    let upload_url = validate_media_download_url(upload_url_raw, allow_http_loopback)?;
    let same_scheme = upload_url.scheme().eq_ignore_ascii_case(start_url.scheme());
    let same_host = match (upload_url.host(), start_url.host()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    let same_port = upload_url.port_or_known_default() == start_url.port_or_known_default();
    if !same_scheme || !same_host || !same_port {
        return Err(MediaError::invalid_request(
            "Gemini x-goog-upload-url origin does not match upload endpoint origin",
        ));
    }
    Ok(upload_url)
}

pub async fn gemini_stt(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &GeminiSttRequest,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    ensure_stt_auth(
        cred,
        endpoint,
        &[MediaAuthKind::XGoogApiKey, MediaAuthKind::ProxyToken],
        "Gemini Audio STT",
    )?;
    let model_id = normalize_gemini_model_id(&endpoint.model)?;
    let base = build_gemini_v1beta_base(&endpoint.base_url, endpoint.embedded_proxy)?;
    let gen_url = format!("{base}/models/{model_id}:generateContent");
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let prompt_text = req
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Generate a transcript of the speech.");

    let audio_part = match req.source.as_ref() {
        Some(SttAudioSource::Bytes {
            filename,
            mime_type,
            bytes,
        }) => {
            if bytes.is_empty() {
                return Err(MediaError::invalid_request(
                    "Gemini Audio bytes must not be empty",
                ));
            }
            let threshold = req
                .inline_threshold_bytes
                .unwrap_or(GEMINI_INLINE_AUDIO_MAX_BYTES);
            if bytes.len() <= threshold {
                json!({
                    "inlineData": {
                        "mimeType": mime_type,
                        "data": STANDARD.encode(bytes),
                    }
                })
            } else {
                let upload_start_url =
                    build_gemini_upload_url(&endpoint.base_url, endpoint.embedded_proxy)?;
                let mut start_headers = headers.clone();
                start_headers.push(("X-Goog-Upload-Protocol".to_owned(), "resumable".to_owned()));
                start_headers.push(("X-Goog-Upload-Command".to_owned(), "start".to_owned()));
                start_headers.push((
                    "X-Goog-Upload-Header-Content-Length".to_owned(),
                    bytes.len().to_string(),
                ));
                start_headers.push((
                    "X-Goog-Upload-Header-Content-Type".to_owned(),
                    mime_type.clone(),
                ));
                let start_req = Engine2Request::post_json(
                    upload_start_url.clone(),
                    json!({ "file": { "display_name": filename } }),
                )
                .with_headers(start_headers, sensitive.clone());
                let start_resp = http
                    .execute_optional_json_with_headers(
                        &start_req,
                        &keys,
                        check_standard_business_status,
                        cancel,
                    )
                    .await?;
                let upload_url_raw = start_resp
                    .1
                    .get("x-goog-upload-url")
                    .and_then(|v| v.to_str().ok())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            "Gemini Files resumable start response missing x-goog-upload-url header",
                        )
                    })?;
                let allow_loopback =
                    req.allow_http_loopback_upload || is_loopback_url_str(&upload_start_url);
                let upload_url = validate_gemini_resumable_upload_url(
                    &upload_start_url,
                    upload_url_raw,
                    allow_loopback,
                )?;

                let mut fin_headers = headers.clone();
                fin_headers.push(("X-Goog-Upload-Offset".to_owned(), "0".to_owned()));
                fin_headers.push((
                    "X-Goog-Upload-Command".to_owned(),
                    "upload, finalize".to_owned(),
                ));
                let fin_req = Engine2Request::post_raw(
                    upload_url.as_str(),
                    mime_type.as_str(),
                    bytes.clone(),
                )
                .with_headers(fin_headers, sensitive.clone());
                let fin_resp = http
                    .execute_json(&fin_req, &keys, check_standard_business_status, cancel)
                    .await?;
                let mut file_obj = fin_resp.body.get("file").unwrap_or(&fin_resp.body).clone();
                let file_name = file_obj
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let mut state = file_obj
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("ACTIVE")
                    .to_ascii_uppercase();
                let mut poll_attempts = 0u32;
                while state == "PROCESSING" {
                    if poll_attempts >= 30 {
                        return Err(MediaError::new(
                            MediaErrorCode::Timeout,
                            "Gemini file upload stayed in PROCESSING state",
                        ));
                    }
                    poll_attempts += 1;
                    let Some(ref fname) = file_name else {
                        break;
                    };
                    let clean_name = fname.trim_start_matches('/');
                    let poll_url = format!("{base}/{clean_name}");
                    let poll_req = Engine2Request::get(poll_url)
                        .with_headers(headers.clone(), sensitive.clone());
                    let polled = http
                        .execute_json(&poll_req, &keys, check_standard_business_status, cancel)
                        .await?;
                    file_obj = polled.body.get("file").unwrap_or(&polled.body).clone();
                    state = file_obj
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("ACTIVE")
                        .to_ascii_uppercase();
                    if state == "PROCESSING" {
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                return Err(MediaError::new(MediaErrorCode::Cancelled, "Gemini file poll cancelled"));
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
                        }
                    }
                }
                if state == "FAILED" {
                    return Err(MediaError::new(
                        MediaErrorCode::ProviderError,
                        "Gemini uploaded audio file processing failed",
                    ));
                }
                let file_uri = file_obj.get("uri").and_then(Value::as_str).ok_or_else(|| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        "Gemini file upload response missing file.uri",
                    )
                })?;
                json!({
                    "fileData": {
                        "mimeType": mime_type,
                        "fileUri": file_uri,
                    }
                })
            }
        }
        Some(SttAudioSource::Url { url, mime_type }) => {
            let mime = mime_type.as_deref().unwrap_or("audio/mpeg");
            json!({
                "fileData": {
                    "mimeType": mime,
                    "fileUri": url,
                }
            })
        }
        None => {
            return Err(MediaError::invalid_request(
                "Gemini Audio STT requires an audio source",
            ));
        }
    };

    let gen_body = json!({
        "contents": [{
            "parts": [
                { "text": prompt_text },
                audio_part
            ]
        }]
    });
    let gen_req = Engine2Request::post_json(gen_url, gen_body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&gen_req, &keys, check_standard_business_status, cancel)
        .await?;

    if let Some(block) = resp
        .body
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        return Err(MediaError::new(
            MediaErrorCode::ProviderError,
            format!("Gemini Audio STT prompt blocked: {block}"),
        ));
    }

    let candidates = resp
        .body
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing candidates[] in Gemini Audio STT response",
            )
        })?;
    let mut text = String::new();
    if let Some(parts) = candidates
        .first()
        .and_then(|c| c.pointer("/content/parts"))
        .and_then(Value::as_array)
    {
        for p in parts {
            if let Some(t) = p.get("text").and_then(Value::as_str) {
                text.push_str(t);
            }
        }
    }
    if text.is_empty() {
        let finish_reason = candidates
            .first()
            .and_then(|c| c.get("finishReason"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(MediaError::new(
            MediaErrorCode::ProviderError,
            format!("Gemini Audio STT returned no transcript text (finishReason: {finish_reason})"),
        ));
    }

    Ok(SttTranscript {
        vendor: "gemini".into(),
        model: model_id.to_owned(),
        text,
        language: None,
        duration_secs: None,
        segments: Vec::new(),
        words: Vec::new(),
        raw_artifact: None,
    })
}

// ============================================================================
// 4. DashScope Paraformer (E3), Qwen-ASR (E2), Realtime ASR WS (E4)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct DashScopeSttRequest {
    pub source: Option<SttAudioSource>,
    pub language_hints: Vec<String>,
    pub diarization_enabled: Option<bool>,
    pub speaker_count: Option<u32>,
    pub format: Option<String>,
    pub sample_rate: Option<u32>,
    pub duration_secs: Option<f64>,
    pub allow_http_loopback_download: bool,
}

fn check_dashscope_error(body: &Value) -> Result<(), MediaError> {
    if let Some(code) = body
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let msg = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("DashScope ASR error");
        return Err(MediaError::business(Some(code.to_owned()), msg, false));
    }
    Ok(())
}

/// 4a. DashScope Paraformer / Filetrans (Engine 3, `X-DashScope-Async: enable`, public/OSS URL only).
pub struct DashScopeParaformerAdapter {
    pub base_url: String,
    pub embedded_proxy: bool,
    pub headers: Vec<(String, String)>,
    pub sensitive_headers: Vec<String>,
}

impl DashScopeParaformerAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_stt_auth(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "DashScope Paraformer STT",
        )?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        Ok((
            Self {
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                headers,
                sensitive_headers: sensitive,
            },
            keys,
        ))
    }
}

impl Engine3Adapter for DashScopeParaformerAdapter {
    fn vendor(&self) -> &str {
        "dashscope"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Stt
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let url = build_dashscope_url(
            &self.base_url,
            self.embedded_proxy,
            "/api/v1/services/audio/asr/transcription",
        )?;
        let mut headers = self.headers.clone();
        headers.push(("X-DashScope-Async".to_owned(), "enable".to_owned()));
        let mut body = input.payload.clone();
        if body.get("model").is_none() {
            body["model"] = json!(input.model);
        }
        Ok(Engine2Request::post_json(url, body)
            .with_headers(headers, self.sensitive_headers.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        check_dashscope_error(body)?;
        let task_id = body
            .pointer("/output/task_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing output.task_id in DashScope Paraformer submit response",
                )
            })?
            .to_owned();
        Ok(SubmitOutcome::Accepted {
            remote_task_id: task_id,
            poll_cursor: None,
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("DashScope Paraformer task missing remote_task_id")
        })?;
        let clean_id = validate_segment_id(remote_id)?;
        let url = build_dashscope_url(
            &self.base_url,
            self.embedded_proxy,
            &format!("/api/v1/tasks/{clean_id}"),
        )?;
        Ok(Engine2Request::get(url)
            .with_headers(self.headers.clone(), self.sensitive_headers.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        check_dashscope_error(body)?;
        let status = body
            .pointer("/output/task_status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        match status.as_str() {
            "SUCCEEDED" => {
                let results = body
                    .pointer("/output/results")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            "DashScope Paraformer SUCCEEDED missing output.results[]",
                        )
                    })?;
                let first = results.first().ok_or_else(|| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        "DashScope Paraformer output.results[] is empty",
                    )
                })?;
                if first
                    .get("subtask_status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case("FAILED"))
                {
                    let msg = first
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("DashScope Paraformer subtask failed");
                    return Ok(PollOutcome::Failed {
                        upstream_code: first
                            .get("code")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        message: msg.to_owned(),
                        retryable: false,
                    });
                }
                let t_url = first
                    .get("transcription_url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            "DashScope Paraformer result missing transcription_url",
                        )
                    })?;
                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(t_url).with_allow_json_bytes(true),
                        suggested_ext: "json".into(),
                        mime_type: "application/json".into(),
                    }],
                    poll_cursor: None,
                })
            }
            "FAILED" => Ok(PollOutcome::Failed {
                upstream_code: body
                    .pointer("/output/code")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                message: body
                    .pointer("/output/message")
                    .and_then(Value::as_str)
                    .unwrap_or("DashScope Paraformer transcription failed")
                    .to_owned(),
                retryable: false,
            }),
            "CANCELED" | "CANCELLED" => Ok(PollOutcome::Cancelled {
                message: "DashScope Paraformer task cancelled".into(),
            }),
            "PENDING" | "RUNNING" => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Running,
                poll_cursor: None,
                retry_after_hint: None,
            }),
            other => Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!("unexpected DashScope Paraformer task_status '{other}'"),
            )),
        }
    }

    fn build_cancel(&self, task: &MediaTaskRecord) -> Result<Option<Engine2Request>, MediaError> {
        let Some(remote_id) = task.remote_task_id.as_deref() else {
            return Ok(None);
        };
        let clean_id = validate_segment_id(remote_id)?;
        let url = build_dashscope_url(
            &self.base_url,
            self.embedded_proxy,
            &format!("/api/v1/tasks/{clean_id}/cancel"),
        )?;
        Ok(Some(
            Engine2Request::post_json(url, json!({}))
                .with_headers(self.headers.clone(), self.sensitive_headers.clone()),
        ))
    }
}

pub fn build_paraformer_submit_payload(
    model: &str,
    req: &DashScopeSttRequest,
) -> Result<Value, MediaError> {
    let url = match req.source.as_ref() {
        Some(SttAudioSource::Url { url, .. }) => {
            validate_media_download_url(url, is_loopback_url_str(url))?
        }
        Some(SttAudioSource::Bytes { .. }) => {
            return Err(MediaError::invalid_request(
                "DashScope Paraformer requires a public/OSS URL (use Qwen-ASR for local audio bytes)",
            ));
        }
        None => {
            return Err(MediaError::invalid_request(
                "DashScope Paraformer requires an audio URL",
            ));
        }
    };
    let mut parameters = json!({});
    if !req.language_hints.is_empty() {
        parameters["language_hints"] = json!(req.language_hints);
    }
    if let Some(d) = req.diarization_enabled {
        parameters["diarization_enabled"] = json!(d);
    }
    if let Some(sc) = req.speaker_count {
        parameters["speaker_count"] = json!(sc);
    }
    Ok(json!({
        "model": model,
        "input": {
            "file_urls": [url.as_str()],
        },
        "parameters": parameters,
    }))
}

pub fn parse_paraformer_transcript_json(
    model: &str,
    raw: &[u8],
    artifact: Option<MediaArtifactRef>,
) -> Result<SttTranscript, MediaError> {
    let val: Value = serde_json::from_slice(raw).map_err(|e| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            format!("invalid Paraformer transcription JSON: {e}"),
        )
    })?;
    let transcripts = val
        .get("transcripts")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing transcripts[] in Paraformer result JSON",
            )
        })?;
    let mut full_text = Vec::new();
    let mut segments = Vec::new();
    let mut words = Vec::new();
    for tr in transcripts {
        if let Some(t) = tr.get("text").and_then(Value::as_str)
            && !t.trim().is_empty()
        {
            full_text.push(t.trim().to_owned());
        }
        if let Some(sents) = tr.get("sentences").and_then(Value::as_array) {
            for s in sents {
                let begin_ms = s.get("begin_time").and_then(Value::as_f64).unwrap_or(0.0);
                let end_ms = s.get("end_time").and_then(Value::as_f64).unwrap_or(0.0);
                if let Some(stext) = s.get("text").and_then(Value::as_str) {
                    segments.push(SttSegment {
                        id: s.get("sentence_id").and_then(Value::as_u64),
                        start_secs: begin_ms / 1000.0,
                        end_secs: end_ms / 1000.0,
                        text: stext.to_owned(),
                        speaker: s
                            .get("speaker_id")
                            .map(|v| match v {
                                Value::String(x) => x.clone(),
                                Value::Number(n) => n.to_string(),
                                _ => String::new(),
                            })
                            .filter(|x| !x.is_empty()),
                    });
                }
                if let Some(ws) = s.get("words").and_then(Value::as_array) {
                    for w in ws {
                        if let Some(wt) = w.get("text").and_then(Value::as_str) {
                            words.push(SttWord {
                                word: wt.to_owned(),
                                start_secs: w
                                    .get("begin_time")
                                    .and_then(Value::as_f64)
                                    .unwrap_or(0.0)
                                    / 1000.0,
                                end_secs: w.get("end_time").and_then(Value::as_f64).unwrap_or(0.0)
                                    / 1000.0,
                                speaker: None,
                                confidence: None,
                            });
                        }
                    }
                }
            }
        }
    }
    let duration_secs = val
        .pointer("/properties/original_duration_in_milliseconds")
        .and_then(Value::as_f64)
        .map(|ms| ms / 1000.0);
    Ok(SttTranscript {
        vendor: "dashscope".into(),
        model: model.to_owned(),
        text: full_text.join("\n"),
        language: None,
        duration_secs,
        segments,
        words,
        raw_artifact: artifact,
    })
}

fn error_code_from_persisted(code: Option<&str>) -> MediaErrorCode {
    match code.map(str::trim) {
        Some("invalid_request") => MediaErrorCode::InvalidRequest,
        Some("auth_rejected") => MediaErrorCode::AuthRejected,
        Some("forbidden") => MediaErrorCode::Forbidden,
        Some("conflict") => MediaErrorCode::Conflict,
        Some("not_found") => MediaErrorCode::NotFound,
        Some("rate_limited") => MediaErrorCode::RateLimited,
        Some("business_error") => MediaErrorCode::BusinessError,
        Some("decode_error") => MediaErrorCode::DecodeError,
        Some("timeout") => MediaErrorCode::Timeout,
        Some("cancelled") => MediaErrorCode::Cancelled,
        Some("artifact_write_failed") => MediaErrorCode::ArtifactWriteFailed,
        Some("persistence_failed") => MediaErrorCode::PersistenceFailed,
        Some("transport") => MediaErrorCode::Transport,
        _ => MediaErrorCode::ProviderError,
    }
}

async fn extract_terminal_stt_payload<A: MediaArtifactStore>(
    artifacts: &A,
    completed: Engine3Completion,
    vendor_label: &str,
) -> Result<(MediaArtifactRef, Bytes), MediaError> {
    match completed.record.status {
        MediaTaskStatus::Failed => {
            let err_code = error_code_from_persisted(completed.record.error_code.as_deref());
            let msg = completed
                .record
                .error_message
                .unwrap_or_else(|| format!("{vendor_label} task failed"));
            return Err(
                MediaError::new(err_code, msg).with_upstream_code(completed.record.error_code)
            );
        }
        MediaTaskStatus::Cancelled => {
            let msg = completed
                .record
                .error_message
                .unwrap_or_else(|| format!("{vendor_label} task cancelled"));
            return Err(MediaError::new(MediaErrorCode::Cancelled, msg));
        }
        _ => {}
    }
    if let Some(pair) = completed.payloads.into_iter().next() {
        return Ok(pair);
    }
    if completed.record.status == MediaTaskStatus::Succeeded
        && let Some(artifact) = completed.record.artifacts.into_iter().next()
    {
        let bytes = artifacts.read_artifact(&artifact).await?;
        return Ok((artifact, bytes));
    }
    Err(MediaError::new(
        MediaErrorCode::DecodeError,
        format!("{vendor_label} completed without transcript payload"),
    ))
}

pub async fn dashscope_paraformer_transcribe<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &DashScopeSttRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    let model = if endpoint.model.trim().is_empty() {
        "paraformer-v2"
    } else {
        endpoint.model.trim()
    };
    let (adapter, keys) = DashScopeParaformerAdapter::from_endpoint(endpoint, cred)?;
    let payload = build_paraformer_submit_payload(model, req)?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model: model.to_owned(),
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    let (artifact, payload_bytes) = extract_terminal_stt_payload(
        runner.artifacts().as_ref(),
        completed,
        "DashScope Paraformer",
    )
    .await?;
    parse_paraformer_transcript_json(model, &payload_bytes, Some(artifact))
}

/// 4b. DashScope Qwen-ASR short audio sync (`<= 5 min`, supports local base64 `data:<mime>;base64,...`).
pub async fn dashscope_qwen_asr(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &DashScopeSttRequest,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    ensure_stt_auth(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "DashScope Qwen-ASR",
    )?;
    if let Some(dur) = req.duration_secs
        && dur > DASHSCOPE_QWEN_ASR_MAX_DURATION_SECS
    {
        return Err(MediaError::invalid_request(format!(
            "DashScope Qwen-ASR supports short audio <= {DASHSCOPE_QWEN_ASR_MAX_DURATION_SECS}s"
        )));
    }
    let model = if endpoint.model.trim().is_empty() {
        "qwen3-asr-flash"
    } else {
        endpoint.model.trim()
    };
    let audio_uri = match req.source.as_ref() {
        Some(SttAudioSource::Bytes {
            mime_type, bytes, ..
        }) => {
            if bytes.is_empty() {
                return Err(MediaError::invalid_request(
                    "DashScope Qwen-ASR audio bytes must not be empty",
                ));
            }
            format!("data:{mime_type};base64,{}", STANDARD.encode(bytes))
        }
        Some(SttAudioSource::Url { url, .. }) => {
            validate_media_download_url(url, is_loopback_url_str(url))?.to_string()
        }
        None => {
            return Err(MediaError::invalid_request(
                "DashScope Qwen-ASR requires an audio source",
            ));
        }
    };

    let url = build_dashscope_url(
        &endpoint.base_url,
        endpoint.embedded_proxy,
        "/api/v1/services/aigc/multimodal-generation/generation",
    )?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    let mut asr_options = json!({ "enable_itn": true });
    if let Some(lang) = req.language_hints.first() {
        asr_options["language"] = json!(lang);
    }
    let body = json!({
        "model": model,
        "input": {
            "messages": [
                { "role": "user", "content": [{ "audio": audio_uri }] }
            ]
        },
        "parameters": {
            "asr_options": asr_options
        }
    });
    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, |_, b| check_dashscope_error(b), cancel)
        .await?;

    let msg_content = resp
        .body
        .pointer("/output/choices/0/message/content")
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing output.choices[0].message.content in Qwen-ASR response",
            )
        })?;
    let text = match msg_content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "unexpected content shape in Qwen-ASR response",
            ));
        }
    };
    let language = resp
        .body
        .pointer("/output/choices/0/message/annotations/0/language")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Ok(SttTranscript {
        vendor: "dashscope".into(),
        model: model.to_owned(),
        text,
        language,
        duration_secs: None,
        segments: Vec::new(),
        words: Vec::new(),
        raw_artifact: None,
    })
}

/// 4c. DashScope Realtime ASR WS (`wss://{host}/api-ws/v1/inference`).
pub async fn dashscope_asr_ws(
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &DashScopeSttRequest,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    ensure_stt_auth(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "DashScope Realtime ASR WS",
    )?;
    let Some(SttAudioSource::Bytes { bytes, .. }) = &req.source else {
        return Err(MediaError::invalid_request(
            "DashScope Realtime ASR WS requires local audio bytes",
        ));
    };
    if bytes.is_empty() {
        return Err(MediaError::invalid_request(
            "DashScope Realtime ASR WS audio bytes must not be empty",
        ));
    }
    let model = if endpoint.model.trim().is_empty() {
        "paraformer-realtime-v2"
    } else {
        endpoint.model.trim()
    };

    let ws_base = http_url_to_ws_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let trimmed = ws_base.as_str().trim_end_matches('/');
    let ws_url = if ws_base
        .path()
        .trim_end_matches('/')
        .ends_with("/api-ws/v1/inference")
    {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/api-ws/v1/inference")
    };

    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
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

    let task_id = uuid::Uuid::new_v4().to_string();
    let mut params = json!({
        "format": req.format.as_deref().unwrap_or("pcm"),
        "sample_rate": req.sample_rate.unwrap_or(16_000),
    });
    if !req.language_hints.is_empty() {
        params["language_hints"] = json!(req.language_hints);
    }

    writer
        .send_json(&json!({
            "header": {
                "action": "run-task",
                "task_id": task_id,
                "streaming": "duplex",
            },
            "payload": {
                "task_group": "audio",
                "task": "asr",
                "function": "recognition",
                "model": model,
                "parameters": params,
                "input": {},
            }
        }))
        .await?;

    // Wait for `task-started`
    loop {
        let ev = recv_ws_text_json(&mut reader, &keys, cancel).await?;
        match ev.pointer("/header/event").and_then(Value::as_str) {
            Some("task-started") => break,
            Some("task-failed") => return Err(dashscope_ws_err(&ev).scrubbed(&keys)),
            _ => {}
        }
    }

    for chunk in bytes.chunks(3200) {
        writer.send_binary(Bytes::copy_from_slice(chunk)).await?;
    }
    writer
        .send_json(&json!({
            "header": {
                "action": "finish-task",
                "task_id": task_id,
                "streaming": "duplex",
            },
            "payload": { "input": {} }
        }))
        .await?;

    let mut sentences = Vec::new();
    let mut last_partial = String::new();
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(MediaError::new(MediaErrorCode::Cancelled, "DashScope ASR WS cancelled"));
            }
            item = reader.recv() => item,
        };
        match next {
            Some(Ok(WsMessage::Text(txt))) => {
                let ev: Value = serde_json::from_str(&txt).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("invalid DashScope ASR WS JSON: {e}"),
                    )
                })?;
                match ev.pointer("/header/event").and_then(Value::as_str) {
                    Some("result-generated") => {
                        if let Some(sent) = ev.pointer("/payload/output/sentence")
                            && let Some(t) = sent.get("text").and_then(Value::as_str)
                        {
                            let is_end =
                                sent.get("sentence_end").and_then(Value::as_bool) == Some(true);
                            if is_end {
                                sentences.push(t.to_owned());
                                last_partial.clear();
                            } else {
                                last_partial = t.to_owned();
                            }
                        }
                    }
                    Some("task-finished") => break,
                    Some("task-failed") => return Err(dashscope_ws_err(&ev).scrubbed(&keys)),
                    _ => {}
                }
            }
            Some(Ok(WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Close(_))) | None => break,
            Some(Ok(WsMessage::Binary(_))) => {}
            Some(Err(e)) => return Err(e.scrubbed(&keys)),
        }
    }

    let _ = writer.close(1000, "done").await;
    if sentences.is_empty() && !last_partial.is_empty() {
        sentences.push(last_partial);
    }

    Ok(SttTranscript {
        vendor: "dashscope".into(),
        model: model.to_owned(),
        text: sentences.join(" "),
        language: req.language_hints.first().cloned(),
        duration_secs: None,
        segments: Vec::new(),
        words: Vec::new(),
        raw_artifact: None,
    })
}

fn dashscope_ws_err(ev: &Value) -> MediaError {
    let code = ev
        .pointer("/header/error_code")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let msg = ev
        .pointer("/header/error_message")
        .and_then(Value::as_str)
        .unwrap_or("DashScope ASR WS failed");
    MediaError::business(code, msg, false)
}

async fn recv_ws_text_json(
    reader: &mut super::ws::WsReader,
    keys: &CredentialSet,
    cancel: &CancellationToken,
) -> Result<Value, MediaError> {
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(MediaError::new(MediaErrorCode::Cancelled, "websocket cancelled"));
            }
            item = reader.recv() => item,
        };
        match next {
            Some(Ok(WsMessage::Text(txt))) => {
                return serde_json::from_str(&txt).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("invalid WS JSON frame: {e}"),
                    )
                    .scrubbed(keys)
                });
            }
            Some(Ok(WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Close(f))) => {
                let reason = f.map(|x| x.reason).unwrap_or_default();
                return Err(MediaError::new(
                    MediaErrorCode::Transport,
                    format!("websocket closed unexpectedly: {reason}"),
                )
                .scrubbed(keys));
            }
            Some(Err(e)) => return Err(e.scrubbed(keys)),
            Some(Ok(WsMessage::Binary(_))) => {}
            None => {
                return Err(MediaError::new(
                    MediaErrorCode::Transport,
                    "websocket closed before response",
                ));
            }
        }
    }
}

pub(crate) fn validate_segment_id(raw: &str) -> Result<&str, MediaError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains("..")
        || trimmed
            .bytes()
            .any(|b| b.is_ascii_control() || matches!(b, b'/' | b'\\' | b'?' | b'#'))
    {
        return Err(MediaError::invalid_request("invalid remote task id"));
    }
    Ok(trimmed)
}

// ============================================================================
// 5. Volcengine ASR (Flash Engine 2, Async Engine 3, SAUC WS Engine 4)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct VolcengineAsrRequest {
    pub source: Option<SttAudioSource>,
    pub resource_id: Option<String>,
    pub uid: Option<String>,
    pub format: Option<String>,
    pub sample_rate: Option<u32>,
    pub language: Option<String>,
    pub enable_itn: Option<bool>,
    pub enable_punc: Option<bool>,
    pub show_utterances: Option<bool>,
}

fn ensure_volcengine_asr_auth(
    cred: &MediaCredential,
    endpoint: &MediaEndpointConfig,
) -> Result<(), MediaError> {
    ensure_stt_auth(
        cred,
        endpoint,
        &[
            MediaAuthKind::VolcengineV3ApiKey,
            MediaAuthKind::VolcengineV3AppAccess,
            MediaAuthKind::ProxyToken,
        ],
        "Volcengine ASR",
    )
}

fn build_volcengine_asr_url(
    base_url: &str,
    api_path: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy || url.path().trim_end_matches('/').ends_with(api_path) {
        Ok(trimmed.to_owned())
    } else {
        Ok(format!("{trimmed}{api_path}"))
    }
}

pub fn build_volcengine_asr_payload(req: &VolcengineAsrRequest) -> Result<Value, MediaError> {
    let audio = match req.source.as_ref() {
        Some(SttAudioSource::Bytes {
            filename, bytes, ..
        }) => {
            if bytes.is_empty() {
                return Err(MediaError::invalid_request(
                    "Volcengine ASR audio bytes must not be empty",
                ));
            }
            let fmt = req.format.as_deref().unwrap_or_else(|| {
                filename
                    .rsplit('.')
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("wav")
            });
            json!({
                "format": fmt,
                "data": STANDARD.encode(bytes),
            })
        }
        Some(SttAudioSource::Url { url, .. }) => {
            let validated = validate_media_download_url(url, is_loopback_url_str(url))?;
            let mut obj = json!({ "url": validated.as_str() });
            if let Some(fmt) = &req.format {
                obj["format"] = json!(fmt);
            }
            obj
        }
        None => {
            return Err(MediaError::invalid_request(
                "Volcengine ASR requires an audio source",
            ));
        }
    };
    let mut request_obj = json!({
        "model_name": "bigmodel",
        "enable_itn": req.enable_itn.unwrap_or(true),
        "enable_punc": req.enable_punc.unwrap_or(true),
    });
    if let Some(su) = req.show_utterances {
        request_obj["show_utterances"] = json!(su);
    }
    if let Some(lang) = &req.language {
        request_obj["language"] = json!(lang);
    }
    Ok(json!({
        "user": {
            "uid": req.uid.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or("coda-cli"),
        },
        "audio": audio,
        "request": request_obj,
    }))
}

pub fn parse_volcengine_asr_response(
    model: &str,
    body: &Value,
    artifact: Option<MediaArtifactRef>,
) -> Result<SttTranscript, MediaError> {
    let result = body.get("result").unwrap_or(body);
    let text = result
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing result.text in Volcengine ASR response",
            )
        })?
        .to_owned();
    let segments = result
        .get("utterances")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|u| {
                    let t = u.get("text").and_then(Value::as_str)?;
                    let start_ms = u.get("start_time").and_then(Value::as_f64).unwrap_or(0.0);
                    let end_ms = u.get("end_time").and_then(Value::as_f64).unwrap_or(0.0);
                    Some(SttSegment {
                        id: None,
                        start_secs: start_ms / 1000.0,
                        end_secs: end_ms / 1000.0,
                        text: t.to_owned(),
                        speaker: u
                            .pointer("/additions/speaker")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let duration_secs = body
        .pointer("/audio_info/duration")
        .and_then(Value::as_f64)
        .map(|ms| ms / 1000.0);
    Ok(SttTranscript {
        vendor: "volcengine".into(),
        model: model.to_owned(),
        text,
        language: None,
        duration_secs,
        segments,
        words: Vec::new(),
        raw_artifact: artifact,
    })
}

/// 5a. Volcengine ASR Flash (`POST /api/v3/auc/bigmodel/recognize/flash`, `volc.bigasr.auc_turbo`).
pub async fn volcengine_asr_flash(
    http: &MediaHttpClient,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &VolcengineAsrRequest,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    ensure_volcengine_asr_auth(cred, endpoint)?;
    let url = build_volcengine_asr_url(
        &endpoint.base_url,
        "/api/v3/auc/bigmodel/recognize/flash",
        endpoint.embedded_proxy,
    )?;
    let (mut headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    headers.push((
        "X-Api-Resource-Id".to_owned(),
        req.resource_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("volc.bigasr.auc_turbo")
            .to_owned(),
    ));
    headers.push((
        "X-Api-Request-Id".to_owned(),
        uuid::Uuid::new_v4().to_string(),
    ));
    headers.push(("X-Api-Sequence".to_owned(), "-1".to_owned()));

    let body = build_volcengine_asr_payload(req)?;
    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(
            &http_req,
            &keys,
            |hdrs, _| {
                if let Some(sc) = hdrs.get("x-api-status-code").and_then(|v| v.to_str().ok())
                    && sc != "20000000"
                {
                    let msg = hdrs
                        .get("x-api-message")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("Volcengine Flash ASR rejected");
                    return Err(MediaError::business(Some(sc.to_owned()), msg, false));
                }
                Ok(())
            },
            cancel,
        )
        .await?;
    let model = if endpoint.model.trim().is_empty() {
        "bigmodel"
    } else {
        endpoint.model.trim()
    };
    parse_volcengine_asr_response(model, &resp.body, None)
}

/// 5b. Volcengine ASR Long-Audio Async (`POST /submit` -> `POST /query` `{}` + `X-Api-Status-Code`).
pub struct VolcengineAsrAdapter {
    pub base_url: String,
    pub embedded_proxy: bool,
    pub headers: Vec<(String, String)>,
    pub sensitive_headers: Vec<String>,
    pub resource_id: String,
    last_request_id: Mutex<Option<String>>,
}

impl VolcengineAsrAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        resource_id: Option<&str>,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_volcengine_asr_auth(cred, endpoint)?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        Ok((
            Self {
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                headers,
                sensitive_headers: sensitive,
                resource_id: resource_id
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("volc.bigasr.auc")
                    .to_owned(),
                last_request_id: Mutex::new(None),
            },
            keys,
        ))
    }
}

impl Engine3Adapter for VolcengineAsrAdapter {
    fn vendor(&self) -> &str {
        "volcengine"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Stt
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let url = build_volcengine_asr_url(
            &self.base_url,
            "/api/v3/auc/bigmodel/submit",
            self.embedded_proxy,
        )?;
        let req_id = input
            .idempotency_key
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Ok(mut guard) = self.last_request_id.lock() {
            *guard = Some(req_id.clone());
        }
        let mut headers = self.headers.clone();
        headers.push(("X-Api-Resource-Id".to_owned(), self.resource_id.clone()));
        headers.push(("X-Api-Request-Id".to_owned(), req_id));
        headers.push(("X-Api-Sequence".to_owned(), "-1".to_owned()));
        Ok(Engine2Request::post_json(url, input.payload.clone())
            .with_headers(headers, self.sensitive_headers.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        _body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        if let Some(sc) = headers
            .get("x-api-status-code")
            .and_then(|v| v.to_str().ok())
            && sc != "20000000"
        {
            let msg = headers
                .get("x-api-message")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("Volcengine ASR submit rejected");
            return Err(MediaError::business(Some(sc.to_owned()), msg, false));
        }
        let echoed = headers
            .get("x-api-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let stored = self.last_request_id.lock().ok().and_then(|g| g.clone());
        let remote_task_id = echoed.or(stored).ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing X-Api-Request-Id for Volcengine ASR submit",
            )
        })?;
        Ok(SubmitOutcome::Accepted {
            remote_task_id,
            poll_cursor: None,
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let req_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request(
                "Volcengine ASR task missing remote_task_id (X-Api-Request-Id)",
            )
        })?;
        let url = build_volcengine_asr_url(
            &self.base_url,
            "/api/v3/auc/bigmodel/query",
            self.embedded_proxy,
        )?;
        let mut headers = self.headers.clone();
        headers.push(("X-Api-Resource-Id".to_owned(), self.resource_id.clone()));
        headers.push(("X-Api-Request-Id".to_owned(), req_id.to_owned()));
        Ok(Engine2Request::post_json(url, json!({}))
            .with_headers(headers, self.sensitive_headers.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        let status_code = headers
            .get("x-api-status-code")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Volcengine ASR query response missing X-Api-Status-Code header",
                )
            })?;
        match status_code {
            "20000000" => {
                let raw = serde_json::to_vec(body)
                    .map_err(|e| MediaError::new(MediaErrorCode::DecodeError, e.to_string()))?;
                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::InlineBytes {
                        bytes: Bytes::from(raw),
                        suggested_ext: "json".into(),
                        mime_type: "application/json".into(),
                    }],
                    poll_cursor: None,
                })
            }
            "20000001" | "20000002" => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Running,
                poll_cursor: None,
                retry_after_hint: None,
            }),
            other => {
                let msg = headers
                    .get("x-api-message")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("Volcengine ASR failed");
                Ok(PollOutcome::Failed {
                    upstream_code: Some(other.to_owned()),
                    message: msg.to_owned(),
                    retryable: false,
                })
            }
        }
    }
}

pub async fn volcengine_asr_transcribe<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &VolcengineAsrRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    let model = if endpoint.model.trim().is_empty() {
        "bigmodel"
    } else {
        endpoint.model.trim()
    };
    let (adapter, keys) =
        VolcengineAsrAdapter::from_endpoint(endpoint, cred, req.resource_id.as_deref())?;
    let payload = build_volcengine_asr_payload(req)?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model: model.to_owned(),
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    let (artifact, payload_bytes) =
        extract_terminal_stt_payload(runner.artifacts().as_ref(), completed, "Volcengine ASR")
            .await?;
    let val: Value = serde_json::from_slice(&payload_bytes).map_err(|e| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            format!("invalid Volcengine ASR JSON payload: {e}"),
        )
    })?;
    parse_volcengine_asr_response(model, &val, Some(artifact))
}

/// 5c. Volcengine SAUC Realtime WS (`wss://.../api/v3/sauc/bigmodel`, `compression = 0`).
pub async fn volcengine_sauc_ws(
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &VolcengineAsrRequest,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    ensure_volcengine_asr_auth(cred, endpoint)?;
    let Some(SttAudioSource::Bytes { bytes, .. }) = &req.source else {
        return Err(MediaError::invalid_request(
            "Volcengine SAUC WS requires local audio bytes",
        ));
    };
    if bytes.is_empty() {
        return Err(MediaError::invalid_request(
            "Volcengine SAUC WS audio bytes must not be empty",
        ));
    }

    let ws_base = http_url_to_ws_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let trimmed = ws_base.as_str().trim_end_matches('/');
    let ws_url = if ws_base
        .path()
        .trim_end_matches('/')
        .ends_with("/api/v3/sauc/bigmodel")
    {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/api/v3/sauc/bigmodel")
    };

    let (mut headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    headers.push((
        "X-Api-Resource-Id".to_owned(),
        req.resource_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("volc.bigasr.sauc.duration")
            .to_owned(),
    ));
    headers.push((
        "X-Api-Request-Id".to_owned(),
        uuid::Uuid::new_v4().to_string(),
    ));
    headers.push((
        "X-Api-Connect-Id".to_owned(),
        uuid::Uuid::new_v4().to_string(),
    ));

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

    let mut request_obj = json!({
        "model_name": "bigmodel",
        "enable_itn": req.enable_itn.unwrap_or(true),
        "enable_punc": req.enable_punc.unwrap_or(true),
    });
    if let Some(lang) = &req.language {
        request_obj["language"] = json!(lang);
    }
    let init_cfg = serde_json::to_vec(&json!({
        "user": {
            "uid": req.uid.as_deref().unwrap_or("coda-cli"),
        },
        "audio": {
            "format": req.format.as_deref().unwrap_or("pcm"),
            "rate": req.sample_rate.unwrap_or(16_000),
            "bits": 16,
            "channel": 1,
        },
        "request": request_obj,
    }))
    .map_err(|e| MediaError::invalid_request(e.to_string()))?;

    writer
        .send_binary(volcengine_frame::encode_simple_frame(
            volcengine_frame::MSG_FULL_CLIENT_REQUEST,
            volcengine_frame::FLAG_NONE,
            volcengine_frame::SERIAL_JSON,
            &init_cfg,
        ))
        .await?;

    let chunks: Vec<&[u8]> = bytes.chunks(6400).collect();
    for (idx, chunk) in chunks.iter().enumerate() {
        if cancel.is_cancelled() {
            let _ = writer.close(1000, "cancelled").await;
            return Err(MediaError::new(
                MediaErrorCode::Cancelled,
                "Volcengine SAUC WS cancelled",
            ));
        }
        let is_last = idx + 1 == chunks.len();
        let flags = if is_last {
            volcengine_frame::FLAG_LAST_AUDIO
        } else {
            volcengine_frame::FLAG_NONE
        };
        writer
            .send_binary(volcengine_frame::encode_simple_frame(
                volcengine_frame::MSG_AUDIO_ONLY_CLIENT,
                flags,
                volcengine_frame::SERIAL_NONE,
                chunk,
            ))
            .await?;
    }

    let mut latest_transcript: Option<SttTranscript> = None;
    let model = if endpoint.model.trim().is_empty() {
        "bigmodel"
    } else {
        endpoint.model.trim()
    };

    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(MediaError::new(MediaErrorCode::Cancelled, "Volcengine SAUC WS cancelled"));
            }
            item = reader.recv() => item,
        };
        match next {
            Some(Ok(WsMessage::Binary(raw))) => {
                let frame = volcengine_frame::decode_frame(&raw).map_err(|e| e.scrubbed(&keys))?;
                if frame.msg_type == volcengine_frame::MSG_ERROR {
                    let msg = String::from_utf8_lossy(&frame.payload).into_owned();
                    return Err(MediaError::business(
                        frame.error_code.map(|c| c.to_string()),
                        msg,
                        false,
                    )
                    .scrubbed(&keys));
                }
                if frame.msg_type == volcengine_frame::MSG_FULL_SERVER_RESPONSE
                    && !frame.payload.is_empty()
                {
                    let val: Value = serde_json::from_slice(&frame.payload).map_err(|e| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            format!("invalid Volcengine SAUC JSON response: {e}"),
                        )
                    })?;
                    if val.pointer("/result/text").is_some() {
                        latest_transcript = Some(parse_volcengine_asr_response(model, &val, None)?);
                    }
                    if frame.flags == volcengine_frame::FLAG_LAST_AUDIO
                        || frame.flags == volcengine_frame::FLAG_NEG_SEQ
                        || frame.sequence.is_some_and(|s| s < 0)
                        || val.get("is_last").and_then(Value::as_bool) == Some(true)
                    {
                        break;
                    }
                }
            }
            Some(Ok(WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Close(_))) | None => break,
            Some(Ok(WsMessage::Text(_))) => {}
            Some(Err(e)) => return Err(e.scrubbed(&keys)),
        }
    }

    let _ = writer.close(1000, "done").await;
    latest_transcript.ok_or_else(|| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            "Volcengine SAUC WS finished without transcript response",
        )
    })
}

// ============================================================================
// 6. Local `whisper.cpp` (`LocalAsrRunner` trait & pure helpers — C11-clean)
// ============================================================================

/// Exact 11-entry FFmpeg demuxer allowlist matching `codabot` `audioTranscription/index.ts`.
pub fn demuxer_for_audio_extension(ext: &str) -> Option<&'static str> {
    let normalized = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    match normalized.as_str() {
        "wav" => Some("wav"),
        "mp3" => Some("mp3"),
        "m4a" => Some("mov"),
        "aac" => Some("aac"),
        "aif" | "aiff" => Some("aiff"),
        "caf" => Some("caf"),
        "flac" => Some("flac"),
        "ogg" | "opus" => Some("ogg"),
        "webm" => Some("matroska"),
        _ => None,
    }
}

pub fn normalize_whisper_language(lang: Option<&str>) -> String {
    let raw = lang
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("zh-CN");
    raw.split(['-', '_'])
        .next()
        .unwrap_or("zh")
        .to_ascii_lowercase()
}

pub fn parse_ffprobe_duration_json(stdout: &[u8]) -> Result<f64, MediaError> {
    let parsed: Value = serde_json::from_slice(stdout).map_err(|e| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            format!("invalid ffprobe JSON output: {e}"),
        )
    })?;
    let duration_val = parsed
        .pointer("/format/duration")
        .ok_or_else(|| MediaError::invalid_request("ffprobe output missing format.duration"))?;
    let seconds = match duration_val {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .ok_or_else(|| MediaError::invalid_request("audio duration is not a valid number"))?;

    if !seconds.is_finite() || seconds <= 0.0 || seconds > LOCAL_ASR_MAX_SECONDS {
        return Err(MediaError::invalid_request(format!(
            "audio duration must be > 0 and <= {LOCAL_ASR_MAX_SECONDS} seconds (got {seconds})"
        )));
    }
    Ok(seconds)
}

pub fn build_ffprobe_args(demuxer: &str, path: &str) -> Vec<String> {
    vec![
        "-v".to_owned(),
        "error".to_owned(),
        "-protocol_whitelist".to_owned(),
        "file,pipe".to_owned(),
        "-f".to_owned(),
        demuxer.to_owned(),
        "-show_entries".to_owned(),
        "format=duration".to_owned(),
        "-of".to_owned(),
        "json".to_owned(),
        path.to_owned(),
    ]
}

pub fn build_ffmpeg_args(demuxer: &str, input_path: &str, wav_path: &str) -> Vec<String> {
    vec![
        "-nostdin".to_owned(),
        "-v".to_owned(),
        "error".to_owned(),
        "-protocol_whitelist".to_owned(),
        "file,pipe".to_owned(),
        "-f".to_owned(),
        demuxer.to_owned(),
        "-i".to_owned(),
        input_path.to_owned(),
        "-map".to_owned(),
        "0:a:0".to_owned(),
        "-vn".to_owned(),
        "-t".to_owned(),
        ((LOCAL_ASR_MAX_SECONDS as u64) + 1).to_string(),
        "-ar".to_owned(),
        "16000".to_owned(),
        "-ac".to_owned(),
        "1".to_owned(),
        "-c:a".to_owned(),
        "pcm_s16le".to_owned(),
        wav_path.to_owned(),
    ]
}

pub fn build_whisper_cli_args(
    model_path: &str,
    wav_path: &str,
    lang_primary: &str,
    threads: usize,
    output_prefix: &str,
) -> Vec<String> {
    vec![
        "--model".to_owned(),
        model_path.to_owned(),
        "--file".to_owned(),
        wav_path.to_owned(),
        "--language".to_owned(),
        lang_primary.to_owned(),
        "--threads".to_owned(),
        threads.clamp(1, 4).to_string(),
        "--no-gpu".to_owned(),
        "--output-txt".to_owned(),
        "--output-file".to_owned(),
        output_prefix.to_owned(),
    ]
}

#[derive(Clone, Debug)]
pub enum LocalAsrInput {
    FilePath(PathBuf),
    Bytes { filename: String, bytes: Bytes },
}

#[derive(Clone, Debug)]
pub struct LocalAsrRequest {
    pub input: LocalAsrInput,
    pub lang: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrStatus {
    pub supported: bool,
    pub executable_path: String,
    pub model_path: String,
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub reason: Option<String>,
}

pub trait LocalAsrRunner: Send + Sync {
    fn status<'a>(
        &'a self,
        lang: Option<&'a str>,
    ) -> impl Future<Output = Result<LocalAsrStatus, MediaError>> + Send + 'a;

    fn transcribe<'a>(
        &'a self,
        req: &'a LocalAsrRequest,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<SttTranscript, MediaError>> + Send + 'a;
}

pub async fn local_whisper_transcribe<R: LocalAsrRunner>(
    runner: &R,
    req: &LocalAsrRequest,
    cancel: &CancellationToken,
) -> Result<SttTranscript, MediaError> {
    match &req.input {
        LocalAsrInput::FilePath(path) => {
            let path_str = path.to_string_lossy();
            if path_str.trim().is_empty()
                || path_str.chars().count() > 4096
                || path_str.chars().any(|c| c.is_control())
            {
                return Err(MediaError::invalid_request("invalid local audio file path"));
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .ok_or_else(|| MediaError::invalid_request("audio file missing extension"))?;
            if demuxer_for_audio_extension(ext).is_none() {
                return Err(MediaError::invalid_request(format!(
                    "unsupported audio extension '.{ext}' for local whisper.cpp"
                )));
            }
        }
        LocalAsrInput::Bytes { filename, bytes } => {
            if bytes.is_empty() {
                return Err(MediaError::invalid_request(
                    "local whisper.cpp audio bytes must not be empty",
                ));
            }
            let ext = filename
                .rsplit('.')
                .next()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| MediaError::invalid_request("audio filename missing extension"))?;
            if demuxer_for_audio_extension(ext).is_none() {
                return Err(MediaError::invalid_request(format!(
                    "unsupported audio extension '.{ext}' for local whisper.cpp"
                )));
            }
        }
    }

    let transcript = runner.transcribe(req, cancel).await?;
    if transcript.text.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "no speech detected in local audio",
        ));
    }
    Ok(transcript)
}

fn is_loopback_url_str(raw: &str) -> bool {
    url::Url::parse(raw.trim())
        .ok()
        .is_some_and(|u| is_loopback_host(&u))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::engine3::Engine3PollConfig;
    use crate::media::{InMemoryMediaArtifactStore, InMemoryMediaTaskRepository};
    use axum::{
        Router,
        body::Bytes as AxumBytes,
        extract::Path as AxumPath,
        http::{HeaderMap as AxumHeaders, StatusCode},
        routing::{get, post},
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn accept_ws_server(
        listener: &tokio::net::TcpListener,
    ) -> (tokio::net::TcpStream, String) {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut req_buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let header_end = loop {
            let n = sock.read(&mut tmp).await.unwrap();
            req_buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let req_text = String::from_utf8_lossy(&req_buf[..header_end]).into_owned();
        let ws_key = req_text
            .lines()
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-key"))
                    .map(|(_, v)| v.trim().to_owned())
            })
            .unwrap();
        let accept = STANDARD.encode(super::super::crypto::sha1_digest(
            format!("{ws_key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
        ));
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
        (sock, req_text)
    }

    fn encode_ws_server_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(0x80 | (opcode & 0x0F));
        let len = payload.len();
        if len <= 125 {
            out.push(len as u8);
        } else if len <= 65535 {
            out.push(126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            out.push(127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
        out.extend_from_slice(payload);
        out
    }

    async fn read_ws_client_frame(sock: &mut tokio::net::TcpStream) -> (u8, Vec<u8>) {
        let mut hdr = [0u8; 2];
        sock.read_exact(&mut hdr).await.unwrap();
        let opcode = hdr[0] & 0x0F;
        let len7 = hdr[1] & 0x7F;
        let len: usize = match len7 {
            0..=125 => usize::from(len7),
            126 => {
                let mut b = [0u8; 2];
                sock.read_exact(&mut b).await.unwrap();
                usize::from(u16::from_be_bytes(b))
            }
            _ => {
                let mut b = [0u8; 8];
                sock.read_exact(&mut b).await.unwrap();
                u64::from_be_bytes(b) as usize
            }
        };
        let mut mask = [0u8; 4];
        sock.read_exact(&mut mask).await.unwrap();
        let mut payload = vec![0u8; len];
        sock.read_exact(&mut payload).await.unwrap();
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i & 3];
        }
        (opcode, payload)
    }

    #[tokio::test]
    async fn openai_stt_multipart_plain_text_sse_and_validations() {
        let app = Router::new()
            .route(
                "/v1/audio/transcriptions",
                post(|headers: AxumHeaders, body: AxumBytes| async move {
                    assert_eq!(
                        headers.get("authorization").and_then(|v| v.to_str().ok()),
                        Some("Bearer sk-oai-stt")
                    );
                    let body_str = String::from_utf8_lossy(&body);
                    if body_str.contains("name=\"stream\"") {
                        let mut h = AxumHeaders::new();
                        h.insert("content-type", "text/event-stream".parse().unwrap());
                        let sse = "data: {\"type\":\"transcript.text.delta\",\"delta\":\"Hello \"}\n\ndata: {\"type\":\"transcript.text.done\",\"text\":\"Hello world\"}\n\n";
                        return (StatusCode::OK, h, AxumBytes::from(sse));
                    }
                    if body_str.contains("srt") {
                        let mut h = AxumHeaders::new();
                        h.insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
                        return (
                            StatusCode::OK,
                            h,
                            AxumBytes::from_static(b"1\n00:00:00,000 --> 00:00:01,000\nHi\n"),
                        );
                    }
                    assert_eq!(body_str.matches("name=\"timestamp_granularities[]\"").count(), 2);
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "application/json".parse().unwrap());
                    (
                        StatusCode::OK,
                        h,
                        AxumBytes::from_static(
                            br#"{"text":"verbose text","language":"en","duration":1.5,"segments":[{"id":0,"start":0.0,"end":1.5,"text":"verbose text"}],"words":[{"word":"verbose","start":0.0,"end":0.7}]}"#,
                        ),
                    )
                }),
            )
            .route(
                "/v1/audio/translations",
                post(|| async {
                    (StatusCode::OK, r#"{"text":"translated text"}"#)
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let ep = MediaEndpointConfig::new("openai", format!("http://{addr}"), "whisper-1");
        let cred = MediaCredential::bearer("sk-oai-stt").unwrap();
        let cancel = CancellationToken::new();

        // Validation: timestamp_granularities without verbose_json -> InvalidRequest
        let bad_gran = OpenAiSttRequest {
            source: Some(SttAudioSource::Bytes {
                filename: "a.mp3".into(),
                mime_type: "audio/mpeg".into(),
                bytes: Bytes::from_static(b"audio"),
            }),
            response_format: Some("json".into()),
            timestamp_granularities: vec![OpenAiSttGranularity::Word],
            ..Default::default()
        };
        assert_eq!(
            openai_stt(&http, &ep, &cred, &bad_gran, &cancel)
                .await
                .unwrap_err()
                .code,
            MediaErrorCode::InvalidRequest
        );

        // verbose_json with repeated granularities
        let v_res = openai_stt(
            &http,
            &ep,
            &cred,
            &OpenAiSttRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "a.mp3".into(),
                    mime_type: "audio/mpeg".into(),
                    bytes: Bytes::from_static(b"audio"),
                }),
                response_format: Some("verbose_json".into()),
                timestamp_granularities: vec![
                    OpenAiSttGranularity::Word,
                    OpenAiSttGranularity::Segment,
                ],
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(v_res.text, "verbose text");
        assert_eq!(v_res.segments.len(), 1);
        assert_eq!(v_res.words.len(), 1);

        // Plain-text srt via execute_bytes
        let srt_res = openai_stt(
            &http,
            &ep,
            &cred,
            &OpenAiSttRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "a.mp3".into(),
                    mime_type: "audio/mpeg".into(),
                    bytes: Bytes::from_static(b"audio"),
                }),
                response_format: Some("srt".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert!(srt_res.text.contains("00:00:00,000"));

        // SSE stream on gpt-4o-transcribe
        let sse_ep =
            MediaEndpointConfig::new("openai", format!("http://{addr}"), "gpt-4o-transcribe");
        let sse_res = openai_stt(
            &http,
            &sse_ep,
            &cred,
            &OpenAiSttRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "a.mp3".into(),
                    mime_type: "audio/mpeg".into(),
                    bytes: Bytes::from_static(b"audio"),
                }),
                stream: true,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(sse_res.text, "Hello world");

        // Translation
        let tr_res = openai_stt(
            &http,
            &ep,
            &cred,
            &OpenAiSttRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "a.mp3".into(),
                    mime_type: "audio/mpeg".into(),
                    bytes: Bytes::from_static(b"audio"),
                }),
                translate: true,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(tr_res.text, "translated text");
    }

    #[tokio::test]
    async fn elevenlabs_scribe_and_gemini_audio_inline_and_resumable_files() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let poll_hits = Arc::new(AtomicUsize::new(0));
        let poll_c = poll_hits.clone();

        let app = Router::new()
            .route(
                "/v1/speech-to-text",
                post(|headers: AxumHeaders, body: AxumBytes| async move {
                    assert_eq!(headers.get("xi-api-key").and_then(|v| v.to_str().ok()), Some("xi-stt"));
                    let s = String::from_utf8_lossy(&body);
                    assert!(s.contains("scribe_v2"));
                    assert!(s.contains("diarize"));
                    (
                        StatusCode::OK,
                        r#"{"text":"scribe result","language_code":"en","words":[{"text":"scribe","start":0.0,"end":0.4,"speaker_id":"speaker_0"}]}"#,
                    )
                }),
            )
            .route(
                "/upload/v1beta/files",
                post(move |headers: AxumHeaders| async move {
                    assert_eq!(
                        headers.get("x-goog-upload-command").and_then(|v| v.to_str().ok()),
                        Some("start")
                    );
                    let mut h = AxumHeaders::new();
                    h.insert(
                        "x-goog-upload-url",
                        format!("http://127.0.0.1:{}/upload/session-1", addr.port())
                            .parse()
                            .unwrap(),
                    );
                    (StatusCode::OK, h, "")
                }),
            )
            .route(
                "/upload/session-1",
                post(|headers: AxumHeaders, body: AxumBytes| async move {
                    assert_eq!(
                        headers.get("x-goog-upload-command").and_then(|v| v.to_str().ok()),
                        Some("upload, finalize")
                    );
                    assert_eq!(body.len(), 64);
                    (
                        StatusCode::OK,
                        r#"{"file":{"name":"files/abc123","uri":"https://generativelanguage.googleapis.com/v1beta/files/abc123","state":"PROCESSING"}}"#,
                    )
                }),
            )
            .route(
                "/v1beta/files/{id}",
                get(move |AxumPath(id): AxumPath<String>| {
                    let n = poll_c.fetch_add(1, Ordering::SeqCst);
                    async move {
                        assert_eq!(id, "abc123");
                        let state = if n == 0 { "PROCESSING" } else { "ACTIVE" };
                        (
                            StatusCode::OK,
                            format!(
                                r#"{{"name":"files/abc123","uri":"https://generativelanguage.googleapis.com/v1beta/files/abc123","state":"{state}"}}"#
                            ),
                        )
                    }
                }),
            )
            .route(
                "/v1beta/models/{action}",
                post(|headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()), Some("goog-stt"));
                    let has_file_data = body.pointer("/contents/0/parts/1/fileData/fileUri").is_some();
                    let has_inline = body.pointer("/contents/0/parts/1/inlineData/data").is_some();
                    assert!(has_file_data || has_inline);
                    let txt = if has_file_data { "uploaded transcript" } else { "inline transcript" };
                    (
                        StatusCode::OK,
                        format!(r#"{{"candidates":[{{"content":{{"parts":[{{"text":"{txt}"}}]}},"finishReason":"STOP"}}]}}"#),
                    )
                }),
            );
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let cancel = CancellationToken::new();

        // ElevenLabs Scribe
        let el_ep = MediaEndpointConfig::new("elevenlabs", format!("http://{addr}"), "scribe_v2");
        let el_cred = MediaCredential::xi_api_key("xi-stt").unwrap();
        let el_res = elevenlabs_stt(
            &http,
            &el_ep,
            &el_cred,
            &ElevenLabsSttRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "clip.wav".into(),
                    mime_type: "audio/wav".into(),
                    bytes: Bytes::from_static(b"RIFF-AUDIO"),
                }),
                diarize: Some(true),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(el_res.text, "scribe result");
        assert_eq!(el_res.words[0].speaker.as_deref(), Some("speaker_0"));

        // Gemini inline vs resumable upload
        let gem_ep = MediaEndpointConfig::new(
            "gemini",
            format!("http://{addr}/v1beta"),
            "gemini-2.5-flash",
        );
        let gem_cred = MediaCredential::x_goog_api_key("goog-stt").unwrap();
        let gem_inline = gemini_stt(
            &http,
            &gem_ep,
            &gem_cred,
            &GeminiSttRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "short.mp3".into(),
                    mime_type: "audio/mpeg".into(),
                    bytes: Bytes::from(vec![1u8; 16]),
                }),
                inline_threshold_bytes: Some(32),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(gem_inline.text, "inline transcript");

        let gem_upload = gemini_stt(
            &http,
            &gem_ep,
            &gem_cred,
            &GeminiSttRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "long.mp3".into(),
                    mime_type: "audio/mpeg".into(),
                    bytes: Bytes::from(vec![2u8; 64]),
                }),
                inline_threshold_bytes: Some(32),
                allow_http_loopback_upload: true,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(gem_upload.text, "uploaded transcript");
    }

    #[tokio::test]
    async fn dashscope_paraformer_qwen_asr_and_ws() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let submit_hits = Arc::new(AtomicUsize::new(0));
        let submit_c = submit_hits.clone();
        let poll_n = Arc::new(AtomicUsize::new(0));
        let poll_c = poll_n.clone();

        let app = Router::new()
            .route(
                "/api/v1/services/audio/asr/transcription",
                post(move |headers: AxumHeaders| {
                    submit_c.fetch_add(1, Ordering::SeqCst);
                    async move {
                        assert_eq!(
                            headers.get("x-dashscope-async").and_then(|v| v.to_str().ok()),
                            Some("enable")
                        );
                        (StatusCode::OK, r#"{"output":{"task_id":"ds-asr-1"}}"#)
                    }
                }),
            )
            .route(
                "/api/v1/tasks/{id}",
                get(move |AxumPath(id): AxumPath<String>| {
                    let n = poll_c.fetch_add(1, Ordering::SeqCst);
                    async move {
                        assert_eq!(id, "ds-asr-1");
                        if n == 0 {
                            (StatusCode::OK, r#"{"output":{"task_status":"RUNNING"}}"#.to_owned())
                        } else {
                            (
                                StatusCode::OK,
                                format!(
                                    r#"{{"output":{{"task_status":"SUCCEEDED","results":[{{"subtask_status":"SUCCEEDED","transcription_url":"http://127.0.0.1:{}/dl/para.json"}}]}}}}"#,
                                    addr.port()
                                ),
                            )
                        }
                    }
                }),
            )
            .route(
                "/dl/para.json",
                get(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "application/json".parse().unwrap());
                    (
                        StatusCode::OK,
                        h,
                        r#"{"properties":{"original_duration_in_milliseconds":2500},"transcripts":[{"text":"Paraformer text","sentences":[{"sentence_id":1,"begin_time":0,"end_time":2500,"text":"Paraformer text"}]}]}"#,
                    )
                }),
            )
            .route(
                "/api/v1/services/aigc/multimodal-generation/generation",
                post(|axum::Json(b): axum::Json<Value>| async move {
                    let audio = b.pointer("/input/messages/0/content/0/audio").and_then(Value::as_str).unwrap();
                    assert!(audio.starts_with("data:audio/wav;base64,"));
                    (
                        StatusCode::OK,
                        r#"{"output":{"choices":[{"message":{"content":[{"text":"Qwen ASR text"}],"annotations":[{"language":"zh"}]}}]}}"#,
                    )
                }),
            );
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let ep = MediaEndpointConfig::new("dashscope", format!("http://{addr}"), "paraformer-v2");
        let cred = MediaCredential::bearer("ds-stt-key").unwrap();
        let cancel = CancellationToken::new();

        // Paraformer rejects raw bytes
        assert_eq!(
            build_paraformer_submit_payload(
                "paraformer-v2",
                &DashScopeSttRequest {
                    source: Some(SttAudioSource::Bytes {
                        filename: "a.wav".into(),
                        mime_type: "audio/wav".into(),
                        bytes: Bytes::from_static(b"RIFF"),
                    }),
                    ..Default::default()
                }
            )
            .unwrap_err()
            .code,
            MediaErrorCode::InvalidRequest
        );

        // Paraformer Engine 3 URL -> poll -> transcription_url download
        let para_runner = Engine3Runner::new(http.clone(), repo, artifacts)
            .with_allow_http_loopback_download(true)
            .with_poll_config(Engine3PollConfig {
                initial_interval: Duration::from_millis(2),
                max_interval: Duration::from_millis(5),
                ..Default::default()
            });
        let p_res = dashscope_paraformer_transcribe(
            &para_runner,
            &ep,
            &cred,
            &DashScopeSttRequest {
                source: Some(SttAudioSource::Url {
                    url: "https://example.com/audio.wav".into(),
                    mime_type: None,
                }),
                allow_http_loopback_download: true,
                ..Default::default()
            },
            Some("para-k1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(p_res.text, "Paraformer text");
        assert_eq!(p_res.duration_secs, Some(2.5));
        assert!(p_res.raw_artifact.is_some());

        // Idempotent replay of completed Paraformer task returns valid transcript without HTTP hits
        let hits_before_replay = submit_hits.load(Ordering::SeqCst);
        let p_replay = dashscope_paraformer_transcribe(
            &para_runner,
            &ep,
            &cred,
            &DashScopeSttRequest {
                source: Some(SttAudioSource::Url {
                    url: "https://example.com/audio.wav".into(),
                    mime_type: None,
                }),
                allow_http_loopback_download: true,
                ..Default::default()
            },
            Some("para-k1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(p_replay.text, "Paraformer text");
        assert_eq!(p_replay.duration_secs, Some(2.5));
        assert_eq!(p_replay.raw_artifact, p_res.raw_artifact);
        assert_eq!(submit_hits.load(Ordering::SeqCst), hits_before_replay);

        // Qwen-ASR short local base64
        let q_ep =
            MediaEndpointConfig::new("dashscope", format!("http://{addr}"), "qwen3-asr-flash");
        let q_res = dashscope_qwen_asr(
            &http,
            &q_ep,
            &cred,
            &DashScopeSttRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "short.wav".into(),
                    mime_type: "audio/wav".into(),
                    bytes: Bytes::from_static(b"RIFF-SHORT"),
                }),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(q_res.text, "Qwen ASR text");
        assert_eq!(q_res.language.as_deref(), Some("zh"));

        // DashScope Realtime ASR WS
        let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let ws_srv = tokio::spawn(async move {
            let (mut sock, _) = accept_ws_server(&ws_listener).await;
            let (_, p1) = read_ws_client_frame(&mut sock).await;
            let v1: Value = serde_json::from_slice(&p1).unwrap();
            let tid = v1
                .pointer("/header/task_id")
                .and_then(Value::as_str)
                .unwrap()
                .to_owned();
            sock.write_all(&encode_ws_server_frame(
                0x1,
                format!(r#"{{"header":{{"event":"task-started","task_id":"{tid}"}}}}"#).as_bytes(),
            ))
            .await
            .unwrap();
            let (op_bin, bin_pay) = read_ws_client_frame(&mut sock).await;
            assert_eq!(op_bin, 0x2);
            assert_eq!(bin_pay, b"PCM-CHUNK");
            let (_, _) = read_ws_client_frame(&mut sock).await;
            sock.write_all(&encode_ws_server_frame(
                0x1,
                format!(
                    r#"{{"header":{{"event":"result-generated","task_id":"{tid}"}},"payload":{{"output":{{"sentence":{{"text":"ws sentence","sentence_end":true}}}}}}}}"#
                )
                .as_bytes(),
            ))
            .await
            .unwrap();
            sock.write_all(&encode_ws_server_frame(
                0x1,
                format!(r#"{{"header":{{"event":"task-finished","task_id":"{tid}"}}}}"#).as_bytes(),
            ))
            .await
            .unwrap();
        });
        let ws_ep = MediaEndpointConfig::new(
            "dashscope",
            format!("ws://{ws_addr}"),
            "paraformer-realtime-v2",
        );
        let ws_res = dashscope_asr_ws(
            &ws_ep,
            &cred,
            &DashScopeSttRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "a.pcm".into(),
                    mime_type: "audio/pcm".into(),
                    bytes: Bytes::from_static(b"PCM-CHUNK"),
                }),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(ws_res.text, "ws sentence");
        ws_srv.await.unwrap();
    }

    #[tokio::test]
    async fn volcengine_asr_flash_async_post_query_and_sauc_ws() {
        let query_hits = Arc::new(AtomicUsize::new(0));
        let query_c = query_hits.clone();
        let submitted_req_id = Arc::new(Mutex::new(String::new()));
        let sub_id_c = submitted_req_id.clone();
        let sub_id_c2 = submitted_req_id.clone();

        let app = Router::new()
            .route(
                "/api/v3/auc/bigmodel/recognize/flash",
                post(|headers: AxumHeaders| async move {
                    assert_eq!(
                        headers.get("x-api-resource-id").and_then(|v| v.to_str().ok()),
                        Some("volc.bigasr.auc_turbo")
                    );
                    let mut h = AxumHeaders::new();
                    h.insert("x-api-status-code", "20000000".parse().unwrap());
                    (
                        StatusCode::OK,
                        h,
                        r#"{"result":{"text":"flash transcript"}}"#,
                    )
                }),
            )
            .route(
                "/api/v3/auc/bigmodel/submit",
                post(move |headers: AxumHeaders| {
                    let s = sub_id_c.clone();
                    async move {
                        let req_id = headers
                            .get("x-api-request-id")
                            .and_then(|v| v.to_str().ok())
                            .unwrap()
                            .to_owned();
                        *s.lock().unwrap() = req_id.clone();
                        let mut h = AxumHeaders::new();
                        h.insert("x-api-status-code", "20000000".parse().unwrap());
                        h.insert("x-api-request-id", req_id.parse().unwrap());
                        // Empty HTTP 200 body — verifies G5 execute_optional_json_with_headers!
                        (StatusCode::OK, h, "")
                    }
                }),
            )
            .route(
                "/api/v3/auc/bigmodel/query",
                post(move |headers: AxumHeaders, axum::Json(body): axum::Json<Value>| {
                    let n = query_c.fetch_add(1, Ordering::SeqCst);
                    let s = sub_id_c2.clone();
                    async move {
                        assert_eq!(body, json!({}));
                        let q_req_id = headers
                            .get("x-api-request-id")
                            .and_then(|v| v.to_str().ok())
                            .unwrap();
                        assert_eq!(q_req_id, s.lock().unwrap().as_str());
                        let mut h = AxumHeaders::new();
                        if n == 0 {
                            h.insert("x-api-status-code", "20000001".parse().unwrap());
                            (StatusCode::OK, h, "{}")
                        } else {
                            h.insert("x-api-status-code", "20000000".parse().unwrap());
                            (
                                StatusCode::OK,
                                h,
                                r#"{"result":{"text":"async volc transcript","utterances":[{"text":"async volc transcript","start_time":0,"end_time":900}]}}"#,
                            )
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let ep = MediaEndpointConfig::new("volcengine", format!("http://{addr}"), "bigmodel");
        let cred = MediaCredential::volcengine_v3_api_key("volc-asr-key").unwrap();
        let cancel = CancellationToken::new();

        // 1. Flash
        let flash = volcengine_asr_flash(
            &http,
            &ep,
            &cred,
            &VolcengineAsrRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "f.wav".into(),
                    mime_type: "audio/wav".into(),
                    bytes: Bytes::from_static(b"RIFF"),
                }),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(flash.text, "flash transcript");

        // 2. Async Engine 3 (empty submit body + POST /query {} + X-Api-Status-Code)
        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let (adapter, keys) = VolcengineAsrAdapter::from_endpoint(&ep, &cred, None).unwrap();
        let payload = build_volcengine_asr_payload(&VolcengineAsrRequest {
            source: Some(SttAudioSource::Url {
                url: "https://example.com/long.wav".into(),
                mime_type: None,
            }),
            ..Default::default()
        })
        .unwrap();
        let runner =
            Engine3Runner::new(http, repo, artifacts).with_poll_config(Engine3PollConfig {
                initial_interval: Duration::from_millis(2),
                max_interval: Duration::from_millis(5),
                ..Default::default()
            });
        let done = runner
            .run_to_completion_with_outputs(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("volc-asr-1".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "bigmodel".into(),
                    route_id: None,
                    payload,
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        let parsed_val: Value = serde_json::from_slice(&done.payloads[0].1).unwrap();
        let parsed = parse_volcengine_asr_response("bigmodel", &parsed_val, None).unwrap();
        assert_eq!(parsed.text, "async volc transcript");
        assert_eq!(parsed.segments.len(), 1);

        // 3. SAUC WS
        let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let ws_srv = tokio::spawn(async move {
            let (mut sock, hdr) = accept_ws_server(&ws_listener).await;
            assert!(hdr.contains("volc.bigasr.sauc.duration"));
            let (_, f_cfg) = read_ws_client_frame(&mut sock).await;
            let dec_cfg = volcengine_frame::decode_frame(&f_cfg).unwrap();
            assert_eq!(dec_cfg.msg_type, volcengine_frame::MSG_FULL_CLIENT_REQUEST);
            let cfg_json: Value = serde_json::from_slice(&dec_cfg.payload).unwrap();
            assert_eq!(
                cfg_json
                    .pointer("/request/language")
                    .and_then(Value::as_str),
                Some("zh-CN")
            );
            let (_, f_aud) = read_ws_client_frame(&mut sock).await;
            let dec_aud = volcengine_frame::decode_frame(&f_aud).unwrap();
            assert_eq!(dec_aud.msg_type, volcengine_frame::MSG_AUDIO_ONLY_CLIENT);
            assert_eq!(dec_aud.flags, volcengine_frame::FLAG_LAST_AUDIO);
            sock.write_all(&encode_ws_server_frame(
                0x2,
                &volcengine_frame::encode_simple_frame(
                    volcengine_frame::MSG_FULL_SERVER_RESPONSE,
                    volcengine_frame::FLAG_LAST_AUDIO,
                    volcengine_frame::SERIAL_JSON,
                    br#"{"is_last":true,"result":{"text":"sauc ws text"}}"#,
                ),
            ))
            .await
            .unwrap();
        });
        let ws_ep = MediaEndpointConfig::new("volcengine", format!("ws://{ws_addr}"), "bigmodel");
        let sauc = volcengine_sauc_ws(
            &ws_ep,
            &cred,
            &VolcengineAsrRequest {
                source: Some(SttAudioSource::Bytes {
                    filename: "s.pcm".into(),
                    mime_type: "audio/pcm".into(),
                    bytes: Bytes::from_static(b"PCM-SAUC"),
                }),
                language: Some("zh-CN".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(sauc.text, "sauc ws text");
        ws_srv.await.unwrap();
    }

    #[tokio::test]
    async fn local_asr_pure_helpers_and_mock_runner() {
        for (ext, expected) in [
            (".wav", "wav"),
            ("MP3", "mp3"),
            (".m4a", "mov"),
            (".aac", "aac"),
            (".aif", "aiff"),
            (".aiff", "aiff"),
            (".caf", "caf"),
            (".flac", "flac"),
            (".ogg", "ogg"),
            (".opus", "ogg"),
            (".webm", "matroska"),
        ] {
            assert_eq!(demuxer_for_audio_extension(ext), Some(expected));
        }
        assert_eq!(demuxer_for_audio_extension(".txt"), None);
        assert_eq!(normalize_whisper_language(Some("en-US")), "en");
        assert_eq!(normalize_whisper_language(None), "zh");

        assert_eq!(
            parse_ffprobe_duration_json(br#"{"format":{"duration":"12.5"}}"#).unwrap(),
            12.5
        );
        assert_eq!(
            parse_ffprobe_duration_json(br#"{"format":{"duration":1200.0}}"#).unwrap(),
            1200.0
        );
        assert!(parse_ffprobe_duration_json(br#"{"format":{"duration":"0"}}"#).is_err());
        assert!(parse_ffprobe_duration_json(br#"{"format":{"duration":"1200.1"}}"#).is_err());

        let args = build_whisper_cli_args("m.bin", "a.wav", "en", 16, "out");
        assert!(args.windows(2).any(|w| w == ["--threads", "4"]));
        assert!(args.contains(&"--no-gpu".to_owned()));

        struct MockRunner;
        impl LocalAsrRunner for MockRunner {
            async fn status<'a>(
                &'a self,
                _lang: Option<&'a str>,
            ) -> Result<LocalAsrStatus, MediaError> {
                Ok(LocalAsrStatus {
                    supported: true,
                    executable_path: "/usr/bin/whisper-cli".into(),
                    model_path: "/models/ggml-small-q5_1.bin".into(),
                    ffmpeg_path: "/usr/bin/ffmpeg".into(),
                    ffprobe_path: "/usr/bin/ffprobe".into(),
                    reason: None,
                })
            }

            async fn transcribe<'a>(
                &'a self,
                _req: &'a LocalAsrRequest,
                _cancel: &'a CancellationToken,
            ) -> Result<SttTranscript, MediaError> {
                Ok(SttTranscript {
                    vendor: "whisper".into(),
                    model: "ggml-small-q5_1.bin".into(),
                    text: "mock whisper transcript".into(),
                    language: Some("en".into()),
                    duration_secs: Some(3.2),
                    segments: Vec::new(),
                    words: Vec::new(),
                    raw_artifact: None,
                })
            }
        }

        let cancel = CancellationToken::new();
        let bad_ext = local_whisper_transcribe(
            &MockRunner,
            &LocalAsrRequest {
                input: LocalAsrInput::Bytes {
                    filename: "bad.exe".into(),
                    bytes: Bytes::from_static(b"123"),
                },
                lang: Some("en-US".into()),
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(bad_ext.code, MediaErrorCode::InvalidRequest);

        let ok = local_whisper_transcribe(
            &MockRunner,
            &LocalAsrRequest {
                input: LocalAsrInput::Bytes {
                    filename: "good.opus".into(),
                    bytes: Bytes::from_static(b"OggS"),
                },
                lang: Some("en-US".into()),
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(ok.text, "mock whisper transcript");
    }

    #[tokio::test]
    async fn gemini_upload_url_rejects_cross_origin_and_cross_port_loopback() {
        let evil_hits = Arc::new(AtomicUsize::new(0));
        let evil_c = evil_hits.clone();
        let evil_app = Router::new().route(
            "/steal",
            post(move || {
                evil_c.fetch_add(1, Ordering::SeqCst);
                async { (StatusCode::OK, r#"{"stolen":true}"#) }
            }),
        );
        let evil_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let evil_addr = evil_listener.local_addr().unwrap();
        let _evil_srv =
            tokio::spawn(async move { axum::serve(evil_listener, evil_app).await.unwrap() });

        let mode = Arc::new(Mutex::new("cross-port".to_owned()));
        let mode_c = mode.clone();
        let gem_app = Router::new().route(
            "/upload/v1beta/files",
            post(move || {
                let m = mode_c.lock().unwrap().clone();
                async move {
                    let mut h = AxumHeaders::new();
                    let target = if m == "cross-port" {
                        format!("http://127.0.0.1:{}/steal", evil_addr.port())
                    } else {
                        "https://evil.example.com/steal".to_owned()
                    };
                    h.insert("x-goog-upload-url", target.parse().unwrap());
                    (StatusCode::OK, h, "{}")
                }
            }),
        );
        let gem_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gem_addr = gem_listener.local_addr().unwrap();
        let _gem_srv =
            tokio::spawn(async move { axum::serve(gem_listener, gem_app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let ep = MediaEndpointConfig::new(
            "gemini",
            format!("http://{gem_addr}/v1beta"),
            "gemini-2.5-flash",
        );
        let cred = MediaCredential::x_goog_api_key("gem-secret-key").unwrap();
        let cancel = CancellationToken::new();
        let req = GeminiSttRequest {
            source: Some(SttAudioSource::Bytes {
                filename: "big.mp3".into(),
                mime_type: "audio/mpeg".into(),
                bytes: Bytes::from(vec![7u8; 64]),
            }),
            inline_threshold_bytes: Some(32),
            allow_http_loopback_upload: true,
            ..Default::default()
        };

        // 1. Cross-port loopback redirect -> InvalidRequest, 0 hits on untrusted server
        let err_port = gemini_stt(&http, &ep, &cred, &req, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err_port.code, MediaErrorCode::InvalidRequest);
        assert_eq!(evil_hits.load(Ordering::SeqCst), 0);

        // 2. Cross-origin HTTPS redirect -> InvalidRequest, 0 outbound requests
        *mode.lock().unwrap() = "https-evil".to_owned();
        let err_https = gemini_stt(&http, &ep, &cred, &req, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err_https.code, MediaErrorCode::InvalidRequest);
        assert_eq!(evil_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn engine3_stt_idempotency_replay_handles_succeeded_failed_and_cancelled() {
        let submit_hits = Arc::new(AtomicUsize::new(0));
        let sub_c = submit_hits.clone();
        let query_hits = Arc::new(AtomicUsize::new(0));
        let q_c = query_hits.clone();

        let app = Router::new()
            .route(
                "/api/v3/auc/bigmodel/submit",
                post(
                    move |headers: AxumHeaders, axum::Json(body): axum::Json<Value>| {
                        sub_c.fetch_add(1, Ordering::SeqCst);
                        async move {
                            let req_id = headers
                                .get("x-api-request-id")
                                .and_then(|v| v.to_str().ok())
                                .unwrap()
                                .to_owned();
                            let url = body
                                .pointer("/audio/url")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let tag = if url.contains("fail") {
                                format!("fail-{req_id}")
                            } else {
                                format!("ok-{req_id}")
                            };
                            let mut h = AxumHeaders::new();
                            h.insert("x-api-status-code", "20000000".parse().unwrap());
                            h.insert("x-api-request-id", tag.parse().unwrap());
                            (StatusCode::OK, h, "")
                        }
                    },
                ),
            )
            .route(
                "/api/v3/auc/bigmodel/query",
                post(move |headers: AxumHeaders| {
                    q_c.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let req_id = headers
                            .get("x-api-request-id")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");
                        let mut h = AxumHeaders::new();
                        if req_id.starts_with("fail-") {
                            h.insert("x-api-status-code", "45000001".parse().unwrap());
                            h.insert("x-api-message", "volc upstream rejected".parse().unwrap());
                            (StatusCode::OK, h, "{}")
                        } else {
                            h.insert("x-api-status-code", "20000000".parse().unwrap());
                            (
                                StatusCode::OK,
                                h,
                                r#"{"result":{"text":"replayed volc text"}}"#,
                            )
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner =
            Engine3Runner::new(http, repo.clone(), artifacts).with_poll_config(Engine3PollConfig {
                initial_interval: Duration::from_millis(1),
                max_interval: Duration::from_millis(2),
                ..Default::default()
            });
        let ep = MediaEndpointConfig::new("volcengine", format!("http://{addr}"), "bigmodel");
        let cred = MediaCredential::volcengine_v3_api_key("volc-idem-key").unwrap();
        let cancel = CancellationToken::new();

        // 1. Succeeded task replay returns valid transcript + raw_artifact without extra HTTP
        let ok_req = VolcengineAsrRequest {
            source: Some(SttAudioSource::Url {
                url: "https://example.com/ok.wav".into(),
                mime_type: None,
            }),
            ..Default::default()
        };
        let first = volcengine_asr_transcribe(
            &runner,
            &ep,
            &cred,
            &ok_req,
            Some("volc-ok-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(first.text, "replayed volc text");
        assert!(first.raw_artifact.is_some());
        let sub_after_first = submit_hits.load(Ordering::SeqCst);
        let q_after_first = query_hits.load(Ordering::SeqCst);

        let replay = volcengine_asr_transcribe(
            &runner,
            &ep,
            &cred,
            &ok_req,
            Some("volc-ok-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(replay.text, "replayed volc text");
        assert_eq!(replay.raw_artifact, first.raw_artifact);
        assert_eq!(submit_hits.load(Ordering::SeqCst), sub_after_first);
        assert_eq!(query_hits.load(Ordering::SeqCst), q_after_first);

        // 2. Failed task replay surfaces persisted ProviderError (not DecodeError)
        let fail_req = VolcengineAsrRequest {
            source: Some(SttAudioSource::Url {
                url: "https://example.com/fail.wav".into(),
                mime_type: None,
            }),
            ..Default::default()
        };
        let err1 = volcengine_asr_transcribe(
            &runner,
            &ep,
            &cred,
            &fail_req,
            Some("volc-fail-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(err1.code, MediaErrorCode::ProviderError);

        let err2 = volcengine_asr_transcribe(
            &runner,
            &ep,
            &cred,
            &fail_req,
            Some("volc-fail-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(err2.code, MediaErrorCode::ProviderError);
        assert_eq!(err2.upstream_code.as_deref(), Some("45000001"));
        assert!(err2.message.contains("volc upstream rejected"));

        // 3. Cancelled task replay surfaces MediaErrorCode::Cancelled (not DecodeError)
        let (adapter, keys) = VolcengineAsrAdapter::from_endpoint(&ep, &cred, None).unwrap();
        let cancel_req = VolcengineAsrRequest {
            source: Some(SttAudioSource::Url {
                url: "https://example.com/cancel.wav".into(),
                mime_type: None,
            }),
            ..Default::default()
        };
        let cancel_payload = build_volcengine_asr_payload(&cancel_req).unwrap();
        let (submitted, _) = runner
            .submit_and_persist(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("volc-cancel-1".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "bigmodel".into(),
                    route_id: None,
                    payload: cancel_payload,
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        repo.mark_cancelled(&submitted.id, Some("user aborted"))
            .unwrap();

        let err_cancel = volcengine_asr_transcribe(
            &runner,
            &ep,
            &cred,
            &cancel_req,
            Some("volc-cancel-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(err_cancel.code, MediaErrorCode::Cancelled);
        assert!(err_cancel.message.contains("user aborted"));
    }
}
