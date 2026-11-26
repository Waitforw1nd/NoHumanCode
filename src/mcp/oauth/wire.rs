//! OAuth discovery and public-client token requests for one registered MCP resource.
use super::{Binding, Metadata, Tokens};
use crate::mcp::http_registry::endpoint;
use anyhow::{Context, Result, anyhow, bail, ensure};
use reqwest::{Client, Response, StatusCode, Url, header};
use serde::{Deserialize, de::DeserializeOwned};
use std::{fmt, time::Duration};

const MAX_BODY: usize = 64 * 1024;

#[derive(Debug)]
pub(super) struct InvalidGrant;
impl fmt::Display for InvalidGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MCP OAuth 授权已失效，请重新登录")
    }
}
impl std::error::Error for InvalidGrant {}

fn client() -> Result<Client> {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| anyhow!("无法建立 MCP OAuth HTTP 客户端"))
}

fn registered_issuer(binding: &Binding) -> Result<Url> {
    endpoint(&binding.config.issuer).context("MCP OAuth issuer 无效")
}

fn http_url(raw: &str) -> Result<Url> {
    ensure!(
        !raw.chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\'),
        "MCP OAuth 端点格式无效"
    );
    let url = Url::parse(raw).map_err(|_| anyhow!("MCP OAuth 端点格式无效"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "MCP OAuth 端点格式无效"
    );
    Ok(url)
}

fn same_origin(raw: &str, base: &Url) -> Result<Url> {
    let url = http_url(raw)?;
    ensure!(
        url.origin() == base.origin(),
        "MCP OAuth 资源元数据端点与登记来源不符"
    );
    Ok(url)
}

// A pinned issuer may delegate to another HTTPS origin. Plain HTTP is limited
// to the same origin as an explicitly registered loopback issuer.
fn issuer_endpoint(raw: &str, issuer: &Url) -> Result<Url> {
    let url = http_url(raw)?;
    ensure!(
        url.scheme() == "https" || url.origin() == issuer.origin(),
        "MCP OAuth HTTP 端点与登记来源不符"
    );
    Ok(url)
}

fn well_known(base: &Url, suffix: &str, path: &str) -> Url {
    let mut url = base.clone();
    url.set_path(&format!("/.well-known/{suffix}{path}"));
    url
}

async fn json<T: DeserializeOwned>(mut response: Response) -> Result<T> {
    ensure!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|s| s
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")),
        "MCP OAuth 响应应为 JSON"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("MCP OAuth 响应中断或超时"))?
    {
        ensure!(chunk.len() <= MAX_BODY - body.len(), "MCP OAuth 响应过大");
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| anyhow!("MCP OAuth JSON 响应无效"))
}

async fn get(client: &Client, url: &Url, accept: &'static str) -> Result<Response> {
    client
        .get(url.clone())
        .header(header::ACCEPT, accept)
        .send()
        .await
        .map_err(|_| anyhow!("MCP OAuth 发现请求失败或超时"))
}

fn ok(response: Response) -> Result<Response> {
    ensure!(
        response.status() == StatusCode::OK,
        "MCP OAuth 请求失败 (HTTP {})",
        response.status().as_u16()
    );
    Ok(response)
}

// Commas inside quoted auth-params are data. A new scheme resets the active challenge.
fn challenge_metadata(headers: &header::HeaderMap, resource: &Url) -> Result<Option<Url>> {
    let mut metadata = None;
    for header in headers.get_all(header::WWW_AUTHENTICATE) {
        let line = header
            .to_str()
            .map_err(|_| anyhow!("MCP OAuth 挑战头无效"))?;
        let (mut quoted, mut escaped, mut bearer) = (false, false, false);
        for field in line.split(|c| {
            if escaped {
                escaped = false;
            } else if quoted && c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = !quoted;
            } else if c == ',' && !quoted {
                return true;
            }
            false
        }) {
            let field = field.trim();
            let first_end = field.find([' ', '\t', '=']).unwrap_or(field.len());
            let first = &field[..first_end];
            let rest = field[first_end..].trim_start();
            let param = if rest.starts_with('=') {
                field
            } else {
                bearer = first.eq_ignore_ascii_case("Bearer");
                rest
            };
            if let Some((name, value)) = param.split_once('=')
                && bearer
                && name.trim_end().eq_ignore_ascii_case("resource_metadata")
            {
                let value = value.trim();
                let raw = if let Some(quoted) = value.strip_prefix('"') {
                    quoted.strip_suffix('"').context("MCP OAuth 挑战头无效")?
                } else {
                    value
                };
                ensure!(metadata.is_none(), "MCP OAuth 挑战头重复指定元数据");
                metadata = Some(same_origin(raw, resource)?);
            }
        }
        ensure!(!quoted && !escaped, "MCP OAuth 挑战头无效");
    }
    Ok(metadata)
}

