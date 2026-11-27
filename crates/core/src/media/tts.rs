//! D.4 Text-to-Speech (TTS) adapters across Engines 2, 3, and 4:
//! 1. OpenAI TTS (`POST /v1/audio/speech`, binary & SSE)
//! 2. Gemini TTS (`POST /v1beta/models/{model}:generateContent`, 24kHz PCM -> 44-byte RIFF `.wav`)
//! 3. ElevenLabs TTS (`POST /v1/text-to-speech/{voice_id}[/stream]`, `xi-api-key`)
//! 4. Azure Speech REST (`POST /cognitiveservices/v1`, `application/ssml+xml` + `User-Agent`)
//! 5. MiniMax TTS (Engine 2 sync/SSE `/v1/t2a_v2`, Engine 4 WS `/ws/v1/t2a_v2`, Engine 3 async `/v1/t2a_async_v2`)
//! 6. Volcengine / Doubao TTS & Official Seed-Audio (V3 HTTP NDJSON, Seed-Audio `/api/v3/tts/create`, V3 Async, V3 Bidi WS, Legacy V1 HTTP & WS `Bearer;<token>`)
//! 7. Alibaba DashScope TTS (Qwen-TTS HTTP/SSE, Bailian `SpeechSynthesizer` HTTP, CosyVoice WS)
//! 8. Partner Gateway Compat Seed Audio *(strictly isolated)* (`POST /v1/tts/create` -> `POST /v1/tts/getAudioStatus`)

use super::engine2::{
    Engine2HybridResponse, Engine2Request, MEDIA_DEFAULT_ARTIFACT_LIMIT, MediaHttpClient,
    StreamControl, check_standard_business_status, current_unix_i64, decode_base64_bytes,
    decode_hex, pcm16_mono_to_wav,
};
use super::engine3::{
    Engine3Adapter, Engine3Runner, Engine3SubmitInput, MediaOutputSource, PollOutcome,
    SubmitOutcome,
};
use super::vectors::build_gemini_v1beta_base;
use super::ws::{WsConnectConfig, WsMessage, connect_ws};
use super::{
    MediaArtifactRef, MediaArtifactStore, MediaAuthKind, MediaCredential, MediaEndpointConfig,
    MediaError, MediaErrorCode, MediaKind, MediaTaskRecord, MediaTaskRepository, MediaTaskStatus,
    http_url_to_ws_url, is_loopback_host, normalize_gemini_model_id, validate_media_download_url,
    validate_media_url,
};
use crate::redaction::CredentialSet;
#[cfg(test)]
use base64::Engine as _;
#[cfg(test)]
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub const MAX_OPENAI_TTS_INPUT_CHARS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsAudioFormat {
    Mp3,
    Opus,
    OggOpus,
    Aac,
    Flac,
    Wav,
    Pcm,
    Mulaw,
    Alaw,
}

impl TtsAudioFormat {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mp3" => Some(Self::Mp3),
            "opus" => Some(Self::Opus),
            "ogg_opus" | "ogg-opus" => Some(Self::OggOpus),
            "aac" => Some(Self::Aac),
            "flac" => Some(Self::Flac),
            "wav" => Some(Self::Wav),
            "pcm" | "pcm16" => Some(Self::Pcm),
            "mulaw" | "ulaw" => Some(Self::Mulaw),
            "alaw" => Some(Self::Alaw),
            _ => None,
        }
    }

    pub const fn as_openai_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus | Self::OggOpus => "opus",
            Self::Aac => "aac",
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Pcm | Self::Mulaw | Self::Alaw => "pcm",
        }
    }

    pub const fn as_minimax_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Pcm | Self::Mulaw | Self::Alaw => "pcm",
            Self::Opus | Self::OggOpus | Self::Aac => "mp3",
        }
    }

    pub const fn as_volcengine_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus | Self::OggOpus => "ogg_opus",
            Self::Wav => "wav",
            Self::Pcm | Self::Mulaw | Self::Alaw => "pcm",
            Self::Aac => "aac",
            Self::Flac => "mp3",
        }
    }

    pub const fn as_partner_seed_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus | Self::OggOpus => "ogg_opus",
            Self::Aac => "aac",
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Pcm | Self::Mulaw | Self::Alaw => "pcm",
        }
    }

    pub const fn mime_and_ext(self) -> (&'static str, &'static str) {
        match self {
            Self::Mp3 => ("audio/mpeg", "mp3"),
            Self::Opus => ("audio/opus", "opus"),
            Self::OggOpus => ("audio/ogg", "ogg"),
            Self::Aac => ("audio/aac", "aac"),
            Self::Flac => ("audio/flac", "flac"),
            Self::Wav => ("audio/wav", "wav"),
            Self::Pcm => ("audio/pcm", "pcm"),
            Self::Mulaw => ("audio/basic", "ulaw"),
            Self::Alaw => ("audio/alaw", "alaw"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TtsSynthesisResponse {
    pub vendor: String,
    pub model: String,
    pub audio: MediaArtifactRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

pub(crate) fn header_request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .or_else(|| headers.get("x-api-request-id"))
        .or_else(|| headers.get("request-id"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn detect_audio_mime_and_ext(
    bytes: &[u8],
    fallback: Option<TtsAudioFormat>,
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
        fallback.unwrap_or(TtsAudioFormat::Mp3).mime_and_ext()
    }
}

fn parse_sample_rate_from_mime(mime: &str) -> Option<u32> {
    for part in mime.split(';') {
        let trimmed = part.trim();
        if let Some(val) = trimmed
            .strip_prefix("rate=")
            .or_else(|| trimmed.strip_prefix("RATE="))
            && let Ok(rate) = val.trim().parse::<u32>()
            && rate > 0
        {
            return Some(rate);
        }
    }
    None
}

pub(crate) fn ensure_auth_kind(
    cred: &MediaCredential,
    endpoint: &MediaEndpointConfig,
    allowed: &[MediaAuthKind],
    vendor_label: &str,
) -> Result<(), MediaError> {
    if endpoint.embedded_proxy && cred.kind == MediaAuthKind::ProxyToken {
        return Ok(());
    }
    if allowed.contains(&cred.kind) {
        return Ok(());
    }
    Err(MediaError::invalid_request(format!(
        "{vendor_label} requires compatible authentication kind (got {:?})",
        cred.kind
    )))
}

// ============================================================================
// 1. OpenAI TTS (Engine 2: POST /v1/audio/speech)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAiTtsStreamFormat {
    Audio,
    Sse,
}

#[derive(Clone, Debug, Default)]
pub struct OpenAiTtsRequest {
    pub input: String,
    pub voice: String,
    pub instructions: Option<String>,
    pub response_format: Option<TtsAudioFormat>,
    pub speed: Option<f64>,
    pub stream_format: Option<OpenAiTtsStreamFormat>,
}

pub fn build_openai_speech_url(base_url: &str, embedded_proxy: bool) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy
        || url
            .path()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/v1")
    {
        Ok(format!("{trimmed}/audio/speech"))
    } else {
        Ok(format!("{trimmed}/v1/audio/speech"))
    }
}

pub async fn openai_tts<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &OpenAiTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "OpenAI TTS",
    )?;
    if endpoint.model.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "OpenAI TTS model must not be empty",
        ));
    }
    if req.input.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "OpenAI TTS input must not be empty",
        ));
    }
    if req.input.chars().count() > MAX_OPENAI_TTS_INPUT_CHARS {
        return Err(MediaError::invalid_request(format!(
            "OpenAI TTS input exceeds {MAX_OPENAI_TTS_INPUT_CHARS} character limit"
        )));
    }
    if req.voice.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "OpenAI TTS voice must not be empty",
        ));
    }
    if let Some(speed) = req.speed
        && (!speed.is_finite() || !(0.25..=4.0).contains(&speed))
    {
        return Err(MediaError::invalid_request(
            "OpenAI TTS speed must be between 0.25 and 4.0",
        ));
    }

    let url = build_openai_speech_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let format = req.response_format.unwrap_or(TtsAudioFormat::Mp3);
    let mut body = json!({
        "model": endpoint.model,
        "input": req.input,
        "voice": req.voice,
        "response_format": format.as_openai_str(),
    });
    if let Some(instructions) = &req.instructions
        && !instructions.trim().is_empty()
    {
        body["instructions"] = json!(instructions);
    }
    if let Some(speed) = req.speed {
        body["speed"] = json!(speed);
    }
    let stream_fmt = req.stream_format.unwrap_or(OpenAiTtsStreamFormat::Audio);
    if let Some(sf) = req.stream_format {
        body["stream_format"] = json!(match sf {
            OpenAiTtsStreamFormat::Audio => "audio",
            OpenAiTtsStreamFormat::Sse => "sse",
        });
    }

    let (mime, ext) = format.mime_and_ext();

    match stream_fmt {
        OpenAiTtsStreamFormat::Audio => {
            let http_req = Engine2Request::post_json(url, body)
                .with_headers(headers, sensitive)
                .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
            let resp = http.execute_bytes(&http_req, &keys, cancel).await?;
            let request_id = header_request_id(&resp.headers);
            let (det_mime, det_ext) = detect_audio_mime_and_ext(&resp.bytes, Some(format));
            let artifact = artifacts
                .write_artifact(owner_id, det_ext, det_mime, &resp.bytes)
                .await?;
            Ok(TtsSynthesisResponse {
                vendor: "openai".into(),
                model: endpoint.model.clone(),
                audio: artifact,
                sample_rate: None,
                duration_ms: None,
                request_id,
                task_id: None,
            })
        }
        OpenAiTtsStreamFormat::Sse => {
            let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
            let mut audio_buf: Vec<u8> = Vec::new();
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
                            format!("invalid OpenAI TTS SSE JSON frame: {e}"),
                        )
                    })?;
                    if let Some(err_obj) = val.get("error") {
                        let msg = err_obj
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("OpenAI TTS SSE error");
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
                    if ev_type == "speech.audio.done" {
                        if let Some(b64) = val.get("audio").and_then(Value::as_str)
                            && !b64.trim().is_empty()
                        {
                            audio_buf.extend_from_slice(&decode_base64_bytes(b64)?);
                        }
                        return Ok(StreamControl::Done);
                    }
                    if let Some(b64) = val.get("audio").and_then(Value::as_str)
                        && !b64.trim().is_empty()
                    {
                        audio_buf.extend_from_slice(&decode_base64_bytes(b64)?);
                    }
                    Ok(StreamControl::Continue)
                },
                cancel,
            )
            .await?;

            if audio_buf.is_empty() {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    "OpenAI TTS SSE stream completed without audio chunks",
                ));
            }
            let (det_mime, det_ext) = if format == TtsAudioFormat::Pcm {
                (mime, ext)
            } else {
                detect_audio_mime_and_ext(&audio_buf, Some(format))
            };
            let artifact = artifacts
                .write_artifact(owner_id, det_ext, det_mime, &audio_buf)
                .await?;
            Ok(TtsSynthesisResponse {
                vendor: "openai".into(),
                model: endpoint.model.clone(),
                audio: artifact,
                sample_rate: None,
                duration_ms: None,
                request_id: None,
                task_id: None,
            })
        }
    }
}

// ============================================================================
// 2. Gemini TTS (Engine 2: POST /v1beta/models/{model}:generateContent)
// ============================================================================

#[derive(Clone, Debug)]
pub struct GeminiSpeakerVoice {
    pub speaker: String,
    pub voice_name: String,
}

#[derive(Clone, Debug)]
pub enum GeminiSpeechVoice {
    Single(String),
    MultiSpeaker(Vec<GeminiSpeakerVoice>),
}

#[derive(Clone, Debug)]
pub struct GeminiTtsRequest {
    pub text: String,
    pub voice: GeminiSpeechVoice,
}

pub async fn gemini_tts<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &GeminiTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::XGoogApiKey, MediaAuthKind::ProxyToken],
        "Gemini TTS",
    )?;
    if req.text.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Gemini TTS text must not be empty",
        ));
    }
    let model_id = normalize_gemini_model_id(&endpoint.model)?;
    let base = build_gemini_v1beta_base(&endpoint.base_url, endpoint.embedded_proxy)?;
    let url = format!("{base}/models/{model_id}:generateContent");

    let speech_config = match &req.voice {
        GeminiSpeechVoice::Single(voice_name) => {
            if voice_name.trim().is_empty() {
                return Err(MediaError::invalid_request(
                    "Gemini TTS voice_name must not be empty",
                ));
            }
            json!({
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": voice_name
                    }
                }
            })
        }
        GeminiSpeechVoice::MultiSpeaker(speakers) => {
            if speakers.is_empty() {
                return Err(MediaError::invalid_request(
                    "Gemini multi-speaker TTS requires at least one speaker config",
                ));
            }
            let configs: Vec<Value> = speakers
                .iter()
                .map(|s| {
                    if s.speaker.trim().is_empty() || s.voice_name.trim().is_empty() {
                        return Err(MediaError::invalid_request(
                            "Gemini multi-speaker entry must have non-empty speaker and voice_name",
                        ));
                    }
                    Ok(json!({
                        "speaker": s.speaker,
                        "voiceConfig": {
                            "prebuiltVoiceConfig": {
                                "voiceName": s.voice_name
                            }
                        }
                    }))
                })
                .collect::<Result<_, _>>()?;
            json!({
                "multiSpeakerVoiceConfig": {
                    "speakerVoiceConfigs": configs
                }
            })
        }
    };

    let body = json!({
        "contents": [{
            "parts": [{ "text": req.text }]
        }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": speech_config
        }
    });

    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    let http_req = Engine2Request::post_json(url, body)
        .with_headers(headers, sensitive)
        .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    if let Some(block_reason) = resp
        .body
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        return Err(MediaError::new(
            MediaErrorCode::ProviderError,
            format!("Gemini TTS prompt blocked: {block_reason}"),
        ));
    }

    let candidates = resp
        .body
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing candidates[] in Gemini TTS response",
            )
        })?;

    let mut found_inline: Option<(String, String)> = None;
    for cand in candidates {
        if let Some(parts) = cand.pointer("/content/parts").and_then(Value::as_array) {
            for part in parts {
                if let Some(inline) = part.get("inlineData").or_else(|| part.get("inline_data"))
                    && let Some(data) = inline.get("data").and_then(Value::as_str)
                    && !data.trim().is_empty()
                {
                    let mime = inline
                        .get("mimeType")
                        .or_else(|| inline.get("mime_type"))
                        .and_then(Value::as_str)
                        .unwrap_or("audio/L16;codec=pcm;rate=24000")
                        .to_owned();
                    found_inline = Some((data.to_owned(), mime));
                    break;
                }
            }
        }
        if found_inline.is_some() {
            break;
        }
    }

    let Some((b64, mime_type)) = found_inline else {
        let finish_reason = candidates
            .first()
            .and_then(|c| c.get("finishReason"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(MediaError::new(
            MediaErrorCode::ProviderError,
            format!("Gemini TTS returned no audio (finishReason: {finish_reason})"),
        ));
    };

    let raw_bytes = decode_base64_bytes(&b64)?;
    let lower_mime = mime_type.to_ascii_lowercase();
    let sample_rate = parse_sample_rate_from_mime(&mime_type).unwrap_or(24_000);
    let is_already_wav =
        raw_bytes.len() >= 12 && &raw_bytes[0..4] == b"RIFF" && &raw_bytes[8..12] == b"WAVE";
    let is_raw_pcm = !is_already_wav
        && (lower_mime.starts_with("audio/l16")
            || lower_mime.starts_with("audio/pcm")
            || (!lower_mime.contains("mpeg")
                && !lower_mime.contains("mp3")
                && !lower_mime.contains("ogg")
                && !lower_mime.contains("opus")
                && !lower_mime.contains("flac")
                && !lower_mime.contains("aac")));

    let (final_bytes, final_mime, final_ext) = if is_raw_pcm {
        (
            pcm16_mono_to_wav(&raw_bytes, sample_rate),
            "audio/wav",
            "wav",
        )
    } else if is_already_wav {
        (raw_bytes, "audio/wav", "wav")
    } else {
        let (m, e) = detect_audio_mime_and_ext(&raw_bytes, Some(TtsAudioFormat::Wav));
        (raw_bytes, m, e)
    };

    let artifact = artifacts
        .write_artifact(owner_id, final_ext, final_mime, &final_bytes)
        .await?;

    Ok(TtsSynthesisResponse {
        vendor: "gemini".into(),
        model: model_id.to_owned(),
        audio: artifact,
        sample_rate: Some(sample_rate),
        duration_ms: None,
        request_id: header_request_id(&resp.headers),
        task_id: None,
    })
}

// ============================================================================
// 3. ElevenLabs TTS (Engine 2: POST /v1/text-to-speech/{voice_id}[/stream])
// ============================================================================

#[derive(Clone, Debug, Default, Serialize)]
pub struct ElevenLabsVoiceSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stability: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity_boost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_speaker_boost: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct ElevenLabsTtsRequest {
    pub voice_id: String,
    pub text: String,
    pub output_format: Option<String>,
    pub stream: bool,
    pub voice_settings: Option<ElevenLabsVoiceSettings>,
    pub language_code: Option<String>,
    pub seed: Option<u64>,
}

fn validate_path_segment(label: &str, value: &str) -> Result<String, MediaError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed != value
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains("..")
        || trimmed.bytes().any(|b| {
            b.is_ascii_control()
                || b.is_ascii_whitespace()
                || matches!(b, b'/' | b'\\' | b'?' | b'#' | b'%' | b'@')
        })
    {
        return Err(MediaError::invalid_request(format!(
            "invalid {label}: contains forbidden characters"
        )));
    }
    Ok(trimmed.to_owned())
}

