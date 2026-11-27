//! D.8 Realtime Voice WebSocket adapters (Engine 4):
//! 1. OpenAI Realtime API (GA: `wss://api.openai.com/v1/realtime?model=gpt-realtime`,
//!    NO `OpenAI-Beta: realtime=v1`, `session.type: "realtime"`, `session.audio.{input,output}`,
//!    `response.output_audio.delta` / `response.output_audio_transcript.delta` / `response.output_text.delta`)
//! 2. Google Gemini Live API (`BidiGenerateContent`, `setup` -> `setupComplete`,
//!    `realtimeInput.{audio,video,text}` NEVER `mediaChunks`, 16kHz PCM in / 24kHz PCM -> WAV out)

use super::engine2::{current_unix_i64, decode_base64_bytes, pcm16_mono_to_wav};
use super::tts::ensure_auth_kind;
use super::ws::{WsCloseFrame, WsConnectConfig, WsMessage, WsReader, WsWriter, connect_ws};
use super::{
    MediaArtifactRef, MediaArtifactStore, MediaAuthKind, MediaCredential, MediaEndpointConfig,
    MediaError, MediaErrorCode, http_url_to_ws_url, normalize_gemini_model_id,
};
use crate::redaction::CredentialSet;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeAudioFormat {
    #[default]
    Pcm16,
    G711Ulaw,
    G711Alaw,
}

