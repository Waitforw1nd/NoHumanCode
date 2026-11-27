pub use crate::provider_transport::ProviderProtocol;
use crate::{
    domain::*,
    messages::{
        ContentBlock, ImageSource, NeutralMessage, NeutralRole, ToolCallBlock, from_legacy_values,
    },
    provider_transport::{
        NetworkPurpose, ProviderClient, ProviderPermit, RESPONSE_LIMIT, TransportError,
        read_provider_error,
    },
    provider_usage::UsageObservation,
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

const MAX_TOOL_CALLS_PER_TURN: usize = 8192;

pub fn client() -> Result<ProviderClient> {
    Ok(ProviderClient::new()?)
}

pub async fn json_response(
    response: crate::provider_transport::ProviderResponse,
) -> std::result::Result<(Value, u16), TransportError> {
    let status = response.status();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| TransportError::Unknown)?;
        if chunk.len() > RESPONSE_LIMIT.saturating_sub(bytes.len()) {
            return Err(TransportError::Unknown);
        }
        bytes.extend_from_slice(&chunk);
    }
    let status_code = status.as_u16();
    if !status.is_success() {
        return Err(TransportError::Response {
            status: status_code,
        });
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransportError::InvalidResponse {
            status: status_code,
        })?;
    if value.is_null() {
        return Err(TransportError::InvalidResponse {
            status: status_code,
        });
    }
    Ok((value, status_code))
}

pub async fn models(
    client: &ProviderClient,
    permit: ProviderPermit,
    route: &Route,
) -> std::result::Result<Vec<String>, TransportError> {
    let protocol = permit.protocol();
    let scrub = permit.credential_scrubber();
    permit.matches_with_protocol(
        &route.base_url,
        NetworkPurpose::Models,
        Some(&route.model),
        None,
        route.protocol,
    )?;
    let response = client.send(permit, None).await?;
    let (value, status) = json_response(response).await?;
    if protocol == ProviderProtocol::Gemini
        && let Some(list) = value.get("models").and_then(Value::as_array)
    {
        let keep_prefix = route.model.starts_with("models/");
        return list
            .iter()
            .map(|v| {
                let raw = v["name"]
                    .as_str()
                    .or_else(|| v["id"].as_str())
                    .or_else(|| v.as_str())
                    .ok_or(TransportError::InvalidResponse { status })?;
                if keep_prefix {
                    Ok(scrub(raw))
                } else {
                    Ok(scrub(raw.strip_prefix("models/").unwrap_or(raw)))
                }
            })
            .collect();
    }
    let list = value["data"]
        .as_array()
        .ok_or(TransportError::InvalidResponse { status })?;
    list.iter()
        .map(|v| {
            v["id"]
                .as_str()
                .or(v.as_str())
                .map(&scrub)
                .ok_or(TransportError::InvalidResponse { status })
        })
        .collect()
}

#[derive(Default)]
struct ToolParts {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    event: String,
    done: bool,
    finish: Option<String>,
    pub text: String,
    pub reasoning: String,
    tools: BTreeMap<usize, ToolParts>,
    pub usage: Value,
    usage_observation: UsageObservation,
    cache_read_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    allow_non_object_tool_args: bool,
    allow_empty_text: bool,
    pub response_id: Option<String>,
}

impl Decoder {
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        self.feed_with_reasoning(bytes, emit, &mut |_| Ok(()))
    }

    pub fn feed_with_reasoning(
        &mut self,
        bytes: &[u8],
        emit: &mut impl FnMut(&str) -> Result<()>,
        emit_reasoning: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        self.buffer.extend_from_slice(bytes);
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(emit, emit_reasoning)?;
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.event.is_empty() {
                    self.event.push('\n');
                }
                self.event.push_str(data.trim_start());
            }
        }
        Ok(())
    }

    fn flush(
        &mut self,
        emit: &mut impl FnMut(&str) -> Result<()>,
        emit_reasoning: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        let data = std::mem::take(&mut self.event);
        if data.is_empty() {
            return Ok(());
        }
        ensure!(!self.done, "流式结束后收到额外事件");
        if data == "[DONE]" {
            ensure!(self.finish.is_some(), "流式响应缺少完成原因");
            self.done = true;
            return Ok(());
        }
        let chunk: Value = serde_json::from_str(&data).context("无效的流式响应")?;
        if !chunk["error"].is_null() {
            bail!("模型流返回错误");
        }
        if self.response_id.is_none()
            && let Some(id) = chunk
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        {
            self.response_id = Some(id.to_owned());
        }
        self.usage_observation.observe(&chunk["usage"]);
        self.usage = self.usage_observation.into_value();
        if let Some(cached) = chunk
            .pointer("/usage/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
        {
            self.cache_read_tokens = Some(cached);
        }
        if let Some(reasoning) = chunk
            .pointer("/usage/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
        {
            self.reasoning_tokens = Some(reasoning);
        }
        if let Some(choice) = chunk["choices"].as_array().and_then(|v| v.first()) {
            let delta = &choice["delta"];
            if let Some(reasoning) = delta["reasoning_content"]
                .as_str()
                .or_else(|| delta["reasoning"].as_str())
                && !reasoning.is_empty()
            {
                self.reasoning.push_str(reasoning);
                emit_reasoning(reasoning)?;
            }
            if let Some(text) = delta["content"].as_str()
                && !text.is_empty()
            {
                self.text.push_str(text);
                emit(text)?;
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                self.finish = Some(reason.into());
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for call in calls {
                    let index = call["index"].as_u64().context("工具调用缺少 index")? as usize;
                    ensure!(index < MAX_TOOL_CALLS_PER_TURN, "单轮工具调用过多");
                    let part = self.tools.entry(index).or_default();
                    if let Some(s) = call["id"].as_str() {
                        part.id.push_str(s);
                    }
                    if let Some(s) = call["function"]["name"].as_str() {
                        part.name.push_str(s);
                    }
                    if let Some(s) = call["function"]["arguments"].as_str() {
                        part.arguments.push_str(s);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn finish_neutral(
        self,
        emit: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<NeutralMessage> {
        ensure!(
            self.buffer.is_empty() && self.event.is_empty(),
            "流式连接缺少完整结束事件"
        );
        ensure!(
            self.done && self.finish.is_some(),
            "流式连接提前中断，请继续此成员重试"
        );
        ensure!(
            self.finish.as_deref() != Some("length"),
            "模型输出达到上限，请提高路由的输出上限后重试"
        );
        ensure!(
            self.finish.as_deref() != Some("content_filter"),
            "模型拒绝生成此内容"
        );
        let content = if self.tools.is_empty() && self.text.trim().is_empty() {
            if !self.allow_empty_text {
                ensure!(!self.reasoning.trim().is_empty(), "模型没有返回内容");
            }
            if !self.reasoning.trim().is_empty() {
                emit(&self.reasoning)?;
                self.reasoning.clone()
            } else {
                self.text
            }
        } else {
            self.text
        };
        let mut blocks = Vec::new();
        if !self.reasoning.is_empty() {
            blocks.push(ContentBlock::Thinking {
                text: self.reasoning,
                signature: None,
                encrypted_content: None,
            });
        }
        if !content.is_empty() || self.tools.is_empty() {
            blocks.push(ContentBlock::Text {
                text: content,
                signature: None,
            });
        }
        for part in self.tools.into_values() {
            if self.allow_non_object_tool_args {
                ensure!(!part.name.is_empty(), "不完整的工具调用");
            } else {
                ensure!(
                    !part.id.is_empty() && !part.name.is_empty(),
                    "不完整的工具调用"
                );
                let _: Value = serde_json::from_str(&part.arguments).context("工具参数不完整")?;
            }
            blocks.push(ContentBlock::ToolCall(ToolCallBlock {
                id: part.id,
                name: part.name,
                arguments: part.arguments,
                thought_signature: None,
                namespace: None,
            }));
        }
        Ok(NeutralMessage::assistant(blocks))
    }

    pub fn finish(self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<Value> {
        Ok(self.finish_neutral(emit)?.to_legacy_value())
    }
}

fn push_merged_anthropic_message(out: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last["role"].as_str() == Some(role)
        && let Some(arr) = last["content"].as_array_mut()
    {
        arr.extend(blocks);
        return;
    }
    out.push(json!({"role": role, "content": blocks}));
}

fn anthropic_image_block(source: &ImageSource) -> Value {
    match source {
        ImageSource::Base64 { mime_type, data } => json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": mime_type,
                "data": data
            }
        }),
        ImageSource::Url { url } => match ImageSource::from_image_url(url) {
            ImageSource::Base64 { mime_type, data } => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime_type,
                    "data": data
                }
            }),
            ImageSource::Url { url } => json!({
                "type": "image",
                "source": {
                    "type": "url",
                    "url": url
                }
            }),
        },
    }
}

#[allow(dead_code)]
pub(crate) fn encode_anthropic_body_neutral(
    model: &str,
    max_tokens: u64,
    messages: &[NeutralMessage],
    tools: Option<&[Value]>,
    tool_choice: Option<&Value>,
) -> Result<Value> {
    encode_anthropic_body_neutral_with_thinking(
        model,
        max_tokens,
        messages,
        tools,
        tool_choice,
        None,
    )
}

pub(crate) fn encode_anthropic_body_neutral_with_thinking(
    model: &str,
    max_tokens: u64,
    messages: &[NeutralMessage],
    tools: Option<&[Value]>,
    tool_choice: Option<&Value>,
    thinking_level: Option<&str>,
) -> Result<Value> {
    ensure!(!model.is_empty(), "missing model for Anthropic request");
    let mut system_parts = Vec::new();
    let mut converted_messages = Vec::new();

    for msg in messages {
        match &msg.role {
            NeutralRole::System => {
                let text = msg.text_content();
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            NeutralRole::User | NeutralRole::Custom { .. } => {
                let mut blocks = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                        ContentBlock::Image { source } => {
                            blocks.push(anthropic_image_block(source));
                        }
                        other => {
                            if let Some(placeholder) =
                                crate::messages::format_media_block_placeholder(other)
                            {
                                blocks.push(json!({"type": "text", "text": placeholder}));
                            }
                        }
                    }
                }
                if blocks.is_empty() && matches!(msg.role, NeutralRole::User) {
                    blocks.push(json!({"type": "text", "text": ""}));
                }
                push_merged_anthropic_message(&mut converted_messages, "user", blocks);
            }
            NeutralRole::Assistant => {
                let mut blocks = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Thinking {
                            text, signature, ..
                        } => {
                            if let Some(sig) = signature.as_deref().filter(|s| !s.is_empty()) {
                                blocks.push(json!({
                                    "type": "thinking",
                                    "thinking": text,
                                    "signature": sig
                                }));
                            }
                        }
                        ContentBlock::RedactedThinking { data } => {
                            blocks.push(json!({
                                "type": "redacted_thinking",
                                "data": data
                            }));
                        }
                        ContentBlock::Text { text, .. } => {
                            if !text.is_empty() {
                                blocks.push(json!({"type": "text", "text": text}));
                            }
                        }
                        ContentBlock::ToolCall(tc) => {
                            ensure!(!tc.id.is_empty(), "assistant tool_call missing id");
                            ensure!(!tc.name.is_empty(), "assistant tool_call missing name");
                            let parsed: Value =
                                serde_json::from_str(&tc.arguments).unwrap_or_else(|_| json!({}));
                            let input = if parsed.is_object() {
                                parsed
                            } else {
                                json!({})
                            };
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.name,
                                "input": input
                            }));
                        }
                        ContentBlock::Image { .. }
                        | ContentBlock::Audio { .. }
                        | ContentBlock::Video { .. }
                        | ContentBlock::Resource { .. } => {}
                    }
                }
                if blocks.is_empty() {
                    blocks.push(json!({"type": "text", "text": ""}));
                }
                push_merged_anthropic_message(&mut converted_messages, "assistant", blocks);
            }
            NeutralRole::ToolResult {
                call_id, is_error, ..
            } => {
                ensure!(!call_id.is_empty(), "tool message missing tool_call_id");
                let has_image = msg
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Image { .. }));
                let has_media = msg.content.iter().any(|b| {
                    matches!(
                        b,
                        ContentBlock::Audio { .. }
                            | ContentBlock::Video { .. }
                            | ContentBlock::Resource { .. }
                    )
                });
                let content_val = if has_image {
                    let mut parts = Vec::new();
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text, .. } => {
                                parts.push(json!({"type": "text", "text": text}));
                            }
                            ContentBlock::Image { source } => {
                                parts.push(anthropic_image_block(source));
                            }
                            other => {
                                if let Some(placeholder) =
                                    crate::messages::format_media_block_placeholder(other)
                                {
                                    parts.push(json!({"type": "text", "text": placeholder}));
                                }
                            }
                        }
                    }
                    Value::Array(parts)
                } else if has_media {
                    Value::String(crate::messages::content_text_with_media_placeholders(
                        &msg.content,
                    ))
                } else {
                    Value::String(msg.text_content())
                };
                let mut block = json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content_val
                });
                if *is_error {
                    block["is_error"] = json!(true);
                }
                push_merged_anthropic_message(&mut converted_messages, "user", vec![block]);
            }
        }
    }

    let mut out = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": converted_messages,
        "stream": true
    });
    if !system_parts.is_empty() {
        out["system"] = Value::String(system_parts.join("\n\n"));
    }
    if let Some(level) = thinking_level
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty() && !matches!(e.as_str(), "off" | "none"))
    {
        if let Some(is_v46) = parse_claude_adaptive_model(model) {
            let effort = match level.as_str() {
                "minimal" | "low" => "low",
                "medium" => "medium",
                "high" => "high",
                "xhigh" => {
                    if is_v46 {
                        "max"
                    } else {
                        "xhigh"
                    }
                }
                "max" | "ultra" => "max",
                _ => "medium",
            };
            out["thinking"] = json!({
                "type": "adaptive"
            });
            out["output_config"] = json!({
                "effort": effort
            });
        } else if max_tokens > 1 {
            let nominal: u64 = match level.as_str() {
                "minimal" => 1024,
                "low" => 2048,
                "medium" => 8192,
                "high" => 16_384,
                "xhigh" | "max" | "ultra" => 32_768,
                _ => 8192,
            };
            let budget = nominal.min(max_tokens.saturating_sub(1));
            if budget > 0 {
                out["thinking"] = json!({
                    "type": "enabled",
                    "budget_tokens": budget
                });
            }
        }
    }
    if let Some(tools) = tools
        && !tools.is_empty()
    {
        let mut anthropic_tools = Vec::with_capacity(tools.len());
        for tool in tools {
            if let Some(func) = tool.get("function") {
                let name = func
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .context("tool missing function.name")?;
                let description = func
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let input_schema = func
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                anthropic_tools.push(json!({
                    "name": name,
                    "description": description,
                    "input_schema": input_schema
                }));
            } else if tool.get("name").is_some() && tool.get("input_schema").is_some() {
                anthropic_tools.push(tool.clone());
            }
        }
        if !anthropic_tools.is_empty() {
            out["tools"] = Value::Array(anthropic_tools);
            if let Some(choice) = tool_choice {
                if choice == "auto" {
                    out["tool_choice"] = json!({"type": "auto"});
                } else if choice == "required" || choice == "any" {
                    out["tool_choice"] = json!({"type": "any"});
                } else if choice.is_object() {
                    out["tool_choice"] = choice.clone();
                }
            }
        }
    }
    Ok(out)
}

