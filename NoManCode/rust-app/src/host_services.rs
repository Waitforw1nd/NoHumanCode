#[cfg(test)]
use crate::engine::{crash_probe, restore_probe};
use crate::{
    approval::{self, ApprovalStatus},
    domain::*,
    project_config::{
        ConfigureProject, ProjectConfigSnapshot, ProjectSelection, RegisterProject, SelectProject,
        TaskProjectBinding,
    },
    secrets,
    store::Store,
    workspace,
    workspace_changes::{self, *},
};
use anyhow::Result;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

// The result is evaluated before observation, so Store guards and transactions
// have already been dropped. Production expands to the original expression.
macro_rules! restore_observe {
    ($engine:expr, $task:expr, $path:expr, $stage:ident, $operation:expr) => {{
        let result = $operation;
        #[cfg(test)]
        $engine.emit_restore_probe($task, $path, restore_probe::Stage::$stage, &result);
        result
    }};
}

pub(crate) struct Active {
    pub(crate) cancel: CancellationToken,
    pub(crate) workspace: String,
    pub(crate) scopes: Vec<String>,
}

pub(crate) struct ReviewRecovery {
    store: Arc<Store>,
    workspace: Arc<WorkspaceSafety>,
    project: Arc<ProjectService>,
    active: Arc<Mutex<HashMap<String, Active>>>,
    #[cfg(test)]
    restore_probe: Arc<Mutex<Option<restore_probe::Hook>>>,
}

