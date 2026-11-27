//! Authorized Provider HTTP requests without process-sealing leak guards or pre-send DB writes.
use futures_util::{Stream, StreamExt};
use reqwest::{Url, header::HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, time::Duration};

pub const RESPONSE_LIMIT: usize = 4_000_000;
pub const ERROR_BODY_LIMIT: usize = 64 * 1024;
pub const DEFAULT_STREAM_READ_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPurpose {
    Models,
    Completion,
    Balance,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    #[default]
    #[serde(
        alias = "openai",
        alias = "openai-completions",
        alias = "openai_completions"
    )]
    OpenAi,
    #[serde(alias = "openai-responses", alias = "openai_responses")]
    OpenAiResponses,
    #[serde(alias = "anthropic-messages", alias = "anthropic_messages")]
    Anthropic,
    #[serde(alias = "gemini-native")]
    Gemini,
}

impl ProviderProtocol {
    pub fn parse_explicit(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "openai-completions" | "openai_completions" | "open_ai" | "openai" => {
                Some(Self::OpenAi)
            }
            "openai-responses" | "openai_responses" | "open_ai_responses" => {
                Some(Self::OpenAiResponses)
            }
            "anthropic-messages" | "anthropic_messages" | "anthropic" => Some(Self::Anthropic),
            "gemini" | "gemini-native" => Some(Self::Gemini),
            _ => None,
        }
    }

    pub fn parse_embedded_model_protocol(raw: &str) -> Option<Self> {
        match raw.trim() {
            "openai-completions" => Some(Self::OpenAi),
            "openai-responses" => Some(Self::OpenAiResponses),
            "anthropic-messages" => Some(Self::Anthropic),
            _ => None,
        }
    }

    pub const fn kernel_wire_name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai-completions",
            Self::OpenAiResponses => "openai-responses",
            Self::Anthropic => "anthropic-messages",
            Self::Gemini => "gemini",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "open_ai",
            Self::OpenAiResponses => "open_ai_responses",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }

    pub fn detect(base_url: &str) -> Self {
        let Ok(url) = Url::parse(base_url.trim()) else {
            return Self::OpenAi;
        };
        let Some(host) = url.host_str() else {
            return Self::OpenAi;
        };
        Self::from_host_and_path(host, url.path())
    }

    pub(crate) fn from_host_and_path(host: &str, path: &str) -> Self {
        let lower_path = path.trim_end_matches('/').to_ascii_lowercase();
        if host.eq_ignore_ascii_case("api.anthropic.com")
            || lower_path.ends_with("/anthropic")
            || lower_path.ends_with("/anthropic/v1")
        {
            Self::Anthropic
        } else if host.eq_ignore_ascii_case("generativelanguage.googleapis.com")
            || lower_path.ends_with("/gemini")
            || lower_path.ends_with("/v1beta")
        {
            Self::Gemini
        } else {
            Self::OpenAi
        }
    }
}

pub(crate) fn normalize_gemini_model_id(model: &str) -> Result<&str, TransportError> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed != model {
        return Err(TransportError::Invalid);
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
        return Err(TransportError::Invalid);
    }
    Ok(id)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkTarget {
    origin: String,
    base_url: String,
    endpoint: String,
    method: &'static str,
    purpose: NetworkPurpose,
    protocol: ProviderProtocol,
}

impl NetworkTarget {
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn method(&self) -> &str {
        self.method
    }
    pub fn purpose(&self) -> NetworkPurpose {
        self.purpose
    }
    pub fn protocol(&self) -> ProviderProtocol {
        self.protocol
    }

    pub(crate) fn resolved_endpoint(&self, model: Option<&str>) -> Result<String, TransportError> {
        if self.protocol == ProviderProtocol::Gemini && self.purpose == NetworkPurpose::Completion {
            let model = model.ok_or(TransportError::Invalid)?;
            let model_id = normalize_gemini_model_id(model)?;
            Ok(self.endpoint.replace("{model}", model_id))
        } else {
            Ok(self.endpoint.clone())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderError {
    pub status: u16,
    pub upstream_code: Option<String>,
    pub message: Option<String>,
    pub retryable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
    Invalid,
    Response { status: u16 },
    Provider(ProviderError),
    InvalidResponse { status: u16 },
    Unknown,
}

impl TransportError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Response { status } | Self::InvalidResponse { status } => Some(*status),
            Self::Provider(err) => Some(err.status),
            Self::Invalid | Self::Unknown => None,
        }
    }