fn parse_claude_adaptive_model(model: &str) -> Option<bool> {
    let base = model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .split('@')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let rest = base.strip_prefix("claude-")?;
    let parts: Vec<&str> = rest.split(['-', '.']).filter(|s| !s.is_empty()).collect();
    let parse_small = |s: Option<&&str>| -> Option<u32> {
        s.and_then(|v| v.parse::<u32>().ok()).filter(|&n| n < 100)
    };
    let (major, minor) = if let Some(m) = parse_small(parts.first()) {
        (m, parse_small(parts.get(1)).unwrap_or(0))
    } else if matches!(
        parts.first().copied(),
        Some("opus" | "sonnet" | "haiku" | "fable")
    ) {
        let m = parse_small(parts.get(1))?;
        (m, parse_small(parts.get(2)).unwrap_or(0))
    } else {
        return None;
    };
    if major >= 5 {
        Some(false)
    } else if major == 4 && minor >= 6 {
        Some(minor == 6)
    } else {
        None
    }
}

fn encode_anthropic_body(openai_body: &Value) -> Result<Value> {
    let model = openai_body
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("missing model for Anthropic request")?;
    let max_tokens = openai_body
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(4096);
    let raw_messages = openai_body
        .get("messages")
        .and_then(Value::as_array)
        .context("missing messages array")?;
    let messages = from_legacy_values(raw_messages)?;
    let tools = openai_body
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let tool_choice = openai_body.get("tool_choice");
    let thinking_level = openai_body
        .get("thinking_level")
        .or_else(|| openai_body.get("reasoning_effort"))
        .and_then(Value::as_str);
    encode_anthropic_body_neutral_with_thinking(
        model,
        max_tokens,
        &messages,
        tools,
        tool_choice,
        thinking_level,
    )
}

fn sanitize_gemini_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut clean = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                if k == "additionalProperties" && !v.is_object() {
                    continue;
                }
                clean.insert(k.clone(), sanitize_gemini_schema(v));
            }
            Value::Object(clean)
        }
        Value::Array(items) => Value::Array(items.iter().map(sanitize_gemini_schema).collect()),
        other => other.clone(),
    }
}

fn push_merged_gemini_content(out: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last["role"].as_str() == Some(role)
        && let Some(arr) = last["parts"].as_array_mut()
    {
        arr.extend(parts);
        return;
    }
    out.push(json!({"role": role, "parts": parts}));
}

fn gemini_inline_data_part(source: &ImageSource) -> Option<Value> {
    match source {
        ImageSource::Base64 { mime_type, data } => Some(json!({
            "inlineData": {
                "mimeType": mime_type,
                "data": data
            }
        })),
        ImageSource::Url { url } => match ImageSource::from_image_url(url) {
            ImageSource::Base64 { mime_type, data } => Some(json!({
                "inlineData": {
                    "mimeType": mime_type,
                    "data": data
                }
            })),
            ImageSource::Url { .. } => None,
        },
    }
}

enum GeminiThinkingMode {
    None,
    Budget { is_pro: bool, is_flash: bool },
    Level { is_pro: bool },
}

fn classify_gemini_thinking_mode(model: &str) -> GeminiThinkingMode {
    let normalized = model
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .split('@')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if normalized.starts_with("gemini-2.5") {
        return GeminiThinkingMode::Budget {
            is_pro: normalized.contains("pro"),
            is_flash: normalized.contains("flash"),
        };
    }
    if normalized.starts_with("gemini-flash-latest")
        || normalized.starts_with("gemini-flash-lite-latest")
        || normalized.starts_with("gemini-pro-latest")
        || normalized.starts_with("gemini-robotics-er-2")
        || normalized.starts_with("gemma-4")
    {
        return GeminiThinkingMode::Level {
            is_pro: normalized.contains("-pro"),
        };
    }
    if let Some(rest) = normalized.strip_prefix("gemini-")
        && let Some(major_str) = rest.split(['-', '.']).next()
        && major_str.parse::<u32>().is_ok_and(|major| major >= 3)
    {
        return GeminiThinkingMode::Level {
            is_pro: normalized.contains("-pro"),
        };
    }
    GeminiThinkingMode::None
}

fn gemini_thinking_config(model: &str, thinking_level: Option<&str>) -> Option<Value> {
    let level = thinking_level
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())?;
    let mode = classify_gemini_thinking_mode(model);
    if matches!(level.as_str(), "off" | "none") {
        return match mode {
            GeminiThinkingMode::Budget { is_flash: true, .. } => {
                Some(json!({ "thinkingBudget": 0 }))
            }
            _ => None,
        };
    }
    match mode {
        GeminiThinkingMode::None => None,
        GeminiThinkingMode::Budget { is_pro, .. } => {
            let budget: u64 = match level.as_str() {
                "minimal" => 1024,
                "low" => 2048,
                "medium" => 8192,
                "high" => 16_384,
                "xhigh" | "max" | "ultra" => {
                    if is_pro {
                        32_768
                    } else {
                        24_576
                    }
                }
                _ => 8192,
            };
            Some(json!({
                "thinkingBudget": budget,
                "includeThoughts": true
            }))
        }
        GeminiThinkingMode::Level { is_pro } => {
            let mapped = match level.as_str() {
                "minimal" => {
                    if is_pro {
                        "low"
                    } else {
                        "minimal"
                    }
                }
                "low" => "low",
                "medium" => "medium",
                "high" | "xhigh" | "max" | "ultra" => "high",
                _ => "medium",
            };
            Some(json!({
                "thinkingLevel": mapped,
                "includeThoughts": true
            }))
        }
    }
}

#[allow(dead_code)]
pub(crate) fn encode_gemini_body_neutral(
    max_tokens: u64,
    messages: &[NeutralMessage],
    tools: Option<&[Value]>,
) -> Result<Value> {
    encode_gemini_body_neutral_with_thinking("", max_tokens, messages, tools, None)
}

pub(crate) fn encode_gemini_body_neutral_with_thinking(
    model: &str,
    max_tokens: u64,
    messages: &[NeutralMessage],
    tools: Option<&[Value]>,
    thinking_level: Option<&str>,
) -> Result<Value> {
    let mut call_names = BTreeMap::new();
    for msg in messages {
        if matches!(msg.role, NeutralRole::Assistant) {
            for call in msg.tool_calls() {
                call_names.insert(call.id.clone(), call.name.clone());
            }
        }
    }

    let mut system_parts = Vec::new();
    let mut contents = Vec::new();

    for msg in messages {
        match &msg.role {
            NeutralRole::System => {
                let text = msg.text_content();
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            NeutralRole::User | NeutralRole::Custom { .. } => {
                let mut parts = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            parts.push(json!({"text": text}));
                        }
                        ContentBlock::Image { source } => {
                            if let Some(part) = gemini_inline_data_part(source) {
                                parts.push(part);
                            }
                        }
                        other => {
                            if let Some(placeholder) =
                                crate::messages::format_media_block_placeholder(other)
                            {
                                parts.push(json!({"text": placeholder}));
                            }
                        }
                    }
                }
                if parts.is_empty() && matches!(msg.role, NeutralRole::User) {
                    parts.push(json!({"text": ""}));
                }
                push_merged_gemini_content(&mut contents, "user", parts);
            }
            NeutralRole::Assistant => {
                let mut parts = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Thinking {
                            text, signature, ..
                        } => {
                            if let Some(sig) = signature.as_deref().filter(|s| !s.is_empty()) {
                                let mut part = json!({
                                    "thought": true,
                                    "thoughtSignature": sig
                                });
                                if !text.is_empty() {
                                    part["text"] = json!(text);
                                }
                                parts.push(part);
                            }
                        }
                        ContentBlock::Text { text, signature } => {
                            let sig = signature.as_deref().filter(|s| !s.is_empty());
                            if !text.is_empty() || sig.is_some() {
                                let mut part = json!({"text": text});
                                if let Some(sig) = sig {
                                    part["thoughtSignature"] = json!(sig);
                                }
                                parts.push(part);
                            }
                        }
                        ContentBlock::ToolCall(tc) => {
                            ensure!(!tc.name.is_empty(), "assistant tool_call missing name");
                            let parsed: Value =
                                serde_json::from_str(&tc.arguments).unwrap_or_else(|_| json!({}));
                            let args = if parsed.is_object() {
                                parsed
                            } else {
                                json!({})
                            };
                            let mut part = json!({
                                "functionCall": {
                                    "name": tc.name,
                                    "args": args
                                }
                            });
                            if let Some(sig) =
                                tc.thought_signature.as_deref().filter(|s| !s.is_empty())
                            {
                                part["thoughtSignature"] = json!(sig);
                            }
                            parts.push(part);
                        }
                        ContentBlock::Image { source } => {
                            if let Some(part) = gemini_inline_data_part(source) {
                                parts.push(part);
                            }
                        }
                        ContentBlock::RedactedThinking { .. }
                        | ContentBlock::Audio { .. }
                        | ContentBlock::Video { .. }
                        | ContentBlock::Resource { .. } => {}
                    }
                }
                if parts.is_empty() {
                    parts.push(json!({"text": ""}));
                }
                push_merged_gemini_content(&mut contents, "model", parts);
            }
            NeutralRole::ToolResult {
                call_id,
                name,
                is_error,
            } => {
                let resolved_name = name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .or_else(|| call_names.get(call_id).map(String::as_str))
                    .unwrap_or("tool");
                let text = msg.text_content();
                let response_obj = match serde_json::from_str::<Value>(&text) {
                    Ok(v) if v.is_object() => v,
                    _ if *is_error => json!({"error": text}),
                    _ => json!({"result": text}),
                };
                let mut parts = vec![json!({
                    "functionResponse": {
                        "name": resolved_name,
                        "response": response_obj
                    }
                })];
                for block in &msg.content {
                    if let ContentBlock::Image { source } = block
                        && let Some(part) = gemini_inline_data_part(source)
                    {
                        parts.push(part);
                    } else if let Some(placeholder) =
                        crate::messages::format_media_block_placeholder(block)
                    {
                        parts.push(json!({"text": placeholder}));
                    }
                }
                push_merged_gemini_content(&mut contents, "user", parts);
            }
        }
    }

    let mut generation_config = json!({
        "maxOutputTokens": max_tokens
    });
    if let Some(thinking_config) = gemini_thinking_config(model, thinking_level) {
        generation_config["thinkingConfig"] = thinking_config;
    }
    let mut out = json!({
        "contents": contents,
        "generationConfig": generation_config
    });
    if !system_parts.is_empty() {
        out["systemInstruction"] = json!({
            "parts": [{"text": system_parts.join("\n\n")}]
        });
    }
    if let Some(tools) = tools
        && !tools.is_empty()
    {
        let mut declarations = Vec::with_capacity(tools.len());
        for tool in tools {
            let func = tool.get("function").unwrap_or(tool);
            let name = func
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .context("tool missing name")?;
            let description = func
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            let mut decl = json!({
                "name": name,
                "description": description
            });
            if let Some(params) = func.get("parameters") {
                decl["parameters"] = sanitize_gemini_schema(params);
            }
            declarations.push(decl);
        }
        if !declarations.is_empty() {
            out["tools"] = json!([{"functionDeclarations": declarations}]);
        }
    }
    Ok(out)
}

fn encode_gemini_body(openai_body: &Value) -> Result<Value> {
    let model = openai_body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("");
    let max_tokens = openai_body
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(4096);
    let raw_messages = openai_body
        .get("messages")
        .and_then(Value::as_array)
        .context("missing messages array")?;
    let messages = from_legacy_values(raw_messages)?;
    let tools = openai_body
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let thinking_level = openai_body
        .get("thinking_level")
        .or_else(|| openai_body.get("reasoning_effort"))
        .and_then(Value::as_str);
    encode_gemini_body_neutral_with_thinking(model, max_tokens, &messages, tools, thinking_level)
}

