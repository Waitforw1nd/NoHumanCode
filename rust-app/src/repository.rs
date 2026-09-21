//! Project, session, and turn repository.
//!
//! Rows here are the durable domain model.  The legacy `runs` / `tasks` JSON
//! blobs stay readable as a compatibility projection, but new code records a
//! turn, its tasks, its idempotency key, and its lifecycle event in one
//! transaction.  A failed transaction leaves none of those rows behind.

use crate::{
    domain::{
        Agent, AgentId, EVENT_SCHEMA_VERSION, Event, IdempotencyHit, LifecycleStatus, Project,
        ProjectId, Session, SessionId, SessionKind, TaskId, Turn, TurnId, TurnTask, event_id, now,
    },
    secrets,
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

pub const MIGRATION_ID: &str = "project-session-turn-repository";

/// Tables and columns added on top of schema 5.  Applied only from inside
/// the schema 6 transaction.  `CREATE IF NOT EXISTS` keeps a retried open
/// safe if a previous attempt created objects and then rolled the version
/// back; the migration marker is what prevents a second rewrite.
const SCHEMA_6_SQL: &str = "
CREATE TABLE IF NOT EXISTS projects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  root_path TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  legacy_run_id TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS turns (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  project_id TEXT NOT NULL REFERENCES projects(id),
  status TEXT NOT NULL,
  request_hash TEXT NOT NULL,
  idempotency_key TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS agents (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  display_name TEXT NOT NULL,
  role TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS turn_tasks (
  id TEXT PRIMARY KEY,
  turn_id TEXT NOT NULL REFERENCES turns(id),
  session_id TEXT NOT NULL REFERENCES sessions(id),
  agent_id TEXT NOT NULL REFERENCES agents(id),
  legacy_task_id TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS turn_task_dependencies (
  task_id TEXT NOT NULL REFERENCES turn_tasks(id),
  depends_on_task_id TEXT NOT NULL REFERENCES turn_tasks(id),
  PRIMARY KEY (task_id, depends_on_task_id)
);
CREATE TABLE IF NOT EXISTS idempotency_records (
  key TEXT PRIMARY KEY,
  request_hash TEXT NOT NULL,
  turn_id TEXT NOT NULL REFERENCES turns(id),
  session_id TEXT NOT NULL REFERENCES sessions(id),
  legacy_run_id TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_project ON sessions(project_id, created_at);
CREATE INDEX IF NOT EXISTS turns_session ON turns(session_id, created_at);
CREATE UNIQUE INDEX IF NOT EXISTS turn_tasks_legacy_task_id_unique ON turn_tasks(legacy_task_id);
CREATE INDEX IF NOT EXISTS turn_tasks_turn ON turn_tasks(turn_id);
CREATE INDEX IF NOT EXISTS agents_session ON agents(session_id);
";

/// Columns the schema 5 `events` table does not have.  Added individually so
/// a retry after a partial DDL failure does not abort on an existing column.
const EVENT_ENVELOPE_COLUMNS: &[(&str, &str)] = &[
    ("schema_version", "INTEGER NOT NULL DEFAULT 0"),
    ("cursor", "TEXT NOT NULL DEFAULT ''"),
    ("session_id", "TEXT"),
    ("turn_id", "TEXT"),
];

const REQUIRED_COLUMNS: &[(&str, &[&str])] = &[
    (
        "agents",
        &["id", "session_id", "display_name", "role", "created_at"],
    ),
    ("turn_task_dependencies", &["task_id", "depends_on_task_id"]),
    (
        "idempotency_records",
        &[
            "key",
            "request_hash",
            "turn_id",
            "session_id",
            "legacy_run_id",
            "created_at",
        ],
    ),
    (
        "projects",
        &["id", "name", "root_path", "created_at", "updated_at"],
    ),
    (
        "sessions",
        &[
            "id",
            "project_id",
            "kind",
            "title",
            "legacy_run_id",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "turns",
        &[
            "id",
            "session_id",
            "project_id",
            "status",
            "request_hash",
            "idempotency_key",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "turn_tasks",
        &[
            "id",
            "turn_id",
            "session_id",
            "agent_id",
            "legacy_task_id",
            "status",
            "created_at",
            "updated_at",
        ],
    ),
];

pub fn apply_schema_6(tx: &Transaction<'_>) -> Result<()> {
    verify_or_reject_partial_tables(tx)?;
    tx.execute_batch(SCHEMA_6_SQL)?;
    verify_or_reject_partial_tables(tx)?;
    ensure_event_columns(tx)?;
    backfill_event_cursors(tx)?;
    let applied = tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, now()],
    )?;
    anyhow::ensure!(applied == 1, "schema 6 迁移标记没有写入");
    tx.pragma_update(None, "user_version", 6)?;
    Ok(())
}

fn verify_or_reject_partial_tables(tx: &Transaction<'_>) -> Result<()> {
    for (table, required) in REQUIRED_COLUMNS {
        let mut existing = Vec::new();
        let mut stmt = tx.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for name in rows {
            existing.push(name?);
        }
        if existing.is_empty() {
            continue;
        }
        let missing: Vec<_> = required
            .iter()
            .filter(|column| !existing.iter().any(|name| name == *column))
            .collect();
        if !missing.is_empty() {
            bail!("schema 6 表 {table} 结构不完整，缺少列 {missing:?}；停止迁移，避免写入时才失败");
        }
    }
    if table_exists(tx, "turn_tasks")? {
        let duplicates: i64 = tx.query_row(
            "SELECT count(*) FROM (SELECT legacy_task_id FROM turn_tasks GROUP BY legacy_task_id HAVING count(*)>1)",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            duplicates == 0,
            "turn_tasks.legacy_task_id 存在重复值，停止迁移"
        );
        let unique: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='turn_tasks_legacy_task_id_unique'",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            unique == 1,
            "schema 6 缺少 turn_tasks_legacy_task_id_unique；停止迁移"
        );
    }
    Ok(())
}

fn table_exists(tx: &Transaction<'_>, table: &str) -> Result<bool> {
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

fn ensure_event_columns(tx: &Transaction<'_>) -> Result<()> {
    let mut existing = Vec::new();
    {
        let mut stmt = tx.prepare("PRAGMA table_info(events)")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for name in rows {
            existing.push(name?);
        }
    }
    for (name, declaration) in EVENT_ENVELOPE_COLUMNS {
        if existing.iter().any(|column| column == name) {
            continue;
        }
        tx.execute_batch(&format!(
            "ALTER TABLE events ADD COLUMN {name} {declaration}"
        ))?;
    }
    Ok(())
}

fn backfill_event_cursors(tx: &Transaction<'_>) -> Result<()> {
    // One-time fill for rows written before the envelope.  Empty cursor is
    // the only rewrite; already stamped rows are left untouched.
    tx.execute(
        "UPDATE events SET cursor = CAST(seq AS TEXT) WHERE cursor = '' OR cursor IS NULL",
        [],
    )?;
    Ok(())
}

pub fn migration_applied(conn: &Connection, id: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM schema_migrations WHERE id=?1",
        [id],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

fn status_text(status: LifecycleStatus) -> &'static str {
    status.as_str()
}

fn parse_status(value: String) -> Result<LifecycleStatus> {
    LifecycleStatus::parse(&value).with_context(|| format!("未知的生命周期状态：{value}"))
}

fn parse_kind(value: String) -> Result<SessionKind> {
    serde_json::from_value(Value::String(value.clone()))
        .with_context(|| format!("未知的会话类型：{value}"))
}

pub struct Repository<'a> {
    conn: &'a Connection,
}

impl<'a> Repository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert_project(&self, project: &Project) -> Result<()> {
        self.conn.execute(
            "INSERT INTO projects(id,name,root_path,created_at,updated_at) VALUES (?1,?2,?3,?4,?5)",
            params![
                project.id.0,
                project.name,
                project.root_path,
                project.created_at,
                project.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn project(&self, id: &ProjectId) -> Result<Project> {
        self.conn
            .query_row(
                "SELECT id,name,root_path,created_at,updated_at FROM projects WHERE id=?1",
                [&id.0],
                project_row,
            )
            .context("项目不存在")
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,name,root_path,created_at,updated_at FROM projects ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], project_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions(id,project_id,kind,title,legacy_run_id,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                session.id.0,
                session.project_id.0,
                session.kind.as_str(),
                session.title,
                session.legacy_run_id,
                session.created_at,
                session.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn session(&self, id: &SessionId) -> Result<Session> {
        self.conn
            .query_row(
                "SELECT id,project_id,kind,title,legacy_run_id,created_at,updated_at FROM sessions WHERE id=?1",
                [&id.0],
                session_row,
            )
            .context("会话不存在")
    }

    pub fn session_by_legacy_run_of_task(&self, task_id: &str) -> Result<Option<Session>> {
        let run_id: Option<String> = self
            .conn
            .query_row("SELECT run_id FROM tasks WHERE id=?1", [task_id], |row| {
                row.get(0)
            })
            .optional()?;
        match run_id {
            Some(run_id) => self.session_by_legacy_run(&run_id),
            None => Ok(None),
        }
    }

    pub fn session_by_legacy_run(&self, run_id: &str) -> Result<Option<Session>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id,project_id,kind,title,legacy_run_id,created_at,updated_at FROM sessions WHERE legacy_run_id=?1",
                [run_id],
                session_row,
            )
            .optional()?)
    }

    pub fn insert_agent(&self, agent: &Agent) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agents(id,session_id,display_name,role,created_at) VALUES (?1,?2,?3,?4,?5)",
            params![
                agent.id.0,
                agent.session_id.0,
                agent.display_name,
                agent.role,
                agent.created_at
            ],
        )?;
        Ok(())
    }

    pub fn agent(&self, id: &AgentId) -> Result<Agent> {
        self.conn
            .query_row(
                "SELECT id,session_id,display_name,role,created_at FROM agents WHERE id=?1",
                [&id.0],
                |row| {
                    Ok(Agent {
                        id: AgentId(row.get(0)?),
                        session_id: SessionId(row.get(1)?),
                        display_name: row.get(2)?,
                        role: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .context("Agent 不存在")
    }

    /// Commit a turn, its executable tasks, the optional idempotency record,
    /// and the opening lifecycle event together.
    ///
    /// The caller inserts the legacy run and task rows in the same
    /// transaction before this returns.  A later failure rolls those rows
    /// back with the domain rows.
    pub fn commit_turn(
        tx: &Transaction<'_>,
        turn: &Turn,
        tasks: &[TurnTask],
        idempotency_key: Option<&str>,
        opening_event: &Event,
    ) -> Result<()> {
        insert_turn_tx(tx, turn)?;
        for task in tasks {
            insert_turn_task_tx(tx, task)?;
        }
        for task in tasks {
            Repository::replace_dependencies(tx, &task.id, &task.depends_on)?;
        }
        ensure_acyclic(tx, &turn.id)?;
        if let Some(key) = idempotency_key {
            let legacy_run_id: String = tx.query_row(
                "SELECT legacy_run_id FROM sessions WHERE id=?1",
                [&turn.session_id.0],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO idempotency_records(key,request_hash,turn_id,session_id,legacy_run_id,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    key,
                    turn.request_hash,
                    turn.id.0,
                    turn.session_id.0,
                    legacy_run_id,
                    turn.created_at
                ],
            )?;
        }
        insert_event_tx(tx, opening_event)?;
        Ok(())
    }

    pub fn turn(&self, id: &TurnId) -> Result<Turn> {
        self.conn
            .query_row(
                "SELECT id,session_id,project_id,status,request_hash,idempotency_key,created_at,updated_at FROM turns WHERE id=?1",
                [&id.0],
                turn_row,
            )
            .context("回合不存在")
    }

    pub fn turns_for_session(&self, session_id: &SessionId) -> Result<Vec<Turn>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,session_id,project_id,status,request_hash,idempotency_key,created_at,updated_at FROM turns WHERE session_id=?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([&session_id.0], turn_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn turn_tasks(&self, turn_id: &TurnId) -> Result<Vec<TurnTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,turn_id,session_id,agent_id,legacy_task_id,status,created_at,updated_at FROM turn_tasks WHERE turn_id=?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([&turn_id.0], turn_task_row)?;
        let mut tasks = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        for task in &mut tasks {
            task.depends_on = self.dependencies(&task.id)?;
        }
        Ok(tasks)
    }

    pub fn update_turn_status(&self, id: &TurnId, status: LifecycleStatus, at: u64) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE turns SET status=?2, updated_at=?3 WHERE id=?1",
            params![id.0, status_text(status), at],
        )?;
        anyhow::ensure!(changed == 1, "回合不存在");
        Ok(())
    }

    pub fn update_task_status(&self, id: &TaskId, status: LifecycleStatus, at: u64) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE turn_tasks SET status=?2, updated_at=?3 WHERE id=?1",
            params![id.0, status_text(status), at],
        )?;
        anyhow::ensure!(changed == 1, "执行任务不存在");
        Ok(())
    }

    /// Resume evidence for queued/running work that did not reach a terminal
    /// state.  The caller records the interrupted event separately.
    pub fn nonterminal_turns(&self) -> Result<Vec<Turn>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,session_id,project_id,status,request_hash,idempotency_key,created_at,updated_at FROM turns WHERE status IN ('queued','running') ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], turn_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn replace_dependencies(
        tx: &Transaction<'_>,
        task_id: &TaskId,
        depends_on: &[TaskId],
    ) -> Result<()> {
        let turn_id: String = tx.query_row(
            "SELECT turn_id FROM turn_tasks WHERE id=?1",
            [&task_id.0],
            |row| row.get(0),
        )?;
        tx.execute(
            "DELETE FROM turn_task_dependencies WHERE task_id=?1",
            [&task_id.0],
        )?;
        for dependency in depends_on {
            anyhow::ensure!(dependency != task_id, "任务不能依赖自己");
            let dependency_turn: String = tx.query_row(
                "SELECT turn_id FROM turn_tasks WHERE id=?1",
                [&dependency.0],
                |row| row.get(0),
            )?;
            anyhow::ensure!(dependency_turn == turn_id, "任务只能依赖同一回合中的任务");
            tx.execute(
                "INSERT INTO turn_task_dependencies(task_id,depends_on_task_id) VALUES (?1,?2)",
                params![task_id.0, dependency.0],
            )?;
        }
        Ok(())
    }

    pub fn dependencies(&self, task_id: &TaskId) -> Result<Vec<TaskId>> {
        let exists: i64 = self.conn.query_row(
            "SELECT count(*) FROM turn_tasks WHERE id=?1",
            [&task_id.0],
            |row| row.get(0),
        )?;
        anyhow::ensure!(exists == 1, "执行任务不存在");
        let mut stmt = self.conn.prepare(
            "SELECT depends_on_task_id FROM turn_task_dependencies WHERE task_id=?1 ORDER BY depends_on_task_id",
        )?;
        let rows = stmt.query_map([&task_id.0], |row| Ok(TaskId(row.get(0)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// New records win.  A key that exists only in the legacy table is still
    /// classified, so a retry cannot fail later as a bare UNIQUE error.
    pub fn idempotency(&self, key: &str, request_hash: &str) -> Result<Option<Result<Turn, ()>>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT request_hash, turn_id FROM idempotency_records WHERE key=?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((previous, turn_id)) = row else {
            return Ok(None);
        };
        if previous != request_hash {
            return Ok(Some(Err(())));
        }
        Ok(Some(Ok(self.turn(&TurnId(turn_id))?)))
    }

    pub fn legacy_idempotency(
        &self,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<Result<String, ()>>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT request_hash, run_id FROM idempotency WHERE key=?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((previous, run_id)) = row else {
            return Ok(None);
        };
        if !previous.is_empty() && previous != request_hash {
            return Ok(Some(Err(())));
        }
        Ok(Some(Ok(run_id)))
    }

    pub fn classify_idempotency(
        &self,
        key: &str,
        request_hash: &str,
    ) -> Result<Option<IdempotencyHit>> {
        match self.idempotency(key, request_hash)? {
            None => Ok(None),
            Some(Ok(_)) => Ok(Some(IdempotencyHit::Same)),
            Some(Err(())) => Ok(Some(IdempotencyHit::Conflict)),
        }
    }

    pub fn append_event(tx: &Transaction<'_>, event: &Event) -> Result<i64> {
        insert_event_tx(tx, event)
    }

    pub fn events_after(&self, session_id: &SessionId, after: i64) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq,schema_version,cursor,session_id,turn_id,task_id,kind,data,at FROM events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT 500",
        )?;
        let rows = stmt.query_map(params![session_id.0, after], event_row)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }
}

fn ensure_acyclic(tx: &Transaction<'_>, turn_id: &TurnId) -> Result<()> {
    let mut edges = std::collections::HashMap::<String, Vec<String>>::new();
    let mut stmt = tx.prepare(
        "SELECT d.task_id, d.depends_on_task_id FROM turn_task_dependencies d JOIN turn_tasks t ON t.id=d.task_id WHERE t.turn_id=?1",
    )?;
    let rows = stmt.query_map([&turn_id.0], |row| Ok((row.get(0)?, row.get(1)?)))?;
    for row in rows {
        let (task_id, dependency): (String, String) = row?;
        edges.entry(task_id).or_default().push(dependency);
    }
    drop(stmt);
    let mut visiting = std::collections::HashSet::new();
    let mut visited = std::collections::HashSet::new();
    for task_id in edges.keys() {
        if cycle_from(task_id, &edges, &mut visiting, &mut visited) {
            bail!("任务依赖存在循环");
        }
    }
    Ok(())
}

fn cycle_from(
    task_id: &str,
    edges: &std::collections::HashMap<String, Vec<String>>,
    visiting: &mut std::collections::HashSet<String>,
    visited: &mut std::collections::HashSet<String>,
) -> bool {
    if visited.contains(task_id) {
        return false;
    }
    if !visiting.insert(task_id.to_owned()) {
        return true;
    }
    if let Some(dependencies) = edges.get(task_id) {
        for dependency in dependencies {
            if cycle_from(dependency, edges, visiting, visited) {
                return true;
            }
        }
    }
    visiting.remove(task_id);
    visited.insert(task_id.to_owned());
    false
}

fn insert_turn_tx(tx: &Transaction<'_>, turn: &Turn) -> Result<()> {
    let project_id: String = tx.query_row(
        "SELECT project_id FROM sessions WHERE id=?1",
        [&turn.session_id.0],
        |row| row.get(0),
    )?;
    anyhow::ensure!(project_id == turn.project_id.0, "回合的项目与会话不一致");
    tx.execute(
        "INSERT INTO turns(id,session_id,project_id,status,request_hash,idempotency_key,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            turn.id.0,
            turn.session_id.0,
            turn.project_id.0,
            status_text(turn.status),
            turn.request_hash,
            turn.idempotency_key,
            turn.created_at,
            turn.updated_at,
        ],
    )?;
    Ok(())
}

