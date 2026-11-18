pub use crate::provider_transport::ProviderProtocol;
use crate::{
    domain::*,
    provider_transport::{
        NetworkPurpose, ProviderClient, ProviderPermit, RESPONSE_LIMIT, TransportError,
    },
    provider_usage::UsageObservation,
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn client() -> Result<ProviderClient> {
    Ok(ProviderClient::new()?)
}

pub(crate) async fn json_response(
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
    permit.matches(
        &route.base_url,
        NetworkPurpose::Models,
        Some(&route.model),
        None,
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
                    Ok(raw.to_owned())
                } else {
                    Ok(raw.strip_prefix("models/").unwrap_or(raw).to_owned())
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
                .map(String::from)
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
    size: usize,
    done: bool,
    finish: Option<String>,
    pub text: String,
    pub reasoning: String,
    tools: BTreeMap<usize, ToolParts>,
    pub usage: Value,
    usage_observation: UsageObservation,
}

impl Decoder {
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        ensure!(
            bytes.len() <= RESPONSE_LIMIT.saturating_sub(self.size),
            "模型输出超过安全上限"
        );
        self.size += bytes.len();
        self.buffer.extend_from_slice(bytes);
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(emit)?;
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.event.is_empty() {
                    self.event.push('\n');
                }
                self.event.push_str(data.trim_start());
            }
        }
        Ok(())
    }

    fn flush(&mut self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
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
        self.usage_observation.observe(&chunk["usage"]);
        self.usage = self.usage_observation.into_value();
        if let Some(choice) = chunk["choices"].as_array().and_then(|v| v.first()) {
            let delta = &choice["delta"];
            if let Some(reasoning) = delta["reasoning_content"]
                .as_str()
                .or_else(|| delta["reasoning"].as_str())
                && !reasoning.is_empty()
            {
                self.reasoning.push_str(reasoning);
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
                    ensure!(index < 64, "单轮工具调用过多");
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

    pub fn finish(self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<Value> {
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
            ensure!(!self.reasoning.trim().is_empty(), "模型没有返回内容");
            emit(&self.reasoning)?;
            self.reasoning.clone()
        } else {
            self.text
        };
        let mut message = json!({"role": "assistant", "content": content});
        if !self.reasoning.is_empty() {
            message["reasoning_content"] = json!(self.reasoning);
        }
        if !self.tools.is_empty() {
            let calls: Result<Vec<Value>> = self
                .tools
                .into_values()
                .map(|part| {
                    ensure!(
                        !part.id.is_empty() && !part.name.is_empty(),
                        "不完整的工具调用"
                    );
                    let _: Value =
                        serde_json::from_str(&part.arguments).context("工具参数不完整")?;
                    Ok(json!({
                        "id": part.id,
                        "type": "function",
                        "function": {"name": part.name, "arguments": part.arguments}
                    }))
                })
                .collect();
            message["tool_calls"] = json!(calls?);
        }
        Ok(message)
    }
}

fn extract_message_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(s) = part.as_str() {
                    out.push_str(s);
                } else if let Some(s) = part.get("text").and_then(Value::as_str) {
                    out.push_str(s);
                }
            }
            out
        }
        Value::Null => String::new(),
        other => other.to_string(),
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
    let messages = openai_body
        .get("messages")
        .and_then(Value::as_array)
        .context("missing messages array")?;

    let mut system_parts = Vec::new();
    let mut converted_messages = Vec::new();

    for msg in messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        match role {
            "system" => {
                let text = extract_message_text(&msg["content"]);
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            "user" => {
                let mut blocks = Vec::new();
                if let Some(parts) = msg["content"].as_array() {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                    }
                } else {
                    let text = extract_message_text(&msg["content"]);
                    blocks.push(json!({"type": "text", "text": text}));
                }
                push_merged_anthropic_message(&mut converted_messages, "user", blocks);
            }
            "assistant" => {
                let mut blocks = Vec::new();
                if let Some(thinking) = msg.get("reasoning_content").and_then(Value::as_str)
                    && !thinking.is_empty()
                {
                    if let Some(sig) = msg.get("thinking_signature").and_then(Value::as_str)
                        && !sig.is_empty()
                    {
                        blocks.push(json!({
                            "type": "thinking",
                            "thinking": thinking,
                            "signature": sig
                        }));
                    } else {
                        blocks.push(json!({
                            "type": "thinking",
                            "thinking": thinking
                        }));
                    }
                }
                let text = extract_message_text(&msg["content"]);
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .context("assistant tool_call missing id")?;
                        let name = call["function"]
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .context("assistant tool_call missing name")?;
                        let raw_args = call["function"]
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}");
                        let parsed: Value =
                            serde_json::from_str(raw_args).unwrap_or_else(|_| json!({}));
                        let input = if parsed.is_object() {
                            parsed
                        } else {
                            json!({})
                        };
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input
                        }));
                    }
                }
                if blocks.is_empty() {
                    blocks.push(json!({"type": "text", "text": ""}));
                }
                push_merged_anthropic_message(&mut converted_messages, "assistant", blocks);
            }
            "tool" => {
                let tool_use_id = msg
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .context("tool message missing tool_call_id")?;
                let content = match &msg["content"] {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content
                });
                push_merged_anthropic_message(&mut converted_messages, "user", vec![block]);
            }
            _ => {
                let text = extract_message_text(&msg["content"]);
                push_merged_anthropic_message(
                    &mut converted_messages,
                    "user",
                    vec![json!({"type": "text", "text": text})],
                );
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
    if let Some(tools) = openai_body.get("tools").and_then(Value::as_array)
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
            if let Some(choice) = openai_body.get("tool_choice") {
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

fn encode_gemini_body(openai_body: &Value) -> Result<Value> {
    let max_tokens = openai_body
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(4096);
    let messages = openai_body
        .get("messages")
        .and_then(Value::as_array)
        .context("missing messages array")?;

    let mut call_names = BTreeMap::new();
    for msg in messages {
        if msg.get("role").and_then(Value::as_str) == Some("assistant")
            && let Some(calls) = msg.get("tool_calls").and_then(Value::as_array)
        {
            for call in calls {
                if let (Some(id), Some(name)) = (
                    call.get("id").and_then(Value::as_str),
                    call["function"].get("name").and_then(Value::as_str),
                ) {
                    call_names.insert(id.to_owned(), name.to_owned());
                }
            }
        }
    }

    let mut system_parts = Vec::new();
    let mut contents = Vec::new();

    for msg in messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        match role {
            "system" => {
                let text = extract_message_text(&msg["content"]);
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            "user" => {
                let text = extract_message_text(&msg["content"]);
                push_merged_gemini_content(&mut contents, "user", vec![json!({"text": text})]);
            }
            "assistant" => {
                let mut parts = Vec::new();
                let text = extract_message_text(&msg["content"]);
                if !text.is_empty() {
                    parts.push(json!({"text": text}));
                }
                if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let name = call["function"]
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .context("assistant tool_call missing name")?;
                        let raw_args = call["function"]
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}");
                        let parsed: Value =
                            serde_json::from_str(raw_args).unwrap_or_else(|_| json!({}));
                        let args = if parsed.is_object() {
                            parsed
                        } else {
                            json!({})
                        };
                        parts.push(json!({
                            "functionCall": {
                                "name": name,
                                "args": args
                            }
                        }));
                    }
                }
                if parts.is_empty() {
                    parts.push(json!({"text": ""}));
                }
                push_merged_gemini_content(&mut contents, "model", parts);
            }
            "tool" => {
                let name = msg
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        msg.get("tool_call_id")
                            .and_then(Value::as_str)
                            .and_then(|id| call_names.get(id).map(String::as_str))
                    })
                    .unwrap_or("tool");
                let response_obj = match &msg["content"] {
                    Value::Object(_) => msg["content"].clone(),
                    Value::String(s) => match serde_json::from_str::<Value>(s) {
                        Ok(v) if v.is_object() => v,
                        _ => json!({"result": s}),
                    },
                    other => json!({"result": other}),
                };
                let part = json!({
                    "functionResponse": {
                        "name": name,
                        "response": response_obj
                    }
                });
                push_merged_gemini_content(&mut contents, "user", vec![part]);
            }
            _ => {
                let text = extract_message_text(&msg["content"]);
                push_merged_gemini_content(&mut contents, "user", vec![json!({"text": text})]);
            }
        }
    }

    let mut out = json!({
        "contents": contents,
        "generationConfig": {
            "maxOutputTokens": max_tokens
        }
    });
    if !system_parts.is_empty() {
        out["systemInstruction"] = json!({
            "parts": [{"text": system_parts.join("\n\n")}]
        });
    }
    if let Some(tools) = openai_body.get("tools").and_then(Value::as_array)
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

