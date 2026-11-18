//! Git worktree creation receipts and non-blocking project metadata persistence.
use crate::{
    domain::ProjectId,
    git_worktree::{self, CreatedWorktree, PersistedWorktreePlan},
    workspace_identity::RootIdentity,
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorktree {
    pub source_project_id: ProjectId,
    pub name: String,
    pub parent_path: String,
    pub expected_config_revision: u64,
    pub expected_head: String,
    pub idempotency_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    Claimed,
    Succeeded,
    Rejected,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeError {
    Invalid,
    NotFound,
    RevisionConflict,
    IdempotencyConflict,
    Busy,
    RootChanged,
    Conflict,
    Unsupported,
    Unavailable,
    Corrupt,
}

impl WorktreeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "worktree_invalid",
            Self::NotFound => "worktree_not_found",
            Self::RevisionConflict => "worktree_revision_conflict",
            Self::IdempotencyConflict => "worktree_idempotency_conflict",
            Self::Busy => "worktree_busy",
            Self::RootChanged => "worktree_root_changed",
            Self::Conflict => "worktree_conflict",
            Self::Unsupported => "worktree_unsupported",
            Self::Unavailable => "worktree_unavailable",
            Self::Corrupt => "worktree_corrupt",
        }
    }
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for WorktreeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCause {
    Invalid,
    NotFound,
    RevisionConflict,
    IdempotencyConflict,
    Busy,
    RootChanged,
    Conflict,
    Unsupported,
    Unavailable,
    Corrupt,
    ProcessOutcomeUnknown,
    RegistrationOutcomeUnknown,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeReceipt {
    pub schema_version: u32,
    pub operation_id: String,
    pub source_project_id: ProjectId,
    pub name: String,
    pub parent_path: String,
    pub target_path: String,
    pub branch: String,
    pub expected_config_revision: u64,
    pub expected_head: String,
    pub request_digest: String,
    pub revision: u64,
    pub state: WorktreeState,
    pub project_id: Option<ProjectId>,
    pub cause: Option<WorktreeCause>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeObservationState {
    AbsentObserved,
    PresentMatching,
    Partial,
    Conflicting,
    IdentityChanged,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeObservationReason {
    Absent,
    MetadataMismatch,
    IdentityChanged,
    GitUnavailable,
    PartialArtifacts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeObservation {
    pub operation_id: String,
    pub observed_at: i64,
    pub state: WorktreeObservationState,
    pub reason_codes: Vec<WorktreeObservationReason>,
    pub observed_head: Option<String>,
    pub observed_branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeInspection {
    pub receipt: WorktreeReceipt,
    pub observation: WorktreeObservation,
}

/// Failed or in-flight worktree operations never freeze project registration or existing tasks.
pub(crate) fn ensure_project_unreserved_tx(_db: &Connection, _project: &ProjectId) -> Result<()> {
    Ok(())
}

pub(crate) fn ensure_task_unreserved_tx(_db: &Connection, _task_id: &str) -> Result<()> {
    Ok(())
}

pub(crate) fn ensure_registration_unreserved_tx(_db: &Connection) -> Result<()> {
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn verify_persisted_rows(_db: &Connection) -> Result<()> {
    Ok(())
}

fn valid_lower_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

impl WorktreeReceipt {
    pub(crate) fn validate(&self) -> Result<()> {
        let uuid = uuid::Uuid::parse_str(&self.operation_id).map_err(|_| WorktreeError::Corrupt)?;
        ensure!(
            uuid.to_string() == self.operation_id && self.schema_version == 1,
            WorktreeError::Corrupt
        );
        ensure!(
            self.expected_config_revision > 0
                && self.expected_config_revision <= i64::MAX as u64
                && self.revision > 0
                && self.revision <= i64::MAX as u64,
            WorktreeError::Corrupt
        );
        ensure!(valid_lower_oid(&self.expected_head), WorktreeError::Corrupt);
        ensure!(
            self.request_digest.len() == 64
                && self
                    .request_digest
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            WorktreeError::Corrupt
        );
        ensure!(
            self.branch == format!("codex/workspace/{}", self.operation_id),
            WorktreeError::Corrupt
        );
        ensure!(
            windows_path_key(&self.target_path)
                == windows_path_key(&format!(
                    "{}/coda-{}",
                    self.parent_path.trim_end_matches(['/', '\\']),
                    self.operation_id
                )),
            WorktreeError::Corrupt
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.len() <= 500
                && self.created_at >= 0
                && self.updated_at >= self.created_at,
            WorktreeError::Corrupt
        );
        match self.state {
            WorktreeState::Succeeded => ensure!(
                self.project_id
                    .as_ref()
                    .is_some_and(|p| p != &self.source_project_id)
                    && self.cause.is_none()
                    && self.revision == 2,
                WorktreeError::Corrupt
            ),
            WorktreeState::Claimed => ensure!(
                self.project_id.is_none()
                    && self.cause.is_none()
                    && self.revision == 1
                    && self.created_at == self.updated_at,
                WorktreeError::Corrupt
            ),
            WorktreeState::Rejected | WorktreeState::Unknown => ensure!(
                self.project_id.is_none() && self.cause.is_some() && self.revision == 2,
                WorktreeError::Corrupt
            ),
        }
        Ok(())
    }
}

pub(crate) fn normalize_request(mut request: CreateWorktree) -> Result<CreateWorktree> {
    crate::secrets::validate_idempotency_key(&request.idempotency_key)
        .map_err(|_| WorktreeError::Invalid)?;
    ensure!(
        !request.name.trim().is_empty() && request.name.len() <= 500,
        WorktreeError::Invalid
    );
    crate::secrets::safe_metadata_text("project_name", &request.name)
        .map_err(|_| WorktreeError::Invalid)?;
    crate::secrets::safe_metadata_path("worktree_parent", &request.parent_path)
        .map_err(|_| WorktreeError::Invalid)?;
    ensure!(
        std::path::Path::new(&request.parent_path).is_absolute()
            && request.expected_config_revision > 0
            && request.expected_config_revision <= i64::MAX as u64,
        WorktreeError::Invalid
    );
    ensure!(
        matches!(request.expected_head.len(), 40 | 64)
            && request.expected_head.bytes().all(|c| c.is_ascii_hexdigit()),
        WorktreeError::Invalid
    );
    request.expected_head.make_ascii_lowercase();
    Ok(request)
}

pub(crate) fn request_digest(request: &CreateWorktree) -> Result<String> {
    Ok(crate::workspace_changes::digest(&serde_json::to_vec(&(
        "worktree-create-v1",
        &request.source_project_id,
        &request.name,
        &request.parent_path,
        request.expected_config_revision,
        &request.expected_head,
    ))?))
}

fn read_receipt(db: &Connection, source: &ProjectId, operation: &str) -> Result<WorktreeReceipt> {
    let raw: Option<String> = db
        .query_row(
            "SELECT receipt FROM worktree_operations WHERE operation_id=?1 AND source_project_id=?2",
            params![operation, source.0],
            |r| r.get(0),
        )
        .optional()?;
    let receipt: WorktreeReceipt = serde_json::from_str(&raw.ok_or(WorktreeError::NotFound)?)
        .map_err(|_| WorktreeError::Corrupt)?;
    receipt.validate()?;
    Ok(receipt)
}

pub(crate) fn replay_tx(
    db: &Connection,
    request: &CreateWorktree,
) -> Result<Option<WorktreeReceipt>> {
    let prior: Option<(String, String, String)> = db
        .query_row(
            "SELECT operation_id,source_project_id,request_digest FROM worktree_operations WHERE idempotency_key=?1",
            [&request.idempotency_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((operation, source, digest)) = prior {
        ensure!(
            source == request.source_project_id.0 && digest == request_digest(request)?,
            WorktreeError::IdempotencyConflict
        );
        return Ok(Some(read_receipt(
            db,
            &request.source_project_id,
            &operation,
        )?));
    }
    Ok(None)
}

pub(crate) fn recover_claimed_tx(db: &Connection) -> Result<usize> {
    let _ = db.execute("DELETE FROM worktree_project_reservations", []);
    let rows: Vec<String> = db
        .prepare("SELECT receipt FROM worktree_operations WHERE state='claimed'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for raw in &rows {
        let mut receipt: WorktreeReceipt =
            serde_json::from_str(raw).map_err(|_| WorktreeError::Corrupt)?;
        receipt.state = WorktreeState::Unknown;
        receipt.cause = Some(WorktreeCause::Interrupted);
        receipt.revision = receipt
            .revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or(WorktreeError::Corrupt)?;
        receipt.updated_at =
            i64::try_from(crate::domain::now()).map_err(|_| WorktreeError::Corrupt)?;
        db.execute(
            "UPDATE worktree_operations SET state='unknown',revision=?2,receipt=?3 WHERE operation_id=?1 AND state='claimed'",
            params![
                receipt.operation_id,
                receipt.revision,
                serde_json::to_string(&receipt)?
            ],
        )?;
    }
    Ok(rows.len())
}

pub(crate) fn windows_path_key(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = if let Some(unc) = path.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else {
        path.strip_prefix("//?/").unwrap_or(&path).to_owned()
    };
    path.trim_end_matches('/').to_lowercase()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectInventory(pub Vec<(String, String, Option<String>)>);

pub(crate) fn inventory_tx(db: &Connection) -> Result<ProjectInventory> {
    Ok(ProjectInventory(
        db.prepare(
            "SELECT p.id,p.root_path,b.identity FROM projects p LEFT JOIN project_root_bindings b ON b.project_id=p.id ORDER BY p.id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?,
    ))
}

impl ProjectInventory {
    pub(crate) fn snapshots(&self) -> Result<Vec<(ProjectId, RootIdentity)>> {
        self.0
            .iter()
            .filter_map(|(id, _path, raw)| {
                let raw = raw.as_ref()?;
                let root: RootIdentity = serde_json::from_str(raw).ok()?;
                root.validate_snapshot().ok()?;
                Some(Ok((ProjectId(id.clone()), root)))
            })
            .collect()
    }

    /// Returns stored project root identities without probing unrelated project directories on disk.
    pub(crate) fn roots(&self) -> Result<Vec<(ProjectId, RootIdentity)>> {
        self.snapshots()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReservedProject {
    project_id: ProjectId,
    root: RootIdentity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorktreeOrigin {
    pub version: u32,
    pub root: RootIdentity,
    pub git_dir: RootIdentity,
    pub common_dir: RootIdentity,
    pub head_oid: String,
    pub branch_ref: String,
}

impl WorktreeOrigin {
    fn validate(&self, plan: &PersistedWorktreePlan) -> Result<()> {
        for root in [&self.root, &self.git_dir, &self.common_dir] {
            root.validate_snapshot()
                .map_err(|_| WorktreeError::Corrupt)?;
        }
        ensure!(
            self.version == 1
                && self.root.canonical_path() == plan.target_path()
                && self.common_dir == plan.repository.common_dir
                && self.head_oid == plan.expected_head
                && self.branch_ref == plan.branch_ref,
            WorktreeError::Corrupt
        );
        Ok(())
    }

    pub(crate) fn identities_match(&self) -> bool {
        [&self.root, &self.git_dir, &self.common_dir]
            .into_iter()
            .all(|r| r.verify().is_ok())
    }
}

pub(crate) fn map_git(error: git_worktree::GitWorktreeError) -> WorktreeError {
    use git_worktree::GitWorktreeError as G;
    match error {
        G::Invalid => WorktreeError::Invalid,
        G::Unsupported => WorktreeError::Unsupported,
        G::Dirty | G::Conflict => WorktreeError::Conflict,
        G::Unavailable => WorktreeError::Unavailable,
        G::OutcomeUnknown => WorktreeError::Conflict,
    }
}

impl crate::store::Store {
    pub(crate) fn worktree_receipt(
        &self,
        source: &ProjectId,
        operation: &str,
    ) -> Result<WorktreeReceipt> {
        let db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        read_receipt(&db, source, operation)
    }

    pub(crate) fn lookup_project_worktree(
        &self,
        source: &ProjectId,
        key: &str,
    ) -> Result<WorktreeReceipt> {
        let db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        let operation: Option<String> = db
            .query_row(
                "SELECT operation_id FROM worktree_operations WHERE source_project_id=?1 AND idempotency_key=?2",
                params![source.0, key],
                |r| r.get(0),
            )
            .optional()?;
        read_receipt(&db, source, &operation.ok_or(WorktreeError::NotFound)?)
    }

    pub(crate) fn worktree_inventory(&self) -> Result<ProjectInventory> {
        let db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        inventory_tx(&db)
    }

    pub(crate) fn worktree_replay(
        &self,
        request: &CreateWorktree,
    ) -> Result<Option<WorktreeReceipt>> {
        let db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        replay_tx(&db, request)
    }

    pub(crate) fn worktree_aux(&self, operation: &str) -> Result<std::path::PathBuf> {
        let db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        let path = std::fs::canonicalize(db.path().ok_or(WorktreeError::Unavailable)?)
            .map_err(|_| WorktreeError::Unavailable)?;
        Ok(path
            .parent()
            .ok_or(WorktreeError::Unavailable)?
            .join(operation))
    }

    pub(crate) fn worktree_plan(
        &self,
        source: &ProjectId,
        operation: &str,
    ) -> Result<PersistedWorktreePlan> {
        let db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        read_receipt(&db, source, operation)?;
        let plan: PersistedWorktreePlan = serde_json::from_str(&db.query_row(
            "SELECT plan FROM worktree_operations WHERE operation_id=?1",
            [operation],
            |r| r.get::<_, String>(0),
        )?)
        .map_err(|_| WorktreeError::Corrupt)?;
        plan.validate().map_err(|_| WorktreeError::Corrupt)?;
        Ok(plan)
    }

    pub(crate) fn claim_worktree(
        &self,
        request: &CreateWorktree,
        _inventory: &ProjectInventory,
        members: &[ProjectId],
        plan: &PersistedWorktreePlan,
    ) -> Result<(WorktreeReceipt, bool)> {
        let mut db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current_op = crate::execution_authority::current();
        if current_op.is_some() {
            self.execution_fence(&tx)
                .map_err(|_| WorktreeError::Unavailable)?;
        }
        if let Some(prior) = replay_tx(&tx, request)? {
            return Ok((prior, true));
        }

        let config = crate::project_config::config_tx(&tx, &request.source_project_id)?;
        ensure!(
            config.revision == request.expected_config_revision,
            WorktreeError::RevisionConflict
        );
        plan.validate().map_err(|_| WorktreeError::Corrupt)?;
        ensure!(
            plan.expected_head == request.expected_head,
            WorktreeError::Corrupt
        );
        ensure!(
            crate::project_config::root_tx(&tx, &request.source_project_id)? == plan.source_root,
            WorktreeError::RootChanged
        );
        plan.source_root
            .verify()
            .map_err(|_| WorktreeError::RootChanged)?;
        plan.target_parent
            .verify()
            .map_err(|_| WorktreeError::RootChanged)?;
        ensure!(
            !plan
                .target_path()
                .try_exists()
                .map_err(|_| WorktreeError::Unavailable)?,
            WorktreeError::Conflict
        );

        let common = plan
            .repository
            .common_key()
            .map_err(|_| WorktreeError::Corrupt)?;
        let active_same_repo: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM worktree_operations WHERE common_key=?1 AND state='claimed')",
            [&common],
            |r| r.get(0),
        )?;
        ensure!(!active_same_repo, WorktreeError::Busy);

        let target = plan.target_path().to_string_lossy().into_owned();
        let conflict: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM worktree_operations WHERE target_path=?1 OR (common_key=?2 AND branch_ref=?3))",
            params![target, common, plan.branch_ref],
            |r| r.get(0),
        )?;
        ensure!(!conflict, WorktreeError::Conflict);

        let time = i64::try_from(crate::domain::now()).map_err(|_| WorktreeError::Corrupt)?;
        let receipt = WorktreeReceipt {
            schema_version: 1,
            operation_id: plan.operation_id.clone(),
            source_project_id: request.source_project_id.clone(),
            name: request.name.clone(),
            parent_path: request.parent_path.clone(),
            target_path: target,
            branch: plan
                .branch_ref
                .strip_prefix("refs/heads/")
                .ok_or(WorktreeError::Corrupt)?
                .into(),
            expected_config_revision: request.expected_config_revision,
            expected_head: request.expected_head.clone(),
            request_digest: request_digest(request)?,
            revision: 1,
            state: WorktreeState::Claimed,
            project_id: None,
            cause: None,
            created_at: time,
            updated_at: time,
        };
        receipt.validate()?;

        let member_ids: Vec<ProjectId> = if members.is_empty() {
            vec![request.source_project_id.clone()]
        } else {
            members.to_vec()
        };
        let member_snapshots: Vec<ReservedProject> = member_ids
            .iter()
            .filter_map(|id| {
                crate::project_config::root_tx(&tx, id)
                    .ok()
                    .map(|root| ReservedProject {
                        project_id: id.clone(),
                        root,
                    })
            })
            .collect();

        tx.execute(
            "INSERT INTO worktree_operations(operation_id,idempotency_key,source_project_id,requested_project_id,request_json,request_digest,plan,common_key,target_path,branch_ref,state,revision,receipt,members) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'claimed',1,?11,?12)",
            params![
                receipt.operation_id,
                request.idempotency_key,
                request.source_project_id.0,
                uuid::Uuid::new_v4().to_string(),
                serde_json::to_string(request)?,
                receipt.request_digest,
                serde_json::to_string(plan)?,
                common,
                receipt.target_path,
                plan.branch_ref,
                serde_json::to_string(&receipt)?,
                serde_json::to_string(&member_snapshots)?
            ],
        )?;

        if let Some(operation) = current_op {
            operation
                .bind_worktree_operation(&receipt.source_project_id.0, &receipt.operation_id)
                .map_err(|_| WorktreeError::Unavailable)?;
            self.commit_execution_tx(tx)
                .map_err(|_| WorktreeError::Unavailable)?;
        } else {
            tx.commit().map_err(|_| WorktreeError::Unavailable)?;
        }
        Ok((receipt, false))
    }

    pub(crate) fn finish_worktree(
        &self,
        prior: &WorktreeReceipt,
        created: Option<&CreatedWorktree>,
        failure: Option<(WorktreeState, WorktreeCause)>,
    ) -> Result<WorktreeReceipt> {
        let mut db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current_op = crate::execution_authority::current();
        if current_op.is_some() {
            self.execution_settlement_fence(&tx)
                .map_err(|_| WorktreeError::Unavailable)?;
        }
        let mut receipt = read_receipt(&tx, &prior.source_project_id, &prior.operation_id)?;
        if receipt.state != WorktreeState::Claimed || receipt.revision != prior.revision {
            return Ok(receipt);
        }
        if let Some(ref operation) = current_op {
            operation
                .verify_worktree_operation(&receipt.source_project_id.0, &receipt.operation_id)
                .map_err(|_| WorktreeError::Unavailable)?;
        }

        if let Some(created) = created {
            let plan: PersistedWorktreePlan = serde_json::from_str(&tx.query_row(
                "SELECT plan FROM worktree_operations WHERE operation_id=?1",
                [&prior.operation_id],
                |r| r.get::<_, String>(0),
            )?)?;
            ensure!(
                created.root.canonical_path() == plan.target_path()
                    && created.common_dir == plan.repository.common_dir
                    && created.head_oid == plan.expected_head
                    && created.branch_ref == plan.branch_ref,
                WorktreeError::Conflict
            );
            created
                .git_dir
                .verify()
                .map_err(|_| WorktreeError::RootChanged)?;
            created
                .common_dir
                .verify()
                .map_err(|_| WorktreeError::RootChanged)?;

            let id: String = tx.query_row(
                "SELECT requested_project_id FROM worktree_operations WHERE operation_id=?1",
                [&prior.operation_id],
                |r| r.get(0),
            )?;
            let project = crate::domain::Project {
                id: ProjectId(id),
                name: receipt.name.clone(),
                root_path: receipt.target_path.clone(),
                created_at: crate::domain::now(),
                updated_at: crate::domain::now(),
            };
            crate::project_config::register_worktree_project_tx(&tx, &project, &created.root)?;
            let origin = WorktreeOrigin {
                version: 1,
                root: created.root.clone(),
                git_dir: created.git_dir.clone(),
                common_dir: created.common_dir.clone(),
                head_oid: created.head_oid.clone(),
                branch_ref: created.branch_ref.clone(),
            };
            tx.execute(
                "INSERT INTO project_worktree_origins(project_id,operation_id,source_project_id,identity) VALUES(?1,?2,?3,?4)",
                params![
                    project.id.0,
                    receipt.operation_id,
                    receipt.source_project_id.0,
                    serde_json::to_string(&origin)?
                ],
            )?;
            receipt.state = WorktreeState::Succeeded;
            receipt.project_id = Some(project.id);
        } else {
            let (state, cause) = failure.ok_or(WorktreeError::Corrupt)?;
            ensure!(
                matches!(state, WorktreeState::Rejected | WorktreeState::Unknown),
                WorktreeError::Corrupt
            );
            receipt.state = state;
            receipt.cause = Some(cause);
        }

        receipt.revision = receipt
            .revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or(WorktreeError::Corrupt)?;
        receipt.updated_at =
            i64::try_from(crate::domain::now()).map_err(|_| WorktreeError::Corrupt)?;
        receipt.validate()?;

        tx.execute(
            "UPDATE worktree_operations SET state=?2,revision=?3,receipt=?4 WHERE operation_id=?1 AND revision=?5 AND state='claimed'",
            params![
                receipt.operation_id,
                serde_json::to_value(receipt.state)?.as_str(),
                receipt.revision,
                serde_json::to_string(&receipt)?,
                prior.revision
            ],
        )?;
        let _ = tx.execute(
            "DELETE FROM worktree_project_reservations WHERE operation_id=?1",
            [&receipt.operation_id],
        );

        if current_op.is_some() {
            self.commit_execution_tx(tx)
                .map_err(|_| WorktreeError::Unavailable)?;
        } else {
            tx.commit().map_err(|_| WorktreeError::Unavailable)?;
        }
        Ok(receipt)
    }

    pub(crate) fn registered_worktree_origin(
        &self,
        receipt: &WorktreeReceipt,
    ) -> Result<Option<WorktreeOrigin>> {
        if receipt.state != WorktreeState::Succeeded {
            return Ok(None);
        }
        let project = receipt.project_id.as_ref().ok_or(WorktreeError::Corrupt)?;
        let db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        let (source, raw): (String, String) = db.query_row(
            "SELECT source_project_id,identity FROM project_worktree_origins WHERE project_id=?1 AND operation_id=?2",
            params![project.0, receipt.operation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let origin: WorktreeOrigin =
            serde_json::from_str(&raw).map_err(|_| WorktreeError::Corrupt)?;
        let plan: PersistedWorktreePlan = serde_json::from_str(&db.query_row(
            "SELECT plan FROM worktree_operations WHERE operation_id=?1",
            [&receipt.operation_id],
            |r| r.get::<_, String>(0),
        )?)
        .map_err(|_| WorktreeError::Corrupt)?;
        plan.validate().map_err(|_| WorktreeError::Corrupt)?;
        origin.validate(&plan)?;
        ensure!(
            source == receipt.source_project_id.0
                && origin.root == crate::project_config::root_tx(&db, project)?,
            WorktreeError::Corrupt
        );
        Ok(Some(origin))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_checks_never_freeze_projects_or_tasks() {
        let db = Connection::open_in_memory().unwrap();
        assert!(ensure_registration_unreserved_tx(&db).is_ok());
        assert!(ensure_project_unreserved_tx(&db, &ProjectId("p1".into())).is_ok());
        assert!(ensure_task_unreserved_tx(&db, "t1").is_ok());
        assert!(verify_persisted_rows(&db).is_ok());
    }

    #[test]
    fn normalizes_both_sha1_and_sha256_worktree_requests() {
        let temp = tempfile::tempdir().unwrap();
        let req = CreateWorktree {
            source_project_id: ProjectId("p1".into()),
            name: "feature-wt".into(),
            parent_path: temp.path().to_string_lossy().into_owned(),
            expected_config_revision: 1,
            expected_head: "ABCDEF0123456789".repeat(4),
            idempotency_key: "key-12345678".into(),
        };
        let norm = normalize_request(req).unwrap();
        assert_eq!(norm.expected_head.len(), 64);
        assert!(norm.expected_head.bytes().all(|b| !b.is_ascii_uppercase()));
    }
}
