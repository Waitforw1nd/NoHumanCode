//! Project, session, and turn repository.
//!
//! Rows here are the durable domain model.  The legacy `runs` / `tasks` JSON
//! blobs stay readable as a compatibility projection, but new code records a
//! turn, its tasks, its idempotency key, and its lifecycle event in one
//! transaction.  A failed transaction leaves none of those rows behind.

use crate::{
    domain::{
        Agent, AgentId, ChatTurnError, EVENT_SCHEMA_VERSION, Event, IdempotencyHit,
        LifecycleStatus, Project, ProjectId, Run, Session, SessionId, SessionKind, Task, TaskId,
        Turn, TurnId, TurnTask, event_id, now, safe_task_value, validate_event_kind,
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

struct ColumnShape {
    name: &'static str,
    declaration: &'static str,
    not_null: bool,
    primary_key: i64,
}

const fn column(name: &'static str, not_null: bool, primary_key: i64) -> ColumnShape {
    ColumnShape {
        name,
        declaration: "TEXT",
        not_null,
        primary_key,
    }
}

const fn integer_column(name: &'static str) -> ColumnShape {
    ColumnShape {
        name,
        declaration: "INTEGER",
        not_null: true,
        primary_key: 0,
    }
}

const SCHEMA_6_COLUMNS: &[(&str, &[ColumnShape])] = &[
    (
        "projects",
        &[
            column("id", false, 1),
            column("name", true, 0),
            column("root_path", true, 0),
            integer_column("created_at"),
            integer_column("updated_at"),
        ],
    ),
    (
        "sessions",
        &[
            column("id", false, 1),
            column("project_id", true, 0),
            column("kind", true, 0),
            column("title", true, 0),
            column("legacy_run_id", true, 0),
            integer_column("created_at"),
            integer_column("updated_at"),
        ],
    ),
    (
        "turns",
        &[
            column("id", false, 1),
            column("session_id", true, 0),
            column("project_id", true, 0),
            column("status", true, 0),
            column("request_hash", true, 0),
            column("idempotency_key", false, 0),
            integer_column("created_at"),
            integer_column("updated_at"),
        ],
    ),
    (
        "agents",
        &[
            column("id", false, 1),
            column("session_id", true, 0),
            column("display_name", true, 0),
            column("role", true, 0),
            integer_column("created_at"),
        ],
    ),
    (
        "turn_tasks",
        &[
            column("id", false, 1),
            column("turn_id", true, 0),
            column("session_id", true, 0),
            column("agent_id", true, 0),
            column("legacy_task_id", true, 0),
            column("status", true, 0),
            integer_column("created_at"),
            integer_column("updated_at"),
        ],
    ),
    (
        "turn_task_dependencies",
        &[
            column("task_id", true, 1),
            column("depends_on_task_id", true, 2),
        ],
    ),
    (
        "idempotency_records",
        &[
            column("key", false, 1),
            column("request_hash", true, 0),
            column("turn_id", true, 0),
            column("session_id", true, 0),
            column("legacy_run_id", true, 0),
            integer_column("created_at"),
        ],
    ),
];

const SCHEMA_6_FOREIGN_KEYS: &[(&str, &str, &str, &str)] = &[
    ("sessions", "project_id", "projects", "id"),
    ("turns", "session_id", "sessions", "id"),
    ("turns", "project_id", "projects", "id"),
    ("agents", "session_id", "sessions", "id"),
    ("turn_tasks", "turn_id", "turns", "id"),
    ("turn_tasks", "session_id", "sessions", "id"),
    ("turn_tasks", "agent_id", "agents", "id"),
    ("turn_task_dependencies", "task_id", "turn_tasks", "id"),
    (
        "turn_task_dependencies",
        "depends_on_task_id",
        "turn_tasks",
        "id",
    ),
    ("idempotency_records", "turn_id", "turns", "id"),
    ("idempotency_records", "session_id", "sessions", "id"),
];

const SCHEMA_6_INDEXES: &[(&str, &str, bool, &[&str])] = &[
    (
        "sessions",
        "sessions_project",
        false,
        &["project_id", "created_at"],
    ),
    (
        "turns",
        "turns_session",
        false,
        &["session_id", "created_at"],
    ),
    (
        "turn_tasks",
        "turn_tasks_legacy_task_id_unique",
        true,
        &["legacy_task_id"],
    ),
    ("turn_tasks", "turn_tasks_turn", false, &["turn_id"]),
    ("agents", "agents_session", false, &["session_id"]),
];

pub fn apply_schema_6(tx: &Transaction<'_>) -> Result<()> {
    verify_schema_tables(tx, SchemaCheck::BeforeMigration)?;
    tx.execute_batch(SCHEMA_6_SQL)?;
    verify_schema_tables(tx, SchemaCheck::Complete)?;
    ensure_event_columns(tx)?;
    verify_event_columns(tx)?;
    backfill_event_cursors(tx)?;
    let applied = tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, now()],
    )?;
    anyhow::ensure!(applied == 1, "schema 6 迁移标记没有写入");
    tx.pragma_update(None, "user_version", 6)?;
    Ok(())
}

/// Recheck the actual database even when its version and migration marker are
/// already current. The only repair allowed here is the unique legacy task
/// index introduced during schema 6 development. Validate existing objects and
/// row identities first; a forged same-name index must never suppress repair.
pub(crate) fn verify_current_schema(tx: &Transaction<'_>) -> Result<()> {
    verify_schema_tables(tx, SchemaCheck::UpgradeLegacyIndex)?;
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS turn_tasks_legacy_task_id_unique ON turn_tasks(legacy_task_id);",
    )?;
    verify_schema_tables(tx, SchemaCheck::Complete)?;
    verify_event_columns(tx)?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SchemaCheck {
    BeforeMigration,
    UpgradeLegacyIndex,
    Complete,
}

