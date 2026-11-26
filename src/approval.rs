//! Capability classification, write-scope policy checks, and one-shot tool-call approval records.

use crate::{domain::scope_path, secrets};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fmt, path::Path};

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
#[serde(deny_unknown_fields)]
pub struct CommandNetworkNotice {
    pub version: u32,
    pub may_access_network: bool,
    pub outbound_sandbox: bool,
}

impl Default for CommandNetworkNotice {
    fn default() -> Self {
        Self {
            version: 1,
            may_access_network: true,
            outbound_sandbox: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandNetworkAck {
    #[default]
    NotRequired,
    Required,
    Acknowledged,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    #[serde(default)]
    pub network_notice: Option<CommandNetworkNotice>,
    #[serde(default)]
    pub command_network_ack: CommandNetworkAck,
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
    NetworkAcknowledgementRequired { id: String },
    InvalidNetworkAcknowledgement { id: String },
    NotFound { id: String },
    Conflict { id: String, status: String },
    BindingConflict { id: String },
    UnknownResult { id: String },
    CorruptState { id: String },
}

impl fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NetworkAcknowledgementRequired { .. } => {
                f.write_str("命令可能访问任意网络，必须明确确认当前没有OS出站沙箱")
            }
            Self::InvalidNetworkAcknowledgement { .. } => f.write_str("此审批不接受命令联网确认"),
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
        "list_files" | "glob_files" | "read_file" | "explore_files" | "explore_parallel"
        | "search_files" | "git_status" | "todo_write" | "todo_read" => ToolEffect::None,
        name if crate::tool_results::is_computer(name) => ToolEffect::None,
        "write_file" | "edit_file" => ToolEffect::WriteFs,
        "run_command" => ToolEffect::ExecCommand,
        _ => ToolEffect::Unknown,
    }
}

pub fn within_write_scopes(path: &str, scopes: &[String]) -> bool {
    let Ok(normalized) = scope_path(path) else {
        return false;
    };
    crate::workspace::within_write_scopes(&normalized, scopes)
}

pub fn evaluate(task: &crate::domain::Task, tool_name: &str) -> PolicyDecision {
    if crate::tool_results::is_computer(tool_name) && !task.spec.tools {
        return PolicyDecision::Deny {
            reason: "当前任务未授权使用桌面工具",
        };
    }
    match classify(tool_name) {
        ToolEffect::None => PolicyDecision::Allow,
        ToolEffect::WriteFs => {
            if task.spec.write_scopes.is_empty() {
                PolicyDecision::Deny {
                    reason: "当前任务无文件写入范围",
                }
            } else {
                PolicyDecision::RequireApproval
            }
        }
        ToolEffect::ExecCommand => PolicyDecision::RequireApproval,
        ToolEffect::Unknown => PolicyDecision::Deny {
            reason: "未分类工具拒绝执行",
        },
    }
}

pub fn evaluate_call(task: &crate::domain::Task, tool_name: &str, args: &Value) -> PolicyDecision {
    let base = evaluate(task, tool_name);
    if base != PolicyDecision::RequireApproval || classify(tool_name) != ToolEffect::WriteFs {
        return base;
    }
    let Some(path) = args["path"].as_str() else {
        return PolicyDecision::Deny {
            reason: "文件修改工具缺少有效 path",
        };
    };
    if !within_write_scopes(path, &task.spec.write_scopes) {
        return PolicyDecision::Deny {
            reason: "写入超出当前成员范围",
        };
    }
    PolicyDecision::RequireApproval
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
    binding_digest_from_args_digest(
        task_id,
        session_id,
        turn_id,
        tool_call_id,
        tool_name,
        workspace,
        write_scopes,
        allow_commands,
        &args_digest(args)?,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn binding_digest_from_args_digest(
    task_id: &str,
    session_id: Option<&str>,
    turn_id: Option<&str>,
    tool_call_id: &str,
    tool_name: &str,
    workspace: &str,
    write_scopes: &[String],
    allow_commands: bool,
    args_digest: &str,
) -> Result<String> {
    anyhow::ensure!(
        args_digest.len() == 64
            && args_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid argument digest"
    );
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
        "args_digest": args_digest,
    });
    let bytes = serde_json::to_vec(&canonical_json(&value))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn compact_diff_lines(diff: &str, max_lines: usize, max_chars: usize) -> String {
    let snippet: String = diff
        .lines()
        .filter(|line| !line.starts_with("--- ") && !line.starts_with("+++ "))
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n");
    truncate_chars(&redacted_text(&snippet), max_chars)
}

pub fn preview(tool_name: &str, args: &Value) -> String {
    let text = match tool_name {
        "write_file" => {
            let path = args["path"].as_str().unwrap_or("");
            let normalized = scope_path(path).unwrap_or_else(|_| "[invalid-path]".into());
            let content = args["content"].as_str().unwrap_or("");
            let bytes = content.len();
            let lines = content.lines().count();
            format!("write_file {normalized} ({bytes} bytes, {lines} lines)")
        }
        "edit_file" => {
            let path = args["path"].as_str().unwrap_or("");
            let normalized = scope_path(path).unwrap_or_else(|_| "[invalid-path]".into());
            if let Some(patch) = args.get("patch").and_then(Value::as_str) {
                let snippet = compact_diff_lines(patch, 6, 160);
                if snippet.is_empty() {
                    format!("edit_file {normalized} (patch)")
                } else {
                    format!("edit_file {normalized} (patch)\n{snippet}")
                }
            } else if let Some(edits) = args.get("edits").and_then(Value::as_array) {
                let first_old = edits
                    .first()
                    .and_then(|e| e["old_string"].as_str())
                    .unwrap_or("");
                let first_new = edits
                    .first()
                    .and_then(|e| e["new_string"].as_str())
                    .unwrap_or("");
                let diff = crate::git_diff::unified_diff(&normalized, first_old, first_new, 1);
                let snippet = compact_diff_lines(&diff, 6, 160);
                if snippet.is_empty() {
                    format!("edit_file {normalized} ({} edits)", edits.len())
                } else {
                    format!("edit_file {normalized} ({} edits)\n{snippet}", edits.len())
                }
            } else {
                let old_s = args["old_string"].as_str().unwrap_or("");
                let new_s = args["new_string"].as_str().unwrap_or("");
                let diff = crate::git_diff::unified_diff(&normalized, old_s, new_s, 1);
                let snippet = compact_diff_lines(&diff, 6, 160);
                if snippet.is_empty() {
                    format!("edit_file {normalized}")
                } else {
                    format!("edit_file {normalized}\n{snippet}")
                }
            }
        }
        "run_command" => {
            let command = args["command"].as_str().unwrap_or("");
            truncate_chars(&redacted_text(&strip_env_refs(command)), 160)
        }
        other => other.to_owned(),
    };
    redacted_text(&text)
}

/// Generates a rich approval summary including a real unified diff snippet against the workspace.
pub fn summary(task: &crate::domain::Task, tool_name: &str, args: &Value) -> String {
    if matches!(tool_name, "write_file" | "edit_file") {
        let root = Path::new(&task.workspace);
        if let Ok(diff) =
            crate::workspace::preview_file_diff(root, tool_name, args, &task.spec.write_scopes)
        {
            let path = args["path"].as_str().unwrap_or("");
            let normalized = scope_path(path).unwrap_or_else(|_| "[invalid-path]".into());
            let snippet = compact_diff_lines(&diff, 12, 320);
            if !snippet.is_empty() {
                return redacted_text(&format!("{tool_name} {normalized}\n{snippet}"));
            }
        }
    }
    preview(tool_name, args)
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
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_owned();
    }
    let mut out: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
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
    fn binding_from_digest_matches_v2_and_rejects_invalid_digest() {
        let args = json!({"path":"a","content":"x"});
        let scopes = vec!["src".to_owned(), "tests".to_owned()];
        let direct = binding_digest(
            "task",
            None,
            Some("turn"),
            "call",
            "write_file",
            "workspace",
            &scopes,
            false,
            &args,
        )
        .unwrap();
        let from_digest = binding_digest_from_args_digest(
            "task",
            None,
            Some("turn"),
            "call",
            "write_file",
            "workspace",
            &scopes,
            false,
            &args_digest(&args).unwrap(),
        )
        .unwrap();
        assert_eq!(direct, from_digest);
        assert_eq!(
            direct,
            "6dac927da50ce55561b4f19217fd4e6ad43bde0f1bee743bc47ac4d53a6a9189"
        );
    }

    #[test]
    fn classifies_modern_toolset_and_includes_diff_summary() {
        assert_eq!(classify("list_files"), ToolEffect::None);
        assert_eq!(classify("glob_files"), ToolEffect::None);
        assert_eq!(classify("read_file"), ToolEffect::None);
        assert_eq!(classify("search_files"), ToolEffect::None);
        assert_eq!(classify("git_status"), ToolEffect::None);
        assert_eq!(classify("todo_write"), ToolEffect::None);
        assert_eq!(classify("todo_read"), ToolEffect::None);
        assert_eq!(classify("write_file"), ToolEffect::WriteFs);
        assert_eq!(classify("edit_file"), ToolEffect::WriteFs);
        assert_eq!(classify("run_command"), ToolEffect::ExecCommand);
        assert_eq!(classify("run_wasm"), ToolEffect::Unknown);

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/app.rs"),
            "fn main() {\n    old_call();\n}\n",
        )
        .unwrap();

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
            workspace: dir.path().to_string_lossy().into_owned(),
            status: "running".into(),
            output: String::new(),
            error: None,
            messages: vec![],
            usage: serde_json::Value::Null,
            created_at: 1,
            updated_at: 1,
        };

        assert_eq!(evaluate(&task, "glob_files"), PolicyDecision::Allow);
        assert_eq!(evaluate(&task, "git_status"), PolicyDecision::Allow);
        assert_eq!(evaluate(&task, "todo_write"), PolicyDecision::Allow);
        assert_eq!(evaluate(&task, "todo_read"), PolicyDecision::Allow);
        assert_eq!(
            evaluate(&task, "edit_file"),
            PolicyDecision::RequireApproval
        );
        assert!(within_write_scopes("src/app.rs", &task.spec.write_scopes));
        assert!(!within_write_scopes(
            "docs/readme.md",
            &task.spec.write_scopes
        ));

        let edit_args = json!({
            "path": "src/app.rs",
            "old_string": "old_call();",
            "new_string": "new_call();"
        });
        assert_eq!(
            evaluate_call(&task, "edit_file", &edit_args),
            PolicyDecision::RequireApproval
        );
        let sum = summary(&task, "edit_file", &edit_args);
        assert!(sum.contains("edit_file src/app.rs"));
        assert!(sum.contains("-    old_call();"));
        assert!(sum.contains("+    new_call();"));
    }
}
