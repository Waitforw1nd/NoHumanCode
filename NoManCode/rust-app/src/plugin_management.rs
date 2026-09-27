//! Durable host-scoped desired state. Receipts record acceptance, not runtime success.
use crate::{domain::now, secrets, store::Store};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const BUILTIN_FILES_ID: &str = "builtin.files";
pub const BUILTIN_SHELL_ID: &str = "builtin.shell";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDesired {
    pub plugin_id: String,
    pub enabled: bool,
    pub revision: u64,
    pub updated_at: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPluginEnabled {
    pub enabled: bool,
    pub expected_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginMutationReceipt {
    pub plugin_id: String,
    pub enabled: bool,
    pub previous_revision: u64,
    pub revision: u64,
    pub committed_at: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginManagementError {
    Invalid,
    NotFound,
    RevisionConflict,
    IdempotencyConflict,
    Unavailable,
    Internal,
}
impl PluginManagementError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "plugin_invalid",
            Self::NotFound => "plugin_not_found",
            Self::RevisionConflict => "plugin_revision_conflict",
            Self::IdempotencyConflict => "plugin_idempotency_conflict",
            Self::Unavailable => "plugin_unavailable",
            Self::Internal => "plugin_internal",
        }
    }
    pub fn message(&self) -> &'static str {
        match self {
            Self::Invalid => "Invalid plugin management request",
            Self::NotFound => "Plugin not found",
            Self::RevisionConflict => "Plugin revision conflict",
            Self::IdempotencyConflict => "Plugin idempotency conflict",
            Self::Unavailable => "Plugin management unavailable",
            Self::Internal => "Plugin management failed; a command may already be committed",
        }
    }
}
impl std::fmt::Display for PluginManagementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for PluginManagementError {}

