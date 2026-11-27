//! Typed neutral message IR bridging SQLite persistence, codaKernel/1 JSONL v2, and provider codecs.

use crate::provider_transport::ProviderProtocol;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { mime_type: String, data: String },
    Url { url: String },
}

impl ImageSource {
    pub fn into_data_uri(&self) -> String {
        match self {
            Self::Base64 { mime_type, data } => format!("data:{mime_type};base64,{data}"),
            Self::Url { url } => url.clone(),
        }
    }

    pub fn from_image_url(url: &str) -> Self {
        if let Some(rest) = url.strip_prefix("data:")
            && let Some((meta, data)) = rest.split_once(";base64,")
            && !meta.trim().is_empty()
        {
            return Self::Base64 {
                mime_type: meta.trim().to_owned(),
                data: data.to_owned(),
            };
        }
        Self::Url {
            url: url.to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallBlock {
    pub id: String,
    pub name: String,
    /// Raw JSON object string; preserves stable_safe_json escaping & key order.
    pub arguments: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    Image {
        source: ImageSource,
    },
    Audio {
        path: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "durationMs"
        )]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Video {
        path: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "durationMs"
        )]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Resource {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "mimeType")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolCall(ToolCallBlock),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum NeutralRole {
    System,
    User,
    Assistant,
    ToolResult {
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        is_error: bool,
    },
    Custom {
        name: String,
        display: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_in_summary: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeutralMessage {
    pub role: NeutralRole,
    pub content: Vec<ContentBlock>,
}

impl NeutralMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: NeutralRole::System,
            content: vec![ContentBlock::Text {
                text: text.into(),
                signature: None,
            }],
        }
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: NeutralRole::User,
            content: vec![ContentBlock::Text {
                text: text.into(),
                signature: None,
            }],
        }
    }

    pub fn user_blocks(content: Vec<ContentBlock>) -> Self {
        Self {
            role: NeutralRole::User,
            content,
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: NeutralRole::Assistant,
            content,
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: NeutralRole::Assistant,
            content: vec![ContentBlock::Text {
                text: text.into(),
                signature: None,
            }],
        }
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        name: Option<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: NeutralRole::ToolResult {
                call_id: call_id.into(),
                name,
                is_error,
            },
            content: vec![ContentBlock::Text {
                text: content.into(),
                signature: None,
            }],
        }
    }

    pub fn tool_result_blocks(
        call_id: impl Into<String>,
        name: Option<String>,
        content: Vec<ContentBlock>,
        is_error: bool,
    ) -> Self {
        Self {
            role: NeutralRole::ToolResult {
                call_id: call_id.into(),
                name,
                is_error,
            },
            content,
        }
    }

    pub fn role_name(&self) -> &'static str {
        match &self.role {
            NeutralRole::System => "system",
            NeutralRole::User => "user",
            NeutralRole::Assistant => "assistant",
            NeutralRole::ToolResult { .. } => "tool",
            NeutralRole::Custom { .. } => "custom",
        }
    }

    pub fn text_content(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text, .. } = block {
                out.push_str(text);
            }
        }
        out
    }

    pub fn reasoning_text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Thinking { text, .. } = block {
                out.push_str(text);
            }
        }
        out
    }

    pub fn tool_calls(&self) -> impl DoubleEndedIterator<Item = &ToolCallBlock> {
        self.content.iter().filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
    }

    pub fn tool_calls_mut(&mut self) -> impl Iterator<Item = &mut ToolCallBlock> {
        self.content.iter_mut().filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
    }

    pub fn tool_call_id(&self) -> Option<&str> {
        match &self.role {
            NeutralRole::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        }
    }

    pub fn is_tool_result(&self) -> bool {
        matches!(self.role, NeutralRole::ToolResult { .. })
    }

    pub fn to_legacy_value(&self) -> Value {
        match &self.role {
            NeutralRole::System => {
                if self.content.is_empty() {
                    json!({"role": "system", "content": ""})
                } else if let [
                    ContentBlock::Text {
                        text,
                        signature: None,
                    },
                ] = self.content.as_slice()
                {
                    json!({"role": "system", "content": text})
                } else {
                    let mut obj = json!({
                        "role": "system",
                        "content": blocks_to_legacy_content_parts(&self.content)
                    });
                    if self.content.iter().any(|b| {
                        !matches!(
                            b,
                            ContentBlock::Text {
                                signature: None,
                                ..
                            } | ContentBlock::Image { .. }
                        )
                    }) {
                        obj["coda_blocks"] = json!(self.content);
                    }
                    obj
                }
            }
            NeutralRole::User => {
                if self.content.is_empty() {
                    json!({"role": "user", "content": ""})
                } else if let [
                    ContentBlock::Text {
                        text,
                        signature: None,
                    },
                ] = self.content.as_slice()
                {
                    json!({"role": "user", "content": text})
                } else {
                    let mut obj = json!({
                        "role": "user",
                        "content": blocks_to_legacy_content_parts(&self.content)
                    });
                    if self.content.iter().any(|b| {
                        matches!(
                            b,
                            ContentBlock::Text {
                                signature: Some(_),
                                ..
                            } | ContentBlock::Audio { .. }
                                | ContentBlock::Video { .. }
                                | ContentBlock::Resource { .. }
                        )
                    }) {
                        obj["coda_blocks"] = json!(self.content);
                    }
                    obj
                }
            }
            NeutralRole::Assistant => {
                let mut map = Map::new();
                map.insert("role".into(), json!("assistant"));
                map.insert("content".into(), json!(self.text_content()));

                let reasoning = self.reasoning_text();
                if !reasoning.is_empty() {
                    map.insert("reasoning_content".into(), json!(reasoning));
                }

                let thinking_blocks: Vec<_> = self
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Thinking {
                            text,
                            signature,
                            encrypted_content,
                        } => Some((text, signature, encrypted_content)),
                        _ => None,
                    })
                    .collect();

                let has_redacted = self
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::RedactedThinking { .. }));

                if thinking_blocks.len() == 1
                    && !has_redacted
                    && thinking_blocks[0].2.is_none()
                    && !thinking_blocks[0].0.is_empty()
                    && let Some(sig) = thinking_blocks[0].1.as_deref()
                    && !sig.is_empty()
                {
                    map.insert("thinking_signature".into(), json!(sig));
                }

                let calls: Vec<Value> = self
                    .tool_calls()
                    .map(|tc| {
                        let mut call = json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments
                            }
                        });
                        if let Some(sig) = &tc.thought_signature {
                            call["thought_signature"] = json!(sig);
                        }
                        if let Some(ns) = &tc.namespace {
                            call["namespace"] = json!(ns);
                        }
                        call
                    })
                    .collect();
                if !calls.is_empty() {
                    map.insert("tool_calls".into(), Value::Array(calls));
                }

                if assistant_needs_coda_blocks(&self.content) {
                    let refs: Vec<Value> = self
                        .content
                        .iter()
                        .map(|block| match block {
                            ContentBlock::ToolCall(tc) => json!({
                                "type": "tool_call_ref",
                                "id": tc.id
                            }),
                            other => json!(other),
                        })
                        .collect();
                    map.insert("coda_blocks".into(), Value::Array(refs));
                }

                Value::Object(map)
            }
            NeutralRole::ToolResult {
                call_id,
                name,
                is_error,
            } => {
                let mut map = Map::new();
                map.insert("role".into(), json!("tool"));
                map.insert("tool_call_id".into(), json!(call_id));
                let content = if self.content.is_empty() {
                    json!("")
                } else if let [
                    ContentBlock::Text {
                        text,
                        signature: None,
                    },
                ] = self.content.as_slice()
                {
                    json!(text)
                } else {
                    Value::Array(blocks_to_legacy_content_parts(&self.content))
                };
                map.insert("content".into(), content);
                if let Some(name) = name {
                    map.insert("name".into(), json!(name));
                }
                if *is_error {
                    map.insert("is_error".into(), json!(true));
                }
                Value::Object(map)
            }
            NeutralRole::Custom {
                name,
                display,
                details,
                include_in_summary,
            } => {
                let mut map = Map::new();
                map.insert("role".into(), json!("custom"));
                map.insert("custom_type".into(), json!(name));
                let content = if self.content.is_empty() {
                    json!("")
                } else if let [
                    ContentBlock::Text {
                        text,
                        signature: None,
                    },
                ] = self.content.as_slice()
                {
                    json!(text)
                } else {
                    Value::Array(blocks_to_legacy_content_parts(&self.content))
                };
                map.insert("content".into(), content);
                map.insert("display".into(), json!(display));
                if let Some(details) = details {
                    map.insert("details".into(), details.clone());
                }
                if let Some(include) = include_in_summary {
                    map.insert("include_in_summary".into(), json!(include));
                }
                Value::Object(map)
            }
        }
    }

    pub fn from_legacy_value(value: &Value) -> Result<Self> {
        let obj = value
            .as_object()
            .context("legacy message must be an object")?;
        let role = obj
            .get("role")
            .and_then(Value::as_str)
            .context("legacy message missing role")?;
        match role {
            "system" => {
                let content = if let Some(blocks) = obj.get("coda_blocks").and_then(Value::as_array)
                {
                    blocks
                        .iter()
                        .map(|b| {
                            serde_json::from_value::<ContentBlock>(b.clone()).map_err(Into::into)
                        })
                        .collect::<Result<Vec<_>>>()?
                } else {
                    parse_legacy_content(obj.get("content").unwrap_or(&Value::Null))
                };
                Ok(Self {
                    role: NeutralRole::System,
                    content,
                })
            }
            "user" => {
                let content = if let Some(blocks) = obj.get("coda_blocks").and_then(Value::as_array)
                {
                    blocks
                        .iter()
                        .map(|b| {
                            serde_json::from_value::<ContentBlock>(b.clone()).map_err(Into::into)
                        })
                        .collect::<Result<Vec<_>>>()?
                } else {
                    parse_legacy_content(obj.get("content").unwrap_or(&Value::Null))
                };
                Ok(Self {
                    role: NeutralRole::User,
                    content,
                })
            }
            "assistant" => {
                let tool_calls = parse_legacy_tool_calls(obj.get("tool_calls"))?;
                if let Some(coda_blocks) = obj.get("coda_blocks").and_then(Value::as_array) {
                    let mut content = Vec::with_capacity(coda_blocks.len());
                    let mut referenced_ids = HashSet::new();
                    for raw_block in coda_blocks {
                        if raw_block.get("type").and_then(Value::as_str) == Some("tool_call_ref") {
                            let id = raw_block
                                .get("id")
                                .and_then(Value::as_str)
                                .context("tool_call_ref missing id")?;
                            let call = tool_calls
                                .iter()
                                .find(|c| c.id == id)
                                .cloned()
                                .with_context(|| format!("missing tool_call for ref {id}"))?;
                            referenced_ids.insert(id.to_owned());
                            content.push(ContentBlock::ToolCall(call));
                        } else {
                            let block: ContentBlock = serde_json::from_value(raw_block.clone())?;
                            if let ContentBlock::ToolCall(ref tc) = block {
                                referenced_ids.insert(tc.id.clone());
                            }
                            content.push(block);
                        }
                    }
                    for tc in tool_calls {
                        if !referenced_ids.contains(&tc.id) {
                            content.push(ContentBlock::ToolCall(tc));
                        }
                    }
                    return Ok(Self {
                        role: NeutralRole::Assistant,
                        content,
                    });
                }

                let mut content = Vec::new();
                if let Some(items) = obj.get("reasoning_items").and_then(Value::as_array) {
                    for item in items {
                        if let Some(enc) = item
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            let summary = item
                                .get("summary")
                                .and_then(Value::as_array)
                                .map(|parts| {
                                    parts
                                        .iter()
                                        .filter_map(|p| p.get("text").and_then(Value::as_str))
                                        .collect::<Vec<_>>()
                                        .join("")
                                })
                                .or_else(|| {
                                    item.get("summary")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned)
                                })
                                .unwrap_or_default();
                            content.push(ContentBlock::Thinking {
                                text: summary,
                                signature: None,
                                encrypted_content: Some(enc.to_owned()),
                            });
                        }
                    }
                }
                if content.is_empty() {
                    let reasoning = obj
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let signature = obj
                        .get("thinking_signature")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    if !reasoning.is_empty() || signature.is_some() {
                        content.push(ContentBlock::Thinking {
                            text: reasoning.to_owned(),
                            signature,
                            encrypted_content: None,
                        });
                    }
                }
                match obj.get("content") {
                    Some(Value::String(s)) => {
                        if !s.is_empty() || (content.is_empty() && tool_calls.is_empty()) {
                            content.push(ContentBlock::Text {
                                text: s.clone(),
                                signature: None,
                            });
                        }
                    }
                    Some(Value::Array(parts)) => {
                        content.extend(parse_legacy_content_array(parts));
                    }
                    _ => {}
                }
                for tc in tool_calls {
                    content.push(ContentBlock::ToolCall(tc));
                }
                Ok(Self {
                    role: NeutralRole::Assistant,
                    content,
                })
            }
            "tool" => {
                let call_id = obj
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let name = obj.get("name").and_then(Value::as_str).map(str::to_owned);
                let is_error = obj
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let content = parse_legacy_content(obj.get("content").unwrap_or(&Value::Null));
                Ok(Self {
                    role: NeutralRole::ToolResult {
                        call_id,
                        name,
                        is_error,
                    },
                    content,
                })
            }
            "custom" => {
                let name = obj
                    .get("custom_type")
                    .and_then(Value::as_str)
                    .context("custom message missing custom_type")?
                    .to_owned();
                let display = obj.get("display").and_then(Value::as_bool).unwrap_or(false);
                let details = obj.get("details").cloned();
                let include_in_summary = obj.get("include_in_summary").and_then(Value::as_bool);
                let content = parse_legacy_content(obj.get("content").unwrap_or(&Value::Null));
                Ok(Self {
                    role: NeutralRole::Custom {
                        name,
                        display,
                        details,
                        include_in_summary,
                    },
                    content,
                })
            }
            other => bail!("unsupported legacy message role: {other}"),
        }
    }
}