impl ReviewRecovery {
    pub(crate) const ID: &'static str = "host.review-recovery";
    pub(crate) const VERSION: &'static str = "1.0.0";
    pub(crate) fn new(
        store: Arc<Store>,
        workspace: Arc<WorkspaceSafety>,
        project: Arc<ProjectService>,
        active: Arc<Mutex<HashMap<String, Active>>>,
        #[cfg(test)] restore_probe: Arc<Mutex<Option<restore_probe::Hook>>>,
    ) -> Self {
        Self {
            store,
            workspace,
            project,
            active,
            #[cfg(test)]
            restore_probe,
        }
    }
    #[cfg(test)]
    pub(crate) fn emit_restore_probe<T>(
        &self,
        task_id: &str,
        path: &str,
        stage: restore_probe::Stage,
        result: &Result<T>,
    ) {
        let callback = self
            .restore_probe
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(id, _)| id == task_id)
            .map(|(_, callback)| callback.clone());
        if let Some(callback) = callback {
            let error = result
                .as_ref()
                .err()
                .map(|error| restore_probe::ErrorCodes {
                    io: error.chain().find_map(|cause| {
                        cause
                            .downcast_ref::<std::io::Error>()
                            .and_then(std::io::Error::raw_os_error)
                    }),
                    sqlite: error.chain().find_map(|cause| {
                        match cause.downcast_ref::<rusqlite::Error>() {
                            Some(rusqlite::Error::SqliteFailure(code, _)) => {
                                Some(code.extended_code)
                            }
                            _ => None,
                        }
                    }),
                });
            callback(restore_probe::Event {
                task_id: task_id.to_owned(),
                path: path.to_owned(),
                stage,
                error,
            });
        }
    }
    pub(crate) fn changes(&self, id: &str) -> Result<Vec<WorkspaceChange>> {
        let task = self.store.task(id).map_err(|error| {
            if error.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<approval::ApprovalError>(),
                    Some(approval::ApprovalError::CorruptState { .. })
                )
            }) {
                return anyhow::Error::from(WorkspaceChangeError::Corrupt);
            }
            if error.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                )
            }) {
                WorkspaceChangeError::NotFound.into()
            } else {
                error
            }
        })?;
        self.project.validate_task(&task, false, false)?;
        let root = std::path::Path::new(&task.workspace);
        let restore_exists = self.store.latest_restore(id)?.is_some();
        let mut grouped: std::collections::BTreeMap<String, PreparedChange> =
            std::collections::BTreeMap::new();
        for record in self
            .store
            .workspace_changes_for_task(id)
            .map_err(|_| WorkspaceChangeError::Corrupt)?
        {
            if record.state == ChangeState::Finished
                && !record
                    .after_digest
                    .as_deref()
                    .is_some_and(workspace_changes::valid_digest)
            {
                return Err(WorkspaceChangeError::Corrupt.into());
            }
            if record.state == ChangeState::Failed {
                continue;
            }
            grouped
                .entry(record.path_key.clone())
                .and_modify(|first| {
                    if record.state == ChangeState::Finished {
                        if first.state == ChangeState::Failed {
                            *first = record.clone();
                        } else {
                            first.after_digest = record.after_digest.clone();
                        }
                    } else {
                        if record.state == ChangeState::Unknown {
                            first.state = record.state;
                        }
                    }
                    if record.restore_state != "pending" {
                        first.restore_state = record.restore_state.clone();
                    }
                })
                .or_insert(record);
        }
        grouped
            .into_values()
            .map(|record| {
                let after = record.after_digest.clone().unwrap_or_default();
                let current = if record.state != ChangeState::Finished {
                    CurrentState::Unknown
                } else {
                    match self
                        .workspace
                        .resolve(root, &record.path, true, &task.spec.write_scopes)
                    {
                        Err(_) => CurrentState::Invalid,
                        Ok(path) => match self.workspace.read_safe_file(&path) {
                            Ok(Some(bytes))
                                if record.restore_state == "restored"
                                    && record.before_digest.as_deref()
                                        == Some(digest(&bytes).as_str()) =>
                            {
                                CurrentState::Restored
                            }
                            Ok(None)
                                if record.restore_state == "restored"
                                    && record.kind == ChangeKind::Created =>
                            {
                                CurrentState::Restored
                            }
                            Ok(Some(bytes))
                                if record.restore_state != "restored"
                                    && digest(&bytes) == after =>
                            {
                                CurrentState::Matches
                            }
                            Ok(Some(_)) => CurrentState::Diverged,
                            Ok(None) => CurrentState::Missing,
                            Err(error)
                                if error.to_string().contains("链接")
                                    || error.to_string().contains("reparse") =>
                            {
                                CurrentState::UnsafeLink
                            }
                            Err(_) => CurrentState::Unreadable,
                        },
                    }
                };
                Ok(WorkspaceChange {
                    change_id: record.id,
                    task_id: record.task_id,
                    path: record.path,
                    kind: record.kind,
                    before_digest: record.before_digest,
                    after_digest: after,
                    state: record.state,
                    restore_state: record.restore_state.clone(),
                    current,
                    restorable: !restore_exists
                        && record.state == ChangeState::Finished
                        && current == CurrentState::Matches
                        && record.restore_state == "pending",
                })
            })
            .collect()
    }

    pub(crate) fn latest_restore(&self, id: &str) -> Result<Option<RestoreReceipt>> {
        self.store.task(id).map_err(|error| {
            if error.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                )
            }) {
                anyhow::Error::from(WorkspaceChangeError::NotFound)
            } else {
                error
            }
        })?;
        self.store.latest_restore(id)
    }

    fn validated_restore_changes(
        &self,
        task: &Task,
        records: Vec<PreparedChange>,
    ) -> Result<Vec<PreparedChange>> {
        let id = task.id.as_str();
        let workspace_digest = digest(task.workspace.as_bytes());
        let mut grouped: std::collections::BTreeMap<String, PreparedChange> =
            std::collections::BTreeMap::new();
        for record in records {
            if record.workspace_digest != workspace_digest {
                return Err(WorkspaceChangeError::Corrupt.into());
            }
            let original_scopes: Vec<String> = serde_json::from_str(&record.write_scopes)
                .map_err(|_| WorkspaceChangeError::Corrupt)?;
            if original_scopes != task.spec.write_scopes {
                return Err(WorkspaceChangeError::Conflict { receipt: None }.into());
            }
            let approval = self
                .store
                .approval_by_tool_call(id, &record.tool_call_id)
                .map_err(|_| WorkspaceChangeError::Corrupt)?
                .ok_or(WorkspaceChangeError::Corrupt)?;
            if approval.status != ApprovalStatus::Approved
                || approval.execution_state != approval::ExecutionState::Finished
            {
                return Err(WorkspaceChangeError::Corrupt.into());
            }
            if approval.binding_digest != record.binding_digest
                || approval.workspace != task.workspace
            {
                return Err(WorkspaceChangeError::Conflict { receipt: None }.into());
            }
            let call = task
                .messages
                .iter()
                .filter_map(|message| message["tool_calls"].as_array())
                .flatten()
                .find(|call| call["id"] == record.tool_call_id)
                .ok_or(WorkspaceChangeError::Corrupt)?;
            if call["function"]["name"] != "write_file" || approval.tool_name != "write_file" {
                return Err(WorkspaceChangeError::Corrupt.into());
            }
            let raw_args = call["function"]["arguments"]
                .as_str()
                .ok_or(WorkspaceChangeError::Corrupt)?;
            let args: Value =
                serde_json::from_str(raw_args).map_err(|_| WorkspaceChangeError::Corrupt)?;
            let raw_path = args["path"].as_str().ok_or(WorkspaceChangeError::Corrupt)?;
            let content = args["content"]
                .as_str()
                .ok_or(WorkspaceChangeError::Corrupt)?;
            let (_, path, path_key) = self
                .workspace
                .change_target(
                    std::path::Path::new(&task.workspace),
                    raw_path,
                    &task.spec.write_scopes,
                )
                .map_err(|_| WorkspaceChangeError::Corrupt)?;
            let expected = approval::binding_digest(
                id,
                approval.session_id.as_deref(),
                approval.turn_id.as_deref(),
                &record.tool_call_id,
                "write_file",
                &task.workspace,
                &task.spec.write_scopes,
                task.spec.allow_commands,
                &args,
            )
            .map_err(|_| WorkspaceChangeError::Corrupt)?;
            if path != record.path
                || path_key != record.path_key
                || Some(digest(content.as_bytes())) != record.after_digest
                || expected != record.binding_digest
                || approval.args_digest
                    != approval::args_digest(&args).map_err(|_| WorkspaceChangeError::Corrupt)?
            {
                return Err(WorkspaceChangeError::Corrupt.into());
            }
            grouped
                .entry(record.path_key.clone())
                .and_modify(|first| first.after_digest = record.after_digest.clone())
                .or_insert(record);
        }
        let changes: Vec<_> = grouped.into_values().collect();
        Ok(changes)
    }

    pub(crate) async fn restore(&self, id: &str) -> Result<RestoreReceipt> {
        let operation = self
            .store
            .execution_operation(self.workspace.stopping.clone())?;
        operation
            .run(async {
                let _gate = self.workspace.gate.lock().await;
                self.workspace.ensure_accepting()?;
                self.restore_locked(id)
            })
            .await
    }

    fn restore_locked(&self, id: &str) -> Result<RestoreReceipt> {
        // The compatibility Task route must not bypass a damaged checkpoint manifest.
        let checkpoint = self.store.checkpoint_for_task(id)?;
        let task = self.store.task(id).map_err(|error| {
            if error.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<approval::ApprovalError>(),
                    Some(approval::ApprovalError::CorruptState { .. })
                )
            }) {
                return anyhow::Error::from(WorkspaceChangeError::Corrupt);
            }
            if error.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                )
            }) {
                anyhow::Error::from(WorkspaceChangeError::NotFound)
            } else {
                error
            }
        })?;
        if let Some(receipt) = self.store.latest_restore(id)? {
            return if receipt.status == RestoreStatus::Complete {
                Ok(receipt)
            } else if receipt.status == RestoreStatus::Unknown
                || receipt.status == RestoreStatus::Claimed
            {
                Err(WorkspaceChangeError::Unknown {
                    receipt: Some(receipt),
                }
                .into())
            } else {
                Err(WorkspaceChangeError::Conflict {
                    receipt: Some(receipt),
                }
                .into())
            };
        }
        restore_observe!(
            self,
            id,
            "",
            RootValidation,
            self.project.validate_task(&task, true, false)
        )?;
        let active_ids: Vec<String> = self.active.lock().unwrap().keys().cloned().collect();
        for active_id in active_ids {
            if active_id == id {
                return Err(WorkspaceChangeError::Active.into());
            }
            let active = self.store.task(&active_id)?;
            if active.spec.allow_commands
                || std::path::Path::new(&active.workspace).canonicalize()?
                    == std::path::Path::new(&task.workspace).canonicalize()?
            {
                return Err(WorkspaceChangeError::Active.into());
            }
        }
        let records = self
            .store
            .workspace_changes_for_task(id)
            .map_err(|_| WorkspaceChangeError::Corrupt)?;
        if records.iter().any(|record| {
            record.state == ChangeState::Finished
                && !record
                    .after_digest
                    .as_deref()
                    .is_some_and(workspace_changes::valid_digest)
        }) {
            return Err(WorkspaceChangeError::Corrupt.into());
        }
        if self.store.has_legacy_file_backup(id)? {
            return Err(WorkspaceChangeError::Unrestorable.into());
        }
        if records.is_empty() {
            if self.store.file_backups(id)?.is_empty() {
                return Ok(RestoreReceipt {
                    restore_id: None,
                    task_id: id.to_owned(),
                    status: RestoreStatus::Complete,
                    restored: 0,
                    outcomes: vec![],
                });
            }
            return Err(WorkspaceChangeError::Unrestorable.into());
        }
        if records
            .iter()
            .any(|record| matches!(record.state, ChangeState::Unknown | ChangeState::Prepared))
        {
            return Err(WorkspaceChangeError::Unknown { receipt: None }.into());
        }
        let records: Vec<_> = records
            .into_iter()
            .filter(|record| record.state == ChangeState::Finished && record.after_digest.is_some())
            .collect();
        if records.is_empty() {
            return Ok(RestoreReceipt {
                restore_id: None,
                task_id: id.to_owned(),
                status: RestoreStatus::Complete,
                restored: 0,
                outcomes: vec![],
            });
        }
        let changes = self.validated_restore_changes(&task, records)?;
        if let Some((checkpoint, _)) = checkpoint {
            let entries: Vec<_> = changes
                .iter()
                .map(|change| crate::checkpoint::CheckpointEntry {
                    path: change.path.clone(),
                    before_digest: change.before_digest.clone(),
                    after_digest: change.after_digest.clone().unwrap_or_default(),
                })
                .collect();
            if entries != checkpoint.entries {
                // A legacy wall-clock manifest may describe a different before
                // image. Never silently restore a different checkpoint.
                return Err(WorkspaceChangeError::Conflict { receipt: None }.into());
            }
        }
        let mut targets = Vec::new();
        let mut originals = Vec::new();
        for change in &changes {
            let target = restore_observe!(
                self,
                id,
                &change.path,
                Resolve,
                self.workspace.resolve(
                    std::path::Path::new(&task.workspace),
                    &change.path,
                    true,
                    &task.spec.write_scopes,
                )
            )
            .map_err(|_| WorkspaceChangeError::Conflict { receipt: None })?;
            let current = restore_observe!(
                self,
                id,
                &change.path,
                ReadCurrent,
                self.workspace.read_safe_file(&target)
            )
            .map_err(|_| WorkspaceChangeError::Conflict { receipt: None })?;
            if current.as_deref().map(digest) != change.after_digest.as_deref().map(str::to_owned) {
                return Err(WorkspaceChangeError::Conflict { receipt: None }.into());
            }
            targets.push(target);
            let before = match change.kind {
                ChangeKind::Created => None,
                ChangeKind::Modified => {
                    let sealed = change
                        .before_blob
                        .as_deref()
                        .ok_or(WorkspaceChangeError::Corrupt)?;
                    let value = secrets::open(sealed).map_err(|_| WorkspaceChangeError::Corrupt)?;
                    if Some(digest(&value)) != change.before_digest {
                        return Err(WorkspaceChangeError::Corrupt.into());
                    }
                    Some(value)
                }
            };
            originals.push(before);
        }
        let receipt = self.store.claim_restore(id, &changes)?;
        anyhow::ensure!(receipt.status == RestoreStatus::Claimed, "恢复领取状态损坏");
        let restore_id = receipt
            .restore_id
            .as_deref()
            .ok_or(WorkspaceChangeError::Corrupt)?
            .to_owned();
        #[cfg(test)]
        crash_probe::block(id, "restore_pre_fs", &restore_id);
        for (completed, ((change, target), original)) in
            changes.iter().zip(targets).zip(originals).enumerate()
        {
            let action = (|| -> Result<()> {
                #[cfg(test)]
                anyhow::ensure!(
                    !crash_probe::take_restore_failure(id, &change.path),
                    "injected restore path failure"
                );
                restore_observe!(
                    self,
                    id,
                    &change.path,
                    RootValidation,
                    self.project.validate_task(&task, false, false)
                )?;
                let checked = restore_observe!(
                    self,
                    id,
                    &change.path,
                    Resolve,
                    self.workspace.resolve(
                        std::path::Path::new(&task.workspace),
                        &change.path,
                        true,
                        &task.spec.write_scopes,
                    )
                )?;
                anyhow::ensure!(checked == target, "恢复路径身份发生变化");
                let current = restore_observe!(
                    self,
                    id,
                    &change.path,
                    ReadCurrent,
                    self.workspace.read_safe_file(&target)
                )?;
                anyhow::ensure!(
                    current.as_deref().map(digest)
                        == change.after_digest.as_deref().map(str::to_owned),
                    "恢复前内容发生变化"
                );
                restore_observe!(
                    self,
                    id,
                    &change.path,
                    FileAction,
                    (|| -> Result<()> {
                        match change.kind {
                            ChangeKind::Created => std::fs::remove_file(&target)?,
                            ChangeKind::Modified => {
                                self.workspace.atomic_replace(
                                    &target,
                                    original.as_deref().ok_or(WorkspaceChangeError::Corrupt)?,
                                )?;
                            }
                        }
                        Ok(())
                    })()
                )?;
                Ok(())
            })();
            if action.is_err() {
                let status = if completed == 0 {
                    RestoreStatus::Unknown
                } else {
                    RestoreStatus::Partial
                };
                let saved = self.store.finish_restore(&restore_id, status);
                let failed_to_persist = saved.is_err();
                let receipt = saved.unwrap_or_else(|_| self.unknown_restore_receipt(&receipt));
                return Err(if status == RestoreStatus::Partial && !failed_to_persist {
                    WorkspaceChangeError::Conflict {
                        receipt: Some(receipt),
                    }
                } else {
                    WorkspaceChangeError::Unknown {
                        receipt: Some(receipt),
                    }
                }
                .into());
            }
            #[cfg(test)]
            crash_probe::block(id, "restore_post_fs", &restore_id);
            if restore_observe!(
                self,
                id,
                &change.path,
                OutcomePersist,
                self.store.finish_restore_path(&restore_id, &change.id)
            )
            .is_err()
            {
                let unknown = self
                    .store
                    .finish_restore(&restore_id, RestoreStatus::Unknown)
                    .unwrap_or_else(|_| self.unknown_restore_receipt(&receipt));
                return Err(WorkspaceChangeError::Unknown {
                    receipt: Some(unknown),
                }
                .into());
            }
            #[cfg(test)]
            self.emit_restore_probe(
                id,
                &change.path,
                restore_probe::Stage::OutcomeCommitted,
                &Ok(()),
            );
        }
        match self
            .store
            .finish_restore(&restore_id, RestoreStatus::Complete)
        {
            Ok(value) => Ok(value),
            Err(_) => {
                let unknown = self
                    .store
                    .finish_restore(&restore_id, RestoreStatus::Unknown)
                    .unwrap_or_else(|_| self.unknown_restore_receipt(&receipt));
                Err(WorkspaceChangeError::Unknown {
                    receipt: Some(unknown),
                }
                .into())
            }
        }
    }
    fn unknown_restore_receipt(&self, claimed: &RestoreReceipt) -> RestoreReceipt {
        let mut receipt = claimed
            .restore_id
            .as_deref()
            .and_then(|id| self.store.restore_receipt(id).ok())
            .unwrap_or_else(|| claimed.clone());
        receipt.status = RestoreStatus::Unknown;
        for outcome in &mut receipt.outcomes {
            if outcome.status == RestoreStatus::Claimed {
                outcome.status = RestoreStatus::Unknown;
            }
        }
        receipt
    }
    /// Review a single path in the workspace bound to this Task. The same gate
    /// serializes reads with this Host's writes, starts, resumes and restores.
    pub(crate) async fn git_diff(
        &self,
        task_id: &str,
        request: crate::git_diff::GitDiffRequest,
    ) -> Result<crate::git_diff::GitDiff> {
        let _gate = self.workspace.gate.lock().await;
        self.workspace.ensure_accepting()?;
        let task = self.store.task(task_id).map_err(|error| {
            if error.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                )
            }) {
                anyhow::Error::from(WorkspaceChangeError::NotFound)
            } else {
                error
            }
        })?;
        self.project.validate_task(&task, false, false)?;
        let root = std::path::Path::new(&task.workspace);
        let canonical = root
            .canonicalize()
            .map_err(|_| WorkspaceChangeError::Conflict { receipt: None })?;
        let active: Vec<_> = self
            .active
            .lock()
            .unwrap()
            .iter()
            .map(|(id, a)| (id.clone(), a.workspace.clone()))
            .collect();
        for (id, workspace) in active {
            if id == task_id
                || std::path::Path::new(&workspace)
                    .canonicalize()
                    .map_err(|_| WorkspaceChangeError::Conflict { receipt: None })?
                    == canonical
            {
                return Err(WorkspaceChangeError::Active.into());
            }
        }
        let operation = self
            .store
            .execution_operation(self.workspace.stopping.clone())?;
        Ok(operation
            .run(crate::git_diff::read_git_diff(root, request))
            .await?)
    }

    pub(crate) fn checkpoint(&self, checkpoint_id: &str) -> Result<crate::checkpoint::Checkpoint> {
        Ok(self.store.checkpoint_record(checkpoint_id)?.0)
    }

    pub(crate) async fn create_checkpoint(
        &self,
        task_id: &str,
        key: &str,
    ) -> Result<crate::checkpoint::CheckpointCreated> {
        let operation = self
            .store
            .execution_operation(self.workspace.stopping.clone())?;
        operation
            .run(self.create_checkpoint_in_domain(task_id, key))
            .await
    }

    async fn create_checkpoint_in_domain(
        &self,
        task_id: &str,
        key: &str,
    ) -> Result<crate::checkpoint::CheckpointCreated> {
        use crate::checkpoint::{CheckpointCreated, CheckpointEntry, CheckpointError, Manifest};
        secrets::validate_idempotency_key(key).map_err(|_| CheckpointError::Invalid)?;
        let _gate = self.workspace.gate.lock().await;
        self.workspace.ensure_accepting()?;
        if let Some((checkpoint, old_key)) = self.store.checkpoint_for_task(task_id)? {
            if old_key != key {
                return Err(CheckpointError::Conflict.into());
            }
            return Ok(CheckpointCreated {
                checkpoint,
                replayed: true,
            });
        }
        let task = self.store.task(task_id).map_err(|error| {
            if error.chain().any(|c| {
                matches!(
                    c.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                )
            }) {
                CheckpointError::NotFound
            } else {
                CheckpointError::Corrupt
            }
        })?;
        if task.status != "completed" {
            return Err(CheckpointError::Unrestorable.into());
        }
        self.project.validate_task(&task, false, false)?;
        let root = std::path::Path::new(&task.workspace);
        let canonical = root.canonicalize().map_err(|_| CheckpointError::Conflict)?;
        let active_ids: Vec<_> = self.active.lock().unwrap().keys().cloned().collect();
        for id in active_ids {
            if id == task_id
                || std::path::Path::new(&self.store.task(&id)?.workspace)
                    .canonicalize()
                    .map_err(|_| CheckpointError::Conflict)?
                    == canonical
            {
                return Err(CheckpointError::Conflict.into());
            }
        }
        if self.store.latest_restore(task_id)?.is_some()
            || self.store.has_legacy_file_backup(task_id)?
        {
            return Err(CheckpointError::Unrestorable.into());
        }
        let records = self
            .store
            .workspace_changes_for_task(task_id)
            .map_err(|_| CheckpointError::Corrupt)?;
        if records.iter().any(|r| {
            matches!(r.state, ChangeState::Unknown | ChangeState::Prepared)
                || r.restore_state != "pending"
        }) {
            return Err(CheckpointError::Unrestorable.into());
        }
        let finished: Vec<_> = records
            .iter()
            .filter(|r| r.state == ChangeState::Finished)
            .cloned()
            .collect();
        if finished.is_empty() {
            return Err(CheckpointError::Unrestorable.into());
        }
        // Authenticate every protected before, including intermediate writes, before committing any fact.
        let mut total = 0usize;
        for record in &finished {
            match record.kind {
                ChangeKind::Created
                    if record.before_blob.is_none() && record.before_digest.is_none() => {}
                ChangeKind::Modified => {
                    let bytes = secrets::open(
                        record
                            .before_blob
                            .as_deref()
                            .ok_or(CheckpointError::Corrupt)?,
                    )
                    .map_err(|_| CheckpointError::Corrupt)?;
                    if Some(digest(&bytes)) != record.before_digest {
                        return Err(CheckpointError::Corrupt.into());
                    }
                    total = total
                        .checked_add(bytes.len())
                        .ok_or(CheckpointError::Unrestorable)?;
                    if total > 8 * 1024 * 1024 {
                        return Err(CheckpointError::Unrestorable.into());
                    }
                }
                _ => return Err(CheckpointError::Corrupt.into()),
            }
        }
        let changes = self
            .validated_restore_changes(&task, finished)
            .map_err(|e| match e.downcast_ref::<WorkspaceChangeError>() {
                Some(WorkspaceChangeError::Conflict { .. }) => CheckpointError::Conflict,
                _ => CheckpointError::Corrupt,
            })?;
        if changes.len() > 128 {
            return Err(CheckpointError::Unrestorable.into());
        }
        let mut entries = Vec::new();
        for change in changes {
            let path = self
                .workspace
                .resolve(root, &change.path, true, &task.spec.write_scopes)
                .map_err(|_| CheckpointError::Conflict)?;
            let current = self
                .workspace
                .read_safe_file(&path)
                .map_err(|_| CheckpointError::Conflict)?;
            if current.as_deref().map(digest) != change.after_digest {
                return Err(CheckpointError::Conflict.into());
            }
            entries.push(CheckpointEntry {
                path: change.path,
                before_digest: change.before_digest,
                after_digest: change.after_digest.ok_or(CheckpointError::Corrupt)?,
            });
        }
        let manifest = Manifest {
            task_id: task_id.into(),
            workspace: task.workspace.clone(),
            write_scopes: task.spec.write_scopes.clone(),
            source_digest: crate::checkpoint::source_digest(&records)?,
            entries,
        };
        self.store.commit_checkpoint(&task, key, &manifest)
    }

    pub(crate) async fn restore_checkpoint(&self, checkpoint_id: &str) -> Result<RestoreReceipt> {
        let operation = self
            .store
            .execution_operation(self.workspace.stopping.clone())?;
        operation
            .run(async {
                let _gate = self.workspace.gate.lock().await;
                self.workspace.ensure_accepting()?;
                let (checkpoint, _) = self.store.checkpoint_record(checkpoint_id)?;
                #[cfg(test)]
                crash_probe::block(&checkpoint.task_id, "checkpoint_pre_claim", checkpoint_id);
                self.restore_locked(&checkpoint.task_id)
            })
            .await
    }
}

