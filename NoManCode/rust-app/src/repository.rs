//! Project, session, turn, and forward-migration repository.
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
pub const APPROVAL_MIGRATION_ID: &str = "tool-call-approval-repository";
pub const WORKSPACE_CHANGE_MIGRATION_ID: &str = "workspace-change-repository";
pub const CHECKPOINT_MIGRATION_ID: &str = "task-before-checkpoint-repository";
pub const NETWORK_MIGRATION_ID: &str = "network-authorization-repository";
pub const PROJECT_CONFIG_MIGRATION_ID: &str = "project-config-root-binding-v1";
pub const PLUGIN_MANAGEMENT_MIGRATION_ID: &str = "builtin-plugin-management-v1";
pub const SHELL_MANAGEMENT_MIGRATION_ID: &str = "builtin-shell-management-v1";
pub const WORKTREE_MIGRATION_ID: &str = "git-worktree-operations-v1";
pub const BUDGET_MIGRATION_ID: &str = "completion-budget-v1";
pub const HOST_CONFIG_MIGRATION_ID: &str = "host-route-configuration-v1";
pub const RUNTIME_MANAGEMENT_MIGRATION_ID: &str = "builtin-runtime-management-v1";
pub(crate) const EXECUTION_OWNER_MIGRATION_ID: &str = "execution-owner-v18";

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

const EVENT_ENVELOPE_COLUMNS: &[(&str, &str)] = &[
    ("schema_version", "INTEGER NOT NULL DEFAULT 0"),
    ("cursor", "TEXT NOT NULL DEFAULT ''"),
    ("session_id", "TEXT"),
    ("turn_id", "TEXT"),
];

pub fn apply_schema_6(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(SCHEMA_6_SQL)?;
    ensure_event_columns(tx)?;
    tx.execute(
        "UPDATE events SET cursor = CAST(seq AS TEXT) WHERE cursor = '' OR cursor IS NULL",
        [],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 6)?;
    Ok(())
}