pub fn build_elevenlabs_tts_url(
    base_url: &str,
    voice_id: &str,
    stream: bool,
    output_format: Option<&str>,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let voice = validate_path_segment("ElevenLabs voice_id", voice_id)?;
    let trimmed = url.as_str().trim_end_matches('/');
    let prefix = if embedded_proxy
        || url
            .path()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/v1")
    {
        format!("{trimmed}/text-to-speech/{voice}")
    } else {
        format!("{trimmed}/v1/text-to-speech/{voice}")
    };
    let mut full = if stream {
        format!("{prefix}/stream")
    } else {
        prefix
    };
    if let Some(fmt) = output_format.map(str::trim).filter(|s| !s.is_empty()) {
        let mut parsed = url::Url::parse(&full)
            .map_err(|_| MediaError::invalid_request("invalid ElevenLabs TTS URL"))?;
        parsed.query_pairs_mut().append_pair("output_format", fmt);
        full = parsed.to_string();
    }
    Ok(full)
}

fn elevenlabs_mime_ext(output_format: Option<&str>, bytes: &[u8]) -> (&'static str, &'static str) {
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
    detect_audio_mime_and_ext(bytes, Some(TtsAudioFormat::Mp3))
}

pub async fn elevenlabs_tts<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &ElevenLabsTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(cred, endpoint, &[MediaAuthKind::XiApiKey], "ElevenLabs TTS")?;
    if req.text.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "ElevenLabs TTS text must not be empty",
        ));
    }

    let url = build_elevenlabs_tts_url(
        &endpoint.base_url,
        &req.voice_id,
        req.stream,
        req.output_format.as_deref(),
        endpoint.embedded_proxy,
    )?;

    let mut body = json!({
        "text": req.text,
    });
    if !endpoint.model.trim().is_empty() {
        body["model_id"] = json!(endpoint.model);
    }
    if let Some(vs) = &req.voice_settings {
        body["voice_settings"] =
            serde_json::to_value(vs).map_err(|e| MediaError::invalid_request(e.to_string()))?;
    }
    if let Some(lang) = &req.language_code {
        body["language_code"] = json!(lang);
    }
    if let Some(seed) = req.seed {
        body["seed"] = json!(seed);
    }

    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    let http_req = Engine2Request::post_json(url, body)
        .with_headers(headers, sensitive)
        .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
    let resp = http.execute_bytes(&http_req, &keys, cancel).await?;
    let (mime, ext) = elevenlabs_mime_ext(req.output_format.as_deref(), &resp.bytes);
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &resp.bytes)
        .await?;

    Ok(TtsSynthesisResponse {
        vendor: "elevenlabs".into(),
        model: endpoint.model.clone(),
        audio: artifact,
        sample_rate: None,
        duration_ms: None,
        request_id: header_request_id(&resp.headers),
        task_id: None,
    })
}

// ============================================================================
// 4. Azure Speech REST (Engine 2: POST /cognitiveservices/v1)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct AzureTtsRequest {
    pub text: String,
    pub raw_ssml: Option<String>,
    pub voice: String,
    pub lang: String,
    pub output_format: String,
    pub user_agent: Option<String>,
    pub rate: Option<String>,
    pub pitch: Option<String>,
}

pub fn azure_tts_regional_base(region: &str) -> Result<String, MediaError> {
    let clean = validate_path_segment("Azure region", region)?;
    Ok(format!(
        "https://{}.tts.speech.microsoft.com/cognitiveservices/v1",
        clean.to_ascii_lowercase()
    ))
}

pub fn build_azure_tts_url(base_url: &str, embedded_proxy: bool) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy
        || url
            .path()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/cognitiveservices/v1")
    {
        Ok(trimmed.to_owned())
    } else {
        Ok(format!("{trimmed}/cognitiveservices/v1"))
    }
}

fn xml_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn azure_mime_ext(output_format: &str, bytes: &[u8]) -> (&'static str, &'static str) {
    let lower = output_format.to_ascii_lowercase();
    if lower.contains("mp3") {
        ("audio/mpeg", "mp3")
    } else if lower.contains("riff") {
        ("audio/wav", "wav")
    } else if lower.contains("opus") {
        ("audio/ogg", "opus")
    } else if lower.contains("raw") {
        ("audio/pcm", "pcm")
    } else {
        detect_audio_mime_and_ext(bytes, Some(TtsAudioFormat::Mp3))
    }
}

pub async fn azure_tts<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &AzureTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::AzureSubscriptionKey, MediaAuthKind::Bearer],
        "Azure Speech TTS",
    )?;
    if req.output_format.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Azure TTS output_format (X-Microsoft-OutputFormat) must not be empty",
        ));
    }

    let ssml = if let Some(raw) = &req.raw_ssml {
        let trimmed = raw.trim();
        if trimmed.is_empty() || !trimmed.contains("<speak") {
            return Err(MediaError::invalid_request(
                "Azure TTS raw_ssml must be a valid <speak> SSML document",
            ));
        }
        trimmed.to_owned()
    } else {
        if req.text.trim().is_empty() {
            return Err(MediaError::invalid_request(
                "Azure TTS text must not be empty",
            ));
        }
        if req.voice.trim().is_empty() {
            return Err(MediaError::invalid_request(
                "Azure TTS voice must not be empty",
            ));
        }
        let lang = if req.lang.trim().is_empty() {
            "en-US"
        } else {
            req.lang.trim()
        };
        let escaped_text = xml_escape(&req.text);
        let escaped_voice = xml_escape(req.voice.trim());
        let escaped_lang = xml_escape(lang);
        let inner = if req.rate.is_some() || req.pitch.is_some() {
            let mut attrs = String::new();
            if let Some(rate) = &req.rate {
                attrs.push_str(&format!(" rate='{}'", xml_escape(rate.trim())));
            }
            if let Some(pitch) = &req.pitch {
                attrs.push_str(&format!(" pitch='{}'", xml_escape(pitch.trim())));
            }
            format!("<prosody{attrs}>{escaped_text}</prosody>")
        } else {
            escaped_text
        };
        format!(
            "<speak version='1.0' xml:lang='{escaped_lang}'><voice xml:lang='{escaped_lang}' name='{escaped_voice}'>{inner}</voice></speak>"
        )
    };

    let url = build_azure_tts_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let (mut headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    let user_agent = req
        .user_agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("coda-cli/0.2.0")
        .to_owned();
    headers.push((
        "X-Microsoft-OutputFormat".to_owned(),
        req.output_format.trim().to_owned(),
    ));
    headers.push(("User-Agent".to_owned(), user_agent));

    let http_req = Engine2Request::post_raw(url, "application/ssml+xml", ssml.into_bytes())
        .with_headers(headers, sensitive)
        .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
    let resp = http.execute_bytes(&http_req, &keys, cancel).await?;
    let (mime, ext) = azure_mime_ext(&req.output_format, &resp.bytes);
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &resp.bytes)
        .await?;

    Ok(TtsSynthesisResponse {
        vendor: "azure".into(),
        model: if endpoint.model.trim().is_empty() {
            req.voice.clone()
        } else {
            endpoint.model.clone()
        },
        audio: artifact,
        sample_rate: None,
        duration_ms: None,
        request_id: header_request_id(&resp.headers),
        task_id: None,
    })
}

// ============================================================================
// 5. MiniMax TTS (Engine 2 sync/SSE, Engine 4 WS, Engine 3 Async)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct MiniMaxTtsRequest {
    pub text: String,
    pub voice_id: String,
    pub speed: Option<f64>,
    pub vol: Option<f64>,
    pub pitch: Option<i32>,
    pub emotion: Option<String>,
    pub sample_rate: Option<u32>,
    pub bitrate: Option<u32>,
    pub format: Option<TtsAudioFormat>,
    pub stream: bool,
    pub output_url: bool,
    pub language_boost: Option<String>,
    pub allow_http_loopback_download: bool,
}

pub fn minimax_regional_base(china_mainland: bool) -> &'static str {
    if china_mainland {
        "https://api.minimaxi.com"
    } else {
        "https://api.minimax.io"
    }
}

pub(crate) fn validate_minimax_host(
    url: &url::Url,
    embedded_proxy: bool,
) -> Result<(), MediaError> {
    if embedded_proxy || is_loopback_host(url) {
        return Ok(());
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return Err(MediaError::invalid_request("MiniMax URL missing host"));
    };
    if host.contains("minimax") && host != "api.minimax.io" && host != "api.minimaxi.com" {
        return Err(MediaError::invalid_request(format!(
            "MiniMax regional host lock: expected api.minimax.io or api.minimaxi.com, got {host}"
        )));
    }
    Ok(())
}

pub(crate) fn build_minimax_url(
    base_url: &str,
    path_suffix: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    validate_minimax_host(&url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy
        || url
            .path()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/v1")
    {
        Ok(format!("{trimmed}{path_suffix}"))
    } else {
        Ok(format!("{trimmed}/v1{path_suffix}"))
    }
}

pub fn build_minimax_payload(model: &str, req: &MiniMaxTtsRequest, force_hex: bool) -> Value {
    let mut voice_setting = json!({
        "voice_id": req.voice_id,
    });
    if let Some(speed) = req.speed {
        voice_setting["speed"] = json!(speed);
    }
    if let Some(vol) = req.vol {
        voice_setting["vol"] = json!(vol);
    }
    if let Some(pitch) = req.pitch {
        voice_setting["pitch"] = json!(pitch);
    }
    if let Some(emotion) = &req.emotion {
        voice_setting["emotion"] = json!(emotion);
    }

    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let mut audio_setting = json!({
        "format": fmt.as_minimax_str(),
        "channel": 1,
    });
    if let Some(sr) = req.sample_rate {
        audio_setting["sample_rate"] = json!(sr);
    }
    if let Some(br) = req.bitrate {
        audio_setting["bitrate"] = json!(br);
    }

    let output_format = if force_hex || req.stream || !req.output_url {
        "hex"
    } else {
        "url"
    };

    let mut body = json!({
        "model": model,
        "text": req.text,
        "stream": req.stream,
        "voice_setting": voice_setting,
        "audio_setting": audio_setting,
        "output_format": output_format,
    });
    if let Some(lb) = &req.language_boost {
        body["language_boost"] = json!(lb);
    }
    body
}

pub async fn minimax_tts<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &MiniMaxTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "MiniMax TTS",
    )?;
    if endpoint.model.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "MiniMax TTS model must not be empty",
        ));
    }
    if req.text.trim().is_empty() || req.voice_id.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "MiniMax TTS requires non-empty text and voice_id",
        ));
    }

    let url = build_minimax_url(&endpoint.base_url, "/t2a_v2", endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    let format = req.format.unwrap_or(TtsAudioFormat::Mp3);

    if !req.stream {
        let body = build_minimax_payload(&endpoint.model, req, false);
        let http_req = Engine2Request::post_json(url.clone(), body)
            .with_headers(headers, sensitive)
            .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
        let resp = http
            .execute_json(&http_req, &keys, check_standard_business_status, cancel)
            .await?;

        let audio_val = resp
            .body
            .pointer("/data/audio")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing data.audio in MiniMax TTS response",
                )
            })?;

        let raw_bytes = if audio_val.starts_with("http://") || audio_val.starts_with("https://") {
            let allow_loopback = req.allow_http_loopback_download || is_loopback_url(&url);
            let dl_url = validate_media_download_url(audio_val, allow_loopback)?;
            let dl_req = Engine2Request::get(dl_url.as_str())
                .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
            http.execute_bytes(&dl_req, &keys, cancel)
                .await?
                .bytes
                .to_vec()
        } else {
            decode_hex(audio_val)?
        };

        let (mime, ext) = detect_audio_mime_and_ext(&raw_bytes, Some(format));
        let artifact = artifacts
            .write_artifact(owner_id, ext, mime, &raw_bytes)
            .await?;
        let duration_ms = resp
            .body
            .pointer("/extra_info/audio_length")
            .and_then(Value::as_u64);
        let sample_rate = resp
            .body
            .pointer("/extra_info/audio_sample_rate")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .or(req.sample_rate);
        return Ok(TtsSynthesisResponse {
            vendor: "minimax".into(),
            model: endpoint.model.clone(),
            audio: artifact,
            sample_rate,
            duration_ms,
            request_id: resp
                .body
                .get("trace_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| header_request_id(&resp.headers)),
            task_id: None,
        });
    }

    // SSE streaming (`stream: true`, `output_format: "hex"`).
    // Track `status == 1` chunks separately from `status == 2` terminal frame to avoid duplicating audio.
    let body = build_minimax_payload(&endpoint.model, req, true);
    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let mut chunks_status1: Vec<u8> = Vec::new();
    let mut final_status2: Option<Vec<u8>> = None;
    let mut sample_rate = req.sample_rate;
    let mut duration_ms = None;

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
                    format!("invalid MiniMax SSE JSON frame: {e}"),
                )
            })?;
            check_standard_business_status(&HeaderMap::new(), &val)?;
            if let Some(sr) = val
                .pointer("/extra_info/audio_sample_rate")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
            {
                sample_rate = Some(sr);
            }
            if let Some(dur) = val
                .pointer("/extra_info/audio_length")
                .and_then(Value::as_u64)
            {
                duration_ms = Some(dur);
            }

            let status = val.pointer("/data/status").and_then(Value::as_i64);
            let hex_audio = val
                .pointer("/data/audio")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());

            match status {
                Some(2) => {
                    if let Some(h) = hex_audio {
                        final_status2 = Some(decode_hex(h)?);
                    }
                    Ok(StreamControl::Done)
                }
                _ => {
                    if let Some(h) = hex_audio {
                        chunks_status1.extend_from_slice(&decode_hex(h)?);
                    }
                    Ok(StreamControl::Continue)
                }
            }
        },
        cancel,
    )
    .await?;

    let audio_bytes = if !chunks_status1.is_empty() {
        chunks_status1
    } else if let Some(b) = final_status2.filter(|v| !v.is_empty()) {
        b
    } else {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "MiniMax SSE stream completed without audio data",
        ));
    };

    let (mime, ext) = detect_audio_mime_and_ext(&audio_bytes, Some(format));
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &audio_bytes)
        .await?;
    Ok(TtsSynthesisResponse {
        vendor: "minimax".into(),
        model: endpoint.model.clone(),
        audio: artifact,
        sample_rate,
        duration_ms,
        request_id: None,
        task_id: None,
    })
}

