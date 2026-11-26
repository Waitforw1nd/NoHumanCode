//! Model Context Protocol client for workspace stdio and Host-registered HTTP servers.

mod http;
mod http_registry;
pub mod oauth;
pub(crate) use http_registry::bearer_env_names;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use tokio_util::sync::CancellationToken;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SKIPPED_LINES: usize = 128;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct McpConfigFile {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ServerConfig {
    Stdio(McpServerConfig),
    Http(http_registry::HttpConfig),
}

impl ServerConfig {
    fn digest(&self) -> String {
        let value = match self {
            Self::Stdio(cfg) => json!({"stdio": cfg}),
            Self::Http(cfg) if cfg.oauth.is_some() => {
                json!({"http": cfg.url.as_str(), "oauth": cfg.oauth})
            }
            Self::Http(cfg) => json!({"http": cfg.url.as_str(), "bearer_env": cfg.bearer_env}),
        };
        format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToolTarget {
    server: String,
    raw: String,
    config_digest: String,
    pub(crate) endpoint: Option<String>,
}

pub(crate) enum CallBinding<'a> {
    Public,
    LegacyStdio,
    Approved(&'a ToolTarget),
}

impl CallBinding<'_> {
    fn allows(&self, name: &str, cfg: &ServerConfig) -> bool {
        match self {
            Self::Public => true,
            Self::LegacyStdio => matches!(cfg, ServerConfig::Stdio(_)),
            Self::Approved(target) => target.server == name && target.config_digest == cfg.digest(),
        }
    }
}

enum Transport {
    Stdio(Box<McpSession>),
    Http(Box<http::HttpSession>),
}

impl Transport {
    async fn connect(
        cwd: &Path,
        config: &ServerConfig,
        access: Option<Arc<oauth::session::Access>>,
    ) -> Result<Self> {
        match config {
            ServerConfig::Stdio(cfg) => {
                Ok(Self::Stdio(Box::new(McpSession::connect(cwd, cfg).await?)))
            }
            ServerConfig::Http(cfg) => Ok(Self::Http(Box::new(
                http::HttpSession::connect(&cfg.url, cfg.bearer()?.as_deref(), access).await?,
            ))),
        }
    }

    fn is_alive(&mut self) -> Result<bool> {
        match self {
            Self::Stdio(s) => Ok(s.child.try_wait()?.is_none()),
            Self::Http(_) => Ok(true),
        }
    }

    async fn list_response(&mut self) -> Result<Value> {
        match self {
            Self::Stdio(s) => s.list_response().await,
            Self::Http(s) => s.list_response().await,
        }
    }

    async fn call_tool(&mut self, raw: &str, args: &Value) -> Result<Value> {
        match self {
            Self::Stdio(s) => s.call_tool(raw, args).await,
            Self::Http(s) => s.call_tool(raw, args).await,
        }
    }

    async fn close(&mut self) {
        match self {
            Self::Stdio(s) => s.close().await,
            Self::Http(s) => {
                s.close().await;
            }
        }
    }
}

/// Sanitizes an arbitrary MCP server or tool identifier to `[a-zA-Z0-9_-]`.
pub fn sanitize_name(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Returns true if `name` is a syntactically valid `mcp__<server>__<tool>` identifier.
pub fn is_valid_qualified_name(name: &str) -> bool {
    if name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return false;
    }
    let Some(rest) = name.strip_prefix("mcp__") else {
        return false;
    };
    let Some((server, tool)) = rest.split_once("__") else {
        return false;
    };
    !server.is_empty() && !tool.is_empty()
}

/// Checks for workspace stdio configuration or a Host-registered HTTP server.
pub fn has_config(workspace: &Path) -> bool {
    let coda_dir = workspace.join(".coda");
    let has_coda = is_safe_dir(&coda_dir) && is_safe_regular_file(&coda_dir.join("mcp.json"));
    let has_dot = is_safe_regular_file(&workspace.join(".mcp.json"));
    has_coda || has_dot || !http_registry::servers(workspace).is_empty()
}

/// Checks whether `qualified_name` maps to a configured, enabled MCP server in `workspace`.
pub fn has_configured_server(workspace: &Path, qualified_name: &str) -> bool {
    if !is_valid_qualified_name(qualified_name) || !has_config(workspace) {
        return false;
    }
    let Ok(servers) = load_servers(workspace) else {
        return false;
    };
    servers
        .keys()
        .any(|name| tool_suffix(name, qualified_name).is_some())
}

const MAX_SESSIONS: usize = 32;

#[derive(Default)]
pub(crate) struct McpPool {
    state: Mutex<PoolState>,
    store: Option<Arc<crate::store::Store>>,
}

#[derive(Default)]
struct PoolState {
    closed: bool,
    // Least recently used first. Only idle entries can be evicted for capacity.
    entries: VecDeque<Arc<Entry>>,
}

struct Entry {
    run_id: String,
    store: Option<Arc<crate::store::Store>>,
    retired: CancellationToken,
    cwd: PathBuf,
    server: String,
    config: ServerConfig,
    connection: tokio::sync::Mutex<Option<Connection>>,
}

struct Connection {
    session: Transport,
    definitions: Vec<Value>,
    raw_names: BTreeMap<String, String>,
}

impl Entry {
    async fn access(&self) -> Result<Option<Arc<oauth::session::Access>>> {
        let ServerConfig::Http(cfg) = &self.config else {
            return Ok(None);
        };
        let Some(auth) = &cfg.oauth else {
            return Ok(None);
        };
        let store = self.store.clone().context("MCP OAuth 需要 Host 凭据存储")?;
        let binding = oauth::Binding::new(&self.cwd, &self.server, &cfg.url, auth)?;
        Ok(Some(Arc::new(
            oauth::session::access(store, &binding).await?,
        )))
    }

    async fn list(
        &self,
        access: Option<Arc<oauth::session::Access>>,
    ) -> Result<(Transport, Vec<Value>)> {
        let mut session = Transport::connect(&self.cwd, &self.config, access).await?;
        let tools = listed_tools(session.list_response().await?)?;
        Ok((session, tools))
    }

    async fn connect(&self, slot: &mut Option<Connection>) -> Result<Connection> {
        ensure!(
            !self.retired.is_cancelled()
                && load_servers(&self.cwd)?.get(&self.server) == Some(&self.config),
            "MCP 服务配置已更改"
        );
        // Keep the transport out of the cache while an exchange is in flight.
        // Cancellation discards the connection along with its partial response.
        let cached = slot.take();
        let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let access = self.access().await?;
            if let Some(mut cached) = cached
                && cached.session.is_alive()?
                && match &cached.session {
                    Transport::Http(session) => {
                        session.generation() == access.as_ref().map(|a| a.generation.as_str())
                    }
                    Transport::Stdio(_) => true,
                }
            {
                return Ok(cached);
            }
            // Only initialization and directory discovery can be repeated.
            let (session, tools) = match self.list(access).await {
                Err(err) if err.is::<http::OAuthRejected>() => {
                    let fresh = self.access().await?;
                    match self.list(fresh.clone()).await {
                        Err(err) if err.is::<http::OAuthRejected>() => {
                            if let Some(access) = fresh {
                                access.rejected(true)?;
                            }
                            return Err(oauth::session::LoginRequired.into());
                        }
                        result => result?,
                    }
                }
                result => result?,
            };
            let prefix = sanitize_name(&self.server);
            let mut definitions = Vec::new();
            let mut raw_names = BTreeMap::new();
            for tool in tools {
                let Some(raw) = tool["name"].as_str() else {
                    continue;
                };
                let qualified = format!("mcp__{prefix}__{}", sanitize_name(raw));
                if prefix.is_empty()
                    || raw.is_empty()
                    || !is_valid_qualified_name(&qualified)
                    || raw_names.contains_key(&qualified)
                {
                    continue;
                }
                let description = tool["description"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("MCP tool {raw} from {}", self.server));
                definitions.push(json!({"type":"function", "function":{
                    "name":qualified, "description":description,
                    "parameters":normalize_input_schema(tool.get("inputSchema"))
                }}));
                raw_names.insert(qualified, raw.to_owned());
            }
            Ok(Connection {
                session,
                definitions,
                raw_names,
            })
        });
        tokio::select! {
            biased;
            _ = self.retired.cancelled() => bail!("MCP 会话已关闭"),
            result = handshake => result.context("MCP 服务握手或工具发现超时 (15s)")?,
        }
    }
}

