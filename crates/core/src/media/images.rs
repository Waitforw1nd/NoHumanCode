//! D.3 Images adapters (Engine 2):
//! - OpenAI Images (`/v1/images/generations` & multipart `/v1/images/edits`)
//! - Gemini Native Image (`:generateContent` with `responseModalities: ["TEXT", "IMAGE"]`) & Imagen (`:predict`)
//! - Volcengine Ark Seedream (`/api/v3/images/generations`, JSON `image` string/array)

use super::engine2::{
    Engine2Request, MEDIA_DEFAULT_ARTIFACT_LIMIT, MediaHttpClient, MultipartForm,
    check_standard_business_status, current_unix_i64, decode_base64_to_artifact,
    strip_data_uri_prefix,
};
use super::vectors::build_gemini_v1beta_base;
use super::{
    MediaArtifactRef, MediaArtifactStore, MediaCredential, MediaEndpointConfig, MediaError,
    MediaErrorCode, normalize_gemini_model_id, validate_media_download_url, validate_media_url,
};
use crate::redaction::CredentialSet;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub const MAX_OPENAI_EDIT_IMAGES: usize = 16;

#[derive(Clone, Debug)]
pub struct ImageInputBytes {
    pub filename: String,
    pub bytes: Bytes,
}

#[derive(Clone, Debug, Default)]
pub struct OpenAiImageRequest {
    pub prompt: String,
    pub n: Option<u32>,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub background: Option<String>,
    pub output_format: Option<String>,
    pub output_compression: Option<u8>,
    pub moderation: Option<String>,
    pub response_format: Option<String>,
    pub style: Option<String>,
    pub user: Option<String>,
    /// Empty for generations; 1..=16 items for edits.
    pub images: Vec<ImageInputBytes>,
    /// Optional PNG mask for edits.
    pub mask: Option<ImageInputBytes>,
    pub allow_http_loopback_download: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GeminiImageRequest {
    pub prompt: String,
    pub reference_images: Vec<ImageInputBytes>,
    pub aspect_ratio: Option<String>,
    pub image_size: Option<String>,
    pub sample_count: Option<u32>,
    pub person_generation: Option<String>,
    pub output_mime_type: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ArkSeedreamRequest {
    pub prompt: String,
    /// Empty = txt2img, 1 = JSON string `image`, 2+ = JSON array `image` on `/api/v3/images/generations`.
    pub reference_images: Vec<String>,
    pub size: Option<String>,
    pub response_format: Option<String>,
    pub watermark: Option<bool>,
    pub sequential_image_generation: Option<String>,
    pub sequential_max_images: Option<u32>,
    pub seed: Option<i64>,
    pub guidance_scale: Option<f64>,
    pub allow_http_loopback_download: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedImage {
    pub artifact: MediaArtifactRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageGenerationResponse {
    pub model: String,
    pub images: Vec<GeneratedImage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub texts: Vec<String>,
}

pub fn is_gpt_image_model(model: &str) -> bool {
    model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase()
        .starts_with("gpt-image-")
}

pub fn sniff_image_mime(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(("image/png", "png"))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("image/jpeg", "jpg"))
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else {
        None
    }
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    }
}

pub fn build_openai_images_url(
    base_url: &str,
    embedded_proxy: bool,
    is_edit: bool,
) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    let suffix = if is_edit {
        "images/edits"
    } else {
        "images/generations"
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

pub fn build_ark_seedream_url(base_url: &str, embedded_proxy: bool) -> Result<String, MediaError> {
    let url = validate_media_url(base_url, embedded_proxy)?;
    let trimmed = url.as_str().trim_end_matches('/');
    if embedded_proxy
        || url
            .path()
            .trim_end_matches('/')
            .to_ascii_lowercase()
            .ends_with("/api/v3")
    {
        Ok(format!("{trimmed}/images/generations"))
    } else {
        Ok(format!("{trimmed}/api/v3/images/generations"))
    }
}

pub async fn openai_image<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &OpenAiImageRequest,
    cancel: &CancellationToken,
) -> Result<ImageGenerationResponse, MediaError> {
    if endpoint.model.trim().is_empty() {
        return Err(MediaError::invalid_request("image model must not be empty"));
    }
    if req.prompt.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "image prompt must not be empty",
        ));
    }

    let is_gpt_image = is_gpt_image_model(&endpoint.model);
    if is_gpt_image && req.response_format.is_some() {
        return Err(MediaError::invalid_request(
            "gpt-image-* models do not accept response_format; use output_format instead",
        ));
    }

    let is_edit = !req.images.is_empty() || req.mask.is_some();
    if is_edit {
        if req.images.is_empty() {
            return Err(MediaError::invalid_request(
                "OpenAI image edit with mask requires at least one input image",
            ));
        }
        if req.images.len() > MAX_OPENAI_EDIT_IMAGES {
            return Err(MediaError::invalid_request(format!(
                "OpenAI image edit supports at most {MAX_OPENAI_EDIT_IMAGES} input images"
            )));
        }
        for img in &req.images {
            let Some((mime, _)) = sniff_image_mime(&img.bytes) else {
                return Err(MediaError::invalid_request(format!(
                    "unsupported input image format for '{}': expected PNG, JPEG, or WebP",
                    img.filename
                )));
            };
            if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
                return Err(MediaError::invalid_request(format!(
                    "unsupported input image format {mime} for '{}'",
                    img.filename
                )));
            }
        }
        if let Some(mask) = &req.mask
            && sniff_image_mime(&mask.bytes).map(|(m, _)| m) != Some("image/png")
        {
            return Err(MediaError::invalid_request(
                "OpenAI image edit mask must be a valid PNG",
            ));
        }
    }

    let url = build_openai_images_url(&endpoint.base_url, endpoint.embedded_proxy, is_edit)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let http_req = if is_edit {
        let mut form = MultipartForm::new().text("model", &endpoint.model)?;
        form = form.text("prompt", &req.prompt)?;
        if let Some(n) = req.n {
            form = form.text("n", n.to_string())?;
        }
        if let Some(size) = &req.size {
            form = form.text("size", size)?;
        }
        if let Some(quality) = &req.quality {
            form = form.text("quality", quality)?;
        }
        if let Some(bg) = &req.background {
            form = form.text("background", bg)?;
        }
        if let Some(ofmt) = &req.output_format {
            form = form.text("output_format", ofmt)?;
        }
        if let Some(comp) = req.output_compression {
            form = form.text("output_compression", comp.to_string())?;
        }
        if let Some(moderation) = &req.moderation {
            form = form.text("moderation", moderation)?;
        }
        if let Some(user) = &req.user {
            form = form.text("user", user)?;
        }
        if !is_gpt_image && let Some(rf) = &req.response_format {
            form = form.text("response_format", rf)?;
        }
        for img in &req.images {
            let (mime, _) = sniff_image_mime(&img.bytes).unwrap_or(("image/png", "png"));
            form = form.file("image[]", &img.filename, mime, img.bytes.clone())?;
        }
        if let Some(mask) = &req.mask {
            form = form.file("mask", &mask.filename, "image/png", mask.bytes.clone())?;
        }
        Engine2Request::post_multipart(url, form).with_headers(headers, sensitive)
    } else {
        let mut body = json!({
            "model": endpoint.model,
            "prompt": req.prompt,
        });
        if let Some(n) = req.n {
            body["n"] = json!(n);
        }
        if let Some(size) = &req.size {
            body["size"] = json!(size);
        }
        if let Some(quality) = &req.quality {
            body["quality"] = json!(quality);
        }
        if let Some(bg) = &req.background {
            body["background"] = json!(bg);
        }
        if let Some(ofmt) = &req.output_format {
            body["output_format"] = json!(ofmt);
        }
        if let Some(comp) = req.output_compression {
            body["output_compression"] = json!(comp);
        }
        if let Some(moderation) = &req.moderation {
            body["moderation"] = json!(moderation);
        }
        if let Some(style) = &req.style {
            body["style"] = json!(style);
        }
        if let Some(user) = &req.user {
            body["user"] = json!(user);
        }
        if !is_gpt_image && let Some(rf) = &req.response_format {
            body["response_format"] = json!(rf);
        }
        Engine2Request::post_json(url, body).with_headers(headers, sensitive)
    };

    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    let default_ext = req
        .output_format
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "png".to_owned());
    let images = persist_image_data_array(
        http,
        artifacts,
        owner_id,
        &resp.body,
        &default_ext,
        req.allow_http_loopback_download,
        &keys,
        cancel,
    )
    .await?;

    Ok(ImageGenerationResponse {
        model: endpoint.model.clone(),
        images,
        texts: Vec::new(),
    })
}

