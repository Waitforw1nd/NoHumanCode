//! MCP 2025-06-18 Streamable HTTP client for a single registered endpoint.
use super::{CALL_TIMEOUT, HANDSHAKE_TIMEOUT, MAX_LINE_BYTES, oauth::session::Access};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{
    Client, Method, RequestBuilder, Response, StatusCode, Url,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue},
};
use serde_json::{Value, json};
use std::{collections::HashSet, fmt, sync::Arc, time::Duration};

const VERSION: &str = "2025-06-18";
const SESSION_ID: &str = "mcp-session-id";
const PROTOCOL_VERSION: &str = "mcp-protocol-version";
const MAX_PAGES: usize = 64;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub(super) struct SessionExpired;
impl fmt::Display for SessionExpired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MCP HTTP 会话已过期")
    }
}
impl std::error::Error for SessionExpired {}

#[derive(Debug)]
pub(super) struct OAuthRejected;
impl fmt::Display for OAuthRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MCP HTTP OAuth 凭据未获接受 (HTTP 401)")
    }
}
impl std::error::Error for OAuthRejected {}

pub(super) struct HttpSession {
    client: Client,
    url: Url,
    authorization: Option<HeaderValue>,
    access: Option<Arc<Access>>,
    session_id: Option<HeaderValue>,
    initialized: bool,
    has_tools: bool,
    next_id: u64,
}

impl HttpSession {
    pub(super) fn generation(&self) -> Option<&str> {
        self.access.as_ref().map(|a| a.generation.as_str())
    }

