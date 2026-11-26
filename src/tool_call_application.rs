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

/// Keep every branch identifiable even when the JSON exceeds the tool-message limit.
pub(crate) fn parallel_result_content(batch_id: &str, mut branches: Vec<Value>) -> String {
    let complete = json!({"batch_id":batch_id,"branches": &branches}).to_string();
    if complete.chars().count() <= TOOL_OUTPUT_MAX_CHARS {
        return complete;
    }
    let budget = (TOOL_OUTPUT_MAX_CHARS - batch_id.len() - 64) / branches.len().max(1);
    for branch in &mut branches {
        if branch.to_string().len() <= budget {
            continue;
        }
        branch["truncated"] = json!(true);
        if let Some(ranges) = branch["evidence"].as_array_mut() {
            let mut remaining = budget / 2;
            let keep = ranges
                .iter()
                .take_while(|range| {
                    let bytes = range.to_string().len() + 1;
                    if bytes > remaining {
                        return false;
                    }
                    remaining -= bytes;
                    true
                })
                .count();
            ranges.truncate(keep);
        }
        let field = if branch["summary"].is_string() {
            "summary"
        } else {
            "error"
        };
        if branch[field].is_string() && branch.to_string().len() > budget {
            let original = branch[field].take();
            let mut text = original.as_str().unwrap();
            let room = budget.saturating_sub(branch.to_string().len() + '…'.len_utf8());
            while serde_json::to_string(text).expect("JSON string").len() > room && !text.is_empty()
            {
                text = &text[..text.floor_char_boundary(text.len() / 2)];
            }
            branch[field] = json!(format!("{text}…"));
        }
    }
    json!({"batch_id":batch_id,"branches": branches}).to_string()
}

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

fn truncate_tool_value(value: Value, keys: &secrets::CredentialSet) -> Value {
    let truncated = truncate_value_strings(value, keys);
    let serialized = truncated.to_string();
    if serialized.chars().count() <= TOOL_OUTPUT_MAX_CHARS {
        return truncated;
    }
    json!({
        "truncated": true,
        "output": truncate_tool_text(&serialized),
    })
}

fn truncate_value_strings(value: Value, keys: &secrets::CredentialSet) -> Value {
    match value {
        Value::String(text) => Value::String(truncate_tool_text(&keys.scrub(&text))),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| truncate_value_strings(item, keys))
                .collect(),
        ),
        Value::Object(map) => {
            let mut fields = serde_json::Map::new();
            let mut renamed = Vec::new();
            for (name, field) in map {
                let safe = keys.scrub(&name);
                let field = truncate_value_strings(field, keys);
                if safe == name {
                    fields.insert(name, field);
                } else {
                    renamed.push((safe, field));
                }
            }
            let mut suffix = 2;
            for (base, field) in renamed {
                let mut name = base.clone();
                while fields.contains_key(&name) {
                    name = format!("{base} #{suffix}");
                    suffix += 1;
                }
                fields.insert(name, field);
            }
            Value::Object(fields)
        }
        other => other,
    }
}