#[derive(Deserialize)]
struct ResourceDocument {
    resource: String,
    authorization_servers: Vec<String>,
}

#[derive(Deserialize)]
struct ServerDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    code_challenge_methods_supported: Vec<String>,
    response_types_supported: Option<Vec<String>>,
    grant_types_supported: Option<Vec<String>>,
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

async fn resource_document(client: &Client, resource: &Url) -> Result<ResourceDocument> {
    let probe = get(client, resource, "application/json, text/event-stream").await?;
    let challenge = if probe.status() == StatusCode::UNAUTHORIZED {
        challenge_metadata(probe.headers(), resource)?
    } else {
        ensure!(
            !probe.status().is_redirection(),
            "MCP OAuth 资源端点返回重定向"
        );
        None
    };
    drop(probe);
    let response = if let Some(url) = challenge {
        get(client, &url, "application/json").await?
    } else {
        let suffix = "oauth-protected-resource";
        let path = if resource.path() == "/" {
            ""
        } else {
            resource.path()
        };
        let response = get(
            client,
            &well_known(resource, suffix, path),
            "application/json",
        )
        .await?;
        if response.status() == StatusCode::NOT_FOUND && !path.is_empty() {
            get(
                client,
                &well_known(resource, suffix, ""),
                "application/json",
            )
            .await?
        } else {
            response
        }
    };
    json(ok(response)?).await
}

fn supports(list: Option<&Vec<String>>, value: &str) -> bool {
    list.is_none_or(|values| values.iter().any(|v| v == value))
}

pub(super) async fn discover(binding: &Binding) -> Result<Metadata> {
    let issuer = registered_issuer(binding)?;
    let client = client()?;
    let resource = resource_document(&client, &binding.resource).await?;
    ensure!(
        resource.resource == binding.resource.as_str()
            && resource
                .authorization_servers
                .contains(&binding.config.issuer),
        "MCP OAuth 资源元数据与登记不符"
    );

    let url = well_known(
        &issuer,
        "oauth-authorization-server",
        issuer.path().strip_suffix('/').unwrap_or(issuer.path()),
    );
    let doc: ServerDocument = json(ok(get(&client, &url, "application/json").await?)?).await?;
    ensure!(
        doc.issuer == binding.config.issuer,
        "MCP OAuth issuer 与登记不符"
    );
    ensure!(
        doc.code_challenge_methods_supported
            .iter()
            .any(|v| v == "S256")
            && supports(doc.response_types_supported.as_ref(), "code")
            && supports(doc.grant_types_supported.as_ref(), "authorization_code")
            && supports(doc.token_endpoint_auth_methods_supported.as_ref(), "none"),
        "MCP OAuth 授权服务器不支持公共客户端 S256 授权码流程"
    );
    let authorization_endpoint = issuer_endpoint(&doc.authorization_endpoint, &issuer)?;
    ensure!(
        authorization_endpoint
            .query_pairs()
            .all(|(name, _)| !matches!(
                name.as_ref(),
                "response_type"
                    | "redirect_uri"
                    | "client_id"
                    | "state"
                    | "scope"
                    | "resource"
                    | "code_challenge"
                    | "code_challenge_method"
            )),
        "MCP OAuth 授权端点重复指定请求参数"
    );
    Ok(Metadata {
        authorization_endpoint,
        token_endpoint: issuer_endpoint(&doc.token_endpoint, &issuer)?,
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    token_type: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

fn bearer(token: &str) -> bool {
    let data = token.trim_end_matches('=');
    !data.is_empty()
        && data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
}

async fn request_tokens(
    binding: &Binding,
    token_endpoint: &Url,
    grant: &[(&str, &str)],
) -> Result<Tokens> {
    let url = issuer_endpoint(token_endpoint.as_str(), &registered_issuer(binding)?)?;
    let mut form = grant.to_vec();
    form.push(("resource", binding.resource.as_str()));
    form.push(("client_id", binding.config.client_id.as_str()));
    ensure!(
        url.query_pairs()
            .all(|(name, _)| !form.iter().any(|(field, _)| *field == name)),
        "MCP OAuth 令牌端点重复指定请求参数"
    );
    let response = client()?
        .post(url)
        .header(header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|_| anyhow!("MCP OAuth 令牌请求失败或超时"))?;
    if response.status() == StatusCode::BAD_REQUEST {
        #[derive(Deserialize)]
        struct OAuthError {
            error: String,
        }
        if json::<OAuthError>(response)
            .await
            .is_ok_and(|doc| doc.error == "invalid_grant")
        {
            return Err(InvalidGrant.into());
        }
        bail!("MCP OAuth 令牌请求被拒绝 (HTTP 400)");
    }
    let doc: TokenResponse = json(ok(response)?).await?;
    ensure!(
        doc.token_type.eq_ignore_ascii_case("bearer")
            && bearer(&doc.access_token)
            && doc
                .refresh_token
                .as_ref()
                .is_none_or(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_graphic())),
        "MCP OAuth 令牌响应无效"
    );
    Ok(Tokens {
        access_token: doc.access_token,
        refresh_token: doc.refresh_token,
        expires_in: doc.expires_in,
    })
}

pub(super) async fn exchange_code(
    binding: &Binding,
    metadata: &Metadata,
    code: &str,
    verifier: &str,
    redirect: &Url,
) -> Result<Tokens> {
    request_tokens(
        binding,
        &metadata.token_endpoint,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect.as_str()),
        ],
    )
    .await
}