    pub(super) async fn connect(
        url: &Url,
        bearer: Option<&str>,
        access: Option<Arc<Access>>,
    ) -> Result<Self> {
        let authorization = access
            .as_ref()
            .map(|a| a.token.as_str())
            .or(bearer)
            .map(|token| -> Result<_> {
                ensure!(
                    !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic()),
                    "无效的 MCP Bearer 凭据"
                );
                let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| anyhow::anyhow!("无效的 MCP Bearer 凭据"))?;
                value.set_sensitive(true);
                Ok(value)
            })
            .transpose()?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(HANDSHAKE_TIMEOUT)
            .timeout(CALL_TIMEOUT)
            .build()
            .map_err(|_| anyhow::anyhow!("无法建立 MCP HTTP 客户端"))?;
        let mut session = Self {
            client,
            url: url.clone(),
            authorization,
            access,
            session_id: None,
            initialized: false,
            has_tools: false,
            next_id: 1,
        };
        let handshake = async {
            let reply = session
                .rpc(
                    "initialize",
                    json!({
                        "protocolVersion": VERSION, "capabilities": {},
                        "clientInfo": {"name": "coda-cli", "version": env!("CARGO_PKG_VERSION")}
                    }),
                )
                .await?;
            let result = reply.get("result").context("MCP HTTP 初始化被拒绝")?;
            ensure!(
                result["protocolVersion"] == VERSION,
                "MCP HTTP 协议版本不兼容"
            );
            let capabilities = result["capabilities"]
                .as_object()
                .context("无效的 MCP 服务能力")?;
            session.has_tools = capabilities.get("tools").is_some_and(Value::is_object);
            session.initialized = true;
            session
                .send_oneway(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .await
        };
        tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
            .await
            .context("MCP HTTP 握手超时")??;
        Ok(session)
    }

    fn request(&self, method: Method) -> RequestBuilder {
        let mut req = self
            .client
            .request(method, self.url.clone())
            .header(ACCEPT, "application/json, text/event-stream");
        if let Some(auth) = &self.authorization {
            req = req.header(AUTHORIZATION, auth);
        }
        if let Some(id) = &self.session_id {
            req = req.header(SESSION_ID, id);
        }
        if self.initialized {
            req = req.header(PROTOCOL_VERSION, VERSION);
        }
        req
    }

    async fn post(&mut self, packet: Value) -> Result<Response> {
        let body = serde_json::to_vec(&packet)?;
        ensure!(body.len() <= MAX_LINE_BYTES, "MCP HTTP 请求体过大");
        let response = self
            .request(Method::POST)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("MCP HTTP 请求失败或超时"))?;
        if response.status() == StatusCode::UNAUTHORIZED
            && let Some(access) = &self.access
        {
            access.rejected(false)?;
            return Err(OAuthRejected.into());
        }
        if response.status() == StatusCode::NOT_FOUND && self.session_id.is_some() {
            self.session_id.take();
            self.initialized = false;
            return Err(SessionExpired.into());
        }
        Ok(response)
    }

    async fn send_oneway(&mut self, packet: Value) -> Result<()> {
        let mut response = self.post(packet).await?;
        ensure!(
            response.status() == StatusCode::ACCEPTED,
            "MCP HTTP 通知或客户端响应未获接受 (HTTP {})",
            response.status().as_u16()
        );
        ensure!(
            response
                .chunk()
                .await
                .map_err(|_| anyhow::anyhow!("MCP HTTP 通知确认中断"))?
                .is_none(),
            "MCP HTTP 202 确认必须为空"
        );
        Ok(())
    }

    async fn rpc(&mut self, method: &'static str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).context("MCP HTTP 请求编号已耗尽")?;
        let mut response = self
            .post(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let status = response.status();
        let media = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .unwrap_or("")
            .trim();
        let is_json = media.eq_ignore_ascii_case("application/json");
        let is_sse = media.eq_ignore_ascii_case("text/event-stream");
        if !status.is_success() {
            ensure!(
                (status.is_client_error() || status.is_server_error()) && is_json,
                "MCP HTTP 请求失败 (HTTP {})",
                status.as_u16()
            );
        }
        ensure!(is_json || is_sse, "MCP HTTP 响应类型不受支持");
        if method == "initialize" && status.is_success() {
            let mut values = response.headers().get_all(SESSION_ID).iter();
            if let Some(header) = values.next() {
                ensure!(
                    values.next().is_none()
                        && !header.is_empty()
                        && header.as_bytes().iter().all(u8::is_ascii_graphic),
                    "无效的 MCP HTTP 会话标识"
                );
                let mut header = header.clone();
                header.set_sensitive(true);
                self.session_id = Some(header);
            }
        }
        if is_json {
            let mut body = Vec::new();
            while let Some(chunk) = next_chunk(&mut response).await? {
                ensure!(
                    chunk.len() <= MAX_LINE_BYTES - body.len(),
                    "MCP HTTP 响应过大"
                );
                body.extend_from_slice(&chunk);
            }
            let msg = parse_packet(&body)?;
            ensure!(
                status.is_success() || msg.get("error").is_some(),
                "MCP HTTP 请求失败 (HTTP {})",
                status.as_u16()
            );
            return validated_reply(msg, id);
        }
        let mut stream = EventStream::default();
        let mut consumed = 0;
        while let Some(chunk) = next_chunk(&mut response).await? {
            for &byte in &chunk {
                consumed += 1;
                ensure!(consumed <= MAX_LINE_BYTES, "MCP HTTP 事件流过大");
                if let Some(data) = stream.push(byte)? {
                    let msg = parse_packet(&data)?;
                    if let Some(method) = msg.get("method") {
                        let method = method.as_str().context("无效的 MCP 服务端请求")?;
                        if let Some(peer_id) = msg.get("id") {
                            ensure!(
                                peer_id.is_string() || peer_id.is_i64() || peer_id.is_u64(),
                                "无效的 MCP 服务端请求编号"
                            );
                            let reply = if method == "ping" {
                                json!({"jsonrpc":"2.0","id":peer_id,"result":{}})
                            } else {
                                json!({"jsonrpc":"2.0","id":peer_id,"error":{"code":-32601,"message":"Method not found"}})
                            };
                            // The original POST already opened its stream. A later rejection of
                            // this separate acknowledgment cannot prove the tool was not run.
                            self.send_oneway(reply).await.map_err(|err| {
                                if err.is::<OAuthRejected>()
                                    || err.is::<super::oauth::session::LoginRequired>()
                                {
                                    err
                                } else {
                                    anyhow::anyhow!("MCP HTTP 服务端请求确认失败")
                                }
                            })?;
                        }
                    } else if msg.get("id").and_then(Value::as_u64) == Some(id) {
                        return validated_reply(msg, id);
                    }
                }
            }
        }
        bail!("MCP HTTP 事件流在响应前结束")
    }

    pub(super) async fn list_response(&mut self) -> Result<Value> {
        ensure!(self.initialized, "MCP HTTP 会话未就绪");
        ensure!(self.has_tools, "MCP HTTP 服务未声明工具能力");
        let mut all = Vec::new();
        let mut params = json!({});
        let mut seen = HashSet::new();
        let mut bytes = 0;
        for _ in 0..MAX_PAGES {
            let mut reply = self.rpc("tools/list", params).await?;
            if reply.get("error").is_some() {
                return Ok(reply);
            }
            bytes += serde_json::to_vec(&reply)?.len();
            ensure!(bytes <= MAX_LINE_BYTES, "MCP 工具目录过大");
            let result = reply["result"]
                .as_object_mut()
                .context("无效的 MCP 工具目录")?;
            let Some(Value::Array(mut tools)) = result.remove("tools") else {
                bail!("MCP 工具目录缺少 tools 数组")
            };
            all.append(&mut tools);
            match result.remove("nextCursor") {
                None | Some(Value::Null) => {
                    all.retain_mut(|tool| self.scrub_tool(tool));
                    self.scrub_object(result, |name, field| {
                        if name == "_meta" {
                            self.scrub(field);
                            true
                        } else {
                            false
                        }
                    });
                    result.insert("tools".into(), Value::Array(all));
                    return Ok(reply);
                }
                Some(Value::String(cursor)) => {
                    ensure!(seen.insert(cursor.clone()), "MCP 工具目录游标重复");
                    params = json!({"cursor":cursor});
                }
                _ => bail!("无效的 MCP 工具目录游标"),
            }
        }
        bail!("MCP 工具目录分页过多")
    }

    pub(super) async fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value> {
        ensure!(
            self.initialized && self.has_tools,
            "MCP HTTP 工具会话未就绪"
        );
        let mut reply = self
            .rpc("tools/call", json!({"name":name,"arguments":args}))
            .await?;
        if let Some(result) = reply.get_mut("result").and_then(Value::as_object_mut) {
            self.scrub_object(result, |name, field| {
                match name {
                    "content" => self.scrub_content(field),
                    "isError" | "structuredContent" | "_meta" => self.scrub(field),
                    _ => return false,
                }
                true
            });
        }
        Ok(reply)
    }

    fn redacted(&self, text: &str) -> Option<String> {
        let bearer = self
            .authorization
            .as_ref()
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "));
        let session = self.session_id.as_ref().and_then(|h| h.to_str().ok());
        let mut secrets = [(bearer, false), (session, true)];
        secrets.sort_by_key(|(secret, _)| std::cmp::Reverse(secret.map(str::len)));
        let mut result = None;
        for (secret, is_session) in secrets {
            let Some(secret) = secret else { continue };
            let current = result.as_deref().unwrap_or(text);
            if is_session && secret.len() <= 8 {
                if let Some(redacted) = redact_short_session_id(current, secret) {
                    result = Some(redacted);
                }
            } else if current.contains(secret) {
                result = Some(crate::secrets::scrub(current, secret));
            }
        }
        result
    }

    fn scrub_tool(&self, tool: &mut Value) -> bool {
        if !tool["name"]
            .as_str()
            .is_some_and(|name| self.redacted(name).is_none())
        {
            return false;
        }
        for key in ["inputSchema", "outputSchema"] {
            if let Some(schema) = tool.get_mut(key)
                && !self.scrub_schema(schema)
            {
                return false;
            }
        }
        if let Some(fields) = tool.as_object_mut() {
            self.scrub_object(fields, |name, field| match name {
                "name" | "inputSchema" | "outputSchema" => true,
                "title" | "description" | "_meta" => {
                    self.scrub(field);
                    true
                }
                "annotations" => {
                    self.scrub_annotations(field);
                    true
                }
                _ => false,
            });
        }
        true
    }

    // Schema keywords and their control values are not application field names. A redacted
    // parameter would no longer address the original tool, so omit such definitions.
    fn scrub_schema(&self, schema: &mut Value) -> bool {
        const TYPES: &[&str] = &[
            "array", "boolean", "integer", "null", "number", "object", "string",
        ];
        const STANDARD_FORMATS: &[&str] = &[
            "date-time",
            "time",
            "date",
            "duration",
            "email",
            "idn-email",
            "hostname",
            "idn-hostname",
            "ipv4",
            "ipv6",
            "uuid",
            "uri",
            "uri-reference",
            "iri",
            "iri-reference",
            "uri-template",
            "json-pointer",
            "relative-json-pointer",
            "regex",
        ];
        const STANDARD_DIALECTS: &[&str] = &[
            "https://json-schema.org/draft/2020-12/schema",
            "https://json-schema.org/draft/2019-09/schema",
            "http://json-schema.org/draft-07/schema#",
            "http://json-schema.org/draft-06/schema#",
            "http://json-schema.org/draft-04/schema#",
        ];
        let Some(fields) = schema.as_object_mut() else {
            return schema.is_boolean();
        };
        for (keyword, value) in fields {
            match keyword.as_str() {
                "properties" | "patternProperties" | "$defs" | "definitions"
                | "dependentSchemas" | "dependencies" => {
                    let Some(entries) = value.as_object_mut() else {
                        return false;
                    };
                    for (name, sub) in entries {
                        if self.redacted(name).is_some() {
                            return false;
                        }
                        if keyword == "dependencies" && sub.is_array() {
                            if self.contains_sensitive_data(sub) {
                                return false;
                            }
                        } else if !self.scrub_schema(sub) {
                            return false;
                        }
                    }
                }
                "items"
                | "prefixItems"
                | "allOf"
                | "oneOf"
                | "anyOf"
                | "not"
                | "if"
                | "then"
                | "else"
                | "additionalProperties"
                | "additionalItems"
                | "propertyNames"
                | "contains"
                | "unevaluatedItems"
                | "unevaluatedProperties"
                | "contentSchema" => {
                    if let Some(items) = value.as_array_mut() {
                        if !items.iter_mut().all(|v| self.scrub_schema(v)) {
                            return false;
                        }
                    } else if !self.scrub_schema(value) {
                        return false;
                    }
                }
                "required" | "dependentRequired" | "enum" | "const" => {
                    if self.contains_sensitive_data(value) {
                        return false;
                    }
                }
                "description" | "title" | "$comment" | "default" | "examples" => self.scrub(value),
                // Retain syntax and validation rules exactly as supplied.
                "type" => {
                    let valid = |v: &Value| v.as_str().is_some_and(|name| TYPES.contains(&name));
                    if !valid(value)
                        && !value
                            .as_array()
                            .is_some_and(|items| items.iter().all(valid))
                    {
                        return false;
                    }
                }
                "$schema"
                    if value
                        .as_str()
                        .is_some_and(|uri| STANDARD_DIALECTS.contains(&uri)) => {}
                "format"
                    if value
                        .as_str()
                        .is_some_and(|name| STANDARD_FORMATS.contains(&name)) => {}
                "$schema" | "$ref" | "$dynamicRef" | "$id" | "$anchor" | "$dynamicAnchor"
                | "$recursiveRef" | "$recursiveAnchor" | "$vocabulary" | "format" | "pattern"
                | "contentEncoding" | "contentMediaType" | "multipleOf" | "minimum" | "maximum"
                | "exclusiveMinimum" | "exclusiveMaximum" | "minLength" | "maxLength"
                | "minItems" | "maxItems" | "uniqueItems" | "minContains" | "maxContains"
                | "minProperties" | "maxProperties" | "readOnly" | "writeOnly" | "deprecated" => {
                    if self.contains_sensitive_data(value) {
                        return false;
                    }
                }
                _ => {
                    if self.redacted(keyword).is_some() {
                        return false;
                    }
                    self.scrub(value);
                }
            }
        }
        true
    }

    fn contains_sensitive_data(&self, value: &Value) -> bool {
        match value {
            Value::String(text) => self.redacted(text).is_some(),
            Value::Array(items) => items.iter().any(|v| self.contains_sensitive_data(v)),
            Value::Object(fields) => fields
                .iter()
                .any(|(name, v)| self.redacted(name).is_some() || self.contains_sensitive_data(v)),
            _ => false,
        }
    }

    fn scrub_content(&self, value: &mut Value) {
        match value {
            Value::Array(items) => items.iter_mut().for_each(|v| self.scrub_content(v)),
            Value::Object(fields) => self.scrub_object(fields, |name, field| {
                match name {
                    "type"
                        if matches!(
                            field.as_str(),
                            Some("text" | "image" | "audio" | "resource" | "resource_link")
                        ) => {}
                    "resource" => self.scrub_content(field),
                    "annotations" => self.scrub_annotations(field),
                    "type" | "text" | "data" | "blob" | "mimeType" | "uri" | "name" | "title"
                    | "description" | "size" | "_meta" => self.scrub(field),
                    _ => return false,
                }
                true
            }),
            _ => self.scrub(value),
        }
    }

    fn scrub_annotations(&self, value: &mut Value) {
        if let Some(fields) = value.as_object_mut() {
            self.scrub_object(fields, |name, field| match name {
                "audience"
                    if field.as_array().is_some_and(|items| {
                        items
                            .iter()
                            .all(|v| matches!(v.as_str(), Some("user" | "assistant")))
                    }) =>
                {
                    true
                }
                "audience" | "priority" | "lastModified" | "title" | "readOnlyHint"
                | "destructiveHint" | "idempotentHint" | "openWorldHint" => {
                    self.scrub(field);
                    true
                }
                _ => false,
            });
        } else {
            self.scrub(value);
        }
    }

    fn scrub(&self, value: &mut Value) {
        match value {
            Value::String(text) => {
                if let Some(safe) = self.redacted(text) {
                    *text = safe;
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.scrub(item)),
            Value::Object(fields) => self.scrub_object(fields, |_, _| false),
            _ => {}
        }
    }

    fn scrub_object(
        &self,
        fields: &mut serde_json::Map<String, Value>,
        mut fixed_field: impl FnMut(&str, &mut Value) -> bool,
    ) {
        let mut renamed = Vec::new();
        fields.retain(|key, field| {
            if fixed_field(key, field) {
                return true;
            }
            self.scrub(field);
            if let Some(safe) = self.redacted(key) {
                renamed.push((safe, field.take()));
                false
            } else {
                true
            }
        });
        let mut suffix = 2;
        for (safe, field) in renamed {
            let mut key = safe.clone();
            while fields.contains_key(&key) {
                key = format!("{safe} #{suffix}");
                suffix += 1;
            }
            fields.insert(key, field);
        }
    }

    fn delete_request(&mut self) -> Option<RequestBuilder> {
        self.session_id.as_ref()?;
        let req = self.request(Method::DELETE).timeout(CLOSE_TIMEOUT);
        self.session_id.take();
        self.initialized = false;
        Some(req)
    }

    pub(super) async fn close(&mut self) {
        if let Some(req) = self.delete_request() {
            let _ = req.send().await;
        }
        self.initialized = false;
    }
}

impl Drop for HttpSession {
    fn drop(&mut self) {
        if let Some(req) = self.delete_request()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                let _ = req.send().await;
            });
        }
    }
}

