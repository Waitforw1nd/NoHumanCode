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
    desired_revision: Option<u64>,
    body: Value,
    decoder: Option<Box<dyn CompletionDecoder>>,
}
impl Drop for PreparedCompletion {
    fn drop(&mut self) {
        let decoder = self.decoder.take();
        let _ = guarded(|| drop(decoder));
    }
}
pub(crate) struct CompletionLease {
    body: Value,
    decoder: Box<dyn CompletionDecoder>,
    in_flight: Arc<AtomicU64>,
}
impl Drop for CompletionLease {
    fn drop(&mut self) {
        let decoder = std::mem::replace(&mut self.decoder, Box::new(EmptyDecoder));
        let _ = guarded(|| drop(decoder));
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
    #[cfg(test)]
    pub(crate) fn builtin() -> Result<Self> {
        Self::assemble(Arc::new(crate::provider::BuiltinCompletionCodec))
    }
    pub(crate) fn builtin_with_state(enabled: bool) -> Result<Self> {
        Self::assemble_with_state(Arc::new(crate::provider::BuiltinCompletionCodec), enabled)
    }
    #[cfg(test)]
    fn assemble(codec: Codec) -> Result<Self> {
        Self::assemble_with_state(codec, true)
    }
    fn assemble_with_state(codec: Codec, enabled: bool) -> Result<Self> {
        let mut host = PluginHost::new(ScopeKey::Host)?;
        host.catalog_mut().register(manifest(PROVIDER, true))?;
        host.catalog_mut().register(manifest(CONSUMER, false))?;
        host.with_factory(PROVIDER, move || Box::new(ProviderPlugin(codec.clone())));
        host.with_factory(CONSUMER, || Box::new(ConsumerPlugin));
        Self::from_host_with_state(host, enabled)
    }
    #[cfg(test)]
    fn from_host(host: PluginHost) -> Result<Self> {
        Self::from_host_with_state(host, true)
    }
    fn from_host_with_state(mut host: PluginHost, enabled: bool) -> Result<Self> {
        if enabled {
            let started = guarded(|| host.start(&[CONSUMER.to_string()])).is_ok_and(|r| r.is_ok());
            if !started {
                let _ = guarded(|| drop(host));
                anyhow::bail!("provider activation failed");
            }
            if host.resolve_bound::<Codec>(CONSUMER, &key()).is_err() {
                let _ = guarded(|| drop(host));
                anyhow::bail!("provider binding failed");
            }
        }
        Ok(Self {
            state: Mutex::new(State {
                last_effects: host.effects().len(),
                last_recovery: host.pending_recovery().len(),
                host: Some(host),
                admission_open: enabled,
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
        let result = (|| {
            let body = guarded(|| codec.encode(input))
                .map_err(|_| unavailable())?
                .map_err(|_| unavailable())?;
            validate_body(input, &body).map_err(|_| unavailable())?;
            let decoder = guarded(|| codec.decoder())
                .map_err(|_| unavailable())?
                .map_err(|_| unavailable())?;
            Ok::<_, NetworkError>((body, decoder))
        })();
        let _ = guarded(|| drop(codec));
        let (body, decoder) = result?;
        Ok(PreparedCompletion {
            effect,
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
        let revision = match prepared.desired_revision {
            Some(revision) => revision,
            None => {
                let _ = guarded(|| drop(prepared));
                return Err(unavailable());
            }
        };
        self.acquire(prepared, || admit(revision))
    }
    pub(crate) fn acquire<A>(
        &self,
        mut prepared: PreparedCompletion,
        admit: impl FnOnce() -> Result<A, NetworkError>,
    ) -> Result<(CompletionLease, A), NetworkError> {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                drop(poisoned);
                let _ = guarded(|| drop(prepared));
                return Err(unavailable());
            }
        };
        let binding = state
            .host
            .as_ref()
            .and_then(|host| host.resolve_bound::<Codec>(CONSUMER, &key()).ok());
        if !state.admission_open
            || state.generation != prepared.generation
            || binding
                .as_ref()
                .is_none_or(|binding| binding.effect_id != prepared.effect)
            || self.in_flight.load(Ordering::SeqCst) == u64::MAX
        {
            drop(state);
            let _ = guarded(|| drop(binding));
            let _ = guarded(|| drop(prepared));
            return Err(unavailable());
        }
        let admitted = match admit() {
            Ok(admitted) => admitted,
            Err(error) => {
                drop(state);
                let _ = guarded(|| drop(binding));
                let _ = guarded(|| drop(prepared));
                return Err(error);
            }
        };
        let body = std::mem::take(&mut prepared.body);
        let decoder = prepared.decoder.take().expect("prepared decoder exists");
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        drop(state);
        let _ = guarded(|| drop(binding));
        Ok((
            CompletionLease {
                body,
                decoder,
                in_flight: self.in_flight.clone(),
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
    #[cfg(test)]
    pub(crate) fn from_host_with_state_for_test(host: PluginHost, enabled: bool) -> Result<Self> {
        Self::from_host_with_state(host, enabled)
    }
    #[cfg(test)]
    pub(crate) fn set_generation_for_test(&self, generation: u64) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("provider runtime lock"))?
            .generation = generation;
        Ok(())
    }
    pub(crate) fn disable(&self) -> Result<()> {
        self.transition(false)
    }
    #[cfg(test)]
    pub(crate) fn enable(&self) -> Result<()> {
        self.transition(true)
    }
    pub(crate) fn enable_persisted(&self, store: &crate::store::Store) -> Result<()> {
        self.transition_with_store(true, Some(store))
    }
    fn transition(&self, enable: bool) -> Result<()> {
        self.transition_with_store(enable, None)
    }
    fn transition_with_store(
        &self,
        enable: bool,
        store: Option<&crate::store::Store>,
    ) -> Result<()> {
        let host = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("provider runtime lock"))?;
            if let Some(store) = store
                && !store.plugin_desired_for(PROVIDER)?.enabled
            {
                anyhow::bail!("provider runtime unavailable");
            }
            if state.host.is_none() {
                anyhow::bail!("provider transition in progress");
            }
            if enable && state.lifecycle_failed {
                anyhow::bail!("provider runtime unavailable");
            }
            state.admission_open = false;
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("generation exhausted"))?;
            state.host.take().expect("host checked above")
        };
        self.finish_transition(host, enable)
    }
    fn finish_transition(&self, mut host: PluginHost, enable: bool) -> Result<()> {
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
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                drop(poisoned);
                let _ = guarded(|| drop(host));
                anyhow::bail!("provider runtime lock");
            }
        };
        state.last_effects = host.effects().len();
        state.last_recovery = host.pending_recovery().len();
        let bound = !enable || host.resolve_bound::<Codec>(CONSUMER, &key()).is_ok();
        state.lifecycle_failed |= outcome.is_err() || !bound;
        state.admission_open = enable && outcome.is_ok() && bound && !state.lifecycle_failed;
        state.host = Some(host);
        if !bound {
            anyhow::bail!("provider binding failed");
        }
        outcome
    }
    pub(crate) fn status(&self) -> Result<ProviderRuntimeStatus> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("provider runtime lock"))?;
        Ok(self.snapshot(&state))
    }
    fn snapshot(&self, state: &State) -> ProviderRuntimeStatus {
        ProviderRuntimeStatus {
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
        }
    }
    pub(crate) fn management_status(
        &self,
        store: &crate::store::Store,
    ) -> std::result::Result<
        crate::runtime_management::RuntimeComponentStatus,
        crate::runtime_management::RuntimeManagementError,
    > {
        use crate::runtime_management::{RuntimeManagementError as E, provider_status};
        let state = self.state.lock().map_err(|_| E::Internal)?;
        let desired = store
            .plugin_desired_for(PROVIDER)
            .map_err(|_| E::Internal)?;
        Ok(provider_status(desired, self.snapshot(&state)))
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
        let (receipt, replayed, host) = {
            let mut state = self.state.lock().map_err(|_| E::Internal)?;
            let active = state
                .host
                .as_ref()
                .is_some_and(|host| host.resolve_bound::<Codec>(CONSUMER, &self::key()).is_ok());
            let changing = if request.enabled {
                !state.admission_open
            } else {
                state.admission_open || active
            };
            let reject = if state.host.is_none() {
                Some(E::Busy)
            } else if request.enabled
                && (state.lifecycle_failed || (changing && state.generation == u64::MAX))
            {
                Some(E::Unavailable)
            } else {
                None
            };
            let (receipt, replayed) = store.set_runtime_desired(PROVIDER, &request, key, reject)?;
            let host = if !replayed && changing {
                state.admission_open = false;
                if let Some(next) = state.generation.checked_add(1) {
                    state.generation = next;
                } else {
                    state.lifecycle_failed = true;
                }
                state.host.take()
            } else {
                None
            };
            (receipt, replayed, host)
        };
        if let Some(host) = host {
            let _ = self.finish_transition(host, request.enabled);
        }
        let current = self.management_status(store)?;
        Ok(RuntimeMutationResult {
            receipt,
            replayed,
            current,
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
impl Drop for ProviderRuntime {
    fn drop(&mut self) {
        let state = match self.state.get_mut() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let host = state.host.take();
        let _ = guarded(|| drop(host));
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

#[cfg(all(test, windows))]
mod persistent_constructor_tests {
    use super::*;
    #[test]
    fn disabled_host_is_registered_without_starting() {
        let mut host = PluginHost::new(ScopeKey::Host).unwrap();
        host.catalog_mut()
            .register(manifest(PROVIDER, true))
            .unwrap();
        host.catalog_mut()
            .register(manifest(CONSUMER, false))
            .unwrap();
        host.with_factory(PROVIDER, || {
            Box::new(ProviderPlugin(Arc::new(
                crate::provider::BuiltinCompletionCodec,
            )))
        });
        host.with_factory(CONSUMER, || Box::new(ConsumerPlugin));
        let runtime = ProviderRuntime::from_host_with_state_for_test(host, false).unwrap();
        let status = runtime.status().unwrap();
        assert!(!status.active && !status.admission_open);
        assert_eq!((status.effects, status.generation), (0, 1));
        runtime.set_generation_for_test(1).unwrap();
    }
}
