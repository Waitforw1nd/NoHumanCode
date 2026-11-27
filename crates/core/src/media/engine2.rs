//! Engine 2: synchronous HTTP & streaming media transport, pure-Rust RFC 7578 multipart,
//! JSON / bytes / NDJSON / SSE decoders, transient retry, PCM->WAV & base64->artifact helpers.

use super::{MediaArtifactRef, MediaArtifactStore, MediaError, MediaErrorCode, unix_now_secs};
use crate::redaction::{CredentialSet, redact_persisted};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::{BufMut, Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use std::time::{Duration, SystemTime};
use tokio_util::sync::CancellationToken;

pub const MEDIA_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub const MEDIA_DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(120);
pub const MEDIA_ERROR_BODY_LIMIT: usize = 64 * 1024;
pub const MEDIA_DEFAULT_JSON_LIMIT: usize = 32 * 1024 * 1024;
pub const MEDIA_DEFAULT_ARTIFACT_LIMIT: usize = 100 * 1024 * 1024;
pub const MEDIA_DEFAULT_VIDEO_LIMIT: usize = 256 * 1024 * 1024;
pub const MEDIA_MAX_DOWNLOAD_REDIRECTS: u8 = 5;
pub const MEDIA_MAX_STREAM_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
enum MultipartPart {
    Text {
        name: String,
        value: String,
    },
    File {
        name: String,
        filename: String,
        content_type: String,
        data: Bytes,
    },
}

/// Pure-Rust RFC 7578 `multipart/form-data` builder supporting repeated field names,
/// exact pre-send size checks, and zero-copy retryable `Bytes` payloads.
#[derive(Clone, Debug)]
pub struct MultipartForm {
    boundary: String,
    parts: Vec<MultipartPart>,
}

impl Default for MultipartForm {
    fn default() -> Self {
        Self::new()
    }
}

impl MultipartForm {
    pub fn new() -> Self {
        Self {
            boundary: format!("----codaform{}", uuid::Uuid::new_v4().simple()),
            parts: Vec::new(),
        }
    }

    pub fn with_boundary(boundary: impl Into<String>) -> Result<Self, MediaError> {
        let boundary = boundary.into();
        validate_header_token("boundary", &boundary)?;
        if boundary.is_empty() || boundary.len() > 70 {
            return Err(MediaError::invalid_request(
                "multipart boundary length must be 1..=70",
            ));
        }
        Ok(Self {
            boundary,
            parts: Vec::new(),
        })
    }

    pub fn text(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, MediaError> {
        let name = name.into();
        validate_header_token("field name", &name)?;
        self.parts.push(MultipartPart::Text {
            name,
            value: value.into(),
        });
        Ok(self)
    }

    pub fn file(
        mut self,
        name: impl Into<String>,
        filename: impl Into<String>,
        content_type: impl Into<String>,
        data: impl Into<Bytes>,
    ) -> Result<Self, MediaError> {
        let name = name.into();
        let filename = filename.into();
        let content_type = content_type.into();
        validate_header_token("field name", &name)?;
        validate_header_token("filename", &filename)?;
        validate_header_token("content-type", &content_type)?;
        self.parts.push(MultipartPart::File {
            name,
            filename,
            content_type,
            data: data.into(),
        });
        Ok(self)
    }

    pub fn encode(&self) -> (String, Bytes) {
        let mut buf = BytesMut::new();
        for part in &self.parts {
            buf.put_slice(b"--");
            buf.put_slice(self.boundary.as_bytes());
            buf.put_slice(b"\r\n");
            match part {
                MultipartPart::Text { name, value } => {
                    buf.put_slice(b"Content-Disposition: form-data; name=\"");
                    buf.put_slice(name.as_bytes());
                    buf.put_slice(b"\"\r\n\r\n");
                    buf.put_slice(value.as_bytes());
                    buf.put_slice(b"\r\n");
                }
                MultipartPart::File {
                    name,
                    filename,
                    content_type,
                    data,
                } => {
                    buf.put_slice(b"Content-Disposition: form-data; name=\"");
                    buf.put_slice(name.as_bytes());
                    buf.put_slice(b"\"; filename=\"");
                    buf.put_slice(filename.as_bytes());
                    buf.put_slice(b"\"\r\nContent-Type: ");
                    buf.put_slice(content_type.as_bytes());
                    buf.put_slice(b"\r\n\r\n");
                    buf.put_slice(data);
                    buf.put_slice(b"\r\n");
                }
            }
        }
        buf.put_slice(b"--");
        buf.put_slice(self.boundary.as_bytes());
        buf.put_slice(b"--\r\n");

        let content_type = format!("multipart/form-data; boundary={}", self.boundary);
        (content_type, buf.freeze())
    }
}

fn validate_header_token(label: &str, value: &str) -> Result<(), MediaError> {
    if value.is_empty()
        || value
            .bytes()
            .any(|b| matches!(b, b'\r' | b'\n' | b'\0' | b'"'))
    {
        return Err(MediaError::invalid_request(format!(
            "invalid {label}: contains forbidden control or quote bytes"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum MediaRequestBody {
    Empty,
    Json(Value),
    Raw { content_type: String, bytes: Bytes },
    Multipart(MultipartForm),
}

#[derive(Clone, Debug)]
pub struct Engine2Request {
    pub method: reqwest::Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub sensitive_headers: Vec<String>,
    pub body: MediaRequestBody,
    pub max_response_bytes: usize,
    pub max_retries: u32,
    pub allow_redirects: bool,
    pub allow_json_bytes: bool,
    pub controlled_redirects: bool,
    pub max_redirects: u8,
}

impl Engine2Request {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: reqwest::Method::GET,
            url: url.into(),
            headers: Vec::new(),
            sensitive_headers: Vec::new(),
            body: MediaRequestBody::Empty,
            max_response_bytes: MEDIA_DEFAULT_JSON_LIMIT,
            max_retries: 3,
            allow_redirects: false,
            allow_json_bytes: false,
            controlled_redirects: false,
            max_redirects: MEDIA_MAX_DOWNLOAD_REDIRECTS,
        }
    }

    pub fn post_json(url: impl Into<String>, body: Value) -> Self {
        Self {
            method: reqwest::Method::POST,
            url: url.into(),
            headers: Vec::new(),
            sensitive_headers: Vec::new(),
            body: MediaRequestBody::Json(body),
            max_response_bytes: MEDIA_DEFAULT_JSON_LIMIT,
            max_retries: 3,
            allow_redirects: false,
            allow_json_bytes: false,
            controlled_redirects: false,
            max_redirects: MEDIA_MAX_DOWNLOAD_REDIRECTS,
        }
    }

    pub fn post_multipart(url: impl Into<String>, form: MultipartForm) -> Self {
        Self {
            method: reqwest::Method::POST,
            url: url.into(),
            headers: Vec::new(),
            sensitive_headers: Vec::new(),
            body: MediaRequestBody::Multipart(form),
            max_response_bytes: MEDIA_DEFAULT_JSON_LIMIT,
            max_retries: 3,
            allow_redirects: false,
            allow_json_bytes: false,
            controlled_redirects: false,
            max_redirects: MEDIA_MAX_DOWNLOAD_REDIRECTS,
        }
    }

    pub fn post_raw(
        url: impl Into<String>,
        content_type: impl Into<String>,
        bytes: impl Into<Bytes>,
    ) -> Self {
        Self {
            method: reqwest::Method::POST,
            url: url.into(),
            headers: Vec::new(),
            sensitive_headers: Vec::new(),
            body: MediaRequestBody::Raw {
                content_type: content_type.into(),
                bytes: bytes.into(),
            },
            max_response_bytes: MEDIA_DEFAULT_JSON_LIMIT,
            max_retries: 3,
            allow_redirects: false,
            allow_json_bytes: false,
            controlled_redirects: false,
            max_redirects: MEDIA_MAX_DOWNLOAD_REDIRECTS,
        }
    }

    pub fn delete(url: impl Into<String>) -> Self {
        Self {
            method: reqwest::Method::DELETE,
            url: url.into(),
            headers: Vec::new(),
            sensitive_headers: Vec::new(),
            body: MediaRequestBody::Empty,
            max_response_bytes: MEDIA_DEFAULT_JSON_LIMIT,
            max_retries: 3,
            allow_redirects: false,
            allow_json_bytes: false,
            controlled_redirects: false,
            max_redirects: MEDIA_MAX_DOWNLOAD_REDIRECTS,
        }
    }

    pub fn with_headers(
        mut self,
        headers: Vec<(String, String)>,
        sensitive_headers: Vec<String>,
    ) -> Self {
        self.headers.extend(headers);
        self.sensitive_headers.extend(sensitive_headers);
        self
    }

    pub fn with_max_response_bytes(mut self, max_bytes: usize) -> Self {
        self.max_response_bytes = max_bytes;
        self
    }

    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    pub fn with_allow_redirects(mut self, allow: bool) -> Self {
        self.allow_redirects = allow;
        self
    }

    pub fn with_allow_json_bytes(mut self, allow: bool) -> Self {
        self.allow_json_bytes = allow;
        self
    }

    pub fn with_controlled_redirects(mut self, max_redirects: u8) -> Self {
        self.controlled_redirects = true;
        self.max_redirects = max_redirects;
        self
    }

    fn collect_scrubber(&self, extra_keys: &CredentialSet) -> CredentialSet {
        let mut secrets = Vec::new();
        for (name, value) in &self.headers {
            if self
                .sensitive_headers
                .iter()
                .any(|s| s.eq_ignore_ascii_case(name))
            {
                secrets.push(value.clone());
                if let Some(token) = value.strip_prefix("Bearer ") {
                    secrets.push(token.to_owned());
                } else if let Some(token) = value.strip_prefix("Bearer;") {
                    secrets.push(token.to_owned());
                }
            }
        }
        extra_keys.merged_with(secrets)
    }
}

#[derive(Clone, Debug)]
pub struct Engine2JsonResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Value,
}

#[derive(Clone, Debug)]
pub struct Engine2BytesResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub bytes: Bytes,
}

#[derive(Clone, Debug)]
pub enum Engine2HybridResponse {
    Bytes(Engine2BytesResponse),
    Json(Engine2JsonResponse),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamControl {
    Continue,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Clone)]
pub struct MediaHttpClient {
    no_redirect_client: reqwest::Client,
    redirect_client: reqwest::Client,
}

impl MediaHttpClient {
    pub fn new() -> Result<Self, MediaError> {
        Self::with_read_timeout(MEDIA_DEFAULT_READ_TIMEOUT)
    }

    pub fn with_read_timeout(read_timeout: Duration) -> Result<Self, MediaError> {
        let no_redirect_client = reqwest::Client::builder()
            .connect_timeout(MEDIA_CONNECT_TIMEOUT)
            .read_timeout(read_timeout)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|e| MediaError::new(MediaErrorCode::Transport, e.to_string()))?;

        let redirect_client = reqwest::Client::builder()
            .connect_timeout(MEDIA_CONNECT_TIMEOUT)
            .read_timeout(read_timeout)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::limited(5))
            .retry(reqwest::retry::never())
            .build()
            .map_err(|e| MediaError::new(MediaErrorCode::Transport, e.to_string()))?;

        Ok(Self {
            no_redirect_client,
            redirect_client,
        })
    }

    pub async fn execute_json<F>(
        &self,
        req: &Engine2Request,
        keys: &CredentialSet,
        business_check: F,
        cancel: &CancellationToken,
    ) -> Result<Engine2JsonResponse, MediaError>
    where
        F: Fn(&HeaderMap, &Value) -> Result<(), MediaError>,
    {
        let merged_keys = req.collect_scrubber(keys);
        let response = self.send_with_retries(req, &merged_keys, cancel).await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = read_bounded_body(response, req.max_response_bytes, cancel)
            .await
            .map_err(|e| e.scrubbed(&merged_keys))?;
        let body: Value = serde_json::from_slice(&bytes).map_err(|e| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                format!("invalid JSON response: {e}"),
            )
            .with_status(status)
            .scrubbed(&merged_keys)
        })?;
        business_check(&headers, &body).map_err(|e| e.scrubbed(&merged_keys))?;
        Ok(Engine2JsonResponse {
            status,
            headers,
            body,
        })
    }

    pub async fn execute_optional_json<F>(
        &self,
        req: &Engine2Request,
        keys: &CredentialSet,
        business_check: F,
        cancel: &CancellationToken,
    ) -> Result<Option<Engine2JsonResponse>, MediaError>
    where
        F: Fn(&HeaderMap, &Value) -> Result<(), MediaError>,
    {
        let (status, headers, maybe_body) = self
            .execute_optional_json_with_headers(req, keys, business_check, cancel)
            .await?;
        Ok(maybe_body.map(|body| Engine2JsonResponse {
            status,
            headers,
            body,
        }))
    }

    pub async fn execute_optional_json_with_headers<F>(
        &self,
        req: &Engine2Request,
        keys: &CredentialSet,
        business_check: F,
        cancel: &CancellationToken,
    ) -> Result<(u16, HeaderMap, Option<Value>), MediaError>
    where
        F: Fn(&HeaderMap, &Value) -> Result<(), MediaError>,
    {
        let merged_keys = req.collect_scrubber(keys);
        let response = self.send_with_retries(req, &merged_keys, cancel).await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = read_bounded_body(response, req.max_response_bytes, cancel)
            .await
            .map_err(|e| e.scrubbed(&merged_keys))?;
        if status == 204 || bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok((status, headers, None));
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|e| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                format!("invalid JSON response: {e}"),
            )
            .with_status(status)
            .scrubbed(&merged_keys)
        })?;
        business_check(&headers, &body).map_err(|e| e.scrubbed(&merged_keys))?;
        Ok((status, headers, Some(body)))
    }

    pub async fn execute_hybrid<F>(
        &self,
        req: &Engine2Request,
        keys: &CredentialSet,
        business_check: F,
        cancel: &CancellationToken,
    ) -> Result<Engine2HybridResponse, MediaError>
    where
        F: Fn(&HeaderMap, &Value) -> Result<(), MediaError>,
    {
        let merged_keys = req.collect_scrubber(keys);
        let response = self.send_with_retries(req, &merged_keys, cancel).await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = read_bounded_body(response, req.max_response_bytes, cancel)
            .await
            .map_err(|e| e.scrubbed(&merged_keys))?;

        if is_json_content_type(&headers) {
            if status == 204 || bytes.iter().all(|b| b.is_ascii_whitespace()) {
                return Ok(Engine2HybridResponse::Json(Engine2JsonResponse {
                    status,
                    headers,
                    body: Value::Null,
                }));
            }
            let body: Value = serde_json::from_slice(&bytes).map_err(|e| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    format!("invalid JSON response: {e}"),
                )
                .with_status(status)
                .scrubbed(&merged_keys)
            })?;
            business_check(&headers, &body).map_err(|e| e.scrubbed(&merged_keys))?;
            return Ok(Engine2HybridResponse::Json(Engine2JsonResponse {
                status,
                headers,
                body,
            }));
        }

        Ok(Engine2HybridResponse::Bytes(Engine2BytesResponse {
            status,
            headers,
            bytes,
        }))
    }

    pub async fn execute_bytes(
        &self,
        req: &Engine2Request,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<Engine2BytesResponse, MediaError> {
        let merged_keys = req.collect_scrubber(keys);
        let response = self.send_with_retries(req, &merged_keys, cancel).await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if !req.allow_json_bytes && is_json_content_type(&headers) {
            let err_bytes = read_bounded_body(response, MEDIA_ERROR_BODY_LIMIT, cancel)
                .await
                .map_err(|e| e.scrubbed(&merged_keys))?;
            return Err(map_json_error_body(status, &err_bytes).scrubbed(&merged_keys));
        }

        let bytes = read_bounded_body(response, req.max_response_bytes, cancel)
            .await
            .map_err(|e| e.scrubbed(&merged_keys))?;
        Ok(Engine2BytesResponse {
            status,
            headers,
            bytes,
        })
    }

    pub async fn execute_bytes_with_controlled_redirects(
        &self,
        req: &Engine2Request,
        allow_http_loopback: bool,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<Engine2BytesResponse, MediaError> {
        let merged_keys = req.collect_scrubber(keys);
        let mut current_url = validate_download_hop_url(&req.url, allow_http_loopback)
            .map_err(|e| e.scrubbed(&merged_keys))?;
        let initial_origin = url_origin(&current_url);
        let max_hops = req.max_redirects.min(10);
        let mut auth_stripped = false;
        let mut hop_req = req.clone();
        hop_req.allow_redirects = false;

        for hop in 0..=max_hops {
            hop_req.url = current_url.to_string();
            if auth_stripped {
                hop_req
                    .headers
                    .retain(|(name, _)| !is_sensitive_auth_header(name, &req.sensitive_headers));
                hop_req.sensitive_headers.clear();
            }

            let response = self
                .send_single_with_retries(
                    &self.no_redirect_client,
                    &hop_req,
                    &merged_keys,
                    cancel,
                    true,
                )
                .await?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();

            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if hop >= max_hops {
                    return Err(MediaError::invalid_request(format!(
                        "media download exceeded maximum redirect hops ({max_hops})"
                    ))
                    .scrubbed(&merged_keys));
                }
                let location = headers
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            format!("redirect HTTP {status} missing Location header"),
                        )
                        .with_status(status)
                        .scrubbed(&merged_keys)
                    })?;
                let validated_next =
                    validate_redirect_hop(&current_url, location, allow_http_loopback)
                        .map_err(|e| e.scrubbed(&merged_keys))?;
                if url_origin(&validated_next) != initial_origin {
                    auth_stripped = true;
                }
                if matches!(status, 301..=303)
                    && !matches!(hop_req.method, reqwest::Method::GET | reqwest::Method::HEAD)
                {
                    hop_req.method = reqwest::Method::GET;
                    hop_req.body = MediaRequestBody::Empty;
                }
                current_url = validated_next;
                continue;
            }

            if !req.allow_json_bytes && is_json_content_type(&headers) {
                let err_bytes = read_bounded_body(response, MEDIA_ERROR_BODY_LIMIT, cancel)
                    .await
                    .map_err(|e| e.scrubbed(&merged_keys))?;
                return Err(map_json_error_body(status, &err_bytes).scrubbed(&merged_keys));
            }

            let bytes = read_bounded_body(response, req.max_response_bytes, cancel)
                .await
                .map_err(|e| e.scrubbed(&merged_keys))?;
            return Ok(Engine2BytesResponse {
                status,
                headers,
                bytes,
            });
        }

        Err(
            MediaError::invalid_request("media download exceeded maximum redirect hops")
                .scrubbed(&merged_keys),
        )
    }

    pub async fn execute_ndjson_stream<F>(
        &self,
        req: &Engine2Request,
        keys: &CredentialSet,
        mut on_item: F,
        cancel: &CancellationToken,
    ) -> Result<(), MediaError>
    where
        F: FnMut(Value) -> Result<StreamControl, MediaError>,
    {
        let merged_keys = req.collect_scrubber(keys);
        let response = self.send_with_retries(req, &merged_keys, cancel).await?;
        let status = response.status().as_u16();
        if is_json_content_type(response.headers()) {
            let err_bytes = read_bounded_body(response, MEDIA_ERROR_BODY_LIMIT, cancel)
                .await
                .map_err(|e| e.scrubbed(&merged_keys))?;
            return Err(map_json_error_body(status, &err_bytes).scrubbed(&merged_keys));
        }
        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut done = false;

        while !done {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(MediaError::new(MediaErrorCode::Cancelled, "NDJSON stream cancelled"));
                }
                next = stream.next() => next,
            };
            let Some(chunk_res) = chunk else {
                break;
            };
            let chunk = chunk_res.map_err(|e| {
                MediaError::new(MediaErrorCode::Transport, e.to_string())
                    .with_status(status)
                    .scrubbed(&merged_keys)
            })?;
            if buf.len() + chunk.len() > MEDIA_MAX_STREAM_LINE_BYTES {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    "NDJSON line exceeded maximum buffer size",
                ));
            }
            buf.extend_from_slice(&chunk);

            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                let line = std::str::from_utf8(&line_bytes[..line_bytes.len() - 1])
                    .map_err(|e| {
                        MediaError::new(MediaErrorCode::DecodeError, format!("invalid UTF-8: {e}"))
                    })?
                    .trim();
                if line.is_empty() {
                    continue;
                }
                let val: Value = serde_json::from_str(line).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("invalid NDJSON frame: {e}"),
                    )
                    .scrubbed(&merged_keys)
                })?;
                if on_item(val).map_err(|e| e.scrubbed(&merged_keys))? == StreamControl::Done {
                    done = true;
                    break;
                }
            }
        }

        if !done && !buf.is_empty() {
            let trailing = std::str::from_utf8(&buf)
                .map_err(|e| {
                    MediaError::new(MediaErrorCode::DecodeError, format!("invalid UTF-8: {e}"))
                })?
                .trim();
            if !trailing.is_empty() {
                let val: Value = serde_json::from_str(trailing).map_err(|e| {
                    MediaError::new(
                        MediaErrorCode::DecodeError,
                        format!("incomplete trailing NDJSON frame: {e}"),
                    )
                    .scrubbed(&merged_keys)
                })?;
                let _ = on_item(val).map_err(|e| e.scrubbed(&merged_keys))?;
            }
        }
        Ok(())
    }

    pub async fn execute_sse_stream<F>(
        &self,
        req: &Engine2Request,
        keys: &CredentialSet,
        mut on_event: F,
        cancel: &CancellationToken,
    ) -> Result<(), MediaError>
    where
        F: FnMut(SseEvent) -> Result<StreamControl, MediaError>,
    {
        let merged_keys = req.collect_scrubber(keys);
        let response = self.send_with_retries(req, &merged_keys, cancel).await?;
        let status = response.status().as_u16();
        if is_json_content_type(response.headers()) {
            let err_bytes = read_bounded_body(response, MEDIA_ERROR_BODY_LIMIT, cancel)
                .await
                .map_err(|e| e.scrubbed(&merged_keys))?;
            return Err(map_json_error_body(status, &err_bytes).scrubbed(&merged_keys));
        }
        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut current_event: Option<String> = None;
        let mut data_lines: Vec<String> = Vec::new();
        let mut done = false;

        while !done {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(MediaError::new(MediaErrorCode::Cancelled, "SSE stream cancelled"));
                }
                next = stream.next() => next,
            };
            let Some(chunk_res) = chunk else {
                break;
            };
            let chunk = chunk_res.map_err(|e| {
                MediaError::new(MediaErrorCode::Transport, e.to_string())
                    .with_status(status)
                    .scrubbed(&merged_keys)
            })?;
            if buf.len() + chunk.len() > MEDIA_MAX_STREAM_LINE_BYTES {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    "SSE line exceeded maximum buffer size",
                ));
            }
            buf.extend_from_slice(&chunk);

            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let raw_line: Vec<u8> = buf.drain(..=pos).collect();
                let mut line =
                    std::str::from_utf8(&raw_line[..raw_line.len() - 1]).map_err(|e| {
                        MediaError::new(MediaErrorCode::DecodeError, format!("invalid UTF-8: {e}"))
                    })?;
                line = line.strip_suffix('\r').unwrap_or(line);

                if line.is_empty() {
                    if !data_lines.is_empty() {
                        let ev = SseEvent {
                            event: current_event.take(),
                            data: data_lines.join("\n"),
                        };
                        data_lines.clear();
                        if on_event(ev).map_err(|e| e.scrubbed(&merged_keys))?
                            == StreamControl::Done
                        {
                            done = true;
                            break;
                        }
                    } else {
                        current_event = None;
                    }
                    continue;
                }
                if line.starts_with(':') {
                    continue;
                }
                if let Some(val) = line.strip_prefix("event:") {
                    current_event = Some(val.trim_start().to_owned());
                } else if let Some(val) = line.strip_prefix("data:") {
                    data_lines.push(val.strip_prefix(' ').unwrap_or(val).to_owned());
                }
            }
        }

        if !done && !data_lines.is_empty() {
            let ev = SseEvent {
                event: current_event.take(),
                data: data_lines.join("\n"),
            };
            let _ = on_event(ev).map_err(|e| e.scrubbed(&merged_keys))?;
        }
        Ok(())
    }

    async fn send_with_retries(
        &self,
        req: &Engine2Request,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, MediaError> {
        let client = if req.allow_redirects {
            &self.redirect_client
        } else {
            &self.no_redirect_client
        };
        self.send_single_with_retries(client, req, keys, cancel, false)
            .await
    }

    async fn send_single_with_retries(
        &self,
        client: &reqwest::Client,
        req: &Engine2Request,
        keys: &CredentialSet,
        cancel: &CancellationToken,
        allow_redirect_status: bool,
    ) -> Result<reqwest::Response, MediaError> {
        let prepared = prepare_body(&req.body)?;

        let mut attempt: u32 = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(MediaError::new(
                    MediaErrorCode::Cancelled,
                    "media request cancelled",
                ));
            }
            let built = build_reqwest(client, req, &prepared).map_err(|e| e.scrubbed(keys))?;
            let response = tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(MediaError::new(MediaErrorCode::Cancelled, "media request cancelled"));
                }
                res = client.execute(built) => res,
            };

            let response = match response {
                Ok(resp) => resp,
                Err(err) => {
                    let retryable = err.is_timeout() || err.is_connect();
                    let code = if err.is_timeout() {
                        MediaErrorCode::Timeout
                    } else {
                        MediaErrorCode::Transport
                    };
                    if retryable && attempt < req.max_retries {
                        let delay = backoff_delay(attempt, None);
                        attempt += 1;
                        sleep_with_cancel(delay, cancel).await?;
                        continue;
                    }
                    return Err(MediaError::new(code, err.to_string())
                        .with_retryable(retryable)
                        .scrubbed(keys));
                }
            };

            let status = response.status().as_u16();
            if response.status().is_success()
                || (allow_redirect_status && matches!(status, 301 | 302 | 303 | 307 | 308))
            {
                return Ok(response);
            }

            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| parse_retry_after(s, SystemTime::now()));

            let err_bytes = read_bounded_body(response, MEDIA_ERROR_BODY_LIMIT, cancel)
                .await
                .unwrap_or_default();
            let err = map_http_error(status, &err_bytes).scrubbed(keys);

            if is_transient_status(status) && attempt < req.max_retries {
                let delay = backoff_delay(attempt, retry_after);
                attempt += 1;
                sleep_with_cancel(delay, cancel).await?;
                continue;
            }
            return Err(err);
        }
    }
}