// Short server-chosen labels can occur inside ordinary identifiers and prose.
fn redact_short_session_id(text: &str, id: &str) -> Option<String> {
    let token_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-');
    let mut out = String::new();
    let mut copied = 0;
    let mut search = 0;
    while let Some(offset) = text[search..].find(id) {
        let start = search + offset;
        let end = start + id.len();
        if text[..start].chars().next_back().is_some_and(token_char)
            || text[end..].chars().next().is_some_and(token_char)
        {
            // Session IDs are visible ASCII, so this also permits overlapping matches.
            search = start + 1;
            continue;
        }
        out.push_str(&text[copied..start]);
        out.push_str("[redacted]");
        copied = end;
        search = end;
    }
    if copied == 0 {
        return None;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

async fn next_chunk(response: &mut Response) -> Result<Option<axum::body::Bytes>> {
    response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("读取 MCP HTTP 响应失败"))
}

fn parse_packet(data: &[u8]) -> Result<Value> {
    let msg: Value =
        serde_json::from_slice(data).map_err(|_| anyhow::anyhow!("无效的 MCP HTTP JSON 响应"))?;
    ensure!(msg["jsonrpc"] == "2.0", "无效的 MCP HTTP JSON-RPC 版本");
    Ok(msg)
}

fn validated_reply(mut msg: Value, id: u64) -> Result<Value> {
    ensure!(
        msg["id"].as_u64() == Some(id) && msg.get("method").is_none(),
        "MCP HTTP 响应编号不匹配"
    );
    match (msg.get("result"), msg.get("error")) {
        (Some(result), None) if result.is_object() => {
            msg.as_object_mut()
                .unwrap()
                .retain(|key, _| matches!(key.as_str(), "jsonrpc" | "id" | "result"));
            Ok(msg)
        }
        (None, Some(error)) if error["code"].is_i64() && error["message"].is_string() => {
            // Preserve a confirmed RPC failure without propagating arbitrary upstream diagnostics.
            Ok(
                json!({"jsonrpc":"2.0", "id":id, "error":{"code":error["code"],"message":"Remote MCP JSON-RPC error"}}),
            )
        }
        _ => bail!("无效的 MCP HTTP JSON-RPC 响应"),
    }
}

#[derive(Default)]
struct EventStream {
    line: Vec<u8>,
    data: Vec<u8>,
    skip_lf: bool,
    started: bool,
    ignore_event: bool,
}

impl EventStream {
    fn push(&mut self, byte: u8) -> Result<Option<Vec<u8>>> {
        if std::mem::take(&mut self.skip_lf) && byte == b'\n' {
            return Ok(None);
        }
        if byte != b'\r' && byte != b'\n' {
            self.line.push(byte);
            return Ok(None);
        }
        self.skip_lf = byte == b'\r';
        let line = std::str::from_utf8(&self.line).context("MCP HTTP 事件流不是 UTF-8")?;
        let line = if self.started {
            line
        } else {
            line.trim_start_matches('\u{feff}')
        };
        self.started = true;
        let mut packet = None;
        if line.is_empty() {
            if !self.ignore_event && !self.data.is_empty() {
                self.data.pop();
                packet = Some(std::mem::take(&mut self.data));
            }
            self.data.clear();
            self.ignore_event = false;
        } else {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "data" => {
                    self.data.extend_from_slice(value.as_bytes());
                    self.data.push(b'\n');
                }
                "event" => self.ignore_event = !value.is_empty() && value != "message",
                _ => {}
            }
        }
        self.line.clear();
        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::HeaderMap,
        response::{IntoResponse, Response as ServerResponse},
        routing::post,
    };
    use std::sync::{Arc, Mutex};
    const BEARER: &str = "fixture-bearer-private";
    const SID: &str = "fixture-session-private";

    #[derive(Clone, Default)]
    struct Trace(Arc<Mutex<Vec<String>>>);
    impl Trace {
        fn record(&self, action: &str) {
            self.0.lock().unwrap().push(action.into());
        }
        fn count(&self, action: &str) -> usize {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|v| *v == action)
                .count()
        }
    }

    async fn fixture(
        State(trace): State<Trace>,
        headers: HeaderMap,
        Json(msg): Json<Value>,
    ) -> ServerResponse {
        assert_eq!(headers[AUTHORIZATION], format!("Bearer {BEARER}"));
        assert_eq!(headers[ACCEPT], "application/json, text/event-stream");
        assert_eq!(headers[CONTENT_TYPE], "application/json");
        let method = msg["method"].as_str().unwrap_or("reply");
        let id = &msg["id"];
        trace.record(method);
        if method == "initialize" {
            assert!(!headers.contains_key(SESSION_ID) && !headers.contains_key(PROTOCOL_VERSION));
            assert_eq!(msg["params"]["protocolVersion"], VERSION);
            return ([(SESSION_ID, SID)],Json(json!({"jsonrpc":"2.0","id":id,"result":{
                "protocolVersion":VERSION, "capabilities":{"tools":{}}, "serverInfo":{"name":"local","version":"1"}
            }}))).into_response();
        }
        assert_eq!(headers[PROTOCOL_VERSION], VERSION);
        assert_eq!(headers[SESSION_ID], SID);
        match method {
            "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
            "reply" => {
                if id == "expired-ping" {
                    return StatusCode::NOT_FOUND.into_response();
                }
                assert_eq!(id, "keep-alive");
                assert_eq!(msg["result"], json!({}));
                StatusCode::ACCEPTED.into_response()
            }
            "tools/list" if msg["params"].get("cursor").is_none() => Json(json!({"jsonrpc":"2.0","id":id,"result":{
                "tools":[{"name":"first","description":format!("Bearer sample {BEARER}; id {SID}")},
                    {"name":BEARER}, {"name":"secret-schema","inputSchema":{"properties":{(SID):{}}}}],
                "nextCursor":format!("{SID}-cursor")
            }})).into_response(),
            "tools/list" => {
                assert_eq!(msg["params"]["cursor"],format!("{SID}-cursor"));
                let payload = format!(concat!(
                    "\u{feff}: heartbeat\r\n\r\n",
                    "data: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}}\r\n\r\n",
                    "data: {{\"jsonrpc\":\"2.0\",\"id\":\"keep-alive\",\"method\":\"ping\"}}\r\n\r\n",
                    "event: message\rdata: {{\"jsonrpc\":\"2.0\",\rdata: \"id\":{},\"result\":{{\"tools\":[{{\"name\":\"检查\"}}]}}}}\r\r"
                ),id);
                let stream = async_stream::stream! {
                    for part in payload.as_bytes().chunks(3) {
                        yield Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(part));
                    }
                    std::future::pending::<()>().await;
                };
                ([(CONTENT_TYPE,"text/event-stream; charset=utf-8")],Body::from_stream(stream)).into_response()
            }
            "tools/call" => {
                let tool = msg["params"]["name"].as_str().unwrap();
                trace.record(tool);
                match tool {
                    "tool-error" => Json(json!({"jsonrpc":"2.0","id":id, "result":{
                        "isError":true, "content":[{"type":"text","text":format!("Bearer sample {BEARER} {SID}")}],
                        "structuredContent":{"[redacted]":"original",(SID):"one",(BEARER):"two"}
                    }})).into_response(),
                    "rpc-error" => (StatusCode::BAD_REQUEST,Json(json!({"jsonrpc":"2.0","id":id,
                        "error":{"code":-32602,"message":BEARER,"data":SID}}))).into_response(),
                    "expired-during-stream" => ([(CONTENT_TYPE,"text/event-stream")],
                        "data: {\"jsonrpc\":\"2.0\",\"id\":\"expired-ping\",\"method\":\"ping\"}\n\n").into_response(),
                    "broken" => ([(CONTENT_TYPE,"text/event-stream")],"data: {").into_response(),
                    "expired" => (StatusCode::NOT_FOUND, BEARER).into_response(),
                    "wrong-id" => Json(json!({"jsonrpc":"2.0","id":"unrelated","result":{}})).into_response(),
                    "redirect" => (StatusCode::TEMPORARY_REDIRECT, [("location", "/other")]).into_response(),
                    _ => StatusCode::NOT_IMPLEMENTED.into_response()
                }
            }
            _ => StatusCode::NOT_IMPLEMENTED.into_response()
        }
    }

    async fn serve() -> (Url, Trace, tokio::task::JoinHandle<()>) {
        let trace = Trace::default();
        let app = Router::new()
            .route(
                "/mcp",
                post(fixture).delete(
                    |State(trace): State<Trace>, headers: HeaderMap| async move {
                        assert_eq!(headers[SESSION_ID], SID);
                        assert_eq!(headers[PROTOCOL_VERSION], VERSION);
                        assert_eq!(headers[AUTHORIZATION], format!("Bearer {BEARER}"));
                        trace.record("delete");
                        StatusCode::METHOD_NOT_ALLOWED
                    },
                ),
            )
            .route(
                "/other",
                post(|State(trace): State<Trace>| async move {
                    trace.record("redirected");
                    StatusCode::NO_CONTENT
                }),
            )
            .with_state(trace.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/mcp", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, trace, server)
    }

    #[tokio::test]
    async fn paginated_json_and_sse_complete_without_waiting_for_eof() {
        let (url, trace, server) = serve().await;
        let mut session = HttpSession::connect(&url, Some(BEARER), None)
            .await
            .unwrap();
        let list = tokio::time::timeout(Duration::from_secs(3), session.list_response())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(list["id"], 3);
        assert_eq!(
            list["result"]["tools"],
            json!([
                {"name":"first","description":"Bearer sample [redacted]; id [redacted]"}, {"name":"检查"}
            ])
        );
        assert_eq!(trace.count("reply"), 1);
        let error = session.call_tool("tool-error", &json!({})).await.unwrap();
        assert_eq!(error["result"]["isError"], true);
        assert_eq!(
            error["result"]["content"][0]["text"],
            "Bearer sample [redacted] [redacted]"
        );
        assert!(!error.to_string().contains(BEARER) && !error.to_string().contains(SID));
        let fields = error["result"]["structuredContent"].as_object().unwrap();
        assert_eq!(fields["[redacted]"], "original");
        assert_eq!(fields.len(), 3);
        let rpc_error = session.call_tool("rpc-error", &json!({})).await.unwrap();
        assert_eq!(rpc_error["error"]["code"], -32602);
        assert!(!rpc_error.to_string().contains(BEARER) && !rpc_error.to_string().contains(SID));
        session.close().await;
        session.close().await;
        drop(session);
        assert_eq!(trace.count("delete"), 1);
        server.abort();
    }

    #[tokio::test]
    async fn short_session_ids_preserve_protocol_and_schema_controls() {
        use axum::extract::Path;

        let app = Router::new().route("/{sid}", post(
            |Path(sid): Path<String>, headers: HeaderMap, Json(packet): Json<Value>| async move {
                let id = &packet["id"];
                if packet["method"] == "initialize" {
                    return ([(SESSION_ID, sid)], Json(json!({"jsonrpc":"2.0","id":id,"result":{
                        "protocolVersion":VERSION, "capabilities":{"tools":{}}, "serverInfo":{"name":"local","version":"1"}
                    }}))).into_response();
                }
                assert_eq!(headers[SESSION_ID], sid);
                match packet["method"].as_str() {
                    Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
                    Some("tools/list") => Json(json!({"jsonrpc":"2.0","id":id,"result":{"tools":[
                        {"name":"list_files_tools_by_type","description":format!("This is text about types and toolsets. ID={sid}"),"inputSchema":{
                            "$schema":"https://json-schema.org/draft/2020-12/schema",
                            "type":"object","properties":{"status":{"type":"string"}},
                            "required":["status"],"additionalProperties":false
                        }}, {"name":"omit","inputSchema":{"type":"object","properties":{(sid):{"type":"string"}}}}
                    ]}})).into_response(),
                    Some("tools/call") => {
                        assert_eq!(packet["params"],json!({"name":"list_files_tools_by_type","arguments":{"status":"hi"}}));
                        Json(json!({"jsonrpc":"2.0","id":id,"result":{
                            "content":[{"type":"text","text":format!("This is text about types and toolsets. ID={sid}")}],
                            "structuredContent":{"status":"items", (sid.clone()):sid}, "isError":false
                        }})).into_response()
                    }
                    _ => StatusCode::BAD_REQUEST.into_response(),
                }
            }
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for id in ["s", "tools", "t", "type"] {
            let mut session = HttpSession::connect(&base.join(id).unwrap(), None, None)
                .await
                .unwrap();
            let list = session.list_response().await.unwrap();
            let tools = list["result"]["tools"].as_array().unwrap();
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0]["name"], "list_files_tools_by_type");
            assert_eq!(
                tools[0]["description"],
                "This is text about types and toolsets. ID=[redacted]"
            );
            assert_eq!(
                tools[0]["inputSchema"],
                json!({
                    "$schema":"https://json-schema.org/draft/2020-12/schema",
                    "type":"object","properties":{"status":{"type":"string"}},
                    "required":["status"],"additionalProperties":false
                })
            );
            let call = session
                .call_tool("list_files_tools_by_type", &json!({"status":"hi"}))
                .await
                .unwrap();
            assert_eq!(
                call["result"],
                json!({
                    "content":[{"type":"text","text":"This is text about types and toolsets. ID=[redacted]"}],
                    "structuredContent":{"status":"items", "[redacted]":"[redacted]"},"isError":false,
                })
            );
            session.close().await;
        }
        server.abort();
    }
    #[tokio::test]
    async fn stateless_sessions_negotiate_the_version_and_never_send_delete() {
        let trace = Trace::default();
        let app = Router::new()
            .route(
                "/{version}",
                post(
                    |axum::extract::Path(version): axum::extract::Path<String>,
                     State(trace): State<Trace>,
                     headers: HeaderMap,
                     Json(packet): Json<Value>| async move {
                        assert!(
                            !headers.contains_key(AUTHORIZATION)
                                && !headers.contains_key(SESSION_ID)
                        );
                        let method = packet["method"].as_str().unwrap();
                        trace.record(method);
                        match method {
                            "initialize" => {
                                Json(json!({"jsonrpc":"2.0","id":packet["id"],"result":{
                                    "protocolVersion":version,"capabilities":{"tools":{}},
                                    "serverInfo":{"name":"local","version":"1"}
                                }}))
                                .into_response()
                            }
                            "notifications/initialized" => {
                                assert_eq!(headers[PROTOCOL_VERSION], VERSION);
                                StatusCode::ACCEPTED.into_response()
                            }
                            "tools/list" => Json(
                                json!({"jsonrpc":"2.0","id":packet["id"],"result":{"tools":[]}}),
                            )
                            .into_response(),
                            _ => StatusCode::BAD_REQUEST.into_response(),
                        }
                    },
                )
                .delete(|State(trace): State<Trace>| async move {
                    trace.record("delete");
                    StatusCode::NO_CONTENT
                }),
            )
            .with_state(trace.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut session = HttpSession::connect(&base.join(VERSION).unwrap(), None, None)
            .await
            .unwrap();
        assert_eq!(
            session.list_response().await.unwrap()["result"]["tools"],
            json!([])
        );
        session.close().await;
        assert!(
            HttpSession::connect(&base.join("2024-11-05").unwrap(), None, None)
                .await
                .is_err()
        );
        assert_eq!(trace.count("initialize"), 2);
        assert_eq!(trace.count("notifications/initialized"), 1);
        assert_eq!(trace.count("delete"), 0);
        server.abort();
    }

    #[tokio::test]
    async fn broken_expired_and_redirected_calls_are_never_replayed() {
        let (url, trace, server) = serve().await;
        for name in [
            "redirect",
            "wrong-id",
            "broken",
            "expired-during-stream",
            "expired",
        ] {
            let mut session = HttpSession::connect(&url, Some(BEARER), None)
                .await
                .unwrap();
            let err = session.call_tool(name, &json!({})).await.unwrap_err();
            assert_eq!(err.is::<SessionExpired>(), name == "expired");
            assert!(!format!("{err:#}").contains(BEARER) && !format!("{err:#}").contains(SID));
            session.close().await;
            assert_eq!(trace.count(name), 1);
        }
        assert_eq!(trace.count("tools/call"), 5);
        assert_eq!(trace.count("delete"), 3);
        assert_eq!(trace.count("redirected"), 0);
        server.abort();
    }
}