pub async fn minimax_tts_ws<A: MediaArtifactStore>(
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &MiniMaxTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "MiniMax WS TTS",
    )?;
    if endpoint.model.trim().is_empty()
        || req.text.trim().is_empty()
        || req.voice_id.trim().is_empty()
    {
        return Err(MediaError::invalid_request(
            "MiniMax WS TTS requires non-empty model, text, and voice_id",
        ));
    }

    let ws_base = http_url_to_ws_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    validate_minimax_host(&ws_base, endpoint.embedded_proxy)?;
    let trimmed = ws_base.as_str().trim_end_matches('/');
    let ws_url = if ws_base
        .path()
        .trim_end_matches('/')
        .to_ascii_lowercase()
        .ends_with("/ws/v1/t2a_v2")
    {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/ws/v1/t2a_v2")
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

    // Wait for `connected_success`
    let connected = recv_ws_json(&mut reader, &keys, cancel).await?;
    check_standard_business_status(&HeaderMap::new(), &connected).map_err(|e| e.scrubbed(&keys))?;
    if connected.get("event").and_then(Value::as_str) != Some("connected_success") {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "MiniMax WS expected connected_success handshake event",
        ));
    }

    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let mut voice_setting = json!({ "voice_id": req.voice_id });
    if let Some(speed) = req.speed {
        voice_setting["speed"] = json!(speed);
    }
    if let Some(vol) = req.vol {
        voice_setting["vol"] = json!(vol);
    }
    if let Some(pitch) = req.pitch {
        voice_setting["pitch"] = json!(pitch);
    }
    if let Some(emotion) = &req.emotion {
        voice_setting["emotion"] = json!(emotion);
    }
    let mut audio_setting = json!({
        "format": fmt.as_minimax_str(),
        "channel": 1,
    });
    if let Some(sr) = req.sample_rate {
        audio_setting["sample_rate"] = json!(sr);
    }
    if let Some(br) = req.bitrate {
        audio_setting["bitrate"] = json!(br);
    }

    let mut start_msg = json!({
        "event": "task_start",
        "model": endpoint.model,
        "voice_setting": voice_setting,
        "audio_setting": audio_setting,
    });
    if let Some(lb) = &req.language_boost {
        start_msg["language_boost"] = json!(lb);
    }
    writer.send_json(&start_msg).await?;

    let started = recv_ws_json(&mut reader, &keys, cancel).await?;
    check_standard_business_status(&HeaderMap::new(), &started).map_err(|e| e.scrubbed(&keys))?;
    if started.get("event").and_then(Value::as_str) != Some("task_started") {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "MiniMax WS expected task_started event",
        ));
    }

    writer
        .send_json(&json!({
            "event": "task_continue",
            "text": req.text,
        }))
        .await?;

    let mut audio_bytes = Vec::new();
    loop {
        let msg = recv_ws_json(&mut reader, &keys, cancel).await?;
        check_standard_business_status(&HeaderMap::new(), &msg).map_err(|e| e.scrubbed(&keys))?;
        if let Some(hex_chunk) = msg
            .pointer("/data/audio")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            audio_bytes.extend_from_slice(&decode_hex(hex_chunk)?);
        }
        let is_final = msg.get("is_final").and_then(Value::as_bool) == Some(true)
            || msg.get("event").and_then(Value::as_str) == Some("task_finished");
        if is_final {
            break;
        }
    }

    let _ = writer.send_json(&json!({ "event": "task_finish" })).await;
    let _ = writer.close(1000, "done").await;

    if audio_bytes.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "MiniMax WS TTS completed without audio frames",
        ));
    }
    let (mime, ext) = detect_audio_mime_and_ext(&audio_bytes, Some(fmt));
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &audio_bytes)
        .await?;
    Ok(TtsSynthesisResponse {
        vendor: "minimax".into(),
        model: endpoint.model.clone(),
        audio: artifact,
        sample_rate: req.sample_rate,
        duration_ms: None,
        request_id: None,
        task_id: None,
    })
}

async fn recv_ws_json(
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
                return serde_json::from_str::<Value>(&txt).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("invalid WS JSON frame: {e}"),
                    )
                    .scrubbed(keys)
                });
            }
            Some(Ok(WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Binary(_))) => {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    "unexpected binary WS frame while waiting for JSON",
                ));
            }
            Some(Ok(WsMessage::Close(frame))) => {
                let reason = frame.map(|f| f.reason).unwrap_or_default();
                return Err(MediaError::new(
                    MediaErrorCode::Transport,
                    format!("websocket closed unexpectedly: {reason}"),
                )
                .scrubbed(keys));
            }
            Some(Err(e)) => return Err(e.scrubbed(keys)),
            None => {
                return Err(MediaError::new(
                    MediaErrorCode::Transport,
                    "websocket closed before response",
                ));
            }
        }
    }
}

pub struct MiniMaxAsyncTtsAdapter {
    pub vendor_tag: String,
    pub base_url: String,
    pub embedded_proxy: bool,
    pub headers: Vec<(String, String)>,
    pub sensitive_headers: Vec<String>,
    pub format: TtsAudioFormat,
}

impl MiniMaxAsyncTtsAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        format: TtsAudioFormat,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "MiniMax Async TTS",
        )?;
        let url = validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        validate_minimax_host(&url, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        let vendor_tag = if endpoint.vendor.trim().eq_ignore_ascii_case("minimax-cn") {
            "minimax-cn".to_owned()
        } else {
            "minimax".to_owned()
        };
        Ok((
            Self {
                vendor_tag,
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                headers,
                sensitive_headers: sensitive,
                format,
            },
            keys,
        ))
    }
}

impl Engine3Adapter for MiniMaxAsyncTtsAdapter {
    fn vendor(&self) -> &str {
        &self.vendor_tag
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Tts
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let url = build_minimax_url(&self.base_url, "/t2a_async_v2", self.embedded_proxy)?;
        let mut body = input.payload.clone();
        if body.get("model").is_none() {
            body["model"] = json!(input.model);
        }
        Ok(Engine2Request::post_json(url, body)
            .with_headers(self.headers.clone(), self.sensitive_headers.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        check_standard_business_status(headers, body)?;
        let remote_task_id = match body.get("task_id") {
            Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
            Some(Value::Number(n)) => n.to_string(),
            _ => {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing task_id in MiniMax async TTS submit response",
                ));
            }
        };
        Ok(SubmitOutcome::Accepted {
            remote_task_id,
            poll_cursor: None,
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("MiniMax async TTS task missing remote_task_id")
        })?;
        let base = build_minimax_url(
            &self.base_url,
            "/query/t2a_async_query_v2",
            self.embedded_proxy,
        )?;
        let mut parsed = url::Url::parse(&base)
            .map_err(|_| MediaError::invalid_request("invalid MiniMax poll URL"))?;
        parsed.query_pairs_mut().append_pair("task_id", remote_id);
        Ok(Engine2Request::get(parsed.to_string())
            .with_headers(self.headers.clone(), self.sensitive_headers.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        check_standard_business_status(headers, body)?;
        let status_str = body
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match status_str.as_str() {
            "success" | "succeeded" | "completed" => {
                let file_id = match body.get("file_id") {
                    Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => {
                        return Err(MediaError::new(
                            MediaErrorCode::DecodeError,
                            "missing file_id in MiniMax async TTS success response",
                        ));
                    }
                };
                let retrieve_base =
                    build_minimax_url(&self.base_url, "/files/retrieve", self.embedded_proxy)?;
                let mut retrieve_url = url::Url::parse(&retrieve_base)
                    .map_err(|_| MediaError::invalid_request("invalid MiniMax retrieve URL"))?;
                retrieve_url
                    .query_pairs_mut()
                    .append_pair("file_id", &file_id);
                let (mime, ext) = self.format.mime_and_ext();
                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::TwoStepDownload {
                        resolve_request: Engine2Request::get(retrieve_url.to_string())
                            .with_headers(self.headers.clone(), self.sensitive_headers.clone()),
                        url_pointer: "/file/download_url".into(),
                        download_headers: Vec::new(),
                        download_sensitive_headers: Vec::new(),
                        suggested_ext: ext.into(),
                        mime_type: mime.into(),
                    }],
                    poll_cursor: Some(file_id),
                })
            }
            "failed" | "failure" | "expired" | "error" => Ok(PollOutcome::Failed {
                upstream_code: body
                    .pointer("/base_resp/status_code")
                    .and_then(Value::as_i64)
                    .map(|c| c.to_string()),
                message: body
                    .pointer("/base_resp/status_msg")
                    .and_then(Value::as_str)
                    .unwrap_or("MiniMax async TTS failed")
                    .to_owned(),
                retryable: false,
            }),
            "processing" | "queued" | "pending" | "running" | "in_progress" => {
                Ok(PollOutcome::Pending {
                    status: MediaTaskStatus::Running,
                    poll_cursor: None,
                    retry_after_hint: None,
                })
            }
            other => Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!("unexpected MiniMax async TTS status '{other}'"),
            )),
        }
    }
}

pub async fn minimax_tts_async<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &MiniMaxTtsRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<MediaTaskRecord, MediaError> {
    let format = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let (adapter, keys) = MiniMaxAsyncTtsAdapter::from_endpoint(endpoint, cred, format)?;
    let payload = build_minimax_payload(&endpoint.model, req, false);
    runner
        .run_to_completion(
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

// ============================================================================
// 6. Volcengine / Doubao TTS & Official Seed-Audio (5 sub-protocols)
// ============================================================================

/// Shared pure-Rust Volcengine binary WebSocket frame codec (`compression = 0`).
pub(crate) mod volcengine_frame {
    use super::{MediaError, MediaErrorCode};

    pub const MSG_FULL_CLIENT_REQUEST: u8 = 0x1;
    pub const MSG_AUDIO_ONLY_CLIENT: u8 = 0x2;
    pub const MSG_FULL_SERVER_RESPONSE: u8 = 0x9;
    pub const MSG_AUDIO_ONLY_SERVER: u8 = 0xB;
    pub const MSG_ERROR: u8 = 0xF;

    pub const FLAG_NONE: u8 = 0x0;
    pub const FLAG_POS_SEQ: u8 = 0x1;
    pub const FLAG_LAST_AUDIO: u8 = 0x2;
    pub const FLAG_NEG_SEQ: u8 = 0x3;
    pub const FLAG_WITH_EVENT: u8 = 0x4;

    pub const SERIAL_NONE: u8 = 0x0;
    pub const SERIAL_JSON: u8 = 0x1;

    pub const EVENT_START_CONNECTION: u32 = 1;
    pub const EVENT_FINISH_CONNECTION: u32 = 2;
    pub const EVENT_CONNECTION_STARTED: u32 = 50;
    pub const EVENT_CONNECTION_FAILED: u32 = 51;
    pub const EVENT_CONNECTION_FINISHED: u32 = 52;
    pub const EVENT_START_SESSION: u32 = 100;
    pub const EVENT_FINISH_SESSION: u32 = 102;
    pub const EVENT_SESSION_STARTED: u32 = 150;
    pub const EVENT_SESSION_FINISHED: u32 = 152;
    pub const EVENT_SESSION_FAILED: u32 = 153;
    pub const EVENT_TASK_REQUEST: u32 = 200;
    pub const EVENT_TTS_RESPONSE: u32 = 352;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct VolcFrame {
        pub msg_type: u8,
        pub flags: u8,
        pub serialization: u8,
        pub event_id: Option<u32>,
        pub session_id: Option<String>,
        pub sequence: Option<i32>,
        pub error_code: Option<u32>,
        pub payload: Vec<u8>,
    }

    pub fn encode_v3_event_frame(
        event_id: u32,
        session_id: Option<&str>,
        json_payload: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(24 + json_payload.len());
        out.push(0x11); // version 1, header size 1 (4 bytes)
        out.push((MSG_FULL_CLIENT_REQUEST << 4) | FLAG_WITH_EVENT);
        out.push(SERIAL_JSON << 4); // compression = 0 (none)
        out.push(0x00);
        out.extend_from_slice(&event_id.to_be_bytes());
        if let Some(sid) = session_id {
            let sid_bytes = sid.as_bytes();
            out.extend_from_slice(&(sid_bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(sid_bytes);
        }
        out.extend_from_slice(&(json_payload.len() as u32).to_be_bytes());
        out.extend_from_slice(json_payload);
        out
    }

    pub fn encode_simple_frame(
        msg_type: u8,
        flags: u8,
        serialization: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.push(0x11);
        out.push(((msg_type & 0x0F) << 4) | (flags & 0x0F));
        out.push((serialization & 0x0F) << 4); // compression = 0
        out.push(0x00);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    pub fn decode_frame(raw: &[u8]) -> Result<VolcFrame, MediaError> {
        if raw.len() < 4 {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "Volcengine binary WS frame shorter than 4-byte header",
            ));
        }
        let header_bytes = usize::from(raw[0] & 0x0F) * 4;
        if header_bytes < 4 || raw.len() < header_bytes {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "Volcengine binary WS frame truncated header",
            ));
        }
        let msg_type = (raw[1] >> 4) & 0x0F;
        let flags = raw[1] & 0x0F;
        let serialization = (raw[2] >> 4) & 0x0F;
        let compression = raw[2] & 0x0F;
        if compression != 0 {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!("unsupported Volcengine WS frame compression {compression} (expected 0)"),
            ));
        }

        let mut cursor = header_bytes;
        if msg_type == MSG_ERROR {
            let error_code = if raw.len() >= cursor + 4 {
                let code = u32::from_be_bytes(raw[cursor..cursor + 4].try_into().unwrap());
                cursor += 4;
                Some(code)
            } else {
                None
            };
            let payload = read_len_prefixed_tail(raw, &mut cursor)?;
            return Ok(VolcFrame {
                msg_type,
                flags,
                serialization,
                event_id: None,
                session_id: None,
                sequence: None,
                error_code,
                payload,
            });
        }

        let mut event_id = None;
        let mut session_id = None;
        let mut sequence = None;

        if (flags & FLAG_WITH_EVENT) != 0 {
            if raw.len() < cursor + 4 {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Volcengine WS frame missing event_id",
                ));
            }
            let eid = u32::from_be_bytes(raw[cursor..cursor + 4].try_into().unwrap());
            cursor += 4;
            event_id = Some(eid);

            // Connection-level events (1, 2, 50, 51, 52) do not carry a session_id.
            let is_conn_event = matches!(
                eid,
                EVENT_START_CONNECTION
                    | EVENT_FINISH_CONNECTION
                    | EVENT_CONNECTION_STARTED
                    | EVENT_CONNECTION_FAILED
                    | EVENT_CONNECTION_FINISHED
            );
            if !is_conn_event && raw.len() >= cursor + 4 {
                let sid_len =
                    u32::from_be_bytes(raw[cursor..cursor + 4].try_into().unwrap()) as usize;
                cursor += 4;
                if raw.len() < cursor + sid_len {
                    return Err(MediaError::new(
                        MediaErrorCode::DecodeError,
                        "Volcengine WS frame truncated session_id",
                    ));
                }
                session_id =
                    Some(String::from_utf8_lossy(&raw[cursor..cursor + sid_len]).into_owned());
                cursor += sid_len;
            }
        } else if (flags == FLAG_POS_SEQ || flags == FLAG_LAST_AUDIO || flags == FLAG_NEG_SEQ)
            && raw.len() >= cursor + 4
        {
            // Optional 4-byte sequence number when followed by length-prefixed payload.
            if raw.len() >= cursor + 8 {
                let candidate_seq = i32::from_be_bytes(raw[cursor..cursor + 4].try_into().unwrap());
                let candidate_len =
                    u32::from_be_bytes(raw[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
                if cursor + 8 + candidate_len == raw.len() {
                    sequence = Some(candidate_seq);
                    cursor += 4;
                }
            }
        }

        let payload = read_len_prefixed_tail(raw, &mut cursor)?;
        Ok(VolcFrame {
            msg_type,
            flags,
            serialization,
            event_id,
            session_id,
            sequence,
            error_code: None,
            payload,
        })
    }

    fn read_len_prefixed_tail(raw: &[u8], cursor: &mut usize) -> Result<Vec<u8>, MediaError> {
        if raw.len() == *cursor {
            return Ok(Vec::new());
        }
        if raw.len() < *cursor + 4 {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "Volcengine WS frame truncated payload length",
            ));
        }
        let len = u32::from_be_bytes(raw[*cursor..*cursor + 4].try_into().unwrap()) as usize;
        *cursor += 4;
        if raw.len() < *cursor + len {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "Volcengine WS frame payload truncated",
            ));
        }
        let payload = raw[*cursor..*cursor + len].to_vec();
        *cursor += len;
        Ok(payload)
    }
}

#[derive(Clone, Debug, Default)]
pub struct VolcengineTtsRequest {
    pub text: String,
    pub speaker: String,
    pub resource_id: Option<String>,
    pub uid: Option<String>,
    pub format: Option<TtsAudioFormat>,
    pub sample_rate: Option<u32>,
    pub speech_rate: Option<i32>,
    pub loudness_rate: Option<i32>,
    /// Legacy V1 `app.appid` + `app.cluster`.
    pub v1_appid: Option<String>,
    pub v1_cluster: Option<String>,
    pub allow_http_loopback_download: bool,
}

fn ensure_volcengine_v3_auth(
    cred: &MediaCredential,
    endpoint: &MediaEndpointConfig,
) -> Result<(), MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[
            MediaAuthKind::VolcengineV3ApiKey,
            MediaAuthKind::VolcengineV3AppAccess,
            MediaAuthKind::ProxyToken,
        ],
        "Volcengine V3 TTS",
    )
}