impl McpPool {
    pub(crate) fn with_store(store: Arc<crate::store::Store>) -> Self {
        Self {
            store: Some(store),
            ..Self::default()
        }
    }

    fn servers(&self, cwd: &Path) -> Result<BTreeMap<String, ServerConfig>> {
        let mut state = self.state.lock().unwrap();
        ensure!(!state.closed, "MCP 会话池已关闭");
        let servers = load_servers(cwd);
        state.entries.retain(|entry| {
            if entry.cwd == cwd
                && servers.as_ref().ok().and_then(|s| s.get(&entry.server)) != Some(&entry.config)
            {
                entry.retired.cancel();
            }
            !entry.retired.is_cancelled() || Arc::strong_count(entry) > 1
        });
        servers
    }

    fn entry(
        &self,
        run_id: &str,
        cwd: &Path,
        name: &str,
        config: &ServerConfig,
    ) -> Result<Arc<Entry>> {
        let mut state = self.state.lock().unwrap();
        ensure!(!state.closed, "MCP 会话池已关闭");
        let entries = &mut state.entries;
        let entry = if let Some(index) = entries.iter().position(|e| {
            !e.retired.is_cancelled()
                && e.run_id == run_id
                && e.cwd == cwd
                && e.server == name
                && &e.config == config
        }) {
            entries.remove(index).unwrap()
        } else {
            if entries.len() == MAX_SESSIONS {
                let idle = entries
                    .iter()
                    .position(|e| Arc::strong_count(e) == 1)
                    .context("MCP 会话池已满，所有连接正在使用")?;
                entries.remove(idle);
            }
            Arc::new(Entry {
                retired: CancellationToken::new(),
                store: self.store.clone(),
                run_id: run_id.to_owned(),
                cwd: cwd.to_owned(),
                server: name.to_owned(),
                config: config.clone(),
                connection: Default::default(),
            })
        };
        entries.push_back(entry.clone());
        Ok(entry)
    }

    pub(crate) async fn discover_tools(&self, run_id: &str, workspace: &Path) -> Vec<Value> {
        self.discover_targets(run_id, workspace, &mut Vec::new())
            .await
            .into_iter()
            .map(|(definition, _)| definition)
            .collect()
    }

    pub(crate) async fn discover_targets(
        &self,
        run_id: &str,
        workspace: &Path,
        login_required: &mut Vec<String>,
    ) -> Vec<(Value, ToolTarget)> {
        let mut definitions = Vec::new();
        let mut seen = HashSet::new();
        let Ok(cwd) = clean_workspace_cwd(workspace) else {
            return definitions;
        };
        let Ok(servers) = self.servers(&cwd) else {
            return definitions;
        };
        for (name, config) in &servers {
            let Ok(entry) = self.entry(run_id, &cwd, name, config) else {
                continue;
            };
            let mut slot = entry.connection.lock().await;
            let connection = match entry.connect(&mut slot).await {
                Ok(connection) => connection,
                Err(error) => {
                    if error.is::<oauth::session::LoginRequired>() {
                        login_required.push(name.clone());
                    }
                    continue;
                }
            };
            let digest = config.digest();
            for definition in &connection.definitions {
                let qualified = definition["function"]["name"].as_str().unwrap();
                if http_owner(&servers, qualified).is_some_and(|owner| owner != name)
                    || !seen.insert(qualified.to_owned())
                {
                    continue;
                }
                let target = ToolTarget {
                    server: name.clone(),
                    raw: connection.raw_names[qualified].clone(),
                    config_digest: digest.clone(),
                    endpoint: match config {
                        ServerConfig::Http(cfg) => Some(cfg.url.to_string()),
                        _ => None,
                    },
                };
                definitions.push((definition.clone(), target));
            }
            *slot = Some(connection);
        }
        definitions
    }