pub fn to_legacy_values(messages: &[NeutralMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(NeutralMessage::to_legacy_value)
        .collect()
}

pub fn from_legacy_values(values: &[Value]) -> Result<Vec<NeutralMessage>> {
    values
        .iter()
        .map(NeutralMessage::from_legacy_value)
        .collect()
}

pub mod sqlite_messages_serde {
    use super::*;

    pub fn serialize<S>(messages: &[NeutralMessage], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let values = to_legacy_values(messages);
        values.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<NeutralMessage>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = Vec::<Value>::deserialize(deserializer)?;
        from_legacy_values(&values).map_err(serde::de::Error::custom)
    }
}

fn assistant_needs_coda_blocks(blocks: &[ContentBlock]) -> bool {
    let mut thinking_count = 0usize;
    let mut text_count = 0usize;
    let mut saw_text = false;
    let mut saw_tool_call = false;

    for block in blocks {
        match block {
            ContentBlock::RedactedThinking { .. } => return true,
            ContentBlock::Image { .. }
            | ContentBlock::Audio { .. }
            | ContentBlock::Video { .. }
            | ContentBlock::Resource { .. } => return true,
            ContentBlock::Thinking {
                text,
                signature,
                encrypted_content,
            } => {
                thinking_count += 1;
                if thinking_count > 1
                    || encrypted_content.is_some()
                    || (text.is_empty() && signature.is_some())
                    || saw_text
                    || saw_tool_call
                {
                    return true;
                }
            }
            ContentBlock::Text { signature, .. } => {
                text_count += 1;
                saw_text = true;
                if text_count > 1 || signature.is_some() || saw_tool_call {
                    return true;
                }
            }
            ContentBlock::ToolCall(_) => {
                saw_tool_call = true;
            }
        }
    }
    false
}

pub(crate) fn format_media_block_placeholder(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Audio {
            path,
            mime_type,
            duration_ms,
            name,
            ..
        } => {
            let mut details = vec![mime_type.clone()];
            if let Some(ms) = duration_ms {
                details.push(format!("{ms}ms"));
            }
            if let Some(label) = name.as_deref().filter(|s| !s.is_empty()) {
                details.push(label.to_owned());
            }
            Some(format!("[audio: {path} ({})]", details.join(", ")))
        }
        ContentBlock::Video {
            path,
            mime_type,
            duration_ms,
            name,
            ..
        } => {
            let mut details = vec![mime_type.clone()];
            if let Some(ms) = duration_ms {
                details.push(format!("{ms}ms"));
            }
            if let Some(label) = name.as_deref().filter(|s| !s.is_empty()) {
                details.push(label.to_owned());
            }
            Some(format!("[video: {path} ({})]", details.join(", ")))
        }
        ContentBlock::Resource {
            path,
            mime_type,
            name,
            ..
        } => {
            let mut details = Vec::new();
            if let Some(mime) = mime_type.as_deref().filter(|s| !s.is_empty()) {
                details.push(mime.to_owned());
            }
            if let Some(label) = name.as_deref().filter(|s| !s.is_empty()) {
                details.push(label.to_owned());
            }
            if details.is_empty() {
                Some(format!("[resource: {path}]"))
            } else {
                Some(format!("[resource: {path} ({})]", details.join(", ")))
            }
        }
        _ => None,
    }
}

pub(crate) fn content_text_with_media_placeholders(blocks: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text, .. } => {
                if !text.is_empty() {
                    parts.push(text.clone());
                }
            }
            other => {
                if let Some(placeholder) = format_media_block_placeholder(other) {
                    parts.push(placeholder);
                }
            }
        }
    }
    parts.join("\n")
}