pub(crate) fn validate_download_hop_url(
    raw: &str,
    allow_http_loopback: bool,
) -> Result<url::Url, MediaError> {
    let url = super::validate_media_download_url(raw, allow_http_loopback)?;
    if super::is_non_loopback_private_ip(&url) {
        return Err(MediaError::invalid_request(
            "media download URL must not target private or link-local IP addresses",
        ));
    }
    if !allow_http_loopback && super::is_loopback_host(&url) {
        return Err(MediaError::invalid_request(
            "media download URL must not target loopback hosts",
        ));
    }
    Ok(url)
}

pub(crate) fn validate_redirect_hop(
    current_url: &url::Url,
    location: &str,
    allow_http_loopback: bool,
) -> Result<url::Url, MediaError> {
    let next_url = current_url
        .join(location)
        .map_err(|_| MediaError::invalid_request("invalid redirect Location URL"))?;
    if current_url.scheme() == "https" && next_url.scheme() != "https" {
        return Err(MediaError::invalid_request(
            "HTTPS to HTTP redirect downgrade is forbidden",
        ));
    }
    validate_download_hop_url(next_url.as_str(), allow_http_loopback)
}

fn url_origin(url: &url::Url) -> (String, String, Option<u16>) {
    (
        url.scheme().to_ascii_lowercase(),
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port_or_known_default(),
    )
}