fn ensure_event_columns(tx: &Transaction<'_>) -> Result<()> {
    let existing: Vec<String> = {
        let mut stmt = tx.prepare("PRAGMA table_info(events)")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (name, declaration) in EVENT_ENVELOPE_COLUMNS {
        if !existing.iter().any(|col| col == name) {
            tx.execute_batch(&format!(
                "ALTER TABLE events ADD COLUMN {name} {declaration}"
            ))?;
        }
    }
    Ok(())
}

pub fn apply_schema_7(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS approvals (
          id TEXT NOT NULL PRIMARY KEY,
          tool_call_id TEXT NOT NULL,
          task_id TEXT NOT NULL,
          turn_id TEXT,
          session_id TEXT,
          tool_name TEXT NOT NULL,
          args_digest TEXT NOT NULL,
          binding_digest TEXT NOT NULL,
          workspace TEXT NOT NULL,
          write_scopes TEXT NOT NULL,
          allow_commands INTEGER NOT NULL CHECK (allow_commands IN (0,1)),
          preview TEXT NOT NULL,
          status TEXT NOT NULL CHECK (status IN ('pending','approved','denied','cancelled')),
          execution_state TEXT NOT NULL CHECK (execution_state IN ('not_started','claimed','finished','unknown','cancelled')),
          created_at INTEGER NOT NULL,
          decided_at INTEGER,
          decided_by TEXT,
          FOREIGN KEY(task_id) REFERENCES tasks(id)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS approvals_task_call_unique ON approvals(task_id, tool_call_id);
        CREATE INDEX IF NOT EXISTS approvals_task_status ON approvals(task_id, status);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![APPROVAL_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 7)?;
    Ok(())
}

pub fn apply_schema_8(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_changes (
          id TEXT NOT NULL PRIMARY KEY,
          task_id TEXT NOT NULL REFERENCES tasks(id),
          tool_call_id TEXT NOT NULL,
          workspace_digest TEXT NOT NULL,
          binding_digest TEXT NOT NULL,
          write_scopes TEXT NOT NULL,
          path TEXT NOT NULL,
          path_key TEXT NOT NULL,
          kind TEXT NOT NULL CHECK (kind IN ('created','modified')),
          before_blob BLOB,
          before_digest TEXT,
          after_digest TEXT,
          state TEXT NOT NULL CHECK (state IN ('prepared','finished','unknown','failed')),
          restore_state TEXT NOT NULL CHECK (restore_state IN ('pending','restored','unknown')),
          created_at INTEGER NOT NULL,
          finished_at INTEGER,
          UNIQUE(task_id, tool_call_id, path_key),
          CHECK ((kind='created' AND before_blob IS NULL AND before_digest IS NULL) OR
                 (kind='modified' AND before_blob IS NOT NULL AND before_digest IS NOT NULL))
        );
        CREATE TABLE IF NOT EXISTS workspace_restores (
          id TEXT NOT NULL PRIMARY KEY,
          task_id TEXT NOT NULL REFERENCES tasks(id),
          status TEXT NOT NULL CHECK (status IN ('claimed','complete','conflict','partial','unknown')),
          created_at INTEGER NOT NULL,
          finished_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS workspace_restore_outcomes (
          restore_id TEXT NOT NULL REFERENCES workspace_restores(id),
          change_id TEXT NOT NULL REFERENCES workspace_changes(id),
          path TEXT NOT NULL,
          status TEXT NOT NULL CHECK (status IN ('claimed','complete','conflict','unknown')),
          updated_at INTEGER NOT NULL,
          PRIMARY KEY(restore_id, change_id)
        );
        CREATE INDEX IF NOT EXISTS workspace_changes_task_path ON workspace_changes(task_id, path_key, created_at);
        CREATE UNIQUE INDEX IF NOT EXISTS workspace_restores_task_complete ON workspace_restores(task_id) WHERE status='complete';
        CREATE INDEX IF NOT EXISTS workspace_restore_outcomes_restore ON workspace_restore_outcomes(restore_id);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![WORKSPACE_CHANGE_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 8)?;
    Ok(())
}

pub fn apply_schema_9(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS checkpoints (
          id TEXT NOT NULL PRIMARY KEY,
          task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id),
          creation_key TEXT NOT NULL,
          kind TEXT NOT NULL CHECK (kind='task_before'),
          generation INTEGER NOT NULL CHECK (generation=1),
          created_at INTEGER NOT NULL CHECK (created_at>=0),
          manifest_digest TEXT NOT NULL CHECK (length(manifest_digest)=64),
          manifest TEXT NOT NULL
        );",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![CHECKPOINT_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 9)?;
    Ok(())
}

pub fn apply_schema_10(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS network_subjects (subject TEXT NOT NULL PRIMARY KEY, snapshot TEXT NOT NULL, credential_blob BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS network_authorizations (id TEXT NOT NULL PRIMARY KEY, subject TEXT NOT NULL REFERENCES network_subjects(subject), config_revision INTEGER NOT NULL CHECK(config_revision>0), credential_revision INTEGER NOT NULL CHECK(credential_revision>0), binding_digest TEXT NOT NULL CHECK(length(binding_digest)=64), purposes TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('active','revoked','stale')), created_at INTEGER NOT NULL CHECK(created_at>=0));
         CREATE TABLE IF NOT EXISTS network_confirmations (subject TEXT NOT NULL REFERENCES network_subjects(subject), creation_key TEXT NOT NULL, request_digest TEXT NOT NULL CHECK(length(request_digest)=64), authorization_id TEXT NOT NULL REFERENCES network_authorizations(id), PRIMARY KEY(subject,creation_key));
         CREATE TABLE IF NOT EXISTS task_network_bindings (task_id TEXT NOT NULL PRIMARY KEY REFERENCES tasks(id), authorization_id TEXT NOT NULL REFERENCES network_authorizations(id), binding_digest TEXT NOT NULL CHECK(length(binding_digest)=64));
         CREATE TABLE IF NOT EXISTS network_calls (id TEXT NOT NULL PRIMARY KEY, authorization_id TEXT NOT NULL REFERENCES network_authorizations(id), task_id TEXT REFERENCES tasks(id), state TEXT NOT NULL CHECK(state IN ('claimed','finished','unknown','not_sent')), receipt TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS command_network_ack (approval_id TEXT NOT NULL PRIMARY KEY REFERENCES approvals(id), binding_digest TEXT NOT NULL CHECK(length(binding_digest)=64), notice_version INTEGER NOT NULL CHECK(notice_version=1), created_at INTEGER NOT NULL CHECK(created_at>=0));
         CREATE TABLE IF NOT EXISTS network_events (seq INTEGER PRIMARY KEY AUTOINCREMENT, subject TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('confirmed','revoked','stale')), object_id TEXT NOT NULL, created_at INTEGER NOT NULL CHECK(created_at>=0));
         CREATE UNIQUE INDEX IF NOT EXISTS network_one_active ON network_authorizations(subject) WHERE state='active';",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![NETWORK_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 10)?;
    Ok(())
}

pub fn apply_schema_11(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS project_configs (project_id TEXT NOT NULL PRIMARY KEY REFERENCES projects(id), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0), value TEXT NOT NULL, permission_digest TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS project_root_bindings (project_id TEXT NOT NULL PRIMARY KEY REFERENCES projects(id), identity TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS project_selection (id INTEGER NOT NULL PRIMARY KEY CHECK(id=1), project_id TEXT REFERENCES project_configs(project_id), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0), CHECK((revision=0 AND project_id IS NULL) OR (revision>0 AND project_id IS NOT NULL)));
         CREATE TABLE IF NOT EXISTS task_project_bindings (task_id TEXT NOT NULL PRIMARY KEY REFERENCES tasks(id), project_id TEXT NOT NULL REFERENCES project_configs(project_id), value TEXT NOT NULL, provenance TEXT NOT NULL CHECK(provenance='native_v1'));
         CREATE TABLE IF NOT EXISTS project_registration_keys (key TEXT NOT NULL PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project_configs(project_id), request_hash TEXT NOT NULL);
         INSERT OR IGNORE INTO project_selection(id, project_id, revision) VALUES (1, NULL, 0);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![PROJECT_CONFIG_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 11)?;
    Ok(())
}

const PLUGIN_TABLES_FULL: &str = "
CREATE TABLE IF NOT EXISTS plugin_desired (
  plugin_id TEXT NOT NULL PRIMARY KEY CHECK(typeof(plugin_id)='text' AND plugin_id IN ('builtin.files','builtin.shell','builtin.chat-agent','builtin.provider-xpeach')),
  enabled INTEGER NOT NULL CHECK(typeof(enabled)='integer' AND enabled IN (0,1)),
  revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0),
  updated_at INTEGER NOT NULL CHECK(typeof(updated_at)='integer')
);
CREATE TABLE IF NOT EXISTS plugin_management_requests (
  key TEXT NOT NULL PRIMARY KEY CHECK(typeof(key)='text' AND length(key) BETWEEN 1 AND 200),
  plugin_id TEXT NOT NULL REFERENCES plugin_desired(plugin_id) CHECK(typeof(plugin_id)='text' AND plugin_id IN ('builtin.files','builtin.shell','builtin.chat-agent','builtin.provider-xpeach')),
  enabled INTEGER NOT NULL CHECK(typeof(enabled)='integer' AND enabled IN (0,1)),
  expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>=0 AND expected_revision<9223372036854775807),
  result_revision INTEGER NOT NULL CHECK(typeof(result_revision)='integer' AND result_revision=expected_revision+1),
  committed_at INTEGER NOT NULL CHECK(typeof(committed_at)='integer')
);
";

