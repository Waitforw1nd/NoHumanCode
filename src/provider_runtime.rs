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
    pub(crate) async fn execute(
        self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
        emit: &mut Emit<'_>,
    ) -> Result<CompletionOutput, TransportError> {
        let mut noop = crate::secrets::StreamRedactor;
        self.execute_with_redactor(client, permit, route, emit, &mut noop)
            .await
    }

    pub(crate) async fn execute_with_redactor(
        self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
        emit: &mut Emit<'_>,
        redactor: &mut crate::secrets::StreamRedactor,
    ) -> Result<CompletionOutput, TransportError> {
        let mut noop_reasoning = crate::secrets::StreamRedactor;
        self.execute_with_reasoning_redactors(
            client,
            permit,
            route,
            emit,
            redactor,
            &mut |_| Ok(()),
            &mut noop_reasoning,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_with_reasoning_redactors(
        mut self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
        emit: &mut (impl FnMut(&str) -> anyhow::Result<()> + Send + ?Sized),
        redactor: &mut crate::secrets::StreamRedactor,
        emit_reasoning: &mut (impl FnMut(&str) -> anyhow::Result<()> + Send + ?Sized),
        reasoning_redactor: &mut crate::secrets::StreamRedactor,
    ) -> Result<CompletionOutput, TransportError> {
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
        let mut output = decoder
            .finish(&mut safe_emit)
            .map_err(|_| TransportError::Unknown)?;
        if emission_failed || reasoning_emission_failed {
            return Err(TransportError::Unknown);
        }
        let reasoning_tail = std::mem::take(reasoning_redactor).finish();
        if !reasoning_tail.is_empty() {
            emit_reasoning(&reasoning_tail).map_err(|_| TransportError::Unknown)?;
        }
        if !emitted && let Some(content) = output.0.get("content").and_then(Value::as_str) {
            let safe = redactor.push(content);
            if !safe.is_empty() {
                emit(&safe).map_err(|_| TransportError::Unknown)?;
            }
            streamed_text.push_str(&safe);
        }
        output
            .0
            .as_object_mut()
            .ok_or(TransportError::Unknown)?
            .insert("content".into(), Value::String(streamed_text));
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
