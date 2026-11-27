//! Runtime abstraction seams decoupling the agent execution loop from CLI/SQLite/Host internals.

use crate::agent_runtime::{AgentError, AgentLease, StrategyAction, StrategyInput, ToolVote};
use crate::domain::{Route, Task};
use crate::messages::{NeutralMessage, ToolCallBlock};
use crate::provider_runtime::{CompletionInput, Emit};
use crate::redaction::CredentialSet;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::future::Future;
use tokio_util::sync::CancellationToken;

/// Opaque concurrency/network slot held by a running task.
pub trait SlotGuard: Send {
    fn release(&mut self);
}

/// Resolution result for task predecessor dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyState {
    Ready(Vec<DependencySummary>),
    Pending,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DependencySummary {
    pub member: String,
    pub role: String,
    pub model: String,
    pub result: String,
}

/// Token usage breakdown compatible with `codaKernel/1` `KernelUsage`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KernelUsageBreakdown {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// Outcome of a single streaming model completion turn.
#[derive(Debug, Clone)]
pub struct CompletionTurnResult {
    pub message: NeutralMessage,
    pub usage: Value,
}

/// Outcome including optional protocol-level token breakdown.
#[derive(Debug, Clone)]
pub struct DetailedCompletionTurnResult {
    pub turn: CompletionTurnResult,
    pub usage_breakdown: Option<KernelUsageBreakdown>,
    pub response_id: Option<String>,
}

/// Opaque per-round tool catalog prepared by `ToolExecutor`.
pub struct PreparedTools {
    pub definitions: Vec<Value>,
    pub backend_state: Box<dyn std::any::Any + Send + Sync>,
}

/// Outcome of executing an assistant message's tool call batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchOutcome {
    Continue,
    TerminateRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundDirective {
    Continue,
    Finish,
}

#[derive(Debug, Clone)]
pub struct PreparedModelContext {
    pub messages: Vec<NeutralMessage>,
    pub model_id: String,
    pub max_tokens: u32,
}

#[derive(Debug)]
pub enum OverflowAction {
    NoRetry,
    RetryWithContext(PreparedModelContext),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnCommitOutcome {
    Continue,
    Finish,
}

/// Pure turn-control seam allowing embedded mode to share `agent_loop::run_task`
/// without polluting core modules with HTTP, SSE, or session-store types.
pub trait RunTurnController: Send + Sync {
    fn is_embedded(&self) -> bool {
        false
    }

    fn before_model_turn<'a>(
        &'a self,
        _task: &'a mut Task,
        _round: usize,
        _cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<RoundDirective>> + Send + 'a {
        async { Ok(RoundDirective::Continue) }
    }

    fn prepare_model_context<'a>(
        &'a self,
        _task: &'a mut Task,
        _available_tools: &'a [Value],
        _round: usize,
        _cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<Option<PreparedModelContext>>> + Send + 'a {
        async { Ok(None) }
    }

    fn emit_completion_delta(&self, _task_id: &str, _delta: &str) -> Result<()> {
        Ok(())
    }

    fn emit_reasoning_delta(&self, _task_id: &str, _delta: &str) -> Result<()> {
        Ok(())
    }

    fn finish_streaming_blocks(&self) -> Result<()> {
        Ok(())
    }

    fn try_overflow_compaction<'a>(
        &'a self,
        _task: &'a mut Task,
        _err: &'a anyhow::Error,
        _available_tools: &'a [Value],
        _round: usize,
        _cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<OverflowAction>> + Send + 'a {
        async { Ok(OverflowAction::NoRetry) }
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_assistant_turn<'a>(
        &'a self,
        _task: &'a mut Task,
        _message: &'a mut NeutralMessage,
        _usage: &'a Value,
        _usage_breakdown: Option<&'a KernelUsageBreakdown>,
        _response_id: Option<&'a str>,
        _available_tools: &'a [Value],
        _cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        async { Ok(()) }
    }

    fn after_tool_batch<'a>(
        &'a self,
        _task: &'a mut Task,
        has_tool_calls: bool,
        batch_terminated: bool,
        _round: usize,
        _cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<TurnCommitOutcome>> + Send + 'a {
        async move {
            Ok(if has_tool_calls && !batch_terminated {
                TurnCommitOutcome::Continue
            } else {
                TurnCommitOutcome::Finish
            })
        }
    }

    fn on_aborted<'a>(
        &'a self,
        _task: &'a mut Task,
        _partial_text: &'a str,
        _partial_thinking: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        async { Ok(()) }
    }

    fn on_run_error<'a>(
        &'a self,
        _task: &'a mut Task,
        _err: &'a anyhow::Error,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        async { Ok(()) }
    }

    fn on_max_rounds_reached<'a>(
        &'a self,
        _task: &'a mut Task,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        async { Ok(()) }
    }
}