fn upgrade_plugin_tables_if_restricted(tx: &Transaction<'_>) -> Result<()> {
    let ddl: Option<String> = tx
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='plugin_desired'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(ddl) = ddl else {
        tx.execute_batch(PLUGIN_TABLES_FULL)?;
        return Ok(());
    };
    if ddl.contains("builtin.provider-xpeach") {
        return Ok(());
    }
    tx.execute_batch(
        "CREATE TEMP TABLE nhc_migrate_desired AS SELECT rowid AS saved_rowid,* FROM plugin_desired;
         CREATE TEMP TABLE nhc_migrate_requests AS SELECT rowid AS saved_rowid,* FROM plugin_management_requests;
         DROP TABLE plugin_management_requests;
         DROP TABLE plugin_desired;",
    )?;
    tx.execute_batch(PLUGIN_TABLES_FULL)?;
    tx.execute_batch(
        "INSERT INTO plugin_desired(rowid,plugin_id,enabled,revision,updated_at)
           SELECT saved_rowid,plugin_id,enabled,revision,updated_at FROM nhc_migrate_desired;
         INSERT INTO plugin_management_requests(rowid,key,plugin_id,enabled,expected_revision,result_revision,committed_at)
           SELECT saved_rowid,key,plugin_id,enabled,expected_revision,result_revision,committed_at FROM nhc_migrate_requests;
         DROP TABLE temp.nhc_migrate_requests;
         DROP TABLE temp.nhc_migrate_desired;",
    )?;
    Ok(())
}