pub fn encode_protocol_body(protocol: ProviderProtocol, openai_body: &Value) -> Result<Value> {
    match protocol {
        ProviderProtocol::OpenAi => Ok(openai_body.clone()),
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

#[derive(Default)]
pub struct AnthropicDecoder {
    buffer: Vec<u8>,
    event_name: String,
    event_data: String,
    size: usize,
    done: bool,
    stop_reason: Option<String>,
    pub text: String,
    pub reasoning: String,
    thinking_signature: String,
    tools: BTreeMap<usize, AnthropicToolParts>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    pub usage: Value,
}

impl AnthropicDecoder {
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        ensure!(
            bytes.len() <= RESPONSE_LIMIT.saturating_sub(self.size),
            "模型输出超过安全上限"
        );
        self.size += bytes.len();
        self.buffer.extend_from_slice(bytes);
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(emit)?;
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

    fn flush(&mut self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
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
                        if let Some(text) = block.get("text").and_then(Value::as_str)
                            && !text.is_empty()
                        {
                            self.text.push_str(text);
                            emit(text)?;
                        }
                    }
                    Some("thinking") => {
                        if let Some(thinking) = block.get("thinking").and_then(Value::as_str)
                            && !thinking.is_empty()
                        {
                            self.reasoning.push_str(thinking);
                        }
                        if let Some(sig) = block.get("signature").and_then(Value::as_str)
                            && !sig.is_empty()
                        {
                            self.thinking_signature.push_str(sig);
                        }
                    }
                    Some("tool_use") => {
                        ensure!(index < 64, "单轮工具调用过多");
                        let part = self.tools.entry(index).or_default();
                        if let Some(id) = block.get("id").and_then(Value::as_str) {
                            part.id.push_str(id);
                        }
                        if let Some(name) = block.get("name").and_then(Value::as_str) {
                            part.name.push_str(name);
                        }
                        if let Some(input) = block.get("input") {
                            part.initial_input = Some(input.clone());
                        }
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
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(thinking) = delta.get("thinking").and_then(Value::as_str)
                            && !thinking.is_empty()
                        {
                            self.reasoning.push_str(thinking);
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(sig) = delta.get("signature").and_then(Value::as_str)
                            && !sig.is_empty()
                        {
                            self.thinking_signature.push_str(sig);
                        }
                    }
                    Some("input_json_delta") => {
                        ensure!(index < 64, "单轮工具调用过多");
                        if let Some(pj) = delta.get("partial_json").and_then(Value::as_str) {
                            self.tools
                                .entry(index)
                                .or_default()
                                .partial_json
                                .push_str(pj);
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

    pub fn finish(self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<Value> {
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
        let content = if self.tools.is_empty() && self.text.trim().is_empty() {
            ensure!(!self.reasoning.trim().is_empty(), "模型没有返回内容");
            emit(&self.reasoning)?;
            self.reasoning.clone()
        } else {
            self.text
        };
        let mut message = json!({"role": "assistant", "content": content});
        if !self.reasoning.is_empty() {
            message["reasoning_content"] = json!(self.reasoning);
        }
        if !self.thinking_signature.is_empty() {
            message["thinking_signature"] = json!(self.thinking_signature);
        }
        if !self.tools.is_empty() {
            let calls: Result<Vec<Value>> = self
                .tools
                .into_values()
                .map(|part| {
                    ensure!(
                        !part.id.is_empty() && !part.name.is_empty(),
                        "不完整的工具调用"
                    );
                    let arguments = if !part.partial_json.trim().is_empty() {
                        let parsed: Value =
                            serde_json::from_str(&part.partial_json).context("工具参数不完整")?;
                        ensure!(parsed.is_object(), "工具参数必须是对象");
                        part.partial_json
                    } else if let Some(initial) = part.initial_input.filter(Value::is_object) {
                        initial.to_string()
                    } else {
                        bail!("工具参数不完整");
                    };
                    Ok(json!({
                        "id": part.id,
                        "type": "function",
                        "function": {"name": part.name, "arguments": arguments}
                    }))
                })
                .collect();
            message["tool_calls"] = json!(calls?);
        }
        Ok(message)
    }
}

#[derive(Default)]
pub struct GeminiDecoder {
    buffer: Vec<u8>,
    event: String,
    size: usize,
    done: bool,
    saw_chunk: bool,
    finish_reason: Option<String>,
    pub text: String,
    pub reasoning: String,
    tools: Vec<ToolParts>,
    pub usage: Value,
}

impl GeminiDecoder {
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        ensure!(
            bytes.len() <= RESPONSE_LIMIT.saturating_sub(self.size),
            "模型输出超过安全上限"
        );
        self.size += bytes.len();
        self.buffer.extend_from_slice(bytes);
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(emit)?;
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.event.is_empty() {
                    self.event.push('\n');
                }
                self.event.push_str(data.trim_start());
            }
        }
        Ok(())
    }

    fn flush(&mut self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
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
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        if let Some(t) = part.get("text").and_then(Value::as_str)
                            && !t.is_empty()
                        {
                            self.reasoning.push_str(t);
                        }
                    } else if let Some(t) = part.get("text").and_then(Value::as_str)
                        && !t.is_empty()
                    {
                        self.text.push_str(t);
                        emit(t)?;
                    }
                    if let Some(fc) = part.get("functionCall") {
                        ensure!(self.tools.len() < 64, "单轮工具调用过多");
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
                        self.tools.push(ToolParts {
                            id,
                            name,
                            arguments: args_val.to_string(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    pub fn finish(self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<Value> {
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
        let content = if self.tools.is_empty() && self.text.trim().is_empty() {
            ensure!(!self.reasoning.trim().is_empty(), "模型没有返回内容");
            emit(&self.reasoning)?;
            self.reasoning.clone()
        } else {
            self.text
        };
        let mut message = json!({"role": "assistant", "content": content});
        if !self.reasoning.is_empty() {
            message["reasoning_content"] = json!(self.reasoning);
        }
        if !self.tools.is_empty() {
            let calls: Vec<Value> = self
                .tools
                .into_iter()
                .map(|part| {
                    json!({
                        "id": part.id,
                        "type": "function",
                        "function": {"name": part.name, "arguments": part.arguments}
                    })
                })
                .collect();
            message["tool_calls"] = json!(calls);
        }
        Ok(message)
    }
}

pub(crate) fn protocol_decoder(
    protocol: ProviderProtocol,
) -> Box<dyn crate::provider_runtime::CompletionDecoder> {
    match protocol {
        ProviderProtocol::OpenAi => Box::new(Decoder::default()),
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

    fn decoder(
        &self,
    ) -> std::result::Result<
        Box<dyn crate::provider_runtime::CompletionDecoder>,
        crate::provider_runtime::CodecError,
    > {
        Ok(Box::new(Decoder::default()))
    }
}

impl crate::provider_runtime::CompletionDecoder for Decoder {
    fn feed(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        Decoder::feed(self, bytes, &mut |text| emit(text))
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
        Decoder::finish(*self, &mut |text| emit(text))
            .map(|message| (message, usage))
            .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }
}

impl crate::provider_runtime::CompletionDecoder for AnthropicDecoder {
    fn feed(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        AnthropicDecoder::feed(self, bytes, &mut |text| emit(text))
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
        AnthropicDecoder::finish(*self, &mut |text| emit(text))
            .map(|message| (message, usage))
            .map_err(|_| crate::provider_runtime::CodecError::Failed)
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
        GeminiDecoder::finish(*self, &mut |text| emit(text))
            .map(|message| (message, usage))
            .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }
}

pub async fn complete(
    client: &ProviderClient,
    permit: ProviderPermit,
    route: &Route,
    messages: &[Value],
    tools: &[Value],
    mut emit: impl FnMut(&str) -> Result<()> + Send,
) -> std::result::Result<(Value, Value), TransportError> {
    let protocol = permit.protocol();
    permit.matches(
        &route.base_url,
        NetworkPurpose::Completion,
        Some(&route.model),
        None,
    )?;
    let mut openai_body = json!({
        "model": permit.model(),
        "messages": messages,
        "max_tokens": route.max_tokens,
        "stream": true,
        "stream_options": {"include_usage": true}
    });
    if !tools.is_empty() {
        openai_body["tools"] = json!(tools);
        openai_body["tool_choice"] = json!("auto");
    }
    let body = encode_protocol_body(protocol, &openai_body).map_err(|_| TransportError::Invalid)?;
    let response = client.send(permit, Some(&body)).await?;
    if !response.status().is_success() {
        json_response(response).await?;
        return Err(TransportError::Unknown);
    }
    let mut decoder = protocol_decoder(protocol);
    let mut bytes = response.bytes_stream();
    let mut response_size = 0usize;
    let emit_ref: &mut crate::provider_runtime::Emit<'_> = &mut emit;
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk.map_err(|_| TransportError::Unknown)?;
        response_size = response_size
            .checked_add(chunk.len())
            .filter(|size| *size <= RESPONSE_LIMIT)
            .ok_or(TransportError::Unknown)?;
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
}
