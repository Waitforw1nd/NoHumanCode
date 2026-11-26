//! Durable read-only exploration jobs scoped to their parent's workspace and session.
use crate::{
    domain::{Task, id, now},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) const MIGRATION_ID: &str = "durable-exploration-v19";

pub(crate) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("
        CREATE TABLE exploration_batches (
            id TEXT PRIMARY KEY, parent_task_id TEXT NOT NULL REFERENCES tasks(id),
            call_key TEXT NOT NULL, request_hash TEXT NOT NULL, session_id TEXT NOT NULL,
            project_id TEXT, workspace TEXT NOT NULL, UNIQUE(parent_task_id,call_key));
        CREATE INDEX exploration_session ON exploration_batches(session_id);
        CREATE UNIQUE INDEX exploration_submission ON exploration_batches(session_id,IFNULL(project_id,''),workspace,call_key)
            WHERE substr(call_key,1,7)='submit:';
        CREATE TABLE exploration_jobs (
            id TEXT PRIMARY KEY, batch_id TEXT NOT NULL REFERENCES exploration_batches(id),
            ordinal INTEGER NOT NULL, input TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('queued','running','succeeded','no_matches','failed','cancelled','unknown')),
            attempt INTEGER NOT NULL DEFAULT 0, result TEXT, UNIQUE(batch_id,ordinal));
        CREATE TABLE exploration_dependencies (
            job_id TEXT NOT NULL REFERENCES exploration_jobs(id),
            dependency_id TEXT NOT NULL REFERENCES exploration_jobs(id),
            PRIMARY KEY(job_id,dependency_id));
        CREATE TABLE exploration_attempts (
            job_id TEXT NOT NULL REFERENCES exploration_jobs(id), attempt INTEGER NOT NULL,
            status TEXT NOT NULL, result TEXT, usage TEXT NOT NULL DEFAULT '[]',
            projected INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(job_id,attempt));
        CREATE TABLE exploration_calls (
            job_id TEXT NOT NULL, attempt INTEGER NOT NULL, call_id TEXT PRIMARY KEY REFERENCES network_calls(id),
            FOREIGN KEY(job_id,attempt) REFERENCES exploration_attempts(job_id,attempt));
        CREATE INDEX exploration_attempt_calls ON exploration_calls(job_id,attempt);")?;
    tx.execute(
        "INSERT INTO schema_migrations(id,applied_at) VALUES (?1,?2)",
        params![MIGRATION_ID, now()],
    )?;
    tx.pragma_update(None, "user_version", 19)?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExplorationInput {
    pub question: String,
    pub path: String,
    pub query: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplorationJob {
    pub id: String,
    pub batch_id: String,
    pub session_id: String,
    pub parent_task_id: String,
    pub ordinal: usize,
    pub attempt: i64,
    pub status: String,
    pub input: ExplorationInput,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<ExplorationBlocker>,
    pub result: Option<Value>,
    pub usage: Vec<Value>,
    pub receipts: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplorationBlocker {
    pub job_id: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplorationBatch {
    pub id: String,
    pub jobs: Vec<ExplorationJob>,
}

fn parent_scope(db: &Connection, parent: &str) -> Result<(String, Option<String>, String)> {
    Ok(db.query_row(
        "SELECT COALESCE(s.id, t.run_id), COALESCE(pb.project_id, s.project_id), json_extract(t.value, '$.workspace')
         FROM tasks t
         LEFT JOIN sessions s ON s.legacy_run_id = t.run_id
         LEFT JOIN task_project_bindings pb ON pb.task_id = t.id
         WHERE t.id = ?1",
        [parent],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?)
}

fn job(db: &Connection, session: &str, id: &str) -> Result<ExplorationJob> {
    let (batch_id, parent_task_id, ordinal, attempt, status, input, result) = db
        .query_row(
            "SELECT j.batch_id, b.parent_task_id, j.ordinal, j.attempt, j.status, j.input, j.result
             FROM exploration_jobs j JOIN exploration_batches b ON b.id = j.batch_id
             WHERE b.session_id = ?1 AND j.id = ?2",
            params![session, id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?
        .context("未找到当前会话的探索子任务")?;
    let mut usage = Vec::new();
    let mut stmt =
        db.prepare("SELECT usage FROM exploration_attempts WHERE job_id=?1 ORDER BY attempt")?;
    for row in stmt.query_map([id], |r| r.get::<_, String>(0))? {
        usage.extend(serde_json::from_str::<Vec<Value>>(&row?)?);
    }
    let mut stmt = db.prepare(
        "SELECT c.attempt,c.call_id,n.receipt,b.record FROM exploration_calls c
        JOIN network_calls n ON n.id=c.call_id LEFT JOIN budget_calls b ON b.call_id=c.call_id
        WHERE c.job_id=?1 ORDER BY c.attempt,c.rowid",
    )?;
    let mut receipts = Vec::new();
    for row in stmt.query_map([id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })? {
        let (attempt, call_id, raw, budget) = row?;
        receipts.push(json!({"attempt":attempt,"call_id":call_id,"receipt":serde_json::from_str::<Value>(&raw)?,
            "budget":budget.as_deref().map(serde_json::from_str::<Value>).transpose()?}));
    }
    let input: ExplorationInput = serde_json::from_str(&input)?;
    let blocked_by = if status == "queued" && !input.depends_on.is_empty() {
        let mut stmt = db.prepare(
            "SELECT p.id,p.status FROM exploration_dependencies d
             JOIN exploration_jobs p ON p.id=d.dependency_id
             WHERE d.job_id=?1 AND p.status NOT IN ('succeeded','no_matches') ORDER BY d.rowid",
        )?;
        stmt.query_map([id], |row| {
            Ok(ExplorationBlocker {
                job_id: row.get(0)?,
                status: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    Ok(ExplorationJob {
        id: id.to_owned(),
        batch_id,
        session_id: session.to_owned(),
        parent_task_id,
        ordinal,
        attempt,
        status,
        input,
        blocked_by,
        result: result.as_deref().map(serde_json::from_str).transpose()?,
        usage,
        receipts,
    })
}

fn batch(db: &Connection, session: &str, id: &str) -> Result<ExplorationBatch> {
    let mut stmt = db.prepare(
        "SELECT j.id FROM exploration_jobs j JOIN exploration_batches b ON b.id = j.batch_id
         WHERE b.session_id = ?1 AND b.id = ?2 ORDER BY j.ordinal",
    )?;
    let jobs = stmt
        .query_map(params![session, id], |row| row.get::<_, String>(0))?
        .map(|result| job(db, session, &result?))
        .collect::<Result<Vec<_>>>()?;
    ensure!(!jobs.is_empty(), "未找到当前会话的探索批次");
    Ok(ExplorationBatch {
        id: id.into(),
        jobs,
    })
}

pub(crate) fn link_call_tx(
    tx: &Transaction<'_>,
    job_id: &str,
    attempt: i64,
    parent_task_id: &str,
    call_id: &str,
) -> Result<()> {
    let n=tx.execute("INSERT INTO exploration_calls(job_id,attempt,call_id)
        SELECT j.id,j.attempt,n.id FROM exploration_jobs j JOIN exploration_batches b ON b.id=j.batch_id
        JOIN network_calls n ON n.task_id=b.parent_task_id WHERE j.id=?1 AND j.attempt=?2 AND b.parent_task_id=?3
        AND j.status='running' AND n.id=?4",params![job_id,attempt,parent_task_id,call_id])?;
    ensure!(n == 1, "探索尝试已失效或回执归属不符");
    Ok(())
}

fn replay_batch(
    db: &Connection,
    parent: &str,
    key: &str,
    hash: Option<&str>,
    session: &str,
    project: Option<&str>,
    workspace: &str,
) -> Result<Option<ExplorationBatch>> {
    let found: Option<(String,String)> = db.query_row(
        "SELECT id,request_hash FROM exploration_batches WHERE call_key=?2 AND session_id=?3
         AND project_id IS ?4 AND workspace=?5 AND (parent_task_id=?1 OR substr(call_key,1,7)='submit:')",
        params![parent,key,session,project,workspace], |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    let Some((id, stored_hash)) = found else {
        return Ok(None);
    };
    ensure!(
        hash.is_none_or(|expected| expected == stored_hash),
        "探索调用标识的参数冲突"
    );
    Ok(Some(batch(db, session, &id)?))
}

impl Store {
    pub fn exploration_jobs_for_parent(&self, parent: &str) -> Result<Vec<ExplorationJob>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let (session, project, workspace) = parent_scope(&tx, parent)?;
        let mut stmt = tx.prepare(
            "SELECT j.id FROM exploration_jobs j JOIN exploration_batches b ON b.id=j.batch_id
             WHERE b.session_id=?1 AND b.project_id IS ?2 AND b.workspace=?3 ORDER BY b.rowid,j.ordinal",
        )?;
        stmt.query_map(params![session, project, workspace], |row| {
            row.get::<_, String>(0)
        })?
        .map(|id| job(&tx, &session, &id?))
        .collect()
    }

    pub fn exploration_job_for_parent(&self, parent: &str, id: &str) -> Result<ExplorationJob> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let (session, project, workspace) = parent_scope(&tx, parent)?;
        let visible: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM exploration_jobs j JOIN exploration_batches b ON b.id=j.batch_id
             WHERE j.id=?1 AND b.session_id=?2 AND b.project_id IS ?3 AND b.workspace=?4)",
            params![id, session, project, workspace], |row| row.get(0),
        )?;
        ensure!(visible, "未找到当前工作区和会话的探索子任务");
        job(&tx, &session, id)
    }

    pub fn exploration_batch_for_parent(&self, parent: &str, id: &str) -> Result<ExplorationBatch> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let (session, project, workspace) = parent_scope(&tx, parent)?;
        let visible: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM exploration_batches WHERE id=?1 AND session_id=?2 AND project_id IS ?3 AND workspace=?4)",
            params![id, session, project, workspace], |row| row.get(0),
        )?;
        ensure!(visible, "未找到当前工作区和会话的探索批次");
        batch(&tx, &session, id)
    }

    pub(crate) fn exploration_session(&self, parent: &str) -> Result<String> {
        Ok(parent_scope(&self.db.lock().unwrap(), parent)?.0)
    }
    pub(crate) fn exploration_job(&self, session: &str, id: &str) -> Result<ExplorationJob> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        job(&tx, session, id)
    }
    pub(crate) fn exploration_batch(&self, session: &str, id: &str) -> Result<ExplorationBatch> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        batch(&tx, session, id)
    }
    pub(crate) fn exploration_by_call(
        &self,
        parent: &str,
        key: &str,
        hash: Option<&str>,
    ) -> Result<Option<ExplorationBatch>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let (session, project, workspace) = parent_scope(&tx, parent)?;
        replay_batch(
            &tx,
            parent,
            key,
            hash,
            &session,
            project.as_deref(),
            &workspace,
        )
    }
    pub(crate) fn submit_exploration(
        &self,
        parent: &str,
        key: &str,
        hash: &str,
        inputs: &[ExplorationInput],
    ) -> Result<ExplorationBatch> {
        ensure!(
            (1..=4).contains(&inputs.len()) && !key.is_empty() && !hash.is_empty(),
            "探索提交无效"
        );
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (session, project, workspace) = parent_scope(&tx, parent)?;
        if let Some(saved) = replay_batch(
            &tx,
            parent,
            key,
            Some(hash),
            &session,
            project.as_deref(),
            &workspace,
        )? {
            return Ok(saved);
        }
        // Validate all edges against committed predecessors before inserting this batch.
        for input in inputs {
            ensure!(
                input.depends_on.len() <= 32
                    && !input.question.trim().is_empty()
                    && input.question.chars().count() <= 2048
                    && !input.path.is_empty()
                    && input.path.chars().count() <= 256
                    && !input.query.is_empty()
                    && input.query.len() <= 4096
                    && !input.query.contains(['\r', '\n', '\0']),
                "探索参数超出限制"
            );
            for dep in &input.depends_on {
                let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM exploration_jobs j JOIN exploration_batches b ON b.id=j.batch_id
                    WHERE j.id=?1 AND b.session_id=?2 AND b.project_id IS ?3 AND b.workspace=?4)",
                    params![dep,session,project,workspace],|r|r.get(0))?;
                ensure!(valid, "探索依赖不属于本会话工作区的较早批次");
            }
        }
        let batch_id = format!("batch-{}", id());
        tx.execute("INSERT INTO exploration_batches(id,parent_task_id,call_key,request_hash,session_id,project_id,workspace) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![batch_id,parent,key,hash,session,project,workspace])?;
        for (ordinal, input) in inputs.iter().enumerate() {
            let job_id = format!("job-{}", id());
            tx.execute("INSERT INTO exploration_jobs(id,batch_id,ordinal,input,status) VALUES (?1,?2,?3,?4,'queued')",
                params![job_id,batch_id,ordinal,serde_json::to_string(input)?])?;
            for dep in &input.depends_on {
                tx.execute(
                    "INSERT INTO exploration_dependencies(job_id,dependency_id) VALUES (?1,?2)",
                    params![job_id, dep],
                )?;
            }
        }
        let saved = batch(&tx, &session, &batch_id)?;
        tx.commit()?;
        Ok(saved)
    }
    pub(crate) fn claim_exploration(
        &self,
        session: &str,
        id: &str,
    ) -> Result<Option<ExplorationJob>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n=tx.execute("UPDATE exploration_jobs SET status='running',attempt=attempt+1 WHERE id=?1 AND status='queued'
            AND batch_id IN (SELECT id FROM exploration_batches WHERE session_id=?2)
            AND NOT EXISTS(SELECT 1 FROM exploration_dependencies d JOIN exploration_jobs p ON p.id=d.dependency_id
                WHERE d.job_id=?1 AND p.status NOT IN ('succeeded','no_matches'))",params![id,session])?;
        if n == 0 {
            return Ok(None);
        }
        tx.execute("INSERT INTO exploration_attempts(job_id,attempt,status) SELECT id,attempt,status FROM exploration_jobs WHERE id=?1",[id])?;
        let claimed = job(&tx, session, id)?;
        tx.commit()?;
        Ok(Some(claimed))
    }
    pub(crate) fn finish_exploration(
        &self,
        session: &str,
        id: &str,
        attempt: i64,
        status: &str,
        result: &Value,
        usage: &[Value],
    ) -> Result<()> {
        ensure!(
            matches!(
                status,
                "succeeded" | "failed" | "no_matches" | "cancelled" | "unknown"
            ),
            "无效的探索结束状态"
        );
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = job(&tx, session, id)?;
        ensure!(
            current.attempt == attempt && current.status == "running",
            "探索尝试已失效"
        );
        finish_tx(&tx, id, attempt, status, Some(result), usage)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn retry_exploration(&self, session: &str, id: &str) -> Result<ExplorationJob> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = job(&tx, session, id)?;
        ensure!(
            matches!(old.status.as_str(), "unknown" | "failed" | "cancelled"),
            "此探索子任务不能重试"
        );
        tx.execute(
            "UPDATE exploration_jobs SET status='queued',result=NULL WHERE id=?1",
            [id],
        )?;
        let saved = job(&tx, session, id)?;
        tx.commit()?;
        Ok(saved)
    }
    pub(crate) fn interrupt_exploration(
        &self,
        session: &str,
        id: &str,
        attempt: i64,
        reason: &str,
        cancelled: bool,
    ) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = job(&tx, session, id)?;
        ensure!(
            old.attempt == attempt && old.status == "running",
            "探索尝试已失效"
        );
        interrupt_tx(
            &tx,
            id,
            attempt,
            reason,
            if cancelled { "cancelled" } else { "failed" },
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn cancel_queued_explorations(&self, batch_id: &str) -> Result<()> {
        self.db.lock().unwrap().execute("UPDATE exploration_jobs SET status='cancelled', result=?1 WHERE batch_id=?2 AND status='queued'",
            params![json!({"error":"cancelled"}).to_string(),batch_id])?;
        Ok(())
    }
    pub(crate) fn project_exploration_usage(
        &self,
        parent: &mut Task,
        batch_id: &str,
    ) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw:String=tx.query_row("SELECT t.value FROM tasks t JOIN exploration_batches b ON b.parent_task_id=t.id WHERE b.id=?1 AND t.id=?2",
            params![batch_id,parent.id],|r|r.get(0))?;
        let mut saved: Task = serde_json::from_str(&raw)?;
        saved.usage = crate::store::merge_usage(&saved.usage, &parent.usage)?;
        project_usage_tx(&tx, &mut saved, Some(batch_id))?;
        tx.commit()?;
        parent.usage = saved.usage;
        Ok(())
    }

    pub(crate) fn project_inactive_exploration_usage(&self, parent: &str) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw: Option<String> = tx.query_row(
            "SELECT value FROM tasks WHERE id=?1 AND json_extract(value,'$.status') NOT IN ('queued','running')
             AND EXISTS(SELECT 1 FROM exploration_batches b JOIN exploration_jobs j ON j.batch_id=b.id
                JOIN exploration_attempts a ON a.job_id=j.id
                WHERE b.parent_task_id=?1 AND a.projected=0 AND a.status<>'running')",
            [parent], |r| r.get(0),
        ).optional()?;
        if let Some(raw) = raw {
            let mut task: Task = serde_json::from_str(&raw)?;
            project_usage_tx(&tx, &mut task, None)?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn project_usage_tx(tx: &Transaction<'_>, task: &mut Task, batch_id: Option<&str>) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT a.usage FROM exploration_batches b JOIN exploration_jobs j ON j.batch_id=b.id
         JOIN exploration_attempts a ON a.job_id=j.id WHERE b.parent_task_id=?1 AND (?2 IS NULL OR b.id=?2)
         AND a.projected=0 AND a.status<>'running' ORDER BY b.rowid,j.ordinal,a.attempt",
    )?;
    for row in stmt.query_map(params![task.id, batch_id], |r| r.get::<_, String>(0))? {
        for usage in serde_json::from_str::<Vec<Value>>(&row?)? {
            crate::engine::add_completion_usage(task, &usage)?;
        }
    }
    tx.execute(
        "UPDATE exploration_attempts SET projected=1 WHERE projected=0 AND status<>'running'
         AND job_id IN (SELECT j.id FROM exploration_jobs j JOIN exploration_batches b ON b.id=j.batch_id
             WHERE b.parent_task_id=?1 AND (?2 IS NULL OR b.id=?2))",
        params![task.id,batch_id],
    )?;
    tx.execute(
        "UPDATE tasks SET value=json_set(value,'$.usage',json(?1)) WHERE id=?2",
        params![task.usage.to_string(), task.id],
    )?;
    Ok(())
}

fn finish_tx(
    tx: &Transaction<'_>,
    id: &str,
    attempt: i64,
    status: &str,
    result: Option<&Value>,
    usage: &[Value],
) -> Result<()> {
    let raw = result.map(serde_json::to_string).transpose()?;
    tx.execute(
        "UPDATE exploration_jobs SET status=?1,result=?2 WHERE id=?3",
        params![status, raw, id],
    )?;
    tx.execute("UPDATE exploration_attempts SET status=?1,result=?2,usage=?3 WHERE job_id=?4 AND attempt=?5",
        params![status,raw,serde_json::to_string(usage)?,id,attempt])?;
    Ok(())
}

fn interrupt_tx(
    tx: &Transaction<'_>,
    id: &str,
    attempt: i64,
    reason: &str,
    unsent_status: &str,
) -> Result<()> {
    let maybe_sent: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM exploration_calls c JOIN network_calls n ON n.id=c.call_id
        WHERE c.job_id=?1 AND c.attempt=?2 AND n.state<>'not_sent')",
        params![id, attempt],
        |r| r.get(0),
    )?;
    let status = if maybe_sent { "unknown" } else { unsent_status };
    let result = json!({"error":reason,"possible_charge":maybe_sent});
    // A receipt may already have settled before the result body was committed.
    let mut stmt=tx.prepare("SELECT json_extract(b.record,'$.usage') FROM exploration_calls c JOIN budget_calls b ON b.call_id=c.call_id
        WHERE c.job_id=?1 AND c.attempt=?2 AND json_type(b.record,'$.usage')='object' ORDER BY c.rowid")?;
    let usage = stmt
        .query_map(params![id, attempt], |r| r.get::<_, String>(0))?
        .map(|raw| Ok(serde_json::from_str(&raw?)?))
        .collect::<Result<Vec<Value>>>()?;
    finish_tx(tx, id, attempt, status, Some(&result), &usage)
}

pub(crate) fn recover_tx(tx: &Transaction<'_>) -> Result<usize> {
    let mut stmt = tx.prepare("SELECT id,attempt FROM exploration_jobs WHERE status='running'")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, attempt) in &rows {
        interrupt_tx(tx, id, *attempt, "进程在保存探索结果前退出", "queued")?;
    }
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(deps: &[&str]) -> ExplorationInput {
        ExplorationInput {
            question: "Where is the caller?".into(),
            path: ".".into(),
            query: "caller".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn seed(db: &Connection) -> Result<()> {
        db.execute_batch("
          INSERT INTO runs(id,title,created_at) VALUES ('r1','one',1),('r2','two',1);
          INSERT INTO projects VALUES ('p1','one','a',1,1),('p2','two','b',1,1);
          INSERT INTO sessions VALUES ('s1','p1','chat','one','r1',1,1),('s2','p2','chat','two','r2',1,1);
          INSERT INTO tasks(id,run_id,value) VALUES
            ('a','r1','{\"workspace\":\"a\"}'),('b','r1','{\"workspace\":\"a\"}'),
            ('other-workspace','r1','{\"workspace\":\"b\"}'),('other-project','r2','{\"workspace\":\"a\"}');")?;
        Ok(())
    }

    #[test]
    fn exploration_jobs_upgrade_v18_and_replay_across_cli_turns() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.db");
        {
            let store = Store::open(&path)?;
            let db = store.db.lock().unwrap();
            db.execute_batch("DROP TABLE exploration_calls; DROP TABLE exploration_attempts;
               DROP TABLE exploration_dependencies; DROP TABLE exploration_jobs; DROP TABLE exploration_batches;
               DELETE FROM schema_migrations WHERE id='durable-exploration-v19'; PRAGMA user_version=18;")?;
            seed(&db)?;
        }
        let store = Store::open(&path)?;
        assert_eq!(store.schema_version()?, 19);
        let original = store.submit_exploration("a", "submit:key", "hash", &[input(&[])])?;
        let replay = store.submit_exploration("b", "submit:key", "hash", &[input(&[])])?;
        assert_eq!(original.jobs[0].id, replay.jobs[0].id);
        assert!(
            store
                .submit_exploration("a", "submit:key", "different", &[input(&[])])
                .is_err()
        );
        let tool = store.submit_exploration("a", "tool:key", "hash", &[input(&[])])?;
        assert_ne!(
            tool.id,
            store
                .submit_exploration("b", "tool:key", "hash", &[input(&[])])?
                .id
        );
        Ok(())
    }

    #[test]
    fn exploration_jobs_dependency_gate_is_scoped_atomic_and_ignores_failed_sibling() -> Result<()>
    {
        let dir = tempfile::tempdir()?;
        let store = Store::open(&dir.path().join("state.db"))?;
        seed(&store.db.lock().unwrap())?;
        let batch = store.submit_exploration("a", "tool:1", "h", &[input(&[]), input(&[])])?;
        let a = &batch.jobs[0].id;
        let b = &batch.jobs[1].id;
        for parent in ["other-workspace", "other-project"] {
            assert!(store.exploration_jobs_for_parent(parent)?.is_empty());
            assert!(store.exploration_job_for_parent(parent, a).is_err());
            assert!(
                store
                    .exploration_batch_for_parent(parent, &batch.id)
                    .is_err()
            );
            assert!(
                store
                    .submit_exploration(parent, "tool:bad", "h", &[input(&[]), input(&[a])])
                    .is_err()
            );
            assert!(
                store
                    .exploration_by_call(parent, "tool:bad", None)?
                    .is_none()
            );
        }
        assert!(
            store
                .submit_exploration("a", "tool:bad", "h", &[input(&["future"])])
                .is_err()
        );
        let next = store.submit_exploration("b", "tool:2", "h", &[input(&[a])])?;
        let c = &next.jobs[0].id;
        assert_eq!(next.jobs[0].blocked_by[0].job_id, *a);
        assert_eq!(next.jobs[0].blocked_by[0].status, "queued");
        assert!(store.claim_exploration("s1", c)?.is_none());
        assert!(store.claim_exploration("s2", a)?.is_none());
        assert!(store.claim_exploration("s1", a)?.is_some());
        let second = store.open_shared()?;
        assert!(second.claim_exploration("s1", a)?.is_none());
        store.finish_exploration("s1", a, 1, "no_matches", &json!({"evidence":[]}), &[])?;
        store.claim_exploration("s1", b)?;
        store.finish_exploration("s1", b, 1, "failed", &json!({}), &[])?;
        let waiting = store.submit_exploration("b", "tool:3", "h", &[input(&[a, b])])?;
        assert_eq!(waiting.jobs[0].blocked_by.len(), 1);
        assert_eq!(waiting.jobs[0].blocked_by[0].job_id, *b);
        assert_eq!(waiting.jobs[0].blocked_by[0].status, "failed");
        assert!(second.exploration_job("s1", c)?.blocked_by.is_empty());
        assert!(second.claim_exploration("s1", c)?.is_some());
        Ok(())
    }

    #[test]
    fn exploration_jobs_usage_projection_survives_replay_and_retry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(&dir.path().join("state.db"))?;
        seed(&store.db.lock().unwrap())?;
        let usage = json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5});
        let mut parent: Task = serde_json::from_value(json!({
            "id":"a","run_id":"r1","workspace":"a","status":"running","output":"",
            "error":null,"messages":[],"usage":usage,"created_at":1,"updated_at":1,
            "spec":{"name":"coder","role":"coder","route_id":"r","prompt":"ask"},
            "route":{"id":"r","name":"r","base_url":"http://localhost","model":"m"}
        }))?;
        store.db.lock().unwrap().execute(
            "UPDATE tasks SET value=?1 WHERE id='a'",
            [serde_json::to_string(&parent)?],
        )?;
        let batch = store.submit_exploration("a", "tool:k", "h", &[input(&[])])?;
        let job = &batch.jobs[0].id;
        store.claim_exploration("s1", job)?;
        store.finish_exploration(
            "s1",
            job,
            1,
            "failed",
            &json!({}),
            std::slice::from_ref(&usage),
        )?;
        let mut stale = parent.clone();
        store.project_exploration_usage(&mut parent, &batch.id)?;
        store.project_exploration_usage(&mut stale, &batch.id)?;
        assert_eq!(stale.usage["total_tokens"], 10);
        store.retry_exploration("s1", job)?;
        store.claim_exploration("s1", job)?;
        store.finish_exploration("s1", job, 2, "succeeded", &json!({}), &[usage])?;
        store.project_exploration_usage(&mut stale, &batch.id)?;
        assert_eq!(store.task("a")?.usage["total_tokens"], 15);
        Ok(())
    }

    #[test]
    fn exploration_jobs_recovery_requires_explicit_retry_after_possible_send() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(&dir.path().join("state.db"))?;
        seed(&store.db.lock().unwrap())?;
        let batch =
            store.submit_exploration("a", "tool:1", "h", &[input(&[]), input(&[]), input(&[])])?;
        for j in &batch.jobs {
            store.claim_exploration("s1", &j.id)?;
        }
        {
            let mut db = store.db.lock().unwrap();
            let tx = db.transaction()?;
            tx.execute_batch("INSERT INTO network_subjects VALUES ('x','{}',X'00');
              INSERT INTO network_authorizations VALUES ('auth','x',1,1,hex(zeroblob(32)),'[]','active',0);
              INSERT INTO network_calls VALUES ('sent','auth','a','finished','{}'),('safe','auth','a','not_sent','{}');")?;
            for (i, call) in [(0, "sent"), (1, "safe")] {
                assert!(link_call_tx(&tx, &batch.jobs[i].id, 2, "a", call).is_err());
                assert!(link_call_tx(&tx, &batch.jobs[i].id, 1, "b", call).is_err());
                link_call_tx(&tx, &batch.jobs[i].id, 1, "a", call)?;
            }
            assert_eq!(recover_tx(&tx)?, 3);
            tx.commit()?;
        }
        let sent = &batch.jobs[0].id;
        let failed = store.exploration_job("s1", sent)?;
        assert_eq!(failed.status, "unknown");
        assert_eq!(failed.receipts[0]["call_id"], "sent");
        assert!(store.claim_exploration("s1", sent)?.is_none());
        store.retry_exploration("s1", sent)?;
        for j in &batch.jobs {
            assert_eq!(store.claim_exploration("s1", &j.id)?.unwrap().attempt, 2);
        }
        assert!(
            store
                .finish_exploration("s1", sent, 1, "succeeded", &json!({}), &[])
                .is_err()
        );
        Ok(())
    }
}