    pub fn provider_error(&self) -> Option<&ProviderError> {
        match self {
            Self::Provider(err) => Some(err),
            _ => None,
        }
    }

    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Provider(err) => err.retryable && transient_status(err.status),
            Self::Response { status } => transient_status(*status),
            _ => false,
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => f.write_str("invalid provider request"),
            Self::Response { status } => write!(f, "provider HTTP failure ({status})"),
            Self::Provider(err) => match (&err.upstream_code, &err.message) {
                (Some(code), Some(msg)) => {
                    write!(f, "provider HTTP failure ({}) [{code}]: {msg}", err.status)
                }
                (Some(code), None) => write!(f, "provider HTTP failure ({}) [{code}]", err.status),
                (None, Some(msg)) => write!(f, "provider HTTP failure ({}): {msg}", err.status),
                (None, None) => write!(f, "provider HTTP failure ({})", err.status),
            },
            Self::InvalidResponse { status } => write!(f, "invalid provider response ({status})"),
            Self::Unknown => f.write_str("provider result unknown"),
        }
    }
}

impl std::error::Error for TransportError {}

fn parse_validated_base(base: &str) -> Result<(String, String, String, String), TransportError> {
    if base.is_empty()
        || base
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || base.contains(['\\', '?', '#'])
    {
        return Err(TransportError::Invalid);
    }
    let bytes = base.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return Err(TransportError::Invalid);
            }
            let pair = &base[i + 1..i + 3];
            if pair.eq_ignore_ascii_case("2f") || pair.eq_ignore_ascii_case("5c") {
                return Err(TransportError::Invalid);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    let (scheme, rest) = base.split_once("://").ok_or(TransportError::Invalid)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(TransportError::Invalid);
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.is_empty() || authority.contains('@') {
        return Err(TransportError::Invalid);
    }
    for segment in path.split('/') {
        let normalized = segment.to_ascii_lowercase().replace("%2e", ".");
        if normalized == "." || normalized == ".." {
            return Err(TransportError::Invalid);
        }
    }
    let url = Url::parse(base).map_err(|_| TransportError::Invalid)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(TransportError::Invalid);
    }
    let host = url.host_str().ok_or(TransportError::Invalid)?.to_owned();
    let port = url.port_or_known_default().ok_or(TransportError::Invalid)?;
    let raw_trimmed_path = url.path().trim_end_matches('/').to_owned();
    let origin = format!("{}://{host}:{port}", url.scheme());
    let base_url = format!("{origin}{raw_trimmed_path}");
    Ok((origin, base_url, host, raw_trimmed_path))
}

pub fn validate_target(
    base: &str,
    purpose: NetworkPurpose,
) -> Result<NetworkTarget, TransportError> {
    validate_target_with_protocol(base, purpose, None)
}

pub fn validate_target_with_protocol(
    base: &str,
    purpose: NetworkPurpose,
    explicit: Option<ProviderProtocol>,
) -> Result<NetworkTarget, TransportError> {
    let (origin, base_url, host, raw_trimmed_path) = parse_validated_base(base)?;
    let protocol = match purpose {
        NetworkPurpose::Balance => ProviderProtocol::OpenAi,
        NetworkPurpose::Models | NetworkPurpose::Completion => explicit
            .unwrap_or_else(|| ProviderProtocol::from_host_and_path(&host, &raw_trimmed_path)),
    };
    let lower_path = raw_trimmed_path.to_ascii_lowercase();
    let (method, endpoint) = match (protocol, purpose) {
        (_, NetworkPurpose::Balance) => ("GET", format!("{base_url}/api/user/self")),
        (ProviderProtocol::OpenAi | ProviderProtocol::OpenAiResponses, NetworkPurpose::Models) => {
            ("GET", format!("{base_url}/models"))
        }
        (ProviderProtocol::OpenAi, NetworkPurpose::Completion) => {
            ("POST", format!("{base_url}/chat/completions"))
        }
        (ProviderProtocol::OpenAiResponses, NetworkPurpose::Completion) => {
            ("POST", format!("{base_url}/responses"))
        }
        (ProviderProtocol::Anthropic, NetworkPurpose::Models) => {
            let api_base = if lower_path.ends_with("/v1") {
                base_url.clone()
            } else {
                format!("{base_url}/v1")
            };
            ("GET", format!("{api_base}/models"))
        }
        (ProviderProtocol::Anthropic, NetworkPurpose::Completion) => {
            let api_base = if lower_path.ends_with("/v1") {
                base_url.clone()
            } else {
                format!("{base_url}/v1")
            };
            ("POST", format!("{api_base}/messages"))
        }
        (ProviderProtocol::Gemini, NetworkPurpose::Models) => {
            let api_base = if raw_trimmed_path.is_empty()
                && host.eq_ignore_ascii_case("generativelanguage.googleapis.com")
            {
                format!("{base_url}/v1beta")
            } else {
                base_url.clone()
            };
            ("GET", format!("{api_base}/models"))
        }
        (ProviderProtocol::Gemini, NetworkPurpose::Completion) => {
            let api_base = if raw_trimmed_path.is_empty()
                && host.eq_ignore_ascii_case("generativelanguage.googleapis.com")
            {
                format!("{base_url}/v1beta")
            } else {
                base_url.clone()
            };
            (
                "POST",
                format!("{api_base}/models/{{model}}:streamGenerateContent?alt=sse"),
            )
        }
    };
    Ok(NetworkTarget {
        origin,
        base_url,
        endpoint,
        method,
        purpose,
        protocol,
    })
}

