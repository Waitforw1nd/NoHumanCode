//! Authorized Provider HTTP requests without process-sealing leak guards or pre-send DB writes.
use futures_util::Stream;
use reqwest::{Url, header::HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, time::Duration};

pub const RESPONSE_LIMIT: usize = 4_000_000;

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
    OpenAi,
    Anthropic,
    Gemini,
}

impl ProviderProtocol {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    Invalid,
    Response { status: u16 },
    InvalidResponse { status: u16 },
    Unknown,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid provider request",
            Self::Response { .. } => "provider HTTP failure",
            Self::InvalidResponse { .. } => "invalid provider response",
            Self::Unknown => "provider result unknown",
        })
    }
}

impl std::error::Error for TransportError {}

pub fn validate_target(
    base: &str,
    purpose: NetworkPurpose,
) -> Result<NetworkTarget, TransportError> {
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
    let host = url.host_str().ok_or(TransportError::Invalid)?;
    let port = url.port_or_known_default().ok_or(TransportError::Invalid)?;
    let raw_trimmed_path = url.path().trim_end_matches('/');
    let origin = format!("{}://{host}:{port}", url.scheme());
    let base_url = format!("{origin}{raw_trimmed_path}");
    let protocol = match purpose {
        NetworkPurpose::Balance => ProviderProtocol::OpenAi,
        NetworkPurpose::Models | NetworkPurpose::Completion => {
            ProviderProtocol::from_host_and_path(host, raw_trimmed_path)
        }
    };
    let lower_path = raw_trimmed_path.to_ascii_lowercase();
    let (method, endpoint) = match (protocol, purpose) {
        (_, NetworkPurpose::Balance) => ("GET", format!("{base_url}/api/user/self")),
        (ProviderProtocol::OpenAi, NetworkPurpose::Models) => ("GET", format!("{base_url}/models")),
        (ProviderProtocol::OpenAi, NetworkPurpose::Completion) => {
            ("POST", format!("{base_url}/chat/completions"))
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

/// Deliberately neither Clone nor Debug. Constructed after route/account admission.
pub struct ProviderPermit {
    target: NetworkTarget,
    request_endpoint: String,
    credential: HeaderValue,
    call_id: String,
    model: Option<String>,
    account_user: Option<HeaderValue>,
    retry_after: Option<RetryAfter>,
    scrub_keys: crate::secrets::CredentialSet,
}

pub(crate) type RetryAfter = std::sync::Arc<std::sync::OnceLock<Duration>>;

impl ProviderPermit {
    pub(crate) fn track_retry_after(mut self, retry_after: RetryAfter) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    pub(crate) fn new(
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
        let scrub_keys = crate::secrets::CredentialSet::new(vec![credential.clone()]);
        let raw_header = match target.protocol {
            ProviderProtocol::OpenAi => format!("Bearer {credential}"),
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
            retry_after: None,
            scrub_keys,
        })
    }

    pub(crate) fn with_scrub_keys(mut self, keys: Vec<String>) -> Self {
        if !keys.is_empty() {
            self.scrub_keys = crate::secrets::CredentialSet::new(keys);
        }
        self
    }

    pub(crate) fn credentials(&self) -> crate::secrets::CredentialSet {
        self.scrub_keys.clone()
    }

    pub(crate) fn credential_scrubber(&self) -> impl Fn(&str) -> String + use<> {
        let keys = self.credentials();
        move |text| keys.scrub(text)
    }

    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub(crate) fn protocol(&self) -> ProviderProtocol {
        self.target.protocol
    }

    pub(crate) fn matches(
        &self,
        base: &str,
        purpose: NetworkPurpose,
        model: Option<&str>,
        user: Option<&str>,
    ) -> Result<(), TransportError> {
        let target = validate_target(base, purpose)?;
        if target != self.target
            || model != self.model.as_deref()
            || user.map(str::as_bytes) != self.account_user.as_ref().map(HeaderValue::as_bytes)
        {
            return Err(TransportError::Invalid);
        }
        Ok(())
    }
}

pub(crate) fn validate_permit_inputs(
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

pub(crate) struct ProviderResponse {
    response: reqwest::Response,
}

impl ProviderResponse {
    pub(crate) fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    pub(crate) fn bytes_stream(
        self,
    ) -> std::pin::Pin<Box<dyn Stream<Item = Result<axum::body::Bytes, reqwest::Error>> + Send>>
    {
        Box::pin(self.response.bytes_stream())
    }
}

pub struct ProviderClient {
    client: reqwest::Client,
}

impl ProviderClient {
    pub(crate) fn new() -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(180))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| TransportError::Invalid)?;
        Ok(Self { client })
    }

    pub(crate) async fn send(
        &self,
        permit: ProviderPermit,
        body: Option<&Value>,
    ) -> Result<ProviderResponse, TransportError> {
        if (permit.target.purpose == NetworkPurpose::Completion) != body.is_some() {
            return Err(TransportError::Invalid);
        }
        if let Some(body) = body {
            match permit.target.protocol {
                ProviderProtocol::OpenAi | ProviderProtocol::Anthropic => {
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
            ProviderProtocol::OpenAi => {
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

pub(crate) fn transient_status(status: u16) -> bool {
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

    #[tokio::test]
    async fn retries_transient_http_failures_before_reading_the_success_body() {
        use crate::network_authorization::*;
        use axum::{Router, routing::get};
        use std::sync::{Arc, Mutex};
        use std::time::Instant;
        let arrivals = Arc::new(Mutex::new(Vec::new()));
        let observed = arrivals.clone();
        let app = Router::new().route(
            "/models",
            get(move || {
                let mut seen = observed.lock().unwrap();
                seen.push(Instant::now());
                let status = match seen.len() {
                    1 => 502,
                    2 => 504,
                    _ => 200,
                };
                async move {
                    (
                        reqwest::StatusCode::from_u16(status).unwrap(),
                        r#"{"data":[{"id":"m"}]}"#,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let target = validate_target(&format!("http://{addr}"), NetworkPurpose::Models).unwrap();
        let client = ProviderClient::new().unwrap();
        let target = &target;
        let client = &client;
        let response = retry_provider(
            &tokio_util::sync::CancellationToken::new(),
            |hint| async move {
                let id = uuid::Uuid::new_v4().to_string();
                let permit = ProviderPermit::new(
                    target.clone(),
                    "test".into(),
                    id.clone(),
                    Some("m".into()),
                    None,
                )
                .unwrap()
                .track_retry_after(hint);
                let result = client.send(permit, None).await;
                let status = match &result {
                    Err(TransportError::Response { status }) => Some(*status),
                    Ok(_) => None,
                    _ => panic!("unexpected transport failure"),
                };
                // The Engine integration tests cover persisted receipts. This fixture isolates
                // the HTTP transport and retry loop so it can also run on hosts without DPAPI.
                let calls = vec![CallReceipt {
                    credential_slot: None,
                    call_id: id,
                    authorization_id: "fixture".into(),
                    subject: NetworkSubject {
                        kind: NetworkSubjectKind::Route,
                        id: "m".into(),
                        project_id: None,
                    },
                    task_id: None,
                    purpose: NetworkPurpose::Models,
                    method: "GET".into(),
                    origin: target.origin().into(),
                    endpoint_digest: crate::workspace_changes::digest(target.endpoint().as_bytes()),
                    claimed_at: 0,
                    finished_at: Some(0),
                    state: CallState::Finished,
                    success: Some(result.is_ok()),
                    http_status: status,
                    result: Some(if result.is_ok() {
                        CallResult::Success
                    } else {
                        CallResult::HttpFailure
                    }),
                }];
                match result {
                    Ok(response) => Ok(NetworkResponse {
                        result: crate::provider::json_response(response).await.unwrap().0,
                        calls,
                    }),
                    Err(_) => Err(NetworkError {
                        kind: NetworkErrorKind::Upstream,
                        dispatch_state: DispatchState::Finished,
                        calls,
                    }),
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(response.result["data"][0]["id"], "m");
        assert_eq!(
            response
                .calls
                .iter()
                .map(|call| call.http_status)
                .collect::<Vec<_>>(),
            [Some(502), Some(504), None]
        );
        let seen = arrivals.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen[1] - seen[0] >= Duration::from_millis(250));
        assert!(seen[2] - seen[1] >= Duration::from_millis(500));
        server.abort();
    }

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
}