    pub(crate) async fn call_tool(
        &self,
        run_id: &str,
        workspace: &Path,
        qualified_name: &str,
        arguments: &Value,
    ) -> Result<Value> {
        self.call_tool_cancellable(
            run_id,
            workspace,
            qualified_name,
            arguments,
            &CancellationToken::new(),
        )
        .await
    }

    pub(crate) async fn call_tool_cancellable(
        &self,
        run_id: &str,
        workspace: &Path,
        qualified_name: &str,
        arguments: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.call_bound(
            run_id,
            workspace,
            qualified_name,
            arguments,
            cancel,
            CallBinding::Public,
        )
        .await
    }

    pub(crate) async fn call_bound(
        &self,
        run_id: &str,
        workspace: &Path,
        qualified_name: &str,
        arguments: &Value,
        cancel: &CancellationToken,
        binding: CallBinding<'_>,
    ) -> Result<Value> {
        ensure!(
            is_valid_qualified_name(qualified_name),
            "无效的 MCP 工具限定名: {qualified_name}"
        );
        let cwd = clean_workspace_cwd(workspace)?;
        let servers = self.servers(&cwd)?;
        let owner = http_owner(&servers, qualified_name);
        let mut unavailable = None;
        for (name, cfg) in servers
            .iter()
            .filter(|(name, _)| tool_suffix(name, qualified_name).is_some())
        {
            if owner.is_some_and(|owner| owner != name) || !binding.allows(name, cfg) {
                continue;
            }
            let entry = self.entry(run_id, &cwd, name, cfg)?;
            let ready: Result<_> = tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("任务已停止"),
                result = async {
                    let mut slot = entry.connection.lock().await;
                    let connection = entry.connect(&mut slot).await?;
                    Ok((slot, connection))
                } => result,
            };
            let (mut slot, mut connection) = match ready {
                Ok(ready) => ready,
                Err(error) => {
                    unavailable = Some(error);
                    continue;
                }
            };
            let Some(raw) = connection.raw_names.get(qualified_name) else {
                *slot = Some(connection);
                continue;
            };
            ensure!(
                !matches!(binding, CallBinding::Approved(target) if target.raw != *raw),
                "已审批的 MCP 工具绑定已更改"
            );
            verify_current_target(&cwd, name, qualified_name, cfg)?;
            let response = dispatch_call(
                &mut connection.session,
                raw,
                arguments,
                cancel,
                &entry.retired,
            )
            .await?;
            *slot = Some(connection);
            return rpc_result(response);
        }
        Err(unavailable.unwrap_or_else(|| anyhow::anyhow!("MCP 服务未提供工具: {qualified_name}")))
    }

    pub(crate) async fn close(&self) {
        let entries = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            for entry in &state.entries {
                entry.retired.cancel();
            }
            std::mem::take(&mut state.entries)
        };
        futures_util::future::join_all(entries.into_iter().map(|entry| async move {
            if let Some(mut connection) = entry.connection.lock().await.take() {
                connection.session.close().await;
            }
        }))
        .await;
    }
}

pub async fn discover_tools(workspace: &Path) -> Vec<Value> {
    let pool = McpPool::default();
    let definitions = pool.discover_tools("", workspace).await;
    pool.close().await;
    definitions
}

pub async fn call_tool(workspace: &Path, qualified_name: &str, arguments: &Value) -> Result<Value> {
    ensure!(
        is_valid_qualified_name(qualified_name),
        "无效的 MCP 工具限定名: {qualified_name}"
    );
    let cwd = clean_workspace_cwd(workspace)?;
    let servers = load_servers(&cwd)?;
    let mut matches = servers.iter().filter_map(|(name, cfg)| {
        tool_suffix(name, qualified_name).map(|suffix| (name, suffix, cfg))
    });
    let (name, suffix, cfg) = matches.next().context("未找到 MCP 服务配置")?;
    if matches.next().is_some() || matches!(cfg, ServerConfig::Http(_)) {
        let pool = McpPool::default();
        let result = pool
            .call_tool("", workspace, qualified_name, arguments)
            .await;
        pool.close().await;
        return result;
    }
    let connect = || async {
        verify_current_target(&cwd, name, qualified_name, cfg)?;
        tokio::time::timeout(HANDSHAKE_TIMEOUT, Transport::connect(&cwd, cfg, None))
            .await
            .context("MCP 服务握手超时 (15s)")?
    };
    let mut session = connect().await?;
    let mut raw = suffix.to_owned();
    if suffix.contains('_') {
        // A server can support calls without enumeration. An explicit JSON-RPC
        // error still leaves the stream synchronized for a single direct call.
        let reply = match tokio::time::timeout(HANDSHAKE_TIMEOUT, session.list_response()).await {
            Ok(Ok(reply)) => Some(reply),
            Ok(Err(error))
                if !error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof) =>
            {
                return Err(error);
            }
            // An unanswered read-only probe may leave a late reply in stdout.
            // Dispatch the original tool name once on a fresh connection.
            _ => {
                session.close().await;
                session = connect().await?;
                None
            }
        };
        if let Some(reply) = reply
            && !is_rpc_error(&reply)
        {
            let tools = listed_tools(reply)?;
            if let Some(name) = tools
                .iter()
                .filter_map(|t| t["name"].as_str())
                .find(|name| sanitize_name(name) == suffix)
            {
                raw = name.to_owned();
            }
        }
    }
    verify_current_target(&cwd, name, qualified_name, cfg)?;
    let token = CancellationToken::new();
    let result = dispatch_call(&mut session, &raw, arguments, &token, &token).await;
    session.close().await;
    rpc_result(result?)
}

