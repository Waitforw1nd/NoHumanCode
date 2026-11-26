//! Workspace-scoped OAuth grants. The entire record is a single DPAPI secret.
use super::{Binding, Metadata, Tokens, wire};
use crate::store::Store;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, LazyLock, Mutex, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
struct Grant {
    generation: String,
    token_endpoint: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

impl Grant {
    fn new(endpoint: String, tokens: Tokens, previous_refresh: Option<String>) -> Result<Self> {
        let time = now()?;
        Ok(Self {
            generation: uuid::Uuid::new_v4().to_string(),
            token_endpoint: endpoint,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token.or(previous_refresh),
            expires_at: tokens
                .expires_in
                .map(|seconds| time.saturating_add(seconds)),
        })
    }

    fn decode(value: &str) -> Result<Self> {
        serde_json::from_str(value)
            .map_err(|_| anyhow::anyhow!("MCP OAuth 登录记录损坏，请重新登录"))
    }

    fn expired(&self) -> Result<bool> {
        let time = now()?;
        Ok(self.expires_at.is_some_and(|expiry| expiry <= time))
    }
}

pub(super) fn save_login(
    store: &Store,
    binding: &Binding,
    metadata: &Metadata,
    tokens: Tokens,
) -> Result<()> {
    let grant = Grant::new(metadata.token_endpoint.to_string(), tokens, None)?;
    store.put_oauth_secret(&binding.storage_key()?, &serde_json::to_string(&grant)?)
}

pub(in crate::mcp) struct Access {
    pub(in crate::mcp) token: String,
    pub(in crate::mcp) generation: String,
    store: Arc<Store>,
    key: String,
}

impl Access {
    // A delayed 401 belongs only to the generation actually sent.
    pub(in crate::mcp) fn rejected(&self, require_login: bool) -> Result<()> {
        if let Some(raw) = self.store.secret(&self.key)? {
            let mut grant = Grant::decode(&raw)?;
            if grant.generation == self.generation && (require_login || !grant.expired()?) {
                grant.expires_at = Some(0);
                let expired = (!require_login && grant.refresh_token.is_some())
                    .then(|| serde_json::to_string(&grant))
                    .transpose()?;
                if self
                    .store
                    .compare_exchange_secret(&self.key, &raw, expired.as_deref())?
                    && expired.is_none()
                {
                    return Err(LoginRequired.into());
                }
            }
        }
        Ok(())
    }
}

// Store takes the cross-process database lock when it opens. Different runs and
// separate Engine handles sharing that store can still refresh at the same time.
type RefreshMutex = tokio::sync::Mutex<()>;

fn refresh_mutex(key: &str) -> Arc<RefreshMutex> {
    static LOCKS: LazyLock<Mutex<HashMap<String, Weak<RefreshMutex>>>> =
        LazyLock::new(Default::default);
    let mut locks = LOCKS.lock().unwrap();
    if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
        return lock;
    }
    locks.retain(|_, lock| lock.strong_count() != 0);
    let lock = Arc::new(RefreshMutex::new(()));
    locks.insert(key.to_owned(), Arc::downgrade(&lock));
    lock
}

#[derive(Debug)]
pub(in crate::mcp) struct LoginRequired;
impl fmt::Display for LoginRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MCP OAuth 需要登录或重新登录")
    }
}
impl std::error::Error for LoginRequired {}

