//! D.7 Video generation adapters (Engine 3):
//! 1. OpenAI Sora (`/v1/videos`, multipart `input_reference`, `/remix`, `/content`, `DELETE`)
//! 2. Google Veo (Gemini `GET /v1beta/{operation_name}` vs Vertex `POST :fetchPredictOperation`)
//! 3. Volcengine Ark Seedance (`/api/v3/contents/generations/tasks`, `content[]` roles, `DELETE`)
//! 4. MiniMax Hailuo V1 (`/v1/video_generation`, PascalCase) & H3 V2 (`/v2/video_generation`, lowercase, `DELETE`)
//! 5. Kling AI (`text2video` / `image2video`, per-request HS256 JWT, status `"succeed"`, string `duration`)
//! 6. Alibaba DashScope Wanx (`X-DashScope-Async: enable`, `PENDING`-only cancel)
//! 7. Runway API (`image_to_video` / `text_to_video`, `X-Runway-Version: 2024-11-06`, pixel `ratio`, `THROTTLED` polling)
//! 8. New API Unified Video (`/v1/videos` + `/content` & compat `/v1/video/generations`, NO `/artifacts` or `/cancel`)

use super::crypto::sha256_hex;
use super::engine2::{
    Engine2Request, MEDIA_DEFAULT_VIDEO_LIMIT, MEDIA_MAX_DOWNLOAD_REDIRECTS, MultipartForm,
    check_standard_business_status, current_unix_i64,
};
use super::engine3::{
    Engine3Adapter, Engine3Completion, Engine3Runner, Engine3SubmitInput, MediaOutputSource,
    PollOutcome, SubmitOutcome,
};
use super::tts::{
    build_dashscope_url, build_minimax_url, check_dashscope_business_status, ensure_auth_kind,
    validate_minimax_host,
};
use super::vectors::build_gemini_v1beta_base;
use super::{
    MediaArtifactRef, MediaArtifactStore, MediaAuthKind, MediaCredential, MediaEndpointConfig,
    MediaError, MediaErrorCode, MediaKind, MediaTaskRecord, MediaTaskRepository, MediaTaskStatus,
    normalize_gemini_model_id, validate_media_url, validate_path_segment,
};
use crate::redaction::CredentialSet;
use bytes::Bytes;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub const RUNWAY_API_VERSION: &str = "2024-11-06";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedVideo {
    pub artifact: MediaArtifactRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoGenerationResult {
    pub vendor: String,
    pub model: String,
    pub task: MediaTaskRecord,
    pub artifacts: Vec<MediaArtifactRef>,
}

pub fn extract_video_result(
    completed: Engine3Completion,
    vendor_label: &str,
) -> Result<VideoGenerationResult, MediaError> {
    match completed.record.status {
        MediaTaskStatus::Succeeded => {
            if completed.record.artifacts.is_empty() {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    format!("{vendor_label} video task completed with zero artifacts"),
                ));
            }
            let model = completed.record.model.clone();
            let artifacts = completed.record.artifacts.clone();
            Ok(VideoGenerationResult {
                vendor: completed.record.vendor.clone(),
                model,
                task: completed.record,
                artifacts,
            })
        }
        MediaTaskStatus::Failed => Err(MediaError::new(
            MediaErrorCode::ProviderError,
            completed
                .record
                .error_message
                .unwrap_or_else(|| format!("{vendor_label} video generation failed")),
        )
        .with_upstream_code(completed.record.error_code)),
        MediaTaskStatus::Cancelled => Err(MediaError::new(
            MediaErrorCode::Cancelled,
            completed
                .record
                .error_message
                .unwrap_or_else(|| format!("{vendor_label} video generation cancelled")),
        )),
        other => Err(MediaError::new(
            MediaErrorCode::ProviderError,
            format!(
                "{vendor_label} video task ended in unexpected status '{}'",
                other.as_str()
            ),
        )),
    }
}

fn build_openai_v1_url(
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

// ============================================================================
// 1. OpenAI Sora (/v1/videos, /remix, /content, DELETE)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SoraSeconds {
    Four,
    Eight,
    Twelve,
}

impl SoraSeconds {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Four => "4",
            Self::Eight => "8",
            Self::Twelve => "12",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, MediaError> {
        match raw.trim() {
            "4" => Ok(Self::Four),
            "8" => Ok(Self::Eight),
            "12" => Ok(Self::Twelve),
            other => Err(MediaError::invalid_request(format!(
                "OpenAI Sora seconds must be string \"4\", \"8\", or \"12\", got '{other}'"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoraContentVariant {
    #[default]
    Video,
    Thumbnail,
    Spritesheet,
}

impl SoraContentVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Thumbnail => "thumbnail",
            Self::Spritesheet => "spritesheet",
        }
    }

    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("thumbnail") => Self::Thumbnail,
            Some("spritesheet") => Self::Spritesheet,
            _ => Self::Video,
        }
    }

    pub fn mime_and_ext(self) -> (&'static str, &'static str) {
        match self {
            Self::Video => ("video/mp4", "mp4"),
            Self::Thumbnail | Self::Spritesheet => ("image/webp", "webp"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SoraInputReference {
    pub filename: String,
    pub mime_type: String,
    pub bytes: Bytes,
}

#[derive(Clone, Debug, Default)]
pub struct SoraVideoRequest {
    pub prompt: String,
    pub size: Option<String>,
    pub seconds: Option<SoraSeconds>,
    pub input_reference: Option<SoraInputReference>,
    pub variant: SoraContentVariant,
}

#[derive(Clone, Debug, Default)]
pub struct SoraRemixRequest {
    pub video_id: String,
    pub prompt: String,
    pub variant: SoraContentVariant,
    pub parent_task_id: Option<String>,
}

pub fn build_sora_submit_payload(model: &str, req: &SoraVideoRequest) -> Result<Value, MediaError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(MediaError::invalid_request(
            "OpenAI Sora requires a non-empty prompt",
        ));
    }
    let model_id = if model.trim().is_empty() {
        "sora-2"
    } else {
        model.trim()
    };
    let mut body = json!({
        "model": model_id,
        "prompt": prompt,
    });
    if let Some(size) = req.size.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["size"] = json!(size);
    }
    if let Some(sec) = req.seconds {
        body["seconds"] = json!(sec.as_str());
    }
    if let Some(ref_img) = &req.input_reference {
        if ref_img.bytes.is_empty() {
            return Err(MediaError::invalid_request(
                "Sora input_reference bytes must not be empty",
            ));
        }
        body["input_reference_meta"] = json!({
            "filename": ref_img.filename,
            "mime_type": ref_img.mime_type,
            "size_bytes": ref_img.bytes.len(),
            "sha256": sha256_hex(&ref_img.bytes),
        });
    }
    Ok(body)
}

pub struct SoraVideoAdapter {
    base_url: String,
    embedded_proxy: bool,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
    variant: SoraContentVariant,
    remix_video_id: Option<String>,
    input_reference: Option<SoraInputReference>,
}

impl SoraVideoAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        variant: SoraContentVariant,
        input_reference: Option<SoraInputReference>,
        remix_video_id: Option<String>,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "OpenAI Sora",
        )?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        if let Some(ref rid) = remix_video_id {
            validate_path_segment("Sora video_id", rid)?;
        }
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        Ok((
            Self {
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                headers,
                sensitive,
                variant,
                remix_video_id,
                input_reference,
            },
            keys,
        ))
    }
}

impl Engine3Adapter for SoraVideoAdapter {
    fn vendor(&self) -> &str {
        "openai"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Video
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        if let Some(ref remix_id) = self.remix_video_id {
            let clean_id = validate_path_segment("Sora video_id", remix_id)?;
            let url = build_openai_v1_url(
                &self.base_url,
                &format!("/videos/{clean_id}/remix"),
                self.embedded_proxy,
            )?;
            let prompt = input
                .payload
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if prompt.is_empty() {
                return Err(MediaError::invalid_request(
                    "OpenAI Sora remix requires a non-empty prompt",
                ));
            }
            return Ok(Engine2Request::post_json(url, json!({ "prompt": prompt }))
                .with_headers(self.headers.clone(), self.sensitive.clone()));
        }

        let url = build_openai_v1_url(&self.base_url, "/videos", self.embedded_proxy)?;
        if let Some(ref_img) = &self.input_reference {
            let mut form = MultipartForm::new()
                .text(
                    "model",
                    input
                        .payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("sora-2"),
                )?
                .text(
                    "prompt",
                    input
                        .payload
                        .get("prompt")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )?
                .file(
                    "input_reference",
                    ref_img.filename.clone(),
                    ref_img.mime_type.clone(),
                    ref_img.bytes.clone(),
                )?;
            if let Some(size) = input.payload.get("size").and_then(Value::as_str) {
                form = form.text("size", size)?;
            }
            if let Some(sec) = input.payload.get("seconds").and_then(Value::as_str) {
                form = form.text("seconds", sec)?;
            }
            return Ok(Engine2Request::post_multipart(url, form)
                .with_headers(self.headers.clone(), self.sensitive.clone()));
        }

        let mut body = input.payload.clone();
        if let Some(obj) = body.as_object_mut() {
            obj.remove("input_reference_meta");
        }
        Ok(Engine2Request::post_json(url, body)
            .with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        let remote_id = body
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "OpenAI Sora submit response missing id",
                )
            })?
            .to_owned();
        Ok(SubmitOutcome::Accepted {
            remote_task_id: remote_id,
            poll_cursor: Some(self.variant.as_str().to_owned()),
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task
            .remote_task_id
            .as_deref()
            .ok_or_else(|| MediaError::invalid_request("missing remote_task_id for Sora poll"))?;
        let clean_id = validate_path_segment("Sora video_id", remote_id)?;
        let url = build_openai_v1_url(
            &self.base_url,
            &format!("/videos/{clean_id}"),
            self.embedded_proxy,
        )?;
        Ok(Engine2Request::get(url).with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
        task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        let status_raw = body
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("in_progress")
            .trim()
            .to_ascii_lowercase();
        match status_raw.as_str() {
            "completed" => {
                let remote_id = task.remote_task_id.as_deref().unwrap_or("");
                let clean_id = validate_path_segment("Sora video_id", remote_id)?;
                let variant = if task.poll_cursor.is_some() {
                    SoraContentVariant::parse(task.poll_cursor.as_deref())
                } else {
                    self.variant
                };
                let (mime, ext) = variant.mime_and_ext();
                let dl_url = build_openai_v1_url(
                    &self.base_url,
                    &format!("/videos/{clean_id}/content?variant={}", variant.as_str()),
                    self.embedded_proxy,
                )?;
                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(dl_url)
                            .with_headers(self.headers.clone(), self.sensitive.clone())
                            .with_max_response_bytes(MEDIA_DEFAULT_VIDEO_LIMIT)
                            .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                        suggested_ext: ext.to_owned(),
                        mime_type: mime.to_owned(),
                    }],
                    poll_cursor: Some(variant.as_str().to_owned()),
                })
            }
            "failed" => {
                let code = body
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let msg = body
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("OpenAI Sora video generation failed")
                    .to_owned();
                Ok(PollOutcome::Failed {
                    upstream_code: code,
                    message: msg,
                    retryable: false,
                })
            }
            "queued" => Ok(PollOutcome::Pending {
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

    fn build_cancel(&self, task: &MediaTaskRecord) -> Result<Option<Engine2Request>, MediaError> {
        let remote_id = task
            .remote_task_id
            .as_deref()
            .ok_or_else(|| MediaError::invalid_request("missing remote_task_id for Sora cancel"))?;
        let clean_id = validate_path_segment("Sora video_id", remote_id)?;
        let url = build_openai_v1_url(
            &self.base_url,
            &format!("/videos/{clean_id}"),
            self.embedded_proxy,
        )?;
        Ok(Some(Engine2Request::delete(url).with_headers(
            self.headers.clone(),
            self.sensitive.clone(),
        )))
    }
}

pub async fn sora_generate_video<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &SoraVideoRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<VideoGenerationResult, MediaError> {
    let model = if endpoint.model.trim().is_empty() {
        "sora-2".to_owned()
    } else {
        endpoint.model.trim().to_owned()
    };
    let payload = build_sora_submit_payload(&model, req)?;
    let (adapter, keys) = SoraVideoAdapter::from_endpoint(
        endpoint,
        cred,
        req.variant,
        req.input_reference.clone(),
        None,
    )?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model,
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    extract_video_result(completed, "OpenAI Sora")
}

pub async fn sora_remix_video<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &SoraRemixRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<VideoGenerationResult, MediaError> {
    let clean_id = validate_path_segment("Sora video_id", &req.video_id)?;
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(MediaError::invalid_request(
            "OpenAI Sora remix requires a non-empty prompt",
        ));
    }
    let model = if endpoint.model.trim().is_empty() {
        "sora-2".to_owned()
    } else {
        endpoint.model.trim().to_owned()
    };
    let (adapter, keys) =
        SoraVideoAdapter::from_endpoint(endpoint, cred, req.variant, None, Some(clean_id.clone()))?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: req.parent_task_id.clone(),
                model,
                route_id: endpoint.route_id.clone(),
                payload: json!({
                    "remix_of": clean_id,
                    "prompt": prompt,
                }),
            },
            &keys,
            cancel,
        )
        .await?;
    extract_video_result(completed, "OpenAI Sora Remix")
}

