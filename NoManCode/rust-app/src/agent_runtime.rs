//! Host-managed, task-scoped agent strategy binding.
use crate::{
    plugin_callback::guarded,
    plugin_catalog::{
        CatalogManifestV1, ExactVersion, InterfaceKey, InterfaceKind, LifecycleKind, RuntimeKind,
        ScopeKey, ScopeKind, Surface,
    },
    plugin_host::{BuiltinPlugin, PluginContext, PluginError, PluginHost},
};
use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};

const PROVIDER: &str = "builtin.chat-agent";
const CONSUMER: &str = "builtin.engine-chat-agent";
const SERVICE: &str = "agent.strategy";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentError {
    Unavailable,
    InvalidDecision,
    Failed,
    Panicked,
    Stopped,
    Busy,
    Lifecycle,
    Internal,
}
impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "agent runtime unavailable",
            Self::InvalidDecision => "invalid agent decision",
            Self::Failed => "agent strategy failed",
            Self::Panicked => "agent strategy panicked",
            Self::Stopped => "agent stopped by policy",
            Self::Busy => "agent runtime busy",
            Self::Lifecycle => "agent lifecycle failed",
            Self::Internal => "agent runtime internal error",
        })
    }
}
impl std::error::Error for AgentError {}
pub(crate) type StrategyError = AgentError;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolClass {
    Read,
    List,
    Search,
    Write,
    Command,
    Other,
}
pub(crate) enum StrategyInput<'a> {
    BeforeCompletion { tools: &'a [ToolClass] },
    AfterCompletion { outcome: CompletionKind<'a> },
}
pub(crate) enum CompletionKind<'a> {
    NoToolCalls,
    ToolGroup(&'a [ToolClass]),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolVote {
    ApplyHostPolicy,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "constructed by replacement strategies")
    )]
    Deny,
}
#[derive(Debug)]
pub(crate) enum StrategyAction {
    UseTools(Vec<usize>),
    ToolVotes(Vec<ToolVote>),
    Finish,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "constructed by replacement strategies")
    )]
    Stop,
}
pub(crate) trait AgentStrategy: Send + Sync {
    fn decide(
        &self,
        input: StrategyInput<'_>,
        messages: &[Value],
        round: usize,
        max_rounds: usize,
    ) -> Result<StrategyAction, StrategyError>;
}
type Strategy = Arc<dyn AgentStrategy>;
struct Builtin;
impl AgentStrategy for Builtin {
    fn decide(
        &self,
        input: StrategyInput<'_>,
        _: &[Value],
        _: usize,
        _: usize,
    ) -> Result<StrategyAction, StrategyError> {
        Ok(match input {
            StrategyInput::BeforeCompletion { tools } => {
                StrategyAction::UseTools((0..tools.len()).collect())
            }
            StrategyInput::AfterCompletion {
                outcome: CompletionKind::NoToolCalls,
            } => StrategyAction::Finish,
            StrategyInput::AfterCompletion {
                outcome: CompletionKind::ToolGroup(tools),
            } => StrategyAction::ToolVotes(vec![ToolVote::ApplyHostPolicy; tools.len()]),
        })
    }
}
fn key() -> InterfaceKey {
    InterfaceKey {
        kind: InterfaceKind::Service,
        name: SERVICE.into(),
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
        config_schema: serde_json::json!({}),
        provides: if provider { vec![key()] } else { vec![] },
        requires: if provider { vec![] } else { vec![key()] },
    }
}
struct StrategyPlugin(Strategy);
impl BuiltinPlugin for StrategyPlugin {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_effect(&key(), Box::new(self.0.clone()))
            .map_err(|_| PluginError::new("agent registration failed"))?;
        Ok(())
    }
    fn deactivate(&mut self, _: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}
struct Consumer;
impl BuiltinPlugin for Consumer {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let bound = ctx
            .bound(&key())
            .map_err(|_| PluginError::new("agent binding failed"))?;
        if bound.payload().downcast_ref::<Strategy>().is_none() {
            return Err(PluginError::new("agent type mismatch"));
        }
        Ok(())
    }
    fn deactivate(&mut self, _: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}
