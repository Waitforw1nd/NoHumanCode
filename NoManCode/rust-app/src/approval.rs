//! Capability classification and one-shot tool-call approval records.
//!
//! New tool names are denied until explicitly classified. Read-only project tools and
//! the current no-import WASM tool pass through. This module does not read
//! files, the environment, the network, or the clock.

use crate::{domain::scope_path, secrets};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolEffect {
    None,
    WriteFs,
    ExecCommand,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyDecision {
    Allow,
    RequireApproval,
    Deny { reason: &'static str },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    NotStarted,
    Claimed,
    Finished,
    Unknown,
    Cancelled,
}

impl ExecutionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Claimed => "claimed",
            Self::Finished => "finished",
            Self::Unknown => "unknown",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "not_started" => Self::NotStarted,
            "claimed" => Self::Claimed,
            "finished" => Self::Finished,
            "unknown" => Self::Unknown,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "approved" => Self::Approved,
            "denied" => Self::Denied,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub id: String,
    pub tool_call_id: String,
    pub task_id: String,
    pub turn_id: Option<String>,
    pub session_id: Option<String>,
    pub tool_name: String,
    pub args_digest: String,
    pub binding_digest: String,
    pub workspace: String,
    pub write_scopes: Vec<String>,
    pub allow_commands: bool,
    pub preview: String,
    pub status: ApprovalStatus,
    pub created_at: u64,
    pub decided_at: Option<u64>,
    pub decided_by: Option<String>,
    pub execution_state: ExecutionState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApprovalError {
    NotFound { id: String },
    Conflict { id: String, status: String },
    BindingConflict { id: String },
    UnknownResult { id: String },
    CorruptState { id: String },
}

impl fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { .. } => f.write_str("审批不存在"),
            Self::Conflict { .. } => f.write_str("审批当前状态不能执行此操作"),
            Self::BindingConflict { .. } => f.write_str("审批绑定不匹配，工具未执行。"),
            Self::UnknownResult { .. } => f.write_str("工具执行结果未知，已停止后续执行。"),
            Self::CorruptState { .. } => f.write_str("审批持久状态损坏，不能继续执行。"),
        }
    }
}

impl std::error::Error for ApprovalError {}

pub fn classify(tool_name: &str) -> ToolEffect {
    match tool_name {
        "list_files" | "read_file" | "search_files" | "run_wasm" => ToolEffect::None,
        "write_file" => ToolEffect::WriteFs,
        "run_command" => ToolEffect::ExecCommand,
        _ => ToolEffect::Unknown,
    }
}

pub fn evaluate(_task: &crate::domain::Task, tool_name: &str) -> PolicyDecision {
    match classify(tool_name) {
        ToolEffect::None => PolicyDecision::Allow,
        ToolEffect::WriteFs | ToolEffect::ExecCommand => PolicyDecision::RequireApproval,
        ToolEffect::Unknown => PolicyDecision::Deny {
            reason: "未分类工具拒绝执行",
        },
    }
}