fn volcengine_v3_protocol_headers(
    resource_id: Option<&str>,
    default_resource: &str,
    include_connect_id: bool,
) -> Vec<(String, String)> {
    let rid = resource_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_resource)
        .to_owned();
    let req_id = uuid::Uuid::new_v4().to_string();
    let mut out = vec![
        ("X-Api-Resource-Id".to_owned(), rid),
        ("X-Api-Request-Id".to_owned(), req_id),
    ];
    if include_connect_id {
        out.push((
            "X-Api-Connect-Id".to_owned(),
            uuid::Uuid::new_v4().to_string(),
        ));
    }
    out
}

fn build_volcengine_url(
    base_url: &str,
    api_path: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy {
        Ok(format!("{trimmed}{api_path}"))
    } else if url.path().trim_end_matches('/').ends_with(api_path) {
        Ok(trimmed.to_owned())
    } else {
        Ok(format!("{trimmed}{api_path}"))
    }
}

pub fn build_volcengine_v3_tts_body(model: &str, req: &VolcengineTtsRequest) -> Value {
    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let mut audio_params = json!({
        "format": fmt.as_volcengine_str(),
        "sample_rate": req.sample_rate.unwrap_or(24_000),
    });
    if let Some(sr) = req.speech_rate {
        audio_params["speech_rate"] = json!(sr);
    }
    if let Some(lr) = req.loudness_rate {
        audio_params["loudness_rate"] = json!(lr);
    }
    let mut req_params = json!({
        "text": req.text,
        "speaker": req.speaker,
        "audio_params": audio_params,
    });
    if !model.trim().is_empty() {
        req_params["model"] = json!(model);
    }
    json!({
        "user": {
            "uid": req.uid.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or("coda-cli"),
        },
        "req_params": req_params,
    })
}

/// 6a. Volcengine V3 HTTP unidirectional stream (`POST /api/v3/tts/unidirectional`, NDJSON `code: 20000000` terminal).
pub async fn volcengine_v3_tts_http<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &VolcengineTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_volcengine_v3_auth(cred, endpoint)?;
    if req.text.trim().is_empty() || req.speaker.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Volcengine V3 TTS requires non-empty text and speaker",
        ));
    }

    let url = build_volcengine_url(
        &endpoint.base_url,
        "/api/v3/tts/unidirectional",
        endpoint.embedded_proxy,
    )?;
    let (mut headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    headers.extend(volcengine_v3_protocol_headers(
        req.resource_id.as_deref(),
        "volc.service_type.10029",
        false,
    ));

    let body = build_volcengine_v3_tts_body(&endpoint.model, req);
    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let mut audio_buf: Vec<u8> = Vec::new();
    let mut saw_terminal = false;

    http.execute_ndjson_stream(
        &http_req,
        &keys,
        |val| {
            let code = val.get("code").and_then(Value::as_i64).unwrap_or(0);
            if code == 20_000_000 {
                if let Some(b64) = val.get("data").and_then(Value::as_str)
                    && !b64.trim().is_empty()
                {
                    audio_buf.extend_from_slice(&decode_base64_bytes(b64)?);
                }
                saw_terminal = true;
                return Ok(StreamControl::Done);
            }
            if code != 0 {
                let msg = val
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Volcengine V3 TTS stream error");
                return Err(MediaError::business(Some(code.to_string()), msg, false));
            }
            if let Some(b64) = val.get("data").and_then(Value::as_str)
                && !b64.trim().is_empty()
            {
                audio_buf.extend_from_slice(&decode_base64_bytes(b64)?);
            }
            Ok(StreamControl::Continue)
        },
        cancel,
    )
    .await?;

    if !saw_terminal || audio_buf.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            if !saw_terminal {
                "Volcengine V3 TTS NDJSON stream ended without terminal code 20000000"
            } else {
                "Volcengine V3 TTS NDJSON stream returned empty audio"
            },
        ));
    }

    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let (mime, ext) = detect_audio_mime_and_ext(&audio_buf, Some(fmt));
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &audio_buf)
        .await?;
    Ok(TtsSynthesisResponse {
        vendor: "volcengine".into(),
        model: endpoint.model.clone(),
        audio: artifact,
        sample_rate: req.sample_rate,
        duration_ms: None,
        request_id: None,
        task_id: None,
    })
}

/// 6b. Official Bytedance Seed-Audio 1.0 (`POST /api/v3/tts/create`, isolated from partner `/v1/tts/create`).
pub async fn volcengine_seed_audio_create<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &VolcengineTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_volcengine_v3_auth(cred, endpoint)?;
    if req.text.trim().is_empty() || req.speaker.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Official Seed-Audio TTS requires non-empty text and speaker",
        ));
    }

    let url = build_volcengine_url(
        &endpoint.base_url,
        "/api/v3/tts/create",
        endpoint.embedded_proxy,
    )?;
    let (mut headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    headers.extend(volcengine_v3_protocol_headers(
        req.resource_id.as_deref(),
        "seed-tts-1.0",
        false,
    ));

    let body = build_volcengine_v3_tts_body(&endpoint.model, req);
    let http_req = Engine2Request::post_json(url.clone(), body)
        .with_headers(headers, sensitive)
        .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);

    match http
        .execute_hybrid(&http_req, &keys, check_standard_business_status, cancel)
        .await?
    {
        Engine2HybridResponse::Bytes(resp) => {
            let (mime, ext) = detect_audio_mime_and_ext(&resp.bytes, Some(fmt));
            let artifact = artifacts
                .write_artifact(owner_id, ext, mime, &resp.bytes)
                .await?;
            Ok(TtsSynthesisResponse {
                vendor: "volcengine".into(),
                model: endpoint.model.clone(),
                audio: artifact,
                sample_rate: req.sample_rate,
                duration_ms: None,
                request_id: header_request_id(&resp.headers),
                task_id: None,
            })
        }
        Engine2HybridResponse::Json(resp) => {
            if let Some(code) = resp.body.get("code").and_then(Value::as_i64)
                && code != 0
                && code != 20_000_000
            {
                let msg = resp
                    .body
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Official Seed-Audio business error");
                return Err(MediaError::business(Some(code.to_string()), msg, false));
            }
            let audio_bytes = if let Some(b64) = resp
                .body
                .get("data")
                .and_then(Value::as_str)
                .or_else(|| resp.body.pointer("/data/audio").and_then(Value::as_str))
            {
                decode_base64_bytes(b64)?
            } else if let Some(audio_url) = resp
                .body
                .get("audio_url")
                .and_then(Value::as_str)
                .or_else(|| resp.body.pointer("/data/audio_url").and_then(Value::as_str))
            {
                let allow_loopback = req.allow_http_loopback_download || is_loopback_url(&url);
                let dl = validate_media_download_url(audio_url, allow_loopback)?;
                let dl_req = Engine2Request::get(dl.as_str())
                    .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
                http.execute_bytes(&dl_req, &keys, cancel)
                    .await?
                    .bytes
                    .to_vec()
            } else {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing audio data or audio_url in Official Seed-Audio response",
                ));
            };

            let (mime, ext) = detect_audio_mime_and_ext(&audio_bytes, Some(fmt));
            let artifact = artifacts
                .write_artifact(owner_id, ext, mime, &audio_bytes)
                .await?;
            Ok(TtsSynthesisResponse {
                vendor: "volcengine".into(),
                model: endpoint.model.clone(),
                audio: artifact,
                sample_rate: req.sample_rate,
                duration_ms: None,
                request_id: header_request_id(&resp.headers),
                task_id: None,
            })
        }
    }
}

/// 6c. Volcengine V3 Async TTS (`POST /api/v3/tts/submit` -> `POST /api/v3/tts/query`).
pub struct VolcengineV3AsyncTtsAdapter {
    pub base_url: String,
    pub embedded_proxy: bool,
    pub headers: Vec<(String, String)>,
    pub sensitive_headers: Vec<String>,
    pub resource_id: String,
    pub format: TtsAudioFormat,
}

impl VolcengineV3AsyncTtsAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        resource_id: Option<&str>,
        format: TtsAudioFormat,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_volcengine_v3_auth(cred, endpoint)?;
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
                    .unwrap_or("volc.service_type.10029")
                    .to_owned(),
                format,
            },
            keys,
        ))
    }
}

impl Engine3Adapter for VolcengineV3AsyncTtsAdapter {
    fn vendor(&self) -> &str {
        "volcengine"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Tts
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let url = build_volcengine_url(&self.base_url, "/api/v3/tts/submit", self.embedded_proxy)?;
        let mut headers = self.headers.clone();
        headers.extend(volcengine_v3_protocol_headers(
            Some(&self.resource_id),
            "volc.service_type.10029",
            false,
        ));
        Ok(Engine2Request::post_json(url, input.payload.clone())
            .with_headers(headers, self.sensitive_headers.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        if let Some(sc) = headers
            .get("x-api-status-code")
            .and_then(|v| v.to_str().ok())
            && sc != "20000000"
            && sc != "0"
        {
            let msg = headers
                .get("x-api-message")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("Volcengine V3 async TTS submit rejected");
            return Err(MediaError::business(Some(sc.to_owned()), msg, false));
        }
        if let Some(code) = body.get("code").and_then(Value::as_i64)
            && code != 0
            && code != 20_000_000
        {
            let msg = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Volcengine V3 async TTS submit rejected");
            return Err(MediaError::business(Some(code.to_string()), msg, false));
        }
        let remote_task_id = body
            .get("task_id")
            .or_else(|| body.pointer("/data/task_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing task_id in Volcengine V3 async TTS submit response",
                )
            })?
            .to_owned();
        Ok(SubmitOutcome::Accepted {
            remote_task_id,
            poll_cursor: None,
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("Volcengine V3 async TTS task missing remote_task_id")
        })?;
        let url = build_volcengine_url(&self.base_url, "/api/v3/tts/query", self.embedded_proxy)?;
        let mut headers = self.headers.clone();
        headers.extend(volcengine_v3_protocol_headers(
            Some(&self.resource_id),
            "volc.service_type.10029",
            false,
        ));
        Ok(
            Engine2Request::post_json(url, json!({ "task_id": remote_id }))
                .with_headers(headers, self.sensitive_headers.clone()),
        )
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        if let Some(sc) = headers
            .get("x-api-status-code")
            .and_then(|v| v.to_str().ok())
        {
            match sc {
                "20000001" | "20000002" => {
                    return Ok(PollOutcome::Pending {
                        status: MediaTaskStatus::Running,
                        poll_cursor: None,
                        retry_after_hint: None,
                    });
                }
                "20000000" | "0" => {}
                other => {
                    let msg = headers
                        .get("x-api-message")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("Volcengine V3 async TTS failed");
                    return Ok(PollOutcome::Failed {
                        upstream_code: Some(other.to_owned()),
                        message: msg.to_owned(),
                        retryable: false,
                    });
                }
            }
        }
        if let Some(code) = body.get("code").and_then(Value::as_i64)
            && code != 0
            && code != 20_000_000
        {
            if code == 20_000_001 || code == 20_000_002 {
                return Ok(PollOutcome::Pending {
                    status: MediaTaskStatus::Running,
                    poll_cursor: None,
                    retry_after_hint: None,
                });
            }
            let msg = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Volcengine V3 async TTS failed")
                .to_owned();
            return Ok(PollOutcome::Failed {
                upstream_code: Some(code.to_string()),
                message: msg,
                retryable: false,
            });
        }

        let (mime, ext) = self.format.mime_and_ext();
        if let Some(audio_url) = body
            .get("audio_url")
            .or_else(|| body.pointer("/data/audio_url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(PollOutcome::Succeeded {
                outputs: vec![MediaOutputSource::DirectDownload {
                    request: Engine2Request::get(audio_url),
                    suggested_ext: ext.into(),
                    mime_type: mime.into(),
                }],
                poll_cursor: None,
            });
        }
        if let Some(b64) = body
            .get("data")
            .and_then(Value::as_str)
            .or_else(|| body.pointer("/data/audio").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(PollOutcome::Succeeded {
                outputs: vec![MediaOutputSource::InlineBase64 {
                    b64: b64.to_owned(),
                    suggested_ext: ext.into(),
                    mime_type: mime.into(),
                }],
                poll_cursor: None,
            });
        }

        let status_num = body.get("task_status").and_then(Value::as_i64);
        let status_str = body
            .get("task_status")
            .or_else(|| body.get("status"))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_ascii_lowercase());
        if status_num == Some(2) || matches!(status_str.as_deref(), Some("failed" | "error")) {
            return Ok(PollOutcome::Failed {
                upstream_code: None,
                message: body
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Volcengine V3 async TTS failed")
                    .to_owned(),
                retryable: false,
            });
        }
        Ok(PollOutcome::Pending {
            status: MediaTaskStatus::Running,
            poll_cursor: None,
            retry_after_hint: None,
        })
    }
}

pub async fn volcengine_v3_tts_async<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &VolcengineTtsRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<MediaTaskRecord, MediaError> {
    let format = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let (adapter, keys) = VolcengineV3AsyncTtsAdapter::from_endpoint(
        endpoint,
        cred,
        req.resource_id.as_deref(),
        format,
    )?;
    let payload = build_volcengine_v3_tts_body(&endpoint.model, req);
    runner
        .run_to_completion(
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

/// 6d. Volcengine V3 Bidirectional WS (`wss://.../api/v3/tts/bidirection`, `compression = 0`).
pub async fn volcengine_v3_tts_ws<A: MediaArtifactStore>(
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &VolcengineTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_volcengine_v3_auth(cred, endpoint)?;
    if req.text.trim().is_empty() || req.speaker.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Volcengine V3 WS TTS requires non-empty text and speaker",
        ));
    }

    let ws_base = http_url_to_ws_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let trimmed = ws_base.as_str().trim_end_matches('/');
    let ws_url = if ws_base
        .path()
        .trim_end_matches('/')
        .ends_with("/api/v3/tts/bidirection")
    {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/api/v3/tts/bidirection")
    };

    let (mut headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    headers.extend(volcengine_v3_protocol_headers(
        req.resource_id.as_deref(),
        "volc.service_type.10029",
        true,
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

    // 1. StartConnection(1) -> ConnectionStarted(50)
    writer
        .send_binary(volcengine_frame::encode_v3_event_frame(
            volcengine_frame::EVENT_START_CONNECTION,
            None,
            b"{}",
        ))
        .await?;
    let f_conn = recv_volc_frame(&mut reader, &keys, cancel).await?;
    if f_conn.event_id != Some(volcengine_frame::EVENT_CONNECTION_STARTED) {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            format!(
                "Volcengine V3 WS expected ConnectionStarted(50), got {:?}",
                f_conn.event_id
            ),
        ));
    }

    // 2. StartSession(100) -> SessionStarted(150)
    let session_id = uuid::Uuid::new_v4().to_string();
    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let mut audio_params = json!({
        "format": fmt.as_volcengine_str(),
        "sample_rate": req.sample_rate.unwrap_or(24_000),
    });
    if let Some(sr) = req.speech_rate {
        audio_params["speech_rate"] = json!(sr);
    }
    if let Some(lr) = req.loudness_rate {
        audio_params["loudness_rate"] = json!(lr);
    }
    let start_session_body = serde_json::to_vec(&json!({
        "user": { "uid": req.uid.as_deref().unwrap_or("coda-cli") },
        "event": volcengine_frame::EVENT_START_SESSION,
        "req_params": {
            "speaker": req.speaker,
            "audio_params": audio_params,
        }
    }))
    .map_err(|e| MediaError::invalid_request(e.to_string()))?;

    writer
        .send_binary(volcengine_frame::encode_v3_event_frame(
            volcengine_frame::EVENT_START_SESSION,
            Some(&session_id),
            &start_session_body,
        ))
        .await?;
    let f_sess = recv_volc_frame(&mut reader, &keys, cancel).await?;
    if f_sess.event_id != Some(volcengine_frame::EVENT_SESSION_STARTED) {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            format!(
                "Volcengine V3 WS expected SessionStarted(150), got {:?}",
                f_sess.event_id
            ),
        ));
    }

    // 3. TaskRequest(200) + FinishSession(102)
    let task_req_body = serde_json::to_vec(&json!({
        "event": volcengine_frame::EVENT_TASK_REQUEST,
        "req_params": {
            "text": req.text,
            "speaker": req.speaker,
            "audio_params": audio_params,
        }
    }))
    .map_err(|e| MediaError::invalid_request(e.to_string()))?;
    writer
        .send_binary(volcengine_frame::encode_v3_event_frame(
            volcengine_frame::EVENT_TASK_REQUEST,
            Some(&session_id),
            &task_req_body,
        ))
        .await?;
    writer
        .send_binary(volcengine_frame::encode_v3_event_frame(
            volcengine_frame::EVENT_FINISH_SESSION,
            Some(&session_id),
            b"{}",
        ))
        .await?;

    // 4. Collect TTSResponse(352) until SessionFinished(152)
    let mut audio_bytes = Vec::new();
    loop {
        let frame = recv_volc_frame(&mut reader, &keys, cancel).await?;
        match frame.event_id {
            Some(volcengine_frame::EVENT_TTS_RESPONSE) => {
                if frame.msg_type == volcengine_frame::MSG_AUDIO_ONLY_SERVER {
                    audio_bytes.extend_from_slice(&frame.payload);
                }
            }
            Some(volcengine_frame::EVENT_SESSION_FINISHED) => break,
            Some(volcengine_frame::EVENT_SESSION_FAILED)
            | Some(volcengine_frame::EVENT_CONNECTION_FAILED) => {
                let msg = String::from_utf8_lossy(&frame.payload).into_owned();
                return Err(MediaError::business(
                    frame.event_id.map(|e| e.to_string()),
                    msg,
                    false,
                )
                .scrubbed(&keys));
            }
            _ if frame.msg_type == volcengine_frame::MSG_AUDIO_ONLY_SERVER => {
                audio_bytes.extend_from_slice(&frame.payload);
            }
            _ => {}
        }
    }

    let _ = writer
        .send_binary(volcengine_frame::encode_v3_event_frame(
            volcengine_frame::EVENT_FINISH_CONNECTION,
            None,
            b"{}",
        ))
        .await;
    let _ = writer.close(1000, "done").await;

    if audio_bytes.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "Volcengine V3 WS TTS finished without audio bytes",
        ));
    }
    let (mime, ext) = detect_audio_mime_and_ext(&audio_bytes, Some(fmt));
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &audio_bytes)
        .await?;
    Ok(TtsSynthesisResponse {
        vendor: "volcengine".into(),
        model: endpoint.model.clone(),
        audio: artifact,
        sample_rate: req.sample_rate,
        duration_ms: None,
        request_id: None,
        task_id: None,
    })
}

