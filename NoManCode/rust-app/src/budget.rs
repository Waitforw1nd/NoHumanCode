//! Project-scoped, forward-only Completion admission and durable usage facts.
use crate::{domain::ProjectId, network_authorization::CallReceipt};
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
        if self.prompt_tokens > i64::MAX as u64
            || self.completion_tokens > i64::MAX as u64
            || self.total_tokens > i64::MAX as u64
            || self.prompt_tokens.checked_add(self.completion_tokens) != Some(self.total_tokens)
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

use crate::{
    domain::valid_name,
    network_authorization::{CallResult, CallState, NetworkPurpose, NetworkSubject},
    provider_usage::UsageObservation,
    store::Store,
};
use anyhow::Result as AnyResult;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

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
    let row: Option<(u64,u64,u64,u64)> = db.query_row("SELECT revision,limit_tokens,reservation_tokens,coverage_start_call_rowid FROM project_budgets WHERE project_id=?1", [&project.0], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
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
            let maximum: u64 = db.query_row(
                "SELECT COALESCE(MAX(rowid),0) FROM network_calls",
                [],
                |r| r.get(0),
            )?;
            if boundary > maximum {
                return Err(corrupt());
            }
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
fn call_ids(db: &Connection) -> AnyResult<Vec<String>> {
    Ok(db
        .prepare("SELECT id FROM network_calls ORDER BY rowid")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
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
    let raw: Option<(String,String,u64,u64,String,Option<String>)>=db.query_row("SELECT record,project_id,budget_revision,reserved_tokens,state,terminal_fingerprint FROM budget_calls WHERE call_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    raw.map(|(raw, project, revision, reserved, state, fingerprint)| {
        let value: BudgetCall = serde_json::from_str(&raw).map_err(|_| corrupt())?;
        if !value.managed
            || value.call_id != id
            || value.project_id.0 != project
            || value.budget_revision != Some(revision)
            || value.reserved_tokens != Some(reserved)
            || serde_json::to_value(value.state)?.as_str() != Some(state.as_str())
        {
            return Err(corrupt());
        }
        if revision == 0 || reserved == 0 || reserved > MAX_RESERVATION_TOKENS {
            return Err(corrupt());
        }
        integer(revision)?;
        let terminal = value.state != Some(BudgetCallState::Claimed);
        if (terminal && fingerprint.as_deref() != Some(semantic(&value)?.as_str()))
            || (!terminal && fingerprint.is_some())
        {
            return Err(corrupt());
        }
        Ok(value)
    })
    .transpose()
}
fn validate_pair(
    db: &Connection,
    value: &BudgetCall,
    network: &CallReceipt,
    rowid: u64,
    config: &Policy,
) -> AnyResult<()> {
    if value.call != *network
        || network.call_id != value.call_id
        || network.subject.project_id.as_ref() != Some(&value.project_id)
        || network.purpose != NetworkPurpose::Completion
        || rowid <= config.boundary
        || value
            .budget_revision
            .is_none_or(|v| v > config.snapshot.revision)
    {
        return Err(corrupt());
    }
    let state = value.state.ok_or_else(corrupt)?;
    let reason = value.reason;
    let good = match state {
        BudgetCallState::Claimed => {
            network.state == CallState::Claimed && value.usage.is_none() && reason.is_none()
        }
        BudgetCallState::Settled | BudgetCallState::Overrun => {
            let u = value.usage.ok_or_else(corrupt)?;
            u.validate().map_err(|_| corrupt())?;
            network.state == CallState::Finished
                && network.result == Some(CallResult::Success)
                && network.success == Some(true)
                && if state == BudgetCallState::Settled {
                    u.total_tokens <= value.reserved_tokens.unwrap() && reason.is_none()
                } else {
                    u.total_tokens > value.reserved_tokens.unwrap()
                        && reason == Some(BudgetCallReason::ReservationOverrun)
                }
        }
        BudgetCallState::NotSent => {
            network.state == CallState::NotSent
                && value.usage.is_none()
                && reason == Some(BudgetCallReason::PreflightRejected)
        }
        BudgetCallState::UsageMissing | BudgetCallState::UsageInvalid => {
            network.state == CallState::Finished
                && network.result == Some(CallResult::Success)
                && network.success == Some(true)
                && value.usage.is_none()
                && reason
                    == Some(if state == BudgetCallState::UsageMissing {
                        BudgetCallReason::UsageMissing
                    } else {
                        BudgetCallReason::UsageInvalid
                    })
        }
        BudgetCallState::Unknown => {
            value.usage.is_none()
                && match network.result {
                    Some(CallResult::TransportUnknown) => {
                        network.state == CallState::Unknown
                            && reason == Some(BudgetCallReason::TransportUnknown)
                    }
                    Some(CallResult::Interrupted) => {
                        network.state == CallState::Unknown
                            && reason == Some(BudgetCallReason::Interrupted)
                    }
                    Some(CallResult::PersistenceUnknown) => {
                        network.state == CallState::Unknown
                            && reason == Some(BudgetCallReason::PersistenceUnknown)
                    }
                    Some(CallResult::HttpFailure | CallResult::InvalidResponse) => {
                        network.state == CallState::Finished
                            && network.success == Some(false)
                            && reason == Some(BudgetCallReason::TransportUnknown)
                    }
                    _ => false,
                }
        }
    };
    if !good {
        return Err(corrupt());
    }
    let _ = db;
    Ok(())
}
fn checked_call(db: &Connection, network: CallReceipt) -> AnyResult<Option<BudgetCall>> {
    let rowid: u64 = db.query_row(
        "SELECT rowid FROM network_calls WHERE id=?1",
        [&network.call_id],
        |r| r.get(0),
    )?;
    if rowid == 0 {
        return Err(corrupt());
    }
    integer(rowid)?;
    let entry = load_entry(db, &network.call_id)?;
    let scope = network
        .subject
        .project_id
        .as_ref()
        .map(|p| policy(db, p))
        .transpose()?
        .flatten();
    match (&entry, scope) {
        (Some(entry), Some(config)) => validate_pair(db, entry, &network, rowid, &config)?,
        (Some(_), None) => return Err(corrupt()),
        (None, Some(config))
            if network.purpose == NetworkPurpose::Completion && rowid > config.boundary =>
        {
            return Err(corrupt());
        }
        _ => {}
    }
    Ok(entry)
}
pub(crate) fn verify_structure(db: &Connection) -> AnyResult<()> {
    let projects: Vec<String> = db
        .prepare("SELECT project_id FROM project_budgets")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for project in projects {
        project_exists(db, &ProjectId(project.clone()))?;
        policy(db, &ProjectId(project))?;
    }
    for id in call_ids(db)? {
        checked_call(db, crate::network_authorization::receipt(db, &id)?)?;
    }
    let orphan:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM budget_calls b LEFT JOIN network_calls n ON b.call_id=n.id WHERE n.id IS NULL)",[],|r|r.get(0))?;
    if orphan {
        return Err(corrupt());
    }
    let requests: Vec<(String, String, String, String, String)> = db
        .prepare("SELECT key,request,result,project_id,request_digest FROM budget_requests")?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut revisions = std::collections::BTreeMap::<(String, u64), BudgetConfig>::new();
    for (key, request, result, sql_project, request_digest) in requests {
        let request: ConfigureBudget = serde_json::from_str(&request).map_err(|_| corrupt())?;
        let result: BudgetMutationResult = serde_json::from_str(&result).map_err(|_| corrupt())?;
        request.config.validate().map_err(|_| corrupt())?;
        crate::secrets::validate_idempotency_key(&key).map_err(|_| corrupt())?;
        let current = policy(db, &request.project_id)?.ok_or_else(corrupt)?;
        if crate::workspace_changes::digest(&serde_json::to_vec(&request)?) != request_digest
            || request.idempotency_key != key
            || sql_project != request.project_id.0
            || result.replayed
            || result.snapshot.project_id != request.project_id
            || result.snapshot.config != Some(request.config)
            || result.snapshot.revision != plus(request.expected_revision, 1)?
            || current.snapshot.revision < result.snapshot.revision
        {
            return Err(corrupt());
        }
        if revisions
            .insert(
                (request.project_id.0.clone(), result.snapshot.revision),
                request.config,
            )
            .is_some()
        {
            return Err(corrupt());
        }
    }
    let scopes: Vec<String> = db
        .prepare("SELECT project_id FROM project_budgets")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for project in scopes {
        let policy = policy(db, &ProjectId(project.clone()))?.ok_or_else(corrupt)?;
        let history: Vec<_> = revisions
            .range((project.clone(), 0)..=(project.clone(), u64::MAX))
            .collect();
        if history.len() as u64 != policy.snapshot.revision
            || history
                .iter()
                .enumerate()
                .any(|(i, ((_, revision), _))| *revision != i as u64 + 1)
            || revisions.get(&(project, policy.snapshot.revision))
                != policy.snapshot.config.as_ref()
        {
            return Err(corrupt());
        }
    }
    for id in call_ids(db)? {
        if let Some(entry) = load_entry(db, &id)? {
            let config = revisions
                .get(&(
                    entry.project_id.0,
                    entry.budget_revision.ok_or_else(corrupt)?,
                ))
                .ok_or_else(corrupt)?;
            if entry.reserved_tokens != Some(config.per_call_reservation_tokens) {
                return Err(corrupt());
            }
        }
    }
    Ok(())
}
pub(crate) fn verify_persisted_rows(db: &Connection) -> AnyResult<()> {
    verify_structure(db)?;
    let projects: Vec<String> = db
        .prepare("SELECT project_id FROM project_budgets")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for project in projects {
        status_calculate(db, &ProjectId(project))?;
    }
    Ok(())
}
fn status_tx(db: &Connection, project: &ProjectId) -> AnyResult<BudgetStatus> {
    verify_persisted_rows(db)?;
    status_calculate(db, project)
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
    for id in call_ids(db)? {
        let network = crate::network_authorization::receipt(db, &id)?;
        if network.subject.project_id.as_ref() != Some(project) {
            continue;
        }
        if let Some(entry) = checked_call(db, network)? {
            match entry.state.ok_or_else(corrupt)? {
                BudgetCallState::Claimed => {
                    result.claimed_count = plus(result.claimed_count, 1)?;
                    result.held_tokens = plus(result.held_tokens, entry.reserved_tokens.unwrap())?;
                }
                BudgetCallState::Unknown
                | BudgetCallState::UsageMissing
                | BudgetCallState::UsageInvalid => {
                    result.unresolved_count = plus(result.unresolved_count, 1)?;
                    result.held_tokens = plus(result.held_tokens, entry.reserved_tokens.unwrap())?;
                }
                BudgetCallState::Settled | BudgetCallState::Overrun => {
                    result.spent_tokens = plus(
                        result.spent_tokens,
                        entry.usage.ok_or_else(corrupt)?.total_tokens,
                    )?;
                    if entry.state == Some(BudgetCallState::Overrun) {
                        result.overrun_count = plus(result.overrun_count, 1)?;
                    }
                }
                BudgetCallState::NotSent => {}
            }
        }
    }
    if let Some(config) = config {
        let cfg = config.snapshot.config.unwrap();
        let occupied = plus(result.spent_tokens, result.held_tokens)?;
        result.available_tokens = Some(cfg.limit_tokens.checked_sub(occupied).unwrap_or(0));
        result.blocked_reason = if result.unresolved_count > 0 {
            Some(BudgetBlockedReason::Unresolved)
        } else if result.overrun_count > 0 {
            Some(BudgetBlockedReason::Overrun)
        } else if plus(occupied, cfg.per_call_reservation_tokens)? > cfg.limit_tokens {
            Some(BudgetBlockedReason::Exceeded)
        } else {
            None
        };
    }
    Ok(result)
}
impl Store {
    pub fn budget_config(&self, project: &ProjectId) -> Result<BudgetConfigSnapshot, BudgetError> {
        (|| -> AnyResult<_> {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction()?;
            let result = status_tx(&tx, project)?.snapshot;
            tx.commit()?;
            Ok(result)
        })()
        .map_err(safe_error)
    }
    pub fn budget_status(&self, project: &ProjectId) -> Result<BudgetStatus, BudgetError> {
        (|| -> AnyResult<_> {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction()?;
            let result = status_tx(&tx, project)?;
            tx.commit()?;
            Ok(result)
        })()
        .map_err(safe_error)
    }
    pub fn budget_call(&self, project: &ProjectId, id: &str) -> Result<BudgetCall, BudgetError> {
        (|| -> AnyResult<_> {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction()?;
            project_exists(&tx, project)?;
            verify_persisted_rows(&tx)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM network_calls WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(err(BudgetErrorKind::NotFound));
            }
            let call = crate::network_authorization::receipt(&tx, id).map_err(|_| corrupt())?;
            if call.subject.project_id.as_ref() != Some(project)
                || call.purpose != NetworkPurpose::Completion
            {
                return Err(err(BudgetErrorKind::NotFound));
            }
            let result = checked_call(&tx, call.clone())?.unwrap_or(BudgetCall {
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
            tx.commit()?;
            Ok(result)
        })()
        .map_err(safe_error)
    }
    pub fn configure_budget(
        &self,
        request: ConfigureBudget,
    ) -> Result<BudgetMutationResult, BudgetError> {
        (||->AnyResult<_>{request.config.validate()?;crate::secrets::validate_idempotency_key(&request.idempotency_key).map_err(|_|err(BudgetErrorKind::Invalid))?;
            if request.expected_revision>i64::MAX as u64 {return Err(err(BudgetErrorKind::Invalid));}
            let mut db=self.db.lock().unwrap();let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            project_exists(&tx,&request.project_id)?;verify_persisted_rows(&tx)?;
            let old:Option<(String,String)>=tx.query_row("SELECT request,result FROM budget_requests WHERE key=?1",[&request.idempotency_key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((raw,value))=old {let original:ConfigureBudget=serde_json::from_str(&raw).map_err(|_|corrupt())?;if original!=request{return Err(err(BudgetErrorKind::Conflict));}let mut result:BudgetMutationResult=serde_json::from_str(&value).map_err(|_|corrupt())?;result.replayed=true;tx.commit()?;return Ok(result);}
            let old=policy(&tx,&request.project_id)?;
            if old.as_ref().map_or(0,|c|c.snapshot.revision)!=request.expected_revision {return Err(err(BudgetErrorKind::RevisionConflict));}
            let boundary=if let Some(old)=old {old.boundary} else {
                for id in call_ids(&tx)? {let call=crate::network_authorization::receipt(&tx,&id)?;if call.subject.project_id.as_ref()==Some(&request.project_id)&&call.purpose==NetworkPurpose::Completion&&call.state==CallState::Claimed{return Err(err(BudgetErrorKind::Busy));}}
                tx.query_row("SELECT COALESCE(MAX(rowid),0) FROM network_calls",[],|r|r.get::<_,u64>(0))?
            };
            let result=BudgetMutationResult{snapshot:BudgetConfigSnapshot{project_id:request.project_id.clone(),revision:plus(request.expected_revision,1)?,config:Some(request.config)},replayed:false};
            tx.execute("INSERT INTO project_budgets VALUES(?1,?2,?3,?4,?5) ON CONFLICT(project_id) DO UPDATE SET revision=excluded.revision,limit_tokens=excluded.limit_tokens,reservation_tokens=excluded.reservation_tokens",params![request.project_id.0,result.snapshot.revision,request.config.limit_tokens,request.config.per_call_reservation_tokens,boundary])?;
            tx.execute("INSERT INTO budget_requests VALUES(?1,?2,?3,?4,?5)",params![request.idempotency_key,request.project_id.0,serde_json::to_string(&request)?,serde_json::to_string(&result)?,crate::workspace_changes::digest(&serde_json::to_vec(&request)?)])?;
            tx.commit()?;Ok(result)
        })().map_err(safe_error)
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
    let status = status_tx(db, project)?;
    if let Some(reason) = status.blocked_reason {
        return Err(err(match reason {
            BudgetBlockedReason::Unresolved => BudgetErrorKind::Unresolved,
            BudgetBlockedReason::Overrun => BudgetErrorKind::Overrun,
            BudgetBlockedReason::Exceeded => BudgetErrorKind::Exceeded,
        }));
    }
    Ok(())
}
pub(crate) fn next_network_rowid(db: &Connection) -> AnyResult<u64> {
    let high:u64=db.query_row("SELECT MAX(COALESCE((SELECT MAX(rowid) FROM network_calls),0),COALESCE((SELECT MAX(coverage_start_call_rowid) FROM project_budgets),0))",[],|r|r.get(0))?;
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
    verify_structure(db)?;
    let current = crate::network_authorization::receipt(db, &candidate.call_id)?;
    let Some(mut entry) = checked_call(db, current.clone())? else {
        return Ok(None);
    };
    let mut identity = candidate.clone();
    identity.state = current.state;
    identity.finished_at = current.finished_at;
    identity.success = current.success;
    identity.http_status = current.http_status;
    identity.result = current.result;
    if identity != current {
        return Err(corrupt());
    }
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
                if u.total_tokens > entry.reserved_tokens.unwrap() {
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
    let rowid: u64 = db.query_row(
        "SELECT rowid FROM network_calls WHERE id=?1",
        [&entry.call_id],
        |r| r.get(0),
    )?;
    validate_pair(
        db,
        &entry,
        candidate,
        rowid,
        &policy(db, &entry.project_id)?.ok_or_else(corrupt)?,
    )?;
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
        network_authorization::{
            ConfirmRequest, DispatchState, NetworkErrorKind, NetworkSubjectKind,
        },
        project_config::{ConfigureProject, ProjectConfig, RegisterProject},
        provider_transport::TransportError,
    };
    use serde_json::{Value, json};
    use std::sync::{Arc, Barrier};
    fn fixture() -> (tempfile::TempDir, Arc<Store>, NetworkSubject) {
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
            project_id: Some(project),
        };
        let p = store.network_preview(subject.clone()).unwrap();
        store
            .network_confirm(
                subject.clone(),
                ConfirmRequest {
                    config_revision: p.config_revision,
                    credential_revision: p.credential_revision,
                    binding_digest: p.binding_digest,
                    purposes: vec![NetworkPurpose::Models, NetworkPurpose::Completion],
                },
                "grant-budget",
            )
            .unwrap();
        (dir, store, subject)
    }
    fn configure(
        store: &Store,
        subject: &NetworkSubject,
        limit: u64,
        reserve: u64,
    ) -> BudgetMutationResult {
        let project = subject.project_id.clone().unwrap();
        let revision = store.budget_config(&project).unwrap().revision;
        store
            .configure_budget(ConfigureBudget {
                project_id: project,
                expected_revision: revision,
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                config: BudgetConfig {
                    limit_tokens: limit,
                    per_call_reservation_tokens: reserve,
                },
            })
            .unwrap()
    }
    fn claim(store: &Store, subject: &NetworkSubject) -> CallReceipt {
        store
            .network_admit(subject, NetworkPurpose::Completion, None)
            .unwrap()
            .1
    }
    fn usage(n: u64) -> Value {
        json!({"prompt_tokens":n,"completion_tokens":0,"total_tokens":n})
    }
    fn complete(
        store: &Store,
        call: CallReceipt,
        n: u64,
    ) -> crate::network_authorization::NetworkResponse<(Value, Value)> {
        store
            .finish_completion(
                call,
                Ok((json!({"role":"assistant","content":"ok"}), usage(n))),
            )
            .unwrap()
    }
    #[test]
    fn configuration_forward_coverage_cas_and_historical_unknown() {
        let (_dir, store, subject) = fixture();
        let project = subject.project_id.as_ref().unwrap();
        assert_eq!(
            store.budget_status(project).unwrap().mode,
            BudgetMode::Unconfigured
        );
        let old = claim(&store, &subject);
        let request = ConfigureBudget {
            project_id: project.clone(),
            expected_revision: 0,
            idempotency_key: "first-budget".into(),
            config: BudgetConfig {
                limit_tokens: 200,
                per_call_reservation_tokens: 100,
            },
        };
        assert_eq!(
            store.configure_budget(request.clone()).unwrap_err().kind,
            BudgetErrorKind::Busy
        );
        store.recover().unwrap();
        assert!(!store.budget_call(project, &old.call_id).unwrap().managed);
        let first = store.configure_budget(request.clone()).unwrap();
        assert_eq!(first.snapshot.revision, 1);
        let boundary = store
            .budget_status(project)
            .unwrap()
            .coverage_start_call_rowid;
        configure(&store, &subject, 0, 100);
        let replay = store.configure_budget(request.clone()).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.snapshot, first.snapshot);
        assert_eq!(
            store
                .budget_status(project)
                .unwrap()
                .coverage_start_call_rowid,
            boundary
        );
        let mut changed = request;
        changed.config.limit_tokens = 1;
        assert_eq!(
            store.configure_budget(changed).unwrap_err().kind,
            BudgetErrorKind::Conflict
        );
        assert_eq!(
            store
                .network_admit(&subject, NetworkPurpose::Completion, None)
                .err()
                .unwrap()
                .kind,
            NetworkErrorKind::BudgetExceeded
        );
        assert!(!store.budget_call(project, &old.call_id).unwrap().managed);
    }
    #[test]
    fn competing_stores_atomically_reserve_last_allowance() {
        let (dir, store, subject) = fixture();
        configure(&store, &subject, 100, 100);
        let second = Arc::new(Store::open(&dir.path().join("budget.db")).unwrap());
        let gate = Arc::new(Barrier::new(3));
        let handles: Vec<_> = [store.clone(), second]
            .into_iter()
            .map(|s| {
                let subject = subject.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    gate.wait();
                    s.network_admit(&subject, NetworkPurpose::Completion, None)
                        .map(|(_, r)| r)
                })
            })
            .collect();
        gate.wait();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results.iter().find_map(|r| r.as_ref().err()).unwrap().kind,
            NetworkErrorKind::BudgetExceeded
        );
        let status = store
            .budget_status(subject.project_id.as_ref().unwrap())
            .unwrap();
        assert_eq!((status.claimed_count, status.held_tokens), (1, 100));
        let db = store.db.lock().unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM network_calls", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM budget_calls", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn exact_settlement_replay_conflict_and_overrun_preserve_first_fact() {
        let (_dir, store, subject) = fixture();
        let project = subject.project_id.as_ref().unwrap();
        configure(&store, &subject, 1000, 100);
        let call = claim(&store, &subject);
        let first = complete(&store, call.clone(), 40);
        let again = complete(&store, call.clone(), 40);
        assert_eq!(first.calls, again.calls);
        let conflict = store
            .finish_completion(call, Ok((json!({}), usage(41))))
            .unwrap_err();
        assert_eq!(conflict.kind, NetworkErrorKind::BudgetConflict);
        assert_eq!(conflict.dispatch_state, DispatchState::Finished);
        assert_eq!(conflict.calls, first.calls);
        let status = store.budget_status(project).unwrap();
        assert_eq!((status.spent_tokens, status.held_tokens), (40, 0));
        let second = claim(&store, &subject);
        complete(&store, second, 101);
        let status = store.budget_status(project).unwrap();
        assert_eq!((status.spent_tokens, status.overrun_count), (141, 1));
        assert_eq!(status.blocked_reason, Some(BudgetBlockedReason::Overrun));
        configure(&store, &subject, 10000, 1000);
        assert_eq!(
            store
                .network_admit(&subject, NetworkPurpose::Completion, None)
                .err()
                .unwrap()
                .kind,
            NetworkErrorKind::BudgetOverrun
        );
    }
    #[test]
    fn missing_invalid_and_transport_failures_hold_reservation() {
        for (label, data, expected) in [
            ("missing", Value::Null, BudgetCallState::UsageMissing),
            (
                "invalid",
                json!({"total_tokens":1}),
                BudgetCallState::UsageInvalid,
            ),
        ] {
            let (_dir, store, subject) = fixture();
            configure(&store, &subject, 1000, 100);
            let call = claim(&store, &subject);
            let response = store
                .finish_completion(call.clone(), Ok((json!({"content":label}), data)))
                .unwrap();
            assert_eq!(response.result.0["content"], label);
            let result = store
                .budget_call(subject.project_id.as_ref().unwrap(), &call.call_id)
                .unwrap();
            assert_eq!(result.state, Some(expected));
            let status = store
                .budget_status(subject.project_id.as_ref().unwrap())
                .unwrap();
            assert_eq!((status.held_tokens, status.unresolved_count), (100, 1));
            configure(&store, &subject, 10000, 1);
            assert_eq!(
                store
                    .network_admit(&subject, NetworkPurpose::Completion, None)
                    .err()
                    .unwrap()
                    .kind,
                NetworkErrorKind::BudgetUnresolved
            );
        }
        for error in [
            TransportError::Unknown,
            TransportError::Response { status: 429 },
            TransportError::InvalidResponse { status: 200 },
        ] {
            let (_dir, store, subject) = fixture();
            configure(&store, &subject, 1000, 100);
            let call = claim(&store, &subject);
            assert!(store.finish_completion(call.clone(), Err(error)).is_err());
            let status = store
                .budget_status(subject.project_id.as_ref().unwrap())
                .unwrap();
            assert_eq!((status.held_tokens, status.unresolved_count), (100, 1));
            assert_eq!(
                store
                    .budget_call(subject.project_id.as_ref().unwrap(), &call.call_id)
                    .unwrap()
                    .state,
                Some(BudgetCallState::Unknown)
            );
        }
    }
    #[test]
    fn not_sent_refunds_but_other_inflight_calls_can_settle_after_block() {
        let (_dir, store, subject) = fixture();
        let project = subject.project_id.as_ref().unwrap();
        configure(&store, &subject, 300, 100);
        let a = claim(&store, &subject);
        let b = claim(&store, &subject);
        let c = claim(&store, &subject);
        store
            .finish_completion(a, Err(TransportError::Unknown))
            .unwrap_err();
        complete(&store, b, 50);
        let error = store
            .finish_completion(c, Err(TransportError::Invalid))
            .unwrap_err();
        assert_eq!(error.dispatch_state, DispatchState::NotSent);
        let status = store.budget_status(project).unwrap();
        assert_eq!(
            (
                status.spent_tokens,
                status.held_tokens,
                status.claimed_count,
                status.unresolved_count
            ),
            (50, 100, 0, 1)
        );
    }
    #[test]
    fn network_and_budget_failures_roll_back_together() {
        let (_dir, store, subject) = fixture();
        let project = subject.project_id.as_ref().unwrap();
        configure(&store, &subject, 1000, 100);
        store.db.lock().unwrap().execute_batch("CREATE TRIGGER reject_budget_claim BEFORE INSERT ON budget_calls BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
        assert!(
            store
                .network_admit(&subject, NetworkPurpose::Completion, None)
                .is_err()
        );
        assert_eq!(
            store
                .db
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM network_calls", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
        store
            .db
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_budget_claim")
            .unwrap();
        let call = claim(&store, &subject);
        store.db.lock().unwrap().execute_batch("CREATE TRIGGER reject_network_finish BEFORE UPDATE ON network_calls BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
        let error = store
            .finish_completion(call.clone(), Ok((json!({}), usage(20))))
            .unwrap_err();
        assert_eq!(error.dispatch_state, DispatchState::Unknown);
        let status = store.budget_status(project).unwrap();
        assert_eq!(
            (
                status.held_tokens,
                status.spent_tokens,
                status.claimed_count
            ),
            (100, 0, 1)
        );
        assert_eq!(
            store.budget_call(project, &call.call_id).unwrap().state,
            Some(BudgetCallState::Claimed)
        );
        store
            .db
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_network_finish")
            .unwrap();
        store.recover().unwrap();
        let once = store.budget_call(project, &call.call_id).unwrap();
        store.recover().unwrap();
        assert_eq!(store.budget_call(project, &call.call_id).unwrap(), once);
    }
    #[test]
    fn missing_covered_row_and_single_sided_terminal_fail_closed_on_recovery() {
        for delete in [false, true] {
            let (dir, store, subject) = fixture();
            configure(&store, &subject, 1000, 100);
            let call = claim(&store, &subject);
            if delete {
                store
                    .db
                    .lock()
                    .unwrap()
                    .execute("DELETE FROM budget_calls WHERE call_id=?1", [&call.call_id])
                    .unwrap();
            } else {
                let mut finished = call.clone();
                finished.state = CallState::Finished;
                finished.finished_at = Some(finished.claimed_at);
                finished.success = Some(true);
                finished.result = Some(CallResult::Success);
                store
                    .db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE network_calls SET state='finished',receipt=?2 WHERE id=?1",
                        params![call.call_id, serde_json::to_string(&finished).unwrap()],
                    )
                    .unwrap();
            }
            assert_eq!(
                store
                    .budget_status(subject.project_id.as_ref().unwrap())
                    .unwrap_err()
                    .kind,
                BudgetErrorKind::Corrupt
            );
            assert!(store.recover().is_err());
            assert!(Store::open(&dir.path().join("budget.db")).is_err());
        }
    }
    #[test]
    fn request_sql_scope_and_policy_history_are_verified() {
        for column in ["project_id", "reservation_tokens", "budget_revision"] {
            let (dir, store, subject) = fixture();
            configure(&store, &subject, 1000, 100);
            let call = claim(&store, &subject);
            if column == "project_id" {
                let other = store.register_project(RegisterProject {
                    name: "other".into(),
                    root_path: {
                        let p = dir.path().join("sibling-root");
                        std::fs::create_dir(&p).unwrap();
                        p.to_string_lossy().into()
                    },
                    idempotency_key: "other-root".into(),
                });
                assert!(other.is_err());
                store.db.lock().unwrap().execute_batch("PRAGMA foreign_keys=OFF; UPDATE budget_requests SET project_id='wrong'; PRAGMA foreign_keys=ON").unwrap();
            } else if column == "reservation_tokens" {
                store
                    .db
                    .lock()
                    .unwrap()
                    .execute("UPDATE project_budgets SET reservation_tokens=99", [])
                    .unwrap();
            } else {
                let mut entry = store
                    .budget_call(subject.project_id.as_ref().unwrap(), &call.call_id)
                    .unwrap();
                entry.reserved_tokens = Some(99);
                store
                    .db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE budget_calls SET reserved_tokens=99,record=?1",
                        [serde_json::to_string(&entry).unwrap()],
                    )
                    .unwrap();
            }
            assert_eq!(
                store
                    .budget_status(subject.project_id.as_ref().unwrap())
                    .unwrap_err()
                    .kind,
                BudgetErrorKind::Corrupt
            );
            assert!(Store::open(&dir.path().join("budget.db")).is_err());
        }
    }
    #[test]
    fn aggregate_overflow_blocks_reads_open_recover_but_retains_inflight_actuals() {
        let (dir, store, subject) = fixture();
        configure(&store, &subject, 300, 100);
        let calls: Vec<_> = (0..3).map(|_| claim(&store, &subject)).collect();
        complete(&store, calls[0].clone(), i64::MAX as u64);
        complete(&store, calls[1].clone(), 1);
        assert_eq!(
            store
                .budget_status(subject.project_id.as_ref().unwrap())
                .unwrap_err()
                .kind,
            BudgetErrorKind::Corrupt
        );
        assert!(Store::open(&dir.path().join("budget.db")).is_err());
        assert!(store.recover().is_err());
        complete(&store, calls[2].clone(), 2);
        let db = store.db.lock().unwrap();
        verify_structure(&db).unwrap();
        assert!(verify_persisted_rows(&db).is_err());
        for (i, n) in [i64::MAX as u64, 1, 2].into_iter().enumerate() {
            let entry = load_entry(&db, &calls[i].call_id).unwrap().unwrap();
            assert_eq!(entry.usage.unwrap().total_tokens, n);
            assert_eq!(entry.call.state, CallState::Finished);
        }
    }
    #[test]
    fn request_digest_and_coverage_high_water_corruption_are_rejected() {
        for sql in [
            "UPDATE budget_requests SET request_digest=printf('%064d',0)",
            "UPDATE project_budgets SET coverage_start_call_rowid=900",
        ] {
            let (dir, store, subject) = fixture();
            configure(&store, &subject, 100, 100);
            store.db.lock().unwrap().execute_batch(sql).unwrap();
            assert_eq!(
                store
                    .budget_status(subject.project_id.as_ref().unwrap())
                    .unwrap_err()
                    .kind,
                BudgetErrorKind::Corrupt
            );
            assert!(Store::open(&dir.path().join("budget.db")).is_err());
        }
    }
}