pub async fn gemini_generate_image<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &GeminiImageRequest,
    cancel: &CancellationToken,
) -> Result<ImageGenerationResponse, MediaError> {
    if req.prompt.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Gemini image prompt must not be empty",
        ));
    }
    let model_id = normalize_gemini_model_id(&endpoint.model)?;
    let api_base = build_gemini_v1beta_base(&endpoint.base_url, endpoint.embedded_proxy)?;
    let url = format!("{api_base}/models/{model_id}:generateContent");
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let mut parts = vec![json!({ "text": req.prompt })];
    for ref_img in &req.reference_images {
        let (mime, _) = sniff_image_mime(&ref_img.bytes).ok_or_else(|| {
            MediaError::invalid_request(format!(
                "unsupported reference image format for '{}'",
                ref_img.filename
            ))
        })?;
        parts.push(json!({
            "inlineData": {
                "mimeType": mime,
                "data": STANDARD.encode(&ref_img.bytes)
            }
        }));
    }

    let mut gen_cfg = json!({
        "responseModalities": ["TEXT", "IMAGE"]
    });
    let mut image_cfg = serde_json::Map::new();
    if let Some(ar) = &req.aspect_ratio {
        image_cfg.insert("aspectRatio".into(), json!(ar));
    }
    if let Some(sz) = &req.image_size {
        image_cfg.insert("imageSize".into(), json!(sz));
    }
    if !image_cfg.is_empty() {
        gen_cfg["imageConfig"] = Value::Object(image_cfg);
    }

    let body = json!({
        "contents": [{ "role": "user", "parts": parts }],
        "generationConfig": gen_cfg
    });

    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    if let Some(block_reason) = resp
        .body
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        return Err(MediaError::business(
            Some(block_reason.to_owned()),
            format!("Gemini image generation blocked: {block_reason}"),
            false,
        ));
    }

    let candidates = resp
        .body
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing candidates[] in Gemini image response",
            )
        })?;

    let mut texts = Vec::new();
    let mut images = Vec::new();

    for cand in candidates {
        if let Some(finish) = cand.get("finishReason").and_then(Value::as_str)
            && matches!(
                finish,
                "SAFETY" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "RECITATION" | "IMAGE_SAFETY"
            )
        {
            return Err(MediaError::business(
                Some(finish.to_owned()),
                format!("Gemini image candidate blocked with finishReason={finish}"),
                false,
            ));
        }
        if let Some(parts_arr) = cand.pointer("/content/parts").and_then(Value::as_array) {
            for part in parts_arr {
                if let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    texts.push(text.to_owned());
                }
                let inline = part.get("inlineData").or_else(|| part.get("inline_data"));
                if let Some(inline) = inline
                    && let Some(b64) = inline.get("data").and_then(Value::as_str)
                {
                    let mime = inline
                        .get("mimeType")
                        .or_else(|| inline.get("mime_type"))
                        .and_then(Value::as_str)
                        .unwrap_or("image/png");
                    let ext = extension_for_mime(mime);
                    let artifact =
                        decode_base64_to_artifact(artifacts, owner_id, b64, ext, mime, None)
                            .await?;
                    images.push(GeneratedImage {
                        artifact,
                        revised_prompt: None,
                    });
                }
            }
        }
    }

    Ok(ImageGenerationResponse {
        model: model_id.to_owned(),
        images,
        texts,
    })
}