fn blocks_to_legacy_content_parts(blocks: &[ContentBlock]) -> Vec<Value> {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text, .. } => {
                parts.push(json!({"type": "text", "text": text}));
            }
            ContentBlock::Image { source } => {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": source.into_data_uri() }
                }));
            }
            other => {
                if let Some(placeholder) = format_media_block_placeholder(other) {
                    parts.push(json!({"type": "text", "text": placeholder}));
                }
            }
        }
    }
    parts
}

fn parse_legacy_content(value: &Value) -> Vec<ContentBlock> {
    match value {
        Value::String(s) => {
            if s.is_empty() {
                Vec::new()
            } else {
                vec![ContentBlock::Text {
                    text: s.clone(),
                    signature: None,
                }]
            }
        }
        Value::Array(parts) => parse_legacy_content_array(parts),
        Value::Null => Vec::new(),
        other => vec![ContentBlock::Text {
            text: other.to_string(),
            signature: None,
        }],
    }
}

fn parse_legacy_content_array(parts: &[Value]) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    for part in parts {
        if let Some(s) = part.as_str() {
            out.push(ContentBlock::Text {
                text: s.to_owned(),
                signature: None,
            });
            continue;
        }
        match part.get("type").and_then(Value::as_str) {
            Some("image_url") => {
                if let Some(url) = part.pointer("/image_url/url").and_then(Value::as_str) {
                    out.push(ContentBlock::Image {
                        source: ImageSource::from_image_url(url),
                    });
                }
            }
            Some("image") => {
                if let Some(src) = part.get("source")
                    && let Some(data) = src.get("data").and_then(Value::as_str)
                    && let Some(mime) = src.get("media_type").and_then(Value::as_str)
                {
                    out.push(ContentBlock::Image {
                        source: ImageSource::Base64 {
                            mime_type: mime.to_owned(),
                            data: data.to_owned(),
                        },
                    });
                }
            }
            _ => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push(ContentBlock::Text {
                        text: text.to_owned(),
                        signature: None,
                    });
                }
            }
        }
    }
    out
}

fn parse_legacy_tool_calls(value: Option<&Value>) -> Result<Vec<ToolCallBlock>> {
    let Some(val) = value else {
        return Ok(Vec::new());
    };
    let arr = val
        .as_array()
        .context("legacy assistant tool_calls must be an array")?;
    let mut calls = Vec::with_capacity(arr.len());
    for call in arr {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let name = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let arguments = match call.pointer("/function/arguments") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => "{}".to_owned(),
        };
        let thought_signature = call
            .get("thought_signature")
            .or_else(|| call.get("thoughtSignature"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let namespace = call
            .get("namespace")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        calls.push(ToolCallBlock {
            id,
            name,
            arguments,
            thought_signature,
            namespace,
        });
    }
    Ok(calls)
}

/// Encode neutral messages to clean OpenAI Chat Completions `messages` JSON,
/// emitting `image_url` for images, preserving `reasoning_content`, and stripping
/// foreign provider fields (`thinking_signature`, `thought_signature`, `redacted_thinking`,
/// `encrypted_content`, `coda_blocks`).
pub(crate) fn encode_openai_chat_messages(messages: &[NeutralMessage]) -> Value {
    let mut out = Vec::with_capacity(messages.len());
    for msg in messages {
        match &msg.role {
            NeutralRole::System => {
                out.push(json!({
                    "role": "system",
                    "content": msg.text_content()
                }));
            }
            NeutralRole::User => {
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
                if has_image {
                    out.push(json!({
                        "role": "user",
                        "content": blocks_to_legacy_content_parts(&msg.content)
                    }));
                } else if has_media {
                    out.push(json!({
                        "role": "user",
                        "content": content_text_with_media_placeholders(&msg.content)
                    }));
                } else {
                    out.push(json!({
                        "role": "user",
                        "content": msg.text_content()
                    }));
                }
            }
            NeutralRole::Assistant => {
                let mut map = Map::new();
                map.insert("role".into(), json!("assistant"));
                map.insert("content".into(), json!(msg.text_content()));
                let reasoning = msg.reasoning_text();
                if !reasoning.is_empty() {
                    map.insert("reasoning_content".into(), json!(reasoning));
                }
                let calls: Vec<Value> = msg
                    .tool_calls()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments
                            }
                        })
                    })
                    .collect();
                if !calls.is_empty() {
                    map.insert("tool_calls".into(), Value::Array(calls));
                }
                out.push(Value::Object(map));
            }
            NeutralRole::ToolResult { call_id, .. } => {
                let has_media = msg.content.iter().any(|b| {
                    matches!(
                        b,
                        ContentBlock::Audio { .. }
                            | ContentBlock::Video { .. }
                            | ContentBlock::Resource { .. }
                    )
                });
                let content = if has_media {
                    content_text_with_media_placeholders(&msg.content)
                } else {
                    msg.text_content()
                };
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": content
                }));
            }
            NeutralRole::Custom { .. } => {
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
                if has_image {
                    out.push(json!({
                        "role": "user",
                        "content": blocks_to_legacy_content_parts(&msg.content)
                    }));
                } else if has_media {
                    let text = content_text_with_media_placeholders(&msg.content);
                    if !text.is_empty() {
                        out.push(json!({
                            "role": "user",
                            "content": text
                        }));
                    }
                } else {
                    let text = msg.text_content();
                    if !text.is_empty() {
                        out.push(json!({
                            "role": "user",
                            "content": text
                        }));
                    }
                }
            }
        }
    }
    Value::Array(out)
}

