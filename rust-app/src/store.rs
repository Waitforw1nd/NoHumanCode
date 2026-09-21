use crate::{domain::*, secrets};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{path::Path, sync::Mutex};

pub const SCHEMA_VERSION: u32 = 5;

pub struct Store {
    db: Mutex<Connection>,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path).context("无法打开任务数据库")?;
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
        tx.execute(
            "INSERT INTO runs(id,title,kind,created_at) VALUES (?1,?2,?3,?4)",
            params![run.id, run.title, run.kind.as_str(), run.created_at],
        )?;
        for task in &run.tasks {
            tx.execute(
                "INSERT INTO tasks VALUES (?1,?2,?3)",
                params![task.id, task.run_id, serde_json::to_string(task)?],
            )?;
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
        tx.execute(
            "INSERT INTO runs(id,title,kind,created_at) VALUES (?1,?2,?3,?4)",
            params![run.id, run.title, run.kind.as_str(), run.created_at],
        )?;
        for task in &run.tasks {
            tx.execute(
                "INSERT INTO tasks VALUES (?1,?2,?3)",
                params![task.id, task.run_id, serde_json::to_string(task)?],
            )?;
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
        let changed = self.db.lock().unwrap().execute(
            "UPDATE tasks SET value=?2 WHERE id=?1",
            params![task.id, serde_json::to_string(task)?],
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
        self.db.lock().unwrap().execute(
            "INSERT INTO events(task_id,kind,data,at) VALUES (?1,?2,?3,?4)",
            params![task_id, kind, data.to_string(), now()],
        )?;
        Ok(())
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
        let mut stmt = db.prepare("SELECT e.seq,e.task_id,e.kind,e.data,e.at FROM events e JOIN tasks t ON t.id=e.task_id WHERE t.run_id=?1 AND e.seq>?2 ORDER BY e.seq LIMIT 500")?;
        let rows = stmt.query_map(params![run_id, after], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, String>(3)?,
                r.get(4)?,
            ))
        })?;
        let mut result = vec![];
        for row in rows {
            let (seq, task_id, kind, data, at) = row?;
            result.push(Event {
                seq,
                task_id,
                kind,
                data: serde_json::from_str(&data)?,
                at,
            });
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
                self.save_task(&task)?;
                self.event(
                    &task.id,
                    "status",
                    json!({"status":"interrupted","error":task.error}),
                )?;
                affected += 1;
            }
        }
        Ok(affected)
    }
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