/// Embedded kernel model proxy target builder (`codaKernel/1 §5`).
/// Enforces loopback IP literal (`127.0.0.1` / `[::1]`), forbids URL-path protocol guessing,
/// and forbids `/v1` rewriting on `/m/<runId>`.
pub fn validate_embedded_proxy_target(
    base: &str,
    protocol: ProviderProtocol,
) -> Result<NetworkTarget, TransportError> {
    validate_embedded_proxy_target_v11(base, protocol, false)
}

/// Embedded kernel model proxy target builder (`codaKernel/1 §5` + `codaKernel/1.1 §10`).
/// When `media_enabled` is true, permits `ProviderProtocol::Gemini` targeting
/// `{base}/gemini/models/{model}:streamGenerateContent?alt=sse`.
pub fn validate_embedded_proxy_target_v11(
    base: &str,
    protocol: ProviderProtocol,
    media_enabled: bool,
) -> Result<NetworkTarget, TransportError> {
    let (origin, base_url, host, _) = parse_validated_base(base)?;
    let is_loopback_ip = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(&host)
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback());
    if !is_loopback_ip {
        return Err(TransportError::Invalid);
    }
    let endpoint = match protocol {
        ProviderProtocol::OpenAi => format!("{base_url}/chat/completions"),
        ProviderProtocol::OpenAiResponses => format!("{base_url}/responses"),
        ProviderProtocol::Anthropic => format!("{base_url}/v1/messages"),
        ProviderProtocol::Gemini => {
            if !media_enabled {
                return Err(TransportError::Invalid);
            }
            let lower = base_url.to_ascii_lowercase();
            if lower.ends_with("/gemini") {
                format!("{base_url}/models/{{model}}:streamGenerateContent?alt=sse")
            } else {
                format!("{base_url}/gemini/models/{{model}}:streamGenerateContent?alt=sse")
            }
        }
    };
    Ok(NetworkTarget {
        origin,
        base_url,
        endpoint,
        method: "POST",
        purpose: NetworkPurpose::Completion,
        protocol,
    })
}

/// Deliberately neither Clone nor Debug. Constructed after route/account admission.
pub struct ProviderPermit {
    target: NetworkTarget,
    request_endpoint: String,
    credential: HeaderValue,
    call_id: String,
    model: Option<String>,
    account_user: Option<HeaderValue>,
    session_id: Option<HeaderValue>,
    retry_after: Option<RetryAfter>,
    scrub_keys: crate::redaction::CredentialSet,
    embedded_proxy: bool,
    media_enabled: bool,
}

pub type RetryAfter = std::sync::Arc<std::sync::OnceLock<Duration>>;

impl ProviderPermit {
    pub fn track_retry_after(mut self, retry_after: RetryAfter) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    pub fn new(
        target: NetworkTarget,
        credential: String,
        call_id: String,
        model: Option<String>,
        account_user: Option<String>,
    ) -> Result<Self, TransportError> {
        validate_permit_inputs(
            &target,
            &credential,
            &call_id,
            model.as_deref(),
            account_user.as_deref(),
        )?;
        let request_endpoint = target.resolved_endpoint(model.as_deref())?;
        let scrub_keys = crate::redaction::CredentialSet::new(vec![credential.clone()]);
        let raw_header = match target.protocol {
            ProviderProtocol::OpenAi | ProviderProtocol::OpenAiResponses => {
                format!("Bearer {credential}")
            }
            ProviderProtocol::Anthropic | ProviderProtocol::Gemini => credential,
        };
        let mut credential =
            HeaderValue::from_str(&raw_header).map_err(|_| TransportError::Invalid)?;
        credential.set_sensitive(true);
        let account_user = account_user
            .map(|s| HeaderValue::from_str(&s).map_err(|_| TransportError::Invalid))
            .transpose()?;
        Ok(Self {
            target,
            request_endpoint,
            credential,
            call_id,
            model,
            account_user,
            session_id: None,
            retry_after: None,
            scrub_keys,
            embedded_proxy: false,
            media_enabled: false,
        })
    }

    pub fn new_embedded_proxy(
        base_url: &str,
        protocol: ProviderProtocol,
        token: String,
        call_id: String,
        model: String,
    ) -> Result<Self, TransportError> {
        Self::new_embedded_proxy_with_media(base_url, protocol, token, call_id, model, false)
    }