pub async fn imagen_predict<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &GeminiImageRequest,
    cancel: &CancellationToken,
) -> Result<ImageGenerationResponse, MediaError> {
    if req.prompt.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Imagen prompt must not be empty",
        ));
    }
    let model_id = normalize_gemini_model_id(&endpoint.model)?;
    let api_base = build_gemini_v1beta_base(&endpoint.base_url, endpoint.embedded_proxy)?;
    let url = format!("{api_base}/models/{model_id}:predict");
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let mut params = serde_json::Map::new();
    if let Some(count) = req.sample_count {
        params.insert("sampleCount".into(), json!(count));
    }
    if let Some(ar) = &req.aspect_ratio {
        params.insert("aspectRatio".into(), json!(ar));
    }
    if let Some(sz) = &req.image_size {
        params.insert("imageSize".into(), json!(sz));
    }
    if let Some(pg) = &req.person_generation {
        params.insert("personGeneration".into(), json!(pg));
    }
    if let Some(mime) = &req.output_mime_type {
        params.insert("outputOptions".into(), json!({ "mimeType": mime }));
    }

    let mut body = json!({
        "instances": [{ "prompt": req.prompt }]
    });
    if !params.is_empty() {
        body["parameters"] = Value::Object(params);
    }

    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    let predictions = resp
        .body
        .get("predictions")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "missing predictions[] in Imagen response",
            )
        })?;

    let mut images = Vec::with_capacity(predictions.len());
    for pred in predictions {
        let b64 = pred
            .get("bytesBase64Encoded")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "missing bytesBase64Encoded in Imagen prediction",
                )
            })?;
        let mime = pred
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("image/png");
        let ext = extension_for_mime(mime);
        let artifact = decode_base64_to_artifact(artifacts, owner_id, b64, ext, mime, None).await?;
        images.push(GeneratedImage {
            artifact,
            revised_prompt: None,
        });
    }

    Ok(ImageGenerationResponse {
        model: model_id.to_owned(),
        images,
        texts: Vec::new(),
    })
}