struct State {
    host: Option<PluginHost>,
    active: bool,
    admission_open: bool,
    transitioning: bool,
    in_flight: u64,
    generation: u64,
    effects: u64,
    recovery: u64,
    last_failure: Option<AgentError>,
    lifecycle_failed: bool,
}
struct Inner {
    state: Mutex<State>,
    #[cfg(test)]
    status_unavailable: std::sync::atomic::AtomicBool,
}
pub(crate) struct AgentRuntime {
    inner: Arc<Inner>,
}
pub(crate) struct AgentLease {
    strategy: Option<Strategy>,
    inner: Arc<Inner>,
}
#[derive(Clone, Debug, Serialize)]
pub struct AgentRuntimeStatus {
    pub plugin_id: &'static str,
    pub service: &'static str,
    pub interface_version: &'static str,
    pub scope: &'static str,
    pub active: bool,
    pub admission_open: bool,
    pub in_flight: u64,
    pub generation: u64,
    pub draining: bool,
    pub transitioning: bool,
    pub effects: u64,
    pub recovery: u64,
    pub last_failure: Option<AgentError>,
}
impl AgentRuntime {
    pub(crate) fn composition_manifest(&self) -> anyhow::Result<CatalogManifestV1> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("agent catalog unavailable"))?;
        let host = state
            .host
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("agent transitioning"))?;
        host.catalog()
            .descriptors()
            .into_iter()
            .find(|m| m.id == PROVIDER)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("agent binding missing"))
    }

    #[cfg(test)]
    pub(crate) fn builtin() -> anyhow::Result<Self> {
        Self::assemble(Arc::new(Builtin))
    }
    pub(crate) fn builtin_with_state(enabled: bool) -> anyhow::Result<Self> {
        Self::assemble_with_state(Arc::new(Builtin), enabled)
    }
    #[cfg(test)]
    fn assemble(strategy: Strategy) -> anyhow::Result<Self> {
        Self::assemble_with_state(strategy, true)
    }
    fn assemble_with_state(strategy: Strategy, enabled: bool) -> anyhow::Result<Self> {
        let mut host = PluginHost::new(ScopeKey::Host)?;
        host.catalog_mut().register(manifest(PROVIDER, true))?;
        host.catalog_mut().register(manifest(CONSUMER, false))?;
        host.with_factory(PROVIDER, move || Box::new(StrategyPlugin(strategy.clone())));
        host.with_factory(CONSUMER, || Box::new(Consumer));
        Self::from_host_with_state(host, enabled)
    }
    #[cfg(test)]
    fn from_host(host: PluginHost) -> anyhow::Result<Self> {
        Self::from_host_with_state(host, true)
    }
    fn from_host_with_state(mut host: PluginHost, enabled: bool) -> anyhow::Result<Self> {
        let started = !enabled
            || guarded(|| host.start(&[CONSUMER.to_string()])).is_ok_and(|result| result.is_ok());
        if !started {
            let _ = guarded(|| drop(host));
            return Err(anyhow::anyhow!("agent activation failed"));
        }
        if enabled && host.resolve_bound::<Strategy>(CONSUMER, &key()).is_err() {
            let _ = guarded(|| drop(host));
            return Err(anyhow::anyhow!("agent binding failed"));
        }
        let effects = host.effects().len() as u64;
        let recovery = host.pending_recovery().len() as u64;
        Ok(Self {
            inner: Arc::new(Inner {
                #[cfg(test)]
                status_unavailable: std::sync::atomic::AtomicBool::new(false),
                state: Mutex::new(State {
                    host: Some(host),
                    active: enabled,
                    admission_open: enabled,
                    transitioning: false,
                    in_flight: 0,
                    generation: 1,
                    effects,
                    recovery,
                    last_failure: None,
                    lifecycle_failed: false,
                }),
            }),
        })
    }
    #[cfg(test)]
    pub(crate) fn with_strategy_for_test(strategy: Strategy) -> anyhow::Result<Self> {
        Self::assemble(strategy)
    }
    #[cfg(test)]
    pub(crate) fn from_host_for_test(host: PluginHost) -> anyhow::Result<Self> {
        Self::from_host(host)
    }
    #[cfg(test)]
    pub(crate) fn from_host_with_state_for_test(
        host: PluginHost,
        enabled: bool,
    ) -> anyhow::Result<Self> {
        Self::from_host_with_state(host, enabled)
    }
    #[cfg(test)]
    pub(crate) fn acquire(&self) -> Result<AgentLease, AgentError> {
        let mut state = self.inner.state.lock().map_err(|_| AgentError::Internal)?;
        if !state.admission_open || state.transitioning || !state.active {
            return Err(AgentError::Unavailable);
        }
        let strategy = state
            .host
            .as_ref()
            .ok_or(AgentError::Internal)?
            .resolve_bound::<Strategy>(CONSUMER, &key())
            .map_err(|_| AgentError::Unavailable)?
            .payload;
        let Some(next) = state.in_flight.checked_add(1) else {
            drop(state);
            let _ = guarded(|| drop(strategy));
            return Err(AgentError::Internal);
        };
        state.in_flight = next;
        Ok(AgentLease {
            strategy: Some(strategy),
            inner: self.inner.clone(),
        })
    }
    pub(crate) fn acquire_persisted(
        &self,
        store: &crate::store::Store,
    ) -> Result<AgentLease, AgentError> {
        let mut state = self.inner.state.lock().map_err(|_| AgentError::Internal)?;
        if !state.admission_open || state.transitioning || !state.active {
            return Err(AgentError::Unavailable);
        }
        let strategy = state
            .host
            .as_ref()
            .ok_or(AgentError::Internal)?
            .resolve_bound::<Strategy>(CONSUMER, &key())
            .map_err(|_| AgentError::Unavailable)?
            .payload;
        if state.in_flight == u64::MAX {
            drop(state);
            let _ = guarded(|| drop(strategy));
            return Err(AgentError::Internal);
        }
        if store.admit_runtime(PROVIDER).is_err() {
            drop(state);
            let _ = guarded(|| drop(strategy));
            return Err(AgentError::Unavailable);
        }
        state.in_flight += 1;
        Ok(AgentLease {
            strategy: Some(strategy),
            inner: self.inner.clone(),
        })
    }
    pub(crate) fn status(&self) -> Result<AgentRuntimeStatus, AgentError> {
        #[cfg(test)]
        if self
            .inner
            .status_unavailable
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(AgentError::Internal);
        }
        let state = self.inner.state.lock().map_err(|_| AgentError::Internal)?;
        Ok(Self::snapshot(&state))
    }
    fn snapshot(state: &State) -> AgentRuntimeStatus {
        AgentRuntimeStatus {
            plugin_id: PROVIDER,
            service: SERVICE,
            interface_version: "1.0.0",
            scope: "process",
            active: state.active,
            admission_open: state.admission_open,
            in_flight: state.in_flight,
            generation: state.generation,
            draining: !state.admission_open && state.in_flight > 0,
            transitioning: state.transitioning,
            effects: state.effects,
            recovery: state.recovery,
            last_failure: state.last_failure,
        }
    }
    #[cfg(test)]
    pub(crate) fn set_status_unavailable_for_test(&self, unavailable: bool) {
        self.inner
            .status_unavailable
            .store(unavailable, std::sync::atomic::Ordering::SeqCst);
    }
    #[cfg(test)]
    pub(crate) fn set_generation_for_test(&self, generation: u64) -> Result<(), AgentError> {
        self.inner
            .state
            .lock()
            .map_err(|_| AgentError::Internal)?
            .generation = generation;
        Ok(())
    }
    pub(crate) fn disable(&self) -> Result<(), AgentError> {
        let (host, overflow) = {
            let mut s = self.inner.state.lock().map_err(|_| AgentError::Internal)?;
            if s.transitioning {
                return Err(AgentError::Busy);
            }
            if !s.admission_open {
                return if s.lifecycle_failed {
                    Err(AgentError::Lifecycle)
                } else {
                    Ok(())
                };
            }
            s.admission_open = false;
            let overflow = match s.generation.checked_add(1) {
                Some(n) => {
                    s.generation = n;
                    false
                }
                None => {
                    s.last_failure = Some(AgentError::Internal);
                    s.lifecycle_failed = true;
                    true
                }
            };
            (Inner::reserve_stop(&mut s)?, overflow)
        };
        let stopped = match host {
            Some(host) => self.inner.finish_stop(host),
            None => Ok(()),
        };
        if overflow {
            Err(AgentError::Internal)
        } else {
            stopped
        }
    }
    #[cfg(test)]
    pub(crate) fn enable(&self) -> Result<(), AgentError> {
        self.enable_with_store(None)
    }
    pub(crate) fn enable_persisted(&self, store: &crate::store::Store) -> Result<(), AgentError> {
        self.enable_with_store(Some(store))
    }
    fn enable_with_store(&self, store: Option<&crate::store::Store>) -> Result<(), AgentError> {
        let host = {
            let mut s = self.inner.state.lock().map_err(|_| AgentError::Internal)?;
            if let Some(store) = store
                && !store
                    .plugin_desired_for(PROVIDER)
                    .map_err(|_| AgentError::Internal)?
                    .enabled
            {
                return Err(AgentError::Unavailable);
            }
            if s.transitioning {
                return Err(AgentError::Busy);
            }
            if s.admission_open && s.active {
                return Ok(());
            }
            if s.in_flight != 0 {
                return Err(AgentError::Busy);
            }
            if s.lifecycle_failed {
                return Err(AgentError::Lifecycle);
            }
            let next = match s.generation.checked_add(1) {
                Some(next) => next,
                None => {
                    s.last_failure = Some(AgentError::Internal);
                    s.lifecycle_failed = true;
                    return Err(AgentError::Internal);
                }
            };
            let host = s.host.take().ok_or(AgentError::Internal)?;
            s.transitioning = true;
            s.active = false;
            s.generation = next;
            host
        };
        self.finish_start(host)
    }
    fn finish_start(&self, mut host: PluginHost) -> Result<(), AgentError> {
        let ok = guarded(|| host.start(&[CONSUMER.to_string()])).is_ok_and(|r| r.is_ok())
            && host.resolve_bound::<Strategy>(CONSUMER, &key()).is_ok();
        let mut s = match self.inner.state.lock() {
            Ok(s) => s,
            Err(poisoned) => {
                drop(poisoned);
                let _ = guarded(|| drop(host));
                return Err(AgentError::Internal);
            }
        };
        s.effects = host.effects().len() as u64;
        s.recovery = host.pending_recovery().len() as u64;
        s.active = ok;
        s.admission_open = ok;
        s.transitioning = false;
        s.lifecycle_failed = !ok;
        s.last_failure = if ok {
            None
        } else {
            Some(AgentError::Lifecycle)
        };
        s.host = Some(host);
        if ok {
            Ok(())
        } else {
            Err(AgentError::Lifecycle)
        }
    }
    pub(crate) fn management_status(
        &self,
        store: &crate::store::Store,
    ) -> Result<
        crate::runtime_management::RuntimeComponentStatus,
        crate::runtime_management::RuntimeManagementError,
    > {
        use crate::runtime_management::{RuntimeManagementError as E, agent_status};
        let s = self.inner.state.lock().map_err(|_| E::Internal)?;
        let desired = store
            .plugin_desired_for(PROVIDER)
            .map_err(|_| E::Internal)?;
        Ok(agent_status(desired, Self::snapshot(&s)))
    }
    pub(crate) fn set_management(
        &self,
        store: &crate::store::Store,
        request: crate::plugin_management::SetPluginEnabled,
        key: &str,
    ) -> Result<
        crate::runtime_management::RuntimeMutationResult,
        crate::runtime_management::RuntimeManagementError,
    > {
        use crate::runtime_management::{RuntimeManagementError as E, RuntimeMutationResult};
        let (receipt, replayed, start, stop) = {
            let mut s = self.inner.state.lock().map_err(|_| E::Internal)?;
            let needs_start = request.enabled && (!s.active || !s.admission_open);
            let reject =
                if s.transitioning || (request.enabled && s.in_flight > 0 && !s.admission_open) {
                    Some(E::Busy)
                } else if request.enabled
                    && (s.lifecycle_failed
                        || (needs_start && (s.generation == u64::MAX || s.host.is_none())))
                {
                    Some(E::Unavailable)
                } else {
                    None
                };
            let (receipt, replayed) = store.set_runtime_desired(PROVIDER, &request, key, reject)?;
            let mut start = None;
            let mut stop = None;
            if !replayed {
                if request.enabled && needs_start {
                    s.generation += 1;
                    s.transitioning = true;
                    s.active = false;
                    start = s.host.take();
                } else if !request.enabled {
                    if s.admission_open {
                        s.admission_open = false;
                        if let Some(next) = s.generation.checked_add(1) {
                            s.generation = next;
                        } else {
                            s.last_failure = Some(AgentError::Internal);
                            s.lifecycle_failed = true;
                        }
                    }
                    stop = Inner::reserve_stop(&mut s).map_err(|_| E::Internal)?;
                }
            }
            (receipt, replayed, start, stop)
        };
        if let Some(host) = start {
            let _ = self.finish_start(host);
        }
        if let Some(host) = stop {
            let _ = self.inner.finish_stop(host);
        }
        let current = self.management_status(store)?;
        Ok(RuntimeMutationResult {
            receipt,
            replayed,
            current,
        })
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        let host = {
            let state = match self.state.get_mut() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.host.take()
        };
        let _ = guarded(|| drop(host));
    }
}
impl Inner {
    fn reserve_stop(s: &mut State) -> Result<Option<PluginHost>, AgentError> {
        if s.admission_open || s.in_flight != 0 || s.transitioning || !s.active {
            return Ok(None);
        }
        let host = s.host.take().ok_or(AgentError::Internal)?;
        s.transitioning = true;
        s.active = false;
        Ok(Some(host))
    }
    fn finish_stop(&self, mut host: PluginHost) -> Result<(), AgentError> {
        let ok = guarded(|| host.stop_subtree(PROVIDER)).is_ok_and(|r| r.is_ok());
        let mut s = match self.state.lock() {
            Ok(s) => s,
            Err(poisoned) => {
                drop(poisoned);
                let _ = guarded(|| drop(host));
                return Err(AgentError::Internal);
            }
        };
        s.effects = host.effects().len() as u64;
        s.recovery = host.pending_recovery().len() as u64;
        s.transitioning = false;
        s.active = false;
        s.lifecycle_failed |= !ok;
        if !ok {
            s.last_failure = Some(AgentError::Lifecycle);
        }
        s.host = Some(host);
        if ok {
            Ok(())
        } else {
            Err(AgentError::Lifecycle)
        }
    }
    fn record_failure(&self, error: AgentError) {
        if let Ok(mut s) = self.state.lock() {
            s.last_failure = Some(error);
        }
    }
}
impl AgentLease {
    pub(crate) fn decide(
        &self,
        input: StrategyInput<'_>,
        messages: &[Value],
        round: usize,
        max_rounds: usize,
    ) -> Result<StrategyAction, AgentError> {
        let phase = match &input {
            StrategyInput::BeforeCompletion { tools } => (0, tools.len()),
            StrategyInput::AfterCompletion {
                outcome: CompletionKind::NoToolCalls,
            } => (1, 0),
            StrategyInput::AfterCompletion {
                outcome: CompletionKind::ToolGroup(tools),
            } => (2, tools.len()),
        };
        let strategy = self.strategy.as_ref().ok_or(AgentError::Internal)?;
        let result = guarded(|| strategy.decide(input, messages, round, max_rounds))
            .map_err(|_| AgentError::Panicked)
            .and_then(|r| r);
        let action = match result {
            Ok(action) => action,
            Err(error) => {
                self.inner.record_failure(error);
                return Err(error);
            }
        };
        let valid = match (&action, phase) {
            (StrategyAction::Stop, _) => true,
            (StrategyAction::UseTools(indices), (0, n)) => {
                indices.iter().all(|&i| i < n) && indices.windows(2).all(|w| w[0] < w[1])
            }
            (StrategyAction::Finish, (1, _)) => true,
            (StrategyAction::ToolVotes(votes), (2, n)) => votes.len() == n,
            _ => false,
        };
        if !valid {
            self.inner.record_failure(AgentError::InvalidDecision);
            return Err(AgentError::InvalidDecision);
        }
        if matches!(action, StrategyAction::Stop) {
            self.inner.record_failure(AgentError::Stopped);
            return Err(AgentError::Stopped);
        }
        Ok(action)
    }
}
impl Drop for AgentLease {
    fn drop(&mut self) {
        let drop_failed = guarded(|| drop(self.strategy.take())).is_err();
        let host = {
            let mut s = match self.inner.state.lock() {
                Ok(s) => s,
                Err(poisoned) => poisoned.into_inner(),
            };
            if drop_failed {
                s.last_failure = Some(AgentError::Lifecycle);
                s.lifecycle_failed = true;
                s.admission_open = false;
            }
            if let Some(count) = s.in_flight.checked_sub(1) {
                s.in_flight = count;
            } else {
                s.last_failure = Some(AgentError::Internal);
                s.lifecycle_failed = true;
                s.admission_open = false;
            }
            match Inner::reserve_stop(&mut s) {
                Ok(host) => host,
                Err(error) => {
                    s.last_failure = Some(error);
                    s.lifecycle_failed = true;
                    None
                }
            }
        };
        if let Some(host) = host {
            let _ = guarded(|| self.inner.finish_stop(host));
        }
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
        host.with_factory(PROVIDER, || Box::new(StrategyPlugin(Arc::new(Builtin))));
        host.with_factory(CONSUMER, || Box::new(Consumer));
        let runtime = AgentRuntime::from_host_with_state_for_test(host, false).unwrap();
        let status = runtime.status().unwrap();
        assert!(!status.active && !status.admission_open);
        assert_eq!((status.effects, status.generation), (0, 1));
        runtime.set_generation_for_test(1).unwrap();
    }
}