    pub fn new_embedded_proxy_with_media(
        base_url: &str,
        protocol: ProviderProtocol,
        token: String,
        call_id: String,
        model: String,
        media_enabled: bool,
    ) -> Result<Self, TransportError> {
        let target = validate_embedded_proxy_target_v11(base_url, protocol, media_enabled)?;
        let mut permit = Self::new(target, token, call_id, Some(model), None)?;
        permit.embedded_proxy = true;
        permit.media_enabled = media_enabled;
        Ok(permit)
    }

    pub fn with_scrub_keys(mut self, keys: Vec<String>) -> Self {
        if !keys.is_empty() {
            self.scrub_keys = crate::redaction::CredentialSet::new(keys);
        }
        self
    }

    pub fn with_session_id(mut self, session_id: impl AsRef<str>) -> Self {
        let trimmed = session_id.as_ref().trim();
        if !trimmed.is_empty()
            && trimmed.len() <= 256
            && trimmed.bytes().all(|b| (0x21..=0x7E).contains(&b))
            && let Ok(val) = HeaderValue::from_str(trimmed)
        {
            self.session_id = Some(val);
        }
        self
    }

    pub fn credentials(&self) -> crate::redaction::CredentialSet {
        self.scrub_keys.clone()
    }

    pub fn credential_scrubber(&self) -> impl Fn(&str) -> String + use<> {
        let keys = self.credentials();
        move |text| keys.scrub(text)
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn protocol(&self) -> ProviderProtocol {
        self.target.protocol
    }

    pub fn is_embedded_proxy(&self) -> bool {
        self.embedded_proxy
    }

    pub fn matches(
        &self,
        base: &str,
        purpose: NetworkPurpose,
        model: Option<&str>,
        user: Option<&str>,
    ) -> Result<(), TransportError> {
        self.matches_with_protocol(base, purpose, model, user, None)
    }

    pub fn matches_with_protocol(
        &self,
        base: &str,
        purpose: NetworkPurpose,
        model: Option<&str>,
        user: Option<&str>,
        protocol: Option<ProviderProtocol>,
    ) -> Result<(), TransportError> {
        let target = if self.embedded_proxy {
            if purpose != NetworkPurpose::Completion || user.is_some() {
                return Err(TransportError::Invalid);
            }
            let explicit = protocol.ok_or(TransportError::Invalid)?;
            validate_embedded_proxy_target_v11(base, explicit, self.media_enabled)?
        } else {
            validate_target_with_protocol(base, purpose, protocol)?
        };
        if target != self.target
            || model != self.model.as_deref()
            || user.map(str::as_bytes) != self.account_user.as_ref().map(HeaderValue::as_bytes)
        {
            return Err(TransportError::Invalid);
        }
        Ok(())
    }
}

pub fn validate_permit_inputs(
    target: &NetworkTarget,
    credential: &str,
    call_id: &str,
    model: Option<&str>,
    account_user: Option<&str>,
) -> Result<(), TransportError> {
    if credential.trim().is_empty()
        || call_id.trim().is_empty()
        || HeaderValue::from_str(credential).is_err()
        || HeaderValue::from_str(&format!("Bearer {credential}")).is_err()
    {
        return Err(TransportError::Invalid);
    }
    match target.purpose {
        NetworkPurpose::Models | NetworkPurpose::Completion => {
            let Some(model_name) = model.filter(|s| !s.trim().is_empty()) else {
                return Err(TransportError::Invalid);
            };
            if account_user.is_some() {
                return Err(TransportError::Invalid);
            }
            if target.protocol == ProviderProtocol::Gemini {
                normalize_gemini_model_id(model_name)?;
            }
        }
        NetworkPurpose::Balance => {
            if model.is_some()
                || account_user
                    .is_none_or(|s| s.trim().is_empty() || HeaderValue::from_str(s).is_err())
            {
                return Err(TransportError::Invalid);
            }
        }
    }
    Ok(())
}

pub struct ProviderResponse {
    response: reqwest::Response,
}

impl ProviderResponse {
    pub fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    pub fn bytes_stream(
        self,
    ) -> std::pin::Pin<Box<dyn Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send>> {
        Box::pin(
            self.response
                .bytes_stream()
                .map(|result| result.map(|bytes| bytes.to_vec())),
        )
    }
}

pub async fn read_provider_error(
    response: ProviderResponse,
    scrub: &(impl Fn(&str) -> String + ?Sized),
) -> TransportError {
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = ERROR_BODY_LIMIT.saturating_sub(bytes.len());
        if remaining == 0 {
            break;
        }
        let take = chunk.len().min(remaining);
        bytes.extend_from_slice(&chunk[..take]);
        if bytes.len() >= ERROR_BODY_LIMIT {
            break;
        }
    }
    let raw = String::from_utf8_lossy(&bytes);
    let mut upstream_code = None;
    let mut message = None;
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) {
        let redacted = crate::redaction::redact_persisted(&Value::Object(map));
        upstream_code = redacted
            .pointer("/error/code")
            .and_then(Value::as_str)
            .or_else(|| redacted.get("code").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(scrub);
        message = redacted
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| redacted.get("message").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(scrub);
    }
    if message.is_none() {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            message = Some(truncate_chars(&scrub(trimmed), 512));
        }
    }
    TransportError::Provider(ProviderError {
        status,
        upstream_code,
        message,
        retryable: transient_status(status),
    })
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut iter = text.char_indices();
    match iter.nth(max_chars) {
        Some((idx, _)) => format!("{}...", &text[..idx]),
        None => text.to_owned(),
    }
}

