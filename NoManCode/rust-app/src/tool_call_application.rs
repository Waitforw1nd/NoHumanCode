//! Host application service for tool admission, approval, execution and recovery.
#[cfg(test)]
use crate::engine::crash_probe;
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
#[cfg(test)]
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
pub(crate) type ShellClaimProbe = (String, Arc<dyn Fn() + Send + Sync>);
pub(crate) struct ToolCallApplication {
    store: Arc<Store>,
    project: Arc<crate::host_services::ProjectService>,
    approval: Arc<crate::host_services::ApprovalService>,
    tool_runtime: Arc<ToolRuntime>,
    workspace: Arc<crate::host_services::WorkspaceSafety>,
    approval_waiters: Mutex<HashMap<String, oneshot::Sender<ApprovalStatus>>>,
    #[cfg(test)]
    shell_claim_probe: Arc<Mutex<Option<ShellClaimProbe>>>,
}
impl ToolCallApplication {
    #[cfg(test)]
    pub(crate) fn new(
        store: Arc<Store>,
        tool_runtime: Arc<ToolRuntime>,
        gate: Arc<AsyncMutex<()>>,
        #[cfg(test)] shell_claim_probe: Arc<Mutex<Option<ShellClaimProbe>>>,
    ) -> Self {
        let project = Arc::new(crate::host_services::ProjectService::new(
            store.clone(),
            gate.clone(),
        ));
        let approval = Arc::new(crate::host_services::ApprovalService::new(store.clone()));
        let workspace = Arc::new(crate::host_services::WorkspaceSafety::new(gate));
        Self::new_with_services(
            store,
            project,
            approval,
            tool_runtime,
            workspace,
            shell_claim_probe,
        )
    }
    pub(crate) fn new_with_services(
        store: Arc<Store>,
        project: Arc<crate::host_services::ProjectService>,
        approval: Arc<crate::host_services::ApprovalService>,
        tool_runtime: Arc<ToolRuntime>,
        workspace: Arc<crate::host_services::WorkspaceSafety>,
        #[cfg(test)] shell_claim_probe: Arc<Mutex<Option<ShellClaimProbe>>>,
    ) -> Self {
        Self {
            store,
            project,
            approval,
            tool_runtime,
            workspace,
            approval_waiters: Mutex::new(HashMap::new()),
            #[cfg(test)]
            shell_claim_probe,
        }
    }
    pub(crate) fn decide_approval_with_network_ack(
        &self,
        approval_id: &str,
        approved: bool,
        decided_by: Option<&str>,
        acknowledge_command_network: bool,
    ) -> Result<approval::ApprovalRecord> {
        let record = self.approval.decide(
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
                    if let PolicyDecision::Deny { reason } = self.approval.evaluate(task, name) {
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
                    self.record_tool_result(task, call_id, name, "Execution was interrupted. Inspect files before repeating a write or command.", key, Origin::InterruptedWithoutClaim)?;
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
        if let PolicyDecision::Deny { reason } = self.approval.evaluate(task, name) {
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
        let tool_call_id = call["id"].as_str().context("工具调用缺少 id")?;
        if name == "run_wasm" {
            return Err(ProjectError::OperationUnsupported.into());
        }
        self.project.validate_task(task, true, false)?;
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
                    if matches!(name, "read_file" | "list_files" | "search_files") {
                        self.project.validate_files_read(task)
                    } else {
                        self.project.validate_task(task, true, false).map(|_| ())
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
                        #[cfg(test)]
                        if name == "run_command" {
                            let probe = self
                                .shell_claim_probe
                                .lock()
                                .unwrap()
                                .as_ref()
                                .filter(|(id, _)| id == &task.id)
                                .map(|(_, f)| f.clone());
                            if let Some(probe) = probe {
                                probe();
                            }
                        }
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
                _=cancel.cancelled()=>{self.approval_waiters.lock().unwrap().remove(&record.id);self.store.cancel_pending_approvals(&record.task_id)?;anyhow::bail!("任务已停止")},
                _=&mut receiver=>{},
                _=tokio::time::sleep(std::time::Duration::from_millis(100))=>{},
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
        let _workspace_gate = if name == "write_file" {
            Some(self.workspace.gate.lock().await)
        } else {
            None
        };
        self.project.validate_task(task, false, false)?;
        let mut prepared = None;
        if name == "write_file" {
            let approval_id = approval_id.context("write_file 缺少审批执行权")?;
            let preflight = (|| -> Result<PreparedChange> {
                let args_text = args.to_string();
                ensure!(
                    secrets::redact_persisted(args) == *args
                        && secrets::redact_persisted(&Value::String(args_text.clone()))
                            == Value::String(args_text),
                    "write_file 参数脱敏后无法安全绑定"
                );
                let path = args["path"].as_str().context("缺少 path")?;
                let (target, path, path_key) = self.workspace.change_target(
                    std::path::Path::new(&task.workspace),
                    path,
                    &task.spec.write_scopes,
                )?;
                let before = self.workspace.read_safe_file(&target)?;
                let change = self.store.prepare_workspace_change(
                    task,
                    tool_call_id,
                    &path,
                    &path_key,
                    before.as_deref(),
                )?;
                #[cfg(test)]
                let recheck = if crash_probe::take_recheck_failure(&task.id) {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "test recheck failure",
                    )
                    .into())
                } else {
                    self.workspace.read_safe_file(&target)
                };
                #[cfg(not(test))]
                let recheck = self.workspace.read_safe_file(&target);
                let rechecked = match recheck {
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
                Ok(change)
            })();
            match preflight {
                Ok(change) => prepared = Some((approval_id.to_owned(), change)),
                Err(error) => {
                    if error.chain().any(|cause| cause.is::<rusqlite::Error>()) {
                        return Err(error);
                    }
                    let message = if error.chain().any(|cause| {
                        matches!(
                            cause.downcast_ref::<WorkspaceChangeError>(),
                            Some(WorkspaceChangeError::Unknown { .. })
                        )
                    }) {
                        "write_file 结果未知；请检查文件后创建新任务"
                    } else {
                        "write_file 拒绝执行；文件未修改"
                    };
                    let serialized = self.store.finish_approval_with_change(
                        task,
                        approval_id,
                        &json!({"error":message}).to_string(),
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
            json!({"name":name,"tool_call_id":tool_call_id,"approval_id":approval_id,"args_digest":approval::args_digest(args)?})
        } else {
            json!({"name":name,"arguments":args})
        };
        if let Err(error) = self.store.event(&task.id, "tool_start", start_data) {
            if let Some((_, change)) = &prepared {
                let _ = self.store.mark_workspace_change_failed(&change.id);
            }
            return Err(error);
        }
        #[cfg(test)]
        if name == "write_file" {
            crash_probe::block(&task.id, "write_pre_fs", &prepared.as_ref().unwrap().1.id);
        }
        let (result, succeeded) = match lease.execute(task, args).await {
            Ok(value) => (value, true),
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
                json!({"error":secrets::scrub(&error.to_string(), key)}),
                false,
            ),
        };
        let serialized = result.to_string().replace(key, "[redacted]");
        if let Some(approval_id) = approval_id {
            let after = if succeeded && name == "write_file" {
                args["content"].as_str().map(str::as_bytes)
            } else {
                None
            };
            if !succeeded && let Some((_, change)) = &prepared {
                self.store.mark_workspace_change_failed(&change.id)?;
            }
            let change = prepared
                .as_ref()
                .and_then(|(_, change)| after.map(|bytes| (change.id.as_str(), bytes)));
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
                    if succeeded && let Some((_, change)) = &prepared {
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

    pub(crate) fn record_tool_result(
        &self,
        task: &mut Task,
        tool_call_id: &str,
        name: &str,
        content: &str,
        key: &str,
        origin: Origin,
    ) -> Result<()> {
        let content = content.replace(key, "[redacted]");
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