fn is_sensitive_auth_header(name: &str, req_sensitive: &[String]) -> bool {
    req_sensitive.iter().any(|s| s.eq_ignore_ascii_case(name))
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "authorization"
                | "x-goog-api-key"
                | "xi-api-key"
                | "ocp-apim-subscription-key"
                | "x-api-key"
                | "x-api-app-key"
                | "x-api-access-key"
        )
}

enum PreparedBody {
    Empty,
    Bytes { content_type: String, bytes: Bytes },
}

fn prepare_body(body: &MediaRequestBody) -> Result<PreparedBody, MediaError> {
    match body {
        MediaRequestBody::Empty => Ok(PreparedBody::Empty),
        MediaRequestBody::Json(val) => {
            let bytes = serde_json::to_vec(val)
                .map_err(|e| MediaError::invalid_request(format!("failed to encode JSON: {e}")))?;
            Ok(PreparedBody::Bytes {
                content_type: "application/json".to_owned(),
                bytes: Bytes::from(bytes),
            })
        }
        MediaRequestBody::Raw {
            content_type,
            bytes,
        } => Ok(PreparedBody::Bytes {
            content_type: content_type.clone(),
            bytes: bytes.clone(),
        }),
        MediaRequestBody::Multipart(form) => {
            let (content_type, bytes) = form.encode();
            Ok(PreparedBody::Bytes {
                content_type,
                bytes,
            })
        }
    }
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            let media_type = ct
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            media_type == "application/json" || media_type.ends_with("+json")
        })
}