pub(crate) fn apply_schema_12(tx: &Transaction<'_>) -> Result<()> {
    upgrade_plugin_tables_if_restricted(tx)?;
    tx.execute(
        "INSERT OR IGNORE INTO plugin_desired(plugin_id,enabled,revision,updated_at) VALUES ('builtin.files',1,0,?1)",
        [now()],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![PLUGIN_MANAGEMENT_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 12)?;
    Ok(())
}

pub(crate) fn apply_schema_13(tx: &Transaction<'_>) -> Result<()> {
    upgrade_plugin_tables_if_restricted(tx)?;
    let ts = now();
    tx.execute(
        "INSERT OR IGNORE INTO plugin_desired(plugin_id,enabled,revision,updated_at) VALUES ('builtin.shell',1,0,?1)",
        [ts],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![SHELL_MANAGEMENT_MIGRATION_ID, ts],
    )?;
    tx.pragma_update(None, "user_version", 13)?;
    Ok(())
}

pub(crate) fn apply_schema_14(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS worktree_operations (operation_id TEXT NOT NULL PRIMARY KEY, idempotency_key TEXT NOT NULL UNIQUE CHECK(typeof(idempotency_key)='text' AND length(idempotency_key) BETWEEN 1 AND 200), source_project_id TEXT NOT NULL REFERENCES project_configs(project_id), requested_project_id TEXT NOT NULL UNIQUE, request_json TEXT NOT NULL CHECK(typeof(request_json)='text'), request_digest TEXT NOT NULL CHECK(typeof(request_digest)='text' AND length(request_digest)=64), plan TEXT NOT NULL CHECK(typeof(plan)='text'), members TEXT NOT NULL CHECK(typeof(members)='text'), common_key TEXT NOT NULL CHECK(typeof(common_key)='text' AND length(common_key)=64), target_path TEXT NOT NULL UNIQUE, branch_ref TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('claimed','succeeded','rejected','unknown')), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0), receipt TEXT NOT NULL CHECK(typeof(receipt)='text'), UNIQUE(common_key,branch_ref));
         CREATE UNIQUE INDEX IF NOT EXISTS worktree_active_common ON worktree_operations(common_key) WHERE state IN ('claimed','unknown');
         CREATE TABLE IF NOT EXISTS worktree_project_reservations (project_id TEXT NOT NULL PRIMARY KEY REFERENCES project_configs(project_id), operation_id TEXT NOT NULL REFERENCES worktree_operations(operation_id));
         CREATE TABLE IF NOT EXISTS project_worktree_origins (project_id TEXT NOT NULL PRIMARY KEY REFERENCES project_configs(project_id), operation_id TEXT NOT NULL UNIQUE REFERENCES worktree_operations(operation_id), source_project_id TEXT NOT NULL REFERENCES project_configs(project_id), identity TEXT NOT NULL CHECK(typeof(identity)='text'));",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![WORKTREE_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 14)?;
    Ok(())
}

pub(crate) fn apply_schema_15(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS project_budgets (project_id TEXT NOT NULL PRIMARY KEY REFERENCES project_configs(project_id), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0), limit_tokens INTEGER NOT NULL CHECK(typeof(limit_tokens)='integer' AND limit_tokens>=0 AND limit_tokens<=1000000000000), reservation_tokens INTEGER NOT NULL CHECK(typeof(reservation_tokens)='integer' AND reservation_tokens>=1 AND reservation_tokens<=1000000000), coverage_start_call_rowid INTEGER NOT NULL CHECK(typeof(coverage_start_call_rowid)='integer' AND coverage_start_call_rowid>=0));
         CREATE TABLE IF NOT EXISTS budget_requests (key TEXT NOT NULL PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project_budgets(project_id), request TEXT NOT NULL CHECK(typeof(request)='text'), result TEXT NOT NULL CHECK(typeof(result)='text'), request_digest TEXT NOT NULL CHECK(length(request_digest)=64));
         CREATE TABLE IF NOT EXISTS budget_calls (call_id TEXT NOT NULL PRIMARY KEY REFERENCES network_calls(id), project_id TEXT NOT NULL REFERENCES project_budgets(project_id), budget_revision INTEGER NOT NULL CHECK(typeof(budget_revision)='integer' AND budget_revision>0), reserved_tokens INTEGER NOT NULL CHECK(typeof(reserved_tokens)='integer' AND reserved_tokens>=1 AND reserved_tokens<=1000000000), state TEXT NOT NULL CHECK(state IN ('claimed','settled','not_sent','usage_missing','usage_invalid','unknown','overrun')), record TEXT NOT NULL CHECK(typeof(record)='text'), terminal_fingerprint TEXT CHECK(terminal_fingerprint IS NULL OR length(terminal_fingerprint)=64), CHECK((state='claimed' AND terminal_fingerprint IS NULL) OR (state<>'claimed' AND terminal_fingerprint IS NOT NULL)));",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![BUDGET_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 15)?;
    Ok(())
}