/// 6e-1. Legacy Volcengine V1 HTTP (`POST /api/v1/tts`, `Authorization: Bearer;<token>`).
pub async fn volcengine_v1_tts_http<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &VolcengineTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[
            MediaAuthKind::VolcengineV1SemicolonBearer,
            MediaAuthKind::ProxyToken,
        ],
        "Volcengine V1 TTS",
    )?;
    if req.text.trim().is_empty() || req.speaker.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Volcengine V1 TTS requires non-empty text and speaker",
        ));
    }

    let url = build_volcengine_url(&endpoint.base_url, "/api/v1/tts", endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let body = build_volcengine_v1_body(req, "query", fmt);

    let http_req = Engine2Request::post_json(url, body)
        .with_headers(headers, sensitive)
        .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
    let resp = http
        .execute_json(&http_req, &keys, |_, v| check_volcengine_v1_code(v), cancel)
        .await?;

    let b64 = resp
        .body
        .get("data")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing data in Volcengine V1 TTS response",
            )
        })?;
    let audio_bytes = decode_base64_bytes(b64)?;
    let (mime, ext) = detect_audio_mime_and_ext(&audio_bytes, Some(fmt));
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &audio_bytes)
        .await?;
    Ok(TtsSynthesisResponse {
        vendor: "volcengine-v1".into(),
        model: endpoint.model.clone(),
        audio: artifact,
        sample_rate: req.sample_rate,
        duration_ms: None,
        request_id: resp
            .body
            .get("reqid")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        task_id: None,
    })
}

/// 6e-2. Legacy Volcengine V1 Binary WS (`wss://.../api/v1/tts/ws_binary`, `Authorization: Bearer;<token>`).
pub async fn volcengine_v1_tts_ws<A: MediaArtifactStore>(
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &VolcengineTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[
            MediaAuthKind::VolcengineV1SemicolonBearer,
            MediaAuthKind::ProxyToken,
        ],
        "Volcengine V1 WS TTS",
    )?;
    if req.text.trim().is_empty() || req.speaker.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Volcengine V1 WS TTS requires non-empty text and speaker",
        ));
    }

    let ws_base = http_url_to_ws_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let trimmed = ws_base.as_str().trim_end_matches('/');
    let ws_url = if ws_base
        .path()
        .trim_end_matches('/')
        .ends_with("/api/v1/tts/ws_binary")
    {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/api/v1/tts/ws_binary")
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

    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let payload_bytes = serde_json::to_vec(&build_volcengine_v1_body(req, "submit", fmt))
        .map_err(|e| MediaError::invalid_request(e.to_string()))?;
    writer
        .send_binary(volcengine_frame::encode_simple_frame(
            volcengine_frame::MSG_FULL_CLIENT_REQUEST,
            volcengine_frame::FLAG_NONE,
            volcengine_frame::SERIAL_JSON,
            &payload_bytes,
        ))
        .await?;

    let mut audio_bytes = Vec::new();
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(MediaError::new(MediaErrorCode::Cancelled, "Volcengine V1 WS cancelled"));
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
                if frame.msg_type == volcengine_frame::MSG_AUDIO_ONLY_SERVER {
                    audio_bytes.extend_from_slice(&frame.payload);
                    if frame.sequence.is_some_and(|s| s < 0)
                        || frame.flags == volcengine_frame::FLAG_LAST_AUDIO
                        || frame.flags == volcengine_frame::FLAG_NEG_SEQ
                    {
                        break;
                    }
                }
            }
            Some(Ok(WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Close(_))) | None => break,
            Some(Ok(WsMessage::Text(txt))) => {
                if let Ok(v) = serde_json::from_str::<Value>(&txt) {
                    check_volcengine_v1_code(&v).map_err(|e| e.scrubbed(&keys))?;
                }
            }
            Some(Err(e)) => return Err(e.scrubbed(&keys)),
        }
    }

    let _ = writer.close(1000, "done").await;
    if audio_bytes.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "Volcengine V1 WS TTS returned no audio bytes",
        ));
    }
    let (mime, ext) = detect_audio_mime_and_ext(&audio_bytes, Some(fmt));
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &audio_bytes)
        .await?;
    Ok(TtsSynthesisResponse {
        vendor: "volcengine-v1".into(),
        model: endpoint.model.clone(),
        audio: artifact,
        sample_rate: req.sample_rate,
        duration_ms: None,
        request_id: None,
        task_id: None,
    })
}

fn build_volcengine_v1_body(
    req: &VolcengineTtsRequest,
    operation: &str,
    fmt: TtsAudioFormat,
) -> Value {
    let mut audio = json!({
        "voice_type": req.speaker,
        "encoding": fmt.as_volcengine_str(),
    });
    if let Some(sr) = req.sample_rate {
        audio["rate"] = json!(sr);
    }
    json!({
        "app": {
            "appid": req.v1_appid.as_deref().unwrap_or("coda-app"),
            "token": "access_token",
            "cluster": req.v1_cluster.as_deref().unwrap_or("volcano_tts"),
        },
        "user": {
            "uid": req.uid.as_deref().unwrap_or("coda-cli"),
        },
        "audio": audio,
        "request": {
            "reqid": uuid::Uuid::new_v4().to_string(),
            "text": req.text,
            "operation": operation,
        }
    })
}

fn check_volcengine_v1_code(body: &Value) -> Result<(), MediaError> {
    let code = body.get("code").and_then(Value::as_i64).unwrap_or(3000);
    if code != 3000 {
        let msg = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Volcengine V1 TTS error");
        return Err(MediaError::business(Some(code.to_string()), msg, false));
    }
    Ok(())
}

async fn recv_volc_frame(
    reader: &mut super::ws::WsReader,
    keys: &CredentialSet,
    cancel: &CancellationToken,
) -> Result<volcengine_frame::VolcFrame, MediaError> {
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(MediaError::new(MediaErrorCode::Cancelled, "Volcengine WS cancelled"));
            }
            item = reader.recv() => item,
        };
        match next {
            Some(Ok(WsMessage::Binary(raw))) => {
                let frame = volcengine_frame::decode_frame(&raw).map_err(|e| e.scrubbed(keys))?;
                if frame.msg_type == volcengine_frame::MSG_ERROR {
                    let msg = String::from_utf8_lossy(&frame.payload).into_owned();
                    return Err(MediaError::business(
                        frame.error_code.map(|c| c.to_string()),
                        msg,
                        false,
                    )
                    .scrubbed(keys));
                }
                return Ok(frame);
            }
            Some(Ok(WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Text(txt))) => {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    format!("unexpected text WS frame in Volcengine binary stream: {txt}"),
                )
                .scrubbed(keys));
            }
            Some(Ok(WsMessage::Close(frame))) => {
                let reason = frame.map(|f| f.reason).unwrap_or_default();
                return Err(MediaError::new(
                    MediaErrorCode::Transport,
                    format!("Volcengine WS closed unexpectedly: {reason}"),
                )
                .scrubbed(keys));
            }
            Some(Err(e)) => return Err(e.scrubbed(keys)),
            None => {
                return Err(MediaError::new(
                    MediaErrorCode::Transport,
                    "Volcengine WS closed before frame",
                ));
            }
        }
    }
}

// ============================================================================
// 7. Alibaba DashScope TTS (Qwen-TTS, Bailian HTTP, CosyVoice WS)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct DashScopeTtsRequest {
    pub text: String,
    pub voice: String,
    pub language_type: Option<String>,
    pub format: Option<TtsAudioFormat>,
    pub sample_rate: Option<u32>,
    pub rate: Option<f64>,
    pub pitch: Option<f64>,
    pub volume: Option<u32>,
    pub sse: bool,
    pub allow_http_loopback_download: bool,
}

pub(crate) fn build_dashscope_url(
    base_url: &str,
    embedded_proxy: bool,
    api_path: &str,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy {
        return Ok(format!("{trimmed}{api_path}"));
    }
    let path_lower = url.path().trim_end_matches('/').to_ascii_lowercase();
    let root = if let Some(prefix) = trimmed.strip_suffix("/api/v1") {
        prefix
    } else if path_lower.ends_with("/api/v1") {
        &trimmed[..trimmed.len() - 7]
    } else if path_lower.ends_with("/v1") {
        &trimmed[..trimmed.len() - 3]
    } else {
        trimmed
    };
    Ok(format!("{root}{api_path}"))
}

pub(crate) fn check_dashscope_business_status(
    _headers: &HeaderMap,
    body: &Value,
) -> Result<(), MediaError> {
    if let Some(code) = body
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let msg = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("DashScope business error");
        return Err(MediaError::business(Some(code.to_owned()), msg, false));
    }
    Ok(())
}

/// 7a. DashScope Qwen-TTS (`POST /api/v1/services/aigc/multimodal-generation/generation`).
pub async fn dashscope_qwen_tts<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &DashScopeTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "DashScope Qwen-TTS",
    )?;
    if endpoint.model.trim().is_empty() || req.text.trim().is_empty() || req.voice.trim().is_empty()
    {
        return Err(MediaError::invalid_request(
            "DashScope Qwen-TTS requires non-empty model, text, and voice",
        ));
    }

    let url = build_dashscope_url(
        &endpoint.base_url,
        endpoint.embedded_proxy,
        "/api/v1/services/aigc/multimodal-generation/generation",
    )?;
    let (mut headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    let mut input = json!({
        "text": req.text,
        "voice": req.voice,
    });
    if let Some(lt) = &req.language_type {
        input["language_type"] = json!(lt);
    }
    let body = json!({
        "model": endpoint.model,
        "input": input,
    });

    if !req.sse {
        let http_req = Engine2Request::post_json(url.clone(), body)
            .with_headers(headers, sensitive)
            .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
        let resp = http
            .execute_json(&http_req, &keys, check_dashscope_business_status, cancel)
            .await?;
        let audio_bytes = if let Some(audio_url) = resp
            .body
            .pointer("/output/audio/url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let allow_loopback = req.allow_http_loopback_download || is_loopback_url(&url);
            let dl = validate_media_download_url(audio_url, allow_loopback)?;
            let dl_req = Engine2Request::get(dl.as_str())
                .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
            http.execute_bytes(&dl_req, &keys, cancel)
                .await?
                .bytes
                .to_vec()
        } else if let Some(b64) = resp
            .body
            .pointer("/output/audio/data")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let raw = decode_base64_bytes(b64)?;
            maybe_wrap_pcm_if_raw(raw, req.format, req.sample_rate.unwrap_or(24_000))
        } else {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "missing output.audio.url or output.audio.data in DashScope Qwen-TTS response",
            ));
        };
        let (mime, ext) = detect_audio_mime_and_ext(&audio_bytes, req.format);
        let artifact = artifacts
            .write_artifact(owner_id, ext, mime, &audio_bytes)
            .await?;
        return Ok(TtsSynthesisResponse {
            vendor: "dashscope".into(),
            model: endpoint.model.clone(),
            audio: artifact,
            sample_rate: req.sample_rate,
            duration_ms: None,
            request_id: resp
                .body
                .get("request_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| header_request_id(&resp.headers)),
            task_id: None,
        });
    }

    headers.push(("X-DashScope-SSE".to_owned(), "enable".to_owned()));
    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let mut pcm_or_audio = Vec::new();
    let mut request_id = None;

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
                    format!("invalid DashScope SSE JSON: {e}"),
                )
            })?;
            check_dashscope_business_status(&HeaderMap::new(), &val)?;
            if request_id.is_none() {
                request_id = val
                    .get("request_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
            if let Some(b64) = val
                .pointer("/output/audio/data")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                pcm_or_audio.extend_from_slice(&decode_base64_bytes(b64)?);
            }
            if val.pointer("/output/finish_reason").and_then(Value::as_str) == Some("stop") {
                return Ok(StreamControl::Done);
            }
            Ok(StreamControl::Continue)
        },
        cancel,
    )
    .await?;

    if pcm_or_audio.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "DashScope Qwen-TTS SSE returned no audio chunks",
        ));
    }
    let final_bytes =
        maybe_wrap_pcm_if_raw(pcm_or_audio, req.format, req.sample_rate.unwrap_or(24_000));
    let (mime, ext) = detect_audio_mime_and_ext(&final_bytes, Some(TtsAudioFormat::Wav));
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &final_bytes)
        .await?;
    Ok(TtsSynthesisResponse {
        vendor: "dashscope".into(),
        model: endpoint.model.clone(),
        audio: artifact,
        sample_rate: req.sample_rate,
        duration_ms: None,
        request_id,
        task_id: None,
    })
}

fn maybe_wrap_pcm_if_raw(
    raw: Vec<u8>,
    requested_fmt: Option<TtsAudioFormat>,
    sample_rate: u32,
) -> Vec<u8> {
    let has_container = (raw.len() >= 12 && &raw[0..4] == b"RIFF" && &raw[8..12] == b"WAVE")
        || raw.starts_with(b"ID3")
        || raw.starts_with(b"OggS")
        || raw.starts_with(b"fLaC")
        || (raw.len() >= 2 && raw[0] == 0xFF && (raw[1] & 0xE0) == 0xE0);
    if !has_container
        && matches!(
            requested_fmt,
            None | Some(TtsAudioFormat::Pcm | TtsAudioFormat::Wav)
        )
    {
        pcm16_mono_to_wav(&raw, sample_rate)
    } else {
        raw
    }
}

/// 7b. DashScope Bailian non-realtime HTTP (`POST /api/v1/services/audio/tts/SpeechSynthesizer`).
pub async fn dashscope_speech_synthesizer_http<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &DashScopeTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "DashScope Bailian SpeechSynthesizer",
    )?;
    if endpoint.model.trim().is_empty() || req.text.trim().is_empty() || req.voice.trim().is_empty()
    {
        return Err(MediaError::invalid_request(
            "DashScope SpeechSynthesizer requires non-empty model, text, and voice",
        ));
    }

    let url = build_dashscope_url(
        &endpoint.base_url,
        endpoint.embedded_proxy,
        "/api/v1/services/audio/tts/SpeechSynthesizer",
    )?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let mut parameters = json!({
        "voice": req.voice,
        "format": fmt.as_minimax_str(),
        "sample_rate": req.sample_rate.unwrap_or(22_050),
    });
    if let Some(rate) = req.rate {
        parameters["rate"] = json!(rate);
    }
    if let Some(pitch) = req.pitch {
        parameters["pitch"] = json!(pitch);
    }
    if let Some(vol) = req.volume {
        parameters["volume"] = json!(vol);
    }

    let body = json!({
        "model": endpoint.model,
        "input": { "text": req.text },
        "parameters": parameters,
    });
    let http_req = Engine2Request::post_json(url, body)
        .with_headers(headers, sensitive)
        .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);

    match http
        .execute_hybrid(&http_req, &keys, check_dashscope_business_status, cancel)
        .await?
    {
        Engine2HybridResponse::Bytes(resp) => {
            let (mime, ext) = detect_audio_mime_and_ext(&resp.bytes, Some(fmt));
            let artifact = artifacts
                .write_artifact(owner_id, ext, mime, &resp.bytes)
                .await?;
            Ok(TtsSynthesisResponse {
                vendor: "dashscope".into(),
                model: endpoint.model.clone(),
                audio: artifact,
                sample_rate: req.sample_rate,
                duration_ms: None,
                request_id: header_request_id(&resp.headers),
                task_id: None,
            })
        }
        Engine2HybridResponse::Json(resp) => {
            check_dashscope_business_status(&resp.headers, &resp.body)?;
            Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "DashScope SpeechSynthesizer returned unexpected JSON without audio",
            ))
        }
    }
}

