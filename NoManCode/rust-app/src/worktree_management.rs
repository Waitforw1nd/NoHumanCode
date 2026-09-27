//! Explicit Git worktree creation receipts. Observation never grants execution.
use crate::domain::ProjectId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorktree {
    pub source_project_id: ProjectId,
    pub name: String,
    pub parent_path: String,
    pub expected_config_revision: u64,
    pub expected_head: String,
    pub idempotency_key: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    Claimed,
    Succeeded,
    Rejected,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeError {
    Invalid,
    NotFound,
    RevisionConflict,
    IdempotencyConflict,
    Busy,
    RootChanged,
    Conflict,
    Unsupported,
    Unavailable,
    Corrupt,
}
impl WorktreeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "worktree_invalid",
            Self::NotFound => "worktree_not_found",
            Self::RevisionConflict => "worktree_revision_conflict",
            Self::IdempotencyConflict => "worktree_idempotency_conflict",
            Self::Busy => "worktree_busy",
            Self::RootChanged => "worktree_root_changed",
            Self::Conflict => "worktree_conflict",
            Self::Unsupported => "worktree_unsupported",
            Self::Unavailable => "worktree_unavailable",
            Self::Corrupt => "worktree_corrupt",
        }
    }
}
impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for WorktreeError {}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCause {
    Invalid,
    NotFound,
    RevisionConflict,
    IdempotencyConflict,
    Busy,
    RootChanged,
    Conflict,
    Unsupported,
    Unavailable,
    Corrupt,
    ProcessOutcomeUnknown,
    RegistrationOutcomeUnknown,
    Interrupted,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeReceipt {
    pub schema_version: u32,
    pub operation_id: String,
    pub source_project_id: ProjectId,
    pub name: String,
    pub parent_path: String,
    pub target_path: String,
    pub branch: String,
    pub expected_config_revision: u64,
    pub expected_head: String,
    pub request_digest: String,
    pub revision: u64,
    pub state: WorktreeState,
    pub project_id: Option<ProjectId>,
    pub cause: Option<WorktreeCause>,
    pub created_at: i64,
    pub updated_at: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeObservationState {
    AbsentObserved,
    PresentMatching,
    Partial,
    Conflicting,
    IdentityChanged,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeObservationReason {
    Absent,
    MetadataMismatch,
    IdentityChanged,
    GitUnavailable,
    PartialArtifacts,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeObservation {
    pub operation_id: String,
    pub observed_at: i64,
    pub state: WorktreeObservationState,
    pub reason_codes: Vec<WorktreeObservationReason>,
    pub observed_head: Option<String>,
    pub observed_branch: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeInspection {
    pub receipt: WorktreeReceipt,
    pub observation: WorktreeObservation,
}
