use super::{Engine, NetworkSlot, key_pool::CompletionOutput};
use crate::agent_runtime::ToolVote;
use crate::domain::{Route, Task};
use crate::messages::ToolCallBlock;
use crate::provider_runtime::{CompletionInput, Emit};
use crate::redaction::CredentialSet;
use crate::runtime_seams::{
    BatchOutcome, CompletionTurnResult, CredentialSource, DependencyState, DependencySummary,
    EventSink, PreparedTools, SlotGuard, TaskRepository, ToolExecutor,
};
use crate::store::Store;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub(crate) struct CliSlotGuard(pub(crate) Option<NetworkSlot>);

impl SlotGuard for CliSlotGuard {
    fn release(&mut self) {
        if let Some(mut slot) = self.0.take() {
            slot.release();
        }
    }
}

pub(crate) struct CliEventSink<'a>(pub(crate) &'a Store);

impl EventSink for CliEventSink<'_> {
    fn completion_delta(&self, task_id: &str, text: &str) -> Result<()> {
        self.0.completion_delta(task_id, text)
    }

    fn reasoning_delta(&self, task_id: &str, text: &str) -> Result<()> {
        self.0.reasoning_delta(task_id, text)
    }

    fn emit_event(&self, task_id: &str, kind: &str, data: Value) -> Result<()> {
        self.0.event(task_id, kind, data)
    }
}

pub(crate) struct CliTaskRepository<'a>(pub(crate) &'a Store);

impl TaskRepository for CliTaskRepository<'_> {
    fn check_dependencies(&self, task: &Task) -> Result<DependencyState> {
        let dependencies = match self.0.dependency_tasks(&task.id)? {
            Some(dependencies) => dependencies,
            None => {
                let dependencies: Vec<_> = self
                    .0
                    .run(&task.run_id)?
                    .tasks
                    .into_iter()
                    .filter(|other| task.spec.depends_on.contains(&other.spec.name))
                    .collect();
                ensure!(
                    dependencies.len() == task.spec.depends_on.len(),
                    "历史任务的前置成员不存在或名称有歧义"
                );
                dependencies
            }
        };
        if dependencies.is_empty() {
            return Ok(DependencyState::Ready(Vec::new()));
        }
        if dependencies.iter().any(|other| {
            matches!(
                other.status.as_str(),
                "failed" | "cancelled" | "interrupted"
            )
        }) {
            return Ok(DependencyState::Failed(
                "前置成员未成功完成；先恢复前置成员，再继续此任务".into(),
            ));
        }
        if dependencies.iter().all(|other| other.status == "completed") {
            let summaries = dependencies
                .iter()
                .map(|other| DependencySummary {
                    member: other.spec.name.clone(),
                    role: other.spec.role.clone(),
                    model: other.route.model.clone(),
                    result: other.output.clone(),
                })
                .collect();
            return Ok(DependencyState::Ready(summaries));
        }
        Ok(DependencyState::Pending)
    }

    fn save_running_snapshot(&self, task: &Task) -> Result<()> {
        self.0.save_running_snapshot(task)
    }

    fn save_assistant_message(&self, task: &Task, has_tool_calls: bool) -> Result<()> {
        if has_tool_calls {
            self.0.save_new_assistant_group(task)?;
        }
        Ok(())
    }

    fn consume_resume_inputs(&self, task: &Task) -> Result<Task> {
        self.0.consume_resume_inputs(task)
    }
}

pub(crate) struct CliCredentialSource<'a>(pub(crate) &'a Engine);

impl CredentialSource for CliCredentialSource<'_> {
    fn credential_set(&self, route: &Route) -> Result<CredentialSet> {
        Ok(crate::key_pool::credential_set(route, &self.0.key(route)?)?)
    }

    async fn acquire_slot(&self, cancel: &CancellationToken) -> Result<Box<dyn SlotGuard>> {
        let slot = self.0.task_slot(cancel).await?;
        Ok(Box::new(CliSlotGuard(Some(slot))) as Box<dyn SlotGuard>)
    }

    async fn complete_streaming<'a>(
        &'a self,
        task: &'a Task,
        route: &'a Route,
        input: &'a CompletionInput<'a>,
        emit_text: &'a mut Emit<'a>,
        emit_reasoning: &'a mut Emit<'a>,
        cancel: &'a CancellationToken,
    ) -> Result<CompletionTurnResult> {
        let subject = self.0.task_route_subject(task)?;
        let resp = self
            .0
            .complete_route(
                &subject,
                Some(task),
                route,
                input,
                CompletionOutput::Stream {
                    text: emit_text,
                    reasoning: emit_reasoning,
                },
                cancel,
            )
            .await?;
        Ok(CompletionTurnResult {
            message: resp.result.0,
            usage: resp.result.1,
        })
    }
}

struct CliPreparedMcp {
    targets: HashMap<String, crate::mcp::ToolTarget>,
}

pub(crate) struct CliToolExecutor<'a>(pub(crate) &'a Engine);

