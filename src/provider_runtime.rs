//! Lightweight builtin completion provider runtime without PluginHost ceremony.
use crate::{
    domain::Route,
    network_authorization::{NetworkError, NetworkErrorKind},
    provider_transport::{
        NetworkPurpose, ProviderClient, ProviderPermit, RESPONSE_LIMIT, TransportError,
    },
};
use anyhow::Result;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

const PROVIDER: &str = "builtin.provider-xpeach";

pub(crate) struct CompletionInput<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub messages: &'a [Value],
    pub tools: &'a [Value],
}

#[derive(Debug)]
pub(crate) enum CodecError {
    #[allow(dead_code)]
    Invalid,
    #[allow(dead_code)]
    Limit,
    Failed,
}

pub(crate) type CompletionOutput = (Value, Value);
pub(crate) type Emit<'a> = dyn FnMut(&str) -> anyhow::Result<()> + Send + 'a;

pub(crate) trait CompletionCodec: Send + Sync {
    fn encode(&self, input: &CompletionInput<'_>) -> Result<Value, CodecError>;
    fn decoder(&self) -> Result<Box<dyn CompletionDecoder>, CodecError>;
}

pub(crate) trait CompletionDecoder: Send {
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
}

type Codec = Arc<dyn CompletionCodec>;

pub(crate) struct ProviderRuntime {
    enabled: AtomicBool,
    in_flight: Arc<AtomicU64>,
    generation: AtomicU64,
    codec: Codec,
    version: String,
}

pub(crate) struct PreparedCompletion {
    generation: u64,
    desired_revision: Option<u64>,
    body: Value,
    decoder: Option<Box<dyn CompletionDecoder>>,
}

pub(crate) struct CompletionLease {
    streamed_content_only: bool,
    body: Value,
    decoder: Option<Box<dyn CompletionDecoder>>,
    in_flight: Arc<AtomicU64>,
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

fn unavailable() -> NetworkError {
    NetworkError::not_sent(NetworkErrorKind::Internal)
}

pub(crate) fn expected_body(input: &CompletionInput<'_>) -> Value {
    let mut body = json!({
        "model": input.model,
        "messages": input.messages,
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
    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn builtin_with_state(enabled: bool) -> Result<Self> {
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

    pub(crate) fn prepare(
        &self,
        input: &CompletionInput<'_>,
    ) -> Result<PreparedCompletion, NetworkError> {
        if !self.enabled.load(Ordering::SeqCst) {
            return Err(unavailable());
        }
        let generation = self.generation.load(Ordering::SeqCst);
        let body = self.codec.encode(input).map_err(|_| unavailable())?;
        let decoder = self.codec.decoder().map_err(|_| unavailable())?;
        Ok(PreparedCompletion {
            generation,
            desired_revision: None,
            body,
            decoder: Some(decoder),
        })
    }

    pub(crate) fn prepare_persisted(
        &self,
        store: &crate::store::Store,
        input: &CompletionInput<'_>,
    ) -> Result<PreparedCompletion, NetworkError> {
        let desired = store
            .plugin_desired_for(PROVIDER)
            .map_err(|_| unavailable())?;
        if !desired.enabled {
            return Err(unavailable());
        }
        let mut prepared = self.prepare(input)?;
        prepared.desired_revision = Some(desired.revision);
        Ok(prepared)
    }

    pub(crate) fn acquire_persisted<A>(
        &self,
        prepared: PreparedCompletion,
        admit: impl FnOnce(u64) -> Result<A, NetworkError>,
    ) -> Result<(CompletionLease, A), NetworkError> {
        let Some(revision) = prepared.desired_revision else {
            return Err(unavailable());
        };
        self.acquire(prepared, || admit(revision))
    }

    pub(crate) fn acquire<A>(
        &self,
        mut prepared: PreparedCompletion,
        admit: impl FnOnce() -> Result<A, NetworkError>,
    ) -> Result<(CompletionLease, A), NetworkError> {
        if !self.enabled.load(Ordering::SeqCst)
            || self.generation.load(Ordering::SeqCst) != prepared.generation
            || self.in_flight.load(Ordering::SeqCst) == u64::MAX
        {
            return Err(unavailable());
        }
        let admitted = admit()?;
        let body = std::mem::take(&mut prepared.body);
        let decoder = prepared.decoder.take().ok_or_else(unavailable)?;
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        Ok((
            CompletionLease {
                streamed_content_only: false,
                body,
                decoder: Some(decoder),
                in_flight: self.in_flight.clone(),
            },
            admitted,
        ))
    }

    pub(crate) fn disable(&self) -> Result<()> {
        if self.enabled.swap(false, Ordering::SeqCst) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    pub(crate) fn enable_persisted(&self, store: &crate::store::Store) -> Result<()> {
        if !store.plugin_desired_for(PROVIDER)?.enabled {
            anyhow::bail!("provider runtime unavailable");
        }
        if !self.enabled.swap(true, Ordering::SeqCst) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    pub(crate) fn status(&self) -> Result<ProviderRuntimeStatus> {
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

    pub(crate) fn management_status(
        &self,
        store: &crate::store::Store,
    ) -> std::result::Result<
        crate::runtime_management::RuntimeComponentStatus,
        crate::runtime_management::RuntimeManagementError,
    > {
        use crate::runtime_management::{RuntimeManagementError as E, provider_status};
        let desired = store
            .plugin_desired_for(PROVIDER)
            .map_err(|_| E::Internal)?;
        let snapshot = self.status().map_err(|_| E::Internal)?;
        Ok(provider_status(desired, snapshot))
    }

    pub(crate) fn set_management(
        &self,
        store: &crate::store::Store,
        request: crate::plugin_management::SetPluginEnabled,
        key: &str,
    ) -> std::result::Result<
        crate::runtime_management::RuntimeMutationResult,
        crate::runtime_management::RuntimeManagementError,
    > {
        use crate::runtime_management::{RuntimeManagementError as E, RuntimeMutationResult};
        let op = store
            .execution_operation(tokio_util::sync::CancellationToken::new())
            .map_err(|_| E::Internal)?;
        let open = self.enabled.load(Ordering::SeqCst);
        let reject =
            if request.enabled && !open && self.generation.load(Ordering::SeqCst) == u64::MAX {
                Some(E::Unavailable)
            } else {
                None
            };
        let (receipt, replayed) =
            store.set_runtime_desired_with_operation(&op, PROVIDER, &request, key, reject)?;
        if !replayed {
            if request.enabled {
                if !self.enabled.swap(true, Ordering::SeqCst) {
                    self.generation.fetch_add(1, Ordering::SeqCst);
                }
            } else if self.enabled.swap(false, Ordering::SeqCst) {
                self.generation.fetch_add(1, Ordering::SeqCst);
            }
        }
        let current = self.management_status(store)?;
        Ok(RuntimeMutationResult {
            receipt,
            replayed,
            current,
        })
    }
}

impl CompletionLease {
    /// Summarizes in isolation. Only the final response is returned to the exploration caller.
    pub(crate) async fn execute_exploration(
        mut self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
    ) -> Result<CompletionOutput, TransportError> {
        self.streamed_content_only = true;
        let scrub = permit.credential_scrubber();
        let messages = self.body["messages"]
            .as_array_mut()
            .ok_or(TransportError::Invalid)?;
        for message in messages {
            if message["role"] == "user"
                && let Some(Value::String(text)) = message.get_mut("content")
            {
                *text = scrub(text);
            }
        }
        let (mut message, usage) = self.execute(client, permit, route, &mut |_| Ok(())).await?;
        let fields = message.as_object_mut().ok_or(TransportError::Unknown)?;
        fields.remove("reasoning_content");
        fields.remove("thinking_signature");
        if let Some(Value::String(text)) = fields.get_mut("content") {
            *text = scrub(text);
        }
        Ok((message, usage))
    }

    pub(crate) async fn execute(
        self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
        emit: &mut Emit<'_>,
    ) -> Result<CompletionOutput, TransportError> {
        self.execute_with_reasoning(client, permit, route, emit, &mut |_| Ok(()))
            .await
    }

    pub(crate) async fn execute_with_reasoning(
        mut self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
        emit: &mut (impl FnMut(&str) -> anyhow::Result<()> + Send + ?Sized),
        emit_reasoning: &mut (impl FnMut(&str) -> anyhow::Result<()> + Send + ?Sized),
    ) -> Result<CompletionOutput, TransportError> {
        let keys = permit.credentials();
        let mut redactor = keys.stream();
        let mut reasoning_redactor = keys.stream();
        use crate::provider_transport::ProviderProtocol;
        let protocol = permit.protocol();
        permit.matches(
            &route.base_url,
            NetworkPurpose::Completion,
            Some(&route.model),
            None,
        )?;
        let wire_body;
        let body_ref = match protocol {
            ProviderProtocol::OpenAi => &self.body,
            ProviderProtocol::Anthropic | ProviderProtocol::Gemini => {
                wire_body = crate::provider::encode_protocol_body(protocol, &self.body)
                    .map_err(|_| TransportError::Invalid)?;
                &wire_body
            }
        };
        let response = client.send(permit, Some(body_ref)).await?;
        if !response.status().is_success() {
            crate::provider::json_response(response).await?;
            return Err(TransportError::Unknown);
        }
        let default_decoder = self.decoder.take().ok_or(TransportError::Unknown)?;
        let mut decoder = match protocol {
            ProviderProtocol::OpenAi => default_decoder,
            ProviderProtocol::Anthropic | ProviderProtocol::Gemini => {
                crate::provider::protocol_decoder(protocol)
            }
        };
        let mut bytes = response.bytes_stream();
        let mut size = 0usize;
        let mut emitted = false;
        let mut streamed_text = String::new();
        let mut emission_failed = false;
        let mut reasoning_emission_failed = false;

        let mut safe_emit = |text: &str| -> anyhow::Result<()> {
            emitted = true;
            anyhow::ensure!(!emission_failed, "completion output unavailable");
            let safe = redactor.push(text);
            if !safe.is_empty() && emit(&safe).is_err() {
                emission_failed = true;
                anyhow::bail!("completion output unavailable");
            }
            streamed_text.push_str(&safe);
            Ok(())
        };
        let mut safe_reasoning_emit = |raw: &str| -> anyhow::Result<()> {
            anyhow::ensure!(
                !reasoning_emission_failed,
                "completion reasoning unavailable"
            );
            let safe = reasoning_redactor.push(raw);
            if !safe.is_empty() && emit_reasoning(&safe).is_err() {
                reasoning_emission_failed = true;
                anyhow::bail!("completion reasoning unavailable");
            }
            Ok(())
        };

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|_| TransportError::Unknown)?;
            if chunk.len() > RESPONSE_LIMIT.saturating_sub(size) {
                return Err(TransportError::Unknown);
            }
            size += chunk.len();
            if !decoder.is_done() {
                decoder
                    .feed_with_reasoning(&chunk, &mut safe_emit, &mut safe_reasoning_emit)
                    .map_err(|_| TransportError::Unknown)?;
            }
        }
        // Native decoders emit real text during feed and may promote reasoning during finish.
        // Always finish for validation and usage, but exploration consumes only wire text deltas.
        let mut output = if self.streamed_content_only {
            decoder.finish(&mut |_| Ok(()))
        } else {
            decoder.finish(&mut safe_emit)
        }
        .map_err(|_| TransportError::Unknown)?;
        if emission_failed || reasoning_emission_failed {
            return Err(TransportError::Unknown);
        }
        let reasoning_tail = reasoning_redactor.finish();
        if !reasoning_tail.is_empty() {
            emit_reasoning(&reasoning_tail).map_err(|_| TransportError::Unknown)?;
        }
        if !self.streamed_content_only
            && !emitted
            && let Some(content) = output.0.get("content").and_then(Value::as_str)
        {
            let safe = redactor.push(content);
            if !safe.is_empty() {
                emit(&safe).map_err(|_| TransportError::Unknown)?;
            }
            streamed_text.push_str(&safe);
        }
        let tail = redactor.finish();
        if !tail.is_empty() {
            emit(&tail).map_err(|_| TransportError::Unknown)?;
            streamed_text.push_str(&tail);
        }
        let fields = output.0.as_object_mut().ok_or(TransportError::Unknown)?;
        fields.insert("content".into(), Value::String(streamed_text));
        scrub_signed_reasoning(fields, &keys);
        if let Some(Value::Array(calls)) = fields.get_mut("tool_calls") {
            for call in calls {
                for path in ["/id", "/function/name", "/function/arguments"] {
                    if let Some(Value::String(value)) = call.pointer_mut(path) {
                        *value = keys.scrub(value);
                    }
                }
            }
        }
        Ok(output)
    }
}

fn scrub_signed_reasoning(
    fields: &mut serde_json::Map<String, Value>,
    keys: &crate::secrets::CredentialSet,
) {
    let mut modified = false;
    if let Some(Value::String(reasoning)) = fields.get_mut("reasoning_content") {
        let safe = keys.scrub(reasoning);
        modified = safe != *reasoning;
        *reasoning = safe;
    }
    // A signature is reusable only with the original thinking text.
    if modified
        || fields
            .get("thinking_signature")
            .and_then(Value::as_str)
            .is_some_and(|s| keys.scrub(s) != s)
    {
        fields.remove("thinking_signature");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_credentials_preserve_only_reusable_thinking_signatures() {
        let keys = crate::secrets::CredentialSet::new(vec!["backup-token".into()]);
        for (thinking, signature, expected) in [
            ("business text", "opaque", Some("opaque")),
            ("key=backup-token", "opaque", None),
            ("business text", "backup-token", None),
        ] {
            let mut message = json!({"reasoning_content":thinking, "thinking_signature":signature});
            scrub_signed_reasoning(message.as_object_mut().unwrap(), &keys);
            assert_eq!(message["thinking_signature"].as_str(), expected);
            assert_eq!(message["reasoning_content"], keys.scrub(thinking));
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
                max_tokens: 64,
                parallel_limit: 1,
                key_env: None,
            };
            let input = CompletionInput {
                model,
                max_tokens: 64,
                messages: &[json!({"role":"user","content":"test"})],
                tools: &[],
            };
            let prepared = runtime.prepare(&input).unwrap();
            let (lease, ()) = runtime.acquire(prepared, || Ok(())).unwrap();
            let (message, usage) = if model == "main" {
                lease
                    .execute(&client, permit, &route, &mut |_| Ok(()))
                    .await
            } else {
                lease.execute_exploration(&client, permit, &route).await
            }
            .unwrap();
            assert_eq!(message["content"], expected, "{model}");
            if model != "main" {
                assert!(message.get("reasoning_content").is_none());
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
            Ok((json!({"role": "assistant", "content": self.buf}), json!({})))
        }
    }

    #[test]
    fn custom_request_fields_are_not_blocked_by_prepare() {
        let runtime = ProviderRuntime::with_codec_for_test(Arc::new(CustomTempCodec)).unwrap();
        let messages = [json!({"role": "user", "content": "hi"})];
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
        assert_eq!(msg["content"], "hello");
    }
}