pub(crate) struct ProjectService {
    store: Arc<Store>,
    gate: Arc<AsyncMutex<()>>,
}
impl ProjectService {
    pub(crate) const ID: &'static str = "host.local-project";
    pub(crate) const VERSION: &'static str = "1.0.0";
    pub(crate) fn new(store: Arc<Store>, gate: Arc<AsyncMutex<()>>) -> Self {
        Self { store, gate }
    }
    pub(crate) fn list(&self) -> Result<Vec<Project>> {
        self.store.projects()
    }
    pub(crate) fn get(&self, id: &ProjectId) -> Result<Project> {
        self.store.project(id)
    }
    pub(crate) fn validate_task(
        &self,
        task: &Task,
        permissions: bool,
        model: bool,
    ) -> Result<TaskProjectBinding> {
        self.store.validate_project_task(task, permissions, model)
    }
    pub(crate) fn validate_files_read(&self, task: &Task) -> Result<()> {
        self.store.validate_files_read(task)
    }
    pub(crate) fn config(&self, id: &ProjectId) -> Result<ProjectConfigSnapshot> {
        self.store.project_config(id)
    }
    pub(crate) fn current(&self) -> Result<ProjectSelection> {
        self.store.current_project()
    }
    pub(crate) fn task_binding(&self, id: &str) -> Result<Option<TaskProjectBinding>> {
        self.store.task_project_binding(id)
    }
    pub(crate) fn workspace(&self, id: &ProjectId) -> Result<std::path::PathBuf> {
        self.store.project_workspace(id)
    }
    pub(crate) fn runs(&self, id: &ProjectId) -> Result<Vec<Run>> {
        self.store.project_runs(id)
    }
    pub(crate) async fn register(&self, request: RegisterProject) -> Result<Project> {
        let _gate = self.gate.lock().await;
        self.store.register_project(request)
    }
    pub(crate) async fn configure(
        &self,
        request: ConfigureProject,
    ) -> Result<ProjectConfigSnapshot> {
        let _gate = self.gate.lock().await;
        self.store.configure_project(request)
    }
    pub(crate) async fn select(&self, request: SelectProject) -> Result<ProjectSelection> {
        let _gate = self.gate.lock().await;
        self.store.select_project(request)
    }
}