pub(super) async fn refresh(
    binding: &Binding,
    token_endpoint: &Url,
    refresh_token: &str,
) -> Result<Tokens> {
    request_tokens(
        binding,
        token_endpoint,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_selects_only_the_bearer_parameter() {
        let resource = Url::parse("https://mcp.example/mcp").unwrap();
        let mut headers = header::HeaderMap::new();
        headers.append(
            header::WWW_AUTHENTICATE,
            r#"Basic realm="a,b", resource_metadata="https://other.example/""#
                .parse()
                .unwrap(),
        );
        headers.append(header::WWW_AUTHENTICATE, r#"Bearer realm="quoted\",comma", Resource_Metadata = "https://mcp.example/meta?q=a,b""#.parse().unwrap());
        assert_eq!(
            challenge_metadata(&headers, &resource)
                .unwrap()
                .unwrap()
                .path(),
            "/meta"
        );
        headers.append(
            header::WWW_AUTHENTICATE,
            r#"Bearer resource_metadata="https://other.example/meta""#
                .parse()
                .unwrap(),
        );
        assert!(challenge_metadata(&headers, &resource).is_err());
    }

    #[test]
    fn issuer_allows_delegated_https_and_same_origin_loopback_http() {
        let issuer = Url::parse("https://issuer.example/tenant").unwrap();
        for valid in [
            "https://login.example/authorize",
            "https://tokens.example/token",
        ] {
            assert!(issuer_endpoint(valid, &issuer).is_ok());
        }
        for invalid in [
            "http://issuer.example/token",
            "http://127.0.0.1/token",
            "https://user@other.example/token",
            "https://other.example/token#fragment",
        ] {
            assert!(issuer_endpoint(invalid, &issuer).is_err());
        }
        let local = Url::parse("http://127.0.0.1:8000/issuer").unwrap();
        assert!(issuer_endpoint("http://127.0.0.1:8000/token", &local).is_ok());
        assert!(issuer_endpoint("http://127.0.0.1:8001/token", &local).is_err());
        assert!(issuer_endpoint("http://other.example:8000/token", &local).is_err());
    }

    #[test]
    fn endpoint_binding_and_bearer_grammar() {
        let issuer = Url::parse("https://as.example/tenant").unwrap();
        for invalid in [
            "https://as.example:8443/token",
            "http://as.example/token",
            "https://user@as.example/token",
            "https://as.example/token#private",
            "../token",
            "blob:https://as.example/token",
        ] {
            assert!(same_origin(invalid, &issuer).is_err());
        }
        assert!(bearer("token-_0.~/+="));
        for invalid in ["", "==", "a=b", "x\r\ny", "x y"] {
            assert!(!bearer(invalid));
        }
    }
}