impl RealtimeAudioFormat {
    pub fn as_openai_audio_type(self) -> &'static str {
        match self {
            Self::Pcm16 => "audio/pcm",
            Self::G711Ulaw => "audio/pcmu",
            Self::G711Alaw => "audio/pcma",
        }
    }

    pub fn mime_and_ext(self, sample_rate: u32) -> (String, &'static str) {
        match self {
            Self::Pcm16 => (format!("audio/pcm;rate={sample_rate}"), "pcm"),
            Self::G711Ulaw => ("audio/basic".to_owned(), "ulaw"),
            Self::G711Alaw => ("audio/alaw".to_owned(), "alaw"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RealtimeTurnRequest {
    pub text_prompt: Option<String>,
    /// 16kHz PCM16 mono for Gemini Live; 24kHz PCM16 mono for OpenAI Realtime.
    pub input_audio_pcm16: Option<Bytes>,
    /// Optional JPEG video frame for Gemini Live `realtimeInput.video`.
    pub input_video_jpeg: Option<Bytes>,
    pub instructions: Option<String>,
    pub voice: Option<String>,
    pub output_modalities: Vec<String>,
    pub input_format: RealtimeAudioFormat,
    pub output_format: RealtimeAudioFormat,
    pub wrap_wav: bool,
    pub turn_timeout_ms: Option<u64>,
}

impl Default for RealtimeTurnRequest {
    fn default() -> Self {
        Self {
            text_prompt: None,
            input_audio_pcm16: None,
            input_video_jpeg: None,
            instructions: None,
            voice: None,
            output_modalities: Vec::new(),
            input_format: RealtimeAudioFormat::Pcm16,
            output_format: RealtimeAudioFormat::Pcm16,
            wrap_wav: true,
            turn_timeout_ms: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeTurnResponse {
    pub vendor: String,
    pub model: String,
    pub text: String,
    pub audio_transcript: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<MediaArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
}

// ============================================================================
// 1. OpenAI Realtime API (GA)
// ============================================================================

pub fn build_openai_realtime_ws_url(
    base_url: &str,
    model: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let mut url = http_url_to_ws_url(base_url, embedded_proxy)?;
    let chosen_model = if model.trim().is_empty() {
        "gpt-realtime"
    } else {
        model.trim()
    };
    let path = url.path().trim_end_matches('/').to_owned();
    if !embedded_proxy {
        let lower = path.to_ascii_lowercase();
        let new_path = if lower.ends_with("/realtime") {
            path
        } else if lower.ends_with("/v1") {
            format!("{path}/realtime")
        } else if path.is_empty() || path == "/" {
            "/v1/realtime".to_owned()
        } else {
            format!("{path}/v1/realtime")
        };
        url.set_path(&new_path);
    }
    url.query_pairs_mut()
        .clear()
        .append_pair("model", chosen_model);
    Ok(url.to_string())
}

pub fn build_openai_realtime_session_update(model: &str, req: &RealtimeTurnRequest) -> Value {
    let mut input_fmt = json!({
        "type": req.input_format.as_openai_audio_type(),
    });
    if req.input_format == RealtimeAudioFormat::Pcm16 {
        input_fmt["rate"] = json!(24_000);
    }
    let mut output_fmt = json!({
        "type": req.output_format.as_openai_audio_type(),
    });
    if req.output_format == RealtimeAudioFormat::Pcm16 {
        output_fmt["rate"] = json!(24_000);
    }
    let mut output_obj = json!({
        "format": output_fmt,
    });
    if let Some(voice) = req
        .voice
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        output_obj["voice"] = json!(voice);
    }

    let mut session = json!({
        "type": "realtime",
        "model": model,
        "audio": {
            "input": {
                "format": input_fmt,
            },
            "output": output_obj,
        }
    });
    if let Some(instr) = req
        .instructions
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        session["instructions"] = json!(instr);
    }
    json!({
        "type": "session.update",
        "session": session,
    })
}

pub struct OpenAiRealtimeSession {
    pub model: String,
    writer: WsWriter,
    reader: WsReader,
    keys: CredentialSet,
}

impl OpenAiRealtimeSession {
    pub async fn connect(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        cancel: &CancellationToken,
    ) -> Result<Self, MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "OpenAI Realtime",
        )?;
        let model = if endpoint.model.trim().is_empty() {
            "gpt-realtime".to_owned()
        } else {
            endpoint.model.trim().to_owned()
        };
        let ws_url =
            build_openai_realtime_ws_url(&endpoint.base_url, &model, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        // GA explicitly removes `OpenAI-Beta: realtime=v1`.
        let filtered_headers: Vec<(String, String)> = headers
            .into_iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("openai-beta"))
            .collect();
        let (writer, reader) = connect_ws(
            &ws_url,
            WsConnectConfig {
                headers: filtered_headers,
                sensitive_headers: sensitive,
                ..Default::default()
            },
            &keys,
            cancel,
        )
        .await?;
        Ok(Self {
            model,
            writer,
            reader,
            keys,
        })
    }

    pub async fn run_turn<A: MediaArtifactStore>(
        &mut self,
        artifacts: &A,
        owner_id: &str,
        req: &RealtimeTurnRequest,
        cancel: &CancellationToken,
    ) -> Result<RealtimeTurnResponse, MediaError> {
        let has_text = req
            .text_prompt
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        let has_audio = req
            .input_audio_pcm16
            .as_ref()
            .is_some_and(|b| !b.is_empty());
        if !has_text && !has_audio {
            return Err(MediaError::invalid_request(
                "OpenAI Realtime turn requires text_prompt or input_audio_pcm16",
            ));
        }

        let timeout = Duration::from_millis(req.turn_timeout_ms.unwrap_or(60_000));
        let deadline = tokio::time::Instant::now() + timeout;

        let update_msg = build_openai_realtime_session_update(&self.model, req);
        self.writer.send_json(&update_msg).await?;

        // Wait for session.updated acknowledgment (skipping initial session.created if emitted).
        loop {
            let msg = recv_with_deadline(
                &mut self.reader,
                deadline,
                cancel,
                &self.keys,
                "OpenAI Realtime",
            )
            .await?;
            match msg {
                WsMessage::Text(txt) => {
                    let val: Value = serde_json::from_str(&txt).map_err(|e| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            format!("invalid OpenAI Realtime JSON: {e}"),
                        )
                        .scrubbed(&self.keys)
                    })?;
                    let ev_type = val.get("type").and_then(Value::as_str).unwrap_or("");
                    if ev_type == "error" {
                        return Err(extract_openai_ws_error(&val, &self.keys));
                    }
                    if ev_type == "session.updated" {
                        break;
                    }
                }
                WsMessage::Close(frame) => {
                    return Err(map_ws_close_error(frame, "OpenAI Realtime", &self.keys));
                }
                WsMessage::Binary(_) | WsMessage::Pong(_) => {}
            }
        }

        if let Some(pcm) = &req.input_audio_pcm16
            && !pcm.is_empty()
        {
            self.writer
                .send_json(&json!({
                    "type": "input_audio_buffer.append",
                    "audio": STANDARD.encode(pcm),
                }))
                .await?;
            self.writer
                .send_json(&json!({ "type": "input_audio_buffer.commit" }))
                .await?;
        }
        if let Some(text) = req
            .text_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            self.writer
                .send_json(&json!({
                    "type": "conversation.item.create",
                    "item": {
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": text,
                        }]
                    }
                }))
                .await?;
        }

        let modalities = if req.output_modalities.is_empty() {
            vec!["audio".to_owned(), "text".to_owned()]
        } else {
            req.output_modalities.clone()
        };
        self.writer
            .send_json(&json!({
                "type": "response.create",
                "response": {
                    "output_modalities": modalities,
                }
            }))
            .await?;

        let mut audio_buf: Vec<u8> = Vec::new();
        let mut text_out = String::new();
        let mut transcript_out = String::new();
        let mut response_id: Option<String> = None;

        loop {
            let msg = recv_with_deadline(
                &mut self.reader,
                deadline,
                cancel,
                &self.keys,
                "OpenAI Realtime",
            )
            .await?;
            match msg {
                WsMessage::Text(txt) => {
                    let val: Value = serde_json::from_str(&txt).map_err(|e| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            format!("invalid OpenAI Realtime event JSON: {e}"),
                        )
                        .scrubbed(&self.keys)
                    })?;
                    let ev_type = val.get("type").and_then(Value::as_str).unwrap_or("");
                    match ev_type {
                        "error" => return Err(extract_openai_ws_error(&val, &self.keys)),
                        "response.output_audio.delta" => {
                            if let Some(delta) = val.get("delta").and_then(Value::as_str) {
                                let decoded = decode_base64_bytes(delta)?;
                                audio_buf.extend_from_slice(&decoded);
                            }
                        }
                        "response.output_audio_transcript.delta" => {
                            if let Some(delta) = val.get("delta").and_then(Value::as_str) {
                                transcript_out.push_str(delta);
                            }
                        }
                        "response.output_text.delta" => {
                            if let Some(delta) = val.get("delta").and_then(Value::as_str) {
                                text_out.push_str(delta);
                            }
                        }
                        "response.done" => {
                            response_id = val
                                .pointer("/response/id")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned);
                            if val.pointer("/response/status").and_then(Value::as_str)
                                == Some("failed")
                            {
                                let msg = val
                                    .pointer("/response/status_details/error/message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("OpenAI Realtime response failed");
                                return Err(
                                    MediaError::business(None, msg, false).scrubbed(&self.keys)
                                );
                            }
                            break;
                        }
                        _ => {}
                    }
                }
                WsMessage::Close(frame) => {
                    if let Some(ref f) = frame
                        && f.code != 1000
                    {
                        return Err(map_ws_close_error(frame, "OpenAI Realtime", &self.keys));
                    }
                    break;
                }
                WsMessage::Binary(_) | WsMessage::Pong(_) => {}
            }
        }

        let audio = if audio_buf.is_empty() {
            None
        } else {
            let (final_bytes, mime, ext) =
                if req.wrap_wav && req.output_format == RealtimeAudioFormat::Pcm16 {
                    let already_wav = audio_buf.len() >= 12
                        && &audio_buf[0..4] == b"RIFF"
                        && &audio_buf[8..12] == b"WAVE";
                    let wav = if already_wav {
                        audio_buf
                    } else {
                        pcm16_mono_to_wav(&audio_buf, 24_000)
                    };
                    (wav, "audio/wav".to_owned(), "wav")
                } else {
                    let (m, e) = req.output_format.mime_and_ext(24_000);
                    (audio_buf, m, e)
                };
            Some(
                artifacts
                    .write_artifact(owner_id, ext, &mime, &final_bytes)
                    .await?,
            )
        };

        Ok(RealtimeTurnResponse {
            vendor: "openai-realtime".to_owned(),
            model: self.model.clone(),
            text: text_out,
            audio_transcript: transcript_out,
            audio,
            response_id,
        })
    }

    pub async fn close(self) -> Result<(), MediaError> {
        self.writer.close(1000, "done").await
    }
}

