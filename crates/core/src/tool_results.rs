//! Internal provenance for non-claimed tool results.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    ReadCompleted,
    AgentDenied,
    HostDenied,
    ApprovalDenied,
    ApprovalCancelled,
    InterruptedWithoutClaim,
    HostTool,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadCompleted => "read_completed",
            Self::AgentDenied => "agent_denied",
            Self::HostDenied => "host_denied",
            Self::ApprovalDenied => "approval_denied",
            Self::ApprovalCancelled => "approval_cancelled",
            Self::InterruptedWithoutClaim => "interrupted_without_claim",
            Self::HostTool => "host_tool",
        }
    }
}

pub fn is_computer_read(name: &str) -> bool {
    matches!(name, "computer_observe" | "computer_wait")
}

pub fn is_computer(name: &str) -> bool {
    is_computer_read(name) || name == "computer_save_as"
}

pub fn is_exploration(name: &str) -> bool {
    matches!(name, "explore_files" | "explore_parallel")
}

pub fn interrupted_message(tool_name: &str) -> &'static str {
    if is_computer_read(tool_name) {
        "Desktop observation was interrupted. Request a new observation for the current state."
    } else if tool_name == "explore_parallel" {
        "Parallel exploration was interrupted before its results were recorded. Dispatched model requests may have incurred usage."
    } else if tool_name == "explore_files" {
        "Exploration was interrupted before its summary was recorded. The model request may have incurred usage."
    } else {
        "Execution was interrupted. Inspect files before repeating a write or command."
    }
}

#[derive(Debug)]
pub enum ToolResultError {
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
pub enum CommitDisposition {
    Inserted,
    Replayed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_tool_origin_has_stable_label() {
        assert_eq!(Origin::HostTool.as_str(), "host_tool");
    }
}
