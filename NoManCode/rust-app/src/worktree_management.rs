//! Explicit Git worktree creation receipts. Observation never grants execution.
use crate::domain::ProjectId;
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
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension};

pub(crate) fn ensure_project_unreserved_tx(db: &Connection, project: &ProjectId) -> Result<()> {
    let reserved: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM worktree_project_reservations WHERE project_id=?1)",
        [&project.0],
        |r| r.get(0),
    )?;
    if reserved {
        return Err(WorktreeError::Busy.into());
    }
    Ok(())
}
pub(crate) fn ensure_task_unreserved_tx(db: &Connection, task_id: &str) -> Result<()> {
    let project: Option<String> = db
        .query_row(
            "SELECT project_id FROM task_project_bindings WHERE task_id=?1",
            [task_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(project) = project {
        ensure_project_unreserved_tx(db, &ProjectId(project))?;
    }
    Ok(())
}
pub(crate) fn ensure_registration_unreserved_tx(db: &Connection) -> Result<()> {
    let reserved: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM worktree_operations WHERE state IN ('claimed','unknown'))",
        [],
        |r| r.get(0),
    )?;
    if reserved {
        return Err(WorktreeError::Busy.into());
    }
    Ok(())
}
// Task terminal status does not resolve an already claimed filesystem effect.
fn ensure_project_quiescent_tx(db: &Connection, project: &ProjectId) -> Result<()> {
    let busy: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks t JOIN task_project_bindings b ON b.task_id=t.id WHERE b.project_id=?1 AND json_extract(t.value,'$.status') IN ('queued','running'))
         OR EXISTS(SELECT 1 FROM approvals a JOIN task_project_bindings b ON b.task_id=a.task_id WHERE b.project_id=?1 AND a.execution_state IN ('claimed','unknown'))
         OR EXISTS(SELECT 1 FROM workspace_changes c JOIN task_project_bindings b ON b.task_id=c.task_id WHERE b.project_id=?1 AND (c.state IN ('prepared','unknown') OR c.restore_state='unknown'))
         OR EXISTS(SELECT 1 FROM workspace_restores r JOIN task_project_bindings b ON b.task_id=r.task_id WHERE b.project_id=?1 AND r.status IN ('claimed','partial','unknown'))
         OR EXISTS(SELECT 1 FROM workspace_restore_outcomes o JOIN workspace_restores r ON r.id=o.restore_id JOIN task_project_bindings b ON b.task_id=r.task_id WHERE b.project_id=?1 AND o.status IN ('claimed','unknown'))",
        [&project.0], |r| r.get(0),
    )?;
    ensure!(!busy, WorktreeError::Busy);
    Ok(())
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
        ensure!(
            self.expected_head.len() == 40
                && self
                    .expected_head
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            WorktreeError::Corrupt
        );
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
                    "{}/nhc-{}",
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
pub(crate) fn verify_persisted_rows(db: &Connection) -> Result<()> {
    let mut statement=db.prepare("SELECT operation_id,idempotency_key,source_project_id,requested_project_id,request_json,request_digest,plan,common_key,target_path,branch_ref,state,revision,receipt,members FROM worktree_operations")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let receipt: WorktreeReceipt = serde_json::from_str(&row.get::<_, String>(12)?)?;
        receipt.validate()?;
        let request: CreateWorktree = serde_json::from_str(&row.get::<_, String>(4)?)?;
        ensure!(
            row.get::<_, String>(0)? == receipt.operation_id
                && row.get::<_, String>(2)? == receipt.source_project_id.0
                && row.get::<_, String>(5)? == receipt.request_digest
                && row.get::<_, String>(8)? == receipt.target_path
                && row.get::<_, u64>(11)? == receipt.revision,
            WorktreeError::Corrupt
        );
        ensure!(
            serde_json::to_value(receipt.state)?.as_str()
                == Some(row.get::<_, String>(10)?.as_str()),
            WorktreeError::Corrupt
        );
        ensure!(
            request.source_project_id == receipt.source_project_id
                && request.name == receipt.name
                && request.parent_path == receipt.parent_path
                && request.expected_config_revision == receipt.expected_config_revision
                && request.expected_head == receipt.expected_head
                && request.idempotency_key == row.get::<_, String>(1)?,
            WorktreeError::Corrupt
        );
        ensure!(
            row.get::<_, String>(9)? == format!("refs/heads/{}", receipt.branch),
            WorktreeError::Corrupt
        );
        let normalized = normalize_request(request.clone()).map_err(|_| WorktreeError::Corrupt)?;
        ensure!(
            normalized == request && request_digest(&request)? == receipt.request_digest,
            WorktreeError::Corrupt
        );
        let project_id = row.get::<_, String>(3)?;
        ensure!(
            uuid::Uuid::parse_str(&project_id).is_ok_and(|id| id.to_string() == project_id)
                && project_id != receipt.source_project_id.0,
            WorktreeError::Corrupt
        );
        let plan: PersistedWorktreePlan =
            serde_json::from_str(&row.get::<_, String>(6)?).map_err(|_| WorktreeError::Corrupt)?;
        plan.validate().map_err(|_| WorktreeError::Corrupt)?;
        ensure!(
            plan.operation_id == receipt.operation_id
                && plan.expected_head == receipt.expected_head
                && plan.target_path() == std::path::Path::new(&receipt.target_path)
                && windows_path_key(
                    plan.target_parent
                        .canonical_path()
                        .to_string_lossy()
                        .as_ref()
                ) == windows_path_key(&receipt.parent_path)
                && plan
                    .repository
                    .common_key()
                    .map_err(|_| WorktreeError::Corrupt)?
                    == row.get::<_, String>(7)?,
            WorktreeError::Corrupt
        );
        ensure!(
            plan.source_root == crate::project_config::root_tx(db, &receipt.source_project_id)?,
            WorktreeError::Corrupt
        );
        let members: Vec<ReservedProject> =
            serde_json::from_str(&row.get::<_, String>(13)?).map_err(|_| WorktreeError::Corrupt)?;
        let mut ids = std::collections::BTreeSet::new();
        for member in members {
            member
                .root
                .validate_snapshot()
                .map_err(|_| WorktreeError::Corrupt)?;
            ensure!(
                ids.insert(member.project_id.0.clone())
                    && member.root == crate::project_config::root_tx(db, &member.project_id)?,
                WorktreeError::Corrupt
            );
        }
        ensure!(
            ids.contains(&receipt.source_project_id.0),
            WorktreeError::Corrupt
        );
        let reserved: std::collections::BTreeSet<String> = db
            .prepare("SELECT project_id FROM worktree_project_reservations WHERE operation_id=?1")?
            .query_map([&receipt.operation_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if matches!(
            receipt.state,
            WorktreeState::Claimed | WorktreeState::Unknown
        ) {
            ensure!(reserved == ids, WorktreeError::Corrupt);
        } else {
            ensure!(reserved.is_empty(), WorktreeError::Corrupt);
        }
        let origin: Option<String> = db
            .query_row(
                "SELECT project_id FROM project_worktree_origins WHERE operation_id=?1",
                [&receipt.operation_id],
                |r| r.get(0),
            )
            .optional()?;
        ensure!(
            origin.as_deref() == receipt.project_id.as_ref().map(|p| p.0.as_str()),
            WorktreeError::Corrupt
        );
        if let Some(project) = &receipt.project_id {
            ensure!(
                project.0 == row.get::<_, String>(3)?,
                WorktreeError::Corrupt
            );
            let (source,raw):(String,String)=db.query_row("SELECT source_project_id,identity FROM project_worktree_origins WHERE operation_id=?1",[&receipt.operation_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let origin: WorktreeOrigin =
                serde_json::from_str(&raw).map_err(|_| WorktreeError::Corrupt)?;
            origin.validate(&plan)?;
            ensure!(
                source == receipt.source_project_id.0
                    && origin.root == crate::project_config::root_tx(db, project)?,
                WorktreeError::Corrupt
            );
            let root_path: String = db.query_row(
                "SELECT root_path FROM projects WHERE id=?1",
                [&project.0],
                |r| r.get(0),
            )?;
            ensure!(root_path == receipt.target_path, WorktreeError::Corrupt);
        }
    }
    let excess:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM project_worktree_origins o JOIN worktree_operations w ON w.operation_id=o.operation_id WHERE w.state!='succeeded')",[],|r|r.get(0))?;
    ensure!(!excess, WorktreeError::Corrupt);
    Ok(())
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
        request.expected_head.len() == 40
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
    let raw:Option<String>=db.query_row("SELECT receipt FROM worktree_operations WHERE operation_id=?1 AND source_project_id=?2",rusqlite::params![operation,source.0],|r|r.get(0)).optional()?;
    let receipt: WorktreeReceipt = serde_json::from_str(&raw.ok_or(WorktreeError::NotFound)?)
        .map_err(|_| WorktreeError::Corrupt)?;
    receipt.validate()?;
    Ok(receipt)
}
pub(crate) fn replay_tx(
    db: &Connection,
    request: &CreateWorktree,
) -> Result<Option<WorktreeReceipt>> {
    let prior:Option<(String,String,String)>=db.query_row("SELECT operation_id,source_project_id,request_digest FROM worktree_operations WHERE idempotency_key=?1",[&request.idempotency_key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
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
        let operation:Option<String>=db.query_row("SELECT operation_id FROM worktree_operations WHERE source_project_id=?1 AND idempotency_key=?2",rusqlite::params![source.0,key],|r|r.get(0)).optional()?;
        read_receipt(&db, source, &operation.ok_or(WorktreeError::NotFound)?)
    }
}
pub(crate) fn recover_claimed_tx(db: &Connection) -> Result<usize> {
    let rows: Vec<String> = db
        .prepare("SELECT receipt FROM worktree_operations WHERE state='claimed'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for raw in &rows {
        let mut receipt: WorktreeReceipt =
            serde_json::from_str(raw).map_err(|_| WorktreeError::Corrupt)?;
        receipt.validate()?;
        receipt.state = WorktreeState::Unknown;
        receipt.cause = Some(WorktreeCause::Interrupted);
        receipt.revision = receipt
            .revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or(WorktreeError::Corrupt)?;
        receipt.updated_at =
            i64::try_from(crate::domain::now()).map_err(|_| WorktreeError::Corrupt)?;
        db.execute("UPDATE worktree_operations SET state='unknown',revision=?2,receipt=?3 WHERE operation_id=?1 AND state='claimed'",rusqlite::params![receipt.operation_id,receipt.revision,serde_json::to_string(&receipt)?])?;
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

use crate::{
    git_worktree::{self, CreatedWorktree, PersistedWorktreePlan},
    workspace_identity::RootIdentity,
};
use rusqlite::params;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectInventory(pub Vec<(String, String, Option<String>)>);
pub(crate) fn inventory_tx(db: &Connection) -> Result<ProjectInventory> {
    Ok(ProjectInventory(db.prepare("SELECT p.id,p.root_path,b.identity FROM projects p LEFT JOIN project_root_bindings b ON b.project_id=p.id ORDER BY p.id")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?))
}
impl ProjectInventory {
    // Diagnosis retains unavailable roots for executable trust exclusion without
    // requiring online availability before the observation can be classified.
    pub(crate) fn snapshots(&self) -> Result<Vec<(ProjectId, RootIdentity)>> {
        self.0
            .iter()
            .map(|(id, path, raw)| {
                let root: RootIdentity =
                    serde_json::from_str(raw.as_ref().ok_or(WorktreeError::Unavailable)?)
                        .map_err(|_| WorktreeError::Corrupt)?;
                root.validate_snapshot()
                    .map_err(|_| WorktreeError::Corrupt)?;
                ensure!(
                    windows_path_key(path)
                        == windows_path_key(&root.canonical_path().to_string_lossy()),
                    WorktreeError::Corrupt
                );
                Ok((ProjectId(id.clone()), root))
            })
            .collect()
    }
    pub(crate) fn roots(&self) -> Result<Vec<(ProjectId, RootIdentity)>> {
        self.0
            .iter()
            .map(|(id, path, raw)| {
                let root: RootIdentity =
                    serde_json::from_str(raw.as_ref().ok_or(WorktreeError::Unavailable)?)
                        .map_err(|_| WorktreeError::Corrupt)?;
                let verified = root.verify().map_err(|_| WorktreeError::RootChanged)?;
                ensure!(
                    std::fs::canonicalize(path).map_err(|_| WorktreeError::Unavailable)?
                        == verified.path(),
                    WorktreeError::RootChanged
                );
                Ok((ProjectId(id.clone()), root))
            })
            .collect()
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
        let root = self.root.canonical_path();
        let admin = self.git_dir.canonical_path();
        let common = self.common_dir.canonical_path();
        ensure!(
            self.version == 1
                && root == plan.target_path()
                && self.common_dir == plan.repository.common_dir
                && self.head_oid == plan.expected_head
                && self.branch_ref == plan.branch_ref
                && admin.parent() == Some(common.join("worktrees").as_path())
                && self.git_dir != plan.repository.git_dir
                && !admin.starts_with(root)
                && !root.starts_with(admin)
                && !common.starts_with(root)
                && !root.starts_with(common),
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
impl crate::store::Store {
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
        inventory: &ProjectInventory,
        members: &[ProjectId],
        plan: &PersistedWorktreePlan,
    ) -> Result<(WorktreeReceipt, bool)> {
        let mut db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        self.execution_fence(&tx)
            .map_err(|_| WorktreeError::Unavailable)?;
        if let Some(prior) = replay_tx(&tx, request)? {
            return Ok((prior, true));
        }
        ensure!(inventory_tx(&tx)? == *inventory, WorktreeError::Conflict);
        let config = crate::project_config::config_tx(&tx, &request.source_project_id)?;
        ensure!(
            config.revision == request.expected_config_revision,
            WorktreeError::RevisionConflict
        );
        plan.validate().map_err(|_| WorktreeError::Corrupt)?;
        ensure!(
            plan.expected_head == request.expected_head
                && members.contains(&request.source_project_id),
            WorktreeError::Corrupt
        );
        ensure!(
            crate::project_config::root_tx(&tx, &request.source_project_id)? == plan.source_root,
            WorktreeError::RootChanged
        );
        let roots = inventory.roots()?;
        plan.source_root
            .verify()
            .map_err(|_| WorktreeError::RootChanged)?;
        plan.target_parent
            .verify()
            .map_err(|_| WorktreeError::RootChanged)?;
        plan.repository
            .git_dir
            .verify()
            .map_err(|_| WorktreeError::RootChanged)?;
        plan.repository
            .common_dir
            .verify()
            .map_err(|_| WorktreeError::RootChanged)?;
        ensure!(
            !plan
                .target_path()
                .try_exists()
                .map_err(|_| WorktreeError::Unavailable)?,
            WorktreeError::Conflict
        );
        for member in members {
            ensure!(
                roots.iter().any(|(id, _)| id == member),
                WorktreeError::Corrupt
            );
            ensure_project_unreserved_tx(&tx, member)?;
            ensure_project_quiescent_tx(&tx, member)?;
        }
        let common = plan
            .repository
            .common_key()
            .map_err(|_| WorktreeError::Corrupt)?;
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM worktree_operations WHERE common_key=?1 AND state IN ('claimed','unknown'))",[&common],|r|r.get(0))?;
        ensure!(!busy, WorktreeError::Busy);
        let target = plan.target_path().to_string_lossy().into_owned();
        let conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM worktree_operations WHERE target_path=?1 OR (common_key=?2 AND branch_ref=?3))",params![target,common,plan.branch_ref],|r|r.get(0))?;
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
        let member_snapshots: Vec<ReservedProject> = members
            .iter()
            .map(|id| {
                Ok(ReservedProject {
                    project_id: id.clone(),
                    root: crate::project_config::root_tx(&tx, id)?,
                })
            })
            .collect::<Result<_>>()?;
        tx.execute("INSERT INTO worktree_operations(operation_id,idempotency_key,source_project_id,requested_project_id,request_json,request_digest,plan,common_key,target_path,branch_ref,state,revision,receipt,members) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'claimed',1,?11,?12)",params![receipt.operation_id,request.idempotency_key,request.source_project_id.0,uuid::Uuid::new_v4().to_string(),serde_json::to_string(request)?,receipt.request_digest,serde_json::to_string(plan)?,common,receipt.target_path,plan.branch_ref,serde_json::to_string(&receipt)?,serde_json::to_string(&member_snapshots)?])?;
        for member in members {
            tx.execute(
                "INSERT INTO worktree_project_reservations(project_id,operation_id) VALUES(?1,?2)",
                params![member.0, receipt.operation_id],
            )?;
        }
        let operation = crate::execution_authority::current().ok_or(WorktreeError::Unavailable)?;
        operation
            .bind_worktree_operation(&receipt.source_project_id.0, &receipt.operation_id)
            .map_err(|_| WorktreeError::Unavailable)?;
        self.commit_execution_tx(tx)
            .map_err(|_| WorktreeError::Unavailable)?;
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
        self.execution_settlement_fence(&tx)
            .map_err(|_| WorktreeError::Unavailable)?;
        let mut receipt = read_receipt(&tx, &prior.source_project_id, &prior.operation_id)?;
        if receipt.state != WorktreeState::Claimed || receipt.revision != prior.revision {
            return Ok(receipt);
        }
        let operation = crate::execution_authority::current().ok_or(WorktreeError::Unavailable)?;
        operation
            .verify_worktree_operation(&receipt.source_project_id.0, &receipt.operation_id)
            .map_err(|_| WorktreeError::Unavailable)?;
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
            tx.execute("INSERT INTO project_worktree_origins(project_id,operation_id,source_project_id,identity) VALUES(?1,?2,?3,?4)",params![project.id.0,receipt.operation_id,receipt.source_project_id.0,serde_json::to_string(&origin)?])?;
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
        tx.execute("UPDATE worktree_operations SET state=?2,revision=?3,receipt=?4 WHERE operation_id=?1 AND revision=?5 AND state='claimed'",params![receipt.operation_id,serde_json::to_value(receipt.state)?.as_str(),receipt.revision,serde_json::to_string(&receipt)?,prior.revision])?;
        if matches!(
            receipt.state,
            WorktreeState::Succeeded | WorktreeState::Rejected
        ) {
            tx.execute(
                "DELETE FROM worktree_project_reservations WHERE operation_id=?1",
                [&receipt.operation_id],
            )?;
        }
        self.commit_execution_tx(tx)
            .map_err(|_| WorktreeError::Unavailable)?;
        Ok(receipt)
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
    pub(crate) fn registered_worktree_origin(
        &self,
        receipt: &WorktreeReceipt,
    ) -> Result<Option<WorktreeOrigin>> {
        if receipt.state != WorktreeState::Succeeded {
            return Ok(None);
        }
        let project = receipt.project_id.as_ref().ok_or(WorktreeError::Corrupt)?;
        let db = self.db.lock().map_err(|_| WorktreeError::Corrupt)?;
        let (source,raw):(String,String)=db.query_row("SELECT source_project_id,identity FROM project_worktree_origins WHERE project_id=?1 AND operation_id=?2",params![project.0,receipt.operation_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
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
    use crate::{domain::*, engine::Engine, project_config::*, store::Store};
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    struct N13Child {
        #[cfg(windows)]
        contained: crate::process_test_fixture::ContainedChild,
        #[cfg(not(windows))]
        child: std::process::Child,
    }
    impl N13Child {
        fn spawn(command: &mut std::process::Command, root: &Path, _phase: &str) -> Self {
            #[cfg(windows)]
            let result = Self {
                contained: crate::process_test_fixture::ContainedChild::spawn(
                    command, root, _phase,
                )
                .unwrap(),
            };
            #[cfg(not(windows))]
            let result = Self {
                child: command.spawn().unwrap(),
            };
            crate::worktree_diagnostics::atomic_write(&root.join("start"), b"start").unwrap();
            result
        }
        fn child(&mut self) -> &mut std::process::Child {
            #[cfg(windows)]
            {
                &mut self.contained.child
            }
            #[cfg(not(windows))]
            {
                &mut self.child
            }
        }
        fn kill_wait(&mut self) -> std::process::ExitStatus {
            #[cfg(windows)]
            {
                self.contained.terminate_and_wait().unwrap()
            }
            #[cfg(not(windows))]
            {
                self.child.kill().unwrap();
                self.child.wait().unwrap()
            }
        }
    }
    impl Drop for N13Child {
        fn drop(&mut self) {
            #[cfg(not(windows))]
            {
                let _ = self.child.kill();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while std::time::Instant::now() < deadline {
                    if self.child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }

    fn raw(root: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        source: PathBuf,
        parent: PathBuf,
        db: PathBuf,
        store: Arc<Store>,
        project: Project,
        head: String,
    }
    impl Fixture {
        #[cfg(windows)]
        fn new_supervised(diagnostics: &Option<Arc<crate::worktree_diagnostics::Case>>) -> Self {
            struct PreserveSetup(Option<tempfile::TempDir>);
            impl Drop for PreserveSetup {
                fn drop(&mut self) {
                    if std::thread::panicking()
                        && let Some(temp) = self.0.take()
                    {
                        eprintln!("N13 retained setup fixture={}", temp.keep().display());
                    }
                }
            }
            let mut temp = PreserveSetup(Some(tempfile::tempdir().unwrap()));
            println!(
                "N13 setup fixture={}",
                temp.0.as_ref().unwrap().path().display()
            );
            let source = temp.0.as_ref().unwrap().path().join("source");
            let parent = temp.0.as_ref().unwrap().path().join("targets");
            let host = temp.0.as_ref().unwrap().path().join("host");
            for path in [&source, &parent, &host] {
                std::fs::create_dir(path).unwrap();
            }
            let step = |number, args: &[&str]| {
                crate::worktree_diagnostics::case_record(diagnostics, "fixture_git_begin", number);
                let result = raw(&source, args);
                crate::worktree_diagnostics::case_record(diagnostics, "fixture_git_end", number);
                result
            };
            step(1, &["init", "--quiet"]);
            step(2, &["config", "user.name", "Fixture"]);
            step(3, &["config", "user.email", "fixture@example.invalid"]);
            step(4, &["config", "core.autocrlf", "false"]);
            std::fs::write(source.join("a.txt"), "baseline\n").unwrap();
            step(5, &["add", "--", "a.txt"]);
            step(6, &["commit", "--quiet", "-m", "baseline"]);
            let head = step(7, &["rev-parse", "HEAD"]).trim().to_owned();
            let db = host.join("host.db");
            let store = Arc::new(Store::open(&db).unwrap());
            store
                .save_settings(&Settings {
                    workspace: source.to_string_lossy().into(),
                    max_concurrency: 2,
                    routes: vec![],
                    newapi: None,
                })
                .unwrap();
            let project = store
                .register_project(RegisterProject {
                    name: "Source".into(),
                    root_path: source.to_string_lossy().into(),
                    idempotency_key: id(),
                })
                .unwrap();
            crate::worktree_diagnostics::case_event(diagnostics, "fixture_ready");
            Self {
                _temp: temp.0.take().unwrap(),
                source,
                parent,
                db,
                store,
                project,
                head,
            }
        }
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let parent = temp.path().join("targets");
            let host = temp.path().join("host");
            for path in [&source, &parent, &host] {
                std::fs::create_dir(path).unwrap();
            }
            raw(&source, &["init", "--quiet"]);
            raw(&source, &["config", "user.name", "Fixture"]);
            raw(
                &source,
                &["config", "user.email", "fixture@example.invalid"],
            );
            raw(&source, &["config", "core.autocrlf", "false"]);
            std::fs::write(source.join("a.txt"), "baseline\n").unwrap();
            raw(&source, &["add", "--", "a.txt"]);
            raw(&source, &["commit", "--quiet", "-m", "baseline"]);
            let head = raw(&source, &["rev-parse", "HEAD"]).trim().to_owned();
            let db = host.join("host.db");
            let store = Arc::new(Store::open(&db).unwrap());
            store
                .save_settings(&Settings {
                    workspace: source.to_string_lossy().into(),
                    max_concurrency: 2,
                    routes: vec![],
                    newapi: None,
                })
                .unwrap();
            let project = store
                .register_project(RegisterProject {
                    name: "Source".into(),
                    root_path: source.to_string_lossy().into(),
                    idempotency_key: id(),
                })
                .unwrap();
            Self {
                _temp: temp,
                source,
                parent,
                db,
                store,
                project,
                head,
            }
        }
        fn request(&self) -> CreateWorktree {
            CreateWorktree {
                source_project_id: self.project.id.clone(),
                name: "Created".into(),
                parent_path: self.parent.to_string_lossy().into(),
                expected_config_revision: self
                    .store
                    .project_config(&self.project.id)
                    .unwrap()
                    .revision,
                expected_head: self.head.clone(),
                idempotency_key: id(),
            }
        }
        async fn plan(&self, request: &CreateWorktree) -> PersistedWorktreePlan {
            let root = RootIdentity::capture(&self.source).unwrap();
            let context = crate::git_worktree::GitContext::new(vec![root.clone()]).unwrap();
            // Preparation launches real Git processes under this fixture's
            // existing controller, not a separate test data domain.
            self.store
                .execution_operation(tokio_util::sync::CancellationToken::new())
                .unwrap()
                .run(async {
                    crate::git_worktree::prepare(
                        &context,
                        &root,
                        &RootIdentity::capture(Path::new(&request.parent_path)).unwrap(),
                        &id(),
                        &request.expected_head,
                    )
                    .await
                    .unwrap()
                    .into_persisted_plan()
                })
                .await
        }
        async fn claim(
            &self,
        ) -> (
            CreateWorktree,
            WorktreeReceipt,
            PersistedWorktreePlan,
            crate::execution_authority::ExecutionOperation,
        ) {
            let request = self.request();
            let plan = self.plan(&request).await;
            let operation = self
                .store
                .execution_operation(tokio_util::sync::CancellationToken::new())
                .unwrap();
            let receipt = operation
                .test_sync(|| {
                    self.store.claim_worktree(
                        &request,
                        &self.store.worktree_inventory().unwrap(),
                        std::slice::from_ref(&self.project.id),
                        &plan,
                    )
                })
                .unwrap()
                .0;
            (request, receipt, plan, operation)
        }
        fn count(&self, table: &str) -> i64 {
            self.store
                .db
                .lock()
                .unwrap()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        }
    }
    #[tokio::test]
    async fn n13_success_defaults_replay_and_diagnosis_are_read_only() {
        let f = Fixture::new();
        let engine = Engine::new(f.store.clone(), 2).unwrap();
        let mut request = f.request();
        request.expected_head.make_ascii_uppercase();
        // An ordinary ancestor parent is allowed; only the actual generated leaf must be disjoint.
        request.parent_path = f._temp.path().to_string_lossy().replace('\\', "/");
        let old_selection = serde_json::to_value(f.store.current_project().unwrap()).unwrap();
        let index = std::fs::read(f.source.join(".git/index")).unwrap();
        let receipt = engine
            .create_project_worktree(request.clone())
            .await
            .unwrap();
        assert_eq!(receipt.state, WorktreeState::Succeeded, "{receipt:?}");
        assert_eq!(receipt.expected_head, f.head);
        assert_eq!(receipt.parent_path, request.parent_path);
        let project = receipt.project_id.as_ref().unwrap();
        assert_eq!(
            f.store.project_config(project).unwrap().config,
            ProjectConfig::default()
        );
        assert_eq!(
            serde_json::to_value(f.store.current_project().unwrap()).unwrap(),
            old_selection
        );
        for table in ["network_authorizations", "tasks", "runs"] {
            assert_eq!(f.count(table), 0, "{table}");
        }
        assert_eq!(f.count("project_worktree_origins"), 1);
        assert_eq!(f.count("worktree_project_reservations"), 0);
        assert_eq!(raw(&f.source, &["rev-parse", "HEAD"]).trim(), f.head);
        assert_eq!(std::fs::read(f.source.join(".git/index")).unwrap(), index);
        let branches = raw(&f.source, &["show-ref"]);
        let inspected = engine
            .inspect_project_worktree(&f.project.id, &receipt.operation_id)
            .await
            .unwrap();
        assert_eq!(
            inspected.observation.state,
            WorktreeObservationState::PresentMatching
        );
        assert_eq!(inspected.receipt, receipt);
        assert_eq!(raw(&f.source, &["show-ref"]), branches);
        // Pure replay occurs before a now-invalid current Git tree or permission revision.
        std::fs::write(f.source.join("untracked.txt"), "dirty").unwrap();
        let config = f.store.project_config(&f.project.id).unwrap();
        f.store
            .configure_project(ConfigureProject {
                project_id: f.project.id.clone(),
                expected_revision: config.revision,
                config: config.config,
            })
            .unwrap();
        assert_eq!(
            engine
                .create_project_worktree(request.clone())
                .await
                .unwrap(),
            receipt
        );
        let mut different = request;
        different.name.push('x');
        assert_eq!(
            engine
                .create_project_worktree(different)
                .await
                .unwrap_err()
                .downcast_ref::<WorktreeError>(),
            Some(&WorktreeError::IdempotencyConflict)
        );
        assert_eq!(raw(&f.source, &["show-ref"]), branches);
        assert_eq!(
            Store::open(&f.db)
                .unwrap()
                .worktree_receipt(&f.project.id, &receipt.operation_id)
                .unwrap(),
            receipt
        );
    }
    #[tokio::test]
    async fn n13_all_terminal_replays_and_recovery_keep_unknown_reserved() {
        for state in [
            WorktreeState::Rejected,
            WorktreeState::Unknown,
            WorktreeState::Claimed,
        ] {
            let f = Fixture::new();
            let engine = Engine::new(f.store.clone(), 2).unwrap();
            let (request, receipt, plan, operation) = f.claim().await;
            let expected = if state == WorktreeState::Claimed {
                receipt.clone()
            } else {
                operation
                    .test_sync(|| {
                        f.store.finish_worktree(
                            &receipt,
                            None,
                            Some((
                                state,
                                if state == WorktreeState::Unknown {
                                    WorktreeCause::ProcessOutcomeUnknown
                                } else {
                                    WorktreeCause::Conflict
                                },
                            )),
                        )
                    })
                    .unwrap()
            };
            drop(operation);
            std::fs::write(f.source.join("dirty"), "dirty").unwrap();
            assert_eq!(
                engine
                    .create_project_worktree(request.clone())
                    .await
                    .unwrap(),
                expected
            );
            assert!(!plan.target_path().exists());
            assert_eq!(f.count("project_worktree_origins"), 0);
            assert_eq!(
                f.count("worktree_project_reservations"),
                if state == WorktreeState::Rejected {
                    0
                } else {
                    1
                }
            );
            let reopened = Store::open(&f.db).unwrap();
            assert!(
                reopened.recover().is_err(),
                "live controller admitted recovery"
            );
            assert_eq!(
                reopened
                    .worktree_receipt(&f.project.id, &receipt.operation_id)
                    .unwrap(),
                expected
            );
            drop(engine);
            reopened.recover().unwrap();
            let recovered = reopened
                .worktree_receipt(&f.project.id, &receipt.operation_id)
                .unwrap();
            if state == WorktreeState::Claimed {
                assert_eq!(recovered.state, WorktreeState::Unknown);
                assert_eq!(recovered.cause, Some(WorktreeCause::Interrupted));
            } else {
                assert_eq!(recovered, expected);
            }
            reopened.recover().unwrap();
            assert_eq!(
                reopened
                    .worktree_receipt(&f.project.id, &receipt.operation_id)
                    .unwrap(),
                recovered
            );
            assert!(!plan.target_path().exists());
            assert_eq!(raw(&f.source, &["branch", "--list", &receipt.branch]), "");
        }
    }
    #[tokio::test]
    async fn n13_finish_rejects_another_operation_and_accepts_original() {
        let f = Fixture::new();
        let _engine = Engine::new(f.store.clone(), 2).unwrap();
        let (request, receipt, plan, original) = f.claim().await;
        let other = f
            .store
            .execution_operation(tokio_util::sync::CancellationToken::new())
            .unwrap();
        let inventory = f.store.worktree_inventory().unwrap();
        let (replayed, was_replay) = other
            .test_sync(|| {
                f.store.claim_worktree(
                    &request,
                    &inventory,
                    std::slice::from_ref(&f.project.id),
                    &plan,
                )
            })
            .unwrap();
        assert!(was_replay);
        assert_eq!(replayed, receipt);
        let before = f
            .store
            .worktree_receipt(&f.project.id, &receipt.operation_id)
            .unwrap();
        let wrong = other.test_sync(|| {
            f.store.finish_worktree(
                &receipt,
                None,
                Some((WorktreeState::Rejected, WorktreeCause::Conflict)),
            )
        });
        assert!(wrong.is_err());
        assert_eq!(
            f.store
                .worktree_receipt(&f.project.id, &receipt.operation_id)
                .unwrap(),
            before
        );
        assert_eq!(f.count("worktree_project_reservations"), 1);
        let finished = original
            .test_sync(|| {
                f.store.finish_worktree(
                    &receipt,
                    None,
                    Some((WorktreeState::Rejected, WorktreeCause::Conflict)),
                )
            })
            .unwrap();
        assert_eq!(finished.state, WorktreeState::Rejected);
        assert_eq!(f.count("worktree_project_reservations"), 0);
    }

    #[tokio::test]
    async fn n13_same_database_two_store_claim_race_has_one_winner() {
        let f = Fixture::new();
        let _engine = Engine::new(f.store.clone(), 2).unwrap();
        let first = f.request();
        let second = f.request();
        let a = f.plan(&first).await;
        let b = f.plan(&second).await;
        let inventory = f.store.worktree_inventory().unwrap();
        let other = Arc::new(Store::open(&f.db).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = [(f.store.clone(), first, a), (other, second, b)]
            .into_iter()
            .map(|(store, request, plan)| {
                let inventory = inventory.clone();
                let member = f.project.id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    crate::execution_authority::test_with_active_store_sync(&store, || {
                        store.claim_worktree(&request, &inventory, &[member], &plan)
                    })
                    .unwrap()
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .into_iter()
                .find_map(Result::err)
                .unwrap()
                .downcast_ref::<WorktreeError>(),
            Some(&WorktreeError::Busy)
        );
        assert_eq!(f.count("worktree_operations"), 1);
        assert_eq!(f.count("worktree_project_reservations"), 1);
        assert_eq!(std::fs::read_dir(&f.parent).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn n13_cancelled_task_unresolved_restore_or_write_blocks_claim() {
        let f = Fixture::new();
        let _engine = Engine::new(f.store.clone(), 2).unwrap();
        let request = f.request();
        let plan = f.plan(&request).await;
        let inventory = f.store.worktree_inventory().unwrap();
        // Persist the cancelled Task independently from its filesystem outcome.
        let db = f.store.db.lock().unwrap();
        db.execute(
            "INSERT INTO runs(id,title,created_at) VALUES('run','Fixture',1)",
            [],
        )
        .unwrap();
        db.execute(
            r#"INSERT INTO tasks(id,run_id,value) VALUES('task','run','{"status":"cancelled"}')"#,
            [],
        )
        .unwrap();
        db.execute("INSERT INTO task_project_bindings(task_id,project_id,value,provenance) VALUES('task',?1,'{}','native_v1')",[&f.project.id.0]).unwrap();
        db.execute_batch("INSERT INTO workspace_changes(id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,state,restore_state,created_at) VALUES('change','task','call','digest','digest','[]','a.txt','a.txt','created','finished','pending',1);
          INSERT INTO workspace_restores(id,task_id,status,created_at) VALUES('restore','task','partial',1);
          INSERT INTO workspace_restore_outcomes(restore_id,change_id,path,status,updated_at) VALUES('restore','change','a.txt','unknown',1);").unwrap();
        drop(db);
        for (parent, outcome, state, restored) in [
            ("partial", "unknown", "finished", "pending"),
            ("complete", "unknown", "finished", "pending"),
            ("complete", "claimed", "finished", "pending"),
            ("complete", "complete", "prepared", "pending"),
            ("complete", "complete", "unknown", "pending"),
            ("complete", "complete", "finished", "unknown"),
        ] {
            {
                let db = f.store.db.lock().unwrap();
                db.execute("UPDATE workspace_restores SET status=?1", [parent])
                    .unwrap();
                db.execute("UPDATE workspace_restore_outcomes SET status=?1", [outcome])
                    .unwrap();
                db.execute(
                    "UPDATE workspace_changes SET state=?1,restore_state=?2",
                    params![state, restored],
                )
                .unwrap();
            }
            let err = crate::execution_authority::test_with_active_store_sync(&f.store, || {
                f.store.claim_worktree(
                    &request,
                    &inventory,
                    std::slice::from_ref(&f.project.id),
                    &plan,
                )
            })
            .unwrap()
            .unwrap_err();
            assert_eq!(
                err.downcast_ref::<WorktreeError>(),
                Some(&WorktreeError::Busy),
                "{parent}/{outcome}/{state}/{restored}"
            );
            assert_eq!(f.count("worktree_operations"), 0);
            assert!(!plan.target_path().exists());
        }
        {
            let db = f.store.db.lock().unwrap();
            db.execute(
                "UPDATE workspace_changes SET state='finished',restore_state='restored'",
                [],
            )
            .unwrap();
        }
        assert!(
            crate::execution_authority::test_with_active_store_sync(&f.store, || {
                f.store.claim_worktree(
                    &request,
                    &inventory,
                    std::slice::from_ref(&f.project.id),
                    &plan,
                )
            })
            .unwrap()
            .is_ok()
        );
    }
    #[tokio::test]
    async fn n13_persisted_request_plan_members_and_receipt_corruption_rejected_without_repair() {
        for sql in [
            "UPDATE worktree_operations SET request_digest=replace(request_digest,substr(request_digest,1,1),'z')",
            "UPDATE worktree_operations SET plan=json_set(plan,'$.expected_head','0000000000000000000000000000000000000000')",
            "DELETE FROM worktree_project_reservations",
            "UPDATE worktree_operations SET members='[]'",
            "UPDATE worktree_operations SET receipt=json_set(receipt,'$.updated_at',-1)",
            "UPDATE worktree_operations SET receipt=json_set(receipt,'$.cause','interrupted')",
        ] {
            let f = Fixture::new();
            let _engine = Engine::new(f.store.clone(), 2).unwrap();
            let (_, _, _, _) = f.claim().await;
            f.store.db.lock().unwrap().execute_batch(sql).unwrap();
            let before = std::fs::read(&f.db).unwrap();
            assert!(Store::open(&f.db).is_err(), "{sql}");
            assert!(Store::open(&f.db).is_err(), "{sql}");
            assert_eq!(std::fs::read(&f.db).unwrap(), before, "{sql}");
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn n13_dropped_caller_owned_worker_still_registers_once() {
        #[cfg(windows)]
        let control_root = if let Ok(root) = std::env::var("NHC_N13_CALLER_CHILD") {
            let root = PathBuf::from(root);
            crate::worktree_diagnostics::wait_start(&root.join("start")).unwrap();
            Some(root)
        } else {
            let root = tempfile::tempdir().unwrap().keep();
            let output = std::fs::File::create(root.join("child.log")).unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "worktree_management::tests::n13_dropped_caller_owned_worker_still_registers_once",
                    "--nocapture",
                ])
                .env("NHC_N13_CALLER_CHILD", &root)
                .stdout(output.try_clone().unwrap())
                .stderr(output);
            let mut child =
                crate::process_test_fixture::ContainedChild::spawn(&mut command, &root, "caller")
                    .unwrap();
            crate::worktree_diagnostics::atomic_write(&root.join("start"), b"start").unwrap();
            let mut supervisor = crate::worktree_diagnostics::Supervisor::with_fixture(
                root.join("worktree-diagnostics.jsonl"),
            );
            loop {
                if let Some(status) = child.child.try_wait().unwrap() {
                    assert!(status.success(), "N13 caller child failed: {status}");
                    supervisor.poll().unwrap();
                    assert_eq!(supervisor.stage(), 10);
                    let events =
                        std::fs::read_to_string(root.join("worktree-diagnostics.jsonl")).unwrap();
                    assert!(events.ends_with('\n'));
                    let last: serde_json::Value =
                        serde_json::from_str(events.lines().last().unwrap()).unwrap();
                    assert_eq!(last["phase"], "n13_assertions_complete");
                    let log = std::fs::read_to_string(root.join("child.log")).unwrap();
                    assert!(log.contains("test result: ok. 1 passed;"));
                    child.terminate_and_wait().unwrap();
                    return;
                }
                supervisor.poll().unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        struct RetainedFixture(Option<Fixture>);
        impl std::ops::Deref for RetainedFixture {
            type Target = Fixture;
            fn deref(&self) -> &Fixture {
                self.0.as_ref().unwrap()
            }
        }
        impl Drop for RetainedFixture {
            fn drop(&mut self) {
                use std::io::Write as _;
                let panicking = std::thread::panicking();
                if let Some(fixture) = self.0.take() {
                    let Fixture { _temp, store, .. } = fixture;
                    let root = _temp.keep();
                    let raw = root.join("raw-observed");
                    let created = std::fs::create_dir(&raw).is_ok();
                    for name in ["host.db", "host.db-wal", "host.db-shm"] {
                        let source = root.join("host").join(name);
                        let result = if !created {
                            "directory_error"
                        } else {
                            match std::fs::copy(&source, raw.join(name)) {
                                Ok(_) => "copied_live_non_snapshot",
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                    "missing"
                                }
                                Err(_) => "copy_error",
                            }
                        };
                        let _ = writeln!(
                            std::io::stderr().lock(),
                            "N13 raw observation {name}: {result}"
                        );
                    }
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "N13 retained fixture={} panicking={panicking} (live copies are not a consistent snapshot; task/child termination not established)",
                        root.display()
                    );
                    drop(store);
                }
            }
        }
        #[cfg(windows)]
        let diagnostic_case = crate::worktree_diagnostics::Case::new(control_root.unwrap(), "n13");
        #[cfg(windows)]
        let f = RetainedFixture(Some(Fixture::new_supervised(&diagnostic_case)));
        #[cfg(not(windows))]
        let f = RetainedFixture(Some(Fixture::new()));
        println!(
            "N13 fixture={} pid={}",
            f._temp.path().display(),
            std::process::id()
        );
        let engine = Engine::new(f.store.clone(), 2).unwrap();
        #[cfg(not(windows))]
        let diagnostic_case =
            crate::worktree_diagnostics::Case::new(f._temp.path().to_path_buf(), "n13");
        engine.set_worktree_diagnostics(diagnostic_case.clone());
        let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
        let ready = std::sync::Mutex::new(Some(ready_tx));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = std::sync::Mutex::new(release_rx);
        engine.set_worktree_probe_for_test(Arc::new(move |phase, _| {
            if phase == crate::engine::WorktreeProbePhase::ClaimCommitted {
                ready.lock().unwrap().take().unwrap().send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
            }
        }));
        let request = f.request();
        let e = engine.clone();
        let r = request.clone();
        let mut caller = tokio::spawn(crate::worktree_diagnostics::scope(
            diagnostic_case.clone(),
            async move { e.create_project_worktree(r).await },
        ));
        #[cfg(windows)]
        let operation_budget = crate::worktree_diagnostics::TOTAL_SECS;
        #[cfg(not(windows))]
        let operation_budget = 10;
        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(operation_budget), async {
                tokio::select! {
                    biased;
                    result = &mut caller => Err(result),
                    ready = &mut ready_rx => { ready.expect("N13 probe sender closed"); Ok(()) },
                }
            })
            .await;
        match outcome {
            Ok(Ok(())) if !caller.is_finished() => crate::worktree_diagnostics::case_event(
                &diagnostic_case,
                "n13_ready_caller_pending",
            ),
            Ok(Ok(())) => {
                crate::worktree_diagnostics::case_event(
                    &diagnostic_case,
                    "n13_ready_caller_finished",
                );
                let result = caller.await;
                let category = match result {
                    Ok(Ok(_)) => "success",
                    Ok(Err(_)) => "error",
                    Err(_) => "join_error",
                };
                panic!("N13 caller completed before abort: {category}");
            }
            Ok(Err(result)) => {
                crate::worktree_diagnostics::case_event(
                    &diagnostic_case,
                    match ready_rx.try_recv() {
                        Ok(()) => "n13_caller_done_ready_observed",
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                            "n13_caller_done_ready_pending"
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                            "n13_caller_done_ready_closed"
                        }
                    },
                );
                let category = match result {
                    Ok(Ok(_)) => "success",
                    Ok(Err(_)) => "error",
                    Err(_) => "join_error",
                };
                crate::worktree_diagnostics::case_event(
                    &diagnostic_case,
                    match category {
                        "success" => "n13_caller_success",
                        "error" => "n13_caller_error",
                        _ => "n13_caller_join_error",
                    },
                );
                panic!("N13 caller completed before abort: {category}");
            }
            Err(_) => {
                crate::worktree_diagnostics::case_event(
                    &diagnostic_case,
                    match ready_rx.try_recv() {
                        Ok(()) => "n13_timeout_ready_observed",
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                            "n13_timeout_ready_pending"
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                            "n13_timeout_ready_closed"
                        }
                    },
                );
                crate::worktree_diagnostics::case_event(
                    &diagnostic_case,
                    if caller.is_finished() {
                        "n13_timeout_caller_finished"
                    } else {
                        "n13_timeout_caller_pending"
                    },
                );
                if caller.is_finished() {
                    let result = caller.await;
                    let category = match result {
                        Ok(Ok(_)) => "success",
                        Ok(Err(_)) => "error",
                        Err(_) => "join_error",
                    };
                    crate::worktree_diagnostics::case_event(
                        &diagnostic_case,
                        match category {
                            "success" => "n13_timeout_caller_success",
                            "error" => "n13_timeout_caller_error",
                            _ => "n13_timeout_caller_join_error",
                        },
                    );
                    panic!("N13 ready timeout; caller completed: {category}");
                }
                panic!("N13 ready timeout; caller pending");
            }
        }
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert_eq!(f.count("worktree_project_reservations"), 1);
        release_tx.send(()).unwrap();
        #[cfg(windows)]
        let completion_budget = crate::worktree_diagnostics::TOTAL_SECS;
        #[cfg(not(windows))]
        let completion_budget = 15;
        let receipt =
            tokio::time::timeout(std::time::Duration::from_secs(completion_budget), async {
                loop {
                    let r = engine
                        .lookup_project_worktree(&f.project.id, &request.idempotency_key)
                        .unwrap();
                    if r.state != WorktreeState::Claimed {
                        break r;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
        assert_eq!(receipt.state, WorktreeState::Succeeded);
        assert_eq!(f.count("project_worktree_origins"), 1);
        assert_eq!(
            engine.create_project_worktree(request).await.unwrap(),
            receipt
        );
        crate::worktree_diagnostics::case_event(&diagnostic_case, "n13_assertions_complete");
    }
    #[tokio::test]
    async fn n13_origin_saved_admin_relationship_and_online_identity_are_enforced() {
        let f = Fixture::new();
        let engine = Engine::new(f.store.clone(), 2).unwrap();
        let receipt = engine.create_project_worktree(f.request()).await.unwrap();
        assert_eq!(receipt.state, WorktreeState::Succeeded);
        let raw: String = f
            .store
            .db
            .lock()
            .unwrap()
            .query_row("SELECT identity FROM project_worktree_origins", [], |r| {
                r.get(0)
            })
            .unwrap();
        let original: WorktreeOrigin = serde_json::from_str(&raw).unwrap();
        for invalid in [
            RootIdentity::capture(&f.source).unwrap(),
            original.root.clone(),
            original.common_dir.clone(),
        ] {
            let mut origin = original.clone();
            origin.git_dir = invalid;
            f.store
                .db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE project_worktree_origins SET identity=?1",
                    [serde_json::to_string(&origin).unwrap()],
                )
                .unwrap();
            for _ in 0..2 {
                assert!(Store::open(&f.db).is_err());
            }
            assert!(
                engine
                    .inspect_project_worktree(&f.project.id, &receipt.operation_id)
                    .await
                    .is_err()
            );
        }
        f.store
            .db
            .lock()
            .unwrap()
            .execute("UPDATE project_worktree_origins SET identity=?1", [&raw])
            .unwrap();
        let admin = original.git_dir.canonical_path();
        let moved = admin.with_file_name("saved-admin");
        std::fs::rename(admin, &moved).unwrap();
        std::fs::create_dir(admin).unwrap();
        // Reopen is offline structural verification, not a requirement that the origin still exists.
        let reopened = Store::open(&f.db).unwrap();
        assert_eq!(
            reopened
                .worktree_receipt(&f.project.id, &receipt.operation_id)
                .unwrap(),
            receipt
        );
        let inspected = engine
            .inspect_project_worktree(&f.project.id, &receipt.operation_id)
            .await
            .unwrap();
        assert_eq!(
            inspected.observation.state,
            WorktreeObservationState::IdentityChanged
        );
        assert_eq!(inspected.receipt, receipt);
        assert_eq!(f.count("worktree_project_reservations"), 0);
    }
    #[tokio::test]
    async fn n13_diagnose_offline_source_and_other_project_are_observations_not_errors() {
        for source_offline in [true, false] {
            let f = Fixture::new();
            let unrelated = f._temp.path().join("unrelated");
            std::fs::create_dir(&unrelated).unwrap();
            f.store
                .register_project(RegisterProject {
                    name: "Unrelated".into(),
                    root_path: unrelated.to_string_lossy().into(),
                    idempotency_key: id(),
                })
                .unwrap();
            let engine = Engine::new(f.store.clone(), 2).unwrap();
            let (_, receipt, _, _) = f.claim().await;
            let path = if source_offline {
                &f.source
            } else {
                &unrelated
            };
            std::fs::rename(path, path.with_file_name("moved")).unwrap();
            let reopened = Store::open(&f.db).unwrap();
            assert_eq!(
                reopened
                    .worktree_receipt(&f.project.id, &receipt.operation_id)
                    .unwrap(),
                receipt
            );
            for _ in 0..2 {
                let inspected = engine
                    .inspect_project_worktree(&f.project.id, &receipt.operation_id)
                    .await
                    .unwrap();
                assert_eq!(
                    inspected.observation.state,
                    if source_offline {
                        WorktreeObservationState::IdentityChanged
                    } else {
                        WorktreeObservationState::Unavailable
                    }
                );
                assert_eq!(inspected.receipt, receipt);
                assert_eq!(f.count("worktree_project_reservations"), 1);
            }
            assert_eq!(f.count("projects"), 2);
        }
    }
    #[test]
    fn n13_real_process_three_durable_stops_kill_wait_recover_without_repeat() {
        struct RetainOnFailure(Option<tempfile::TempDir>);
        impl std::ops::Deref for RetainOnFailure {
            type Target = tempfile::TempDir;
            fn deref(&self) -> &tempfile::TempDir {
                self.0.as_ref().unwrap()
            }
        }
        impl Drop for RetainOnFailure {
            fn drop(&mut self) {
                if std::thread::panicking()
                    && let Some(temp) = self.0.take()
                {
                    eprintln!("N13 retained process fixture={}", temp.keep().display());
                }
            }
        }
        const TEST: &str = "worktree_management::tests::n13_real_process_three_durable_stops_kill_wait_recover_without_repeat";
        if let Ok(root) = std::env::var("NHC_WORKTREE_B_CHILD") {
            let root = PathBuf::from(root);
            crate::worktree_diagnostics::wait_start(&root.join("start")).unwrap();
            let phase = std::env::var("NHC_WORKTREE_B_PHASE").unwrap();
            let request: CreateWorktree =
                serde_json::from_slice(&std::fs::read(root.join("request.json")).unwrap()).unwrap();
            let store = Arc::new(Store::open(&root.join("host/host.db")).unwrap());
            let engine = Engine::new(store, 2).unwrap();
            let diagnostics = crate::worktree_diagnostics::Case::new(root.clone(), "n13-process");
            engine.set_worktree_probe_for_test(Arc::new(move |at, op| {
                let matches = matches!(
                    (phase.as_str(), at),
                    ("claim", crate::engine::WorktreeProbePhase::ClaimCommitted)
                        | (
                            "created",
                            crate::engine::WorktreeProbePhase::CreatedBeforeRegister
                        )
                        | (
                            "registered",
                            crate::engine::WorktreeProbePhase::RegisteredBeforeReply
                        )
                );
                if matches {
                    crate::worktree_diagnostics::atomic_write(&root.join("ready"), op.as_bytes())
                        .unwrap();
                    std::thread::sleep(std::time::Duration::from_secs(40));
                    panic!("parent did not terminate stopped child");
                }
            }));
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(crate::worktree_diagnostics::scope(
                    diagnostics,
                    engine.create_project_worktree(request),
                ))
                .unwrap();
            panic!("child returned without stopping");
        }
        for phase in ["claim", "created", "registered"] {
            let fixture = Fixture::new();
            let request = fixture.request();
            let Fixture {
                _temp,
                source,
                db,
                store,
                project,
                ..
            } = fixture;
            let retained = RetainOnFailure(Some(_temp));
            // Transfer ownership to the real child. The parent keeps paths and
            // observations, never a second Store or controller while it lives.
            drop(store);
            std::fs::write(
                retained.path().join("request.json"),
                serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
            let old_head = raw(&source, &["rev-parse", "HEAD"]);
            let old_index = std::fs::read(source.join(".git/index")).unwrap();
            let output = std::fs::File::create(retained.path().join("child.log")).unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", TEST, "--nocapture"])
                .env("NHC_WORKTREE_B_CHILD", retained.path())
                .env("NHC_WORKTREE_B_PHASE", phase)
                .stdout(output.try_clone().unwrap())
                .stderr(output);
            let mut child = N13Child::spawn(&mut command, retained.path(), phase);
            let mut supervisor = crate::worktree_diagnostics::Supervisor::new(
                retained.path().join("worktree-diagnostics.jsonl"),
            );
            loop {
                if let Some(status) = child.child().try_wait().unwrap() {
                    panic!(
                        "{phase} child exited {status}: {}",
                        std::fs::read_to_string(retained.path().join("child.log")).unwrap()
                    );
                }
                if let Err(error) = supervisor.poll() {
                    let status = child.kill_wait();
                    panic!("{phase} supervision failed: {error:#}; child wait={status:?}");
                }
                if let Ok(operation) = std::fs::read_to_string(retained.path().join("ready")) {
                    supervisor.poll().unwrap();
                    let expected_stage = match phase {
                        "claim" => 8,
                        "created" => 9,
                        "registered" => 10,
                        _ => unreachable!(),
                    };
                    assert_eq!(supervisor.stage(), expected_stage);
                    assert!(uuid::Uuid::parse_str(&operation).is_ok());
                    assert!(
                        child.child().try_wait().unwrap().is_none(),
                        "{phase} child exited before kill"
                    );
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let operation = std::fs::read_to_string(retained.path().join("ready")).unwrap();
            assert!(
                Store::open(&db).is_err(),
                "live child lost domain ownership"
            );
            // Read the child's committed receipt without acquiring a Host owner
            // or running initialization/recovery against its live data domain.
            let observer =
                Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap();
            let stopped = read_receipt(&observer, &project.id, &operation).unwrap();
            drop(observer);
            assert_eq!(
                stopped.state,
                if phase == "registered" {
                    WorktreeState::Succeeded
                } else {
                    WorktreeState::Claimed
                }
            );
            let pid = child.child().id();
            let status = child.kill_wait();
            assert!(!status.success());
            eprintln!(
                "N13 actual kill+wait phase={phase} child={} exit={status} operation={operation}",
                pid
            );
            let refs = raw(&source, &["show-ref"]);
            let list = raw(&source, &["worktree", "list", "--porcelain"]);
            let reopened = Arc::new(Store::open(&db).unwrap());
            reopened.recover().unwrap();
            let recovered = reopened.worktree_receipt(&project.id, &operation).unwrap();
            let count = |table: &str| -> i64 {
                reopened
                    .db
                    .lock()
                    .unwrap()
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                    .unwrap()
            };
            assert_eq!(
                recovered.state,
                if phase == "registered" {
                    WorktreeState::Succeeded
                } else {
                    WorktreeState::Unknown
                }
            );
            assert_eq!(count("projects"), if phase == "registered" { 2 } else { 1 });
            assert_eq!(
                count("worktree_project_reservations"),
                if phase == "registered" { 0 } else { 1 }
            );
            let engine = Engine::new(reopened.clone(), 2).unwrap();
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                assert_eq!(
                    engine.create_project_worktree(request).await.unwrap(),
                    recovered
                );
                for _ in 0..2 {
                    let result = engine
                        .inspect_project_worktree(&project.id, &operation)
                        .await
                        .unwrap();
                    assert_eq!(result.receipt, recovered);
                    assert_eq!(
                        result.observation.state,
                        if phase == "claim" {
                            WorktreeObservationState::AbsentObserved
                        } else {
                            WorktreeObservationState::PresentMatching
                        }
                    );
                }
            });
            assert!(
                reopened.recover().is_err(),
                "live controller admitted recovery"
            );
            assert_eq!(
                reopened.worktree_receipt(&project.id, &operation).unwrap(),
                recovered
            );
            drop(engine);
            reopened.recover().unwrap();
            assert_eq!(
                reopened.worktree_receipt(&project.id, &operation).unwrap(),
                recovered
            );
            assert_eq!(
                count("worktree_project_reservations"),
                if phase == "registered" { 0 } else { 1 }
            );
            assert_eq!(raw(&source, &["show-ref"]), refs);
            assert_eq!(raw(&source, &["worktree", "list", "--porcelain"]), list);
            assert_eq!(raw(&source, &["rev-parse", "HEAD"]), old_head);
            assert_eq!(std::fs::read(source.join(".git/index")).unwrap(), old_index);
            for table in ["network_calls", "tasks", "runs"] {
                assert_eq!(count(table), 0);
            }
        }
    }

    #[tokio::test]
    async fn n13_two_real_worktrees_cannot_substitute_each_others_saved_admin() {
        let f = Fixture::new();
        let engine = Engine::new(f.store.clone(), 2).unwrap();
        let first = engine.create_project_worktree(f.request()).await.unwrap();
        let second = engine.create_project_worktree(f.request()).await.unwrap();
        assert_eq!(first.state, WorktreeState::Succeeded);
        assert_eq!(second.state, WorktreeState::Succeeded);
        let get_origin = |op: &str| -> WorktreeOrigin {
            serde_json::from_str(
                &f.store
                    .db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT identity FROM project_worktree_origins WHERE operation_id=?1",
                        [op],
                        |r| r.get::<_, String>(0),
                    )
                    .unwrap(),
            )
            .unwrap()
        };
        let mut origin = get_origin(&first.operation_id);
        let other = get_origin(&second.operation_id);
        assert_ne!(origin.git_dir, other.git_dir);
        assert_eq!(origin.common_dir, other.common_dir);
        origin.git_dir = other.git_dir;
        f.store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE project_worktree_origins SET identity=?1 WHERE operation_id=?2",
                params![serde_json::to_string(&origin).unwrap(), first.operation_id],
            )
            .unwrap();
        // Both admins are structurally valid and exist; only the actual target's observed admin can disambiguate them.
        drop(Store::open(&f.db).unwrap());
        let refs = raw(&f.source, &["show-ref"]);
        let rows = f.count("projects");
        for _ in 0..2 {
            let observed = engine
                .inspect_project_worktree(&f.project.id, &first.operation_id)
                .await
                .unwrap();
            assert_eq!(
                observed.observation.state,
                WorktreeObservationState::IdentityChanged
            );
            assert_eq!(observed.receipt, first);
        }
        assert_eq!(raw(&f.source, &["show-ref"]), refs);
        assert_eq!(f.count("projects"), rows);
    }
    #[tokio::test]
    async fn n13_same_key_two_store_race_replays_one_operation() {
        let f = Fixture::new();
        let _engine = Engine::new(f.store.clone(), 2).unwrap();
        let request = f.request();
        let first = f.plan(&request).await;
        let second = f.plan(&request).await;
        let inventory = f.store.worktree_inventory().unwrap();
        let other = Arc::new(Store::open(&f.db).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = [(f.store.clone(), first), (other, second)]
            .into_iter()
            .map(|(store, plan)| {
                let request = request.clone();
                let inventory = inventory.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    crate::execution_authority::test_with_active_store_sync(&store, || {
                        store.claim_worktree(
                            &request,
                            &inventory,
                            std::slice::from_ref(&request.source_project_id),
                            &plan,
                        )
                    })
                    .unwrap()
                    .unwrap()
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results[0].0, results[1].0);
        assert_ne!(results[0].1, results[1].1);
        assert_eq!(f.count("worktree_operations"), 1);
        assert_eq!(f.count("worktree_project_reservations"), 1);
        assert_eq!(std::fs::read_dir(&f.parent).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn n13_registration_inventory_and_claim_race_has_no_unreserved_new_member() {
        for _ in 0..3 {
            let f = Fixture::new();
            let _engine = Engine::new(f.store.clone(), 2).unwrap();
            let request = f.request();
            let plan = f.plan(&request).await;
            let inventory = f.store.worktree_inventory().unwrap();
            let other = Arc::new(Store::open(&f.db).unwrap());
            let root = f._temp.path().join("new");
            std::fs::create_dir(&root).unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let b = barrier.clone();
            let store = f.store.clone();
            let claimed = std::thread::spawn(move || {
                b.wait();
                crate::execution_authority::test_with_active_store_sync(&store, || {
                    store.claim_worktree(
                        &request,
                        &inventory,
                        std::slice::from_ref(&request.source_project_id),
                        &plan,
                    )
                })
                .unwrap()
            });
            let registered = std::thread::spawn(move || {
                barrier.wait();
                crate::execution_authority::test_with_active_store_sync(&other, || {
                    other.register_project(RegisterProject {
                        name: "Concurrent".into(),
                        root_path: root.to_string_lossy().into(),
                        idempotency_key: id(),
                    })
                })
                .unwrap()
            });
            match (claimed.join().unwrap(), registered.join().unwrap()) {
                (Ok(_), Err(error)) => {
                    assert_eq!(
                        error.downcast_ref::<WorktreeError>(),
                        Some(&WorktreeError::Busy)
                    );
                    assert_eq!(f.count("projects"), 1);
                    assert_eq!(f.count("worktree_operations"), 1);
                }
                (Err(error), Ok(_)) => {
                    assert_eq!(
                        error.downcast_ref::<WorktreeError>(),
                        Some(&WorktreeError::Conflict)
                    );
                    assert_eq!(f.count("projects"), 2);
                    assert_eq!(f.count("worktree_operations"), 0);
                }
                result => panic!("registration/claim must have exactly one winner: {result:?}"),
            }
        }
    }
    #[tokio::test]
    async fn n13_removed_sibling_reservation_is_detected_and_source_scopes_do_not_leak() {
        let f = Fixture::new();
        let engine = Engine::new(f.store.clone(), 2).unwrap();
        let successful = engine.create_project_worktree(f.request()).await.unwrap();
        let sibling = successful.project_id.clone().unwrap();
        let request = f.request();
        let plan = f.plan(&request).await;
        let inventory = f.store.worktree_inventory().unwrap();
        let members = vec![f.project.id.clone(), sibling.clone()];
        let receipt = crate::execution_authority::test_with_active_store_sync(&f.store, || {
            f.store
                .claim_worktree(&request, &inventory, &members, &plan)
        })
        .unwrap()
        .unwrap()
        .0;
        assert_eq!(f.count("worktree_project_reservations"), 2);
        assert_eq!(
            engine
                .worktree_receipt(&sibling, &receipt.operation_id)
                .unwrap_err()
                .downcast_ref::<WorktreeError>(),
            Some(&WorktreeError::NotFound)
        );
        assert_eq!(
            engine
                .lookup_project_worktree(&sibling, &request.idempotency_key)
                .unwrap_err()
                .downcast_ref::<WorktreeError>(),
            Some(&WorktreeError::NotFound)
        );
        let mut conflicting = request.clone();
        conflicting.source_project_id = sibling.clone();
        assert_eq!(
            engine
                .create_project_worktree(conflicting)
                .await
                .unwrap_err()
                .downcast_ref::<WorktreeError>(),
            Some(&WorktreeError::IdempotencyConflict)
        );
        f.store
            .db
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM worktree_project_reservations WHERE project_id=?1",
                [&sibling.0],
            )
            .unwrap();
        for _ in 0..2 {
            assert!(Store::open(&f.db).is_err());
        }
        assert_eq!(f.count("worktree_project_reservations"), 1);
    }
    #[tokio::test]
    async fn n13_stale_configuration_and_head_fail_before_claim() {
        let f = Fixture::new();
        let engine = Engine::new(f.store.clone(), 2).unwrap();
        let mut request = f.request();
        request.expected_config_revision += 1;
        assert_eq!(
            engine
                .create_project_worktree(request)
                .await
                .unwrap_err()
                .downcast_ref::<WorktreeError>(),
            Some(&WorktreeError::RevisionConflict)
        );
        let mut request = f.request();
        request.expected_head = "0".repeat(40);
        assert_eq!(
            engine
                .create_project_worktree(request)
                .await
                .unwrap_err()
                .downcast_ref::<WorktreeError>(),
            Some(&WorktreeError::Conflict)
        );
        assert_eq!(f.count("worktree_operations"), 0);
        assert_eq!(f.count("projects"), 1);
        assert_eq!(std::fs::read_dir(&f.parent).unwrap().count(), 0);
    }
}