pub struct ProviderClient {
    client: reqwest::Client,
}

impl ProviderClient {
    pub fn new() -> Result<Self, TransportError> {
        Self::with_read_timeout(DEFAULT_STREAM_READ_TIMEOUT)
    }

    pub fn with_read_timeout(read_timeout: Duration) -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(read_timeout)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| TransportError::Invalid)?;
        Ok(Self { client })
    }

    pub async fn send(
        &self,
        permit: ProviderPermit,
        body: Option<&Value>,
    ) -> Result<ProviderResponse, TransportError> {
        if (permit.target.purpose == NetworkPurpose::Completion) != body.is_some() {
            return Err(TransportError::Invalid);
        }
        if let Some(body) = body {
            match permit.target.protocol {
                ProviderProtocol::OpenAi
                | ProviderProtocol::OpenAiResponses
                | ProviderProtocol::Anthropic => {
                    if body.get("model").and_then(Value::as_str) != permit.model.as_deref() {
                        return Err(TransportError::Invalid);
                    }
                }
                ProviderProtocol::Gemini => {
                    if let Some(model_val) = body.get("model")
                        && model_val.as_str() != permit.model.as_deref()
                    {
                        return Err(TransportError::Invalid);
                    }
                }
            }
        }
        let method = reqwest::Method::from_bytes(permit.target.method.as_bytes())
            .map_err(|_| TransportError::Invalid)?;
        let mut builder = self.client.request(method, &permit.request_endpoint);
        builder = match permit.target.protocol {
            ProviderProtocol::OpenAi | ProviderProtocol::OpenAiResponses => {
                builder.header(reqwest::header::AUTHORIZATION, permit.credential)
            }
            ProviderProtocol::Anthropic => builder
                .header("x-api-key", permit.credential)
                .header("anthropic-version", "2023-06-01"),
            ProviderProtocol::Gemini => builder.header("x-goog-api-key", permit.credential),
        };
        if let Some(user) = permit.account_user {
            builder = builder.header("New-Api-User", user);
        }
        if permit.target.protocol == ProviderProtocol::OpenAiResponses
            && let Some(session_id) = permit.session_id
        {
            builder = builder
                .header("x-session-id", session_id.clone())
                .header("session_id", session_id);
        }
        if let Some(body) = body {
            builder = builder.json(body);
        }
        let request = builder.build().map_err(|_| TransportError::Invalid)?;
        let _call_id = permit.call_id;
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|_| TransportError::Unknown)?;
        let status = response.status().as_u16();
        if transient_status(status)
            && let Some(hint) = permit.retry_after
        {
            if let Some(delay) = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|header| header.to_str().ok())
                .and_then(|s| parse_retry_after(s, std::time::SystemTime::now()))
            {
                let _ = hint.set(delay);
            }
            return Err(TransportError::Response { status });
        }
        Ok(ProviderResponse { response })
    }
}

pub fn transient_status(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}

