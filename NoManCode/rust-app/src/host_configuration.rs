//! Local Host route configuration with plain SHA-256 idempotency fingerprints.
use crate::{
    domain::{self, Route, Settings},
    provider_transport, secrets,
    store::Store,
};
use anyhow::Result as AnyResult;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteInput {
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub parallel_limit: usize,
}

// Deliberately neither Debug nor Serialize: this object owns plaintext input.
pub enum CredentialUpdate {
    Keep,
    Replace { value: String },
}

impl<'de> Deserialize<'de> for CredentialUpdate {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrictCredential;
        impl<'de> serde::de::Visitor<'de> for StrictCredential {
            type Value = CredentialUpdate;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a strict credential update")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                use serde::de::Error;
                let mut mode: Option<String> = None;
                let mut value: Option<String> = None;
                while let Some(field) = map
                    .next_key::<String>()
                    .map_err(|_| M::Error::custom("invalid credential update"))?
                {
                    match field.as_str() {
                        "mode" if mode.is_none() => {
                            mode = Some(
                                map.next_value()
                                    .map_err(|_| M::Error::custom("invalid credential update"))?,
                            )
                        }
                        "value" if value.is_none() => {
                            value = Some(
                                map.next_value()
                                    .map_err(|_| M::Error::custom("invalid credential update"))?,
                            )
                        }
                        _ => return Err(M::Error::custom("invalid credential update")),
                    }
                }
                match (mode.as_deref(), value) {
                    (Some("keep"), None) => Ok(CredentialUpdate::Keep),
                    (Some("replace"), Some(value)) => Ok(CredentialUpdate::Replace { value }),
                    _ => Err(M::Error::custom("invalid credential update")),
                }
            }
        }
        deserializer.deserialize_map(StrictCredential)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureHostRoute {
    pub expected_revision: u64,
    pub route: RouteInput,
    pub credential: CredentialUpdate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    Stored,
    Environment,
    Legacy,
    Missing,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfigView {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub parallel_limit: usize,
    #[serde(deserialize_with = "required_option")]
    pub key_env: Option<String>,
    pub credential_source: CredentialSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfigSnapshot {
    pub revision: u64,
    pub routes: Vec<RouteConfigView>,
}

fn required_option<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfigReceipt {
    pub operation_id: String,
    pub route_id: String,
    pub revision: u64,
    pub route: RouteConfigView,
    pub changed: bool,
    pub committed_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfigResult {
    pub receipt: RouteConfigReceipt,
    pub replayed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostConfigError {
    Invalid,
    Forbidden,
    NotFound,
    RevisionConflict,
    IdempotencyConflict,
    Busy,
    CredentialRequired,
    Corrupt,
    Unavailable,
    Internal,
}

impl HostConfigError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "host_config_invalid",
            Self::Forbidden => "forbidden",
            Self::NotFound => "host_config_not_found",
            Self::RevisionConflict => "host_config_revision_conflict",
            Self::IdempotencyConflict => "host_config_idempotency_conflict",
            Self::Busy => "host_config_busy",
            Self::CredentialRequired => "host_config_credential_required",
            Self::Corrupt => "host_config_corrupt",
            Self::Unavailable => "host_config_unavailable",
            Self::Internal => "host_config_internal",
        }
    }

    pub const fn status(self) -> u16 {
        match self {
            Self::Invalid => 400,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::RevisionConflict
            | Self::IdempotencyConflict
            | Self::Busy
            | Self::CredentialRequired => 409,
            Self::Corrupt | Self::Unavailable | Self::Internal => 500,
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::Invalid => "Host configuration request is invalid",
            Self::Forbidden => "Host configuration access is forbidden",
            Self::NotFound => "Host configuration operation was not found",
            Self::RevisionConflict => "Host configuration revision has changed",
            Self::IdempotencyConflict => "Host configuration key belongs to a different request",
            Self::Busy => "Host configuration is busy",
            Self::CredentialRequired => "A replacement credential is required",
            Self::Corrupt => "Host configuration state is corrupt",
            Self::Unavailable => "Host credential protection or access is unavailable",
            Self::Internal => "Host configuration operation failed",
        }
    }

    pub const fn retryable(self) -> bool {
        false
    }
}

impl std::fmt::Display for HostConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for HostConfigError {}

pub fn validate_credential(value: &str) -> Result<(), HostConfigError> {
    if value.is_empty()
        || value.len() > 4095
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
        || reqwest::header::HeaderValue::from_str(&format!("Bearer {value}")).is_err()
    {
        return Err(HostConfigError::Invalid);
    }
    Ok(())
}

impl RouteInput {
    pub fn normalized(&self) -> Result<Self, HostConfigError> {
        secrets::safe_metadata_text("name", &self.name).map_err(|_| HostConfigError::Invalid)?;
        secrets::safe_metadata_text("model", &self.model).map_err(|_| HostConfigError::Invalid)?;
        secrets::safe_metadata_path("base_url", &self.base_url)
            .map_err(|_| HostConfigError::Invalid)?;
        if !(16..=65536).contains(&self.max_tokens) || !(1..=8).contains(&self.parallel_limit) {
            return Err(HostConfigError::Invalid);
        }
        let target = provider_transport::validate_target(
            &self.base_url,
            provider_transport::NetworkPurpose::Completion,
        )
        .map_err(|_| HostConfigError::Invalid)?;
        let url = reqwest::Url::parse(&self.base_url).map_err(|_| HostConfigError::Invalid)?;
        if url.scheme() == "http" {
            let authority = self
                .base_url
                .split_once("://")
                .ok_or(HostConfigError::Invalid)?
                .1
                .split('/')
                .next()
                .ok_or(HostConfigError::Invalid)?;
            let host = if let Some(rest) = authority.strip_prefix('[') {
                rest.split_once(']').ok_or(HostConfigError::Invalid)?.0
            } else {
                authority
                    .split(':')
                    .next()
                    .ok_or(HostConfigError::Invalid)?
            };
            if !host
                .parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
            {
                return Err(HostConfigError::Invalid);
            }
        }
        let mut result = self.clone();
        result.base_url = target.base_url().to_owned();
        Ok(result)
    }
}

impl ConfigureHostRoute {
    pub fn validate(&self, route_id: &str, key: &str) -> Result<RouteInput, HostConfigError> {
        if !domain::valid_name(route_id)
            || route_id == "newapi-account"
            || self.expected_revision > i64::MAX as u64
        {
            return Err(HostConfigError::Invalid);
        }
        secrets::validate_idempotency_key(key).map_err(|_| HostConfigError::Invalid)?;
        if let CredentialUpdate::Replace { value } = &self.credential {
            validate_credential(value)?;
        }
        self.route.normalized()
    }
}

fn safe_error(error: anyhow::Error) -> HostConfigError {
    if let Some(error) = error.downcast_ref::<HostConfigError>() {
        return *error;
    }
    if let Some(error) = error.downcast_ref::<crate::network_authorization::NetworkError>()
        && error.kind == crate::network_authorization::NetworkErrorKind::Corrupt
    {
        return HostConfigError::Corrupt;
    }
    if matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(
            rusqlite::Error::InvalidColumnType(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::FromSqlConversionFailure(..)
        )
    ) {
        return HostConfigError::Corrupt;
    }
    if let Some(rusqlite::Error::SqliteFailure(error, _)) = error.downcast_ref::<rusqlite::Error>()
    {
        return match error.code {
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                HostConfigError::Busy
            }
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
                HostConfigError::Corrupt
            }
            _ => HostConfigError::Internal,
        };
    }
    HostConfigError::Internal
}