// ============================================================================
// 2. Google Veo (Gemini GET operation_name vs Vertex POST :fetchPredictOperation)
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VeoPlatform {
    #[default]
    Gemini,
    Vertex,
}

impl VeoPlatform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::Vertex => "vertex",
        }
    }

    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("gemini") => Some(Self::Gemini),
            Some("vertex") => Some(Self::Vertex),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VeoMediaInput {
    pub bytes_base64: Option<String>,
    pub mime_type: Option<String>,
    pub gcs_uri: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct VeoVideoRequest {
    pub prompt: String,
    pub platform: VeoPlatform,
    pub image: Option<VeoMediaInput>,
    pub last_frame: Option<VeoMediaInput>,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
    pub duration_seconds: Option<u32>,
    pub person_generation: Option<String>,
    pub negative_prompt: Option<String>,
    pub storage_uri: Option<String>,
}

fn validate_operation_name(raw: &str) -> Result<String, MediaError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed != raw
        || trimmed.starts_with('/')
        || trimmed.contains("..")
        || trimmed.contains("://")
        || trimmed.to_ascii_lowercase().contains("%2f")
        || trimmed.to_ascii_lowercase().contains("%5c")
        || trimmed.bytes().any(|b| {
            b.is_ascii_control()
                || b.is_ascii_whitespace()
                || matches!(b, b'\\' | b'?' | b'#' | b'@')
        })
    {
        return Err(MediaError::invalid_request(
            "invalid Google Veo operation_name",
        ));
    }
    Ok(trimmed.to_owned())
}

fn build_veo_media_json(input: &VeoMediaInput) -> Option<Value> {
    if let Some(b64) = &input.bytes_base64 {
        Some(json!({
            "bytesBase64Encoded": b64,
            "mimeType": input.mime_type.as_deref().unwrap_or("image/png"),
        }))
    } else {
        input.gcs_uri.as_ref().map(|uri| {
            json!({
                "gcsUri": uri,
                "mimeType": input.mime_type.as_deref().unwrap_or("image/png"),
            })
        })
    }
}

pub fn build_veo_submit_payload(req: &VeoVideoRequest) -> Result<Value, MediaError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(MediaError::invalid_request(
            "Google Veo requires a non-empty prompt",
        ));
    }
    let mut instance = json!({ "prompt": prompt });
    if let Some(img) = req.image.as_ref().and_then(build_veo_media_json) {
        instance["image"] = img;
    }
    if let Some(last) = req.last_frame.as_ref().and_then(build_veo_media_json) {
        instance["lastFrame"] = last;
    }

    let mut params = serde_json::Map::new();
    if let Some(ar) = req
        .aspect_ratio
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        params.insert("aspectRatio".into(), json!(ar));
    }
    if let Some(res) = req
        .resolution
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        params.insert("resolution".into(), json!(res));
    }
    if let Some(dur) = req.duration_seconds {
        params.insert("durationSeconds".into(), json!(dur));
    }
    if let Some(pg) = req
        .person_generation
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        params.insert("personGeneration".into(), json!(pg));
    }
    if let Some(neg) = req
        .negative_prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        params.insert("negativePrompt".into(), json!(neg));
    }
    if let Some(storage) = req
        .storage_uri
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        params.insert("storageUri".into(), json!(storage));
    }

    let mut body = json!({
        "instances": [instance],
    });
    if !params.is_empty() {
        body["parameters"] = Value::Object(params);
    }
    Ok(body)
}

pub struct VeoVideoAdapter {
    vendor_tag: String,
    base_url: String,
    model: String,
    embedded_proxy: bool,
    platform: VeoPlatform,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
}

impl VeoVideoAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        platform: VeoPlatform,
    ) -> Result<(Self, CredentialSet), MediaError> {
        let (allowed, label, vendor_tag) = match platform {
            VeoPlatform::Gemini => (
                &[MediaAuthKind::XGoogApiKey, MediaAuthKind::ProxyToken][..],
                "Google Veo (Gemini API)",
                if endpoint.vendor.eq_ignore_ascii_case("veo-vertex") {
                    "veo-vertex"
                } else {
                    "veo"
                },
            ),
            VeoPlatform::Vertex => (
                &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken][..],
                "Google Veo (Vertex AI)",
                "veo-vertex",
            ),
        };
        ensure_auth_kind(cred, endpoint, allowed, label)?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let model = if endpoint.model.trim().is_empty() {
            "veo-2.0-generate-001".to_owned()
        } else {
            normalize_gemini_model_id(endpoint.model.trim())?.to_owned()
        };
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        Ok((
            Self {
                vendor_tag: vendor_tag.to_owned(),
                base_url: endpoint.base_url.clone(),
                model,
                embedded_proxy: endpoint.embedded_proxy,
                platform,
                headers,
                sensitive,
            },
            keys,
        ))
    }

    fn effective_platform(&self, task: &MediaTaskRecord) -> VeoPlatform {
        VeoPlatform::parse(task.poll_cursor.as_deref()).unwrap_or(self.platform)
    }

    fn resolve_model_for_task(&self, task: &MediaTaskRecord) -> Result<String, MediaError> {
        if !task.model.trim().is_empty() {
            return Ok(normalize_gemini_model_id(task.model.trim())?.to_owned());
        }
        if let Some(op) = task.remote_task_id.as_deref()
            && let Some(pos) = op.find("/models/")
        {
            let rest = &op[pos + 8..];
            if let Some(end) = rest.find('/') {
                let candidate = &rest[..end];
                if !candidate.is_empty() {
                    return Ok(normalize_gemini_model_id(candidate)?.to_owned());
                }
            }
        }
        Ok(self.model.clone())
    }

    fn build_vertex_model_url(&self, model: &str, action: &str) -> Result<String, MediaError> {
        let url = validate_media_url(&self.base_url, self.embedded_proxy)?;
        let trimmed = url.as_str().trim_end_matches('/');
        let path_trimmed = url.path().trim_end_matches('/');
        if !self.embedded_proxy && (path_trimmed.is_empty() || path_trimmed == "/") {
            Ok(format!(
                "{trimmed}/v1/publishers/google/models/{model}:{action}"
            ))
        } else if path_trimmed.contains("/publishers/google") {
            Ok(format!("{trimmed}/models/{model}:{action}"))
        } else {
            Ok(format!(
                "{trimmed}/publishers/google/models/{model}:{action}"
            ))
        }
    }
}

impl Engine3Adapter for VeoVideoAdapter {
    fn vendor(&self) -> &str {
        &self.vendor_tag
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Video
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let model = if input.model.trim().is_empty() {
            self.model.clone()
        } else {
            normalize_gemini_model_id(input.model.trim())?.to_owned()
        };
        let url = match self.platform {
            VeoPlatform::Gemini => {
                let base = build_gemini_v1beta_base(&self.base_url, self.embedded_proxy)?;
                format!("{base}/models/{model}:predictLongRunning")
            }
            VeoPlatform::Vertex => self.build_vertex_model_url(&model, "predictLongRunning")?,
        };
        Ok(Engine2Request::post_json(url, input.payload.clone())
            .with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        if let Some(err_obj) = body.get("error").filter(|v| !v.is_null()) {
            let msg = err_obj
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Veo submit error");
            return Err(MediaError::business(None, msg, false));
        }
        let op_name = body.get("name").and_then(Value::as_str).ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "Google Veo submit response missing operation name",
            )
        })?;
        let clean_op = validate_operation_name(op_name)?;
        Ok(SubmitOutcome::Accepted {
            remote_task_id: clean_op,
            poll_cursor: Some(self.platform.as_str().to_owned()),
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let op_name = task
            .remote_task_id
            .as_deref()
            .ok_or_else(|| MediaError::invalid_request("missing operation name for Veo poll"))?;
        let clean_op = validate_operation_name(op_name)?;
        match self.effective_platform(task) {
            VeoPlatform::Gemini => {
                let base = build_gemini_v1beta_base(&self.base_url, self.embedded_proxy)?;
                let url = format!("{base}/{clean_op}");
                Ok(Engine2Request::get(url)
                    .with_headers(self.headers.clone(), self.sensitive.clone()))
            }
            VeoPlatform::Vertex => {
                let model = self.resolve_model_for_task(task)?;
                let url = self.build_vertex_model_url(&model, "fetchPredictOperation")?;
                Ok(
                    Engine2Request::post_json(url, json!({ "operationName": clean_op }))
                        .with_headers(self.headers.clone(), self.sensitive.clone()),
                )
            }
        }
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
        task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        if let Some(err_obj) = body.get("error").filter(|v| !v.is_null()) {
            let code = err_obj.get("code").and_then(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            });
            let msg = err_obj
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Google Veo operation failed")
                .to_owned();
            return Ok(PollOutcome::Failed {
                upstream_code: code,
                message: msg,
                retryable: false,
            });
        }

        if body.get("done").and_then(Value::as_bool) != Some(true) {
            return Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Running,
                poll_cursor: None,
                retry_after_hint: None,
            });
        }

        let platform = self.effective_platform(task);
        let mut outputs = Vec::new();
        let mut saw_gcs_only = false;

        if let Some(samples) = body
            .pointer("/response/generateVideoResponse/generatedSamples")
            .and_then(Value::as_array)
        {
            for sample in samples {
                let video = sample.get("video").unwrap_or(sample);
                if let Some(uri) = video
                    .get("uri")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if uri.starts_with("gs://") {
                        saw_gcs_only = true;
                        continue;
                    }
                    outputs.push(MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(uri)
                            .with_headers(self.headers.clone(), self.sensitive.clone())
                            .with_max_response_bytes(MEDIA_DEFAULT_VIDEO_LIMIT)
                            .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                        suggested_ext: "mp4".to_owned(),
                        mime_type: "video/mp4".to_owned(),
                    });
                } else if let Some(b64) = video
                    .get("bytesBase64Encoded")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    outputs.push(MediaOutputSource::InlineBase64 {
                        b64: b64.to_owned(),
                        suggested_ext: "mp4".to_owned(),
                        mime_type: "video/mp4".to_owned(),
                    });
                }
            }
        }

        if outputs.is_empty()
            && let Some(videos) = body
                .pointer("/response/videos")
                .or_else(|| body.pointer("/response/generatedVideos"))
                .and_then(Value::as_array)
        {
            for video in videos {
                let v = video.get("video").unwrap_or(video);
                let mime = v
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("video/mp4");
                if let Some(b64) = v
                    .get("bytesBase64Encoded")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    outputs.push(MediaOutputSource::InlineBase64 {
                        b64: b64.to_owned(),
                        suggested_ext: "mp4".to_owned(),
                        mime_type: mime.to_owned(),
                    });
                } else if let Some(uri) = v
                    .get("uri")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    outputs.push(MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(uri)
                            .with_headers(self.headers.clone(), self.sensitive.clone())
                            .with_max_response_bytes(MEDIA_DEFAULT_VIDEO_LIMIT)
                            .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                        suggested_ext: "mp4".to_owned(),
                        mime_type: mime.to_owned(),
                    });
                } else if v
                    .get("gcsUri")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.starts_with("gs://"))
                {
                    saw_gcs_only = true;
                }
            }
        }

        if outputs.is_empty() {
            if saw_gcs_only {
                return Ok(PollOutcome::Failed {
                    upstream_code: Some("gcs_uri_unsupported".to_owned()),
                    message: "Vertex Veo returned gs:// gcsUri only; omit storageUri to receive inline bytesBase64Encoded video".to_owned(),
                    retryable: false,
                });
            }
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!(
                    "Google Veo ({}) operation finished without video outputs",
                    platform.as_str()
                ),
            ));
        }

        Ok(PollOutcome::Succeeded {
            outputs,
            poll_cursor: Some(platform.as_str().to_owned()),
        })
    }
}

pub async fn veo_generate_video<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &VeoVideoRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<VideoGenerationResult, MediaError> {
    let model = if endpoint.model.trim().is_empty() {
        "veo-2.0-generate-001".to_owned()
    } else {
        normalize_gemini_model_id(endpoint.model.trim())?.to_owned()
    };
    let payload = build_veo_submit_payload(req)?;
    let (adapter, keys) = VeoVideoAdapter::from_endpoint(endpoint, cred, req.platform)?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model,
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    extract_video_result(completed, "Google Veo")
}

// ============================================================================
// 3. Volcengine Ark Seedance (/api/v3/contents/generations/tasks)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArkContentRole {
    FirstFrame,
    LastFrame,
    ReferenceImage,
    ReferenceVideo,
    ReferenceAudio,
}

