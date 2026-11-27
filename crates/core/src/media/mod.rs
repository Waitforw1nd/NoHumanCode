//! C11-clean pure media protocol domain types, validation helpers, and trait seams.

pub mod crypto;
pub mod engine2;
pub mod engine3;
pub mod images;
pub mod music;
pub mod realtime;
pub mod stt;
pub mod tts;
pub mod vectors;
pub mod video;
pub mod ws;

use crate::redaction::{
    CredentialSet, redact_persisted, validate_idempotency_key, validate_persisted_id,
};
use coda_protocol::{ErrorBody, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Embedding,
    Rerank,
    Image,
    Tts,
    Stt,
    Music,
    Video,
    Realtime,
}

impl MediaKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedding => "embedding",
            Self::Rerank => "rerank",
            Self::Image => "image",
            Self::Tts => "tts",
            Self::Stt => "stt",
            Self::Music => "music",
            Self::Video => "video",
            Self::Realtime => "realtime",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "embedding" => Some(Self::Embedding),
            "rerank" => Some(Self::Rerank),
            "image" => Some(Self::Image),
            "tts" => Some(Self::Tts),
            "stt" => Some(Self::Stt),
            "music" => Some(Self::Music),
            "video" => Some(Self::Video),
            "realtime" => Some(Self::Realtime),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaTaskStatus {
    #[default]
    Queued,
    Submitted,
    Running,
    Throttled,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

impl MediaTaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Submitted => "submitted",
            Self::Running => "running",
            Self::Throttled => "throttled",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "queued" => Some(Self::Queued),
            "submitted" => Some(Self::Submitted),
            "running" => Some(Self::Running),
            "throttled" => Some(Self::Throttled),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    pub const fn is_resumable(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Submitted | Self::Running | Self::Throttled
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaArtifactRef {
    pub path: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaTaskRecord {
    pub id: String,
    pub idempotency_key: Option<String>,
    pub request_hash: String,
    pub session_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub media_kind: MediaKind,
    pub vendor: String,
    pub model: String,
    pub route_id: Option<String>,
    pub status: MediaTaskStatus,
    pub remote_task_id: Option<String>,
    pub poll_cursor: Option<String>,
    pub request_summary: Value,
    pub artifacts: Vec<MediaArtifactRef>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewMediaTask {
    pub id: String,
    pub idempotency_key: Option<String>,
    pub request_hash: String,
    pub session_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub media_kind: MediaKind,
    pub vendor: String,
    pub model: String,
    pub route_id: Option<String>,
    pub status: MediaTaskStatus,
    pub remote_task_id: Option<String>,
    pub poll_cursor: Option<String>,
    pub request_summary: Value,
    pub artifacts: Vec<MediaArtifactRef>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl From<NewMediaTask> for MediaTaskRecord {
    fn from(value: NewMediaTask) -> Self {
        Self {
            id: value.id,
            idempotency_key: value.idempotency_key,
            request_hash: value.request_hash,
            session_id: value.session_id,
            parent_task_id: value.parent_task_id,
            media_kind: value.media_kind,
            vendor: value.vendor,
            model: value.model,
            route_id: value.route_id,
            status: value.status,
            remote_task_id: value.remote_task_id,
            poll_cursor: value.poll_cursor,
            request_summary: value.request_summary,
            artifacts: value.artifacts,
            error_code: value.error_code,
            error_message: value.error_message,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaTaskUpdate {
    pub status: MediaTaskStatus,
    pub remote_task_id: Option<String>,
    pub poll_cursor: Option<String>,
    pub artifacts: Option<Vec<MediaArtifactRef>>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub updated_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateMediaTaskResult {
    Created(MediaTaskRecord),
    Replayed(MediaTaskRecord),
    IdempotencyConflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaErrorCode {
    InvalidRequest,
    AuthRejected,
    Forbidden,
    Conflict,
    NotFound,
    RateLimited,
    ProviderError,
    BusinessError,
    DecodeError,
    Timeout,
    Cancelled,
    ArtifactWriteFailed,
    PersistenceFailed,
    Transport,
}

impl MediaErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::AuthRejected => "auth_rejected",
            Self::Forbidden => "forbidden",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::RateLimited => "rate_limited",
            Self::ProviderError => "provider_error",
            Self::BusinessError => "business_error",
            Self::DecodeError => "decode_error",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::ArtifactWriteFailed => "artifact_write_failed",
            Self::PersistenceFailed => "persistence_failed",
            Self::Transport => "transport",
        }
    }

    pub const fn to_protocol_error_code(self) -> ErrorCode {
        match self {
            Self::InvalidRequest | Self::BusinessError | Self::DecodeError => {
                ErrorCode::RequestFailed
            }
            Self::AuthRejected | Self::Forbidden => ErrorCode::Forbidden,
            Self::Conflict => ErrorCode::Conflict,
            Self::NotFound => ErrorCode::NotFound,
            Self::RateLimited | Self::ProviderError | Self::Timeout | Self::Transport => {
                ErrorCode::Unavailable
            }
            Self::Cancelled | Self::ArtifactWriteFailed | Self::PersistenceFailed => {
                ErrorCode::Internal
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaError {
    pub code: MediaErrorCode,
    pub status: Option<u16>,
    pub upstream_code: Option<String>,
    pub retryable: bool,
    pub message: String,
}

impl MediaError {
    pub fn new(code: MediaErrorCode, message: impl Into<String>) -> Self {
        let retryable = matches!(
            code,
            MediaErrorCode::RateLimited | MediaErrorCode::Timeout | MediaErrorCode::Transport
        );
        Self {
            code,
            status: None,
            upstream_code: None,
            retryable,
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(MediaErrorCode::InvalidRequest, message)
    }

    pub fn business(
        upstream_code: Option<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code: MediaErrorCode::BusinessError,
            status: Some(200),
            upstream_code,
            retryable,
            message: message.into(),
        }
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_upstream_code(mut self, code: Option<String>) -> Self {
        self.upstream_code = code;
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub(crate) fn scrub_in_place(&mut self, keys: &CredentialSet) {
        self.message = keys.scrub(&self.message);
        if let Some(code) = &mut self.upstream_code {
            *code = keys.scrub(code);
        }
    }

    pub(crate) fn scrubbed(mut self, keys: &CredentialSet) -> Self {
        self.scrub_in_place(keys);
        self
    }
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.status, &self.upstream_code) {
            (Some(status), Some(code)) => {
                write!(
                    f,
                    "{} (HTTP {status}, {code}): {}",
                    self.code.as_str(),
                    self.message
                )
            }
            (Some(status), None) => {
                write!(
                    f,
                    "{} (HTTP {status}): {}",
                    self.code.as_str(),
                    self.message
                )
            }
            (None, Some(code)) => {
                write!(f, "{} ({code}): {}", self.code.as_str(), self.message)
            }
            (None, None) => write!(f, "{}: {}", self.code.as_str(), self.message),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<&MediaError> for ErrorBody {
    fn from(err: &MediaError) -> Self {
        Self {
            code: err.code.to_protocol_error_code(),
            message: err.message.clone(),
            retryable: err.retryable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaAuthKind {
    Bearer,
    XGoogApiKey,
    XiApiKey,
    AzureSubscriptionKey,
    VolcengineV3ApiKey,
    VolcengineV3AppAccess,
    VolcengineV1SemicolonBearer,
    KlingAkSk,
    ProxyToken,
}

pub(crate) type MaterializedHeaders = (Vec<(String, String)>, Vec<String>, CredentialSet);

#[derive(Clone)]
pub struct MediaCredential {
    pub kind: MediaAuthKind,
    primary: String,
    secondary: Option<String>,
    extra_headers: Vec<(String, String)>,
}

impl fmt::Debug for MediaCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaCredential")
            .field("kind", &self.kind)
            .field("primary", &"[redacted]")
            .field("secondary", &self.secondary.as_ref().map(|_| "[redacted]"))
            .field("extra_headers_count", &self.extra_headers.len())
            .finish()
    }
}

impl MediaCredential {
    pub fn new(
        kind: MediaAuthKind,
        primary: impl Into<String>,
        secondary: Option<String>,
    ) -> Result<Self, MediaError> {
        let primary = primary.into();
        if primary.trim().is_empty() {
            return Err(MediaError::invalid_request(
                "media credential primary value must not be empty",
            ));
        }
        if matches!(
            kind,
            MediaAuthKind::KlingAkSk | MediaAuthKind::VolcengineV3AppAccess
        ) && secondary.as_deref().is_none_or(|s| s.trim().is_empty())
        {
            return Err(MediaError::invalid_request(
                "media credential requires a non-empty secondary secret",
            ));
        }
        Ok(Self {
            kind,
            primary,
            secondary,
            extra_headers: Vec::new(),
        })
    }

    pub fn bearer(token: impl Into<String>) -> Result<Self, MediaError> {
        Self::new(MediaAuthKind::Bearer, token, None)
    }

    pub fn x_goog_api_key(key: impl Into<String>) -> Result<Self, MediaError> {
        Self::new(MediaAuthKind::XGoogApiKey, key, None)
    }

    pub fn xi_api_key(key: impl Into<String>) -> Result<Self, MediaError> {
        Self::new(MediaAuthKind::XiApiKey, key, None)
    }

    pub fn azure_subscription_key(key: impl Into<String>) -> Result<Self, MediaError> {
        Self::new(MediaAuthKind::AzureSubscriptionKey, key, None)
    }

    pub fn volcengine_v3_api_key(key: impl Into<String>) -> Result<Self, MediaError> {
        Self::new(MediaAuthKind::VolcengineV3ApiKey, key, None)
    }

    pub fn volcengine_v3_app_access(
        app_key: impl Into<String>,
        access_key: impl Into<String>,
    ) -> Result<Self, MediaError> {
        Self::new(
            MediaAuthKind::VolcengineV3AppAccess,
            app_key,
            Some(access_key.into()),
        )
    }

    pub fn volcengine_v1_semicolon_bearer(token: impl Into<String>) -> Result<Self, MediaError> {
        Self::new(MediaAuthKind::VolcengineV1SemicolonBearer, token, None)
    }

    pub fn kling_ak_sk(
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
    ) -> Result<Self, MediaError> {
        Self::new(
            MediaAuthKind::KlingAkSk,
            access_key,
            Some(secret_key.into()),
        )
    }

    pub fn proxy_token(token: impl Into<String>) -> Result<Self, MediaError> {
        Self::new(MediaAuthKind::ProxyToken, token, None)
    }

    pub fn with_extra_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }

    pub fn with_auth_kind(
        self,
        kind: MediaAuthKind,
        app_key_override: Option<&str>,
        secondary_override: Option<String>,
    ) -> Result<Self, MediaError> {
        let primary = if kind == MediaAuthKind::VolcengineV3AppAccess {
            app_key_override
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or(self.primary)
        } else {
            self.primary
        };
        let secondary = secondary_override.or(self.secondary);
        let mut rebuilt = Self::new(kind, primary, secondary)?;
        rebuilt.extra_headers = self.extra_headers;
        Ok(rebuilt)
    }

    pub(crate) fn credential_set(&self) -> CredentialSet {
        let mut keys = vec![self.primary.clone()];
        if let Some(sec) = &self.secondary {
            keys.push(sec.clone());
        }
        for (_, value) in &self.extra_headers {
            keys.push(value.clone());
        }
        CredentialSet::new(keys)
    }

    /// Materializes request auth headers and returns `(headers, sensitive_header_names, scrubber)`.
    pub fn materialize_headers(
        &self,
        now_unix_secs: i64,
    ) -> Result<MaterializedHeaders, MediaError> {
        let mut headers = Vec::new();
        let mut sensitive = Vec::new();
        let mut scrub_keys = vec![self.primary.clone()];
        if let Some(sec) = &self.secondary {
            scrub_keys.push(sec.clone());
        }

        match self.kind {
            MediaAuthKind::Bearer | MediaAuthKind::ProxyToken => {
                headers.push((
                    "Authorization".to_owned(),
                    format!("Bearer {}", self.primary),
                ));
                sensitive.push("Authorization".to_owned());
            }
            MediaAuthKind::XGoogApiKey => {
                headers.push(("x-goog-api-key".to_owned(), self.primary.clone()));
                sensitive.push("x-goog-api-key".to_owned());
            }
            MediaAuthKind::XiApiKey => {
                headers.push(("xi-api-key".to_owned(), self.primary.clone()));
                sensitive.push("xi-api-key".to_owned());
            }
            MediaAuthKind::AzureSubscriptionKey => {
                headers.push(("Ocp-Apim-Subscription-Key".to_owned(), self.primary.clone()));
                sensitive.push("Ocp-Apim-Subscription-Key".to_owned());
            }
            MediaAuthKind::VolcengineV3ApiKey => {
                headers.push(("X-Api-Key".to_owned(), self.primary.clone()));
                sensitive.push("X-Api-Key".to_owned());
            }
            MediaAuthKind::VolcengineV3AppAccess => {
                let access_key = self.secondary.as_deref().unwrap_or_default();
                headers.push(("X-Api-App-Key".to_owned(), self.primary.clone()));
                headers.push(("X-Api-Access-Key".to_owned(), access_key.to_owned()));
                sensitive.push("X-Api-App-Key".to_owned());
                sensitive.push("X-Api-Access-Key".to_owned());
            }
            MediaAuthKind::VolcengineV1SemicolonBearer => {
                headers.push((
                    "Authorization".to_owned(),
                    format!("Bearer;{}", self.primary),
                ));
                sensitive.push("Authorization".to_owned());
            }
            MediaAuthKind::KlingAkSk => {
                let sk = self.secondary.as_deref().unwrap_or_default();
                let jwt = crypto::encode_kling_jwt(&self.primary, sk, now_unix_secs)?;
                scrub_keys.push(jwt.clone());
                headers.push(("Authorization".to_owned(), format!("Bearer {jwt}")));
                sensitive.push("Authorization".to_owned());
            }
        }

        for (name, value) in &self.extra_headers {
            headers.push((name.clone(), value.clone()));
            scrub_keys.push(value.clone());
            sensitive.push(name.clone());
        }

        Ok((headers, sensitive, CredentialSet::new(scrub_keys)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaEndpointConfig {
    pub vendor: String,
    pub base_url: String,
    pub model: String,
    pub route_id: Option<String>,
    pub timeout_ms: Option<u64>,
    pub embedded_proxy: bool,
}

impl MediaEndpointConfig {
    pub fn new(
        vendor: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            vendor: vendor.into(),
            base_url: base_url.into(),
            model: model.into(),
            route_id: None,
            timeout_ms: None,
            embedded_proxy: false,
        }
    }
}

pub fn validate_media_url(raw: &str, embedded_loopback_only: bool) -> Result<url::Url, MediaError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed != raw
        || trimmed
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || trimmed.contains(['\\', '?', '#'])
    {
        return Err(MediaError::invalid_request("invalid media base URL"));
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return Err(MediaError::invalid_request(
                    "invalid percent-encoding in media URL",
                ));
            }
            let pair = &trimmed[i + 1..i + 3];
            if pair.eq_ignore_ascii_case("2f") || pair.eq_ignore_ascii_case("5c") {
                return Err(MediaError::invalid_request(
                    "encoded slash not allowed in media URL",
                ));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    let url = url::Url::parse(trimmed)
        .map_err(|_| MediaError::invalid_request("failed to parse media URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(MediaError::invalid_request(
            "media URL must use http or https",
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(MediaError::invalid_request(
            "media URL must not contain credentials, query, or fragment",
        ));
    }
    for segment in url.path().split('/') {
        let normalized = segment.to_ascii_lowercase().replace("%2e", ".");
        if normalized == "." || normalized == ".." {
            return Err(MediaError::invalid_request(
                "dot path segment not allowed in media URL",
            ));
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| MediaError::invalid_request("media URL missing host"))?;
    if embedded_loopback_only {
        let is_loopback_ip = host
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(host)
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
        if !is_loopback_ip {
            return Err(MediaError::invalid_request(
                "embedded proxy media URL must be a loopback IP literal",
            ));
        }
    }
    Ok(url)
}

pub fn validate_ws_url(raw: &str, embedded_loopback_only: bool) -> Result<url::Url, MediaError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed != raw
        || trimmed
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || trimmed.contains(['\\', '?', '#'])
    {
        return Err(MediaError::invalid_request("invalid websocket base URL"));
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return Err(MediaError::invalid_request(
                    "invalid percent-encoding in websocket URL",
                ));
            }
            let pair = &trimmed[i + 1..i + 3];
            if pair.eq_ignore_ascii_case("2f") || pair.eq_ignore_ascii_case("5c") {
                return Err(MediaError::invalid_request(
                    "encoded slash not allowed in websocket URL",
                ));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    let url = url::Url::parse(trimmed)
        .map_err(|_| MediaError::invalid_request("failed to parse websocket URL"))?;
    match url.scheme() {
        "wss" => {}
        "ws" => {
            if !is_loopback_host(&url) {
                return Err(MediaError::invalid_request(
                    "insecure ws:// is only allowed for loopback hosts",
                ));
            }
        }
        _ => {
            return Err(MediaError::invalid_request(
                "websocket URL must use ws or wss",
            ));
        }
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(MediaError::invalid_request(
            "websocket URL must not contain credentials, query, or fragment",
        ));
    }
    for segment in url.path().split('/') {
        let normalized = segment.to_ascii_lowercase().replace("%2e", ".");
        if normalized == "." || normalized == ".." {
            return Err(MediaError::invalid_request(
                "dot path segment not allowed in websocket URL",
            ));
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| MediaError::invalid_request("websocket URL missing host"))?;
    if embedded_loopback_only {
        let is_loopback_ip = host
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(host)
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
        if !is_loopback_ip {
            return Err(MediaError::invalid_request(
                "embedded proxy websocket URL must be a loopback IP literal",
            ));
        }
    }
    Ok(url)
}

pub fn http_url_to_ws_url(raw: &str, embedded_loopback_only: bool) -> Result<url::Url, MediaError> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("https://") {
        validate_ws_url(&format!("wss://{rest}"), embedded_loopback_only)
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        validate_ws_url(&format!("ws://{rest}"), embedded_loopback_only)
    } else {
        validate_ws_url(trimmed, embedded_loopback_only)
    }
}

pub fn validate_media_download_url(
    raw: &str,
    allow_http_loopback: bool,
) -> Result<url::Url, MediaError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.bytes().any(|b| b.is_ascii_control()) {
        return Err(MediaError::invalid_request("invalid media download URL"));
    }
    let url = url::Url::parse(trimmed)
        .map_err(|_| MediaError::invalid_request("failed to parse media download URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(MediaError::invalid_request(
            "media download URL must not contain embedded credentials",
        ));
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if allow_http_loopback && is_loopback_host(&url) => Ok(url),
        "http" => Err(MediaError::invalid_request(
            "insecure http:// media download is only permitted for loopback targets",
        )),
        _ => Err(MediaError::invalid_request(
            "media download URL must use https or loopback http",
        )),
    }
}

pub(crate) fn is_loopback_host(url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<IpAddr>()
        .is_ok_and(|ip| match ip {
            IpAddr::V4(v4) => v4.is_loopback(),
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
                    || v6.to_ipv4().is_some_and(|v4| v4.is_loopback())
            }
        })
}

pub(crate) fn is_non_loopback_private_ip(url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let clean = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    let Ok(ip) = clean.parse::<IpAddr>() else {
        return false;
    };
    match ip {
        IpAddr::V4(v4) => is_non_loopback_private_v4(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return false;
            }
            if let Some(mapped) = v6.to_ipv4_mapped().or_else(|| v6.to_ipv4()) {
                return is_non_loopback_private_v4(mapped);
            }
            let seg0 = v6.segments()[0];
            let is_ula = (seg0 & 0xfe00) == 0xfc00;
            let is_link_local = (seg0 & 0xffc0) == 0xfe80;
            v6.is_unspecified() || v6.is_multicast() || is_ula || is_link_local
        }
    }
}

fn is_non_loopback_private_v4(v4: std::net::Ipv4Addr) -> bool {
    if v4.is_loopback() {
        return false;
    }
    let octets = v4.octets();
    let is_cgnat = octets[0] == 100 && (64..=127).contains(&octets[1]);
    v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || is_cgnat
}

pub(crate) fn validate_path_segment(label: &str, value: &str) -> Result<String, MediaError> {
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

pub(crate) fn normalize_gemini_model_id(model: &str) -> Result<&str, MediaError> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed != model {
        return Err(MediaError::invalid_request("invalid Gemini model id"));
    }
    let id = trimmed.strip_prefix("models/").unwrap_or(trimmed);
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains("..")
        || id.bytes().any(|b| {
            b.is_ascii_control()
                || b.is_ascii_whitespace()
                || matches!(b, b'/' | b'\\' | b'?' | b'#' | b'%' | b':' | b'@')
        })
    {
        return Err(MediaError::invalid_request("invalid Gemini model id"));
    }
    Ok(id)
}

pub(crate) fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub trait MediaTaskRepository: Send + Sync {
    fn create_task(&self, new_task: &NewMediaTask) -> Result<CreateMediaTaskResult, MediaError>;
    fn get_task(&self, id: &str) -> Result<Option<MediaTaskRecord>, MediaError>;
    fn get_by_idempotency_key(&self, key: &str) -> Result<Option<MediaTaskRecord>, MediaError>;
    fn update_task_status(
        &self,
        id: &str,
        update: &MediaTaskUpdate,
    ) -> Result<MediaTaskRecord, MediaError>;
    fn mark_cancelled(&self, id: &str, reason: Option<&str>)
    -> Result<MediaTaskRecord, MediaError>;
    fn list_resumable_tasks(
        &self,
        session_id: Option<&str>,
    ) -> Result<Vec<MediaTaskRecord>, MediaError>;
}

pub trait MediaArtifactStore: Send + Sync {
    fn write_artifact<'a>(
        &'a self,
        owner_id: &'a str,
        suggested_ext: &'a str,
        mime_type: &'a str,
        bytes: &'a [u8],
    ) -> impl Future<Output = Result<MediaArtifactRef, MediaError>> + Send + 'a;

    fn read_artifact<'a>(
        &'a self,
        artifact: &'a MediaArtifactRef,
    ) -> impl Future<Output = Result<bytes::Bytes, MediaError>> + Send + 'a;
}

pub trait MediaCredentialSource: Send + Sync {
    fn resolve_media_endpoint<'a>(
        &'a self,
        route_id: Option<&'a str>,
        vendor: &'a str,
        model: &'a str,
    ) -> impl Future<Output = Result<(MediaEndpointConfig, MediaCredential), MediaError>> + Send + 'a;

    fn credential_set(&self, cred: &MediaCredential) -> CredentialSet {
        cred.credential_set()
    }
}

#[derive(Default)]
pub struct InMemoryMediaTaskRepository {
    inner: Mutex<InMemoryRepoState>,
}

#[derive(Default)]
struct InMemoryRepoState {
    order: Vec<String>,
    tasks: HashMap<String, MediaTaskRecord>,
    by_idempotency: HashMap<String, String>,
}

impl InMemoryMediaTaskRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MediaTaskRepository for InMemoryMediaTaskRepository {
    fn create_task(&self, new_task: &NewMediaTask) -> Result<CreateMediaTaskResult, MediaError> {
        validate_persisted_id("media_task_id", &new_task.id)
            .map_err(|e| MediaError::invalid_request(e.to_string()))?;
        if new_task.request_hash.len() != 64
            || !new_task.request_hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(MediaError::invalid_request(
                "media task request_hash must be a 64-char hex digest",
            ));
        }
        if let Some(key) = new_task.idempotency_key.as_deref() {
            validate_idempotency_key(key)
                .map_err(|e| MediaError::invalid_request(e.to_string()))?;
        }
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| MediaError::new(MediaErrorCode::PersistenceFailed, "repo poisoned"))?;
        if let Some(key) = new_task.idempotency_key.as_deref()
            && let Some(existing_id) = guard.by_idempotency.get(key)
            && let Some(existing) = guard.tasks.get(existing_id)
        {
            if existing.request_hash == new_task.request_hash {
                return Ok(CreateMediaTaskResult::Replayed(existing.clone()));
            }
            return Ok(CreateMediaTaskResult::IdempotencyConflict);
        }
        if guard.tasks.contains_key(&new_task.id) {
            return Err(MediaError::new(
                MediaErrorCode::Conflict,
                "media task id already exists",
            ));
        }
        let mut record: MediaTaskRecord = new_task.clone().into();
        record.request_summary = redact_persisted(&record.request_summary);
        if let Some(key) = &record.idempotency_key {
            guard.by_idempotency.insert(key.clone(), record.id.clone());
        }
        guard.order.push(record.id.clone());
        guard.tasks.insert(record.id.clone(), record.clone());
        Ok(CreateMediaTaskResult::Created(record))
    }

    fn get_task(&self, id: &str) -> Result<Option<MediaTaskRecord>, MediaError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| MediaError::new(MediaErrorCode::PersistenceFailed, "repo poisoned"))?;
        Ok(guard.tasks.get(id).cloned())
    }

    fn get_by_idempotency_key(&self, key: &str) -> Result<Option<MediaTaskRecord>, MediaError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| MediaError::new(MediaErrorCode::PersistenceFailed, "repo poisoned"))?;
        Ok(guard
            .by_idempotency
            .get(key)
            .and_then(|id| guard.tasks.get(id))
            .cloned())
    }

    fn update_task_status(
        &self,
        id: &str,
        update: &MediaTaskUpdate,
    ) -> Result<MediaTaskRecord, MediaError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| MediaError::new(MediaErrorCode::PersistenceFailed, "repo poisoned"))?;
        let record = guard
            .tasks
            .get_mut(id)
            .ok_or_else(|| MediaError::new(MediaErrorCode::NotFound, "media task not found"))?;
        if record.status.is_terminal() && record.status != update.status {
            return Err(MediaError::new(
                MediaErrorCode::Conflict,
                format!(
                    "cannot transition terminal media task from {} to {}",
                    record.status.as_str(),
                    update.status.as_str()
                ),
            ));
        }
        record.status = update.status;
        if let Some(remote_id) = &update.remote_task_id {
            record.remote_task_id = Some(remote_id.clone());
        }
        if let Some(cursor) = &update.poll_cursor {
            record.poll_cursor = Some(cursor.clone());
        }
        if let Some(artifacts) = &update.artifacts {
            record.artifacts = artifacts.clone();
        }
        if let Some(code) = &update.error_code {
            record.error_code = Some(code.clone());
        }
        if let Some(msg) = &update.error_message {
            record.error_message = Some(msg.clone());
        }
        record.updated_at = update.updated_at;
        Ok(record.clone())
    }

    fn mark_cancelled(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> Result<MediaTaskRecord, MediaError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| MediaError::new(MediaErrorCode::PersistenceFailed, "repo poisoned"))?;
        let record = guard
            .tasks
            .get_mut(id)
            .ok_or_else(|| MediaError::new(MediaErrorCode::NotFound, "media task not found"))?;
        if matches!(
            record.status,
            MediaTaskStatus::Succeeded | MediaTaskStatus::Failed
        ) {
            return Ok(record.clone());
        }
        record.status = MediaTaskStatus::Cancelled;
        if record.error_code.is_none() {
            record.error_code = Some("cancelled".to_owned());
        }
        if let Some(reason) = reason {
            record.error_message = Some(reason.to_owned());
        }
        record.updated_at = unix_now_secs();
        Ok(record.clone())
    }

    fn list_resumable_tasks(
        &self,
        session_id: Option<&str>,
    ) -> Result<Vec<MediaTaskRecord>, MediaError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| MediaError::new(MediaErrorCode::PersistenceFailed, "repo poisoned"))?;
        let mut items: Vec<MediaTaskRecord> = guard
            .order
            .iter()
            .filter_map(|id| guard.tasks.get(id))
            .filter(|rec| {
                rec.status.is_resumable()
                    && session_id.is_none_or(|sid| rec.session_id.as_deref() == Some(sid))
            })
            .cloned()
            .collect();
        items.sort_by_key(|rec| rec.created_at);
        Ok(items)
    }
}

#[derive(Default)]
pub struct InMemoryMediaArtifactStore {
    items: Mutex<Vec<(MediaArtifactRef, Vec<u8>)>>,
}

impl InMemoryMediaArtifactStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Vec<(MediaArtifactRef, Vec<u8>)> {
        self.items.lock().unwrap().clone()
    }
}