pub fn args_digest(args: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(&canonical_json(args))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(crate) fn validate_call_history(messages: &[Value]) -> Result<()> {
    let mut ids = std::collections::HashSet::new();
    let mut answers = std::collections::HashSet::new();
    for message in messages {
        if let Some(calls) = message.get("tool_calls") {
            let calls = calls
                .as_array()
                .ok_or_else(|| ApprovalError::CorruptState { id: String::new() })?;
            for call in calls {
                let id = call["id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| ApprovalError::CorruptState { id: String::new() })?;
                if message["role"] != "assistant" || !ids.insert(id) {
                    return Err(ApprovalError::CorruptState { id: id.into() }.into());
                }
            }
        }
        if message["role"] == "tool" {
            let id = message["tool_call_id"].as_str().unwrap_or_default();
            if !ids.contains(id) || !answers.insert(id) {
                return Err(ApprovalError::CorruptState { id: id.into() }.into());
            }
        }
    }
    Ok(())
}

/// The digest covers every value which can change the authority granted by an
/// approval. The structured JSON is canonicalized before hashing.
#[allow(clippy::too_many_arguments)]
pub fn binding_digest(
    task_id: &str,
    session_id: Option<&str>,
    turn_id: Option<&str>,
    tool_call_id: &str,
    tool_name: &str,
    workspace: &str,
    write_scopes: &[String],
    allow_commands: bool,
    args: &Value,
) -> Result<String> {
    let value = serde_json::json!({
        "version": "approval-binding-v2",
        "task_id": task_id,
        "session_id": session_id,
        "turn_id": turn_id,
        "tool_call_id": tool_call_id,
        "tool_name": tool_name,
        "workspace": workspace,
        "write_scopes": write_scopes,
        "allow_commands": allow_commands,
        "args_digest": args_digest(args)?,
    });
    let bytes = serde_json::to_vec(&canonical_json(&value))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn preview(tool_name: &str, args: &Value) -> String {
    let text = match tool_name {
        "write_file" => {
            let path = args["path"].as_str().unwrap_or("");
            let normalized = scope_path(path).unwrap_or_else(|_| "[invalid-path]".into());
            let bytes = args["content"].as_str().map(|text| text.len()).unwrap_or(0);
            format!("write_file {normalized} ({bytes} bytes)")
        }
        "run_command" => {
            let command = args["command"].as_str().unwrap_or("");
            truncate_chars(&redacted_text(&strip_env_refs(command)), 160)
        }
        other => other.to_owned(),
    };
    redacted_text(&text)
}

fn redacted_text(value: &str) -> String {
    match secrets::redact_persisted(&Value::String(value.to_owned())) {
        Value::String(text) => text,
        _ => value.to_owned(),
    }
}

fn strip_env_refs(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '$' && chars.get(index + 1..index + 5) == Some(&['e', 'n', 'v', ':']) {
            index += 5;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
            {
                index += 1;
            }
            out.push_str("[redacted]");
            continue;
        }
        if chars[index] == '%' {
            let mut end = index + 1;
            while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            if end > index + 1 && chars.get(end) == Some(&'%') {
                out.push_str("[redacted]");
                index = end + 1;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

pub fn tool_error(status: ApprovalStatus) -> String {
    match status {
        ApprovalStatus::Denied => "审批已拒绝，工具未执行。".into(),
        ApprovalStatus::Cancelled => "审批已取消，工具未执行。".into(),
        ApprovalStatus::Pending | ApprovalStatus::Approved => "审批状态不能作为工具结果。".into(),
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let limit = if value.chars().count() > max_chars {
        max_chars.saturating_sub(1)
    } else {
        max_chars
    };
    for ch in value.chars().take(limit) {
        out.push(ch);
    }
    if value.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut ordered = serde_json::Map::new();
            for key in keys {
                if let Some(child) = map.get(&key) {
                    ordered.insert(key, canonical_json(child));
                }
            }
            Value::Object(ordered)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classification_defaults_unknown_tools_to_approval_deny() {
        assert_eq!(classify("read_file"), ToolEffect::None);
        assert_eq!(classify("write_file"), ToolEffect::WriteFs);
        assert_eq!(classify("run_command"), ToolEffect::ExecCommand);
        assert_eq!(classify("new_tool"), ToolEffect::Unknown);
        let task = crate::domain::Task {
            id: "t".into(),
            run_id: "r".into(),
            spec: crate::domain::TaskSpec {
                name: "worker".into(),
                role: "role".into(),
                route_id: "route".into(),
                prompt: "prompt".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 2,
            },
            route: crate::domain::Route {
                id: "route".into(),
                name: "route".into(),
                base_url: "http://127.0.0.1:1".into(),
                model: "m".into(),
                max_tokens: 128,
                parallel_limit: 1,
                key_env: None,
            },
            workspace: "workspace".into(),
            status: "running".into(),
            output: String::new(),
            error: None,
            messages: vec![],
            usage: serde_json::Value::Null,
            created_at: 1,
            updated_at: 1,
        };
        assert!(matches!(
            evaluate(&task, "write_file"),
            PolicyDecision::RequireApproval
        ));
        assert!(matches!(
            evaluate(&task, "new_tool"),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            evaluate(&task, "list_files"),
            PolicyDecision::Allow
        ));
    }

    #[test]
    fn digest_is_stable_and_preview_omits_secret_and_body() {
        let first = json!({"b": 1, "a": {"z": 2, "y": 3}});
        let second = json!({"a": {"y": 3, "z": 2}, "b": 1});
        assert_eq!(args_digest(&first).unwrap(), args_digest(&second).unwrap());
        assert_ne!(
            args_digest(&json!({"path":"src/a.txt"})).unwrap(),
            args_digest(&json!({"path":"src/b.txt"})).unwrap()
        );
        let write_preview = preview(
            "write_file",
            &json!({"path":"src/secret.txt","content":"synthetic-key-body"}),
        );
        assert!(!write_preview.contains("synthetic-key-body"));
        assert!(write_preview.contains("src/secret.txt"));
        let command = preview(
            "run_command",
            &json!({"command":"Write-Output Bearer synthetic-token-value"}),
        );
        assert!(!command.contains("synthetic-token-value"));
        assert!(command.chars().count() <= 160);
    }
}
