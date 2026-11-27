//! Lightweight builtin completion provider runtime without PluginHost ceremony.
use crate::{
    domain::Route,
    messages::{ContentBlock, NeutralMessage, NeutralRole, encode_openai_chat_messages},
    provider_transport::{
        NetworkPurpose, ProviderClient, ProviderPermit, ProviderProtocol, TransportError,
        read_provider_error,
    },
    redaction::CredentialSet,
};
use anyhow::Result;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub const PROVIDER_ID: &str = "builtin.provider-xpeach";

pub struct CompletionInput<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub messages: &'a [NeutralMessage],
    pub tools: &'a [Value],
}

#[derive(Debug)]
pub enum CodecError {
    #[allow(dead_code)]
    Invalid,
    #[allow(dead_code)]
    Limit,
    Failed,
}

pub type CompletionOutput = (NeutralMessage, Value);
pub type Emit<'a> = dyn FnMut(&str) -> anyhow::Result<()> + Send + 'a;

pub trait CompletionCodec: Send + Sync {
    fn encode(&self, input: &CompletionInput<'_>) -> Result<Value, CodecError>;
    fn encode_protocol(
        &self,
        _protocol: ProviderProtocol,
        body: &Value,
    ) -> Result<Value, CodecError> {
        Ok(body.clone())
    }
    fn encode_neutral_protocol(
        &self,
        protocol: ProviderProtocol,
        input: &CompletionInput<'_>,
    ) -> Result<Value, CodecError> {
        let canonical = self.encode(input)?;
        self.encode_protocol(protocol, &canonical)
    }
    fn encode_neutral_protocol_with_thinking(
        &self,
        protocol: ProviderProtocol,
        input: &CompletionInput<'_>,
        _thinking_level: Option<&str>,
    ) -> Result<Value, CodecError> {
        self.encode_neutral_protocol(protocol, input)
    }
    fn decoder(&self) -> Result<Box<dyn CompletionDecoder>, CodecError>;
    fn decoder_for(
        &self,
        _protocol: ProviderProtocol,
    ) -> Result<Box<dyn CompletionDecoder>, CodecError> {
        self.decoder()
    }
}

pub trait CompletionDecoder: Send {
    fn set_allow_non_object_tool_args(&mut self, _allow: bool) {}
    fn set_allow_empty_text(&mut self, _allow: bool) {}
    fn response_id(&self) -> Option<&str> {
        None
    }
    fn feed(&mut self, bytes: &[u8], emit: &mut Emit<'_>) -> Result<(), CodecError>;
    fn feed_with_reasoning(
        &mut self,
        bytes: &[u8],
        emit: &mut Emit<'_>,
        _emit_reasoning: &mut Emit<'_>,
    ) -> Result<(), CodecError> {
        self.feed(bytes, emit)
    }
    fn is_done(&self) -> bool;
    fn finish(self: Box<Self>, emit: &mut Emit<'_>) -> Result<CompletionOutput, CodecError>;
    fn finish_with_breakdown(
        self: Box<Self>,
        emit: &mut Emit<'_>,
    ) -> Result<
        (
            CompletionOutput,
            Option<crate::runtime_seams::KernelUsageBreakdown>,
        ),
        CodecError,
    > {
        Ok((self.finish(emit)?, None))
    }
}

type Codec = Arc<dyn CompletionCodec>;

pub struct ProviderRuntime {
    enabled: AtomicBool,
    in_flight: Arc<AtomicU64>,
    generation: AtomicU64,
    codec: Codec,
    version: String,
}

pub struct PreparedCompletion {
    generation: u64,
    desired_revision: Option<u64>,
    body: Value,
    decoder: Option<Box<dyn CompletionDecoder>>,
    model: String,
    max_tokens: u32,
    messages: Vec<NeutralMessage>,
    tools: Vec<Value>,
}

pub struct CompletionLease {
    streamed_content_only: bool,
    body: Value,
    decoder: Option<Box<dyn CompletionDecoder>>,
    in_flight: Arc<AtomicU64>,
    codec: Codec,
    model: String,
    max_tokens: u32,
    messages: Vec<NeutralMessage>,
    tools: Vec<Value>,
    thinking_level: Option<String>,
    allow_empty_text: bool,
    prompt_cache_key: Option<String>,
}

impl Drop for CompletionLease {
    fn drop(&mut self) {
        let _ = self.decoder.take();
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug)]
pub struct ProviderRuntimeStatus {
    pub admission_open: bool,
    pub in_flight: u64,
    pub effects: usize,
    pub recovery: usize,
    pub active: bool,
    pub generation: u64,
    pub transitioning: bool,
    pub lifecycle_failed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderRuntimeError {
    Unavailable,
    CodecFailed,
    InvalidPreparedState,
}

impl ProviderRuntimeError {
    pub fn unavailable() -> Self {
        Self::Unavailable
    }

    pub fn codec_failed() -> Self {
        Self::CodecFailed
    }

    pub fn invalid_prepared_state() -> Self {
        Self::InvalidPreparedState
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "provider_runtime_unavailable",
            Self::CodecFailed => "provider_codec_failed",
            Self::InvalidPreparedState => "provider_prepared_state_invalid",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::Unavailable => "Provider runtime unavailable",
            Self::CodecFailed => "Provider completion codec failed",
            Self::InvalidPreparedState => "Prepared completion state invalid",
        }
    }
}

impl std::fmt::Display for ProviderRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ProviderRuntimeError {}

pub fn expected_body(input: &CompletionInput<'_>) -> Value {
    let mut body = json!({
        "model": input.model,
        "messages": encode_openai_chat_messages(input.messages),
        "max_tokens": input.max_tokens,
        "stream": true,
        "stream_options": { "include_usage": true }
    });
    if !input.tools.is_empty() {
        body["tools"] = json!(input.tools);
        body["tool_choice"] = json!("auto");
    }
    body
}

impl ProviderRuntime {
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn builtin_with_state(enabled: bool) -> Result<Self> {
        Self::with_codec_and_state(Arc::new(crate::provider::BuiltinCompletionCodec), enabled)
    }

    #[cfg(test)]
    pub(crate) fn with_codec_for_test(codec: Codec) -> Result<Self> {
        Self::with_codec_and_state(codec, true)
    }

    fn with_codec_and_state(codec: Codec, enabled: bool) -> Result<Self> {
        Ok(Self {
            enabled: AtomicBool::new(enabled),
            in_flight: Arc::new(AtomicU64::new(0)),
            generation: AtomicU64::new(1),
            codec,
            version: "1.0.0".into(),
        })
    }

    pub fn prepare(
        &self,
        input: &CompletionInput<'_>,
    ) -> Result<PreparedCompletion, ProviderRuntimeError> {
        if !self.enabled.load(Ordering::SeqCst) {
            return Err(ProviderRuntimeError::unavailable());
        }
        let generation = self.generation.load(Ordering::SeqCst);
        let body = self
            .codec
            .encode(input)
            .map_err(|_| ProviderRuntimeError::codec_failed())?;
        let decoder = self
            .codec
            .decoder()
            .map_err(|_| ProviderRuntimeError::codec_failed())?;
        Ok(PreparedCompletion {
            generation,
            desired_revision: None,
            body,
            decoder: Some(decoder),
            model: input.model.to_owned(),
            max_tokens: input.max_tokens,
            messages: input.messages.to_vec(),
            tools: input.tools.to_vec(),
        })
    }

    pub fn set_desired_revision(prepared: &mut PreparedCompletion, revision: u64) {
        prepared.desired_revision = Some(revision);
    }

    pub fn acquire_persisted<A, E>(
        &self,
        prepared: PreparedCompletion,
        admit: impl FnOnce(u64) -> Result<A, E>,
    ) -> Result<(CompletionLease, A), E>
    where
        E: From<ProviderRuntimeError>,
    {
        let Some(revision) = prepared.desired_revision else {
            return Err(ProviderRuntimeError::invalid_prepared_state().into());
        };
        self.acquire(prepared, || admit(revision))
    }

    pub fn acquire<A, E>(
        &self,
        mut prepared: PreparedCompletion,
        admit: impl FnOnce() -> Result<A, E>,
    ) -> Result<(CompletionLease, A), E>
    where
        E: From<ProviderRuntimeError>,
    {
        if !self.enabled.load(Ordering::SeqCst)
            || self.generation.load(Ordering::SeqCst) != prepared.generation
            || self.in_flight.load(Ordering::SeqCst) == u64::MAX
        {
            return Err(ProviderRuntimeError::unavailable().into());
        }
        let admitted = admit()?;
        let body = std::mem::take(&mut prepared.body);
        let decoder = prepared
            .decoder
            .take()
            .ok_or_else(|| E::from(ProviderRuntimeError::invalid_prepared_state()))?;
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        Ok((
            CompletionLease {
                streamed_content_only: false,
                body,
                decoder: Some(decoder),
                in_flight: self.in_flight.clone(),
                codec: self.codec.clone(),
                model: prepared.model,
                max_tokens: prepared.max_tokens,
                messages: prepared.messages,
                tools: prepared.tools,
                thinking_level: None,
                allow_empty_text: false,
                prompt_cache_key: None,
            },
            admitted,
        ))
    }

    pub fn acquire_unpersisted(
        &self,
        prepared: PreparedCompletion,
    ) -> Result<CompletionLease, ProviderRuntimeError> {
        let (lease, ()) = self.acquire(prepared, || Ok::<(), ProviderRuntimeError>(()))?;
        Ok(lease)
    }

    pub fn disable(&self) -> Result<()> {
        if self.enabled.swap(false, Ordering::SeqCst) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn generation_value(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn apply_desired_enabled(&self, enabled: bool) {
        if enabled {
            if !self.enabled.swap(true, Ordering::SeqCst) {
                self.generation.fetch_add(1, Ordering::SeqCst);
            }
        } else if self.enabled.swap(false, Ordering::SeqCst) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn status(&self) -> Result<ProviderRuntimeStatus> {
        let active = self.enabled.load(Ordering::SeqCst);
        Ok(ProviderRuntimeStatus {
            admission_open: active,
            in_flight: self.in_flight.load(Ordering::SeqCst),
            effects: usize::from(active),
            recovery: 0,
            active,
            generation: self.generation.load(Ordering::SeqCst),
            transitioning: false,
            lifecycle_failed: false,
        })
    }
}

pub fn normalize_openai_prompt_cache_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= 64 {
        return Some(trimmed.to_owned());
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(trimmed.as_bytes());
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    Some(hex)
}

impl CompletionLease {
    pub fn with_thinking_level(mut self, thinking_level: Option<String>) -> Self {
        self.thinking_level = thinking_level;
        self
    }

    pub fn with_allow_empty_text(mut self, allow: bool) -> Self {
        self.allow_empty_text = allow;
        self
    }

    pub fn with_prompt_cache_key(mut self, key: Option<String>) -> Self {
        self.prompt_cache_key = key;
        self
    }

    /// Summarizes in isolation. Only the final response is returned to the exploration caller.
    pub async fn execute_exploration(
        mut self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
    ) -> Result<CompletionOutput, TransportError> {
        self.streamed_content_only = true;
        let scrub = permit.credential_scrubber();
        for msg in &mut self.messages {
            if matches!(msg.role, NeutralRole::User) {
                for block in &mut msg.content {
                    if let ContentBlock::Text { text, .. } = block {
                        *text = scrub(text);
                    }
                }
            }
        }
        if let Some(messages) = self.body.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages {
                if message["role"] == "user"
                    && let Some(Value::String(text)) = message.get_mut("content")
                {
                    *text = scrub(text);
                }
            }
        }
        let (mut message, usage) = self.execute(client, permit, route, &mut |_| Ok(())).await?;
        message.content.retain(|block| {
            !matches!(
                block,
                ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
            )
        });
        for block in &mut message.content {
            if let ContentBlock::Text { text, .. } = block {
                *text = scrub(text);
            }
        }
        Ok((message, usage))
    }

    pub async fn execute(
        self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
        emit: &mut Emit<'_>,
    ) -> Result<CompletionOutput, TransportError> {
        self.execute_with_reasoning(client, permit, route, emit, &mut |_| Ok(()))
            .await
    }

    pub async fn execute_with_reasoning(
        self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
        emit: &mut (impl FnMut(&str) -> anyhow::Result<()> + Send + ?Sized),
        emit_reasoning: &mut (impl FnMut(&str) -> anyhow::Result<()> + Send + ?Sized),
    ) -> Result<CompletionOutput, TransportError> {
        let (output, _, _) = self
            .execute_with_reasoning_detailed(client, permit, route, emit, emit_reasoning)
            .await?;
        Ok(output)
    }

    pub async fn execute_with_reasoning_detailed(
        mut self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
        emit: &mut (impl FnMut(&str) -> anyhow::Result<()> + Send + ?Sized),
        emit_reasoning: &mut (impl FnMut(&str) -> anyhow::Result<()> + Send + ?Sized),
    ) -> Result<
        (
            CompletionOutput,
            Option<crate::runtime_seams::KernelUsageBreakdown>,
            Option<String>,
        ),
        TransportError,
    > {
        let keys = permit.credentials();
        let scrub = permit.credential_scrubber();
        let mut redactor = keys.stream();
        let mut reasoning_redactor = keys.stream();
        let protocol = permit.protocol();
        permit.matches_with_protocol(
            &route.base_url,
            NetworkPurpose::Completion,
            Some(&route.model),
            None,
            route.protocol,
        )?;
        let input = CompletionInput {
            model: &self.model,
            max_tokens: self.max_tokens,
            messages: &self.messages,
            tools: &self.tools,
        };
        let mut wire_body = if protocol == ProviderProtocol::OpenAi {
            let expected = expected_body(&input);
            if self.body == expected {
                self.codec
                    .encode_neutral_protocol_with_thinking(
                        protocol,
                        &input,
                        self.thinking_level.as_deref(),
                    )
                    .map_err(|_| TransportError::Invalid)?
            } else {
                // Custom test codecs may return a bespoke body from `encode`.
                self.codec
                    .encode_protocol(protocol, &self.body)
                    .map_err(|_| TransportError::Invalid)?
            }
        } else {
            self.codec
                .encode_neutral_protocol_with_thinking(
                    protocol,
                    &input,
                    self.thinking_level.as_deref(),
                )
                .map_err(|_| TransportError::Invalid)?
        };
        if protocol == ProviderProtocol::OpenAiResponses
            && let Some(key) = self
                .prompt_cache_key
                .as_deref()
                .and_then(normalize_openai_prompt_cache_key)
            && let Some(obj) = wire_body.as_object_mut()
            && obj
                .get("prompt_cache_key")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        {
            obj.insert("prompt_cache_key".to_owned(), Value::String(key));
        }
        let is_embedded = permit.is_embedded_proxy();
        let response = client.send(permit, Some(&wire_body)).await?;
        let response_status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(read_provider_error(response, &scrub).await);
        }
        let default_decoder = self.decoder.take().ok_or(TransportError::Unknown)?;
        let mut decoder = if protocol == ProviderProtocol::OpenAi {
            default_decoder
        } else {
            self.codec
                .decoder_for(protocol)
                .map_err(|_| TransportError::Unknown)?
        };
        decoder.set_allow_non_object_tool_args(is_embedded);
        decoder.set_allow_empty_text(self.allow_empty_text);
        let mut bytes = response.bytes_stream();
        let mut emitted = false;
        let mut streamed_text = String::new();
        let emission_failed = AtomicBool::new(false);
        let reasoning_emission_failed = AtomicBool::new(false);

        let mut safe_emit = |text: &str| -> anyhow::Result<()> {
            emitted = true;
            anyhow::ensure!(
                !emission_failed.load(Ordering::Relaxed),
                "completion output unavailable"
            );
            let safe = redactor.push(text);
            if !safe.is_empty() && emit(&safe).is_err() {
                emission_failed.store(true, Ordering::Relaxed);
                anyhow::bail!("completion output unavailable");
            }
            streamed_text.push_str(&safe);
            Ok(())
        };
        let mut safe_reasoning_emit = |raw: &str| -> anyhow::Result<()> {
            anyhow::ensure!(
                !reasoning_emission_failed.load(Ordering::Relaxed),
                "completion reasoning unavailable"
            );
            let safe = reasoning_redactor.push(raw);
            if !safe.is_empty() && emit_reasoning(&safe).is_err() {
                reasoning_emission_failed.store(true, Ordering::Relaxed);
                anyhow::bail!("completion reasoning unavailable");
            }
            Ok(())
        };

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|_| TransportError::Unknown)?;
            if !decoder.is_done() {
                decoder
                    .feed_with_reasoning(&chunk, &mut safe_emit, &mut safe_reasoning_emit)
                    .map_err(|_| {
                        if emission_failed.load(Ordering::Relaxed)
                            || reasoning_emission_failed.load(Ordering::Relaxed)
                        {
                            TransportError::Unknown
                        } else {
                            TransportError::InvalidResponse {
                                status: response_status,
                            }
                        }
                    })?;
            }
        }
        // Native decoders emit real text during feed and may promote reasoning during finish.
        // Always finish for validation and usage, but exploration consumes only wire text deltas.
        let response_id = decoder.response_id().map(str::to_owned);
        let (mut output, breakdown) = if self.streamed_content_only {
            decoder.finish_with_breakdown(&mut |_| Ok(()))
        } else {
            decoder.finish_with_breakdown(&mut safe_emit)
        }
        .map_err(|err| {
            if emission_failed.load(Ordering::Relaxed)
                || reasoning_emission_failed.load(Ordering::Relaxed)
            {
                TransportError::Unknown
            } else {
                match err {
                    CodecError::Invalid => TransportError::InvalidResponse {
                        status: response_status,
                    },
                    CodecError::Limit | CodecError::Failed => TransportError::Unknown,
                }
            }
        })?;
        if emission_failed.load(Ordering::Relaxed)
            || reasoning_emission_failed.load(Ordering::Relaxed)
        {
            return Err(TransportError::Unknown);
        }
        let reasoning_tail = reasoning_redactor.finish();
        if !reasoning_tail.is_empty() {
            emit_reasoning(&reasoning_tail).map_err(|_| TransportError::Unknown)?;
        }
        if !self.streamed_content_only && !emitted {
            let content = output.0.text_content();
            if !content.is_empty() {
                let safe = redactor.push(&content);
                if !safe.is_empty() {
                    emit(&safe).map_err(|_| TransportError::Unknown)?;
                }
                streamed_text.push_str(&safe);
            }
        }
        let tail = redactor.finish();
        if !tail.is_empty() {
            emit(&tail).map_err(|_| TransportError::Unknown)?;
            streamed_text.push_str(&tail);
        }
        apply_streamed_text_and_scrub(&mut output.0, streamed_text, &keys);
        Ok((output, breakdown, response_id))
    }
}

fn apply_streamed_text_and_scrub(
    message: &mut NeutralMessage,
    streamed_text: String,
    keys: &CredentialSet,
) {
    let mut text_blocks = message
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::Text { .. }))
        .count();
    if text_blocks == 0 {
        if !streamed_text.is_empty() || message.tool_calls().next().is_none() {
            let insert_pos = message
                .content
                .iter()
                .position(|b| matches!(b, ContentBlock::ToolCall(_)))
                .unwrap_or(message.content.len());
            message.content.insert(
                insert_pos,
                ContentBlock::Text {
                    text: streamed_text,
                    signature: None,
                },
            );
        }
    } else if text_blocks == 1 {
        for block in &mut message.content {
            if let ContentBlock::Text { text, signature } = block {
                if *text != streamed_text {
                    *signature = None;
                }
                *text = streamed_text;
                break;
            }
        }
    } else {
        for block in &mut message.content {
            if let ContentBlock::Text { text, signature } = block {
                let scrubbed = keys.scrub(text);
                if scrubbed != *text {
                    *signature = None;
                }
                *text = scrubbed;
            }
        }
    }
    let _ = &mut text_blocks;