pub async fn ark_seedream_generate<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    endpoint: &MediaEndpointConfig,
    cred: &MediaCredential,
    owner_id: &str,
    req: &ArkSeedreamRequest,
    cancel: &CancellationToken,
) -> Result<ImageGenerationResponse, MediaError> {
    if endpoint.model.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Ark Seedream model must not be empty",
        ));
    }
    if req.prompt.trim().is_empty() {
        return Err(MediaError::invalid_request(
            "Ark Seedream prompt must not be empty",
        ));
    }

    let url = build_ark_seedream_url(&endpoint.base_url, endpoint.embedded_proxy)?;
    let (headers, sensitive, keys) = cred.materialize_headers(current_unix_i64())?;

    let mut body = json!({
        "model": endpoint.model,
        "prompt": req.prompt,
    });
    match req.reference_images.len() {
        0 => {}
        1 => {
            body["image"] = json!(req.reference_images[0]);
        }
        _ => {
            body["image"] = json!(req.reference_images);
        }
    }
    if let Some(size) = &req.size {
        body["size"] = json!(size);
    }
    if let Some(rf) = &req.response_format {
        body["response_format"] = json!(rf);
    }
    if let Some(wm) = req.watermark {
        body["watermark"] = json!(wm);
    }
    if let Some(seq) = &req.sequential_image_generation {
        body["sequential_image_generation"] = json!(seq);
    }
    if let Some(max_imgs) = req.sequential_max_images {
        body["sequential_image_generation_options"] = json!({ "max_images": max_imgs });
    }
    if let Some(seed) = req.seed {
        body["seed"] = json!(seed);
    }
    if let Some(gs) = req.guidance_scale {
        body["guidance_scale"] = json!(gs);
    }

    let http_req = Engine2Request::post_json(url, body).with_headers(headers, sensitive);
    let resp = http
        .execute_json(&http_req, &keys, check_standard_business_status, cancel)
        .await?;

    let images = persist_image_data_array(
        http,
        artifacts,
        owner_id,
        &resp.body,
        "png",
        req.allow_http_loopback_download,
        &keys,
        cancel,
    )
    .await?;

    Ok(ImageGenerationResponse {
        model: endpoint.model.clone(),
        images,
        texts: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn persist_image_data_array<A: MediaArtifactStore>(
    http: &MediaHttpClient,
    artifacts: &A,
    owner_id: &str,
    body: &Value,
    default_ext: &str,
    allow_http_loopback_download: bool,
    keys: &CredentialSet,
    cancel: &CancellationToken,
) -> Result<Vec<GeneratedImage>, MediaError> {
    let data = body.get("data").and_then(Value::as_array).ok_or_else(|| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            "missing data[] in image generation response",
        )
    })?;

    let mut out = Vec::with_capacity(data.len());
    for item in data {
        if let Some(err_obj) = item.get("error").filter(|v| !v.is_null()) {
            let code = err_obj
                .get("code")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let msg = err_obj
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("image item generation error");
            return Err(MediaError::business(code, msg.to_owned(), false));
        }

        let revised_prompt = item
            .get("revised_prompt")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let b64_opt = item
            .get("b64_json")
            .or_else(|| item.get("b64"))
            .or_else(|| item.get("base64"))
            .and_then(Value::as_str);

        let artifact = if let Some(raw_b64) = b64_opt {
            let clean_b64 = strip_data_uri_prefix(raw_b64.trim());
            let decoded = STANDARD.decode(clean_b64).map_err(|e| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    format!("invalid base64 image payload: {e}"),
                )
            })?;
            let (mime, ext) = sniff_image_mime(&decoded)
                .unwrap_or_else(|| (mime_for_ext(default_ext), default_ext_str(default_ext)));
            artifacts
                .write_artifact(owner_id, ext, mime, &decoded)
                .await?
        } else if let Some(url_str) = item.get("url").and_then(Value::as_str) {
            let validated = validate_media_download_url(url_str, allow_http_loopback_download)?;
            let dl_req = Engine2Request::get(validated.to_string())
                .with_max_response_bytes(MEDIA_DEFAULT_ARTIFACT_LIMIT);
            let dl = http.execute_bytes(&dl_req, keys, cancel).await?;
            let (mime, ext) = sniff_image_mime(&dl.bytes).unwrap_or_else(|| {
                let header_ct = dl
                    .headers
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("image/png");
                let ext = extension_for_mime(header_ct);
                (mime_for_ext(ext), ext)
            });
            artifacts
                .write_artifact(owner_id, ext, mime, &dl.bytes)
                .await?
        } else {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "image response item missing both b64_json and url",
            ));
        };

        out.push(GeneratedImage {
            artifact,
            revised_prompt,
        });
    }

    Ok(out)
}