pub(crate) fn encode_responses_body_neutral(
    model: &str,
    max_output_tokens: u64,
    messages: &[NeutralMessage],
    tools: Option<&[Value]>,
    tool_choice: Option<&Value>,
    reasoning_effort: Option<&str>,
    reasoning_summary: Option<&str>,
) -> Result<Value> {
    ensure!(!model.is_empty(), "missing model for Responses request");
    let mut system_parts = Vec::new();
    let mut input = Vec::new();

    for msg in messages {
        match &msg.role {
            NeutralRole::System => {
                let text = msg.text_content();
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            NeutralRole::User | NeutralRole::Custom { .. } => {
                let mut parts = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            parts.push(json!({"type": "input_text", "text": text}));
                        }
                        ContentBlock::Image { source } => {
                            parts.push(json!({
                                "type": "input_image",
                                "image_url": source.into_data_uri()
                            }));
                        }
                        other => {
                            if let Some(placeholder) =
                                crate::messages::format_media_block_placeholder(other)
                            {
                                parts.push(json!({"type": "input_text", "text": placeholder}));
                            }
                        }
                    }
                }
                if parts.is_empty() && matches!(msg.role, NeutralRole::User) {
                    parts.push(json!({"type": "input_text", "text": ""}));
                }
                if !parts.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": parts
                    }));
                }
            }
            NeutralRole::Assistant => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Thinking {
                            text,
                            encrypted_content: Some(enc),
                            ..
                        } if !enc.is_empty() => {
                            let summary = if text.is_empty() {
                                Vec::new()
                            } else {
                                vec![json!({"type": "summary_text", "text": text})]
                            };
                            input.push(json!({
                                "type": "reasoning",
                                "summary": summary,
                                "encrypted_content": enc
                            }));
                        }
                        ContentBlock::Text { text, .. } if !text.is_empty() => {
                            input.push(json!({
                                "type": "message",
                                "role": "assistant",
                                "status": "completed",
                                "content": [{
                                    "type": "output_text",
                                    "text": text,
                                    "annotations": []
                                }]
                            }));
                        }
                        ContentBlock::ToolCall(tc) => {
                            ensure!(!tc.id.is_empty(), "assistant tool_call missing id");
                            ensure!(!tc.name.is_empty(), "assistant tool_call missing name");
                            let arguments = if tc.arguments.trim().is_empty() {
                                "{}".to_owned()
                            } else {
                                tc.arguments.clone()
                            };
                            input.push(json!({
                                "type": "function_call",
                                "call_id": tc.id,
                                "name": tc.name,
                                "arguments": arguments
                            }));
                        }
                        _ => {}
                    }
                }
            }
            NeutralRole::ToolResult { call_id, .. } => {
                ensure!(!call_id.is_empty(), "tool message missing tool_call_id");
                let has_image = msg
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Image { .. }));
                let has_media = msg.content.iter().any(|b| {
                    matches!(
                        b,
                        ContentBlock::Audio { .. }
                            | ContentBlock::Video { .. }
                            | ContentBlock::Resource { .. }
                    )
                });
                let output = if has_image {
                    let mut parts = Vec::new();
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text, .. } => {
                                parts.push(json!({"type": "input_text", "text": text}));
                            }
                            ContentBlock::Image { source } => {
                                parts.push(json!({
                                    "type": "input_image",
                                    "image_url": source.into_data_uri()
                                }));
                            }
                            other => {
                                if let Some(placeholder) =
                                    crate::messages::format_media_block_placeholder(other)
                                {
                                    parts.push(json!({"type": "input_text", "text": placeholder}));
                                }
                            }
                        }
                    }
                    Value::Array(parts)
                } else if has_media {
                    Value::String(crate::messages::content_text_with_media_placeholders(
                        &msg.content,
                    ))
                } else {
                    Value::String(msg.text_content())
                };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output
                }));
            }
        }
    }

    let mut out = json!({
        "model": model,
        "input": input,
        "max_output_tokens": max_output_tokens,
        "store": false,
        "stream": true,
        "include": ["reasoning.encrypted_content"]
    });
    if !system_parts.is_empty() {
        out["instructions"] = Value::String(system_parts.join("\n\n"));
    }
    if let Some(effort) = reasoning_effort
        .map(str::trim)
        .filter(|e| !e.is_empty() && !matches!(*e, "off" | "none"))
    {
        out["reasoning"] = json!({
            "effort": effort,
            "summary": reasoning_summary.unwrap_or("auto")
        });
    }
    if let Some(tools) = tools
        && !tools.is_empty()
    {
        let mut flat_tools = Vec::with_capacity(tools.len());
        for tool in tools {
            if let Some(func) = tool.get("function") {
                let name = func
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .context("tool missing function.name")?;
                let mut item = json!({
                    "type": "function",
                    "name": name,
                    "description": func.get("description").and_then(Value::as_str).unwrap_or("")
                });
                if let Some(params) = func.get("parameters") {
                    item["parameters"] = params.clone();
                }
                flat_tools.push(item);
            } else if tool.get("name").is_some() {
                let mut item = tool.clone();
                if item.get("type").is_none() {
                    item["type"] = json!("function");
                }
                flat_tools.push(item);
            }
        }
        if !flat_tools.is_empty() {
            out["tools"] = Value::Array(flat_tools);
            if let Some(choice) = tool_choice {
                if let Some(s) = choice.as_str() {
                    out["tool_choice"] = json!(s);
                } else if let Some(name) = choice.pointer("/function/name").and_then(Value::as_str)
                {
                    out["tool_choice"] = json!({"type": "function", "name": name});
                } else if choice.is_object() {
                    out["tool_choice"] = choice.clone();
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn encode_responses_body(openai_body: &Value) -> Result<Value> {
    let model = openai_body
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("missing model for Responses request")?;
    let max_output_tokens = openai_body
        .get("max_tokens")
        .or_else(|| openai_body.get("max_output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(4096);
    let raw_messages = openai_body
        .get("messages")
        .and_then(Value::as_array)
        .context("missing messages array")?;
    let messages = from_legacy_values(raw_messages)?;
    let tools = openai_body
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice);
    let tool_choice = openai_body.get("tool_choice");
    let reasoning_effort = openai_body
        .get("reasoning_effort")
        .or_else(|| openai_body.get("thinking_level"))
        .or_else(|| openai_body.pointer("/reasoning/effort"))
        .and_then(Value::as_str);
    let reasoning_summary = openai_body
        .pointer("/reasoning/summary")
        .and_then(Value::as_str);
    encode_responses_body_neutral(
        model,
        max_output_tokens,
        &messages,
        tools,
        tool_choice,
        reasoning_effort,
        reasoning_summary,
    )
}

pub(crate) fn encode_neutral_protocol_body(
    protocol: ProviderProtocol,
    input: &crate::provider_runtime::CompletionInput<'_>,
) -> Result<Value> {
    encode_neutral_protocol_body_with_thinking(protocol, input, None)
}

pub(crate) fn encode_neutral_protocol_body_with_thinking(
    protocol: ProviderProtocol,
    input: &crate::provider_runtime::CompletionInput<'_>,
    thinking_level: Option<&str>,
) -> Result<Value> {
    let tools = (!input.tools.is_empty()).then_some(input.tools);
    let tool_choice = tools.map(|_| json!("auto"));
    match protocol {
        ProviderProtocol::OpenAi => {
            let mut body = crate::provider_runtime::expected_body(input);
            if let Some(effort) = thinking_level
                .map(str::trim)
                .filter(|e| !e.is_empty() && !matches!(*e, "off" | "none"))
            {
                body["reasoning_effort"] = json!(effort);
            }
            Ok(body)
        }
        ProviderProtocol::OpenAiResponses => encode_responses_body_neutral(
            input.model,
            u64::from(input.max_tokens),
            input.messages,
            tools,
            tool_choice.as_ref(),
            thinking_level,
            None,
        ),
        ProviderProtocol::Anthropic => encode_anthropic_body_neutral_with_thinking(
            input.model,
            u64::from(input.max_tokens),
            input.messages,
            tools,
            tool_choice.as_ref(),
            thinking_level,
        ),
        ProviderProtocol::Gemini => encode_gemini_body_neutral_with_thinking(
            input.model,
            u64::from(input.max_tokens),
            input.messages,
            tools,
            thinking_level,
        ),
    }
}

pub fn encode_protocol_body(protocol: ProviderProtocol, openai_body: &Value) -> Result<Value> {
    match protocol {
        ProviderProtocol::OpenAi => Ok(openai_body.clone()),
        ProviderProtocol::OpenAiResponses => encode_responses_body(openai_body),
        ProviderProtocol::Anthropic => encode_anthropic_body(openai_body),
        ProviderProtocol::Gemini => encode_gemini_body(openai_body),
    }
}

#[derive(Default)]
struct AnthropicToolParts {
    id: String,
    name: String,
    initial_input: Option<Value>,
    partial_json: String,
}

enum AnthropicStreamBlock {
    Text { text: String },
    Thinking { thinking: String, signature: String },
    RedactedThinking { data: String },
    ToolUse(AnthropicToolParts),
}

#[derive(Default)]
pub struct AnthropicDecoder {
    buffer: Vec<u8>,
    event_name: String,
    event_data: String,
    done: bool,
    stop_reason: Option<String>,
    pub text: String,
    pub reasoning: String,
    blocks: BTreeMap<usize, AnthropicStreamBlock>,
    tool_count: usize,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    raw_input_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    allow_non_object_tool_args: bool,
    allow_empty_text: bool,
    pub response_id: Option<String>,
    pub usage: Value,
}

impl AnthropicDecoder {
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        self.feed_with_reasoning(bytes, emit, &mut |_| Ok(()))
    }

    pub fn feed_with_reasoning(
        &mut self,
        bytes: &[u8],
        emit: &mut impl FnMut(&str) -> Result<()>,
        emit_reasoning: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        self.buffer.extend_from_slice(bytes);
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(emit, emit_reasoning)?;
            } else if let Some(ev) = line.strip_prefix("event:") {
                self.event_name = ev.trim().to_owned();
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.event_data.is_empty() {
                    self.event_data.push('\n');
                }
                self.event_data.push_str(data.trim_start());
            }
        }
        Ok(())
    }

    fn refresh_usage(&mut self) {
        if self.input_tokens.is_some() || self.output_tokens.is_some() {
            let prompt = self.input_tokens.unwrap_or(0);
            let completion = self.output_tokens.unwrap_or(0);
            let total = prompt.saturating_add(completion);
            self.usage = json!({
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": total
            });
        }
    }

    fn flush(
        &mut self,
        emit: &mut impl FnMut(&str) -> Result<()>,
        emit_reasoning: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        let event_name = std::mem::take(&mut self.event_name);
        let data = std::mem::take(&mut self.event_data);
        if data.is_empty() {
            return Ok(());
        }
        ensure!(!self.done, "流式结束后收到额外事件");
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let chunk: Value = serde_json::from_str(&data).context("无效的流式响应")?;
        if chunk.get("type").and_then(Value::as_str) == Some("error") || !chunk["error"].is_null() {
            bail!("模型流返回错误");
        }
        let kind = chunk
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(event_name.as_str());
        match kind {
            "ping" | "content_block_stop" => {}
            "message_start" => {
                if let Some(id) = chunk
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    self.response_id = Some(id.to_owned());
                }
                if let Some(u) = chunk["message"].get("usage").and_then(Value::as_object) {
                    let input = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let cache_create = u
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let cache_read = u
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    self.raw_input_tokens = Some(input);
                    self.cache_write_tokens = Some(cache_create);
                    self.cache_read_tokens = Some(cache_read);
                    self.input_tokens = Some(
                        input
                            .saturating_add(cache_create)
                            .saturating_add(cache_read),
                    );
                    if let Some(out) = u.get("output_tokens").and_then(Value::as_u64) {
                        self.output_tokens = Some(out);
                    }
                    self.refresh_usage();
                }
            }
            "content_block_start" => {
                let index = chunk.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block = &chunk["content_block"];
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let initial = block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if !initial.is_empty() {
                            self.text.push_str(initial);
                            emit(initial)?;
                        }
                        self.blocks.insert(
                            index,
                            AnthropicStreamBlock::Text {
                                text: initial.to_owned(),
                            },
                        );
                    }
                    Some("thinking") => {
                        let thinking = block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if !thinking.is_empty() {
                            self.reasoning.push_str(thinking);
                            emit_reasoning(thinking)?;
                        }
                        let signature = block
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        self.blocks.insert(
                            index,
                            AnthropicStreamBlock::Thinking {
                                thinking: thinking.to_owned(),
                                signature,
                            },
                        );
                    }
                    Some("redacted_thinking") => {
                        let data = block
                            .get("data")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        self.blocks
                            .insert(index, AnthropicStreamBlock::RedactedThinking { data });
                    }
                    Some("tool_use") => {
                        ensure!(
                            self.tool_count < MAX_TOOL_CALLS_PER_TURN,
                            "单轮工具调用过多"
                        );
                        self.tool_count += 1;
                        let part = AnthropicToolParts {
                            id: block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            name: block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            initial_input: block.get("input").cloned(),
                            partial_json: String::new(),
                        };
                        self.blocks
                            .insert(index, AnthropicStreamBlock::ToolUse(part));
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let index = chunk.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let delta = &chunk["delta"];
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str)
                            && !text.is_empty()
                        {
                            self.text.push_str(text);
                            emit(text)?;
                            match self.blocks.get_mut(&index) {
                                Some(AnthropicStreamBlock::Text { text: existing }) => {
                                    existing.push_str(text);
                                }
                                _ => {
                                    self.blocks.insert(
                                        index,
                                        AnthropicStreamBlock::Text {
                                            text: text.to_owned(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(thinking) = delta.get("thinking").and_then(Value::as_str)
                            && !thinking.is_empty()
                        {
                            self.reasoning.push_str(thinking);
                            emit_reasoning(thinking)?;
                            match self.blocks.get_mut(&index) {
                                Some(AnthropicStreamBlock::Thinking {
                                    thinking: existing, ..
                                }) => {
                                    existing.push_str(thinking);
                                }
                                _ => {
                                    self.blocks.insert(
                                        index,
                                        AnthropicStreamBlock::Thinking {
                                            thinking: thinking.to_owned(),
                                            signature: String::new(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(sig) = delta.get("signature").and_then(Value::as_str)
                            && !sig.is_empty()
                        {
                            match self.blocks.get_mut(&index) {
                                Some(AnthropicStreamBlock::Thinking { signature, .. }) => {
                                    signature.push_str(sig);
                                }
                                _ => {
                                    self.blocks.insert(
                                        index,
                                        AnthropicStreamBlock::Thinking {
                                            thinking: String::new(),
                                            signature: sig.to_owned(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(pj) = delta.get("partial_json").and_then(Value::as_str) {
                            match self.blocks.get_mut(&index) {
                                Some(AnthropicStreamBlock::ToolUse(part)) => {
                                    part.partial_json.push_str(pj);
                                }
                                _ => {
                                    ensure!(
                                        self.tool_count < MAX_TOOL_CALLS_PER_TURN,
                                        "单轮工具调用过多"
                                    );
                                    self.tool_count += 1;
                                    self.blocks.insert(
                                        index,
                                        AnthropicStreamBlock::ToolUse(AnthropicToolParts {
                                            partial_json: pj.to_owned(),
                                            ..Default::default()
                                        }),
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = chunk["delta"].get("stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_owned());
                }
                if let Some(u) = chunk.get("usage").and_then(Value::as_object) {
                    if let Some(inp) = u.get("input_tokens").and_then(Value::as_u64) {
                        let cache_create = u
                            .get("cache_creation_input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        let cache_read = u
                            .get("cache_read_input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        self.raw_input_tokens = Some(inp);
                        self.cache_write_tokens = Some(cache_create);
                        self.cache_read_tokens = Some(cache_read);
                        self.input_tokens =
                            Some(inp.saturating_add(cache_create).saturating_add(cache_read));
                    }
                    if let Some(out) = u.get("output_tokens").and_then(Value::as_u64) {
                        self.output_tokens = Some(out);
                    }
                    self.refresh_usage();
                }
            }
            "message_stop" => {
                self.done = true;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn finish_neutral(
        self,
        emit: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<NeutralMessage> {
        ensure!(
            self.buffer.is_empty() && self.event_data.is_empty(),
            "流式连接缺少完整结束事件"
        );
        ensure!(self.done, "流式连接提前中断，请继续此成员重试");
        ensure!(
            self.stop_reason.as_deref() != Some("max_tokens"),
            "模型输出达到上限，请提高路由的输出上限后重试"
        );
        ensure!(
            self.stop_reason.as_deref() != Some("refusal"),
            "模型拒绝生成此内容"
        );

        let mut content_blocks = Vec::new();
        let mut has_tools = false;
        let mut has_non_blank_text = false;
        let mut has_redacted = false;

        for block in self.blocks.into_values() {
            match block {
                AnthropicStreamBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    if !thinking.is_empty() || !signature.is_empty() {
                        content_blocks.push(ContentBlock::Thinking {
                            text: thinking,
                            signature: (!signature.is_empty()).then_some(signature),
                            encrypted_content: None,
                        });
                    }
                }
                AnthropicStreamBlock::RedactedThinking { data } => {
                    has_redacted = true;
                    content_blocks.push(ContentBlock::RedactedThinking { data });
                }
                AnthropicStreamBlock::Text { text } => {
                    if !text.trim().is_empty() {
                        has_non_blank_text = true;
                    }
                    if !text.is_empty() {
                        content_blocks.push(ContentBlock::Text {
                            text,
                            signature: None,
                        });
                    }
                }
                AnthropicStreamBlock::ToolUse(part) => {
                    has_tools = true;
                    if self.allow_non_object_tool_args {
                        ensure!(!part.name.is_empty(), "不完整的工具调用");
                    } else {
                        ensure!(
                            !part.id.is_empty() && !part.name.is_empty(),
                            "不完整的工具调用"
                        );
                    }
                    let arguments = if self.allow_non_object_tool_args {
                        if !part.partial_json.trim().is_empty() {
                            part.partial_json
                        } else if let Some(initial) = part.initial_input {
                            initial.to_string()
                        } else {
                            "{}".to_owned()
                        }
                    } else if !part.partial_json.trim().is_empty() {
                        let parsed: Value =
                            serde_json::from_str(&part.partial_json).context("工具参数不完整")?;
                        ensure!(parsed.is_object(), "工具参数必须是对象");
                        part.partial_json
                    } else if let Some(initial) = part.initial_input.filter(Value::is_object) {
                        initial.to_string()
                    } else {
                        bail!("工具参数不完整");
                    };
                    content_blocks.push(ContentBlock::ToolCall(ToolCallBlock {
                        id: part.id,
                        name: part.name,
                        arguments,
                        thought_signature: None,
                        namespace: None,
                    }));
                }
            }
        }

        if !has_tools && !has_non_blank_text {
            if !has_redacted && !self.allow_empty_text {
                ensure!(!self.reasoning.trim().is_empty(), "模型没有返回内容");
            }
            if !self.reasoning.trim().is_empty() {
                emit(&self.reasoning)?;
                content_blocks.push(ContentBlock::Text {
                    text: self.reasoning,
                    signature: None,
                });
            }
        }

        Ok(NeutralMessage::assistant(content_blocks))
    }

    pub fn finish(self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<Value> {
        Ok(self.finish_neutral(emit)?.to_legacy_value())
    }
}

#[derive(Default)]
pub struct GeminiDecoder {
    buffer: Vec<u8>,
    event: String,
    done: bool,
    saw_chunk: bool,
    finish_reason: Option<String>,
    pub text: String,
    pub reasoning: String,
    blocks: Vec<ContentBlock>,
    tool_count: usize,
    pub usage: Value,
}

impl GeminiDecoder {
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        self.feed_with_reasoning(bytes, emit, &mut |_| Ok(()))
    }

    pub fn feed_with_reasoning(
        &mut self,
        bytes: &[u8],
        emit: &mut impl FnMut(&str) -> Result<()>,
        emit_reasoning: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        self.buffer.extend_from_slice(bytes);
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(emit, emit_reasoning)?;
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.event.is_empty() {
                    self.event.push('\n');
                }
                self.event.push_str(data.trim_start());
            }
        }
        Ok(())
    }

    fn flush(
        &mut self,
        emit: &mut impl FnMut(&str) -> Result<()>,
        emit_reasoning: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        let data = std::mem::take(&mut self.event);
        if data.is_empty() {
            return Ok(());
        }
        ensure!(!self.done, "流式结束后收到额外事件");
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let chunk: Value = serde_json::from_str(&data).context("无效的流式响应")?;
        if !chunk["error"].is_null() {
            bail!("模型流返回错误");
        }
        if chunk["promptFeedback"]
            .get("blockReason")
            .and_then(Value::as_str)
            .is_some()
        {
            bail!("模型拒绝生成此内容");
        }
        self.saw_chunk = true;
        if let Some(meta) = chunk.get("usageMetadata").and_then(Value::as_object)
            && (meta.contains_key("promptTokenCount")
                || meta.contains_key("candidatesTokenCount")
                || meta.contains_key("totalTokenCount"))
        {
            let prompt = meta
                .get("promptTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let completion = meta
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let sum = prompt.saturating_add(completion);
            let total = meta
                .get("totalTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(sum)
                .max(sum);
            self.usage = json!({
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": total
            });
        }
        if let Some(candidate) = chunk
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
        {
            if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
                self.finish_reason = Some(reason.to_owned());
            }
            if let Some(parts) = candidate["content"].get("parts").and_then(Value::as_array) {
                for part in parts {
                    let part_sig = part
                        .get("thoughtSignature")
                        .or_else(|| part.get("thought_signature"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        let t = part.get("text").and_then(Value::as_str).unwrap_or("");
                        if !t.is_empty() || part_sig.is_some() {
                            if !t.is_empty() {
                                self.reasoning.push_str(t);
                                emit_reasoning(t)?;
                            }
                            if let Some(ContentBlock::Thinking {
                                text, signature, ..
                            }) = self.blocks.last_mut()
                                && signature.is_none()
                            {
                                text.push_str(t);
                                if part_sig.is_some() {
                                    *signature = part_sig.clone();
                                }
                            } else {
                                self.blocks.push(ContentBlock::Thinking {
                                    text: t.to_owned(),
                                    signature: part_sig.clone(),
                                    encrypted_content: None,
                                });
                            }
                        }
                    } else if let Some(t) = part.get("text").and_then(Value::as_str)
                        && (!t.is_empty() || part_sig.is_some())
                    {
                        if !t.is_empty() {
                            self.text.push_str(t);
                            emit(t)?;
                        }
                        if let Some(ContentBlock::Text { text, signature }) = self.blocks.last_mut()
                            && signature.is_none()
                        {
                            text.push_str(t);
                            if part_sig.is_some() {
                                *signature = part_sig.clone();
                            }
                        } else {
                            self.blocks.push(ContentBlock::Text {
                                text: t.to_owned(),
                                signature: part_sig.clone(),
                            });
                        }
                    }
                    if let Some(inline) = part.get("inlineData").or_else(|| part.get("inline_data"))
                        && let Some(data) = inline.get("data").and_then(Value::as_str)
                        && let Some(mime_type) = inline
                            .get("mimeType")
                            .or_else(|| inline.get("mime_type"))
                            .and_then(Value::as_str)
                    {
                        self.blocks.push(ContentBlock::Image {
                            source: ImageSource::Base64 {
                                mime_type: mime_type.to_owned(),
                                data: data.to_owned(),
                            },
                        });
                    }
                    if let Some(fc) = part.get("functionCall") {
                        ensure!(
                            self.tool_count < MAX_TOOL_CALLS_PER_TURN,
                            "单轮工具调用过多"
                        );
                        self.tool_count += 1;
                        let name = fc
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .context("不完整的工具调用")?
                            .to_owned();
                        let args_val = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                        ensure!(args_val.is_object(), "工具参数必须是对象");
                        let id = fc
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("gemini-call-{}", uuid::Uuid::new_v4()));
                        let sig = part_sig.or_else(|| {
                            fc.get("thoughtSignature")
                                .or_else(|| fc.get("thought_signature"))
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                        });
                        let tc = ToolCallBlock {
                            id,
                            name,
                            arguments: args_val.to_string(),
                            thought_signature: sig,
                            namespace: None,
                        };
                        self.blocks.push(ContentBlock::ToolCall(tc));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn finish_neutral(
        self,
        emit: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<NeutralMessage> {
        ensure!(
            self.buffer.is_empty() && self.event.is_empty(),
            "流式连接缺少完整结束事件"
        );
        ensure!(
            self.saw_chunk && (self.finish_reason.is_some() || self.done),
            "流式连接提前中断，请继续此成员重试"
        );
        match self.finish_reason.as_deref() {
            Some("MAX_TOKENS") => bail!("模型输出达到上限，请提高路由的输出上限后重试"),
            Some("SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII") => {
                bail!("模型拒绝生成此内容")
            }
            Some("MALFORMED_FUNCTION_CALL") => bail!("不完整的工具调用"),
            _ => {}
        }
        let mut blocks = self.blocks;
        let has_image = blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Image { .. }));
        if self.tool_count == 0 && self.text.trim().is_empty() && !has_image {
            ensure!(!self.reasoning.trim().is_empty(), "模型没有返回内容");
            emit(&self.reasoning)?;
            blocks.push(ContentBlock::Text {
                text: self.reasoning,
                signature: None,
            });
        }
        Ok(NeutralMessage::assistant(blocks))
    }

    pub fn finish(self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<Value> {
        Ok(self.finish_neutral(emit)?.to_legacy_value())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResponsesUsageDetails {
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Clone, Debug, Default)]
struct ResponsesReasoningItem {
    id: String,
    summary: String,
    encrypted_content: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ResponsesToolParts {
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
pub struct ResponsesDecoder {
    buffer: Vec<u8>,
    event_name: String,
    event_data: String,
    done: bool,
    terminal_status: Option<String>,
    incomplete_reason: Option<String>,
    refusal: Option<String>,
    pub response_id: Option<String>,
    pub text: String,
    pub reasoning: String,
    item_id_to_index: HashMap<String, usize>,
    reasoning_items: BTreeMap<usize, ResponsesReasoningItem>,
    tools: BTreeMap<usize, ResponsesToolParts>,
    pub usage: Value,
    pub usage_details: Option<ResponsesUsageDetails>,
    allow_non_object_tool_args: bool,
    allow_empty_text: bool,
}

impl ResponsesDecoder {
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        self.feed_with_reasoning(bytes, emit, &mut |_| Ok(()))
    }

    pub fn feed_with_reasoning(
        &mut self,
        bytes: &[u8],
        emit: &mut impl FnMut(&str) -> Result<()>,
        emit_reasoning: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        self.buffer.extend_from_slice(bytes);
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(emit, emit_reasoning)?;
            } else if line.starts_with(':') {
                continue;
            } else if let Some(ev) = line.strip_prefix("event:") {
                self.event_name = ev.trim().to_owned();
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.event_data.is_empty() {
                    self.event_data.push('\n');
                }
                self.event_data.push_str(data.trim_start());
            }
        }
        Ok(())
    }

    fn resolve_output_index(&self, chunk: &Value) -> usize {
        if let Some(idx) = chunk.get("output_index").and_then(Value::as_u64) {
            return idx as usize;
        }
        if let Some(item_id) = chunk.get("item_id").and_then(Value::as_str)
            && let Some(&idx) = self.item_id_to_index.get(item_id)
        {
            return idx;
        }
        0
    }

    fn capture_usage(&mut self, usage: &Value) {
        let Some(u) = usage.as_object() else {
            return;
        };
        let prompt = u
            .get("input_tokens")
            .or_else(|| u.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion = u
            .get("output_tokens")
            .or_else(|| u.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let sum = prompt.saturating_add(completion);
        let total = u
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(sum)
            .max(sum);
        self.usage = json!({
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": total
        });
        let cached_tokens = usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let reasoning_tokens = usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.usage_details = Some(ResponsesUsageDetails {
            cached_tokens,
            reasoning_tokens,
        });
    }

    fn flush(
        &mut self,
        emit: &mut impl FnMut(&str) -> Result<()>,
        emit_reasoning: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<()> {
        let event_name = std::mem::take(&mut self.event_name);
        let data = std::mem::take(&mut self.event_data);
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            ensure!(self.terminal_status.is_some(), "流式响应缺少完成事件");
            self.done = true;
            return Ok(());
        }
        ensure!(!self.done, "流式结束后收到额外事件");
        let chunk: Value = serde_json::from_str(&data).context("无效的流式响应")?;
        let kind = chunk
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(event_name.as_str());
        if kind == "error" || kind == "response.failed" || !chunk["error"].is_null() {
            bail!("模型流返回错误");
        }
        match kind {
            "response.created" | "response.in_progress" => {
                if let Some(id) = chunk
                    .pointer("/response/id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    self.response_id = Some(id.to_owned());
                }
            }
            "response.output_item.added" => {
                let output_index = self.resolve_output_index(&chunk);
                let item = &chunk["item"];
                if let Some(item_id) = item
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    self.item_id_to_index
                        .insert(item_id.to_owned(), output_index);
                }
                match item.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        let entry = self.reasoning_items.entry(output_index).or_default();
                        if let Some(id) = item.get("id").and_then(Value::as_str) {
                            entry.id = id.to_owned();
                        }
                        if let Some(enc) = item
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            entry.encrypted_content = Some(enc.to_owned());
                        }
                        if let Some(summary_arr) = item.get("summary").and_then(Value::as_array) {
                            for part in summary_arr {
                                if let Some(t) = part.get("text").and_then(Value::as_str)
                                    && !t.is_empty()
                                {
                                    entry.summary.push_str(t);
                                    self.reasoning.push_str(t);
                                    emit_reasoning(t)?;
                                }
                            }
                        }
                    }
                    Some("function_call") => {
                        ensure!(
                            self.tools.contains_key(&output_index)
                                || self.tools.len() < MAX_TOOL_CALLS_PER_TURN,
                            "单轮工具调用过多"
                        );
                        let part = self.tools.entry(output_index).or_default();
                        if let Some(id) = item.get("id").and_then(Value::as_str) {
                            part.item_id = id.to_owned();
                        }
                        if let Some(cid) = item.get("call_id").and_then(Value::as_str) {
                            part.call_id = cid.to_owned();
                        }
                        if let Some(name) = item.get("name").and_then(Value::as_str) {
                            part.name = name.to_owned();
                        }
                        if let Some(args) = item.get("arguments").and_then(Value::as_str) {
                            part.arguments = args.to_owned();
                        }
                    }
                    _ => {}
                }
            }
            "response.content_part.added" => {
                let part = &chunk["part"];
                match part.get("type").and_then(Value::as_str) {
                    Some("output_text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str)
                            && !text.is_empty()
                        {
                            self.text.push_str(text);
                            emit(text)?;
                        }
                    }
                    Some("refusal") => {
                        let r = part
                            .get("refusal")
                            .and_then(Value::as_str)
                            .unwrap_or("refusal")
                            .to_owned();
                        self.refusal = Some(r);
                    }
                    _ => {}
                }
            }
            "response.output_text.delta" => {
                if let Some(delta) = chunk.get("delta").and_then(Value::as_str)
                    && !delta.is_empty()
                {
                    self.text.push_str(delta);
                    emit(delta)?;
                }
            }
            "response.output_text.done" => {
                if self.text.is_empty()
                    && let Some(text) = chunk.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    self.text.push_str(text);
                    emit(text)?;
                }
            }
            "response.reasoning_summary_part.added" | "response.reasoning_summary_text.delta" => {
                let output_index = self.resolve_output_index(&chunk);
                let delta = chunk
                    .get("delta")
                    .and_then(Value::as_str)
                    .or_else(|| chunk.pointer("/part/text").and_then(Value::as_str));
                if let Some(text) = delta
                    && !text.is_empty()
                {
                    let entry = self.reasoning_items.entry(output_index).or_default();
                    entry.summary.push_str(text);
                    self.reasoning.push_str(text);
                    emit_reasoning(text)?;
                }
            }
            "response.reasoning_summary_text.done" => {
                let output_index = self.resolve_output_index(&chunk);
                if let Some(text) = chunk.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    let entry = self.reasoning_items.entry(output_index).or_default();
                    if entry.summary.is_empty() {
                        entry.summary.push_str(text);
                        self.reasoning.push_str(text);
                        emit_reasoning(text)?;
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let output_index = self.resolve_output_index(&chunk);
                ensure!(
                    self.tools.contains_key(&output_index)
                        || self.tools.len() < MAX_TOOL_CALLS_PER_TURN,
                    "单轮工具调用过多"
                );
                if let Some(delta) = chunk.get("delta").and_then(Value::as_str) {
                    self.tools
                        .entry(output_index)
                        .or_default()
                        .arguments
                        .push_str(delta);
                }
            }
            "response.function_call_arguments.done" => {
                let output_index = self.resolve_output_index(&chunk);
                if let Some(args) = chunk
                    .get("arguments")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    ensure!(
                        self.tools.contains_key(&output_index)
                            || self.tools.len() < MAX_TOOL_CALLS_PER_TURN,
                        "单轮工具调用过多"
                    );
                    self.tools.entry(output_index).or_default().arguments = args.to_owned();
                }
            }
            "response.output_item.done" => {
                let output_index = self.resolve_output_index(&chunk);
                let item = &chunk["item"];
                match item.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        let entry = self.reasoning_items.entry(output_index).or_default();
                        if let Some(id) = item
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            entry.id = id.to_owned();
                        }
                        if let Some(enc) = item
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            entry.encrypted_content = Some(enc.to_owned());
                        }
                        if entry.summary.is_empty()
                            && let Some(summary_arr) = item.get("summary").and_then(Value::as_array)
                        {
                            let mut combined = String::new();
                            for part in summary_arr {
                                if let Some(t) = part.get("text").and_then(Value::as_str) {
                                    combined.push_str(t);
                                }
                            }
                            if !combined.is_empty() {
                                entry.summary = combined.clone();
                                self.reasoning.push_str(&combined);
                                emit_reasoning(&combined)?;
                            }
                        }
                    }
                    Some("function_call") => {
                        ensure!(
                            self.tools.contains_key(&output_index)
                                || self.tools.len() < MAX_TOOL_CALLS_PER_TURN,
                            "单轮工具调用过多"
                        );
                        let part = self.tools.entry(output_index).or_default();
                        if let Some(id) = item
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            part.item_id = id.to_owned();
                        }
                        if let Some(cid) = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            part.call_id = cid.to_owned();
                        }
                        if let Some(name) = item
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            part.name = name.to_owned();
                        }
                        if let Some(args) = item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            part.arguments = args.to_owned();
                        }
                    }
                    Some("message") => {
                        if let Some(parts) = item.get("content").and_then(Value::as_array) {
                            for p in parts {
                                match p.get("type").and_then(Value::as_str) {
                                    Some("refusal") => {
                                        let r = p
                                            .get("refusal")
                                            .and_then(Value::as_str)
                                            .unwrap_or("refusal")
                                            .to_owned();
                                        self.refusal = Some(r);
                                    }
                                    Some("output_text") => {
                                        if self.text.is_empty()
                                            && let Some(t) = p.get("text").and_then(Value::as_str)
                                            && !t.is_empty()
                                        {
                                            self.text.push_str(t);
                                            emit(t)?;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            "response.completed" => {
                self.done = true;
                self.terminal_status = Some("completed".into());
                let resp = &chunk["response"];
                if let Some(id) = resp
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    self.response_id = Some(id.to_owned());
                }
                if let Some(output) = resp.get("output").and_then(Value::as_array) {
                    for (idx, item) in output.iter().enumerate() {
                        if item.get("type").and_then(Value::as_str) == Some("reasoning")
                            && let Some(enc) = item
                                .get("encrypted_content")
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                        {
                            let entry = self.reasoning_items.entry(idx).or_default();
                            if entry.encrypted_content.is_none() {
                                entry.encrypted_content = Some(enc.to_owned());
                            }
                            if entry.id.is_empty()
                                && let Some(id) = item.get("id").and_then(Value::as_str)
                            {
                                entry.id = id.to_owned();
                            }
                        }
                    }
                }
                self.capture_usage(&resp["usage"]);
            }
            "response.incomplete" => {
                self.done = true;
                self.terminal_status = Some("incomplete".into());
                let resp = &chunk["response"];
                self.incomplete_reason = resp
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.capture_usage(&resp["usage"]);
            }
            _ => {}
        }
        Ok(())
    }

    pub fn finish_neutral(
        self,
        emit: &mut impl FnMut(&str) -> Result<()>,
    ) -> Result<NeutralMessage> {
        ensure!(
            self.buffer.is_empty() && self.event_data.is_empty(),
            "流式连接缺少完整结束事件"
        );
        ensure!(self.done, "流式连接提前中断，请继续此成员重试");
        if matches!(
            self.incomplete_reason.as_deref(),
            Some("max_output_tokens" | "max_tokens")
        ) {
            bail!("模型输出达到上限，请提高路由的输出上限后重试");
        }
        if self.incomplete_reason.as_deref() == Some("content_filter") || self.refusal.is_some() {
            bail!("模型拒绝生成此内容");
        }

        let mut blocks = Vec::new();
        for item in self.reasoning_items.into_values() {
            if !item.summary.is_empty() || item.encrypted_content.is_some() {
                blocks.push(ContentBlock::Thinking {
                    text: item.summary,
                    signature: None,
                    encrypted_content: item.encrypted_content,
                });
            }
        }

        let has_tools = !self.tools.is_empty();
        let content = if !has_tools && self.text.trim().is_empty() {
            if !self.allow_empty_text {
                ensure!(!self.reasoning.trim().is_empty(), "模型没有返回内容");
            }
            if !self.reasoning.trim().is_empty() {
                emit(&self.reasoning)?;
                self.reasoning
            } else {
                self.text
            }
        } else {
            self.text
        };
        if !content.is_empty() || !has_tools {
            blocks.push(ContentBlock::Text {
                text: content,
                signature: None,
            });
        }
        for part in self.tools.into_values() {
            let id = if !part.call_id.is_empty() {
                part.call_id
            } else {
                part.item_id
            };
            if self.allow_non_object_tool_args {
                ensure!(!part.name.is_empty(), "不完整的工具调用");
            } else {
                ensure!(!id.is_empty() && !part.name.is_empty(), "不完整的工具调用");
                let parsed: Value =
                    serde_json::from_str(&part.arguments).context("工具参数不完整")?;
                ensure!(parsed.is_object(), "工具参数必须是对象");
            }
            blocks.push(ContentBlock::ToolCall(ToolCallBlock {
                id,
                name: part.name,
                arguments: part.arguments,
                thought_signature: None,
                namespace: None,
            }));
        }
        Ok(NeutralMessage::assistant(blocks))
    }

    pub fn finish(self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<Value> {
        Ok(self.finish_neutral(emit)?.to_legacy_value())
    }
}

pub(crate) fn protocol_decoder(
    protocol: ProviderProtocol,
) -> Box<dyn crate::provider_runtime::CompletionDecoder> {
    match protocol {
        ProviderProtocol::OpenAi => Box::new(Decoder::default()),
        ProviderProtocol::OpenAiResponses => Box::new(ResponsesDecoder::default()),
        ProviderProtocol::Anthropic => Box::new(AnthropicDecoder::default()),
        ProviderProtocol::Gemini => Box::new(GeminiDecoder::default()),
    }
}

pub(crate) struct BuiltinCompletionCodec;

impl crate::provider_runtime::CompletionCodec for BuiltinCompletionCodec {
    fn encode(
        &self,
        input: &crate::provider_runtime::CompletionInput<'_>,
    ) -> std::result::Result<Value, crate::provider_runtime::CodecError> {
        Ok(crate::provider_runtime::expected_body(input))
    }

    fn encode_protocol(
        &self,
        protocol: ProviderProtocol,
        body: &Value,
    ) -> std::result::Result<Value, crate::provider_runtime::CodecError> {
        encode_protocol_body(protocol, body)
            .map_err(|_| crate::provider_runtime::CodecError::Invalid)
    }

    fn encode_neutral_protocol(
        &self,
        protocol: ProviderProtocol,
        input: &crate::provider_runtime::CompletionInput<'_>,
    ) -> std::result::Result<Value, crate::provider_runtime::CodecError> {
        encode_neutral_protocol_body(protocol, input)
            .map_err(|_| crate::provider_runtime::CodecError::Invalid)
    }

    fn encode_neutral_protocol_with_thinking(
        &self,
        protocol: ProviderProtocol,
        input: &crate::provider_runtime::CompletionInput<'_>,
        thinking_level: Option<&str>,
    ) -> std::result::Result<Value, crate::provider_runtime::CodecError> {
        encode_neutral_protocol_body_with_thinking(protocol, input, thinking_level)
            .map_err(|_| crate::provider_runtime::CodecError::Invalid)
    }

    fn decoder(
        &self,
    ) -> std::result::Result<
        Box<dyn crate::provider_runtime::CompletionDecoder>,
        crate::provider_runtime::CodecError,
    > {
        Ok(Box::new(Decoder::default()))
    }

    fn decoder_for(
        &self,
        protocol: ProviderProtocol,
    ) -> std::result::Result<
        Box<dyn crate::provider_runtime::CompletionDecoder>,
        crate::provider_runtime::CodecError,
    > {
        Ok(protocol_decoder(protocol))
    }
}

#[cfg(test)]
mod thinking_level_tests {
    use super::*;
    use crate::provider_runtime::CompletionInput;

    #[test]
    fn thinking_level_forwarded_for_openai_anthropic_and_responses() {
        let msgs = [NeutralMessage::user_text("hi")];
        let input = CompletionInput {
            model: "test-model",
            max_tokens: 4096,
            messages: &msgs,
            tools: &[],
        };

        // OpenAI Completions
        let oa_none =
            encode_neutral_protocol_body_with_thinking(ProviderProtocol::OpenAi, &input, None)
                .unwrap();
        assert!(oa_none.get("reasoning_effort").is_none());
        let oa_off = encode_neutral_protocol_body_with_thinking(
            ProviderProtocol::OpenAi,
            &input,
            Some("off"),
        )
        .unwrap();
        assert!(oa_off.get("reasoning_effort").is_none());
        let oa_med = encode_neutral_protocol_body_with_thinking(
            ProviderProtocol::OpenAi,
            &input,
            Some("medium"),
        )
        .unwrap();
        assert_eq!(oa_med["reasoning_effort"], "medium");

        // Anthropic Messages (budget bounded < max_tokens)
        let ant_none =
            encode_neutral_protocol_body_with_thinking(ProviderProtocol::Anthropic, &input, None)
                .unwrap();
        assert!(ant_none.get("thinking").is_none());
        let ant_off = encode_neutral_protocol_body_with_thinking(
            ProviderProtocol::Anthropic,
            &input,
            Some("off"),
        )
        .unwrap();
        assert!(ant_off.get("thinking").is_none());
        let ant_low = encode_neutral_protocol_body_with_thinking(
            ProviderProtocol::Anthropic,
            &input,
            Some("low"),
        )
        .unwrap();
        assert_eq!(ant_low["thinking"]["type"], "enabled");
        assert_eq!(ant_low["thinking"]["budget_tokens"], 2048);
        assert!(ant_low["thinking"]["budget_tokens"].as_u64().unwrap() < 4096);

        let ant_high = encode_neutral_protocol_body_with_thinking(
            ProviderProtocol::Anthropic,
            &input,
            Some("high"),
        )
        .unwrap();
        assert_eq!(ant_high["thinking"]["type"], "enabled");
        assert_eq!(ant_high["thinking"]["budget_tokens"], 4095);
        assert!(ant_high["thinking"]["budget_tokens"].as_u64().unwrap() < 4096);

        // OpenAI Responses
        let resp_high = encode_neutral_protocol_body_with_thinking(
            ProviderProtocol::OpenAiResponses,
            &input,
            Some("high"),
        )
        .unwrap();
        assert_eq!(resp_high["reasoning"]["effort"], "high");
    }
}

fn usage_breakdown_from_value(
    usage: &Value,
    cache_read: u64,
    cache_write: u64,
    reasoning: Option<u64>,
) -> Option<crate::runtime_seams::KernelUsageBreakdown> {
    let obj = usage.as_object()?;
    let prompt = obj.get("prompt_tokens").and_then(Value::as_u64)?;
    let output = obj.get("completion_tokens").and_then(Value::as_u64)?;
    let total = obj
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| prompt.saturating_add(output));
    Some(crate::runtime_seams::KernelUsageBreakdown {
        input: prompt
            .saturating_sub(cache_read)
            .saturating_sub(cache_write),
        output,
        cache_read,
        cache_write,
        reasoning: reasoning.filter(|v| *v > 0),
        total_tokens: Some(total),
    })
}

impl crate::provider_runtime::CompletionDecoder for Decoder {
    fn set_allow_non_object_tool_args(&mut self, allow: bool) {
        self.allow_non_object_tool_args = allow;
    }

    fn set_allow_empty_text(&mut self, allow: bool) {
        self.allow_empty_text = allow;
    }

    fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    fn feed(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        Decoder::feed(self, bytes, &mut |text| emit(text))
            .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }

    fn feed_with_reasoning(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
        emit_reasoning: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        Decoder::feed_with_reasoning(self, bytes, &mut |text| emit(text), &mut |reasoning| {
            emit_reasoning(reasoning)
        })
        .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn finish(
        self: Box<Self>,
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<
        crate::provider_runtime::CompletionOutput,
        crate::provider_runtime::CodecError,
    > {
        let usage = self.usage.clone();
        let interrupted_after_progress = self.buffer.is_empty()
            && self.event.is_empty()
            && !(self.done && self.finish.is_some())
            && (!self.text.is_empty()
                || !self.reasoning.is_empty()
                || !self.tools.is_empty()
                || self.finish.is_some());
        Decoder::finish_neutral(*self, &mut |text| emit(text))
            .map(|message| (message, usage))
            .map_err(|_| {
                if interrupted_after_progress {
                    crate::provider_runtime::CodecError::Failed
                } else {
                    crate::provider_runtime::CodecError::Invalid
                }
            })
    }

    fn finish_with_breakdown(
        self: Box<Self>,
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<
        (
            crate::provider_runtime::CompletionOutput,
            Option<crate::runtime_seams::KernelUsageBreakdown>,
        ),
        crate::provider_runtime::CodecError,
    > {
        let usage = self.usage.clone();
        let breakdown = usage_breakdown_from_value(
            &usage,
            self.cache_read_tokens.unwrap_or(0),
            0,
            self.reasoning_tokens,
        );
        let interrupted_after_progress = self.buffer.is_empty()
            && self.event.is_empty()
            && !(self.done && self.finish.is_some())
            && (!self.text.is_empty()
                || !self.reasoning.is_empty()
                || !self.tools.is_empty()
                || self.finish.is_some());
        let msg = Decoder::finish_neutral(*self, &mut |text| emit(text)).map_err(|_| {
            if interrupted_after_progress {
                crate::provider_runtime::CodecError::Failed
            } else {
                crate::provider_runtime::CodecError::Invalid
            }
        })?;
        Ok(((msg, usage), breakdown))
    }
}

impl crate::provider_runtime::CompletionDecoder for AnthropicDecoder {
    fn set_allow_non_object_tool_args(&mut self, allow: bool) {
        self.allow_non_object_tool_args = allow;
    }

    fn set_allow_empty_text(&mut self, allow: bool) {
        self.allow_empty_text = allow;
    }

    fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    fn feed(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        AnthropicDecoder::feed(self, bytes, &mut |text| emit(text))
            .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }

    fn feed_with_reasoning(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
        emit_reasoning: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        AnthropicDecoder::feed_with_reasoning(
            self,
            bytes,
            &mut |text| emit(text),
            &mut |reasoning| emit_reasoning(reasoning),
        )
        .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn finish(
        self: Box<Self>,
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<
        crate::provider_runtime::CompletionOutput,
        crate::provider_runtime::CodecError,
    > {
        let usage = self.usage.clone();
        let interrupted_after_progress = self.buffer.is_empty()
            && self.event_data.is_empty()
            && !self.done
            && (self.response_id.is_some()
                || !self.blocks.is_empty()
                || !self.text.is_empty()
                || !self.reasoning.is_empty());
        AnthropicDecoder::finish_neutral(*self, &mut |text| emit(text))
            .map(|message| (message, usage))
            .map_err(|_| {
                if interrupted_after_progress {
                    crate::provider_runtime::CodecError::Failed
                } else {
                    crate::provider_runtime::CodecError::Invalid
                }
            })
    }

    fn finish_with_breakdown(
        self: Box<Self>,
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<
        (
            crate::provider_runtime::CompletionOutput,
            Option<crate::runtime_seams::KernelUsageBreakdown>,
        ),
        crate::provider_runtime::CodecError,
    > {
        let usage = self.usage.clone();
        let breakdown = if self.raw_input_tokens.is_some() || self.output_tokens.is_some() {
            let input = self.raw_input_tokens.unwrap_or(0);
            let output = self.output_tokens.unwrap_or(0);
            let cache_read = self.cache_read_tokens.unwrap_or(0);
            let cache_write = self.cache_write_tokens.unwrap_or(0);
            let total = input
                .saturating_add(output)
                .saturating_add(cache_read)
                .saturating_add(cache_write);
            Some(crate::runtime_seams::KernelUsageBreakdown {
                input,
                output,
                cache_read,
                cache_write,
                reasoning: None,
                total_tokens: Some(total),
            })
        } else {
            None
        };
        let interrupted_after_progress = self.buffer.is_empty()
            && self.event_data.is_empty()
            && !self.done
            && (self.response_id.is_some()
                || !self.blocks.is_empty()
                || !self.text.is_empty()
                || !self.reasoning.is_empty());
        let msg =
            AnthropicDecoder::finish_neutral(*self, &mut |text| emit(text)).map_err(|_| {
                if interrupted_after_progress {
                    crate::provider_runtime::CodecError::Failed
                } else {
                    crate::provider_runtime::CodecError::Invalid
                }
            })?;
        Ok(((msg, usage), breakdown))
    }
}

impl crate::provider_runtime::CompletionDecoder for GeminiDecoder {
    fn feed(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        GeminiDecoder::feed(self, bytes, &mut |text| emit(text))
            .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }

    fn feed_with_reasoning(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
        emit_reasoning: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        GeminiDecoder::feed_with_reasoning(self, bytes, &mut |text| emit(text), &mut |reasoning| {
            emit_reasoning(reasoning)
        })
        .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn finish(
        self: Box<Self>,
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<
        crate::provider_runtime::CompletionOutput,
        crate::provider_runtime::CodecError,
    > {
        let usage = self.usage.clone();
        let interrupted_after_progress = self.buffer.is_empty()
            && self.event.is_empty()
            && self.saw_chunk
            && self.finish_reason.is_none()
            && !self.done;
        GeminiDecoder::finish_neutral(*self, &mut |text| emit(text))
            .map(|message| (message, usage))
            .map_err(|_| {
                if interrupted_after_progress {
                    crate::provider_runtime::CodecError::Failed
                } else {
                    crate::provider_runtime::CodecError::Invalid
                }
            })
    }

    fn finish_with_breakdown(
        self: Box<Self>,
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<
        (
            crate::provider_runtime::CompletionOutput,
            Option<crate::runtime_seams::KernelUsageBreakdown>,
        ),
        crate::provider_runtime::CodecError,
    > {
        let usage = self.usage.clone();
        let breakdown = usage_breakdown_from_value(&usage, 0, 0, None);
        let interrupted_after_progress = self.buffer.is_empty()
            && self.event.is_empty()
            && self.saw_chunk
            && self.finish_reason.is_none()
            && !self.done;
        let msg = GeminiDecoder::finish_neutral(*self, &mut |text| emit(text)).map_err(|_| {
            if interrupted_after_progress {
                crate::provider_runtime::CodecError::Failed
            } else {
                crate::provider_runtime::CodecError::Invalid
            }
        })?;
        Ok(((msg, usage), breakdown))
    }
}

impl crate::provider_runtime::CompletionDecoder for ResponsesDecoder {
    fn set_allow_non_object_tool_args(&mut self, allow: bool) {
        self.allow_non_object_tool_args = allow;
    }

    fn set_allow_empty_text(&mut self, allow: bool) {
        self.allow_empty_text = allow;
    }

    fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    fn feed(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        ResponsesDecoder::feed(self, bytes, &mut |text| emit(text))
            .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }

    fn feed_with_reasoning(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
        emit_reasoning: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        ResponsesDecoder::feed_with_reasoning(
            self,
            bytes,
            &mut |text| emit(text),
            &mut |reasoning| emit_reasoning(reasoning),
        )
        .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn finish(
        self: Box<Self>,
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<
        crate::provider_runtime::CompletionOutput,
        crate::provider_runtime::CodecError,
    > {
        let usage = self.usage.clone();
        let interrupted_after_progress = self.buffer.is_empty()
            && self.event_data.is_empty()
            && !self.done
            && (self.response_id.is_some()
                || !self.text.is_empty()
                || !self.reasoning.is_empty()
                || !self.tools.is_empty()
                || !self.reasoning_items.is_empty());
        ResponsesDecoder::finish_neutral(*self, &mut |text| emit(text))
            .map(|message| (message, usage))
            .map_err(|_| {
                if interrupted_after_progress {
                    crate::provider_runtime::CodecError::Failed
                } else {
                    crate::provider_runtime::CodecError::Invalid
                }
            })
    }

    fn finish_with_breakdown(
        self: Box<Self>,
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<
        (
            crate::provider_runtime::CompletionOutput,
            Option<crate::runtime_seams::KernelUsageBreakdown>,
        ),
        crate::provider_runtime::CodecError,
    > {
        let usage = self.usage.clone();
        let (cached, reasoning) = self
            .usage_details
            .as_ref()
            .map(|d| (d.cached_tokens, Some(d.reasoning_tokens)))
            .unwrap_or((0, None));
        let breakdown = usage_breakdown_from_value(&usage, cached, 0, reasoning);
        let interrupted_after_progress = self.buffer.is_empty()
            && self.event_data.is_empty()
            && !self.done
            && (self.response_id.is_some()
                || !self.text.is_empty()
                || !self.reasoning.is_empty()
                || !self.tools.is_empty()
                || !self.reasoning_items.is_empty());
        let msg =
            ResponsesDecoder::finish_neutral(*self, &mut |text| emit(text)).map_err(|_| {
                if interrupted_after_progress {
                    crate::provider_runtime::CodecError::Failed
                } else {
                    crate::provider_runtime::CodecError::Invalid
                }
            })?;
        Ok(((msg, usage), breakdown))
    }
}

pub async fn complete_neutral(
    client: &ProviderClient,
    permit: ProviderPermit,
    route: &Route,
    messages: &[NeutralMessage],
    tools: &[Value],
    mut emit: impl FnMut(&str) -> Result<()> + Send,
) -> std::result::Result<(NeutralMessage, Value), TransportError> {
    let protocol = permit.protocol();
    let scrub = permit.credential_scrubber();
    permit.matches_with_protocol(
        &route.base_url,
        NetworkPurpose::Completion,
        Some(&route.model),
        None,
        route.protocol,
    )?;
    let input = crate::provider_runtime::CompletionInput {
        model: permit.model().unwrap_or(&route.model),
        max_tokens: route.max_tokens,
        messages,
        tools,
    };
    let body =
        encode_neutral_protocol_body(protocol, &input).map_err(|_| TransportError::Invalid)?;
    let response = client.send(permit, Some(&body)).await?;
    if !response.status().is_success() {
        return Err(read_provider_error(response, &scrub).await);
    }
    let mut decoder = protocol_decoder(protocol);
    let mut bytes = response.bytes_stream();
    let emit_ref: &mut crate::provider_runtime::Emit<'_> = &mut emit;
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk.map_err(|_| TransportError::Unknown)?;
        if !decoder.is_done() {
            decoder
                .feed(&chunk, emit_ref)
                .map_err(|_| TransportError::Unknown)?;
        }
    }
    decoder
        .finish(emit_ref)
        .map_err(|_| TransportError::Unknown)
}

pub async fn complete(
    client: &ProviderClient,
    permit: ProviderPermit,
    route: &Route,
    messages: &[Value],
    tools: &[Value],
    emit: impl FnMut(&str) -> Result<()> + Send,
) -> std::result::Result<(Value, Value), TransportError> {
    let neutral = from_legacy_values(messages).map_err(|_| TransportError::Invalid)?;
    let (message, usage) = complete_neutral(client, permit, route, &neutral, tools, emit).await?;
    Ok((message.to_legacy_value(), usage))
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .filter(|n| n.is_finite())
}

pub fn normalize_balance(value: Value, account: &Account) -> Result<Value> {
    ensure!(value["success"] != false, "New API 账号验证失败");
    let data = value.get("data").unwrap_or(&value);
    let quota = number(&data["quota"]).context("余额响应缺少有效 quota")?;
    let used = number(&data["used_quota"]);
    Ok(json!({
        "username": data["username"],
        "display_name": data["display_name"],
        "group": data["group"],
        "quota_raw": quota,
        "balance": quota / account.quota_per_unit,
        "used": used.map(|v| v / account.quota_per_unit),
        "quota_per_unit": account.quota_per_unit,
        "checked_at": now()
    }))
}

pub async fn balance(
    client: &ProviderClient,
    permit: ProviderPermit,
    account: &Account,
) -> std::result::Result<Value, TransportError> {
    permit.matches(
        &account.base_url,
        NetworkPurpose::Balance,
        None,
        Some(&account.user_id),
    )?;
    let response = client.send(permit, None).await?;
    let (value, status) = json_response(response).await?;
    normalize_balance(value, account).map_err(|_| TransportError::InvalidResponse { status })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_sse_fragmented_tools_and_unredacted_code() {
        let code = "let cancel_token = 1; // sk-learn Bearer auth";
        let c1 = json!({"choices":[{"delta":{"content":code}}]});
        let c2 = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]});
        let c3 = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"桃子.txt\"}"}}]},"finish_reason":"tool_calls"}]});
        let data = format!("data: {c1}\r\n\r\ndata: {c2}\n\ndata: {c3}\n\ndata: [DONE]\n\n");
        let mut decoder = Decoder::default();
        let mut text = String::new();
        for byte in data.as_bytes() {
            decoder
                .feed(&[*byte], &mut |s| {
                    text.push_str(s);
                    Ok(())
                })
                .unwrap();
        }
        let message = decoder.finish(&mut |_| Ok(())).unwrap();
        assert_eq!(text, code);
        assert_eq!(message["content"], code);
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"桃子.txt\"}"
        );
    }

    #[test]
    fn reasoning_content_delta_with_answer_and_reasoning_only_chunks() {
        let with_answer = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"17 * 19 = 323\",\"content\":null}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"323\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":25}}\n\n",
            "data: [DONE]\n\n"
        );
        let mut decoder = Decoder::default();
        let mut streamed = String::new();
        decoder
            .feed(with_answer.as_bytes(), &mut |s| {
                streamed.push_str(s);
                Ok(())
            })
            .unwrap();
        let usage = decoder.usage.clone();
        let message = decoder.finish(&mut |_| Ok(())).unwrap();
        assert_eq!(streamed, "323");
        assert_eq!(message["content"], "323");
        assert_eq!(message["reasoning_content"], "17 * 19 = 323");
        assert_eq!(usage["total_tokens"], 25);

        // Chunks carrying only `reasoning` and null `content` still succeed cleanly.
        let reasoning_only = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"step 1; \",\"content\":null}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"step 2\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut decoder2 = Decoder::default();
        let mut fallback = String::new();
        decoder2
            .feed(reasoning_only.as_bytes(), &mut |s| {
                fallback.push_str(s);
                Ok(())
            })
            .unwrap();
        let message2 = decoder2
            .finish(&mut |s| {
                fallback.push_str(s);
                Ok(())
            })
            .unwrap();
        assert_eq!(fallback, "step 1; step 2");
        assert_eq!(message2["content"], "step 1; step 2");
    }

    #[test]
    fn truncated_stream_and_balance_validation() {
        let mut d = Decoder::default();
        d.feed(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            &mut |_| Ok(()),
        )
        .unwrap();
        assert!(d.finish(&mut |_| Ok(())).is_err());

        let a = Account {
            base_url: "https://example.com".into(),
            user_id: "1".into(),
            quota_per_unit: 500_000.0,
        };
        assert_eq!(
            normalize_balance(
                json!({"success": true, "data": {"quota": "1000000", "used_quota": 500000}}),
                &a
            )
            .unwrap()["balance"],
            2.0
        );
        assert!(normalize_balance(json!({"success": false}), &a).is_err());
    }

    #[test]
    fn anthropic_and_gemini_decoders_handle_fragmented_utf8_tools_and_usage() {
        let ant_sse = concat!(
            "event: message_start\r\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":40,\"cache_read_input_tokens\":10,\"output_tokens\":1}}}\r\n\r\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"先检查桃子.txt\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"正在读取桃子...\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"桃子.txt\\\"}\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":25}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let mut ant = AnthropicDecoder::default();
        let mut ant_streamed = String::new();
        for b in ant_sse.as_bytes() {
            ant.feed(&[*b], &mut |s| {
                ant_streamed.push_str(s);
                Ok(())
            })
            .unwrap();
        }
        let ant_usage = ant.usage.clone();
        let ant_msg = ant.finish(&mut |_| Ok(())).unwrap();
        assert_eq!(ant_streamed, "正在读取桃子...");
        assert_eq!(ant_msg["content"], "正在读取桃子...");
        assert_eq!(ant_msg["reasoning_content"], "先检查桃子.txt");
        assert_eq!(ant_msg["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(ant_msg["tool_calls"][0]["function"]["name"], "read_file");
        assert_eq!(
            ant_msg["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"桃子.txt\"}"
        );
        assert_eq!(ant_usage["prompt_tokens"], 50);
        assert_eq!(ant_usage["completion_tokens"], 25);
        assert_eq!(ant_usage["total_tokens"], 75);

        let gem_sse = concat!(
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"thought\":true,\"text\":\"思考中\"},{\"text\":\"你好，桃子\"}]}}],\"usageMetadata\":{\"promptTokenCount\":12,\"candidatesTokenCount\":4,\"totalTokenCount\":20}}\r\n\r\n",
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"read_file\",\"args\":{\"path\":\"桃子.txt\"}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":12,\"candidatesTokenCount\":9,\"totalTokenCount\":26}}\n\n"
        );
        let mut gem = GeminiDecoder::default();
        let mut gem_streamed = String::new();
        for b in gem_sse.as_bytes() {
            gem.feed(&[*b], &mut |s| {
                gem_streamed.push_str(s);
                Ok(())
            })
            .unwrap();
        }
        let gem_usage = gem.usage.clone();
        let gem_msg = gem.finish(&mut |_| Ok(())).unwrap();
        assert_eq!(gem_streamed, "你好，桃子");
        assert_eq!(gem_msg["content"], "你好，桃子");
        assert_eq!(gem_msg["reasoning_content"], "思考中");
        assert_eq!(gem_msg["tool_calls"][0]["function"]["name"], "read_file");
        assert_eq!(
            serde_json::from_str::<Value>(
                gem_msg["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            json!({"path": "桃子.txt"})
        );
        assert_eq!(gem_usage["prompt_tokens"], 12);
        assert_eq!(gem_usage["completion_tokens"], 9);
        assert_eq!(gem_usage["total_tokens"], 26);
    }

    #[test]
    fn completion_decoders_stream_reasoning_chunks_separately_from_content() {
        // 1. OpenAI protocol decoder (`reasoning_content` + `reasoning`)
        let openai_sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think-1 \",\"content\":null}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"think-2\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer-ok\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut oai = protocol_decoder(ProviderProtocol::OpenAi);
        let mut oai_text = Vec::new();
        let mut oai_reasoning = Vec::new();
        for b in openai_sse.as_bytes() {
            oai.feed_with_reasoning(
                &[*b],
                &mut |s| {
                    oai_text.push(s.to_owned());
                    Ok(())
                },
                &mut |r| {
                    oai_reasoning.push(r.to_owned());
                    Ok(())
                },
            )
            .unwrap();
        }
        let (oai_msg, _) = oai.finish(&mut |_| Ok(())).unwrap();
        let oai_msg = oai_msg.to_legacy_value();
        assert_eq!(oai_reasoning, vec!["think-1 ", "think-2"]);
        assert_eq!(oai_text, vec!["answer-ok"]);
        assert_eq!(oai_msg["content"], "answer-ok");
        assert_eq!(oai_msg["reasoning_content"], "think-1 think-2");

        // 2. Anthropic protocol decoder (`thinking` start + `thinking_delta`)
        let anthropic_sse = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"步骤一：\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"分析依赖\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"结论完成\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":8}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        let mut ant = protocol_decoder(ProviderProtocol::Anthropic);
        let mut ant_text = Vec::new();
        let mut ant_reasoning = Vec::new();
        for b in anthropic_sse.as_bytes() {
            ant.feed_with_reasoning(
                &[*b],
                &mut |s| {
                    ant_text.push(s.to_owned());
                    Ok(())
                },
                &mut |r| {
                    ant_reasoning.push(r.to_owned());
                    Ok(())
                },
            )
            .unwrap();
        }
        let (ant_msg, _) = ant.finish(&mut |_| Ok(())).unwrap();
        let ant_msg = ant_msg.to_legacy_value();
        assert_eq!(ant_reasoning, vec!["步骤一：", "分析依赖"]);
        assert_eq!(ant_text, vec!["结论完成"]);
        assert_eq!(ant_msg["content"], "结论完成");
        assert_eq!(ant_msg["reasoning_content"], "步骤一：分析依赖");

        // 3. Gemini protocol decoder (`"thought": true` vs regular text + thought-only fallback)
        let gemini_sse = concat!(
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"thought\":true,\"text\":\"先排查配置；\"}]}}]}\r\n\r\n",
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"thought\":true,\"text\":\"确认端口\"},{\"text\":\"8080\"}]},\"finishReason\":\"STOP\"}]}\n\n"
        );
        let mut gem = protocol_decoder(ProviderProtocol::Gemini);
        let mut gem_text = Vec::new();
        let mut gem_reasoning = Vec::new();
        for b in gemini_sse.as_bytes() {
            gem.feed_with_reasoning(
                &[*b],
                &mut |s| {
                    gem_text.push(s.to_owned());
                    Ok(())
                },
                &mut |r| {
                    gem_reasoning.push(r.to_owned());
                    Ok(())
                },
            )
            .unwrap();
        }
        let (gem_msg, _) = gem.finish(&mut |_| Ok(())).unwrap();
        let gem_msg = gem_msg.to_legacy_value();
        assert_eq!(gem_reasoning, vec!["先排查配置；", "确认端口"]);
        assert_eq!(gem_text, vec!["8080"]);
        assert_eq!(gem_msg["content"], "8080");
        assert_eq!(gem_msg["reasoning_content"], "先排查配置；确认端口");
    }

    #[test]
    fn anthropic_decoder_and_encoder_roundtrip_redacted_thinking_order_images_and_error() {
        let ant_sse = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"t1\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-0\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque-redacted\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"t2\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-2\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":3,\"delta\":{\"type\":\"text_delta\",\"text\":\"Done.\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":3}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":4,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":4,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.png\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":4}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":12}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        let mut dec = AnthropicDecoder::default();
        dec.feed_with_reasoning(ant_sse.as_bytes(), &mut |_| Ok(()), &mut |_| Ok(()))
            .unwrap();
        let assistant = dec.finish_neutral(&mut |_| Ok(())).unwrap();
        assert_eq!(assistant.content.len(), 5);
        assert!(matches!(
            &assistant.content[0],
            ContentBlock::Thinking { text, signature: Some(sig), .. } if text == "t1" && sig == "sig-0"
        ));
        assert!(matches!(
            &assistant.content[1],
            ContentBlock::RedactedThinking { data } if data == "opaque-redacted"
        ));
        assert!(matches!(
            &assistant.content[2],
            ContentBlock::Thinking { text, signature: Some(sig), .. } if text == "t2" && sig == "sig-2"
        ));

        let history = vec![
            NeutralMessage::system("sys"),
            NeutralMessage::user_blocks(vec![
                ContentBlock::Text {
                    text: "inspect".into(),
                    signature: None,
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        mime_type: "image/png".into(),
                        data: "AAAA".into(),
                    },
                },
            ]),
            assistant,
            NeutralMessage::tool_result_blocks(
                "tu_1",
                Some("read_file".into()),
                vec![
                    ContentBlock::Text {
                        text: "tool fail".into(),
                        signature: None,
                    },
                    ContentBlock::Image {
                        source: ImageSource::Base64 {
                            mime_type: "image/jpeg".into(),
                            data: "BBBB".into(),
                        },
                    },
                ],
                true,
            ),
            NeutralMessage::tool_result("tu_2", None, "second err", true),
        ];
        let body =
            encode_anthropic_body_neutral("claude-sonnet-4", 1024, &history, None, None).unwrap();
        assert_eq!(body["system"], "sys");
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3); // user, assistant, merged tool_result user
        assert_eq!(msgs[0]["content"][1]["type"], "image");
        assert_eq!(msgs[0]["content"][1]["source"]["media_type"], "image/png");
        let asst_blocks = msgs[1]["content"].as_array().unwrap();
        assert_eq!(asst_blocks[0]["type"], "thinking");
        assert_eq!(asst_blocks[0]["signature"], "sig-0");
        assert_eq!(asst_blocks[1]["type"], "redacted_thinking");
        assert_eq!(asst_blocks[1]["data"], "opaque-redacted");
        assert_eq!(asst_blocks[2]["type"], "thinking");
        assert_eq!(asst_blocks[2]["signature"], "sig-2");
        assert_eq!(asst_blocks[3]["type"], "text");
        assert_eq!(asst_blocks[4]["type"], "tool_use");
        let tool_blocks = msgs[2]["content"].as_array().unwrap();
        assert_eq!(tool_blocks.len(), 2);
        assert_eq!(tool_blocks[0]["type"], "tool_result");
        assert_eq!(tool_blocks[0]["is_error"], true);
        assert_eq!(tool_blocks[0]["content"][1]["type"], "image");
        assert_eq!(tool_blocks[1]["tool_use_id"], "tu_2");
        assert_eq!(tool_blocks[1]["is_error"], true);
    }

    #[test]
    fn gemini_decoder_and_encoder_roundtrip_thought_signature_and_inline_images() {
        let gem_sse = concat!(
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[",
            "{\"thought\":true,\"text\":\"reasoning\",\"thoughtSignature\":\"tsig-1\"},",
            "{\"inlineData\":{\"mimeType\":\"image/png\",\"data\":\"iVBOR\"}},",
            "{\"functionCall\":{\"id\":\"gc-1\",\"name\":\"read_file\",\"args\":{\"path\":\"x.rs\"}},\"thoughtSignature\":\"tsig-fc\"}",
            "]},\"finishReason\":\"STOP\"}]}\n\n"
        );
        let mut dec = GeminiDecoder::default();
        dec.feed_with_reasoning(gem_sse.as_bytes(), &mut |_| Ok(()), &mut |_| Ok(()))
            .unwrap();
        let assistant = dec.finish_neutral(&mut |_| Ok(())).unwrap();
        assert!(matches!(
            &assistant.content[0],
            ContentBlock::Thinking { text, signature: Some(sig), .. } if text == "reasoning" && sig == "tsig-1"
        ));
        assert!(matches!(
            &assistant.content[1],
            ContentBlock::Image { source: ImageSource::Base64 { mime_type, data } } if mime_type == "image/png" && data == "iVBOR"
        ));
        assert!(matches!(
            &assistant.content[2],
            ContentBlock::ToolCall(tc) if tc.id == "gc-1" && tc.thought_signature.as_deref() == Some("tsig-fc")
        ));

        let history = vec![
            NeutralMessage::user_blocks(vec![
                ContentBlock::Text {
                    text: "look".into(),
                    signature: None,
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        mime_type: "image/jpeg".into(),
                        data: "JPGDATA".into(),
                    },
                },
            ]),
            assistant,
            NeutralMessage::tool_result("gc-1", Some("read_file".into()), "{\"ok\":true}", false),
        ];
        let body = encode_gemini_body_neutral(2048, &history, None).unwrap();
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(
            contents[0]["parts"][1]["inlineData"]["mimeType"],
            "image/jpeg"
        );
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["thought"], true);
        assert_eq!(contents[1]["parts"][0]["thoughtSignature"], "tsig-1");
        assert_eq!(contents[1]["parts"][1]["inlineData"]["data"], "iVBOR");
        assert_eq!(contents[1]["parts"][2]["functionCall"]["name"], "read_file");
        assert_eq!(contents[1]["parts"][2]["thoughtSignature"], "tsig-fc");
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"]["name"],
            "read_file"
        );
    }

    #[test]
    fn responses_encoder_and_decoder_roundtrip_encrypted_content_and_usage() {
        let history = vec![
            NeutralMessage::system("Follow rules."),
            NeutralMessage::user_blocks(vec![
                ContentBlock::Text {
                    text: "check".into(),
                    signature: None,
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        mime_type: "image/png".into(),
                        data: "PNG".into(),
                    },
                },
            ]),
            NeutralMessage::assistant(vec![
                ContentBlock::Thinking {
                    text: "unsigned should drop".into(),
                    signature: None,
                    encrypted_content: None,
                },
                ContentBlock::Thinking {
                    text: "signed summary".into(),
                    signature: Some("rs_item_1".into()),
                    encrypted_content: Some("enc_prev".into()),
                },
                ContentBlock::Text {
                    text: "Calling tool".into(),
                    signature: None,
                },
                ContentBlock::ToolCall(ToolCallBlock {
                    id: "call_r1".into(),
                    name: "read_file".into(),
                    arguments: "{\"path\":\"lib.rs\"}".into(),
                    thought_signature: None,
                    namespace: None,
                }),
            ]),
            NeutralMessage::tool_result("call_r1", Some("read_file".into()), "fn main() {}", false),
        ];
        let tools = [json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
            }
        })];
        let encoded = encode_responses_body_neutral(
            "gpt-5",
            2048,
            &history,
            Some(&tools),
            None,
            Some("high"),
            Some("detailed"),
        )
        .unwrap();
        assert_eq!(encoded["model"], "gpt-5");
        assert_eq!(encoded["instructions"], "Follow rules.");
        assert_eq!(encoded["max_output_tokens"], 2048);
        assert_eq!(encoded["store"], false);
        assert_eq!(encoded["stream"], true);
        assert_eq!(encoded["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(
            encoded["reasoning"],
            json!({"effort": "high", "summary": "detailed"})
        );
        assert_eq!(encoded["tools"][0]["type"], "function");
        assert_eq!(encoded["tools"][0]["name"], "read_file");
        assert!(encoded["tools"][0].get("function").is_none());

        let input = encoded["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][1]["type"], "input_image");
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["encrypted_content"], "enc_prev");
        assert_eq!(input[2]["role"], "assistant");
        assert_eq!(input[2]["content"][0]["type"], "output_text");
        assert_eq!(input[3]["type"], "function_call");
        assert_eq!(input[3]["call_id"], "call_r1");
        assert_eq!(input[4]["type"], "function_call_output");
        assert_eq!(input[4]["call_id"], "call_r1");

        let sse = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
            "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_1\",\"output_index\":0,\"delta\":\"step 1\"}\n\n",
            "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"enc_abc\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"step 1\"}]}}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":1,\"delta\":\"Done\"}\n\n",
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":2,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_42\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"output_index\":2,\"delta\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}\n\n",
            "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":2,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_42\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":100,\"input_tokens_details\":{\"cached_tokens\":20},\"output_tokens\":45,\"output_tokens_details\":{\"reasoning_tokens\":15},\"total_tokens\":145}}}\n\n"
        );
        let mut dec = ResponsesDecoder::default();
        let mut out_text = String::new();
        let mut out_reason = String::new();
        for byte in sse.as_bytes() {
            dec.feed_with_reasoning(
                &[*byte],
                &mut |t| {
                    out_text.push_str(t);
                    Ok(())
                },
                &mut |r| {
                    out_reason.push_str(r);
                    Ok(())
                },
            )
            .unwrap();
        }
        assert!(crate::provider_runtime::CompletionDecoder::is_done(&dec));
        assert_eq!(out_text, "Done");
        assert_eq!(out_reason, "step 1");
        assert_eq!(
            dec.usage_details,
            Some(ResponsesUsageDetails {
                cached_tokens: 20,
                reasoning_tokens: 15
            })
        );
        assert!(matches!(
            crate::provider_usage::classify_usage(&dec.usage),
            crate::provider_usage::UsageObservation::Valid(u)
                if u.prompt_tokens == 100 && u.completion_tokens == 45 && u.total_tokens == 145
        ));
        let msg = dec.finish_neutral(&mut |_| Ok(())).unwrap();
        assert!(matches!(
            &msg.content[0],
            ContentBlock::Thinking {
                text,
                signature: None,
                encrypted_content: Some(enc)
            } if text == "step 1" && enc == "enc_abc"
        ));
        assert_eq!(msg.text_content(), "Done");
        let calls: Vec<_> = msg.tool_calls().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_42");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, "{\"path\":\"a.rs\"}");

        let mut incomplete = ResponsesDecoder::default();
        incomplete
            .feed(
                b"event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
                &mut |_| Ok(()),
            )
            .unwrap();
        assert!(incomplete.finish_neutral(&mut |_| Ok(())).is_err());

        let mut failed = ResponsesDecoder::default();
        assert!(
            failed
                .feed(
                    b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"boom\"}}}\n\n",
                    &mut |_| Ok(()),
                )
                .is_err()
        );
    }

    #[test]
    fn anthropic_encoder_omits_unsigned_thinking_and_preserves_empty_text_signed_thinking() {
        let history = vec![
            NeutralMessage::user_text("hi"),
            NeutralMessage::assistant(vec![
                ContentBlock::Thinking {
                    text: "unsigned scrubbed thinking".into(),
                    signature: None,
                    encrypted_content: None,
                },
                ContentBlock::Thinking {
                    text: "empty sig thinking".into(),
                    signature: Some(String::new()),
                    encrypted_content: None,
                },
                ContentBlock::Thinking {
                    text: String::new(),
                    signature: Some("sigA-part2".into()),
                    encrypted_content: None,
                },
                ContentBlock::Text {
                    text: "ans".into(),
                    signature: None,
                },
            ]),
            NeutralMessage::user_text("next"),
            NeutralMessage::assistant(vec![ContentBlock::Thinking {
                text: "only unsigned".into(),
                signature: None,
                encrypted_content: None,
            }]),
        ];
        let body =
            encode_anthropic_body_neutral("claude-sonnet-4", 1024, &history, None, None).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(
            msgs[1]["content"],
            json!([
                {"type": "thinking", "thinking": "", "signature": "sigA-part2"},
                {"type": "text", "text": "ans"}
            ])
        );
        // Assistant message with only unsigned thinking degrades safely to empty text block
        // rather than schema-invalid unsigned thinking block.
        assert_eq!(msgs[3]["content"], json!([{"type": "text", "text": ""}]));
    }

    #[test]
    fn gemini_decoder_and_encoder_preserve_no_text_empty_text_and_consecutive_thought_signatures() {
        let gem_sse = concat!(
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[",
            "{\"thought\":true,\"thoughtSignature\":\"sig-no-text\"},",
            "{\"thought\":true,\"text\":\"t1\",\"thoughtSignature\":\"sig-1\"},",
            "{\"thought\":true,\"text\":\"t2\",\"thoughtSignature\":\"sig-2\"},",
            "{\"text\":\"\",\"thoughtSignature\":\"sig-empty-text\"},",
            "{\"functionCall\":{\"id\":\"gc-1\",\"name\":\"read_file\",\"args\":{\"path\":\"a.rs\"}},\"thoughtSignature\":\"sig-fc\"}",
            "]},\"finishReason\":\"STOP\"}]}\n\n"
        );
        let mut dec = GeminiDecoder::default();
        dec.feed_with_reasoning(gem_sse.as_bytes(), &mut |_| Ok(()), &mut |_| Ok(()))
            .unwrap();
        let assistant = dec.finish_neutral(&mut |_| Ok(())).unwrap();
        assert_eq!(assistant.content.len(), 5);
        assert!(matches!(
            &assistant.content[0],
            ContentBlock::Thinking { text, signature: Some(sig), .. } if text.is_empty() && sig == "sig-no-text"
        ));
        assert!(matches!(
            &assistant.content[1],
            ContentBlock::Thinking { text, signature: Some(sig), .. } if text == "t1" && sig == "sig-1"
        ));
        assert!(matches!(
            &assistant.content[2],
            ContentBlock::Thinking { text, signature: Some(sig), .. } if text == "t2" && sig == "sig-2"
        ));
        assert!(matches!(
            &assistant.content[3],
            ContentBlock::Text { text, signature: Some(sig) } if text.is_empty() && sig == "sig-empty-text"
        ));
        assert!(matches!(
            &assistant.content[4],
            ContentBlock::ToolCall(tc) if tc.thought_signature.as_deref() == Some("sig-fc")
        ));

        let body =
            encode_gemini_body_neutral(1024, &[NeutralMessage::user_text("q"), assistant], None)
                .unwrap();
        let parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 5);
        assert_eq!(
            parts[0],
            json!({"thought": true, "thoughtSignature": "sig-no-text"})
        );
        assert_eq!(
            parts[1],
            json!({"thought": true, "text": "t1", "thoughtSignature": "sig-1"})
        );
        assert_eq!(
            parts[2],
            json!({"thought": true, "text": "t2", "thoughtSignature": "sig-2"})
        );
        assert_eq!(
            parts[3],
            json!({"text": "", "thoughtSignature": "sig-empty-text"})
        );
        assert_eq!(parts[4]["thoughtSignature"], "sig-fc");
    }

    #[test]
    fn all_completion_decoders_handle_over_1100_tool_calls_per_turn() {
        const COUNT: usize = 1200;

        // 1. OpenAI Completions
        let mut oai_dec = Decoder::default();
        for i in 0..COUNT {
            let chunk = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":{i},\"id\":\"call_{i}\",\"function\":{{\"name\":\"t\",\"arguments\":\"{{}}\"}}}}]}}}}]}}\n\n"
            );
            oai_dec.feed(chunk.as_bytes(), &mut |_| Ok(())).unwrap();
        }
        oai_dec
            .feed(
                b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
                &mut |_| Ok(()),
            )
            .unwrap();
        let oai_msg = oai_dec.finish_neutral(&mut |_| Ok(())).unwrap();
        assert_eq!(oai_msg.tool_calls().count(), COUNT);

        // 2. OpenAI Responses
        let mut resp_dec = ResponsesDecoder::default();
        for i in 0..COUNT {
            let ev = format!(
                "event: response.output_item.done\ndata: {{\"type\":\"response.output_item.done\",\"output_index\":{i},\"item\":{{\"type\":\"function_call\",\"id\":\"fc_{i}\",\"call_id\":\"call_{i}\",\"name\":\"t\",\"arguments\":\"{{}}\"}}}}\n\n"
            );
            resp_dec.feed(ev.as_bytes(), &mut |_| Ok(())).unwrap();
        }
        resp_dec
            .feed(
                b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_big\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":10,\"total_tokens\":20}}}\n\ndata: [DONE]\n\n",
                &mut |_| Ok(()),
            )
            .unwrap();
        let resp_msg = resp_dec.finish_neutral(&mut |_| Ok(())).unwrap();
        assert_eq!(resp_msg.tool_calls().count(), COUNT);

        // 3. Anthropic
        let mut anth_dec = AnthropicDecoder::default();
        for i in 0..COUNT {
            let ev = format!(
                "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":{i},\"content_block\":{{\"type\":\"tool_use\",\"id\":\"tu_{i}\",\"name\":\"t\",\"input\":{{}}}}}}\n\n"
            );
            anth_dec.feed(ev.as_bytes(), &mut |_| Ok(())).unwrap();
        }
        anth_dec
            .feed(
                b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                &mut |_| Ok(()),
            )
            .unwrap();
        let anth_msg = anth_dec.finish_neutral(&mut |_| Ok(())).unwrap();
        assert_eq!(anth_msg.tool_calls().count(), COUNT);

        // 4. Gemini
        let mut gem_dec = GeminiDecoder::default();
        let mut parts = Vec::with_capacity(COUNT);
        for i in 0..COUNT {
            parts.push(json!({"functionCall":{"id":format!("gc_{i}"),"name":"t","args":{}}}));
        }
        let gem_sse = format!(
            "data: {}\n\n",
            json!({"candidates":[{"content":{"role":"model","parts":parts},"finishReason":"STOP"}]})
        );
        gem_dec.feed(gem_sse.as_bytes(), &mut |_| Ok(())).unwrap();
        let gem_msg = gem_dec.finish_neutral(&mut |_| Ok(())).unwrap();
        assert_eq!(gem_msg.tool_calls().count(), COUNT);
    }

    #[test]
    fn gemini_thinking_config_routes_by_model_generation_and_preserves_thought_signature() {
        let history = vec![
            NeutralMessage::user_text("solve"),
            NeutralMessage::assistant(vec![ContentBlock::ToolCall(ToolCallBlock {
                id: "gc-1".into(),
                name: "read_file".into(),
                arguments: "{\"path\":\"a.rs\"}".into(),
                thought_signature: Some("sig-keep".into()),
                namespace: None,
            })]),
            NeutralMessage::tool_result("gc-1", Some("read_file".into()), "ok", false),
        ];

        // Gemini 2.5 Pro -> thinkingBudget + includeThoughts, xhigh -> 32768
        let pro_25 = encode_gemini_body_neutral_with_thinking(
            "gemini-2.5-pro",
            4096,
            &history,
            None,
            Some("xhigh"),
        )
        .unwrap();
        assert_eq!(
            pro_25["generationConfig"]["thinkingConfig"],
            json!({"thinkingBudget": 32768, "includeThoughts": true})
        );
        assert_eq!(
            pro_25["contents"][1]["parts"][0]["thoughtSignature"],
            "sig-keep"
        );

        // Gemini 2.5 Flash -> xhigh capped at 24576, off -> thinkingBudget: 0
        let flash_25_max = encode_gemini_body_neutral_with_thinking(
            "models/gemini-2.5-flash",
            4096,
            &history,
            None,
            Some("max"),
        )
        .unwrap();
        assert_eq!(
            flash_25_max["generationConfig"]["thinkingConfig"],
            json!({"thinkingBudget": 24576, "includeThoughts": true})
        );
        let flash_25_off = encode_gemini_body_neutral_with_thinking(
            "gemini-2.5-flash-lite",
            4096,
            &history,
            None,
            Some("off"),
        )
        .unwrap();
        assert_eq!(
            flash_25_off["generationConfig"]["thinkingConfig"],
            json!({"thinkingBudget": 0})
        );
        // Gemini 2.5 Pro on "off" MUST omit thinkingConfig
        let pro_25_off = encode_gemini_body_neutral_with_thinking(
            "gemini-2.5-pro",
            4096,
            &history,
            None,
            Some("off"),
        )
        .unwrap();
        assert!(
            pro_25_off["generationConfig"]
                .get("thinkingConfig")
                .is_none()
        );

        // Gemini 3+ -> thinkingLevel + includeThoughts
        let g3_flash_min = encode_gemini_body_neutral_with_thinking(
            "gemini-3-flash-preview",
            4096,
            &history,
            None,
            Some("minimal"),
        )
        .unwrap();
        assert_eq!(
            g3_flash_min["generationConfig"]["thinkingConfig"],
            json!({"thinkingLevel": "minimal", "includeThoughts": true})
        );
        let g3_pro_min = encode_gemini_body_neutral_with_thinking(
            "gemini-3.1-pro-preview",
            4096,
            &history,
            None,
            Some("minimal"),
        )
        .unwrap();
        assert_eq!(
            g3_pro_min["generationConfig"]["thinkingConfig"],
            json!({"thinkingLevel": "low", "includeThoughts": true})
        );
        let g4_ultra = encode_gemini_body_neutral_with_thinking(
            "gemini-4-argon",
            4096,
            &history,
            None,
            Some("ultra"),
        )
        .unwrap();
        assert_eq!(
            g4_ultra["generationConfig"]["thinkingConfig"],
            json!({"thinkingLevel": "high", "includeThoughts": true})
        );
        let g3_off = encode_gemini_body_neutral_with_thinking(
            "gemini-3-flash-preview",
            4096,
            &history,
            None,
            Some("none"),
        )
        .unwrap();
        assert!(g3_off["generationConfig"].get("thinkingConfig").is_none());

        // Wired through encode_neutral_protocol_body_with_thinking
        let input = crate::provider_runtime::CompletionInput {
            model: "gemini-3-flash-preview",
            max_tokens: 2048,
            messages: &history,
            tools: &[],
        };
        let wired = encode_neutral_protocol_body_with_thinking(
            ProviderProtocol::Gemini,
            &input,
            Some("high"),
        )
        .unwrap();
        assert_eq!(
            wired["generationConfig"]["thinkingConfig"],
            json!({"thinkingLevel": "high", "includeThoughts": true})
        );
    }

    #[test]
    fn anthropic_adaptive_thinking_activates_for_claude_46_plus_only() {
        let history = vec![NeutralMessage::user_text("hi")];

        let opus_46_xhigh = encode_anthropic_body_neutral_with_thinking(
            "claude-opus-4-6",
            4096,
            &history,
            None,
            None,
            Some("xhigh"),
        )
        .unwrap();
        assert_eq!(opus_46_xhigh["thinking"], json!({"type": "adaptive"}));
        assert_eq!(opus_46_xhigh["output_config"], json!({"effort": "max"}));
        assert!(opus_46_xhigh["thinking"].get("budget_tokens").is_none());

        let sonnet_46_min = encode_anthropic_body_neutral_with_thinking(
            "claude-sonnet-4-6-20260205",
            4096,
            &history,
            None,
            None,
            Some("minimal"),
        )
        .unwrap();
        assert_eq!(sonnet_46_min["thinking"], json!({"type": "adaptive"}));
        assert_eq!(sonnet_46_min["output_config"], json!({"effort": "low"}));

        let opus_47_xhigh = encode_anthropic_body_neutral_with_thinking(
            "claude-opus-4-7",
            4096,
            &history,
            None,
            None,
            Some("xhigh"),
        )
        .unwrap();
        assert_eq!(opus_47_xhigh["thinking"], json!({"type": "adaptive"}));
        assert_eq!(opus_47_xhigh["output_config"], json!({"effort": "xhigh"}));

        let fable_5 = encode_anthropic_body_neutral_with_thinking(
            "claude-fable-5",
            4096,
            &history,
            None,
            None,
            Some("high"),
        )
        .unwrap();
        assert_eq!(fable_5["thinking"], json!({"type": "adaptive"}));
        assert_eq!(fable_5["output_config"], json!({"effort": "high"}));

        let off_46 = encode_anthropic_body_neutral_with_thinking(
            "claude-opus-4-6",
            4096,
            &history,
            None,
            None,
            Some("off"),
        )
        .unwrap();
        assert!(off_46.get("thinking").is_none());
        assert!(off_46.get("output_config").is_none());

        let legacy_45 = encode_anthropic_body_neutral_with_thinking(
            "claude-sonnet-4-5-20250929",
            16_384,
            &history,
            None,
            None,
            Some("medium"),
        )
        .unwrap();
        assert_eq!(
            legacy_45["thinking"],
            json!({"type": "enabled", "budget_tokens": 8192})
        );
        assert!(legacy_45.get("output_config").is_none());

        // encode_protocol_body(Anthropic, ...) must forward thinking_level / reasoning_effort
        let proto_xhigh = encode_protocol_body(
            ProviderProtocol::Anthropic,
            &json!({
                "model": "claude-opus-4-6",
                "max_tokens": 4096,
                "thinking_level": "xhigh",
                "messages": [{"role": "user", "content": "hi"}]
            }),
        )
        .unwrap();
        assert_eq!(proto_xhigh["thinking"], json!({"type": "adaptive"}));
        assert_eq!(proto_xhigh["output_config"], json!({"effort": "max"}));

        let proto_effort = encode_protocol_body(
            ProviderProtocol::Anthropic,
            &json!({
                "model": "claude-sonnet-4-5-20250929",
                "max_tokens": 16_384,
                "reasoning_effort": "medium",
                "messages": [{"role": "user", "content": "hi"}]
            }),
        )
        .unwrap();
        assert_eq!(
            proto_effort["thinking"],
            json!({"type": "enabled", "budget_tokens": 8192})
        );

        let proto_off = encode_protocol_body(
            ProviderProtocol::Anthropic,
            &json!({
                "model": "claude-opus-4-6",
                "max_tokens": 4096,
                "thinking_level": "off",
                "messages": [{"role": "user", "content": "hi"}]
            }),
        )
        .unwrap();
        assert!(proto_off.get("thinking").is_none());
        assert!(proto_off.get("output_config").is_none());
    }
}
