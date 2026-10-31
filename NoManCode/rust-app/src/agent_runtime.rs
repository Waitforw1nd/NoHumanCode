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
    Deny,
}
#[derive(Debug)]
pub(crate) enum StrategyAction {
    UseTools(Vec<usize>),
    ToolVotes(Vec<ToolVote>),
    Finish,
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
    pub(crate) fn builtin() -> anyhow::Result<Self> {
        Self::assemble(Arc::new(Builtin))
    }
    fn assemble(strategy: Strategy) -> anyhow::Result<Self> {
        let mut host = PluginHost::new(ScopeKey::Host)?;
        host.catalog_mut().register(manifest(PROVIDER, true))?;
        host.catalog_mut().register(manifest(CONSUMER, false))?;
        host.with_factory(PROVIDER, move || Box::new(StrategyPlugin(strategy.clone())));
        host.with_factory(CONSUMER, || Box::new(Consumer));
        Self::from_host(host)
    }
    fn from_host(mut host: PluginHost) -> anyhow::Result<Self> {
        guarded(|| host.start(&[CONSUMER.to_string()]))
            .map_err(|_| anyhow::anyhow!("agent activation failed"))?
            .map_err(|_| anyhow::anyhow!("agent activation failed"))?;
        host.resolve_bound::<Strategy>(CONSUMER, &key())
            .map_err(|_| anyhow::anyhow!("agent binding failed"))?;
        let effects = host.effects().len() as u64;
        let recovery = host.pending_recovery().len() as u64;
        Ok(Self {
            inner: Arc::new(Inner {
                #[cfg(test)]
                status_unavailable: std::sync::atomic::AtomicBool::new(false),
                state: Mutex::new(State {
                    host: Some(host),
                    active: true,
                    admission_open: true,
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
        state.in_flight = state.in_flight.checked_add(1).ok_or(AgentError::Internal)?;
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
        Ok(AgentRuntimeStatus {
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
        })
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
    pub(crate) fn enable(&self) -> Result<(), AgentError> {
        let mut host = {
            let mut s = self.inner.state.lock().map_err(|_| AgentError::Internal)?;
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
        let ok = guarded(|| host.start(&[CONSUMER.to_string()])).is_ok_and(|r| r.is_ok())
            && host.resolve_bound::<Strategy>(CONSUMER, &key()).is_ok();
        let mut s = self.inner.state.lock().map_err(|_| AgentError::Internal)?;
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
        let mut s = self.state.lock().map_err(|_| AgentError::Internal)?;
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