impl ArkContentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstFrame => "first_frame",
            Self::LastFrame => "last_frame",
            Self::ReferenceImage => "reference_image",
            Self::ReferenceVideo => "reference_video",
            Self::ReferenceAudio => "reference_audio",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkContentMedia {
    pub url: String,
    pub role: ArkContentRole,
}

#[derive(Clone, Debug, Default)]
pub struct ArkSeedanceRequest {
    pub prompt: String,
    pub media: Vec<ArkContentMedia>,
    pub duration: Option<u32>,
    pub resolution: Option<String>,
    pub ratio: Option<String>,
    pub generate_audio: Option<bool>,
    pub watermark: Option<bool>,
    pub seed: Option<i64>,
}

fn build_ark_v3_tasks_url(
    base_url: &str,
    suffix: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    let path_lower = url.path().trim_end_matches('/').to_ascii_lowercase();
    if embedded_proxy || path_lower.ends_with("/api/v3") {
        Ok(format!("{trimmed}/contents/generations/tasks{suffix}"))
    } else if path_lower.ends_with("/v3") {
        let prefix = &trimmed[..trimmed.len() - 3];
        Ok(format!(
            "{prefix}/api/v3/contents/generations/tasks{suffix}"
        ))
    } else {
        Ok(format!(
            "{trimmed}/api/v3/contents/generations/tasks{suffix}"
        ))
    }
}

pub fn build_ark_seedance_payload(
    model: &str,
    req: &ArkSeedanceRequest,
) -> Result<Value, MediaError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() && req.media.is_empty() {
        return Err(MediaError::invalid_request(
            "Ark Seedance requires prompt or media reference items",
        ));
    }
    let model_id = if model.trim().is_empty() {
        "doubao-seedance-1-0-pro-250528"
    } else {
        model.trim()
    };
    let mut content = Vec::new();
    if !prompt.is_empty() {
        content.push(json!({
            "type": "text",
            "text": prompt,
        }));
    }
    for item in &req.media {
        let url = item.url.trim();
        if url.is_empty() {
            return Err(MediaError::invalid_request(
                "Ark Seedance media url must not be empty",
            ));
        }
        match item.role {
            ArkContentRole::FirstFrame
            | ArkContentRole::LastFrame
            | ArkContentRole::ReferenceImage => {
                content.push(json!({
                    "type": "image_url",
                    "image_url": { "url": url },
                    "role": item.role.as_str(),
                }));
            }
            ArkContentRole::ReferenceVideo => {
                content.push(json!({
                    "type": "video_url",
                    "video_url": { "url": url },
                    "role": item.role.as_str(),
                }));
            }
            ArkContentRole::ReferenceAudio => {
                content.push(json!({
                    "type": "audio_url",
                    "audio_url": { "url": url },
                    "role": item.role.as_str(),
                }));
            }
        }
    }

    let mut body = json!({
        "model": model_id,
        "content": content,
    });
    if let Some(d) = req.duration {
        body["duration"] = json!(d);
    }
    if let Some(res) = req
        .resolution
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["resolution"] = json!(res);
    }
    if let Some(ratio) = req
        .ratio
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["ratio"] = json!(ratio);
    }
    if let Some(ga) = req.generate_audio {
        body["generate_audio"] = json!(ga);
    }
    if let Some(wm) = req.watermark {
        body["watermark"] = json!(wm);
    }
    if let Some(seed) = req.seed {
        body["seed"] = json!(seed);
    }
    Ok(body)
}

pub struct ArkSeedanceAdapter {
    base_url: String,
    embedded_proxy: bool,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
}

impl ArkSeedanceAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "Volcengine Ark Seedance",
        )?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        Ok((
            Self {
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                headers,
                sensitive,
            },
            keys,
        ))
    }
}

impl Engine3Adapter for ArkSeedanceAdapter {
    fn vendor(&self) -> &str {
        "seedance"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Video
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let url = build_ark_v3_tasks_url(&self.base_url, "", self.embedded_proxy)?;
        Ok(Engine2Request::post_json(url, input.payload.clone())
            .with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        let remote_id = body
            .get("id")
            .or_else(|| body.get("task_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Ark Seedance submit response missing id",
                )
            })?
            .to_owned();
        Ok(SubmitOutcome::Accepted {
            remote_task_id: remote_id,
            poll_cursor: None,
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("missing remote_task_id for Ark Seedance poll")
        })?;
        let clean_id = validate_path_segment("Ark Seedance task_id", remote_id)?;
        let url =
            build_ark_v3_tasks_url(&self.base_url, &format!("/{clean_id}"), self.embedded_proxy)?;
        Ok(Engine2Request::get(url).with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        let status_raw = body
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("running")
            .trim()
            .to_ascii_lowercase();
        match status_raw.as_str() {
            "succeeded" => {
                let video_url = body
                    .pointer("/content/video_url")
                    .or_else(|| body.pointer("/content/0/video_url"))
                    .or_else(|| body.get("video_url"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            "Ark Seedance succeeded without content.video_url",
                        )
                    })?;
                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(video_url)
                            .with_max_response_bytes(MEDIA_DEFAULT_VIDEO_LIMIT)
                            .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                        suggested_ext: "mp4".to_owned(),
                        mime_type: "video/mp4".to_owned(),
                    }],
                    poll_cursor: None,
                })
            }
            "failed" => {
                let code = body
                    .pointer("/error/code")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let msg = body
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Ark Seedance video generation failed")
                    .to_owned();
                Ok(PollOutcome::Failed {
                    upstream_code: code,
                    message: msg,
                    retryable: false,
                })
            }
            "cancelled" | "canceled" => Ok(PollOutcome::Cancelled {
                message: "Ark Seedance task cancelled".to_owned(),
            }),
            "queued" => Ok(PollOutcome::Pending {
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

    fn build_cancel(&self, task: &MediaTaskRecord) -> Result<Option<Engine2Request>, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("missing remote_task_id for Ark Seedance cancel")
        })?;
        let clean_id = validate_path_segment("Ark Seedance task_id", remote_id)?;
        let url =
            build_ark_v3_tasks_url(&self.base_url, &format!("/{clean_id}"), self.embedded_proxy)?;
        Ok(Some(Engine2Request::delete(url).with_headers(
            self.headers.clone(),
            self.sensitive.clone(),
        )))
    }
}

pub async fn ark_seedance_generate<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &ArkSeedanceRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<VideoGenerationResult, MediaError> {
    let model = if endpoint.model.trim().is_empty() {
        "doubao-seedance-1-0-pro-250528".to_owned()
    } else {
        endpoint.model.trim().to_owned()
    };
    let payload = build_ark_seedance_payload(&model, req)?;
    let (adapter, keys) = ArkSeedanceAdapter::from_endpoint(endpoint, cred)?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model,
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    extract_video_result(completed, "Volcengine Ark Seedance")
}

// ============================================================================
// 4. MiniMax Hailuo V1 (/v1/video_generation) & H3 V2 (/v2/video_generation)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct MiniMaxVideoRequest {
    pub prompt: String,
    pub first_frame_image: Option<String>,
    pub last_frame_image: Option<String>,
    pub duration: Option<u32>,
    pub resolution: Option<String>,
    pub prompt_optimizer: Option<bool>,
}

fn is_minimax_h3_model(model: &str) -> bool {
    model.trim().to_ascii_lowercase().starts_with("minimax-h3")
}

fn build_minimax_v2_url(
    base_url: &str,
    path_suffix: &str,
    embedded_proxy: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    validate_minimax_host(&url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy {
        return Ok(format!("{trimmed}{path_suffix}"));
    }
    let path_lower = url.path().trim_end_matches('/').to_ascii_lowercase();
    let root = if path_lower.ends_with("/v1") || path_lower.ends_with("/v2") {
        &trimmed[..trimmed.len() - 3]
    } else {
        trimmed
    };
    Ok(format!("{root}/v2{path_suffix}"))
}

pub fn build_minimax_video_payload(
    model: &str,
    req: &MiniMaxVideoRequest,
) -> Result<Value, MediaError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty()
        && req
            .first_frame_image
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
    {
        return Err(MediaError::invalid_request(
            "MiniMax Video requires prompt or first_frame_image",
        ));
    }
    let model_id = if model.trim().is_empty() {
        "MiniMax-Hailuo-2.3"
    } else {
        model.trim()
    };
    let mut body = json!({
        "model": model_id,
    });
    if !prompt.is_empty() {
        body["prompt"] = json!(prompt);
    }
    if let Some(first) = req
        .first_frame_image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["first_frame_image"] = json!(first);
    }
    if let Some(last) = req
        .last_frame_image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["last_frame_image"] = json!(last);
    }
    if let Some(dur) = req.duration {
        body["duration"] = json!(dur);
    }
    if let Some(res) = req
        .resolution
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["resolution"] = json!(res);
    }
    if let Some(opt) = req.prompt_optimizer {
        body["prompt_optimizer"] = json!(opt);
    }
    Ok(body)
}

pub struct MiniMaxVideoAdapter {
    vendor_tag: String,
    base_url: String,
    embedded_proxy: bool,
    default_v2: bool,
    last_submitted_v2: std::sync::atomic::AtomicBool,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
}

impl MiniMaxVideoAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "MiniMax Video",
        )?;
        let url = validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        validate_minimax_host(&url, endpoint.embedded_proxy)?;
        let vendor_tag = if endpoint.vendor.eq_ignore_ascii_case("minimax-cn") {
            "minimax-cn"
        } else {
            "minimax"
        };
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        let default_v2 = is_minimax_h3_model(&endpoint.model);
        Ok((
            Self {
                vendor_tag: vendor_tag.to_owned(),
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                default_v2,
                last_submitted_v2: std::sync::atomic::AtomicBool::new(default_v2),
                headers,
                sensitive,
            },
            keys,
        ))
    }

    fn is_v2_for_task(&self, task: &MediaTaskRecord) -> bool {
        match task.poll_cursor.as_deref() {
            Some("v2") => true,
            Some("v1") => false,
            _ => {
                if !task.model.trim().is_empty() {
                    is_minimax_h3_model(&task.model)
                } else {
                    self.default_v2
                }
            }
        }
    }
}