fn default_ext_str(ext: &str) -> &str {
    match ext {
        "png" | "jpg" | "jpeg" | "webp" | "gif" => ext,
        _ => "png",
    }
}

fn mime_for_ext(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/png",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::InMemoryMediaArtifactStore;
    use axum::{
        Router,
        body::Bytes as AxumBytes,
        http::{HeaderMap as AxumHeaders, StatusCode},
        routing::{get, post},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    const TINY_PNG: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00];
    const TINY_JPG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];

    #[tokio::test]
    async fn openai_gpt_image_omits_response_format_and_dalle_downloads_url() {
        let saw_gpt_image = Arc::new(AtomicBool::new(false));
        let saw_clone = saw_gpt_image.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let app = Router::new()
            .route(
                "/v1/images/generations",
                post(move |headers: AxumHeaders, axum::Json(body): axum::Json<Value>| {
                    let flag = saw_clone.clone();
                    async move {
                        assert_eq!(
                            headers.get("authorization").and_then(|v| v.to_str().ok()),
                            Some("Bearer sk-img")
                        );
                        let model = body["model"].as_str().unwrap();
                        if model.starts_with("gpt-image-") {
                            assert!(
                                body.get("response_format").is_none(),
                                "gpt-image-* MUST NOT emit response_format"
                            );
                            assert_eq!(body["output_format"], "png");
                            assert_eq!(body["background"], "transparent");
                            assert_eq!(body["quality"], "high");
                            flag.store(true, Ordering::SeqCst);
                            return (
                                StatusCode::OK,
                                axum::Json(json!({
                                    "data": [{"b64_json": STANDARD.encode(TINY_PNG), "revised_prompt": "clean"}]
                                })),
                            );
                        }
                        assert_eq!(body["response_format"], "url");
                        (
                            StatusCode::OK,
                            axum::Json(json!({
                                "data": [{"url": format!("http://{addr}/files/dalle.png")}]
                            })),
                        )
                    }
                }),
            )
            .route(
                "/files/dalle.png",
                get(|| async { (StatusCode::OK, TINY_PNG.to_vec()) }),
            );

        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let store = InMemoryMediaArtifactStore::new();
        let cred = MediaCredential::bearer("sk-img").unwrap();
        let cancel = CancellationToken::new();

        // Pre-flight rejects explicit response_format for gpt-image-1.5
        let ep_gpt = MediaEndpointConfig::new("openai", format!("http://{addr}"), "gpt-image-1.5");
        let rejected = openai_image(
            &http,
            &store,
            &ep_gpt,
            &cred,
            "owner-1",
            &OpenAiImageRequest {
                prompt: "cat".into(),
                response_format: Some("url".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap_err();
        assert_eq!(rejected.code, MediaErrorCode::InvalidRequest);

        // Valid gpt-image-1 generation
        let ep_gpt1 = MediaEndpointConfig::new("openai", format!("http://{addr}"), "gpt-image-1");
        let res = openai_image(
            &http,
            &store,
            &ep_gpt1,
            &cred,
            "owner-1",
            &OpenAiImageRequest {
                prompt: "logo".into(),
                output_format: Some("png".into()),
                background: Some("transparent".into()),
                quality: Some("high".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert!(saw_gpt_image.load(Ordering::SeqCst));
        assert_eq!(res.images.len(), 1);
        assert_eq!(res.images[0].artifact.mime_type, "image/png");
        assert_eq!(res.images[0].artifact.size_bytes, TINY_PNG.len() as u64);

        // Legacy dall-e-3 allows response_format: "url" and downloads artifact
        let ep_dalle = MediaEndpointConfig::new("openai", format!("http://{addr}"), "dall-e-3");
        let res2 = openai_image(
            &http,
            &store,
            &ep_dalle,
            &cred,
            "owner-2",
            &OpenAiImageRequest {
                prompt: "sunset".into(),
                response_format: Some("url".into()),
                allow_http_loopback_download: true,
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(res2.images.len(), 1);
        assert_eq!(res2.images[0].artifact.size_bytes, TINY_PNG.len() as u64);
    }

    #[tokio::test]
    async fn openai_edits_multipart_repeated_image_array_and_mask() {
        let app = Router::new().route(
            "/v1/images/edits",
            post(|headers: AxumHeaders, body: AxumBytes| async move {
                let ct = headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap();
                assert!(ct.starts_with("multipart/form-data; boundary="));
                let raw = String::from_utf8_lossy(&body);
                assert_eq!(raw.matches("name=\"image[]\"").count(), 2);
                assert!(raw.contains("name=\"mask\"; filename=\"mask.png\""));
                assert!(!raw.contains("name=\"response_format\""));
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
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let store = InMemoryMediaArtifactStore::new();
        let ep = MediaEndpointConfig::new("openai", format!("http://{addr}"), "gpt-image-1");
        let cred = MediaCredential::bearer("sk-edit").unwrap();
        let cancel = CancellationToken::new();

        // Reject > 16 images
        let too_many: Vec<ImageInputBytes> = (0..17)
            .map(|i| ImageInputBytes {
                filename: format!("img-{i}.png"),
                bytes: Bytes::from_static(TINY_PNG),
            })
            .collect();
        assert_eq!(
            openai_image(
                &http,
                &store,
                &ep,
                &cred,
                "owner",
                &OpenAiImageRequest {
                    prompt: "edit".into(),
                    images: too_many,
                    ..Default::default()
                },
                &cancel,
            )
            .await
            .unwrap_err()
            .code,
            MediaErrorCode::InvalidRequest
        );

        // Reject non-PNG mask
        assert_eq!(
            openai_image(
                &http,
                &store,
                &ep,
                &cred,
                "owner",
                &OpenAiImageRequest {
                    prompt: "edit".into(),
                    images: vec![ImageInputBytes {
                        filename: "a.png".into(),
                        bytes: Bytes::from_static(TINY_PNG),
                    }],
                    mask: Some(ImageInputBytes {
                        filename: "mask.jpg".into(),
                        bytes: Bytes::from_static(TINY_JPG),
                    }),
                    ..Default::default()
                },
                &cancel,
            )
            .await
            .unwrap_err()
            .code,
            MediaErrorCode::InvalidRequest
        );

        // Valid 2-image + PNG mask edit
        let res = openai_image(
            &http,
            &store,
            &ep,
            &cred,
            "owner",
            &OpenAiImageRequest {
                prompt: "combine".into(),
                images: vec![
                    ImageInputBytes {
                        filename: "a.png".into(),
                        bytes: Bytes::from_static(TINY_PNG),
                    },
                    ImageInputBytes {
                        filename: "b.jpg".into(),
                        bytes: Bytes::from_static(TINY_JPG),
                    },
                ],
                mask: Some(ImageInputBytes {
                    filename: "mask.png".into(),
                    bytes: Bytes::from_static(TINY_PNG),
                }),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(res.images.len(), 1);
    }

    #[tokio::test]
    async fn gemini_native_image_imagen_and_ark_seedream() {
        let app = Router::new()
            .route(
                "/v1beta/models/gemini-2.5-flash-image:generateContent",
                post(|headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(
                        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                        Some("goog-img")
                    );
                    assert_eq!(
                        body["generationConfig"]["responseModalities"],
                        json!(["TEXT", "IMAGE"])
                    );
                    assert_eq!(body["generationConfig"]["imageConfig"]["aspectRatio"], "16:9");
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "candidates": [{
                                "content": {
                                    "parts": [
                                        {"text": "Here is your image"},
                                        {"inlineData": {"mimeType": "image/png", "data": STANDARD.encode(TINY_PNG)}}
                                    ]
                                },
                                "finishReason": "STOP"
                            }]
                        })),
                    )
                }),
            )
            .route(
                "/v1beta/models/imagen-3.0-generate-002:predict",
                post(|headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(
                        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                        Some("goog-img")
                    );
                    assert_eq!(body["instances"][0]["prompt"], "mountain");
                    assert_eq!(body["parameters"]["sampleCount"], 1);
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "predictions": [{
                                "bytesBase64Encoded": STANDARD.encode(TINY_PNG),
                                "mimeType": "image/png"
                            }]
                        })),
                    )
                }),
            )
            .route(
                "/api/v3/images/generations",
                post(|headers: AxumHeaders, axum::Json(body): axum::Json<Value>| async move {
                    assert_eq!(
                        headers.get("authorization").and_then(|v| v.to_str().ok()),
                        Some("Bearer ark-key")
                    );
                    assert_eq!(body["size"], "2K");
                    assert_eq!(body["watermark"], false);
                    // Multi-ref sends JSON array `image`
                    assert!(body["image"].is_array());
                    assert_eq!(body["image"].as_array().unwrap().len(), 2);
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
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = MediaHttpClient::new().unwrap();
        let store = InMemoryMediaArtifactStore::new();
        let cancel = CancellationToken::new();

        // Gemini Native Image
        let g_cred = MediaCredential::x_goog_api_key("goog-img").unwrap();
        let g_res = gemini_generate_image(
            &http,
            &store,
            &MediaEndpointConfig::new("gemini", format!("http://{addr}"), "gemini-2.5-flash-image"),
            &g_cred,
            "gem-owner",
            &GeminiImageRequest {
                prompt: "draw".into(),
                aspect_ratio: Some("16:9".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(g_res.texts, vec!["Here is your image"]);
        assert_eq!(g_res.images.len(), 1);

        // Imagen :predict
        let im_res = imagen_predict(
            &http,
            &store,
            &MediaEndpointConfig::new(
                "gemini",
                format!("http://{addr}"),
                "imagen-3.0-generate-002",
            ),
            &g_cred,
            "im-owner",
            &GeminiImageRequest {
                prompt: "mountain".into(),
                sample_count: Some(1),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(im_res.images.len(), 1);

        // Volcengine Ark Seedream (/api/v3/images/generations)
        let ark_cred = MediaCredential::bearer("ark-key").unwrap();
        let ark_res = ark_seedream_generate(
            &http,
            &store,
            &MediaEndpointConfig::new("ark", format!("http://{addr}"), "doubao-seedream-3-0"),
            &ark_cred,
            "ark-owner",
            &ArkSeedreamRequest {
                prompt: "style transfer".into(),
                reference_images: vec![
                    "https://example.com/1.png".into(),
                    "https://example.com/2.png".into(),
                ],
                size: Some("2K".into()),
                response_format: Some("b64_json".into()),
                watermark: Some(false),
                sequential_image_generation: Some("disabled".into()),
                ..Default::default()
            },
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(ark_res.images.len(), 1);
    }
}