/// 7c. DashScope CosyVoice WS (`wss://{host}/api-ws/v1/inference`).
pub async fn dashscope_cosyvoice_ws<A: MediaArtifactStore>(
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &DashScopeTtsRequest,
    cancel: &CancellationToken,
) -> Result<TtsSynthesisResponse, MediaError> {
    ensure_auth_kind(
        cred,
        endpoint,
        &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
        "DashScope CosyVoice WS",
    )?;
    if endpoint.model.trim().is_empty() || req.text.trim().is_empty() || req.voice.trim().is_empty()
    {
        return Err(MediaError::invalid_request(
            "DashScope CosyVoice WS requires non-empty model, text, and voice",
        ));
    }

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
    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let mut params = json!({
        "text_type": "PlainText",
        "voice": req.voice,
        "format": fmt.as_minimax_str(),
        "sample_rate": req.sample_rate.unwrap_or(22_050),
    });
    if let Some(rate) = req.rate {
        params["rate"] = json!(rate);
    }
    if let Some(pitch) = req.pitch {
        params["pitch"] = json!(pitch);
    }
    if let Some(vol) = req.volume {
        params["volume"] = json!(vol);
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
                "task": "tts",
                "function": "SpeechSynthesizer",
                "model": endpoint.model,
                "parameters": params,
                "input": {},
            }
        }))
        .await?;

    // Wait for `task-started`
    loop {
        let ev = recv_ws_json(&mut reader, &keys, cancel).await?;
        let ev_name = ev.pointer("/header/event").and_then(Value::as_str);
        match ev_name {
            Some("task-started") => break,
            Some("task-failed") => {
                return Err(dashscope_ws_task_failed_err(&ev).scrubbed(&keys));
            }
            _ => {}
        }
    }

    writer
        .send_json(&json!({
            "header": {
                "action": "continue-task",
                "task_id": task_id,
                "streaming": "duplex",
            },
            "payload": {
                "input": { "text": req.text }
            }
        }))
        .await?;

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

    let mut audio_bytes = Vec::new();
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(MediaError::new(MediaErrorCode::Cancelled, "DashScope CosyVoice WS cancelled"));
            }
            item = reader.recv() => item,
        };
        match next {
            Some(Ok(WsMessage::Binary(b))) => audio_bytes.extend_from_slice(&b),
            Some(Ok(WsMessage::Text(txt))) => {
                let ev: Value = serde_json::from_str(&txt).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("invalid DashScope WS JSON event: {e}"),
                    )
                })?;
                match ev.pointer("/header/event").and_then(Value::as_str) {
                    Some("task-finished") => break,
                    Some("task-failed") => {
                        return Err(dashscope_ws_task_failed_err(&ev).scrubbed(&keys));
                    }
                    _ => {}
                }
            }
            Some(Ok(WsMessage::Pong(_))) => continue,
            Some(Ok(WsMessage::Close(_))) | None => break,
            Some(Err(e)) => return Err(e.scrubbed(&keys)),
        }
    }

    let _ = writer.close(1000, "done").await;
    if audio_bytes.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "DashScope CosyVoice WS completed without audio bytes",
        ));
    }
    let (mime, ext) = detect_audio_mime_and_ext(&audio_bytes, Some(fmt));
    let artifact = artifacts
        .write_artifact(owner_id, ext, mime, &audio_bytes)
        .await?;
    Ok(TtsSynthesisResponse {
        vendor: "dashscope".into(),
        model: endpoint.model.clone(),
        audio: artifact,
        sample_rate: req.sample_rate,
        duration_ms: None,
        request_id: Some(task_id),
        task_id: None,
    })
}

fn dashscope_ws_task_failed_err(ev: &Value) -> MediaError {
    let code = ev
        .pointer("/header/error_code")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let msg = ev
        .pointer("/header/error_message")
        .and_then(Value::as_str)
        .unwrap_or("DashScope WS task failed");
    MediaError::business(code, msg, false)
}

// ============================================================================
// 8. Partner Gateway Compat Seed Audio (Non-official · Strictly Isolated)
// ============================================================================

const PARTNER_SEED_SAMPLE_RATES: [u32; 7] = [8000, 16000, 22050, 24000, 32000, 44100, 48000];

#[derive(Clone, Debug, Default)]
pub struct PartnerSeedAudioRequest {
    pub text: String,
    pub voice: Option<String>,
    pub format: Option<TtsAudioFormat>,
    pub sample_rate: Option<u32>,
    /// Multiplier in `0.5..=2.0` -> mapped to `speechRate` (`((speed - 1.0) * 100.0).round()`).
    pub speed: Option<f64>,
    /// Raw `speechRate` override (ignored if `speed` is set).
    pub speech_rate: Option<i32>,
    /// Volume in dB (`|volume_db| <= 20*log10(2)`) -> mapped to `loudnessRate`.
    pub volume_db: Option<f64>,
    /// Raw `loudnessRate` override (ignored if `volume_db` is set).
    pub loudness_rate: Option<i32>,
    /// Pitch in `-12..=12` -> mapped to `pitchRate`.
    pub pitch: Option<i32>,
    pub duration: Option<f64>,
    pub image_url: Option<String>,
    pub audio_url: Option<String>,
    pub allow_http_loopback_download: bool,
}

fn build_partner_seed_url(
    base_url: &str,
    suffix: &str,
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
        Ok(format!("{trimmed}{suffix}"))
    } else {
        Ok(format!("{trimmed}/v1{suffix}"))
    }
}

pub fn build_partner_seed_audio_payload(
    model: &str,
    req: &PartnerSeedAudioRequest,
) -> Result<Value, MediaError> {
    if model.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Partner Seed Audio model must not be empty",
        ));
    }
    if req.text.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Partner Seed Audio text must not be empty",
        ));
    }
    let sample_rate = req.sample_rate.unwrap_or(24_000);
    if !PARTNER_SEED_SAMPLE_RATES.contains(&sample_rate) {
        return Err(MediaError::invalid_request(format!(
            "Partner Seed Audio unsupported sampleRate {sample_rate}"
        )));
    }
    let speech_rate = if let Some(speed) = req.speed {
        if !speed.is_finite() || !(0.5..=2.0).contains(&speed) {
            return Err(MediaError::invalid_request(
                "Partner Seed Audio speed must be in 0.5..=2.0",
            ));
        }
        ((speed - 1.0) * 100.0).round() as i32
    } else {
        req.speech_rate.unwrap_or(0)
    };
    let loudness_rate = if let Some(vol_db) = req.volume_db {
        let max_db = 20.0 * 2.0_f64.log10();
        if !vol_db.is_finite() || vol_db.abs() > max_db + 1e-9 {
            return Err(MediaError::invalid_request(
                "Partner Seed Audio volume_db out of range",
            ));
        }
        ((10_f64.powf(vol_db / 20.0) - 1.0) * 100.0).round() as i32
    } else {
        req.loudness_rate.unwrap_or(0)
    };
    let pitch_rate = req.pitch.unwrap_or(0);
    if !(-12..=12).contains(&pitch_rate) {
        return Err(MediaError::invalid_request(
            "Partner Seed Audio pitch must be in -12..=12",
        ));
    }
    if let Some(dur) = req.duration
        && (!dur.is_finite() || dur <= 0.0 || dur > 3600.0)
    {
        return Err(MediaError::invalid_request(
            "Partner Seed Audio duration must be in (0, 3600]",
        ));
    }

    let fmt = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let mut body = json!({
        "model": model,
        "text": req.text.trim(),
        "format": fmt.as_partner_seed_str(),
        "sampleRate": sample_rate,
        "speechRate": speech_rate,
        "loudnessRate": loudness_rate,
        "pitchRate": pitch_rate,
    });
    if let Some(voice) = &req.voice {
        body["voice"] = json!(voice);
    }
    if let Some(dur) = req.duration {
        body["duration"] = json!(dur);
    }
    let mut references = Vec::new();
    if let Some(audio_url) = &req.audio_url {
        references.push(json!({ "audio_url": audio_url }));
        body["audio_url"] = json!(audio_url);
    }
    if let Some(image_url) = &req.image_url {
        references.push(json!({ "image_url": image_url }));
        body["image_url"] = json!(image_url);
    }
    if !references.is_empty() {
        body["references"] = Value::Array(references);
    }
    Ok(body)
}

pub struct PartnerSeedAudioAdapter {
    pub base_url: String,
    pub embedded_proxy: bool,
    pub headers: Vec<(String, String)>,
    pub sensitive_headers: Vec<String>,
    pub format: TtsAudioFormat,
}

impl PartnerSeedAudioAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        format: TtsAudioFormat,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "Partner Seed Audio TTS",
        )?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        Ok((
            Self {
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                headers,
                sensitive_headers: sensitive,
                format,
            },
            keys,
        ))
    }

    fn output_from_direct_str(&self, raw: &str) -> Result<MediaOutputSource, MediaError> {
        let trimmed = raw.trim();
        let (mime, ext) = self.format.mime_and_ext();
        if trimmed.starts_with("data:audio/") && trimmed.contains(";base64,") {
            let mime_part = trimmed
                .strip_prefix("data:")
                .and_then(|r| r.split_once(";base64,"))
                .map(|(m, _)| m.to_owned())
                .unwrap_or_else(|| mime.to_owned());
            return Ok(MediaOutputSource::InlineBase64 {
                b64: trimmed.to_owned(),
                suggested_ext: ext.to_owned(),
                mime_type: mime_part,
            });
        }
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return Ok(MediaOutputSource::DirectDownload {
                request: Engine2Request::get(trimmed),
                suggested_ext: ext.to_owned(),
                mime_type: mime.to_owned(),
            });
        }
        Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "invalid Partner Seed Audio direct audio reference",
        ))
    }
}

impl Engine3Adapter for PartnerSeedAudioAdapter {
    fn vendor(&self) -> &str {
        "partner-seed-audio"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Tts
    }

    fn accepts_hybrid_submit(&self) -> bool {
        true
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let url = build_partner_seed_url(&self.base_url, "/tts/create", self.embedded_proxy)?;
        Ok(Engine2Request::post_json(url, input.payload.clone())
            .with_headers(self.headers.clone(), self.sensitive_headers.clone())
            .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT))
    }

    fn parse_submit_bytes(
        &self,
        _status: u16,
        headers: &HeaderMap,
        bytes: &Bytes,
    ) -> Result<SubmitOutcome, MediaError> {
        let ct = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if ct.starts_with("text/") {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!("Partner Seed Audio returned unexpected non-audio text body ({ct})"),
            ));
        }
        if !ct.starts_with("audio/") && bytes.is_empty() {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "Partner Seed Audio returned unexpected empty non-audio body",
            ));
        }
        let (mime, ext) = detect_audio_mime_and_ext(bytes, Some(self.format));
        Ok(SubmitOutcome::Completed {
            remote_task_id: None,
            outputs: vec![MediaOutputSource::InlineBytes {
                bytes: bytes.clone(),
                suggested_ext: ext.to_owned(),
                mime_type: if ct.starts_with("audio/") {
                    ct
                } else {
                    mime.to_owned()
                },
            }],
        })
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        check_standard_business_status(headers, body)?;
        let direct = body
            .get("audio_url")
            .or_else(|| body.get("url"))
            .or_else(|| body.pointer("/data/audio_url"))
            .or_else(|| body.pointer("/data/url"))
            .or_else(|| body.pointer("/data/data"))
            .or_else(|| body.get("data"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| {
                s.starts_with("http://")
                    || s.starts_with("https://")
                    || s.starts_with("data:audio/")
            });
        if let Some(d) = direct {
            return Ok(SubmitOutcome::Completed {
                remote_task_id: None,
                outputs: vec![self.output_from_direct_str(d)?],
            });
        }

        let task_id = body
            .get("taskICode")
            .or_else(|| body.get("task_id"))
            .or_else(|| body.get("id"))
            .or_else(|| body.pointer("/data/taskICode"))
            .or_else(|| body.get("data"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Partner Seed Audio submit response missing taskICode or audio result",
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
            MediaError::invalid_request(
                "Partner Seed Audio task missing remote_task_id (taskICode)",
            )
        })?;
        let url =
            build_partner_seed_url(&self.base_url, "/tts/getAudioStatus", self.embedded_proxy)?;
        Ok(
            Engine2Request::post_json(url, json!({ "taskICode": remote_id }))
                .with_headers(self.headers.clone(), self.sensitive_headers.clone()),
        )
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        check_standard_business_status(headers, body)?;
        let status = body
            .get("status")
            .or_else(|| body.pointer("/data/status"))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_ascii_lowercase())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing status in Partner Seed Audio getAudioStatus response",
                )
            })?;

        match status.as_str() {
            "success" | "completed" | "succeeded" => {
                let raw_ref = body
                    .pointer("/data/data")
                    .or_else(|| body.pointer("/data/audio_url"))
                    .or_else(|| body.pointer("/data/url"))
                    .or_else(|| body.get("audio_url"))
                    .or_else(|| body.get("url"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            "Partner Seed Audio succeeded without audio URL or data URI",
                        )
                    })?;
                Ok(PollOutcome::Succeeded {
                    outputs: vec![self.output_from_direct_str(raw_ref)?],
                    poll_cursor: None,
                })
            }
            "failed" | "failure" | "error" | "rejected" | "cancelled" | "canceled" => {
                Ok(PollOutcome::Failed {
                    upstream_code: None,
                    message: body
                        .get("message")
                        .or_else(|| body.get("error"))
                        .and_then(Value::as_str)
                        .unwrap_or("Partner Seed Audio synthesis failed")
                        .to_owned(),
                    retryable: false,
                })
            }
            "queued" | "pending" | "processing" | "in_progress" | "running" | "submitted"
            | "not_start" | "queueing" => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Running,
                poll_cursor: None,
                retry_after_hint: None,
            }),
            other => Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!("unexpected Partner Seed Audio status '{other}'"),
            )),
        }
    }
}