    message.content.retain_mut(|block| match block {
        ContentBlock::Thinking {
            text,
            signature,
            encrypted_content,
        } => {
            let safe_text = keys.scrub(text);
            let text_changed = safe_text != *text;
            *text = safe_text;
            let sig_changed = signature.as_deref().is_some_and(|s| keys.scrub(s) != s);
            let enc_changed = encrypted_content
                .as_deref()
                .is_some_and(|s| keys.scrub(s) != s);
            if text_changed || sig_changed || enc_changed {
                *signature = None;
                *encrypted_content = None;
            }
            true
        }
        ContentBlock::RedactedThinking { data } => keys.scrub(data) == *data,
        ContentBlock::ToolCall(tc) => {
            tc.id = keys.scrub(&tc.id);
            tc.name = keys.scrub(&tc.name);
            tc.arguments = keys.scrub(&tc.arguments);
            if tc
                .thought_signature
                .as_deref()
                .is_some_and(|s| keys.scrub(s) != s)
            {
                tc.thought_signature = None;
            }
            true
        }
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_credentials_preserve_only_reusable_thinking_signatures() {
        let keys = crate::redaction::CredentialSet::new(vec!["backup-token".into()]);
        for (thinking, signature, expected) in [
            ("business text", "opaque", Some("opaque")),
            ("key=backup-token", "opaque", None),
            ("business text", "backup-token", None),
        ] {
            let mut message = NeutralMessage::assistant(vec![ContentBlock::Thinking {
                text: thinking.into(),
                signature: Some(signature.into()),
                encrypted_content: None,
            }]);
            apply_streamed_text_and_scrub(&mut message, String::new(), &keys);
            let actual_sig = match &message.content[0] {
                ContentBlock::Thinking { signature, .. } => signature.as_deref(),
                _ => None,
            };
            assert_eq!(actual_sig, expected);
            assert_eq!(message.reasoning_text(), keys.scrub(thinking));
        }
    }

    #[tokio::test]
    async fn exploration_uses_wire_text_even_when_reasoning_matches() {
        use crate::provider_transport::validate_target;
        use axum::{Json, Router, routing::post};

        let app = Router::new().route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let mut delta = json!({"reasoning_content":"42"});
                match body["model"].as_str().unwrap() {
                    "same" => delta["content"] = json!("42"),
                    "blank" => delta["content"] = json!("  "),
                    _ => {}
                }
                let frame = json!({"choices":[{"delta":delta,"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}});
                format!("data: {frame}\n\ndata: [DONE]\n\n")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let runtime = ProviderRuntime::builtin_with_state(true).unwrap();
        let client = ProviderClient::new().unwrap();
        for (model, expected) in [
            ("same", "42"),
            ("only", ""),
            ("blank", "  "),
            ("main", "42"),
        ] {
            let target = validate_target(&base, NetworkPurpose::Completion).unwrap();
            let permit = ProviderPermit::new(
                target,
                "key".into(),
                "call".into(),
                Some(model.into()),
                None,
            )
            .unwrap();
            let route = Route {
                id: "mock".into(),
                name: "mock".into(),
                base_url: base.clone(),
                model: model.into(),
                protocol: None,
                max_tokens: 64,
                parallel_limit: 1,
                key_env: None,
            };
            let messages = [NeutralMessage::user_text("test")];
            let input = CompletionInput {
                model,
                max_tokens: 64,
                messages: &messages,
                tools: &[],
            };
            let prepared = runtime.prepare(&input).unwrap();
            let lease = runtime.acquire_unpersisted(prepared).unwrap();
            let (message, usage) = if model == "main" {
                lease
                    .execute(&client, permit, &route, &mut |_| Ok(()))
                    .await
            } else {
                lease.execute_exploration(&client, permit, &route).await
            }
            .unwrap();
            assert_eq!(message.text_content(), expected, "{model}");
            if model != "main" {
                assert!(message.reasoning_text().is_empty());
            }
            assert_eq!(usage["total_tokens"], 3);
        }
        server.abort();
    }

    struct CustomTempCodec;
    impl CompletionCodec for CustomTempCodec {
        fn encode(&self, input: &CompletionInput<'_>) -> Result<Value, CodecError> {
            let mut body = expected_body(input);
            body["temperature"] = json!(0.2);
            body["reasoning_effort"] = json!("high");
            Ok(body)
        }
        fn decoder(&self) -> Result<Box<dyn CompletionDecoder>, CodecError> {
            Ok(Box::new(crate::provider::Decoder::default()))
        }
    }

    struct LegacyOnlyDecoder {
        buf: String,
    }

    impl CompletionDecoder for LegacyOnlyDecoder {
        fn feed(&mut self, bytes: &[u8], emit: &mut Emit<'_>) -> Result<(), CodecError> {
            let text = std::str::from_utf8(bytes).map_err(|_| CodecError::Failed)?;
            self.buf.push_str(text);
            emit(text).map_err(|_| CodecError::Failed)
        }

        fn is_done(&self) -> bool {
            false
        }

        fn finish(self: Box<Self>, _emit: &mut Emit<'_>) -> Result<CompletionOutput, CodecError> {
            Ok((NeutralMessage::assistant_text(self.buf), json!({})))
        }
    }

    #[test]
    fn custom_request_fields_are_not_blocked_by_prepare() {
        let runtime = ProviderRuntime::with_codec_for_test(Arc::new(CustomTempCodec)).unwrap();
        let messages = [NeutralMessage::user_text("hi")];
        let prepared = runtime
            .prepare(&CompletionInput {
                model: "gpt-test",
                max_tokens: 256,
                messages: &messages,
                tools: &[],
            })
            .unwrap();
        assert_eq!(prepared.body["temperature"], json!(0.2));
        assert_eq!(prepared.body["reasoning_effort"], json!("high"));
    }

    #[test]
    fn default_feed_with_reasoning_delegates_to_feed() {
        let mut decoder: Box<dyn CompletionDecoder> =
            Box::new(LegacyOnlyDecoder { buf: String::new() });
        let mut out = String::new();
        let mut reasoning = String::new();
        decoder
            .feed_with_reasoning(
                b"hello",
                &mut |s| {
                    out.push_str(s);
                    Ok(())
                },
                &mut |r| {
                    reasoning.push_str(r);
                    Ok(())
                },
            )
            .unwrap();
        let (msg, _) = decoder.finish(&mut |_| Ok(())).unwrap();
        assert_eq!(out, "hello");
        assert!(reasoning.is_empty());
        assert_eq!(msg.text_content(), "hello");
    }

    #[test]
    fn apply_streamed_text_and_scrub_clears_tampered_signatures_and_drops_tainted_redacted_thinking()
     {
        let keys = crate::redaction::CredentialSet::new(vec!["secret-tok".into()]);
        let mut msg = NeutralMessage::assistant(vec![
            ContentBlock::Thinking {
                text: "clean summary".into(),
                signature: Some("rs_1".into()),
                encrypted_content: Some("enc_secret-tok_payload".into()),
            },
            ContentBlock::RedactedThinking {
                data: "contains-secret-tok".into(),
            },
            ContentBlock::RedactedThinking {
                data: "clean-opaque".into(),
            },
            ContentBlock::Text {
                text: "ok".into(),
                signature: Some("tsig".into()),
            },
        ]);
        apply_streamed_text_and_scrub(&mut msg, "edited".into(), &keys);
        assert!(matches!(
            &msg.content[0],
            ContentBlock::Thinking {
                signature: None,
                encrypted_content: None,
                ..
            }
        ));
        assert!(matches!(
            &msg.content[1],
            ContentBlock::RedactedThinking { data } if data == "clean-opaque"
        ));
        assert!(matches!(
            &msg.content[2],
            ContentBlock::Text { text, signature: None } if text == "edited"
        ));
    }

    #[tokio::test]
    async fn streaming_sse_exceeding_response_limit_succeeds_for_completion_lease() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let total_chunks = 120usize;
        let chunk_text = "x".repeat(40_000); // 4.8 MB of content (> 4 MiB RESPONSE_LIMIT)
        let chunk_for_server = chunk_text.clone();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req_buf = vec![0u8; 8192];
            let _ = sock.read(&mut req_buf).await.unwrap();
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
            let event = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{chunk_for_server}\"}}}}]}}\n\n"
            );
            let frame = format!("{:x}\r\n{event}\r\n", event.len());
            for _ in 0..total_chunks {
                sock.write_all(frame.as_bytes()).await.unwrap();
            }
            let done = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":5,\"total_tokens\":10}}\n\ndata: [DONE]\n\n";
            let done_frame = format!("{:x}\r\n{done}\r\n0\r\n\r\n", done.len());
            sock.write_all(done_frame.as_bytes()).await.unwrap();
        });

        let runtime = ProviderRuntime::builtin_with_state(true).unwrap();
        let client = ProviderClient::new().unwrap();
        let base = format!("http://{addr}/v1");
        let route = Route {
            id: "r".into(),
            name: "r".into(),
            base_url: base.clone(),
            model: "big-stream".into(),
            protocol: Some(ProviderProtocol::OpenAi),
            max_tokens: 128,
            parallel_limit: 1,
            key_env: None,
        };
        let target = crate::provider_transport::validate_target_with_protocol(
            &base,
            NetworkPurpose::Completion,
            route.protocol,
        )
        .unwrap();
        let permit = ProviderPermit::new(
            target,
            "k".into(),
            "c".into(),
            Some("big-stream".into()),
            None,
        )
        .unwrap();
        let messages = [NeutralMessage::user_text("stream big")];
        let input = CompletionInput {
            model: &route.model,
            max_tokens: route.max_tokens,
            messages: &messages,
            tools: &[],
        };
        let prepared = runtime.prepare(&input).unwrap();
        let lease = runtime.acquire_unpersisted(prepared).unwrap();
        let mut streamed_len = 0usize;
        let (msg, usage) = lease
            .execute_with_reasoning(
                &client,
                permit,
                &route,
                &mut |s| {
                    streamed_len += s.len();
                    Ok(())
                },
                &mut |_| Ok(()),
            )
            .await
            .unwrap();
        assert!(streamed_len > crate::provider_transport::RESPONSE_LIMIT);
        assert_eq!(msg.text_content().len(), total_chunks * chunk_text.len());
        assert_eq!(usage["total_tokens"], 10);
        server.abort();
    }

    #[tokio::test]
    async fn non_retryable_401_and_409_provider_errors_never_retry_and_scrub_secrets() {
        use std::sync::atomic::AtomicUsize;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let secret = "sk-live-secret-value-999";
        for (status, code) in [(401u16, "invalid_api_key"), (409u16, "state_conflict")] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let hits = Arc::new(AtomicUsize::new(0));
            let hits_srv = hits.clone();
            let secret_srv = secret.to_owned();
            let code_srv = code.to_owned();
            let server = tokio::spawn(async move {
                while let Ok((mut sock, _)) = listener.accept().await {
                    hits_srv.fetch_add(1, Ordering::SeqCst);
                    let mut req_buf = vec![0u8; 4096];
                    let _ = sock.read(&mut req_buf).await.unwrap();
                    let body = json!({
                        "error": {
                            "code": code_srv,
                            "message": format!("upstream failed for {secret_srv}")
                        }
                    })
                    .to_string();
                    let resp = format!(
                        "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                }
            });

            let runtime = ProviderRuntime::builtin_with_state(true).unwrap();
            let client = ProviderClient::new().unwrap();
            let base = format!("http://{addr}/v1");
            let route = Route {
                id: "r".into(),
                name: "r".into(),
                base_url: base.clone(),
                model: "m".into(),
                protocol: Some(ProviderProtocol::OpenAi),
                max_tokens: 64,
                parallel_limit: 1,
                key_env: None,
            };
            let target = crate::provider_transport::validate_target_with_protocol(
                &base,
                NetworkPurpose::Completion,
                route.protocol,
            )
            .unwrap();
            let permit =
                ProviderPermit::new(target, secret.into(), "c".into(), Some("m".into()), None)
                    .unwrap();
            let messages = [NeutralMessage::user_text("hi")];
            let input = CompletionInput {
                model: "m",
                max_tokens: 64,
                messages: &messages,
                tools: &[],
            };
            let prepared = runtime.prepare(&input).unwrap();
            let lease = runtime.acquire_unpersisted(prepared).unwrap();
            let err = lease
                .execute_with_reasoning(&client, permit, &route, &mut |_| Ok(()), &mut |_| Ok(()))
                .await
                .unwrap_err();
            assert_eq!(hits.load(Ordering::SeqCst), 1);
            assert!(!err.is_retryable());
            match err {
                TransportError::Provider(pe) => {
                    assert_eq!(pe.status, status);
                    assert_eq!(pe.upstream_code.as_deref(), Some(code));
                    assert!(!pe.retryable);
                    let msg = pe.message.unwrap();
                    assert!(!msg.contains(secret));
                    assert!(msg.contains("[redacted]"));
                }
                other => panic!("expected TransportError::Provider, got {other:?}"),
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn unified_completion_lease_executes_openai_responses_with_encrypted_content_and_usage() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen_req = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_srv = seen_req.clone();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req_buf = vec![0u8; 8192];
            let n = sock.read(&mut req_buf).await.unwrap();
            *seen_srv.lock().unwrap() = String::from_utf8_lossy(&req_buf[..n]).into_owned();
            let sse = concat!(
                "event: response.reasoning_summary_text.delta\ndata: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_1\",\"output_index\":0,\"delta\":\"plan\"}\n\n",
                "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"enc_wire_9\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"plan\"}]}}\n\n",
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"m_1\",\"output_index\":1,\"delta\":\"hello\"}\n\n",
                "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_x\",\"status\":\"completed\",\"usage\":{\"input_tokens\":30,\"output_tokens\":12,\"total_tokens\":42}}}\n\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{sse}",
                sse.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });

        let runtime = ProviderRuntime::builtin_with_state(true).unwrap();
        let client = ProviderClient::new().unwrap();
        let base = format!("http://{addr}/v1");
        let route = Route {
            id: "resp".into(),
            name: "Responses".into(),
            base_url: base.clone(),
            model: "gpt-5".into(),
            protocol: Some(ProviderProtocol::OpenAiResponses),
            max_tokens: 256,
            parallel_limit: 1,
            key_env: None,
        };
        let target = crate::provider_transport::validate_target_with_protocol(
            &base,
            NetworkPurpose::Completion,
            route.protocol,
        )
        .unwrap();
        let permit =
            ProviderPermit::new(target, "k".into(), "c".into(), Some("gpt-5".into()), None)
                .unwrap();
        let messages = [
            NeutralMessage::system("Be concise"),
            NeutralMessage::user_text("say hello"),
        ];
        let input = CompletionInput {
            model: "gpt-5",
            max_tokens: 256,
            messages: &messages,
            tools: &[],
        };
        let prepared = runtime.prepare(&input).unwrap();
        let lease = runtime.acquire_unpersisted(prepared).unwrap();
        let (msg, usage) = lease
            .execute_with_reasoning(&client, permit, &route, &mut |_| Ok(()), &mut |_| Ok(()))
            .await
            .unwrap();
        let raw_req = seen_req.lock().unwrap().clone();
        assert!(raw_req.starts_with("POST /v1/responses "));
        let body_str = raw_req.split("\r\n\r\n").nth(1).unwrap();
        let body: Value = serde_json::from_str(body_str).unwrap();
        assert_eq!(body["store"], false);
        assert_eq!(body["instructions"], "Be concise");
        assert!(matches!(
            &msg.content[0],
            ContentBlock::Thinking { encrypted_content: Some(enc), .. } if enc == "enc_wire_9"
        ));
        assert_eq!(msg.text_content(), "hello");
        assert!(matches!(
            crate::provider_usage::classify_usage(&usage),
            crate::provider_usage::UsageObservation::Valid(u)
                if u.prompt_tokens == 30 && u.completion_tokens == 12 && u.total_tokens == 42
        ));
        server.abort();
    }

    #[tokio::test]
    async fn openai_responses_emits_prompt_cache_key_and_session_headers() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        assert_eq!(normalize_openai_prompt_cache_key("  "), None);
        assert_eq!(
            normalize_openai_prompt_cache_key("swe-chat-7f3a").as_deref(),
            Some("swe-chat-7f3a")
        );
        let long_id = "s".repeat(96);
        let hashed = normalize_openai_prompt_cache_key(&long_id).unwrap();
        assert_eq!(hashed.len(), 64);
        assert_eq!(hashed, normalize_openai_prompt_cache_key(&long_id).unwrap());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen_req = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_srv = seen_req.clone();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req_buf = vec![0u8; 8192];
            let n = sock.read(&mut req_buf).await.unwrap();
            *seen_srv.lock().unwrap() = String::from_utf8_lossy(&req_buf[..n]).into_owned();
            let sse = concat!(
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"m_1\",\"output_index\":0,\"delta\":\"ok\"}\n\n",
                "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_swe\",\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1,\"total_tokens\":6}}}\n\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{sse}",
                sse.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });

        let runtime = ProviderRuntime::builtin_with_state(true).unwrap();
        let client = ProviderClient::new().unwrap();
        let base = format!("http://{addr}/v1");
        let route = Route {
            id: "swe".into(),
            name: "SWE".into(),
            base_url: base.clone(),
            model: "swe-2-flash".into(),
            protocol: Some(ProviderProtocol::OpenAiResponses),
            max_tokens: 256,
            parallel_limit: 1,
            key_env: None,
        };
        let target = crate::provider_transport::validate_target_with_protocol(
            &base,
            NetworkPurpose::Completion,
            route.protocol,
        )
        .unwrap();
        let permit = ProviderPermit::new(
            target,
            "sk-test-token-1234".into(),
            "c".into(),
            Some("swe-2-flash".into()),
            None,
        )
        .unwrap()
        .with_session_id("swe-chat-7f3a");
        let messages = [NeutralMessage::user_text("hi")];
        let input = CompletionInput {
            model: "swe-2-flash",
            max_tokens: 256,
            messages: &messages,
            tools: &[],
        };
        let prepared = runtime.prepare(&input).unwrap();
        let lease = runtime
            .acquire_unpersisted(prepared)
            .unwrap()
            .with_prompt_cache_key(Some("swe-chat-7f3a".into()));
        let (msg, _) = lease
            .execute_with_reasoning(&client, permit, &route, &mut |_| Ok(()), &mut |_| Ok(()))
            .await
            .unwrap();
        assert_eq!(msg.text_content(), "ok");
        let raw_req = seen_req.lock().unwrap().clone();
        let (head, body_str) = raw_req.split_once("\r\n\r\n").unwrap();
        let head_lower = head.to_ascii_lowercase();
        assert!(
            head_lower.contains("x-session-id: swe-chat-7f3a"),
            "missing x-session-id header: {head}"
        );
        assert!(
            head_lower.contains("session_id: swe-chat-7f3a"),
            "missing session_id header: {head}"
        );
        let body: Value = serde_json::from_str(body_str).unwrap();
        assert_eq!(body["prompt_cache_key"], "swe-chat-7f3a");
        server.abort();
    }

    #[tokio::test]
    async fn malformed_http_200_payload_maps_to_invalid_response_and_protocol_decode_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        for (content_type, raw_body) in [
            ("application/json; charset=utf-8", "{BAD_JSON_TRUNCATED"),
            ("text/event-stream", "data: {not-valid-json\n\n"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let ct = content_type.to_owned();
            let body = raw_body.to_owned();
            let server = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut req_buf = vec![0u8; 4096];
                let _ = sock.read(&mut req_buf).await.unwrap();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });

            let runtime = ProviderRuntime::builtin_with_state(true).unwrap();
            let client = ProviderClient::new().unwrap();
            let base = format!("http://{addr}/v1");
            let route = Route {
                id: "bad".into(),
                name: "Bad".into(),
                base_url: base.clone(),
                model: "m".into(),
                protocol: Some(ProviderProtocol::OpenAi),
                max_tokens: 64,
                parallel_limit: 1,
                key_env: None,
            };
            let target = crate::provider_transport::validate_target_with_protocol(
                &base,
                NetworkPurpose::Completion,
                route.protocol,
            )
            .unwrap();
            let permit =
                ProviderPermit::new(target, "k".into(), "c".into(), Some("m".into()), None)
                    .unwrap();
            let messages = [NeutralMessage::user_text("hi")];
            let input = CompletionInput {
                model: "m",
                max_tokens: 64,
                messages: &messages,
                tools: &[],
            };
            let prepared = runtime.prepare(&input).unwrap();
            let lease = runtime.acquire_unpersisted(prepared).unwrap();
            let err = lease
                .execute_with_reasoning(&client, permit, &route, &mut |_| Ok(()), &mut |_| Ok(()))
                .await
                .unwrap_err();
            assert_eq!(err, TransportError::InvalidResponse { status: 200 });

            let embedded_err =
                crate::kernel_embedded::credential_source::classify_transport_error(err, &|s| {
                    s.to_owned()
                });
            assert_eq!(embedded_err.code, "PROTOCOL_DECODE_ERROR");
            assert_eq!(embedded_err.status, Some(200));
            assert!(!embedded_err.retryable);
            server.abort();
        }
    }

