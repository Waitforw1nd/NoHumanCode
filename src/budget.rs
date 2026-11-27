//! Project-scoped Completion budget configuration, accounting, and admission checks.
use crate::{
    domain::{ProjectId, valid_name},
    network_authorization::{CallReceipt, CallResult, NetworkPurpose, NetworkSubject},
    provider_usage::UsageObservation,
    store::Store,
};
use anyhow::Result as AnyResult;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

pub const MAX_LIMIT_TOKENS: u64 = 1_000_000_000_000;
pub const MAX_RESERVATION_TOKENS: u64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfig {
    pub limit_tokens: u64,
    pub per_call_reservation_tokens: u64,
}

impl BudgetConfig {
    pub fn validate(&self) -> Result<(), BudgetError> {
        if self.limit_tokens > MAX_LIMIT_TOKENS
            || !(1..=MAX_RESERVATION_TOKENS).contains(&self.per_call_reservation_tokens)
        {
            return Err(BudgetErrorKind::Invalid.into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfigSnapshot {
    pub project_id: ProjectId,
    pub revision: u64,
    pub config: Option<BudgetConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureBudget {
    pub project_id: ProjectId,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub config: BudgetConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetMutationResult {
    pub snapshot: BudgetConfigSnapshot,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl BudgetUsage {
    pub fn validate(&self) -> Result<(), BudgetError> {
        let Some(sum) = self.prompt_tokens.checked_add(self.completion_tokens) else {
            return Err(BudgetErrorKind::Invalid.into());
        };
        if self.prompt_tokens > i64::MAX as u64
            || self.completion_tokens > i64::MAX as u64
            || self.total_tokens > i64::MAX as u64
            || self.total_tokens < sum
        {
            return Err(BudgetErrorKind::Invalid.into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetMode {
    Unconfigured,
    Configured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetCallState {
    Claimed,
    Settled,
    NotSent,
    UsageMissing,
    UsageInvalid,
    Unknown,
    Overrun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetBlockedReason {
    Unresolved,
    Overrun,
    Exceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetCallReason {
    PreflightRejected,
    UsageMissing,
    UsageInvalid,
    TransportUnknown,
    Interrupted,
    PersistenceUnknown,
    ReservationOverrun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetStatus {
    pub project_id: ProjectId,
    pub snapshot: BudgetConfigSnapshot,
    pub mode: BudgetMode,
    pub spent_tokens: u64,
    pub held_tokens: u64,
    pub available_tokens: Option<u64>,
    pub claimed_count: u64,
    pub unresolved_count: u64,
    pub overrun_count: u64,
    pub blocked_reason: Option<BudgetBlockedReason>,
    pub coverage: String,
    pub coverage_start_call_rowid: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetCall {
    pub project_id: ProjectId,
    pub call_id: String,
    pub call: CallReceipt,
    pub managed: bool,
    pub budget_revision: Option<u64>,
    pub reserved_tokens: Option<u64>,
    pub state: Option<BudgetCallState>,
    pub usage: Option<BudgetUsage>,
    pub reason: Option<BudgetCallReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetErrorKind {
    Invalid,
    NotFound,
    RevisionConflict,
    Conflict,
    Busy,
    Exceeded,
    Unresolved,
    Overrun,
    Corrupt,
    Internal,
}

impl BudgetErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "budget_invalid",
            Self::NotFound => "budget_not_found",
            Self::RevisionConflict => "budget_revision_conflict",
            Self::Conflict => "budget_conflict",
            Self::Busy => "budget_busy",
            Self::Exceeded => "budget_exceeded",
            Self::Unresolved => "budget_unresolved",
            Self::Overrun => "budget_overrun",
            Self::Corrupt => "budget_corrupt",
            Self::Internal => "budget_internal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetError {
    pub kind: BudgetErrorKind,
}

impl From<BudgetErrorKind> for BudgetError {
    fn from(kind: BudgetErrorKind) -> Self {
        Self { kind }
    }
}

impl std::fmt::Display for BudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.kind.code())
    }
}

impl std::error::Error for BudgetError {}

fn err(kind: BudgetErrorKind) -> anyhow::Error {
    BudgetError::from(kind).into()
}

fn corrupt() -> anyhow::Error {
    err(BudgetErrorKind::Corrupt)
}

pub(crate) fn safe_error(error: anyhow::Error) -> BudgetError {
    error
        .downcast_ref::<BudgetError>()
        .copied()
        .unwrap_or(BudgetErrorKind::Internal.into())
}

fn integer(value: u64) -> AnyResult<u64> {
    if value > i64::MAX as u64 {
        return Err(corrupt());
    }
    Ok(value)
}

fn plus(a: u64, b: u64) -> AnyResult<u64> {
    integer(a.checked_add(b).ok_or_else(corrupt)?)
}

fn project_exists(db: &Connection, project: &ProjectId) -> AnyResult<()> {
    if !valid_name(&project.0) {
        return Err(err(BudgetErrorKind::Invalid));
    }
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_configs WHERE project_id=?1)",
        [&project.0],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(err(BudgetErrorKind::NotFound));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Policy {
    snapshot: BudgetConfigSnapshot,
    boundary: u64,
}

fn policy(db: &Connection, project: &ProjectId) -> AnyResult<Option<Policy>> {
    let row: Option<(u64, u64, u64, u64)> = db
        .query_row(
            "SELECT revision,limit_tokens,reservation_tokens,coverage_start_call_rowid FROM project_budgets WHERE project_id=?1",
            [&project.0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    row.map(
        |(revision, limit_tokens, per_call_reservation_tokens, boundary)| {
            let config = BudgetConfig {
                limit_tokens,
                per_call_reservation_tokens,
            };
            config.validate().map_err(|_| corrupt())?;
            if revision == 0 {
                return Err(corrupt());
            }
            integer(revision)?;
            integer(boundary)?;
            Ok(Policy {
                snapshot: BudgetConfigSnapshot {
                    project_id: project.clone(),
                    revision,
                    config: Some(config),
                },
                boundary,
            })
        },
    )
    .transpose()
}

fn semantic(call: &BudgetCall) -> AnyResult<String> {
    Ok(crate::workspace_changes::digest(&serde_json::to_vec(&(
        call.call.state,
        call.call.success,
        call.call.http_status,
        call.call.result,
        call.state,
        call.usage,
        call.reason,
    ))?))
}

fn load_entry(db: &Connection, id: &str) -> AnyResult<Option<BudgetCall>> {
    let raw: Option<String> = db
        .query_row(
            "SELECT record FROM budget_calls WHERE call_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|raw| serde_json::from_str::<BudgetCall>(&raw).map_err(|_| corrupt()))
        .transpose()
}

#[allow(dead_code)]
pub(crate) fn verify_structure(_db: &Connection) -> AnyResult<()> {
    Ok(())
}

fn status_calculate(db: &Connection, project: &ProjectId) -> AnyResult<BudgetStatus> {
    project_exists(db, project)?;
    let config = policy(db, project)?;
    let mut result = BudgetStatus {
        project_id: project.clone(),
        snapshot: config
            .as_ref()
            .map(|c| c.snapshot.clone())
            .unwrap_or(BudgetConfigSnapshot {
                project_id: project.clone(),
                revision: 0,
                config: None,
            }),
        mode: if config.is_some() {
            BudgetMode::Configured
        } else {
            BudgetMode::Unconfigured
        },
        spent_tokens: 0,
        held_tokens: 0,
        available_tokens: None,
        claimed_count: 0,
        unresolved_count: 0,
        overrun_count: 0,
        blocked_reason: None,
        coverage: "since_configuration".into(),
        coverage_start_call_rowid: config.as_ref().map(|c| c.boundary),
    };
    if let Some(config) = config {
        let mut stmt = db.prepare("SELECT record FROM budget_calls WHERE project_id=?1")?;
        let records: Vec<String> = stmt
            .query_map([&project.0], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for raw in records {
            let entry: BudgetCall = serde_json::from_str(&raw).map_err(|_| corrupt())?;
            match entry.state.ok_or_else(corrupt)? {
                BudgetCallState::Claimed => {
                    result.claimed_count = plus(result.claimed_count, 1)?;
                    result.held_tokens =
                        plus(result.held_tokens, entry.reserved_tokens.unwrap_or(0))?;
                }
                BudgetCallState::Unknown
                | BudgetCallState::UsageMissing
                | BudgetCallState::UsageInvalid => {
                    result.unresolved_count = plus(result.unresolved_count, 1)?;
                }
                BudgetCallState::Settled | BudgetCallState::Overrun => {
                    if let Some(usage) = entry.usage {
                        result.spent_tokens = plus(result.spent_tokens, usage.total_tokens)?;
                    }
                    if entry.state == Some(BudgetCallState::Overrun) {
                        result.overrun_count = plus(result.overrun_count, 1)?;
                    }
                }
                BudgetCallState::NotSent => {}
            }
        }
        let cfg = config.snapshot.config.unwrap();
        let occupied = plus(result.spent_tokens, result.held_tokens)?;
        result.available_tokens = Some(cfg.limit_tokens.saturating_sub(occupied));
        result.blocked_reason = if occupied >= cfg.limit_tokens {
            Some(BudgetBlockedReason::Exceeded)
        } else {
            None
        };
    }
    Ok(result)
}

impl Store {
    pub fn budget_config(&self, project: &ProjectId) -> Result<BudgetConfigSnapshot, BudgetError> {
        let db = self.db.lock().unwrap();
        status_calculate(&db, project)
            .map(|status| status.snapshot)
            .map_err(safe_error)
    }

    pub fn budget_status(&self, project: &ProjectId) -> Result<BudgetStatus, BudgetError> {
        let db = self.db.lock().unwrap();
        status_calculate(&db, project).map_err(safe_error)
    }

    pub fn budget_call(&self, project: &ProjectId, id: &str) -> Result<BudgetCall, BudgetError> {
        (|| -> AnyResult<_> {
            let db = self.db.lock().unwrap();
            project_exists(&db, project)?;
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM network_calls WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(err(BudgetErrorKind::NotFound));
            }
            let call = crate::network_authorization::receipt(&db, id).map_err(|_| corrupt())?;
            if call.subject.project_id.as_ref() != Some(project)
                || call.purpose != NetworkPurpose::Completion
            {
                return Err(err(BudgetErrorKind::NotFound));
            }
            let result = load_entry(&db, id)?.unwrap_or(BudgetCall {
                project_id: project.clone(),
                call_id: id.into(),
                call,
                managed: false,
                budget_revision: None,
                reserved_tokens: None,
                state: None,
                usage: None,
                reason: None,
            });
            Ok(result)
        })()
        .map_err(safe_error)
    }

    pub fn configure_budget(
        &self,
        request: ConfigureBudget,
    ) -> Result<BudgetMutationResult, BudgetError> {
        (|| -> AnyResult<_> {
            request.config.validate()?;
            crate::secrets::validate_idempotency_key(&request.idempotency_key)
                .map_err(|_| err(BudgetErrorKind::Invalid))?;
            if request.expected_revision > i64::MAX as u64 {
                return Err(err(BudgetErrorKind::Invalid));
            }
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            project_exists(&tx, &request.project_id)?;
            let old_req: Option<(String, String)> = tx
                .query_row(
                    "SELECT request,result FROM budget_requests WHERE key=?1",
                    [&request.idempotency_key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((raw, value)) = old_req {
                let original: ConfigureBudget =
                    serde_json::from_str(&raw).map_err(|_| corrupt())?;
                if original != request {
                    return Err(err(BudgetErrorKind::Conflict));
                }
                let mut result: BudgetMutationResult =
                    serde_json::from_str(&value).map_err(|_| corrupt())?;
                result.replayed = true;
                tx.commit()?;
                return Ok(result);
            }
            let old = policy(&tx, &request.project_id)?;
            if old.as_ref().map_or(0, |c| c.snapshot.revision) != request.expected_revision {
                return Err(err(BudgetErrorKind::RevisionConflict));
            }
            let boundary = if let Some(old) = old {
                old.boundary
            } else {
                tx.query_row(
                    "SELECT COALESCE(MAX(rowid),0) FROM network_calls",
                    [],
                    |r| r.get::<_, u64>(0),
                )?
            };
            let result = BudgetMutationResult {
                snapshot: BudgetConfigSnapshot {
                    project_id: request.project_id.clone(),
                    revision: plus(request.expected_revision, 1)?,
                    config: Some(request.config),
                },
                replayed: false,
            };
            tx.execute(
                "INSERT INTO project_budgets VALUES(?1,?2,?3,?4,?5) ON CONFLICT(project_id) DO UPDATE SET revision=excluded.revision,limit_tokens=excluded.limit_tokens,reservation_tokens=excluded.reservation_tokens",
                params![
                    request.project_id.0,
                    result.snapshot.revision,
                    request.config.limit_tokens,
                    request.config.per_call_reservation_tokens,
                    boundary
                ],
            )?;
            tx.execute(
                "INSERT INTO budget_requests VALUES(?1,?2,?3,?4,?5)",
                params![
                    request.idempotency_key,
                    request.project_id.0,
                    serde_json::to_string(&request)?,
                    serde_json::to_string(&result)?,
                    crate::workspace_changes::digest(&serde_json::to_vec(&request)?)
                ],
            )?;
            tx.commit()?;
            Ok(result)
        })()
        .map_err(safe_error)
    }
}

pub(crate) fn admit_check_tx(
    db: &Connection,
    subject: &NetworkSubject,
    purpose: NetworkPurpose,
) -> AnyResult<()> {
    if purpose != NetworkPurpose::Completion {
        return Ok(());
    }
    let Some(project) = &subject.project_id else {
        return Ok(());
    };
    let status = status_calculate(db, project)?;
    if status.blocked_reason == Some(BudgetBlockedReason::Exceeded) {
        return Err(err(BudgetErrorKind::Exceeded));
    }
    Ok(())
}

pub(crate) fn next_network_rowid(db: &Connection) -> AnyResult<u64> {
    let high: u64 = db.query_row(
        "SELECT MAX(COALESCE((SELECT MAX(rowid) FROM network_calls),0),COALESCE((SELECT MAX(coverage_start_call_rowid) FROM project_budgets),0))",
        [],
        |r| r.get(0),
    )?;
    plus(high, 1)
}

pub(crate) fn reserve_tx(db: &Transaction<'_>, call: &CallReceipt) -> AnyResult<()> {
    if call.purpose != NetworkPurpose::Completion {
        return Ok(());
    }
    let Some(project) = &call.subject.project_id else {
        return Ok(());
    };
    let Some(config) = policy(db, project)? else {
        return Ok(());
    };
    let value = BudgetCall {
        project_id: project.clone(),
        call_id: call.call_id.clone(),
        call: call.clone(),
        managed: true,
        budget_revision: Some(config.snapshot.revision),
        reserved_tokens: Some(config.snapshot.config.unwrap().per_call_reservation_tokens),
        state: Some(BudgetCallState::Claimed),
        usage: None,
        reason: None,
    };
    db.execute(
        "INSERT INTO budget_calls VALUES(?1,?2,?3,?4,'claimed',?5,NULL)",
        params![
            value.call_id,
            project.0,
            value.budget_revision,
            value.reserved_tokens,
            serde_json::to_string(&value)?
        ],
    )?;
    Ok(())
}

pub(crate) fn finish_tx(
    db: &Transaction<'_>,
    candidate: &CallReceipt,
    observation: Option<UsageObservation>,
) -> AnyResult<Option<CallReceipt>> {
    let Some(mut entry) = load_entry(db, &candidate.call_id)? else {
        return Ok(None);
    };
    let old = entry.clone();
    entry.call = candidate.clone();
    entry.usage = None;
    let (state, reason) = match candidate.result {
        Some(CallResult::Success) => match observation.unwrap_or(UsageObservation::Missing) {
            UsageObservation::Valid(u) => {
                let usage = BudgetUsage {
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                    total_tokens: u.total_tokens,
                };
                usage.validate().map_err(|_| corrupt())?;
                entry.usage = Some(usage);
                if u.total_tokens > entry.reserved_tokens.unwrap_or(0) {
                    (
                        BudgetCallState::Overrun,
                        Some(BudgetCallReason::ReservationOverrun),
                    )
                } else {
                    (BudgetCallState::Settled, None)
                }
            }
            UsageObservation::Missing => (
                BudgetCallState::UsageMissing,
                Some(BudgetCallReason::UsageMissing),
            ),
            UsageObservation::Invalid(_) => (
                BudgetCallState::UsageInvalid,
                Some(BudgetCallReason::UsageInvalid),
            ),
        },
        Some(CallResult::PreflightRejected) => (
            BudgetCallState::NotSent,
            Some(BudgetCallReason::PreflightRejected),
        ),
        Some(CallResult::Interrupted) => (
            BudgetCallState::Unknown,
            Some(BudgetCallReason::Interrupted),
        ),
        Some(CallResult::PersistenceUnknown) => (
            BudgetCallState::Unknown,
            Some(BudgetCallReason::PersistenceUnknown),
        ),
        _ => (
            BudgetCallState::Unknown,
            Some(BudgetCallReason::TransportUnknown),
        ),
    };
    entry.state = Some(state);
    entry.reason = reason;
    if old.state != Some(BudgetCallState::Claimed) {
        if semantic(&old)? != semantic(&entry)? {
            return Err(err(BudgetErrorKind::Conflict));
        }
        return Ok(Some(old.call));
    }
    db.execute(
        "UPDATE budget_calls SET state=?2,record=?3,terminal_fingerprint=?4 WHERE call_id=?1",
        params![
            entry.call_id,
            serde_json::to_value(state)?.as_str(),
            serde_json::to_string(&entry)?,
            semantic(&entry)?
        ],
    )?;
    Ok(Some(entry.call))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{Route, Settings},
        network_authorization::NetworkSubjectKind,
        project_config::{ConfigureProject, ProjectConfig, RegisterProject},
        provider_transport::TransportError,
    };
    use serde_json::json;
    use std::sync::Arc;

    fn setup() -> (tempfile::TempDir, Arc<Store>, ProjectId, NetworkSubject) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("budget.db")).unwrap());
        store
            .save_settings(&Settings {
                workspace: dir.path().to_string_lossy().into(),
                max_concurrency: 4,
                routes: vec![Route {
                    id: "route".into(),
                    name: "Route".into(),
                    base_url: "http://127.0.0.1:9".into(),
                    model: "mock".into(),
                    protocol: None,
                    max_tokens: 128,
                    parallel_limit: 4,
                    key_env: None,
                }],
                newapi: None,
            })
            .unwrap();
        store
            .put_secret("route::route", "budget-test-secret")
            .unwrap();
        let project = store
            .register_project(RegisterProject {
                name: "budget".into(),
                root_path: dir.path().to_string_lossy().into(),
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            })
            .unwrap()
            .id;
        store
            .configure_project(ConfigureProject {
                project_id: project.clone(),
                expected_revision: 1,
                config: ProjectConfig {
                    allowed_route_ids: vec!["route".into()],
                    default_route_id: Some("route".into()),
                    tools: false,
                    write_scopes: vec![],
                    allow_commands: false,
                },
            })
            .unwrap();
        let subject = NetworkSubject {
            kind: NetworkSubjectKind::Route,
            id: "route".into(),
            project_id: Some(project.clone()),
        };
        (dir, store, project, subject)
    }

    #[test]
    fn budget_usage_accepts_reasoning_totals() {
        let usage = BudgetUsage {
            prompt_tokens: 20,
            completion_tokens: 10,
            total_tokens: 45,
        };
        assert!(usage.validate().is_ok());
        let bad = BudgetUsage {
            prompt_tokens: 20,
            completion_tokens: 10,
            total_tokens: 29,
        };
        assert_eq!(bad.validate().unwrap_err().kind, BudgetErrorKind::Invalid);
    }

    #[test]
    fn overrun_and_unresolved_never_permanently_brick_and_raising_limit_unblocks() {
        let (_dir, store, project, subject) = setup();
        store
            .configure_budget(ConfigureBudget {
                project_id: project.clone(),
                expected_revision: 0,
                idempotency_key: "cfg-1".into(),
                config: BudgetConfig {
                    limit_tokens: 100,
                    per_call_reservation_tokens: 20,
                },
            })
            .unwrap();

        // Missing usage increments unresolved_count without bricking the project.
        let (_, missing_call) = store
            .network_admit(&subject, NetworkPurpose::Completion, None)
            .unwrap();
        store
            .finish_completion(
                missing_call,
                Ok((json!({"role":"assistant","content":"ok"}), json!(null))),
            )
            .unwrap();
        let status = store.budget_status(&project).unwrap();
        assert_eq!(status.unresolved_count, 1);
        assert_eq!(status.held_tokens, 0);
        assert_eq!(status.blocked_reason, None);

        // HTTP 429 failure never permanently holds reservation or bricks future calls.
        let (_, failed_call) = store
            .network_admit(&subject, NetworkPurpose::Completion, None)
            .unwrap();
        let _ = store.finish_completion::<serde_json::Value>(
            failed_call,
            Err(TransportError::Response { status: 429 }),
        );
        assert_eq!(store.budget_status(&project).unwrap().blocked_reason, None);

        // Overrun within limit does not block; exceeding limit blocks until limit is raised.
        let (_, overrun_call) = store
            .network_admit(&subject, NetworkPurpose::Completion, None)
            .unwrap();
        store
            .finish_completion(
                overrun_call,
                Ok((
                    json!({"role":"assistant","content":"ok"}),
                    json!({"prompt_tokens":30,"completion_tokens":20,"total_tokens":55}),
                )),
            )
            .unwrap();
        let status = store.budget_status(&project).unwrap();
        assert_eq!(status.overrun_count, 1);
        assert_eq!(status.spent_tokens, 55);
        assert_eq!(status.blocked_reason, None);

        let (_, hit_limit) = store
            .network_admit(&subject, NetworkPurpose::Completion, None)
            .unwrap();
        store
            .finish_completion(
                hit_limit,
                Ok((
                    json!({"role":"assistant","content":"ok"}),
                    json!({"prompt_tokens":30,"completion_tokens":15,"total_tokens":45}),
                )),
            )
            .unwrap();
        let status = store.budget_status(&project).unwrap();
        assert_eq!(status.spent_tokens, 100);
        assert_eq!(status.blocked_reason, Some(BudgetBlockedReason::Exceeded));

        store
            .configure_budget(ConfigureBudget {
                project_id: project.clone(),
                expected_revision: 1,
                idempotency_key: "cfg-2".into(),
                config: BudgetConfig {
                    limit_tokens: 200,
                    per_call_reservation_tokens: 20,
                },
            })
            .unwrap();
        assert_eq!(store.budget_status(&project).unwrap().blocked_reason, None);
    }
}