pub async fn openai_realtime_turn<A: MediaArtifactStore>(
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &RealtimeTurnRequest,
    cancel: &CancellationToken,
) -> Result<RealtimeTurnResponse, MediaError> {
    let mut session = OpenAiRealtimeSession::connect(endpoint, cred, cancel).await?;
    let res = session.run_turn(artifacts, owner_id, req, cancel).await;
    let _ = session.close().await;
    res
}

fn extract_openai_ws_error(val: &Value, keys: &CredentialSet) -> MediaError {
    let code = val
        .pointer("/error/code")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let msg = val
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("OpenAI Realtime WebSocket error");
    MediaError::business(code, msg, false).scrubbed(keys)
}

// ============================================================================
// 2. Gemini Live API (BidiGenerateContent, realtimeInput.{audio,video,text})
// ============================================================================

fn strip_trailing_api_version(path: &str) -> Option<&str> {
    let lower = path.to_ascii_lowercase();
    for ver in ["/v1beta1", "/v1beta", "/v1"] {
        if lower.ends_with(ver) {
            return Some(&path[..path.len() - ver.len()]);
        }
    }
    None
}

fn validate_vertex_model_resource_path(model: &str) -> Result<String, MediaError> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed != model {
        return Err(MediaError::invalid_request(
            "invalid Vertex model resource path",
        ));
    }
    let segments: Vec<&str> = trimmed.split('/').collect();
    let valid_shape = matches!(
        segments.as_slice(),
        [
            "projects",
            proj,
            "locations",
            loc,
            "publishers",
            pub_name,
            "models",
            model_id
        ] if !proj.is_empty() && !loc.is_empty() && !pub_name.is_empty() && !model_id.is_empty()
    ) || matches!(
        segments.as_slice(),
        ["projects", proj, "locations", loc, "models", model_id]
            if !proj.is_empty() && !loc.is_empty() && !model_id.is_empty()
    );
    if !valid_shape {
        return Err(MediaError::invalid_request(
            "invalid Vertex model resource path (expected projects/{project}/locations/{location}/publishers/google/models/{model})",
        ));
    }
    for seg in &segments {
        super::validate_path_segment("Vertex model segment", seg)?;
        if seg.contains(':') {
            return Err(MediaError::invalid_request(
                "invalid Vertex model segment: contains forbidden characters",
            ));
        }
    }
    Ok(trimmed.to_owned())
}

pub fn build_gemini_live_ws_url(
    base_url: &str,
    embedded_proxy: bool,
    is_vertex: bool,
) -> Result<String, MediaError> {
    let mut url = http_url_to_ws_url(base_url, embedded_proxy)?;
    let path = url.path().trim_end_matches('/').to_owned();
    if path.contains("BidiGenerateContent") {
        return Ok(url.to_string());
    }
    const GEMINI_LIVE_SUFFIX: &str =
        "/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
    const VERTEX_LIVE_SUFFIX: &str =
        "/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent";

    let suffix = if is_vertex {
        VERTEX_LIVE_SUFFIX
    } else {
        GEMINI_LIVE_SUFFIX
    };

    let new_path = if is_vertex && !embedded_proxy && path.to_ascii_lowercase().starts_with("/v1/")
    {
        suffix.to_owned()
    } else if let Some(prefix) = strip_trailing_api_version(&path) {
        format!("{prefix}{suffix}")
    } else if path.is_empty() || path == "/" {
        suffix.to_owned()
    } else {
        format!("{path}{suffix}")
    };
    url.set_path(&new_path);
    Ok(url.to_string())
}