    #[tokio::test]
    async fn network_stream_read_error_remains_transport_unknown() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req_buf = vec![0u8; 4096];
            let _ = sock.read(&mut req_buf).await.unwrap();
            // Declare chunked transfer-encoding then drop connection mid-chunk (invalid chunk header).
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello",
                )
                .await;
            drop(sock);
        });

        let runtime = ProviderRuntime::builtin_with_state(true).unwrap();
        let client = ProviderClient::new().unwrap();
        let base = format!("http://{addr}/v1");
        let route = Route {
            id: "drop".into(),
            name: "Drop".into(),
            base_url: base.clone(),
            model: "m".into(),
            protocol: Some(ProviderProtocol::OpenAi),
            max_tokens: 64,
            parallel_limit: 1,
            key_env: None,
        };
        let target = crate::provider_transport::validate_target_with_protocol(
            &base,
            NetworkPurpose::Completion,
            route.protocol,
        )
        .unwrap();
        let permit =
            ProviderPermit::new(target, "k".into(), "c".into(), Some("m".into()), None).unwrap();
        let messages = [NeutralMessage::user_text("hi")];
        let input = CompletionInput {
            model: "m",
            max_tokens: 64,
            messages: &messages,
            tools: &[],
        };
        let prepared = runtime.prepare(&input).unwrap();
        let lease = runtime.acquire_unpersisted(prepared).unwrap();
        let err = lease
            .execute_with_reasoning(&client, permit, &route, &mut |_| Ok(()), &mut |_| Ok(()))
            .await
            .unwrap_err();
        assert_eq!(err, TransportError::Unknown);
        server.abort();
    }
}