impl MediaArtifactStore for InMemoryMediaArtifactStore {
    async fn write_artifact<'a>(
        &'a self,
        owner_id: &'a str,
        suggested_ext: &'a str,
        mime_type: &'a str,
        bytes: &'a [u8],
    ) -> Result<MediaArtifactRef, MediaError> {
        if bytes.is_empty() {
            return Err(MediaError::new(
                MediaErrorCode::ArtifactWriteFailed,
                "cannot persist empty media artifact",
            ));
        }
        let clean_ext = sanitize_extension(suggested_ext);
        let sha256 = crypto::sha256_hex(bytes);
        let id = uuid::Uuid::now_v7();
        let path = format!("memory://{owner_id}/{id}.{clean_ext}");
        let artifact = MediaArtifactRef {
            path,
            mime_type: mime_type.to_owned(),
            size_bytes: bytes.len() as u64,
            sha256,
        };
        self.items
            .lock()
            .map_err(|_| {
                MediaError::new(
                    MediaErrorCode::ArtifactWriteFailed,
                    "in-memory artifact store poisoned",
                )
            })?
            .push((artifact.clone(), bytes.to_vec()));
        Ok(artifact)
    }

    async fn read_artifact<'a>(
        &'a self,
        artifact: &'a MediaArtifactRef,
    ) -> Result<bytes::Bytes, MediaError> {
        let guard = self.items.lock().map_err(|_| {
            MediaError::new(
                MediaErrorCode::NotFound,
                "in-memory artifact store poisoned",
            )
        })?;
        guard
            .iter()
            .find(|(item, _)| item.path == artifact.path)
            .map(|(_, data)| bytes::Bytes::from(data.clone()))
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::NotFound,
                    format!("in-memory media artifact not found: {}", artifact.path),
                )
            })
    }
}

pub fn sanitize_extension(raw: &str) -> String {
    let trimmed = raw.trim().trim_start_matches('.');
    let clean: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(10)
        .collect::<String>()
        .to_ascii_lowercase();
    if clean.is_empty() {
        "bin".to_owned()
    } else {
        clean
    }
}
