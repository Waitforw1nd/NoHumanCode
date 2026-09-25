use crate::{approval, domain::*, repository, secrets, workspace_changes::*};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::{io::Write, path::Path, sync::Mutex};

pub const SCHEMA_VERSION: u32 = 8;

#[derive(Debug)]
pub struct IdempotencyConflict;
impl std::fmt::Display for IdempotencyConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("同一个 Idempotency-Key 不能用于不同请求或对象")
    }
}
impl std::error::Error for IdempotencyConflict {}

pub struct Store {
    db: Mutex<Connection>,
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
    pub fn open(path: &Path) -> Result<Self> {
        let mut db = Connection::open(path).context("无法打开任务数据库")?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
          CREATE TABLE IF NOT EXISTS config (id TEXT PRIMARY KEY, value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS secrets (id TEXT PRIMARY KEY, value BLOB NOT NULL);
          CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS schema_migrations (id TEXT PRIMARY KEY, applied_at INTEGER NOT NULL);
          CREATE INDEX IF NOT EXISTS tasks_run ON tasks(run_id);
          CREATE INDEX IF NOT EXISTS events_task ON events(task_id, seq);")?;
        let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        anyhow::ensure!(
            version <= SCHEMA_VERSION,
            "任务数据库版本 {} 高于当前程序支持的版本 {}",
            version,
            SCHEMA_VERSION
        );
        if version < 2 {
            db.execute_batch("CREATE TABLE IF NOT EXISTS idempotency (key TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), created_at INTEGER NOT NULL);
              CREATE INDEX IF NOT EXISTS idempotency_run ON idempotency(run_id);")?;
        }
        if version < 3 {
            db.execute_batch("ALTER TABLE idempotency ADD COLUMN request_hash TEXT NOT NULL DEFAULT ''; PRAGMA user_version=3;")?;
        }
        if version < 4 {
            db.execute_batch("ALTER TABLE runs ADD COLUMN kind TEXT NOT NULL DEFAULT 'team'; PRAGMA user_version=4;")?;
        }
        if version < 5 {
            // Persist the session kind for old chat runs once so history
            // routing does not depend on a member name after migration. New
            // requests never use this compatibility path.
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
        // Marker, not user_version, decides whether schema 6 still has to
        // run.  A crash after DDL but before the marker retries the
        // idempotent statements and only then advances user_version.
        if !repository::migration_applied(&db, repository::MIGRATION_ID)? {
            let tx = db.transaction()?;
            repository::apply_schema_6(&tx)?;
            tx.commit()?;
        } else {
            anyhow::ensure!(
                (6..=SCHEMA_VERSION).contains(&version),
                "迁移标记与数据库版本不一致"
            );
            let tx = db.transaction()?;
            repository::verify_current_schema(&tx)?;
            tx.commit()?;
        }
        if !repository::migration_applied(&db, repository::APPROVAL_MIGRATION_ID)? {
            let tx = db.transaction()?;
            repository::apply_schema_7(&tx)?;
            tx.commit()?;
        } else {
            let current: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
            anyhow::ensure!(
                (7..=SCHEMA_VERSION).contains(&current),
                "审批迁移标记与数据库版本不一致"
            );
            let tx = db.transaction()?;
            repository::verify_schema_7(&tx)?;
            tx.commit()?;
        }
        if !repository::migration_applied(&db, repository::WORKSPACE_CHANGE_MIGRATION_ID)? {
            let tx = db.transaction()?;
            repository::apply_schema_8(&tx)?;
            tx.commit()?;
        } else {
            let current: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
            anyhow::ensure!(
                current == SCHEMA_VERSION,
                "文件变更迁移标记与数据库版本不一致"
            );
            let tx = db.transaction()?;
            repository::verify_schema_8(&tx)?;
            tx.commit()?;
        }
        Ok(Self { db: Mutex::new(db) })
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
        self.db.lock().unwrap().execute("INSERT INTO config VALUES ('settings',?1) ON CONFLICT(id) DO UPDATE SET value=excluded.value", [serde_json::to_string(value)?])?;
        Ok(())
    }
    pub fn put_secret(&self, id: &str, secret: &str) -> Result<()> {
        let bytes = secrets::seal(secret.as_bytes())?;
        self.db.lock().unwrap().execute(
            "INSERT INTO secrets VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET value=excluded.value",
            params![id, bytes],
        )?;
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
        let tx = db.transaction()?;
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
        let tx = db.transaction()?;
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
        let tx = db.transaction()?;
        save_task_tx(&tx, task, false)?;
        tx.commit()?;
        Ok(())
    }
    pub fn resume_task(&self, task: &Task, input: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_resume_targets_latest_chat_turn(&tx, &task.id)?;
        save_task_tx(&tx, task, true)?;
        task_event_tx(&tx, &task.id, "delta", json!({"text": "\n\n—— 继续 ——\n"}))?;
        task_event_tx(&tx, &task.id, "resume", json!({"message": input}))?;
        tx.commit()?;
        Ok(())
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
        Ok(stmt.query_map([], |r| Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"created_at":r.get::<_,u64>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn event(&self, task_id: &str, kind: &str, data: Value) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        // Status events are projections of durable state, never a second
        // unguarded state mutation path.
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
            let status: String = tx.query_row("SELECT t.status FROM turns t JOIN turn_tasks tt ON tt.turn_id=t.id WHERE tt.legacy_task_id=?1", [task_id], |row| row.get(0))?;
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

    pub fn insert_project(&self, project: &Project) -> Result<()> {
        secrets::validate_persisted_id("project_id", &project.id.0)?;
        secrets::safe_metadata_text("project_name", &project.name)?;
        secrets::safe_metadata_path("project_path", &project.root_path)?;
        repository::Repository::new(&self.db.lock().unwrap()).insert_project(project)
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
        repository::Repository::new(&self.db.lock().unwrap()).insert_session(session)
    }

    pub fn session(&self, id: &SessionId) -> Result<Session> {
        repository::Repository::new(&self.db.lock().unwrap()).session(id)
    }

    pub fn insert_agent(&self, agent: &Agent) -> Result<()> {
        secrets::validate_persisted_id("agent_id", &agent.id.0)?;
        secrets::validate_persisted_id("agent_session_id", &agent.session_id.0)?;
        secrets::safe_metadata_text("agent_display_name", &agent.display_name)?;
        secrets::safe_metadata_text("agent_role", &agent.role)?;
        repository::Repository::new(&self.db.lock().unwrap()).insert_agent(agent)
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

    /// Persist a turn, its tasks, the legacy run projection, and the
    /// idempotency row in one transaction.  `key` is `None` when the caller
    /// did not send an Idempotency-Key.
    pub fn commit_turn(
        &self,
        turn: &Turn,
        tasks: &[TurnTask],
        key: Option<&str>,
        legacy_tasks: &[Task],
    ) -> Result<Turn> {
        validate_turn_input(turn, tasks, key)?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        if let Some(existing) = self.commit_turn_in_tx(&tx, turn, tasks, key, legacy_tasks)? {
            return Ok(existing);
        }
        tx.commit()?;
        Ok(turn.clone())
    }

    pub fn commit_turn_bundle(&self, bundle: TurnBundle<'_>, key: Option<&str>) -> Result<Turn> {
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
        anyhow::ensure!(
            legacy_tasks
                .iter()
                .all(|task| legacy_ids.contains(task.id.as_str())),
            "旧任务必须对应回合任务的 legacy_task_id"
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

    /// None identifies a legacy-only task; Some uses the persisted ID graph.
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
        let mut stmt = db.prepare("SELECT t.value FROM turn_task_dependencies d JOIN turn_tasks tt ON tt.id=d.depends_on_task_id JOIN tasks t ON t.id=tt.legacy_task_id WHERE d.task_id=?1 ORDER BY d.depends_on_task_id")?;
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

    /// Commit order of a session's turns, not `created_at,id` order.
    pub fn latest_committed_turn(&self, id: &SessionId) -> Result<Option<Turn>> {
        repository::Repository::new(&self.db.lock().unwrap()).latest_committed_turn(id)
    }

    /// Append one tool-free chat turn to an existing session.
    ///
    /// `request_hash` must already be the stable send-turn digest. This command
    /// classifies that key, rechecks the committed predecessor, and inserts the
    /// new turn, both task projections, the idempotency rows, and the opening
    /// event in one immediate transaction. It never launches a model call.
    ///
    /// A matching replay returns the original turn even after a later turn
    /// exists. A mismatched key or a legacy-only binding is
    /// [`IdempotencyConflict`]. A stale or busy predecessor is [`ChatTurnError`].
    pub fn append_chat_turn(
        &self,
        turn: &Turn,
        task: &TurnTask,
        legacy_task: &Task,
        key: &str,
        expected_last_turn_id: &TurnId,
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
        if latest.status != LifecycleStatus::Completed {
            return Err(ChatTurnError::SessionBusy.into());
        }
        let previous_tasks = repo.turn_tasks(&latest.id)?;
        let previous = match previous_tasks.as_slice() {
            [only] if only.depends_on.is_empty() => only,
            _ => return Err(ChatTurnError::UnsupportedSession.into()),
        };
        if previous.status != LifecycleStatus::Completed || previous.agent_id != task.agent_id {
            return Err(if previous.agent_id != task.agent_id {
                ChatTurnError::UnsupportedSession
            } else {
                ChatTurnError::SessionBusy
            }
            .into());
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
        if !tool_free_chat_task(&previous_legacy)
            || previous_legacy.run_id != session.legacy_run_id
            || !tool_free_chat_prefix(&previous_legacy.messages)
        {
            return Err(ChatTurnError::UnsupportedSession.into());
        }
        let project = repo.project(&session.project_id)?;
        if !same_project_path(&previous_legacy.workspace, &project.root_path) {
            return Err(ChatTurnError::UnsupportedSession.into());
        }
        if !candidate_continues_snapshot(
            &previous_legacy,
            legacy_task,
            &command_message(legacy_task),
        ) {
            return Err(ChatTurnError::PredecessorChanged.into());
        }
        if !tool_free_chat_task(legacy_task)
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
        tx.commit()?;
        Ok(ChatTurnReceipt {
            turn: turn.clone(),
            task: task.clone(),
            replayed: false,
        })
    }

    /// New records win.  A key that exists only in the legacy table returns
    /// `LegacyRun`; it is not invented as a Turn and it is not reported as
    /// missing.  A different digest is always a conflict.
    pub fn idempotency_replay(
        &self,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<IdempotencyReplay>> {
        let db = self.db.lock().unwrap();
        let repo = repository::Repository::new(&db);
        let classified = classify_both(&repo, key, request_hash);
        drop(db);
        classified
    }

    /// Compatibility wrapper.  A legacy-only key is an error here because this
    /// method cannot safely return a Turn for it.
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
            approval.tool_name == "write_file" && approval.workspace == task.workspace,
            "write_file 审批绑定不匹配"
        );
        let completed: i64 = tx.query_row(
            "SELECT count(*) FROM workspace_restores WHERE task_id=?1",
            [&task.id],
            |row| row.get(0),
        )?;
        if completed != 0 {
            return Err(WorkspaceChangeError::Unrestorable.into());
        }
        let uncertain: i64 = tx.query_row(
            "SELECT count(*) FROM workspace_changes WHERE task_id=?1 AND state IN ('prepared','unknown')",
            [&task.id], |row| row.get(0),
        )?;
        if uncertain != 0 {
            return Err(WorkspaceChangeError::Unknown { receipt: None }.into());
        }
        let latest: Option<String> = tx.query_row(
            "SELECT after_digest FROM workspace_changes WHERE task_id=?1 AND path_key=?2 AND state='finished' ORDER BY created_at DESC, rowid DESC LIMIT 1",
            params![task.id, path_key], |row| row.get(0)).optional()?;
        if let Some(expected) = latest {
            let actual = before
                .map(digest)
                .ok_or_else(|| WorkspaceChangeError::Conflict { receipt: None })?;
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
            "INSERT INTO workspace_changes(id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,before_blob,before_digest,after_digest,state,restore_state,created_at,finished_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL,'prepared','pending',?12,NULL)",
            params![id, task.id, tool_call_id, workspace_digest, approval.binding_digest, write_scopes, path, path_key, if kind == ChangeKind::Created {"created"} else {"modified"}, before_blob, before_digest, now()])?;
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
        db.query_row("SELECT id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,before_blob,before_digest,after_digest,state,restore_state FROM workspace_changes WHERE id=?1", [id], workspace_change_row).map_err(Into::into)
    }

    pub(crate) fn workspace_changes_for_task(&self, task_id: &str) -> Result<Vec<PreparedChange>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,before_blob,before_digest,after_digest,state,restore_state FROM workspace_changes WHERE task_id=?1 ORDER BY created_at,rowid")?;
        Ok(stmt
            .query_map([task_id], workspace_change_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn mark_workspace_change_failed(&self, id: &str) -> Result<()> {
        self.db.lock().unwrap().execute("UPDATE workspace_changes SET state='failed',finished_at=?1 WHERE id=?2 AND state='prepared'", params![now(), id])?;
        Ok(())
    }

    pub(crate) fn mark_workspace_change_unknown(&self, id: &str) -> Result<()> {
        self.db.lock().unwrap().execute("UPDATE workspace_changes SET state='unknown',finished_at=?1 WHERE id=?2 AND state='prepared'", params![now(), id])?;
        Ok(())
    }

    pub(crate) fn latest_restore(&self, task_id: &str) -> Result<Option<RestoreReceipt>> {
        let db = self.db.lock().unwrap();
        let id: Option<String> = db.query_row("SELECT id FROM workspace_restores WHERE task_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1", [task_id], |row| row.get(0)).optional()?;
        id.map(|id| restore_receipt(&db, &id)).transpose()
    }

    pub(crate) fn claim_restore(
        &self,
        task_id: &str,
        changes: &[PreparedChange],
    ) -> Result<RestoreReceipt> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(id) = tx.query_row("SELECT id FROM workspace_restores WHERE task_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1", [task_id], |row| row.get::<_, String>(0)).optional()? {
            return restore_receipt(&tx, &id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO workspace_restores(id,task_id,status,created_at,finished_at) VALUES (?1,?2,'claimed',?3,NULL)", params![id,task_id,now()])?;
        for change in changes {
            tx.execute("INSERT INTO workspace_restore_outcomes(restore_id,change_id,path,status,updated_at) VALUES (?1,?2,?3,'claimed',?4)", params![id,change.id,change.path,now()])?;
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
        let tx = db.transaction()?;
        let changed = tx.execute("UPDATE workspace_restore_outcomes SET status='complete',updated_at=?1 WHERE restore_id=?2 AND change_id=?3 AND status='claimed'", params![now(),restore_id,change_id])?;
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
        let tx = db.transaction()?;
        let changed = tx.execute("UPDATE workspace_restores SET status=?1,finished_at=?2 WHERE id=?3 AND status='claimed'", params![status,now(),restore_id])?;
        anyhow::ensure!(changed == 1, "恢复状态竞争失败");
        if status != "complete" {
            tx.execute("UPDATE workspace_changes SET restore_state='unknown' WHERE id IN (SELECT change_id FROM workspace_restore_outcomes WHERE restore_id=?1 AND status='claimed')", [restore_id])?;
            tx.execute("UPDATE workspace_restore_outcomes SET status='unknown',updated_at=?2 WHERE restore_id=?1 AND status='claimed'", params![restore_id,now()])?;
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
        restore_receipt(&*self.db.lock().unwrap(), id)
    }
    pub fn events(&self, run_id: &str, after: i64) -> Result<Vec<Event>> {
        let db = self.db.lock().unwrap();
        if let Some(session) = repository::Repository::new(&db).session_by_legacy_run(run_id)? {
            let events = repository::Repository::new(&db).events_after(&session.id, after)?;
            return Ok(events);
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

    /// Create a pending approval and its requested event, or return the existing
    /// row for the same tool call without emitting another request.
    pub fn ensure_approval(
        &self,
        task: &Task,
        tool_call_id: &str,
        tool_name: &str,
        args: &Value,
    ) -> Result<(approval::ApprovalRecord, bool)> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
        let record = approval::ApprovalRecord {
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
                "session_id": record.session_id
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
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current = repository::Repository::new(&tx).approval(approval_id)?;
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
                "UPDATE approvals SET status=CASE WHEN status='pending' THEN 'cancelled' ELSE status END, execution_state='cancelled', decided_at=CASE WHEN status='pending' THEN ?2 ELSE decided_at END, decided_by=CASE WHEN status='pending' THEN 'system:task-cancelled' ELSE decided_by END WHERE id=?1 AND execution_state='not_started' AND status IN ('pending','approved')",
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
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
        let changed = tx.execute(
            "UPDATE approvals SET execution_state='claimed' WHERE id=?1 AND status='approved' AND execution_state='not_started'",
            [&current.id],
        )?;
        anyhow::ensure!(changed == 1, "审批执行权竞争失败");
        let claimed = repository::Repository::new(&tx).approval(&current.id)?;
        tx.commit()?;
        Ok(claimed)
    }

    /// Persist the tool message, event and finished marker in one transaction.
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
        tx.execute("UPDATE approvals SET execution_state='finished' WHERE id=?1 AND execution_state='claimed'", [&approval_id])?;
        tx.commit()?;
        Ok(result)
    }

    /// Restart marks queued/running tasks interrupted. Pending approvals stay
    /// pending: an approval is a user decision, not a process lease.
    pub fn recover(&self) -> Result<usize> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let pending = repository::Repository::new(&tx).nonterminal_turns()?;
        let values: Vec<String> = {
            let mut stmt = tx.prepare("SELECT value FROM tasks")?;
            stmt.query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut affected = 0;
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
        let unknown = tx.execute(
            "UPDATE approvals SET execution_state='unknown' WHERE execution_state='claimed'",
            [],
        )?;
        affected += unknown;
        affected += tx.execute(
            "UPDATE workspace_changes SET state='unknown' WHERE state='prepared'",
            [],
        )?;
        affected += tx.execute(
            "UPDATE workspace_restores SET status='unknown',finished_at=?1 WHERE status='claimed'",
            [now()],
        )?;
        tx.execute("UPDATE workspace_restore_outcomes SET status='unknown',updated_at=?1 WHERE status='claimed'", [now()])?;
        for previous in pending {
            if repository::Repository::new(&tx).turn(&previous.id)?.status != previous.status {
                affected += 1;
            }
        }
        tx.commit()?;
        Ok(affected)
    }
}

fn workspace_change_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PreparedChange> {
    let kind: String = row.get(8)?;
    let state: String = row.get(12)?;
    Ok(PreparedChange {
        id: row.get(0)?,
        task_id: row.get(1)?,
        tool_call_id: row.get(2)?,
        workspace_digest: row.get(3)?,
        binding_digest: row.get(4)?,
        write_scopes: row.get(5)?,
        path: row.get(6)?,
        path_key: row.get(7)?,
        kind: if kind == "created" {
            ChangeKind::Created
        } else {
            ChangeKind::Modified
        },
        before_blob: row.get(9)?,
        before_digest: row.get(10)?,
        after_digest: row.get(11)?,
        state: match state.as_str() {
            "prepared" => ChangeState::Prepared,
            "finished" => ChangeState::Finished,
            "unknown" => ChangeState::Unknown,
            _ => ChangeState::Failed,
        },
        restore_state: row.get(13)?,
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
    let mut stmt = db.prepare("SELECT change_id,path,status FROM workspace_restore_outcomes WHERE restore_id=?1 ORDER BY rowid")?;
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

struct UnicodeStringFormatter;

impl serde_json::ser::Formatter for UnicodeStringFormatter {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> std::io::Result<()>
    where
        W: ?Sized + Write,
    {
        for unit in fragment.encode_utf16() {
            write!(writer, "\\u{unit:04x}")?;
        }
        Ok(())
    }
}

fn stable_safe_json(value: &Value) -> Result<String> {
    let compact = value.to_string();
    if secrets::redact_persisted(&Value::String(compact.clone())) == Value::String(compact.clone())
    {
        return Ok(compact);
    }
    // Encode string fragments only after structural redaction. This keeps a
    // second task-level text redaction from consuming JSON delimiters while
    // preserving the same parsed value for the event integrity check.
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, UnicodeStringFormatter);
    value.serialize(&mut serializer)?;
    let encoded = String::from_utf8(bytes)?;
    anyhow::ensure!(
        serde_json::from_str::<Value>(&encoded)? == *value
            && secrets::redact_persisted(&Value::String(encoded.clone()))
                == Value::String(encoded.clone()),
        "安全工具结果无法稳定序列化"
    );
    Ok(encoded)
}

fn command_message(task: &Task) -> String {
    task.messages
        .last()
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_owned()
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

/// The candidate must be the persisted predecessor plus exactly one user message.
/// Identity, route, workspace, and the copied prefix are compared inside the
/// append transaction so a completed resume cannot be silently dropped.
fn candidate_continues_snapshot(previous: &Task, candidate: &Task, message: &str) -> bool {
    if candidate.spec.prompt != message
        || serde_json::to_value(&candidate.route).ok() != serde_json::to_value(&previous.route).ok()
        || candidate.workspace != previous.workspace
        || candidate.spec.name != previous.spec.name
        || candidate.spec.role != previous.spec.role
        || candidate.spec.route_id != previous.spec.route_id
        || candidate.spec.max_rounds != previous.spec.max_rounds
    {
        return false;
    }
    let Some((last, prefix)) = candidate.messages.split_last() else {
        return false;
    };
    prefix == previous.messages.as_slice()
        && last["role"] == "user"
        && last["content"] == message
        && last.get("tool_calls").is_none()
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
        || !crate::workspace::definitions(&durable)
            .iter()
            .any(|t| t["function"]["name"] == name)
        || approval::evaluate(&durable, name) != approval::PolicyDecision::RequireApproval
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
    // Match the persisted redacted representation without persisting the raw
    // argument. On resume the caller supplies the redacted value; the stored
    // approval digest below will reject it if the original cannot be restored.
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
    let safe = secrets::redact_persisted(&Value::String(raw_args.into()));
    let persisted = &call["function"]["arguments"];
    let equivalent = safe == *persisted
        || match (
            safe.as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok()),
            persisted
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok()),
        ) {
            (Some(a), Some(b)) => approval::args_digest(&a)? == approval::args_digest(&b)?,
            _ => false,
        };
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

fn insert_approval(
    tx: &rusqlite::Transaction<'_>,
    record: &approval::ApprovalRecord,
) -> Result<()> {
    secrets::validate_persisted_id("approval_id", &record.id)?;
    secrets::validate_persisted_id("tool_call_id", &record.tool_call_id)?;
    secrets::validate_persisted_id("approval_task_id", &record.task_id)?;
    tx.execute(
        "INSERT INTO approvals(id,tool_call_id,task_id,turn_id,session_id,tool_name,args_digest,binding_digest,workspace,write_scopes,allow_commands,preview,status,execution_state,created_at,decided_at,decided_by) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
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

fn tool_free_chat_task(task: &Task) -> bool {
    !task.spec.tools
        && !task.spec.allow_commands
        && task.spec.write_scopes.is_empty()
        && task.spec.depends_on.is_empty()
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

/// A chat turn that already has a successor is a frozen snapshot.
/// Legacy-only tasks and non-chat sessions keep their existing resume rules.
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

fn task_event_tx(
    tx: &rusqlite::Transaction<'_>,
    task_id: &str,
    kind: &str,
    data: Value,
) -> Result<()> {
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
        secrets::redact_persisted(&data),
        now(),
    );
    event.session_id = event.session_id.filter(|id| !id.is_empty());
    repository::Repository::append_event(tx, &event)?;
    Ok(())
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
    // A stale background snapshot must never undo committed cancellation.
    // It may still append the real result of a claim which won before cancel.
    let mut cancelled_task;
    let task = if !resume && previous.status == "cancelled" {
        cancelled_task = task.clone();
        cancelled_task.status = "cancelled".into();
        cancelled_task.error = previous.error.clone();
        &cancelled_task
    } else {
        task
    };
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
        // Explicit resume is the sole terminal-to-queued bridge. Every
        // intermediate state and its event are committed with the final state.
        if old != LifecycleStatus::Restored {
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
                set_turn_status_tx(
                    tx,
                    &turn_id,
                    LifecycleStatus::AwaitingResume,
                    &task.id,
                    task.updated_at,
                )?;
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

// Read-only import. The Node installation and its session files are never rewritten.
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
                id: format!("peachsh-key-{n}"),
                name: format!("🍑 Key {n}"),
                base_url: "https://xpeach.codes/v1".into(),
                model: "gpt-5.6-sol".into(),
                max_tokens: 4096,
                parallel_limit: 1,
                key_env: Some(format!("PEACHSH_KEY_{n}")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::MIGRATION_ID;

    #[test]
    fn safe_tool_result_json_is_stable_and_roundtrips_unicode() {
        let ordinary = json!({"exit_code":0,"stdout":"ordinary output","stderr":""});
        assert_eq!(stable_safe_json(&ordinary).unwrap(), ordinary.to_string());

        let sensitive = json!({
            "exit_code": 7,
            "stdout": "bearer [redacted]\n普通🙂",
            "stderr": "ordinary failure"
        });
        let encoded = stable_safe_json(&sensitive).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), sensitive);
        assert_eq!(
            secrets::redact_persisted(&Value::String(encoded.clone())),
            Value::String(encoded)
        );

        let error = json!({"error":"path rejected"});
        let encoded = stable_safe_json(&error).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), error);
    }

    fn legacy_schema_5(path: &Path) {
        let db = Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE config (id TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE secrets (id TEXT PRIMARY KEY, value BLOB NOT NULL);
             CREATE TABLE runs (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'team');
             CREATE TABLE tasks (id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), value TEXT NOT NULL);
             CREATE TABLE events (seq INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE schema_migrations (id TEXT PRIMARY KEY, applied_at INTEGER NOT NULL);
             CREATE TABLE idempotency (key TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), created_at INTEGER NOT NULL, request_hash TEXT NOT NULL DEFAULT '');
             PRAGMA user_version=5;",
        )
        .unwrap();
        db.execute(
            "INSERT INTO schema_migrations(id,applied_at) VALUES ('session-kind-explicit',1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO runs(id,title,kind,created_at) VALUES ('legacy-run','旧运行','team',1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO tasks(id,run_id,value) VALUES ('legacy-task','legacy-run',?1)",
            [json!({"id":"legacy-task","run_id":"legacy-run","spec":{"name":"chat-session","role":"旧成员"},"status":"completed"}).to_string()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO events(task_id,kind,data,at) VALUES ('legacy-task','status','{\"status\":\"completed\"}',1)",
            [],
        )
        .unwrap();
    }

    fn sample_graph(store: &Store) -> (Project, Session, Agent, Turn, TurnTask, Task) {
        let project = Project {
            id: ProjectId::from("prj-stable"),
            name: "任意项目".into(),
            root_path: "D:/work".into(),
            created_at: 10,
            updated_at: 10,
        };
        let session = Session {
            id: SessionId::from("ses-stable"),
            project_id: project.id.clone(),
            kind: SessionKind::Chat,
            title: "chat-session".into(),
            legacy_run_id: "ses-stable".into(),
            created_at: 10,
            updated_at: 10,
        };
        let agent = Agent {
            id: AgentId::from("agt-stable"),
            session_id: session.id.clone(),
            display_name: "chat-session".into(),
            role: "对话".into(),
            created_at: 10,
        };
        let turn = Turn {
            id: TurnId::from("trn-stable"),
            session_id: session.id.clone(),
            project_id: project.id.clone(),
            status: LifecycleStatus::Queued,
            request_hash: "digest-a".into(),
            idempotency_key: Some("idem-a".into()),
            created_at: 10,
            updated_at: 10,
        };
        let task = TurnTask {
            id: TaskId::from("tsk-stable"),
            turn_id: turn.id.clone(),
            session_id: session.id.clone(),
            agent_id: agent.id.clone(),
            legacy_task_id: "legacy-task-row".into(),
            depends_on: Vec::new(),
            status: LifecycleStatus::Queued,
            created_at: 10,
            updated_at: 10,
        };
        let legacy = Task {
            id: task.legacy_task_id.clone(),
            run_id: session.legacy_run_id.clone(),
            spec: TaskSpec {
                name: "chat-session".into(),
                role: "对话".into(),
                route_id: "route".into(),
                prompt: "hello".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: false,
                allow_commands: false,
                max_rounds: 1,
            },
            route: Route {
                id: "route".into(),
                name: "route".into(),
                base_url: "https://example.invalid/v1".into(),
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 1,
                key_env: None,
            },
            workspace: "D:/work".into(),
            status: "queued".into(),
            output: String::new(),
            error: None,
            messages: vec![],
            usage: Value::Null,
            created_at: 10,
            updated_at: 10,
        };
        store.insert_project(&project).unwrap();
        store.insert_session(&session).unwrap();
        store.insert_agent(&agent).unwrap();
        let _ = store;
        (project, session, agent, turn, task, legacy)
    }

    #[test]
    fn schema_5_migrates_once_and_keeps_legacy_rows() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("v5.db");
        legacy_schema_5(&path);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let again = Store::open(&path).unwrap();
        assert_eq!(again.schema_version().unwrap(), SCHEMA_VERSION);
        let db = Connection::open(&path).unwrap();
        let markers: i64 = db
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE id=?1",
                [MIGRATION_ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(markers, 1);
        let db = Connection::open(&path).unwrap();
        let raw: String = db
            .query_row(
                "SELECT value FROM tasks WHERE id='legacy-task'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let raw: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(raw["spec"]["name"], "chat-session");
        assert_eq!(raw["status"], "completed");
        drop(db);
        let listing = store.runs().unwrap();
        assert_eq!(listing[0]["id"], "legacy-run");
        assert_eq!(listing[0]["kind"], "team");
        let events = store.events("legacy-run", 0).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cursor, events[0].seq.to_string());
        assert_eq!(events[0].schema_version, 0);
        assert!(events[0].session_id.is_none());
    }

    #[test]
    fn interrupted_schema_6_retries_without_advancing_early() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("partial.db");
        legacy_schema_5(&path);
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, root_path TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
                 INSERT INTO projects(id,name,root_path,created_at,updated_at) VALUES ('partial','x','D:/work',1,1);
                 PRAGMA user_version=5;",
            )
            .unwrap();
            let version: u32 = db
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 5);
            let marker: i64 = db
                .query_row(
                    "SELECT count(*) FROM schema_migrations WHERE id=?1",
                    [MIGRATION_ID],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(marker, 0);
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let projects = store.projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id.0, "partial");
        let _ = Store::open(&path).unwrap();
        let db = Connection::open(&path).unwrap();
        let markers: i64 = db
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE id=?1",
                [MIGRATION_ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(markers, 1);
        let version: u32 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn turn_identity_idempotency_and_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("repo.db")).unwrap();
        let (project, session, agent, turn, task, legacy) = sample_graph(&store);
        store
            .commit_turn(
                &turn,
                std::slice::from_ref(&task),
                Some("idem-a"),
                std::slice::from_ref(&legacy),
            )
            .unwrap();
        let loaded = store.turn(&turn.id).unwrap();
        assert_eq!(loaded.id, turn.id);
        assert_eq!(loaded.session_id, session.id);
        assert_eq!(loaded.project_id, project.id);
        assert_eq!(store.session(&session.id).unwrap().kind, SessionKind::Chat);
        assert_eq!(store.agent(&agent.id).unwrap().display_name, "chat-session");
        assert_ne!(session.kind.as_str(), agent.display_name);
        let replay = store
            .idempotent_turn("idem-a", "digest-a")
            .unwrap()
            .unwrap();
        assert_eq!(replay.id, turn.id);
        assert!(store.idempotent_turn("idem-a", "digest-b").is_err());
        let before = store.runs().unwrap().len();
        let mut conflicting = turn.clone();
        conflicting.id = TurnId::from("trn-other");
        conflicting.request_hash = "digest-b".into();
        let err = store.commit_turn(
            &conflicting,
            std::slice::from_ref(&task),
            Some("idem-a"),
            std::slice::from_ref(&legacy),
        );
        assert!(err.is_err());
        assert_eq!(store.runs().unwrap().len(), before);
        assert!(store.turn(&TurnId::from("trn-other")).is_err());

        let db = Connection::open(temp.path().join("repo.db")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_turn BEFORE INSERT ON turns BEGIN SELECT RAISE(ABORT,'fixture failure'); END;",
        )
        .unwrap();
        drop(db);
        let second = Turn {
            id: TurnId::from("trn-rollback"),
            request_hash: "digest-c".into(),
            idempotency_key: Some("idem-c".into()),
            ..turn
        };
        let second_task = TurnTask {
            id: TaskId::from("tsk-rollback"),
            turn_id: second.id.clone(),
            legacy_task_id: "legacy-rollback".into(),
            ..task
        };
        let second_legacy = Task {
            id: "legacy-rollback".into(),
            ..legacy
        };
        assert!(
            store
                .commit_turn(&second, &[second_task], Some("idem-c"), &[second_legacy])
                .is_err()
        );
        assert!(
            store
                .run("ses-stable")
                .unwrap()
                .tasks
                .iter()
                .all(|task| task.id != "legacy-rollback")
        );
        assert!(
            store
                .idempotent_turn("idem-c", "digest-c")
                .unwrap()
                .is_none()
        );
        let events = store.events("ses-stable", 0).unwrap();
        let mut previous = 0;
        for event in &events {
            assert!(event.seq > previous);
            assert_eq!(event.cursor, event.seq.to_string());
            previous = event.seq;
        }
        assert!(
            events
                .iter()
                .any(|event| event.kind == "status"
                    && event.turn_id.as_deref() == Some("trn-stable"))
        );
        let encoded = serde_json::to_value(&events[0]).unwrap();
        let blob = encoded.to_string().to_ascii_lowercase();
        assert!(!blob.contains("sk-live"));
        assert!(!blob.contains("dpapi"));
        assert!(!blob.contains("bearer "));
        assert!(!encoded.to_string().contains("api_key"));
    }

    #[test]
    fn recover_marks_running_turn_interrupted() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("recover.db")).unwrap();
        let (_, _, _, mut turn, mut task, mut legacy) = sample_graph(&store);
        turn.status = LifecycleStatus::Running;
        task.status = LifecycleStatus::Running;
        legacy.status = "running".into();
        store
            .commit_turn(&turn, &[task], turn.idempotency_key.as_deref(), &[legacy])
            .unwrap();
        assert_eq!(store.recover().unwrap(), 2);
        assert_eq!(
            store.turn(&turn.id).unwrap().status,
            LifecycleStatus::Interrupted
        );
        assert_eq!(store.task("legacy-task-row").unwrap().status, "interrupted");
        let events = store.events(&turn.session_id.0, 0).unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.data["status"] == "interrupted")
        );
        let again = Store::open(&temp.path().join("recover.db")).unwrap();
        assert_eq!(again.recover().unwrap(), 0);
        assert_eq!(
            again.turn(&turn.id).unwrap().status,
            LifecycleStatus::Interrupted
        );
    }

    #[test]
    fn review_fixes_redact_replay_and_real_legacy_run() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("review.db")).unwrap();
        let (project, _session, mut agent, turn, mut task, mut legacy) = sample_graph(&store);
        let session = Session {
            id: SessionId::from("ses-legacy-different"),
            project_id: project.id,
            kind: SessionKind::Chat,
            title: "chat-session".into(),
            legacy_run_id: "legacy-run-different".into(),
            created_at: 10,
            updated_at: 10,
        };
        agent.id = AgentId::from("agt-legacy-different");
        agent.session_id = session.id.clone();
        task.session_id = session.id.clone();
        task.agent_id = agent.id.clone();
        store.insert_session(&session).unwrap();
        store.insert_agent(&agent).unwrap();
        legacy.run_id = session.legacy_run_id.clone();
        legacy.output = "bearer peach-token".into();
        legacy.error = Some("token=ntn_accountsecret".into());
        let mut replay_turn = turn.clone();
        replay_turn.session_id = session.id.clone();
        let saved = store
            .commit_turn(
                &replay_turn,
                std::slice::from_ref(&task),
                replay_turn.idempotency_key.as_deref(),
                std::slice::from_ref(&legacy),
            )
            .unwrap();
        assert_eq!(saved.id, replay_turn.id);
        let again = store
            .commit_turn(
                &Turn {
                    id: TurnId::from("trn-new-attempt"),
                    ..replay_turn.clone()
                },
                std::slice::from_ref(&task),
                replay_turn.idempotency_key.as_deref(),
                &[],
            )
            .unwrap();
        assert_eq!(again.id, replay_turn.id);
        assert!(store.turn(&TurnId::from("trn-new-attempt")).is_err());
        let db = Connection::open(temp.path().join("review.db")).unwrap();
        let legacy_run: String = db
            .query_row(
                "SELECT legacy_run_id FROM idempotency_records WHERE key='idem-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let old_run: String = db
            .query_row(
                "SELECT run_id FROM idempotency WHERE key='idem-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_run, "legacy-run-different");
        assert_eq!(old_run, "legacy-run-different");
        assert_ne!(legacy_run, session.id.0);
        let stored: String = db
            .query_row("SELECT value FROM tasks WHERE id=?1", [&legacy.id], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!stored.contains("peach-token"));
        assert!(!stored.contains("ntn_accountsecret"));
        store
            .event(
                &legacy.id,
                "delta",
                json!({"text":"xai-notaskey","api_key":"plain-provider-token"}),
            )
            .unwrap();
        store
            .event(
                &legacy.id,
                "tool_result",
                json!({"nested":{"authorization":"Bearer peach-token"}}),
            )
            .unwrap();
        store
            .event(
                &legacy.id,
                "error",
                json!({"message":"secret=sk-live-secret"}),
            )
            .unwrap();
        let raw_events: Vec<String> = {
            let mut stmt = db
                .prepare("SELECT data FROM events WHERE task_id=?1")
                .unwrap();
            stmt.query_map([&legacy.id], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let joined = raw_events.join("\n");
        assert!(!joined.contains("peach-token"));
        assert!(!joined.contains("plain-provider-token"));
        assert!(!joined.contains("xai-notaskey"));
        assert!(!joined.contains("sk-live-secret"));
        let err = store.event(&legacy.id, "status", json!({"status":"queued"}));
        let _ = Event {
            seq: 9,
            cursor: "8".into(),
            ..stamped_event(0, &session.id.0, None, &legacy.id, "status", json!({}), 1)
        };
        assert!(err.is_ok());
        let mut injected =
            stamped_event(4, &session.id.0, None, &legacy.id, "status", json!({}), 1);
        injected.seq = 4;
        injected.cursor = "9".into();
        let mut db = store.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        assert!(repository::Repository::append_event(&tx, &injected).is_err());
    }

    #[test]
    fn legacy_create_run_redacts_prompt_messages_and_errors() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("legacy-create.db")).unwrap();
        let mut legacy = sample_graph(&store).5;
        legacy.spec.prompt = "please use sk-promptsecret".into();
        legacy.messages = vec![
            json!({"role":"tool","api_key":"plain-provider-token","arguments":{"accessToken":"camel-token"}}),
        ];
        legacy.output = "xai-notaskey".into();
        legacy.error = Some("authorization: Bearer peach-token".into());
        store
            .create_run(&Run {
                id: legacy.run_id.clone(),
                title: "legacy".into(),
                kind: SessionKind::Team,
                created_at: 1,
                tasks: vec![legacy.clone()],
            })
            .unwrap();
        let db = Connection::open(temp.path().join("legacy-create.db")).unwrap();
        let raw: String = db
            .query_row("SELECT value FROM tasks WHERE id=?1", [&legacy.id], |row| {
                row.get(0)
            })
            .unwrap();
        for secret in [
            "sk-promptsecret",
            "plain-provider-token",
            "camel-token",
            "xai-notaskey",
        ] {
            assert!(!raw.contains(secret), "{secret} leaked through create_run");
        }
        assert!(raw.contains("please use"));
    }

    #[test]
    fn dependencies_are_same_turn_and_acyclic() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("deps.db")).unwrap();
        let (_project, session, agent, turn, mut first, legacy) = sample_graph(&store);
        let mut second = first.clone();
        second.id = TaskId::from("tsk-second");
        second.legacy_task_id = "legacy-second".into();
        second.depends_on = vec![first.id.clone()];
        let mut second_legacy = legacy.clone();
        second_legacy.id = second.legacy_task_id.clone();
        first.depends_on.clear();
        store
            .commit_turn(
                &turn,
                &[first.clone(), second.clone()],
                turn.idempotency_key.as_deref(),
                &[legacy.clone(), second_legacy],
            )
            .unwrap();
        assert_eq!(
            store.task_dependencies(&second.id).unwrap(),
            vec![first.id.clone()]
        );
        let mut cycle = turn.clone();
        cycle.id = TurnId::from("trn-cycle");
        cycle.request_hash = "digest-cycle".into();
        cycle.idempotency_key = Some("idem-cycle".into());
        let mut a = first.clone();
        a.turn_id = cycle.id.clone();
        a.id = TaskId::from("tsk-a");
        a.legacy_task_id = "legacy-a".into();
        let mut b = second.clone();
        b.turn_id = cycle.id.clone();
        b.id = TaskId::from("tsk-b");
        b.legacy_task_id = "legacy-b".into();
        a.depends_on = vec![b.id.clone()];
        b.depends_on = vec![a.id.clone()];
        let legacy_a = Task {
            id: a.legacy_task_id.clone(),
            ..legacy.clone()
        };
        let legacy_b = Task {
            id: b.legacy_task_id.clone(),
            ..legacy
        };
        assert!(
            store
                .commit_turn(
                    &cycle,
                    &[a.clone(), b.clone()],
                    cycle.idempotency_key.as_deref(),
                    &[legacy_a, legacy_b]
                )
                .is_err()
        );
        assert!(store.task_dependencies(&TaskId::from("tsk-a")).is_err());
        let _ = (session, agent);
    }

    #[test]
    fn partial_schema_6_table_stops_before_user_version() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("broken.db");
        legacy_schema_5(&path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL);")
            .unwrap();
        drop(db);
        let error = match Store::open(&path) {
            Ok(_) => panic!("不完整 schema 不应打开"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("结构不完整"));
        let db = Connection::open(&path).unwrap();
        let version: u32 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);
    }

    #[test]
    fn event_task_must_belong_to_turn_or_session() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("event-owner.db")).unwrap();
        let (_project, session, _agent, turn, task, legacy) = sample_graph(&store);
        store
            .commit_turn(
                &turn,
                std::slice::from_ref(&task),
                turn.idempotency_key.as_deref(),
                std::slice::from_ref(&legacy),
            )
            .unwrap();
        store
            .event(&legacy.id, "status", json!({"status":"queued"}))
            .unwrap();
        let mut db = store.db.lock().unwrap();
        let tx = db.transaction().unwrap();
        let mismatch = stamped_event(
            0,
            &session.id.0,
            Some(&turn.id.0),
            "missing-task",
            "status",
            json!({}),
            1,
        );
        assert!(repository::Repository::append_event(&tx, &mismatch).is_err());
    }

    #[test]
    fn legacy_idempotency_replay_is_explicit() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&temp.path().join("replay.db")).unwrap();
        let legacy = {
            let (_project, session, _agent, _turn, _task, legacy) = sample_graph(&store);
            assert_eq!(session.legacy_run_id, legacy.run_id);
            legacy
        };
        store
            .create_run_with_idempotency(
                &Run {
                    id: legacy.run_id.clone(),
                    title: "legacy replay".into(),
                    kind: SessionKind::Team,
                    created_at: 1,
                    tasks: vec![legacy.clone()],
                },
                "legacy-key",
                "digest-legacy",
            )
            .unwrap();
        match store
            .idempotency_replay("legacy-key", "digest-legacy")
            .unwrap()
        {
            Some(IdempotencyReplay::LegacyRun(run)) => assert_eq!(run.id, legacy.run_id),
            other => panic!("expected legacy run, got {other:?}"),
        }
        assert!(
            store
                .idempotent_turn("legacy-key", "digest-legacy")
                .is_err()
        );
        assert!(
            store
                .idempotency_replay("legacy-key", "other-digest")
                .is_err()
        );
        assert!(store.task_dependencies(&TaskId::from("missing")).is_err());
        let empty = tempfile::tempdir().unwrap();
        let empty_store = Store::open(&empty.path().join("empty.db")).unwrap();
        let (_project, _session, _agent, turn, task, legacy_task) = sample_graph(&empty_store);
        assert!(
            empty_store
                .commit_turn(&turn, &[], turn.idempotency_key.as_deref(), &[])
                .is_err()
        );
        let legacy_copy = legacy_task.clone();
        assert!(
            store
                .create_run(&Run {
                    id: legacy_copy.run_id.clone(),
                    title: "bearer peach-token".into(),
                    kind: SessionKind::Team,
                    created_at: 1,
                    tasks: vec![]
                })
                .is_err()
        );
        let mut colliding = task.clone();
        colliding.id = TaskId::from(legacy_copy.id.clone());
        colliding.legacy_task_id = "legacy-other".into();
        let other = Turn {
            id: TurnId::from("trn-collide"),
            request_hash: "digest-collide".into(),
            idempotency_key: Some("idem-collide".into()),
            ..turn
        };
        let other_legacy = Task {
            id: "legacy-other".into(),
            ..legacy_copy
        };
        assert!(
            empty_store
                .commit_turn(&other, &[colliding], Some("idem-collide"), &[other_legacy])
                .is_err()
        );
        let db = Connection::open(temp.path().join("replay.db")).unwrap();
        db.execute(
            "UPDATE idempotency SET request_hash='other-digest' WHERE key='legacy-key'",
            [],
        )
        .unwrap();
        drop(db);
        assert!(
            store
                .idempotency_replay("legacy-key", "digest-legacy")
                .is_err()
        );
        assert_eq!(store.runs().unwrap().len(), 1);
    }
}