impl Engine3Adapter for MiniMaxVideoAdapter {
    fn vendor(&self) -> &str {
        &self.vendor_tag
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Video
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let is_v2 = is_minimax_h3_model(&input.model) || self.default_v2;
        self.last_submitted_v2
            .store(is_v2, std::sync::atomic::Ordering::SeqCst);
        let url = if is_v2 {
            build_minimax_v2_url(&self.base_url, "/video_generation", self.embedded_proxy)?
        } else {
            build_minimax_url(&self.base_url, "/video_generation", self.embedded_proxy)?
        };
        Ok(Engine2Request::post_json(url, input.payload.clone())
            .with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        check_standard_business_status(headers, body)?;
        let remote_id = body
            .get("task_id")
            .or_else(|| body.pointer("/data/task_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "MiniMax Video submit response missing task_id",
                )
            })?
            .to_owned();
        let is_v2 = self
            .last_submitted_v2
            .load(std::sync::atomic::Ordering::SeqCst);
        Ok(SubmitOutcome::Accepted {
            remote_task_id: remote_id,
            poll_cursor: Some(if is_v2 { "v2" } else { "v1" }.to_owned()),
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("missing remote_task_id for MiniMax Video poll")
        })?;
        let clean_id = validate_path_segment("MiniMax task_id", remote_id)?;
        let url = if self.is_v2_for_task(task) {
            build_minimax_v2_url(
                &self.base_url,
                &format!("/query/video_generation/{clean_id}"),
                self.embedded_proxy,
            )?
        } else {
            let base = build_minimax_url(
                &self.base_url,
                "/query/video_generation",
                self.embedded_proxy,
            )?;
            let mut parsed = url::Url::parse(&base)
                .map_err(|_| MediaError::invalid_request("invalid MiniMax poll URL"))?;
            parsed.query_pairs_mut().append_pair("task_id", &clean_id);
            parsed.to_string()
        };
        Ok(Engine2Request::get(url).with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
        task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        check_standard_business_status(headers, body)?;
        let is_v2 = self.is_v2_for_task(task);
        let status_raw = body
            .get("status")
            .or_else(|| body.pointer("/data/status"))
            .and_then(Value::as_str)
            .unwrap_or("Processing")
            .trim();
        let status_lower = status_raw.to_ascii_lowercase();

        match status_lower.as_str() {
            "success" | "succeeded" | "completed" => {
                if let Some(direct_url) = body
                    .get("video_url")
                    .or_else(|| body.pointer("/data/video_url"))
                    .or_else(|| body.pointer("/file/download_url"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    return Ok(PollOutcome::Succeeded {
                        outputs: vec![MediaOutputSource::DirectDownload {
                            request: Engine2Request::get(direct_url)
                                .with_max_response_bytes(MEDIA_DEFAULT_VIDEO_LIMIT)
                                .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                            suggested_ext: "mp4".to_owned(),
                            mime_type: "video/mp4".to_owned(),
                        }],
                        poll_cursor: Some(if is_v2 { "v2" } else { "v1" }.to_owned()),
                    });
                }

                let file_id = body
                    .get("file_id")
                    .or_else(|| body.pointer("/data/file_id"))
                    .and_then(|v| {
                        v.as_str()
                            .map(ToOwned::to_owned)
                            .or_else(|| v.as_i64().map(|n| n.to_string()))
                    })
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            "MiniMax Video succeeded without file_id or video_url",
                        )
                    })?;
                let clean_fid = validate_path_segment("MiniMax file_id", &file_id)?;
                let retrieve_base =
                    build_minimax_url(&self.base_url, "/files/retrieve", self.embedded_proxy)?;
                let mut retrieve_url = url::Url::parse(&retrieve_base)
                    .map_err(|_| MediaError::invalid_request("invalid MiniMax retrieve URL"))?;
                retrieve_url
                    .query_pairs_mut()
                    .append_pair("file_id", &clean_fid);

                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::TwoStepDownload {
                        resolve_request: Engine2Request::get(retrieve_url.as_str())
                            .with_headers(self.headers.clone(), self.sensitive.clone()),
                        url_pointer: "/file/download_url".to_owned(),
                        download_headers: Vec::new(),
                        download_sensitive_headers: Vec::new(),
                        suggested_ext: "mp4".to_owned(),
                        mime_type: "video/mp4".to_owned(),
                    }],
                    poll_cursor: Some(if is_v2 { "v2" } else { "v1" }.to_owned()),
                })
            }
            "fail" | "failed" | "error" => {
                let msg = body
                    .pointer("/base_resp/status_msg")
                    .or_else(|| body.get("error_message"))
                    .and_then(Value::as_str)
                    .unwrap_or("MiniMax video generation failed")
                    .to_owned();
                Ok(PollOutcome::Failed {
                    upstream_code: Some(status_raw.to_owned()),
                    message: msg,
                    retryable: false,
                })
            }
            "cancelled" | "canceled" => Ok(PollOutcome::Cancelled {
                message: "MiniMax video task cancelled".to_owned(),
            }),
            "preparing" | "queueing" | "queued" => Ok(PollOutcome::Pending {
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

    fn build_cancel(&self, task: &MediaTaskRecord) -> Result<Option<Engine2Request>, MediaError> {
        if !self.is_v2_for_task(task) {
            return Ok(None);
        }
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("missing remote_task_id for MiniMax H3 cancel")
        })?;
        let clean_id = validate_path_segment("MiniMax task_id", remote_id)?;
        let url = build_minimax_v2_url(
            &self.base_url,
            &format!("/video_generation/{clean_id}"),
            self.embedded_proxy,
        )?;
        Ok(Some(Engine2Request::delete(url).with_headers(
            self.headers.clone(),
            self.sensitive.clone(),
        )))
    }
}

pub async fn minimax_video_generate<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &MiniMaxVideoRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<VideoGenerationResult, MediaError> {
    let model = if endpoint.model.trim().is_empty() {
        "MiniMax-Hailuo-2.3".to_owned()
    } else {
        endpoint.model.trim().to_owned()
    };
    let payload = build_minimax_video_payload(&model, req)?;
    let (adapter, keys) = MiniMaxVideoAdapter::from_endpoint(endpoint, cred)?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model,
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    extract_video_result(completed, "MiniMax Video")
}

// ============================================================================
// 5. Kling AI (text2video / image2video, per-request HS256 JWT, status "succeed")
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KlingVideoMode {
    #[default]
    Text2Video,
    Image2Video,
}

impl KlingVideoMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text2Video => "text2video",
            Self::Image2Video => "image2video",
        }
    }

    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("text2video") => Some(Self::Text2Video),
            Some("image2video") => Some(Self::Image2Video),
            _ => None,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Text2Video => 0,
            Self::Image2Video => 1,
        }
    }

    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Text2Video),
            1 => Some(Self::Image2Video),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KlingDuration {
    Five,
    Ten,
}

impl KlingDuration {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Five => "5",
            Self::Ten => "10",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, MediaError> {
        match raw.trim() {
            "5" => Ok(Self::Five),
            "10" => Ok(Self::Ten),
            other => Err(MediaError::invalid_request(format!(
                "Kling duration must be string \"5\" or \"10\", got '{other}'"
            ))),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct KlingVideoRequest {
    pub mode: KlingVideoMode,
    pub prompt: Option<String>,
    pub negative_prompt: Option<String>,
    pub image: Option<String>,
    pub image_tail: Option<String>,
    pub duration: Option<KlingDuration>,
    pub aspect_ratio: Option<String>,
    pub cfg_scale: Option<f64>,
    pub kling_mode: Option<String>,
}

fn check_kling_business_status(body: &Value) -> Result<(), MediaError> {
    if let Some(code) = body.get("code").and_then(Value::as_i64)
        && code != 0
    {
        let msg = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Kling AI business error");
        return Err(MediaError::business(Some(code.to_string()), msg, false));
    }
    Ok(())
}

pub fn build_kling_video_payload(
    model: &str,
    req: &KlingVideoRequest,
) -> Result<Value, MediaError> {
    let model_name = if model.trim().is_empty() {
        "kling-v1-6"
    } else {
        model.trim()
    };
    let mut body = json!({
        "model_name": model_name,
        "_kling_mode": req.mode.as_str(),
    });
    match req.mode {
        KlingVideoMode::Text2Video => {
            let prompt = req
                .prompt
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    MediaError::invalid_request("Kling text2video requires a non-empty prompt")
                })?;
            body["prompt"] = json!(prompt);
        }
        KlingVideoMode::Image2Video => {
            let img = req
                .image
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| MediaError::invalid_request("Kling image2video requires image"))?;
            body["image"] = json!(img);
            if let Some(p) = req
                .prompt
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                body["prompt"] = json!(p);
            }
            if let Some(tail) = req
                .image_tail
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                body["image_tail"] = json!(tail);
            }
        }
    }
    if let Some(neg) = req
        .negative_prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["negative_prompt"] = json!(neg);
    }
    if let Some(dur) = req.duration {
        body["duration"] = json!(dur.as_str());
    }
    if let Some(ar) = req
        .aspect_ratio
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["aspect_ratio"] = json!(ar);
    }
    if let Some(cfg) = req.cfg_scale {
        body["cfg_scale"] = json!(cfg);
    }
    if let Some(km) = req
        .kling_mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["mode"] = json!(km);
    }
    Ok(body)
}

pub struct KlingVideoAdapter {
    vendor_tag: String,
    base_url: String,
    embedded_proxy: bool,
    mode: KlingVideoMode,
    last_submitted_mode: std::sync::atomic::AtomicU8,
    cred: MediaCredential,
}

impl KlingVideoAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        mode: KlingVideoMode,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::KlingAkSk, MediaAuthKind::ProxyToken],
            "Kling AI",
        )?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (_, _, keys) = cred.materialize_headers(current_unix_i64())?;
        let vendor_tag = if endpoint.vendor.eq_ignore_ascii_case("kling-intl") {
            "kling-intl"
        } else {
            "kling"
        };
        Ok((
            Self {
                vendor_tag: vendor_tag.to_owned(),
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                mode,
                last_submitted_mode: std::sync::atomic::AtomicU8::new(mode.as_u8()),
                cred: cred.clone(),
            },
            keys,
        ))
    }

    fn effective_mode(&self, task: &MediaTaskRecord) -> KlingVideoMode {
        KlingVideoMode::parse(task.poll_cursor.as_deref())
            .or_else(|| {
                KlingVideoMode::parse(
                    task.request_summary
                        .get("_kling_mode")
                        .and_then(Value::as_str),
                )
            })
            .or_else(|| {
                if task.request_summary.get("image").is_some() {
                    Some(KlingVideoMode::Image2Video)
                } else {
                    None
                }
            })
            .unwrap_or(self.mode)
    }

    fn build_kling_url(&self, mode: KlingVideoMode, suffix: &str) -> Result<String, MediaError> {
        build_openai_v1_url(
            &self.base_url,
            &format!("/videos/{}{suffix}", mode.as_str()),
            self.embedded_proxy,
        )
    }
}

impl Engine3Adapter for KlingVideoAdapter {
    fn vendor(&self) -> &str {
        &self.vendor_tag
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Video
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let mut body = input.payload.clone();
        let mode = KlingVideoMode::parse(body.get("_kling_mode").and_then(Value::as_str))
            .unwrap_or(self.mode);
        self.last_submitted_mode
            .store(mode.as_u8(), std::sync::atomic::Ordering::SeqCst);
        if let Some(obj) = body.as_object_mut() {
            obj.remove("_kling_mode");
        }
        let url = self.build_kling_url(mode, "")?;
        let (headers, sensitive, _) = self.cred.materialize_headers(current_unix_i64())?;
        Ok(Engine2Request::post_json(url, body).with_headers(headers, sensitive))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        check_kling_business_status(body)?;
        let remote_id = body
            .pointer("/data/task_id")
            .or_else(|| body.get("task_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Kling submit response missing data.task_id",
                )
            })?
            .to_owned();
        let mode = KlingVideoMode::from_u8(
            self.last_submitted_mode
                .load(std::sync::atomic::Ordering::SeqCst),
        )
        .unwrap_or(self.mode);
        Ok(SubmitOutcome::Accepted {
            remote_task_id: remote_id,
            poll_cursor: Some(mode.as_str().to_owned()),
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task
            .remote_task_id
            .as_deref()
            .ok_or_else(|| MediaError::invalid_request("missing remote_task_id for Kling poll"))?;
        let clean_id = validate_path_segment("Kling task_id", remote_id)?;
        let mode = self.effective_mode(task);
        let url = self.build_kling_url(mode, &format!("/{clean_id}"))?;
        let (headers, sensitive, _) = self.cred.materialize_headers(current_unix_i64())?;
        Ok(Engine2Request::get(url).with_headers(headers, sensitive))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
        task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        check_kling_business_status(body)?;
        let data = body.get("data").unwrap_or(body);
        let status_raw = data
            .get("task_status")
            .and_then(Value::as_str)
            .unwrap_or("processing")
            .trim()
            .to_ascii_lowercase();
        let mode = self.effective_mode(task);

        match status_raw.as_str() {
            "succeed" | "succeeded" => {
                let videos = data
                    .pointer("/task_result/videos")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            "Kling task succeeded without data.task_result.videos",
                        )
                    })?;
                let mut outputs = Vec::with_capacity(videos.len());
                for v in videos {
                    if let Some(url) = v
                        .get("url")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        outputs.push(MediaOutputSource::DirectDownload {
                            request: Engine2Request::get(url)
                                .with_max_response_bytes(MEDIA_DEFAULT_VIDEO_LIMIT)
                                .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                            suggested_ext: "mp4".to_owned(),
                            mime_type: "video/mp4".to_owned(),
                        });
                    }
                }
                if outputs.is_empty() {
                    return Err(MediaError::new(
                        MediaErrorCode::DecodeError,
                        "Kling task succeeded with empty video URLs",
                    ));
                }
                Ok(PollOutcome::Succeeded {
                    outputs,
                    poll_cursor: Some(mode.as_str().to_owned()),
                })
            }
            "failed" => {
                let msg = data
                    .get("task_status_msg")
                    .or_else(|| body.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Kling video generation failed")
                    .to_owned();
                Ok(PollOutcome::Failed {
                    upstream_code: Some("failed".to_owned()),
                    message: msg,
                    retryable: false,
                })
            }
            "submitted" | "queued" => Ok(PollOutcome::Pending {
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

pub async fn kling_video_generate<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &KlingVideoRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<VideoGenerationResult, MediaError> {
    let model = if endpoint.model.trim().is_empty() {
        "kling-v1-6".to_owned()
    } else {
        endpoint.model.trim().to_owned()
    };
    let payload = build_kling_video_payload(&model, req)?;
    let (adapter, keys) = KlingVideoAdapter::from_endpoint(endpoint, cred, req.mode)?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model,
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    extract_video_result(completed, "Kling AI")
}

// ============================================================================
// 6. Alibaba DashScope Wanx (X-DashScope-Async: enable, PENDING-only cancel)
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct DashScopeWanxRequest {
    pub prompt: String,
    pub img_url: Option<String>,
    pub size: Option<String>,
    pub duration: Option<u32>,
    pub prompt_extend: Option<bool>,
    pub watermark: Option<bool>,
    pub seed: Option<i64>,
}

pub fn build_dashscope_wanx_payload(
    model: &str,
    req: &DashScopeWanxRequest,
) -> Result<Value, MediaError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() && req.img_url.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Err(MediaError::invalid_request(
            "DashScope Wanx requires prompt or img_url",
        ));
    }
    let model_id = if model.trim().is_empty() {
        "wan2.1-t2v-turbo"
    } else {
        model.trim()
    };
    let mut input = serde_json::Map::new();
    if !prompt.is_empty() {
        input.insert("prompt".into(), json!(prompt));
    }
    if let Some(img) = req
        .img_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        input.insert("img_url".into(), json!(img));
    }
    let mut params = serde_json::Map::new();
    if let Some(sz) = req.size.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        params.insert("size".into(), json!(sz));
    }
    if let Some(dur) = req.duration {
        params.insert("duration".into(), json!(dur));
    }
    if let Some(pe) = req.prompt_extend {
        params.insert("prompt_extend".into(), json!(pe));
    }
    if let Some(wm) = req.watermark {
        params.insert("watermark".into(), json!(wm));
    }
    if let Some(seed) = req.seed {
        params.insert("seed".into(), json!(seed));
    }
    let mut body = json!({
        "model": model_id,
        "input": Value::Object(input),
    });
    if !params.is_empty() {
        body["parameters"] = Value::Object(params);
    }
    Ok(body)
}

