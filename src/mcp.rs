//! Standard Model Context Protocol (MCP) JSON-RPC 2.0 client over newline-delimited stdio.

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

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

#[derive(Clone, Debug, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
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

/// Fast zero-allocation check for whether `.coda/mcp.json` or `.mcp.json` exists in `workspace`.
pub fn has_config(workspace: &Path) -> bool {
    let coda_dir = workspace.join(".coda");
    let has_coda = is_safe_dir(&coda_dir) && is_safe_regular_file(&coda_dir.join("mcp.json"));
    let has_dot = is_safe_regular_file(&workspace.join(".mcp.json"));
    has_coda || has_dot
}

/// Checks whether `qualified_name` maps to a configured, enabled MCP server in `workspace`.
pub fn has_configured_server(workspace: &Path, qualified_name: &str) -> bool {
    if !is_valid_qualified_name(qualified_name) || !has_config(workspace) {
        return false;
    }
    let Ok(servers) = load_servers(workspace) else {
        return false;
    };
    resolve_server_and_tool(&servers, qualified_name).is_some()
}

/// Discovers tools from `<workspace>/.coda/mcp.json` and `<workspace>/.mcp.json`.
/// Returns `vec![]` with zero subprocess overhead when neither file exists.
pub async fn discover_tools(workspace: &Path) -> Vec<Value> {
    if !has_config(workspace) {
        return Vec::new();
    }
    let Ok(servers) = load_servers(workspace) else {
        return Vec::new();
    };
    if servers.is_empty() {
        return Vec::new();
    }
    let Ok(cwd) = clean_workspace_cwd(workspace) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut seen_names = std::collections::HashSet::new();
    for (server_name, server_cfg) in &servers {
        let server_prefix = sanitize_name(server_name);
        if server_prefix.is_empty() {
            continue;
        }
        let listed = tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            discover_from_server(&cwd, server_name, server_cfg),
        )
        .await;
        if let Ok(Ok(tools)) = listed {
            for tool in tools {
                let Some(raw_tool_name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let sanitized_tool = sanitize_name(raw_tool_name);
                if sanitized_tool.is_empty() {
                    continue;
                }
                let qualified = format!("mcp__{server_prefix}__{sanitized_tool}");
                if !is_valid_qualified_name(&qualified) || !seen_names.insert(qualified.clone()) {
                    continue;
                }
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("MCP tool {raw_tool_name} from {server_name}"));
                let parameters = normalize_input_schema(tool.get("inputSchema"));
                out.push(json!({
                    "type": "function",
                    "function": {
                        "name": qualified,
                        "description": description,
                        "parameters": parameters,
                    }
                }));
            }
        }
    }
    out
}

/// Invokes `tools/call` on the configured MCP server corresponding to `qualified_name`.
pub async fn call_tool(workspace: &Path, qualified_name: &str, arguments: &Value) -> Result<Value> {
    ensure!(
        is_valid_qualified_name(qualified_name),
        "无效的 MCP 工具限定名: {qualified_name}"
    );
    let servers = load_servers(workspace)?;
    let (_, server_cfg, tool_suffix) = resolve_server_and_tool(&servers, qualified_name)
        .with_context(|| format!("未找到 MCP 服务配置: {qualified_name}"))?;
    let cwd = clean_workspace_cwd(workspace)?;

    let mut session =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, McpSession::connect(&cwd, server_cfg))
            .await
            .context("MCP 服务握手超时 (15s)")??;

    let args_obj = if arguments.is_object() {
        arguments.clone()
    } else {
        json!({})
    };
    let call_outcome = tokio::time::timeout(CALL_TIMEOUT, async {
        match session.call_tool(&tool_suffix, &args_obj, 2).await {
            Ok(result) => Ok(result),
            Err(first_err) if tool_suffix.contains('_') => {
                // If the raw tool name contained non-[a-zA-Z0-9_-] characters that were sanitized
                // to '_', resolve the original raw name from `tools/list` and retry once.
                if let Ok(tools) = session.list_tools(3).await
                    && let Some(raw_name) = tools
                        .iter()
                        .filter_map(|t| t.get("name").and_then(Value::as_str))
                        .find(|raw| sanitize_name(raw) == tool_suffix && *raw != tool_suffix)
                {
                    return session.call_tool(raw_name, &args_obj, 4).await;
                }
                Err(first_err)
            }
            Err(err) => Err(err),
        }
    })
    .await;

    session.close().await;
    call_outcome.context("MCP 工具执行超时 (60s)")?
}

fn is_safe_dir(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

fn is_safe_regular_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

fn read_config_file(path: &Path) -> Result<McpConfigFile> {
    ensure!(is_safe_regular_file(path), "MCP 配置文件不可安全读取");
    let raw = std::fs::read_to_string(path).context("无法读取 MCP 配置文件")?;
    serde_json::from_str::<McpConfigFile>(&raw).context("无效的 MCP 配置 JSON")
}

fn load_servers(workspace: &Path) -> Result<BTreeMap<String, McpServerConfig>> {
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

fn resolve_server_and_tool<'a>(
    servers: &'a BTreeMap<String, McpServerConfig>,
    qualified_name: &str,
) -> Option<(&'a str, &'a McpServerConfig, String)> {
    let rest = qualified_name.strip_prefix("mcp__")?;
    for (server_name, cfg) in servers {
        let sanitized_server = sanitize_name(server_name);
        if sanitized_server.is_empty() {
            continue;
        }
        let prefix = format!("{sanitized_server}__");
        if let Some(tool_suffix) = rest.strip_prefix(&prefix)
            && !tool_suffix.is_empty()
        {
            return Some((server_name.as_str(), cfg, tool_suffix.to_owned()));
        }
    }
    None
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

async fn discover_from_server(
    cwd: &Path,
    _server_name: &str,
    server: &McpServerConfig,
) -> Result<Vec<Value>> {
    let mut session = McpSession::connect(cwd, server).await?;
    let tools = session.list_tools(2).await;
    session.close().await;
    tools
}

struct McpSession {
    child: Child,
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
        for (k, v) in std::env::vars_os() {
            if !is_sensitive_env_var(&k.to_string_lossy()) {
                cmd.env(k, v);
            }
        }
        for (k, v) in &server.env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("无法启动 MCP 服务进程: {}", server.command))?;
        let stdin = child.stdin.take().context("MCP 子进程缺少 stdin")?;
        let stdout = child.stdout.take().context("MCP 子进程缺少 stdout")?;

        let mut session = Self {
            child,
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
        let _ = session.read_response(1).await?;
        session
            .send_json(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }))
            .await?;

        Ok(session)
    }

    async fn list_tools(&mut self, request_id: u64) -> Result<Vec<Value>> {
        self.send_json(&json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/list",
            "params": {}
        }))
        .await?;
        let resp = self.read_response(request_id).await?;
        Ok(resp["result"]["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    async fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: &Value,
        request_id: u64,
    ) -> Result<Value> {
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
        let resp = self.read_response(request_id).await?;
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
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
                bail!("MCP 服务在返回响应前退出");
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
            if let Some(err) = msg.get("error").filter(|v| !v.is_null()) {
                bail!("MCP JSON-RPC 错误: {err}");
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