fn build_reqwest(
    client: &reqwest::Client,
    req: &Engine2Request,
    body: &PreparedBody,
) -> Result<reqwest::Request, MediaError> {
    let body_has_content_type = matches!(body, PreparedBody::Bytes { .. });
    let mut builder = client.request(req.method.clone(), &req.url);
    for (name, value) in &req.headers {
        if body_has_content_type && name.eq_ignore_ascii_case("content-type") {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| MediaError::invalid_request(format!("invalid header name: {name}")))?;
        let mut header_val = HeaderValue::from_str(value)
            .map_err(|_| MediaError::invalid_request(format!("invalid header value for {name}")))?;
        if req
            .sensitive_headers
            .iter()
            .any(|s| s.eq_ignore_ascii_case(name))
        {
            header_val.set_sensitive(true);
        }
        builder = builder.header(header_name, header_val);
    }
    if let PreparedBody::Bytes {
        content_type,
        bytes,
    } = body
    {
        builder = builder
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(bytes.clone());
    }
    builder
        .build()
        .map_err(|e| MediaError::invalid_request(format!("failed to build HTTP request: {e}")))
}

async fn read_bounded_body(
    response: reqwest::Response,
    limit: usize,
    cancel: &CancellationToken,
) -> Result<Bytes, MediaError> {
    let status = response.status().as_u16();
    let mut stream = response.bytes_stream();
    let mut out = BytesMut::new();
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(MediaError::new(MediaErrorCode::Cancelled, "response read cancelled"));
            }
            item = stream.next() => item,
        };
        let Some(chunk_res) = next else {
            break;
        };
        let chunk = chunk_res.map_err(|e| {
            MediaError::new(MediaErrorCode::Transport, e.to_string()).with_status(status)
        })?;
        if out.len().saturating_add(chunk.len()) > limit {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!("response body exceeded limit of {limit} bytes"),
            )
            .with_status(status));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out.freeze())
}

pub(crate) fn is_transient_status(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}

pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(Duration::from_secs(value.parse().unwrap_or(10)));
    }
    let deadline = httpdate::parse_http_date(value).ok()?;
    Some(deadline.duration_since(now).unwrap_or_default())
}

fn backoff_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(hint) = retry_after {
        return hint.min(Duration::from_secs(10));
    }
    let ms = 250u64.saturating_mul(1u64 << attempt.min(6));
    Duration::from_millis(ms.min(10_000))
}

async fn sleep_with_cancel(delay: Duration, cancel: &CancellationToken) -> Result<(), MediaError> {
    if delay.is_zero() {
        if cancel.is_cancelled() {
            return Err(MediaError::new(
                MediaErrorCode::Cancelled,
                "media request cancelled",
            ));
        }
        return Ok(());
    }
    tokio::select! {
        _ = cancel.cancelled() => Err(MediaError::new(MediaErrorCode::Cancelled, "media request cancelled")),
        _ = tokio::time::sleep(delay) => Ok(()),
    }
}

pub(crate) fn map_http_error(status: u16, body: &[u8]) -> MediaError {
    let code = match status {
        401 => MediaErrorCode::AuthRejected,
        403 => MediaErrorCode::Forbidden,
        404 => MediaErrorCode::NotFound,
        409 => MediaErrorCode::Conflict,
        429 => MediaErrorCode::RateLimited,
        400 | 422 => MediaErrorCode::InvalidRequest,
        _ => MediaErrorCode::ProviderError,
    };
    let retryable = is_transient_status(status);
    let (upstream_code, message) = extract_error_fields(status, body);
    MediaError::new(code, message)
        .with_status(status)
        .with_upstream_code(upstream_code)
        .with_retryable(retryable)
}

fn map_json_error_body(status: u16, body: &[u8]) -> MediaError {
    let (upstream_code, message) = extract_error_fields(status, body);
    MediaError::business(upstream_code, message, false).with_status(status)
}