pub struct NoopTurnController;

impl RunTurnController for NoopTurnController {}

/// Strategy decider for `BeforeCompletion` / `AfterCompletion`.
pub trait StrategyDecider: Send + Sync {
    fn decide(
        &self,
        input: StrategyInput<'_>,
        messages: &[NeutralMessage],
        round: usize,
        max_rounds: usize,
    ) -> std::result::Result<StrategyAction, AgentError>;
}

impl StrategyDecider for AgentLease {
    fn decide(
        &self,
        input: StrategyInput<'_>,
        messages: &[NeutralMessage],
        round: usize,
        max_rounds: usize,
    ) -> std::result::Result<StrategyAction, AgentError> {
        AgentLease::decide(self, input, messages, round, max_rounds)
    }
}

/// Seam 1: EventSink — synchronous emission of deltas and structured events.
pub trait EventSink: Send + Sync {
    fn completion_delta(&self, task_id: &str, text: &str) -> Result<()>;
    fn reasoning_delta(&self, task_id: &str, text: &str) -> Result<()>;
    fn emit_event(&self, task_id: &str, kind: &str, data: Value) -> Result<()>;
}

/// Seam 2: TaskRepository — durable or in-memory task state, dependencies, and resume inputs.
pub trait TaskRepository: Send + Sync {
    fn check_dependencies(&self, task: &Task) -> Result<DependencyState>;
    fn save_running_snapshot(&self, task: &Task) -> Result<()>;
    fn save_assistant_message(&self, task: &Task, has_tool_calls: bool) -> Result<()>;
    fn consume_resume_inputs(&self, task: &Task) -> Result<Task>;
}

/// Seam 3: CredentialSource — credential resolution, slot acquisition, and authorized model completion.
pub trait CredentialSource: Send + Sync {
    fn credential_set(&self, route: &Route) -> Result<CredentialSet>;

    fn acquire_slot<'a>(
        &'a self,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<Box<dyn SlotGuard>>> + Send + 'a;

    fn complete_streaming<'a>(
        &'a self,
        task: &'a Task,
        route: &'a Route,
        input: &'a CompletionInput<'a>,
        emit_text: &'a mut Emit<'a>,
        emit_reasoning: &'a mut Emit<'a>,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<CompletionTurnResult>> + Send + 'a;

    fn complete_streaming_detailed<'a>(
        &'a self,
        task: &'a Task,
        route: &'a Route,
        input: &'a CompletionInput<'a>,
        emit_text: &'a mut Emit<'a>,
        emit_reasoning: &'a mut Emit<'a>,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<DetailedCompletionTurnResult>> + Send + 'a {
        async move {
            let turn = self
                .complete_streaming(task, route, input, emit_text, emit_reasoning, cancel)
                .await?;
            Ok(DetailedCompletionTurnResult {
                turn,
                usage_breakdown: None,
                response_id: None,
            })
        }
    }
}

/// Seam 4: ToolExecutor — open-tool reconciliation, round tool discovery, and batch execution.
pub trait ToolExecutor: Send + Sync {
    fn reconcile_open_tools<'a>(
        &'a self,
        task: &'a mut Task,
        keys: &'a CredentialSet,
        slot: &'a mut Box<dyn SlotGuard>,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn prepare_round_tools<'a>(
        &'a self,
        task: &'a Task,
        events: &'a dyn EventSink,
        announced_logins: &'a mut HashSet<String>,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<PreparedTools>> + Send + 'a;

    #[allow(clippy::too_many_arguments)]
    fn execute_tool_batch<'a>(
        &'a self,
        task: &'a mut Task,
        calls: &'a [ToolCallBlock],
        votes: &'a [ToolVote],
        advertised_tools: &'a [Value],
        prepared: &'a PreparedTools,
        keys: &'a CredentialSet,
        slot: &'a mut Box<dyn SlotGuard>,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<BatchOutcome>> + Send + 'a;
}
