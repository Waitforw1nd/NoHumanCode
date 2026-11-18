//! Host application service for tool admission, approval, execution and recovery.
use crate::{
    approval::{self, ApprovalStatus, PolicyDecision},
    domain::*,
    project_config::ProjectError,
    secrets,
    store::Store,
    tool_results::Origin,
    tool_runtime::{ToolLease, ToolRuntime},
    workspace_changes::{self, *},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const TOOL_OUTPUT_MAX_CHARS: usize = 32_000;
const TOOL_OUTPUT_HEAD_CHARS: usize = 16_000;
const TOOL_OUTPUT_TAIL_CHARS: usize = 8_000;

pub(crate) fn truncate_tool_text(text: &str) -> String {
    let total = text.chars().count();
    if total <= TOOL_OUTPUT_MAX_CHARS {
        return text.to_owned();
    }
    let head_end = text
        .char_indices()
        .nth(TOOL_OUTPUT_HEAD_CHARS)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    let tail_start = text
        .char_indices()
        .nth(total.saturating_sub(TOOL_OUTPUT_TAIL_CHARS))
        .map(|(idx, _)| idx)
        .unwrap_or(head_end)
        .max(head_end);
    let omitted = total.saturating_sub(TOOL_OUTPUT_HEAD_CHARS + TOOL_OUTPUT_TAIL_CHARS);
    format!(
        "{}\n[... truncated {} chars ...]\n{}",
        &text[..head_end],
        omitted,
        &text[tail_start..]
    )
}

pub(crate) fn truncate_tool_value(value: Value) -> Value {
    let truncated = truncate_value_strings(value);
    let serialized = truncated.to_string();
    if serialized.chars().count() <= TOOL_OUTPUT_MAX_CHARS {
        return truncated;
    }
    json!({
        "truncated": true,
        "output": truncate_tool_text(&serialized),
    })
}

fn truncate_value_strings(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(truncate_tool_text(&text)),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(truncate_value_strings).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, truncate_value_strings(v)))
                .collect(),
        ),
        other => other,
    }
}

pub(crate) struct ToolCallApplication {
    store: Arc<Store>,
    project: Arc<crate::host_services::ProjectService>,
    approval: Arc<crate::host_services::ApprovalService>,
    tool_runtime: Arc<ToolRuntime>,
    workspace: Arc<crate::host_services::WorkspaceSafety>,
    approval_waiters: Mutex<HashMap<String, oneshot::Sender<ApprovalStatus>>>,
}

