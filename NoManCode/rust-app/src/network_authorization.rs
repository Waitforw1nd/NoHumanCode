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
}
impl NetworkSubject {
    pub(crate) fn key(&self) -> Result<String, NetworkError> {
        match self.kind {
            NetworkSubjectKind::Route
                if crate::domain::valid_name(&self.id) && self.id != "newapi-account" =>
            {
                Ok(format!("route:{}", self.id))
            }
            NetworkSubjectKind::Account if self.id == "newapi" => Ok("account:newapi".into()),
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
impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    error
        .downcast_ref::<NetworkError>()
        .cloned()
        .unwrap_or_else(|| NetworkError::not_sent(NetworkErrorKind::Internal))
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
        .map(|v| String::from_utf8(secrets::open(&v)?).map_err(Into::into))
        .transpose()
}
fn current_subject(db: &Connection, subject: &NetworkSubject) -> AnyResult<CurrentSubject> {
    subject.key()?;
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
                    .map(|value| (format!("env:{name}"), value))
            });
            let selected = if let Some(env) = env {
                Some(env)
            } else if let Some(key) =
                read_secret(db, &format!("route::{}", route.id))?.filter(|v| !v.trim().is_empty())
            {
                Some((format!("secret:route::{}", route.id), key))
            } else {
                read_secret(db, &route.id)?
                    .filter(|v| !v.trim().is_empty())
                    .map(|key| (format!("secret:{}", route.id), key))
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
                "secret:newapi-account".into(),
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
    row.map(|(s, b)| Ok((json::<SubjectSnapshot>(&s)?, b)))
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
        && secrets::open(sealed)? == current.credential.as_bytes())
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
    snapshot.config_revision = old
        .as_ref()
        .map(|(s, _)| s.config_revision + u64::from(!same_config(s, &snapshot)))
        .unwrap_or(1);
    snapshot.credential_revision = old
        .as_ref()
        .map(|(s, _)| s.credential_revision + 1)
        .unwrap_or(1);
    snapshot.binding_digest = digest(&serde_json::to_vec(
        &json!({"version":1,"subject":snapshot.subject,"config_revision":snapshot.config_revision,"credential_revision":snapshot.credential_revision,"origin":snapshot.origin,"base_url":snapshot.base_url,"identity":snapshot.identity,"credential_ref":snapshot.credential_ref}),
    )?);
    db.execute(
        "UPDATE network_authorizations SET state='stale' WHERE subject=?1 AND state='active'",
        [&key],
    )?;
    db.execute("INSERT INTO network_subjects(subject,snapshot,credential_blob) VALUES (?1,?2,?3) ON CONFLICT(subject) DO UPDATE SET snapshot=excluded.snapshot,credential_blob=excluded.credential_blob",params![key,serde_json::to_string(&snapshot)?,secrets::seal(current.credential.as_bytes())?])?;
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
        }
    } else if key == "account:newapi" {
        NetworkSubject {
            kind: NetworkSubjectKind::Account,
            id: "newapi".into(),
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
        (||->AnyResult<_>{
        secrets::validate_idempotency_key(key).map_err(|_|failure(NetworkErrorKind::Invalid))?;let subject_key=subject.key()?;
        request.purposes.sort();request.purposes.dedup();if request.purposes.is_empty() || request.purposes.iter().any(|p|!allowed(&subject).contains(p)){return Err(failure(NetworkErrorKind::Invalid));}
        let request_digest=digest(&serde_json::to_vec(&request)?);
        let mut db=self.db.lock().unwrap();let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing:Option<(String,String)>=tx.query_row("SELECT request_digest,authorization_id FROM network_confirmations WHERE subject=?1 AND creation_key=?2",params![subject_key,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((hash,id))=existing {if hash!=request_digest{return Err(failure(NetworkErrorKind::Conflict));}return Ok(NetworkDecision{authorization:authorization(&tx,&id)?,replayed:true});}
        let (snapshot,sealed)=stored_subject(&tx,&subject)?.ok_or_else(||failure(NetworkErrorKind::Required))?;
        let current=current_subject(&tx,&subject)?;
        if !matches_current(&snapshot,&sealed,&current)? || request.config_revision!=snapshot.config_revision || request.credential_revision!=snapshot.credential_revision || request.binding_digest!=snapshot.binding_digest {return Err(failure(NetworkErrorKind::Stale));}
        let active=active_authorization(&tx,&subject)?;
        let (id,replayed)=if let Some(active)=active.filter(|a|a.binding_digest==snapshot.binding_digest && a.purposes==request.purposes){(active.authorization_id,true)}else{
            tx.execute("UPDATE network_authorizations SET state='stale' WHERE subject=?1 AND state='active'",[&subject_key])?;
            let id=uuid::Uuid::new_v4().to_string();tx.execute("INSERT INTO network_authorizations VALUES (?1,?2,?3,?4,?5,?6,'active',?7)",params![id,subject_key,snapshot.config_revision,snapshot.credential_revision,snapshot.binding_digest,serde_json::to_string(&request.purposes)?,now()])?;
            tx.execute("INSERT INTO network_events(subject,kind,object_id,created_at) VALUES (?1,'confirmed',?2,?3)",params![subject_key,id,now()])?;(id,false)
        };
        tx.execute("INSERT INTO network_confirmations VALUES (?1,?2,?3,?4)",params![subject_key,key,request_digest,id])?;
        let result=authorization(&tx,&id)?;tx.commit()?;Ok(NetworkDecision{authorization:result,replayed})
    })().map_err(safe_error)
    }
    pub fn network_authorization(&self, id: &str) -> Result<NetworkAuthorization, NetworkError> {
        authorization(&self.db.lock().unwrap(), id).map_err(safe_error)
    }
    pub fn network_revoke(&self, id: &str) -> Result<NetworkAuthorization, NetworkError> {
        (||->AnyResult<_>{let mut db=self.db.lock().unwrap();let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;let current=authorization(&tx,id)?;if current.state!=AuthorizationState::Revoked {tx.execute("UPDATE network_authorizations SET state='revoked' WHERE id=?1",[id])?;tx.execute("INSERT INTO network_events(subject,kind,object_id,created_at) VALUES (?1,'revoked',?2,?3)",params![current.subject.key()?,id,now()])?;}let result=authorization(&tx,id)?;tx.commit()?;Ok(result)})().map_err(safe_error)
    }
}
