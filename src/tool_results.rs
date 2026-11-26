//! Internal provenance for non-claimed tool results.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    ReadCompleted,
    AgentDenied,
    HostDenied,
    ApprovalDenied,
    ApprovalCancelled,
    InterruptedWithoutClaim,
}

impl Origin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReadCompleted => "read_completed",
            Self::AgentDenied => "agent_denied",
            Self::HostDenied => "host_denied",
            Self::ApprovalDenied => "approval_denied",
            Self::ApprovalCancelled => "approval_cancelled",
            Self::InterruptedWithoutClaim => "interrupted_without_claim",
        }
    }
}

pub(crate) fn is_exploration(name: &str) -> bool {
    matches!(name, "explore_files" | "explore_parallel")
}

pub(crate) fn interrupted_message(tool_name: &str) -> &'static str {
    if tool_name == "explore_parallel" {
        "Parallel exploration was interrupted before its results were recorded. Dispatched model requests may have incurred usage."
    } else if tool_name == "explore_files" {
        "Exploration was interrupted before its summary was recorded. The model request may have incurred usage."
    } else {
        "Execution was interrupted. Inspect files before repeating a write or command."
    }
}

#[derive(Debug)]
pub(crate) enum ToolResultError {
    Conflict,
    Corrupt,
}

impl std::fmt::Display for ToolResultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict => f.write_str("tool result conflicts with persisted state"),
            Self::Corrupt => f.write_str("persisted tool result state is corrupt"),
        }
    }
}

impl std::error::Error for ToolResultError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommitDisposition {
    Inserted,
    Replayed,
}