#[derive(Debug)]
pub(crate) struct McpOutcomeUnknown;

impl std::fmt::Display for McpOutcomeUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MCP 工具已开始派发，执行结果未知")
    }
}
impl std::error::Error for McpOutcomeUnknown {}

async fn dispatch_call(
    session: &mut Transport,
    raw: &str,
    arguments: &Value,
    cancel: &CancellationToken,
    retired: &CancellationToken,
) -> Result<Value> {
    let empty = json!({});
    let args = if arguments.is_object() {
        arguments
    } else {
        &empty
    };
    let mut started = false;
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(anyhow::anyhow!("任务已停止")),
        _ = retired.cancelled() => Err(anyhow::anyhow!("MCP 会话已关闭")),
        result = async {
            started = true;
            let response = tokio::time::timeout(CALL_TIMEOUT, session.call_tool(raw, args)).await
                .context("MCP 工具执行超时 (60s)")??;
            ensure!(is_rpc_error(&response) || response.get("result").is_some(), "MCP 响应缺少 result");
            Ok(response)
        } => result,
    };
    if started
        && !result
            .as_ref()
            .is_err_and(|e| e.is::<http::SessionExpired>())
    {
        result.context(McpOutcomeUnknown)
    } else {
        result
    }
}

fn is_safe_dir(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    meta.is_dir() && !is_link_or_reparse_point(&meta)
}

fn is_safe_regular_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    meta.is_file() && meta.len() <= MAX_CONFIG_BYTES && !is_link_or_reparse_point(&meta)
}

fn is_link_or_reparse_point(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

fn read_config_file(path: &Path) -> Result<McpConfigFile> {
    ensure!(is_safe_regular_file(path), "MCP 配置文件不可安全读取");
    let raw = std::fs::read_to_string(path).context("无法读取 MCP 配置文件")?;
    serde_json::from_str::<McpConfigFile>(&raw).context("无效的 MCP 配置 JSON")
}

fn http_owner<'a>(servers: &'a BTreeMap<String, ServerConfig>, qualified: &str) -> Option<&'a str> {
    servers
        .iter()
        .filter(|(name, cfg)| {
            matches!(cfg, ServerConfig::Http(_)) && tool_suffix(name, qualified).is_some()
        })
        .max_by_key(|(name, _)| sanitize_name(name).len())
        .map(|(name, _)| name.as_str())
}

fn verify_current_target(
    cwd: &Path,
    name: &str,
    qualified: &str,
    config: &ServerConfig,
) -> Result<()> {
    let current = load_servers(cwd)?;
    ensure!(
        current.get(name) == Some(config)
            && http_owner(&current, qualified).is_none_or(|owner| owner == name),
        "MCP 工具目标配置已更改"
    );
    Ok(())
}

fn load_servers(workspace: &Path) -> Result<BTreeMap<String, ServerConfig>> {
    merge_servers(
        load_stdio_servers(workspace),
        http_registry::servers(workspace),
    )
}

fn merge_servers(
    stdio: Result<BTreeMap<String, McpServerConfig>>,
    http: BTreeMap<String, http_registry::HttpConfig>,
) -> Result<BTreeMap<String, ServerConfig>> {
    let mut merged = BTreeMap::new();
    let local = match stdio {
        Ok(local) => local,
        Err(error) if http.is_empty() => return Err(error),
        Err(_) => BTreeMap::new(), // An invalid workspace file cannot hide Host registrations.
    };
    for (name, cfg) in local {
        let local_prefix = format!("{}__", sanitize_name(&name));
        if !http
            .keys()
            .any(|host| local_prefix.starts_with(&format!("{host}__")))
        {
            merged.insert(name, ServerConfig::Stdio(cfg));
        }
    }
    merged.extend(
        http.into_iter()
            .map(|(name, cfg)| (name, ServerConfig::Http(cfg))),
    );
    Ok(merged)
}

fn load_stdio_servers(workspace: &Path) -> Result<BTreeMap<String, McpServerConfig>> {
    let mut merged = BTreeMap::new();
    let dot_cfg = workspace.join(".mcp.json");
    if is_safe_regular_file(&dot_cfg) {
        let parsed = read_config_file(&dot_cfg)?;
        merged.extend(parsed.mcp_servers);
    }
    let coda_dir = workspace.join(".coda");
    let coda_cfg = coda_dir.join("mcp.json");
    if is_safe_dir(&coda_dir) && is_safe_regular_file(&coda_cfg) {
        let parsed = read_config_file(&coda_cfg)?;
        merged.extend(parsed.mcp_servers);
    }
    merged.retain(|name, cfg| {
        !cfg.disabled
            && !name.trim().is_empty()
            && !cfg.command.trim().is_empty()
            && !cfg.command.contains('\0')
            && cfg.args.iter().all(|a| !a.contains('\0'))
    });
    Ok(merged)
}

fn tool_suffix<'a>(server: &str, qualified: &'a str) -> Option<&'a str> {
    qualified
        .strip_prefix(&format!("mcp__{}__", sanitize_name(server)))
        .filter(|s| !s.is_empty())
}

fn normalize_input_schema(schema: Option<&Value>) -> Value {
    if let Some(Value::Object(map)) = schema {
        let mut normalized = map.clone();
        if !normalized.contains_key("type") {
            normalized.insert("type".into(), Value::String("object".into()));
        }
        if normalized.get("type").and_then(Value::as_str) == Some("object")
            && !normalized.contains_key("properties")
        {
            normalized.insert("properties".into(), json!({}));
        }
        Value::Object(normalized)
    } else {
        json!({
            "type": "object",
            "properties": {}
        })
    }
}

