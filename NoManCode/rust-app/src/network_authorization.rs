//! Safe network authorization and dispatch facts. Credentials never appear in these DTOs.
pub use crate::provider_transport::NetworkPurpose;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkSubjectKind {
    Route,
    Account,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSubject {
    pub kind: NetworkSubjectKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<crate::domain::ProjectId>,
}
impl NetworkSubject {
    pub(crate) fn key(&self) -> Result<String, NetworkError> {
        match self.kind {
            NetworkSubjectKind::Route
                if crate::domain::valid_name(&self.id) && self.id != "newapi-account" =>
            {
                match &self.project_id {
                    Some(project) if crate::domain::valid_name(&project.0) => {
                        Ok(format!("project-route:{}:{}", project.0, self.id))
                    }
                    Some(_) => Err(NetworkError::not_sent(NetworkErrorKind::Invalid)),
                    None => Ok(format!("route:{}", self.id)),
                }
            }
            NetworkSubjectKind::Account if self.id == "newapi" && self.project_id.is_none() => {
                Ok("account:newapi".into())
            }
            _ => Err(NetworkError::not_sent(NetworkErrorKind::Invalid)),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmRequest {
    pub config_revision: u64,
    pub credential_revision: u64,
    pub binding_digest: String,
    pub purposes: Vec<NetworkPurpose>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationState {
    Active,
    Revoked,
    Stale,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkAuthorization {
    pub authorization_id: String,
    pub subject: NetworkSubject,
    pub config_revision: u64,
    pub credential_revision: u64,
    pub binding_digest: String,
    pub purposes: Vec<NetworkPurpose>,
    pub state: AuthorizationState,
    pub created_at: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPreview {
    pub subject: NetworkSubject,
    pub config_revision: u64,
    pub credential_revision: u64,
    pub origin: String,
    pub base_url: String,
    pub credential_ref: String,
    pub binding_digest: String,
    pub purposes: Vec<NetworkPurpose>,
    pub authorization: Option<NetworkAuthorization>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkDecision {
    pub authorization: NetworkAuthorization,
    pub replayed: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    Claimed,
    Finished,
    Unknown,
    NotSent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallResult {
    Success,
    HttpFailure,
    InvalidResponse,
    TransportUnknown,
    Interrupted,
    PersistenceUnknown,
    PreflightRejected,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallReceipt {
    pub call_id: String,
    pub authorization_id: String,
    pub subject: NetworkSubject,
    pub task_id: Option<String>,
    pub purpose: NetworkPurpose,
    pub method: String,
    pub origin: String,
    pub endpoint_digest: String,
    pub state: CallState,
    pub claimed_at: u64,
    pub finished_at: Option<u64>,
    pub success: Option<bool>,
    pub http_status: Option<u16>,
    pub result: Option<CallResult>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkResponse<T> {
    pub result: T,
    pub calls: Vec<CallReceipt>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkErrorKind {
    ProjectInvalid,
    ProjectNotFound,
    ProjectRevisionConflict,
    ProjectDuplicateRoot,
    ProjectOverlappingRoot,
    ProjectSelectionRequired,
    ProjectLegacyUnbound,
    ProjectRootChanged,
    ProjectRootUnavailable,
    ProjectPermissionRevoked,
    ProjectOperationUnsupported,
    ProjectCorrupt,
    ProjectInternal,
    WorkspaceSelectionManaged,
    BudgetInvalid,
    BudgetNotFound,
    BudgetRevisionConflict,
    BudgetConflict,
    BudgetBusy,
    BudgetExceeded,
    BudgetUnresolved,
    BudgetOverrun,
    BudgetCorrupt,
    BudgetInternal,
    Invalid,
    NotFound,
    Required,
    Stale,
    Revoked,
    Conflict,
    Corrupt,
    Unknown,
    Internal,
    Upstream,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    NotSent,
    Finished,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkError {
    pub kind: NetworkErrorKind,
    pub dispatch_state: DispatchState,
    pub calls: Vec<CallReceipt>,
}
impl NetworkError {
    pub fn not_sent(kind: NetworkErrorKind) -> Self {
        Self {
            kind,
            dispatch_state: DispatchState::NotSent,
            calls: vec![],
        }
    }
}
impl NetworkErrorKind {
    pub fn budget_code(&self) -> Option<&'static str> {
        use crate::budget::BudgetErrorKind as B;
        Some(
            match self {
                Self::BudgetInvalid => B::Invalid,
                Self::BudgetNotFound => B::NotFound,
                Self::BudgetRevisionConflict => B::RevisionConflict,
                Self::BudgetConflict => B::Conflict,
                Self::BudgetBusy => B::Busy,
                Self::BudgetExceeded => B::Exceeded,
                Self::BudgetUnresolved => B::Unresolved,
                Self::BudgetOverrun => B::Overrun,
                Self::BudgetCorrupt => B::Corrupt,
                Self::BudgetInternal => B::Internal,
                _ => return None,
            }
            .code(),
        )
    }

    pub fn project_code(&self) -> Option<&'static str> {
        use crate::project_config::ProjectError;
        Some(match self {
            Self::ProjectInvalid => ProjectError::Invalid.code(),
            Self::ProjectNotFound => ProjectError::NotFound.code(),
            Self::ProjectRevisionConflict => ProjectError::RevisionConflict.code(),
            Self::ProjectDuplicateRoot => ProjectError::DuplicateRoot.code(),
            Self::ProjectOverlappingRoot => ProjectError::OverlappingRoot.code(),
            Self::ProjectSelectionRequired => ProjectError::SelectionRequired.code(),
            Self::ProjectLegacyUnbound => ProjectError::LegacyUnbound.code(),
            Self::ProjectRootChanged => ProjectError::RootChanged.code(),
            Self::ProjectRootUnavailable => ProjectError::RootUnavailable.code(),
            Self::ProjectPermissionRevoked => ProjectError::PermissionRevoked.code(),
            Self::ProjectOperationUnsupported => ProjectError::OperationUnsupported.code(),
            Self::ProjectCorrupt => ProjectError::Corrupt.code(),
            Self::ProjectInternal => ProjectError::Internal.code(),
            Self::WorkspaceSelectionManaged => ProjectError::WorkspaceSelectionManaged.code(),
            _ => return None,
        })
    }
}
impl From<crate::project_config::ProjectError> for NetworkError {
    fn from(error: crate::project_config::ProjectError) -> Self {
        use crate::project_config::ProjectError;
        Self::not_sent(match error {
            ProjectError::Invalid => NetworkErrorKind::ProjectInvalid,
            ProjectError::NotFound => NetworkErrorKind::ProjectNotFound,
            ProjectError::RevisionConflict => NetworkErrorKind::ProjectRevisionConflict,
            ProjectError::DuplicateRoot => NetworkErrorKind::ProjectDuplicateRoot,
            ProjectError::OverlappingRoot => NetworkErrorKind::ProjectOverlappingRoot,
            ProjectError::SelectionRequired => NetworkErrorKind::ProjectSelectionRequired,
            ProjectError::LegacyUnbound => NetworkErrorKind::ProjectLegacyUnbound,
            ProjectError::RootChanged => NetworkErrorKind::ProjectRootChanged,
            ProjectError::RootUnavailable => NetworkErrorKind::ProjectRootUnavailable,
            ProjectError::PermissionRevoked => NetworkErrorKind::ProjectPermissionRevoked,
            ProjectError::OperationUnsupported => NetworkErrorKind::ProjectOperationUnsupported,
            ProjectError::Corrupt => NetworkErrorKind::ProjectCorrupt,
            ProjectError::Internal => NetworkErrorKind::ProjectInternal,
            ProjectError::WorkspaceSelectionManaged => NetworkErrorKind::WorkspaceSelectionManaged,
        })
    }
}
impl From<crate::budget::BudgetError> for NetworkError {
    fn from(error: crate::budget::BudgetError) -> Self {
        use crate::budget::BudgetErrorKind as B;
        Self::not_sent(match error.kind {
            B::Invalid => NetworkErrorKind::BudgetInvalid,
            B::NotFound => NetworkErrorKind::BudgetNotFound,
            B::RevisionConflict => NetworkErrorKind::BudgetRevisionConflict,
            B::Conflict => NetworkErrorKind::BudgetConflict,
            B::Busy => NetworkErrorKind::BudgetBusy,
            B::Exceeded => NetworkErrorKind::BudgetExceeded,
            B::Unresolved => NetworkErrorKind::BudgetUnresolved,
            B::Overrun => NetworkErrorKind::BudgetOverrun,
            B::Corrupt => NetworkErrorKind::BudgetCorrupt,
            B::Internal => NetworkErrorKind::BudgetInternal,
        })
    }
}
impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(code) = self.kind.budget_code().or(self.kind.project_code()) {
            return f.write_str(code);
        }
        f.write_str(match self.kind {
            NetworkErrorKind::Invalid => "网络请求无效",
            NetworkErrorKind::NotFound => "网络对象不存在",
            NetworkErrorKind::Required => "需要明确网络授权",
            NetworkErrorKind::Stale => "网络授权绑定已变化",
            NetworkErrorKind::Revoked => "网络授权已撤销",
            NetworkErrorKind::Conflict => "网络授权状态冲突",
            NetworkErrorKind::Corrupt => "网络授权持久状态损坏",
            NetworkErrorKind::Unknown => "网络调用结果未知",
            NetworkErrorKind::Internal => "网络授权操作失败",
            NetworkErrorKind::Upstream => "上游返回失败响应",
            _ => "项目网络请求被拒绝",
        })
    }
}
impl std::error::Error for NetworkError {}

use crate::{
    domain::{Settings, now},
    provider_transport::{validate_permit_inputs, validate_target},
    secrets,
    store::Store,
    workspace_changes::digest,
};
use anyhow::Result as AnyResult;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

pub(crate) fn safe_error(error: anyhow::Error) -> NetworkError {
    if let Some(budget) = error.downcast_ref::<crate::budget::BudgetError>() {
        return (*budget).into();
    }
    error
        .downcast_ref::<NetworkError>()
        .cloned()
        .unwrap_or_else(|| {
            error
                .downcast_ref::<crate::project_config::ProjectError>()
                .copied()
                .map(Into::into)
                .unwrap_or_else(|| NetworkError::not_sent(NetworkErrorKind::Internal))
        })
}
fn failure(kind: NetworkErrorKind) -> anyhow::Error {
    NetworkError::not_sent(kind).into()
}
fn json<T: serde::de::DeserializeOwned>(text: &str) -> AnyResult<T> {
    serde_json::from_str(text).map_err(|_| failure(NetworkErrorKind::Corrupt))
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubjectSnapshot {
    subject: NetworkSubject,
    config_revision: u64,
    credential_revision: u64,
    origin: String,
    base_url: String,
    identity: String,
    credential_ref: String,
    binding_digest: String,
}
struct CurrentSubject {
    snapshot: SubjectSnapshot,
    credential: String,
}
fn read_secret(db: &Connection, id: &str) -> AnyResult<Option<String>> {
    let value: Option<Vec<u8>> = db
        .query_row("SELECT value FROM secrets WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    value
        .map(|v| {
            String::from_utf8(
                secrets::open(&v)
                    .map_err(|_| crate::host_configuration::HostConfigError::Unavailable)?,
            )
            .map_err(Into::into)
        })
        .transpose()
}
fn current_subject(db: &Connection, subject: &NetworkSubject) -> AnyResult<CurrentSubject> {
    subject.key()?;
    if subject.kind == NetworkSubjectKind::Route {
        let project = subject
            .project_id
            .as_ref()
            .ok_or(crate::project_config::ProjectError::SelectionRequired)?;
        let config = crate::project_config::config_tx(db, project)?;
        crate::project_config::workspace_tx(db, project)?;
        if !config.config.allowed_route_ids.contains(&subject.id) {
            return Err(crate::project_config::ProjectError::PermissionRevoked.into());
        }
    }
    let value: Option<String> = db
        .query_row("SELECT value FROM config WHERE id='settings'", [], |r| {
            r.get(0)
        })
        .optional()?;
    let settings: Settings = json(&value.ok_or_else(|| failure(NetworkErrorKind::Required))?)?;
    let (base, identity, credential_ref, credential, purpose) = match subject.kind {
        NetworkSubjectKind::Route => {
            let route = settings
                .routes
                .iter()
                .find(|r| r.id == subject.id)
                .ok_or_else(|| failure(NetworkErrorKind::NotFound))?;
            let env = route.key_env.as_ref().and_then(|name| {
                std::env::var(name)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .map(|value| (format!("env/{name}"), value))
            });
            let selected = if let Some(env) = env {
                Some(env)
            } else if let Some(key) =
                read_secret(db, &format!("route::{}", route.id))?.filter(|v| !v.trim().is_empty())
            {
                Some((format!("stored/route/{}", route.id), key))
            } else {
                read_secret(db, &route.id)?
                    .filter(|v| !v.trim().is_empty())
                    .map(|key| (format!("stored/legacy/{}", route.id), key))
            };
            let (reference, key) = selected.ok_or_else(|| failure(NetworkErrorKind::Required))?;
            (
                route.base_url.clone(),
                route.model.clone(),
                reference,
                key,
                NetworkPurpose::Completion,
            )
        }
        NetworkSubjectKind::Account => {
            let account = settings
                .newapi
                .ok_or_else(|| failure(NetworkErrorKind::NotFound))?;
            let key = read_secret(db, "newapi-account")?
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| failure(NetworkErrorKind::Required))?;
            (
                account.base_url,
                account.user_id,
                "stored/account/newapi".into(),
                key,
                NetworkPurpose::Balance,
            )
        }
    };
    let target = validate_target(&base, purpose).map_err(|_| failure(NetworkErrorKind::Invalid))?;
    let (model, user) = if subject.kind == NetworkSubjectKind::Route {
        (Some(identity.as_str()), None)
    } else {
        (None, Some(identity.as_str()))
    };
    validate_permit_inputs(&target, &credential, "preflight", model, user)
        .map_err(|_| failure(NetworkErrorKind::Invalid))?;
    if credential.len() > 4096
        || secrets::safe_metadata_text("credential_ref", &credential_ref).is_err()
        || secrets::safe_metadata_path("base_url", target.base_url()).is_err()
        || secrets::safe_metadata_text("identity", &identity).is_err()
    {
        return Err(failure(NetworkErrorKind::Invalid));
    }
    Ok(CurrentSubject {
        snapshot: SubjectSnapshot {
            subject: subject.clone(),
            config_revision: 0,
            credential_revision: 0,
            origin: target.origin().into(),
            base_url: target.base_url().into(),
            identity,
            credential_ref,
            binding_digest: String::new(),
        },
        credential,
    })
}
fn stored_subject(
    db: &Connection,
    subject: &NetworkSubject,
) -> AnyResult<Option<(SubjectSnapshot, Vec<u8>)>> {
    let row: Option<(String, Vec<u8>)> = db
        .query_row(
            "SELECT snapshot,credential_blob FROM network_subjects WHERE subject=?1",
            [subject.key()?],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(s, b)| {
        let snapshot = json::<SubjectSnapshot>(&s)?;
        if snapshot.subject != *subject
            || snapshot.config_revision == 0
            || snapshot.credential_revision == 0
            || snapshot.binding_digest != snapshot_digest(&snapshot)?
        {
            return Err(failure(NetworkErrorKind::Corrupt));
        }
        Ok((snapshot, b))
    })
    .transpose()
}
fn same_config(a: &SubjectSnapshot, b: &SubjectSnapshot) -> bool {
    a.subject == b.subject
        && a.origin == b.origin
        && a.base_url == b.base_url
        && a.identity == b.identity
        && a.credential_ref == b.credential_ref
}
fn matches_current(
    snapshot: &SubjectSnapshot,
    sealed: &[u8],
    current: &CurrentSubject,
) -> AnyResult<bool> {
    Ok(same_config(snapshot, &current.snapshot)
        && secrets::open(sealed)
            .map_err(|_| crate::host_configuration::HostConfigError::Unavailable)?
            == current.credential.as_bytes())
}
fn snapshot_digest(snapshot: &SubjectSnapshot) -> AnyResult<String> {
    Ok(digest(&serde_json::to_vec(
        &json!({"version":if snapshot.subject.project_id.is_some() {2} else {1},"subject":snapshot.subject,"config_revision":snapshot.config_revision,"credential_revision":snapshot.credential_revision,"origin":snapshot.origin,"base_url":snapshot.base_url,"identity":snapshot.identity,"credential_ref":snapshot.credential_ref}),
    )?))
}
fn refresh_subject(db: &Connection, current: CurrentSubject) -> AnyResult<SubjectSnapshot> {
    let key = current.snapshot.subject.key()?;
    let old = stored_subject(db, &current.snapshot.subject)?;
    if let Some((snapshot, sealed)) = &old
        && matches_current(snapshot, sealed, &current)?
    {
        return Ok(snapshot.clone());
    }
    let mut snapshot = current.snapshot;
    snapshot.config_revision = if let Some((old, _)) = &old {
        old.config_revision
            .checked_add(u64::from(!same_config(old, &snapshot)))
            .ok_or_else(|| failure(NetworkErrorKind::Corrupt))?
    } else {
        1
    };
    snapshot.credential_revision = if let Some((old, _)) = &old {
        old.credential_revision
            .checked_add(1)
            .ok_or_else(|| failure(NetworkErrorKind::Corrupt))?
    } else {
        1
    };
    snapshot.binding_digest = snapshot_digest(&snapshot)?;
    db.execute(
        "UPDATE network_authorizations SET state='stale' WHERE subject=?1 AND state='active'",
        [&key],
    )?;
    db.execute("INSERT INTO network_subjects(subject,snapshot,credential_blob) VALUES (?1,?2,?3) ON CONFLICT(subject) DO UPDATE SET snapshot=excluded.snapshot,credential_blob=excluded.credential_blob",params![key,serde_json::to_string(&snapshot)?,secrets::seal(current.credential.as_bytes()).map_err(|_| crate::host_configuration::HostConfigError::Unavailable)?])?;
    Ok(snapshot)
}
fn allowed(subject: &NetworkSubject) -> Vec<NetworkPurpose> {
    match subject.kind {
        NetworkSubjectKind::Route => vec![NetworkPurpose::Models, NetworkPurpose::Completion],
        NetworkSubjectKind::Account => vec![NetworkPurpose::Balance],
    }
}
fn authorization(db: &Connection, id: &str) -> AnyResult<NetworkAuthorization> {
    let row:Option<(String,u64,u64,String,String,String,u64)>=db.query_row("SELECT subject,config_revision,credential_revision,binding_digest,purposes,state,created_at FROM network_authorizations WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let (key, config_revision, credential_revision, binding_digest, purposes, state, created_at) =
        row.ok_or_else(|| failure(NetworkErrorKind::NotFound))?;
    let subject = if let Some(id) = key.strip_prefix("route:") {
        NetworkSubject {
            kind: NetworkSubjectKind::Route,
            id: id.into(),
            project_id: None,
        }
    } else if let Some(rest) = key.strip_prefix("project-route:") {
        let (project, id) = rest
            .split_once(':')
            .ok_or_else(|| failure(NetworkErrorKind::Corrupt))?;
        NetworkSubject {
            kind: NetworkSubjectKind::Route,
            id: id.into(),
            project_id: Some(crate::domain::ProjectId(project.into())),
        }
    } else if key == "account:newapi" {
        NetworkSubject {
            kind: NetworkSubjectKind::Account,
            id: "newapi".into(),
            project_id: None,
        }
    } else {
        return Err(failure(NetworkErrorKind::Corrupt));
    };
    let purposes: Vec<NetworkPurpose> = json(&purposes)?;
    if uuid::Uuid::parse_str(id).is_err()
        || config_revision == 0
        || credential_revision == 0
        || !crate::workspace_changes::valid_digest(&binding_digest)
        || purposes.is_empty()
        || purposes.windows(2).any(|w| w[0] >= w[1])
        || purposes.iter().any(|p| !allowed(&subject).contains(p))
    {
        return Err(failure(NetworkErrorKind::Corrupt));
    }
    let state = match state.as_str() {
        "active" => AuthorizationState::Active,
        "revoked" => AuthorizationState::Revoked,
        "stale" => AuthorizationState::Stale,
        _ => return Err(failure(NetworkErrorKind::Corrupt)),
    };
    Ok(NetworkAuthorization {
        authorization_id: id.into(),
        subject,
        config_revision,
        credential_revision,
        binding_digest,
        purposes,
        state,
        created_at,
    })
}
fn active_authorization(
    db: &Connection,
    subject: &NetworkSubject,
) -> AnyResult<Option<NetworkAuthorization>> {
    let id: Option<String> = db
        .query_row(
            "SELECT id FROM network_authorizations WHERE subject=?1 AND state='active'",
            [subject.key()?],
            |r| r.get(0),
        )
        .optional()?;
    id.map(|id| authorization(db, &id)).transpose()
}
impl Store {
    pub fn network_preview(&self, subject: NetworkSubject) -> Result<NetworkPreview, NetworkError> {
        (|| -> AnyResult<_> {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let current = current_subject(&tx, &subject)?;
            let snapshot = refresh_subject(&tx, current)?;
            let authorization = active_authorization(&tx, &subject)?;
            tx.commit()?;
            Ok(NetworkPreview {
                subject: subject.clone(),
                config_revision: snapshot.config_revision,
                credential_revision: snapshot.credential_revision,
                origin: snapshot.origin,
                base_url: snapshot.base_url,
                credential_ref: snapshot.credential_ref,
                binding_digest: snapshot.binding_digest,
                purposes: allowed(&subject),
                authorization,
            })
        })()
        .map_err(safe_error)
    }
    pub fn network_confirm(
        &self,
        subject: NetworkSubject,
        mut request: ConfirmRequest,
        key: &str,
    ) -> Result<NetworkDecision, NetworkError> {
        (|| -> AnyResult<_> {
            secrets::validate_idempotency_key(key).map_err(|_|failure(NetworkErrorKind::Invalid))?;
            let subject_key=subject.key()?;
            request.purposes.sort();
            request.purposes.dedup();
            if request.purposes.is_empty() || request.purposes.iter().any(|p|!allowed(&subject).contains(p)) {return Err(failure(NetworkErrorKind::Invalid));}
            let request_digest=digest(&serde_json::to_vec(&request)?);
            let mut db=self.db.lock().unwrap();
            let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let existing:Option<(String,String)>=tx.query_row(
                "SELECT request_digest,authorization_id FROM network_confirmations WHERE subject=?1 AND creation_key=?2",
                params![subject_key,key],|r|Ok((r.get(0)?,r.get(1)?))
            ).optional()?;
            if let Some((hash,id))=existing {
                if hash!=request_digest {return Err(failure(NetworkErrorKind::Conflict));}
                let original=authorization(&tx,&id)?;
                if original.subject!=subject || original.config_revision!=request.config_revision || original.credential_revision!=request.credential_revision || original.binding_digest!=request.binding_digest || original.purposes!=request.purposes {return Err(failure(NetworkErrorKind::Corrupt));}
                return Ok(NetworkDecision{authorization:original,replayed:true});
            }
            let (snapshot,sealed)=stored_subject(&tx,&subject)?.ok_or_else(||failure(NetworkErrorKind::Required))?;
            let current=current_subject(&tx,&subject)?;
            if !matches_current(&snapshot,&sealed,&current)? || request.config_revision!=snapshot.config_revision || request.credential_revision!=snapshot.credential_revision || request.binding_digest!=snapshot.binding_digest {return Err(failure(NetworkErrorKind::Stale));}
            let active=active_authorization(&tx,&subject)?;
            let (id,replayed)=if let Some(active)=active.filter(|a|a.binding_digest==snapshot.binding_digest && a.purposes==request.purposes) {
                (active.authorization_id,true)
            } else {
                tx.execute("UPDATE network_authorizations SET state='stale' WHERE subject=?1 AND state='active'",[&subject_key])?;
                let id=uuid::Uuid::new_v4().to_string();
                tx.execute("INSERT INTO network_authorizations VALUES (?1,?2,?3,?4,?5,?6,'active',?7)",params![id,subject_key,snapshot.config_revision,snapshot.credential_revision,snapshot.binding_digest,serde_json::to_string(&request.purposes)?,now()])?;
                tx.execute("INSERT INTO network_events(subject,kind,object_id,created_at) VALUES (?1,'confirmed',?2,?3)",params![subject_key,id,now()])?;
                (id,false)
            };
            tx.execute("INSERT INTO network_confirmations VALUES (?1,?2,?3,?4)",params![subject_key,key,request_digest,id])?;
            let result=authorization(&tx,&id)?;
            tx.commit()?;
            Ok(NetworkDecision{authorization:result,replayed})
        })().map_err(safe_error)
    }
    pub fn network_authorization(&self, id: &str) -> Result<NetworkAuthorization, NetworkError> {
        authorization(&self.db.lock().unwrap(), id).map_err(safe_error)
    }
    pub fn network_revoke(&self, id: &str) -> Result<NetworkAuthorization, NetworkError> {
        (|| -> AnyResult<_> {
            let mut db=self.db.lock().unwrap();
            let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let current=authorization(&tx,id)?;
            if current.state!=AuthorizationState::Revoked {
                tx.execute("UPDATE network_authorizations SET state='revoked' WHERE id=?1",[id])?;
                tx.execute("INSERT INTO network_events(subject,kind,object_id,created_at) VALUES (?1,'revoked',?2,?3)",params![current.subject.key()?,id,now()])?;
            }
            let result=authorization(&tx,id)?;
            tx.commit()?;
            Ok(result)
        })().map_err(safe_error)
    }
}
fn checked_authorization(
    db: &Connection,
    subject: &NetworkSubject,
    purposes: &[NetworkPurpose],
    task: Option<&crate::domain::Task>,
) -> AnyResult<(NetworkAuthorization, CurrentSubject)> {
    if let Some(task) = task {
        let binding = crate::project_config::validate_task_tx(db, task, true, true)?;
        if subject.project_id.as_ref() != Some(&binding.project_id) {
            return Err(crate::project_config::ProjectError::PermissionRevoked.into());
        }
    }
    let grant = if let Some(task) = task {
        let binding: Option<(String,String)> = db.query_row("SELECT authorization_id,binding_digest FROM task_network_bindings WHERE task_id=?1", [&task.id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (id, binding) = binding.ok_or_else(|| failure(NetworkErrorKind::Required))?;
        let grant = authorization(db, &id)?;
        if grant.binding_digest != binding || &grant.subject != subject {
            return Err(failure(NetworkErrorKind::Corrupt));
        }
        grant
    } else {
        active_authorization(db, subject)?.ok_or_else(|| failure(NetworkErrorKind::Required))?
    };
    match grant.state {
        AuthorizationState::Revoked => return Err(failure(NetworkErrorKind::Revoked)),
        AuthorizationState::Stale => return Err(failure(NetworkErrorKind::Stale)),
        AuthorizationState::Active => {}
    }
    if purposes.iter().any(|p| !grant.purposes.contains(p)) {
        return Err(failure(NetworkErrorKind::Required));
    }
    let (snapshot, sealed) =
        stored_subject(db, subject)?.ok_or_else(|| failure(NetworkErrorKind::Corrupt))?;
    let current = current_subject(db, subject)?;
    if !matches_current(&snapshot, &sealed, &current)?
        || snapshot.binding_digest != grant.binding_digest
        || snapshot.config_revision != grant.config_revision
        || snapshot.credential_revision != grant.credential_revision
    {
        return Err(failure(NetworkErrorKind::Stale));
    }
    if let Some(task) = task {
        let target = validate_target(&task.route.base_url, NetworkPurpose::Completion)
            .map_err(|_| failure(NetworkErrorKind::Invalid))?;
        if task.route.id != subject.id
            || task.route.model != snapshot.identity
            || target.base_url() != snapshot.base_url
            || target.origin() != snapshot.origin
        {
            return Err(failure(NetworkErrorKind::Stale));
        }
    }
    Ok((grant, current))
}
pub(crate) fn bind_task_tx(db: &Connection, task: &crate::domain::Task) -> AnyResult<()> {
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: task.route.id.clone(),
        project_id: Some(crate::project_config::validate_task_tx(db, task, true, true)?.project_id),
    };
    let (grant, current) =
        checked_authorization(db, &subject, &[NetworkPurpose::Completion], None)?;
    let target = validate_target(&task.route.base_url, NetworkPurpose::Completion)
        .map_err(|_| failure(NetworkErrorKind::Invalid))?;
    if target.base_url() != current.snapshot.base_url
        || task.route.model != current.snapshot.identity
    {
        return Err(failure(NetworkErrorKind::Stale));
    }
    db.execute(
        "INSERT INTO task_network_bindings VALUES (?1,?2,?3)",
        params![task.id, grant.authorization_id, grant.binding_digest],
    )?;
    Ok(())
}
pub(crate) fn receipt(db: &Connection, id: &str) -> AnyResult<CallReceipt> {
    let row: Option<(String, String, Option<String>, String)> = db
        .query_row(
            "SELECT receipt,authorization_id,task_id,state FROM network_calls WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (raw, grant, task, state) = row.ok_or_else(|| failure(NetworkErrorKind::NotFound))?;
    let value: CallReceipt = json(&raw)?;
    let expected_state = serde_json::to_value(value.state)?;
    if value.call_id != id
        || value.authorization_id != grant
        || value.task_id != task
        || expected_state.as_str() != Some(&state)
        || uuid::Uuid::parse_str(id).is_err()
        || !crate::workspace_changes::valid_digest(&value.endpoint_digest)
        || authorization(db, &grant)?.subject != value.subject
    {
        return Err(failure(NetworkErrorKind::Corrupt));
    }
    let valid = match value.state {
        CallState::Claimed => {
            value.finished_at.is_none()
                && value.success.is_none()
                && value.result.is_none()
                && value.http_status.is_none()
        }
        CallState::Finished => {
            value.finished_at.is_some()
                && match (value.success, value.result, value.http_status) {
                    (Some(true), Some(CallResult::Success), None) => true,
                    (Some(true), Some(CallResult::Success), Some(status)) => {
                        (200..300).contains(&status)
                    }
                    (Some(false), Some(CallResult::HttpFailure), Some(status)) => {
                        (100..600).contains(&status) && !(200..300).contains(&status)
                    }
                    (Some(false), Some(CallResult::InvalidResponse), Some(status)) => {
                        (200..300).contains(&status)
                    }
                    _ => false,
                }
        }
        CallState::Unknown => {
            value.http_status.is_none()
                && value.success.is_none()
                && matches!(
                    value.result,
                    Some(
                        CallResult::TransportUnknown
                            | CallResult::Interrupted
                            | CallResult::PersistenceUnknown
                    )
                )
        }
        CallState::NotSent => {
            value.http_status.is_none()
                && value.finished_at.is_some()
                && value.success.is_none()
                && value.result == Some(CallResult::PreflightRejected)
        }
    };
    let canonical_origin = validate_target(&value.origin, value.purpose)
        .map_err(|_| failure(NetworkErrorKind::Corrupt))?;
    if canonical_origin.origin() != value.origin
        || reqwest::Url::parse(&value.origin)
            .map_err(|_| failure(NetworkErrorKind::Corrupt))?
            .path()
            != "/"
    {
        return Err(failure(NetworkErrorKind::Corrupt));
    }
    let grant = authorization(db, &value.authorization_id)?;
    let method = if value.purpose == NetworkPurpose::Completion {
        "POST"
    } else {
        "GET"
    };
    if !allowed(&value.subject).contains(&value.purpose)
        || !grant.purposes.contains(&value.purpose)
        || value.method != method
        || value.finished_at.is_some_and(|at| at < value.claimed_at)
    {
        return Err(failure(NetworkErrorKind::Corrupt));
    }
    if !valid {
        return Err(failure(NetworkErrorKind::Corrupt));
    }
    Ok(value)
}
fn save_receipt(db: &rusqlite::Transaction<'_>, value: &CallReceipt) -> AnyResult<()> {
    let state = serde_json::to_value(value.state)?;
    db.execute(
        "UPDATE network_calls SET state=?2,receipt=?3 WHERE id=?1",
        params![value.call_id, state.as_str(), serde_json::to_string(value)?],
    )?;
    if let Some(task) = &value.task_id {
        crate::store::task_event_tx(
            db,
            task,
            "network.call",
            json!({"call_id":value.call_id,"state":value.state}),
        )?;
    }
    Ok(())
}
pub(crate) fn recover_calls_tx(db: &rusqlite::Transaction<'_>) -> AnyResult<usize> {
    let ids: Vec<String> = {
        let mut s = db.prepare("SELECT id FROM network_calls WHERE state='claimed'")?;
        s.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for id in &ids {
        let mut value = receipt(db, id)?;
        value.state = CallState::Unknown;
        value.result = Some(CallResult::Interrupted);
        value.finished_at = Some(now());
        crate::budget::finish_tx(db, &value, None)?;
        save_receipt(db, &value)?;
    }
    Ok(ids.len())
}
impl Store {
    pub(crate) fn network_preflight(
        &self,
        subject: &NetworkSubject,
        purposes: &[NetworkPurpose],
        task: Option<&crate::domain::Task>,
    ) -> Result<(), NetworkError> {
        checked_authorization(&self.db.lock().unwrap(), subject, purposes, task)
            .map(|_| ())
            .map_err(safe_error)
    }
    pub(crate) fn network_admit(
        &self,
        subject: &NetworkSubject,
        purpose: NetworkPurpose,
        task: Option<&crate::domain::Task>,
    ) -> Result<(crate::provider_transport::ProviderPermit, CallReceipt), NetworkError> {
        (|| -> AnyResult<_> {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let (grant, current) = checked_authorization(&tx, subject, &[purpose], task)?;
            crate::budget::admit_check_tx(&tx,subject,purpose)?;
            let target = validate_target(&current.snapshot.base_url, purpose)
                .map_err(|_| failure(NetworkErrorKind::Invalid))?;
            let id = uuid::Uuid::new_v4().to_string();
            let (model, user) = match subject.kind {
                NetworkSubjectKind::Route => (Some(current.snapshot.identity.clone()), None),
                NetworkSubjectKind::Account => (None, Some(current.snapshot.identity.clone())),
            };
            validate_permit_inputs(
                &target,
                &current.credential,
                &id,
                model.as_deref(),
                user.as_deref(),
            )
            .map_err(|_| failure(NetworkErrorKind::Invalid))?;
            let value = CallReceipt {
                call_id: id.clone(),
                authorization_id: grant.authorization_id,
                subject: subject.clone(),
                task_id: task.map(|t| t.id.clone()),
                purpose,
                method: target.method().into(),
                origin: target.origin().into(),
                endpoint_digest: digest(target.endpoint().as_bytes()),
                state: CallState::Claimed,
                claimed_at: now(),
                finished_at: None,
                success: None,
                http_status: None,
                result: None,
            };
            #[cfg(test)]
            crate::engine::crash_probe::block(&subject.key()?, "network-before-claim", &id);
            #[cfg(test)]
            if purpose==NetworkPurpose::Completion {crate::engine::crash_probe::block(&subject.key()?,"budget-before-claim",&id);}
            tx.execute(
                "INSERT INTO network_calls(rowid,id,authorization_id,task_id,state,receipt) VALUES (?5,?1,?2,?3,'claimed',?4)",
                params![
                    id,
                    value.authorization_id,
                    value.task_id,
                    serde_json::to_string(&value)?,
                    crate::budget::next_network_rowid(&tx)?
                ],
            )?;
            crate::budget::reserve_tx(&tx,&value)?;
            if let Some(task) = task {
                crate::store::task_event_tx(
                    &tx,
                    &task.id,
                    "network.call",
                    json!({"call_id":id,"state":"claimed"}),
                )?;
            }
            tx.commit()?;
            #[cfg(test)]
            crate::engine::crash_probe::block(&subject.key()?, "network-after-claim", &id);
            #[cfg(test)]
            if purpose==NetworkPurpose::Completion {crate::engine::crash_probe::block(&subject.key()?,"budget-after-claim",&id);}
            let permit = crate::provider_transport::ProviderPermit::new(
                target,
                current.credential,
                id,
                model,
                user,
            );
            drop(db);
            match permit {
                Ok(permit) => Ok((permit, value)),
                Err(error) => Err(self
                    .finish_network::<()>(value, Err(error))
                    .unwrap_err()
                    .into()),
            }
        })()
        .map_err(safe_error)
    }
    pub fn network_call(&self, id: &str) -> Result<CallReceipt, NetworkError> {
        receipt(&self.db.lock().unwrap(), id).map_err(safe_error)
    }
    pub fn network_calls_for_task(&self, task_id: &str) -> Result<Vec<CallReceipt>, NetworkError> {
        (|| -> AnyResult<_> {
            let db = self.db.lock().unwrap();
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)",
                [task_id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(failure(NetworkErrorKind::NotFound));
            }
            let ids: Vec<String> = {
                let mut s =
                    db.prepare("SELECT id FROM network_calls WHERE task_id=?1 ORDER BY rowid")?;
                s.query_map([task_id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            ids.iter().map(|id| receipt(&db, id)).collect()
        })()
        .map_err(safe_error)
    }
    pub(crate) fn finish_network<T>(
        &self,
        value: CallReceipt,
        result: Result<T, crate::provider_transport::TransportError>,
    ) -> Result<NetworkResponse<T>, NetworkError> {
        self.finish_network_observed(value, result, None)
    }
    pub(crate) fn finish_completion(
        &self,
        value: CallReceipt,
        result: Result<
            (serde_json::Value, serde_json::Value),
            crate::provider_transport::TransportError,
        >,
    ) -> Result<NetworkResponse<(serde_json::Value, serde_json::Value)>, NetworkError> {
        let observed = result
            .as_ref()
            .ok()
            .map(|(_, usage)| crate::provider_usage::classify_usage(usage));
        self.finish_network_observed(value, result, observed)
    }
    fn finish_network_observed<T>(
        &self,
        mut value: CallReceipt,
        result: Result<T, crate::provider_transport::TransportError>,
        observed: Option<crate::provider_usage::UsageObservation>,
    ) -> Result<NetworkResponse<T>, NetworkError> {
        use crate::provider_transport::TransportError;
        value.finished_at = Some(now());
        let kind = match &result {
            Ok(_) => {
                value.state = CallState::Finished;
                value.success = Some(true);
                value.result = Some(CallResult::Success);
                None
            }
            Err(TransportError::Response { status }) => {
                value.state = CallState::Finished;
                value.success = Some(false);
                value.http_status = Some(*status);
                value.result = Some(CallResult::HttpFailure);
                Some(NetworkErrorKind::Upstream)
            }
            Err(TransportError::InvalidResponse { status }) => {
                value.state = CallState::Finished;
                value.success = Some(false);
                value.http_status = Some(*status);
                value.result = Some(CallResult::InvalidResponse);
                Some(NetworkErrorKind::Upstream)
            }
            Err(TransportError::Invalid) => {
                value.state = CallState::NotSent;
                value.result = Some(CallResult::PreflightRejected);
                Some(NetworkErrorKind::Invalid)
            }
            Err(TransportError::Unknown) => {
                value.state = CallState::Unknown;
                value.result = Some(CallResult::TransportUnknown);
                Some(NetworkErrorKind::Unknown)
            }
        };
        #[cfg(test)]
        if value.purpose == NetworkPurpose::Completion {
            crate::engine::crash_probe::block(
                &value.subject.key()?,
                "budget-before-finish",
                &value.call_id,
            );
        }
        let saved = (|| -> AnyResult<()> {
            let mut db = self.db.lock().unwrap();
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let old = receipt(&tx, &value.call_id)?;
            let managed = crate::budget::finish_tx(&tx, &value, observed)?;
            if let Some(record) = managed {
                value = record;
            } else if old.state != CallState::Claimed {
                return Err(failure(NetworkErrorKind::Conflict));
            }
            if old.state == CallState::Claimed {
                save_receipt(&tx, &value)?;
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = saved {
            if let Some(budget) = error.downcast_ref::<crate::budget::BudgetError>()
                && matches!(
                    budget.kind,
                    crate::budget::BudgetErrorKind::Conflict
                        | crate::budget::BudgetErrorKind::Corrupt
                )
            {
                let mut error: NetworkError = (*budget).into();
                error.dispatch_state = DispatchState::Unknown;
                if budget.kind == crate::budget::BudgetErrorKind::Conflict
                    && let Ok(persisted) = receipt(&self.db.lock().unwrap(), &value.call_id)
                {
                    error.dispatch_state = match persisted.state {
                        CallState::Finished => DispatchState::Finished,
                        CallState::NotSent => DispatchState::NotSent,
                        _ => DispatchState::Unknown,
                    };
                    error.calls = vec![persisted];
                }
                return Err(error);
            }
            value.state = CallState::Unknown;
            value.success = None;
            value.http_status = None;
            value.result = Some(CallResult::PersistenceUnknown);
            return Err(NetworkError {
                kind: NetworkErrorKind::Unknown,
                dispatch_state: DispatchState::Unknown,
                calls: vec![value],
            });
        }
        if let Some(kind) = kind {
            let dispatch_state = match value.state {
                CallState::Finished => DispatchState::Finished,
                CallState::NotSent => DispatchState::NotSent,
                _ => DispatchState::Unknown,
            };
            return Err(NetworkError {
                kind,
                dispatch_state,
                calls: vec![value],
            });
        }
        Ok(NetworkResponse {
            result: result.map_err(|_| NetworkError::not_sent(NetworkErrorKind::Internal))?,
            calls: vec![value],
        })
    }
}

impl Store {
    pub(crate) fn save_network_configuration(
        &self,
        settings: &Settings,
        keys: &std::collections::HashMap<String, String>,
    ) -> AnyResult<()> {
        settings.validate()?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        crate::host_configuration::save_configuration_tx(&tx, Some(settings), keys)?;
        tx.commit()?;
        Ok(())
    }
}

pub(crate) fn refresh_configuration_subjects_tx(tx: &Connection) -> AnyResult<()> {
    let subjects: Vec<NetworkSubject> = {
        let mut stmt = tx.prepare("SELECT snapshot FROM network_subjects")?;
        let values: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        values
            .iter()
            .map(|v| Ok(json::<SubjectSnapshot>(v)?.subject))
            .collect::<AnyResult<_>>()?
    };
    for subject in subjects {
        if subject.kind == NetworkSubjectKind::Route && subject.project_id.is_none() {
            continue;
        }
        match current_subject(tx, &subject) {
            Ok(current) => {
                refresh_subject(tx, current)?;
            }
            Err(error)
                if error.downcast_ref::<NetworkError>().is_some_and(|e| {
                    matches!(
                        e.kind,
                        NetworkErrorKind::NotFound
                            | NetworkErrorKind::Required
                            | NetworkErrorKind::Invalid
                    )
                }) =>
            {
                tx.execute("UPDATE network_authorizations SET state='stale' WHERE subject=?1 AND state='active'",[subject.key()?])?;
            }
            Err(error)
                if error
                    .downcast_ref::<crate::project_config::ProjectError>()
                    .is_some() =>
            {
                tx.execute("UPDATE network_authorizations SET state='stale' WHERE subject=?1 AND state='active'",[subject.key()?])?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