pub struct DashScopeWanxAdapter {
    base_url: String,
    embedded_proxy: bool,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
}

impl DashScopeWanxAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "DashScope Wanx",
        )?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        Ok((
            Self {
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                headers,
                sensitive,
            },
            keys,
        ))
    }
}

impl Engine3Adapter for DashScopeWanxAdapter {
    fn vendor(&self) -> &str {
        "wanx"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Video
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let url = build_dashscope_url(
            &self.base_url,
            self.embedded_proxy,
            "/api/v1/services/aigc/video-generation/video-synthesis",
        )?;
        let mut headers = self.headers.clone();
        headers.push(("X-DashScope-Async".to_owned(), "enable".to_owned()));
        Ok(Engine2Request::post_json(url, input.payload.clone())
            .with_headers(headers, self.sensitive.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        check_dashscope_business_status(headers, body)?;
        let remote_id = body
            .pointer("/output/task_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "DashScope Wanx submit response missing output.task_id",
                )
            })?
            .to_owned();
        Ok(SubmitOutcome::Accepted {
            remote_task_id: remote_id,
            poll_cursor: None,
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("missing remote_task_id for DashScope Wanx poll")
        })?;
        let clean_id = validate_path_segment("DashScope task_id", remote_id)?;
        let url = build_dashscope_url(
            &self.base_url,
            self.embedded_proxy,
            &format!("/api/v1/tasks/{clean_id}"),
        )?;
        Ok(Engine2Request::get(url).with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        check_dashscope_business_status(headers, body)?;
        let status_raw = body
            .pointer("/output/task_status")
            .and_then(Value::as_str)
            .unwrap_or("RUNNING")
            .trim()
            .to_ascii_uppercase();
        match status_raw.as_str() {
            "SUCCEEDED" => {
                let video_url = body
                    .pointer("/output/video_url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            "DashScope Wanx SUCCEEDED without output.video_url",
                        )
                    })?;
                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(video_url)
                            .with_max_response_bytes(MEDIA_DEFAULT_VIDEO_LIMIT)
                            .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                        suggested_ext: "mp4".to_owned(),
                        mime_type: "video/mp4".to_owned(),
                    }],
                    poll_cursor: None,
                })
            }
            "FAILED" | "UNKNOWN" => {
                let code = body
                    .pointer("/output/code")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or(Some(status_raw));
                let msg = body
                    .pointer("/output/message")
                    .and_then(Value::as_str)
                    .unwrap_or("DashScope Wanx video generation failed")
                    .to_owned();
                Ok(PollOutcome::Failed {
                    upstream_code: code,
                    message: msg,
                    retryable: false,
                })
            }
            "CANCELED" | "CANCELLED" => Ok(PollOutcome::Cancelled {
                message: "DashScope Wanx task canceled".to_owned(),
            }),
            "PENDING" => Ok(PollOutcome::Pending {
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

    fn build_cancel(&self, task: &MediaTaskRecord) -> Result<Option<Engine2Request>, MediaError> {
        if task.status == MediaTaskStatus::Running {
            return Err(MediaError::new(
                MediaErrorCode::Conflict,
                "DashScope Wanx only supports cancelling PENDING/queued tasks (cannot cancel RUNNING task)",
            ));
        }
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("missing remote_task_id for DashScope Wanx cancel")
        })?;
        let clean_id = validate_path_segment("DashScope task_id", remote_id)?;
        let url = build_dashscope_url(
            &self.base_url,
            self.embedded_proxy,
            &format!("/api/v1/tasks/{clean_id}/cancel"),
        )?;
        Ok(Some(
            Engine2Request::post_json(url, json!({}))
                .with_headers(self.headers.clone(), self.sensitive.clone()),
        ))
    }

    fn parse_cancel_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<(), MediaError> {
        check_dashscope_business_status(headers, body)
    }
}

pub async fn dashscope_wanx_generate<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &DashScopeWanxRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<VideoGenerationResult, MediaError> {
    let model = if endpoint.model.trim().is_empty() {
        "wan2.1-t2v-turbo".to_owned()
    } else {
        endpoint.model.trim().to_owned()
    };
    let payload = build_dashscope_wanx_payload(&model, req)?;
    let (adapter, keys) = DashScopeWanxAdapter::from_endpoint(endpoint, cred)?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model,
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    extract_video_result(completed, "DashScope Wanx")
}

// ============================================================================
// 7. Runway API (X-Runway-Version: 2024-11-06, pixel ratio, THROTTLED polling)
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunwayVideoMode {
    #[default]
    ImageToVideo,
    TextToVideo,
}

impl RunwayVideoMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImageToVideo => "image_to_video",
            Self::TextToVideo => "text_to_video",
        }
    }

    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("image_to_video") => Some(Self::ImageToVideo),
            Some("text_to_video") => Some(Self::TextToVideo),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RunwayVideoRequest {
    pub mode: RunwayVideoMode,
    pub prompt_text: Option<String>,
    pub prompt_image: Option<String>,
    pub ratio: Option<String>,
    pub duration: Option<u32>,
    pub seed: Option<u64>,
}

pub fn validate_runway_pixel_ratio(raw: &str) -> Result<String, MediaError> {
    let trimmed = raw.trim();
    let Some((w_str, h_str)) = trimmed.split_once(':') else {
        return Err(MediaError::invalid_request(format!(
            "Runway ratio must be pixel dimensions like '1280:720', got '{trimmed}'"
        )));
    };
    let Ok(w) = w_str.parse::<u32>() else {
        return Err(MediaError::invalid_request(format!(
            "Runway ratio width must be an integer >= 256, got '{trimmed}'"
        )));
    };
    let Ok(h) = h_str.parse::<u32>() else {
        return Err(MediaError::invalid_request(format!(
            "Runway ratio height must be an integer >= 256, got '{trimmed}'"
        )));
    };
    if w < 256 || h < 256 {
        return Err(MediaError::invalid_request(format!(
            "Runway ratio must use pixel dimensions >= 256 (e.g. '1280:720'), NOT aspect ratio '{trimmed}'"
        )));
    }
    Ok(format!("{w}:{h}"))
}

pub fn build_runway_video_payload(
    model: &str,
    req: &RunwayVideoRequest,
) -> Result<Value, MediaError> {
    let model_id = if model.trim().is_empty() {
        match req.mode {
            RunwayVideoMode::ImageToVideo => "gen4_turbo",
            RunwayVideoMode::TextToVideo => "veo3.1",
        }
    } else {
        model.trim()
    };
    let mut body = json!({
        "model": model_id,
        "_runway_mode": req.mode.as_str(),
    });
    match req.mode {
        RunwayVideoMode::ImageToVideo => {
            let img = req
                .prompt_image
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    MediaError::invalid_request("Runway image_to_video requires prompt_image")
                })?;
            body["promptImage"] = json!(img);
            if let Some(txt) = req
                .prompt_text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                body["promptText"] = json!(txt);
            }
            if let Some(r) = req.ratio.as_deref() {
                body["ratio"] = json!(validate_runway_pixel_ratio(r)?);
            }
        }
        RunwayVideoMode::TextToVideo => {
            let txt = req
                .prompt_text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    MediaError::invalid_request("Runway text_to_video requires prompt_text")
                })?;
            let ratio_raw = req.ratio.as_deref().unwrap_or("1280:720");
            body["promptText"] = json!(txt);
            body["ratio"] = json!(validate_runway_pixel_ratio(ratio_raw)?);
        }
    }
    if let Some(dur) = req.duration {
        body["duration"] = json!(dur);
    }
    if let Some(seed) = req.seed {
        body["seed"] = json!(seed);
    }
    Ok(body)
}

pub struct RunwayVideoAdapter {
    base_url: String,
    embedded_proxy: bool,
    mode: RunwayVideoMode,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
}

impl RunwayVideoAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        mode: RunwayVideoMode,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "Runway API",
        )?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (mut headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        headers.push(("X-Runway-Version".to_owned(), RUNWAY_API_VERSION.to_owned()));
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
}

impl Engine3Adapter for RunwayVideoAdapter {
    fn vendor(&self) -> &str {
        "runway"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Video
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let mut body = input.payload.clone();
        let mode = RunwayVideoMode::parse(body.get("_runway_mode").and_then(Value::as_str))
            .unwrap_or(self.mode);
        if let Some(obj) = body.as_object_mut() {
            obj.remove("_runway_mode");
        }
        let path = match mode {
            RunwayVideoMode::ImageToVideo => "/image_to_video",
            RunwayVideoMode::TextToVideo => "/text_to_video",
        };
        let url = build_openai_v1_url(&self.base_url, path, self.embedded_proxy)?;
        Ok(Engine2Request::post_json(url, body)
            .with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        let remote_id = body
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "Runway submit response missing id",
                )
            })?
            .to_owned();
        Ok(SubmitOutcome::Accepted {
            remote_task_id: remote_id,
            poll_cursor: None,
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task
            .remote_task_id
            .as_deref()
            .ok_or_else(|| MediaError::invalid_request("missing remote_task_id for Runway poll"))?;
        let clean_id = validate_path_segment("Runway task_id", remote_id)?;
        let url = build_openai_v1_url(
            &self.base_url,
            &format!("/tasks/{clean_id}"),
            self.embedded_proxy,
        )?;
        Ok(Engine2Request::get(url).with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        let status_raw = body
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("RUNNING")
            .trim()
            .to_ascii_uppercase();
        match status_raw.as_str() {
            "SUCCEEDED" => {
                let arr = body
                    .get("output")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            "Runway task SUCCEEDED without output array",
                        )
                    })?;
                let mut outputs = Vec::with_capacity(arr.len());
                for item in arr {
                    if let Some(url) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        outputs.push(MediaOutputSource::DirectDownload {
                            request: Engine2Request::get(url)
                                .with_max_response_bytes(MEDIA_DEFAULT_VIDEO_LIMIT)
                                .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS),
                            suggested_ext: "mp4".to_owned(),
                            mime_type: "video/mp4".to_owned(),
                        });
                    }
                }
                if outputs.is_empty() {
                    return Err(MediaError::new(
                        MediaErrorCode::DecodeError,
                        "Runway task SUCCEEDED with empty output URLs",
                    ));
                }
                Ok(PollOutcome::Succeeded {
                    outputs,
                    poll_cursor: None,
                })
            }
            "FAILED" => {
                let code = body
                    .get("failureCode")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let msg = body
                    .get("failure")
                    .and_then(Value::as_str)
                    .unwrap_or("Runway video generation failed")
                    .to_owned();
                Ok(PollOutcome::Failed {
                    upstream_code: code,
                    message: msg,
                    retryable: false,
                })
            }
            "CANCELLED" | "CANCELED" => Ok(PollOutcome::Cancelled {
                message: "Runway task cancelled".to_owned(),
            }),
            "THROTTLED" => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Throttled,
                poll_cursor: None,
                retry_after_hint: None,
            }),
            "PENDING" => Ok(PollOutcome::Pending {
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

    fn build_cancel(&self, task: &MediaTaskRecord) -> Result<Option<Engine2Request>, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("missing remote_task_id for Runway cancel")
        })?;
        let clean_id = validate_path_segment("Runway task_id", remote_id)?;
        let url = build_openai_v1_url(
            &self.base_url,
            &format!("/tasks/{clean_id}"),
            self.embedded_proxy,
        )?;
        Ok(Some(Engine2Request::delete(url).with_headers(
            self.headers.clone(),
            self.sensitive.clone(),
        )))
    }
}

pub async fn runway_video_generate<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &RunwayVideoRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<VideoGenerationResult, MediaError> {
    let model = if endpoint.model.trim().is_empty() {
        match req.mode {
            RunwayVideoMode::ImageToVideo => "gen4_turbo".to_owned(),
            RunwayVideoMode::TextToVideo => "veo3.1".to_owned(),
        }
    } else {
        endpoint.model.trim().to_owned()
    };
    let payload = build_runway_video_payload(&model, req)?;
    let (adapter, keys) = RunwayVideoAdapter::from_endpoint(endpoint, cred, req.mode)?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model,
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    extract_video_result(completed, "Runway")
}

