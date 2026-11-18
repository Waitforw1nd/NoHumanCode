//! Durable host-scoped desired state for builtin capabilities and runtime components.
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

pub(crate) fn validate_ledger_id(id: &str) -> Result<()> {
    if matches!(
        id,
        BUILTIN_FILES_ID | BUILTIN_SHELL_ID | "builtin.chat-agent" | "builtin.provider-xpeach"
    ) {
        Ok(())
    } else {
        Err(PluginManagementError::NotFound.into())
    }
}

pub(crate) fn desired_for_tx(db: &Connection, id: &str) -> Result<PluginDesired> {
    validate_ledger_id(id)?;
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
    pub(crate) fn plugin_desired_for(&self, id: &str) -> Result<PluginDesired> {
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

    pub(crate) fn admit_runtime(&self, id: &str) -> Result<()> {
        let db = self
            .db
            .lock()
            .map_err(|_| PluginManagementError::Internal)?;
        ensure_plugin_enabled_tx(&db, id)
    }

    pub(crate) fn set_plugin_desired_with_operation(
        &self,
        _op: &crate::execution_authority::ExecutionOperation,
        id: &str,
        request: &SetPluginEnabled,
        key: &str,
    ) -> Result<(PluginMutationReceipt, bool)> {
        validate_ledger_id(id)?;
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
        let prior: Option<PluginMutationReceipt> = tx
            .query_row(
                "SELECT plugin_id,enabled,expected_revision,result_revision,committed_at \
                 FROM plugin_management_requests WHERE key=?1",
                [key],
                |r| {
                    Ok(PluginMutationReceipt {
                        plugin_id: r.get(0)?,
                        enabled: r.get(1)?,
                        previous_revision: r.get(2)?,
                        revision: r.get(3)?,
                        committed_at: r.get(4)?,
                    })
                },
            )
            .optional()?;
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
        tx.execute(
            "INSERT INTO plugin_management_requests(key,plugin_id,enabled,expected_revision,result_revision,committed_at) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                key,
                id,
                receipt.enabled,
                receipt.previous_revision,
                receipt.revision,
                receipt.committed_at
            ],
        )?;
        tx.commit()?;
        Ok((receipt, false))
    }

    pub(crate) fn set_runtime_desired_with_operation(
        &self,
        _op: &crate::execution_authority::ExecutionOperation,
        id: &str,
        request: &SetPluginEnabled,
        key: &str,
        reject: Option<crate::runtime_management::RuntimeManagementError>,
    ) -> std::result::Result<
        (PluginMutationReceipt, bool),
        crate::runtime_management::RuntimeManagementError,
    > {
        use crate::runtime_management::RuntimeManagementError as E;
        if !matches!(id, "builtin.chat-agent" | "builtin.provider-xpeach")
            || request.expected_revision > i64::MAX as u64
            || secrets::validate_idempotency_key(key).is_err()
        {
            return Err(E::Invalid);
        }
        let mut db = self.db.lock().map_err(|_| E::Internal)?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| E::Internal)?;
        let prior: Option<PluginMutationReceipt> = tx
            .query_row(
                "SELECT plugin_id,enabled,expected_revision,result_revision,committed_at \
                 FROM plugin_management_requests WHERE key=?1",
                [key],
                |r| {
                    Ok(PluginMutationReceipt {
                        plugin_id: r.get(0)?,
                        enabled: r.get(1)?,
                        previous_revision: r.get(2)?,
                        revision: r.get(3)?,
                        committed_at: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|_| E::Internal)?;
        if let Some(receipt) = prior {
            if receipt.plugin_id != id
                || receipt.enabled != request.enabled
                || receipt.previous_revision != request.expected_revision
            {
                return Err(E::IdempotencyConflict);
            }
            tx.commit().map_err(|_| E::Internal)?;
            return Ok((receipt, true));
        }
        if let Some(rejection) = reject {
            return Err(rejection);
        }
        let current = desired_for_tx(&tx, id).map_err(|_| E::Internal)?;
        if current.revision != request.expected_revision || current.revision == i64::MAX as u64 {
            return Err(E::RevisionConflict);
        }
        let committed_at = i64::try_from(now()).map_err(|_| E::Internal)?;
        if committed_at <= 0 {
            return Err(E::Internal);
        }
        let receipt = PluginMutationReceipt {
            plugin_id: id.to_owned(),
            enabled: request.enabled,
            previous_revision: current.revision,
            revision: current.revision + 1,
            committed_at,
        };
        tx.execute(
            "UPDATE plugin_desired SET enabled=?1,revision=?2,updated_at=?3 WHERE plugin_id=?4",
            params![receipt.enabled, receipt.revision, receipt.committed_at, id],
        )
        .map_err(|_| E::Internal)?;
        tx.execute(
            "INSERT INTO plugin_management_requests(key,plugin_id,enabled,expected_revision,result_revision,committed_at) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                key,
                id,
                receipt.enabled,
                receipt.previous_revision,
                receipt.revision,
                receipt.committed_at
            ],
        )
        .map_err(|_| E::Internal)?;
        tx.commit().map_err(|_| E::Internal)?;
        Ok((receipt, false))
    }

    #[cfg(test)]
    pub(crate) fn set_plugin_desired(
        &self,
        id: &str,
        request: &SetPluginEnabled,
        key: &str,
    ) -> Result<(PluginMutationReceipt, bool)> {
        let op = self.execution_operation(tokio_util::sync::CancellationToken::new())?;
        self.set_plugin_desired_with_operation(&op, id, request, key)
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
        use crate::tool_runtime::FilesRuntimeState;
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
        let converged = if desired.enabled {
            runtime.state == FilesRuntimeState::Active && runtime.admission_open
        } else {
            matches!(
                runtime.state,
                FilesRuntimeState::Stopped | FilesRuntimeState::Unloaded
            ) && !runtime.admission_open
                && runtime.effects_count == 0
                && runtime.subscriptions_count == 0
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cas_and_idempotency_replay_across_plugins() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.sqlite")).unwrap();
        let (files, shell) = store.builtin_plugin_states().unwrap();
        assert_eq!((files.revision, shell.revision), (0, 0));

        let off = SetPluginEnabled {
            enabled: false,
            expected_revision: 0,
        };
        let (first, replayed) = store
            .set_plugin_desired(BUILTIN_FILES_ID, &off, "k-files-off")
            .unwrap();
        assert!(!replayed);
        assert_eq!(first.revision, 1);

        let (replay, replayed) = store
            .set_plugin_desired(BUILTIN_FILES_ID, &off, "k-files-off")
            .unwrap();
        assert!(replayed);
        assert_eq!(replay, first);

        assert_eq!(
            store
                .set_plugin_desired(BUILTIN_SHELL_ID, &off, "k-files-off")
                .unwrap_err()
                .downcast_ref::<PluginManagementError>(),
            Some(&PluginManagementError::IdempotencyConflict)
        );
        assert_eq!(
            store
                .set_plugin_desired(BUILTIN_FILES_ID, &off, "k-stale")
                .unwrap_err()
                .downcast_ref::<PluginManagementError>(),
            Some(&PluginManagementError::RevisionConflict)
        );
    }
}