pub(crate) struct ToolCallApplication {
    #[cfg(test)]
    pub(crate) save_fixture: std::sync::OnceLock<fn(&std::path::Path) -> Result<()>>,
    computer: Arc<crate::computer::ComputerService>,
    pub(crate) mcp: crate::mcp::McpPool,
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
        computer: Arc<crate::computer::ComputerService>,
    ) -> Self {
        Self {
            #[cfg(test)]
            save_fixture: Default::default(),
            computer,
            mcp: crate::mcp::McpPool::with_store(store.clone()),
            store,
            project,
            approval,
            tool_runtime,
            workspace,
            approval_waiters: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn computer_enabled(&self, task: &Task) -> bool {
        task.spec.tools
            && self
                .computer
                .enabled_for(std::path::Path::new(&task.workspace))
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
        engine: &crate::engine::Engine,
        task: &mut Task,
        cancel: &CancellationToken,
        keys: &secrets::CredentialSet,
        slot: &mut crate::engine::NetworkSlot,
    ) -> Result<()> {
        approval::validate_call_history(&task.messages)?;
        let calls = unanswered_tool_calls(&task.messages);
        for call in calls {
            let name = call["function"]["name"].as_str().context("工具名称缺失")?;
            let call_id = call["id"].as_str().context("工具调用缺少 id")?;
            if name == "explore_parallel"
                && let Some(batch) =
                    self.store
                        .exploration_by_call(&task.id, &format!("tool:{call_id}"), None)?
            {
                slot.release();
                engine
                    .complete_exploration_batch(task, call_id, batch, keys, cancel)
                    .await?;
                *slot = engine.task_slot(cancel).await?;
                continue;
            }
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
                            keys,
                            Origin::HostDenied,
                        )?;
                        continue;
                    }
                    self.record_tool_result(
                        task,
                        call_id,
                        name,
                        crate::tool_results::interrupted_message(name),
                        keys,
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
                        keys,
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
            self.execute_or_wait(task, &call, name, &args, keys, cancel, None)
                .await?;
        }
        Ok(())
    }

    pub(crate) fn reject_unadvertised(
        &self,
        task: &mut Task,
        call: &Value,
        name: &str,
        keys: &secrets::CredentialSet,
    ) -> Result<()> {
        if let PolicyDecision::Deny { reason } = crate::store::evaluate_tool_policy(task, name) {
            let call_id = call["id"].as_str().context("工具调用缺少 id")?;
            self.record_tool_result(task, call_id, name, reason, keys, Origin::HostDenied)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_or_wait(
        &self,
        task: &mut Task,
        call: &Value,
        name: &str,
        args: &Value,
        keys: &secrets::CredentialSet,
        cancel: &CancellationToken,
        advertised: Option<&crate::mcp::ToolTarget>,
    ) -> Result<()> {
        let _operation =
            crate::execution_authority::current().context("tool operation required")?;
        self.execute_or_wait_in_domain(task, call, name, args, keys, cancel, advertised)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_or_wait_in_domain(
        &self,
        task: &mut Task,
        call: &Value,
        name: &str,
        args: &Value,
        keys: &secrets::CredentialSet,
        cancel: &CancellationToken,
        advertised: Option<&crate::mcp::ToolTarget>,
    ) -> Result<()> {
        let tool_call_id = call["id"].as_str().context("工具调用缺少 id")?;
        if name == "run_wasm" {
            return Err(ProjectError::OperationUnsupported.into());
        }
        self.project.validate_task(task, true, false)?;
        if crate::tool_results::is_computer_read(name) {
            ensure!(self.computer_enabled(task), "当前工作区未授权使用桌面工具");
            ensure!(
                !cancel.is_cancelled() && self.store.task(&task.id)?.status == "running",
                "任务已停止"
            );
            self.store.event(
                &task.id,
                "tool_start",
                json!({"name":name,"tool_call_id":tool_call_id}),
            )?;
            let result = self
                .computer
                .execute(
                    &task.run_id,
                    std::path::Path::new(&task.workspace),
                    name,
                    args,
                    cancel,
                )
                .await;
            ensure!(!cancel.is_cancelled(), "任务已停止");
            let result = match result {
                Ok(value) => truncate_tool_value(value, keys),
                Err(error) => json!({"error": truncate_tool_text(&keys.scrub(&error.to_string()))}),
            };
            return self.record_tool_result(
                task,
                tool_call_id,
                name,
                &result.to_string(),
                keys,
                Origin::ReadCompleted,
            );
        }
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
            let (record, _) =
                self.approval
                    .ensure(&approval_task, tool_call_id, name, args, advertised)?;
            let bound = self.store.approval_mcp_target(&record)?;
            ensure!(
                advertised.is_none_or(|target| Some(target) == bound.as_ref()),
                "MCP 工具审批目标不一致"
            );
            drop(approval_task);
            let status = self.wait_for_approval(&record, cancel).await?;
            return match status {
                ApprovalStatus::Approved => {
                    let approval_task = transient_approval_task(task, call, name, args)?;
                    let claimed = self
                        .approval
                        .claim(&approval_task, tool_call_id, name, args)?;
                    drop(approval_task);
                    self.perform_mcp_tool(
                        task,
                        tool_call_id,
                        name,
                        args,
                        keys,
                        &claimed.id,
                        cancel,
                        bound.as_ref(),
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
                        keys,
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
                .perform_direct_workspace_tool(task, tool_call_id, name, args, keys)
                .await;
        }
        // Save As borrows the file writer's admission/lease, not its executor.
        let capability = if name == "computer_save_as" {
            ensure!(
                task.spec.tools
                    && self
                        .computer
                        .save_enabled_for(std::path::Path::new(&task.workspace)),
                "当前工作区未授权使用桌面保存工具"
            );
            "write_file"
        } else {
            name
        };
        ensure!(
            self.tool_runtime
                .definitions(task)?
                .iter()
                .any(|t| t["function"]["name"] == capability),
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
                self.perform_tool(task, tool_call_id, name, args, keys, None, lease, cancel)
                    .await
            }
            PolicyDecision::Deny { reason } => {
                self.record_tool_result(
                    task,
                    tool_call_id,
                    name,
                    reason,
                    keys,
                    Origin::HostDenied,
                )?;
                Ok(())
            }
            PolicyDecision::RequireApproval => {
                let approval_task = transient_approval_task(task, call, name, args)?;
                let (record, _) =
                    self.approval
                        .ensure(&approval_task, tool_call_id, name, args, None)?;
                drop(approval_task);
                let status = self.wait_for_approval(&record, cancel).await?;
                match status {
                    ApprovalStatus::Approved => {
                        let approval_task = transient_approval_task(task, call, name, args)?;
                        let (lease, claimed) =
                            self.tool_runtime.acquire(task, capability, || {
                                self.approval
                                    .claim(&approval_task, tool_call_id, name, args)
                            })?;
                        drop(approval_task);
                        self.perform_tool(
                            task,
                            tool_call_id,
                            name,
                            args,
                            keys,
                            Some(&claimed.id),
                            lease,
                            cancel,
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
                            keys,
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
        keys: &secrets::CredentialSet,
        approval_id: Option<&str>,
        lease: ToolLease,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let is_file_mutation = matches!(name, "write_file" | "edit_file" | "computer_save_as");
        let _workspace_gate = if is_file_mutation {
            Some(self.workspace.gate.lock().await)
        } else {
            None
        };
        let mut save_target = None;
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
                if name == "computer_save_as" {
                    self.project.validate_task(task, true, false)?;
                    ensure!(
                        self.computer
                            .save_enabled_for(std::path::Path::new(&task.workspace))
                            && !cancel.is_cancelled()
                            && self.store.task(&task.id)?.status == "running",
                        "save no longer authorized"
                    );
                    self.store.verify_save_snapshot(
                        &task.id,
                        approval_id,
                        &approval::save_snapshot(&target, args)?,
                    )?;
                    save_target = Some(target.clone());
                }
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
                    let message = keys.scrub(&error.to_string());
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
        let execution = if name == "computer_save_as" {
            let target = save_target
                .as_deref()
                .context("missing approved save target")?;
            let (id, change, expected) = prepared.as_ref().context("missing save preflight")?;
            // Never retry an invocation. Only the subsequent independent disk reads may repeat.
            let verified = async {
                self.dispatch_save(task, args, target).await?;
                self.verify_saved_file(task, &change.path, target, expected, cancel)
                    .await
            }
            .await;
            if verified
                .as_ref()
                .is_err_and(|e| !e.is::<crate::computer::SaveNotDispatched>())
                || cancel.is_cancelled()
            {
                self.store.mark_workspace_change_unknown(&change.id)?;
                return Err(approval::ApprovalError::UnknownResult { id: id.clone() }.into());
            }
            verified
        } else {
            lease.execute(task, args).await
        };
        let (result, succeeded) = match execution {
            Ok(value) => (truncate_tool_value(value, keys), true),
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
                json!({"error": truncate_tool_text(&keys.scrub(&error.to_string()))}),
                false,
            ),
        };
        let serialized = result.to_string();
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
                keys,
                Origin::ReadCompleted,
            )
        }
    }

    async fn dispatch_save(
        &self,
        task: &Task,
        args: &Value,
        target: &std::path::Path,
    ) -> Result<()> {
        #[cfg(test)]
        if let Some(fixture) = self.save_fixture.get() {
            return fixture(target);
        }
        self.computer
            .save_as(
                &task.run_id,
                std::path::Path::new(&task.workspace),
                args["observation_ref"]
                    .as_str()
                    .context("missing observation_ref")?,
                target,
            )
            .await
    }

    async fn verify_saved_file(
        &self,
        task: &Task,
        relative: &str,
        target: &std::path::Path,
        expected: &[u8],
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut previous_match = false;
        loop {
            ensure!(!cancel.is_cancelled(), "save interrupted");
            self.project.validate_task(task, true, false)?;
            let (path, _, _) = self.workspace.change_target(
                std::path::Path::new(&task.workspace),
                relative,
                &task.spec.write_scopes,
            )?;
            ensure!(path == target, "save target changed");
            let bytes = match self.workspace.read_safe_file(target) {
                Ok(bytes) => bytes,
                // Windows applications may hold a sharing/lock violation while saving.
                Err(e)
                    if e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| matches!(e.raw_os_error(), Some(32 | 33))) =>
                {
                    None
                }
                Err(e) => return Err(e),
            };
            if let Some(actual) = bytes.filter(|actual| actual == expected) {
                if previous_match {
                    return Ok(
                        json!({"path":relative,"verified":true,"bytes":actual.len(),"sha256":workspace_changes::digest(&actual)}),
                    );
                }
                previous_match = true;
            } else {
                previous_match = false;
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "disk readback did not match"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn perform_mcp_tool(
        &self,
        task: &mut Task,
        tool_call_id: &str,
        name: &str,
        args: &Value,
        keys: &secrets::CredentialSet,
        approval_id: &str,
        cancel: &CancellationToken,
        bound: Option<&crate::mcp::ToolTarget>,
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
        let execution = self
            .mcp
            .call_bound(
                &task.run_id,
                std::path::Path::new(&task.workspace),
                name,
                args,
                cancel,
                bound.map_or(
                    crate::mcp::CallBinding::LegacyStdio,
                    crate::mcp::CallBinding::Approved,
                ),
            )
            .await;
        let result = match execution {
            Ok(value) => truncate_tool_value(value, keys),
            Err(error) if error.is::<crate::mcp::McpOutcomeUnknown>() => {
                return Err(approval::ApprovalError::UnknownResult {
                    id: approval_id.to_owned(),
                }
                .into());
            }
            Err(error) => {
                json!({"error": truncate_tool_text(&keys.scrub(&error.to_string()))})
            }
        };
        let serialized = result.to_string();
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
        keys: &secrets::CredentialSet,
    ) -> Result<()> {
        self.store.event(
            &task.id,
            "tool_start",
            json!({"name": name, "arguments": args}),
        )?;
        let execution = crate::workspace::execute(task, name, args).await;
        let result = match execution {
            Ok(value) => truncate_tool_value(value, keys),
            Err(error) => {
                json!({"error": truncate_tool_text(&keys.scrub(&error.to_string()))})
            }
        };
        let serialized = result.to_string();
        self.record_tool_result(
            task,
            tool_call_id,
            name,
            &serialized,
            keys,
            Origin::ReadCompleted,
        )
    }

    pub(crate) fn record_tool_result(
        &self,
        task: &mut Task,
        tool_call_id: &str,
        name: &str,
        content: &str,
        keys: &secrets::CredentialSet,
        origin: Origin,
    ) -> Result<()> {
        let content = if origin == Origin::ReadCompleted {
            // Read results have already been scrubbed as JSON values before truncation.
            truncate_tool_text(content)
        } else {
            truncate_tool_text(&keys.scrub(content))
        };
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
    fn scrubs_all_actual_members_before_json_escaping_and_truncation() {
        let primary = r#"test\"primary"#;
        let backup = r#"alt\"token"#;
        let keys = secrets::CredentialSet::new(vec![primary.into(), backup.into()]);
        let ordinary = r#"let token = "ordinary";"#;
        let result = truncate_tool_value(
            json!({
                (primary): [format!("read {backup}"), ordinary]
            }),
            &keys,
        );
        assert_eq!(result, json!({"[redacted]": ["read [redacted]", ordinary]}));
        let collisions = truncate_tool_value(
            json!({ (primary):1, (backup):2, "[redacted]":3, "[redacted] #2":4 }),
            &keys,
        );
        let fields = collisions.as_object().unwrap();
        assert_eq!(fields.len(), 4);
        assert_eq!(fields["[redacted]"], 3);
        assert_eq!(fields["[redacted] #2"], 4);
        assert!(fields.values().any(|v| *v == 1) && fields.values().any(|v| *v == 2));
        let prefix = "x".repeat(TOOL_OUTPUT_HEAD_CHARS - 3);
        let output = truncate_tool_value(
            json!(format!(
                "{prefix}{backup}{}",
                "z".repeat(TOOL_OUTPUT_MAX_CHARS)
            )),
            &keys,
        );
        assert!(
            output
                .as_str()
                .unwrap()
                .starts_with(&format!("{prefix}[re"))
        );
    }

    #[test]
    fn oversized_parallel_results_keep_status_and_evidence_in_valid_json() {
        let branches = (1..=4).map(|index| json!({
            "index":index, "status":"ok", "summary":"\u{0002}文件".repeat(2000),
            "evidence":(1..=150).map(|line| json!({"path":"src/main.rs","start_line":line,"end_line":line})).collect::<Vec<_>>()
        })).collect();
        let text = parallel_result_content("batch", branches);
        assert!(text.chars().count() <= TOOL_OUTPUT_MAX_CHARS);
        let result: Value = serde_json::from_str(&text).unwrap();
        let branches = result["branches"].as_array().unwrap();
        assert_eq!(branches.len(), 4);
        for (index, branch) in branches.iter().enumerate() {
            assert_eq!(branch["index"], index + 1);
            assert_eq!(branch["status"], "ok");
            assert_eq!(branch["truncated"], true);
            assert_eq!(branch["evidence"][0]["start_line"], 1);
            assert!(!branch["summary"].as_str().unwrap().is_empty());
        }
    }

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