fn parse_retry_after(value: &str, now: std::time::SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(Duration::from_secs(value.parse().unwrap_or(u64::MAX)));
    }
    let deadline = httpdate::parse_http_date(value).ok()?;
    Some(deadline.duration_since(now).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_scrubber_follows_each_permit_and_preserves_business_text() {
        for base in [
            "https://example.test",
            "https://example.test/anthropic",
            "https://example.test/v1beta",
        ] {
            let make = |key: &str| {
                ProviderPermit::new(
                    validate_target(base, NetworkPurpose::Completion).unwrap(),
                    key.into(),
                    "call".into(),
                    Some("m".into()),
                    None,
                )
                .unwrap()
                .credential_scrubber()
            };
            let old = make("old-key-123");
            let new = make("new-key-456");
            let business = "Bearer authorization; password = example";
            let text = format!("{business} old-key-123 new-key-456");
            assert_eq!(old(&text), format!("{business} [redacted] new-key-456"));
            assert_eq!(new(&text), format!("{business} old-key-123 [redacted]"));
        }
    }

    #[test]
    fn retry_after_parses_seconds_http_dates_and_invalid_values() {
        let now = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let second = Duration::from_secs(1);
        assert_eq!(parse_retry_after(" 12 ", now), Some(second * 12));
        assert_eq!(parse_retry_after("0", now), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after(&httpdate::fmt_http_date(now + second * 5), now),
            Some(second * 5)
        );
        assert_eq!(
            parse_retry_after(&httpdate::fmt_http_date(now - second), now),
            Some(Duration::ZERO)
        );
        assert_eq!(
            parse_retry_after("99999999999999999999999", now),
            Some(Duration::from_secs(u64::MAX))
        );
        for invalid in ["", "-1", "+1", "1.2", "later"] {
            assert_eq!(parse_retry_after(invalid, now), None);
        }
    }

    #[tokio::test]
    async fn retry_after_on_http_failure_is_scoped_to_its_permit() {
        use axum::{Router, routing::get};
        let deadline =
            httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(60));
        let app = Router::new()
            .route(
                "/first/models",
                get(|| async {
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "3")],
                    )
                }),
            )
            .route(
                "/second/models",
                get(|| async move {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        [("retry-after", deadline)],
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ProviderClient::new().unwrap();
        let send = |path: &'static str| {
            let client = &client;
            async move {
                let base = format!("http://{addr}/{path}");
                let target = validate_target(&base, NetworkPurpose::Models).unwrap();
                let hint = RetryAfter::default();
                let permit = ProviderPermit::new(
                    target,
                    "fixture".into(),
                    path.into(),
                    Some("m".into()),
                    None,
                )
                .unwrap()
                .track_retry_after(hint.clone());
                let result = client.send(permit, None).await.err().unwrap();
                (result, *hint.get().unwrap())
            }
        };
        let (first, second) = tokio::join!(send("first"), send("second"));
        assert_eq!(
            first,
            (
                TransportError::Response { status: 429 },
                Duration::from_secs(3)
            )
        );
        assert_eq!(second.0, TransportError::Response { status: 503 });
        assert!(second.1 > Duration::from_secs(10) && second.1 <= Duration::from_secs(60));
        server.abort();
    }

    #[test]
    fn targets_have_one_canonical_origin_and_preserve_base_path() {
        let a = validate_target("HTTPS://Example.COM/v1///", NetworkPurpose::Completion).unwrap();
        let b = validate_target("https://example.com:443/v1", NetworkPurpose::Completion).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.origin(), "https://example.com:443");
        assert_eq!(a.base_url(), "https://example.com:443/v1");
        assert_eq!(a.endpoint(), "https://example.com:443/v1/chat/completions");
        assert_eq!(a.method(), "POST");
        assert_eq!(a.protocol(), ProviderProtocol::OpenAi);
        let ip = validate_target("http://[::1]/", NetworkPurpose::Balance).unwrap();
        assert_eq!(ip.origin(), "http://[::1]:80");
        assert_eq!(ip.endpoint(), "http://[::1]:80/api/user/self");
        assert_eq!(ip.method(), "GET");
    }

    #[test]
    fn anthropic_and_gemini_targets_resolve_without_duplicate_version_segments() {
        let ant_bare =
            validate_target("https://api.anthropic.com", NetworkPurpose::Completion).unwrap();
        assert_eq!(ant_bare.protocol(), ProviderProtocol::Anthropic);
        assert_eq!(
            ant_bare.endpoint(),
            "https://api.anthropic.com:443/v1/messages"
        );

        let ant_v1 = validate_target(
            "http://127.0.0.1:8080/anthropic/v1",
            NetworkPurpose::Completion,
        )
        .unwrap();
        assert_eq!(ant_v1.protocol(), ProviderProtocol::Anthropic);
        assert_eq!(
            ant_v1.endpoint(),
            "http://127.0.0.1:8080/anthropic/v1/messages"
        );

        let gem =
            validate_target("http://127.0.0.1:8080/v1beta", NetworkPurpose::Completion).unwrap();
        assert_eq!(gem.protocol(), ProviderProtocol::Gemini);
        assert_eq!(
            gem.resolved_endpoint(Some("gemini-2.5-pro")).unwrap(),
            "http://127.0.0.1:8080/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
        assert!(matches!(
            ProviderPermit::new(
                gem,
                "AIzaTest".into(),
                "c".into(),
                Some("../bad".into()),
                None
            ),
            Err(TransportError::Invalid)
        ));
    }

    #[test]
    fn targets_reject_ambiguity_before_url_parser_normalizes_it() {
        for base in [
            "file:///x",
            "https:///example.com",
            "http://",
            "http://user@localhost",
            "http://localhost?x",
            "http://localhost#x",
            "http://localhost/a\\b",
            " http://localhost",
            "http://localhost/a/../b",
            "http://localhost/./b",
            "http://localhost/%2e",
            "http://localhost/a%2fb",
            "http://localhost/%zz",
        ] {
            assert_eq!(
                validate_target(base, NetworkPurpose::Models),
                Err(TransportError::Invalid),
                "{base}"
            );
        }
    }

    #[test]
    fn permit_rejects_header_injection_and_wrong_identity_shape() {
        let route = || validate_target("http://localhost", NetworkPurpose::Models).unwrap();
        let account = || validate_target("http://localhost", NetworkPurpose::Balance).unwrap();
        for key in ["", " ", "key\r\ninjected: secret"] {
            assert!(matches!(
                ProviderPermit::new(route(), key.into(), "c".into(), Some("m".into()), None),
                Err(TransportError::Invalid)
            ));
        }
        assert!(matches!(
            ProviderPermit::new(route(), "k".into(), "c".into(), None, None),
            Err(TransportError::Invalid)
        ));
        assert!(matches!(
            ProviderPermit::new(
                account(),
                "k".into(),
                "c".into(),
                None,
                Some("u\r\nx: y".into())
            ),
            Err(TransportError::Invalid)
        ));
    }

    #[test]
    fn transport_errors_never_retain_upstream_sources() {
        use std::error::Error;
        for error in [
            TransportError::Invalid,
            TransportError::Response { status: 307 },
            TransportError::InvalidResponse { status: 200 },
            TransportError::Unknown,
        ] {
            assert!(error.source().is_none());
            assert!(!format!("{error:?} {error}").contains("http"));
        }
    }

    #[test]
    fn route_protocol_legacy_compat_and_explicit_aliases() {
        let legacy: crate::domain::Route = serde_json::from_str(
            r#"{"id":"r1","name":"OpenAI","base_url":"https://api.example.com/v1","model":"gpt-4o","max_tokens":1024,"parallel_limit":1}"#,
        )
        .unwrap();
        assert_eq!(legacy.protocol, None);
        assert_eq!(legacy.effective_protocol(), ProviderProtocol::OpenAi);

        for (raw, expected) in [
            ("openai-completions", ProviderProtocol::OpenAi),
            ("openai-responses", ProviderProtocol::OpenAiResponses),
            ("anthropic-messages", ProviderProtocol::Anthropic),
            ("gemini", ProviderProtocol::Gemini),
        ] {
            let parsed = ProviderProtocol::parse_explicit(raw).unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.kernel_wire_name(), raw);
            let route: crate::domain::Route = serde_json::from_str(&format!(
                r#"{{"id":"r","name":"R","base_url":"https://api.example.com/v1","model":"m","protocol":"{raw}","max_tokens":128,"parallel_limit":1}}"#
            ))
            .unwrap();
            assert_eq!(route.effective_protocol(), expected);
            let target = validate_target_with_protocol(
                &route.base_url,
                NetworkPurpose::Completion,
                route.protocol,
            )
            .unwrap();
            assert_eq!(target.protocol(), expected);
        }
        assert_eq!(
            ProviderProtocol::parse_embedded_model_protocol("openai-responses"),
            Some(ProviderProtocol::OpenAiResponses)
        );
        assert_eq!(
            ProviderProtocol::parse_embedded_model_protocol("gemini"),
            None
        );
    }

    #[test]
    fn embedded_proxy_target_forbids_path_guessing_and_path_rewriting() {
        for (url, proto, expected_endpoint) in [
            (
                "http://127.0.0.1:4100/m/run_1/anthropic",
                ProviderProtocol::OpenAiResponses,
                "http://127.0.0.1:4100/m/run_1/anthropic/responses",
            ),
            (
                "http://127.0.0.1:4100/m/v1",
                ProviderProtocol::Anthropic,
                "http://127.0.0.1:4100/m/v1/v1/messages",
            ),
            (
                "http://127.0.0.1:4100/m/run_3",
                ProviderProtocol::OpenAi,
                "http://127.0.0.1:4100/m/run_3/chat/completions",
            ),
        ] {
            let target = validate_embedded_proxy_target(url, proto).unwrap();
            assert_eq!(target.protocol(), proto);
            assert_eq!(
                target.resolved_endpoint(Some("gpt-5")).unwrap(),
                expected_endpoint
            );
            let _permit = ProviderPermit::new_embedded_proxy(
                url,
                proto,
                "proxy-tok".into(),
                "call-1".into(),
                "gpt-5".into(),
            )
            .unwrap();
        }
        assert_eq!(
            validate_embedded_proxy_target(
                "http://127.0.0.1:4100/m/run_4",
                ProviderProtocol::Gemini
            ),
            Err(TransportError::Invalid)
        );
        // Loopback IPv6 `[::1]` is allowed; `localhost` and external domains are rejected.
        let v6 = validate_embedded_proxy_target(
            "http://[::1]:4100/m/run_5",
            ProviderProtocol::OpenAiResponses,
        )
        .unwrap();
        assert_eq!(v6.endpoint(), "http://[::1]:4100/m/run_5/responses");
        assert_eq!(
            validate_embedded_proxy_target(
                "http://localhost:4100/m/run_6",
                ProviderProtocol::OpenAiResponses
            ),
            Err(TransportError::Invalid)
        );
        assert_eq!(
            validate_embedded_proxy_target(
                "https://api.example.com/m/run_7",
                ProviderProtocol::OpenAiResponses
            ),
            Err(TransportError::Invalid)
        );
    }

    #[test]
    fn embedded_proxy_with_media_matches_gemini_protocol() {
        assert_eq!(
            ProviderPermit::new_embedded_proxy_with_media(
                "http://127.0.0.1:4100/m/run_gem",
                ProviderProtocol::Gemini,
                "proxy-tok".into(),
                "call-gem".into(),
                "gemini-2.5-flash".into(),
                false,
            )
            .err(),
            Some(TransportError::Invalid)
        );

        let permit = ProviderPermit::new_embedded_proxy_with_media(
            "http://127.0.0.1:4100/m/run_gem",
            ProviderProtocol::Gemini,
            "proxy-tok".into(),
            "call-gem".into(),
            "gemini-2.5-flash".into(),
            true,
        )
        .unwrap();
        assert_eq!(
            permit.matches_with_protocol(
                "http://127.0.0.1:4100/m/run_gem",
                NetworkPurpose::Completion,
                Some("gemini-2.5-flash"),
                None,
                Some(ProviderProtocol::Gemini),
            ),
            Ok(())
        );
        assert_eq!(
            permit.matches_with_protocol(
                "http://127.0.0.1:4100/m/run_gem",
                NetworkPurpose::Completion,
                Some("gemini-2.5-flash"),
                None,
                Some(ProviderProtocol::OpenAi),
            ),
            Err(TransportError::Invalid)
        );
    }

    #[tokio::test]
    async fn read_timeout_allows_slow_chunked_stream_and_aborts_stalled_stream() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // Request 1: slow chunks every 60ms across 240ms (> 2 * 120ms read timeout)
            let (mut sock1, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = sock1.read(&mut buf).await.unwrap();
            sock1
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            for chunk in [
                "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"c\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ] {
                tokio::time::sleep(Duration::from_millis(60)).await;
                let frame = format!("{:x}\r\n{chunk}\r\n", chunk.len());
                sock1.write_all(frame.as_bytes()).await.unwrap();
            }
            sock1.write_all(b"0\r\n\r\n").await.unwrap();

            // Request 2: stalls past read timeout after first chunk
            let (mut sock2, _) = listener.accept().await.unwrap();
            let _ = sock2.read(&mut buf).await.unwrap();
            sock2
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            let first = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
            let frame = format!("{:x}\r\n{first}\r\n", first.len());
            sock2.write_all(frame.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(450)).await;
        });

        let client = ProviderClient::with_read_timeout(Duration::from_millis(120)).unwrap();
        let base = format!("http://{addr}/v1");
        let route = crate::domain::Route {
            id: "r".into(),
            name: "r".into(),
            base_url: base.clone(),
            model: "m".into(),
            protocol: Some(ProviderProtocol::OpenAi),
            max_tokens: 64,
            parallel_limit: 1,
            key_env: None,
        };
        let target =
            validate_target_with_protocol(&base, NetworkPurpose::Completion, route.protocol)
                .unwrap();
        let permit1 = ProviderPermit::new(
            target.clone(),
            "k".into(),
            "c1".into(),
            Some("m".into()),
            None,
        )
        .unwrap();
        let messages = [crate::messages::NeutralMessage::user_text("hi")];
        let (msg, _) =
            crate::provider::complete_neutral(&client, permit1, &route, &messages, &[], |_| Ok(()))
                .await
                .unwrap();
        assert_eq!(msg.text_content(), "abc");

        let permit2 =
            ProviderPermit::new(target, "k".into(), "c2".into(), Some("m".into()), None).unwrap();
        let err =
            crate::provider::complete_neutral(&client, permit2, &route, &messages, &[], |_| Ok(()))
                .await
                .unwrap_err();
        assert_eq!(err, TransportError::Unknown);
        server.abort();
    }

    #[tokio::test]
    async fn json_response_enforces_response_limit() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await.unwrap();
            let oversize = format!("{{\"data\":\"{}\"}}", "x".repeat(RESPONSE_LIMIT + 1024));
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                oversize.len()
            );
            let _ = sock.write_all(header.as_bytes()).await;
            let _ = sock.write_all(oversize.as_bytes()).await;
        });

        let client = ProviderClient::new().unwrap();
        let base = format!("http://{addr}/v1");
        let target = validate_target(&base, NetworkPurpose::Models).unwrap();
        let permit =
            ProviderPermit::new(target, "k".into(), "c".into(), Some("m".into()), None).unwrap();
        let resp = client.send(permit, None).await.unwrap();
        let err = crate::provider::json_response(resp).await.unwrap_err();
        assert_eq!(err, TransportError::Unknown);
        server.abort();
    }
}