fn clean_workspace_cwd(workspace: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(workspace).context("MCP 工作目录不存在")?;
    #[cfg(windows)]
    {
        let raw = canonical.to_str().context("MCP 工作目录不是有效 UTF-8")?;
        let stripped = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
            rest.to_owned()
        } else {
            raw.to_owned()
        };
        let cwd = PathBuf::from(stripped);
        ensure!(
            cwd.canonicalize().context("MCP 启动目录不可用")? == canonical,
            "MCP 启动目录与真实工作区不一致"
        );
        Ok(cwd)
    }
    #[cfg(not(windows))]
    {
        Ok(canonical)
    }
}

fn is_sensitive_env_var(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.contains("KEY") || upper.contains("SECRET") || upper.contains("TOKEN")
}

fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(raw) = path.to_str() {
            if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{unc}"));
            }
            if let Some(rest) = raw.strip_prefix(r"\\?\") {
                return PathBuf::from(rest);
            }
        }
    }
    path
}

fn resolve_mcp_program_and_args(
    command: &str,
    args: &[String],
    workspace_cwd: &Path,
) -> (PathBuf, Vec<String>) {
    let trimmed = command.trim();
    let lower = trimmed.to_ascii_lowercase();
    let is_ps = matches!(
        lower.as_str(),
        "pwsh" | "pwsh.exe" | "powershell" | "powershell.exe"
    );
    let mut effective_args = Vec::with_capacity(args.len() + 2);
    if is_ps {
        let has_file = args.iter().any(|a| a.eq_ignore_ascii_case("-file"));
        let has_exec_policy = args
            .iter()
            .any(|a| a.eq_ignore_ascii_case("-executionpolicy"));
        if has_file && !has_exec_policy {
            effective_args.push("-ExecutionPolicy".to_owned());
            effective_args.push("Bypass".to_owned());
        }
    }
    effective_args.extend(args.iter().cloned());

    if is_ps {
        let workspace_canon =
            std::fs::canonicalize(workspace_cwd).unwrap_or_else(|_| workspace_cwd.to_path_buf());
        if let Some(paths) = std::env::var_os("PATH") {
            let candidates: &[&str] = if lower.starts_with("pwsh") {
                if cfg!(windows) {
                    &["pwsh.exe", "pwsh"]
                } else {
                    &["pwsh"]
                }
            } else if cfg!(windows) {
                &["powershell.exe"]
            } else {
                &["pwsh", "powershell"]
            };
            for dir in std::env::split_paths(&paths).filter(|d| d.is_absolute()) {
                for bin in candidates {
                    if let Ok(canon) = dir.join(bin).canonicalize()
                        && canon.is_file()
                        && !canon.starts_with(&workspace_canon)
                    {
                        return (strip_verbatim_prefix(canon), effective_args);
                    }
                }
            }
        }
        if let Ok(fallback) = crate::command_execution::resolve_powershell(&workspace_canon) {
            return (strip_verbatim_prefix(fallback), effective_args);
        }
    }
    (PathBuf::from(trimmed), effective_args)
}

fn is_rpc_error(response: &Value) -> bool {
    response["error"]["code"].as_i64().is_some() && response["error"]["message"].as_str().is_some()
}

fn listed_tools(response: Value) -> Result<Vec<Value>> {
    rpc_result(response)?["tools"]
        .as_array()
        .cloned()
        .context("MCP tools/list 响应缺少 tools 数组")
}

fn rpc_result(response: Value) -> Result<Value> {
    if let Some(err) = response.get("error").filter(|v| !v.is_null()) {
        bail!("MCP JSON-RPC 错误: {err}");
    }
    response
        .get("result")
        .cloned()
        .context("MCP 响应缺少 result")
}

