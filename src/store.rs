use crate::{approval, domain::*, repository, secrets, workspace_changes::*};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{path::Path, sync::Mutex};

pub const SCHEMA_VERSION: u32 = 18;

fn is_readonly_file_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_files"
            | "search_files"
            | "glob_files"
            | "git_status"
            | "todo_write"
            | "todo_read"
    )
}

fn is_file_mutation_tool(name: &str) -> bool {
    matches!(name, "write_file" | "edit_file")
}

fn is_authorized_mcp_tool(task: &Task, name: &str) -> bool {
    task.spec.tools && task.spec.allow_commands && crate::mcp::is_valid_qualified_name(name)
}

fn is_known_task_tool(task: &Task, name: &str) -> bool {
    crate::workspace::definitions(task)
        .iter()
        .any(|tool| tool["function"]["name"] == name)
        || is_authorized_mcp_tool(task, name)
}

pub(crate) fn evaluate_tool_policy(task: &Task, name: &str) -> approval::PolicyDecision {
    if crate::mcp::is_valid_qualified_name(name) {
        if task.spec.tools && task.spec.allow_commands {
            approval::PolicyDecision::RequireApproval
        } else {
            approval::PolicyDecision::Deny {
                reason: "当前任务未授权运行 MCP 工具",
            }
        }
    } else {
        approval::evaluate(task, name)
    }
}

#[derive(Debug)]
pub struct IdempotencyConflict;
impl std::fmt::Display for IdempotencyConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("同一个 Idempotency-Key 不能用于不同请求或对象")
    }
}
impl std::error::Error for IdempotencyConflict {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeError {
    Sealed,
}
impl std::fmt::Display for ResumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("任务已有恢复记录，不能继续原任务")
    }
}
impl std::error::Error for ResumeError {}

fn ensure_resume_unsealed(db: &Connection, task_id: &str) -> Result<()> {
    let sealed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM checkpoints WHERE task_id=?1)",
        [task_id],
        |r| r.get(0),
    )?;
    if sealed {
        return Err(crate::checkpoint::CheckpointError::Conflict.into());
    }
    let restored: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workspace_restores WHERE task_id=?1 AND status='complete')",
        [task_id],
        |r| r.get(0),
    )?;
    if restored {
        return Err(ResumeError::Sealed.into());
    }
    Ok(())
}

pub struct Store {
    pub(crate) db: Mutex<Connection>,
    pub(crate) authority: std::sync::Arc<crate::execution_authority::ExecutionAuthority>,
}

fn pending_resume_inputs_tx(db: &Connection, task_id: &str) -> Result<Vec<(i64, String)>> {
    let mut statement = db.prepare(
        "SELECT e.seq,e.data FROM events e \
         WHERE e.task_id=?1 AND e.kind='resume' \
           AND json_extract(e.data,'$.delivery_version')=1 \
           AND json_extract(e.data,'$.pending')=1 \
           AND NOT EXISTS ( \
             SELECT 1 FROM events c \
             WHERE c.task_id=e.task_id AND c.kind='unknown:resume_consumed' \
               AND json_extract(c.data,'$.delivery_version')=1 \
               AND json_extract(c.data,'$.resume_seq')=e.seq \
           ) \
         ORDER BY e.seq",
    )?;
    let rows = statement.query_map([task_id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (seq, raw) = row?;
        let value: Value = serde_json::from_str(&raw)?;
        let message = value["message"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("继续输入记录损坏"))?;
        Ok((seq, message.to_owned()))
    })
    .collect()
}

pub struct TurnBundle<'a> {
    pub project: &'a Project,
    pub session: &'a Session,
    pub agents: &'a [Agent],
    pub turn: &'a Turn,
    pub tasks: &'a [TurnTask],
    pub legacy_tasks: &'a [Task],
}

impl Store {
    #[doc(hidden)]
    pub fn instance_lock_handle(&self) -> Result<std::fs::File> {
        self.authority.instance_lock_handle()
    }

    pub fn open_shared(&self) -> Result<Self> {
        let db = self.authority.connection()?;
        let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        anyhow::ensure!(
            version == SCHEMA_VERSION,
            "shared domain schema version changed"
        );
        Ok(Self {
            db: Mutex::new(db),
            authority: self.authority.clone(),
        })
    }