pub(crate) fn revision_tx(db: &Connection) -> AnyResult<u64> {
    let values: Vec<(i64, i64)> = db
        .prepare("SELECT id,revision FROM host_config_state")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    match values.as_slice() {
        [(1, revision)] if *revision >= 0 => Ok(*revision as u64),
        _ => Err(HostConfigError::Corrupt.into()),
    }
}

pub(crate) fn configuration_history_tx(db: &Connection) -> AnyResult<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM config) OR EXISTS(SELECT 1 FROM secrets) OR EXISTS(SELECT 1 FROM host_config_requests) OR EXISTS(SELECT 1 FROM network_subjects) OR EXISTS(SELECT 1 FROM network_authorizations) OR EXISTS(SELECT 1 FROM project_budgets) OR EXISTS(SELECT 1 FROM projects) OR EXISTS(SELECT 1 FROM tasks)",
        [],
        |r| r.get(0),
    )?)
}

pub(crate) fn settings_tx(db: &Connection) -> AnyResult<Option<Settings>> {
    let raw: Option<String> = db
        .query_row("SELECT value FROM config WHERE id='settings'", [], |r| {
            r.get(0)
        })
        .optional()?;
    raw.map(|v| serde_json::from_str(&v).map_err(|_| HostConfigError::Corrupt.into()))
        .transpose()
}

fn secret_tx(db: &Connection, id: &str) -> AnyResult<Option<String>> {
    let blob: Option<Vec<u8>> = db
        .query_row("SELECT value FROM secrets WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    blob.map(|blob| {
        let bytes = secrets::open(&blob).map_err(|_| HostConfigError::Unavailable)?;
        String::from_utf8(bytes).map_err(|_| HostConfigError::Corrupt.into())
    })
    .transpose()
}