pub async fn partner_seed_audio_tts<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &PartnerSeedAudioRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<MediaTaskRecord, MediaError> {
    let format = req.format.unwrap_or(TtsAudioFormat::Mp3);
    let (adapter, keys) = PartnerSeedAudioAdapter::from_endpoint(endpoint, cred, format)?;
    let payload = build_partner_seed_audio_payload(&endpoint.model, req)?;
    runner
        .run_to_completion(
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

fn is_loopback_url(raw: &str) -> bool {
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
        extract::{Path as AxumPath, Query},
        http::{HeaderMap as AxumHeaders, StatusCode},
        routing::{get, post},
    };
    use std::collections::HashMap;
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
    async fn openai_tts_audio_sse_and_char_limit_validation() {
        let app = Router::new().route(
            "/v1/audio/speech",
            post(|headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(
                    headers.get("authorization").and_then(|v| v.to_str().ok()),
                    Some("Bearer sk-openai-tts")
                );
                if body.get("stream_format").and_then(Value::as_str) == Some("sse") {
                    let c1 = STANDARD.encode(b"ID3-PART1-");
                    let c2 = STANDARD.encode(b"PART2");
                    let sse = format!(
                        "data: {{\"type\":\"speech.audio.delta\",\"audio\":\"{c1}\"}}\n\ndata: {{\"type\":\"speech.audio.delta\",\"audio\":\"{c2}\"}}\n\ndata: {{\"type\":\"speech.audio.done\"}}\n\n"
                    );
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "text/event-stream".parse().unwrap());
                    (StatusCode::OK, h, AxumBytes::from(sse))
                } else {
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "audio/mpeg".parse().unwrap());
                    h.insert("x-request-id", "req-oai-1".parse().unwrap());
                    (StatusCode::OK, h, AxumBytes::from_static(b"ID3-SYNC-AUDIO"))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = InMemoryMediaArtifactStore::new();
        let ep = MediaEndpointConfig::new("openai", format!("http://{addr}"), "gpt-4o-mini-tts");
        let cred = MediaCredential::bearer("sk-openai-tts").unwrap();
        let cancel = CancellationToken::new();

        // > 4096 chars rejected locally
        let too_long = OpenAiTtsRequest {
            input: "a".repeat(4097),
            voice: "alloy".into(),
            ..Default::default()
        };
        let err = openai_tts(&http, &artifacts, &ep, &cred, "o1", &too_long, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::InvalidRequest);

        // Sync binary
        let sync_req = OpenAiTtsRequest {
            input: "hello".into(),
            voice: "coral".into(),
            instructions: Some("cheerful".into()),
            response_format: Some(TtsAudioFormat::Mp3),
            speed: Some(1.1),
            stream_format: Some(OpenAiTtsStreamFormat::Audio),
        };
        let sync_res = openai_tts(&http, &artifacts, &ep, &cred, "o1", &sync_req, &cancel)
            .await
            .unwrap();
        assert_eq!(sync_res.audio.mime_type, "audio/mpeg");
        assert_eq!(sync_res.request_id.as_deref(), Some("req-oai-1"));

        // SSE stream
        let sse_req = OpenAiTtsRequest {
            input: "hello sse".into(),
            voice: "coral".into(),
            stream_format: Some(OpenAiTtsStreamFormat::Sse),
            ..Default::default()
        };
        let sse_res = openai_tts(&http, &artifacts, &ep, &cred, "o2", &sse_req, &cancel)
            .await
            .unwrap();
        assert_eq!(sse_res.audio.size_bytes, 15);
        assert_eq!(&artifacts.snapshot()[1].1, b"ID3-PART1-PART2");
    }

    #[tokio::test]
    async fn gemini_tts_wraps_24khz_pcm_with_44_byte_riff_wav_header() {
        let app = Router::new().route(
            "/v1beta/models/{action}",
            post(
                |AxumPath(action): AxumPath<String>,
                 headers: AxumHeaders,
                 axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(action, "gemini-2.5-flash-preview-tts:generateContent");
                    assert_eq!(
                        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                        Some("goog-tts-key")
                    );
                    assert_eq!(
                        body.pointer("/generationConfig/responseModalities/0")
                            .and_then(Value::as_str),
                        Some("AUDIO")
                    );
                    let pcm = vec![0x11u8, 0x22, 0x33, 0x44];
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
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = InMemoryMediaArtifactStore::new();
        let ep = MediaEndpointConfig::new(
            "gemini",
            format!("http://{addr}"),
            "gemini-2.5-flash-preview-tts",
        );
        let cred = MediaCredential::x_goog_api_key("goog-tts-key").unwrap();
        let cancel = CancellationToken::new();

        let resp = gemini_tts(
            &http,
            &artifacts,
            &ep,
            &cred,
            "gem-1",
            &GeminiTtsRequest {
                text: "Say hello".into(),
                voice: GeminiSpeechVoice::Single("Kore".into()),
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(resp.audio.mime_type, "audio/wav");
        assert!(resp.audio.path.ends_with(".wav"));
        assert_eq!(resp.sample_rate, Some(24_000));
        let wav = &artifacts.snapshot()[0].1;
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(&wav[44..], &[0x11u8, 0x22, 0x33, 0x44]);
    }

    #[tokio::test]
    async fn elevenlabs_and_azure_tts_headers_ssml_and_auth_isolation() {
        let app = Router::new()
            .route(
                "/v1/text-to-speech/{voice_id}/stream",
                post(
                    |AxumPath(voice_id): AxumPath<String>,
                     Query(q): Query<HashMap<String, String>>,
                     headers: AxumHeaders| async move {
                        assert_eq!(voice_id, "JBFqnCBsd6RMkjVDRZzb");
                        assert_eq!(
                            q.get("output_format").map(String::as_str),
                            Some("mp3_44100_128")
                        );
                        assert_eq!(
                            headers.get("xi-api-key").and_then(|v| v.to_str().ok()),
                            Some("xi-secret")
                        );
                        assert!(headers.get("authorization").is_none());
                        let mut h = AxumHeaders::new();
                        h.insert("content-type", "audio/mpeg".parse().unwrap());
                        (StatusCode::OK, h, AxumBytes::from_static(b"ID3-EL"))
                    },
                ),
            )
            .route(
                "/cognitiveservices/v1",
                post(|headers: AxumHeaders, body: AxumBytes| async move {
                    assert_eq!(
                        headers
                            .get("ocp-apim-subscription-key")
                            .and_then(|v| v.to_str().ok()),
                        Some("az-sub-key")
                    );
                    assert_eq!(
                        headers.get("content-type").and_then(|v| v.to_str().ok()),
                        Some("application/ssml+xml")
                    );
                    assert_eq!(headers.get_all("content-type").iter().count(), 1);
                    assert_eq!(
                        headers
                            .get("x-microsoft-outputformat")
                            .and_then(|v| v.to_str().ok()),
                        Some("audio-24khz-48kbitrate-mono-mp3")
                    );
                    assert!(headers.get("user-agent").is_some());
                    let ssml = String::from_utf8(body.to_vec()).unwrap();
                    assert!(ssml.contains("&lt;hi&gt;"));
                    (StatusCode::OK, AxumBytes::from_static(b"ID3-AZURE"))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = InMemoryMediaArtifactStore::new();
        let cancel = CancellationToken::new();

        // ElevenLabs rejects Bearer credential
        let el_ep = MediaEndpointConfig::new(
            "elevenlabs",
            format!("http://{addr}"),
            "eleven_multilingual_v2",
        );
        let wrong_cred = MediaCredential::bearer("not-xi").unwrap();
        let el_req = ElevenLabsTtsRequest {
            voice_id: "JBFqnCBsd6RMkjVDRZzb".into(),
            text: "Hello".into(),
            output_format: Some("mp3_44100_128".into()),
            stream: true,
            ..Default::default()
        };
        assert_eq!(
            elevenlabs_tts(
                &http,
                &artifacts,
                &el_ep,
                &wrong_cred,
                "e1",
                &el_req,
                &cancel
            )
            .await
            .unwrap_err()
            .code,
            MediaErrorCode::InvalidRequest
        );

        let el_cred = MediaCredential::xi_api_key("xi-secret").unwrap();
        let el_res = elevenlabs_tts(&http, &artifacts, &el_ep, &el_cred, "e1", &el_req, &cancel)
            .await
            .unwrap();
        assert_eq!(el_res.audio.size_bytes, 6);

        let az_ep = MediaEndpointConfig::new("azure", format!("http://{addr}"), "");
        let az_cred = MediaCredential::azure_subscription_key("az-sub-key").unwrap();
        let az_res = azure_tts(
            &http,
            &artifacts,
            &az_ep,
            &az_cred,
            "az1",
            &AzureTtsRequest {
                text: "<hi>".into(),
                voice: "en-US-JennyNeural".into(),
                lang: "en-US".into(),
                output_format: "audio-24khz-48kbitrate-mono-mp3".into(),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(az_res.audio.size_bytes, 9);
    }

    #[tokio::test]
    async fn minimax_tts_http_sse_biz_error_ws_and_async_engines() {
        // 1. Regional lock rejects typo domain api.minimax.com
        assert!(
            validate_minimax_host(&url::Url::parse("https://api.minimax.com").unwrap(), false)
                .is_err()
        );
        assert!(
            validate_minimax_host(&url::Url::parse("https://api.minimax.io").unwrap(), false)
                .is_ok()
        );
        assert!(
            validate_minimax_host(&url::Url::parse("https://api.minimaxi.com").unwrap(), false)
                .is_ok()
        );

        let app = Router::new()
            .route(
                "/v1/t2a_v2",
                post(|axum::Json(body): axum::Json<Value>| async move {
                    if body.get("text").and_then(Value::as_str) == Some("biz-err") {
                        return (
                            StatusCode::OK,
                            AxumHeaders::new(),
                            AxumBytes::from_static(
                                br#"{"base_resp":{"status_code":1004,"status_msg":"insufficient balance"}}"#,
                            ),
                        );
                    }
                    if body.get("stream").and_then(Value::as_bool) == Some(true) {
                        let sse = concat!(
                            "data: {\"base_resp\":{\"status_code\":0},\"data\":{\"audio\":\"494433\",\"status\":1}}\n\n",
                            "data: {\"base_resp\":{\"status_code\":0},\"data\":{\"audio\":\"2d4d\",\"status\":1}}\n\n",
                            "data: {\"base_resp\":{\"status_code\":0},\"data\":{\"audio\":\"4944332d4d\",\"status\":2},\"extra_info\":{\"audio_sample_rate\":32000}}\n\n"
                        );
                        let mut h = AxumHeaders::new();
                        h.insert("content-type", "text/event-stream".parse().unwrap());
                        return (StatusCode::OK, h, AxumBytes::from(sse));
                    }
                    (
                        StatusCode::OK,
                        AxumHeaders::new(),
                        AxumBytes::from_static(
                            br#"{"base_resp":{"status_code":0},"data":{"audio":"4944334142"}}"#,
                        ),
                    )
                }),
            )
            .route(
                "/v1/t2a_async_v2",
                post(|| async {
                    (
                        StatusCode::OK,
                        r#"{"base_resp":{"status_code":0},"task_id":123456}"#,
                    )
                }),
            )
            .route(
                "/v1/query/t2a_async_query_v2",
                get(|Query(q): Query<HashMap<String, String>>| async move {
                    assert_eq!(q.get("task_id").map(String::as_str), Some("123456"));
                    (
                        StatusCode::OK,
                        r#"{"base_resp":{"status_code":0},"status":"Success","file_id":9988}"#,
                    )
                }),
            )
            .route(
                "/v1/files/retrieve",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    let fid = q.get("file_id").map(String::as_str).unwrap_or("");
                    assert_eq!(fid, "9988");
                    (
                        StatusCode::OK,
                        r#"{"base_resp":{"status_code":0},"file":{"download_url":"/files/dl.mp3"}}"#,
                    )
                }),
            );

        // Rebind with full host URL for TwoStepDownload
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dl_app = app
            .route(
                "/v1/files/retrieve_full",
                get(move || async move {
                    (
                        StatusCode::OK,
                        format!(
                            r#"{{"base_resp":{{"status_code":0}},"file":{{"download_url":"http://127.0.0.1:{}/dl/mm.mp3"}}}}"#,
                            addr.port()
                        ),
                    )
                }),
            )
            .route(
                "/dl/mm.mp3",
                get(|| async { (StatusCode::OK, b"ID3-MM-ASYNC".to_vec()) }),
            );
        let _srv = tokio::spawn(async move { axum::serve(listener, dl_app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let ep = MediaEndpointConfig::new("minimax", format!("http://{addr}"), "speech-2.8-hd");
        let cred = MediaCredential::bearer("mm-key").unwrap();
        let cancel = CancellationToken::new();

        // Sync hex
        let sync_res = minimax_tts(
            &http,
            artifacts.as_ref(),
            &ep,
            &cred,
            "mm-sync",
            &MiniMaxTtsRequest {
                text: "hi".into(),
                voice_id: "English_expressive_narrator".into(),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(sync_res.audio.size_bytes, 5);
        assert_eq!(&artifacts.snapshot()[0].1, b"ID3AB");

        // HTTP 200 base_resp.status_code != 0 -> BusinessError
        let biz_err = minimax_tts(
            &http,
            artifacts.as_ref(),
            &ep,
            &cred,
            "mm-biz",
            &MiniMaxTtsRequest {
                text: "biz-err".into(),
                voice_id: "v1".into(),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(biz_err.code, MediaErrorCode::BusinessError);
        assert_eq!(biz_err.upstream_code.as_deref(), Some("1004"));

        // SSE stream deduplicates status=2 vs status=1 chunks
        let sse_res = minimax_tts(
            &http,
            artifacts.as_ref(),
            &ep,
            &cred,
            "mm-sse",
            &MiniMaxTtsRequest {
                text: "stream".into(),
                voice_id: "v1".into(),
                stream: true,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(sse_res.sample_rate, Some(32_000));
        assert_eq!(&artifacts.snapshot()[1].1, b"ID3-M");

        // Engine 4 WS
        let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let ws_srv = tokio::spawn(async move {
            let (mut sock, req) = accept_ws_server(&ws_listener).await;
            assert!(req.contains("Authorization: Bearer mm-key"));
            sock.write_all(&encode_ws_server_frame(
                0x1,
                br#"{"event":"connected_success","base_resp":{"status_code":0}}"#,
            ))
            .await
            .unwrap();
            let (_, start_pay) = read_ws_client_frame(&mut sock).await;
            assert!(String::from_utf8_lossy(&start_pay).contains("task_start"));
            sock.write_all(&encode_ws_server_frame(
                0x1,
                br#"{"event":"task_started","base_resp":{"status_code":0}}"#,
            ))
            .await
            .unwrap();
            let (_, cont_pay) = read_ws_client_frame(&mut sock).await;
            assert!(String::from_utf8_lossy(&cont_pay).contains("task_continue"));
            sock.write_all(&encode_ws_server_frame(
                0x1,
                br#"{"event":"task_continued","base_resp":{"status_code":0},"data":{"audio":"494433"},"is_final":false}"#,
            ))
            .await
            .unwrap();
            sock.write_all(&encode_ws_server_frame(
                0x1,
                br#"{"event":"task_finished","base_resp":{"status_code":0},"data":{"audio":"2d5753"},"is_final":true}"#,
            ))
            .await
            .unwrap();
            let (_, fin_pay) = read_ws_client_frame(&mut sock).await;
            assert!(String::from_utf8_lossy(&fin_pay).contains("task_finish"));
        });
        let ws_ep = MediaEndpointConfig::new("minimax", format!("ws://{ws_addr}"), "speech-2.8-hd");
        let ws_res = minimax_tts_ws(
            artifacts.as_ref(),
            &ws_ep,
            &cred,
            "mm-ws",
            &MiniMaxTtsRequest {
                text: "ws hi".into(),
                voice_id: "v1".into(),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(ws_res.audio.size_bytes, 6);
        assert_eq!(&artifacts.snapshot()[2].1, b"ID3-WS");
        ws_srv.await.unwrap();

        // Engine 3 Async (with TwoStepDownload)
        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();
        let async_app = Router::new()
            .route(
                "/v1/t2a_async_v2",
                post(|| async { (StatusCode::OK, r#"{"base_resp":{"status_code":0},"task_id":"mm-t-1"}"#) }),
            )
            .route(
                "/v1/query/t2a_async_query_v2",
                get(|| async { (StatusCode::OK, r#"{"base_resp":{"status_code":0},"status":"Success","file_id":"f-1"}"#) }),
            )
            .route(
                "/v1/files/retrieve",
                get(move || async move {
                    (
                        StatusCode::OK,
                        format!(
                            r#"{{"base_resp":{{"status_code":0}},"file":{{"download_url":"http://127.0.0.1:{}/dl/mm.mp3"}}}}"#,
                            addr2.port()
                        ),
                    )
                }),
            )
            .route("/dl/mm.mp3", get(|| async { (StatusCode::OK, b"ID3-MM-ASYNC".to_vec()) }));
        let _srv2 = tokio::spawn(async move { axum::serve(listener2, async_app).await.unwrap() });
        let async_ep =
            MediaEndpointConfig::new("minimax", format!("http://{addr2}"), "speech-2.8-hd");
        let mm_runner = Engine3Runner::new(http.clone(), repo, artifacts.clone())
            .with_allow_http_loopback_download(true);
        let rec = minimax_tts_async(
            &mm_runner,
            &async_ep,
            &cred,
            &MiniMaxTtsRequest {
                text: "long".into(),
                voice_id: "v1".into(),
                allow_http_loopback_download: true,
                ..Default::default()
            },
            Some("mm-async-k".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(rec.status, MediaTaskStatus::Succeeded);
        assert_eq!(&artifacts.snapshot()[3].1, b"ID3-MM-ASYNC");
    }

    #[tokio::test]
    async fn volcengine_all_5_protocols_and_compression_reject() {
        let app = Router::new()
            .route(
                "/api/v3/tts/unidirectional",
                post(|headers: AxumHeaders| async move {
                    assert_eq!(headers.get("x-api-key").and_then(|v| v.to_str().ok()), Some("volc-v3-key"));
                    assert!(headers.get("x-api-resource-id").is_some());
                    let a1 = STANDARD.encode(b"ID3-V3-");
                    let a2 = STANDARD.encode(b"HTTP");
                    let ndjson = format!(
                        "{{\"code\":0,\"data\":\"{a1}\"}}\n{{\"code\":0,\"data\":\"{a2}\"}}\n{{\"code\":20000000,\"message\":\"ok\"}}\n"
                    );
                    (StatusCode::OK, ndjson)
                }),
            )
            .route(
                "/api/v3/tts/create",
                post(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "audio/mpeg".parse().unwrap());
                    (StatusCode::OK, h, AxumBytes::from_static(b"ID3-OFFICIAL-SEED"))
                }),
            )
            .route(
                "/api/v3/tts/submit",
                post(|| async { (StatusCode::OK, r#"{"code":0,"task_id":"vt-1"}"#) }),
            )
            .route(
                "/api/v3/tts/query",
                post(|axum::Json(b): axum::Json<Value>| async move {
                    assert_eq!(b["task_id"], "vt-1");
                    let mut h = AxumHeaders::new();
                    h.insert("x-api-status-code", "20000000".parse().unwrap());
                    (
                        StatusCode::OK,
                        h,
                        format!(r#"{{"code":20000000,"data":"{}"}}"#, STANDARD.encode(b"ID3-V3-ASYNC")),
                    )
                }),
            )
            .route(
                "/api/v1/tts",
                post(|headers: AxumHeaders| async move {
                    assert_eq!(
                        headers.get("authorization").and_then(|v| v.to_str().ok()),
                        Some("Bearer;v1-semicolon-tok")
                    );
                    (
                        StatusCode::OK,
                        format!(r#"{{"code":3000,"reqid":"r1","data":"{}"}}"#, STANDARD.encode(b"ID3-V1-HTTP")),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let ep = MediaEndpointConfig::new("volcengine", format!("http://{addr}"), "seed-tts-1.1");
        let cred_v3 = MediaCredential::volcengine_v3_api_key("volc-v3-key").unwrap();
        let cred_v1 = MediaCredential::volcengine_v1_semicolon_bearer("v1-semicolon-tok").unwrap();
        let cancel = CancellationToken::new();
        let req = VolcengineTtsRequest {
            text: "你好".into(),
            speaker: "zh_female_cancan".into(),
            ..Default::default()
        };

        // 1. V3 HTTP NDJSON (code 20000000)
        let r_http = volcengine_v3_tts_http(
            &http,
            artifacts.as_ref(),
            &ep,
            &cred_v3,
            "v3-http",
            &req,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(&artifacts.snapshot()[0].1, b"ID3-V3-HTTP");
        assert_eq!(r_http.audio.size_bytes, 11);

        // 2. Official Seed-Audio (/api/v3/tts/create)
        let _r_seed = volcengine_seed_audio_create(
            &http,
            artifacts.as_ref(),
            &ep,
            &cred_v3,
            "v3-seed",
            &req,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(&artifacts.snapshot()[1].1, b"ID3-OFFICIAL-SEED");

        // 3. V3 Async
        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let volc_runner = Engine3Runner::new(http.clone(), repo, artifacts.clone());
        let r_async = volcengine_v3_tts_async(
            &volc_runner,
            &ep,
            &cred_v3,
            &req,
            Some("v3-a".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(r_async.status, MediaTaskStatus::Succeeded);
        assert_eq!(&artifacts.snapshot()[2].1, b"ID3-V3-ASYNC");

        // 4. V1 HTTP semicolon auth
        let _r_v1_http = volcengine_v1_tts_http(
            &http,
            artifacts.as_ref(),
            &ep,
            &cred_v1,
            "v1-http",
            &req,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(&artifacts.snapshot()[3].1, b"ID3-V1-HTTP");

        // 5. V3 Bidi WS
        let ws3_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws3_addr = ws3_listener.local_addr().unwrap();
        let ws3_srv = tokio::spawn(async move {
            let (mut sock, req_hdr) = accept_ws_server(&ws3_listener).await;
            assert!(
                req_hdr
                    .to_ascii_lowercase()
                    .contains("x-api-key: volc-v3-key")
            );
            let (_, f1) = read_ws_client_frame(&mut sock).await;
            assert_eq!(
                volcengine_frame::decode_frame(&f1).unwrap().event_id,
                Some(1)
            );
            sock.write_all(&encode_ws_server_frame(
                0x2,
                &volcengine_frame::encode_v3_event_frame(50, None, b"{}"),
            ))
            .await
            .unwrap();
            let (_, f2) = read_ws_client_frame(&mut sock).await;
            let dec2 = volcengine_frame::decode_frame(&f2).unwrap();
            assert_eq!(dec2.event_id, Some(100));
            let sid = dec2.session_id.unwrap();
            sock.write_all(&encode_ws_server_frame(
                0x2,
                &volcengine_frame::encode_v3_event_frame(150, Some(&sid), b"{}"),
            ))
            .await
            .unwrap();
            let (_, _f3) = read_ws_client_frame(&mut sock).await;
            let (_, _f4) = read_ws_client_frame(&mut sock).await;
            let audio_frame = vec![
                0x11,
                (volcengine_frame::MSG_AUDIO_ONLY_SERVER << 4) | volcengine_frame::FLAG_WITH_EVENT,
                volcengine_frame::SERIAL_NONE << 4,
                0x00,
            ];
            let mut audio_frame = audio_frame;
            audio_frame.extend_from_slice(&352u32.to_be_bytes());
            audio_frame.extend_from_slice(&(sid.len() as u32).to_be_bytes());
            audio_frame.extend_from_slice(sid.as_bytes());
            let audio_pay = b"ID3-V3-WS";
            audio_frame.extend_from_slice(&(audio_pay.len() as u32).to_be_bytes());
            audio_frame.extend_from_slice(audio_pay);
            sock.write_all(&encode_ws_server_frame(0x2, &audio_frame))
                .await
                .unwrap();
            sock.write_all(&encode_ws_server_frame(
                0x2,
                &volcengine_frame::encode_v3_event_frame(152, Some(&sid), b"{}"),
            ))
            .await
            .unwrap();
        });
        let ws3_ep =
            MediaEndpointConfig::new("volcengine", format!("ws://{ws3_addr}"), "seed-tts-1.1");
        let _r_v3_ws = volcengine_v3_tts_ws(
            artifacts.as_ref(),
            &ws3_ep,
            &cred_v3,
            "v3-ws",
            &req,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(&artifacts.snapshot()[4].1, b"ID3-V3-WS");
        ws3_srv.await.unwrap();

        // 6. V1 WS (`Bearer;`) + compression != 0 rejection
        let ws1_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws1_addr = ws1_listener.local_addr().unwrap();
        let ws1_srv = tokio::spawn(async move {
            let (mut sock, req_hdr) = accept_ws_server(&ws1_listener).await;
            assert!(req_hdr.contains("Authorization: Bearer;v1-semicolon-tok"));
            let (_, _) = read_ws_client_frame(&mut sock).await;
            let mut f = vec![
                0x11,
                (volcengine_frame::MSG_AUDIO_ONLY_SERVER << 4) | volcengine_frame::FLAG_NEG_SEQ,
                0x00,
                0x00,
            ];
            f.extend_from_slice(&(-1i32).to_be_bytes());
            let pay = b"ID3-V1-WS";
            f.extend_from_slice(&(pay.len() as u32).to_be_bytes());
            f.extend_from_slice(pay);
            sock.write_all(&encode_ws_server_frame(0x2, &f))
                .await
                .unwrap();
        });
        let ws1_ep = MediaEndpointConfig::new("volcengine-v1", format!("ws://{ws1_addr}"), "v1");
        let _r_v1_ws = volcengine_v1_tts_ws(
            artifacts.as_ref(),
            &ws1_ep,
            &cred_v1,
            "v1-ws",
            &req,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(&artifacts.snapshot()[5].1, b"ID3-V1-WS");
        ws1_srv.await.unwrap();

        // Compression != 0 rejected cleanly
        let bad_frame = vec![0x11, 0xB0, 0x01, 0x00, 0, 0, 0, 1, 0x41];
        assert_eq!(
            volcengine_frame::decode_frame(&bad_frame).unwrap_err().code,
            MediaErrorCode::DecodeError
        );
    }

    #[tokio::test]
    async fn dashscope_qwen_bailian_and_cosyvoice_ws() {
        let app = Router::new()
            .route(
                "/api/v1/services/aigc/multimodal-generation/generation",
                post(|headers: AxumHeaders| async move {
                    if headers.get("x-dashscope-sse").is_some() {
                        let c = STANDARD.encode([0x01u8, 0x02, 0x03, 0x04]);
                        let sse = format!(
                            "data: {{\"request_id\":\"ds-sse-1\",\"output\":{{\"audio\":{{\"data\":\"{c}\"}},\"finish_reason\":\"stop\"}}}}\n\n"
                        );
                        let mut h = AxumHeaders::new();
                        h.insert("content-type", "text/event-stream".parse().unwrap());
                        (StatusCode::OK, h, AxumBytes::from(sse))
                    } else {
                        let mut h = AxumHeaders::new();
                        h.insert("content-type", "application/json".parse().unwrap());
                        (
                            StatusCode::OK,
                            h,
                            AxumBytes::from(
                                format!(
                                    r#"{{"request_id":"ds-1","output":{{"audio":{{"data":"{}"}}}}}}"#,
                                    STANDARD.encode(b"ID3-QWEN")
                                )
                                .into_bytes(),
                            ),
                        )
                    }
                }),
            )
            .route(
                "/api/v1/services/audio/tts/SpeechSynthesizer",
                post(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "audio/mpeg".parse().unwrap());
                    (StatusCode::OK, h, AxumBytes::from_static(b"ID3-BAILIAN"))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let artifacts = InMemoryMediaArtifactStore::new();
        let ep = MediaEndpointConfig::new(
            "dashscope",
            format!("http://{addr}/api/v1"),
            "qwen3-tts-flash",
        );
        let cred = MediaCredential::bearer("ds-key").unwrap();
        let cancel = CancellationToken::new();

        let _q1 = dashscope_qwen_tts(
            &http,
            &artifacts,
            &ep,
            &cred,
            "ds-1",
            &DashScopeTtsRequest {
                text: "你好".into(),
                voice: "Cherry".into(),
                format: Some(TtsAudioFormat::Mp3),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(&artifacts.snapshot()[0].1, b"ID3-QWEN");

        let q2 = dashscope_qwen_tts(
            &http,
            &artifacts,
            &ep,
            &cred,
            "ds-2",
            &DashScopeTtsRequest {
                text: "你好".into(),
                voice: "Cherry".into(),
                sse: true,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(q2.audio.mime_type, "audio/wav");
        assert_eq!(&artifacts.snapshot()[1].1[..4], b"RIFF");

        let _bailian = dashscope_speech_synthesizer_http(
            &http,
            &artifacts,
            &ep,
            &cred,
            "ds-3",
            &DashScopeTtsRequest {
                text: "hello".into(),
                voice: "longanyang".into(),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(&artifacts.snapshot()[2].1, b"ID3-BAILIAN");

        // CosyVoice WS
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
            let (_, _) = read_ws_client_frame(&mut sock).await;
            let (_, _) = read_ws_client_frame(&mut sock).await;
            sock.write_all(&encode_ws_server_frame(0x2, b"ID3-COSY"))
                .await
                .unwrap();
            sock.write_all(&encode_ws_server_frame(
                0x1,
                format!(r#"{{"header":{{"event":"task-finished","task_id":"{tid}"}}}}"#).as_bytes(),
            ))
            .await
            .unwrap();
        });
        let ws_ep =
            MediaEndpointConfig::new("dashscope", format!("ws://{ws_addr}"), "cosyvoice-v3-flash");
        let _cosy = dashscope_cosyvoice_ws(
            &artifacts,
            &ws_ep,
            &cred,
            "ds-ws",
            &DashScopeTtsRequest {
                text: "hello cosy".into(),
                voice: "longanyang".into(),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(&artifacts.snapshot()[3].1, b"ID3-COSY");
        ws_srv.await.unwrap();
    }

    #[tokio::test]
    async fn partner_seed_audio_isolated_endpoint_transforms_and_hybrid_async() {
        let seen_paths = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_c = seen_paths.clone();
        let poll_count = Arc::new(AtomicUsize::new(0));
        let poll_c = poll_count.clone();

        let app = Router::new()
            .route(
                "/v1/tts/create",
                post(move |axum::Json(body): axum::Json<Value>| {
                    let s = seen_c.clone();
                    async move {
                        s.lock().unwrap().push("/v1/tts/create".into());
                        assert_eq!(body["format"], "ogg_opus");
                        assert_eq!(body["speechRate"], 50);
                        assert_eq!(body["pitchRate"], 2);
                        if body["text"] == "sync-binary" {
                            let mut h = AxumHeaders::new();
                            h.insert("content-type", "audio/ogg".parse().unwrap());
                            (StatusCode::OK, h, AxumBytes::from_static(b"OggS-PARTNER"))
                        } else {
                            let mut h = AxumHeaders::new();
                            h.insert("content-type", "application/json".parse().unwrap());
                            (
                                StatusCode::OK,
                                h,
                                AxumBytes::from_static(br#"{"taskICode":"icode-42"}"#),
                            )
                        }
                    }
                }),
            )
            .route(
                "/v1/tts/getAudioStatus",
                post(move |axum::Json(body): axum::Json<Value>| {
                    let n = poll_c.fetch_add(1, Ordering::SeqCst);
                    async move {
                        assert_eq!(body["taskICode"], "icode-42");
                        if n == 0 {
                            (StatusCode::OK, r#"{"status":"processing"}"#.to_owned())
                        } else {
                            let data_uri =
                                format!("data:audio/ogg;base64,{}", STANDARD.encode(b"OggS-ASYNC"));
                            (
                                StatusCode::OK,
                                format!(r#"{{"status":"success","data":{{"data":"{data_uri}"}}}}"#),
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
        let ep = MediaEndpointConfig::new(
            "partner-seed-audio",
            format!("http://{addr}"),
            "seed-audio-1",
        );
        let cred = MediaCredential::bearer("partner-tok").unwrap();
        let cancel = CancellationToken::new();

        // Sync binary via Engine3Runner hybrid submit
        let psa_runner = Engine3Runner::new(http.clone(), repo.clone(), artifacts.clone());
        let rec_bin = partner_seed_audio_tts(
            &psa_runner,
            &ep,
            &cred,
            &PartnerSeedAudioRequest {
                text: "sync-binary".into(),
                format: Some(TtsAudioFormat::Opus),
                speed: Some(1.5),
                pitch: Some(2),
                ..Default::default()
            },
            Some("p-bin".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(rec_bin.status, MediaTaskStatus::Succeeded);
        assert_eq!(&artifacts.snapshot()[0].1, b"OggS-PARTNER");

        // Async taskICode -> getAudioStatus -> data URI
        let (adapter, keys) =
            PartnerSeedAudioAdapter::from_endpoint(&ep, &cred, TtsAudioFormat::Opus).unwrap();
        let payload = build_partner_seed_audio_payload(
            "seed-audio-1",
            &PartnerSeedAudioRequest {
                text: "async-task".into(),
                format: Some(TtsAudioFormat::Opus),
                speed: Some(1.5),
                pitch: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        let runner =
            Engine3Runner::new(http, repo, artifacts.clone()).with_poll_config(Engine3PollConfig {
                initial_interval: Duration::from_millis(2),
                max_interval: Duration::from_millis(5),
                ..Default::default()
            });
        let rec_async = runner
            .run_to_completion(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("p-async".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "seed-audio-1".into(),
                    route_id: None,
                    payload,
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(rec_async.status, MediaTaskStatus::Succeeded);
        assert_eq!(&artifacts.snapshot()[1].1, b"OggS-ASYNC");
        assert!(
            seen_paths
                .lock()
                .unwrap()
                .iter()
                .all(|p| p == "/v1/tts/create")
        );

        // Reject text/* Content-Type bodies in PartnerSeedAudioAdapter::parse_submit_bytes
        let mut html_headers = AxumHeaders::new();
        html_headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
        let html_err = adapter
            .parse_submit_bytes(200, &html_headers, &Bytes::from_static(b"<html>err</html>"))
            .unwrap_err();
        assert_eq!(html_err.code, MediaErrorCode::DecodeError);

        // MiniMaxAsyncTtsAdapter preserves "minimax-cn" vendor tag when configured for minimax-cn
        let (cn_adapter, _) = MiniMaxAsyncTtsAdapter::from_endpoint(
            &MediaEndpointConfig::new("minimax-cn", "https://api.minimaxi.com", "speech-2.8-hd"),
            &MediaCredential::bearer("mm-cn").unwrap(),
            TtsAudioFormat::Wav,
        )
        .unwrap();
        assert_eq!(cn_adapter.vendor(), "minimax-cn");
    }
}
