use super::oauth::OAuthConfig;
use anyhow::{Context, Result, ensure};
use reqwest::Url;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    net::IpAddr,
    path::{Path, PathBuf},
};

const REGISTRY_ENV: &str = "CODA_MCP_HTTP_SERVERS";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Registration {
    workspace: PathBuf,
    name: String,
    url: String,
    bearer_token_env: Option<String>,
    oauth: Option<OAuthConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HttpConfig {
    pub(super) url: Url,
    pub(super) bearer_env: Option<String>,
    pub(super) oauth: Option<OAuthConfig>,
}

impl HttpConfig {
    pub(super) fn bearer(&self) -> Result<Option<String>> {
        let Some(name) = &self.bearer_env else {
            return Ok(None);
        };
        ensure!(
            !name.is_empty() && !name.contains(['=', '\0']),
            "MCP HTTP Bearer 环境变量名称无效"
        );
        let value = std::env::var(name).context("MCP HTTP Bearer 凭据未设置")?;
        ensure!(
            !value.is_empty() && value.bytes().all(|b| b.is_ascii_graphic()),
            "MCP HTTP Bearer 凭据无效"
        );
        Ok(Some(value))
    }
}

fn registrations(raw: &str) -> impl Iterator<Item = Registration> {
    let entries = (raw.len() as u64 <= super::MAX_CONFIG_BYTES)
        .then(|| serde_json::from_str::<Vec<serde_json::Value>>(raw).ok())
        .flatten()
        .unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|entry| serde_json::from_value(entry).ok())
}

pub(super) fn endpoint(raw: &str) -> Option<Url> {
    if raw
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let host = url.host_str()?;
    let loopback = host
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback());
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || url.scheme() == "http" && loopback)
    {
        return None;
    }
    Some(url)
}

fn for_workspace(raw: &str, workspace: &Path) -> BTreeMap<String, HttpConfig> {
    let mut matches = BTreeMap::new();
    let Ok(cwd) = workspace.canonicalize() else {
        return BTreeMap::new();
    };
    for entry in registrations(raw) {
        if !entry.workspace.is_absolute()
            || entry.workspace.canonicalize().ok().as_ref() != Some(&cwd)
            || entry.name.is_empty()
            || entry.name.len() > 120
            || super::sanitize_name(&entry.name) != entry.name
        {
            continue;
        }
        let config = endpoint(&entry.url).and_then(|url| {
            if let Some(oauth) = &entry.oauth
                && (entry.bearer_token_env.is_some() || !oauth.valid_for(&url))
            {
                return None;
            }
            Some(HttpConfig {
                url,
                bearer_env: entry.bearer_token_env,
                oauth: entry.oauth,
            })
        });
        // A duplicate registration does not select a destination by array order.
        matches
            .entry(entry.name)
            .and_modify(|previous| *previous = None)
            .or_insert(config);
    }
    matches
        .into_iter()
        .filter_map(|(name, config)| Some((name, config?)))
        .collect()
}

pub(super) fn servers(workspace: &Path) -> BTreeMap<String, HttpConfig> {
    std::env::var(REGISTRY_ENV)
        .map(|raw| for_workspace(&raw, workspace))
        .unwrap_or_default()
}

/// Host credential variables must stay out of shell and stdio subprocess environments.
pub(crate) fn bearer_env_names() -> Vec<String> {
    let Ok(raw) = std::env::var(REGISTRY_ENV) else {
        return Vec::new();
    };
    registrations(&raw)
        .filter_map(|entry| entry.bearer_token_env.map(|n| n.to_ascii_uppercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_explicit_http_destinations_match_the_exact_workspace() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("sub");
        std::fs::create_dir(&child).unwrap();
        let good = json!({"workspace":root.path(),"name":"manual","url":"https://docs.example/mcp","bearerTokenEnv":"DOCS_LOGIN"});
        let entries = json!([good, {"workspace":root.path(),"name":"invalid","url":"http://docs.example/mcp"}, {"unrelated":true}]);
        let map = for_workspace(&entries.to_string(), root.path());
        assert_eq!(map.len(), 1);
        assert_eq!(map["manual"].bearer_env.as_deref(), Some("DOCS_LOGIN"));
        assert!(for_workspace(&entries.to_string(), &child).is_empty());
        assert!(for_workspace(&json!([good, good]).to_string(), root.path()).is_empty());
    }

    #[test]
    fn oauth_registration_requires_a_trusted_issuer_and_a_single_auth_method() {
        let root = tempfile::tempdir().unwrap();
        let entry = json!({"workspace":root.path(),"name":"docs","url":"https://docs.example/mcp",
            "oauth":{"issuer":"https://auth.example/tenant","clientId":"coda","scopes":["docs.read"]}});
        let registered = |item| for_workspace(&json!([item]).to_string(), root.path());
        assert!(registered(entry.clone())["docs"].oauth.is_some());
        let mut mixed = entry.clone();
        mixed["bearerTokenEnv"] = json!("DOCS_LOGIN");
        assert!(registered(mixed).is_empty());
        for (key, value) in [
            ("issuer", json!("http://127.0.0.1/token")),
            ("clientId", json!("")),
            ("scopes", json!(["multiple scopes"])),
        ] {
            let mut invalid = entry.clone();
            invalid["oauth"][key] = value;
            assert!(registered(invalid).is_empty());
        }
    }

    #[test]
    fn endpoint_allows_tls_and_explicit_loopback_http() {
        for raw in [
            "https://mcp.example/mcp",
            "http://127.0.0.1:8000/mcp",
            "http://[::1]/mcp",
        ] {
            assert!(endpoint(raw).is_some(), "{raw}");
        }
        for raw in [
            "http://mcp.example/mcp",
            "https://user:pass@mcp.example/mcp",
            "https://mcp.example/mcp?token=abc",
            "https://mcp.example/mcp#fragment",
            "file:///local",
        ] {
            assert!(endpoint(raw).is_none(), "{raw}");
        }
    }
}
