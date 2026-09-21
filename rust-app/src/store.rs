use crate::{domain::*, repository, secrets};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{path::Path, sync::Mutex};

pub const SCHEMA_VERSION: u32 = 6;

pub struct Store {
    db: Mutex<Connection>,
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
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
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
        let row: Option<(String, String)> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT run_id,request_hash FROM idempotency WHERE key=?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, previous_hash)) = row {
            anyhow::ensure!(
                previous_hash.is_empty() || previous_hash == request_hash,
                "同一个 Idempotency-Key 不能用于不同请求"
            );
            return self.run(&id).map(Some);
        }
        Ok(None)
    }
    pub fn save_task(&self, task: &Task) -> Result<()> {
        let value = safe_task_value(task)?;
        let changed = self.db.lock().unwrap().execute(
            "UPDATE tasks SET value=?2 WHERE id=?1",
            params![task.id, value.to_string()],
        )?;
        anyhow::ensure!(changed == 1, "任务不存在");
        Ok(())
    }
    pub fn task(&self, id: &str) -> Result<Task> {
        let value: String = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM tasks WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .context("任务不存在")?;
        Ok(serde_json::from_str(&value)?)
    }
    pub fn run(&self, id: &str) -> Result<Run> {
        let db = self.db.lock().unwrap();
        let mut run = db
            .query_row(
                "SELECT id,title,kind,created_at FROM runs WHERE id=?1",
                [id],
                |r| {
                    let kind: String = r.get(2)?;
                    let kind = serde_json::from_str(&format!("\"{kind}\"")).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok(Run {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        kind,
                        created_at: r.get(3)?,
                        tasks: vec![],
                    })
                },
            )
            .context("任务组不存在")?;
        let mut stmt = db.prepare("SELECT value FROM tasks WHERE run_id=?1 ORDER BY rowid")?;
        for v in stmt.query_map([id], |r| r.get::<_, String>(0))? {
            run.tasks.push(serde_json::from_str(&v?)?);
        }
        Ok(run)
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
        let session = repository::Repository::new(&tx).session_by_legacy_run_of_task(task_id)?;
        let event = stamped_event(
            0,
            session.as_ref().map(|s| s.id.0.as_str()).unwrap_or(""),
            None,
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
        secrets::safe_metadata_text("project_path", &project.root_path)?;
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
        reject_record_secrets(&agent.display_name)?;
        reject_record_secrets(&agent.role)?;
        repository::Repository::new(&self.db.lock().unwrap()).insert_agent(agent)
    }

    pub fn agent(&self, id: &AgentId) -> Result<Agent> {
        repository::Repository::new(&self.db.lock().unwrap()).agent(id)
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
        anyhow::ensure!(!tasks.is_empty(), "回合至少需要一个任务");
        reject_record_secrets(&turn.request_hash)?;
        if let Some(key) = key {
            reject_record_secrets(key)?;
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
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        if let Some(key) = key {
            let repo = repository::Repository::new(&tx);
            match repo.classify_idempotency(key, &turn.request_hash)? {
                Some(IdempotencyHit::Same) => {
                    return repo
                        .idempotency(key, &turn.request_hash)?
                        .transpose()
                        .map_err(|_| anyhow::anyhow!("同一个 Idempotency-Key 不能用于不同请求"))?
                        .context("幂等记录缺少原回合");
                }
                Some(IdempotencyHit::Conflict) => {
                    bail!("同一个 Idempotency-Key 不能用于不同请求");
                }
                None => match repo.legacy_idempotency(key, &turn.request_hash)? {
                    Some(Ok(_)) => bail!("同一个 Idempotency-Key 已经绑定旧运行，不能再创建新回合"),
                    Some(Err(())) => bail!("同一个 Idempotency-Key 不能用于不同请求"),
                    None => {}
                },
            }
        }
        let session = repository::Repository::new(&tx).session(&turn.session_id)?;
        let legacy_ids: std::collections::HashSet<&str> = tasks
            .iter()
            .map(|task| task.legacy_task_id.as_str())
            .collect();
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
            tx.execute(
                "INSERT INTO runs(id,title,kind,created_at) VALUES (?1,?2,?3,?4)",
                params![
                    session.legacy_run_id,
                    session.title,
                    session.kind.as_str(),
                    session.created_at
                ],
            )?;
        }
        for task in legacy_tasks {
            insert_legacy_task(&tx, task)?;
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
        repository::Repository::commit_turn(&tx, turn, tasks, key, &event)?;
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
        tx.commit()?;
        Ok(turn.clone())
    }

    pub fn turn(&self, id: &TurnId) -> Result<Turn> {
        repository::Repository::new(&self.db.lock().unwrap()).turn(id)
    }

    pub fn task_dependencies(&self, id: &TaskId) -> Result<Vec<TaskId>> {
        repository::Repository::new(&self.db.lock().unwrap()).dependencies(id)
    }

    pub fn turn_tasks(&self, id: &TurnId) -> Result<Vec<TurnTask>> {
        repository::Repository::new(&self.db.lock().unwrap()).turn_tasks(id)
    }

    pub fn turns_for_session(&self, id: &SessionId) -> Result<Vec<Turn>> {
        repository::Repository::new(&self.db.lock().unwrap()).turns_for_session(id)
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
        match repo.idempotency(key, request_hash)? {
            Some(Ok(turn)) => return Ok(Some(IdempotencyReplay::Turn(turn))),
            Some(Err(())) => bail!("同一个 Idempotency-Key 不能用于不同请求"),
            None => {}
        }
        let legacy = repo.legacy_idempotency(key, request_hash)?;
        drop(db);
        match legacy {
            Some(Ok(run_id)) => Ok(Some(IdempotencyReplay::LegacyRun(self.run(&run_id)?))),
            Some(Err(())) => bail!("同一个 Idempotency-Key 不能用于不同请求"),
            None => Ok(None),
        }
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
    pub fn events(&self, run_id: &str, after: i64) -> Result<Vec<Event>> {
        let db = self.db.lock().unwrap();
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
    pub fn recover(&self) -> Result<usize> {
        let mut affected = 0;
        let values: Vec<String> = {
            let db = self.db.lock().unwrap();
            let mut stmt = db.prepare("SELECT value FROM tasks")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for value in values {
            let mut task: Task = serde_json::from_str(&value)?;
            if task.status == "running" || task.status == "queued" {
                let streamed: Vec<String> = {
                    let db = self.db.lock().unwrap();
                    let mut stmt = db.prepare(
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
                let mut db = self.db.lock().unwrap();
                let tx = db.transaction()?;
                let value = safe_task_value(&task)?;
                let changed = tx.execute(
                    "UPDATE tasks SET value=?2 WHERE id=?1",
                    params![task.id, value.to_string()],
                )?;
                anyhow::ensure!(changed == 1, "任务不存在");
                let session =
                    repository::Repository::new(&tx).session_by_legacy_run_of_task(&task.id)?;
                let event = stamped_event(
                    0,
                    session
                        .as_ref()
                        .map(|item| item.id.0.as_str())
                        .unwrap_or(""),
                    None,
                    &task.id,
                    "status",
                    secrets::redact_persisted(&json!({"status":"interrupted","error":task.error})),
                    now(),
                );
                let event = Event {
                    session_id: event.session_id.filter(|id| !id.is_empty()),
                    ..event
                };
                repository::Repository::append_event(&tx, &event)?;
                tx.commit()?;
                affected += 1;
            }
        }
        affected += self.recover_turns()?;
        Ok(affected)
    }

    fn recover_turns(&self) -> Result<usize> {
        let pending = repository::Repository::new(&self.db.lock().unwrap()).nonterminal_turns()?;
        let mut affected = 0;
        for turn in pending {
            let at = now();
            let tasks = self.turn_tasks(&turn.id)?;
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction()?;
            {
                let repo = repository::Repository::new(&tx);
                repo.update_turn_status(&turn.id, LifecycleStatus::Interrupted, at)?;
                for task in &tasks {
                    if matches!(
                        task.status,
                        LifecycleStatus::Queued | LifecycleStatus::Running
                    ) {
                        repo.update_task_status(&task.id, LifecycleStatus::Interrupted, at)?;
                    }
                }
            }
            let task_id = tasks
                .first()
                .map(|task| task.legacy_task_id.clone())
                .unwrap_or_else(|| turn.id.0.clone());
            let event = stamped_event(
                0,
                &turn.session_id.0,
                Some(&turn.id.0),
                &task_id,
                "status",
                json!({
                    "status": "interrupted",
                    "turn_id": turn.id.0,
                    "error": "服务曾中断；回合保留为 interrupted，不会记成 completed"
                }),
                at,
            );
            repository::Repository::append_event(&tx, &event)?;
            tx.commit()?;
            affected += 1;
        }
        Ok(affected)
    }
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
    let value = safe_task_value(task)?;
    tx.execute(
        "INSERT INTO tasks(id,run_id,value) VALUES (?1,?2,?3)",
        params![task.id, task.run_id, value.to_string()],
    )?;
    Ok(())
}

fn reject_record_secrets(value: &str) -> Result<()> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("sk-") || lower.contains("dpapi") || lower.contains("bearer ") {
        bail!("领域记录不能包含凭据材料");
    }
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
        assert_eq!(store.schema_version().unwrap(), 6);
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
        assert_eq!(version, 6);
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
        assert!(
            store
                .create_run(&Run {
                    id: legacy_task.run_id,
                    title: "bearer peach-token".into(),
                    kind: SessionKind::Team,
                    created_at: 1,
                    tasks: vec![]
                })
                .is_err()
        );
        let _ = task;
    }
}
