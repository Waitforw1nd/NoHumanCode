use serde::{Deserialize, Serialize};
use std::fmt;

pub(crate) fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedChange {
    pub id: String,
    pub task_id: String,
    pub tool_call_id: String,
    pub workspace_digest: String,
    pub binding_digest: String,
    pub write_scopes: String,
    pub path: String,
    pub path_key: String,
    pub kind: ChangeKind,
    pub before_blob: Option<Vec<u8>>,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub state: ChangeState,
    pub restore_state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeState {
    Prepared,
    Finished,
    Unknown,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentState {
    Matches,
    Missing,
    Diverged,
    Unreadable,
    Invalid,
    UnsafeLink,
    Unknown,
    Restored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreStatus {
    Claimed,
    Complete,
    Conflict,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceChange {
    pub change_id: String,
    pub task_id: String,
    pub path: String,
    pub kind: ChangeKind,
    pub before_digest: Option<String>,
    pub after_digest: String,
    pub state: ChangeState,
    pub restore_state: String,
    pub current: CurrentState,
    pub restorable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePathOutcome {
    pub change_id: String,
    pub path: String,
    pub status: RestoreStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReceipt {
    pub restore_id: Option<String>,
    pub task_id: String,
    pub status: RestoreStatus,
    pub restored: usize,
    pub outcomes: Vec<RestorePathOutcome>,
}

impl RestoreReceipt {
    pub fn ok(&self) -> bool {
        self.status == RestoreStatus::Complete
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceChangeError {
    NotFound,
    Active,
    Conflict { receipt: Option<RestoreReceipt> },
    Unrestorable,
    Unknown { receipt: Option<RestoreReceipt> },
    Corrupt,
    Internal,
}

impl fmt::Display for WorkspaceChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "任务不存在",
            Self::Active => "任务仍在运行",
            Self::Conflict { .. } => "工作区内容已变化",
            Self::Unrestorable => "变更没有可信的恢复数据",
            Self::Unknown { .. } => "恢复结果未知",
            Self::Corrupt => "变更记录损坏",
            Self::Internal => "文件恢复失败",
        })
    }
}

impl std::error::Error for WorkspaceChangeError {}