fn extract_error_fields(status: u16, body: &[u8]) -> (Option<String>, String) {
    let raw = String::from_utf8_lossy(body);
    let mut upstream_code = None;
    let mut message = None;
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) {
        let redacted = redact_persisted(&Value::Object(map));
        upstream_code = redacted
            .pointer("/error/code")
            .or_else(|| redacted.pointer("/base_resp/status_code"))
            .or_else(|| redacted.get("code"))
            .and_then(|v| match v {
                Value::String(s) => Some(s.trim().to_owned()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty());
        message = redacted
            .pointer("/error/message")
            .or_else(|| redacted.pointer("/base_resp/status_msg"))
            .or_else(|| redacted.get("message"))
            .or_else(|| redacted.get("msg"))
            .or_else(|| redacted.get("detail"))
            .or_else(|| redacted.get("title"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
    }
    let final_msg = message.unwrap_or_else(|| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            format!("upstream HTTP {status}")
        } else {
            truncate_chars(trimmed, 512)
        }
    });
    (upstream_code, final_msg)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut iter = text.char_indices();
    match iter.nth(max_chars) {
        Some((idx, _)) => format!("{}...", &text[..idx]),
        None => text.to_owned(),
    }
}

/// Checks common vendor HTTP-200 business error envelopes (e.g., MiniMax `base_resp.status_code != 0`).
pub fn check_standard_business_status(
    _headers: &HeaderMap,
    body: &Value,
) -> Result<(), MediaError> {
    if let Some(status_code) = body
        .pointer("/base_resp/status_code")
        .and_then(Value::as_i64)
        && status_code != 0
    {
        let msg = body
            .pointer("/base_resp/status_msg")
            .and_then(Value::as_str)
            .unwrap_or("vendor business error");
        return Err(MediaError::business(
            Some(status_code.to_string()),
            msg.to_owned(),
            false,
        ));
    }
    Ok(())
}

/// Prepends an exact 44-byte RIFF/WAVE header for 16-bit mono PCM audio (e.g., Gemini TTS 24kHz).
pub fn pcm16_mono_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    build_pcm16_wav(pcm, sample_rate, 1)
}

/// Prepends an exact 44-byte RIFF/WAVE header for 16-bit PCM audio with `channels` channels
/// (e.g., Lyria RealTime 48kHz stereo PCM -> `pcm16_to_wav(&pcm, 48_000, 2)`).
pub fn pcm16_to_wav(pcm: &[u8], sample_rate: u32, channels: u16) -> Result<Vec<u8>, MediaError> {
    if sample_rate == 0 || channels == 0 {
        return Err(MediaError::invalid_request(
            "PCM WAV conversion requires sample_rate > 0 and channels >= 1",
        ));
    }
    Ok(build_pcm16_wav(pcm, sample_rate, channels))
}

fn build_pcm16_wav(pcm: &[u8], sample_rate: u32, channels: u16) -> Vec<u8> {
    let data_len = u32::try_from(pcm.len()).unwrap_or(u32::MAX);
    let riff_size = data_len.saturating_add(36);
    let block_align = channels.saturating_mul(2);
    let byte_rate = sample_rate.saturating_mul(u32::from(block_align));
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes()); // block align (channels * 16 / 8)
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultipartResponsePart {
    pub content_type: String,
    pub content_disposition: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// Pure-Rust RFC 2046 `multipart/*` response parser (used by ElevenLabs `/v1/music/detailed`).
pub fn parse_multipart_mixed(
    content_type: &str,
    body: &[u8],
) -> Result<Vec<MultipartResponsePart>, MediaError> {
    let mut segments = content_type.split(';');
    let media_type = segments.next().unwrap_or("").trim().to_ascii_lowercase();
    if !media_type.starts_with("multipart/") {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            format!("expected multipart/* Content-Type, got '{content_type}'"),
        ));
    }

    let mut boundary: Option<String> = None;
    for param in segments {
        let Some((k, v)) = param.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("boundary") {
            let trimmed = v.trim();
            let unquoted = trimmed
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(trimmed);
            if !unquoted.is_empty() && unquoted.len() <= 70 {
                boundary = Some(unquoted.to_owned());
            }
            break;
        }
    }
    let boundary = boundary.ok_or_else(|| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            "multipart response missing boundary parameter",
        )
    })?;

    let dash_boundary = format!("--{boundary}");
    let dash_bytes = dash_boundary.as_bytes();
    let start_pos = find_subslice(body, dash_bytes).ok_or_else(|| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            "multipart response missing opening boundary",
        )
    })?;
    let mut cursor = start_pos + dash_bytes.len();
    let mut parts = Vec::new();

    loop {
        if cursor >= body.len() {
            break;
        }
        if body[cursor..].starts_with(b"--") {
            break;
        }
        if body[cursor..].starts_with(b"\r\n") {
            cursor += 2;
        } else if body[cursor..].starts_with(b"\n") {
            cursor += 1;
        } else {
            return Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "malformed multipart boundary delimiter",
            ));
        }

        let (header_end, body_start) = match (
            find_subslice(&body[cursor..], b"\r\n\r\n"),
            find_subslice(&body[cursor..], b"\n\n"),
        ) {
            (Some(crlf_rel), Some(lf_rel)) => {
                if crlf_rel <= lf_rel {
                    (cursor + crlf_rel, cursor + crlf_rel + 4)
                } else {
                    (cursor + lf_rel, cursor + lf_rel + 2)
                }
            }
            (Some(crlf_rel), None) => (cursor + crlf_rel, cursor + crlf_rel + 4),
            (None, Some(lf_rel)) => (cursor + lf_rel, cursor + lf_rel + 2),
            (None, None) => {
                return Err(MediaError::new(
                    MediaErrorCode::DecodeError,
                    "multipart part missing header terminator",
                ));
            }
        };

        let header_str = std::str::from_utf8(&body[cursor..header_end]).map_err(|e| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                format!("multipart part headers are not valid UTF-8: {e}"),
            )
        })?;

        let mut headers = Vec::new();
        let mut part_ct = "application/octet-stream".to_owned();
        let mut part_disp = None;
        for line in header_str.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let Some((name, val)) = line.split_once(':') else {
                continue;
            };
            let name_trim = name.trim().to_owned();
            let val_trim = val.trim().to_owned();
            if name_trim.eq_ignore_ascii_case("content-type") {
                part_ct = val_trim.clone();
            } else if name_trim.eq_ignore_ascii_case("content-disposition") {
                part_disp = Some(val_trim.clone());
            }
            headers.push((name_trim, val_trim));
        }

        let (part_end_rel, next_cursor_rel) = find_next_boundary(&body[body_start..], dash_bytes)
            .ok_or_else(|| {
            MediaError::new(
                MediaErrorCode::DecodeError,
                "multipart part missing closing boundary",
            )
        })?;
        let part_end = body_start + part_end_rel;
        let next_cursor = body_start + next_cursor_rel;

        parts.push(MultipartResponsePart {
            content_type: part_ct,
            content_disposition: part_disp,
            headers,
            body: Bytes::copy_from_slice(&body[body_start..part_end]),
        });
        cursor = next_cursor;
    }

    if parts.is_empty() {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "multipart response contained zero parts",
        ));
    }
    Ok(parts)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn find_prefixed_subslice(haystack: &[u8], prefix: &[u8], suffix: &[u8]) -> Option<usize> {
    let total = prefix.len() + suffix.len();
    if haystack.len() < total {
        return None;
    }
    haystack
        .windows(total)
        .position(|w| &w[..prefix.len()] == prefix && &w[prefix.len()..] == suffix)
}

fn find_next_boundary(haystack: &[u8], dash_bytes: &[u8]) -> Option<(usize, usize)> {
    let mut search_from = 0usize;
    while search_from < haystack.len() {
        let crlf = find_prefixed_subslice(&haystack[search_from..], b"\r\n", dash_bytes)
            .map(|rel| (search_from + rel, 2usize));
        let lf = find_prefixed_subslice(&haystack[search_from..], b"\n", dash_bytes)
            .map(|rel| (search_from + rel, 1usize));
        let best = match (crlf, lf) {
            (Some(a), Some(b)) => {
                // When `\r\n--boundary` starts at `i`, `\n--boundary` starts at `i + 1`.
                // Pick the earlier start offset so `\r\n` is consumed cleanly.
                if a.0 <= b.0 { a } else { b }
            }
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return None,
        };
        let after = best.0 + best.1 + dash_bytes.len();
        let tail = &haystack[after..];
        if tail.is_empty()
            || tail.starts_with(b"\r\n")
            || tail.starts_with(b"\n")
            || tail.starts_with(b"--")
        {
            return Some((best.0, after));
        }
        search_from = best.0 + 1;
    }
    None
}