impl ToolExecutor for CliToolExecutor<'_> {
    fn reconcile_open_tools<'a>(
        &'a self,
        task: &'a mut Task,
        keys: &'a CredentialSet,
        slot: &'a mut Box<dyn SlotGuard>,
        cancel: &'a CancellationToken,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        self.0
            .tool_calls
            .reconcile_open_tools(self.0, task, cancel, keys, slot)
    }

    async fn prepare_round_tools(
        &self,
        task: &Task,
        events: &dyn EventSink,
        announced_logins: &mut HashSet<String>,
        cancel: &CancellationToken,
    ) -> Result<PreparedTools> {
        let mut available = self.0.tool_runtime.definitions(task)?;
        if self.0.tool_calls.computer_enabled(task) {
            self.0
                .composition
                .projects
                .validate_task(task, true, false)?;
            let writable = self
                .0
                .composition
                .computer
                .save_enabled_for(Path::new(&task.workspace))
                && available
                    .iter()
                    .any(|t| t["function"]["name"] == "write_file");
            available.extend(
                crate::computer::definitions()
                    .into_iter()
                    .filter(|t| writable || t["function"]["name"] != "computer_save_as"),
            );
        }
        let mut mcp_targets = HashMap::new();
        if task.spec.tools
            && self
                .0
                .plugin_status(crate::plugin_management::BUILTIN_FILES_ID)
                .is_ok_and(|s| s.desired.enabled && s.runtime.admission_open)
        {
            available.push(crate::workspace::exploration_definition());
            available.push(crate::workspace::parallel_exploration_definition());
            for tool in crate::workspace::definitions(task) {
                if let Some(name) = tool["function"]["name"].as_str()
                    && matches!(name, "git_status" | "todo_write" | "todo_read")
                    && !available
                        .iter()
                        .any(|existing| existing["function"]["name"] == name)
                {
                    available.push(tool);
                }
            }
        }
        if task.spec.tools
            && task.spec.allow_commands
            && self
                .0
                .plugin_status(crate::plugin_management::BUILTIN_SHELL_ID)
                .is_ok_and(|s| s.desired.enabled && s.runtime.admission_open)
        {
            let mut login_required = Vec::new();
            let mcp_tools = tokio::select! {
                biased;
                _ = cancel.cancelled() => anyhow::bail!("任务已停止"),
                tools = self.0.tool_calls.mcp.discover_targets(&task.run_id, Path::new(&task.workspace), &mut login_required) => tools,
            };
            for server in login_required {
                if announced_logins.insert(server.clone()) {
                    events.emit_event(
                        &task.id,
                        "unknown:mcp_login_required",
                        json!({"server": server}),
                    )?;
                }
            }
            for (tool, target) in mcp_tools {
                if let Some(name) = tool["function"]["name"].as_str()
                    && !available
                        .iter()
                        .any(|existing| existing["function"]["name"] == name)
                {
                    mcp_targets.insert(name.to_owned(), target);
                    available.push(tool);
                }
            }
        }
        Ok(PreparedTools {
            definitions: available,
            backend_state: Box::new(CliPreparedMcp {
                targets: mcp_targets,
            }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_batch(
        &self,
        task: &mut Task,
        calls: &[ToolCallBlock],
        votes: &[ToolVote],
        advertised_tools: &[Value],
        prepared: &PreparedTools,
        keys: &CredentialSet,
        slot: &mut Box<dyn SlotGuard>,
        cancel: &CancellationToken,
    ) -> Result<BatchOutcome> {
        let mcp_targets = prepared
            .backend_state
            .downcast_ref::<CliPreparedMcp>()
            .map(|m| &m.targets);
        for (call, &vote) in calls.iter().zip(votes) {
            ensure!(!cancel.is_cancelled(), "任务已停止");
            let name = call.name.as_str();
            ensure!(!name.is_empty(), "工具名称缺失");
            let args: Value = serde_json::from_str(&call.arguments)?;
            if !advertised_tools
                .iter()
                .any(|t| t["function"]["name"] == name)
            {
                self.0
                    .tool_calls
                    .reject_unadvertised(task, call, name, keys)?;
                anyhow::bail!("模型请求了未授权的工具");
            }
            let call_id = call.id.as_str();
            ensure!(!call_id.is_empty(), "工具调用缺少 id");
            match vote {
                ToolVote::ApplyHostPolicy if name == "explore_files" => {
                    self.0
                        .explore_files(task, call_id, &args, keys, cancel)
                        .await?;
                }
                ToolVote::ApplyHostPolicy if name == "explore_parallel" => {
                    slot.release();
                    self.0
                        .explore_parallel(task, call_id, &args, keys, cancel)
                        .await?;
                    *slot = Box::new(CliSlotGuard(Some(self.0.task_slot(cancel).await?)));
                }
                ToolVote::ApplyHostPolicy if name == "computer_wait" => {
                    slot.release();
                    self.0
                        .tool_calls
                        .execute_or_wait(task, call, name, &args, keys, cancel, None)
                        .await?;
                    *slot = Box::new(CliSlotGuard(Some(self.0.task_slot(cancel).await?)));
                }
                ToolVote::ApplyHostPolicy => {
                    let target = mcp_targets.and_then(|t| t.get(name));
                    self.0
                        .tool_calls
                        .execute_or_wait(task, call, name, &args, keys, cancel, target)
                        .await?;
                }
                ToolVote::Deny => {
                    self.0.tool_calls.record_tool_result(
                        task,
                        call_id,
                        name,
                        r#"{"error":"blocked_by_agent_policy"}"#,
                        keys,
                        crate::tool_results::Origin::AgentDenied,
                    )?;
                }
            }
        }
        Ok(BatchOutcome::Continue)
    }
}