fn verify_schema_tables(tx: &Transaction<'_>, mode: SchemaCheck) -> Result<()> {
    for (table, expected) in SCHEMA_6_COLUMNS {
        if !table_exists(tx, table)? {
            anyhow::ensure!(
                mode == SchemaCheck::BeforeMigration,
                "schema 6 缺少表 {table}；停止打开数据库"
            );
            continue;
        }
        verify_columns(tx, table, expected)?;
        verify_foreign_keys(tx, table)?;
        verify_indexes(tx, table, mode)?;
        let mut primary_key: Vec<_> = expected
            .iter()
            .filter(|column| column.primary_key > 0)
            .collect();
        primary_key.sort_by_key(|column| column.primary_key);
        let primary_key: Vec<_> = primary_key.iter().map(|column| column.name).collect();
        verify_unique_columns(tx, table, &primary_key)?;
        if *table == "sessions" {
            verify_unique_columns(tx, table, &["legacy_run_id"])?;
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
        let cross_collision: i64 = tx.query_row(
            "SELECT count(*) FROM turn_tasks a JOIN turn_tasks b ON a.id=b.legacy_task_id WHERE a.id<>b.id",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            cross_collision == 0,
            "turn_tasks 存在 id 与其他 legacy_task_id 的交叉碰撞；停止迁移"
        );
    }
    Ok(())
}

fn verify_columns(tx: &Transaction<'_>, table: &str, expected: &[ColumnShape]) -> Result<()> {
    let mut actual = Vec::new();
    let mut stmt = tx.prepare(&format!("PRAGMA table_xinfo({table})"))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    for row in rows {
        actual.push(row?);
    }
    for shape in expected {
        let Some((_, declaration, not_null, primary_key)) =
            actual.iter().find(|(name, _, _, _)| name == shape.name)
        else {
            bail!(
                "schema 6 表 {table} 结构不完整，缺少列 {}；停止迁移",
                shape.name
            );
        };
        anyhow::ensure!(
            declaration.eq_ignore_ascii_case(shape.declaration),
            "schema 6 表 {table} 列 {} 的声明类型错误，应为 {}",
            shape.name,
            shape.declaration
        );
        anyhow::ensure!(
            *primary_key == shape.primary_key,
            "schema 6 表 {table} 列 {} 的主键序号错误",
            shape.name
        );
        anyhow::ensure!(
            (*not_null == 1) == shape.not_null,
            "schema 6 表 {table} 列 {} 的 NOT NULL 属性错误",
            shape.name
        );
    }
    anyhow::ensure!(
        actual.len() == expected.len(),
        "schema 6 表 {table} 含有未知列；停止打开数据库"
    );
    Ok(())
}

fn verify_foreign_keys(tx: &Transaction<'_>, table: &str) -> Result<()> {
    let expected: Vec<_> = SCHEMA_6_FOREIGN_KEYS
        .iter()
        .filter(|(t, _, _, _)| *t == table)
        .collect();
    let mut actual = Vec::new();
    let mut stmt = tx.prepare(&format!("PRAGMA foreign_key_list({table})"))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    for row in rows {
        actual.push(row?);
    }
    anyhow::ensure!(
        actual.len() == expected.len(),
        "schema 6 表 {table} 的外键数量错误"
    );
    for (_, from, target, to) in expected {
        anyhow::ensure!(
            actual.iter().any(
                |(id, seq, table, column, target_column, on_update, on_delete, match_kind)| table
                    == target
                    && column == from
                    && target_column == to
                    && *seq == 0
                    && actual
                        .iter()
                        .filter(|(other_id, ..)| other_id == id)
                        .count()
                        == 1
                    && on_update == "NO ACTION"
                    && on_delete == "NO ACTION"
                    && match_kind == "NONE"
            ),
            "schema 6 表 {table} 缺少外键 {from}->{target}.{to}"
        );
    }
    Ok(())
}

fn verify_indexes(tx: &Transaction<'_>, table: &str, mode: SchemaCheck) -> Result<()> {
    for (_, index, unique, columns) in SCHEMA_6_INDEXES.iter().filter(|(t, _, _, _)| *t == table) {
        let index_owner: Option<String> = tx
            .query_row(
                "SELECT tbl_name FROM sqlite_master WHERE name=?1",
                [index],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = index_owner {
            anyhow::ensure!(owner == table, "schema 6 索引 {index} 所属表错误");
        }
        let Some((_, actual_unique, partial)) = index_list(tx, table)?
            .into_iter()
            .find(|(name, _, _)| name == index)
        else {
            if mode == SchemaCheck::BeforeMigration
                || (mode == SchemaCheck::UpgradeLegacyIndex
                    && *index == "turn_tasks_legacy_task_id_unique")
            {
                continue;
            }
            bail!("schema 6 表 {table} 缺少索引 {index}；停止迁移");
        };
        anyhow::ensure!(
            actual_unique == *unique,
            "schema 6 索引 {index} 的 UNIQUE 属性错误"
        );
        anyhow::ensure!(!partial, "schema 6 索引 {index} 不能是部分索引");
        anyhow::ensure!(
            index_columns_match(tx, index, columns)?,
            "schema 6 索引 {index} 的列、顺序或排序规则错误"
        );
    }
    Ok(())
}

fn index_list(tx: &Transaction<'_>, table: &str) -> Result<Vec<(String, bool, bool)>> {
    let mut stmt = tx.prepare(&format!("PRAGMA index_list({table})"))?;
    Ok(stmt
        .query_map([], |row| Ok((row.get(1)?, row.get(2)?, row.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

fn index_columns_match(tx: &Transaction<'_>, index: &str, expected: &[&str]) -> Result<bool> {
    // Quote database-owned names: autoindexes are not necessarily names that
    // our own DDL produced when opening a malformed/older database.
    let escaped_index = index.replace('"', "\"\"");
    let mut stmt = tx.prepare(&format!("PRAGMA index_xinfo(\"{escaped_index}\")"))?;
    let entries = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, bool>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let keys: Vec<_> = entries.iter().filter(|(_, _, _, key)| *key).collect();
    Ok(keys.len() == expected.len()
        && keys
            .iter()
            .zip(expected)
            .all(|((name, descending, collation, _), expected_name)| {
                name.as_deref() == Some(*expected_name) && !descending && collation == "BINARY"
            }))
}

fn verify_unique_columns(tx: &Transaction<'_>, table: &str, columns: &[&str]) -> Result<()> {
    for (index, unique, partial) in index_list(tx, table)? {
        if unique && !partial && index_columns_match(tx, &index, columns)? {
            return Ok(());
        }
    }
    bail!("schema 6 表 {table} 的 {:?} 缺少完整 UNIQUE 约束", columns)
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

fn verify_event_columns(tx: &Transaction<'_>) -> Result<()> {
    verify_columns(
        tx,
        "events",
        &[
            ColumnShape {
                name: "seq",
                declaration: "INTEGER",
                not_null: false,
                primary_key: 1,
            },
            column("task_id", true, 0),
            column("kind", true, 0),
            column("data", true, 0),
            integer_column("at"),
            integer_column("schema_version"),
            column("cursor", true, 0),
            column("session_id", false, 0),
            column("turn_id", false, 0),
        ],
    )
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

    pub(crate) fn insert_project(&self, project: &Project) -> Result<()> {
        secrets::validate_persisted_id("project_id", &project.id.0)?;
        secrets::safe_metadata_text("project_name", &project.name)?;
        secrets::safe_metadata_path("project_path", &project.root_path)?;
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

    pub(crate) fn insert_session(&self, session: &Session) -> Result<()> {
        for (field, value) in [
            ("session_id", session.id.0.as_str()),
            ("project_id", session.project_id.0.as_str()),
            ("legacy_run_id", session.legacy_run_id.as_str()),
        ] {
            secrets::validate_persisted_id(field, value)?;
        }
        secrets::safe_metadata_text("session_title", &session.title)?;
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

    pub(crate) fn insert_agent(&self, agent: &Agent) -> Result<()> {
        secrets::validate_persisted_id("agent_id", &agent.id.0)?;
        secrets::validate_persisted_id("agent_session_id", &agent.session_id.0)?;
        secrets::safe_metadata_text("agent_display_name", &agent.display_name)?;
        secrets::safe_metadata_text("agent_role", &agent.role)?;
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

    pub(crate) fn legacy_run(&self, id: &str) -> Result<Run> {
        let (title, kind, created_at): (String, String, u64) = self
            .conn
            .query_row(
                "SELECT title,kind,created_at FROM runs WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .context("旧幂等记录指向不存在的运行")?;
        let kind = serde_json::from_value(Value::String(kind)).context("旧运行的会话类型无效")?;
        let mut run = Run {
            id: id.to_owned(),
            title,
            kind,
            created_at,
            tasks: Vec::new(),
        };
        let mut stmt = self
            .conn
            .prepare("SELECT id,run_id,value FROM tasks WHERE run_id=?1 ORDER BY rowid")?;
        let rows = stmt.query_map([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (task_id, run_id, value) = row?;
            let task: Task = serde_json::from_str(&value).context("旧任务 JSON 无效")?;
            anyhow::ensure!(
                task.id == task_id && task.run_id == run_id && run_id == id,
                "旧任务 JSON 身份与数据库关系不一致"
            );
            // Historic task blobs predate the current persistence boundary.
            // Apply the same known-field/token redaction before returning them.
            run.tasks
                .push(serde_json::from_value(safe_task_value(&task)?)?);
        }
        Ok(run)
    }

    pub fn agent_lookup(&self, id: &AgentId) -> Result<std::result::Result<Agent, ChatTurnError>> {
        let row: Option<(String, String, String, String, i64)> = self
            .conn
            .query_row(
                "SELECT id,session_id,display_name,role,created_at FROM agents WHERE id=?1",
                [&id.0],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, session_id, display_name, role, created_at)) = row else {
            return Ok(Err(ChatTurnError::NotFound));
        };
        if created_at < 0 {
            return Ok(Err(ChatTurnError::CorruptState));
        }
        Ok(Ok(Agent {
            id: AgentId(id),
            session_id: SessionId(session_id),
            display_name,
            role,
            created_at: created_at as u64,
        }))
    }

    pub fn session_lookup(
        &self,
        id: &SessionId,
    ) -> Result<std::result::Result<Session, ChatTurnError>> {
        match self.session(id) {
            Ok(session) => Ok(Ok(session)),
            Err(error) if error.to_string().contains("不存在") => {
                Ok(Err(ChatTurnError::NotFound))
            }
            Err(error) => Err(error),
        }
    }

    pub fn turn_lookup(&self, id: &TurnId) -> Result<std::result::Result<Turn, ChatTurnError>> {
        match self.turn(id) {
            Ok(turn) => Ok(Ok(turn)),
            Err(error) if error.to_string().contains("不存在") => {
                Ok(Err(ChatTurnError::NotFound))
            }
            Err(error) => Err(error),
        }
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
    pub(crate) fn commit_turn(
        tx: &Transaction<'_>,
        turn: &Turn,
        tasks: &[TurnTask],
        idempotency_key: Option<&str>,
        opening_event: &Event,
    ) -> Result<()> {
        anyhow::ensure!(!tasks.is_empty(), "回合至少需要一个任务");
        anyhow::ensure!(
            tasks
                .iter()
                .all(|task| task.turn_id == turn.id && task.session_id == turn.session_id),
            "任务必须属于提交的回合和会话"
        );
        anyhow::ensure!(
            turn.idempotency_key.as_deref() == idempotency_key,
            "幂等 key 与回合不一致"
        );
        anyhow::ensure!(
            opening_event.turn_id.as_deref() == Some(turn.id.0.as_str())
                && opening_event.session_id.as_deref() == Some(turn.session_id.0.as_str()),
            "首事件必须属于提交的回合和会话"
        );
        tx.execute_batch("SAVEPOINT commit_domain_turn")?;
        let result = (|| {
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
        })();
        if result.is_err() {
            tx.execute_batch("ROLLBACK TO commit_domain_turn; RELEASE commit_domain_turn")?;
        } else {
            tx.execute_batch("RELEASE commit_domain_turn")?;
        }
        result
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

    /// Latest committed turn for append/resume guards.
    ///
    /// `turns_for_session` orders by second-precision `created_at` then id, and
    /// a UUID is not a commit order. Commit order is the minimum event `seq`
    /// written for each turn in its creation transaction. A later event on an
    /// older turn must not move that turn ahead of a successor. Every turn in
    /// the session must have at least one owned event; a missing one is corrupt
    /// rather than skipped. This does not prove a deleted opening event when
    /// later events remain.
    pub fn latest_committed_turn(&self, session_id: &SessionId) -> Result<Option<Turn>> {
        let turn_count: i64 = self.conn.query_row(
            "SELECT count(*) FROM turns WHERE session_id=?1",
            [&session_id.0],
            |row| row.get(0),
        )?;
        if turn_count == 0 {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare(
            "SELECT t.id, MIN(e.seq) AS opening_seq FROM turns t LEFT JOIN events e ON e.turn_id=t.id AND e.session_id=t.session_id WHERE t.session_id=?1 GROUP BY t.id ORDER BY opening_seq DESC",
        )?;
        let rows = stmt.query_map([&session_id.0], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
        })?;
        let mut ranked = Vec::new();
        for row in rows {
            ranked.push(row?);
        }
        if ranked.len() as i64 != turn_count || ranked.iter().any(|(_, seq)| seq.is_none()) {
            return Err(ChatTurnError::CorruptState.into());
        }
        let turn_id = ranked
            .first()
            .map(|(id, _)| id.clone())
            .context("回合缺少所属会话的首条事件，不能判断提交顺序")?;
        Ok(Some(self.turn(&TurnId(turn_id))?))
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

    pub(crate) fn update_turn_status(
        &self,
        id: &TurnId,
        status: LifecycleStatus,
        at: u64,
    ) -> Result<()> {
        let current: String = self
            .conn
            .query_row("SELECT status FROM turns WHERE id=?1", [&id.0], |row| {
                row.get(0)
            })
            .context("回合不存在")?;
        let current = parse_status(current)?;
        anyhow::ensure!(
            current.can_transition(status),
            "非法回合状态转换：{} -> {}",
            current.as_str(),
            status.as_str()
        );
        let changed = self.conn.execute(
            "UPDATE turns SET status=?2, updated_at=?3 WHERE id=?1",
            params![id.0, status_text(status), at],
        )?;
        anyhow::ensure!(changed == 1, "回合不存在");
        Ok(())
    }

    pub(crate) fn update_task_status(
        &self,
        id: &TaskId,
        status: LifecycleStatus,
        at: u64,
    ) -> Result<()> {
        let current: String = self
            .conn
            .query_row(
                "SELECT status FROM turn_tasks WHERE id=?1",
                [&id.0],
                |row| row.get(0),
            )
            .context("执行任务不存在")?;
        let current = parse_status(current)?;
        anyhow::ensure!(
            current.can_transition(status),
            "非法任务状态转换：{} -> {}",
            current.as_str(),
            status.as_str()
        );
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

    fn replace_dependencies(
        tx: &Transaction<'_>,
        task_id: &TaskId,
        depends_on: &[TaskId],
    ) -> Result<()> {
        let turn_id: String = tx.query_row(
            "SELECT turn_id FROM turn_tasks WHERE id=?1",
            [&task_id.0],
            |row| row.get(0),
        )?;
        let mut unique = std::collections::HashSet::new();
        for dependency in depends_on {
            anyhow::ensure!(unique.insert(dependency), "依赖任务重复");
            anyhow::ensure!(dependency != task_id, "任务不能依赖自己");
            let dependency_turn: String = tx.query_row(
                "SELECT turn_id FROM turn_tasks WHERE id=?1",
                [&dependency.0],
                |row| row.get(0),
            )?;
            anyhow::ensure!(dependency_turn == turn_id, "任务只能依赖同一回合中的任务");
        }
        tx.execute_batch("SAVEPOINT replace_task_dependencies")?;
        let result = (|| {
            tx.execute(
                "DELETE FROM turn_task_dependencies WHERE task_id=?1",
                [&task_id.0],
            )?;
            for dependency in depends_on {
                tx.execute(
                    "INSERT INTO turn_task_dependencies(task_id,depends_on_task_id) VALUES (?1,?2)",
                    params![task_id.0, dependency.0],
                )?;
            }
            ensure_acyclic(tx, &TurnId(turn_id))?;
            Ok(())
        })();
        if result.is_err() {
            tx.execute_batch(
                "ROLLBACK TO replace_task_dependencies; RELEASE replace_task_dependencies",
            )?;
        } else {
            tx.execute_batch("RELEASE replace_task_dependencies")?;
        }
        result
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
    for (field, value) in [
        ("turn_id", turn.id.0.as_str()),
        ("session_id", turn.session_id.0.as_str()),
        ("project_id", turn.project_id.0.as_str()),
        ("request_hash", turn.request_hash.as_str()),
    ] {
        secrets::validate_persisted_id(field, value)?;
    }
    if let Some(key) = &turn.idempotency_key {
        secrets::validate_idempotency_key(key)?;
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedTask {
    id: String,
    turn_id: String,
    session_id: String,
    legacy_task_id: String,
}

fn resolve_task_turn(tx: &Transaction<'_>, task_id: &str) -> Result<Option<ResolvedTask>> {
    let by_id: Option<ResolvedTask> = tx
        .query_row(
            "SELECT id,turn_id,session_id,legacy_task_id FROM turn_tasks WHERE id=?1",
            [task_id],
            |row| {
                Ok(ResolvedTask {
                    id: row.get(0)?,
                    turn_id: row.get(1)?,
                    session_id: row.get(2)?,
                    legacy_task_id: row.get(3)?,
                })
            },
        )
        .optional()?;
    let by_legacy: Vec<ResolvedTask> = {
        let mut stmt = tx.prepare(
            "SELECT id,turn_id,session_id,legacy_task_id FROM turn_tasks WHERE legacy_task_id=?1",
        )?;
        let rows = stmt.query_map([task_id], |row| {
            Ok(ResolvedTask {
                id: row.get(0)?,
                turn_id: row.get(1)?,
                session_id: row.get(2)?,
                legacy_task_id: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    anyhow::ensure!(
        by_legacy.len() <= 1,
        "legacy_task_id 重复，任务引用无法确定"
    );
    let resolved = match (by_id, by_legacy.first()) {
        (Some(id_task), Some(legacy_task)) if id_task.id != legacy_task.id => {
            bail!("任务引用歧义：id 与 legacy_task_id 指向不同任务")
        }
        (Some(id_task), _) => Some(id_task),
        (None, Some(legacy_task)) => Some(legacy_task.clone()),
        (None, None) => None,
    };
    if let Some(task) = &resolved {
        let collision: i64 = tx.query_row(
            "SELECT count(*) FROM tasks WHERE id=?1 AND id<>?2",
            params![task_id, task.legacy_task_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            collision == 0,
            "任务引用歧义：新任务 ID 与其他旧任务 ID 冲突"
        );
    }
    Ok(resolved)
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
    for (field, value) in [
        ("turn_task_id", task.id.0.as_str()),
        ("turn_id", task.turn_id.0.as_str()),
        ("session_id", task.session_id.0.as_str()),
        ("agent_id", task.agent_id.0.as_str()),
        ("legacy_task_id", task.legacy_task_id.as_str()),
    ] {
        secrets::validate_persisted_id(field, value)?;
    }
    let collision: i64 = tx.query_row(
        "SELECT count(*) FROM turn_tasks WHERE id=?1 OR legacy_task_id=?2 OR id=?2",
        params![task.legacy_task_id, task.id.0],
        |row| row.get(0),
    )?;
    anyhow::ensure!(collision == 0, "任务 ID 与已有 legacy_task_id 冲突");
    let legacy_collision: i64 = tx.query_row(
        "SELECT count(*) FROM tasks WHERE id=?1 AND id<>?2",
        params![task.id.0, task.legacy_task_id],
        |row| row.get(0),
    )?;
    anyhow::ensure!(legacy_collision == 0, "新任务 ID 与其他旧任务 ID 冲突");
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
    validate_event_kind(&event.kind)?;
    anyhow::ensure!(event.seq == 0, "新事件的 seq 必须由数据库分配");
    anyhow::ensure!(
        event.schema_version == EVENT_SCHEMA_VERSION,
        "新事件的 schema_version 无效"
    );
    let resolved = resolve_task_turn(tx, &event.task_id)?;
    let persisted_task_id = resolved
        .as_ref()
        .map(|task| task.legacy_task_id.as_str())
        .unwrap_or(&event.task_id);
    if let Some(task) = &resolved {
        anyhow::ensure!(
            event.session_id.as_deref() == Some(task.session_id.as_str()),
            "新任务事件必须关联所属会话"
        );
        let projection_matches: i64 = tx.query_row(
            "SELECT count(*) FROM tasks t JOIN sessions s ON s.legacy_run_id=t.run_id WHERE t.id=?1 AND s.id=?2",
            params![persisted_task_id, task.session_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(projection_matches == 1, "事件任务的旧投影不属于该会话");
    }
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
        anyhow::ensure!(
            resolved
                .as_ref()
                .is_some_and(|task| task.turn_id == *turn_id),
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
                [persisted_task_id],
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
            [persisted_task_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(exists == 1, "事件引用了不存在的任务");
    }
    let data = secrets::redact_persisted(&event.data);
    tx.execute(
        "INSERT INTO events(task_id,kind,data,at,schema_version,cursor,session_id,turn_id) VALUES (?1,?2,?3,?4,?5,'',?6,?7)",
        params![
            persisted_task_id,
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

#[cfg(test)]
mod schema_tests {
    use super::*;

    fn schema_5() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA user_version=5;
             CREATE TABLE schema_migrations(id TEXT PRIMARY KEY, applied_at INTEGER NOT NULL);
             CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL,
                 kind TEXT NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);",
        )
        .unwrap();
        db
    }

    fn migration_state(db: &Connection) -> (u32, bool) {
        (
            db.pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap(),
            migration_applied(db, MIGRATION_ID).unwrap(),
        )
    }

    fn migrate(db: &mut Connection) {
        let tx = db.transaction().unwrap();
        apply_schema_6(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(migration_state(db), (6, true));
    }

    fn reopen(db: &mut Connection) -> Result<()> {
        let tx = db.transaction()?;
        verify_current_schema(&tx)?;
        tx.commit()?;
        Ok(())
    }

    #[test]
    fn malformed_schema_never_advances_version_or_marker() {
        let cases = [
            (
                "projects id has no primary key",
                "id TEXT PRIMARY KEY,\n  name TEXT",
                "id TEXT,\n  name TEXT",
                "主键",
            ),
            (
                "root path has wrong type",
                "root_path TEXT NOT NULL",
                "root_path BLOB NOT NULL",
                "声明类型",
            ),
            (
                "agent role allows null",
                "role TEXT NOT NULL",
                "role TEXT",
                "NOT NULL",
            ),
            (
                "session omits foreign key",
                "project_id TEXT NOT NULL REFERENCES projects(id)",
                "project_id TEXT NOT NULL",
                "外键",
            ),
            (
                "session legacy run omits unique",
                "legacy_run_id TEXT NOT NULL UNIQUE",
                "legacy_run_id TEXT NOT NULL",
                "UNIQUE",
            ),
            (
                "dependency primary key is incomplete",
                "PRIMARY KEY (task_id, depends_on_task_id)",
                "PRIMARY KEY (task_id)",
                "主键",
            ),
            (
                "idempotency target foreign key is absent",
                "turn_id TEXT NOT NULL REFERENCES turns(id)",
                "turn_id TEXT NOT NULL",
                "外键",
            ),
            (
                "idempotency request hash is nullable",
                "request_hash TEXT NOT NULL",
                "request_hash TEXT",
                "NOT NULL",
            ),
            (
                "primary key uses incompatible collation",
                "id TEXT PRIMARY KEY,\n  name TEXT",
                "id TEXT PRIMARY KEY COLLATE NOCASE,\n  name TEXT",
                "UNIQUE",
            ),
        ];
        for (label, original, replacement, expected) in cases {
            let mut db = schema_5();
            let ddl = SCHEMA_6_SQL.replace(original, replacement);
            assert_ne!(ddl, SCHEMA_6_SQL, "fixture must mutate: {label}");
            db.execute_batch(&ddl).unwrap();
            let error = {
                let tx = db.transaction().unwrap();
                apply_schema_6(&tx).unwrap_err().to_string()
            };
            assert!(error.contains(expected), "{label}: {error}");
            assert_eq!(migration_state(&db), (5, false), "{label}");
        }
    }

    #[test]
    fn forged_named_indexes_are_rejected_even_with_valid_marker() {
        let index_definitions = [
            "CREATE INDEX turn_tasks_legacy_task_id_unique ON turn_tasks(legacy_task_id)",
            "CREATE UNIQUE INDEX turn_tasks_legacy_task_id_unique ON turn_tasks(legacy_task_id) WHERE status='running'",
            "CREATE UNIQUE INDEX turn_tasks_legacy_task_id_unique ON turn_tasks(id)",
            "CREATE UNIQUE INDEX turn_tasks_legacy_task_id_unique ON turn_tasks(legacy_task_id COLLATE NOCASE)",
            "CREATE UNIQUE INDEX turn_tasks_legacy_task_id_unique ON agents(id)",
        ];
        for definition in index_definitions {
            let mut db = schema_5();
            migrate(&mut db);
            db.execute_batch("DROP INDEX turn_tasks_legacy_task_id_unique;")
                .unwrap();
            db.execute_batch(definition).unwrap();
            assert!(reopen(&mut db).is_err(), "{definition}");
            assert_eq!(migration_state(&db), (6, true));
        }
    }

    #[test]
    fn session_unique_constraint_cannot_be_replaced_by_a_partial_index() {
        let mut db = schema_5();
        let ddl = SCHEMA_6_SQL.replace(
            "legacy_run_id TEXT NOT NULL UNIQUE",
            "legacy_run_id TEXT NOT NULL",
        );
        db.execute_batch(&ddl).unwrap();
        db.execute_batch(
            "CREATE UNIQUE INDEX fake_session_unique ON sessions(legacy_run_id) WHERE kind='team';",
        )
        .unwrap();
        let error = {
            let tx = db.transaction().unwrap();
            apply_schema_6(&tx).unwrap_err().to_string()
        };
        assert!(error.contains("UNIQUE"), "{error}");
        assert_eq!(migration_state(&db), (5, false));
    }

    fn seed_one_task(db: &Connection) {
        db.execute_batch(
            "INSERT INTO projects VALUES('project','project','D:/workspace',1,1);
             INSERT INTO sessions VALUES('session','project','team','session','run',1,1);
             INSERT INTO turns VALUES('turn','session','project','queued','hash',NULL,1,1);
             INSERT INTO agents VALUES('agent','session','agent','worker',1);
             INSERT INTO turn_tasks VALUES('task','turn','session','agent','legacy', 'queued',1,1);",
        ).unwrap();
    }

    #[test]
    fn marked_schema_6_repairs_only_the_missing_legacy_unique_index() {
        let mut db = schema_5();
        migrate(&mut db);
        seed_one_task(&db);
        db.execute_batch("DROP INDEX turn_tasks_legacy_task_id_unique;")
            .unwrap();
        reopen(&mut db).unwrap();
        reopen(&mut db).unwrap();
        assert!(db.execute_batch(
            "INSERT INTO turn_tasks VALUES('other','turn','session','agent','legacy','queued',1,1)",
        ).is_err());
        assert_eq!(migration_state(&db), (6, true));
        db.execute_batch("DROP INDEX sessions_project;").unwrap();
        assert!(
            reopen(&mut db)
                .unwrap_err()
                .to_string()
                .contains("sessions_project")
        );
    }

    #[test]
    fn missing_index_repair_refuses_duplicate_and_crossed_task_ids() {
        for (new_id, legacy_id, expected) in
            [("other", "legacy", "重复"), ("legacy", "other", "交叉碰撞")]
        {
            let mut db = schema_5();
            migrate(&mut db);
            seed_one_task(&db);
            db.execute_batch("DROP INDEX turn_tasks_legacy_task_id_unique;")
                .unwrap();
            db.execute(
                "INSERT INTO turn_tasks VALUES(?1,'turn','session','agent',?2,'queued',1,1)",
                params![new_id, legacy_id],
            )
            .unwrap();
            let error = reopen(&mut db).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
            assert_eq!(migration_state(&db), (6, true));
            let indexes: i64 = db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='turn_tasks_legacy_task_id_unique'", [], |row| row.get(0),
            ).unwrap();
            assert_eq!(
                indexes, 0,
                "failed validation must not leave a repaired index"
            );
        }
    }

    #[test]
    fn partial_schema_with_missing_indexes_retries_atomically() {
        let mut db = schema_5();
        db.execute_batch(SCHEMA_6_SQL).unwrap();
        db.execute_batch(
            "DROP INDEX sessions_project; DROP INDEX turn_tasks_legacy_task_id_unique;",
        )
        .unwrap();
        migrate(&mut db);
        reopen(&mut db).unwrap();
    }

    #[test]
    fn failed_marker_write_rolls_back_all_schema_changes() {
        let mut db = schema_5();
        db.execute_batch(
            "CREATE TRIGGER reject_marker BEFORE INSERT ON schema_migrations BEGIN SELECT RAISE(ABORT, 'marker failure'); END;",
        ).unwrap();
        {
            let tx = db.transaction().unwrap();
            assert!(apply_schema_6(&tx).is_err());
        }
        assert_eq!(migration_state(&db), (5, false));
        let tables: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='projects'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        let columns: i64 = db
            .query_row(
                "SELECT count(*) FROM pragma_table_info('events')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 5);
    }

    #[test]
    fn current_marker_does_not_hide_missing_tables_or_bad_event_columns() {
        let mut db = schema_5();
        migrate(&mut db);
        db.execute_batch("DROP TABLE turn_task_dependencies;")
            .unwrap();
        assert!(reopen(&mut db).unwrap_err().to_string().contains("缺少表"));
        assert_eq!(migration_state(&db), (6, true));

        let mut db = schema_5();
        db.execute_batch("ALTER TABLE events ADD COLUMN cursor BLOB NOT NULL DEFAULT '';")
            .unwrap();
        {
            let tx = db.transaction().unwrap();
            assert!(
                apply_schema_6(&tx)
                    .unwrap_err()
                    .to_string()
                    .contains("声明类型")
            );
        }
        assert_eq!(migration_state(&db), (5, false));
    }
}

#[cfg(test)]
mod task_identity_tests {
    use super::*;
    use crate::domain::stamped_event;
    use serde_json::json;

    fn database() -> Connection {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE schema_migrations(id TEXT PRIMARY KEY, applied_at INTEGER NOT NULL);
             CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE runs(id TEXT PRIMARY KEY, title TEXT NOT NULL, kind TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE tasks(id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), value TEXT NOT NULL);",
        ).unwrap();
        let tx = db.transaction().unwrap();
        apply_schema_6(&tx).unwrap();
        tx.commit().unwrap();
        db.execute_batch(
            "INSERT INTO projects VALUES('project','project','D:/workspace',1,1);
             INSERT INTO projects VALUES('other-project','other','D:/other',1,1);
             INSERT INTO sessions VALUES('session','project','team','session','run',1,1);
             INSERT INTO sessions VALUES('other-session','other-project','team','other','other-run',1,1);
             INSERT INTO turns VALUES('turn','session','project','queued','hash',NULL,1,1);
             INSERT INTO agents VALUES('agent','session','agent','worker',1);
             INSERT INTO runs VALUES('run','run','team',1);
             INSERT INTO runs VALUES('other-run','other','team',1);
             INSERT INTO tasks VALUES('legacy','run','{}');
             INSERT INTO tasks VALUES('old-only','other-run','{}');",
        ).unwrap();
        db
    }

    fn task() -> TurnTask {
        TurnTask {
            id: TaskId::from("canonical"),
            turn_id: TurnId::from("turn"),
            session_id: SessionId::from("session"),
            agent_id: AgentId::from("agent"),
            legacy_task_id: "legacy".into(),
            depends_on: Vec::new(),
            status: LifecycleStatus::Queued,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn new_task_id_cannot_claim_an_unrelated_legacy_task() {
        let mut db = database();
        let tx = db.transaction().unwrap();
        let mut task = task();
        task.id = TaskId::from("old-only");
        let error = insert_turn_task_tx(&tx, &task).unwrap_err().to_string();
        assert!(error.contains("其他旧任务"), "{error}");
        assert_eq!(
            tx.query_row("SELECT count(*) FROM turn_tasks", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        // The canonical ID may intentionally equal its own legacy projection.
        task.id = TaskId::from("legacy");
        insert_turn_task_tx(&tx, &task).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn events_normalize_task_ids_and_preserve_session_and_project_scope() {
        let mut db = database();
        let tx = db.transaction().unwrap();
        insert_turn_task_tx(&tx, &task()).unwrap();
        for task_id in ["canonical", "legacy"] {
            let event = stamped_event(
                0,
                "session",
                Some("turn"),
                task_id,
                "delta",
                json!({"text":"visible"}),
                1,
            );
            insert_event_tx(&tx, &event).unwrap();
        }
        let repo = Repository::new(&tx);
        let events = repo.events_after(&SessionId::from("session"), 0).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event.task_id == "legacy"));
        assert!(
            repo.events_after(&SessionId::from("other-session"), 0)
                .unwrap()
                .is_empty()
        );
        assert_eq!(tx.query_row(
            "SELECT count(*) FROM events e JOIN tasks t ON t.id=e.task_id WHERE t.run_id='other-run'", [], |row| row.get::<_, i64>(0),
        ).unwrap(), 0);

        let mut event = stamped_event(0, "other-session", None, "canonical", "delta", json!({}), 1);
        assert!(insert_event_tx(&tx, &event).is_err());
        event.session_id = None;
        assert!(insert_event_tx(&tx, &event).is_err());
        event.session_id = Some("session".into());
        // Session-scoped events can also use the canonical task ID.
        insert_event_tx(&tx, &event).unwrap();
        tx.execute("UPDATE tasks SET run_id='other-run' WHERE id='legacy'", [])
            .unwrap();
        assert!(
            insert_event_tx(&tx, &event)
                .unwrap_err()
                .to_string()
                .contains("旧投影")
        );
        assert_eq!(
            tx.query_row("SELECT count(*) FROM events", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
    }

    #[test]
    fn event_resolution_rejects_preexisting_cross_table_identity_collision() {
        let mut db = database();
        let tx = db.transaction().unwrap();
        // Simulate rows written by an older version which lacked the guard.
        tx.execute_batch("INSERT INTO turn_tasks VALUES('old-only','turn','session','agent','legacy','queued',1,1);").unwrap();
        let event = stamped_event(
            0,
            "session",
            Some("turn"),
            "old-only",
            "delta",
            json!({}),
            1,
        );
        assert!(
            insert_event_tx(&tx, &event)
                .unwrap_err()
                .to_string()
                .contains("引用歧义")
        );
        assert_eq!(
            tx.query_row("SELECT count(*) FROM events", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn repository_uses_path_and_http_idempotency_limits() {
        let mut db = database();
        let tx = db.transaction().unwrap();
        let project = Project {
            id: ProjectId::from("long-path"),
            name: "long path".into(),
            root_path: format!("D:/{}", "folder/".repeat(100)),
            created_at: 1,
            updated_at: 1,
        };
        Repository::new(&tx).insert_project(&project).unwrap();
        let mut turn = Turn {
            id: TurnId::from("key-boundary"),
            session_id: SessionId::from("session"),
            project_id: ProjectId::from("project"),
            status: LifecycleStatus::Queued,
            request_hash: "hash".into(),
            idempotency_key: Some("k".repeat(200)),
            created_at: 1,
            updated_at: 1,
        };
        insert_turn_tx(&tx, &turn).unwrap();
        turn.id = TurnId::from("key-too-long");
        turn.idempotency_key = Some("k".repeat(201));
        assert!(insert_turn_tx(&tx, &turn).is_err());
    }

    #[test]
    fn legacy_replay_checks_relational_identity_and_redacts_historical_tasks() {
        let db = database();
        let original = json!({
            "id":"legacy", "run_id":"run", "workspace":"D:/workspace",
            "spec":{"name":"worker", "role":"developer", "route_id":"route", "prompt":"hello"},
            "route":{"id":"route", "name":"route", "base_url":"https://example.com", "model":"model"},
            "status":"completed", "output":"Authorization: Bearer historical-bearer-token", "error":null,
            "messages":[{"APIKey":"historical-field-secret", "content":"ordinary content"}],
            "usage":{}, "created_at":1, "updated_at":1
        });
        db.execute(
            "UPDATE tasks SET value=?1 WHERE id='legacy'",
            [original.to_string()],
        )
        .unwrap();
        let run = Repository::new(&db).legacy_run("run").unwrap();
        assert_eq!(run.tasks.len(), 1);
        assert_eq!(run.tasks[0].id, "legacy");
        assert_eq!(run.tasks[0].messages[0]["content"], "ordinary content");
        let result = serde_json::to_string(&run).unwrap();
        assert!(!result.contains("historical-field-secret"));
        assert!(!result.contains("historical-bearer-token"));
        for (field, mismatched) in [("id", "different-task"), ("run_id", "other-run")] {
            let mut corrupted = original.clone();
            corrupted[field] = Value::String(mismatched.into());
            db.execute(
                "UPDATE tasks SET value=?1 WHERE id='legacy'",
                [corrupted.to_string()],
            )
            .unwrap();
            assert!(
                Repository::new(&db)
                    .legacy_run("run")
                    .unwrap_err()
                    .to_string()
                    .contains("关系不一致")
            );
        }
    }
}
