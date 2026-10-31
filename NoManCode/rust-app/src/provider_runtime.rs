//! Host-controlled binding and admission for the builtin completion codec.
use crate::{
    domain::Route,
    network_authorization::{NetworkError, NetworkErrorKind},
    plugin_callback::guarded,
    plugin_catalog::{
        CatalogManifestV1, ExactVersion, InterfaceKey, InterfaceKind, LifecycleKind, RuntimeKind,
        ScopeKey, ScopeKind, Surface,
    },
    plugin_host::{BuiltinPlugin, EffectId, PluginContext, PluginError, PluginHost},
    provider_transport::{
        NetworkPurpose, ProviderClient, ProviderPermit, RESPONSE_LIMIT, TransportError,
    },
};
use anyhow::Result;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

const PROVIDER: &str = "builtin.provider-xpeach";
const CONSUMER: &str = "builtin.engine-provider";

pub(crate) struct CompletionInput<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub messages: &'a [Value],
    pub tools: &'a [Value],
}
#[derive(Debug)]
pub(crate) enum CodecError {
    Invalid,
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
    fn is_done(&self) -> bool;
    fn finish(self: Box<Self>, emit: &mut Emit<'_>) -> Result<CompletionOutput, CodecError>;
}
type Codec = Arc<dyn CompletionCodec>;

fn key() -> InterfaceKey {
    InterfaceKey {
        kind: InterfaceKind::Service,
        name: "provider.completion-codec".into(),
        version: ExactVersion {
            major: 1,
            minor: 0,
            patch: 0,
        },
    }
}
fn manifest(id: &str, provider: bool) -> CatalogManifestV1 {
    CatalogManifestV1 {
        manifest_version: 1,
        id: id.into(),
        display_name: id.into(),
        version: ExactVersion {
            major: 1,
            minor: 0,
            patch: 0,
        },
        scope: ScopeKind::Host,
        runtime: RuntimeKind::Builtin,
        lifecycle: LifecycleKind::HostManaged,
        surfaces: vec![Surface::Cli, Surface::Headless],
        permissions: vec![],
        config_schema: json!({}),
        provides: if provider { vec![key()] } else { vec![] },
        requires: if provider { vec![] } else { vec![key()] },
    }
}
struct ProviderPlugin(Codec);
impl BuiltinPlugin for ProviderPlugin {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_effect(&key(), Box::new(self.0.clone()))
            .map_err(|_| PluginError::new("provider registration failed"))?;
        Ok(())
    }
    fn deactivate(&mut self, _: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}
struct ConsumerPlugin;
impl BuiltinPlugin for ConsumerPlugin {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let binding = ctx
            .bound(&key())
            .map_err(|_| PluginError::new("provider binding failed"))?;
        if binding.payload().downcast_ref::<Codec>().is_none() {
            return Err(PluginError::new("provider type mismatch"));
        }
        Ok(())
    }
    fn deactivate(&mut self, _: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}
struct State {
    host: Option<PluginHost>,
    admission_open: bool,
    generation: u64,
    last_effects: usize,
    last_recovery: usize,
    lifecycle_failed: bool,
}
pub(crate) struct ProviderRuntime {
    state: Mutex<State>,
    in_flight: Arc<AtomicU64>,
}
pub(crate) struct PreparedCompletion {
    effect: EffectId,
    generation: u64,
    body: Value,
    decoder: Box<dyn CompletionDecoder>,
}
pub(crate) struct CompletionLease {
    body: Value,
    decoder: Box<dyn CompletionDecoder>,
    in_flight: Arc<AtomicU64>,
}
impl Drop for CompletionLease {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}
fn unavailable() -> NetworkError {
    NetworkError::not_sent(NetworkErrorKind::Internal)
}
pub(crate) fn expected_body(input: &CompletionInput<'_>) -> Value {
    let mut body = json!({"model":input.model,"messages":input.messages,"max_tokens":input.max_tokens,"stream":true,"stream_options":{"include_usage":true}});
    if !input.tools.is_empty() {
        body["tools"] = json!(input.tools);
        body["tool_choice"] = json!("auto");
    }
    body
}
fn validate_body(input: &CompletionInput<'_>, body: &Value) -> Result<(), CodecError> {
    if *body != expected_body(input) {
        return Err(CodecError::Invalid);
    }
    Ok(())
}
fn account_response_bytes(size: &mut usize, additional: usize) -> Result<(), CodecError> {
    if additional > RESPONSE_LIMIT.saturating_sub(*size) {
        return Err(CodecError::Limit);
    }
    *size += additional;
    Ok(())
}
impl ProviderRuntime {
    pub(crate) fn builtin() -> Result<Self> {
        Self::assemble(Arc::new(crate::provider::BuiltinCompletionCodec))
    }
    fn assemble(codec: Codec) -> Result<Self> {
        let mut host = PluginHost::new(ScopeKey::Host)?;
        host.catalog_mut().register(manifest(PROVIDER, true))?;
        host.catalog_mut().register(manifest(CONSUMER, false))?;
        host.with_factory(PROVIDER, move || Box::new(ProviderPlugin(codec.clone())));
        host.with_factory(CONSUMER, || Box::new(ConsumerPlugin));
        Self::from_host(host)
    }
    fn from_host(mut host: PluginHost) -> Result<Self> {
        guarded(|| host.start(&[CONSUMER.to_string()]))
            .map_err(|_| anyhow::anyhow!("provider activation failed"))?
            .map_err(|_| anyhow::anyhow!("provider activation failed"))?;
        host.resolve_bound::<Codec>(CONSUMER, &key())
            .map_err(|_| anyhow::anyhow!("provider binding failed"))?;
        Ok(Self {
            state: Mutex::new(State {
                last_effects: host.effects().len(),
                last_recovery: host.pending_recovery().len(),
                host: Some(host),
                admission_open: true,
                generation: 1,
                lifecycle_failed: false,
            }),
            in_flight: Arc::new(AtomicU64::new(0)),
        })
    }
    pub(crate) fn prepare(
        &self,
        input: &CompletionInput<'_>,
    ) -> Result<PreparedCompletion, NetworkError> {
        let (codec, effect, generation) = {
            let state = self.state.lock().map_err(|_| unavailable())?;
            if !state.admission_open {
                return Err(unavailable());
            }
            let binding = state
                .host
                .as_ref()
                .ok_or_else(unavailable)?
                .resolve_bound::<Codec>(CONSUMER, &key())
                .map_err(|_| unavailable())?;
            (binding.payload, binding.effect_id, state.generation)
        };
        let body = guarded(|| codec.encode(input))
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        validate_body(input, &body).map_err(|_| unavailable())?;
        let decoder = guarded(|| codec.decoder())
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        Ok(PreparedCompletion {
            effect,
            generation,
            body,
            decoder,
        })
    }
    pub(crate) fn acquire<A>(
        &self,
        prepared: PreparedCompletion,
        admit: impl FnOnce() -> Result<A, NetworkError>,
    ) -> Result<(CompletionLease, A), NetworkError> {
        let state = self.state.lock().map_err(|_| unavailable())?;
        if !state.admission_open || state.generation != prepared.generation {
            return Err(unavailable());
        }
        let binding = state
            .host
            .as_ref()
            .ok_or_else(unavailable)?
            .resolve_bound::<Codec>(CONSUMER, &key())
            .map_err(|_| unavailable())?;
        if binding.effect_id != prepared.effect || self.in_flight.load(Ordering::SeqCst) == u64::MAX
        {
            return Err(unavailable());
        }
        let count = self.in_flight.clone();
        let admitted = match admit() {
            Ok(admitted) => admitted,
            Err(error) => {
                drop(state);
                drop(prepared);
                return Err(error);
            }
        };
        let PreparedCompletion { body, decoder, .. } = prepared;
        count.fetch_add(1, Ordering::SeqCst);
        Ok((
            CompletionLease {
                body,
                decoder,
                in_flight: count,
            },
            admitted,
        ))
    }
    #[cfg(test)]
    pub(crate) fn with_codec_for_test(codec: Codec) -> Result<Self> {
        Self::assemble(codec)
    }
    #[cfg(test)]
    pub(crate) fn from_host_for_test(host: PluginHost) -> Result<Self> {
        Self::from_host(host)
    }
    pub(crate) fn disable(&self) -> Result<()> {
        self.transition(false)
    }
    pub(crate) fn enable(&self) -> Result<()> {
        self.transition(true)
    }
    fn transition(&self, enable: bool) -> Result<()> {
        let mut host = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("provider runtime lock"))?;
            if state.host.is_none() {
                anyhow::bail!("provider transition in progress");
            }
            state.admission_open = false;
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("generation exhausted"))?;
            state.host.take().expect("host checked above")
        };
        let outcome = if enable {
            guarded(|| host.start(&[CONSUMER.to_string()]))
                .map_err(|_| anyhow::anyhow!("provider activation failed"))
                .and_then(|result| {
                    result
                        .map(|_| ())
                        .map_err(|_| anyhow::anyhow!("provider activation failed"))
                })
        } else {
            guarded(|| host.stop_subtree(PROVIDER))
                .map_err(|_| anyhow::anyhow!("provider deactivation failed"))
                .and_then(|result| {
                    result
                        .map(|_| ())
                        .map_err(|_| anyhow::anyhow!("provider deactivation failed"))
                })
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("provider runtime lock"))?;
        state.last_effects = host.effects().len();
        state.last_recovery = host.pending_recovery().len();
        state.lifecycle_failed = outcome.is_err();
        state.admission_open =
            enable && outcome.is_ok() && host.resolve_bound::<Codec>(CONSUMER, &key()).is_ok();
        state.host = Some(host);
        outcome
    }
    pub(crate) fn status(&self) -> Result<ProviderRuntimeStatus> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("provider runtime lock"))?;
        Ok(ProviderRuntimeStatus {
            admission_open: state.admission_open,
            in_flight: self.in_flight.load(Ordering::SeqCst),
            effects: state.last_effects,
            recovery: state.last_recovery,
            active: state
                .host
                .as_ref()
                .is_some_and(|host| host.resolve_bound::<Codec>(CONSUMER, &key()).is_ok()),
            generation: state.generation,
            transitioning: state.host.is_none(),
            lifecycle_failed: state.lifecycle_failed,
        })
    }
    #[cfg(test)]
    pub(crate) fn disable_for_test(&self) -> Result<()> {
        self.disable()
    }
    #[cfg(test)]
    pub(crate) fn enable_for_test(&self) -> Result<()> {
        self.enable()
    }
    #[cfg(test)]
    pub(crate) fn status_for_test(&self) -> Result<ProviderRuntimeStatus> {
        self.status()
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
    /// Effect/recovery counts are the last settled snapshot during transition.
    pub transitioning: bool,
    pub lifecycle_failed: bool,
}
impl CompletionLease {
    pub(crate) async fn execute(
        mut self,
        client: &ProviderClient,
        permit: ProviderPermit,
        route: &Route,
        emit: &mut Emit<'_>,
    ) -> Result<CompletionOutput, TransportError> {
        permit.matches(
            &route.base_url,
            NetworkPurpose::Completion,
            Some(&route.model),
            None,
        )?;
        let response = client.send(permit, Some(&self.body)).await?;
        if !response.status().is_success() {
            crate::provider::json_response(response).await?;
            return Err(TransportError::Unknown);
        }
        let mut bytes = response.bytes_stream();
        let mut size = 0usize;
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|_| TransportError::Unknown)?;
            account_response_bytes(&mut size, chunk.len()).map_err(|_| TransportError::Unknown)?;
            guarded(|| self.decoder.feed(&chunk, emit))
                .map_err(|_| TransportError::Unknown)?
                .map_err(|_| TransportError::Unknown)?;
            if guarded(|| self.decoder.is_done()).map_err(|_| TransportError::Unknown)? {
                break;
            }
        }
        // Move out of a Drop type without unsafe or retaining the decoder after completion.
        let decoder = std::mem::replace(&mut self.decoder, Box::new(EmptyDecoder));
        guarded(|| decoder.finish(emit))
            .map_err(|_| TransportError::Unknown)?
            .map_err(|_| TransportError::Unknown)
    }
}
struct EmptyDecoder;
impl CompletionDecoder for EmptyDecoder {
    fn feed(&mut self, _: &[u8], _: &mut Emit<'_>) -> Result<(), CodecError> {
        Err(CodecError::Failed)
    }
    fn is_done(&self) -> bool {
        true
    }
    fn finish(self: Box<Self>, _: &mut Emit<'_>) -> Result<CompletionOutput, CodecError> {
        Err(CodecError::Failed)
    }
}