pub(in crate::mcp) async fn access(store: Arc<Store>, binding: &Binding) -> Result<Access> {
    let key = binding.storage_key()?;
    let mutex = refresh_mutex(&key);
    loop {
        let guard = mutex.clone().lock_owned().await;
        let raw = store.secret(&key)?.ok_or(LoginRequired)?;
        let grant = Grant::decode(&raw)?;
        if !grant.expired()? {
            return Ok(Access {
                store,
                key,
                token: grant.access_token,
                generation: grant.generation,
            });
        }
        let refresh = grant.refresh_token.ok_or(LoginRequired)?;
        let endpoint = grant
            .token_endpoint
            .parse()
            .context("MCP OAuth 登录端点无效")?;
        let store = store.clone();
        let key = key.clone();
        let binding = binding.clone();
        // A server may already have consumed a rotating refresh token when a
        // task is cancelled. Finish this bounded exchange and persist its result.
        tokio::spawn(async move {
            let _guard = guard;
            match wire::refresh(&binding, &endpoint, &refresh).await {
                Ok(tokens) => {
                    let next = Grant::new(grant.token_endpoint, tokens, Some(refresh))?;
                    let updated = serde_json::to_string(&next)?;
                    if store.compare_exchange_secret(&key, &raw, Some(&updated))?
                        && next.expired()?
                    {
                        bail!("MCP OAuth 刷新返回已过期的凭据");
                    }
                }
                Err(error) if error.is::<wire::InvalidGrant>() => {
                    if store.compare_exchange_secret(&key, &raw, None)? {
                        return Err(LoginRequired.into());
                    }
                }
                Err(error) => return Err(error),
            }
            Ok(())
        })
        .await
        .context("MCP OAuth 刷新任务中断")??;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(token: &str, refresh_token: Option<&str>, expires_in: Option<u64>) -> Tokens {
        Tokens {
            access_token: token.to_owned(),
            refresh_token: refresh_token.map(str::to_owned),
            expires_in,
        }
    }

    #[test]
    fn refreshed_grants_honor_rotation_and_optional_expiry() {
        let valid = Grant::new(
            "endpoint".into(),
            tokens("dummy-A", None, None),
            Some("dummy-R".into()),
        )
        .unwrap();
        assert!(!valid.expired().unwrap());
        assert_eq!(valid.refresh_token.as_deref(), Some("dummy-R"));
        let expired = Grant::new(
            "endpoint".into(),
            tokens("dummy-B", Some("dummy-R2"), Some(0)),
            valid.refresh_token,
        )
        .unwrap();
        assert!(expired.expired().unwrap());
        assert_eq!(expired.refresh_token.as_deref(), Some("dummy-R2"));
        assert_ne!(expired.generation, valid.generation);
    }

    fn fixture(root: &std::path::Path, endpoint: reqwest::Url) -> (Arc<Store>, Binding, Metadata) {
        let store = Arc::new(Store::open(&root.join("store.sqlite3")).unwrap());
        let meta = Metadata {
            authorization_endpoint: endpoint.join("/authorize").unwrap(),
            token_endpoint: endpoint.clone(),
        };
        let config = super::super::OAuthConfig {
            issuer: endpoint.origin().ascii_serialization(),
            client_id: "client".into(),
            scopes: vec![],
        };
        let binding =
            Binding::new(root, "fixture", &endpoint.join("/mcp").unwrap(), &config).unwrap();
        (store, binding, meta)
    }

    #[tokio::test]
    #[cfg_attr(not(windows), ignore = "requires Windows DPAPI")]
    async fn concurrent_access_rotates_one_time_refresh_token_once() {
        use axum::{Form, Json, Router, extract::State, http::StatusCode, routing::post};
        use serde_json::json;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let requests = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let app = Router::new().route("/token", post(|State(count): State<Arc<AtomicUsize>>, Form(form): Form<HashMap<String, String>>| async move {
            if count.fetch_add(1, Ordering::SeqCst) == 0 && form["refresh_token"] == "dummy-R1" {
                (StatusCode::OK, Json(json!({"token_type":"Bearer","access_token":"dummy-A2","refresh_token":"dummy-R2"})))
            } else {
                (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"})))
            }
        })).with_state(requests.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let (store, binding, meta) = fixture(dir.path(), endpoint);
        save_login(
            &store,
            &binding,
            &meta,
            tokens("dummy-A1", Some("dummy-R1"), Some(0)),
        )
        .unwrap();
        let (left, right) = tokio::try_join!(
            access(store.clone(), &binding),
            access(store.clone(), &binding)
        )
        .unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(left.generation, right.generation);
        assert_eq!(left.token, "dummy-A2");
        assert_eq!(right.token, "dummy-A2");
        let saved = store
            .secret(&binding.storage_key().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            Grant::decode(&saved).unwrap().refresh_token.as_deref(),
            Some("dummy-R2")
        );
        server.abort();
    }

    #[tokio::test]
    #[cfg_attr(not(windows), ignore = "requires Windows DPAPI")]
    async fn old_rejection_cannot_revoke_new_login_with_identical_token_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let (store, binding, meta) =
            fixture(dir.path(), "http://127.0.0.1:12345/token".parse().unwrap());
        save_login(&store, &binding, &meta, tokens("dummy-access", None, None)).unwrap();
        let old = access(store.clone(), &binding).await.unwrap();
        save_login(&store, &binding, &meta, tokens("dummy-access", None, None)).unwrap();
        old.rejected(false).unwrap();
        let new = access(store.clone(), &binding).await.unwrap();
        assert_ne!(new.generation, old.generation);
        assert!(new.rejected(false).unwrap_err().is::<LoginRequired>());
        assert!(matches!(access(store, &binding).await, Err(err) if err.is::<LoginRequired>()));
    }
}