// ============================================================================
// 8. New API Unified Video (/v1/videos + /content & compat /v1/video/generations)
//    Strictly NO /artifacts or /cancel!
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewApiVideoPathMode {
    #[default]
    Videos,
    VideoGenerations,
}

impl NewApiVideoPathMode {
    pub fn as_cursor(self) -> &'static str {
        match self {
            Self::Videos => "videos",
            Self::VideoGenerations => "video_generations",
        }
    }

    pub fn from_cursor(raw: Option<&str>) -> Option<Self> {
        match raw.map(str::trim) {
            Some("videos") => Some(Self::Videos),
            Some("video_generations") => Some(Self::VideoGenerations),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NewApiVideoRequest {
    pub prompt: String,
    pub path_mode: NewApiVideoPathMode,
    pub seconds: Option<String>,
    pub size: Option<String>,
    pub ratio: Option<String>,
    pub first_frame_image: Option<String>,
    pub last_frame_image: Option<String>,
}

pub fn build_newapi_video_payload(
    model: &str,
    req: &NewApiVideoRequest,
) -> Result<Value, MediaError> {
    let prompt = req.prompt.trim();
    if prompt.is_empty() {
        return Err(MediaError::invalid_request(
            "New API Video requires a non-empty prompt",
        ));
    }
    let mut body = json!({
        "prompt": prompt,
    });
    if !model.trim().is_empty() {
        body["model"] = json!(model.trim());
    }
    if let Some(sec) = req
        .seconds
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["seconds"] = json!(sec);
    }
    if let Some(sz) = req.size.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["size"] = json!(sz);
    }
    if let Some(r) = req
        .ratio
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["ratio"] = json!(r);
    }
    if let Some(ff) = req
        .first_frame_image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["first_frame_image"] = json!(ff);
    }
    if let Some(lf) = req
        .last_frame_image
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        body["last_frame_image"] = json!(lf);
    }
    Ok(body)
}

pub struct NewApiVideoAdapter {
    base_url: String,
    embedded_proxy: bool,
    path_mode: NewApiVideoPathMode,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
}

impl NewApiVideoAdapter {
    pub fn from_endpoint(
        endpoint: &MediaEndpointConfig,
        cred: &MediaCredential,
        path_mode: NewApiVideoPathMode,
    ) -> Result<(Self, CredentialSet), MediaError> {
        ensure_auth_kind(
            cred,
            endpoint,
            &[MediaAuthKind::Bearer, MediaAuthKind::ProxyToken],
            "New API Video",
        )?;
        validate_media_url(&endpoint.base_url, endpoint.embedded_proxy)?;
        let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;
        Ok((
            Self {
                base_url: endpoint.base_url.clone(),
                embedded_proxy: endpoint.embedded_proxy,
                path_mode,
                headers,
                sensitive,
            },
            keys,
        ))
    }

    fn effective_mode(&self, task: &MediaTaskRecord) -> NewApiVideoPathMode {
        NewApiVideoPathMode::from_cursor(task.poll_cursor.as_deref()).unwrap_or(self.path_mode)
    }
}

impl Engine3Adapter for NewApiVideoAdapter {
    fn vendor(&self) -> &str {
        "newapi-video"
    }

    fn kind(&self) -> MediaKind {
        MediaKind::Video
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let suffix = match self.path_mode {
            NewApiVideoPathMode::Videos => "/videos",
            NewApiVideoPathMode::VideoGenerations => "/video/generations",
        };
        let url = build_openai_v1_url(&self.base_url, suffix, self.embedded_proxy)?;
        Ok(Engine2Request::post_json(url, input.payload.clone())
            .with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        check_standard_business_status(&HeaderMap::new(), body)?;
        let remote_id = body
            .get("id")
            .or_else(|| body.get("task_id"))
            .or_else(|| body.pointer("/data/task_id"))
            .or_else(|| body.pointer("/data/id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "New API Video submit response missing id/task_id",
                )
            })?
            .to_owned();
        Ok(SubmitOutcome::Accepted {
            remote_task_id: remote_id,
            poll_cursor: Some(self.path_mode.as_cursor().to_owned()),
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let remote_id = task.remote_task_id.as_deref().ok_or_else(|| {
            MediaError::invalid_request("missing remote_task_id for New API Video poll")
        })?;
        let clean_id = validate_path_segment("New API task_id", remote_id)?;
        let suffix = match self.effective_mode(task) {
            NewApiVideoPathMode::Videos => format!("/videos/{clean_id}"),
            NewApiVideoPathMode::VideoGenerations => format!("/video/generations/{clean_id}"),
        };
        let url = build_openai_v1_url(&self.base_url, &suffix, self.embedded_proxy)?;
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
        let mode = self.effective_mode(task);
        let status_raw = body
            .get("status")
            .or_else(|| body.pointer("/data/status"))
            .and_then(Value::as_str)
            .unwrap_or("processing")
            .trim()
            .to_ascii_lowercase();

        match status_raw.as_str() {
            "succeeded" | "completed" | "success" | "succeed" | "done" => {
                let direct_url = body
                    .pointer("/content/video_url")
                    .or_else(|| body.get("video_url"))
                    .or_else(|| body.pointer("/output/video_url"))
                    .or_else(|| body.pointer("/data/video_url"))
                    .or_else(|| body.pointer("/data/output"))
                    .or_else(|| body.get("download_url"))
                    .or_else(|| body.get("url"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());

                let (dl_url, use_auth) = if let Some(u) = direct_url {
                    (u.to_owned(), false)
                } else if mode == NewApiVideoPathMode::Videos {
                    let remote_id = task.remote_task_id.as_deref().unwrap_or("");
                    let clean_id = validate_path_segment("New API task_id", remote_id)?;
                    let content_url = build_openai_v1_url(
                        &self.base_url,
                        &format!("/videos/{clean_id}/content"),
                        self.embedded_proxy,
                    )?;
                    (content_url, true)
                } else {
                    return Err(MediaError::new(
                        MediaErrorCode::DecodeError,
                        "New API Video compat task succeeded without video URL",
                    ));
                };

                let mut dl_req = Engine2Request::get(dl_url)
                    .with_max_response_bytes(MEDIA_DEFAULT_VIDEO_LIMIT)
                    .with_controlled_redirects(MEDIA_MAX_DOWNLOAD_REDIRECTS);
                if use_auth {
                    dl_req = dl_req.with_headers(self.headers.clone(), self.sensitive.clone());
                }
                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::DirectDownload {
                        request: dl_req,
                        suggested_ext: "mp4".to_owned(),
                        mime_type: "video/mp4".to_owned(),
                    }],
                    poll_cursor: Some(mode.as_cursor().to_owned()),
                })
            }
            "failed" | "failure" | "error" => {
                let msg = body
                    .pointer("/error/message")
                    .or_else(|| body.get("fail_reason"))
                    .or_else(|| body.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("New API video generation failed")
                    .to_owned();
                Ok(PollOutcome::Failed {
                    upstream_code: Some(status_raw),
                    message: msg,
                    retryable: false,
                })
            }
            "cancelled" | "canceled" => Ok(PollOutcome::Cancelled {
                message: "New API video task cancelled".to_owned(),
            }),
            "throttled" => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Throttled,
                poll_cursor: None,
                retry_after_hint: None,
            }),
            "queued" | "pending" | "not_start" | "queueing" | "preparing" => {
                Ok(PollOutcome::Pending {
                    status: MediaTaskStatus::Queued,
                    poll_cursor: None,
                    retry_after_hint: None,
                })
            }
            _ => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Running,
                poll_cursor: None,
                retry_after_hint: None,
            }),
        }
    }
}

