//! Closed host-scoped management types for the built-in agent and completion provider.
use crate::{agent_runtime::AgentError, plugin_management::PluginMutationReceipt};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeId {
    ChatAgent,
    CompletionProvider,
}
impl RuntimeId {
    pub fn parse(value: &str) -> Result<Self, RuntimeManagementError> {
        match value {
            "builtin.chat-agent" => Ok(Self::ChatAgent),
            "builtin.provider-xpeach" => Ok(Self::CompletionProvider),
            _ => Err(RuntimeManagementError::NotFound),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatAgent => "builtin.chat-agent",
            Self::CompletionProvider => "builtin.provider-xpeach",
        }
    }
    pub fn capability(self) -> &'static str {
        match self {
            Self::ChatAgent => "agent.strategy@1.0.0",
            Self::CompletionProvider => "provider.completion-codec@1.0.0",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeManagementError {
    Invalid,
    NotFound,
    RevisionConflict,
    IdempotencyConflict,
    Busy,
    Unavailable,
    Internal,
}
impl RuntimeManagementError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "runtime_invalid",
            Self::NotFound => "runtime_not_found",
            Self::RevisionConflict => "runtime_revision_conflict",
            Self::IdempotencyConflict => "runtime_idempotency_conflict",
            Self::Busy => "runtime_busy",
            Self::Unavailable => "runtime_unavailable",
            Self::Internal => "runtime_internal",
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::Invalid => "Invalid runtime management request",
            Self::NotFound => "Runtime component not found",
            Self::RevisionConflict => "Runtime revision conflict",
            Self::IdempotencyConflict => "Runtime idempotency conflict",
            Self::Busy => "Runtime transition busy",
            Self::Unavailable => "Runtime component unavailable",
            Self::Internal => "Runtime management outcome unavailable",
        }
    }
}
impl std::fmt::Display for RuntimeManagementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for RuntimeManagementError {}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeDesired {
    pub scope: &'static str,
    pub enabled: bool,
    pub revision: u64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind")]
pub enum RuntimeState {
    #[serde(rename = "agent")]
    Agent {
        scope: &'static str,
        active: bool,
        admission_open: bool,
        in_flight: u64,
        generation: u64,
        draining: bool,
        transitioning: bool,
        effects: u64,
        recovery: u64,
        last_failure: Option<AgentError>,
    },
    #[serde(rename = "completion_provider")]
    CompletionProvider {
        scope: &'static str,
        active: bool,
        admission_open: bool,
        in_flight: u64,
        generation: u64,
        transitioning: bool,
        effects: u64,
        recovery: u64,
        lifecycle_failed: bool,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeComponentStatus {
    pub id: &'static str,
    pub capability: &'static str,
    pub desired: RuntimeDesired,
    pub runtime: RuntimeState,
    pub converged: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeMutationResult {
    pub receipt: PluginMutationReceipt,
    pub replayed: bool,
    pub current: RuntimeComponentStatus,
}

pub(crate) fn agent_status(
    desired: crate::plugin_management::PluginDesired,
    state: crate::agent_runtime::AgentRuntimeStatus,
) -> RuntimeComponentStatus {
    let converged = !state.transitioning
        && state.last_failure.is_none()
        && state.recovery == 0
        && if desired.enabled {
            state.active && state.admission_open && !state.draining
        } else {
            !state.active
                && !state.admission_open
                && !state.draining
                && state.in_flight == 0
                && state.effects == 0
        };
    RuntimeComponentStatus {
        id: RuntimeId::ChatAgent.as_str(),
        capability: RuntimeId::ChatAgent.capability(),
        desired: RuntimeDesired {
            scope: "host",
            enabled: desired.enabled,
            revision: desired.revision,
            updated_at: desired.updated_at,
        },
        runtime: RuntimeState::Agent {
            scope: "process",
            active: state.active,
            admission_open: state.admission_open,
            in_flight: state.in_flight,
            generation: state.generation,
            draining: state.draining,
            transitioning: state.transitioning,
            effects: state.effects,
            recovery: state.recovery,
            last_failure: state.last_failure,
        },
        converged,
    }
}

pub(crate) fn provider_status(
    desired: crate::plugin_management::PluginDesired,
    state: crate::provider_runtime::ProviderRuntimeStatus,
) -> RuntimeComponentStatus {
    let converged = !state.transitioning
        && !state.lifecycle_failed
        && state.recovery == 0
        && if desired.enabled {
            state.active && state.admission_open
        } else {
            !state.active && !state.admission_open && state.effects == 0
        };
    RuntimeComponentStatus {
        id: RuntimeId::CompletionProvider.as_str(),
        capability: RuntimeId::CompletionProvider.capability(),
        desired: RuntimeDesired {
            scope: "host",
            enabled: desired.enabled,
            revision: desired.revision,
            updated_at: desired.updated_at,
        },
        runtime: RuntimeState::CompletionProvider {
            scope: "process",
            active: state.active,
            admission_open: state.admission_open,
            in_flight: state.in_flight,
            generation: state.generation,
            transitioning: state.transitioning,
            effects: state.effects as u64,
            recovery: state.recovery as u64,
            lifecycle_failed: state.lifecycle_failed,
        },
        converged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::Engine, plugin_management::SetPluginEnabled, store::Store};
    use std::sync::Arc;

    #[test]
    fn replay_preserves_receipt_without_reapplying_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("runtime.sqlite")).unwrap());
        let engine = Engine::new(store, 2).unwrap();
        let off = SetPluginEnabled {
            enabled: false,
            expected_revision: 0,
        };
        let disabled = engine
            .set_runtime_enabled(RuntimeId::ChatAgent, off.clone(), "agent-off")
            .unwrap();
        assert!(!disabled.replayed);
        assert!(disabled.current.converged);
        let enabled = engine
            .set_runtime_enabled(
                RuntimeId::ChatAgent,
                SetPluginEnabled {
                    enabled: true,
                    expected_revision: 1,
                },
                "agent-on",
            )
            .unwrap();
        assert!(enabled.current.desired.enabled);
        let replay = engine
            .set_runtime_enabled(RuntimeId::ChatAgent, off.clone(), "agent-off")
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.receipt, disabled.receipt);
        assert!(replay.current.desired.enabled);
        assert!(replay.current.converged);
        assert_eq!(
            engine
                .set_runtime_enabled(RuntimeId::CompletionProvider, off, "agent-off")
                .unwrap_err(),
            RuntimeManagementError::IdempotencyConflict
        );
    }

    #[test]
    fn disabled_runtimes_restart_without_activation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("runtime.sqlite")).unwrap());
        let engine = Engine::new(store.clone(), 2).unwrap();
        for (id, key) in [
            (RuntimeId::ChatAgent, "agent-off"),
            (RuntimeId::CompletionProvider, "provider-off"),
        ] {
            engine
                .set_runtime_enabled(
                    id,
                    SetPluginEnabled {
                        enabled: false,
                        expected_revision: 0,
                    },
                    key,
                )
                .unwrap();
        }
        drop(engine);
        let restarted = Engine::new(store, 2).unwrap();
        for component in restarted.runtime_components().unwrap() {
            assert!(!component.desired.enabled);
            assert!(component.converged);
        }
        assert!(restarted.enable_builtin_agent().is_err());
        assert!(restarted.enable_builtin_provider().is_err());
    }
}