pub(crate) struct ApprovalService {
    store: Arc<Store>,
}
impl ApprovalService {
    pub(crate) const ID: &'static str = "host.approval-policy";
    pub(crate) const VERSION: &'static str = "1.0.0";
    pub(crate) fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
    pub(crate) fn evaluate(&self, task: &Task, name: &str) -> approval::PolicyDecision {
        approval::evaluate(task, name)
    }
    pub(crate) fn get(&self, id: &str) -> Result<approval::ApprovalRecord> {
        self.store.approval(id)
    }
    pub(crate) fn for_task(&self, id: &str) -> Result<Vec<approval::ApprovalRecord>> {
        self.store.approvals_for_task(id)
    }
    pub(crate) fn decide(
        &self,
        op: &crate::execution_authority::ExecutionOperation,
        id: &str,
        approved: bool,
        by: &str,
        ack: bool,
    ) -> Result<approval::ApprovalRecord> {
        self.store
            .decide_approval_with_network_ack_for(id, approved, by, ack, op)
    }
    pub(crate) fn ensure(
        &self,
        task: &Task,
        id: &str,
        name: &str,
        args: &Value,
    ) -> Result<(approval::ApprovalRecord, bool)> {
        self.store.ensure_current_approval(task, id, name, args)
    }
    pub(crate) fn claim(
        &self,
        task: &Task,
        id: &str,
        name: &str,
        args: &Value,
    ) -> Result<approval::ApprovalRecord> {
        self.store.claim_current_approval(task, id, name, args)
    }
}