pub(crate) fn apply_schema_16(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS host_config_state (id INTEGER NOT NULL PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0 AND revision<=9223372036854775807));
         CREATE TABLE IF NOT EXISTS host_config_requests (key TEXT NOT NULL PRIMARY KEY CHECK(typeof(key)='text' AND length(key) BETWEEN 1 AND 200), operation_id TEXT NOT NULL UNIQUE CHECK(typeof(operation_id)='text'), route_id TEXT NOT NULL CHECK(typeof(route_id)='text'), expected_revision INTEGER NOT NULL CHECK(typeof(expected_revision)='integer' AND expected_revision>=0), result_revision INTEGER NOT NULL CHECK(typeof(result_revision)='integer' AND result_revision>=expected_revision AND result_revision<=9223372036854775807), request_fingerprint_blob BLOB NOT NULL CHECK(typeof(request_fingerprint_blob)='blob' AND length(request_fingerprint_blob)>0), receipt TEXT NOT NULL CHECK(typeof(receipt)='text'), committed_at INTEGER NOT NULL CHECK(typeof(committed_at)='integer' AND committed_at>=0), CHECK(result_revision=expected_revision OR (expected_revision<9223372036854775807 AND result_revision=expected_revision+1)));
         INSERT OR IGNORE INTO host_config_state(id, revision) VALUES (1, 0);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![HOST_CONFIG_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 16)?;
    Ok(())
}

