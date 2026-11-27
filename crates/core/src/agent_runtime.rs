//! Lightweight builtin agent strategy runtime without PluginHost ceremony.
use crate::messages::NeutralMessage;
use serde::Serialize;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

pub const AGENT_PROVIDER_ID: &str = "builtin.chat-agent";
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

pub type StrategyError = AgentError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolClass {
    Read,
    List,
    Search,
    Write,
    Command,
    Other,
}

pub enum StrategyInput<'a> {
    BeforeCompletion { tools: &'a [ToolClass] },
    AfterCompletion { outcome: CompletionKind<'a> },
}

pub enum CompletionKind<'a> {
    NoToolCalls,
    ToolGroup(&'a [ToolClass]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolVote {
    ApplyHostPolicy,
    #[allow(dead_code)]
    Deny,
}

#[derive(Debug)]
pub enum StrategyAction {
    UseTools(Vec<usize>),
    ToolVotes(Vec<ToolVote>),
    Finish,
    #[allow(dead_code)]
    Stop,
}

pub(crate) trait AgentStrategy: Send + Sync {
    fn decide(
        &self,
        input: StrategyInput<'_>,
        messages: &[NeutralMessage],
        round: usize,
        max_rounds: usize,
    ) -> Result<StrategyAction, StrategyError>;
}

struct Builtin;
impl AgentStrategy for Builtin {
    fn decide(
        &self,
        input: StrategyInput<'_>,
        _: &[NeutralMessage],
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

pub struct AgentRuntime {
    enabled: AtomicBool,
    in_flight: Arc<AtomicU64>,
    generation: AtomicU64,
    last_failure: Arc<Mutex<Option<AgentError>>>,
    strategy: Arc<dyn AgentStrategy>,
}

pub struct AgentLease {
    strategy: Arc<dyn AgentStrategy>,
    in_flight: Arc<AtomicU64>,
    last_failure: Arc<Mutex<Option<AgentError>>>,
}

impl AgentRuntime {
    pub fn builtin_with_state(enabled: bool) -> anyhow::Result<Self> {
        Self::with_strategy_and_state(Arc::new(Builtin), enabled)
    }

    fn with_strategy_and_state(
        strategy: Arc<dyn AgentStrategy>,
        enabled: bool,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            enabled: AtomicBool::new(enabled),
            in_flight: Arc::new(AtomicU64::new(0)),
            generation: AtomicU64::new(1),
            last_failure: Arc::new(Mutex::new(None)),
            strategy,
        })
    }

    pub fn acquire_unpersisted(&self) -> Result<AgentLease, AgentError> {
        if !self.enabled.load(Ordering::SeqCst) {
            return Err(AgentError::Unavailable);
        }
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        Ok(AgentLease {
            strategy: self.strategy.clone(),
            in_flight: self.in_flight.clone(),
            last_failure: self.last_failure.clone(),
        })
    }

    pub fn status(&self) -> Result<AgentRuntimeStatus, AgentError> {
        let admission_open = self.enabled.load(Ordering::SeqCst);
        let in_flight = self.in_flight.load(Ordering::SeqCst);
        let active = admission_open || in_flight > 0;
        let last_failure = *self.last_failure.lock().map_err(|_| AgentError::Internal)?;
        Ok(AgentRuntimeStatus {
            plugin_id: AGENT_PROVIDER_ID,
            service: SERVICE,
            interface_version: "1.0.0",
            scope: "process",
            active,
            admission_open,
            in_flight,
            generation: self.generation.load(Ordering::SeqCst),
            draining: !admission_open && in_flight > 0,
            transitioning: false,
            effects: u64::from(active),
            recovery: 0,
            last_failure,
        })
    }

    pub fn disable(&self) -> Result<(), AgentError> {
        if self.enabled.swap(false, Ordering::SeqCst) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    pub fn enable_unpersisted(&self) -> Result<(), AgentError> {
        if self.enabled.load(Ordering::SeqCst) {
            return Ok(());
        }
        if self.in_flight.load(Ordering::SeqCst) != 0 {
            return Err(AgentError::Busy);
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut failure) = self.last_failure.lock() {
            *failure = None;
        }
        self.enabled.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn in_flight_count(&self) -> u64 {
        self.in_flight.load(Ordering::SeqCst)
    }

    pub fn generation_value(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn apply_desired_enabled(&self, enabled: bool) {
        if enabled {
            if !self.enabled.swap(true, Ordering::SeqCst) {
                self.generation.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut failure) = self.last_failure.lock() {
                    *failure = None;
                }
            }
        } else if self.enabled.swap(false, Ordering::SeqCst) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl AgentLease {
    fn record_failure(&self, error: AgentError) {
        if let Ok(mut slot) = self.last_failure.lock() {
            *slot = Some(error);
        }
    }

    pub fn decide(
        &self,
        input: StrategyInput<'_>,
        messages: &[NeutralMessage],
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
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.strategy.decide(input, messages, round, max_rounds)
        }))
        .map_err(|_| AgentError::Panicked)
        .and_then(|r| r);
        let action = match result {
            Ok(action) => action,
            Err(error) => {
                self.record_failure(error);
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
            self.record_failure(AgentError::InvalidDecision);
            return Err(AgentError::InvalidDecision);
        }
        if matches!(action, StrategyAction::Stop) {
            self.record_failure(AgentError::Stopped);
            return Err(AgentError::Stopped);
        }
        Ok(action)
    }
}

impl Drop for AgentLease {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}