    #[allow(dead_code)]
    pub(crate) fn execution_fence(&self, _tx: &rusqlite::Transaction<'_>) -> Result<()> {
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn execution_fence_for(
        &self,
        _tx: &rusqlite::Transaction<'_>,
        _op: &crate::execution_authority::ExecutionOperation,
    ) -> Result<()> {
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn execution_settlement_fence_for(
        &self,
        _tx: &rusqlite::Transaction<'_>,
        _op: &crate::execution_authority::ExecutionOperation,
    ) -> Result<()> {
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn execution_settlement_fence(&self, _tx: &rusqlite::Transaction<'_>) -> Result<()> {
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn execution_unknown_fence(&self, _tx: &rusqlite::Transaction<'_>) -> Result<()> {
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn execution_generation(
        &self,
        op: &crate::execution_authority::ExecutionOperation,
    ) -> Result<Box<dyn crate::process_lifetime::DomainLease>> {
        op.generation_for(&self.authority)
    }

    pub(crate) fn commit_execution_tx(&self, tx: rusqlite::Transaction<'_>) -> Result<()> {
        tx.commit()?;
        Ok(())
    }

    pub fn execution_admission_status(&self) -> Result<(bool, usize, usize)> {
        Ok(self.authority.status())
    }

    pub(crate) fn execution_operation(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::execution_authority::ExecutionOperation> {
        self.authority.operation(cancel)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let authority = crate::execution_authority::ExecutionAuthority::acquire(path)?;
        let mut initialized = authority
            .initialization
            .lock()
            .map_err(|_| anyhow::anyhow!("domain initialization poisoned"))?;
        let mut db = authority.connection().context("无法打开任务数据库")?;
        let current_version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        anyhow::ensure!(
            current_version <= SCHEMA_VERSION,
            "任务数据库版本 {} 高于当前程序支持的版本 {}",
            current_version,
            SCHEMA_VERSION
        );
        if *initialized && current_version == SCHEMA_VERSION {
            drop(initialized);
            return Ok(Self {
                db: Mutex::new(db),
                authority,
            });
        }
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS config (id TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS secrets (id TEXT PRIMARY KEY, value BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS schema_migrations (id TEXT PRIMARY KEY, applied_at INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS tasks_run ON tasks(run_id);
             CREATE INDEX IF NOT EXISTS events_task ON events(task_id, seq);",
        )?;
        let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 2 {
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS idempotency (key TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), created_at INTEGER NOT NULL);
                 CREATE INDEX IF NOT EXISTS idempotency_run ON idempotency(run_id);",
            )?;
        }
        if version < 3 {
            ensure_column_if_missing(
                &db,
                "idempotency",
                "request_hash",
                "TEXT NOT NULL DEFAULT ''",
            )?;
            db.pragma_update(None, "user_version", 3)?;
        }
        if version < 4 {
            ensure_column_if_missing(&db, "runs", "kind", "TEXT NOT NULL DEFAULT 'team'")?;
            db.pragma_update(None, "user_version", 4)?;
        }
        if version < 5 {
            let run_ids: Vec<String> = {
                let mut stmt = db.prepare("SELECT id FROM runs WHERE kind='team'")?;
                stmt.query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for run_id in run_ids {
                let values: Vec<String> = {
                    let mut stmt = db.prepare("SELECT value FROM tasks WHERE run_id=?1")?;
                    stmt.query_map([&run_id], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<_>>()?
                };
                let is_chat = values.iter().any(|value| {
                    serde_json::from_str::<Value>(value)
                        .ok()
                        .and_then(|task| {
                            task.pointer("/spec/name")
                                .or_else(|| task.get("name"))
                                .and_then(Value::as_str)
                                .map(|name| name == "chat-session")
                        })
                        .unwrap_or(false)
                });
                if is_chat {
                    db.execute("UPDATE runs SET kind='chat' WHERE id=?1", [&run_id])?;
                }
            }
            db.execute(
                "INSERT OR IGNORE INTO schema_migrations(id,applied_at) VALUES ('session-kind-explicit',?1)",
                [now()],
            )?;
            db.pragma_update(None, "user_version", 5)?;
        }
        {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if !repository::migration_applied(&tx, repository::MIGRATION_ID)? {
                repository::apply_schema_6(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::APPROVAL_MIGRATION_ID)? {
                repository::apply_schema_7(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::WORKSPACE_CHANGE_MIGRATION_ID)? {
                repository::apply_schema_8(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::CHECKPOINT_MIGRATION_ID)? {
                repository::apply_schema_9(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::NETWORK_MIGRATION_ID)? {
                repository::apply_schema_10(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::PROJECT_CONFIG_MIGRATION_ID)? {
                repository::apply_schema_11(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::PLUGIN_MANAGEMENT_MIGRATION_ID)? {
                repository::apply_schema_12(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::SHELL_MANAGEMENT_MIGRATION_ID)? {
                repository::apply_schema_13(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::WORKTREE_MIGRATION_ID)? {
                repository::apply_schema_14(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::BUDGET_MIGRATION_ID)? {
                repository::apply_schema_15(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::HOST_CONFIG_MIGRATION_ID)? {
                repository::apply_schema_16(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::RUNTIME_MANAGEMENT_MIGRATION_ID)? {
                repository::apply_schema_17(&tx)?;
            }
            if !repository::migration_applied(&tx, repository::EXECUTION_OWNER_MIGRATION_ID)? {
                repository::apply_schema_18(&tx, &authority.identity_key()?)?;
            } else {
                let v: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
                if v < SCHEMA_VERSION {
                    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
                }
            }
            tx.commit()?;
        }
        *initialized = true;
        drop(initialized);
        Ok(Self {
            db: Mutex::new(db),
            authority,
        })
    }

    pub fn schema_version(&self) -> Result<u32> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn settings(&self) -> Result<Option<Settings>> {
        let db = self.db.lock().unwrap();
        let value: Option<String> = db
            .query_row("SELECT value FROM config WHERE id='settings'", [], |row| {
                row.get(0)
            })
            .optional()?;
        value
            .map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    pub fn save_settings(&self, value: &Settings) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        crate::host_configuration::save_configuration_tx(
            &tx,
            Some(value),
            &Default::default(),
            false,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn put_secret(&self, id: &str, secret: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        crate::host_configuration::save_configuration_tx(
            &tx,
            None,
            &std::collections::HashMap::from([(id.to_owned(), secret.to_owned())]),
            false,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn secret(&self, id: &str) -> Result<Option<String>> {
        let bytes: Option<Vec<u8>> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM secrets WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        bytes
            .map(|v| Ok(String::from_utf8(secrets::open(&v)?)?))
            .transpose()
    }

    pub fn create_run(&self, run: &Run) -> Result<()> {
        anyhow::ensure!(
            run.tasks.iter().all(|task| task.run_id == run.id),
            "旧任务必须属于本次运行"
        );
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        insert_legacy_run(&tx, run)?;
        for task in &run.tasks {
            insert_legacy_task(&tx, task)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn create_run_with_idempotency(
        &self,
        run: &Run,
        key: &str,
        request_hash: &str,
    ) -> Result<()> {
        secrets::validate_idempotency_key(key)?;
        secrets::validate_persisted_id("request_hash", request_hash)?;
        anyhow::ensure!(
            run.tasks.iter().all(|task| task.run_id == run.id),
            "旧任务必须属于本次运行"
        );
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if classify_both(&repository::Repository::new(&tx), key, request_hash)?.is_some() {
            return Err(IdempotencyConflict.into());
        }
        insert_legacy_run(&tx, run)?;
        for task in &run.tasks {
            insert_legacy_task(&tx, task)?;
        }
        tx.execute(
            "INSERT INTO idempotency(key,run_id,created_at,request_hash) VALUES (?1,?2,?3,?4)",
            params![key, run.id, now(), request_hash],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn idempotent_run(&self, key: &str, request_hash: &str) -> Result<Option<Run>> {
        match self.idempotency_replay(key, request_hash)? {
            Some(IdempotencyReplay::Turn(turn)) => {
                let session = self.session(&turn.session_id)?;
                Ok(Some(self.run(&session.legacy_run_id)?))
            }
            Some(IdempotencyReplay::LegacyRun(run)) => Ok(Some(run)),
            None => Ok(None),
        }
    }

    pub fn save_task(&self, task: &Task) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        save_task_tx(&tx, task, false)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn save_running_snapshot(&self, task: &Task) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let protected = protect_running_projection(&tx, task)?;
        save_task_tx(&tx, &protected, false)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn save_unknown_terminal(&self, task: &Task) -> Result<()> {
        anyhow::ensure!(
            matches!(task.status.as_str(), "failed" | "cancelled"),
            "unknown outcome must be failed or cancelled"
        );
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut durable = legacy_task_row(&tx, &task.id)?;
        anyhow::ensure!(
            matches!(durable.status.as_str(), "queued" | "running" | "cancelled"),
            "task is already terminal"
        );
        if durable.status != "cancelled" {
            durable.status = task.status.clone();
        }
        durable.error = Some("执行结果未知；请检查后再继续".into());
        durable.updated_at = now();
        save_task_tx(&tx, &durable, false)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn recover_legacy_read_result(
        &self,
        caller: &Task,
        call_id: &str,
        name: &str,
    ) -> Result<Option<Task>> {
        use crate::tool_results::ToolResultError;
        if !is_readonly_file_tool(name) {
            return Ok(None);
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut durable = legacy_task_row(&tx, &caller.id)?;
        if safe_task_value(caller)?["messages"] != safe_task_value(&durable)?["messages"]
            || caller.run_id != durable.run_id
            || !crate::workspace::definitions(&durable)
                .iter()
                .any(|tool| tool["function"]["name"] == name)
        {
            return Err(ToolResultError::Conflict.into());
        }
        approval::validate_call_history(&durable.messages).map_err(|_| ToolResultError::Corrupt)?;
        let (turn, session) = approval_scope(&tx, &durable.id)?;
        let event_session = repository::Repository::new(&tx)
            .session_by_legacy_run_of_task(&durable.id)?
            .map(|session| session.id.0);
        let inherited =
            inherited_message_count(&tx, &durable, turn.as_deref(), session.as_deref())?;
        let existing: Vec<(String, Option<String>, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT data,turn_id,session_id FROM events WHERE task_id=?1 AND kind='tool_result' ORDER BY seq",
            )?;
            let rows = stmt.query_map([&durable.id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if existing.iter().any(|(_, event_turn, event_session_id)| {
            event_turn != &turn || event_session_id != &event_session
        }) {
            return Err(ToolResultError::Corrupt.into());
        }
        let messages = &durable.messages[inherited..];
        let answers: Vec<_> = messages.iter().filter(|m| m["role"] == "tool").collect();
        if existing.len() == answers.len() {
            return Ok(None);
        }
        if existing.len() != answers.len() + 1 {
            return Err(ToolResultError::Conflict.into());
        }
        let events: Vec<Value> = existing
            .iter()
            .map(|(raw, _, _)| serde_json::from_str(raw).map_err(|_| ToolResultError::Corrupt))
            .collect::<std::result::Result<_, _>>()?;
        validate_owned_result_events(messages, &events[..answers.len()])?;
        let event = events.last().ok_or(ToolResultError::Corrupt)?;
        if event.get("origin").is_some()
            || event.get("origin_version").is_some()
            || event["name"] != name
            || event.get("result").is_none()
            || event
                .get("tool_call_id")
                .is_some_and(|event_id| event_id != call_id)
        {
            return Err(ToolResultError::Conflict.into());
        }
        let content = stable_safe_json(&event["result"])?;
        durable
            .messages
            .push(json!({"role":"tool","tool_call_id":call_id,"content":content}));
        durable.updated_at = now();
        save_task_tx(&tx, &durable, false)?;
        tx.commit()?;
        Ok(Some(durable))
    }

    pub(crate) fn commit_tool_result(
        &self,
        caller: &Task,
        call_id: &str,
        name: &str,
        content: &str,
        origin: crate::tool_results::Origin,
    ) -> Result<(Task, crate::tool_results::CommitDisposition)> {
        use crate::tool_results::{CommitDisposition, Origin, ToolResultError};
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut durable = legacy_task_row(&tx, &caller.id)?;
        let safe_caller = safe_task_value(caller)?;
        let safe_durable = safe_task_value(&durable)?;
        for field in ["id", "run_id", "spec", "route", "workspace", "created_at"] {
            if safe_caller[field] != safe_durable[field] {
                return Err(ToolResultError::Conflict.into());
            }
        }
        let policy = evaluate_tool_policy(&durable, name);
        if !is_known_task_tool(&durable, name)
            && !(origin == Origin::HostDenied
                && matches!(policy, approval::PolicyDecision::Deny { .. }))
        {
            return Err(ToolResultError::Conflict.into());
        }
        approval::validate_call_history(&durable.messages).map_err(|_| ToolResultError::Corrupt)?;
        let result = serde_json::from_str::<Value>(content)
            .unwrap_or_else(|_| Value::String(content.to_owned()));
        let result = secrets::redact_persisted(&result);
        let content = if matches!(
            origin,
            Origin::ApprovalDenied | Origin::ApprovalCancelled | Origin::InterruptedWithoutClaim
        ) {
            result.as_str().ok_or(ToolResultError::Conflict)?.to_owned()
        } else {
            stable_safe_json(&result)?
        };
        let (turn_id, session_id) = approval_scope(&tx, &durable.id)?;
        let inherited =
            inherited_message_count(&tx, &durable, turn_id.as_deref(), session_id.as_deref())?;
        let existing: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT data FROM events WHERE task_id=?1 AND kind='tool_result' ORDER BY seq",
            )?;
            let rows = stmt.query_map([&durable.id], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let events: Vec<Value> = existing
            .iter()
            .map(|raw| serde_json::from_str(raw).map_err(|_| ToolResultError::Corrupt))
            .collect::<std::result::Result<Vec<Value>, _>>()?;
        let matching: Vec<_> = events
            .iter()
            .filter(|event| event["tool_call_id"] == call_id)
            .collect();
        let answers: Vec<_> = durable
            .messages
            .iter()
            .filter(|message| message["role"] == "tool" && message["tool_call_id"] == call_id)
            .collect();
        let replay = !answers.is_empty() || !matching.is_empty();
        let caller_messages = safe_caller["messages"]
            .as_array()
            .ok_or(ToolResultError::Conflict)?;
        let durable_messages = safe_durable["messages"]
            .as_array()
            .ok_or(ToolResultError::Corrupt)?;
        if (!replay && caller_messages != durable_messages)
            || (replay && !durable_messages.starts_with(caller_messages))
        {
            return Err(ToolResultError::Conflict.into());
        }
        let group_index = durable
            .messages
            .iter()
            .position(|m| {
                m["tool_calls"]
                    .as_array()
                    .is_some_and(|calls| calls.iter().any(|call| call["id"] == call_id))
            })
            .ok_or(ToolResultError::Corrupt)?;
        if group_index < inherited {
            return Err(ToolResultError::Conflict.into());
        }
        validate_prior_groups(&durable.messages[inherited..group_index])?;
        let group = durable.messages[group_index]["tool_calls"]
            .as_array()
            .ok_or(ToolResultError::Corrupt)?;
        let call_index = group
            .iter()
            .position(|call| call["id"] == call_id)
            .ok_or(ToolResultError::Corrupt)?;
        let preceding = durable
            .messages
            .get(group_index + 1..group_index + 1 + call_index)
            .ok_or(ToolResultError::Corrupt)?;
        let call = &group[call_index];
        if durable.messages[group_index]["role"] != "assistant"
            || call["id"] != call_id
            || call["function"]["name"] != name
            || preceding.iter().zip(group).any(|(message, call)| {
                message["role"] != "tool" || message["tool_call_id"] != call["id"]
            })
        {
            return Err(ToolResultError::Conflict.into());
        }
        let record =
            repository::Repository::new(&tx).approval_by_tool_call(&durable.id, call_id)?;
        let has_change: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_changes WHERE task_id=?1 AND tool_call_id=?2)",
            params![durable.id, call_id],
            |row| row.get(0),
        )?;
        if has_change {
            return Err(ToolResultError::Conflict.into());
        }
        let valid = match origin {
            Origin::ReadCompleted => record.is_none() && is_readonly_file_tool(name),
            Origin::AgentDenied => {
                record.is_none() && result == json!({"error":"blocked_by_agent_policy"})
            }
            Origin::HostDenied => {
                record.is_none()
                    && matches!(policy, approval::PolicyDecision::Deny { reason } if result == Value::String(reason.into()))
            }
            Origin::InterruptedWithoutClaim => {
                record.is_none()
                    && result
                        == Value::String(
                            "Execution was interrupted. Inspect files before repeating a write or command.".into(),
                        )
            }
            Origin::ApprovalDenied | Origin::ApprovalCancelled => {
                record.as_ref().is_some_and(|record| {
                    let status = if origin == Origin::ApprovalDenied {
                        approval::ApprovalStatus::Denied
                    } else {
                        approval::ApprovalStatus::Cancelled
                    };
                    record.tool_name == name
                        && record.turn_id == turn_id
                        && record.session_id == session_id
                        && (record.status == status
                            || (status == approval::ApprovalStatus::Cancelled
                                && record.execution_state == approval::ExecutionState::Cancelled))
                        && !matches!(
                            record.execution_state,
                            approval::ExecutionState::Claimed
                                | approval::ExecutionState::Finished
                                | approval::ExecutionState::Unknown
                        )
                        && result == Value::String(approval::tool_error(status))
                })
            }
        };
        if !valid {
            return Err(ToolResultError::Conflict.into());
        }
        if replay {
            let answer = durable.messages.get(group_index + 1 + call_index);
            if answers.len() != 1
                || matching.len() != 1
                || answer != answers.first().copied()
                || matching[0]["name"] != name
                || matching[0]["result"] != result
                || matching[0]["origin"] != origin.as_str()
                || answers[0]["content"].as_str().map(tool_result_value) != Some(result)
            {
                return Err(ToolResultError::Conflict.into());
            }
            tx.commit()?;
            return Ok((durable, CommitDisposition::Replayed));
        }
        if group_index + 1 + call_index != durable.messages.len() {
            return Err(ToolResultError::Conflict.into());
        }
        if !caller.output.is_empty() && caller.output.len() >= durable.output.len() {
            durable.output = caller.output.clone();
        }
        durable.usage = merge_usage(&durable.usage, &caller.usage)?;
        durable
            .messages
            .push(json!({"role":"tool","tool_call_id":call_id,"content":content}));
        durable.updated_at = now();
        save_task_tx(&tx, &durable, false)?;
        task_event_tx(
            &tx,
            &durable.id,
            "tool_result",
            json!({
                "name": name,
                "tool_call_id": call_id,
                "result": result,
                "origin_version": 1,
                "origin": origin.as_str()
            }),
        )?;
        tx.commit()?;
        Ok((durable, CommitDisposition::Inserted))
    }

    pub(crate) fn save_new_assistant_group(&self, task: &Task) -> Result<()> {
        let calls = task
            .messages
            .last()
            .and_then(|message| message.get("tool_calls"))
            .and_then(Value::as_array)
            .context("assistant tool group missing")?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for call in calls {
            let id = call["id"].as_str().context("tool call id missing")?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM approvals WHERE task_id=?1 AND tool_call_id=?2)",
                params![&task.id, id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(!exists, "new tool call conflicts with existing approval");
        }
        let protected = protect_running_projection(&tx, task)?;
        save_task_tx(&tx, &protected, false)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn check_resume_unsealed(&self, task_id: &str) -> Result<()> {
        ensure_resume_unsealed(&self.db.lock().unwrap(), task_id)
    }

    pub fn resume_task(&self, task: &Task, input: &str) -> Result<()> {
        self.resume_task_inner(task, input, false)
    }

    pub(crate) fn resume_task_with_pending_input(&self, task: &Task, input: &str) -> Result<()> {
        self.resume_task_inner(task, input, true)
    }

    fn resume_task_inner(&self, task: &Task, input: &str, pending: bool) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_resume_unsealed(&tx, &task.id)?;
        crate::project_config::validate_task_tx(&tx, task, true, true)?;
        ensure_resume_targets_latest_chat_turn(&tx, &task.id)?;
        crate::worktree_management::ensure_task_unreserved_tx(&tx, &task.id)?;
        save_task_tx(&tx, task, true)?;
        task_event_tx(&tx, &task.id, "delta", json!({"text": "\n\n—— 继续 ——\n"}))?;
        task_event_tx(
            &tx,
            &task.id,
            "resume",
            json!({"message": input, "delivery_version": 1, "pending": pending}),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn consume_resume_inputs(&self, caller: &Task) -> Result<Task> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let pending = pending_resume_inputs_tx(&tx, &caller.id)?;
        if pending.is_empty() {
            return Ok(caller.clone());
        }
        let durable = legacy_task_row(&tx, &caller.id)?;
        anyhow::ensure!(
            safe_task_value(caller)?["messages"] == safe_task_value(&durable)?["messages"],
            "继续输入的消息快照已变化"
        );
        approval::validate_call_history(&durable.messages)?;
        anyhow::ensure!(
            crate::tool_call_application::unanswered_tool_calls(&durable.messages).is_empty(),
            "工具调用组未闭合，不能追加继续输入"
        );
        let mut task = caller.clone();
        for (_, message) in &pending {
            task.messages
                .push(json!({"role":"user", "content":message}));
        }
        task.updated_at = now();
        let protected = protect_running_projection(&tx, &task)?;
        save_task_tx(&tx, &protected, false)?;
        for (seq, _) in pending {
            task_event_tx(
                &tx,
                &task.id,
                "unknown:resume_consumed",
                json!({"delivery_version":1,"resume_seq":seq}),
            )?;
        }
        tx.commit()?;
        Ok(protected)
    }

    pub fn task(&self, id: &str) -> Result<Task> {
        let db = self.db.lock().unwrap();
        let (run_id, value): (String, String) = db
            .query_row("SELECT run_id,value FROM tasks WHERE id=?1", [id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .context("任务不存在")?;
        let task: Task = serde_json::from_str(&value)?;
        anyhow::ensure!(
            task.id == id && task.run_id == run_id,
            "旧任务 JSON 身份与关系列不一致"
        );
        Ok(serde_json::from_value(safe_task_value(&task)?)?)
    }

    pub fn run(&self, id: &str) -> Result<Run> {
        repository::Repository::new(&self.db.lock().unwrap()).legacy_run(id)
    }

    pub fn runs(&self) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT id,title,kind,created_at FROM runs ORDER BY created_at DESC,rowid DESC LIMIT 100",
        )?;
        Ok(stmt
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "title": r.get::<_, String>(1)?,
                    "kind": r.get::<_, String>(2)?,
                    "created_at": r.get::<_, u64>(3)?
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn run_context(&self, run_id: &str) -> Result<RunContext> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",
            [run_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(RunContextError::NotFound.into());
        }
        let session_ids: Vec<String> = tx
            .prepare("SELECT id FROM sessions WHERE legacy_run_id=?1")?
            .query_map([run_id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let session_id = match session_ids.as_slice() {
            [] => return Err(RunContextError::Unmapped.into()),
            [only] => SessionId(only.clone()),
            _ => return Err(RunContextError::CorruptState.into()),
        };
        let repo = repository::Repository::new(&tx);
        let session = repo.session(&session_id).map_err(run_context_error)?;
        let project = repo
            .project(&session.project_id)
            .map_err(run_context_error)?;
        let latest_turn = repo
            .latest_committed_turn(&session.id)
            .map_err(run_context_error)?
            .ok_or(RunContextError::CorruptState)?;
        let tasks = repo
            .turn_tasks(&latest_turn.id)
            .map_err(run_context_error)?;
        if session.id != session_id
            || session.legacy_run_id != run_id
            || project.id != session.project_id
            || latest_turn.session_id != session.id
            || latest_turn.project_id != project.id
            || tasks.is_empty()
        {
            return Err(RunContextError::CorruptState.into());
        }
        for task in &tasks {
            let agent = repo.agent(&task.agent_id).map_err(run_context_error)?;
            let legacy = legacy_task_row(&tx, &task.legacy_task_id).map_err(run_context_error)?;
            if task.turn_id != latest_turn.id
                || task.session_id != session.id
                || agent.session_id != session.id
                || legacy.run_id != run_id
                || legacy.status != task.status.as_str()
                || task
                    .depends_on
                    .iter()
                    .any(|id| !tasks.iter().any(|candidate| &candidate.id == id))
            {
                return Err(RunContextError::CorruptState.into());
            }
        }
        tx.commit()?;
        Ok(RunContext {
            run_id: run_id.to_owned(),
            project,
            session,
            latest_turn,
            tasks,
        })
    }

    pub fn event(&self, task_id: &str, kind: &str, data: Value) -> Result<()> {
        anyhow::ensure!(
            kind != "unknown:resume_consumed"
                && !(kind == "resume" && data.get("delivery_version").is_some()),
            "继续输入的消费证据只能由事务生成"
        );
        anyhow::ensure!(
            kind != "tool_result"
                || (data.get("origin").is_none() && data.get("origin_version").is_none()),
            "tool_result origin is reserved for the result transaction"
        );
        anyhow::ensure!(
            kind != "approval.requested"
                || (data.get("evidence_version").is_none()
                    && data.get("safe_call_digest").is_none()),
            "approval evidence is reserved for the approval transaction"
        );
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if kind == "status" || kind == "task.status" {
            let value: String =
                tx.query_row("SELECT value FROM tasks WHERE id=?1", [task_id], |row| {
                    row.get(0)
                })?;
            let task: Task = serde_json::from_str(&value)?;
            anyhow::ensure!(
                data["status"].as_str() == Some(task.status.as_str()),
                "状态事件必须与已存任务状态一致"
            );
        }
        if kind == "turn.status" {
            let status: String = tx.query_row(
                "SELECT t.status FROM turns t JOIN turn_tasks tt ON tt.turn_id=t.id WHERE tt.legacy_task_id=?1",
                [task_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                data["status"].as_str() == Some(status.as_str()),
                "状态事件必须与已存回合状态一致"
            );
        }
        let repo = repository::Repository::new(&tx);
        let session = repo.session_by_legacy_run_of_task(task_id)?;
        let turn_id: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM turn_tasks WHERE legacy_task_id=?1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        let event = stamped_event(
            0,
            session.as_ref().map(|s| s.id.0.as_str()).unwrap_or(""),
            turn_id.as_deref(),
            task_id,
            kind,
            secrets::redact_persisted(&data),
            now(),
        );
        let event = Event {
            session_id: event.session_id.filter(|id| !id.is_empty()),
            ..event
        };
        repository::Repository::append_event(&tx, &event)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn completion_delta(&self, task_id: &str, safe_text: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let event = task_event_envelope(&tx, task_id, "delta", json!({"text":safe_text}))?;
        repository::Repository::append_completion_delta(&tx, &event)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn reasoning_delta(&self, task_id: &str, safe_text: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let event =
            task_event_envelope(&tx, task_id, "reasoning_delta", json!({"text":safe_text}))?;
        repository::Repository::append_reasoning_delta(&tx, &event)?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_project(&self, project: &Project) -> Result<()> {
        secrets::validate_persisted_id("project_id", &project.id.0)?;
        secrets::safe_metadata_text("project_name", &project.name)?;
        secrets::safe_metadata_path("project_path", &project.root_path)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        repository::Repository::new(&tx).insert_project(project)?;
        tx.commit()?;
        Ok(())
    }

    pub fn project(&self, id: &ProjectId) -> Result<Project> {
        repository::Repository::new(&self.db.lock().unwrap()).project(id)
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        repository::Repository::new(&self.db.lock().unwrap()).projects()
    }

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        secrets::validate_persisted_id("session_id", &session.id.0)?;
        secrets::validate_persisted_id("legacy_run_id", &session.legacy_run_id)?;
        secrets::safe_metadata_text("session_title", &session.title)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        repository::Repository::new(&tx).insert_session(session)?;
        tx.commit()?;
        Ok(())
    }

    pub fn session(&self, id: &SessionId) -> Result<Session> {
        repository::Repository::new(&self.db.lock().unwrap()).session(id)
    }

    pub fn insert_agent(&self, agent: &Agent) -> Result<()> {
        secrets::validate_persisted_id("agent_id", &agent.id.0)?;
        secrets::validate_persisted_id("agent_session_id", &agent.session_id.0)?;
        secrets::safe_metadata_text("agent_display_name", &agent.display_name)?;
        secrets::safe_metadata_text("agent_role", &agent.role)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        repository::Repository::new(&tx).insert_agent(agent)?;
        tx.commit()?;
        Ok(())
    }

    pub fn agent(&self, id: &AgentId) -> Result<Agent> {
        repository::Repository::new(&self.db.lock().unwrap()).agent(id)
    }

    pub fn agent_lookup(&self, id: &AgentId) -> Result<std::result::Result<Agent, ChatTurnError>> {
        repository::Repository::new(&self.db.lock().unwrap()).agent_lookup(id)
    }

    pub fn session_lookup(
        &self,
        id: &SessionId,
    ) -> Result<std::result::Result<Session, ChatTurnError>> {
        repository::Repository::new(&self.db.lock().unwrap()).session_lookup(id)
    }

    pub fn turn_lookup(&self, id: &TurnId) -> Result<std::result::Result<Turn, ChatTurnError>> {
        repository::Repository::new(&self.db.lock().unwrap()).turn_lookup(id)
    }

    pub fn commit_turn(
        &self,
        turn: &Turn,
        tasks: &[TurnTask],
        key: Option<&str>,
        legacy_tasks: &[Task],
    ) -> Result<Turn> {
        validate_turn_input(turn, tasks, key)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(existing) = self.commit_turn_in_tx(&tx, turn, tasks, key, legacy_tasks)? {
            return Ok(existing);
        }
        tx.commit()?;
        Ok(turn.clone())
    }

    pub fn commit_turn_bundle(&self, bundle: TurnBundle<'_>, key: Option<&str>) -> Result<Turn> {
        self.commit_turn_bundle_inner(bundle, key, None)
    }

    pub(crate) fn commit_turn_bundle_in_project(
        &self,
        bundle: TurnBundle<'_>,
        key: Option<&str>,
        revision: u64,
    ) -> Result<Turn> {
        self.commit_turn_bundle_inner(bundle, key, Some(revision))
    }

    fn commit_turn_bundle_inner(
        &self,
        bundle: TurnBundle<'_>,
        key: Option<&str>,
        project_revision: Option<u64>,
    ) -> Result<Turn> {
        let TurnBundle {
            project,
            session,
            agents,
            turn,
            tasks,
            legacy_tasks,
        } = bundle;
        validate_turn_input(turn, tasks, key)?;
        anyhow::ensure!(session.project_id == project.id, "会话不属于项目");
        anyhow::ensure!(turn.project_id == project.id, "回合不属于项目");
        anyhow::ensure!(turn.session_id == session.id, "回合不属于会话");
        anyhow::ensure!(
            agents.iter().all(|agent| agent.session_id == session.id),
            "Agent 不属于会话"
        );
        anyhow::ensure!(
            tasks.iter().all(|task| task.session_id == session.id),
            "任务不属于会话"
        );
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(key) = key {
            match classify_both(&repository::Repository::new(&tx), key, &turn.request_hash)? {
                Some(IdempotencyReplay::Turn(existing)) => return Ok(existing),
                Some(IdempotencyReplay::LegacyRun(_)) => {
                    bail!("同一个 Idempotency-Key 已经绑定旧运行，不能再创建新回合")
                }
                None => {}
            }
        }
        let repo = repository::Repository::new(&tx);
        let existing_project: Option<(String, String)> = tx
            .query_row(
                "SELECT name,root_path FROM projects WHERE id=?1",
                [&project.id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((name, root_path)) = existing_project {
            anyhow::ensure!(
                name == project.name && root_path == project.root_path,
                "项目标识已绑定不同路径"
            );
        } else {
            repo.insert_project(project)?;
        }
        repo.insert_session(session)?;
        for agent in agents {
            repo.insert_agent(agent)?;
        }
        if let Some(existing) = self.commit_turn_in_tx(&tx, turn, tasks, key, legacy_tasks)? {
            return Ok(existing);
        }
        if let Some(revision) = project_revision {
            for task in legacy_tasks {
                crate::project_config::bind_task_tx(&tx, task, &project.id, revision)?;
                crate::network_authorization::bind_task_tx(&tx, task)?;
            }
        }
        tx.commit()?;
        Ok(turn.clone())
    }

    fn commit_turn_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        turn: &Turn,
        tasks: &[TurnTask],
        key: Option<&str>,
        legacy_tasks: &[Task],
    ) -> Result<Option<Turn>> {
        if let Some(key) = key {
            match classify_both(&repository::Repository::new(tx), key, &turn.request_hash)? {
                Some(IdempotencyReplay::Turn(existing)) => return Ok(Some(existing)),
                Some(IdempotencyReplay::LegacyRun(_)) => {
                    bail!("同一个 Idempotency-Key 已经绑定旧运行，不能再创建新回合")
                }
                None => {}
            }
        }
        let session = repository::Repository::new(tx).session(&turn.session_id)?;
        let legacy_ids: std::collections::HashSet<&str> = tasks
            .iter()
            .map(|task| task.legacy_task_id.as_str())
            .collect();
        let projected_ids: std::collections::HashSet<&str> =
            legacy_tasks.iter().map(|task| task.id.as_str()).collect();
        anyhow::ensure!(
            legacy_ids.len() == tasks.len()
                && projected_ids.len() == legacy_tasks.len()
                && legacy_ids == projected_ids,
            "新旧任务投影必须完整一一对应"
        );
        anyhow::ensure!(
            tasks.iter().all(|task| legacy_tasks
                .iter()
                .any(|legacy| legacy.id == task.legacy_task_id
                    && legacy.status == task.status.as_str())),
            "新旧任务初始状态不一致"
        );
        anyhow::ensure!(
            legacy_tasks
                .iter()
                .all(|task| task.run_id == session.legacy_run_id),
            "旧任务的 run_id 必须等于会话的 legacy_run_id"
        );
        let run_exists: i64 = tx.query_row(
            "SELECT count(*) FROM runs WHERE id=?1",
            [&session.legacy_run_id],
            |row| row.get(0),
        )?;
        if run_exists == 0 {
            insert_legacy_run(
                tx,
                &Run {
                    id: session.legacy_run_id.clone(),
                    title: session.title.clone(),
                    kind: session.kind,
                    created_at: session.created_at,
                    tasks: Vec::new(),
                },
            )?;
        } else {
            let (title, kind): (String, String) = tx.query_row(
                "SELECT title,kind FROM runs WHERE id=?1",
                [&session.legacy_run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            secrets::safe_metadata_text("run_title", &title)?;
            anyhow::ensure!(
                kind == session.kind.as_str(),
                "旧 Run 的会话类型与当前 Session 不一致"
            );
        }
        crate::worktree_management::ensure_project_unreserved_tx(tx, &turn.project_id)?;
        for task in legacy_tasks {
            insert_legacy_task(tx, task)?;
        }
        let event = stamped_event(
            0,
            &turn.session_id.0,
            Some(&turn.id.0),
            tasks
                .first()
                .map(|task| task.legacy_task_id.as_str())
                .unwrap_or(&turn.id.0),
            "status",
            json!({"status": turn.status.as_str()}),
            turn.created_at,
        );
        repository::Repository::commit_turn(tx, turn, tasks, key, &event)?;
        if let Some(key) = key {
            tx.execute(
                "INSERT INTO idempotency(key,run_id,created_at,request_hash) VALUES (?1,?2,?3,?4)",
                params![
                    key,
                    session.legacy_run_id,
                    turn.created_at,
                    turn.request_hash
                ],
            )?;
        }
        Ok(None)
    }

    pub fn turn(&self, id: &TurnId) -> Result<Turn> {
        repository::Repository::new(&self.db.lock().unwrap()).turn(id)
    }

    pub fn task_dependencies(&self, id: &TaskId) -> Result<Vec<TaskId>> {
        repository::Repository::new(&self.db.lock().unwrap()).dependencies(id)
    }

    pub fn dependency_tasks(&self, legacy_id: &str) -> Result<Option<Vec<Task>>> {
        let db = self.db.lock().unwrap();
        let task_id: Option<String> = db
            .query_row(
                "SELECT id FROM turn_tasks WHERE legacy_task_id=?1",
                [legacy_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(task_id) = task_id else {
            return Ok(None);
        };
        let mut stmt = db.prepare(
            "SELECT t.value FROM turn_task_dependencies d \
             JOIN turn_tasks tt ON tt.id=d.depends_on_task_id \
             JOIN tasks t ON t.id=tt.legacy_task_id \
             WHERE d.task_id=?1 ORDER BY d.depends_on_task_id",
        )?;
        let values = stmt
            .query_map([task_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(
            values
                .into_iter()
                .map(|value| serde_json::from_str(&value).map_err(Into::into))
                .collect::<Result<Vec<_>>>()?,
        ))
    }

    pub fn turn_tasks(&self, id: &TurnId) -> Result<Vec<TurnTask>> {
        repository::Repository::new(&self.db.lock().unwrap()).turn_tasks(id)
    }

    pub fn turns_for_session(&self, id: &SessionId) -> Result<Vec<Turn>> {
        repository::Repository::new(&self.db.lock().unwrap()).turns_for_session(id)
    }

    pub fn latest_committed_turn(&self, id: &SessionId) -> Result<Option<Turn>> {
        repository::Repository::new(&self.db.lock().unwrap()).latest_committed_turn(id)
    }

    pub(crate) fn chat_continuation_prefix(
        &self,
        session: &SessionId,
        previous: &Task,
    ) -> Result<Vec<Value>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let prefix = chat_continuation_prefix_tx(&tx, session, previous)?;
        tx.commit()?;
        Ok(prefix)
    }

    pub fn append_chat_turn(
        &self,
        turn: &Turn,
        task: &TurnTask,
        legacy_task: &Task,
        key: &str,
        expected_last_turn_id: &TurnId,
    ) -> Result<ChatTurnReceipt> {
        self.append_chat_turn_inner(turn, task, legacy_task, key, expected_last_turn_id, false)
    }

    pub(crate) fn append_chat_turn_authorized(
        &self,
        turn: &Turn,
        task: &TurnTask,
        legacy_task: &Task,
        key: &str,
        expected_last_turn_id: &TurnId,
    ) -> Result<ChatTurnReceipt> {
        self.append_chat_turn_inner(turn, task, legacy_task, key, expected_last_turn_id, true)
    }

    fn append_chat_turn_inner(
        &self,
        turn: &Turn,
        task: &TurnTask,
        legacy_task: &Task,
        key: &str,
        expected_last_turn_id: &TurnId,
        authorize: bool,
    ) -> Result<ChatTurnReceipt> {
        validate_turn_input(turn, std::slice::from_ref(task), Some(key))?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match classify_both(&repository::Repository::new(&tx), key, &turn.request_hash)? {
            Some(IdempotencyReplay::Turn(existing)) => {
                let tasks = repository::Repository::new(&tx).turn_tasks(&existing.id)?;
                let task = tasks.into_iter().next().ok_or(ChatTurnError::NotFound)?;
                return Ok(ChatTurnReceipt {
                    turn: existing,
                    task,
                    replayed: true,
                });
            }
            Some(IdempotencyReplay::LegacyRun(_)) => {
                return Err(IdempotencyConflict.into());
            }
            None => {}
        }
        let repo = repository::Repository::new(&tx);
        let session = repo.session(&turn.session_id)?;
        if session.kind != SessionKind::Chat || session.project_id != turn.project_id {
            return Err(ChatTurnError::UnsupportedSession.into());
        }
        let latest = repo
            .latest_committed_turn(&session.id)?
            .ok_or(ChatTurnError::NotFound)?;
        if &latest.id != expected_last_turn_id {
            return Err(ChatTurnError::StalePredecessor.into());
        }
        if matches!(
            latest.status,
            LifecycleStatus::Queued | LifecycleStatus::Running | LifecycleStatus::AwaitingResume
        ) {
            return Err(ChatTurnError::SessionBusy.into());
        }
        let previous_tasks = repo.turn_tasks(&latest.id)?;
        let previous = match previous_tasks.as_slice() {
            [only] if only.depends_on.is_empty() => only,
            _ => return Err(ChatTurnError::UnsupportedSession.into()),
        };
        if previous.agent_id != task.agent_id {
            return Err(ChatTurnError::UnsupportedSession.into());
        }
        if matches!(
            previous.status,
            LifecycleStatus::Queued | LifecycleStatus::Running | LifecycleStatus::AwaitingResume
        ) {
            return Err(ChatTurnError::SessionBusy.into());
        }
        let agent_count: i64 = tx.query_row(
            "SELECT count(*) FROM agents WHERE session_id=?1",
            [&session.id.0],
            |row| row.get(0),
        )?;
        if agent_count != 1 {
            return Err(ChatTurnError::UnsupportedSession.into());
        }
        let agent = repo.agent(&task.agent_id)?;
        if agent.session_id != session.id {
            return Err(ChatTurnError::UnsupportedSession.into());
        }
        let previous_legacy = legacy_task_row(&tx, &previous.legacy_task_id)?;
        if !previous_legacy.spec.depends_on.is_empty()
            || previous_legacy.run_id != session.legacy_run_id
        {
            return Err(ChatTurnError::UnsupportedSession.into());
        }
        let project = repo.project(&session.project_id)?;
        if !same_project_path(&previous_legacy.workspace, &project.root_path) {
            return Err(ChatTurnError::UnsupportedSession.into());
        }
        if authorize {
            crate::project_config::validate_task_tx(&tx, &previous_legacy, true, true)?;
        }
        let configured_route: Option<Route> = tx
            .query_row("SELECT value FROM config WHERE id='settings'", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .map(|raw| serde_json::from_str::<Settings>(&raw))
            .transpose()?
            .and_then(|settings| {
                settings
                    .routes
                    .into_iter()
                    .find(|route| route.id == previous_legacy.spec.route_id)
            });
        let prefix = chat_continuation_prefix_tx(&tx, &session.id, &previous_legacy)?;
        if !candidate_continues_snapshot(
            &previous_legacy,
            legacy_task,
            &command_message(legacy_task),
            &prefix,
            configured_route.as_ref(),
        ) {
            return Err(ChatTurnError::PredecessorChanged.into());
        }
        if !legacy_task.spec.depends_on.is_empty()
            || legacy_task.run_id != session.legacy_run_id
            || legacy_task.id != task.legacy_task_id
            || legacy_task.status != LifecycleStatus::Queued.as_str()
        {
            return Err(ChatTurnError::UnsupportedSession.into());
        }
        if self
            .commit_turn_in_tx(
                &tx,
                turn,
                std::slice::from_ref(task),
                Some(key),
                std::slice::from_ref(legacy_task),
            )?
            .is_some()
        {
            return Err(IdempotencyConflict.into());
        }
        if authorize {
            let config = crate::project_config::config_tx(&tx, &turn.project_id)?;
            crate::project_config::bind_task_tx(
                &tx,
                legacy_task,
                &turn.project_id,
                config.revision,
            )?;
            crate::network_authorization::bind_task_tx(&tx, legacy_task)?;
        }
        tx.commit()?;
        Ok(ChatTurnReceipt {
            turn: turn.clone(),
            task: task.clone(),
            replayed: false,
        })
    }

    pub fn idempotency_replay(
        &self,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<IdempotencyReplay>> {
        let db = self.db.lock().unwrap();
        let repo = repository::Repository::new(&db);
        classify_both(&repo, key, request_hash)
    }

    pub fn idempotent_turn(&self, key: &str, request_hash: &str) -> Result<Option<Turn>> {
        match self.idempotency_replay(key, request_hash)? {
            Some(IdempotencyReplay::Turn(turn)) => Ok(Some(turn)),
            Some(IdempotencyReplay::LegacyRun(_)) => {
                bail!("legacy replay requires legacy handling")
            }
            None => Ok(None),
        }
    }

    pub fn file_backups(&self, task_id: &str) -> Result<Vec<Value>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT data FROM events WHERE task_id=?1 AND kind='file_backup' ORDER BY seq",
        )?;
        let rows = stmt.query_map([task_id], |row| row.get::<_, String>(0))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(serde_json::from_str(&row?)?);
        }
        Ok(result)
    }

    pub(crate) fn has_legacy_file_backup(&self, task_id: &str) -> Result<bool> {
        Ok(self
            .file_backups(task_id)?
            .iter()
            .any(|value| value.get("change_id").is_none()))
    }

    pub(crate) fn prepare_workspace_change(
        &self,
        task: &Task,
        tool_call_id: &str,
        relative: &str,
        path_key: &str,
        before: Option<&[u8]>,
    ) -> Result<PreparedChange> {
        let path = relative.to_owned();
        let path_key = path_key.to_owned();
        let workspace_digest = digest(task.workspace.as_bytes());
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let approval = repository::Repository::new(&tx)
            .approval_by_tool_call(&task.id, tool_call_id)?
            .ok_or(WorkspaceChangeError::Corrupt)?;
        anyhow::ensure!(
            is_file_mutation_tool(&approval.tool_name) && approval.workspace == task.workspace,
            "文件变更审批绑定不匹配"
        );
        let completed: i64 = tx.query_row(
            "SELECT (SELECT count(*) FROM workspace_restores WHERE task_id=?1)+(SELECT count(*) FROM checkpoints WHERE task_id=?1)",
            [&task.id],
            |row| row.get(0),
        )?;
        if completed != 0 {
            return Err(WorkspaceChangeError::Unrestorable.into());
        }
        let uncertain: i64 = tx.query_row(
            "SELECT count(*) FROM workspace_changes WHERE task_id=?1 AND state IN ('prepared','unknown')",
            [&task.id],
            |row| row.get(0),
        )?;
        if uncertain != 0 {
            return Err(WorkspaceChangeError::Unknown { receipt: None }.into());
        }
        let latest: Option<String> = tx
            .query_row(
                "SELECT after_digest FROM workspace_changes WHERE task_id=?1 AND path_key=?2 AND state='finished' ORDER BY rowid DESC LIMIT 1",
                params![task.id, path_key],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(expected) = latest {
            let actual = before
                .map(digest)
                .ok_or(WorkspaceChangeError::Conflict { receipt: None })?;
            if actual != expected {
                return Err(WorkspaceChangeError::Conflict { receipt: None }.into());
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let kind = if before.is_some() {
            ChangeKind::Modified
        } else {
            ChangeKind::Created
        };
        let before_digest = before.map(digest);
        let before_blob = before.map(secrets::seal).transpose()?;
        let write_scopes = serde_json::to_string(&approval.write_scopes)?;
        tx.execute(
            "INSERT INTO workspace_changes(id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,before_blob,before_digest,after_digest,state,restore_state,created_at,finished_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL,'prepared','pending',?12,NULL)",
            params![
                id,
                task.id,
                tool_call_id,
                workspace_digest,
                approval.binding_digest,
                write_scopes,
                path,
                path_key,
                if kind == ChangeKind::Created { "created" } else { "modified" },
                before_blob,
                before_digest,
                now()
            ],
        )?;
        task_event_tx(
            &tx,
            &task.id,
            "file_backup",
            json!({"change_id":id,"path":path,"kind":kind,"state":"prepared"}),
        )?;
        tx.commit()?;
        drop(db);
        self.workspace_change_by_id(&id)
    }

    pub(crate) fn workspace_change_by_id(&self, id: &str) -> Result<PreparedChange> {
        let db = self.db.lock().unwrap();
        db.query_row(
            "SELECT id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,before_blob,before_digest,after_digest,state,restore_state \
             FROM workspace_changes WHERE id=?1",
            [id],
            workspace_change_row,
        )
        .map_err(Into::into)
    }

    pub(crate) fn workspace_changes_for_task(&self, task_id: &str) -> Result<Vec<PreparedChange>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,before_blob,before_digest,after_digest,state,restore_state \
             FROM workspace_changes WHERE task_id=?1 ORDER BY rowid",
        )?;
        Ok(stmt
            .query_map([task_id], workspace_change_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn mark_workspace_change_failed(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE workspace_changes SET state='failed',finished_at=?1 WHERE id=?2 AND state='prepared'",
            params![now(), id],
        )?;
        anyhow::ensure!(changed == 1, "文件变更失败状态竞争");
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn mark_workspace_change_unknown(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE workspace_changes SET state='unknown',finished_at=?1 WHERE id=?2 AND state='prepared'",
            params![now(), id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn latest_restore(&self, task_id: &str) -> Result<Option<RestoreReceipt>> {
        let db = self.db.lock().unwrap();
        let id: Option<String> = db
            .query_row(
                "SELECT id FROM workspace_restores WHERE task_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1",
                [task_id],
                |row| row.get(0),
            )
            .optional()?;
        id.map(|id| restore_receipt(&db, &id)).transpose()
    }

    pub(crate) fn claim_restore(
        &self,
        task_id: &str,
        changes: &[PreparedChange],
    ) -> Result<RestoreReceipt> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(id) = tx
            .query_row(
                "SELECT id FROM workspace_restores WHERE task_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1",
                [task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            return restore_receipt(&tx, &id);
        }
        crate::project_config::validate_task_tx(&tx, &legacy_task_row(&tx, task_id)?, true, false)?;
        crate::worktree_management::ensure_task_unreserved_tx(&tx, task_id)?;
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO workspace_restores(id,task_id,status,created_at,finished_at) VALUES (?1,?2,'claimed',?3,NULL)",
            params![id, task_id, now()],
        )?;
        for change in changes {
            tx.execute(
                "INSERT INTO workspace_restore_outcomes(restore_id,change_id,path,status,updated_at) VALUES (?1,?2,?3,'claimed',?4)",
                params![id, change.id, change.path, now()],
            )?;
        }
        task_event_tx(
            &tx,
            task_id,
            "file_restore",
            json!({"restore_id":id,"status":"claimed","count":changes.len()}),
        )?;
        tx.commit()?;
        drop(db);
        self.restore_receipt(&id)
    }

    pub(crate) fn finish_restore_path(&self, restore_id: &str, change_id: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE workspace_restore_outcomes SET status='complete',updated_at=?1 WHERE restore_id=?2 AND change_id=?3 AND status='claimed'",
            params![now(), restore_id, change_id],
        )?;
        anyhow::ensure!(changed == 1, "恢复路径状态竞争失败");
        tx.execute(
            "UPDATE workspace_changes SET restore_state='restored' WHERE id=?1",
            [change_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn finish_restore(
        &self,
        restore_id: &str,
        status: RestoreStatus,
    ) -> Result<RestoreReceipt> {
        let status = match status {
            RestoreStatus::Claimed => "claimed",
            RestoreStatus::Complete => "complete",
            RestoreStatus::Conflict => "conflict",
            RestoreStatus::Partial => "partial",
            RestoreStatus::Unknown => "unknown",
        };
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE workspace_restores SET status=?1,finished_at=?2 WHERE id=?3 AND status='claimed'",
            params![status, now(), restore_id],
        )?;
        anyhow::ensure!(changed == 1, "恢复状态竞争失败");
        if status != "complete" {
            tx.execute(
                "UPDATE workspace_changes SET restore_state='unknown' WHERE id IN (SELECT change_id FROM workspace_restore_outcomes WHERE restore_id=?1 AND status='claimed')",
                [restore_id],
            )?;
            tx.execute(
                "UPDATE workspace_restore_outcomes SET status='unknown',updated_at=?2 WHERE restore_id=?1 AND status='claimed'",
                params![restore_id, now()],
            )?;
        }
        let task_id: String = tx.query_row(
            "SELECT task_id FROM workspace_restores WHERE id=?1",
            [restore_id],
            |row| row.get(0),
        )?;
        task_event_tx(
            &tx,
            &task_id,
            "file_restore",
            json!({"restore_id":restore_id,"status":status}),
        )?;
        tx.commit()?;
        drop(db);
        self.restore_receipt(restore_id)
    }

    pub(crate) fn restore_receipt(&self, id: &str) -> Result<RestoreReceipt> {
        restore_receipt(&self.db.lock().unwrap(), id)
    }

    pub fn events(&self, run_id: &str, after: i64) -> Result<Vec<Event>> {
        let db = self.db.lock().unwrap();
        if let Some(session) = repository::Repository::new(&db).session_by_legacy_run(run_id)? {
            return repository::Repository::new(&db).events_after(&session.id, after);
        }
        let mut stmt = db.prepare(
            "SELECT e.seq,e.schema_version,e.cursor,e.session_id,e.turn_id,e.task_id,e.kind,e.data,e.at \
             FROM events e JOIN tasks t ON t.id=e.task_id \
             WHERE t.run_id=?1 AND e.seq>?2 ORDER BY e.seq LIMIT 500",
        )?;
        let rows = stmt.query_map(params![run_id, after], event_columns)?;
        let mut result = vec![];
        let mut previous = after;
        for row in rows {
            let event = row?;
            anyhow::ensure!(event.seq > previous, "事件游标没有单调递增");
            previous = event.seq;
            result.push(event);
        }
        Ok(result)
    }

    pub fn approval_by_tool_call(
        &self,
        task_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<approval::ApprovalRecord>> {
        repository::Repository::new(&self.db.lock().unwrap())
            .approval_by_tool_call(task_id, tool_call_id)
    }

    pub fn approval(&self, id: &str) -> Result<approval::ApprovalRecord> {
        repository::Repository::new(&self.db.lock().unwrap()).approval(id)
    }

    pub fn approvals_for_task(&self, task_id: &str) -> Result<Vec<approval::ApprovalRecord>> {
        repository::Repository::new(&self.db.lock().unwrap()).approvals_for_task(task_id)
    }

    pub fn pending_approvals_for_task(
        &self,
        task_id: &str,
    ) -> Result<Vec<approval::ApprovalRecord>> {
        Ok(self
            .approvals_for_task(task_id)?
            .into_iter()
            .filter(|r| r.status == approval::ApprovalStatus::Pending)
            .collect())
    }

    pub fn ensure_approval(
        &self,
        task: &Task,
        tool_call_id: &str,
        tool_name: &str,
        args: &Value,
    ) -> Result<(approval::ApprovalRecord, bool)> {
        self.ensure_approval_inner(task, tool_call_id, tool_name, args, false)
    }

    pub(crate) fn ensure_current_approval(
        &self,
        task: &Task,
        tool_call_id: &str,
        tool_name: &str,
        args: &Value,
    ) -> Result<(approval::ApprovalRecord, bool)> {
        self.ensure_approval_inner(task, tool_call_id, tool_name, args, true)
    }

    fn ensure_approval_inner(
        &self,
        task: &Task,
        tool_call_id: &str,
        tool_name: &str,
        args: &Value,
        require_current: bool,
    ) -> Result<(approval::ApprovalRecord, bool)> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if require_current {
            validate_current_approval_call(&tx, task, tool_call_id, tool_name)?;
        }
        let repo = repository::Repository::new(&tx);
        let (turn_id, session_id, binding) =
            approval_binding(&tx, task, tool_call_id, tool_name, args)?;
        if let Some(existing) = repo.approval_by_tool_call(&task.id, tool_call_id)? {
            let expected = approval::binding_digest(
                &task.id,
                existing.session_id.as_deref(),
                existing.turn_id.as_deref(),
                tool_call_id,
                tool_name,
                &task.workspace,
                &task.spec.write_scopes,
                task.spec.allow_commands,
                args,
            )?;
            if expected != existing.binding_digest
                || existing.tool_name != tool_name
                || existing.workspace != task.workspace
                || existing.write_scopes != task.spec.write_scopes
                || existing.allow_commands != task.spec.allow_commands
                || existing.args_digest != approval::args_digest(args)?
            {
                return Err(approval::ApprovalError::BindingConflict { id: existing.id }.into());
            }
            return Ok((existing, false));
        }
        let durable = legacy_task_row(&tx, &task.id)?;
        if durable.status == "cancelled" {
            return Err(approval::ApprovalError::Conflict {
                id: task.id.clone(),
                status: "cancelled".into(),
            }
            .into());
        }
        crate::worktree_management::ensure_task_unreserved_tx(&tx, &task.id)?;
        let record = approval::ApprovalRecord {
            network_notice: (tool_name == "run_command")
                .then(approval::CommandNetworkNotice::default),
            command_network_ack: if tool_name == "run_command" {
                approval::CommandNetworkAck::Required
            } else {
                approval::CommandNetworkAck::NotRequired
            },
            id: id(),
            tool_call_id: tool_call_id.to_owned(),
            task_id: task.id.clone(),
            turn_id: turn_id.clone(),
            session_id: session_id.clone(),
            tool_name: tool_name.to_owned(),
            args_digest: approval::args_digest(args)?,
            binding_digest: binding,
            workspace: task.workspace.clone(),
            write_scopes: task.spec.write_scopes.clone(),
            allow_commands: task.spec.allow_commands,
            preview: approval::preview(tool_name, args),
            status: approval::ApprovalStatus::Pending,
            execution_state: approval::ExecutionState::NotStarted,
            created_at: now(),
            decided_at: None,
            decided_by: None,
        };
        let safe_call_digest = safe_approval_call_digest(&tx, &durable, &record)?;
        insert_approval(&tx, &record)?;
        approval_event(
            &tx,
            &record,
            "approval.requested",
            json!({
                "approval_id": record.id,
                "tool_call_id": record.tool_call_id,
                "tool_name": record.tool_name,
                "preview": record.preview,
                "task_id": record.task_id,
                "turn_id": record.turn_id,
                "session_id": record.session_id,
                "evidence_version": 1,
                "safe_call_digest": safe_call_digest
            }),
        )?;
        tx.commit()?;
        Ok((record, true))
    }

    pub fn decide_approval(
        &self,
        approval_id: &str,
        approved: bool,
        decided_by: &str,
    ) -> Result<approval::ApprovalRecord> {
        self.decide_approval_with_network_ack(approval_id, approved, decided_by, false)
    }

    pub fn decide_approval_with_network_ack(
        &self,
        approval_id: &str,
        approved: bool,
        decided_by: &str,
        acknowledge_command_network: bool,
    ) -> Result<approval::ApprovalRecord> {
        let op = crate::execution_authority::current()
            .unwrap_or_else(crate::execution_authority::ExecutionOperation::detached);
        self.decide_approval_with_network_ack_for(
            approval_id,
            approved,
            decided_by,
            acknowledge_command_network,
            &op,
        )
    }

    pub(crate) fn decide_approval_with_network_ack_for(
        &self,
        approval_id: &str,
        approved: bool,
        decided_by: &str,
        acknowledge_command_network: bool,
        _op: &crate::execution_authority::ExecutionOperation,
    ) -> Result<approval::ApprovalRecord> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = repository::Repository::new(&tx).approval(approval_id)?;
        if approved {
            crate::project_config::validate_task_tx(
                &tx,
                &legacy_task_row(&tx, &current.task_id)?,
                true,
                false,
            )?;
        }
        if acknowledge_command_network && current.tool_name != "run_command" {
            return Err(
                approval::ApprovalError::InvalidNetworkAcknowledgement { id: current.id }.into(),
            );
        }
        if approved && current.tool_name == "run_command" {
            if current.execution_state == approval::ExecutionState::Unknown {
                return Err(approval::ApprovalError::UnknownResult { id: current.id }.into());
            }
            if !acknowledge_command_network {
                return Err(approval::ApprovalError::NetworkAcknowledgementRequired {
                    id: current.id,
                }
                .into());
            }
            if current.execution_state != approval::ExecutionState::NotStarted {
                return Err(approval::ApprovalError::Conflict {
                    id: current.id,
                    status: current.execution_state.as_str().into(),
                }
                .into());
            }
            if matches!(
                current.status,
                approval::ApprovalStatus::Pending | approval::ApprovalStatus::Approved
            ) {
                if current.command_network_ack != approval::CommandNetworkAck::Acknowledged {
                    crate::worktree_management::ensure_task_unreserved_tx(&tx, &current.task_id)?;
                    tx.execute(
                        "INSERT INTO command_network_ack VALUES (?1,?2,1,?3)",
                        params![current.id, current.binding_digest, now()],
                    )?;
                    approval_event(
                        &tx,
                        &current,
                        "approval.command_network_ack",
                        json!({"approval_id":current.id,"notice_version":1,"may_access_network":true,"outbound_sandbox":false}),
                    )?;
                }
                if current.status == approval::ApprovalStatus::Approved {
                    let result = repository::Repository::new(&tx).approval(approval_id)?;
                    tx.commit()?;
                    return Ok(result);
                }
            }
        }
        if current.status != approval::ApprovalStatus::Pending {
            return Err(approval::ApprovalError::Conflict {
                id: approval_id.to_owned(),
                status: current.status.as_str().to_owned(),
            }
            .into());
        }
        let decided_at = now();
        let status = if approved {
            approval::ApprovalStatus::Approved
        } else {
            approval::ApprovalStatus::Denied
        };
        anyhow::ensure!(
            !decided_by.is_empty() && decided_by.len() <= 128,
            "审批决定人无效"
        );
        secrets::safe_metadata_text("decided_by", decided_by)?;
        if approved {
            crate::worktree_management::ensure_task_unreserved_tx(&tx, &current.task_id)?;
        }
        let changed = tx.execute(
            "UPDATE approvals SET status=?2, decided_at=?3, decided_by=?4 WHERE id=?1 AND status='pending'",
            params![approval_id, status.as_str(), decided_at, decided_by],
        )?;
        anyhow::ensure!(changed == 1, "审批决定竞争失败");
        let updated = repository::Repository::new(&tx).approval(approval_id)?;
        approval_event(
            &tx,
            &updated,
            "approval.resolved",
            json!({
                "approval_id": updated.id,
                "status": updated.status.as_str(),
                "decided_by": updated.decided_by,
                "decided_at": updated.decided_at
            }),
        )?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn cancel_pending_approvals(&self, task_id: &str) -> Result<Vec<approval::ApprovalRecord>> {
        let op = crate::execution_authority::current()
            .unwrap_or_else(crate::execution_authority::ExecutionOperation::detached);
        self.cancel_pending_approvals_for(task_id, &op)
    }

    pub(crate) fn cancel_pending_approvals_for(
        &self,
        task_id: &str,
        _op: &crate::execution_authority::ExecutionOperation,
    ) -> Result<Vec<approval::ApprovalRecord>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut task = legacy_task_row(&tx, task_id)?;
        if matches!(task.status.as_str(), "queued" | "running") {
            task.status = "cancelled".into();
            task.error = Some("任务已停止".into());
            task.updated_at = now();
            save_task_tx(&tx, &task, false)?;
        }
        let pending = repository::Repository::new(&tx)
            .approvals_for_task(task_id)?
            .into_iter()
            .filter(|record| {
                record.execution_state == approval::ExecutionState::NotStarted
                    && matches!(
                        record.status,
                        approval::ApprovalStatus::Pending | approval::ApprovalStatus::Approved
                    )
            })
            .collect::<Vec<_>>();
        let decided_at = now();
        let mut cancelled = Vec::new();
        for record in pending {
            let was_pending = record.status == approval::ApprovalStatus::Pending;
            tx.execute(
                "UPDATE approvals SET status=CASE WHEN status='pending' THEN 'cancelled' ELSE status END, \
                 execution_state='cancelled', \
                 decided_at=CASE WHEN status='pending' THEN ?2 ELSE decided_at END, \
                 decided_by=CASE WHEN status='pending' THEN 'system:task-cancelled' ELSE decided_by END \
                 WHERE id=?1 AND execution_state='not_started' AND status IN ('pending','approved')",
                params![record.id, decided_at],
            )?;
            let updated = repository::Repository::new(&tx).approval(&record.id)?;
            if was_pending {
                approval_event(
                    &tx,
                    &updated,
                    "approval.resolved",
                    json!({
                        "approval_id": updated.id,
                        "status": updated.status.as_str(),
                        "decided_by": updated.decided_by,
                        "decided_at": updated.decided_at
                    }),
                )?;
            }
            cancelled.push(updated);
        }
        tx.commit()?;
        Ok(cancelled)
    }

    pub fn claim_approval(
        &self,
        task: &Task,
        tool_call_id: &str,
        tool_name: &str,
        args: &Value,
    ) -> Result<approval::ApprovalRecord> {
        self.claim_approval_inner(task, tool_call_id, tool_name, args, false)
    }

    pub(crate) fn claim_current_approval(
        &self,
        task: &Task,
        tool_call_id: &str,
        tool_name: &str,
        args: &Value,
    ) -> Result<approval::ApprovalRecord> {
        self.claim_approval_inner(task, tool_call_id, tool_name, args, true)
    }

    fn claim_approval_inner(
        &self,
        task: &Task,
        tool_call_id: &str,
        tool_name: &str,
        args: &Value,
        require_current: bool,
    ) -> Result<approval::ApprovalRecord> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if require_current {
            validate_current_approval_call(&tx, task, tool_call_id, tool_name)?;
        }
        let current = repository::Repository::new(&tx)
            .approval_by_tool_call(&task.id, tool_call_id)?
            .ok_or_else(|| approval::ApprovalError::NotFound {
                id: tool_call_id.to_owned(),
            })?;
        let (_, _, expected) = approval_binding(&tx, task, tool_call_id, tool_name, args)?;
        if expected != current.binding_digest
            || approval::args_digest(args)? != current.args_digest
            || current.tool_name != tool_name
            || current.workspace != task.workspace
            || current.write_scopes != task.spec.write_scopes
            || current.allow_commands != task.spec.allow_commands
        {
            return Err(approval::ApprovalError::BindingConflict { id: current.id }.into());
        }
        if matches!(
            current.execution_state,
            approval::ExecutionState::Unknown | approval::ExecutionState::Claimed
        ) {
            return Err(approval::ApprovalError::UnknownResult { id: current.id }.into());
        }
        if current.status == approval::ApprovalStatus::Cancelled
            || current.execution_state == approval::ExecutionState::Cancelled
            || legacy_task_row(&tx, &task.id)?.status != "running"
        {
            return Err(approval::ApprovalError::Conflict {
                id: current.id,
                status: "cancelled".into(),
            }
            .into());
        }
        anyhow::ensure!(
            current.status == approval::ApprovalStatus::Approved,
            "审批未批准，工具未执行"
        );
        anyhow::ensure!(
            current.execution_state == approval::ExecutionState::NotStarted,
            "审批执行状态不可领取"
        );
        if current.tool_name == "run_command"
            && current.command_network_ack != approval::CommandNetworkAck::Acknowledged
        {
            return Err(
                approval::ApprovalError::NetworkAcknowledgementRequired { id: current.id }.into(),
            );
        }
        crate::project_config::validate_task_tx(&tx, task, true, false)?;
        if is_file_mutation_tool(tool_name) {
            crate::plugin_management::ensure_files_enabled_tx(&tx)?;
        } else if tool_name == "run_command" || is_authorized_mcp_tool(task, tool_name) {
            crate::plugin_management::ensure_plugin_enabled_tx(
                &tx,
                crate::plugin_management::BUILTIN_SHELL_ID,
            )?;
        }
        crate::worktree_management::ensure_task_unreserved_tx(&tx, &task.id)?;
        let changed = tx.execute(
            "UPDATE approvals SET execution_state='claimed' WHERE id=?1 AND status='approved' AND execution_state='not_started'",
            [&current.id],
        )?;
        anyhow::ensure!(changed == 1, "审批执行权竞争失败");
        let claimed = repository::Repository::new(&tx).approval(&current.id)?;
        tx.commit()?;
        Ok(claimed)
    }

    pub fn finish_approval(
        &self,
        task: &Task,
        approval_id: &str,
        result: &str,
        name: &str,
        tool_call_id: &str,
    ) -> Result<String> {
        self.finish_approval_with_change(task, approval_id, result, name, tool_call_id, None)
    }

    pub(crate) fn finish_approval_with_change(
        &self,
        task: &Task,
        approval_id: &str,
        result: &str,
        name: &str,
        tool_call_id: &str,
        change: Option<(&str, &[u8])>,
    ) -> Result<String> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = repository::Repository::new(&tx).approval(approval_id)?;
        if current.task_id != task.id
            || current.tool_call_id != tool_call_id
            || current.tool_name != name
        {
            return Err(approval::ApprovalError::CorruptState {
                id: approval_id.to_owned(),
            }
            .into());
        }
        anyhow::ensure!(
            current.execution_state == approval::ExecutionState::Claimed,
            "审批执行状态不是 claimed"
        );
        let value = serde_json::from_str::<Value>(result)
            .unwrap_or_else(|_| Value::String(result.to_owned()));
        let value = secrets::redact_persisted(&value);
        let result = stable_safe_json(&value)?;
        let mut updated_task = task.clone();
        updated_task
            .messages
            .push(json!({"role":"tool","tool_call_id":tool_call_id,"content":result}));
        updated_task.updated_at = now();
        save_task_tx(&tx, &updated_task, false)?;
        task_event_tx(
            &tx,
            &task.id,
            "tool_result",
            json!({"name":name,"tool_call_id":tool_call_id,"approval_id":approval_id,"result":value}),
        )?;
        if let Some((change_id, after)) = change {
            let changed = tx.execute(
                "UPDATE workspace_changes SET after_digest=?1,state='finished',finished_at=?2 WHERE id=?3 AND task_id=?4 AND tool_call_id=?5 AND state='prepared'",
                params![digest(after), now(), change_id, task.id, tool_call_id],
            )?;
            anyhow::ensure!(changed == 1, "文件变更完成状态竞争失败");
            task_event_tx(
                &tx,
                &task.id,
                "file_backup",
                json!({"change_id":change_id,"state":"finished"}),
            )?;
        }
        tx.execute(
            "UPDATE approvals SET execution_state='finished' WHERE id=?1 AND execution_state='claimed'",
            [&approval_id],
        )?;
        tx.commit()?;
        Ok(result)
    }

    /// Startup recovery: mark queued/running tasks as `interrupted` and settle
    /// unfinished claims cleanly as cancelled/failed/conflict WITHOUT sealing
    /// the task, session, or project in an unrecoverable `'unknown'` dead-end.
    pub fn recover(&self) -> Result<usize> {
        let controller = self.authority.claim()?;
        self.recover_with_controller(&controller)
    }

    pub(crate) fn recover_with_controller(
        &self,
        controller: &crate::execution_authority::ExecutionController,
    ) -> Result<usize> {
        anyhow::ensure!(
            controller.owns(&self.authority) && controller.can_recover(),
            "recovery requires an unstarted controller for this domain"
        );
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let pending = repository::Repository::new(&tx).nonterminal_turns()?;
        let values: Vec<String> = {
            let mut stmt = tx.prepare("SELECT value FROM tasks")?;
            stmt.query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut affected = crate::network_authorization::recover_calls_tx(&tx)?;
        for value in values {
            let mut task: Task = serde_json::from_str(&value)?;
            if matches!(task.status.as_str(), "queued" | "running") {
                let streamed: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "SELECT data FROM events WHERE task_id=?1 AND kind='delta' ORDER BY seq",
                    )?;
                    stmt.query_map([&task.id], |row| row.get(0))?
                        .collect::<rusqlite::Result<_>>()?
                };
                if !streamed.is_empty() {
                    let mut text = String::new();
                    for chunk in streamed {
                        let value: Value = serde_json::from_str(&chunk)?;
                        text.push_str(value["text"].as_str().unwrap_or_default());
                    }
                    task.output = text;
                }
                task.status = "interrupted".into();
                task.error = Some("服务曾中断；可以继续此成员，模型和身份保持不变".into());
                task.updated_at = now();
                save_task_tx(&tx, &task, false)?;
                affected += 1;
            }
        }
        affected += tx.execute(
            "UPDATE approvals SET execution_state='cancelled' WHERE execution_state IN ('claimed','unknown')",
            [],
        )?;
        affected += tx.execute(
            "UPDATE workspace_changes SET state='failed', finished_at=COALESCE(finished_at,?1) WHERE state IN ('prepared','unknown')",
            [now()],
        )?;
        tx.execute(
            "UPDATE workspace_changes SET restore_state='pending' WHERE restore_state='unknown'",
            [],
        )?;
        affected += tx.execute(
            "UPDATE workspace_restores SET status='conflict', finished_at=COALESCE(finished_at,?1) WHERE status IN ('claimed','unknown')",
            [now()],
        )?;
        tx.execute(
            "UPDATE workspace_restore_outcomes SET status='conflict', updated_at=?1 WHERE status IN ('claimed','unknown')",
            [now()],
        )?;
        for previous in pending {
            if repository::Repository::new(&tx).turn(&previous.id)?.status != previous.status {
                affected += 1;
            }
        }
        affected += crate::worktree_management::recover_claimed_tx(&tx)?;
        let _ = tx.execute("UPDATE execution_owner SET phase='active' WHERE id=1", []);
        tx.commit()?;
        controller.finish_recovery()?;
        Ok(affected)
    }

    pub fn checkpoints_for_tasks(
        &self,
        task_ids: &[String],
    ) -> Result<Vec<crate::checkpoint::Checkpoint>> {
        if task_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let mut rows_with_order: Vec<(i64, String)> = Vec::new();
        for chunk in task_ids.chunks(500) {
            let placeholders = (1..=chunk.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT rowid, id FROM checkpoints WHERE task_id IN ({placeholders}) ORDER BY rowid DESC"
            );
            let mut stmt = tx.prepare(&sql)?;
            let mapped = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in mapped {
                rows_with_order.push(row?);
            }
        }
        rows_with_order.sort_by_key(|a| std::cmp::Reverse(a.0));
        rows_with_order.dedup_by(|a, b| a.1 == b.1);
        let mut checkpoints = Vec::with_capacity(rows_with_order.len());
        for (_, id) in rows_with_order {
            checkpoints.push(read_checkpoint(&tx, &id)?.0);
        }
        Ok(checkpoints)
    }

    pub(crate) fn checkpoint_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<(crate::checkpoint::Checkpoint, String)>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT id,creation_key FROM checkpoints WHERE task_id=?1",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(id, key)| Ok((read_checkpoint(&tx, &id)?.0, key)))
            .transpose()
    }

    pub(crate) fn checkpoint_record(
        &self,
        id: &str,
    ) -> Result<(crate::checkpoint::Checkpoint, crate::checkpoint::Manifest)> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        read_checkpoint(&tx, id)
    }

    pub(crate) fn commit_checkpoint(
        &self,
        task: &Task,
        key: &str,
        manifest: &crate::checkpoint::Manifest,
    ) -> Result<crate::checkpoint::CheckpointCreated> {
        use crate::checkpoint::{Checkpoint, CheckpointCreated, CheckpointError};
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some((id, old_key)) = tx
            .query_row(
                "SELECT id,creation_key FROM checkpoints WHERE task_id=?1",
                [&task.id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if old_key != key {
                return Err(CheckpointError::Conflict.into());
            }
            return Ok(CheckpointCreated {
                checkpoint: read_checkpoint(&tx, &id)?.0,
                replayed: true,
            });
        }
        let value: String =
            tx.query_row("SELECT value FROM tasks WHERE id=?1", [&task.id], |r| {
                r.get(0)
            })?;
        let current: Task = serde_json::from_str(&value).map_err(|_| CheckpointError::Corrupt)?;
        crate::project_config::validate_task_tx(&tx, &current, false, false)?;
        if safe_task_value(&current)? != safe_task_value(task)? || current.status != "completed" {
            return Err(CheckpointError::Conflict.into());
        }
        let unsafe_state: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_restores WHERE task_id=?1) \
             OR EXISTS(SELECT 1 FROM approvals WHERE task_id=?1 AND (status='pending' OR execution_state IN ('claimed','unknown') OR (status='approved' AND execution_state!='finished'))) \
             OR EXISTS(SELECT 1 FROM workspace_changes WHERE task_id=?1 AND (state IN ('prepared','unknown') OR restore_state!='pending'))",
            [&task.id],
            |r| r.get(0),
        )?;
        if unsafe_state {
            return Err(CheckpointError::Unrestorable.into());
        }
        verify_checkpoint_sources(&tx, manifest)?;
        crate::worktree_management::ensure_task_unreserved_tx(&tx, &task.id)?;
        let serialized = serde_json::to_string(manifest)?;
        let checkpoint = Checkpoint {
            checkpoint_id: uuid::Uuid::new_v4().to_string(),
            task_id: task.id.clone(),
            kind: "task_before".into(),
            generation: 1,
            created_at: now(),
            manifest_digest: digest(serialized.as_bytes()),
            entries: manifest.entries.clone(),
        };
        tx.execute(
            "INSERT INTO checkpoints(id,task_id,creation_key,kind,generation,created_at,manifest_digest,manifest) \
             VALUES (?1,?2,?3,'task_before',1,?4,?5,?6)",
            params![
                checkpoint.checkpoint_id,
                task.id,
                key,
                checkpoint.created_at,
                checkpoint.manifest_digest,
                serialized
            ],
        )?;
        task_event_tx(
            &tx,
            &task.id,
            "checkpoint.created",
            json!({
                "checkpoint_id": checkpoint.checkpoint_id,
                "task_id": task.id,
                "count": checkpoint.entries.len(),
                "manifest_digest": checkpoint.manifest_digest
            }),
        )?;
        tx.commit()?;
        Ok(CheckpointCreated {
            checkpoint,
            replayed: false,
        })
    }
}

fn ensure_column_if_missing(
    db: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<()> {
    let existing: Vec<String> = {
        let mut stmt = db.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if !existing.iter().any(|c| c == column) {
        db.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))?;
    }
    Ok(())
}

fn workspace_change_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PreparedChange> {
    let kind: String = row.get(8)?;
    let state: String = row.get(12)?;
    let invalid = |column, name| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, name)),
        )
    };
    let kind = match kind.as_str() {
        "created" => ChangeKind::Created,
        "modified" => ChangeKind::Modified,
        _ => return Err(invalid(8, "invalid workspace change kind")),
    };
    let state = match state.as_str() {
        "prepared" => ChangeState::Prepared,
        "finished" => ChangeState::Finished,
        "unknown" => ChangeState::Unknown,
        "failed" => ChangeState::Failed,
        _ => return Err(invalid(12, "invalid workspace change state")),
    };
    let restore_state: String = row.get(13)?;
    if !matches!(restore_state.as_str(), "pending" | "restored" | "unknown") {
        return Err(invalid(13, "invalid workspace restore state"));
    }
    Ok(PreparedChange {
        id: row.get(0)?,
        task_id: row.get(1)?,
        tool_call_id: row.get(2)?,
        workspace_digest: row.get(3)?,
        binding_digest: row.get(4)?,
        write_scopes: row.get(5)?,
        path: row.get(6)?,
        path_key: row.get(7)?,
        kind,
        before_blob: row.get(9)?,
        before_digest: row.get(10)?,
        after_digest: row.get(11)?,
        state,
        restore_state,
    })
}

fn restore_receipt(db: &Connection, id: &str) -> Result<RestoreReceipt> {
    let (task_id, status): (String, String) = db.query_row(
        "SELECT task_id,status FROM workspace_restores WHERE id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let status = match status.as_str() {
        "claimed" => RestoreStatus::Claimed,
        "complete" => RestoreStatus::Complete,
        "conflict" => RestoreStatus::Conflict,
        "partial" => RestoreStatus::Partial,
        _ => RestoreStatus::Unknown,
    };
    let mut stmt = db.prepare(
        "SELECT change_id,path,status FROM workspace_restore_outcomes WHERE restore_id=?1 ORDER BY rowid",
    )?;
    let outcomes = stmt
        .query_map([id], |row| {
            let value: String = row.get(2)?;
            Ok(RestorePathOutcome {
                change_id: row.get(0)?,
                path: row.get(1)?,
                status: match value.as_str() {
                    "claimed" => RestoreStatus::Claimed,
                    "complete" => RestoreStatus::Complete,
                    "conflict" => RestoreStatus::Conflict,
                    _ => RestoreStatus::Unknown,
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let restored = outcomes
        .iter()
        .filter(|outcome| outcome.status == RestoreStatus::Complete)
        .count();
    Ok(RestoreReceipt {
        restore_id: Some(id.to_owned()),
        task_id,
        status,
        restored,
        outcomes,
    })
}

fn tool_result_value(content: &str) -> Value {
    serde_json::from_str(content).unwrap_or_else(|_| Value::String(content.into()))
}

fn command_message(task: &Task) -> String {
    task.messages
        .last()
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_owned()
}

fn run_context_error(error: anyhow::Error) -> anyhow::Error {
    if let Some(sqlite) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<rusqlite::Error>())
    {
        match sqlite {
            rusqlite::Error::QueryReturnedNoRows
            | rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..) => {}
            _ => return error,
        }
    }
    RunContextError::CorruptState.into()
}

fn same_project_path(left: &str, right: &str) -> bool {
    let Ok(left) = std::fs::canonicalize(left) else {
        return false;
    };
    let Ok(right) = std::fs::canonicalize(right) else {
        return false;
    };
    left == right
}

fn close_dangling_calls(messages: &[Value]) -> Vec<Value> {
    let answered: std::collections::HashSet<String> = messages
        .iter()
        .filter_map(|m| m["tool_call_id"].as_str().map(str::to_owned))
        .collect();
    let mut out = Vec::with_capacity(messages.len() + 2);
    for msg in messages {
        out.push(msg.clone());
        if msg["role"] == "assistant"
            && let Some(calls) = msg["tool_calls"].as_array()
        {
            for call in calls {
                if let Some(id) = call["id"].as_str()
                    && !answered.contains(id)
                {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "content": "{\"error\":\"Previous turn ended before tool execution completed.\"}"
                    }));
                }
            }
        }
    }
    out
}

fn chat_continuation_prefix_tx(
    tx: &rusqlite::Transaction<'_>,
    session_id: &SessionId,
    previous: &Task,
) -> Result<Vec<Value>> {
    let repo = repository::Repository::new(tx);
    let session = repo.session(session_id)?;
    let latest = repo
        .latest_committed_turn(session_id)?
        .ok_or(ChatTurnError::NotFound)?;
    let mut stmt = tx.prepare(
        "SELECT t.id FROM turns t JOIN events e ON e.turn_id=t.id AND e.session_id=t.session_id \
         WHERE t.session_id=?1 GROUP BY t.id ORDER BY MIN(e.seq)",
    )?;
    let turns = stmt
        .query_map([&session_id.0], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut prefix: Vec<Value> = Vec::new();
    let mut recovery_facts = Vec::new();
    let mut agent_id = None;
    for turn_id in turns {
        let turn = repo.turn(&TurnId(turn_id))?;
        let tasks = repo.turn_tasks(&turn.id)?;
        let projected = match tasks.as_slice() {
            [only] if only.depends_on.is_empty() => only,
            _ => return Err(ChatTurnError::UnsupportedSession.into()),
        };
        let task = legacy_task_row(tx, &projected.legacy_task_id)?;
        if turn.project_id != session.project_id
            || projected.session_id != session.id
            || task.run_id != session.legacy_run_id
            || !task.spec.depends_on.is_empty()
            || agent_id
                .as_ref()
                .is_some_and(|id| id != &projected.agent_id)
        {
            return Err(ChatTurnError::CorruptState.into());
        }
        agent_id = Some(projected.agent_id.clone());
        let unresolved: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM approvals WHERE task_id=?1 AND (status='pending' OR execution_state IN ('claimed','unknown') OR (status='approved' AND execution_state NOT IN ('finished','cancelled')))) \
             OR EXISTS(SELECT 1 FROM workspace_changes WHERE task_id=?1 AND (state IN ('prepared','unknown') OR restore_state='unknown')) \
             OR EXISTS(SELECT 1 FROM workspace_restores WHERE task_id=?1 AND status IN ('claimed','partial','unknown')) \
             OR EXISTS(SELECT 1 FROM workspace_restore_outcomes o JOIN workspace_restores r ON r.id=o.restore_id WHERE r.task_id=?1 AND o.status IN ('claimed','unknown'))",
            [&task.id],
            |row| row.get(0),
        )?;
        if unresolved {
            return Err(ChatTurnError::UnresolvedEffects.into());
        }
        if matches!(
            task.status.as_str(),
            "queued" | "running" | "awaiting_resume"
        ) || matches!(
            projected.status,
            LifecycleStatus::Queued | LifecycleStatus::Running | LifecycleStatus::AwaitingResume
        ) || matches!(
            turn.status,
            LifecycleStatus::Queued | LifecycleStatus::Running | LifecycleStatus::AwaitingResume
        ) {
            return Err(ChatTurnError::SessionBusy.into());
        }
        if turn.id == latest.id && (task.id != previous.id || task.messages != previous.messages) {
            return Err(ChatTurnError::PredecessorChanged.into());
        }
        let mut restores = tx.prepare(
            "SELECT id FROM workspace_restores WHERE task_id=?1 AND status='complete' ORDER BY created_at,rowid",
        )?;
        let restores = restores
            .query_map([&task.id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for restore_id in restores {
            let receipt = restore_receipt(tx, &restore_id)?;
            if receipt
                .outcomes
                .iter()
                .any(|outcome| outcome.status != RestoreStatus::Complete)
            {
                return Err(ChatTurnError::CorruptState.into());
            }
            recovery_facts.push(json!({"task_id":task.id,"restore_id":restore_id}));
        }
        prefix = close_dangling_calls(&task.messages);
    }
    if !recovery_facts.is_empty() {
        prefix.push(json!({
            "role": "user",
            "content": format!(
                "Host recovery fact: completed workspace restores {}. Prior tool results describe historical file contents, not current disk state. Read the current files before further edits.",
                serde_json::to_string(&recovery_facts)?
            )
        }));
    }
    Ok(prefix)
}

fn candidate_continues_snapshot(
    previous: &Task,
    candidate: &Task,
    message: &str,
    expected_prefix: &[Value],
    configured_route: Option<&Route>,
) -> bool {
    let candidate_route = serde_json::to_value(&candidate.route).ok();
    let route_matches_previous = candidate_route == serde_json::to_value(&previous.route).ok();
    let route_matches_configured = configured_route
        .and_then(|route| serde_json::to_value(route).ok())
        .is_some_and(|expected| candidate_route.as_ref() == Some(&expected));
    if candidate.spec.prompt != message
        || (!route_matches_previous && !route_matches_configured)
        || candidate.workspace != previous.workspace
        || candidate.spec.name != previous.spec.name
        || candidate.spec.role != previous.spec.role
        || candidate.spec.route_id != previous.spec.route_id
        || candidate.spec.max_rounds != previous.spec.max_rounds
        || candidate.spec.tools != previous.spec.tools
        || candidate.spec.write_scopes != previous.spec.write_scopes
        || candidate.spec.allow_commands != previous.spec.allow_commands
    {
        return false;
    }
    let Some((last, prefix)) = candidate.messages.split_last() else {
        return false;
    };
    let closed_prev = close_dangling_calls(&previous.messages);
    (prefix == expected_prefix
        || prefix == closed_prev.as_slice()
        || prefix == previous.messages.as_slice())
        && last["role"] == "user"
        && last["content"] == message
        && last.get("tool_calls").is_none()
}

fn validate_current_approval_call(
    tx: &rusqlite::Transaction<'_>,
    task: &Task,
    call_id: &str,
    name: &str,
) -> Result<()> {
    let conflict = || approval::ApprovalError::BindingConflict {
        id: call_id.to_owned(),
    };
    let durable = legacy_task_row(tx, &task.id)?;
    let saved = safe_task_value(&durable)?;
    let caller = safe_task_value(task)?;
    for field in [
        "id",
        "run_id",
        "spec",
        "route",
        "workspace",
        "created_at",
        "messages",
    ] {
        if saved[field] != caller[field] {
            return Err(conflict().into());
        }
    }
    approval::validate_call_history(&durable.messages)?;
    let (turn, session) = approval_scope(tx, &durable.id)?;
    let inherited = inherited_message_count(tx, &durable, turn.as_deref(), session.as_deref())?;
    let index = durable
        .messages
        .iter()
        .rposition(|message| message.get("tool_calls").is_some())
        .ok_or_else(conflict)?;
    if index < inherited {
        return Err(conflict().into());
    }
    validate_prior_groups(&durable.messages[inherited..index])?;
    let group = durable.messages[index]["tool_calls"]
        .as_array()
        .ok_or_else(conflict)?;
    let prior = &durable.messages[index + 1..];
    let call = group.get(prior.len()).ok_or_else(conflict)?;
    if durable.messages[index]["role"] != "assistant"
        || call["id"] != call_id
        || call["type"] != "function"
        || call["function"]["name"] != name
        || prior.iter().zip(group).any(|(answer, previous)| {
            answer["role"] != "tool" || answer["tool_call_id"] != previous["id"]
        })
    {
        return Err(conflict().into());
    }
    Ok(())
}

fn approval_binding(
    tx: &rusqlite::Transaction<'_>,
    task: &Task,
    call_id: &str,
    name: &str,
    args: &Value,
) -> Result<(Option<String>, Option<String>, String)> {
    let conflict = || approval::ApprovalError::BindingConflict {
        id: call_id.to_owned(),
    };
    let durable =
        legacy_task_row(tx, &task.id).map_err(|_| approval::ApprovalError::CorruptState {
            id: task.id.clone(),
        })?;
    if durable.workspace != task.workspace
        || durable.spec.write_scopes != task.spec.write_scopes
        || durable.spec.allow_commands != task.spec.allow_commands
        || durable.spec.tools != task.spec.tools
        || !is_known_task_tool(&durable, name)
        || evaluate_tool_policy(&durable, name) != approval::PolicyDecision::RequireApproval
    {
        return Err(conflict().into());
    }
    approval::validate_call_history(&durable.messages)?;
    let call = durable
        .messages
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .find(|c| c["id"] == call_id)
        .ok_or_else(|| approval::ApprovalError::CorruptState { id: call_id.into() })?;
    let raw_call = task
        .messages
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .find(|c| c["id"] == call_id)
        .ok_or_else(conflict)?;
    let raw_args = raw_call["function"]["arguments"]
        .as_str()
        .ok_or_else(conflict)?;
    let raw_value: Value = serde_json::from_str(raw_args).map_err(|_| conflict())?;
    let persisted = call["function"]["arguments"]
        .as_str()
        .ok_or_else(conflict)?;
    let equivalent = safe_tool_arguments_equal(raw_args, persisted)?;
    if call["function"]["name"] != name
        || !equivalent
        || approval::args_digest(&raw_value)? != approval::args_digest(args)?
    {
        return Err(conflict().into());
    }
    let (turn, session) = approval_scope(tx, &task.id)?;
    let digest = approval::binding_digest(
        &task.id,
        session.as_deref(),
        turn.as_deref(),
        call_id,
        name,
        &durable.workspace,
        &durable.spec.write_scopes,
        durable.spec.allow_commands,
        args,
    )?;
    Ok((turn, session, digest))
}

fn approval_scope(
    tx: &rusqlite::Transaction<'_>,
    task_id: &str,
) -> Result<(Option<String>, Option<String>)> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT turn_id, session_id FROM turn_tasks WHERE legacy_task_id=?1",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((turn_id, session_id)) => (Some(turn_id), Some(session_id)),
        None => (None, None),
    })
}

fn safe_approval_call_digest(
    tx: &rusqlite::Transaction<'_>,
    task: &Task,
    record: &approval::ApprovalRecord,
) -> Result<String> {
    use crate::tool_results::ToolResultError;
    let (turn, session) = approval_scope(tx, &task.id)?;
    let call = task
        .messages
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .find(|call| call["id"] == record.tool_call_id)
        .ok_or(ToolResultError::Conflict)?;
    let args: Value = serde_json::from_str(
        call["function"]["arguments"]
            .as_str()
            .ok_or(ToolResultError::Corrupt)?,
    )
    .map_err(|_| ToolResultError::Corrupt)?;
    approval::args_digest(&json!({
        "version": "approval-safe-call-v1",
        "approval_id": record.id,
        "task_id": task.id,
        "run_id": task.run_id,
        "turn_id": turn,
        "session_id": session,
        "tool_call_id": record.tool_call_id,
        "tool_name": record.tool_name,
        "safe_args": args
    }))
}

fn validate_prior_groups(messages: &[Value]) -> Result<()> {
    use crate::tool_results::ToolResultError;
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        if let Some(calls) = message.get("tool_calls") {
            let calls = calls.as_array().ok_or(ToolResultError::Corrupt)?;
            if message["role"] != "assistant" || calls.is_empty() {
                return Err(ToolResultError::Corrupt.into());
            }
            for call in calls {
                index += 1;
                let answer = messages.get(index).ok_or(ToolResultError::Conflict)?;
                if answer["role"] != "tool" || answer["tool_call_id"] != call["id"] {
                    return Err(ToolResultError::Conflict.into());
                }
            }
        } else if message["role"] == "tool" {
            return Err(ToolResultError::Corrupt.into());
        }
        index += 1;
    }
    Ok(())
}

fn validate_owned_result_events(messages: &[Value], events: &[Value]) -> Result<()> {
    use crate::tool_results::ToolResultError;
    let answers: Vec<_> = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect();
    if answers.len() != events.len() {
        return Err(ToolResultError::Conflict.into());
    }
    for (answer, event) in answers.iter().zip(events) {
        let id = answer["tool_call_id"]
            .as_str()
            .ok_or(ToolResultError::Corrupt)?;
        let call = messages
            .iter()
            .filter_map(|m| m["tool_calls"].as_array())
            .flatten()
            .find(|call| call["id"] == id)
            .ok_or(ToolResultError::Corrupt)?;
        let content = answer["content"].as_str().ok_or(ToolResultError::Corrupt)?;
        let result = tool_result_value(content);
        if event.get("result").is_none()
            || event["name"] != call["function"]["name"]
            || event["result"] != result
            || event.get("tool_call_id").is_some_and(|value| value != id)
        {
            return Err(ToolResultError::Corrupt.into());
        }
        if event.get("origin").is_some() || event.get("origin_version").is_some() {
            let name = call["function"]["name"]
                .as_str()
                .ok_or(ToolResultError::Corrupt)?;
            let valid_semantics = match event["origin"].as_str() {
                Some("read_completed") => is_readonly_file_tool(name),
                Some("agent_denied") => result == json!({"error":"blocked_by_agent_policy"}),
                Some("host_denied") => result.is_string(),
                Some("approval_denied") => {
                    result == Value::String(approval::tool_error(approval::ApprovalStatus::Denied))
                }
                Some("approval_cancelled") => {
                    result
                        == Value::String(approval::tool_error(approval::ApprovalStatus::Cancelled))
                }
                Some("interrupted_without_claim") => {
                    result
                        == Value::String(
                            "Execution was interrupted. Inspect files before repeating a write or command.".into(),
                        )
                }
                _ => false,
            };
            if !valid_semantics {
                return Err(ToolResultError::Corrupt.into());
            }
        }
    }
    Ok(())
}

fn inherited_message_count(
    tx: &rusqlite::Transaction<'_>,
    task: &Task,
    turn_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<usize> {
    use crate::tool_results::ToolResultError;
    let (Some(turn_id), Some(session_id)) = (turn_id, session_id) else {
        return Ok(0);
    };
    let repo = repository::Repository::new(tx);
    let session = repo.session(&SessionId(session_id.to_owned()))?;
    let turn = repo.turn(&TurnId(turn_id.to_owned()))?;
    if session.legacy_run_id != task.run_id
        || turn.session_id != session.id
        || turn.project_id != session.project_id
    {
        return Err(ToolResultError::Corrupt.into());
    }
    if session.kind != SessionKind::Chat {
        return Ok(0);
    }
    let previous: Option<(String, String)> = tx
        .query_row(
            "SELECT tt.legacy_task_id,tt.turn_id FROM turn_tasks tt \
             JOIN events e ON e.turn_id=tt.turn_id AND e.session_id=tt.session_id \
             WHERE tt.session_id=?1 AND tt.turn_id<>?2 \
             GROUP BY tt.turn_id,tt.legacy_task_id \
             HAVING MIN(e.seq)<(SELECT MIN(seq) FROM events WHERE turn_id=?2 AND session_id=?1) \
             ORDER BY MIN(e.seq) DESC LIMIT 1",
            params![session_id, turn_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((previous, _)) = previous else {
        return Ok(0);
    };
    let predecessor = legacy_task_row(tx, &previous)?;
    let closed_pred = close_dangling_calls(&predecessor.messages);
    if task.messages.starts_with(&closed_pred) {
        Ok(closed_pred.len())
    } else if task.messages.starts_with(&predecessor.messages) {
        Ok(predecessor.messages.len())
    } else {
        Ok(0)
    }
}

fn insert_approval(
    tx: &rusqlite::Transaction<'_>,
    record: &approval::ApprovalRecord,
) -> Result<()> {
    secrets::validate_persisted_id("approval_id", &record.id)?;
    secrets::validate_persisted_id("tool_call_id", &record.tool_call_id)?;
    secrets::validate_persisted_id("approval_task_id", &record.task_id)?;
    tx.execute(
        "INSERT INTO approvals(id,tool_call_id,task_id,turn_id,session_id,tool_name,args_digest,binding_digest,workspace,write_scopes,allow_commands,preview,status,execution_state,created_at,decided_at,decided_by) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            record.id,
            record.tool_call_id,
            record.task_id,
            record.turn_id,
            record.session_id,
            record.tool_name,
            record.args_digest,
            record.binding_digest,
            record.workspace,
            serde_json::to_string(&record.write_scopes)?,
            record.allow_commands as i64,
            record.preview,
            record.status.as_str(),
            record.execution_state.as_str(),
            record.created_at,
            record.decided_at,
            record.decided_by
        ],
    )?;
    Ok(())
}

fn approval_event(
    tx: &rusqlite::Transaction<'_>,
    record: &approval::ApprovalRecord,
    kind: &str,
    data: Value,
) -> Result<()> {
    let mut event = stamped_event(
        0,
        record.session_id.as_deref().unwrap_or(""),
        record.turn_id.as_deref(),
        &record.task_id,
        kind,
        secrets::redact_persisted(&data),
        now(),
    );
    event.session_id = event.session_id.filter(|id| !id.is_empty());
    repository::Repository::append_event(tx, &event)?;
    Ok(())
}

fn legacy_task_row(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<Task> {
    let (run_id, value): (String, String) = tx
        .query_row("SELECT run_id,value FROM tasks WHERE id=?1", [id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .context("任务不存在")?;
    let task: Task = serde_json::from_str(&value)?;
    anyhow::ensure!(
        task.id == id && task.run_id == run_id,
        "旧任务 JSON 身份与关系列不一致"
    );
    Ok(task)
}

fn ensure_resume_targets_latest_chat_turn(
    tx: &rusqlite::Transaction<'_>,
    legacy_task_id: &str,
) -> Result<()> {
    let repo = repository::Repository::new(tx);
    let mapped: Option<(String, String)> = tx
        .query_row(
            "SELECT turn_id,session_id FROM turn_tasks WHERE legacy_task_id=?1",
            [legacy_task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((turn_id, session_id)) = mapped else {
        return Ok(());
    };
    let session = repo.session(&SessionId(session_id))?;
    if session.kind != SessionKind::Chat {
        return Ok(());
    }
    let latest = repo
        .latest_committed_turn(&session.id)?
        .ok_or(ChatTurnError::NotFound)?;
    if latest.id.0 != turn_id {
        return Err(ChatTurnError::StalePredecessor.into());
    }
    Ok(())
}

pub(crate) fn task_event_tx(
    tx: &rusqlite::Transaction<'_>,
    task_id: &str,
    kind: &str,
    data: Value,
) -> Result<()> {
    let event = task_event_envelope(tx, task_id, kind, secrets::redact_persisted(&data))?;
    repository::Repository::append_event(tx, &event)?;
    Ok(())
}

fn task_event_envelope(
    tx: &rusqlite::Transaction<'_>,
    task_id: &str,
    kind: &str,
    data: Value,
) -> Result<Event> {
    let repo = repository::Repository::new(tx);
    let session = repo.session_by_legacy_run_of_task(task_id)?;
    let turn_id: Option<String> = tx
        .query_row(
            "SELECT turn_id FROM turn_tasks WHERE legacy_task_id=?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?;
    let mut event = stamped_event(
        0,
        session.as_ref().map(|s| s.id.0.as_str()).unwrap_or(""),
        turn_id.as_deref(),
        task_id,
        kind,
        data,
        now(),
    );
    event.session_id = event.session_id.filter(|id| !id.is_empty());
    Ok(event)
}

fn set_turn_status_tx(
    tx: &rusqlite::Transaction<'_>,
    turn: &TurnId,
    status: LifecycleStatus,
    task_id: &str,
    at: u64,
) -> Result<()> {
    repository::Repository::new(tx).update_turn_status(turn, status, at)?;
    task_event_tx(
        tx,
        task_id,
        "turn.status",
        json!({"status":status.as_str()}),
    )
}

fn protect_running_projection(tx: &rusqlite::Transaction<'_>, task: &Task) -> Result<Task> {
    let previous = legacy_task_row(tx, &task.id)?;
    let mut protected = task.clone();
    if previous.output.len() > task.output.len() && previous.output.starts_with(&task.output) {
        protected.output = previous.output.clone();
    }
    protected.usage = merge_usage(&previous.usage, &task.usage)?;
    Ok(protected)
}

fn merge_usage(persisted: &Value, candidate: &Value) -> Result<Value> {
    if persisted == candidate {
        return Ok(persisted.clone());
    }
    if !persisted.is_object() && candidate.is_object() {
        return Ok(candidate.clone());
    }
    if !candidate.is_object() && persisted.is_object() {
        return Ok(persisted.clone());
    }
    let (Some(old), Some(new)) = (persisted.as_object(), candidate.as_object()) else {
        return Err(crate::tool_results::ToolResultError::Conflict.into());
    };
    let mut merged = old.clone();
    for key in old
        .keys()
        .chain(new.keys())
        .collect::<std::collections::HashSet<_>>()
    {
        match (old.get(key), new.get(key)) {
            (Some(a), Some(b)) if a == b => {}
            (a, b)
                if matches!(
                    key.as_str(),
                    "prompt_tokens" | "completion_tokens" | "total_tokens"
                ) =>
            {
                let old_number = a.and_then(Value::as_u64).unwrap_or(0);
                let new_number = b.and_then(Value::as_u64).unwrap_or(0);
                merged.insert(key.clone(), json!(old_number.max(new_number)));
            }
            (_, Some(value)) => {
                merged.insert(key.clone(), value.clone());
            }
            (Some(_), None) => {}
            (None, None) => {}
        }
    }
    Ok(Value::Object(merged))
}

fn save_task_tx(tx: &rusqlite::Transaction<'_>, task: &Task, resume: bool) -> Result<()> {
    secrets::validate_persisted_id("task_id", &task.id)?;
    let (run_id, previous): (String, String) = tx
        .query_row(
            "SELECT run_id,value FROM tasks WHERE id=?1",
            [&task.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("任务不存在")?;
    let previous: Task = serde_json::from_str(&previous)?;
    let mut protected = task.clone();
    if !resume && previous.status == "cancelled" {
        protected.status = "cancelled".into();
        protected.error = previous.error.clone();
    }
    let old_messages = safe_task_value(&previous)?["messages"]
        .as_array()
        .context("persisted messages are not an array")?
        .clone();
    let new_messages = safe_task_value(task)?["messages"]
        .as_array()
        .context("candidate messages are not an array")?
        .clone();
    if old_messages.starts_with(&new_messages) {
        if old_messages.len() != new_messages.len() {
            anyhow::ensure!(
                !resume && matches!(protected.status.as_str(), "failed" | "cancelled"),
                "旧任务快照不能推进状态"
            );
        }
        protected.messages = previous.messages.clone();
    } else {
        anyhow::ensure!(new_messages.starts_with(&old_messages), "任务消息前缀冲突");
    }
    let task = &protected;
    anyhow::ensure!(
        task.run_id == run_id && previous.id == task.id && previous.run_id == run_id,
        "任务身份与持久化关联不一致"
    );
    let old_value = safe_task_value(&previous)?;
    let value = safe_task_value(task)?;
    for field in ["spec", "route", "workspace", "created_at"] {
        anyhow::ensure!(
            old_value[field] == value[field],
            "任务成员、模型、路径与创建时间不可变"
        );
    }
    let old = LifecycleStatus::parse(&previous.status).context("已存任务状态无效")?;
    let next = LifecycleStatus::parse(&task.status).context("任务状态无效")?;
    let mapped: Option<(String, String, String)> = tx
        .query_row(
            "SELECT id,turn_id,status FROM turn_tasks WHERE legacy_task_id=?1",
            [&task.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((_, _, status)) = &mapped {
        anyhow::ensure!(status == old.as_str(), "新旧任务状态不一致");
    }
    if resume {
        anyhow::ensure!(
            next == LifecycleStatus::Queued
                && matches!(
                    old,
                    LifecycleStatus::Completed
                        | LifecycleStatus::Failed
                        | LifecycleStatus::Cancelled
                        | LifecycleStatus::Interrupted
                        | LifecycleStatus::AwaitingResume
                        | LifecycleStatus::Restored
                ),
            "只有结束或中断的任务可以继续"
        );
        if old != LifecycleStatus::Restored && old != LifecycleStatus::AwaitingResume {
            if let Some((id, _, _)) = &mapped {
                repository::Repository::new(tx).update_task_status(
                    &TaskId(id.clone()),
                    LifecycleStatus::AwaitingResume,
                    task.updated_at,
                )?;
            }
            task_event_tx(tx, &task.id, "status", json!({"status":"awaiting_resume"}))?;
        }
    } else {
        anyhow::ensure!(
            old.can_transition(next),
            "非法任务状态转换：{} -> {}",
            old.as_str(),
            next.as_str()
        );
    }
    tx.execute(
        "UPDATE tasks SET value=?2 WHERE id=?1",
        params![task.id, value.to_string()],
    )?;
    if let Some((id, turn_id, _)) = &mapped {
        let repo = repository::Repository::new(tx);
        repo.update_task_status(&TaskId(id.clone()), next, task.updated_at)?;
        let turn_id = TurnId(turn_id.clone());
        let current = repo.turn(&turn_id)?;
        let states: Vec<_> = repo
            .turn_tasks(&turn_id)?
            .into_iter()
            .map(|t| t.status)
            .collect();
        let aggregate = if states.contains(&LifecycleStatus::Running) {
            LifecycleStatus::Running
        } else if states.contains(&LifecycleStatus::Queued) {
            if current.status == LifecycleStatus::Running {
                LifecycleStatus::Running
            } else {
                LifecycleStatus::Queued
            }
        } else if states.contains(&LifecycleStatus::Interrupted) {
            LifecycleStatus::Interrupted
        } else if states.contains(&LifecycleStatus::Failed) {
            LifecycleStatus::Failed
        } else if states.contains(&LifecycleStatus::Cancelled) {
            LifecycleStatus::Cancelled
        } else if states.iter().all(|s| *s == LifecycleStatus::Completed) {
            LifecycleStatus::Completed
        } else {
            current.status
        };
        if aggregate != current.status {
            if resume && !current.status.can_transition(aggregate) {
                if current.status != LifecycleStatus::AwaitingResume {
                    set_turn_status_tx(
                        tx,
                        &turn_id,
                        LifecycleStatus::AwaitingResume,
                        &task.id,
                        task.updated_at,
                    )?;
                }
                set_turn_status_tx(
                    tx,
                    &turn_id,
                    LifecycleStatus::Queued,
                    &task.id,
                    task.updated_at,
                )?;
                if aggregate != LifecycleStatus::Queued {
                    set_turn_status_tx(tx, &turn_id, aggregate, &task.id, task.updated_at)?;
                }
            } else {
                set_turn_status_tx(tx, &turn_id, aggregate, &task.id, task.updated_at)?;
            }
        }
    }
    if old != next || resume {
        task_event_tx(
            tx,
            &task.id,
            "status",
            json!({"status":next.as_str(),"error":task.error}),
        )?;
    }
    Ok(())
}

fn event_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let data: String = row.get(7)?;
    let seq: i64 = row.get(0)?;
    let cursor: String = row.get(2)?;
    let data = serde_json::from_str(&data).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Event {
        schema_version: row.get(1)?,
        seq,
        cursor: if cursor.is_empty() {
            seq.to_string()
        } else {
            cursor
        },
        session_id: row.get(3)?,
        turn_id: row.get(4)?,
        task_id: row.get(5)?,
        kind: row.get(6)?,
        data,
        at: row.get(8)?,
    })
}

fn validate_turn_input(turn: &Turn, tasks: &[TurnTask], key: Option<&str>) -> Result<()> {
    anyhow::ensure!(!tasks.is_empty(), "回合至少需要一个任务");
    anyhow::ensure!(
        tasks.iter().all(|task| task.status == turn.status),
        "回合与任务初始状态必须一致"
    );
    for (field, value) in [
        ("turn_id", turn.id.0.as_str()),
        ("turn_session_id", turn.session_id.0.as_str()),
        ("turn_project_id", turn.project_id.0.as_str()),
        ("request_hash", turn.request_hash.as_str()),
    ] {
        secrets::validate_persisted_id(field, value)?;
    }
    if let Some(key) = key {
        secrets::validate_idempotency_key(key)?;
        anyhow::ensure!(
            turn.idempotency_key.as_deref() == Some(key),
            "回合中的幂等 key 必须与请求 key 一致"
        );
    } else {
        anyhow::ensure!(
            turn.idempotency_key.is_none(),
            "未提供幂等 key 时回合不能残留幂等 key"
        );
    }
    for task in tasks {
        for (field, value) in [
            ("turn_task_id", task.id.0.as_str()),
            ("turn_task_turn_id", task.turn_id.0.as_str()),
            ("turn_task_session_id", task.session_id.0.as_str()),
            ("turn_task_agent_id", task.agent_id.0.as_str()),
            ("legacy_task_id", task.legacy_task_id.as_str()),
        ] {
            secrets::validate_persisted_id(field, value)?;
        }
    }
    Ok(())
}

fn classify_both(
    repo: &repository::Repository<'_>,
    key: &str,
    request_hash: &str,
) -> Result<Option<IdempotencyReplay>> {
    let current = repo.idempotency(key, request_hash)?;
    let legacy = repo.legacy_idempotency(key, request_hash)?;
    match (current, legacy) {
        (Some(Err(())), _) | (_, Some(Err(()))) => Err(IdempotencyConflict.into()),
        (Some(Ok(turn)), Some(Ok(run_id))) => {
            let bound = repo.session(&turn.session_id)?.legacy_run_id;
            if bound != run_id {
                return Err(IdempotencyConflict.into());
            }
            Ok(Some(IdempotencyReplay::Turn(turn)))
        }
        (Some(Ok(turn)), None) => Ok(Some(IdempotencyReplay::Turn(turn))),
        (None, Some(Ok(run_id))) => Ok(Some(IdempotencyReplay::LegacyRun(
            repo.legacy_run(&run_id)?,
        ))),
        (None, None) => Ok(None),
    }
}

fn insert_legacy_run(tx: &rusqlite::Transaction<'_>, run: &Run) -> Result<()> {
    secrets::validate_persisted_id("run_id", &run.id)?;
    let title = secrets::safe_metadata_text("run_title", &run.title)?;
    tx.execute(
        "INSERT INTO runs(id,title,kind,created_at) VALUES (?1,?2,?3,?4)",
        params![run.id, title, run.kind.as_str(), run.created_at],
    )?;
    Ok(())
}

fn insert_legacy_task(tx: &rusqlite::Transaction<'_>, task: &Task) -> Result<()> {
    secrets::validate_persisted_id("task_id", &task.id)?;
    secrets::validate_persisted_id("run_id", &task.run_id)?;
    anyhow::ensure!(
        LifecycleStatus::parse(&task.status).is_some(),
        "任务状态无效"
    );
    let collisions: i64 = tx.query_row(
        "SELECT count(*) FROM turn_tasks WHERE id=?1 AND legacy_task_id<>?1",
        [&task.id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(collisions == 0, "旧任务 ID 与领域任务 ID 冲突");
    let value = safe_task_value(task)?;
    tx.execute(
        "INSERT INTO tasks(id,run_id,value) VALUES (?1,?2,?3)",
        params![task.id, task.run_id, value.to_string()],
    )?;
    Ok(())
}

pub fn fresh_settings(store: &Store, workspace: &Path) -> Result<Settings> {
    let mut db = store.db.lock().unwrap();
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    crate::host_configuration::verify_persisted_rows(&tx)?;
    if let Some(settings) = crate::host_configuration::settings_tx(&tx)? {
        return Ok(settings);
    }
    let history = crate::host_configuration::configuration_history_tx(&tx)?;
    anyhow::ensure!(
        !history && crate::host_configuration::revision_tx(&tx)? == 0,
        "fresh Host configuration requires empty history"
    );
    std::fs::create_dir_all(workspace)?;
    let settings = Settings {
        workspace: workspace.canonicalize()?.to_string_lossy().into_owned(),
        max_concurrency: 3,
        routes: vec![],
        newapi: None,
    };
    settings.validate()?;
    tx.execute(
        "INSERT INTO config(id,value) VALUES('settings',?1)",
        [serde_json::to_string(&settings)?],
    )?;
    tx.commit()?;
    Ok(settings)
}

pub fn initial_settings(store: &Store, legacy: &Path, workspace: &Path) -> Result<Settings> {
    std::fs::create_dir_all(workspace)?;
    let mut settings = Settings {
        workspace: workspace.canonicalize()?.to_string_lossy().into(),
        max_concurrency: 3,
        routes: vec![],
        newapi: None,
    };
    let yaml = std::fs::read_to_string(legacy.join("settings.yaml")).unwrap_or_default();
    let source: Value = serde_yaml::from_str(&yaml).unwrap_or(Value::Null);
    let creds: Value = serde_yaml::from_str(
        &std::fs::read_to_string(legacy.join(".credentials.yaml")).unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    if let Some(providers) = source
        .pointer("/llm-pi-ai/providers")
        .and_then(Value::as_object)
    {
        for (id, provider) in providers {
            let route = Route {
                id: id.clone(),
                name: provider["displayName"].as_str().unwrap_or(id).into(),
                base_url: provider["baseURL"]
                    .as_str()
                    .unwrap_or("https://xpeach.codes/v1")
                    .into(),
                model: provider
                    .pointer("/models/0/id")
                    .and_then(Value::as_str)
                    .unwrap_or("gpt-5.6-sol")
                    .into(),
                max_tokens: 4096,
                parallel_limit: 1,
                key_env: provider["apiKeyEnv"].as_str().map(String::from),
            };
            if let Some(secret) = route
                .key_env
                .as_ref()
                .and_then(|name| creds["refs"][name].as_str())
                .filter(|s| !s.is_empty())
            {
                store.put_secret(&route.id, secret)?;
            }
            settings.routes.push(route);
        }
    }
    if settings.routes.is_empty() {
        for n in 1..=3 {
            settings.routes.push(Route {
                id: format!("coda-key-{n}"),
                name: format!("Coda Key {n}"),
                base_url: "https://xpeach.codes/v1".into(),
                model: "gpt-5.6-sol".into(),
                max_tokens: 4096,
                parallel_limit: 1,
                key_env: Some(format!("CODA_KEY_{n}")),
            });
        }
    }
    if let Ok(text) = std::fs::read_to_string(legacy.join("newapi-account.yaml")) {
        let account: Value = serde_yaml::from_str(&text)?;
        if let (Some(base), Some(token_ref)) =
            (account["baseURL"].as_str(), account["tokenRef"].as_str())
        {
            let user_id = account["userId"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| account["userId"].to_string());
            if let Some(token) = creds["refs"][token_ref].as_str() {
                store.put_secret("newapi-account", token)?;
                settings.newapi = Some(Account {
                    base_url: base.into(),
                    user_id,
                    quota_per_unit: 500_000.0,
                });
            }
        }
    }
    settings.validate()?;
    store.save_settings(&settings)?;
    Ok(settings)
}

fn checkpoint_sources(db: &Connection, task_id: &str) -> Result<Vec<PreparedChange>> {
    let mut stmt = db.prepare(
        "SELECT id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,before_blob,before_digest,after_digest,state,restore_state \
         FROM workspace_changes WHERE task_id=?1 ORDER BY rowid",
    )?;
    Ok(stmt
        .query_map([task_id], workspace_change_row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn verify_checkpoint_sources(
    db: &Connection,
    manifest: &crate::checkpoint::Manifest,
) -> Result<Vec<PreparedChange>> {
    use crate::checkpoint::CheckpointError;
    let value: String = db
        .query_row(
            "SELECT value FROM tasks WHERE id=?1",
            [&manifest.task_id],
            |r| r.get(0),
        )
        .map_err(|_| CheckpointError::Corrupt)?;
    let task: Task = serde_json::from_str(&value).map_err(|_| CheckpointError::Corrupt)?;
    let mut records =
        checkpoint_sources(db, &manifest.task_id).map_err(|_| CheckpointError::Corrupt)?;
    if crate::checkpoint::source_digest(&records)? != manifest.source_digest {
        let mut statement = db.prepare(
            "SELECT id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,before_blob,before_digest,after_digest,state,restore_state \
             FROM workspace_changes WHERE task_id=?1 ORDER BY created_at,rowid",
        )?;
        records = statement
            .query_map([&manifest.task_id], workspace_change_row)?
            .collect::<rusqlite::Result<_>>()?;
    }
    if task.id != manifest.task_id
        || task.workspace != manifest.workspace
        || task.spec.write_scopes != manifest.write_scopes
        || crate::checkpoint::source_digest(&records)? != manifest.source_digest
    {
        return Err(CheckpointError::Corrupt.into());
    }
    Ok(records)
}

fn read_checkpoint(
    db: &Connection,
    id: &str,
) -> Result<(crate::checkpoint::Checkpoint, crate::checkpoint::Manifest)> {
    use crate::checkpoint::{Checkpoint, CheckpointError, Manifest};
    let row: Option<(String, String, u32, u64, String, String, String)> = db
        .query_row(
            "SELECT task_id,kind,generation,created_at,manifest_digest,manifest,creation_key FROM checkpoints WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(|_| CheckpointError::Corrupt)?;
    let (task_id, kind, generation, created_at, manifest_digest, serialized, key) =
        row.ok_or(CheckpointError::NotFound)?;
    let manifest: Manifest =
        serde_json::from_str(&serialized).map_err(|_| CheckpointError::Corrupt)?;
    if uuid::Uuid::parse_str(id).is_err()
        || kind != "task_before"
        || generation != 1
        || digest(serialized.as_bytes()) != manifest_digest
        || task_id != manifest.task_id
        || manifest.entries.is_empty()
        || manifest.entries.len() > 128
        || secrets::validate_idempotency_key(&key).is_err()
    {
        return Err(CheckpointError::Corrupt.into());
    }
    let records = verify_checkpoint_sources(db, &manifest)?;
    let mut expected = std::collections::BTreeMap::new();
    for record in records.iter().filter(|r| r.state == ChangeState::Finished) {
        let after = record
            .after_digest
            .clone()
            .filter(|s| valid_digest(s))
            .ok_or(CheckpointError::Corrupt)?;
        let entry =
            expected
                .entry(record.path_key.clone())
                .or_insert(crate::checkpoint::CheckpointEntry {
                    path: record.path.clone(),
                    before_digest: record.before_digest.clone(),
                    after_digest: after.clone(),
                });
        entry.after_digest = after;
    }
    if expected.into_values().collect::<Vec<_>>() != manifest.entries {
        return Err(CheckpointError::Corrupt.into());
    }
    Ok((
        Checkpoint {
            checkpoint_id: id.into(),
            task_id,
            kind,
            generation,
            created_at,
            manifest_digest,
            entries: manifest.entries.clone(),
        },
        manifest,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_route() -> Route {
        Route {
            id: "r1".into(),
            name: "Route 1".into(),
            base_url: "http://127.0.0.1:8080/v1".into(),
            model: "test-model".into(),
            max_tokens: 512,
            parallel_limit: 1,
            key_env: None,
        }
    }

    fn sample_task(id: &str, run_id: &str, workspace: &str, prompt: &str) -> Task {
        Task {
            id: id.into(),
            run_id: run_id.into(),
            spec: TaskSpec {
                name: "coder".into(),
                role: "engineer".into(),
                route_id: "r1".into(),
                prompt: prompt.into(),
                depends_on: vec![],
                write_scopes: vec!["*".into()],
                tools: true,
                allow_commands: true,
                max_rounds: 8,
            },
            route: sample_route(),
            workspace: workspace.into(),
            status: "queued".into(),
            output: String::new(),
            error: None,
            messages: vec![json!({"role":"user","content":prompt})],
            usage: Value::Null,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn direct_crud_recovery_and_chat_continuation_after_interruption() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        let ws_str = ws.canonicalize().unwrap().to_string_lossy().into_owned();

        let store = Store::open(&dir.path().join("state.sqlite")).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let project = Project {
            id: ProjectId("proj1".into()),
            name: "Workspace".into(),
            root_path: ws_str.clone(),
            created_at: 1,
            updated_at: 1,
        };
        let session = Session {
            id: SessionId("sess1".into()),
            project_id: project.id.clone(),
            kind: SessionKind::Chat,
            title: "Chat".into(),
            legacy_run_id: "run1".into(),
            created_at: 1,
            updated_at: 1,
        };
        let agent = Agent {
            id: AgentId("agent1".into()),
            session_id: session.id.clone(),
            display_name: "coder".into(),
            role: "engineer".into(),
            created_at: 1,
        };
        let mut legacy1 = sample_task("t1", "run1", &ws_str, "first turn");
        legacy1.messages.push(json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": { "name": "edit_file", "arguments": "{\"path\":\"a.rs\",\"old_string\":\"x\",\"new_string\":\"y\"}" }
            }]
        }));
        let ttask1 = TurnTask {
            id: TaskId("t1".into()),
            turn_id: TurnId("turn1".into()),
            session_id: session.id.clone(),
            agent_id: agent.id.clone(),
            legacy_task_id: "t1".into(),
            depends_on: vec![],
            status: LifecycleStatus::Queued,
            created_at: 1,
            updated_at: 1,
        };
        let turn1 = Turn {
            id: TurnId("turn1".into()),
            session_id: session.id.clone(),
            project_id: project.id.clone(),
            status: LifecycleStatus::Queued,
            request_hash: "h1".into(),
            idempotency_key: Some("k1".into()),
            created_at: 1,
            updated_at: 1,
        };
        store
            .commit_turn_bundle(
                TurnBundle {
                    project: &project,
                    session: &session,
                    agents: std::slice::from_ref(&agent),
                    turn: &turn1,
                    tasks: std::slice::from_ref(&ttask1),
                    legacy_tasks: std::slice::from_ref(&legacy1),
                },
                Some("k1"),
            )
            .unwrap();

        // Simulate crash recovery without sealing task in 'unknown'
        let affected = store.recover().unwrap();
        assert!(affected >= 1);
        assert_eq!(store.task("t1").unwrap().status, "interrupted");
        assert_eq!(
            store.turn(&TurnId("turn1".into())).unwrap().status,
            LifecycleStatus::Interrupted
        );

        // Append a new chat turn directly after the interrupted turn!
        let persisted1 = store.task("t1").unwrap();
        let mut prefix = store
            .chat_continuation_prefix(&session.id, &persisted1)
            .unwrap();
        prefix.push(json!({"role":"user","content":"continue please"}));
        let mut legacy2 = sample_task("t2", "run1", &ws_str, "continue please");
        legacy2.messages = prefix;
        let ttask2 = TurnTask {
            id: TaskId("t2".into()),
            turn_id: TurnId("turn2".into()),
            session_id: session.id.clone(),
            agent_id: agent.id.clone(),
            legacy_task_id: "t2".into(),
            depends_on: vec![],
            status: LifecycleStatus::Queued,
            created_at: 2,
            updated_at: 2,
        };
        let turn2 = Turn {
            id: TurnId("turn2".into()),
            session_id: session.id.clone(),
            project_id: project.id.clone(),
            status: LifecycleStatus::Queued,
            request_hash: "h2".into(),
            idempotency_key: Some("k2".into()),
            created_at: 2,
            updated_at: 2,
        };
        let receipt = store
            .append_chat_turn(&turn2, &ttask2, &legacy2, "k2", &TurnId("turn1".into()))
            .unwrap();
        assert!(!receipt.replayed);
        assert_eq!(receipt.turn.id, TurnId("turn2".into()));
    }
}
