//! Fixed, authorized Provider requests. Authorization and durable claims live in the Host.
use futures_util::{Stream, StreamExt};
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkTarget {
    origin: String,
    base_url: String,
    endpoint: String,
    method: &'static str,
    purpose: NetworkPurpose,
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
    let origin = format!("{}://{host}:{port}", url.scheme());
    let base_url = format!("{origin}{}", url.path().trim_end_matches('/'));
    let (method, tail) = match purpose {
        NetworkPurpose::Models => ("GET", "models"),
        NetworkPurpose::Completion => ("POST", "chat/completions"),
        NetworkPurpose::Balance => ("GET", "api/user/self"),
    };
    let endpoint = format!("{base_url}/{tail}");
    Ok(NetworkTarget {
        origin,
        base_url,
        endpoint,
        method,
        purpose,
    })
}

/// Deliberately neither Clone nor Debug. Only the Host constructs this after a durable claim.
pub struct ProviderPermit {
    target: NetworkTarget,
    credential: HeaderValue,
    call_id: String,
    model: Option<String>,
    account_user: Option<HeaderValue>,
    authority: Option<(
        crate::execution_authority::ExecutionOperation,
        std::sync::Arc<crate::execution_authority::ExecutionAuthority>,
    )>,
}
impl ProviderPermit {
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
        let mut credential = HeaderValue::from_str(&format!("Bearer {credential}"))
            .map_err(|_| TransportError::Invalid)?;
        credential.set_sensitive(true);
        let account_user = account_user
            .map(|s| HeaderValue::from_str(&s).map_err(|_| TransportError::Invalid))
            .transpose()?;
        Ok(Self {
            target,
            credential,
            call_id,
            model,
            account_user,
            authority: None,
        })
    }
    pub(crate) fn bind_operation(
        mut self,
        operation: crate::execution_authority::ExecutionOperation,
        authority: std::sync::Arc<crate::execution_authority::ExecutionAuthority>,
    ) -> Self {
        self.authority = Some((operation, authority));
        self
    }
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
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