// --- codaKernel/1 JSONL v2 sessionEntrySchema bridge ---

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum KernelContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Audio {
        path: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "durationMs"
        )]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Video {
        path: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "durationMs"
        )]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Resource {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "mimeType")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum SessionBlock {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Audio {
        path: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "durationMs"
        )]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Video {
        path: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "durationMs"
        )]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Resource {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "mimeType")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted: Option<bool>,
    },
    ToolCall {
        #[serde(rename = "callId")]
        call_id: String,
        name: String,
        args: Map<String, Value>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "thoughtSignature"
        )]
        thought_signature: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KernelModel {
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_provider_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomValue {
    pub value: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionMessageRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum SessionEntry {
    Message {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        role: SessionMessageRole,
        content: Vec<SessionBlock>,
    },
    ToolResult {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        #[serde(rename = "callId")]
        call_id: String,
        name: String,
        content: Vec<KernelContent>,
        #[serde(rename = "isError")]
        is_error: bool,
    },
    Model {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        model: KernelModel,
    },
    ThinkingLevel {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        level: String,
    },
    Usage {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        category: String,
        model: KernelModel,
        usage: Map<String, Value>,
    },
    Compaction {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        summary: String,
        #[serde(rename = "firstKeptEntryId")]
        first_kept_entry_id: String,
        #[serde(rename = "tokensBefore")]
        tokens_before: u64,
    },
    BranchSummary {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        #[serde(rename = "fromId")]
        from_id: String,
        summary: String,
    },
    ContextEdit {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        #[serde(rename = "targetId")]
        target_id: String,
        replacement: Option<Vec<SessionBlock>>,
    },
    Label {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        #[serde(rename = "targetId")]
        target_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    SessionInfo {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Custom {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<CustomValue>,
    },
    CustomMessage {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Map<String, Value>>,
        name: String,
        content: Vec<KernelContent>,
        display: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<CustomValue>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "includeInSummary"
        )]
        include_in_summary: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEntryPayload {
    Message {
        role: SessionMessageRole,
        content: Vec<SessionBlock>,
    },
    ToolResult {
        call_id: String,
        name: String,
        content: Vec<KernelContent>,
        is_error: bool,
    },
    CustomMessage {
        name: String,
        content: Vec<KernelContent>,
        display: bool,
        details: Option<CustomValue>,
        include_in_summary: Option<bool>,
    },
}

impl SessionBlock {
    /// Protocol-agnostic conversion; keeps `Thinking.signature` in `ContentBlock::Thinking.signature`
    /// with `encrypted_content: None` so foreign signatures are never promoted blindly.
    pub fn to_content_block(&self) -> ContentBlock {
        self.to_content_block_for_protocol(None)
    }

    /// Protocol-aware conversion: when `protocol == Some(ProviderProtocol::OpenAiResponses)`,
    /// promotes non-redacted `SessionBlock::Thinking.signature` into `encrypted_content`
    /// while leaving Anthropic/Gemini/OpenAI Chat/unknown signatures in `signature`.
    pub fn to_content_block_for_protocol(
        &self,
        protocol: Option<ProviderProtocol>,
    ) -> ContentBlock {
        match self {
            Self::Text { text, signature } => ContentBlock::Text {
                text: text.clone(),
                signature: signature.clone(),
            },
            Self::Image { data, mime_type } => ContentBlock::Image {
                source: ImageSource::Base64 {
                    mime_type: mime_type.clone(),
                    data: data.clone(),
                },
            },
            Self::Audio {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => ContentBlock::Audio {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            },
            Self::Video {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => ContentBlock::Video {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            },
            Self::Resource {
                path,
                mime_type,
                bytes,
                sha256,
                name,
            } => ContentBlock::Resource {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                sha256: sha256.clone(),
                name: name.clone(),
            },
            Self::Thinking {
                text,
                signature,
                redacted,
            } => {
                if *redacted == Some(true) {
                    ContentBlock::RedactedThinking { data: text.clone() }
                } else if protocol == Some(ProviderProtocol::OpenAiResponses) {
                    ContentBlock::Thinking {
                        text: text.clone(),
                        signature: None,
                        encrypted_content: signature.clone(),
                    }
                } else {
                    ContentBlock::Thinking {
                        text: text.clone(),
                        signature: signature.clone(),
                        encrypted_content: None,
                    }
                }
            }
            Self::ToolCall {
                call_id,
                name,
                args,
                thought_signature,
                namespace,
            } => ContentBlock::ToolCall(ToolCallBlock {
                id: call_id.clone(),
                name: name.clone(),
                arguments: Value::Object(args.clone()).to_string(),
                thought_signature: thought_signature.clone(),
                namespace: namespace.clone(),
            }),
        }
    }

    pub fn from_content_block(block: &ContentBlock) -> Result<Self> {
        Self::from_content_block_inner(block, false)
    }

    /// Embedded-safe variant: malformed or non-object tool call arguments degrade
    /// to `{}` in `SessionEntry` so the turn can still record `entry(assistant)`
    /// and synthesize an `isError: true` tool result per `codaKernel/1 §7.1`.
    pub fn from_content_block_lossy(block: &ContentBlock) -> Result<Self> {
        Self::from_content_block_inner(block, true)
    }

    fn from_content_block_inner(block: &ContentBlock, lossy_tool_args: bool) -> Result<Self> {
        match block {
            ContentBlock::Text { text, signature } => Ok(Self::Text {
                text: text.clone(),
                signature: signature.clone(),
            }),
            ContentBlock::Image { source } => {
                let (mime_type, data) = image_source_to_base64(source)?;
                Ok(Self::Image { data, mime_type })
            }
            ContentBlock::Audio {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => Ok(Self::Audio {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            }),
            ContentBlock::Video {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => Ok(Self::Video {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            }),
            ContentBlock::Resource {
                path,
                mime_type,
                bytes,
                sha256,
                name,
            } => Ok(Self::Resource {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                sha256: sha256.clone(),
                name: name.clone(),
            }),
            ContentBlock::Thinking {
                text,
                signature,
                encrypted_content,
            } => Ok(Self::Thinking {
                text: text.clone(),
                signature: signature.clone().or_else(|| encrypted_content.clone()),
                redacted: None,
            }),
            ContentBlock::RedactedThinking { data } => Ok(Self::Thinking {
                text: data.clone(),
                signature: None,
                redacted: Some(true),
            }),
            ContentBlock::ToolCall(tc) => {
                let args = match serde_json::from_str::<Value>(&tc.arguments) {
                    Ok(Value::Object(map)) => map,
                    Ok(_) if lossy_tool_args => Map::new(),
                    Err(_) if lossy_tool_args => Map::new(),
                    Ok(_) => {
                        bail!("tool call {} arguments must be a JSON object", tc.id);
                    }
                    Err(err) => {
                        return Err(err).with_context(|| {
                            format!("tool call {} arguments are not valid JSON", tc.id)
                        });
                    }
                };
                Ok(Self::ToolCall {
                    call_id: tc.id.clone(),
                    name: tc.name.clone(),
                    args,
                    thought_signature: tc.thought_signature.clone(),
                    namespace: tc.namespace.clone(),
                })
            }
        }
    }

    pub fn validate_for_embedded(
        &self,
        media_enabled: bool,
    ) -> std::result::Result<(), &'static str> {
        match self {
            Self::Text { .. }
            | Self::Image { .. }
            | Self::Thinking { .. }
            | Self::ToolCall { .. } => Ok(()),
            Self::Audio {
                path, mime_type, ..
            }
            | Self::Video {
                path, mime_type, ..
            } => {
                if !media_enabled {
                    return Err(
                        "audio, video, and resource blocks require codaKernel/1.1 media support",
                    );
                }
                validate_kernel_media_path(path)?;
                if mime_type.trim().is_empty() {
                    return Err("media block mimeType must be non-empty");
                }
                Ok(())
            }
            Self::Resource {
                path, mime_type, ..
            } => {
                if !media_enabled {
                    return Err(
                        "audio, video, and resource blocks require codaKernel/1.1 media support",
                    );
                }
                validate_kernel_media_path(path)?;
                if let Some(mime) = mime_type
                    && mime.trim().is_empty()
                {
                    return Err("resource block mimeType must be non-empty when present");
                }
                Ok(())
            }
        }
    }
}

pub(crate) fn validate_kernel_media_path(path: &str) -> std::result::Result<(), &'static str> {
    if path.trim().is_empty() {
        return Err("media block path must be non-empty");
    }
    if path.chars().count() > 4096 {
        return Err("media block path must not exceed 4096 characters");
    }
    if path.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err("media block path must not contain control characters");
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err("media block path must be workspace-relative");
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err("media block path must not include a drive letter");
    }
    if path.split(['/', '\\']).any(|seg| seg == "..") {
        return Err("media block path must not contain '..' traversal segments");
    }
    Ok(())
}

impl KernelContent {
    pub fn is_media_block(&self) -> bool {
        matches!(
            self,
            Self::Audio { .. } | Self::Video { .. } | Self::Resource { .. }
        )
    }

    pub fn validate_for_embedded(
        &self,
        media_enabled: bool,
    ) -> std::result::Result<(), &'static str> {
        match self {
            Self::Text { .. } | Self::Image { .. } => Ok(()),
            Self::Audio {
                path, mime_type, ..
            }
            | Self::Video {
                path, mime_type, ..
            } => {
                if !media_enabled {
                    return Err(
                        "audio, video, and resource blocks require codaKernel/1.1 media support",
                    );
                }
                validate_kernel_media_path(path)?;
                if mime_type.trim().is_empty() {
                    return Err("media block mimeType must be non-empty");
                }
                Ok(())
            }
            Self::Resource {
                path, mime_type, ..
            } => {
                if !media_enabled {
                    return Err(
                        "audio, video, and resource blocks require codaKernel/1.1 media support",
                    );
                }
                validate_kernel_media_path(path)?;
                if let Some(mime) = mime_type
                    && mime.trim().is_empty()
                {
                    return Err("resource block mimeType must be non-empty when present");
                }
                Ok(())
            }
        }
    }

    pub fn to_session_block(&self) -> SessionBlock {
        match self {
            Self::Text { text } => SessionBlock::Text {
                text: text.clone(),
                signature: None,
            },
            Self::Image { data, mime_type } => SessionBlock::Image {
                data: data.clone(),
                mime_type: mime_type.clone(),
            },
            Self::Audio {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => SessionBlock::Audio {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            },
            Self::Video {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => SessionBlock::Video {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            },
            Self::Resource {
                path,
                mime_type,
                bytes,
                sha256,
                name,
            } => SessionBlock::Resource {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                sha256: sha256.clone(),
                name: name.clone(),
            },
        }
    }

    pub fn to_content_block(&self) -> ContentBlock {
        match self {
            Self::Text { text } => ContentBlock::Text {
                text: text.clone(),
                signature: None,
            },
            Self::Image { data, mime_type } => ContentBlock::Image {
                source: ImageSource::Base64 {
                    mime_type: mime_type.clone(),
                    data: data.clone(),
                },
            },
            Self::Audio {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => ContentBlock::Audio {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            },
            Self::Video {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => ContentBlock::Video {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            },
            Self::Resource {
                path,
                mime_type,
                bytes,
                sha256,
                name,
            } => ContentBlock::Resource {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                sha256: sha256.clone(),
                name: name.clone(),
            },
        }
    }

    pub fn from_content_block(block: &ContentBlock) -> Result<Self> {
        match block {
            ContentBlock::Text { text, .. } => Ok(Self::Text { text: text.clone() }),
            ContentBlock::Image { source } => {
                let (mime_type, data) = image_source_to_base64(source)?;
                Ok(Self::Image { data, mime_type })
            }
            ContentBlock::Audio {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => Ok(Self::Audio {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            }),
            ContentBlock::Video {
                path,
                mime_type,
                bytes,
                duration_ms,
                sha256,
                name,
            } => Ok(Self::Video {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                duration_ms: *duration_ms,
                sha256: sha256.clone(),
                name: name.clone(),
            }),
            ContentBlock::Resource {
                path,
                mime_type,
                bytes,
                sha256,
                name,
            } => Ok(Self::Resource {
                path: path.clone(),
                mime_type: mime_type.clone(),
                bytes: *bytes,
                sha256: sha256.clone(),
                name: name.clone(),
            }),
            other => bail!("unsupported block in KernelContent: {other:?}"),
        }
    }
}

fn image_source_to_base64(source: &ImageSource) -> Result<(String, String)> {
    match source {
        ImageSource::Base64 { mime_type, data } => Ok((mime_type.clone(), data.clone())),
        ImageSource::Url { url } => match ImageSource::from_image_url(url) {
            ImageSource::Base64 { mime_type, data } => Ok((mime_type, data)),
            ImageSource::Url { .. } => {
                bail!("codaKernel/1 image blocks require base64 data and mimeType")
            }
        },
    }
}

fn protocol_from_metadata(metadata: &Map<String, Value>) -> Option<ProviderProtocol> {
    metadata
        .get("protocol")
        .and_then(Value::as_str)
        .and_then(ProviderProtocol::parse_explicit)
        .or_else(|| {
            metadata
                .get("provider")
                .and_then(Value::as_str)
                .and_then(ProviderProtocol::parse_explicit)
        })
}

fn infer_protocol_from_model_id(model_id: &str) -> Option<ProviderProtocol> {
    let trimmed = model_id.trim();
    let after_custom = if let Some(rest) = trimmed.strip_prefix("custom:") {
        rest.split_once(':').map(|(_, id)| id).unwrap_or(rest)
    } else {
        trimmed
    };
    let stripped = after_custom
        .rsplit_once('/')
        .map(|(_, id)| id)
        .unwrap_or(after_custom);
    let lower = stripped.to_ascii_lowercase();
    let after_custom_lower = after_custom.to_ascii_lowercase();
    let full_lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("claude") {
        Some(ProviderProtocol::Anthropic)
    } else if lower.starts_with("gemini")
        || after_custom_lower.starts_with("models/gemini")
        || full_lower.starts_with("models/gemini")
    {
        Some(ProviderProtocol::Gemini)
    } else {
        // OpenAI model IDs (e.g. gpt-*, o1*, o3*, o4*) can run on either Chat Completions
        // or Responses, so avoid guessing OpenAiResponses without explicit protocol metadata.
        None
    }
}

fn infer_model_entry_protocol(
    metadata: Option<&Map<String, Value>>,
    model: &KernelModel,
) -> Option<ProviderProtocol> {
    metadata
        .and_then(protocol_from_metadata)
        .or_else(|| {
            model
                .custom_provider_id
                .as_deref()
                .and_then(ProviderProtocol::parse_explicit)
        })
        .or_else(|| infer_protocol_from_model_id(&model.model_id))
}

impl SessionEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Message { id, .. }
            | Self::ToolResult { id, .. }
            | Self::Model { id, .. }
            | Self::ThinkingLevel { id, .. }
            | Self::Usage { id, .. }
            | Self::Compaction { id, .. }
            | Self::BranchSummary { id, .. }
            | Self::ContextEdit { id, .. }
            | Self::Label { id, .. }
            | Self::SessionInfo { id, .. }
            | Self::Custom { id, .. }
            | Self::CustomMessage { id, .. } => id,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            Self::Message { parent_id, .. }
            | Self::ToolResult { parent_id, .. }
            | Self::Model { parent_id, .. }
            | Self::ThinkingLevel { parent_id, .. }
            | Self::Usage { parent_id, .. }
            | Self::Compaction { parent_id, .. }
            | Self::BranchSummary { parent_id, .. }
            | Self::ContextEdit { parent_id, .. }
            | Self::Label { parent_id, .. }
            | Self::SessionInfo { parent_id, .. }
            | Self::Custom { parent_id, .. }
            | Self::CustomMessage { parent_id, .. } => parent_id.as_deref(),
        }
    }

    pub fn timestamp(&self) -> &str {
        match self {
            Self::Message { timestamp, .. }
            | Self::ToolResult { timestamp, .. }
            | Self::Model { timestamp, .. }
            | Self::ThinkingLevel { timestamp, .. }
            | Self::Usage { timestamp, .. }
            | Self::Compaction { timestamp, .. }
            | Self::BranchSummary { timestamp, .. }
            | Self::ContextEdit { timestamp, .. }
            | Self::Label { timestamp, .. }
            | Self::SessionInfo { timestamp, .. }
            | Self::Custom { timestamp, .. }
            | Self::CustomMessage { timestamp, .. } => timestamp,
        }
    }

    /// Protocol-agnostic conversion from a single `SessionEntry` to `NeutralMessage`.
    ///
    /// Because `codaKernel/1` `SessionEntry::Message` does not require a `protocol` field,
    /// this leaves `SessionBlock::Thinking.signature` in `ContentBlock::Thinking.signature`
    /// (`encrypted_content: None`) so foreign signatures (e.g. Anthropic) are never
    /// mistakenly emitted as OpenAI Responses `encrypted_content`.
    /// Callers hydrating a known protocol or branch should use
    /// [`Self::to_neutral_message_for_protocol`] or [`session_entries_to_neutral_messages`].
    pub fn to_neutral_message(&self) -> Result<Option<NeutralMessage>> {
        self.to_neutral_message_for_protocol(None)
    }

    /// Protocol-aware conversion from a single `SessionEntry` to `NeutralMessage`.
    ///
    /// When `protocol == Some(ProviderProtocol::OpenAiResponses)`, non-redacted
    /// `SessionBlock::Thinking.signature` is promoted to `ContentBlock::Thinking.encrypted_content`
    /// (`signature: None`) so `encode_responses_body_neutral` preserves reasoning continuity
    /// across `codaKernel/1` hydration while preserving cross-provider isolation.
    pub fn to_neutral_message_for_protocol(
        &self,
        protocol: Option<ProviderProtocol>,
    ) -> Result<Option<NeutralMessage>> {
        match self {
            Self::Message { role, content, .. } => {
                let neutral_role = match role {
                    SessionMessageRole::System => NeutralRole::System,
                    SessionMessageRole::User => NeutralRole::User,
                    SessionMessageRole::Assistant => NeutralRole::Assistant,
                };
                Ok(Some(NeutralMessage {
                    role: neutral_role,
                    content: content
                        .iter()
                        .map(|block| block.to_content_block_for_protocol(protocol))
                        .collect(),
                }))
            }
            Self::ToolResult {
                call_id,
                name,
                content,
                is_error,
                ..
            } => Ok(Some(NeutralMessage::tool_result_blocks(
                call_id.clone(),
                Some(name.clone()),
                content
                    .iter()
                    .map(KernelContent::to_content_block)
                    .collect(),
                *is_error,
            ))),
            Self::CustomMessage {
                name,
                content,
                display,
                details,
                include_in_summary,
                ..
            } => Ok(Some(NeutralMessage {
                role: NeutralRole::Custom {
                    name: name.clone(),
                    display: *display,
                    details: details.as_ref().map(|d| d.value.clone()),
                    include_in_summary: *include_in_summary,
                },
                content: content
                    .iter()
                    .map(KernelContent::to_content_block)
                    .collect(),
            })),
            Self::Model { .. }
            | Self::ThinkingLevel { .. }
            | Self::Usage { .. }
            | Self::Compaction { .. }
            | Self::BranchSummary { .. }
            | Self::ContextEdit { .. }
            | Self::Label { .. }
            | Self::SessionInfo { .. }
            | Self::Custom { .. } => Ok(None),
        }
    }
}

/// Converts an ordered branch of `codaKernel/1` `SessionEntry` items into `NeutralMessage`s
/// while tracking `SessionEntry::Model` entries along the branch so prior `openai-responses`
/// turns retain `encrypted_content` and prior `anthropic`/`gemini` turns keep `signature`
/// isolated without cross-provider contamination.
pub fn session_entries_to_neutral_messages(
    entries: &[SessionEntry],
    active_protocol: Option<ProviderProtocol>,
) -> Result<Vec<NeutralMessage>> {
    let last_model_idx = entries
        .iter()
        .rposition(|entry| matches!(entry, SessionEntry::Model { .. }));
    let mut current_protocol = if last_model_idx.is_some() {
        None
    } else {
        active_protocol
    };
    let mut messages = Vec::new();

    for (idx, entry) in entries.iter().enumerate() {
        match entry {
            SessionEntry::Model {
                metadata, model, ..
            } => {
                current_protocol =
                    infer_model_entry_protocol(metadata.as_ref(), model).or_else(|| {
                        if Some(idx) == last_model_idx {
                            active_protocol
                        } else {
                            None
                        }
                    });
            }
            SessionEntry::Message { metadata, .. } => {
                let msg_protocol = metadata
                    .as_ref()
                    .and_then(protocol_from_metadata)
                    .or(current_protocol);
                if let Some(msg) = entry.to_neutral_message_for_protocol(msg_protocol)? {
                    messages.push(msg);
                }
            }
            _ => {
                if let Some(msg) = entry.to_neutral_message_for_protocol(current_protocol)? {
                    messages.push(msg);
                }
            }
        }
    }

    Ok(messages)
}

impl NeutralMessage {
    pub fn to_session_entry_payload(&self) -> Result<SessionEntryPayload> {
        self.to_session_entry_payload_inner(false)
    }

    pub fn to_session_entry_payload_lossy(&self) -> Result<SessionEntryPayload> {
        self.to_session_entry_payload_inner(true)
    }

    fn to_session_entry_payload_inner(&self, lossy_tool_args: bool) -> Result<SessionEntryPayload> {
        match &self.role {
            NeutralRole::System | NeutralRole::User | NeutralRole::Assistant => {
                let role = match &self.role {
                    NeutralRole::System => SessionMessageRole::System,
                    NeutralRole::User => SessionMessageRole::User,
                    NeutralRole::Assistant => SessionMessageRole::Assistant,
                    _ => unreachable!(),
                };
                let content = self
                    .content
                    .iter()
                    .map(|b| SessionBlock::from_content_block_inner(b, lossy_tool_args))
                    .collect::<Result<Vec<_>>>()?;
                Ok(SessionEntryPayload::Message { role, content })
            }
            NeutralRole::ToolResult {
                call_id,
                name,
                is_error,
            } => {
                let content = self
                    .content
                    .iter()
                    .map(KernelContent::from_content_block)
                    .collect::<Result<Vec<_>>>()?;
                let tool_name = name
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "tool".into());
                ensure!(!call_id.is_empty(), "toolResult call_id cannot be empty");
                Ok(SessionEntryPayload::ToolResult {
                    call_id: call_id.clone(),
                    name: tool_name,
                    content,
                    is_error: *is_error,
                })
            }
            NeutralRole::Custom {
                name,
                display,
                details,
                include_in_summary,
            } => {
                let content = self
                    .content
                    .iter()
                    .map(KernelContent::from_content_block)
                    .collect::<Result<Vec<_>>>()?;
                Ok(SessionEntryPayload::CustomMessage {
                    name: name.clone(),
                    content,
                    display: *display,
                    details: details.as_ref().map(|v| CustomValue { value: v.clone() }),
                    include_in_summary: *include_in_summary,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_legacy_roundtrip_preserves_plain_and_rich_messages() {
        let legacy_rows = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "hello"}),
            json!({
                "role": "assistant",
                "content": "checking",
                "reasoning_content": "plan",
                "thinking_signature": "sig-1",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}
                }]
            }),
            json!({"role": "tool", "tool_call_id": "call-1", "content": "{\"ok\":true}"}),
        ];
        let neutral = from_legacy_values(&legacy_rows).unwrap();
        assert_eq!(to_legacy_values(&neutral), legacy_rows);

        let rich = vec![
            NeutralMessage::user_blocks(vec![
                ContentBlock::Text {
                    text: "look".into(),
                    signature: None,
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        mime_type: "image/png".into(),
                        data: "AAAA".into(),
                    },
                },
            ]),
            NeutralMessage::assistant(vec![
                ContentBlock::Thinking {
                    text: "step1".into(),
                    signature: Some("sig-a".into()),
                    encrypted_content: None,
                },
                ContentBlock::RedactedThinking {
                    data: "opaque-blob".into(),
                },
                ContentBlock::Thinking {
                    text: "step2".into(),
                    signature: Some("sig-b".into()),
                    encrypted_content: Some("enc-xyz".into()),
                },
                ContentBlock::Text {
                    text: "running tool".into(),
                    signature: Some("text-sig".into()),
                },
                ContentBlock::ToolCall(ToolCallBlock {
                    id: "c-2".into(),
                    name: "write_file".into(),
                    arguments: "{\"path\":\"b.rs\"}".into(),
                    thought_signature: Some("gem-sig".into()),
                    namespace: Some("ns".into()),
                }),
            ]),
            NeutralMessage::tool_result("c-2", Some("write_file".into()), "denied", true),
        ];

        let serialized = to_legacy_values(&rich);
        let roundtripped = from_legacy_values(&serialized).unwrap();
        assert_eq!(roundtripped, rich);
        // Tool arguments must NOT be duplicated in coda_blocks.
        let blocks = serialized[1]["coda_blocks"].as_array().unwrap();
        assert_eq!(
            blocks.last().unwrap(),
            &json!({"type": "tool_call_ref", "id": "c-2"})
        );
    }

    #[test]
    fn coda_kernel_session_entry_roundtrips_thinking_redacted_tool_and_custom() {
        let entry_json = json!({
            "type": "message",
            "id": "e-1",
            "parentId": null,
            "timestamp": "2026-11-26T12:00:00Z",
            "role": "assistant",
            "content": [
                {"type": "thinking", "text": "t1", "signature": "sig-1"},
                {"type": "thinking", "text": "redacted-payload", "redacted": true},
                {"type": "text", "text": "hi"},
                {"type": "toolCall", "callId": "call-9", "name": "read", "args": {"path": "x"}, "thoughtSignature": "tsig", "namespace": "host"}
            ]
        });
        let entry: SessionEntry = serde_json::from_value(entry_json).unwrap();
        let neutral = entry.to_neutral_message().unwrap().unwrap();
        assert_eq!(
            neutral.content,
            vec![
                ContentBlock::Thinking {
                    text: "t1".into(),
                    signature: Some("sig-1".into()),
                    encrypted_content: None,
                },
                ContentBlock::RedactedThinking {
                    data: "redacted-payload".into(),
                },
                ContentBlock::Text {
                    text: "hi".into(),
                    signature: None,
                },
                ContentBlock::ToolCall(ToolCallBlock {
                    id: "call-9".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"x\"}".into(),
                    thought_signature: Some("tsig".into()),
                    namespace: Some("host".into()),
                }),
            ]
        );
        let payload = neutral.to_session_entry_payload().unwrap();
        match payload {
            SessionEntryPayload::Message { role, content } => {
                assert_eq!(role, SessionMessageRole::Assistant);
                assert_eq!(content.len(), 4);
                assert!(matches!(
                    &content[1],
                    SessionBlock::Thinking { text, redacted: Some(true), .. } if text == "redacted-payload"
                ));
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let custom_entry_json = json!({
            "type": "customMessage",
            "id": "e-2",
            "parentId": "e-1",
            "timestamp": "2026-11-26T12:00:01Z",
            "name": "codaNote",
            "content": [{"type": "text", "text": "note"}],
            "display": true,
            "details": {"value": null},
            "includeInSummary": false
        });
        let custom_entry: SessionEntry = serde_json::from_value(custom_entry_json).unwrap();
        let custom_neutral = custom_entry.to_neutral_message().unwrap().unwrap();
        match custom_neutral.to_session_entry_payload().unwrap() {
            SessionEntryPayload::CustomMessage {
                name,
                display,
                details,
                include_in_summary,
                ..
            } => {
                assert_eq!(name, "codaNote");
                assert!(display);
                assert_eq!(details, Some(CustomValue { value: Value::Null }));
                assert_eq!(include_in_summary, Some(false));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn encode_openai_chat_messages_strips_foreign_fields_and_encodes_images() {
        let msgs = vec![
            NeutralMessage::user_blocks(vec![
                ContentBlock::Text {
                    text: "what is this?".into(),
                    signature: None,
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        mime_type: "image/jpeg".into(),
                        data: "Zm9v".into(),
                    },
                },
            ]),
            NeutralMessage::assistant(vec![
                ContentBlock::Thinking {
                    text: "ponder".into(),
                    signature: Some("anthropic-sig".into()),
                    encrypted_content: Some("responses-enc".into()),
                },
                ContentBlock::RedactedThinking {
                    data: "secret-redacted".into(),
                },
                ContentBlock::Text {
                    text: "answer".into(),
                    signature: Some("txt-sig".into()),
                },
                ContentBlock::ToolCall(ToolCallBlock {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: "{\"path\":\"a.txt\"}".into(),
                    thought_signature: Some("gemini-sig".into()),
                    namespace: Some("ns".into()),
                }),
            ]),
        ];
        let encoded = encode_openai_chat_messages(&msgs);
        let arr = encoded.as_array().unwrap();
        assert_eq!(
            arr[0],
            json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is this?"},
                    {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,Zm9v"}}
                ]
            })
        );
        let assistant = arr[1].as_object().unwrap();
        assert_eq!(assistant["content"], "answer");
        assert_eq!(assistant["reasoning_content"], "ponder");
        assert!(assistant.get("thinking_signature").is_none());
        assert!(assistant.get("coda_blocks").is_none());
        let tc = &assistant["tool_calls"][0];
        assert!(tc.get("thought_signature").is_none());
        assert!(tc.get("namespace").is_none());
    }

    #[test]
    fn responses_session_entry_roundtrip_preserves_encrypted_content_and_isolates_anthropic_turns()
    {
        use crate::provider::{ResponsesDecoder, encode_responses_body_neutral};

        // 1. Decode OpenAI Responses SSE carrying encrypted_content -> SessionEntry -> hydrate for OpenAiResponses
        let sse = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
            "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_1\",\"output_index\":0,\"delta\":\"plan\"}\n\n",
            "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"enc_wire_1\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"plan\"}]}}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":1,\"delta\":\"ok\"}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15}}}\n\n"
        );
        let mut dec = ResponsesDecoder::default();
        dec.feed_with_reasoning(sse.as_bytes(), &mut |_| Ok(()), &mut |_| Ok(()))
            .unwrap();
        let decoded = dec.finish_neutral(&mut |_| Ok(())).unwrap();
        let payload = decoded.to_session_entry_payload().unwrap();
        let (role, content) = match payload {
            SessionEntryPayload::Message { role, content } => (role, content),
            other => panic!("unexpected payload: {other:?}"),
        };
        let responses_entry = SessionEntry::Message {
            id: "e-resp-1".into(),
            parent_id: None,
            timestamp: "2026-11-26T20:00:00Z".into(),
            metadata: None,
            role,
            content,
        };

        // Protocol-agnostic conversion keeps encrypted_content: None so foreign signatures are never promoted blindly.
        let agnostic = responses_entry.to_neutral_message().unwrap().unwrap();
        assert!(matches!(
            &agnostic.content[0],
            ContentBlock::Thinking {
                text,
                signature: Some(sig),
                encrypted_content: None,
            } if text == "plan" && sig == "enc_wire_1"
        ));

        // Protocol-aware conversion for OpenAiResponses promotes signature -> encrypted_content.
        let hydrated = responses_entry
            .to_neutral_message_for_protocol(Some(ProviderProtocol::OpenAiResponses))
            .unwrap()
            .unwrap();
        assert!(matches!(
            &hydrated.content[0],
            ContentBlock::Thinking {
                text,
                signature: None,
                encrypted_content: Some(enc),
            } if text == "plan" && enc == "enc_wire_1"
        ));

        let encoded = encode_responses_body_neutral(
            "gpt-5.2",
            1024,
            &[NeutralMessage::user_text("hi"), hydrated],
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let input = encoded["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["encrypted_content"], "enc_wire_1");
        assert_eq!(input[1]["summary"][0]["text"], "plan");

        // 2. Model-switching branch: prior Anthropic Thinking.signature stays isolated while
        //    OpenAiResponses turns retain encrypted_content.
        let entries = vec![
            SessionEntry::Model {
                id: "m-ant".into(),
                parent_id: None,
                timestamp: "2026-11-26T20:00:00Z".into(),
                metadata: None,
                model: KernelModel {
                    model_id: "claude-sonnet-4-20250514".into(),
                    custom_provider_id: None,
                },
            },
            SessionEntry::Message {
                id: "e-ant-u".into(),
                parent_id: Some("m-ant".into()),
                timestamp: "2026-11-26T20:00:01Z".into(),
                metadata: None,
                role: SessionMessageRole::User,
                content: vec![SessionBlock::Text {
                    text: "first".into(),
                    signature: None,
                }],
            },
            SessionEntry::Message {
                id: "e-ant-a".into(),
                parent_id: Some("e-ant-u".into()),
                timestamp: "2026-11-26T20:00:02Z".into(),
                metadata: None,
                role: SessionMessageRole::Assistant,
                content: vec![
                    SessionBlock::Thinking {
                        text: "claude reasoning".into(),
                        signature: Some("ant-sig-secret".into()),
                        redacted: None,
                    },
                    SessionBlock::Thinking {
                        text: "opaque-redacted".into(),
                        signature: None,
                        redacted: Some(true),
                    },
                    SessionBlock::Text {
                        text: "claude reply".into(),
                        signature: None,
                    },
                ],
            },
            SessionEntry::Model {
                id: "m-resp".into(),
                parent_id: Some("e-ant-a".into()),
                timestamp: "2026-11-26T20:00:03Z".into(),
                metadata: None,
                model: KernelModel {
                    model_id: "gpt-5.2".into(),
                    custom_provider_id: None,
                },
            },
            SessionEntry::Message {
                id: "e-resp-u".into(),
                parent_id: Some("m-resp".into()),
                timestamp: "2026-11-26T20:00:04Z".into(),
                metadata: None,
                role: SessionMessageRole::User,
                content: vec![SessionBlock::Text {
                    text: "second".into(),
                    signature: None,
                }],
            },
            SessionEntry::Message {
                id: "e-resp-2".into(),
                parent_id: Some("e-resp-u".into()),
                timestamp: "2026-11-26T20:00:05Z".into(),
                metadata: None,
                role: SessionMessageRole::Assistant,
                content: match responses_entry
                    .to_neutral_message()
                    .unwrap()
                    .unwrap()
                    .to_session_entry_payload()
                    .unwrap()
                {
                    SessionEntryPayload::Message { content, .. } => content,
                    other => panic!("unexpected payload: {other:?}"),
                },
            },
        ];

        let branch_msgs =
            session_entries_to_neutral_messages(&entries, Some(ProviderProtocol::OpenAiResponses))
                .unwrap();
        assert_eq!(branch_msgs.len(), 4);
        // Anthropic turn keeps signature and has encrypted_content: None
        assert!(matches!(
            &branch_msgs[1].content[0],
            ContentBlock::Thinking {
                text,
                signature: Some(sig),
                encrypted_content: None,
            } if text == "claude reasoning" && sig == "ant-sig-secret"
        ));
        assert!(matches!(
            &branch_msgs[1].content[1],
            ContentBlock::RedactedThinking { data } if data == "opaque-redacted"
        ));
        // OpenAI Responses turn promotes signature -> encrypted_content
        assert!(matches!(
            &branch_msgs[3].content[0],
            ContentBlock::Thinking {
                text,
                signature: None,
                encrypted_content: Some(enc),
            } if text == "plan" && enc == "enc_wire_1"
        ));

        let branch_encoded =
            encode_responses_body_neutral("gpt-5.2", 1024, &branch_msgs, None, None, None, None)
                .unwrap();
        let reasoning_items: Vec<_> = branch_encoded["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "reasoning")
            .collect();
        assert_eq!(reasoning_items.len(), 1);
        assert_eq!(reasoning_items[0]["encrypted_content"], "enc_wire_1");

        // 3. Reverse switch: explicit metadata protocol on prior OpenAiResponses Model entry
        //    retains encrypted_content even when active_protocol is Anthropic.
        let mut resp_meta = Map::new();
        resp_meta.insert("protocol".into(), json!("openai-responses"));
        let reverse_entries = vec![
            SessionEntry::Model {
                id: "m-1".into(),
                parent_id: None,
                timestamp: "2026-11-26T20:01:00Z".into(),
                metadata: Some(resp_meta),
                model: KernelModel {
                    model_id: "gpt-5.2".into(),
                    custom_provider_id: None,
                },
            },
            responses_entry,
            SessionEntry::Model {
                id: "m-2".into(),
                parent_id: Some("e-resp-1".into()),
                timestamp: "2026-11-26T20:01:01Z".into(),
                metadata: None,
                model: KernelModel {
                    model_id: "claude-sonnet-4-20250514".into(),
                    custom_provider_id: None,
                },
            },
            SessionEntry::Message {
                id: "e-ant-2".into(),
                parent_id: Some("m-2".into()),
                timestamp: "2026-11-26T20:01:02Z".into(),
                metadata: None,
                role: SessionMessageRole::Assistant,
                content: vec![SessionBlock::Thinking {
                    text: "ant".into(),
                    signature: Some("ant-sig-2".into()),
                    redacted: None,
                }],
            },
        ];
        let reverse_msgs = session_entries_to_neutral_messages(
            &reverse_entries,
            Some(ProviderProtocol::Anthropic),
        )
        .unwrap();
        assert_eq!(reverse_msgs.len(), 2);
        assert!(matches!(
            &reverse_msgs[0].content[0],
            ContentBlock::Thinking {
                signature: None,
                encrypted_content: Some(enc),
                ..
            } if enc == "enc_wire_1"
        ));
        assert!(matches!(
            &reverse_msgs[1].content[0],
            ContentBlock::Thinking {
                signature: Some(sig),
                encrypted_content: None,
                ..
            } if sig == "ant-sig-2"
        ));
    }

    #[test]
    fn session_entries_isolate_pre_model_messages_and_handle_custom_protocol_inference() {
        use crate::provider::encode_responses_body_neutral;

        // 1. Pre-Model SessionEntry::Message (e.g. post-compaction slice starting before first
        //    SessionEntry::Model) must remain isolated when switching to OpenAiResponses, while
        //    post-Model messages under OpenAiResponses promote signature -> encrypted_content.
        let entries = vec![
            SessionEntry::Message {
                id: "e-pre-model".into(),
                parent_id: None,
                timestamp: "2026-11-26T20:10:00Z".into(),
                metadata: None,
                role: SessionMessageRole::Assistant,
                content: vec![
                    SessionBlock::Thinking {
                        text: "pre-model-claude".into(),
                        signature: Some("ant-sig-pre".into()),
                        redacted: None,
                    },
                    SessionBlock::Text {
                        text: "pre reply".into(),
                        signature: None,
                    },
                ],
            },
            SessionEntry::Model {
                id: "m-gpt".into(),
                parent_id: Some("e-pre-model".into()),
                timestamp: "2026-11-26T20:10:01Z".into(),
                metadata: None,
                model: KernelModel {
                    model_id: "gpt-5.2".into(),
                    custom_provider_id: None,
                },
            },
            SessionEntry::Message {
                id: "e-post-model".into(),
                parent_id: Some("m-gpt".into()),
                timestamp: "2026-11-26T20:10:02Z".into(),
                metadata: None,
                role: SessionMessageRole::Assistant,
                content: vec![
                    SessionBlock::Thinking {
                        text: "post-model-resp".into(),
                        signature: Some("enc_resp_1".into()),
                        redacted: None,
                    },
                    SessionBlock::Text {
                        text: "post reply".into(),
                        signature: None,
                    },
                ],
            },
        ];

        let msgs =
            session_entries_to_neutral_messages(&entries, Some(ProviderProtocol::OpenAiResponses))
                .unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(
            &msgs[0].content[0],
            ContentBlock::Thinking {
                text,
                signature: Some(sig),
                encrypted_content: None,
            } if text == "pre-model-claude" && sig == "ant-sig-pre"
        ));
        assert!(matches!(
            &msgs[1].content[0],
            ContentBlock::Thinking {
                text,
                signature: None,
                encrypted_content: Some(enc),
            } if text == "post-model-resp" && enc == "enc_resp_1"
        ));

        let encoded =
            encode_responses_body_neutral("gpt-5.2", 1024, &msgs, None, None, None, None).unwrap();
        let reasoning_items: Vec<_> = encoded["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "reasoning")
            .collect();
        assert_eq!(reasoning_items.len(), 1);
        assert_eq!(reasoning_items[0]["encrypted_content"], "enc_resp_1");

        // 2. Single-model branch with no SessionEntry::Model entries still uses active_protocol.
        let single_model_msgs = session_entries_to_neutral_messages(
            &entries[2..],
            Some(ProviderProtocol::OpenAiResponses),
        )
        .unwrap();
        assert_eq!(single_model_msgs.len(), 1);
        assert!(matches!(
            &single_model_msgs[0].content[0],
            ContentBlock::Thinking {
                signature: None,
                encrypted_content: Some(enc),
                ..
            } if enc == "enc_resp_1"
        ));

        // 3. infer_protocol_from_model_id handles custom:<provider>:<org>/<model> and models/gemini.
        assert_eq!(
            infer_protocol_from_model_id("custom:my-proxy:anthropic/claude-sonnet-4"),
            Some(ProviderProtocol::Anthropic)
        );
        assert_eq!(
            infer_protocol_from_model_id("models/gemini-2.5-pro"),
            Some(ProviderProtocol::Gemini)
        );
        assert_eq!(
            infer_protocol_from_model_id("custom:my-proxy:models/gemini-2.5-pro"),
            Some(ProviderProtocol::Gemini)
        );
        assert_eq!(
            infer_protocol_from_model_id("custom:my-proxy:openai/gpt-5.2"),
            None
        );

        // 4. protocol_from_metadata falls back to "provider" when "protocol" is unrecognized.
        let mut fallback_meta = Map::new();
        fallback_meta.insert("protocol".into(), json!("custom-gw"));
        fallback_meta.insert("provider".into(), json!("openai-responses"));
        assert_eq!(
            protocol_from_metadata(&fallback_meta),
            Some(ProviderProtocol::OpenAiResponses)
        );

        let fallback_branch = vec![SessionEntry::Message {
            id: "e-meta-fallback".into(),
            parent_id: None,
            timestamp: "2026-11-26T20:10:03Z".into(),
            metadata: Some(fallback_meta),
            role: SessionMessageRole::Assistant,
            content: vec![SessionBlock::Thinking {
                text: "meta".into(),
                signature: Some("enc_fallback".into()),
                redacted: None,
            }],
        }];
        let fallback_msgs = session_entries_to_neutral_messages(&fallback_branch, None).unwrap();
        assert!(matches!(
            &fallback_msgs[0].content[0],
            ContentBlock::Thinking {
                signature: None,
                encrypted_content: Some(enc),
                ..
            } if enc == "enc_fallback"
        ));
    }

    #[test]
    fn media_blocks_round_trip_and_validate_paths_in_lockstep() {
        let audio = KernelContent::Audio {
            path: "media/clip.mp3".into(),
            mime_type: "audio/mpeg".into(),
            bytes: Some(2048),
            duration_ms: Some(1500),
            sha256: Some("abc123".into()),
            name: Some("clip.mp3".into()),
        };
        let video = KernelContent::Video {
            path: "media/demo.mp4".into(),
            mime_type: "video/mp4".into(),
            bytes: Some(4096),
            duration_ms: Some(5000),
            sha256: None,
            name: None,
        };
        let resource = KernelContent::Resource {
            path: "docs/spec.pdf".into(),
            mime_type: Some("application/pdf".into()),
            bytes: Some(512),
            sha256: None,
            name: Some("spec.pdf".into()),
        };

        let audio_json = serde_json::to_value(&audio).unwrap();
        assert_eq!(audio_json["type"], "audio");
        assert_eq!(audio_json["path"], "media/clip.mp3");
        assert_eq!(audio_json["mimeType"], "audio/mpeg");
        assert_eq!(audio_json["bytes"], 2048);
        assert_eq!(audio_json["durationMs"], 1500);
        assert_eq!(audio_json["sha256"], "abc123");
        assert_eq!(audio_json["name"], "clip.mp3");

        let video_json = serde_json::to_value(&video).unwrap();
        assert_eq!(video_json["type"], "video");
        assert!(video_json.get("sha256").is_none());

        let res_json = serde_json::to_value(&resource).unwrap();
        assert_eq!(res_json["type"], "resource");
        assert_eq!(res_json["path"], "docs/spec.pdf");

        // Unknown fields rejected by deny_unknown_fields.
        assert!(
            serde_json::from_value::<KernelContent>(json!({
                "type": "audio",
                "path": "a.mp3",
                "mimeType": "audio/mpeg",
                "extra": 1
            }))
            .is_err()
        );

        // 1.0 rejects media blocks; 1.1 accepts valid workspace-relative paths.
        assert!(audio.validate_for_embedded(false).is_err());
        assert!(video.validate_for_embedded(false).is_err());
        assert!(resource.validate_for_embedded(false).is_err());
        assert!(audio.validate_for_embedded(true).is_ok());
        assert!(video.validate_for_embedded(true).is_ok());
        assert!(resource.validate_for_embedded(true).is_ok());

        let max_len_ok = "a".repeat(4096);
        assert!(
            KernelContent::Resource {
                path: max_len_ok,
                mime_type: None,
                bytes: None,
                sha256: None,
                name: None,
            }
            .validate_for_embedded(true)
            .is_ok()
        );

        let too_long = "a".repeat(4097);
        let too_long_with_ext = format!("{}.mp3", "a".repeat(4097));
        for bad_path in [
            "",
            "   ",
            "/etc/passwd",
            "\\windows\\system32",
            "C:\\audio.mp3",
            "../secret.mp3",
            "media/../secret.mp3",
            "bad\npath.mp3",
            too_long.as_str(),
            too_long_with_ext.as_str(),
        ] {
            let bad = KernelContent::Resource {
                path: bad_path.into(),
                mime_type: None,
                bytes: None,
                sha256: None,
                name: None,
            };
            assert!(
                bad.validate_for_embedded(true).is_err(),
                "expected error for path {bad_path:?}"
            );
        }

        // Round-trip through ContentBlock <-> SessionBlock <-> KernelContent.
        for kc in [&audio, &video, &resource] {
            let cb = kc.to_content_block();
            let sb = SessionBlock::from_content_block(&cb).unwrap();
            assert_eq!(sb, kc.to_session_block());
            assert_eq!(sb.to_content_block(), cb);
            assert_eq!(KernelContent::from_content_block(&cb).unwrap(), *kc);
        }

        // encode_openai_chat_messages preserves text-only 1.0 user message and formats media placeholders cleanly.
        let plain_msg = NeutralMessage::user_text("hello");
        let encoded_plain = encode_openai_chat_messages(&[plain_msg]);
        assert_eq!(encoded_plain[0]["content"], "hello");

        let media_msg = NeutralMessage::user_blocks(vec![
            ContentBlock::Text {
                text: "listen".into(),
                signature: None,
            },
            audio.to_content_block(),
        ]);
        let encoded_media = encode_openai_chat_messages(&[media_msg]);
        let content_str = encoded_media[0]["content"].as_str().unwrap();
        assert!(content_str.contains("listen"));
        assert!(content_str.contains("[audio: media/clip.mp3"));
    }
}