/// Strips an optional `data:<mime>;base64,` prefix and decodes base64 into raw bytes.
pub(crate) fn decode_base64_bytes(b64_or_data_uri: &str) -> Result<Vec<u8>, MediaError> {
    let payload = strip_data_uri_prefix(b64_or_data_uri.trim());
    STANDARD.decode(payload).map_err(|e| {
        MediaError::new(
            MediaErrorCode::DecodeError,
            format!("invalid base64 media payload: {e}"),
        )
    })
}

/// Strips an optional `data:<mime>;base64,` prefix, decodes base64, optionally wraps PCM in WAV,
/// and persists to `MediaArtifactStore`.
pub async fn decode_base64_to_artifact<A: MediaArtifactStore>(
    store: &A,
    owner_id: &str,
    b64_or_data_uri: &str,
    suggested_ext: &str,
    mime_type: &str,
    wrap_pcm_sample_rate: Option<u32>,
) -> Result<MediaArtifactRef, MediaError> {
    let decoded = decode_base64_bytes(b64_or_data_uri)?;
    let bytes = match wrap_pcm_sample_rate {
        Some(rate) => pcm16_mono_to_wav(&decoded, rate),
        None => decoded,
    };
    store
        .write_artifact(owner_id, suggested_ext, mime_type, &bytes)
        .await
}

pub(crate) fn strip_data_uri_prefix(raw: &str) -> &str {
    if let Some(rest) = raw.strip_prefix("data:")
        && let Some((_, data)) = rest.split_once(";base64,")
    {
        return data;
    }
    raw
}

/// Pure-Rust hex decoder (used by MiniMax TTS/Music `output_format: "hex"`).
pub fn decode_hex(raw: &str) -> Result<Vec<u8>, MediaError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if !trimmed.len().is_multiple_of(2) {
        return Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "invalid hex payload: odd length",
        ));
    }
    let bytes = trimmed.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, MediaError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "invalid hex character in payload",
        )),
    }
}