pub struct GeminiLiveSession {
    vendor_tag: String,
    pub model: String,
    writer: WsWriter,
    reader: WsReader,
    keys: CredentialSet,
    is_vertex: bool,
}

impl GeminiLiveSession {
    pub async fn connect(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        cancel: &CancellationToken,
    ) -> Result<Self, MediaError> {
        let is_vertex = endpoint.vendor.eq_ignore_ascii_case("gemini-live-vertex")
            || cred.kind == MediaAuthKind::Bearer && endpoint.base_url.contains("aiplatform");
        let allowed = if is_vertex {
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken][..]
        } else {
            &[
                MediaAuthKind::XGoogApiKey,
                MediaAuthKind::Bearer,
                MediaAuthKind::ProxyToken,
            ][..]
        };
        ensure_auth_kind(cred, endpoint, allowed, "Gemini Live API")?;
        let model = if endpoint.model.trim().is_empty() {
            "gemini-2.5-flash-native-audio-preview-12-2025".to_owned()
        } else if is_vertex && endpoint.model.trim().starts_with("projects/") {
            validate_vertex_model_resource_path(&endpoint.model)?
        } else {
            normalize_gemini_model_id(&endpoint.model)?.to_owned()
        };
        let ws_url =
            build_gemini_live_ws_url(&endpoint.base_url, endpoint.embedded_proxy, is_vertex)?;
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        let (writer, reader) = connect_ws(
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
        let vendor_tag = if is_vertex {
            "gemini-live-vertex"
        } else {
            "gemini-live"
        };
        Ok(Self {
            vendor_tag: vendor_tag.to_owned(),
            model,
            writer,
            reader,
            keys,
            is_vertex,
        })
    }

    pub async fn run_turn<A: MediaArtifactStore>(
        &mut self,
        artifacts: &A,
        owner_id: &str,
        req: &RealtimeTurnRequest,
        cancel: &CancellationToken,
    ) -> Result<RealtimeTurnResponse, MediaError> {
        let has_text = req
            .text_prompt
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        let has_audio = req
            .input_audio_pcm16
            .as_ref()
            .is_some_and(|b| !b.is_empty());
        let has_video = req.input_video_jpeg.as_ref().is_some_and(|b| !b.is_empty());
        if !has_text && !has_audio && !has_video {
            return Err(MediaError::invalid_request(
                "Gemini Live turn requires text_prompt, input_audio_pcm16, or input_video_jpeg",
            ));
        }

        let timeout = Duration::from_millis(req.turn_timeout_ms.unwrap_or(60_000));
        let deadline = tokio::time::Instant::now() + timeout;

        let setup_model = if self.is_vertex && self.model.starts_with("projects/") {
            self.model.clone()
        } else {
            format!("models/{}", self.model)
        };
        let modalities: Vec<String> = if req.output_modalities.is_empty() {
            vec!["AUDIO".to_owned()]
        } else {
            req.output_modalities
                .iter()
                .map(|m| m.trim().to_ascii_uppercase())
                .collect()
        };
        let mut gen_cfg = json!({
            "responseModalities": modalities,
        });
        if let Some(voice) = req
            .voice
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            gen_cfg["speechConfig"] = json!({
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": voice,
                    }
                }
            });
        }
        let mut setup_obj = json!({
            "model": setup_model,
            "generationConfig": gen_cfg,
        });
        if let Some(instr) = req
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            setup_obj["systemInstruction"] = json!({
                "parts": [{ "text": instr }]
            });
        }

        // First frame MUST be setup; wait for setupComplete before sending realtimeInput.
        self.writer
            .send_json(&json!({ "setup": setup_obj }))
            .await?;

        loop {
            let msg = recv_with_deadline(
                &mut self.reader,
                deadline,
                cancel,
                &self.keys,
                "Gemini Live",
            )
            .await?;
            match msg {
                WsMessage::Text(txt) => {
                    let val: Value = serde_json::from_str(&txt).map_err(|e| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            format!("invalid Gemini Live setup JSON: {e}"),
                        )
                        .scrubbed(&self.keys)
                    })?;
                    if handle_gemini_live_setup_json(&val, &self.keys)? {
                        break;
                    }
                }
                WsMessage::Binary(bin) => {
                    if let Ok(val) = serde_json::from_slice::<Value>(&bin)
                        && handle_gemini_live_setup_json(&val, &self.keys)?
                    {
                        break;
                    }
                }
                WsMessage::Close(frame) => {
                    return Err(map_ws_close_error(frame, "Gemini Live", &self.keys));
                }
                WsMessage::Pong(_) => {}
            }
        }

        // Emit realtimeInput.{audio, video, text} — NEVER mediaChunks!
        if let Some(pcm) = &req.input_audio_pcm16
            && !pcm.is_empty()
        {
            self.writer
                .send_json(&json!({
                    "realtimeInput": {
                        "audio": {
                            "data": STANDARD.encode(pcm),
                            "mimeType": "audio/pcm;rate=16000",
                        }
                    }
                }))
                .await?;
            self.writer
                .send_json(&json!({
                    "realtimeInput": {
                        "audioStreamEnd": true
                    }
                }))
                .await?;
        }
        if let Some(jpeg) = &req.input_video_jpeg
            && !jpeg.is_empty()
        {
            self.writer
                .send_json(&json!({
                    "realtimeInput": {
                        "video": {
                            "data": STANDARD.encode(jpeg),
                            "mimeType": "image/jpeg",
                        }
                    }
                }))
                .await?;
        }
        if let Some(text) = req
            .text_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            self.writer
                .send_json(&json!({
                    "realtimeInput": {
                        "text": text,
                    }
                }))
                .await?;
        }

        let mut pcm_out: Vec<u8> = Vec::new();
        let mut text_out = String::new();
        let mut transcript_out = String::new();

        loop {
            let msg = recv_with_deadline(
                &mut self.reader,
                deadline,
                cancel,
                &self.keys,
                "Gemini Live",
            )
            .await?;
            match msg {
                WsMessage::Text(txt) => {
                    let val: Value = serde_json::from_str(&txt).map_err(|e| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            format!("invalid Gemini Live server JSON: {e}"),
                        )
                        .scrubbed(&self.keys)
                    })?;
                    if handle_gemini_live_turn_json(
                        &val,
                        &mut pcm_out,
                        &mut text_out,
                        &mut transcript_out,
                        &self.keys,
                    )? {
                        break;
                    }
                }
                WsMessage::Binary(bin) => {
                    if let Ok(val) = serde_json::from_slice::<Value>(&bin)
                        && handle_gemini_live_turn_json(
                            &val,
                            &mut pcm_out,
                            &mut text_out,
                            &mut transcript_out,
                            &self.keys,
                        )?
                    {
                        break;
                    }
                }
                WsMessage::Close(frame) => {
                    if let Some(ref f) = frame
                        && f.code != 1000
                    {
                        return Err(map_ws_close_error(frame, "Gemini Live", &self.keys));
                    }
                    break;
                }
                WsMessage::Pong(_) => {}
            }
        }

        let audio = if pcm_out.is_empty() {
            None
        } else {
            let (final_bytes, mime, ext) = if req.wrap_wav {
                let already_wav =
                    pcm_out.len() >= 12 && &pcm_out[0..4] == b"RIFF" && &pcm_out[8..12] == b"WAVE";
                let wav = if already_wav {
                    pcm_out
                } else {
                    pcm16_mono_to_wav(&pcm_out, 24_000)
                };
                (wav, "audio/wav", "wav")
            } else {
                (pcm_out, "audio/pcm;rate=24000", "pcm")
            };
            Some(
                artifacts
                    .write_artifact(owner_id, ext, mime, &final_bytes)
                    .await?,
            )
        };

        Ok(RealtimeTurnResponse {
            vendor: self.vendor_tag.clone(),
            model: self.model.clone(),
            text: text_out,
            audio_transcript: transcript_out,
            audio,
            response_id: None,
        })
    }

    pub async fn close(self) -> Result<(), MediaError> {
        self.writer.close(1000, "done").await
    }
}