pub async fn newapi_video_generate<R: MediaTaskRepository, A: MediaArtifactStore>(
    runner: &Engine3Runner<R, A>,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    req: &NewApiVideoRequest,
    idempotency_key: Option<String>,
    session_id: Option<String>,
    cancel: &CancellationToken,
) -> Result<VideoGenerationResult, MediaError> {
    let model = endpoint.model.trim().to_owned();
    let payload = build_newapi_video_payload(&model, req)?;
    let (adapter, keys) = NewApiVideoAdapter::from_endpoint(endpoint, cred, req.path_mode)?;
    let completed = runner
        .run_to_completion_with_outputs(
            &adapter,
            &Engine3SubmitInput {
                idempotency_key,
                session_id,
                parent_task_id: None,
                model,
                route_id: endpoint.route_id.clone(),
                payload,
            },
            &keys,
            cancel,
        )
        .await?;
    extract_video_result(completed, "New API Video")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::crypto::hmac_sha256;
    use crate::media::engine2::MediaHttpClient;
    use crate::media::engine3::Engine3PollConfig;
    use crate::media::{InMemoryMediaArtifactStore, InMemoryMediaTaskRepository};
    use axum::{
        Router,
        body::Bytes as AxumBytes,
        extract::{Path as AxumPath, Query},
        http::{HeaderMap as AxumHeaders, StatusCode},
        routing::{delete, get, post},
    };
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    use std::collections::HashMap;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::Duration;

    fn fast_poll() -> Engine3PollConfig {
        Engine3PollConfig {
            initial_interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(5),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn sora_json_multipart_remix_content_variants_and_cancel() {
        let saw_multipart = Arc::new(AtomicBool::new(false));
        let saw_mp_c = saw_multipart.clone();
        let saw_delete = Arc::new(AtomicBool::new(false));
        let saw_del_c = saw_delete.clone();

        let app = Router::new()
            .route(
                "/v1/videos",
                post(move |headers: AxumHeaders, body: AxumBytes| {
                    let saw_mp = saw_mp_c.clone();
                    async move {
                        let ct = headers
                            .get("content-type")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");
                        if ct.starts_with("multipart/form-data") {
                            saw_mp.store(true, Ordering::SeqCst);
                            let txt = String::from_utf8_lossy(&body);
                            assert!(txt.contains("name=\"input_reference\""));
                            assert!(txt.contains("name=\"seconds\"\r\n\r\n8\r\n"));
                            return (StatusCode::OK, json!({"id":"sora-mp-1"}).to_string());
                        }
                        let v: Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(v["seconds"], "4");
                        (StatusCode::OK, json!({"id":"sora-json-1"}).to_string())
                    }
                }),
            )
            .route(
                "/v1/videos/{id}/remix",
                post(|AxumPath(id): AxumPath<String>, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(id, "sora-json-1");
                    assert_eq!(body["prompt"], "add snow");
                    (StatusCode::OK, json!({"id":"sora-remix-1"}).to_string())
                }),
            )
            .route(
                "/v1/videos/{id}",
                get(|AxumPath(_id): AxumPath<String>| async move {
                    (StatusCode::OK, json!({"status":"completed"}).to_string())
                }),
            )
            .route(
                "/v1/videos/{id}",
                delete(move || {
                    saw_del_c.store(true, Ordering::SeqCst);
                    async { StatusCode::NO_CONTENT }
                }),
            )
            .route(
                "/v1/videos/{id}/content",
                get(|Query(q): Query<HashMap<String, String>>| async move {
                    let variant = q.get("variant").map(String::as_str).unwrap_or("video");
                    (StatusCode::OK, format!("SORA-{variant}").into_bytes())
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner = Engine3Runner::new(
            MediaHttpClient::new().unwrap(),
            repo.clone(),
            artifacts.clone(),
        )
        .with_poll_config(fast_poll());
        let ep = MediaEndpointConfig::new("sora", format!("http://{addr}"), "sora-2");
        let cred = MediaCredential::bearer("sk-sora").unwrap();
        let cancel = CancellationToken::new();

        // String seconds validation
        assert!(SoraSeconds::parse("6").is_err());

        let res1 = sora_generate_video(
            &runner,
            &ep,
            &cred,
            &SoraVideoRequest {
                prompt: "drone shot".into(),
                seconds: Some(SoraSeconds::Four),
                variant: SoraContentVariant::Video,
                ..Default::default()
            },
            Some("sora-k1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            artifacts.read_artifact(&res1.artifacts[0]).await.unwrap(),
            b"SORA-video".as_slice()
        );

        // Multipart input_reference keeps metadata-only in request_summary
        let res2 = sora_generate_video(
            &runner,
            &ep,
            &cred,
            &SoraVideoRequest {
                prompt: "animate".into(),
                seconds: Some(SoraSeconds::Eight),
                input_reference: Some(SoraInputReference {
                    filename: "ref.png".into(),
                    mime_type: "image/png".into(),
                    bytes: Bytes::from_static(b"PNGDATA-RAW-BYTES"),
                }),
                variant: SoraContentVariant::Thumbnail,
                ..Default::default()
            },
            Some("sora-k2".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert!(saw_multipart.load(Ordering::SeqCst));
        assert!(
            res2.task
                .request_summary
                .get("input_reference_meta")
                .is_some()
        );
        assert_eq!(
            artifacts.read_artifact(&res2.artifacts[0]).await.unwrap(),
            b"SORA-thumbnail".as_slice()
        );

        // Remix
        let remix = sora_remix_video(
            &runner,
            &ep,
            &cred,
            &SoraRemixRequest {
                video_id: "sora-json-1".into(),
                prompt: "add snow".into(),
                variant: SoraContentVariant::Spritesheet,
                parent_task_id: Some(res1.task.id.clone()),
            },
            Some("sora-k3".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            remix.task.parent_task_id.as_deref(),
            Some(res1.task.id.as_str())
        );
        assert_eq!(
            artifacts.read_artifact(&remix.artifacts[0]).await.unwrap(),
            b"SORA-spritesheet".as_slice()
        );

        // Cancel via DELETE
        let (adapter, keys) =
            SoraVideoAdapter::from_endpoint(&ep, &cred, SoraContentVariant::Video, None, None)
                .unwrap();
        let (sub, _) = runner
            .submit_and_persist(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("sora-k4".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "sora-2".into(),
                    route_id: None,
                    payload: json!({"model":"sora-2","prompt":"cancel me","seconds":"4"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        let cancelled = runner
            .cancel_task(&adapter, &sub.id, Some("abort"), &keys, &cancel)
            .await
            .unwrap();
        assert_eq!(cancelled.status, MediaTaskStatus::Cancelled);
        assert!(saw_delete.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn veo_gemini_redirect_strips_auth_and_vertex_uses_post_fetch_predict_operation() {
        let cdn_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cdn_addr = cdn_listener.local_addr().unwrap();
        let cdn_app = Router::new().route(
            "/signed/veo.mp4",
            get(|headers: AxumHeaders| async move {
                assert!(!headers.contains_key("x-goog-api-key"));
                assert!(!headers.contains_key("authorization"));
                (StatusCode::OK, b"VEO-SIGNED-MP4".to_vec())
            }),
        );
        tokio::spawn(async move { axum::serve(cdn_listener, cdn_app).await.unwrap() });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_srv = format!("http://{addr}");
        let base_c = base_srv.clone();
        let saw_vertex_post = Arc::new(AtomicBool::new(false));
        let saw_vp_c = saw_vertex_post.clone();

        let app = Router::new()
            .route(
                "/v1beta/models/{model_action}",
                post(|AxumPath(_ma): AxumPath<String>, headers: AxumHeaders| async move {
                    assert_eq!(
                        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                        Some("goog-veo-key")
                    );
                    (
                        StatusCode::OK,
                        json!({"name":"models/veo-2.0/operations/op-1"}).to_string(),
                    )
                }),
            )
            .route(
                "/v1beta/models/{m}/operations/{op}",
                get(move || {
                    let b = base_c.clone();
                    async move {
                        (
                            StatusCode::OK,
                            json!({
                                "done": true,
                                "response": {
                                    "generateVideoResponse": {
                                        "generatedSamples": [{"video": {"uri": format!("{b}/veo-dl")}}]
                                    }
                                }
                            })
                            .to_string(),
                        )
                    }
                }),
            )
            .route(
                "/veo-dl",
                get(move |headers: AxumHeaders| async move {
                    assert_eq!(
                        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                        Some("goog-veo-key")
                    );
                    let mut h = axum::http::HeaderMap::new();
                    h.insert(
                        "location",
                        format!("http://{cdn_addr}/signed/veo.mp4").parse().unwrap(),
                    );
                    (StatusCode::FOUND, h)
                }),
            )
            .route(
                "/v1/publishers/google/models/{model_action}",
                post(move |AxumPath(ma): AxumPath<String>, headers: AxumHeaders, axum::Json(body): axum::Json<Value>| {
                    let svp = saw_vp_c.clone();
                    async move {
                        assert_eq!(
                            headers.get("authorization").and_then(|v| v.to_str().ok()),
                            Some("Bearer vertex-oauth-tok")
                        );
                        if ma.ends_with(":predictLongRunning") {
                            let op = if body.pointer("/parameters/storageUri").is_some() {
                                "projects/p/locations/us/publishers/google/models/veo-2.0/operations/op-gcs"
                            } else {
                                "projects/p/locations/us/publishers/google/models/veo-2.0/operations/op-v1"
                            };
                            (StatusCode::OK, json!({"name": op}).to_string())
                        } else if ma.ends_with(":fetchPredictOperation") {
                            svp.store(true, Ordering::SeqCst);
                            let op_name = body["operationName"].as_str().unwrap();
                            if op_name.ends_with("op-gcs") {
                                (
                                    StatusCode::OK,
                                    json!({"done":true,"response":{"videos":[{"gcsUri":"gs://b/v.mp4"}]}}).to_string(),
                                )
                            } else {
                                let b64 = STANDARD.encode(b"VERTEX-INLINE-MP4");
                                (
                                    StatusCode::OK,
                                    json!({"done":true,"response":{"videos":[{"bytesBase64Encoded":b64,"mimeType":"video/mp4"}]}}).to_string(),
                                )
                            }
                        } else {
                            (StatusCode::NOT_FOUND, String::new())
                        }
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner = Engine3Runner::new(MediaHttpClient::new().unwrap(), repo, artifacts.clone())
            .with_poll_config(fast_poll());
        let cancel = CancellationToken::new();

        // Gemini Veo
        let gem_res = veo_generate_video(
            &runner,
            &MediaEndpointConfig::new("veo", &base_srv, "veo-2.0"),
            &MediaCredential::x_goog_api_key("goog-veo-key").unwrap(),
            &VeoVideoRequest {
                prompt: "timelapse".into(),
                platform: VeoPlatform::Gemini,
                ..Default::default()
            },
            Some("veo-gem-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            artifacts
                .read_artifact(&gem_res.artifacts[0])
                .await
                .unwrap(),
            b"VEO-SIGNED-MP4".as_slice()
        );

        // Vertex Veo rejects XGoogApiKey & uses POST :fetchPredictOperation
        let v_ep = MediaEndpointConfig::new("veo-vertex", &base_srv, "veo-2.0");
        assert!(
            veo_generate_video(
                &runner,
                &v_ep,
                &MediaCredential::x_goog_api_key("bad").unwrap(),
                &VeoVideoRequest {
                    prompt: "city".into(),
                    platform: VeoPlatform::Vertex,
                    ..Default::default()
                },
                None,
                None,
                &cancel,
            )
            .await
            .is_err()
        );

        let v_res = veo_generate_video(
            &runner,
            &v_ep,
            &MediaCredential::bearer("vertex-oauth-tok").unwrap(),
            &VeoVideoRequest {
                prompt: "city".into(),
                platform: VeoPlatform::Vertex,
                ..Default::default()
            },
            Some("veo-vtx-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert!(saw_vertex_post.load(Ordering::SeqCst));
        assert_eq!(
            artifacts.read_artifact(&v_res.artifacts[0]).await.unwrap(),
            b"VERTEX-INLINE-MP4".as_slice()
        );
    }

    #[tokio::test]
    async fn seedance_minimax_v1_v2_kling_wanx_runway_newapi_video_matrix() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_srv = format!("http://{addr}");
        let b1 = base_srv.clone();
        let b2 = base_srv.clone();
        let b3 = base_srv.clone();
        let b4 = base_srv.clone();
        let b4_i2v = base_srv.clone();
        let b5 = base_srv.clone();
        let b6 = base_srv.clone();
        let runway_polls = Arc::new(AtomicUsize::new(0));
        let rp_c = runway_polls.clone();
        let wanx_cancelled = Arc::new(AtomicBool::new(false));
        let wc_c = wanx_cancelled.clone();

        let app = Router::new()
            // Ark Seedance
            .route(
                "/api/v3/contents/generations/tasks",
                post(|axum::Json(body): axum::Json<Value>| async move {
                    let content = body["content"].as_array().unwrap();
                    assert_eq!(content[1]["role"], "first_frame");
                    assert_eq!(content[2]["role"], "reference_audio");
                    (StatusCode::OK, json!({"id":"ark-1"}).to_string())
                }),
            )
            .route(
                "/api/v3/contents/generations/tasks/{id}",
                get(move || {
                    let b = b1.clone();
                    async move {
                        (StatusCode::OK, json!({"status":"succeeded","content":{"video_url":format!("{b}/dl/ark.mp4")}}).to_string())
                    }
                }),
            )
            // MiniMax V1 & V2
            .route(
                "/v1/video_generation",
                post(|| async { (StatusCode::OK, json!({"base_resp":{"status_code":0},"task_id":"mm-v1-1"}).to_string()) }),
            )
            .route(
                "/v1/query/video_generation",
                get(|| async { (StatusCode::OK, json!({"base_resp":{"status_code":0},"status":"Success","file_id":"f-1"}).to_string()) }),
            )
            .route(
                "/v1/files/retrieve",
                get(move || {
                    let b = b2.clone();
                    async move {
                        (StatusCode::OK, json!({"base_resp":{"status_code":0},"file":{"download_url":format!("{b}/dl/mm1.mp4")}}).to_string())
                    }
                }),
            )
            .route(
                "/v2/video_generation",
                post(|| async { (StatusCode::OK, json!({"base_resp":{"status_code":0},"task_id":"mm-v2-1"}).to_string()) }),
            )
            .route(
                "/v2/query/video_generation/{id}",
                get(move || {
                    let b = b3.clone();
                    async move {
                        (StatusCode::OK, json!({"base_resp":{"status_code":0},"status":"succeeded","video_url":format!("{b}/dl/mm2.mp4")}).to_string())
                    }
                }),
            )
            .route(
                "/v2/video_generation/{id}",
                delete(|| async { (StatusCode::OK, json!({"base_resp":{"status_code":0}}).to_string()) }),
            )
            // Kling text2video & image2video
            .route(
                "/v1/videos/text2video",
                post(|headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                    verify_kling_jwt_header(&headers, "kling-ak", "kling-sk");
                    assert_eq!(body["duration"], "5");
                    (StatusCode::OK, json!({"code":0,"data":{"task_id":"kl-t2v"}}).to_string())
                }),
            )
            .route(
                "/v1/videos/text2video/{id}",
                get(move |headers: AxumHeaders| {
                    let b = b4.clone();
                    async move {
                        verify_kling_jwt_header(&headers, "kling-ak", "kling-sk");
                        (StatusCode::OK, json!({"code":0,"data":{"task_status":"succeed","task_result":{"videos":[{"url":format!("{b}/dl/kl.mp4")}]}}}).to_string())
                    }
                }),
            )
            .route(
                "/v1/videos/image2video",
                post(|headers: AxumHeaders| async move {
                    verify_kling_jwt_header(&headers, "kling-ak", "kling-sk");
                    (StatusCode::OK, json!({"code":0,"data":{"task_id":"kl-i2v"}}).to_string())
                }),
            )
            .route(
                "/v1/videos/image2video/{id}",
                get(move |headers: AxumHeaders| {
                    let b = b4_i2v.clone();
                    async move {
                        verify_kling_jwt_header(&headers, "kling-ak", "kling-sk");
                        (StatusCode::OK, json!({"code":0,"data":{"task_status":"succeed","task_result":{"videos":[{"url":format!("{b}/dl/kl.mp4")}]}}}).to_string())
                    }
                }),
            )
            // DashScope Wanx
            .route(
                "/api/v1/services/aigc/video-generation/video-synthesis",
                post(|headers: AxumHeaders| async move {
                    assert_eq!(
                        headers.get("x-dashscope-async").and_then(|v| v.to_str().ok()),
                        Some("enable")
                    );
                    (StatusCode::OK, json!({"output":{"task_id":"wanx-1"}}).to_string())
                }),
            )
            .route(
                "/api/v1/tasks/{id}",
                get(move || {
                    let b = b5.clone();
                    async move {
                        (StatusCode::OK, json!({"output":{"task_status":"SUCCEEDED","video_url":format!("{b}/dl/wanx.mp4")}}).to_string())
                    }
                }),
            )
            .route(
                "/api/v1/tasks/{id}/cancel",
                post(move || {
                    wc_c.store(true, Ordering::SeqCst);
                    async { (StatusCode::OK, json!({"output":{"task_status":"CANCELED"}}).to_string()) }
                }),
            )
            // Runway (THROTTLED -> SUCCEEDED)
            .route(
                "/v1/text_to_video",
                post(|headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(
                        headers.get("x-runway-version").and_then(|v| v.to_str().ok()),
                        Some(RUNWAY_API_VERSION)
                    );
                    assert_eq!(body["ratio"], "1280:720");
                    (StatusCode::OK, json!({"id":"rw-1"}).to_string())
                }),
            )
            .route(
                "/v1/tasks/{id}",
                get(move |headers: AxumHeaders| {
                    let n = rp_c.fetch_add(1, Ordering::SeqCst);
                    let b = b6.clone();
                    async move {
                        assert_eq!(
                            headers.get("x-runway-version").and_then(|v| v.to_str().ok()),
                            Some(RUNWAY_API_VERSION)
                        );
                        if n == 0 {
                            (StatusCode::OK, json!({"status":"THROTTLED"}).to_string())
                        } else {
                            (StatusCode::OK, json!({"status":"SUCCEEDED","output":[format!("{b}/dl/rw.mp4")]}).to_string())
                        }
                    }
                }),
            )
            // New API Video (/v1/videos + compat /v1/video/generations)
            .route(
                "/v1/videos",
                post(|| async { (StatusCode::OK, json!({"id":"na-1"}).to_string()) }),
            )
            .route(
                "/v1/videos/{id}",
                get(|| async { (StatusCode::OK, json!({"status":"completed"}).to_string()) }),
            )
            .route(
                "/v1/videos/{id}/content",
                get(|| async { (StatusCode::OK, b"NA-CONTENT".to_vec()) }),
            )
            .route(
                "/v1/video/generations",
                post(|| async { (StatusCode::OK, json!({"task_id":"na-c-1"}).to_string()) }),
            )
            .route(
                "/v1/video/generations/{id}",
                get(move || {
                    let b = base_srv.clone();
                    async move {
                        (StatusCode::OK, json!({"status":"succeeded","video_url":format!("{b}/dl/na2.mp4")}).to_string())
                    }
                }),
            )
            .route(
                "/dl/{name}",
                get(|AxumPath(name): AxumPath<String>| async move {
                    (StatusCode::OK, format!("VID-{name}").into_bytes())
                }),
            );

        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner = Engine3Runner::new(
            MediaHttpClient::new().unwrap(),
            repo.clone(),
            artifacts.clone(),
        )
        .with_poll_config(fast_poll());
        let cancel = CancellationToken::new();
        let bearer = MediaCredential::bearer("tok-vid").unwrap();
        let base_url = format!("http://{addr}");

        // 1. Seedance
        let sd = ark_seedance_generate(
            &runner,
            &MediaEndpointConfig::new("seedance", &base_url, "seedance-1"),
            &bearer,
            &ArkSeedanceRequest {
                prompt: "river".into(),
                media: vec![
                    ArkContentMedia {
                        url: "https://example.com/a.png".into(),
                        role: ArkContentRole::FirstFrame,
                    },
                    ArkContentMedia {
                        url: "https://example.com/b.mp3".into(),
                        role: ArkContentRole::ReferenceAudio,
                    },
                ],
                ..Default::default()
            },
            Some("sd-1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            artifacts.read_artifact(&sd.artifacts[0]).await.unwrap(),
            b"VID-ark.mp4".as_slice()
        );

        // 2. MiniMax mixed V1 + V2 resume_pending_tasks
        let mm_ep_v1 = MediaEndpointConfig::new("minimax", &base_url, "MiniMax-Hailuo-2.3");
        let mm_ep_v2 = MediaEndpointConfig::new("minimax", &base_url, "MiniMax-H3");
        let (mm_a1, mm_keys) = MiniMaxVideoAdapter::from_endpoint(&mm_ep_v1, &bearer).unwrap();
        let (mm_a2, _) = MiniMaxVideoAdapter::from_endpoint(&mm_ep_v2, &bearer).unwrap();
        let (t_v1, _) = runner
            .submit_and_persist(
                &mm_a1,
                &Engine3SubmitInput {
                    idempotency_key: Some("mm-v1-k".into()),
                    session_id: Some("sess-mm".into()),
                    parent_task_id: None,
                    model: "MiniMax-Hailuo-2.3".into(),
                    route_id: None,
                    payload: json!({"model":"MiniMax-Hailuo-2.3","prompt":"cat"}),
                },
                &mm_keys,
                &cancel,
            )
            .await
            .unwrap();
        let (t_v2, _) = runner
            .submit_and_persist(
                &mm_a2,
                &Engine3SubmitInput {
                    idempotency_key: Some("mm-v2-k".into()),
                    session_id: Some("sess-mm".into()),
                    parent_task_id: None,
                    model: "MiniMax-H3".into(),
                    route_id: None,
                    payload: json!({"model":"MiniMax-H3","prompt":"dog"}),
                },
                &mm_keys,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(t_v1.poll_cursor.as_deref(), Some("v1"));
        assert_eq!(t_v2.poll_cursor.as_deref(), Some("v2"));
        let mm_resumed = runner
            .resume_pending_tasks(&mm_a1, Some("sess-mm"), &mm_keys, &cancel)
            .await
            .unwrap();
        assert_eq!(mm_resumed.len(), 2);
        assert!(mm_resumed.iter().all(|r| r.is_ok()));

        // 3. Kling mixed text2video + image2video resume_pending_tasks
        let kl_cred = MediaCredential::kling_ak_sk("kling-ak", "kling-sk").unwrap();
        let kl_ep = MediaEndpointConfig::new("kling", &base_url, "kling-v1-6");
        let (kl_t2v, kl_keys) =
            KlingVideoAdapter::from_endpoint(&kl_ep, &kl_cred, KlingVideoMode::Text2Video).unwrap();
        let (kl_i2v, _) =
            KlingVideoAdapter::from_endpoint(&kl_ep, &kl_cred, KlingVideoMode::Image2Video)
                .unwrap();
        assert!(KlingDuration::parse("7").is_err());
        runner
            .submit_and_persist(
                &kl_t2v,
                &Engine3SubmitInput {
                    idempotency_key: Some("kl-1".into()),
                    session_id: Some("sess-kl".into()),
                    parent_task_id: None,
                    model: "kling-v1-6".into(),
                    route_id: None,
                    payload: build_kling_video_payload(
                        "kling-v1-6",
                        &KlingVideoRequest {
                            mode: KlingVideoMode::Text2Video,
                            prompt: Some("ocean".into()),
                            duration: Some(KlingDuration::Five),
                            ..Default::default()
                        },
                    )
                    .unwrap(),
                },
                &kl_keys,
                &cancel,
            )
            .await
            .unwrap();
        runner
            .submit_and_persist(
                &kl_i2v,
                &Engine3SubmitInput {
                    idempotency_key: Some("kl-2".into()),
                    session_id: Some("sess-kl".into()),
                    parent_task_id: None,
                    model: "kling-v1-6".into(),
                    route_id: None,
                    payload: build_kling_video_payload(
                        "kling-v1-6",
                        &KlingVideoRequest {
                            mode: KlingVideoMode::Image2Video,
                            image: Some("https://example.com/i.png".into()),
                            duration: Some(KlingDuration::Five),
                            ..Default::default()
                        },
                    )
                    .unwrap(),
                },
                &kl_keys,
                &cancel,
            )
            .await
            .unwrap();
        let kl_resumed = runner
            .resume_pending_tasks(&kl_t2v, Some("sess-kl"), &kl_keys, &cancel)
            .await
            .unwrap();
        assert_eq!(kl_resumed.len(), 2);
        assert!(kl_resumed.iter().all(|r| r.is_ok()));

        // 4. DashScope Wanx submit + PENDING cancel
        let wanx_ep = MediaEndpointConfig::new("wanx", &base_url, "wan2.1-t2v-turbo");
        let (wanx_ad, wanx_keys) = DashScopeWanxAdapter::from_endpoint(&wanx_ep, &bearer).unwrap();
        let (w_sub, _) = runner
            .submit_and_persist(
                &wanx_ad,
                &Engine3SubmitInput {
                    idempotency_key: Some("wx-1".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "wan2.1-t2v-turbo".into(),
                    route_id: None,
                    payload: json!({"model":"wan2.1-t2v-turbo","input":{"prompt":"snow"}}),
                },
                &wanx_keys,
                &cancel,
            )
            .await
            .unwrap();
        let w_canc = runner
            .cancel_task(&wanx_ad, &w_sub.id, Some("stop"), &wanx_keys, &cancel)
            .await
            .unwrap();
        assert_eq!(w_canc.status, MediaTaskStatus::Cancelled);
        assert!(wanx_cancelled.load(Ordering::SeqCst));

        // 5. Runway rejects 16:9 and polls through THROTTLED -> SUCCEEDED
        assert!(validate_runway_pixel_ratio("16:9").is_err());
        let rw = runway_video_generate(
            &runner,
            &MediaEndpointConfig::new("runway", &base_url, "veo3.1"),
            &bearer,
            &RunwayVideoRequest {
                mode: RunwayVideoMode::TextToVideo,
                prompt_text: Some("neon city".into()),
                ratio: Some("1280:720".into()),
                ..Default::default()
            },
            Some("rw-k1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(runway_polls.load(Ordering::SeqCst), 2);
        assert_eq!(
            artifacts.read_artifact(&rw.artifacts[0]).await.unwrap(),
            b"VID-rw.mp4".as_slice()
        );

        // 6. New API Video primary + compat
        let na_ep = MediaEndpointConfig::new("newapi-video", &base_url, "sora-2");
        let na1 = newapi_video_generate(
            &runner,
            &na_ep,
            &bearer,
            &NewApiVideoRequest {
                prompt: "forest".into(),
                path_mode: NewApiVideoPathMode::Videos,
                ..Default::default()
            },
            Some("na-k1".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            artifacts.read_artifact(&na1.artifacts[0]).await.unwrap(),
            b"NA-CONTENT".as_slice()
        );
        let na2 = newapi_video_generate(
            &runner,
            &na_ep,
            &bearer,
            &NewApiVideoRequest {
                prompt: "desert".into(),
                path_mode: NewApiVideoPathMode::VideoGenerations,
                ..Default::default()
            },
            Some("na-k2".into()),
            None,
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(
            artifacts.read_artifact(&na2.artifacts[0]).await.unwrap(),
            b"VID-na2.mp4".as_slice()
        );
    }

    fn verify_kling_jwt_header(headers: &AxumHeaders, expected_ak: &str, sk: &str) {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .expect("missing Kling Authorization header");
        let token = auth.strip_prefix("Bearer ").expect("expected Bearer JWT");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let expected_sig =
            URL_SAFE_NO_PAD.encode(hmac_sha256(sk.as_bytes(), signing_input.as_bytes()));
        assert_eq!(parts[2], expected_sig);
        let claims_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        let claims: Value = serde_json::from_slice(&claims_bytes).unwrap();
        assert_eq!(claims["iss"], expected_ak);
        let exp = claims["exp"].as_i64().unwrap();
        let nbf = claims["nbf"].as_i64().unwrap();
        assert_eq!(exp - nbf, 1805);
    }

    #[test]
    fn minimax_and_kling_parse_submit_response_track_submitted_mode_cursor() {
        let mm_ep = MediaEndpointConfig::new("minimax", "https://api.minimax.io", "");
        let (mm_ad, _) =
            MiniMaxVideoAdapter::from_endpoint(&mm_ep, &MediaCredential::bearer("k").unwrap())
                .unwrap();
        let req = mm_ad
            .build_submit(&Engine3SubmitInput {
                idempotency_key: None,
                session_id: None,
                parent_task_id: None,
                model: "MiniMax-H3".into(),
                route_id: None,
                payload: json!({"model": "MiniMax-H3", "prompt": "cat"}),
            })
            .unwrap();
        assert!(req.url.contains("/v2/video_generation"));
        let mm_out = mm_ad
            .parse_submit_response(
                200,
                &HeaderMap::new(),
                &json!({"base_resp": {"status_code": 0}, "task_id": "mm-h3-1"}),
            )
            .unwrap();
        match mm_out {
            SubmitOutcome::Accepted { poll_cursor, .. } => {
                assert_eq!(poll_cursor.as_deref(), Some("v2"));
            }
            _ => panic!("expected Accepted"),
        }

        let kl_ep =
            MediaEndpointConfig::new("kling", "https://api-beijing.klingai.com", "kling-v1-6");
        let (kl_ad, _) = KlingVideoAdapter::from_endpoint(
            &kl_ep,
            &MediaCredential::kling_ak_sk("ak", "sk").unwrap(),
            KlingVideoMode::Text2Video,
        )
        .unwrap();
        let kl_req = kl_ad
            .build_submit(&Engine3SubmitInput {
                idempotency_key: None,
                session_id: None,
                parent_task_id: None,
                model: "kling-v1-6".into(),
                route_id: None,
                payload: json!({"_kling_mode": "image2video", "image": "https://example.com/a.png"}),
            })
            .unwrap();
        assert!(kl_req.url.contains("/v1/videos/image2video"));
        let kl_out = kl_ad
            .parse_submit_response(
                200,
                &HeaderMap::new(),
                &json!({"code": 0, "data": {"task_id": "kl-i2v-1"}}),
            )
            .unwrap();
        match kl_out {
            SubmitOutcome::Accepted { poll_cursor, .. } => {
                assert_eq!(poll_cursor.as_deref(), Some("image2video"));
            }
            _ => panic!("expected Accepted"),
        }
    }
}