pub(crate) fn current_unix_i64() -> i64 {
    i64::try_from(unix_now_secs()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Bytes as AxumBytes,
        http::{HeaderMap as AxumHeaders, StatusCode},
        routing::post,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn multipart_form_supports_repeated_fields_and_blocks_header_injection() {
        assert!(MultipartForm::new().text("bad\r\n", "v").is_err());
        assert!(
            MultipartForm::new()
                .file("image[]", "a\"b.png", "image/png", vec![1u8, 2])
                .is_err()
        );

        let form = MultipartForm::with_boundary("TESTBOUNDARY")
            .unwrap()
            .text("model", "gpt-image-1")
            .unwrap()
            .file("image[]", "a.png", "image/png", vec![1u8, 2, 3])
            .unwrap()
            .file("image[]", "b.png", "image/png", vec![4u8, 5])
            .unwrap()
            .file("mask", "mask.png", "image/png", vec![6u8])
            .unwrap();
        let (ct, body) = form.encode();
        assert_eq!(ct, "multipart/form-data; boundary=TESTBOUNDARY");
        let text = String::from_utf8_lossy(&body);
        assert_eq!(text.matches("name=\"image[]\"").count(), 2);
        assert!(text.contains("name=\"mask\"; filename=\"mask.png\""));
        assert!(text.ends_with("--TESTBOUNDARY--\r\n"));
    }

    #[test]
    fn pcm16_mono_to_wav_writes_valid_44_byte_riff_header() {
        let pcm = vec![0x01u8, 0x02, 0x03, 0x04];
        let wav = pcm16_mono_to_wav(&pcm, 24_000);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 40);
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert_eq!(&wav[44..], &pcm[..]);
    }

    #[tokio::test]
    async fn engine2_handles_business_errors_retries_and_secret_scrubbing() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_clone = attempts.clone();
        let app = Router::new()
            .route(
                "/retry-then-ok",
                post(move |headers: AxumHeaders| {
                    let n = attempts_clone.fetch_add(1, Ordering::SeqCst);
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    async move {
                        if n == 0 {
                            let mut h = AxumHeaders::new();
                            h.insert("Retry-After", "0".parse().unwrap());
                            (
                                StatusCode::TOO_MANY_REQUESTS,
                                h,
                                format!(
                                    "{{\"error\":{{\"message\":\"rate limited for {auth}\"}}}}"
                                ),
                            )
                        } else {
                            (
                                StatusCode::OK,
                                AxumHeaders::new(),
                                r#"{"base_resp":{"status_code":0},"ok":true}"#.to_owned(),
                            )
                        }
                    }
                }),
            )
            .route(
                "/auth-fail",
                post(|| async {
                    (
                        StatusCode::UNAUTHORIZED,
                        r#"{"error":{"code":"invalid_key","message":"bad token sk-secret-xyz"}}"#,
                    )
                }),
            )
            .route(
                "/biz-200",
                post(|| async {
                    (
                        StatusCode::OK,
                        r#"{"base_resp":{"status_code":1004,"status_msg":"quota exceeded"}}"#,
                    )
                }),
            )
            .route(
                "/ndjson",
                post(|| async {
                    (
                        StatusCode::OK,
                        "{\"chunk\":1}\n{\"code\":20000000,\"done\":true}\n",
                    )
                }),
            )
            .route(
                "/sse",
                post(|| async {
                    (
                        StatusCode::OK,
                        "event: audio\ndata: {\"part\":1}\n\ndata: [DONE]\n\n",
                    )
                }),
            )
            .route(
                "/bytes-err",
                post(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "application/json".parse().unwrap());
                    (
                        StatusCode::OK,
                        h,
                        AxumBytes::from_static(br#"{"code":5001,"message":"gateway json error"}"#),
                    )
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = MediaHttpClient::new().unwrap();
        let cancel = CancellationToken::new();
        let keys = CredentialSet::new(vec!["sk-secret-xyz".to_owned()]);

        // 1. 429 retry + business ok
        let req = Engine2Request::post_json(
            format!("http://{addr}/retry-then-ok"),
            serde_json::json!({}),
        )
        .with_headers(
            vec![("Authorization".into(), "Bearer sk-secret-xyz".into())],
            vec!["Authorization".into()],
        );
        let ok = client
            .execute_json(&req, &keys, check_standard_business_status, &cancel)
            .await
            .unwrap();
        assert_eq!(ok.body["ok"], true);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        // 2. 401 never retries & scrubs secret
        let auth_req =
            Engine2Request::post_json(format!("http://{addr}/auth-fail"), serde_json::json!({}));
        let err = client
            .execute_json(&auth_req, &keys, check_standard_business_status, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::AuthRejected);
        assert!(!err.retryable);
        assert!(!err.to_string().contains("sk-secret-xyz"));
        assert!(err.to_string().contains("[redacted]"));

        // 3. HTTP 200 business error
        let biz_req =
            Engine2Request::post_json(format!("http://{addr}/biz-200"), serde_json::json!({}));
        let err = client
            .execute_json(&biz_req, &keys, check_standard_business_status, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::BusinessError);
        assert_eq!(err.upstream_code.as_deref(), Some("1004"));

        // 4. NDJSON stream
        let nd_req =
            Engine2Request::post_json(format!("http://{addr}/ndjson"), serde_json::json!({}));
        let mut items = Vec::new();
        client
            .execute_ndjson_stream(
                &nd_req,
                &keys,
                |v| {
                    let done = v.get("done").and_then(Value::as_bool) == Some(true);
                    items.push(v);
                    Ok(if done {
                        StreamControl::Done
                    } else {
                        StreamControl::Continue
                    })
                },
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(items.len(), 2);

        // 5. SSE stream
        let sse_req =
            Engine2Request::post_json(format!("http://{addr}/sse"), serde_json::json!({}));
        let mut events = Vec::new();
        client
            .execute_sse_stream(
                &sse_req,
                &keys,
                |ev| {
                    let is_done = ev.data == "[DONE]";
                    events.push(ev);
                    Ok(if is_done {
                        StreamControl::Done
                    } else {
                        StreamControl::Continue
                    })
                },
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_deref(), Some("audio"));

        // 6. execute_bytes detects HTTP 200 JSON error body
        let bytes_req =
            Engine2Request::post_json(format!("http://{addr}/bytes-err"), serde_json::json!({}));
        let err = client
            .execute_bytes(&bytes_req, &keys, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::BusinessError);
    }

    #[tokio::test]
    async fn engine2_problem_json_optional_json_204_and_longest_first_scrubber() {
        let app = Router::new()
            .route(
                "/problem-json",
                post(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert(
                        "content-type",
                        "application/problem+json; charset=utf-8".parse().unwrap(),
                    );
                    (
                        StatusCode::OK,
                        h,
                        AxumBytes::from_static(br#"{"title":"problem","detail":"denied"}"#),
                    )
                }),
            )
            .route(
                "/cancel-204",
                post(|| async { (StatusCode::NO_CONTENT, AxumBytes::new()) }),
            )
            .route(
                "/cancel-biz-err",
                post(|| async {
                    (
                        StatusCode::OK,
                        r#"{"base_resp":{"status_code":1004,"status_msg":"cannot cancel"}}"#,
                    )
                }),
            )
            .route(
                "/echo-bearer-err",
                post(|| async {
                    (
                        StatusCode::FORBIDDEN,
                        r#"{"error":{"message":"rejected Bearer sk-inner-tok"}}"#,
                    )
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = MediaHttpClient::new().unwrap();
        let cancel = CancellationToken::new();
        let keys = CredentialSet::new(vec!["sk-inner-tok".to_owned()]);

        // 1. execute_bytes treats application/problem+json (+json) as BusinessError
        let prob_req =
            Engine2Request::post_json(format!("http://{addr}/problem-json"), serde_json::json!({}));
        let prob_err = client
            .execute_bytes(&prob_req, &keys, &cancel)
            .await
            .unwrap_err();
        assert_eq!(prob_err.code, MediaErrorCode::BusinessError);
        assert_eq!(prob_err.message, "denied");

        // 2. execute_optional_json accepts HTTP 204 empty body as Ok(None)
        let cancel_req =
            Engine2Request::post_json(format!("http://{addr}/cancel-204"), serde_json::json!({}));
        let empty = client
            .execute_optional_json(&cancel_req, &keys, check_standard_business_status, &cancel)
            .await
            .unwrap();
        assert!(empty.is_none());

        // 3. execute_optional_json still runs business_check on 200 JSON
        let bad_cancel_req = Engine2Request::post_json(
            format!("http://{addr}/cancel-biz-err"),
            serde_json::json!({}),
        );
        let bad_cancel = client
            .execute_optional_json(
                &bad_cancel_req,
                &keys,
                check_standard_business_status,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(bad_cancel.code, MediaErrorCode::BusinessError);

        // 4. Merged longest-first CredentialSet scrubs "Bearer sk-inner-tok" as one "[redacted]"
        let bearer_req = Engine2Request::post_json(
            format!("http://{addr}/echo-bearer-err"),
            serde_json::json!({}),
        )
        .with_headers(
            vec![("Authorization".into(), "Bearer sk-inner-tok".into())],
            vec!["Authorization".into()],
        );
        let bearer_err = client
            .execute_json(&bearer_req, &keys, check_standard_business_status, &cancel)
            .await
            .unwrap_err();
        assert_eq!(bearer_err.message, "rejected [redacted]");
    }

    #[test]
    fn decode_hex_parses_even_hex_and_rejects_invalid() {
        assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_hex(" 48656c6C6f \n").unwrap(), b"Hello");
        assert!(decode_hex("abc").is_err());
        assert!(decode_hex("zz").is_err());
    }

    #[tokio::test]
    async fn engine2_hybrid_post_raw_no_duplicate_ct_and_sse_json_error() {
        let app = Router::new()
            .route(
                "/raw-ct",
                post(|headers: AxumHeaders, body: AxumBytes| async move {
                    let ct_count = headers.get_all("content-type").iter().count();
                    let ct = headers
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    (
                        StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "ct_count": ct_count,
                            "ct": ct,
                            "len": body.len(),
                        })),
                    )
                }),
            )
            .route(
                "/hybrid-audio",
                post(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "audio/mpeg".parse().unwrap());
                    (StatusCode::OK, h, AxumBytes::from_static(b"ID3-MP3"))
                }),
            )
            .route(
                "/hybrid-json-biz",
                post(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "application/json".parse().unwrap());
                    (
                        StatusCode::OK,
                        h,
                        AxumBytes::from_static(
                            br#"{"base_resp":{"status_code":1001,"status_msg":"bad"}}"#,
                        ),
                    )
                }),
            )
            .route(
                "/sse-json-200",
                post(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert("content-type", "application/json".parse().unwrap());
                    (
                        StatusCode::OK,
                        h,
                        AxumBytes::from_static(
                            br#"{"error":{"code":"quota","message":"no quota for sk-sse-secret"}}"#,
                        ),
                    )
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = MediaHttpClient::new().unwrap();
        let cancel = CancellationToken::new();
        let keys = CredentialSet::new(vec!["sk-sse-secret".to_owned()]);

        let raw_req = Engine2Request::post_raw(
            format!("http://{addr}/raw-ct"),
            "application/ssml+xml",
            b"<speak>hi</speak>".to_vec(),
        )
        .with_headers(vec![("Content-Type".into(), "text/plain".into())], vec![]);
        let raw_resp = client
            .execute_json(&raw_req, &keys, check_standard_business_status, &cancel)
            .await
            .unwrap();
        assert_eq!(raw_resp.body["ct_count"], 1);
        assert_eq!(raw_resp.body["ct"], "application/ssml+xml");
        assert_eq!(raw_resp.body["len"], 17);

        let hyb_audio = client
            .execute_hybrid(
                &Engine2Request::post_json(
                    format!("http://{addr}/hybrid-audio"),
                    serde_json::json!({}),
                ),
                &keys,
                check_standard_business_status,
                &cancel,
            )
            .await
            .unwrap();
        match hyb_audio {
            Engine2HybridResponse::Bytes(b) => assert_eq!(&b.bytes[..], b"ID3-MP3"),
            _ => panic!("expected bytes hybrid response"),
        }

        let hyb_err = client
            .execute_hybrid(
                &Engine2Request::post_json(
                    format!("http://{addr}/hybrid-json-biz"),
                    serde_json::json!({}),
                ),
                &keys,
                check_standard_business_status,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(hyb_err.code, MediaErrorCode::BusinessError);

        let sse_err = client
            .execute_sse_stream(
                &Engine2Request::post_json(
                    format!("http://{addr}/sse-json-200"),
                    serde_json::json!({}),
                ),
                &keys,
                |_| Ok(StreamControl::Continue),
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(sse_err.code, MediaErrorCode::BusinessError);
        assert!(!sse_err.message.contains("sk-sse-secret"));
        assert!(sse_err.message.contains("[redacted]"));
    }

    #[tokio::test]
    async fn execute_bytes_rejects_json_by_default_and_allows_when_opted_in() {
        let app = Router::new()
            .route(
                "/json-200",
                axum::routing::get(|| async {
                    let mut h = HeaderMap::new();
                    h.insert("content-type", "application/json".parse().unwrap());
                    (
                        StatusCode::OK,
                        h,
                        br#"{"transcripts":[{"text":"ok"}]}"#.to_vec(),
                    )
                }),
            )
            .route(
                "/json-400",
                axum::routing::get(|| async {
                    let mut h = HeaderMap::new();
                    h.insert("content-type", "application/json".parse().unwrap());
                    (
                        StatusCode::BAD_REQUEST,
                        h,
                        br#"{"error":{"message":"bad request"}}"#.to_vec(),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = MediaHttpClient::new().unwrap();
        let cancel = CancellationToken::new();
        let keys = CredentialSet::new(vec![]);

        let default_err = client
            .execute_bytes(
                &Engine2Request::get(format!("http://{addr}/json-200")),
                &keys,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(default_err.code, MediaErrorCode::BusinessError);

        let opt_in_ok = client
            .execute_bytes(
                &Engine2Request::get(format!("http://{addr}/json-200")).with_allow_json_bytes(true),
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(&opt_in_ok.bytes[..], br#"{"transcripts":[{"text":"ok"}]}"#);

        let opt_in_400 = client
            .execute_bytes(
                &Engine2Request::get(format!("http://{addr}/json-400")).with_allow_json_bytes(true),
                &keys,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(opt_in_400.code, MediaErrorCode::InvalidRequest);
    }

    #[test]
    fn pcm16_to_wav_writes_stereo_48khz_header_and_validates_params() {
        assert!(pcm16_to_wav(&[0u8; 4], 0, 2).is_err());
        assert!(pcm16_to_wav(&[0u8; 4], 48_000, 0).is_err());
        let pcm = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let wav = pcm16_to_wav(&pcm, 48_000, 2).unwrap();
        assert_eq!(wav.len(), 52);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 44);
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 192_000);
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 8);
        assert_eq!(&wav[44..], &pcm[..]);
    }

    #[test]
    fn parse_multipart_mixed_splits_json_and_audio_and_rejects_malformed() {
        let body = b"--b123\r\nContent-Type: application/json\r\n\r\n{\"title\":\"song\"}\r\n--b123\r\nContent-Type: audio/mpeg\r\nContent-Disposition: attachment; filename=\"track.mp3\"\r\n\r\nID3DATA\r\n--b123--\r\n";
        let parts = parse_multipart_mixed("multipart/mixed; boundary=\"b123\"", body).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].content_type, "application/json");
        assert_eq!(&parts[0].body[..], br#"{"title":"song"}"#);
        assert_eq!(parts[1].content_type, "audio/mpeg");
        assert_eq!(
            parts[1].content_disposition.as_deref(),
            Some("attachment; filename=\"track.mp3\"")
        );
        assert_eq!(&parts[1].body[..], b"ID3DATA");

        assert!(parse_multipart_mixed("application/json", body).is_err());
        assert!(parse_multipart_mixed("multipart/mixed", body).is_err());
        assert!(parse_multipart_mixed("multipart/mixed; boundary=b123", b"no-boundary").is_err());
    }

    #[tokio::test]
    async fn controlled_redirects_strip_auth_cross_origin_and_enforce_limits() {
        use axum::routing::get;

        let addr_b = {
            let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr_b = listener_b.local_addr().unwrap();
            let app_b = Router::new().route(
                "/signed",
                get(|headers: AxumHeaders| async move {
                    let has_key = headers.contains_key("x-goog-api-key")
                        || headers.contains_key("authorization");
                    if has_key {
                        (
                            StatusCode::BAD_REQUEST,
                            AxumBytes::from_static(b"leaked auth"),
                        )
                    } else {
                        (StatusCode::OK, AxumBytes::from_static(b"MP4VIDEO"))
                    }
                }),
            );
            tokio::spawn(async move { axum::serve(listener_b, app_b).await.unwrap() });
            addr_b
        };

        let hops = Arc::new(AtomicUsize::new(0));
        let hops_clone = hops.clone();
        let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr_a = listener_a.local_addr().unwrap();
        let app_a = Router::new()
            .route(
                "/start",
                get(move |headers: AxumHeaders| {
                    let key = headers
                        .get("x-goog-api-key")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    async move {
                        assert_eq!(key, "goog-secret-123");
                        let mut h = AxumHeaders::new();
                        h.insert("location", "/same-origin".parse().unwrap());
                        (StatusCode::FOUND, h)
                    }
                }),
            )
            .route(
                "/same-origin",
                get(move |headers: AxumHeaders| async move {
                    // Same-origin retains auth
                    assert_eq!(
                        headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()),
                        Some("goog-secret-123")
                    );
                    let mut h = AxumHeaders::new();
                    h.insert(
                        "location",
                        format!("http://{addr_b}/signed").parse().unwrap(),
                    );
                    (StatusCode::FOUND, h)
                }),
            )
            .route(
                "/loop",
                get(move || {
                    let n = hops_clone.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let mut h = AxumHeaders::new();
                        h.insert("location", format!("/loop?n={n}").parse().unwrap());
                        (StatusCode::FOUND, h)
                    }
                }),
            )
            .route(
                "/to-private",
                get(|| async {
                    let mut h = AxumHeaders::new();
                    h.insert(
                        "location",
                        "https://169.254.169.254/latest".parse().unwrap(),
                    );
                    (StatusCode::FOUND, h)
                }),
            );
        tokio::spawn(async move { axum::serve(listener_a, app_a).await.unwrap() });

        let client = MediaHttpClient::new().unwrap();
        let cancel = CancellationToken::new();
        let keys = CredentialSet::new(vec!["goog-secret-123".to_owned()]);

        let req = Engine2Request::get(format!("http://{addr_a}/start"))
            .with_headers(
                vec![("x-goog-api-key".into(), "goog-secret-123".into())],
                vec!["x-goog-api-key".into()],
            )
            .with_controlled_redirects(5);
        let resp = client
            .execute_bytes_with_controlled_redirects(&req, true, &keys, &cancel)
            .await
            .unwrap();
        assert_eq!(&resp.bytes[..], b"MP4VIDEO");

        // Exceeding max hops returns error
        let loop_req =
            Engine2Request::get(format!("http://{addr_a}/loop")).with_controlled_redirects(3);
        let loop_err = client
            .execute_bytes_with_controlled_redirects(&loop_req, true, &keys, &cancel)
            .await
            .unwrap_err();
        assert_eq!(loop_err.code, MediaErrorCode::InvalidRequest);

        // Redirect to link-local IP is blocked even when allow_http_loopback=true
        let ssrf_req =
            Engine2Request::get(format!("http://{addr_a}/to-private")).with_controlled_redirects(5);
        let ssrf_err = client
            .execute_bytes_with_controlled_redirects(&ssrf_req, true, &keys, &cancel)
            .await
            .unwrap_err();
        assert_eq!(ssrf_err.code, MediaErrorCode::InvalidRequest);
    }

    #[test]
    fn validate_download_hop_url_blocks_ipv4_mapped_and_compatible_loopback_private_and_downgrades()
    {
        // IPv4-mapped & IPv4-compatible IPv6 loopback MUST be rejected when allow_http_loopback=false
        let mapped_err =
            validate_download_hop_url("https://[::ffff:127.0.0.1]:8443/secret.mp4", false)
                .unwrap_err();
        assert_eq!(mapped_err.code, MediaErrorCode::InvalidRequest);

        let compat_err =
            validate_download_hop_url("https://[::127.0.0.1]:8443/secret.mp4", false).unwrap_err();
        assert_eq!(compat_err.code, MediaErrorCode::InvalidRequest);

        assert!(validate_download_hop_url("https://[::ffff:127.0.0.1]:8443/ok.mp4", true).is_ok());

        // IPv4-mapped & IPv4-compatible private/link-local MUST always be rejected
        for url in [
            "https://[::ffff:169.254.169.254]/latest",
            "https://[::169.254.169.254]/latest",
            "https://[::10.0.0.1]/internal",
            "https://[::ffff:10.0.0.1]/internal",
        ] {
            let err = validate_download_hop_url(url, true).unwrap_err();
            assert_eq!(err.code, MediaErrorCode::InvalidRequest, "url={url}");
        }

        // HTTPS -> HTTP redirect downgrade MUST be rejected even when allow_http_loopback=true
        let https_origin = url::Url::parse("https://cdn.example.com/start").unwrap();
        let downgrade_err =
            validate_redirect_hop(&https_origin, "http://cdn.example.com/file.mp4", true)
                .unwrap_err();
        assert_eq!(downgrade_err.code, MediaErrorCode::InvalidRequest);
        assert!(downgrade_err.message.contains("downgrade"));

        let loopback_downgrade_err =
            validate_redirect_hop(&https_origin, "http://127.0.0.1:8080/file.mp4", true)
                .unwrap_err();
        assert_eq!(loopback_downgrade_err.code, MediaErrorCode::InvalidRequest);
    }

    #[test]
    fn parse_multipart_mixed_prefers_earlier_lf_over_later_crlf_in_payload() {
        let mut body = Vec::new();
        body.extend_from_slice(
            b"--b123\nContent-Type: application/json\n\n{\"title\":\"lf\"}\n--b123\nContent-Type: audio/mpeg\n\n",
        );
        // Binary payload intentionally contains `\r\n\r\n` and `\r\n--b123` (not a boundary) and `\r\n`
        body.extend_from_slice(b"AUDIO\r\n\r\nBYTES\r\n--b123notbound");
        body.extend_from_slice(b"\n--b123--\n");

        let parts = parse_multipart_mixed("multipart/mixed; boundary=b123", &body).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].content_type, "application/json");
        assert_eq!(&parts[0].body[..], br#"{"title":"lf"}"#);
        assert_eq!(parts[1].content_type, "audio/mpeg");
        assert_eq!(&parts[1].body[..], b"AUDIO\r\n\r\nBYTES\r\n--b123notbound");
    }
}