pub(crate) fn apply_schema_17(tx: &Transaction<'_>) -> Result<()> {
    upgrade_plugin_tables_if_restricted(tx)?;
    tx.execute_batch(
        "INSERT OR IGNORE INTO plugin_desired(plugin_id,enabled,revision,updated_at) VALUES ('builtin.chat-agent',1,0,0),('builtin.provider-xpeach',1,0,0);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![RUNTIME_MANAGEMENT_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 17)?;
    Ok(())
}

pub(crate) fn apply_schema_18(tx: &Transaction<'_>, identity: &str) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS execution_owner (id INTEGER PRIMARY KEY CHECK(id=1), domain_identity TEXT NOT NULL, epoch INTEGER NOT NULL CHECK(typeof(epoch)='integer' AND epoch>=0), phase TEXT NOT NULL CHECK(phase IN ('recovering','active')));",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO execution_owner(id,domain_identity,epoch,phase) VALUES(1,?1,0,'recovering')",
        [identity],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
        params![EXECUTION_OWNER_MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 18)?;
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

    pub(crate) fn approval_by_tool_call(
        &self,
        task_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<crate::approval::ApprovalRecord>> {
        let mut row = self
            .conn
            .query_row(
                "SELECT id,tool_call_id,task_id,turn_id,session_id,tool_name,args_digest,binding_digest,workspace,write_scopes,allow_commands,preview,status,execution_state,created_at,decided_at,decided_by \
                 FROM approvals WHERE task_id=?2 AND tool_call_id=?1",
                params![tool_call_id, task_id],
                approval_row,
            )
            .optional()
            .map_err(|_| crate::approval::ApprovalError::CorruptState {
                id: tool_call_id.into(),
            })?;
        if let Some(record) = &mut row {
            self.hydrate_network_ack(record)?;
        }
        Ok(row)
    }

    pub(crate) fn approval(&self, id: &str) -> Result<crate::approval::ApprovalRecord> {
        let mut record = self
            .conn
            .query_row(
                "SELECT id,tool_call_id,task_id,turn_id,session_id,tool_name,args_digest,binding_digest,workspace,write_scopes,allow_commands,preview,status,execution_state,created_at,decided_at,decided_by \
                 FROM approvals WHERE id=?1",
                [id],
                approval_row,
            )
            .optional()
            .map_err(|_| crate::approval::ApprovalError::CorruptState { id: id.into() })?
            .ok_or_else(|| crate::approval::ApprovalError::NotFound { id: id.to_owned() })?;
        self.hydrate_network_ack(&mut record)?;
        Ok(record)
    }

    pub(crate) fn approvals_for_task(
        &self,
        task_id: &str,
    ) -> Result<Vec<crate::approval::ApprovalRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,tool_call_id,task_id,turn_id,session_id,tool_name,args_digest,binding_digest,workspace,write_scopes,allow_commands,preview,status,execution_state,created_at,decided_at,decided_by \
             FROM approvals WHERE task_id=?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([task_id], approval_row)?;
        let mut records: Vec<_> = rows
            .collect::<rusqlite::Result<_>>()
            .map_err(|_| crate::approval::ApprovalError::CorruptState { id: task_id.into() })?;
        for record in &mut records {
            self.hydrate_network_ack(record)?;
        }
        Ok(records)
    }

    fn hydrate_network_ack(&self, record: &mut crate::approval::ApprovalRecord) -> Result<()> {
        use crate::approval::{ApprovalError, CommandNetworkAck, CommandNetworkNotice};
        let sidecar: Option<(String, u32)> = if !migration_applied(self.conn, NETWORK_MIGRATION_ID)?
        {
            None
        } else {
            self.conn
                .query_row(
                    "SELECT binding_digest,notice_version FROM command_network_ack WHERE approval_id=?1",
                    [&record.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
        };
        if let Some((digest, version)) = &sidecar
            && (record.tool_name != "run_command"
                || digest != &record.binding_digest
                || *version != 1)
        {
            return Err(ApprovalError::CorruptState {
                id: record.id.clone(),
            }
            .into());
        }
        if record.tool_name == "run_command" {
            record.network_notice = Some(CommandNetworkNotice::default());
            record.command_network_ack = if sidecar.is_some() {
                CommandNetworkAck::Acknowledged
            } else {
                CommandNetworkAck::Required
            };
        } else {
            record.network_notice = None;
            record.command_network_ack = CommandNetworkAck::NotRequired;
        }
        Ok(())
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
        classify_row(self.conn.query_row(
            "SELECT id,project_id,kind,title,legacy_run_id,created_at,updated_at FROM sessions WHERE id=?1",
            [&id.0],
            session_row,
        ))
    }

    pub fn turn_lookup(&self, id: &TurnId) -> Result<std::result::Result<Turn, ChatTurnError>> {
        classify_row(self.conn.query_row(
            "SELECT id,session_id,project_id,status,request_hash,idempotency_key,created_at,updated_at FROM turns WHERE id=?1",
            [&id.0],
            turn_row,
        ))
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

    pub(crate) fn append_completion_delta(tx: &Transaction<'_>, event: &Event) -> Result<i64> {
        anyhow::ensure!(
            event.kind == "delta"
                && event.data.as_object().is_some_and(|data| {
                    data.len() == 1 && data.get("text").is_some_and(Value::is_string)
                }),
            "可信 completion 入口只接受文本 delta"
        );
        insert_event_data_tx(tx, event, &event.data)
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
    let data = secrets::redact_persisted(&event.data);
    insert_event_data_tx(tx, event, &data)
}

fn insert_event_data_tx(tx: &Transaction<'_>, event: &Event, data: &Value) -> Result<i64> {
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

fn classify_row<T>(result: rusqlite::Result<T>) -> Result<std::result::Result<T, ChatTurnError>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Err(ChatTurnError::NotFound)),
        Err(error @ rusqlite::Error::FromSqlConversionFailure(..))
        | Err(error @ rusqlite::Error::IntegralValueOutOfRange(..)) => {
            Err(anyhow::Error::from(error).context(ChatTurnError::CorruptState))
        }
        Err(error) => Err(error.into()),
    }
}

fn approval_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::approval::ApprovalRecord> {
    Ok(crate::approval::ApprovalRecord {
        network_notice: None,
        command_network_ack: Default::default(),
        id: row.get(0)?,
        tool_call_id: row.get(1)?,
        task_id: row.get(2)?,
        turn_id: row.get(3)?,
        session_id: row.get(4)?,
        tool_name: row.get(5)?,
        args_digest: row.get(6)?,
        binding_digest: row.get(7)?,
        workspace: row.get(8)?,
        write_scopes: serde_json::from_str(&row.get::<_, String>(9)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        allow_commands: row.get::<_, i64>(10)? != 0,
        preview: row.get(11)?,
        status: {
            let value: String = row.get(12)?;
            crate::approval::ApprovalStatus::parse(&value).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    12,
                    rusqlite::types::Type::Text,
                    "审批状态无效".into(),
                )
            })?
        },
        execution_state: {
            let value: String = row.get(13)?;
            crate::approval::ExecutionState::parse(&value).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    13,
                    rusqlite::types::Type::Text,
                    "执行状态无效".into(),
                )
            })?
        },
        created_at: row.get(14)?,
        decided_at: row.get(15)?,
        decided_by: row.get(16)?,
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn forward_migrations_and_turn_crud_roundtrip() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE schema_migrations(id TEXT PRIMARY KEY, applied_at INTEGER NOT NULL);
             CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE runs(id TEXT PRIMARY KEY, title TEXT NOT NULL, kind TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE tasks(id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), value TEXT NOT NULL);",
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        apply_schema_6(&tx).unwrap();
        apply_schema_7(&tx).unwrap();
        apply_schema_8(&tx).unwrap();
        apply_schema_9(&tx).unwrap();
        apply_schema_10(&tx).unwrap();
        apply_schema_11(&tx).unwrap();
        apply_schema_12(&tx).unwrap();
        apply_schema_13(&tx).unwrap();
        apply_schema_14(&tx).unwrap();
        apply_schema_15(&tx).unwrap();
        apply_schema_16(&tx).unwrap();
        apply_schema_17(&tx).unwrap();
        apply_schema_18(&tx, "test-identity").unwrap();
        tx.commit().unwrap();

        let version: u32 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 18);

        let tx = db.transaction().unwrap();
        let repo = Repository::new(&tx);
        repo.insert_project(&Project {
            id: ProjectId("proj".into()),
            name: "Project".into(),
            root_path: "D:/repo".into(),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
        tx.execute("INSERT INTO runs VALUES('run','Chat','chat',1)", [])
            .unwrap();
        tx.execute("INSERT INTO tasks VALUES('task1','run','{}')", [])
            .unwrap();
        repo.insert_session(&Session {
            id: SessionId("sess".into()),
            project_id: ProjectId("proj".into()),
            kind: SessionKind::Chat,
            title: "Chat".into(),
            legacy_run_id: "run".into(),
            created_at: 1,
            updated_at: 1,
        })
        .unwrap();
        repo.insert_agent(&Agent {
            id: AgentId("agent".into()),
            session_id: SessionId("sess".into()),
            display_name: "coder".into(),
            role: "engineer".into(),
            created_at: 1,
        })
        .unwrap();
        let turn = Turn {
            id: TurnId("turn1".into()),
            session_id: SessionId("sess".into()),
            project_id: ProjectId("proj".into()),
            status: LifecycleStatus::Queued,
            request_hash: "h1".into(),
            idempotency_key: Some("k1".into()),
            created_at: 1,
            updated_at: 1,
        };
        let ttask = TurnTask {
            id: TaskId("task1".into()),
            turn_id: TurnId("turn1".into()),
            session_id: SessionId("sess".into()),
            agent_id: AgentId("agent".into()),
            legacy_task_id: "task1".into(),
            depends_on: vec![],
            status: LifecycleStatus::Queued,
            created_at: 1,
            updated_at: 1,
        };
        let opening = Event {
            schema_version: EVENT_SCHEMA_VERSION,
            seq: 0,
            cursor: String::new(),
            session_id: Some("sess".into()),
            turn_id: Some("turn1".into()),
            task_id: "task1".into(),
            kind: "status".into(),
            data: json!({"status":"queued"}),
            at: 1,
        };
        Repository::commit_turn(&tx, &turn, &[ttask], Some("k1"), &opening).unwrap();
        tx.commit().unwrap();

        let repo = Repository::new(&db);
        assert_eq!(
            repo.latest_committed_turn(&SessionId("sess".into()))
                .unwrap()
                .unwrap()
                .id,
            TurnId("turn1".into())
        );
        assert_eq!(
            repo.events_after(&SessionId("sess".into()), 0)
                .unwrap()
                .len(),
            1
        );
    }
}
