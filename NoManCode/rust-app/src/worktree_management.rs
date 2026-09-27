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
                    && self.cause.is_none(),
                WorktreeError::Corrupt
            ),
            WorktreeState::Claimed => ensure!(
                self.project_id.is_none() && self.cause.is_none(),
                WorktreeError::Corrupt
            ),
            WorktreeState::Rejected | WorktreeState::Unknown => ensure!(
                self.project_id.is_none() && self.cause.is_some(),
                WorktreeError::Corrupt
            ),
        }
        Ok(())
    }
}
pub(crate) fn verify_persisted_rows(db: &Connection) -> Result<()> {
    let mut statement=db.prepare("SELECT operation_id,idempotency_key,source_project_id,requested_project_id,request_json,request_digest,plan,common_key,target_path,branch_ref,state,revision,receipt FROM worktree_operations")?;
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
        let reserved: i64 = db.query_row(
            "SELECT count(*) FROM worktree_project_reservations WHERE operation_id=?1",
            [&receipt.operation_id],
            |r| r.get(0),
        )?;
        if matches!(
            receipt.state,
            WorktreeState::Claimed | WorktreeState::Unknown
        ) {
            ensure!(reserved > 0, WorktreeError::Corrupt);
            ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM worktree_project_reservations WHERE project_id=?1 AND operation_id=?2)",rusqlite::params![receipt.source_project_id.0,receipt.operation_id],|r|r.get::<_,bool>(0))?,WorktreeError::Corrupt);
        } else {
            ensure!(reserved == 0, WorktreeError::Corrupt);
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
        }
    }
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
        read_receipt(
            &self.db.lock().map_err(|_| WorktreeError::Corrupt)?,
            source,
            operation,
        )
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
