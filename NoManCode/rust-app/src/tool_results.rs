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

#[cfg(test)]
pub(crate) mod crash_probe {
    use std::{
        path::PathBuf,
        sync::{Mutex, OnceLock},
        time::Duration,
    };

    struct Armed {
        database: String,
        task: String,
        call: String,
        phase: &'static str,
        signal: PathBuf,
    }

    static ARMED: OnceLock<Mutex<Vec<Armed>>> = OnceLock::new();

    #[allow(dead_code)]
    pub(crate) fn arm(
        database: String,
        task: String,
        call: String,
        phase: &'static str,
        signal: PathBuf,
    ) {
        ARMED
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .push(Armed {
                database,
                task,
                call,
                phase,
                signal,
            });
    }

    pub(crate) fn block(database: &str, task: &str, call: &str, phase: &'static str) {
        let armed = {
            let mut entries = ARMED.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap();
            entries
                .iter()
                .position(|entry| {
                    entry.database == database
                        && entry.task == task
                        && entry.call == call
                        && entry.phase == phase
                })
                .map(|index| entries.remove(index))
        };
        if let Some(entry) = armed {
            let staging = entry.signal.with_extension("tmp");
            std::fs::write(&staging, format!("{task}\n{call}\n{phase}\n")).unwrap();
            std::fs::rename(staging, entry.signal).unwrap();
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
    }
}