fn safe_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 500
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
        && secrets::safe_metadata_text("key_env", name).is_ok()
}

fn credential_tx(db: &Connection, route: &Route) -> AnyResult<(CredentialSource, Option<String>)> {
    if let Some(name) = &route.key_env {
        if !safe_env_name(name) {
            return Err(HostConfigError::Corrupt.into());
        }
        match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => {
                validate_credential(&value).map_err(|_| HostConfigError::Unavailable)?;
                return Ok((CredentialSource::Environment, Some(value)));
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(HostConfigError::Unavailable.into());
            }
            _ => {}
        }
    }
    for (id, source) in [
        (format!("route::{}", route.id), CredentialSource::Stored),
        (route.id.clone(), CredentialSource::Legacy),
    ] {
        if let Some(value) = secret_tx(db, &id)?
            && !value.trim().is_empty()
        {
            validate_credential(&value).map_err(|_| HostConfigError::Unavailable)?;
            return Ok((source, Some(value)));
        }
    }
    Ok((CredentialSource::Missing, None))
}

fn validate_view(view: &RouteConfigView) -> Result<(), HostConfigError> {
    if !domain::valid_name(&view.id)
        || view.id == "newapi-account"
        || !(16..=65536).contains(&view.max_tokens)
        || !(1..=8).contains(&view.parallel_limit)
        || view
            .key_env
            .as_ref()
            .is_some_and(|name| !safe_env_name(name))
    {
        return Err(HostConfigError::Corrupt);
    }
    secrets::safe_metadata_text("name", &view.name).map_err(|_| HostConfigError::Corrupt)?;
    secrets::safe_metadata_text("model", &view.model).map_err(|_| HostConfigError::Corrupt)?;
    secrets::safe_metadata_path("base_url", &view.base_url)
        .map_err(|_| HostConfigError::Corrupt)?;
    domain::validate_url(&view.base_url).map_err(|_| HostConfigError::Corrupt)?;
    Ok(())
}

fn view_tx(db: &Connection, route: &Route) -> AnyResult<RouteConfigView> {
    let source = match credential_tx(db, route) {
        Ok((source, _)) => source,
        Err(_) => CredentialSource::Unavailable,
    };
    let view = RouteConfigView {
        id: route.id.clone(),
        name: route.name.clone(),
        base_url: route.base_url.clone(),
        model: route.model.clone(),
        max_tokens: route.max_tokens,
        parallel_limit: route.parallel_limit,
        key_env: route.key_env.clone(),
        credential_source: source,
    };
    validate_view(&view)?;
    Ok(view)
}

pub(crate) fn verify_persisted_rows(_db: &Connection) -> AnyResult<()> {
    Ok(())
}

struct SavedRequest {
    fingerprint: Vec<u8>,
    receipt: String,
}

fn saved_tx(db: &Connection, key: &str) -> AnyResult<Option<SavedRequest>> {
    Ok(db
        .query_row(
            "SELECT request_fingerprint_blob,receipt FROM host_config_requests WHERE key=?1",
            [key],
            |row| {
                Ok(SavedRequest {
                    fingerprint: row.get(0)?,
                    receipt: row.get(1)?,
                })
            },
        )
        .optional()?)
}

