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
