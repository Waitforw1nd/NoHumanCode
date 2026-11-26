use super::{binding, session, wire};
use crate::store::Store;
use anyhow::{Context, Result, bail};
use axum::{
    Router,
    http::{HeaderMap, StatusCode, Uri, header},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;
use sha2::{Digest, Sha256};
use std::{
    net::Ipv4Addr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub async fn login(
    store: &Store,
    workspace: &Path,
    server: &str,
    callback_port: u16,
) -> Result<()> {
    let binding = binding(workspace, server)?;
    let flow = async {
        let metadata = wire::discover(&binding).await?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, callback_port))
            .await
            .context("无法监听 MCP OAuth 本机回调端口")?;
        let authority = listener.local_addr()?.to_string();
        let redirect = Url::parse(&format!("http://{authority}/callback"))?;
        let state = Uuid::new_v4().simple().to_string();
        let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = metadata.authorization_endpoint.clone();
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &binding.config.client_id)
                .append_pair("redirect_uri", redirect.as_str())
                .append_pair("state", &state)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("resource", binding.resource.as_str());
            if !binding.config.scopes.is_empty() {
                query.append_pair("scope", &binding.config.scopes.join(" "));
            }
        }
        eprintln!("请在本机浏览器打开以下地址授权，按 Ctrl+C 取消。\n{url}");
        let code =
            wait_for_callback(listener, authority, state, binding.config.issuer.clone()).await?;
        let tokens = wire::exchange_code(&binding, &metadata, &code, &verifier, &redirect).await?;
        Ok::<_, anyhow::Error>((metadata, tokens))
    };
    let (metadata, tokens) = tokio::select! {
        biased;
        _ = tokio::signal::ctrl_c() => bail!("MCP OAuth 登录已取消"),
        result = tokio::time::timeout(Duration::from_secs(300), flow) =>
            result.context("MCP OAuth 登录超时，请重新登录")??,
    };
    session::save_login(store, &binding, &metadata, tokens)
}

enum Callback {
    Code(String),
    Denied,
    AuthorizationError,
}

fn parse_callback(uri: &Uri, state: &str, issuer: &str) -> Option<Callback> {
    let mut fields = std::collections::HashMap::new();
    let pairs = axum::extract::Query::<Vec<(String, String)>>::try_from_uri(uri).ok()?;
    for (key, value) in pairs.0 {
        if matches!(key.as_str(), "state" | "code" | "error" | "iss")
            && fields.insert(key, value).is_some()
        {
            return None;
        }
    }
    if fields.get("state")? != state || fields.get("iss").is_some_and(|value| value != issuer) {
        return None;
    }
    match (fields.remove("code"), fields.remove("error")) {
        (Some(code), None)
            if !code.is_empty() && code.bytes().all(|b| (0x20..=0x7e).contains(&b)) =>
        {
            Some(Callback::Code(code))
        }
        (None, Some(error)) if error == "access_denied" => Some(Callback::Denied),
        (None, Some(error))
            if !error.is_empty()
                && error
                    .bytes()
                    .all(|b| matches!(b, 0x20..=0x21 | 0x23..=0x5b | 0x5d..=0x7e)) =>
        {
            Some(Callback::AuthorizationError)
        }
        _ => None,
    }
}

async fn wait_for_callback(
    listener: TcpListener,
    authority: String,
    state: String,
    issuer: String,
) -> Result<String> {
    let (send, receive) = oneshot::channel();
    let send = Arc::new(Mutex::new(Some(send)));
    let router = Router::new().route(
        "/callback",
        get(
            move |uri: Uri, headers: HeaderMap, method: axum::http::Method| {
                let sender = send.clone();
                let result = if method != axum::http::Method::GET
                    || headers
                        .get(header::HOST)
                        .and_then(|host| host.to_str().ok())
                        != Some(authority.as_str())
                {
                    None
                } else {
                    parse_callback(&uri, &state, &issuer)
                };
                async move {
                    let mut status = StatusCode::BAD_REQUEST;
                    if let Some(callback) = result
                        && let Some(send) = sender.lock().unwrap().take()
                    {
                        let granted = matches!(callback, Callback::Code(_));
                        if send.send(callback).is_ok() && granted {
                            status = StatusCode::OK;
                        }
                    }
                    (
                        status,
                        [
                            (header::CACHE_CONTROL, "no-store"),
                            (header::REFERRER_POLICY, "no-referrer"),
                        ],
                        "Return to Coda CLI for the login result.",
                    )
                }
            },
        ),
    );
    let stop = CancellationToken::new();
    let serve = std::future::IntoFuture::into_future(
        axum::serve(listener, router).with_graceful_shutdown(stop.clone().cancelled_owned()),
    );
    tokio::pin!(serve);
    let result = tokio::select! {
        result = receive => result.context("MCP OAuth 回调中断")?,
        result = &mut serve => { result?; bail!("MCP OAuth 回调监听已结束") }
    };
    stop.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), serve).await;
    match result {
        Callback::Code(code) => Ok(code),
        Callback::Denied => bail!("MCP OAuth 授权已被拒绝或取消"),
        Callback::AuthorizationError => bail!("MCP OAuth 授权服务器未能完成授权"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loopback_receives_only_a_matching_unambiguous_authorization_code() {
        for (query, expected) in [
            ("state=nonce&code=a%2Bb", Some("a+b")),
            ("state=nonce&error=access_denied", None),
            (
                "state=nonce&iss=https%3A%2F%2Fissuer&error=invalid_scope&error_description=private",
                None,
            ),
        ] {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let authority = listener.local_addr().unwrap().to_string();
            let url = format!("http://{authority}/callback");
            let callback = tokio::spawn(wait_for_callback(
                listener,
                authority,
                "nonce".into(),
                "https://issuer".into(),
            ));
            let client = reqwest::Client::new();
            for invalid in [
                "state=wrong&code=private",
                "code=private",
                "state=nonce&code=a&code=b",
                "state=nonce&iss=https%3A%2F%2Funexpected&code=private",
                "state=wrong&error=access_denied",
                "state=nonce&error=access_denied&code=private",
                "state=nonce&code=",
                "state=nonce&error=%0A",
            ] {
                let response = client.get(format!("{url}?{invalid}")).send().await.unwrap();
                assert_eq!(response.status(), StatusCode::BAD_REQUEST);
                assert!(!response.text().await.unwrap().contains("private"));
            }
            let response = client.get(format!("{url}?{query}")).send().await.unwrap();
            assert_eq!(response.status().is_success(), expected.is_some());
            let code = callback.await.unwrap();
            if query.contains("invalid_scope") {
                assert_eq!(
                    code.unwrap_err().to_string(),
                    "MCP OAuth 授权服务器未能完成授权"
                );
            } else {
                assert_eq!(code.ok().as_deref(), expected);
            }
        }
    }
}
