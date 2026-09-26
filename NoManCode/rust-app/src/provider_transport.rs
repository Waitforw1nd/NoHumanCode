//! Fixed, authorized Provider requests. Authorization and durable claims live in the Host.
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
        })
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
    ) -> Result<reqwest::Response, TransportError> {
        if (permit.target.purpose == NetworkPurpose::Completion) != body.is_some() {
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
        let _call_id = permit.call_id;
        self.client
            .execute(request)
            .await
            .map_err(|_| TransportError::Unknown)
    }
}