pub(crate) fn validate_plugin_id(id: &str) -> Result<()> {
    if !matches!(id, BUILTIN_FILES_ID | BUILTIN_SHELL_ID) {
        return Err(PluginManagementError::NotFound.into());
    }
    Ok(())
}
pub(crate) fn desired_tx(db: &Connection) -> Result<PluginDesired> {
    desired_for_tx(db, BUILTIN_FILES_ID)
}
pub(crate) fn desired_for_tx(db: &Connection, id: &str) -> Result<PluginDesired> {
    validate_plugin_id(id)?;
    let desired = db.query_row(
        "SELECT plugin_id,enabled,revision,updated_at FROM plugin_desired WHERE plugin_id=?1",
        [id],
        |r| {
            Ok(PluginDesired {
                plugin_id: r.get(0)?,
                enabled: r.get(1)?,
                revision: r.get(2)?,
                updated_at: r.get(3)?,
            })
        },
    )?;
    Ok(desired)
}
pub(crate) fn ensure_files_enabled_tx(db: &Connection) -> Result<()> {
    ensure_plugin_enabled_tx(db, BUILTIN_FILES_ID)
}
pub(crate) fn ensure_plugin_enabled_tx(db: &Connection, id: &str) -> Result<()> {
    if !desired_for_tx(db, id)?.enabled {
        return Err(PluginManagementError::Unavailable.into());
    }
    Ok(())
}
impl Store {
    pub(crate) fn plugin_desired(&self) -> Result<PluginDesired> {
        let db = self
            .db
            .lock()
            .map_err(|_| PluginManagementError::Internal)?;
        desired_tx(&db)
    }
    pub(crate) fn plugin_desired_for(&self, id: &str) -> Result<PluginDesired> {
        if id == BUILTIN_FILES_ID {
            return self.plugin_desired();
        }
        let db = self
            .db
            .lock()
            .map_err(|_| PluginManagementError::Internal)?;
        desired_for_tx(&db, id)
    }
    pub(crate) fn builtin_plugin_states(&self) -> Result<(PluginDesired, PluginDesired)> {
        let db = self
            .db
            .lock()
            .map_err(|_| PluginManagementError::Internal)?;
        Ok((
            desired_for_tx(&db, BUILTIN_FILES_ID)?,
            desired_for_tx(&db, BUILTIN_SHELL_ID)?,
        ))
    }
    // Caller holds the runtime management lock. Never calls runtime from Store.
    pub(crate) fn set_plugin_desired(
        &self,
        id: &str,
        request: &SetPluginEnabled,
        key: &str,
    ) -> Result<(PluginMutationReceipt, bool)> {
        validate_plugin_id(id)?;
        if request.expected_revision > i64::MAX as u64
            || secrets::validate_idempotency_key(key).is_err()
        {
            return Err(PluginManagementError::Invalid.into());
        }
        let mut db = self
            .db
            .lock()
            .map_err(|_| PluginManagementError::Internal)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let prior: Option<PluginMutationReceipt> = tx.query_row(
            "SELECT plugin_id,enabled,expected_revision,result_revision,committed_at FROM plugin_management_requests WHERE key=?1",
            [key], |r| Ok(PluginMutationReceipt { plugin_id:r.get(0)?, enabled:r.get(1)?, previous_revision:r.get(2)?, revision:r.get(3)?, committed_at:r.get(4)? }),
        ).optional()?;
        if let Some(receipt) = prior {
            if receipt.plugin_id != id
                || receipt.enabled != request.enabled
                || receipt.previous_revision != request.expected_revision
            {
                return Err(PluginManagementError::IdempotencyConflict.into());
            }
            tx.commit()?;
            return Ok((receipt, true));
        }
        let current = desired_for_tx(&tx, id)?;
        if current.revision != request.expected_revision || current.revision == i64::MAX as u64 {
            return Err(PluginManagementError::RevisionConflict.into());
        }
        let receipt = PluginMutationReceipt {
            plugin_id: id.to_owned(),
            enabled: request.enabled,
            previous_revision: current.revision,
            revision: current.revision + 1,
            committed_at: i64::try_from(now()).map_err(|_| PluginManagementError::Internal)?,
        };
        tx.execute(
            "UPDATE plugin_desired SET enabled=?1,revision=?2,updated_at=?3 WHERE plugin_id=?4",
            params![receipt.enabled, receipt.revision, receipt.committed_at, id],
        )?;
        tx.execute("INSERT INTO plugin_management_requests(key,plugin_id,enabled,expected_revision,result_revision,committed_at) VALUES (?1,?2,?3,?4,?5,?6)", params![key, id, receipt.enabled, receipt.previous_revision, receipt.revision, receipt.committed_at])?;
        tx.commit()?;
        Ok((receipt, false))
    }
    pub(crate) fn validate_files_read(&self, task: &crate::domain::Task) -> Result<()> {
        let db = self
            .db
            .lock()
            .map_err(|_| PluginManagementError::Internal)?;
        ensure_files_enabled_tx(&db)?;
        crate::project_config::validate_task_tx(&db, task, true, false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.sqlite")).unwrap();
        (dir, store)
    }
    fn request(enabled: bool, expected_revision: u64) -> SetPluginEnabled {
        SetPluginEnabled {
            enabled,
            expected_revision,
        }
    }
    fn error(result: Result<(PluginMutationReceipt, bool)>, expected: PluginManagementError) {
        assert_eq!(
            result.unwrap_err().downcast_ref::<PluginManagementError>(),
            Some(&expected)
        );
    }
    #[test]
    fn shell_global_key_and_independent_revisions() {
        let (_dir, store) = store();
        let (files, shell) = store.builtin_plugin_states().unwrap();
        assert_eq!((files.revision, shell.revision), (0, 0));
        let first = store
            .set_plugin_desired(BUILTIN_FILES_ID, &request(false, 0), "shared")
            .unwrap()
            .0;
        let before = store.builtin_plugin_states().unwrap();
        error(
            store.set_plugin_desired(BUILTIN_SHELL_ID, &request(false, 0), "shared"),
            PluginManagementError::IdempotencyConflict,
        );
        assert_eq!(before, store.builtin_plugin_states().unwrap());
        let shell = store
            .set_plugin_desired(BUILTIN_SHELL_ID, &request(true, 0), "shell")
            .unwrap()
            .0;
        assert_eq!(shell.revision, 1);
        assert_eq!(
            store.plugin_desired_for(BUILTIN_FILES_ID).unwrap().revision,
            1
        );
        assert_eq!(
            store
                .set_plugin_desired(BUILTIN_FILES_ID, &request(false, 0), "shared")
                .unwrap(),
            (first, true)
        );
        assert_eq!(
            store
                .set_plugin_desired(BUILTIN_SHELL_ID, &request(true, 0), "shell")
                .unwrap(),
            (shell, true)
        );
    }
    #[test]
    fn store_cas_replay_conflicts_and_atomic_failure() {
        let (_dir, store) = store();
        let initial = store.plugin_desired().unwrap();
        assert!(initial.enabled);
        assert_eq!(initial.revision, 0);
        let (first, replayed) = store
            .set_plugin_desired(BUILTIN_FILES_ID, &request(false, 0), "first")
            .unwrap();
        assert!(!replayed);
        assert_eq!(first.previous_revision, 0);
        assert_eq!(first.revision, 1);
        let (second, _) = store
            .set_plugin_desired(BUILTIN_FILES_ID, &request(true, 1), "second")
            .unwrap();
        let (same, replayed) = store
            .set_plugin_desired(BUILTIN_FILES_ID, &request(false, 0), "first")
            .unwrap();
        assert!(replayed);
        assert_eq!(same, first);
        assert_eq!(store.plugin_desired().unwrap().revision, second.revision);
        error(
            store.set_plugin_desired(BUILTIN_FILES_ID, &request(true, 0), "first"),
            PluginManagementError::IdempotencyConflict,
        );
        error(
            store.set_plugin_desired(BUILTIN_FILES_ID, &request(false, 0), "stale"),
            PluginManagementError::RevisionConflict,
        );
        error(
            store.set_plugin_desired("other", &request(false, 2), "other"),
            PluginManagementError::NotFound,
        );
        error(
            store.set_plugin_desired(BUILTIN_FILES_ID, &request(false, u64::MAX), "large"),
            PluginManagementError::Invalid,
        );
        error(
            store.set_plugin_desired(BUILTIN_FILES_ID, &request(false, 2), ""),
            PluginManagementError::Invalid,
        );
        let before = store.plugin_desired().unwrap();
        store.db.lock().unwrap().execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON plugin_management_requests BEGIN SELECT RAISE(ABORT,'receipt failure'); END;").unwrap();
        assert!(
            store
                .set_plugin_desired(BUILTIN_FILES_ID, &request(false, 2), "retryable-key")
                .is_err()
        );
        assert_eq!(store.plugin_desired().unwrap(), before);
        let count: i64 = store
            .db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM plugin_management_requests", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
        store
            .db
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_receipt")
            .unwrap();
        let (receipt, replayed) = store
            .set_plugin_desired(BUILTIN_FILES_ID, &request(true, 2), "retryable-key")
            .unwrap();
        assert!(!replayed);
        assert_eq!(receipt.revision, 3); // same enabled is a new explicit intention
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE plugin_desired SET revision=?1 WHERE plugin_id='builtin.files'",
                [i64::MAX],
            )
            .unwrap();
        error(
            store.set_plugin_desired(
                BUILTIN_FILES_ID,
                &request(false, i64::MAX as u64),
                "overflow",
            ),
            PluginManagementError::RevisionConflict,
        );
        assert_eq!(store.plugin_desired().unwrap().revision, i64::MAX as u64);
    }
    #[test]
    fn independent_stores_cas_one_winner() {
        let (dir, first) = store();
        let second = Store::open(&dir.path().join("state.sqlite")).unwrap();
        let timeout = std::time::Duration::from_secs(8);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let mut releases = Vec::new();
        let mut workers = Vec::new();
        for (n, store) in [first, second].into_iter().enumerate() {
            let ready = ready_tx.clone();
            let done = done_tx.clone();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            releases.push(release_tx);
            workers.push(std::thread::spawn(move || {
                ready.send(n).unwrap();
                release_rx
                    .recv_timeout(timeout)
                    .expect("CAS release timeout");
                let result = store.set_plugin_desired(
                    BUILTIN_FILES_ID,
                    &request(false, 0),
                    &format!("cas-{n}"),
                );
                drop(store);
                done.send(result).unwrap();
            }));
        }
        for _ in 0..2 {
            ready_rx.recv_timeout(timeout).expect("CAS ready timeout");
        }
        for release in releases {
            release.send(()).unwrap();
        }
        let results = (0..2)
            .map(|_| {
                done_rx
                    .recv_timeout(timeout)
                    .expect("CAS completion timeout")
            })
            .collect::<Vec<_>>();
        for worker in workers {
            let deadline = std::time::Instant::now() + timeout;
            while !worker.is_finished() && std::time::Instant::now() < deadline {
                std::thread::yield_now();
            }
            assert!(worker.is_finished(), "CAS worker termination timeout");
            worker.join().unwrap();
        }
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter_map(|r| r.as_ref().err())
                .next()
                .unwrap()
                .downcast_ref::<PluginManagementError>(),
            Some(&PluginManagementError::RevisionConflict)
        );
        let reopened = Store::open(&dir.path().join("state.sqlite")).unwrap();
        assert_eq!(reopened.plugin_desired().unwrap().revision, 1);
        let count: i64 = reopened
            .db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM plugin_management_requests", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
    #[test]
    fn schema_13_reopen_marker_constraint_and_data_corruption_rejected() {
        for mode in [
            "marker",
            "constraint",
            "type",
            "primary",
            "foreign",
            "row",
            "receipt",
        ] {
            let (dir, store) = store();
            assert_eq!(store.schema_version().unwrap(), 13);
            assert_eq!(
                Store::open(&dir.path().join("state.sqlite"))
                    .unwrap()
                    .plugin_desired()
                    .unwrap(),
                store.plugin_desired().unwrap()
            );
            let db = store.db.lock().unwrap();
            match mode {
                "marker" => {
                    db.execute(
                        "DELETE FROM schema_migrations WHERE id=?1",
                        [crate::repository::PLUGIN_MANAGEMENT_MIGRATION_ID],
                    )
                    .unwrap();
                }
                "constraint" => {
                    db.execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_master SET sql=replace(sql, 'revision>=0', 'revision>=-1') WHERE name='plugin_desired'; PRAGMA writable_schema=OFF;").unwrap();
                }
                "type" | "primary" | "foreign" => {
                    let (from, to) = match mode {
                        "type" => ("enabled INTEGER", "enabled TEXT"),
                        "primary" => ("PRIMARY KEY", "UNIQUE"),
                        "foreign" => (
                            "REFERENCES plugin_desired(plugin_id)",
                            "REFERENCES projects(id)",
                        ),
                        _ => unreachable!(),
                    };
                    db.execute_batch("PRAGMA writable_schema=ON").unwrap();
                    assert_eq!(db.execute("UPDATE sqlite_master SET sql=replace(sql,?1,?2) WHERE name='plugin_management_requests'", params![from,to]).unwrap(),1);
                    db.execute_batch("PRAGMA writable_schema=OFF").unwrap();
                }
                "row" => {
                    db.execute_batch(
                        "PRAGMA ignore_check_constraints=ON; UPDATE plugin_desired SET enabled=3",
                    )
                    .unwrap();
                }
                "receipt" => {
                    db.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO plugin_management_requests VALUES ('bad','builtin.files',1,0,7,1)").unwrap();
                }
                _ => unreachable!(),
            }
            drop(db);
            drop(store);
            assert!(
                Store::open(&dir.path().join("state.sqlite")).is_err(),
                "{mode}"
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginBlockedReason {
    Disabled,
    RuntimeFailed,
    RuntimeUnavailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginStatus {
    pub plugin_id: String,
    pub version: String,
    pub scope: String,
    pub desired: PluginDesired,
    pub runtime: crate::tool_runtime::FilesRuntimeStatus,
    pub affected_tools: Vec<String>,
    pub compat_tools: Vec<String>,
    pub affected_dependents: Vec<String>,
    pub converged: bool,
    pub blocked_reason: Option<PluginBlockedReason>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginMutationResult {
    pub receipt: PluginMutationReceipt,
    pub replayed: bool,
    pub current: PluginStatus,
}
impl PluginStatus {
    pub(crate) fn observe(
        desired: PluginDesired,
        runtime: crate::tool_runtime::FilesRuntimeStatus,
    ) -> Self {
        use crate::tool_runtime::{FilesInstanceState, FilesRuntimeState};
        let shell = desired.plugin_id == BUILTIN_SHELL_ID;
        let dependent = if shell {
            "builtin.engine-shell-tools"
        } else {
            "builtin.engine-tools"
        };
        let version = if shell {
            crate::tool_runtime::SHELL_PLUGIN_VERSION
        } else {
            crate::tool_runtime::FILES_PLUGIN_VERSION
        };
        let tools: &[&str] = if shell {
            &crate::tool_runtime::SHELL_TOOLS
        } else {
            &crate::tool_runtime::FILE_TOOLS
        };
        let complete_instances = runtime.instances.len() == 2
            && [desired.plugin_id.as_str(), dependent]
                .iter()
                .all(|id| runtime.instances.iter().any(|i| i.plugin_id == *id));
        let converged = if desired.enabled {
            runtime.state == FilesRuntimeState::Active
                && runtime.admission_open
                && complete_instances
                && runtime
                    .instances
                    .iter()
                    .all(|i| i.state == FilesInstanceState::Active)
        } else {
            matches!(
                runtime.state,
                FilesRuntimeState::Stopped | FilesRuntimeState::Unloaded
            ) && !runtime.admission_open
                && runtime.effects_count == 0
                && runtime.subscriptions_count == 0
                && runtime.providers.is_empty()
                && complete_instances
                && runtime.instances.iter().all(|i| {
                    matches!(
                        i.state,
                        FilesInstanceState::Stopped | FilesInstanceState::Unloaded
                    ) && i.failure_stage.is_none()
                })
        };
        let blocked_reason = if runtime.state == FilesRuntimeState::Failed {
            Some(PluginBlockedReason::RuntimeFailed)
        } else if !desired.enabled {
            Some(PluginBlockedReason::Disabled)
        } else if !converged {
            Some(PluginBlockedReason::RuntimeUnavailable)
        } else {
            None
        };
        Self {
            plugin_id: desired.plugin_id.clone(),
            version: version.into(),
            scope: "host".into(),
            desired,
            runtime,
            affected_tools: tools.iter().map(|name| (*name).into()).collect(),
            compat_tools: crate::tool_runtime::COMPAT_TOOLS
                .iter()
                .map(|name| (*name).into())
                .collect(),
            affected_dependents: vec![dependent.into()],
            converged,
            blocked_reason,
        }
    }
}
pub(crate) fn safe_error(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<PluginManagementError>().is_some() {
        error
    } else {
        error.context(PluginManagementError::Internal)
    }
}