fn insert_turn_task_tx(tx: &Transaction<'_>, task: &TurnTask) -> Result<()> {
    let turn_session: String = tx.query_row(
        "SELECT session_id FROM turns WHERE id=?1",
        [&task.turn_id.0],
        |row| row.get(0),
    )?;
    anyhow::ensure!(turn_session == task.session_id.0, "任务的会话与回合不一致");
    let agent_session: String = tx.query_row(
        "SELECT session_id FROM agents WHERE id=?1",
        [&task.agent_id.0],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        agent_session == task.session_id.0,
        "任务的 Agent 不属于该会话"
    );
    tx.execute(
        "INSERT INTO turn_tasks(id,turn_id,session_id,agent_id,legacy_task_id,status,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            task.id.0,
            task.turn_id.0,
            task.session_id.0,
            task.agent_id.0,
            task.legacy_task_id,
            status_text(task.status),
            task.created_at,
            task.updated_at,
        ],
    )?;
    Ok(())
}

fn insert_event_tx(tx: &Transaction<'_>, event: &Event) -> Result<i64> {
    anyhow::ensure!(event.seq == 0, "新事件的 seq 必须由数据库分配");
    anyhow::ensure!(
        event.schema_version == EVENT_SCHEMA_VERSION,
        "新事件的 schema_version 无效"
    );
    if let Some(session_id) = &event.session_id {
        let known: i64 = tx.query_row(
            "SELECT count(*) FROM sessions WHERE id=?1",
            [session_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(known == 1, "事件引用了不存在的会话");
    }
    if let Some(turn_id) = &event.turn_id {
        let session_id: String = tx.query_row(
            "SELECT session_id FROM turns WHERE id=?1",
            [turn_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            event.session_id.as_deref() == Some(session_id.as_str()),
            "事件的回合不属于该会话"
        );
        let task_turn: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM turn_tasks WHERE id=?1 OR legacy_task_id=?1",
                [&event.task_id],
                |row| row.get(0),
            )
            .optional()?;
        anyhow::ensure!(
            task_turn.as_deref() == Some(turn_id.as_str()),
            "事件的任务不属于该回合"
        );
    } else if let Some(session_id) = &event.session_id {
        let legacy_run_id: String = tx.query_row(
            "SELECT legacy_run_id FROM sessions WHERE id=?1",
            [session_id],
            |row| row.get(0),
        )?;
        let run_id: Option<String> = tx
            .query_row(
                "SELECT run_id FROM tasks WHERE id=?1",
                [&event.task_id],
                |row| row.get(0),
            )
            .optional()?;
        anyhow::ensure!(
            run_id.as_deref() == Some(legacy_run_id.as_str()),
            "事件的任务不属于该会话"
        );
    } else {
        let exists: i64 = tx.query_row(
            "SELECT count(*) FROM tasks WHERE id=?1",
            [&event.task_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(exists == 1, "事件引用了不存在的任务");
    }
    let data = secrets::redact_persisted(&event.data);
    tx.execute(
        "INSERT INTO events(task_id,kind,data,at,schema_version,cursor,session_id,turn_id) VALUES (?1,?2,?3,?4,?5,'',?6,?7)",
        params![
            event.task_id,
            event.kind,
            data.to_string(),
            event.at,
            EVENT_SCHEMA_VERSION,
            event.session_id,
            event.turn_id,
        ],
    )?;
    let seq = tx.last_insert_rowid();
    tx.execute(
        "UPDATE events SET cursor=?2 WHERE seq=?1",
        params![seq, seq.to_string()],
    )?;
    let _ = event_id(seq);
    Ok(seq)
}

fn project_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: ProjectId(row.get(0)?),
        name: row.get(1)?,
        root_path: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let kind: String = row.get(2)?;
    let kind = parse_kind(kind).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, error.into())
    })?;
    Ok(Session {
        id: SessionId(row.get(0)?),
        project_id: ProjectId(row.get(1)?),
        kind,
        title: row.get(3)?,
        legacy_run_id: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn turn_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Turn> {
    let status: String = row.get(3)?;
    let status = parse_status(status).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, error.into())
    })?;
    Ok(Turn {
        id: TurnId(row.get(0)?),
        session_id: SessionId(row.get(1)?),
        project_id: ProjectId(row.get(2)?),
        status,
        request_hash: row.get(4)?,
        idempotency_key: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn turn_task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnTask> {
    let status: String = row.get(5)?;
    let status = parse_status(status).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, error.into())
    })?;
    Ok(TurnTask {
        id: TaskId(row.get(0)?),
        turn_id: TurnId(row.get(1)?),
        session_id: SessionId(row.get(2)?),
        agent_id: AgentId(row.get(3)?),
        legacy_task_id: row.get(4)?,
        depends_on: Vec::new(),
        status,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let data: String = row.get(7)?;
    let data: Value = serde_json::from_str(&data).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let seq: i64 = row.get(0)?;
    let cursor: String = row.get(2)?;
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