impl ToolCallApplication {
    pub(crate) fn new_with_services(
        store: Arc<Store>,
        project: Arc<crate::host_services::ProjectService>,
        approval: Arc<crate::host_services::ApprovalService>,
        tool_runtime: Arc<ToolRuntime>,
        workspace: Arc<crate::host_services::WorkspaceSafety>,
    ) -> Self {
        Self {
            store,
            project,
            approval,
            tool_runtime,
            workspace,
            approval_waiters: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn decide_approval_with_network_ack(
        &self,
        op: &crate::execution_authority::ExecutionOperation,
        approval_id: &str,
        approved: bool,
        decided_by: Option<&str>,
        acknowledge_command_network: bool,
    ) -> Result<approval::ApprovalRecord> {
        let record = self.approval.decide(
            op,
            approval_id,
            approved,
            decided_by.unwrap_or("user"),
            acknowledge_command_network,
        )?;
        if let Some(waiter) = self.approval_waiters.lock().unwrap().remove(approval_id) {
            let _ = waiter.send(record.status);
        }
        Ok(record)
    }

    pub(crate) async fn reconcile_open_tools(
        &self,
        task: &mut Task,
        cancel: &CancellationToken,
        key: &str,
    ) -> Result<()> {
        approval::validate_call_history(&task.messages)?;
        let calls = unanswered_tool_calls(&task.messages);
        for call in calls {
            let name = call["function"]["name"].as_str().context("工具名称缺失")?;
            let call_id = call["id"].as_str().context("工具调用缺少 id")?;
            match self.store.approval_by_tool_call(&task.id, call_id)? {
                None => {
                    if let Some(recovered) =
                        self.store.recover_legacy_read_result(task, call_id, name)?
                    {
                        *task = recovered;
                        continue;
                    }
                    if let PolicyDecision::Deny { reason } =
                        crate::store::evaluate_tool_policy(task, name)
                    {
                        self.record_tool_result(
                            task,
                            call_id,
                            name,
                            reason,
                            key,
                            Origin::HostDenied,
                        )?;
                        continue;
                    }
                    self.record_tool_result(
                        task,
                        call_id,
                        name,
                        "Execution was interrupted. Inspect files before repeating a write or command.",
                        key,
                        Origin::InterruptedWithoutClaim,
                    )?;
                    continue;
                }
                Some(record)
                    if matches!(
                        record.status,
                        ApprovalStatus::Denied | ApprovalStatus::Cancelled
                    ) || record.execution_state == approval::ExecutionState::Cancelled =>
                {
                    let status = if record.execution_state == approval::ExecutionState::Cancelled {
                        ApprovalStatus::Cancelled
                    } else {
                        record.status
                    };
                    self.record_tool_result(
                        task,
                        call_id,
                        name,
                        &approval::tool_error(status),
                        key,
                        if status == ApprovalStatus::Denied {
                            Origin::ApprovalDenied
                        } else {
                            Origin::ApprovalCancelled
                        },
                    )?;
                    continue;
                }
                Some(_) => {}
            }
            let args: Value = serde_json::from_str(
                call["function"]["arguments"]
                    .as_str()
                    .context("工具参数缺失")?,
            )?;
            self.execute_or_wait(task, &call, name, &args, key, cancel)
                .await?;
        }
        Ok(())
    }

    pub(crate) fn reject_unadvertised(
        &self,
        task: &mut Task,
        call: &Value,
        name: &str,
        key: &str,
    ) -> Result<()> {
        if let PolicyDecision::Deny { reason } = crate::store::evaluate_tool_policy(task, name) {
            let call_id = call["id"].as_str().context("工具调用缺少 id")?;
            self.record_tool_result(task, call_id, name, reason, key, Origin::HostDenied)?;
        }
        Ok(())
    }

    pub(crate) async fn execute_or_wait(
        &self,
        task: &mut Task,
        call: &Value,
        name: &str,
        args: &Value,
        key: &str,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let _operation =
            crate::execution_authority::current().context("tool operation required")?;
        self.execute_or_wait_in_domain(task, call, name, args, key, cancel)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_or_wait_in_domain(
        &self,
        task: &mut Task,
        call: &Value,
        name: &str,
        args: &Value,
        key: &str,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let tool_call_id = call["id"].as_str().context("工具调用缺少 id")?;
        if name == "run_wasm" {
            return Err(ProjectError::OperationUnsupported.into());
        }
        self.project.validate_task(task, true, false)?;
        if name.starts_with("mcp__") {
            ensure!(
                crate::mcp::is_valid_qualified_name(name)
                    && task.spec.tools
                    && task.spec.allow_commands,
                "模型请求了未授权的工具"
            );
            let shell_open = self
                .tool_runtime
                .with_shell_control(|c| Ok(c.snapshot().admission_open))?;
            ensure!(
                shell_open
                    && self
                        .store
                        .plugin_desired_for(crate::plugin_management::BUILTIN_SHELL_ID)?
                        .enabled,
                "tool runtime unavailable"
            );
            ensure!(
                crate::mcp::has_configured_server(std::path::Path::new(&task.workspace), name),
                "模型请求了未授权的工具"
            );
            ensure!(
                self.store.task(&task.id)?.status != "cancelled" && !cancel.is_cancelled(),
                "任务已停止"
            );
            let approval_task = transient_approval_task(task, call, name, args)?;
            let (record, _) = self
                .approval
                .ensure(&approval_task, tool_call_id, name, args)?;
            drop(approval_task);
            let status = self.wait_for_approval(&record, cancel).await?;
            return match status {
                ApprovalStatus::Approved => {
                    let approval_task = transient_approval_task(task, call, name, args)?;
                    let claimed = self
                        .approval
                        .claim(&approval_task, tool_call_id, name, args)?;
                    drop(approval_task);
                    self.perform_mcp_tool(task, tool_call_id, name, args, key, &claimed.id)
                        .await
                }
                ApprovalStatus::Denied | ApprovalStatus::Cancelled => {
                    let mut approval_task = transient_approval_task(task, call, name, args)?;
                    self.record_tool_result(
                        &mut approval_task,
                        tool_call_id,
                        name,
                        &approval::tool_error(status),
                        key,
                        if status == ApprovalStatus::Denied {
                            Origin::ApprovalDenied
                        } else {
                            Origin::ApprovalCancelled
                        },
                    )?;
                    *task = approval_task;
                    if status == ApprovalStatus::Cancelled || cancel.is_cancelled() {
                        anyhow::bail!("任务已停止");
                    }
                    Ok(())
                }
                ApprovalStatus::Pending => anyhow::bail!("审批仍在等待"),
            };
        }
        if matches!(name, "git_status" | "todo_write" | "todo_read") {
            let files_open = self
                .tool_runtime
                .with_files_control(|c| Ok(c.snapshot().admission_open))?;
            ensure!(files_open, "tool runtime unavailable");
            ensure!(
                crate::workspace::definitions(task)
                    .iter()
                    .any(|t| t["function"]["name"] == name),
                "模型请求了未授权的工具"
            );
            self.project.validate_files_read(task)?;
            ensure!(
                self.store.task(&task.id)?.status != "cancelled" && !cancel.is_cancelled(),
                "任务已停止"
            );
            return self
                .perform_direct_workspace_tool(task, tool_call_id, name, args, key)
                .await;
        }
        ensure!(
            self.tool_runtime
                .definitions(task)?
                .iter()
                .any(|t| t["function"]["name"] == name),
            "模型请求了未授权的工具"
        );
        ensure!(
            self.store.task(&task.id)?.status != "cancelled" && !cancel.is_cancelled(),
            "任务已停止"
        );
        match self.approval.evaluate(task, name) {
            PolicyDecision::Allow => {
                let (lease, _) = self.tool_runtime.acquire(task, name, || {
                    if matches!(
                        name,
                        "read_file"
                            | "list_files"
                            | "search_files"
                            | "glob_files"
                            | "git_status"
                            | "todo_write"
                            | "todo_read"
                    ) {
                        self.project.validate_files_read(task)
                    } else {
                        Ok(())
                    }
                })?;
                self.perform_tool(task, tool_call_id, name, args, key, None, lease)
                    .await
            }
            PolicyDecision::Deny { reason } => {
                self.record_tool_result(task, tool_call_id, name, reason, key, Origin::HostDenied)?;
                Ok(())
            }
            PolicyDecision::RequireApproval => {
                let approval_task = transient_approval_task(task, call, name, args)?;
                let (record, _) = self
                    .approval
                    .ensure(&approval_task, tool_call_id, name, args)?;
                drop(approval_task);
                let status = self.wait_for_approval(&record, cancel).await?;
                match status {
                    ApprovalStatus::Approved => {
                        let approval_task = transient_approval_task(task, call, name, args)?;
                        let (lease, claimed) = self.tool_runtime.acquire(task, name, || {
                            self.approval
                                .claim(&approval_task, tool_call_id, name, args)
                        })?;
                        drop(approval_task);
                        self.perform_tool(
                            task,
                            tool_call_id,
                            name,
                            args,
                            key,
                            Some(&claimed.id),
                            lease,
                        )
                        .await
                    }
                    ApprovalStatus::Denied | ApprovalStatus::Cancelled => {
                        let mut approval_task = transient_approval_task(task, call, name, args)?;
                        self.record_tool_result(
                            &mut approval_task,
                            tool_call_id,
                            name,
                            &approval::tool_error(status),
                            key,
                            if status == ApprovalStatus::Denied {
                                Origin::ApprovalDenied
                            } else {
                                Origin::ApprovalCancelled
                            },
                        )?;
                        *task = approval_task;
                        if status == ApprovalStatus::Cancelled || cancel.is_cancelled() {
                            anyhow::bail!("任务已停止");
                        }
                        Ok(())
                    }
                    ApprovalStatus::Pending => anyhow::bail!("审批仍在等待"),
                }
            }
        }
    }

    async fn wait_for_approval(
        &self,
        record: &approval::ApprovalRecord,
        cancel: &CancellationToken,
    ) -> Result<ApprovalStatus> {
        loop {
            let (sender, mut receiver) = oneshot::channel();
            self.approval_waiters
                .lock()
                .unwrap()
                .insert(record.id.clone(), sender);
            let current = self.store.approval(&record.id)?;
            let waiting = current.status == ApprovalStatus::Pending
                || (current.status == ApprovalStatus::Approved
                    && current.command_network_ack == approval::CommandNetworkAck::Required);
            if !waiting {
                self.approval_waiters.lock().unwrap().remove(&record.id);
                return Ok(current.status);
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    self.approval_waiters.lock().unwrap().remove(&record.id);
                    self.store.cancel_pending_approvals(&record.task_id)?;
                    anyhow::bail!("任务已停止")
                },
                _ = &mut receiver => {},
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn perform_tool(
        &self,
        task: &mut Task,
        tool_call_id: &str,
        name: &str,
        args: &Value,
        key: &str,
        approval_id: Option<&str>,
        lease: ToolLease,
    ) -> Result<()> {
        let is_file_mutation = matches!(name, "write_file" | "edit_file");
        let _workspace_gate = if is_file_mutation {
            Some(self.workspace.gate.lock().await)
        } else {
            None
        };
        let mut prepared: Option<(String, PreparedChange, Vec<u8>)> = None;
        if is_file_mutation {
            let approval_id = approval_id.context("文件写入工具缺少审批执行权")?;
            let preflight = (|| -> Result<(PreparedChange, Vec<u8>)> {
                let (target, before, expected_after, display_path) =
                    crate::workspace::preview_file_mutation(
                        std::path::Path::new(&task.workspace),
                        name,
                        args,
                        &task.spec.write_scopes,
                    )?;
                let path_key = display_path.to_lowercase();
                let change = self.store.prepare_workspace_change(
                    task,
                    tool_call_id,
                    &display_path,
                    &path_key,
                    before.as_deref(),
                )?;
                let rechecked = match self.workspace.read_safe_file(&target) {
                    Ok(value) => value,
                    Err(error) => {
                        self.store.mark_workspace_change_failed(&change.id)?;
                        return Err(error);
                    }
                };
                if rechecked.as_deref().map(workspace_changes::digest)
                    != before.as_deref().map(workspace_changes::digest)
                {
                    self.store.mark_workspace_change_failed(&change.id)?;
                    return Err(WorkspaceChangeError::Conflict { receipt: None }.into());
                }
                Ok((change, expected_after))
            })();
            match preflight {
                Ok((change, expected_after)) => {
                    prepared = Some((approval_id.to_owned(), change, expected_after));
                }
                Err(error) => {
                    if error.chain().any(|cause| cause.is::<rusqlite::Error>()) {
                        return Err(error);
                    }
                    let message = secrets::scrub(&error.to_string(), key);
                    let serialized = self.store.finish_approval_with_change(
                        task,
                        approval_id,
                        &json!({"error": message}).to_string(),
                        name,
                        tool_call_id,
                        None,
                    )?;
                    task.messages.push(
                        json!({"role":"tool","tool_call_id":tool_call_id,"content":serialized}),
                    );
                    task.updated_at = now();
                    return Ok(());
                }
            }
        }
        let start_data = if let Some(approval_id) = approval_id {
            json!({
                "name": name,
                "tool_call_id": tool_call_id,
                "approval_id": approval_id,
                "args_digest": approval::args_digest(args)?
            })
        } else {
            json!({"name": name, "arguments": args})
        };
        if let Err(error) = self.store.event(&task.id, "tool_start", start_data) {
            if let Some((_, change, _)) = &prepared {
                let _ = self.store.mark_workspace_change_failed(&change.id);
            }
            return Err(error);
        }
        let execution = lease.execute(task, args).await;
        let (result, succeeded) = match execution {
            Ok(value) => (truncate_tool_value(value), true),
            Err(error)
                if name == "run_command"
                    && error.chain().any(|cause| {
                        cause.is::<crate::command_execution::CommandOutcomeUnknown>()
                    }) =>
            {
                let id =
                    approval_id.context("Command result unknown without an approval binding")?;
                return Err(approval::ApprovalError::UnknownResult { id: id.to_owned() }.into());
            }
            Err(error) => (
                json!({"error": truncate_tool_text(&secrets::scrub(&error.to_string(), key))}),
                false,
            ),
        };
        let serialized = result.to_string().replace(key, "[redacted]");
        if let Some(approval_id) = approval_id {
            if !succeeded && let Some((_, change, _)) = &prepared {
                self.store.mark_workspace_change_failed(&change.id)?;
            }
            let change = prepared.as_ref().and_then(|(_, change, expected_after)| {
                succeeded.then_some((change.id.as_str(), expected_after.as_slice()))
            });
            let serialized = self
                .store
                .finish_approval_with_change(
                    task,
                    approval_id,
                    &serialized,
                    name,
                    tool_call_id,
                    change,
                )
                .inspect_err(|_| {
                    if succeeded && let Some((_, change, _)) = &prepared {
                        let _ = self.store.mark_workspace_change_unknown(&change.id);
                    }
                })?;
            task.messages
                .push(json!({"role":"tool","tool_call_id":tool_call_id,"content":serialized}));
            task.updated_at = now();
            Ok(())
        } else {
            self.record_tool_result(
                task,
                tool_call_id,
                name,
                &serialized,
                key,
                Origin::ReadCompleted,
            )
        }
    }

    async fn perform_mcp_tool(
        &self,
        task: &mut Task,
        tool_call_id: &str,
        name: &str,
        args: &Value,
        key: &str,
        approval_id: &str,
    ) -> Result<()> {
        self.store.event(
            &task.id,
            "tool_start",
            json!({
                "name": name,
                "tool_call_id": tool_call_id,
                "approval_id": approval_id,
                "args_digest": approval::args_digest(args)?
            }),
        )?;
        let execution =
            crate::mcp::call_tool(std::path::Path::new(&task.workspace), name, args).await;
        let result = match execution {
            Ok(value) => truncate_tool_value(value),
            Err(error) => {
                json!({"error": truncate_tool_text(&secrets::scrub(&error.to_string(), key))})
            }
        };
        let serialized = result.to_string().replace(key, "[redacted]");
        let serialized =
            self.store
                .finish_approval(task, approval_id, &serialized, name, tool_call_id)?;
        task.messages
            .push(json!({"role":"tool","tool_call_id":tool_call_id,"content":serialized}));
        task.updated_at = now();
        Ok(())
    }

    async fn perform_direct_workspace_tool(
        &self,
        task: &mut Task,
        tool_call_id: &str,
        name: &str,
        args: &Value,
        key: &str,
    ) -> Result<()> {
        self.store.event(
            &task.id,
            "tool_start",
            json!({"name": name, "arguments": args}),
        )?;
        let execution = crate::workspace::execute(task, name, args).await;
        let result = match execution {
            Ok(value) => truncate_tool_value(value),
            Err(error) => {
                json!({"error": truncate_tool_text(&secrets::scrub(&error.to_string(), key))})
            }
        };
        let serialized = result.to_string().replace(key, "[redacted]");
        self.record_tool_result(
            task,
            tool_call_id,
            name,
            &serialized,
            key,
            Origin::ReadCompleted,
        )
    }

    pub(crate) fn record_tool_result(
        &self,
        task: &mut Task,
        tool_call_id: &str,
        name: &str,
        content: &str,
        key: &str,
        origin: Origin,
    ) -> Result<()> {
        let content = truncate_tool_text(&content.replace(key, "[redacted]"));
        let (updated, _) =
            self.store
                .commit_tool_result(task, tool_call_id, name, &content, origin)?;
        *task = updated;
        Ok(())
    }
}

fn transient_approval_task(task: &Task, call: &Value, name: &str, args: &Value) -> Result<Task> {
    approval::validate_call_history(&task.messages)?;
    let id = call["id"].as_str().context("工具调用缺少 id")?;
    let raw = call["function"]["arguments"]
        .as_str()
        .context("工具参数缺失")?;
    let parsed: Value = serde_json::from_str(raw)?;
    ensure!(
        call["type"] == "function"
            && call["function"]["name"] == name
            && parsed.is_object()
            && approval::args_digest(&parsed)? == approval::args_digest(args)?,
        "工具调用参数不匹配"
    );
    let group_index = task
        .messages
        .iter()
        .rposition(|message| message.get("tool_calls").is_some())
        .context("工具调用组缺失")?;
    let group = task.messages[group_index]["tool_calls"]
        .as_array()
        .context("工具调用组损坏")?;
    let prior = &task.messages[group_index + 1..];
    let current = group.get(prior.len()).context("工具调用次序不匹配")?;
    ensure!(
        task.messages[group_index]["role"] == "assistant"
            && current["id"] == id
            && current["type"] == "function"
            && current["function"]["name"] == name
            && prior.iter().zip(group).all(|(answer, previous)| {
                answer["role"] == "tool" && answer["tool_call_id"] == previous["id"]
            }),
        "工具调用身份或次序不匹配"
    );
    let saved = current["function"]["arguments"]
        .as_str()
        .context("持久工具参数缺失")?;
    let equivalent = crate::domain::safe_tool_arguments_equal(raw, saved)?;
    ensure!(equivalent, "工具调用安全参数不匹配");
    let mut transient = task.clone();
    transient.messages[group_index]["tool_calls"][prior.len()]["function"]["arguments"] =
        Value::String(raw.to_owned());
    Ok(transient)
}

pub(crate) fn unanswered_tool_calls(messages: &[Value]) -> Vec<Value> {
    let answered: std::collections::HashSet<String> = messages
        .iter()
        .filter_map(|message| message["tool_call_id"].as_str().map(str::to_owned))
        .collect();
    let mut calls = Vec::new();
    for message in messages {
        if let Some(tool_calls) = message["tool_calls"].as_array() {
            for call in tool_calls {
                if call["id"].as_str().is_some_and(|id| !answered.contains(id)) {
                    calls.push(call.clone());
                }
            }
        }
    }
    calls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_oversized_tool_text_preserving_head_and_tail() {
        let head = "H".repeat(TOOL_OUTPUT_HEAD_CHARS);
        let middle = "M".repeat(20_000);
        let tail = "T".repeat(TOOL_OUTPUT_TAIL_CHARS);
        let raw = format!("{head}{middle}{tail}");
        let out = truncate_tool_text(&raw);
        assert!(out.starts_with(&head));
        assert!(out.ends_with(&tail));
        assert!(out.contains("[... truncated 20000 chars ...]"));
        assert!(out.chars().count() < TOOL_OUTPUT_MAX_CHARS);
    }
}
