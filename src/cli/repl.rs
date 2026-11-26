//! In-process chat and ask sessions, streaming output, and interactive approvals.

use super::init_route::ensure_route_ready;
use super::mentions::expand_context_mentions;
use super::{
    AskArgs, ChatArgs, Failure, host_route_id_valid, open_local_engine, resolve_user_data_dir,
};
use coda::approval::ApprovalStatus;
use coda::domain::{
    AgentId, Project, Route, RunRequest, SendChatTurn, SessionId, SessionKind, Task, TaskSpec,
    TurnId,
};
use coda::engine::Engine;
use coda::network_authorization::{
    AuthorizationState, ConfirmRequest, NetworkSubject, NetworkSubjectKind,
};
use coda::project_config::{
    ConfigureProject, ProjectConfig, ProjectConfigSnapshot, RegisterProject, SelectProject,
    StartInProject,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const WORKBENCH_MAX_ROUNDS: usize = 48;

async fn ensure_project_ready(
    engine: &Arc<Engine>,
    target_dir: PathBuf,
    route_id: &str,
) -> Result<(Project, ProjectConfigSnapshot), Failure> {
    if !target_dir.is_dir() {
        return Err(Failure::custom(
            "invalid_workspace",
            format!("工作目录不存在或不是文件夹: {}", target_dir.display()),
        ));
    }
    let canonical = std::fs::canonicalize(&target_dir).map_err(|e| {
        Failure::custom(
            "invalid_workspace",
            format!("无法解析目录 {}: {e}", target_dir.display()),
        )
    })?;

    let existing_projects = engine
        .store
        .projects()
        .map_err(|e| Failure::custom("project_error", e.to_string()))?;

    let project = if let Some(found) = existing_projects.iter().find(|p| {
        std::fs::canonicalize(&p.root_path)
            .map(|cp| cp == canonical)
            .unwrap_or(false)
    }) {
        found.clone()
    } else if let Some(containing) = existing_projects.iter().find(|p| {
        std::fs::canonicalize(&p.root_path)
            .map(|cp| canonical.starts_with(&cp))
            .unwrap_or(false)
    }) {
        containing.clone()
    } else {
        let folder_name = target_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "workspace".to_owned());
        engine
            .register_project(RegisterProject {
                name: folder_name,
                root_path: canonical.to_string_lossy().into_owned(),
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .map_err(|e| Failure::custom("project_error", format!("注册工作目录失败: {e}")))?
    };

    let mut snap = engine
        .project_config(&project.id)
        .map_err(|e| Failure::custom("project_error", format!("读取项目配置失败: {e}")))?;

    let needs_update = !snap.config.tools
        || !snap.config.allow_commands
        || !snap.config.write_scopes.iter().any(|s| s == "*")
        || !snap.config.allowed_route_ids.iter().any(|r| r == route_id)
        || snap.config.default_route_id.as_deref() != Some(route_id);

    if needs_update {
        let mut allowed = snap.config.allowed_route_ids.clone();
        if !allowed.iter().any(|r| r == route_id) {
            allowed.push(route_id.to_owned());
        }
        let mut scopes = snap.config.write_scopes.clone();
        if !scopes.iter().any(|s| s == "*") {
            scopes = vec!["*".to_owned()];
        }
        snap = engine
            .configure_project(ConfigureProject {
                project_id: project.id.clone(),
                expected_revision: snap.revision,
                config: ProjectConfig {
                    allowed_route_ids: allowed,
                    default_route_id: Some(route_id.to_owned()),
                    tools: true,
                    write_scopes: scopes,
                    allow_commands: true,
                },
            })
            .await
            .map_err(|e| Failure::custom("project_error", format!("更新项目权限失败: {e}")))?;
    }

    let current = engine
        .current_project()
        .map_err(|e| Failure::custom("project_error", e.to_string()))?;
    if current.project_id.as_ref() != Some(&project.id) {
        engine
            .select_project(SelectProject {
                project_id: project.id.clone(),
                expected_revision: current.revision,
            })
            .await
            .map_err(|e| Failure::custom("project_error", format!("选中当前项目失败: {e}")))?;
    }

    // Belt-and-suspenders: ensure network preview/authorization is warm for this project route.
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: route_id.to_owned(),
        project_id: Some(project.id.clone()),
    };
    if let Ok(preview) = engine.network_preview(subject.clone()).await
        && preview
            .authorization
            .as_ref()
            .is_none_or(|auth| auth.state != AuthorizationState::Active)
    {
        let _ = engine
            .network_confirm(
                subject,
                ConfirmRequest {
                    config_revision: preview.config_revision,
                    credential_revision: preview.credential_revision,
                    binding_digest: preview.binding_digest,
                    purposes: preview.purposes,
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .await;
    }

    Ok((project, snap))
}

struct ActiveChatSession {
    session_id: SessionId,
    agent_id: AgentId,
    last_turn_id: TurnId,
    run_id: String,
    event_cursor: i64,
    turn_count: usize,
    task_history: Vec<String>,
}

struct ProjectSessionSummary {
    session_id: SessionId,
    run_id: String,
    title: String,
    agent_id: AgentId,
    last_turn_id: TurnId,
    latest_status: String,
    turn_count: usize,
    task_history: Vec<String>,
    last_event_seq: i64,
    updated_at: u64,
}

impl ProjectSessionSummary {
    fn into_active(self) -> ActiveChatSession {
        ActiveChatSession {
            session_id: self.session_id,
            agent_id: self.agent_id,
            last_turn_id: self.last_turn_id,
            run_id: self.run_id,
            event_cursor: self.last_event_seq,
            turn_count: self.turn_count,
            task_history: self.task_history,
        }
    }
}

fn same_workspace_path(left: &str, right: &str) -> bool {
    if left.eq_ignore_ascii_case(right) {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn drain_latest_event_seq(engine: &Engine, run_id: &str) -> i64 {
    let mut cursor = 0i64;
    while let Ok(batch) = engine.store.events(run_id, cursor) {
        let Some(last) = batch.last() else {
            break;
        };
        if last.seq <= cursor {
            break;
        }
        cursor = last.seq;
        if batch.len() < 500 {
            break;
        }
    }
    cursor
}

fn list_project_chat_sessions(engine: &Engine, project: &Project) -> Vec<ProjectSessionSummary> {
    let Ok(runs) = engine.store.runs() else {
        return Vec::new();
    };
    let mut summaries = Vec::new();
    for item in runs {
        let Some(run_id) = item["id"].as_str() else {
            continue;
        };
        let Ok(ctx) = engine.store.run_context(run_id) else {
            continue;
        };
        if ctx.session.kind != SessionKind::Chat {
            continue;
        }
        if ctx.project.id != project.id
            && !same_workspace_path(&ctx.project.root_path, &project.root_path)
        {
            continue;
        }
        let [latest_task] = ctx.tasks.as_slice() else {
            continue;
        };
        if !latest_task.depends_on.is_empty() {
            continue;
        }
        let turns = engine
            .store
            .turns_for_session(&ctx.session.id)
            .unwrap_or_else(|_| vec![ctx.latest_turn.clone()]);
        let mut task_history = Vec::with_capacity(turns.len().max(1));
        for turn in &turns {
            if let Ok(tt) = engine.store.turn_tasks(&turn.id) {
                for entry in tt {
                    task_history.push(entry.legacy_task_id);
                }
            }
        }
        if task_history.is_empty() {
            task_history.push(latest_task.legacy_task_id.clone());
        }
        let last_event_seq = drain_latest_event_seq(engine, &ctx.run_id);
        let updated_at = ctx.latest_turn.updated_at.max(ctx.session.updated_at);
        let turn_count = turns.len().max(1);
        summaries.push(ProjectSessionSummary {
            session_id: ctx.session.id,
            run_id: ctx.run_id,
            title: ctx.session.title,
            agent_id: latest_task.agent_id.clone(),
            last_turn_id: ctx.latest_turn.id,
            latest_status: ctx.latest_turn.status.as_str().to_owned(),
            turn_count,
            task_history,
            last_event_seq,
            updated_at,
        });
    }
    summaries.sort_by_key(|a| std::cmp::Reverse((a.last_event_seq, a.updated_at)));
    summaries
}

fn resolve_session_by_id(
    engine: &Engine,
    project: &Project,
    target: &str,
) -> Result<ProjectSessionSummary, Failure> {
    let target = target.trim();
    let sessions = list_project_chat_sessions(engine, project);
    if let Some(exact) = sessions
        .iter()
        .position(|s| s.session_id.0 == target || s.run_id == target)
    {
        let mut sessions = sessions;
        return Ok(sessions.swap_remove(exact));
    }
    if target.len() >= 4 {
        let matches: Vec<usize> = sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.session_id.0.starts_with(target) || s.run_id.starts_with(target))
            .map(|(idx, _)| idx)
            .collect();
        if matches.len() == 1 {
            let mut sessions = sessions;
            return Ok(sessions.swap_remove(matches[0]));
        }
        if matches.len() > 1 {
            return Err(Failure::custom(
                "ambiguous_session",
                format!("会话前缀 `{target}` 匹配到多个历史会话，请提供完整 ID"),
            ));
        }
    }
    Err(Failure::custom(
        "session_not_found",
        format!("当前项目中未找到会话或运行 `{target}`"),
    ))
}

fn resolve_initial_session(
    engine: &Engine,
    project: &Project,
    continue_session: bool,
    resume_id: Option<&str>,
    json_output: bool,
) -> Result<Option<ActiveChatSession>, Failure> {
    if let Some(target) = resume_id {
        let summary = resolve_session_by_id(engine, project, target)?;
        if !json_output {
            eprintln!(
                "\x1b[90m  ↻ Resuming session {} ({} turns): {}\x1b[0m",
                summary.session_id.0, summary.turn_count, summary.title
            );
        }
        return Ok(Some(summary.into_active()));
    }
    if continue_session {
        let Some(summary) = list_project_chat_sessions(engine, project)
            .into_iter()
            .next()
        else {
            return Err(Failure::fixed(
                "session_not_found",
                "当前工作目录尚无历史对话可继续；请去掉 --continue (-c) 开启新会话",
                false,
            ));
        };
        if !json_output {
            eprintln!(
                "\x1b[90m  ↻ Continuing session {} ({} turns): {}\x1b[0m",
                summary.session_id.0, summary.turn_count, summary.title
            );
        }
        return Ok(Some(summary.into_active()));
    }
    Ok(None)
}

fn exploration_scope(arguments: &Value) -> String {
    let paths = arguments["paths"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(4)
        .map(|path| {
            let mut chars = path.chars();
            let mut label: String = chars.by_ref().take(60).collect();
            if chars.next().is_some() {
                label.push('…');
            }
            format!("{label:?}")
        })
        .collect::<Vec<_>>();
    if paths.is_empty() {
        "Exploring files".to_owned()
    } else {
        format!("Exploring {}", paths.join(", "))
    }
}

fn summarize_tool_start(engine: &Engine, task_id: &str, name: &str, data: &Value) -> String {
    if name == "explore_parallel" {
        return match (data["tool_call_id"].as_str(), data["total"].as_u64()) {
            (Some(id), Some(total)) => format!("[{id:?}] Exploring {total} branches"),
            _ => "Exploring branches".into(),
        };
    }
    if name == "explore_files" {
        return exploration_scope(&data["arguments"]);
    }
    let args_val = if data["arguments"].is_object() {
        Some(data["arguments"].clone())
    } else if let Some(call_id) = data["tool_call_id"].as_str()
        && let Ok(task) = engine.store.task(task_id)
    {
        task.messages
            .iter()
            .filter_map(|m| m["tool_calls"].as_array())
            .flatten()
            .find(|c| c["id"].as_str() == Some(call_id))
            .and_then(|c| c["function"]["arguments"].as_str())
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
    } else {
        data["preview"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
    };
    if let Some(parsed) = args_val {
        match name {
            "read_file" => {
                if let Some(p) = parsed["path"].as_str() {
                    let start = parsed.get("start_line").and_then(Value::as_u64);
                    let end = parsed.get("end_line").and_then(Value::as_u64);
                    return match (start, end) {
                        (Some(s), Some(e)) => format!("{p}:{s}-{e}"),
                        (Some(s), None) => format!("{p}:{s}-"),
                        (None, Some(e)) => format!("{p}:1-{e}"),
                        (None, None) => p.to_owned(),
                    };
                }
            }
            "write_file" | "edit_file" | "list_files" => {
                if let Some(p) = parsed["path"].as_str() {
                    return p.to_owned();
                }
            }
            "glob_files" => {
                if let Some(pat) = parsed["pattern"].as_str() {
                    if let Some(sub) = parsed["path"]
                        .as_str()
                        .filter(|p| !p.is_empty() && *p != ".")
                    {
                        return format!("{pat} (in {sub})");
                    }
                    return pat.to_owned();
                }
            }
            "search_files" => {
                if let Some(q) = parsed["query"].as_str() {
                    if let Some(glob) = parsed["file_pattern"].as_str().filter(|g| !g.is_empty()) {
                        return format!("{q:?} ({glob})");
                    }
                    return format!("{q:?}");
                }
            }
            "run_command" => {
                if let Some(cmd) = parsed["command"].as_str() {
                    let one_line = cmd.trim().replace('\n', "; ");
                    return one_line.chars().take(90).collect();
                }
            }
            "git_status" => {
                if let Some(p) = parsed["path"]
                    .as_str()
                    .filter(|p| !p.is_empty() && *p != ".")
                {
                    return p.to_owned();
                }
                return String::new();
            }
            "todo_write" => {
                if let Some(items) = parsed
                    .get("todos")
                    .or_else(|| parsed.get("items"))
                    .and_then(Value::as_array)
                {
                    let total = items.len();
                    let completed = items
                        .iter()
                        .filter(|i| i["status"].as_str() == Some("completed"))
                        .count();
                    let in_progress = items
                        .iter()
                        .filter(|i| i["status"].as_str() == Some("in_progress"))
                        .count();
                    if let Some(active_item) = items
                        .iter()
                        .find(|i| i["status"].as_str() == Some("in_progress"))
                        .and_then(|i| {
                            i["content"]
                                .as_str()
                                .or_else(|| i["title"].as_str())
                                .or_else(|| i["task"].as_str())
                        })
                    {
                        let short: String = active_item.trim().chars().take(48).collect();
                        return format!("{completed}/{total} done · active: {short}");
                    }
                    return format!(
                        "{total} items ({completed} completed, {in_progress} in_progress)"
                    );
                }
            }
            other if other.starts_with("mcp__") => {
                if let Some(obj) = parsed.as_object() {
                    if obj.is_empty() {
                        return String::new();
                    }
                    let compact = Value::Object(obj.clone()).to_string();
                    return compact.chars().take(80).collect();
                }
            }
            _ => {}
        }
    }
    data["preview"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect()
}

fn render_ansi_unified_diff(diff_text: &str) {
    let mut stderr = io::stderr().lock();
    let max_lines = 120usize;
    let lines: Vec<&str> = diff_text.lines().collect();
    for line in lines.iter().take(max_lines) {
        if line.starts_with("+++") || line.starts_with("---") {
            let _ = writeln!(stderr, "  \x1b[1m{line}\x1b[0m");
        } else if line.starts_with("@@") {
            let _ = writeln!(stderr, "  \x1b[36m{line}\x1b[0m");
        } else if line.starts_with('+') {
            let _ = writeln!(stderr, "  \x1b[32m{line}\x1b[0m");
        } else if line.starts_with('-') {
            let _ = writeln!(stderr, "  \x1b[31m{line}\x1b[0m");
        } else {
            let _ = writeln!(stderr, "  \x1b[90m{line}\x1b[0m");
        }
    }
    if lines.len() > max_lines {
        let _ = writeln!(
            stderr,
            "  \x1b[90m... ({} more diff lines)\x1b[0m",
            lines.len() - max_lines
        );
    }
    let _ = stderr.flush();
}

fn display_approval_preview(engine: &Engine, record: &coda::approval::ApprovalRecord) {
    if record.tool_name.starts_with("mcp__")
        && let Ok(Some(endpoint)) = engine.store.approval_mcp_http_endpoint(record)
    {
        eprintln!(
            "\n\x1b[1;33m● {}\x1b[0m\n  MCP HTTP destination: {endpoint}",
            record.tool_name
        );
        let detail = record
            .preview
            .split_once('\n')
            .filter(|(header, _)| header.starts_with("MCP HTTP "))
            .map_or(record.preview.as_str(), |(_, detail)| detail);
        if !detail.is_empty() && detail != record.tool_name {
            eprintln!("  {detail}");
        }
        return;
    }
    let Ok(task) = engine.store.task(&record.task_id) else {
        eprintln!(
            "\n\x1b[33m[approval]\x1b[0m {} {}",
            record.tool_name, record.preview
        );
        return;
    };

    let raw_args = task
        .messages
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .find(|c| c["id"].as_str() == Some(record.tool_call_id.as_str()))
        .and_then(|c| c["function"]["arguments"].as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .or_else(|| serde_json::from_str::<Value>(&record.preview).ok());

    if matches!(record.tool_name.as_str(), "write_file" | "edit_file")
        && let Some(args) = &raw_args
        && let Ok((_, before_bytes, after_bytes, rel_path)) = coda::workspace::preview_file_mutation(
            Path::new(&task.workspace),
            &record.tool_name,
            args,
            &task.spec.write_scopes,
        )
    {
        let before_str = before_bytes
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("");
        let after_str = std::str::from_utf8(&after_bytes).unwrap_or("");
        eprintln!(
            "\n\x1b[1;33m● {}\x1b[0m \x1b[1m{}\x1b[0m",
            record.tool_name, rel_path
        );
        let patch = coda::git_diff::unified_diff(&rel_path, before_str, after_str, 3);
        if !patch.trim().is_empty() {
            render_ansi_unified_diff(&patch);
        }
        return;
    }

    if record.tool_name == "run_command" {
        let cmd = raw_args
            .as_ref()
            .and_then(|a| a["command"].as_str())
            .unwrap_or(&record.preview);
        eprintln!("\n\x1b[1;33m● run_command\x1b[0m (PowerShell):\n  \x1b[1m{cmd}\x1b[0m");
        return;
    }

    eprintln!(
        "\n\x1b[1;33m● {}\x1b[0m {}",
        record.tool_name, record.preview
    );
}

async fn prompt_inline_approval(session_auto_approve: &mut bool) -> bool {
    if !io::stdin().is_terminal() {
        eprintln!("非交互终端且未开启 --auto-approve (-y)，自动拒绝待审批操作。");
        return false;
    }
    eprint!("\x1b[1;36mApprove? [y]es / [n]o / [a]llow-session:\x1b[0m ");
    let _ = io::stderr().flush();
    let answer = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
        line
    })
    .await
    .unwrap_or_default();

    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "a" | "allow" | "allow-session" | "all" => {
            *session_auto_approve = true;
            eprintln!("\x1b[32m✓ 已开启当前会话自动批准 (输入 /auto 可随时切换)\x1b[0m");
            true
        }
        _ => false,
    }
}

async fn execute_turn_and_stream(
    engine: &Arc<Engine>,
    project: &Project,
    route: &Route,
    session_state: &mut Option<ActiveChatSession>,
    prompt: &str,
    auto_approve: &mut bool,
    json_output: bool,
) -> Result<Task, Failure> {
    let enriched_prompt =
        expand_context_mentions(Path::new(&project.root_path), prompt, json_output);

    let (task_id, run_id, mut cursor) = if let Some(active) = session_state.as_mut() {
        match engine
            .send_chat_turn(SendChatTurn {
                session_id: active.session_id.clone(),
                agent_id: active.agent_id.clone(),
                expected_last_turn_id: active.last_turn_id.clone(),
                message: enriched_prompt.clone(),
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            })
            .await
        {
            Ok(receipt) => {
                active.last_turn_id = receipt.turn.id;
                active.turn_count += 1;
                active
                    .task_history
                    .push(receipt.task.legacy_task_id.clone());
                (
                    receipt.task.legacy_task_id,
                    active.run_id.clone(),
                    active.event_cursor,
                )
            }
            Err(err) => {
                if !json_output {
                    eprintln!("\x1b[33m  ! 无法继续原会话 ({err})，已开启新会话\x1b[0m");
                }
                *session_state = None;
                start_fresh_session(
                    engine,
                    project,
                    route,
                    prompt,
                    &enriched_prompt,
                    session_state,
                )
                .await?
            }
        }
    } else {
        start_fresh_session(
            engine,
            project,
            route,
            prompt,
            &enriched_prompt,
            session_state,
        )
        .await?
    };

    let mut handled_approvals = HashSet::new();
    let mut mid_line = false;
    let mut in_reasoning = false;
    let mut reasoning_ended_newline = false;

    let drain_events = |cursor: &mut i64,
                        mid_line: &mut bool,
                        in_reasoning: &mut bool,
                        reasoning_ended_newline: &mut bool| {
        while let Ok(events) = engine.store.events(&run_id, *cursor) {
            if events.is_empty() {
                break;
            }
            let batch_len = events.len();
            let prev_cursor = *cursor;
            for ev in events {
                *cursor = ev.seq;
                if ev.task_id != task_id || json_output {
                    continue;
                }
                match ev.kind.as_str() {
                    "unknown:mcp_login_required" => {
                        if let Some(server) = ev.data["server"].as_str() {
                            finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                            eprintln!(
                                "MCP {server} 需要登录。请在此工作区运行 coda mcp login {server}，或在聊天中输入 /mcp login {server}。"
                            );
                        }
                    }
                    "reasoning_delta" => {
                        if let Some(text) = ev.data["text"].as_str()
                            && !text.is_empty()
                        {
                            if !*in_reasoning {
                                if *mid_line {
                                    println!();
                                    *mid_line = false;
                                }
                                eprint!("\x1b[90m  💭 ");
                                *in_reasoning = true;
                            }
                            eprint!("\x1b[90m{text}\x1b[0m");
                            let _ = io::stderr().flush();
                            *reasoning_ended_newline = text.ends_with('\n');
                        }
                    }
                    "delta" => {
                        if let Some(text) = ev.data["text"].as_str()
                            && !text.is_empty()
                        {
                            finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                            print!("{text}");
                            let _ = io::stdout().flush();
                            *mid_line = !text.ends_with('\n');
                        }
                    }
                    "tool_start" | "tool.started" => {
                        finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                        let name = ev.data["name"]
                            .as_str()
                            .or_else(|| ev.data["tool_name"].as_str())
                            .unwrap_or("tool");
                        if *mid_line
                            && (!matches!(name, "explore_files" | "explore_parallel")
                                || io::stdout().is_terminal())
                        {
                            println!();
                            *mid_line = false;
                        }
                        let detail = summarize_tool_start(engine, &task_id, name, &ev.data);
                        let badge = if name.starts_with("mcp__") {
                            "mcp"
                        } else {
                            "tool"
                        };
                        if detail.is_empty() {
                            eprintln!("\x1b[90m  ▸ [{badge}] {name}\x1b[0m");
                        } else {
                            eprintln!("\x1b[90m  ▸ [{badge}] {name} {detail}\x1b[0m");
                        }
                    }
                    "unknown:exploration_branch" if ev.data["name"] == "explore_parallel" => {
                        let (Some(id), Some(index), Some(total)) = (
                            ev.data["tool_call_id"].as_str(),
                            ev.data["index"].as_u64(),
                            ev.data["total"].as_u64(),
                        ) else {
                            continue;
                        };
                        let status = match ev.data["status"].as_str() {
                            Some("ok") => "Summary ready",
                            Some("no_matches") => "No matches",
                            Some("error") => "Failed",
                            _ => continue,
                        };
                        finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                        eprintln!(
                            "\x1b[90m    ↳ [explore_parallel] [{id:?}] Branch {index}/{total}: {status}\x1b[0m"
                        );
                    }
                    "tool_result" | "tool.finished" => {
                        let name = ev.data["name"]
                            .as_str()
                            .or_else(|| ev.data["tool_name"].as_str())
                            .unwrap_or("tool");
                        let res = &ev.data["result"];
                        if name == "explore_parallel" {
                            if let Some(id) = ev.data["tool_call_id"].as_str() {
                                finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                                let status = if res["branches"].is_array() {
                                    "Results returned"
                                } else {
                                    "Exploration failed"
                                };
                                eprintln!("\x1b[90m    ↳ [{name}] [{id:?}] {status}\x1b[0m");
                            }
                        } else if name == "explore_files" {
                            finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                            if res["summary"].is_string() && res.get("error").is_none() {
                                eprintln!("\x1b[90m    ↳ [{name}] Summary ready\x1b[0m");
                            } else {
                                eprintln!("\x1b[33m    ↳ [{name}] No summary returned\x1b[0m");
                            }
                        } else if let Some(err) = res.get("error").and_then(Value::as_str) {
                            finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                            eprintln!("\x1b[33m    ↳ [{name}] error: {err}\x1b[0m");
                        } else if let Some(code) = res.get("exit_code").and_then(Value::as_i64)
                            && code != 0
                        {
                            finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                            eprintln!("\x1b[33m    ↳ [{name}] exit_code={code}\x1b[0m");
                        }
                    }
                    _ => {}
                }
            }
            if *cursor <= prev_cursor || batch_len < 500 {
                break;
            }
        }
    };

    loop {
        drain_events(
            &mut cursor,
            &mut mid_line,
            &mut in_reasoning,
            &mut reasoning_ended_newline,
        );

        if let Ok(approvals) = engine.store.approvals_for_task(&task_id) {
            for record in approvals {
                if record.status == ApprovalStatus::Pending
                    && handled_approvals.insert(record.id.clone())
                {
                    if !json_output {
                        finish_reasoning_line(&mut in_reasoning, &mut reasoning_ended_newline);
                    }
                    if mid_line && !json_output {
                        println!();
                        mid_line = false;
                    }
                    if !json_output {
                        display_approval_preview(engine, &record);
                    }
                    let approve = if *auto_approve {
                        if !json_output {
                            eprintln!("\x1b[90m  ✓ [auto-approved] {}\x1b[0m", record.tool_name);
                        }
                        true
                    } else if json_output {
                        false
                    } else {
                        prompt_inline_approval(auto_approve).await
                    };
                    let ack_net = approve && record.tool_name == "run_command";
                    let _ = engine.decide_approval_with_network_ack(
                        &record.id,
                        approve,
                        Some(if *auto_approve { "cli-auto" } else { "user" }),
                        ack_net,
                    );
                }
            }
        }

        let task = engine
            .store
            .task(&task_id)
            .map_err(|e| Failure::custom("task_error", format!("读取任务状态失败: {e}")))?;

        if !matches!(task.status.as_str(), "queued" | "running") && !engine.is_busy() {
            drain_events(
                &mut cursor,
                &mut mid_line,
                &mut in_reasoning,
                &mut reasoning_ended_newline,
            );
            if !json_output {
                finish_reasoning_line(&mut in_reasoning, &mut reasoning_ended_newline);
            }
            if mid_line && !json_output {
                println!();
            }
            if !json_output && task.usage.is_object() {
                let prompt_tokens = task.usage["prompt_tokens"].as_u64();
                let completion_tokens = task.usage["completion_tokens"].as_u64();
                let total_tokens = task.usage["total_tokens"].as_u64();
                if prompt_tokens.is_some() || completion_tokens.is_some() || total_tokens.is_some()
                {
                    let p = prompt_tokens.unwrap_or(0);
                    let c = completion_tokens.unwrap_or(0);
                    let t = total_tokens.unwrap_or(p + c);
                    eprintln!("\x1b[90m  [tokens: prompt={p}, completion={c}, total={t}]\x1b[0m");
                }
            }
            if let Some(active) = session_state.as_mut() {
                active.event_cursor = cursor;
            }
            return Ok(task);
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(25)) => {}
            _ = tokio::signal::ctrl_c() => {
                if !json_output {
                    finish_reasoning_line(&mut in_reasoning, &mut reasoning_ended_newline);
                }
                if mid_line && !json_output {
                    println!();
                    mid_line = false;
                }
                if !json_output {
                    eprintln!("\x1b[33m^C 正在停止当前回合...\x1b[0m");
                }
                let _ = engine.cancel(&task_id);
            }
        }
    }
}

fn finish_reasoning_line(in_reasoning: &mut bool, reasoning_ended_newline: &mut bool) {
    if *in_reasoning {
        if !*reasoning_ended_newline {
            eprintln!();
        }
        *in_reasoning = false;
        *reasoning_ended_newline = false;
    }
}

async fn start_fresh_session(
    engine: &Arc<Engine>,
    project: &Project,
    route: &Route,
    display_prompt: &str,
    enriched_prompt: &str,
    session_state: &mut Option<ActiveChatSession>,
) -> Result<(String, String, i64), Failure> {
    let snap = engine
        .project_config(&project.id)
        .map_err(|e| Failure::custom("project_error", e.to_string()))?;
    let first_line = display_prompt.lines().next().unwrap_or(display_prompt);
    let title: String = first_line.trim().chars().take(60).collect();
    let run = engine
        .start_in_project(StartInProject {
            project_id: project.id.clone(),
            expected_config_revision: snap.revision,
            request: RunRequest {
                title: if title.is_empty() {
                    "coding session".into()
                } else {
                    title
                },
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "coder".into(),
                    role: "autonomous coding agent".into(),
                    route_id: route.id.clone(),
                    prompt: enriched_prompt.to_owned(),
                    depends_on: vec![],
                    write_scopes: vec!["*".into()],
                    tools: true,
                    allow_commands: true,
                    max_rounds: WORKBENCH_MAX_ROUNDS,
                }],
            },
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        })
        .await
        .map_err(|e| Failure::custom("start_error", format!("启动会话失败: {e}")))?;

    let task_id = run
        .tasks
        .first()
        .map(|t| t.id.clone())
        .ok_or_else(|| Failure::fixed("start_error", "新运行未包含任务", false))?;

    if let Ok(ctx) = engine.store.run_context(&run.id)
        && let Some(first_task) = ctx.tasks.first()
    {
        *session_state = Some(ActiveChatSession {
            session_id: ctx.session.id,
            agent_id: first_task.agent_id.clone(),
            last_turn_id: ctx.latest_turn.id,
            run_id: run.id.clone(),
            event_cursor: 0,
            turn_count: 1,
            task_history: vec![task_id.clone()],
        });
    }

    Ok((task_id, run.id, 0))
}

fn run_quick_git(workspace: &Path, args: &[&str]) -> Option<String> {
    let git_exe = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .filter(|d| d.is_absolute())
            .map(|d| d.join(if cfg!(windows) { "git.exe" } else { "git" }))
            .find(|c| c.is_file() && !c.starts_with(workspace))
    })?;
    let mut cmd = std::process::Command::new(git_exe);
    cmd.current_dir(workspace)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["--no-pager"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
}

fn read_repl_input<R: BufRead, F: FnMut()>(
    reader: &mut R,
    mut on_continuation: F,
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    loop {
        let mut buf = String::new();
        match reader.read_line(&mut buf) {
            Ok(0) => {
                if lines.is_empty() {
                    return None;
                }
                break;
            }
            Ok(_) => {
                let trimmed_eol = buf.trim_end_matches(['\r', '\n']);
                if let Some(without_slash) = trimmed_eol.strip_suffix('\\') {
                    lines.push(without_slash.to_owned());
                    on_continuation();
                } else {
                    lines.push(trimmed_eol.to_owned());
                    break;
                }
            }
            Err(_) => {
                return if lines.is_empty() {
                    None
                } else {
                    Some(lines.join("\n"))
                };
            }
        }
    }
    Some(lines.join("\n"))
}

fn print_repl_help() {
    println!(
        "\x1b[1mREPL Slash Commands:\x1b[0m\n\
         \x20 \x1b[36m/jobs [command]\x1b[0m       List, show, submit, continue or retry exploration jobs\n\
         \x20 \x1b[36m/sessions\x1b[0m             List recent chat sessions in this workspace\n\
         \x20 \x1b[36m/resume <id>\x1b[0m          Switch active context to continue a previous session\n\
         \x20 \x1b[36m/todos\x1b[0m                Show structured task checklist ([✓] / [▸] / [ ]) for the active session\n\
         \x20 \x1b[36m/checkpoint\x1b[0m           Create a durable checkpoint on the latest modified turn\n\
         \x20 \x1b[36m/checkpoints\x1b[0m          List durable checkpoints for the active session (or project)\n\
         \x20 \x1b[36m/restore <ckpt_id>\x1b[0m    Restore workspace files from a durable checkpoint\n\
         \x20 \x1b[36m/compact\x1b[0m              Compact message history and show tokens/KB saved\n\
         \x20 \x1b[36m/diff\x1b[0m                 Show workspace Git status/diff summary & recorded task changes\n\
         \x20 \x1b[36m/undo\x1b[0m                 Restore workspace file edits from the most recent modified turn\n\
         \x20 \x1b[36m/status\x1b[0m               Show active workspace, route, model, session turns & token usage\n\
         \x20 \x1b[36m/mcp login <server>\x1b[0m   Authorize a registered MCP server in this workspace\n\
         \x20 \x1b[36m/routes\x1b[0m               List configured provider routes (active marked with *)\n\
         \x20 \x1b[36m/route [id]\x1b[0m           Show routes or switch active route (resets session if changed)\n\
         \x20 \x1b[36m/model <name>\x1b[0m         Switch active route model seamlessly while keeping session context\n\
         \x20 \x1b[36m/auto\x1b[0m                 Toggle automatic approval for file edits & shell commands\n\
         \x20 \x1b[36m/new\x1b[0m                  Start a fresh conversation session\n\
         \x20 \x1b[36m/help\x1b[0m                 Show this command reference\n\
         \x20 \x1b[36m/exit\x1b[0m                 Cleanly shut down and exit Coda CLI\n\
         \x1b[90mTip: Reference files with @path, @path:start-end, or @dir/; end a line with \\ for multiline input.\x1b[0m"
    );
}

pub(super) async fn run_chat(
    instance_lock: &mut Option<std::fs::File>,
    top_data_dir: Option<PathBuf>,
    top_workspace: Option<PathBuf>,
    json_output: bool,
    args: ChatArgs,
) -> Result<(), Failure> {
    let data_dir = resolve_user_data_dir(args.data_dir.or(top_data_dir));
    let target_dir = args
        .dir
        .or(top_workspace)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let engine = open_local_engine(instance_lock, &data_dir)?;
    let result = async {
        let mut route =
            ensure_route_ready(&engine, args.route.as_deref(), args.model.as_deref()).await?;
        let (project, _) = ensure_project_ready(&engine, target_dir, &route.id).await?;
        let mut auto_approve = args.auto_approve;
        let mut session_state = resolve_initial_session(
            &engine,
            &project,
            args.continue_session,
            args.resume.as_deref(),
            json_output,
        )?;

        if !json_output {
            println!(
                "\x1b[1;35m⚡ Coda CLI\x1b[0m · model=\x1b[1m{}\x1b[0m · route=\x1b[36m{}\x1b[0m · auto-approve={}\n\
                 \x1b[90mWorkspace: {}  (type /help for commands, /exit to quit)\x1b[0m",
                route.model,
                route.id,
                if auto_approve { "\x1b[32mON\x1b[0m" } else { "off" },
                project.root_path
            );
        }

        let initial = args.prompt.join(" ");
        if !initial.trim().is_empty() {
            let task = execute_turn_and_stream(
                &engine,
                &project,
                &route,
                &mut session_state,
                initial.trim(),
                &mut auto_approve,
                json_output,
            )
            .await?;
            if task.status != "completed"
                && let Some(err) = &task.error
            {
                eprintln!("\x1b[31m[{}] {}\x1b[0m", task.status, err);
            }
        }

        loop {
            if !json_output {
                print!("\n\x1b[1;32mcoda>\x1b[0m ");
                let _ = io::stdout().flush();
            }
            let read = tokio::task::spawn_blocking(move || {
                let stdin = io::stdin();
                let mut lock = stdin.lock();
                read_repl_input(&mut lock, || {
                    if !json_output {
                        print!("... ");
                        let _ = io::stdout().flush();
                    }
                })
            });
            // On Unix, Tokio's first Ctrl+C listener replaces the default SIGINT
            // action for the process lifetime. Keep idle Ctrl+C usable after a
            // /jobs command. Do not race a chat turn or slash command for input:
            // if interrupted here we leave the REPL, never start another reader.
            #[cfg(unix)]
            let maybe_line = tokio::select! {
                signal = tokio::signal::ctrl_c() => {
                    signal.map_err(|e| Failure::custom("signal_error", e.to_string()))?;
                    break;
                }
                line = read => line.unwrap_or(None),
            };
            #[cfg(not(unix))]
            let maybe_line = read.await.unwrap_or(None);

            let Some(raw_line) = maybe_line else {
                break;
            };
            let input = raw_line.trim();
            if input.is_empty() {
                continue;
            }

            if !input.contains('\n')
                && let Some(cmd_rest) = input.strip_prefix('/')
            {
                let (slash_cmd, arg_str) = cmd_rest
                    .split_once(char::is_whitespace)
                    .map(|(c, a)| (c, a.trim()))
                    .unwrap_or((cmd_rest, ""));
                match slash_cmd {
                    "exit" | "quit" | "q" => break,
                    "help" | "h" | "?" => print_repl_help(),
                    "mcp" => {
                        let args: Vec<_> = arg_str.split_whitespace().collect();
                        let (server, callback_port) = match args.as_slice() {
                            ["login", server] => (*server, Some(0)),
                            ["login", server, "--callback-port", port] => (*server, port.parse::<u16>().ok()),
                            _ => ("", None),
                        };
                        let Some(port) = callback_port else {
                            eprintln!("用法：/mcp login <server> [--callback-port <port>]");
                            continue;
                        };
                        match coda::mcp::oauth::login(&engine.store, Path::new(&project.root_path), server, port).await {
                            Ok(()) => println!("MCP OAuth 登录成功。"),
                            Err(error) => eprintln!("MCP OAuth 登录失败：{error}"),
                        }
                    }
                    "auto" => {
                        auto_approve = !auto_approve;
                        println!(
                            "Auto-approve is now {}",
                            if auto_approve {
                                "\x1b[32mON\x1b[0m"
                            } else {
                                "\x1b[33mOFF\x1b[0m"
                            }
                        );
                    }
                    "new" | "clear" => {
                        session_state = None;
                        println!("\x1b[32m✓ 已重置上下文；下一条消息将开启全新会话。\x1b[0m");
                    }
                    "jobs" => {
                        let Some(active) = &session_state else {
                            eprintln!("请先开启会话或用 /resume 选择会话。");
                            continue;
                        };
                        match super::jobs::slash(&engine, &active.session_id, arg_str, json_output).await {
                            Ok(()) => {}
                            Err(error) => error.print(json_output),
                        }
                    }
                    "sessions" => {
                        let sessions = list_project_chat_sessions(&engine, &project);
                        if sessions.is_empty() {
                            println!("当前项目暂无历史会话记录。");
                        } else {
                            let active_id = session_state.as_ref().map(|a| a.session_id.0.as_str());
                            println!("\x1b[1mRecent Sessions ({}):\x1b[0m", project.name);
                            for item in sessions.iter().take(20) {
                                let marker = if active_id == Some(item.session_id.0.as_str()) {
                                    "\x1b[32m*\x1b[0m"
                                } else {
                                    " "
                                };
                                println!(
                                    " {marker} \x1b[36m{}\x1b[0m  [{}]  ({} turn{})  {}",
                                    item.session_id.0,
                                    item.latest_status,
                                    item.turn_count,
                                    if item.turn_count == 1 { "" } else { "s" },
                                    item.title
                                );
                            }
                        }
                    }
                    "resume" => {
                        if arg_str.is_empty() {
                            println!("用法: /resume <session_id_or_run_id> (输入 /sessions 查看列表)");
                        } else {
                            match resolve_session_by_id(&engine, &project, arg_str) {
                                Ok(summary) => {
                                    println!(
                                        "\x1b[32m✓ 已切换到会话 {} ({} turns, status={}): {}\x1b[0m",
                                        summary.session_id.0,
                                        summary.turn_count,
                                        summary.latest_status,
                                        summary.title
                                    );
                                    session_state = Some(summary.into_active());
                                }
                                Err(err) => err.print(false),
                            }
                        }
                    }
                    "todos" | "todo" => {
                        let latest_tid = session_state
                            .as_ref()
                            .and_then(|active| active.task_history.last())
                            .cloned();
                        match latest_tid {
                            Some(tid) => match engine.store.task(&tid) {
                                Ok(task) => {
                                    let items = coda::workspace::todos_for_task(&task);
                                    if items.is_empty() {
                                        println!("当前会话暂无 Todo 任务项。");
                                    } else {
                                        let completed = items
                                            .iter()
                                            .filter(|i| i.status == "completed")
                                            .count();
                                        let in_progress = items
                                            .iter()
                                            .filter(|i| i.status == "in_progress")
                                            .count();
                                        let pending = items
                                            .iter()
                                            .filter(|i| i.status == "pending")
                                            .count();
                                        println!(
                                            "\x1b[1mSession Todos\x1b[0m ({} total: \x1b[32m{} completed\x1b[0m, \x1b[36m{} in_progress\x1b[0m, {} pending):",
                                            items.len(),
                                            completed,
                                            in_progress,
                                            pending
                                        );
                                        for item in &items {
                                            let badge = match item.status.as_str() {
                                                "completed" => "\x1b[32m[✓]\x1b[0m",
                                                "in_progress" => "\x1b[36m[▸]\x1b[0m",
                                                _ => "\x1b[90m[ ]\x1b[0m",
                                            };
                                            println!(
                                                "  {badge} {} \x1b[90m({})\x1b[0m",
                                                item.content, item.id
                                            );
                                        }
                                    }
                                }
                                Err(err) => eprintln!("\x1b[31m读取会话任务失败: {err}\x1b[0m"),
                            },
                            None => println!("当前没有活跃会话可查看 Todo 清单。"),
                        }
                    }
                    "checkpoint" => {
                        let target_task = session_state.as_ref().and_then(|active| {
                            active
                                .task_history
                                .iter()
                                .rev()
                                .find(|tid| {
                                    engine
                                        .changes(tid)
                                        .map(|c| !c.is_empty())
                                        .unwrap_or(false)
                                })
                                .or_else(|| active.task_history.last())
                                .cloned()
                        });
                        match target_task {
                            Some(tid) => {
                                let key = format!("cli-ckpt-{tid}");
                                match engine.create_checkpoint(&tid, &key).await {
                                    Ok(created) => {
                                        println!(
                                            "\x1b[32m✓ Checkpoint created:\x1b[0m \x1b[1m{}\x1b[0m (task={}, files={}, replayed={})",
                                            created.checkpoint.checkpoint_id,
                                            created.checkpoint.task_id,
                                            created.checkpoint.entries.len(),
                                            created.replayed
                                        );
                                        for entry in &created.checkpoint.entries {
                                            println!("  - {}", entry.path);
                                        }
                                    }
                                    Err(err) => {
                                        eprintln!("\x1b[31m创建检查点失败: {err}\x1b[0m");
                                    }
                                }
                            }
                            None => println!("当前会话尚无已完成任务可创建检查点。"),
                        }
                    }
                    "checkpoints" => {
                        let task_ids: Vec<String> = if let Some(active) = &session_state {
                            active.task_history.clone()
                        } else {
                            list_project_chat_sessions(&engine, &project)
                                .into_iter()
                                .flat_map(|s| s.task_history)
                                .collect()
                        };
                        if task_ids.is_empty() {
                            println!("当前项目暂无可用检查点记录。");
                        } else {
                            match engine.checkpoints_for_tasks(&task_ids) {
                                Ok(checkpoints) if checkpoints.is_empty() => {
                                    println!(
                                        "当前会话暂无已创建的检查点（输入 /checkpoint 创建检查点）。"
                                    );
                                }
                                Ok(checkpoints) => {
                                    println!(
                                        "\x1b[1mCheckpoints ({}):\x1b[0m",
                                        checkpoints.len()
                                    );
                                    for ckpt in checkpoints {
                                        let paths: Vec<&str> =
                                            ckpt.entries.iter().map(|e| e.path.as_str()).collect();
                                        let paths_summary = if paths.is_empty() {
                                            "(no file entries)".to_owned()
                                        } else {
                                            paths.join(", ")
                                        };
                                        println!(
                                            "  \x1b[36m{}\x1b[0m  task={}  ({} file{})  {}",
                                            ckpt.checkpoint_id,
                                            ckpt.task_id,
                                            ckpt.entries.len(),
                                            if ckpt.entries.len() == 1 { "" } else { "s" },
                                            paths_summary
                                        );
                                    }
                                }
                                Err(err) => {
                                    eprintln!("\x1b[31m查询检查点列表失败: {err}\x1b[0m");
                                }
                            }
                        }
                    }
                    "restore" => {
                        if arg_str.is_empty() {
                            println!("用法: /restore <checkpoint_id>");
                        } else {
                            match engine.restore_checkpoint(arg_str).await {
                                Ok(receipt) => {
                                    println!(
                                        "\x1b[32m✓ 已恢复检查点 {arg_str} ({} 处文件, status={:?}):\x1b[0m",
                                        receipt.restored, receipt.status
                                    );
                                    for item in &receipt.outcomes {
                                        println!("  - {} ({:?})", item.path, item.status);
                                    }
                                }
                                Err(err) => {
                                    eprintln!("\x1b[31m恢复检查点失败: {err}\x1b[0m");
                                }
                            }
                        }
                    }
                    "compact" => {
                        let latest_tid = session_state
                            .as_ref()
                            .and_then(|active| active.task_history.last())
                            .cloned();
                        match latest_tid {
                            Some(tid) => match engine.store.task(&tid) {
                                Ok(mut task) => {
                                    let before_count = task.messages.len();
                                    let before_bytes = serde_json::to_vec(&task.messages)
                                        .map(|b| b.len())
                                        .unwrap_or(0);
                                    let compacted =
                                        coda::engine::compact_messages(&task.messages);
                                    let after_count = compacted.len();
                                    let after_bytes = serde_json::to_vec(&compacted)
                                        .map(|b| b.len())
                                        .unwrap_or(0);
                                    let saved_kb =
                                        before_bytes.saturating_sub(after_bytes) as f64 / 1024.0;
                                    if compacted != task.messages {
                                        task.messages = compacted;
                                        task.updated_at = coda::domain::now();
                                        let _ = engine.store.save_task(&task);
                                        println!(
                                            "\x1b[32m✓ Context compacted:\x1b[0m {before_count} -> {after_count} messages ({saved_kb:.1} KB saved)"
                                        );
                                    } else {
                                        println!(
                                            "\x1b[90m✓ Context already compact:\x1b[0m {before_count} -> {after_count} messages ({saved_kb:.1} KB saved)"
                                        );
                                    }
                                }
                                Err(err) => eprintln!("\x1b[31m读取任务消息失败: {err}\x1b[0m"),
                            },
                            None => println!("当前没有活跃会话可压缩。"),
                        }
                    }
                    "routes" | "route" => {
                        if arg_str.is_empty() {
                            match engine.store.settings() {
                                Ok(Some(settings)) if !settings.routes.is_empty() => {
                                    println!("\x1b[1mConfigured Routes:\x1b[0m");
                                    for item in &settings.routes {
                                        let marker = if item.id == route.id {
                                            "\x1b[32m*\x1b[0m"
                                        } else {
                                            " "
                                        };
                                        println!(
                                            " {marker} \x1b[36m{}\x1b[0m  model=\x1b[1m{}\x1b[0m  ({})",
                                            item.id, item.model, item.base_url
                                        );
                                    }
                                }
                                Ok(_) => println!("当前尚未配置任何模型路由。"),
                                Err(err) => eprintln!("\x1b[31m读取路由配置失败: {err}\x1b[0m"),
                            }
                        } else if !host_route_id_valid(arg_str) {
                            eprintln!("无效的路由 ID `{arg_str}`。");
                        } else {
                            match ensure_route_ready(&engine, Some(arg_str), None).await {
                                Ok(selected) => {
                                    let changed = selected.id != route.id;
                                    match ensure_project_ready(
                                        &engine,
                                        PathBuf::from(&project.root_path),
                                        &selected.id,
                                    )
                                    .await
                                    {
                                        Ok(_) => {
                                            route = selected;
                                            if changed {
                                                session_state = None;
                                                println!(
                                                    "\x1b[32m✓ 已切换到路由 `{}` (model=`{}`, base=`{}`)；已重置当前会话上下文。\x1b[0m",
                                                    route.id, route.model, route.base_url
                                                );
                                            } else {
                                                println!(
                                                    "\x1b[32m✓ 当前已在路由 `{}` (model=`{}`, base=`{}`)\x1b[0m",
                                                    route.id, route.model, route.base_url
                                                );
                                            }
                                        }
                                        Err(err) => err.print(false),
                                    }
                                }
                                Err(err) => err.print(false),
                            }
                        }
                    }
                    "model" => {
                        if arg_str.is_empty() {
                            println!("当前路由 `{}` 使用模型: \x1b[1m{}\x1b[0m", route.id, route.model);
                        } else {
                            match ensure_route_ready(&engine, Some(&route.id), Some(arg_str)).await
                            {
                                Ok(updated) => {
                                    route = updated;
                                    let _ = ensure_project_ready(
                                        &engine,
                                        PathBuf::from(&project.root_path),
                                        &route.id,
                                    )
                                    .await;
                                    if session_state.is_some() {
                                        println!(
                                            "\x1b[32m✓ 已切换模型为 `{}` (保留当前会话上下文，下一轮直接生效)\x1b[0m",
                                            route.model
                                        );
                                    } else {
                                        println!(
                                            "\x1b[32m✓ 已切换模型为 `{}`\x1b[0m",
                                            route.model
                                        );
                                    }
                                }
                                Err(err) => err.print(false),
                            }
                        }
                    }
                    "status" => {
                        println!("Workspace   : {}", project.root_path);
                        println!("Project ID  : {}", project.id.0);
                        println!("Route/Model : {} / {}", route.id, route.model);
                        println!("Auto-Approve: {}", if auto_approve { "ON" } else { "OFF" });
                        if let Some(active) = &session_state {
                            let mut total_tokens = 0u64;
                            for tid in &active.task_history {
                                if let Ok(t) = engine.store.task(tid) {
                                    total_tokens +=
                                        t.usage["total_tokens"].as_u64().unwrap_or(0);
                                }
                            }
                            println!(
                                "Session     : {} ({} turns, {} total tokens)",
                                active.session_id.0, active.turn_count, total_tokens
                            );
                        } else {
                            println!("Session     : (none yet)");
                        }
                    }
                    "diff" => {
                        let root = Path::new(&project.root_path);
                        if let Some(status) = run_quick_git(root, &["status", "--short"]) {
                            if status.is_empty() {
                                println!("Git working tree is clean.");
                            } else {
                                println!("\x1b[1mGit Status:\x1b[0m\n{status}");
                                if let Some(stat) = run_quick_git(root, &["diff", "--stat"])
                                    && !stat.is_empty()
                                {
                                    println!("\n\x1b[1mGit Diff Stat:\x1b[0m\n{stat}");
                                }
                            }
                        }
                        if let Some(active) = &session_state
                            && let Some(last_tid) = active.task_history.last()
                            && let Ok(changes) = engine.changes(last_tid)
                            && !changes.is_empty()
                        {
                            println!("\n\x1b[1mLatest Turn Tracked File Edits:\x1b[0m");
                            for ch in changes {
                                println!(
                                    "  - {} ({:?}, restorable={})",
                                    ch.path, ch.kind, ch.restorable
                                );
                            }
                        }
                    }
                    "undo" => {
                        let target_task = session_state.as_ref().and_then(|active| {
                            active.task_history.iter().rev().find(|tid| {
                                engine
                                    .changes(tid)
                                    .map(|c| c.iter().any(|item| item.restorable))
                                    .unwrap_or(false)
                            })
                        }).cloned();
                        match target_task {
                            Some(tid) => match engine.restore(&tid).await {
                                Ok(receipt) => {
                                    println!(
                                        "\x1b[32m✓ 已回滚任务 {} 的 {} 处文件修改 (status={:?}):\x1b[0m",
                                        tid, receipt.restored, receipt.status
                                    );
                                    for item in &receipt.outcomes {
                                        println!("  - {} ({:?})", item.path, item.status);
                                    }
                                }
                                Err(err) => eprintln!("\x1b[31m回滚失败: {err}\x1b[0m"),
                            },
                            None => println!("当前会话没有可回滚的文件修改。"),
                        }
                    }
                    other => {
                        eprintln!("未知命令 `/{other}`；输入 `/help` 查看支持的命令。");
                    }
                }
                continue;
            }

            let task = execute_turn_and_stream(
                &engine,
                &project,
                &route,
                &mut session_state,
                input,
                &mut auto_approve,
                json_output,
            )
            .await?;
            if task.status != "completed"
                && let Some(err) = &task.error
            {
                eprintln!("\x1b[31m[{}] {}\x1b[0m", task.status, err);
            }
        }
        Ok(())
    }
    .await;

    let _ = engine.shutdown(Duration::from_secs(10)).await;
    result
}

pub(super) async fn run_ask(
    instance_lock: &mut Option<std::fs::File>,
    top_data_dir: Option<PathBuf>,
    top_workspace: Option<PathBuf>,
    json_output: bool,
    args: AskArgs,
) -> Result<(), Failure> {
    let data_dir = resolve_user_data_dir(args.data_dir.or(top_data_dir));
    let target_dir = args
        .dir
        .or(top_workspace)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let prompt = args.prompt.join(" ");

    let engine = open_local_engine(instance_lock, &data_dir)?;
    let outcome = async {
        let route =
            ensure_route_ready(&engine, args.route.as_deref(), args.model.as_deref()).await?;
        let (project, _) = ensure_project_ready(&engine, target_dir, &route.id).await?;
        let mut auto_approve = args.auto_approve;
        let mut session_state = resolve_initial_session(
            &engine,
            &project,
            args.continue_session,
            args.resume.as_deref(),
            json_output,
        )?;

        let task = execute_turn_and_stream(
            &engine,
            &project,
            &route,
            &mut session_state,
            prompt.trim(),
            &mut auto_approve,
            json_output,
        )
        .await?;

        if json_output {
            println!(
                "{}",
                json!({
                    "ok": task.status == "completed",
                    "status": task.status,
                    "task_id": task.id,
                    "run_id": task.run_id,
                    "session_id": session_state.as_ref().map(|s| &s.session_id.0),
                    "output": task.output,
                    "error": task.error,
                    "usage": task.usage,
                })
            );
        }

        if task.status == "completed" {
            Ok(())
        } else {
            Err(Failure::custom(
                "task_failed",
                task.error
                    .unwrap_or_else(|| format!("任务未完成，结束状态: {}", task.status)),
            ))
        }
    }
    .await;

    let _ = engine.shutdown(Duration::from_secs(10)).await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exploration_progress_shows_only_single_line_file_labels() {
        let scope = exploration_scope(&json!({
            "question": "private question",
            "paths": ["src/lib.rs", "example\nfile.rs"]
        }));
        assert_eq!(scope, r#"Exploring "src/lib.rs", "example\nfile.rs""#);
        assert_eq!(exploration_scope(&Value::Null), "Exploring files");
    }

    #[test]
    fn read_repl_input_joins_backslash_multiline_continuations() {
        let mut cursor = io::Cursor::new(
            b"Explain this:\\\r\nfn main() {\\\n    println!(\"hi\");\n}\n" as &[u8],
        );
        let mut continuations = 0usize;
        let joined = read_repl_input(&mut cursor, || {
            continuations += 1;
        })
        .unwrap();
        assert_eq!(continuations, 2);
        assert_eq!(joined, "Explain this:\nfn main() {\n    println!(\"hi\");");

        let closing = read_repl_input(&mut cursor, || {}).unwrap();
        assert_eq!(closing, "}");

        assert!(read_repl_input(&mut cursor, || {}).is_none());
    }
}