/// Pure preflight: no permit, claim, or network access is created here.
pub(crate) fn validate_permit_inputs(
    target: &NetworkTarget,
    credential: &str,
    call_id: &str,
    model: Option<&str>,
    account_user: Option<&str>,
) -> Result<(), TransportError> {
    if credential.trim().is_empty()
        || call_id.trim().is_empty()
        || HeaderValue::from_str(&format!("Bearer {credential}")).is_err()
    {
        return Err(TransportError::Invalid);
    }
    match target.purpose {
        NetworkPurpose::Models | NetworkPurpose::Completion => {
            if model.is_none_or(|s| s.trim().is_empty()) || account_user.is_some() {
                return Err(TransportError::Invalid);
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

// The transport generation stays alive through response-body EOF. If the
// request or stream is abandoned, retain the same domain owner until exit.
struct NetworkIoGuard {
    operation: Option<crate::execution_authority::ExecutionOperation>,
    generation: Option<Box<dyn crate::process_lifetime::DomainLease>>,
}
impl NetworkIoGuard {
    fn settle(mut self) {
        if let Some(generation) = self.generation.take() {
            generation.settle();
        }
        self.operation.take();
    }
}
impl Drop for NetworkIoGuard {
    fn drop(&mut self) {
        if let Some(mut generation) = self.generation.take() {
            generation.seal_unknown();
            std::mem::forget(generation);
            if let Some(operation) = self.operation.take() {
                std::mem::forget(operation);
            }
        }
    }
}
pub(crate) struct ProviderResponse {
    response: reqwest::Response,
    guard: NetworkIoGuard,
}
impl ProviderResponse {
    pub(crate) fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }
    pub(crate) fn bytes_stream(
        self,
    ) -> std::pin::Pin<Box<dyn Stream<Item = Result<axum::body::Bytes, reqwest::Error>> + Send>>
    {
        let stream = Box::pin(self.response.bytes_stream());
        let guard = self.guard;
        Box::pin(futures_util::stream::unfold(
            (stream, guard),
            |(mut stream, guard)| async move {
                match stream.next().await {
                    Some(Ok(bytes)) => Some((Ok(bytes), (stream, guard))),
                    Some(Err(error)) => {
                        drop(guard);
                        Some((
                            Err(error),
                            (
                                stream,
                                NetworkIoGuard {
                                    operation: None,
                                    generation: None,
                                },
                            ),
                        ))
                    }
                    None => {
                        guard.settle();
                        None
                    }
                }
            },
        ))
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
        if let Some(body) = body
            && body.get("model").and_then(Value::as_str) != permit.model.as_deref()
        {
            return Err(TransportError::Invalid);
        }
        let method = reqwest::Method::from_bytes(permit.target.method.as_bytes())
            .map_err(|_| TransportError::Invalid)?;
        let mut builder = self
            .client
            .request(method, &permit.target.endpoint)
            .header(reqwest::header::AUTHORIZATION, permit.credential);
        if let Some(user) = permit.account_user {
            builder = builder.header("New-Api-User", user);
        }
        if let Some(body) = body {
            builder = builder.json(body);
        }
        let request = builder.build().map_err(|_| TransportError::Invalid)?;
        #[cfg(test)]
        crate::engine::runtime_barrier::block(&permit.target.endpoint, "network_before_send");
        let _call_id = permit.call_id;
        let (operation, authority) = permit.authority.ok_or(TransportError::Invalid)?;
        let generation = operation
            .generation_for(&authority)
            .map_err(|_| TransportError::Invalid)?;
        let guard = NetworkIoGuard {
            operation: Some(operation),
            generation: Some(generation),
        };
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|_| TransportError::Unknown)?;
        Ok(ProviderResponse { response, guard })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_have_one_canonical_origin_and_preserve_base_path() {
        let a = validate_target("HTTPS://Example.COM/v1///", NetworkPurpose::Completion).unwrap();
        let b = validate_target("https://example.com:443/v1", NetworkPurpose::Completion).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.origin(), "https://example.com:443");
        assert_eq!(a.base_url(), "https://example.com:443/v1");
        assert_eq!(a.endpoint(), "https://example.com:443/v1/chat/completions");
        assert_eq!(a.method(), "POST");
        let ip = validate_target("http://[::1]/", NetworkPurpose::Balance).unwrap();
        assert_eq!(ip.origin(), "http://[::1]:80");
        assert_eq!(ip.endpoint(), "http://[::1]:80/api/user/self");
        assert_eq!(ip.method(), "GET");
        assert_eq!(
            serde_json::to_string(&NetworkPurpose::Completion).unwrap(),
            "\"completion\""
        );
    }

    #[test]
    fn targets_reject_ambiguity_before_url_parser_normalizes_it() {
        for base in [
            "file:///x",
            "https:///example.com",
            "http://",
            "http://user@localhost",
            "http://@localhost",
            "http://localhost?x",
            "http://localhost#x",
            "http://localhost/a\\b",
            " http://localhost",
            "http://local\thost",
            "http://localhost/a/../b",
            "http://localhost/./b",
            "http://localhost/%2e",
            "http://localhost/.%2E/b",
            "http://localhost/%2E%2e/b",
            "http://localhost/a%2fb",
            "http://localhost/a%5Cb",
            "http://localhost/%",
            "http://localhost/%2",
            "http://localhost/%zz",
        ] {
            assert_eq!(
                validate_target(base, NetworkPurpose::Models),
                Err(TransportError::Invalid),
                "{base}"
            );
        }
        assert!(validate_target("http://localhost/a..b/%252e", NetworkPurpose::Models).is_ok());
        assert_ne!(
            validate_target("http://localhost/v1", NetworkPurpose::Models).unwrap(),
            validate_target("http://localhost/v10", NetworkPurpose::Models).unwrap()
        );
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
        assert!(matches!(
            ProviderPermit::new(
                account(),
                "k".into(),
                "c".into(),
                Some("m".into()),
                Some("u".into())
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