pub async fn gemini_live_turn<A: MediaArtifactStore>(
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &RealtimeTurnRequest,
    cancel: &CancellationToken,
) -> Result<RealtimeTurnResponse, MediaError> {
    let mut session = GeminiLiveSession::connect(endpoint, cred, cancel).await?;
    let res = session.run_turn(artifacts, owner_id, req, cancel).await;
    let _ = session.close().await;
    res
}

fn handle_gemini_live_setup_json(val: &Value, keys: &CredentialSet) -> Result<bool, MediaError> {
    if let Some(err_obj) = val.get("error").filter(|v| !v.is_null()) {
        let msg = err_obj
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Gemini Live setup error");
        return Err(MediaError::business(None, msg, false).scrubbed(keys));
    }
    Ok(val.get("setupComplete").is_some() || val.get("setup_complete").is_some())
}

fn handle_gemini_live_turn_json(
    val: &Value,
    pcm_out: &mut Vec<u8>,
    text_out: &mut String,
    transcript_out: &mut String,
    keys: &CredentialSet,
) -> Result<bool, MediaError> {
    if let Some(err_obj) = val.get("error").filter(|v| !v.is_null()) {
        let msg = err_obj
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Gemini Live server error");
        return Err(MediaError::business(None, msg, false).scrubbed(keys));
    }
    let sc = val
        .get("serverContent")
        .or_else(|| val.get("server_content"));
    if let Some(sc) = sc {
        if let Some(parts) = sc
            .pointer("/modelTurn/parts")
            .or_else(|| sc.pointer("/model_turn/parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    text_out.push_str(t);
                }
                if let Some(inline) = part.get("inlineData").or_else(|| part.get("inline_data"))
                    && let Some(b64) = inline.get("data").and_then(Value::as_str)
                {
                    let decoded = decode_base64_bytes(b64)?;
                    pcm_out.extend_from_slice(&decoded);
                }
            }
        }
        if let Some(tx) = sc
            .pointer("/outputTranscription/text")
            .or_else(|| sc.pointer("/output_transcription/text"))
            .and_then(Value::as_str)
        {
            transcript_out.push_str(tx);
        }
        if sc
            .get("turnComplete")
            .or_else(|| sc.get("turn_complete"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn recv_with_deadline(
    reader: &mut WsReader,
    deadline: tokio::time::Instant,
    cancel: &CancellationToken,
    keys: &CredentialSet,
    label: &str,
) -> Result<WsMessage, MediaError> {
    let next = tokio::select! {
        _ = cancel.cancelled() => {
            return Err(MediaError::new(MediaErrorCode::Cancelled, format!("{label} cancelled")));
        }
        _ = tokio::time::sleep_until(deadline) => {
            return Err(MediaError::new(MediaErrorCode::Timeout, format!("{label} turn timed out")));
        }
        msg = reader.recv() => msg,
    };
    let Some(msg_res) = next else {
        return Err(MediaError::new(
            MediaErrorCode::Transport,
            format!("{label} WebSocket closed unexpectedly"),
        ));
    };
    msg_res.map_err(|e| e.scrubbed(keys))
}

fn map_ws_close_error(
    frame: Option<WsCloseFrame>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::InMemoryMediaArtifactStore;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn url_builders_normalize_after_validation() {
        let oa = build_openai_realtime_ws_url("https://api.openai.com/v1", "gpt-realtime", false)
            .unwrap();
        assert_eq!(oa, "wss://api.openai.com/v1/realtime?model=gpt-realtime");

        let oa_root =
            build_openai_realtime_ws_url("https://api.openai.com", "gpt-realtime", false).unwrap();
        assert_eq!(
            oa_root,
            "wss://api.openai.com/v1/realtime?model=gpt-realtime"
        );

        let gem = build_gemini_live_ws_url(
            "https://generativelanguage.googleapis.com/v1beta",
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            gem,
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent"
        );

        let vtx =
            build_gemini_live_ws_url("https://us-central1-aiplatform.googleapis.com", false, true)
                .unwrap();
        assert_eq!(
            vtx,
            "wss://us-central1-aiplatform.googleapis.com/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent"
        );
    }

    #[tokio::test]
    async fn openai_realtime_ga_protocol_and_wav_output() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pcm_out = vec![0x01u8, 0x02, 0x03, 0x04];
        let pcm_b64 = STANDARD.encode(&pcm_out);

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut br = BufReader::new(r);
            let mut req_line = String::new();
            br.read_line(&mut req_line).await.unwrap();
            assert!(req_line.contains("/v1/realtime?model=gpt-realtime"));

            let mut saw_auth = false;
            let mut saw_beta = false;
            loop {
                let mut line = String::new();
                br.read_line(&mut line).await.unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if lower.starts_with("authorization:") && line.contains("Bearer oa-rt-secret") {
                    saw_auth = true;
                }
                if lower.starts_with("openai-beta:") {
                    saw_beta = true;
                }
                if let Some(k) = line
                    .strip_prefix("Sec-WebSocket-Key:")
                    .or_else(|| line.strip_prefix("sec-websocket-key:"))
                {
                    let accept_src = format!("{}258EAFA5-E914-47DA-95CA-C5AB0DC85B11", k.trim());
                    let accept =
                        STANDARD.encode(super::super::crypto::sha1_digest(accept_src.as_bytes()));
                    w.write_all(
                        format!(
                            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                }
            }
            assert!(saw_auth);
            assert!(
                !saw_beta,
                "OpenAI Realtime GA MUST NOT send OpenAI-Beta header"
            );

            send_server_ws_text(&mut w, r#"{"type":"session.created"}"#).await;
            let upd_txt = read_client_ws_text(&mut br).await;
            let upd: Value = serde_json::from_str(&upd_txt).unwrap();
            assert_eq!(upd["type"], "session.update");
            assert_eq!(upd["session"]["type"], "realtime");
            assert!(upd["session"]["audio"]["input"]["format"]["type"].is_string());
            assert!(upd["session"]["audio"]["output"]["format"]["type"].is_string());
            send_server_ws_text(&mut w, r#"{"type":"session.updated"}"#).await;

            let item_txt = read_client_ws_text(&mut br).await;
            assert!(item_txt.contains("conversation.item.create"));
            let resp_create_txt = read_client_ws_text(&mut br).await;
            assert!(resp_create_txt.contains("response.create"));

            send_server_ws_text(
                &mut w,
                &json!({"type":"response.output_audio.delta","delta":pcm_b64}).to_string(),
            )
            .await;
            send_server_ws_text(
                &mut w,
                r#"{"type":"response.output_audio_transcript.delta","delta":"Hello "}"#,
            )
            .await;
            send_server_ws_text(
                &mut w,
                r#"{"type":"response.output_text.delta","delta":"world"}"#,
            )
            .await;
            send_server_ws_text(
                &mut w,
                r#"{"type":"response.done","response":{"id":"resp-1","status":"completed"}}"#,
            )
            .await;
        });

        let artifacts = InMemoryMediaArtifactStore::new();
        let cancel = CancellationToken::new();
        let resp = openai_realtime_turn(
            &artifacts,
            &MediaEndpointConfig::new("openai", format!("http://{addr}"), "gpt-realtime"),
            &MediaCredential::bearer("oa-rt-secret").unwrap(),
            "u1",
            &RealtimeTurnRequest {
                text_prompt: Some("Say hello".into()),
                voice: Some("alloy".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();

        assert_eq!(resp.text, "world");
        assert_eq!(resp.audio_transcript, "Hello ");
        assert_eq!(resp.response_id.as_deref(), Some("resp-1"));
        let art = resp.audio.expect("expected WAV artifact");
        let bytes = artifacts.read_artifact(&art).await.unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            24_000
        );
        assert_eq!(&bytes[44..], &pcm_out[..]);
    }

    #[tokio::test]
    async fn gemini_live_setup_realtime_input_and_1007_close_scrubbing() {
        // 1. Happy path: setup -> setupComplete -> realtimeInput.{audio,video,text} -> 24kHz WAV
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let out_pcm = vec![0xAAu8, 0xBB, 0xCC, 0xDD];
        let out_b64 = STANDARD.encode(&out_pcm);

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut br = BufReader::new(r);
            let mut req_line = String::new();
            br.read_line(&mut req_line).await.unwrap();
            assert!(req_line.contains("GenerativeService.BidiGenerateContent"));
            assert!(!req_line.contains("?key="));

            let mut saw_xgoog = false;
            loop {
                let mut line = String::new();
                br.read_line(&mut line).await.unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if line.to_ascii_lowercase().starts_with("x-goog-api-key:")
                    && line.contains("gem-live-secret")
                {
                    saw_xgoog = true;
                }
                if let Some(k) = line
                    .strip_prefix("Sec-WebSocket-Key:")
                    .or_else(|| line.strip_prefix("sec-websocket-key:"))
                {
                    let accept_src = format!("{}258EAFA5-E914-47DA-95CA-C5AB0DC85B11", k.trim());
                    let accept =
                        STANDARD.encode(super::super::crypto::sha1_digest(accept_src.as_bytes()));
                    w.write_all(
                        format!(
                            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                }
            }
            assert!(saw_xgoog);

            let setup_txt = read_client_ws_text(&mut br).await;
            assert!(setup_txt.contains("\"setup\""));
            send_server_ws_text(&mut w, r#"{"setupComplete":{}}"#).await;

            let f1 = read_client_ws_text(&mut br).await;
            assert!(f1.contains("audio/pcm;rate=16000"));
            assert!(!f1.contains("mediaChunks"));
            let f2 = read_client_ws_text(&mut br).await;
            assert!(f2.contains("audioStreamEnd"));
            let f3 = read_client_ws_text(&mut br).await;
            assert!(f3.contains("\"video\""));
            assert!(!f3.contains("mediaChunks"));
            let f4 = read_client_ws_text(&mut br).await;
            assert!(f4.contains("\"text\""));

            send_server_ws_text(
                &mut w,
                &json!({
                    "serverContent": {
                        "modelTurn": {
                            "parts": [
                                {"text": "Hi there"},
                                {"inlineData": {"mimeType": "audio/pcm;rate=24000", "data": out_b64}}
                            ]
                        },
                        "outputTranscription": {"text": "Hi there spoken"},
                        "turnComplete": true
                    }
                })
                .to_string(),
            )
            .await;
        });

        let artifacts = InMemoryMediaArtifactStore::new();
        let cancel = CancellationToken::new();
        let resp = gemini_live_turn(
            &artifacts,
            &MediaEndpointConfig::new(
                "gemini-live",
                format!("http://{addr}/v1beta"),
                "gemini-2.5-flash-native-audio-preview-12-2025",
            ),
            &MediaCredential::x_goog_api_key("gem-live-secret").unwrap(),
            "u1",
            &RealtimeTurnRequest {
                input_audio_pcm16: Some(Bytes::from_static(&[1, 2, 3, 4])),
                input_video_jpeg: Some(Bytes::from_static(&[0xFF, 0xD8, 0xFF])),
                text_prompt: Some("Describe what you see".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();

        assert_eq!(resp.text, "Hi there");
        assert_eq!(resp.audio_transcript, "Hi there spoken");
        let saved = artifacts.read_artifact(&resp.audio.unwrap()).await.unwrap();
        assert_eq!(&saved[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(saved[24..28].try_into().unwrap()),
            24_000
        );
        assert_eq!(&saved[44..], &out_pcm[..]);

        // 2. Non-1000 WS close (1007) scrubs secret
        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener2.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut br = BufReader::new(r);
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
                    let accept =
                        STANDARD.encode(super::super::crypto::sha1_digest(accept_src.as_bytes()));
                    w.write_all(
                        format!(
                            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                }
            }
            let _ = read_client_ws_text(&mut br).await;
            let mut close_payload = Vec::new();
            close_payload.extend_from_slice(&1007u16.to_be_bytes());
            close_payload.extend_from_slice(b"bad frame for gem-live-secret");
            let mut frame = vec![0x88u8, close_payload.len() as u8];
            frame.extend_from_slice(&close_payload);
            w.write_all(&frame).await.unwrap();
        });

        let err = gemini_live_turn(
            &artifacts,
            &MediaEndpointConfig::new(
                "gemini-live",
                format!("http://{addr2}/v1beta"),
                "gemini-2.5-flash-native-audio-preview-12-2025",
            ),
            &MediaCredential::x_goog_api_key("gem-live-secret").unwrap(),
            "u1",
            &RealtimeTurnRequest {
                text_prompt: Some("hi".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::Transport);
        assert!(!err.message.contains("gem-live-secret"));
        assert!(err.message.contains("[redacted]"));
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
        send_server_ws_frame(w, 0x81, text.as_bytes()).await;
    }

    async fn send_server_ws_binary(w: &mut tokio::net::tcp::OwnedWriteHalf, bytes: &[u8]) {
        send_server_ws_frame(w, 0x82, bytes).await;
    }

    async fn send_server_ws_frame(
        w: &mut tokio::net::tcp::OwnedWriteHalf,
        first_byte: u8,
        bytes: &[u8],
    ) {
        let mut frame = Vec::with_capacity(bytes.len() + 10);
        frame.push(first_byte);
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
    fn build_gemini_live_ws_url_handles_vertex_v1_and_custom_paths() {
        let vtx_default = build_gemini_live_ws_url(
            "https://us-central1-aiplatform.googleapis.com/v1",
            false,
            true,
        )
        .unwrap();
        assert_eq!(
            vtx_default,
            "wss://us-central1-aiplatform.googleapis.com/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent"
        );

        let vtx_projects = build_gemini_live_ws_url(
            "https://us-central1-aiplatform.googleapis.com/v1/projects/my-proj/locations/us-central1",
            false,
            true,
        )
        .unwrap();
        assert_eq!(
            vtx_projects,
            "wss://us-central1-aiplatform.googleapis.com/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent"
        );

        let vtx_proxy =
            build_gemini_live_ws_url("http://127.0.0.1:8080/proxy/v1", true, true).unwrap();
        assert_eq!(
            vtx_proxy,
            "ws://127.0.0.1:8080/proxy/ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent"
        );

        let gem_default = build_gemini_live_ws_url(
            "https://generativelanguage.googleapis.com/v1beta",
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            gem_default,
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent"
        );

        assert!(
            validate_vertex_model_resource_path(
                "projects/my-proj/locations/us-central1/publishers/google/models/gemini-live-2.5-flash"
            )
            .is_ok()
        );
        assert!(
            validate_vertex_model_resource_path(
                "projects/my-proj/locations/../publishers/google/models/m"
            )
            .is_err()
        );
        assert!(validate_vertex_model_resource_path("projects/p/models/m?x=1").is_err());
    }

    #[tokio::test]
    async fn gemini_live_vertex_resource_model_and_binary_frames_handle_transcription_and_errors() {
        // 1. Binary frames carry outputTranscription + PCM + turnComplete; Vertex uses /v1 base + projects/... model
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let out_pcm = vec![0x11u8, 0x22, 0x33, 0x44];
        let out_b64 = STANDARD.encode(&out_pcm);

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut br = BufReader::new(r);
            let mut req_line = String::new();
            br.read_line(&mut req_line).await.unwrap();
            assert!(
                req_line.starts_with(
                    "GET /ws/google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent "
                ),
                "unexpected Vertex WS path: {req_line}"
            );
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
                    let accept =
                        STANDARD.encode(super::super::crypto::sha1_digest(accept_src.as_bytes()));
                    w.write_all(
                        format!(
                            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                }
            }

            let setup_txt = read_client_ws_text(&mut br).await;
            let setup_val: Value = serde_json::from_str(&setup_txt).unwrap();
            assert_eq!(
                setup_val["setup"]["model"],
                "projects/my-proj/locations/us-central1/publishers/google/models/gemini-live-2.5-flash"
            );
            send_server_ws_binary(&mut w, br#"{"setupComplete":{}}"#).await;

            let _ri = read_client_ws_text(&mut br).await;
            let turn_payload = serde_json::to_vec(&json!({
                "serverContent": {
                    "modelTurn": {
                        "parts": [
                            {"inlineData": {"mimeType": "audio/pcm;rate=24000", "data": out_b64}}
                        ]
                    },
                    "outputTranscription": {"text": "binary-transcript"},
                    "turnComplete": true
                }
            }))
            .unwrap();
            send_server_ws_binary(&mut w, &turn_payload).await;
        });

        let artifacts = InMemoryMediaArtifactStore::new();
        let cancel = CancellationToken::new();
        let resp = gemini_live_turn(
            &artifacts,
            &MediaEndpointConfig::new(
                "gemini-live-vertex",
                format!("http://{addr}/v1"),
                "projects/my-proj/locations/us-central1/publishers/google/models/gemini-live-2.5-flash",
            ),
            &MediaCredential::bearer("vtx-live-secret").unwrap(),
            "u1",
            &RealtimeTurnRequest {
                text_prompt: Some("hello vertex".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(resp.vendor, "gemini-live-vertex");
        assert_eq!(resp.audio_transcript, "binary-transcript");
        let saved = artifacts.read_artifact(&resp.audio.unwrap()).await.unwrap();
        assert_eq!(&saved[44..], &out_pcm[..]);

        // 2. Binary error frame MUST propagate BusinessError and scrub secret
        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr2 = listener2.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener2.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut br = BufReader::new(r);
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
                    let accept =
                        STANDARD.encode(super::super::crypto::sha1_digest(accept_src.as_bytes()));
                    w.write_all(
                        format!(
                            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                }
            }
            let _ = read_client_ws_text(&mut br).await;
            send_server_ws_binary(&mut w, br#"{"setupComplete":{}}"#).await;
            let _ = read_client_ws_text(&mut br).await;
            let err_payload = serde_json::to_vec(&json!({
                "error": {"code": 400, "message": "binary error with vtx-live-secret"}
            }))
            .unwrap();
            send_server_ws_binary(&mut w, &err_payload).await;
        });

        let err = gemini_live_turn(
            &artifacts,
            &MediaEndpointConfig::new(
                "gemini-live-vertex",
                format!("http://{addr2}/v1"),
                "gemini-live-2.5-flash",
            ),
            &MediaCredential::bearer("vtx-live-secret").unwrap(),
            "u1",
            &RealtimeTurnRequest {
                text_prompt: Some("trigger err".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::BusinessError);
        assert!(!err.message.contains("vtx-live-secret"));
        assert!(err.message.contains("[redacted]"));
    }
}