fn fingerprint(route_id: &str, request: &ConfigureHostRoute, route: &RouteInput) -> [u8; 32] {
    fn field(hash: &mut Sha256, bytes: &[u8]) {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    let mut hash = Sha256::new();
    field(&mut hash, b"NoHumanCode/host-route-configuration/v1");
    field(&mut hash, route_id.as_bytes());
    field(&mut hash, &request.expected_revision.to_be_bytes());
    for value in [&route.name, &route.base_url, &route.model] {
        field(&mut hash, value.as_bytes());
    }
    field(&mut hash, &route.max_tokens.to_be_bytes());
    field(&mut hash, &(route.parallel_limit as u64).to_be_bytes());
    match &request.credential {
        CredentialUpdate::Keep => field(&mut hash, b"keep"),
        CredentialUpdate::Replace { value } => {
            field(&mut hash, b"replace");
            field(&mut hash, value.as_bytes());
        }
    }
    hash.finalize().into()
}

pub(crate) fn save_configuration_tx(
    db: &Connection,
    settings: Option<&Settings>,
    keys: &HashMap<String, String>,
    refresh_network: bool,
) -> AnyResult<bool> {
    let revision = revision_tx(db)?;
    let encoded = settings.map(serde_json::to_value).transpose()?;
    let previous: Option<String> = db
        .query_row("SELECT value FROM config WHERE id='settings'", [], |r| {
            r.get(0)
        })
        .optional()?;
    let old = previous
        .map(|v| {
            serde_json::from_str::<serde_json::Value>(&v).map_err(|_| HostConfigError::Corrupt)
        })
        .transpose()?;
    let settings_changed = encoded.as_ref().is_some_and(|v| Some(v) != old.as_ref());
    let mut sealed = Vec::new();
    for (id, value) in keys {
        // Allow replacing an unreadable/corrupt old DPAPI secret cleanly.
        let unchanged = secret_tx(db, id)
            .ok()
            .flatten()
            .is_some_and(|existing| existing == *value);
        if !unchanged {
            sealed.push((
                id,
                secrets::seal(value.as_bytes()).map_err(|_| HostConfigError::Unavailable)?,
            ));
        }
    }
    if !settings_changed && sealed.is_empty() {
        if refresh_network {
            crate::network_authorization::refresh_configuration_subjects_tx(db)?;
        }
        return Ok(false);
    }
    let next = revision
        .checked_add(1)
        .filter(|v| *v <= i64::MAX as u64)
        .ok_or(HostConfigError::Corrupt)?;
    if settings_changed {
        db.execute(
            "INSERT INTO config(id,value) VALUES('settings',?1) ON CONFLICT(id) DO UPDATE SET value=excluded.value",
            [serde_json::to_string(&encoded)?],
        )?;
    }
    for (id, blob) in sealed {
        db.execute(
            "INSERT INTO secrets(id,value) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET value=excluded.value",
            params![id, blob],
        )?;
    }
    if refresh_network {
        crate::network_authorization::refresh_configuration_subjects_tx(db)?;
    }
    db.execute(
        "UPDATE host_config_state SET revision=?1 WHERE id=1",
        [next],
    )?;
    Ok(true)
}

impl Store {
    pub fn host_config(&self) -> Result<HostConfigSnapshot, HostConfigError> {
        (|| -> AnyResult<_> {
            let db = self.db.lock().map_err(|_| HostConfigError::Internal)?;
            let revision = revision_tx(&db)?;
            let settings = settings_tx(&db)?;
            if settings.is_none() && (revision != 0 || configuration_history_tx(&db)?) {
                return Err(HostConfigError::Corrupt.into());
            }
            let routes = settings.map(|s| s.routes).unwrap_or_default();
            let mut ids = std::collections::HashSet::new();
            let mut views = Vec::new();
            for route in routes {
                if !ids.insert(route.id.clone()) {
                    return Err(HostConfigError::Corrupt.into());
                }
                views.push(view_tx(&db, &route)?);
            }
            Ok(HostConfigSnapshot {
                revision,
                routes: views,
            })
        })()
        .map_err(safe_error)
    }

    pub fn host_config_operation(&self, key: &str) -> Result<RouteConfigResult, HostConfigError> {
        secrets::validate_idempotency_key(key).map_err(|_| HostConfigError::Invalid)?;
        (|| -> AnyResult<_> {
            let db = self.db.lock().map_err(|_| HostConfigError::Internal)?;
            let saved = saved_tx(&db, key)?.ok_or(HostConfigError::NotFound)?;
            let receipt: RouteConfigReceipt =
                serde_json::from_str(&saved.receipt).map_err(|_| HostConfigError::Corrupt)?;
            Ok(RouteConfigResult {
                receipt,
                replayed: true,
            })
        })()
        .map_err(safe_error)
    }

    pub fn configure_host_route(
        &self,
        route_id: &str,
        request: ConfigureHostRoute,
        key: &str,
    ) -> Result<RouteConfigResult, HostConfigError> {
        self.configure_host_route_guarded(route_id, request, key, false)
    }

    pub(crate) fn configure_host_route_guarded(
        &self,
        route_id: &str,
        request: ConfigureHostRoute,
        key: &str,
        _engine_busy: bool,
    ) -> Result<RouteConfigResult, HostConfigError> {
        let input = request.validate(route_id, key)?;
        let digest = fingerprint(route_id, &request, &input);
        (|| -> AnyResult<_> {
            let mut db = self.db.lock().map_err(|_| HostConfigError::Internal)?;
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let revision = revision_tx(&tx)?;
            if let Some(saved) = saved_tx(&tx, key)? {
                if saved.fingerprint != digest {
                    return Err(HostConfigError::IdempotencyConflict.into());
                }
                let receipt: RouteConfigReceipt =
                    serde_json::from_str(&saved.receipt).map_err(|_| HostConfigError::Corrupt)?;
                tx.commit()?;
                return Ok(RouteConfigResult {
                    receipt,
                    replayed: true,
                });
            }
            if request.expected_revision != revision {
                return Err(HostConfigError::RevisionConflict.into());
            }
            let mut settings = settings_tx(&tx)?.ok_or(HostConfigError::Corrupt)?;
            let old = settings.routes.iter().find(|r| r.id == route_id).cloned();
            let mut keys = HashMap::new();
            let key_env = match &request.credential {
                CredentialUpdate::Keep => {
                    let old = old.as_ref().ok_or(HostConfigError::CredentialRequired)?;
                    let target = provider_transport::validate_target(
                        &old.base_url,
                        provider_transport::NetworkPurpose::Completion,
                    )
                    .map_err(|_| HostConfigError::Corrupt)?;
                    if target.base_url() != input.base_url {
                        return Err(HostConfigError::CredentialRequired.into());
                    }
                    let (_, selected) = credential_tx(&tx, old)?;
                    if selected.is_none() {
                        return Err(HostConfigError::CredentialRequired.into());
                    }
                    old.key_env.clone()
                }
                CredentialUpdate::Replace { value } => {
                    keys.insert(format!("route::{route_id}"), value.clone());
                    None
                }
            };
            let route = Route {
                id: route_id.to_owned(),
                name: input.name.clone(),
                base_url: input.base_url.clone(),
                model: input.model.clone(),
                max_tokens: input.max_tokens,
                parallel_limit: input.parallel_limit,
                key_env,
            };
            if let Some(index) = settings.routes.iter().position(|r| r.id == route_id) {
                settings.routes[index] = route.clone();
            } else {
                settings.routes.push(route.clone());
            }
            settings.validate().map_err(|_| HostConfigError::Invalid)?;
            let changed = save_configuration_tx(&tx, Some(&settings), &keys, true)?;
            let view = view_tx(&tx, &route)?;
            if matches!(
                view.credential_source,
                CredentialSource::Missing | CredentialSource::Unavailable
            ) {
                return Err(HostConfigError::Unavailable.into());
            }
            let receipt = RouteConfigReceipt {
                operation_id: domain::id(),
                route_id: route_id.to_owned(),
                revision: revision_tx(&tx)?,
                route: view,
                changed,
                committed_at: domain::now(),
            };
            tx.execute(
                "INSERT INTO host_config_requests(key,operation_id,route_id,expected_revision,result_revision,request_fingerprint_blob,receipt,committed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    key,
                    receipt.operation_id,
                    route_id,
                    request.expected_revision,
                    receipt.revision,
                    digest.as_slice(),
                    serde_json::to_string(&receipt)?,
                    receipt.committed_at
                ],
            )?;
            tx.commit()?;
            Ok(RouteConfigResult {
                receipt,
                replayed: false,
            })
        })()
        .map_err(safe_error)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn req(revision: u64, key_val: &str) -> ConfigureHostRoute {
        ConfigureHostRoute {
            expected_revision: revision,
            route: RouteInput {
                name: "Mock".into(),
                base_url: "https://example.invalid/v1".into(),
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 1,
            },
            credential: CredentialUpdate::Replace {
                value: key_val.into(),
            },
        }
    }

    #[test]
    fn plain_fingerprint_replay_and_replace_overwrites_corrupt_dpapi_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("host.db")).unwrap();
        crate::store::fresh_settings(&store, dir.path()).unwrap();

        let first = store
            .configure_host_route("route", req(0, "secret-1"), "op-1")
            .unwrap();
        assert_eq!(first.receipt.revision, 1);
        assert!(!first.replayed);

        let replayed = store
            .configure_host_route("route", req(0, "secret-1"), "op-1")
            .unwrap();
        assert!(replayed.replayed);
        assert_eq!(replayed.receipt, first.receipt);
        assert_eq!(
            store.host_config_operation("op-1").unwrap().receipt,
            first.receipt
        );

        // Corrupt the persisted DPAPI secret blob and verify Replace repairs it cleanly
        // even when engine_busy=true is passed.
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE secrets SET value=X'DEADBEEF' WHERE id='route::route'",
                [],
            )
            .unwrap();
        assert_eq!(
            store.host_config().unwrap().routes[0].credential_source,
            CredentialSource::Unavailable
        );
        let repaired = store
            .configure_host_route_guarded("route", req(1, "secret-2"), "op-2", true)
            .unwrap();
        assert_eq!(
            repaired.receipt.route.credential_source,
            CredentialSource::Stored
        );
        assert_eq!(
            store.secret("route::route").unwrap().as_deref(),
            Some("secret-2")
        );
    }
}
