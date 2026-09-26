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