pub(crate) struct SessionService {
    store: Arc<Store>,
}
impl SessionService {
    pub(crate) const ID: &'static str = "host.session-log";
    pub(crate) const VERSION: &'static str = "1.0.0";
    pub(crate) fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
    pub(crate) fn session(&self, id: &SessionId) -> Result<Session> {
        self.store.session(id)
    }
    pub(crate) fn turns_for_session(&self, id: &SessionId) -> Result<Vec<Turn>> {
        self.store.turns_for_session(id)
    }
    pub(crate) fn turn(&self, id: &TurnId) -> Result<Turn> {
        self.store.turn(id)
    }
    pub(crate) fn run(&self, id: &str) -> Result<Run> {
        self.store.run(id)
    }
    pub(crate) fn runs(&self) -> Result<Vec<Value>> {
        self.store.runs()
    }
    pub(crate) fn run_context(&self, id: &str) -> Result<RunContext> {
        self.store.run_context(id)
    }
    pub(crate) fn events(&self, id: &str, after: i64) -> Result<Vec<Event>> {
        self.store.events(id, after)
    }
    pub(crate) fn task(&self, id: &str) -> Result<Task> {
        self.store.task(id)
    }
}

pub(crate) struct RecoveryService {
    store: Arc<Store>,
    controller: crate::execution_authority::ExecutionController,
}
impl RecoveryService {
    pub(crate) const ID: &'static str = "host.recovery";
    pub(crate) const VERSION: &'static str = "1.0.0";
    pub(crate) fn new(store: Arc<Store>) -> Result<Self> {
        let controller = store.authority.claim()?;
        Ok(Self { store, controller })
    }
    pub(crate) fn owns(&self, store: &Arc<Store>) -> bool {
        Arc::ptr_eq(&self.store, store)
    }
    pub(crate) fn recover(&self) -> Result<usize> {
        self.store.recover_with_controller(&self.controller)
    }
}

