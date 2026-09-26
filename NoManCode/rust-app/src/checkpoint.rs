use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointEntry {
    pub path: String,
    pub before_digest: Option<String>,
    pub after_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub checkpoint_id: String,
    pub task_id: String,
    pub kind: String,
    pub generation: u32,
    pub created_at: u64,
    pub manifest_digest: String,
    pub entries: Vec<CheckpointEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointCreated {
    pub checkpoint: Checkpoint,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointError {
    Invalid,
    NotFound,
    Conflict,
    Unrestorable,
    Corrupt,
    Internal,
}
impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "检查点请求无效",
            Self::NotFound => "检查点或任务不存在",
            Self::Conflict => "检查点状态冲突",
            Self::Unrestorable => "任务没有可信的检查点数据",
            Self::Corrupt => "检查点记录损坏",
            Self::Internal => "检查点操作失败",
        })
    }
}
impl std::error::Error for CheckpointError {}

/// Internal immutable source identity. Protected before bytes remain in workspace_changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub task_id: String,
    pub workspace: String,
    pub write_scopes: Vec<String>,
    pub source_digest: String,
    pub entries: Vec<CheckpointEntry>,
}

pub(crate) fn source_digest(
    records: &[crate::workspace_changes::PreparedChange],
) -> anyhow::Result<String> {
    let values: Vec<_> = records.iter().map(|r| serde_json::json!({
        "id":r.id,"task_id":r.task_id,"tool_call_id":r.tool_call_id,
        "workspace_digest":r.workspace_digest,"binding_digest":r.binding_digest,
        "write_scopes":r.write_scopes,"path":r.path,"path_key":r.path_key,
        "kind":r.kind,"before_identity":r.before_blob.as_deref().map(crate::workspace_changes::digest),
        "before_digest":r.before_digest,"after_digest":r.after_digest,"state":r.state
    })).collect();
    Ok(crate::workspace_changes::digest(&serde_json::to_vec(
        &values,
    )?))
}
