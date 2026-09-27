//! Project-scoped, forward-only Completion admission and durable usage facts.
use crate::{domain::ProjectId, network_authorization::CallReceipt};
use serde::{Deserialize, Serialize};

pub const MAX_LIMIT_TOKENS: u64 = 1_000_000_000_000;
pub const MAX_RESERVATION_TOKENS: u64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfig {
    pub limit_tokens: u64,
    pub per_call_reservation_tokens: u64,
}
impl BudgetConfig {
    pub fn validate(&self) -> Result<(), BudgetError> {
        if self.limit_tokens > MAX_LIMIT_TOKENS
            || !(1..=MAX_RESERVATION_TOKENS).contains(&self.per_call_reservation_tokens)
        {
            return Err(BudgetErrorKind::Invalid.into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfigSnapshot {
    pub project_id: ProjectId,
    pub revision: u64,
    pub config: Option<BudgetConfig>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureBudget {
    pub project_id: ProjectId,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub config: BudgetConfig,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetMutationResult {
    pub snapshot: BudgetConfigSnapshot,
    pub replayed: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
impl BudgetUsage {
    pub fn validate(&self) -> Result<(), BudgetError> {
        if self.prompt_tokens > i64::MAX as u64
            || self.completion_tokens > i64::MAX as u64
            || self.total_tokens > i64::MAX as u64
            || self.prompt_tokens.checked_add(self.completion_tokens) != Some(self.total_tokens)
        {
            return Err(BudgetErrorKind::Invalid.into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetMode { Unconfigured, Configured }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetCallState { Claimed, Settled, NotSent, UsageMissing, UsageInvalid, Unknown, Overrun }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetBlockedReason { Unresolved, Overrun, Exceeded }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetCallReason {
    PreflightRejected, UsageMissing, UsageInvalid, TransportUnknown, Interrupted,
    PersistenceUnknown, ReservationOverrun,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetStatus {
    pub project_id: ProjectId,
    pub snapshot: BudgetConfigSnapshot,
    pub mode: BudgetMode,
    pub spent_tokens: u64,
    pub held_tokens: u64,
    pub available_tokens: Option<u64>,
    pub claimed_count: u64,
    pub unresolved_count: u64,
    pub overrun_count: u64,
    pub blocked_reason: Option<BudgetBlockedReason>,
    pub coverage: String,
    pub coverage_start_call_rowid: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetCall {
    pub project_id: ProjectId,
    pub call_id: String,
    pub call: CallReceipt,
    pub managed: bool,
    pub budget_revision: Option<u64>,
    pub reserved_tokens: Option<u64>,
    pub state: Option<BudgetCallState>,
    pub usage: Option<BudgetUsage>,
    pub reason: Option<BudgetCallReason>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetErrorKind {
    Invalid, NotFound, RevisionConflict, Conflict, Busy, Exceeded, Unresolved, Overrun,
    Corrupt, Internal,
}
impl BudgetErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "budget_invalid", Self::NotFound => "budget_not_found",
            Self::RevisionConflict => "budget_revision_conflict", Self::Conflict => "budget_conflict",
            Self::Busy => "budget_busy", Self::Exceeded => "budget_exceeded",
            Self::Unresolved => "budget_unresolved", Self::Overrun => "budget_overrun",
            Self::Corrupt => "budget_corrupt", Self::Internal => "budget_internal",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetError { pub kind: BudgetErrorKind }
impl From<BudgetErrorKind> for BudgetError {
    fn from(kind: BudgetErrorKind) -> Self { Self { kind } }
}
impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.kind.code()) }
}
impl std::error::Error for BudgetError {}