struct McpSession {
    child: Child,
    next_id: u64,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl McpSession {
    async fn connect(cwd: &Path, server: &McpServerConfig) -> Result<Self> {
        let (program, effective_args) =
            resolve_mcp_program_and_args(&server.command, &server.args, cwd);
        let mut cmd = Command::new(&program);
        cmd.args(&effective_args)
            .current_dir(cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let bearer_vars = bearer_env_names();
        let pool_credentials = crate::key_pool_config::credential_env_names()?;
        for (k, v) in std::env::vars_os() {
            let name = k.to_string_lossy();
            if !is_sensitive_env_var(&name)
                && !pool_credentials.contains(&name.to_ascii_uppercase())
                && !bearer_vars.iter().any(|var| name.eq_ignore_ascii_case(var))
            {
                cmd.env(k, v);
            }
        }
        for (k, v) in &server.env {
            if !pool_credentials.contains(&k.to_ascii_uppercase()) {
                cmd.env(k, v);
            }
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("无法启动 MCP 服务进程: {}", server.command))?;
        let stdin = child.stdin.take().context("MCP 子进程缺少 stdin")?;
        let stdout = child.stdout.take().context("MCP 子进程缺少 stdout")?;

        let mut session = Self {
            child,
            next_id: 2,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        };

        session
            .send_json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "coda-cli",
                        "version": "0.2.0"
                    }
                }
            }))
            .await?;
        rpc_result(session.read_response(1).await?)?;
        session
            .send_json(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }))
            .await?;

        Ok(session)
    }

    async fn list_response(&mut self) -> Result<Value> {
        let request_id = self.next_id;
        self.next_id += 1;
        self.send_json(&json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/list",
            "params": {}
        }))
        .await?;
        self.read_response(request_id).await
    }

    async fn call_tool(&mut self, tool_name: &str, arguments: &Value) -> Result<Value> {
        let request_id = self.next_id;
        self.next_id += 1;
        self.send_json(&json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments
            }
        }))
        .await?;
        self.read_response(request_id).await
    }

    async fn send_json(&mut self, payload: &Value) -> Result<()> {
        let stdin = self.stdin.as_mut().context("MCP stdin 已关闭")?;
        let mut encoded = serde_json::to_vec(payload)?;
        encoded.push(b'\n');
        stdin
            .write_all(&encoded)
            .await
            .context("写入 MCP stdin 失败")?;
        stdin.flush().await.context("刷新 MCP stdin 失败")?;
        Ok(())
    }

    async fn read_response(&mut self, expected_id: u64) -> Result<Value> {
        for _ in 0..MAX_SKIPPED_LINES {
            let Some(line) = read_bounded_line(&mut self.stdout).await? else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "MCP 服务在返回响应前退出",
                )
                .into());
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            if msg.get("id").and_then(Value::as_u64) != Some(expected_id) {
                continue;
            }
            return Ok(msg);
        }
        bail!("MCP 服务输出过多非响应消息")
    }

    async fn close(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_millis(500), self.child.wait()).await;
    }
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<String>> {
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf().await.context("读取 MCP stdout 失败")?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            ensure!(
                buf.len() + pos <= MAX_LINE_BYTES,
                "MCP 单行响应超过 4 MiB 上限"
            );
            buf.extend_from_slice(&available[..pos]);
            reader.consume(pos + 1);
            break;
        }
        ensure!(
            buf.len() + available.len() <= MAX_LINE_BYTES,
            "MCP 单行响应超过 4 MiB 上限"
        );
        let consumed = available.len();
        buf.extend_from_slice(available);
        reader.consume(consumed);
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf)
        .map(Some)
        .context("MCP stdout 不是有效 UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Re-run this test binary as the stdio server. The fixture exercises the
    // native child-process path on both Windows and Linux.
    #[test]
    fn pooled_stdio_fixture() {
        use std::io::{BufRead, Write};
        let Ok(version) = std::env::var("CODA_MCP_TEST_VERSION") else {
            return;
        };
        let pid = std::process::id();
        let mut count = 0;
        let mut last_id = 0;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("events")
            .unwrap();
        println!(); // End the test harness's status line before the first response.
        for line in std::io::stdin().lock().lines() {
            let req: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let Some(id) = req["id"].as_u64() else {
                continue;
            };
            assert!(id > last_id);
            last_id = id;
            let method = req["method"].as_str().unwrap();
            writeln!(log, "{pid} {method}").unwrap();
            let action = req["params"]["arguments"]["action"].as_str();
            if method == "tools/list" {
                match version.as_str() {
                    "direct-silent" => continue,
                    "direct-eof" | "direct-failed-reconnect" => {
                        std::fs::write("probe-ended", "").unwrap();
                        std::process::exit(0);
                    }
                    _ => {}
                }
            }
            let failed_reconnect = method == "initialize"
                && version == "direct-failed-reconnect"
                && Path::new("probe-ended").exists();
            if method == "tools/list" && version == "direct"
                || action == Some("rpc_error")
                || failed_reconnect
            {
                println!(
                    "{}",
                    json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32601,"message":"unsupported"}})
                );
                std::io::stdout().flush().unwrap();
                continue;
            }
            let result = match method {
                "initialize" => json!({}),
                "tools/list" => json!({"tools":[{"name":"count.next", "description":version}]}),
                "tools/call" => {
                    if version.starts_with("direct") {
                        assert!(matches!(
                            req["params"]["name"].as_str(),
                            Some("echo" | "echo_tool")
                        ));
                    } else {
                        assert_eq!(req["params"]["name"], "count.next");
                    }
                    match req["params"]["arguments"]["action"].as_str() {
                        Some("malformed") => {
                            println!(
                                "{}",
                                json!({"jsonrpc":"2.0", "id":id, "error":"unstructured"})
                            );
                            std::io::stdout().flush().unwrap();
                            continue;
                        }
                        Some("disconnect") => std::process::exit(0),
                        Some("hang") => std::thread::sleep(Duration::from_secs(120)),
                        _ => {}
                    }
                    count += 1;
                    json!({"pid":pid, "count":count, "version":version})
                }
                _ => panic!("unexpected method {method}"),
            };
            println!("{}", json!({"jsonrpc":"2.0", "id":id, "result":result}));
            std::io::stdout().flush().unwrap();
        }
    }

    fn fixture_config(path: &Path, version: &str) {
        let config = json!({"mcpServers":{"demo":{
            "command":std::env::current_exe().unwrap(), "args":["--exact","mcp::tests::pooled_stdio_fixture","--nocapture"],
            "env":{"CODA_MCP_TEST_VERSION":version}
        }}});
        std::fs::write(path.join(".mcp.json"), config.to_string()).unwrap();
    }

    #[tokio::test]
    async fn pooled_stdio_reuses_schema_and_state_and_recovers_without_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fixture_config(path, "first");
        let pool = McpPool::default();
        let tool = "mcp__demo__count_next";
        let definitions = pool.discover_tools("a", path).await;
        assert_eq!(definitions[0]["function"]["description"], "first");
        assert_eq!(pool.discover_tools("a", path).await, definitions);
        let args = json!({});
        let (one, two) = tokio::join!(
            pool.call_tool("a", path, tool, &args),
            pool.call_tool("a", path, tool, &args)
        );
        let one = one.unwrap();
        let two = two.unwrap();
        assert_eq!(one["pid"], two["pid"]);
        assert_eq!(
            one["count"].as_u64().unwrap() + two["count"].as_u64().unwrap(),
            3
        );
        let other = pool.call_tool("b", path, tool, &args).await.unwrap();
        assert_ne!(one["pid"], other["pid"]);
        assert_eq!(other["count"], 1);
        let events = || std::fs::read_to_string(path.join("events")).unwrap();
        assert_eq!(events().matches("tools/list").count(), 2);

        assert!(
            pool.call_tool("a", path, tool, &json!({"action":"disconnect"}))
                .await
                .unwrap_err()
                .is::<McpOutcomeUnknown>()
        );
        assert_eq!(events().matches("tools/call").count(), 4);
        let fresh = pool.call_tool("a", path, tool, &args).await.unwrap();
        assert_ne!(fresh["pid"], one["pid"]);
        assert_eq!(fresh["count"], 1);

        fixture_config(path, "second");
        let changed = pool.discover_tools("a", path).await;
        assert_eq!(changed[0]["function"]["description"], "second");
        let changed = pool.call_tool("a", path, tool, &args).await.unwrap();
        assert_ne!(changed["pid"], fresh["pid"]);
        assert_eq!(changed["count"], 1);
        std::fs::remove_file(path.join(".mcp.json")).unwrap();
        assert!(pool.discover_tools("a", path).await.is_empty());
        assert!(pool.call_tool("a", path, tool, &args).await.is_err());
        assert!(pool.state.lock().unwrap().entries.is_empty());
        fixture_config(path, "third");
        let last = pool.call_tool("a", path, tool, &args).await.unwrap();
        assert_eq!(last["count"], 1);
        pool.close().await;
        #[cfg(target_os = "linux")]
        assert!(!PathBuf::from(format!("/proc/{}", last["pid"])).exists());
        assert!(pool.call_tool("a", path, tool, &args).await.is_err());
    }

    #[test]
    fn pool_evicts_idle_sessions_without_displacing_active_requests() {
        let dir = tempfile::tempdir().unwrap();
        fixture_config(dir.path(), "capacity");
        let pool = McpPool::default();
        let cfg = pool.servers(dir.path()).unwrap().remove("demo").unwrap();
        let mut active = Vec::new();
        for index in 0..MAX_SESSIONS {
            active.push(
                pool.entry(&index.to_string(), dir.path(), "demo", &cfg)
                    .unwrap(),
            );
        }
        assert!(pool.entry("new", dir.path(), "demo", &cfg).is_err());
        drop(active.pop());
        let new = pool.entry("new", dir.path(), "demo", &cfg).unwrap();
        assert_eq!(pool.state.lock().unwrap().entries.len(), MAX_SESSIONS);
        for (index, entry) in active.iter().enumerate() {
            let same = pool
                .entry(&index.to_string(), dir.path(), "demo", &cfg)
                .unwrap();
            assert!(Arc::ptr_eq(entry, &same));
        }
        assert_eq!(new.run_id, "new");
    }

    #[tokio::test]
    async fn cancelling_in_flight_stdio_discards_child_and_stream() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fixture_config(path, "cancel");
        let pool = McpPool::default();
        let tool = "mcp__demo__count_next";
        let initial = pool.call_tool("a", path, tool, &json!({})).await.unwrap();
        let args = json!({"action":"hang"});
        let cancel = CancellationToken::new();
        let cancel_after_dispatch = async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while std::fs::read_to_string(path.join("events"))
                    .unwrap()
                    .matches("tools/call")
                    .count()
                    < 2
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            cancel.cancel();
        };
        let (result, ()) = tokio::join!(
            pool.call_tool_cancellable("a", path, tool, &args, &cancel),
            cancel_after_dispatch
        );
        assert!(result.unwrap_err().is::<McpOutcomeUnknown>());
        assert!(
            !pool
                .call_tool_cancellable("a", path, tool, &args, &cancel)
                .await
                .unwrap_err()
                .is::<McpOutcomeUnknown>()
        );
        let fresh = pool.call_tool("a", path, tool, &json!({})).await.unwrap();
        assert_ne!(fresh["pid"], initial["pid"]);
        assert_eq!(fresh["count"], 1);
        pool.close().await;
    }

    #[tokio::test]
    async fn direct_stdio_accepts_call_only_server_and_preserves_rpc_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fixture_config(path, "direct");
        for tool in ["mcp__demo__echo", "mcp__demo__echo_tool"] {
            assert_eq!(
                call_tool(path, tool, &json!({})).await.unwrap()["version"],
                "direct"
            );
        }
        let events = std::fs::read_to_string(path.join("events")).unwrap();
        assert_eq!(events.matches("tools/call").count(), 2);
        assert_eq!(events.matches("tools/list").count(), 1);

        fixture_config(path, "errors");
        let pool = McpPool::default();
        let tool = "mcp__demo__count_next";
        let err = pool
            .call_tool("a", path, tool, &json!({"action":"rpc_error"}))
            .await
            .unwrap_err();
        assert!(!err.is::<McpOutcomeUnknown>());
        assert!(err.to_string().contains("unsupported"));
        pool.call_tool("a", path, tool, &json!({})).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("events"))
                .unwrap()
                .matches("initialize")
                .count(),
            3
        );
        let malformed = pool
            .call_tool("a", path, tool, &json!({"action":"malformed"}))
            .await
            .unwrap_err();
        assert!(malformed.is::<McpOutcomeUnknown>());
    }

    #[tokio::test]
    async fn direct_call_only_server_reconnects_after_unanswered_listing() {
        for (mode, arguments) in [
            ("direct-silent", json!({})),
            ("direct-eof", json!({})),
            ("direct-eof", json!({"action":"rpc_error"})),
            ("direct-failed-reconnect", json!({})),
        ] {
            let dir = tempfile::tempdir().unwrap();
            fixture_config(dir.path(), mode);
            let result = call_tool(dir.path(), "mcp__demo__echo_tool", &arguments).await;
            let events = std::fs::read_to_string(dir.path().join("events")).unwrap();
            assert_eq!(events.matches("initialize").count(), 2, "{events}");
            assert_eq!(events.matches("tools/list").count(), 1, "{events}");
            let handshake_failed = mode == "direct-failed-reconnect";
            assert_eq!(
                events.matches("tools/call").count(),
                usize::from(!handshake_failed),
                "{events}"
            );
            if handshake_failed || arguments["action"] == "rpc_error" {
                let error = result.unwrap_err();
                assert!(error.to_string().contains("unsupported"));
                assert!(!error.is::<McpOutcomeUnknown>());
            } else {
                assert_eq!(result.unwrap()["version"], mode);
            }
        }
    }

    #[tokio::test]
    async fn overlapping_server_names_route_to_the_advertising_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fixture_config(path, "short");
        let file = path.join(".mcp.json");
        let mut config: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let mut other = config["mcpServers"]["demo"].clone();
        other["env"]["CODA_MCP_TEST_VERSION"] = json!("long");
        config["mcpServers"]["demo__v2"] = other;
        std::fs::write(file, config.to_string()).unwrap();
        let pool = McpPool::default();
        assert_eq!(pool.discover_tools("a", path).await.len(), 2);
        let result = pool
            .call_tool("a", path, "mcp__demo__v2__count_next", &json!({}))
            .await
            .unwrap();
        assert_eq!(result["version"], "long");
        assert_eq!(
            std::fs::read_to_string(path.join("events"))
                .unwrap()
                .matches("tools/call")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn changed_server_cannot_receive_a_previously_advertised_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        fixture_config(path, "original");
        let pool = McpPool::default();
        let (_, target) = pool
            .discover_targets("a", path, &mut Vec::new())
            .await
            .pop()
            .unwrap();
        fixture_config(path, "replacement");
        let result = pool
            .call_bound(
                "a",
                path,
                "mcp__demo__count_next",
                &json!({}),
                &CancellationToken::new(),
                CallBinding::Approved(&target),
            )
            .await;
        assert!(result.is_err());
        let events = std::fs::read_to_string(path.join("events")).unwrap();
        assert_eq!(events.matches("initialize").count(), 1);
        assert!(!events.contains("tools/call"));
    }

    #[test]
    fn http_owns_its_entire_qualified_namespace() {
        let stdio = ["a", "a__re.ote", "a__re_ote__deeper", "unrelated"]
            .into_iter()
            .map(|name| {
                (
                    name.to_owned(),
                    serde_json::from_value(json!({"command":"fixture"})).unwrap(),
                )
            })
            .collect();
        let http = BTreeMap::from([(
            "a__re_ote".to_owned(),
            http_registry::HttpConfig {
                url: "http://127.0.0.1/mcp".parse().unwrap(),
                bearer_env: None,
                oauth: None,
            },
        )]);
        let merged = merge_servers(Ok(stdio), http).unwrap();
        assert!(matches!(merged["a__re_ote"], ServerConfig::Http(_)));
        assert!(!merged.contains_key("a__re.ote"));
        assert!(!merged.contains_key("a__re_ote__deeper"));
        assert_eq!(
            http_owner(&merged, "mcp__a__re_ote__foo"),
            Some("a__re_ote")
        );
        assert!(http_owner(&merged, "mcp__a__other").is_none());
        assert!(merged.contains_key("a") && merged.contains_key("unrelated"));
    }

    #[tokio::test]
    async fn empty_workspace_returns_no_tools_immediately() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_config(dir.path()));
        let tools = discover_tools(dir.path()).await;
        assert!(tools.is_empty());
        assert_eq!(sanitize_name("my.server/v1"), "my_server_v1");
        assert!(is_valid_qualified_name("mcp__demo__echo_tool"));
        assert!(!is_valid_qualified_name("mcp__demo"));
        assert!(!is_valid_qualified_name("read_file"));
    }

    #[tokio::test]
    async fn stdio_discover_and_call_tool_with_env_filtering() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"
