//! OAuth for a public client registered by the Host for a specific remote MCP service.
mod login;
pub(super) mod session;
mod wire;

use anyhow::{Context, Result};
pub use login::login;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct OAuthConfig {
    pub(super) client_id: String,
    pub(super) issuer: String,
    #[serde(default)]
    pub(super) scopes: Vec<String>,
}

impl OAuthConfig {
    pub(super) fn valid_for(&self, resource: &Url) -> bool {
        super::http_registry::endpoint(&self.issuer).is_some_and(|url| {
            (url.scheme() == "https" || resource.scheme() == "http")
                && !self.client_id.trim().is_empty()
                && self.client_id.len() <= 1024
                && !self.client_id.chars().any(char::is_control)
                && self.scopes.len() <= 64
                && self.scopes.iter().all(|scope| {
                    !scope.is_empty()
                        && scope.len() <= 256
                        && scope
                            .bytes()
                            .all(|b| matches!(b, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
                })
        })
    }
}

#[derive(Clone)]
pub(super) struct Binding {
    pub(super) workspace: PathBuf,
    pub(super) server: String,
    pub(super) resource: Url,
    pub(super) config: OAuthConfig,
}

impl Binding {
    pub(super) fn new(
        workspace: &Path,
        server: &str,
        resource: &Url,
        config: &OAuthConfig,
    ) -> Result<Self> {
        Ok(Self {
            workspace: workspace
                .canonicalize()
                .context("无法定位 MCP OAuth 工作区")?,
            server: server.to_owned(),
            resource: resource.clone(),
            config: config.clone(),
        })
    }

    pub(super) fn storage_key(&self) -> Result<String> {
        let fields = (
            &self.workspace,
            &self.server,
            self.resource.as_str(),
            &self.config,
        );
        let digest = Sha256::digest(serde_json::to_vec(&fields)?);
        Ok(format!("mcp-oauth::{digest:x}"))
    }
}

fn binding(workspace: &Path, server: &str) -> Result<Binding> {
    let entry = super::http_registry::servers(workspace)
        .remove(server)
        .context("此工作区未登记该 MCP HTTP 服务，请检查 CODA_MCP_HTTP_SERVERS")?;
    let config = entry.oauth.context("该 MCP 服务未配置 OAuth")?;
    Binding::new(workspace, server, &entry.url, &config)
}

struct Metadata {
    authorization_endpoint: Url,
    token_endpoint: Url,
}

// Token responses are never formatted in errors, terminal output or task events.
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}