pub(crate) struct WorkspaceSafety {
    pub(crate) gate: Arc<AsyncMutex<()>>,
    pub(crate) stopping: CancellationToken,
}
impl WorkspaceSafety {
    pub(crate) const ID: &'static str = "host.workspace";
    pub(crate) const VERSION: &'static str = "1.0.0";
    pub(crate) fn new(gate: Arc<AsyncMutex<()>>) -> Self {
        Self {
            gate,
            stopping: CancellationToken::new(),
        }
    }
    fn ensure_accepting(&self) -> Result<()> {
        anyhow::ensure!(!self.stopping.is_cancelled(), "Host 正在关闭，拒绝新工作");
        Ok(())
    }
    pub(crate) fn resolve(
        &self,
        root: &std::path::Path,
        relative: &str,
        writing: bool,
        scopes: &[String],
    ) -> Result<std::path::PathBuf> {
        workspace::resolve(root, relative, writing, scopes)
    }
    pub(crate) fn change_target(
        &self,
        root: &std::path::Path,
        relative: &str,
        scopes: &[String],
    ) -> Result<(std::path::PathBuf, String, String)> {
        workspace::change_target(root, relative, scopes)
    }
    pub(crate) fn read_safe_file(&self, path: &std::path::Path) -> Result<Option<Vec<u8>>> {
        workspace::read_safe_file(path)
    }
    pub(crate) fn atomic_replace(&self, path: &std::path::Path, bytes: &[u8]) -> Result<()> {
        workspace::atomic_replace(path, bytes)
    }
}