try {
    [Console]::InputEncoding = [System.Text.Encoding]::UTF8
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
} catch {}
while ($null -ne ($line = [Console]::In.ReadLine())) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    $req = $line | ConvertFrom-Json
    if ($req.method -eq 'initialize') {
        $r = '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"demo","version":"0.1.0"}}}'
        [Console]::Out.WriteLine($r)
        [Console]::Out.Flush()
    } elseif ($req.method -eq 'tools/list') {
        $r = '{"jsonrpc":"2.0","id":' + $req.id + ',"result":{"tools":[{"name":"echo_tool","description":"Echoes input","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}},"required":["msg"]}}]}}'
        [Console]::Out.WriteLine($r)
        [Console]::Out.Flush()
    } elseif ($req.method -eq 'tools/call') {
        $m = $req.params.arguments.msg
        $f = $env:FOO
        $t = $env:EXPLICIT_TOKEN
        $out = @{
            jsonrpc = '2.0'
            id = $req.id
            result = @{
                content = @( @{ type = 'text'; text = "echo:${m}|foo=${f}|token=${t}" } )
                isError = $false
            }
        } | ConvertTo-Json -Compress -Depth 8
        [Console]::Out.WriteLine($out)
        [Console]::Out.Flush()
    }
}
"#;
        std::fs::create_dir_all(dir.path().join(".coda")).unwrap();
        std::fs::write(dir.path().join("mock_mcp.ps1"), script).unwrap();
        std::fs::write(
            dir.path().join(".coda").join("mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": {
                    "demo": {
                        "command": "pwsh",
                        "args": ["-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "mock_mcp.ps1"],
                        "env": {
                            "FOO": "bar",
                            "EXPLICIT_TOKEN": "allowed-explicit"
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(has_config(dir.path()));
        assert!(has_configured_server(dir.path(), "mcp__demo__echo_tool"));
        assert!(is_sensitive_env_var("OPENAI_API_KEY"));
        assert!(is_sensitive_env_var("AWS_SECRET_ACCESS_KEY"));
        assert!(is_sensitive_env_var("GITHUB_TOKEN"));
        assert!(!is_sensitive_env_var("PATH"));

        let discovered = discover_tools(dir.path()).await;
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0]["function"]["name"], "mcp__demo__echo_tool");
        assert_eq!(discovered[0]["function"]["description"], "Echoes input");

        let result = call_tool(dir.path(), "mcp__demo__echo_tool", &json!({"msg": "ping"}))
            .await
            .unwrap();
        assert_eq!(
            result["content"][0]["text"],
            "echo:ping|foo=bar|token=allowed-explicit"
        );
    }
}
