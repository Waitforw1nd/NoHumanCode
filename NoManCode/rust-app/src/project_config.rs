//! Explicit project registration and permission ceilings. Historical tasks are never adopted.
use crate::{
    domain::*,
    repository::Repository,
    secrets,
    store::Store,
    workspace_identity::{RootIdentity, RootOverlap},
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub allowed_route_ids: Vec<String>,
    pub default_route_id: Option<String>,
    pub tools: bool,
    pub write_scopes: Vec<String>,
    pub allow_commands: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfigSnapshot {
    pub project_id: ProjectId,
    pub revision: u64,
    pub config: ProjectConfig,
    pub permission_digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSelection {
    pub project_id: Option<ProjectId>,
    pub revision: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterProject {
    pub name: String,
    pub root_path: String,
    pub idempotency_key: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureProject {
    pub project_id: ProjectId,
    pub expected_revision: u64,
    pub config: ProjectConfig,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectProject {
    pub project_id: ProjectId,
    pub expected_revision: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartInProject {
    pub project_id: ProjectId,
    pub expected_config_revision: u64,
    pub request: RunRequest,
    pub idempotency_key: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProjectBinding {
    pub binding_version: u32,
    pub task_id: String,
    pub project_id: ProjectId,
    pub root_identity: RootIdentity,
    pub config_revision: u64,
    pub permission_digest: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectError {
    Invalid,
    NotFound,
    RevisionConflict,
    DuplicateRoot,
    OverlappingRoot,
    SelectionRequired,
    LegacyUnbound,
    RootChanged,
    RootUnavailable,
    PermissionRevoked,
    OperationUnsupported,
    WorkspaceSelectionManaged,
    Corrupt,
    Internal,
}
impl ProjectError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "project_invalid",
            Self::NotFound => "project_not_found",
            Self::RevisionConflict => "project_revision_conflict",
            Self::DuplicateRoot => "project_duplicate_root",
            Self::OverlappingRoot => "project_overlapping_root",
            Self::SelectionRequired => "project_selection_required",
            Self::LegacyUnbound => "project_legacy_unbound",
            Self::RootChanged => "project_root_changed",
            Self::RootUnavailable => "project_root_unavailable",
            Self::PermissionRevoked => "project_permission_revoked",
            Self::OperationUnsupported => "project_operation_unsupported",
            Self::WorkspaceSelectionManaged => "workspace_selection_managed",
            Self::Corrupt => "project_corrupt",
            Self::Internal => "project_internal",
        }
    }
}
impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ProjectError {}

pub(crate) fn root_error(error: crate::workspace_identity::RootIdentityError) -> ProjectError {
    use crate::workspace_identity::RootIdentityError as E;
    match error {
        E::Unavailable(_) => ProjectError::RootUnavailable,
        E::IdentityChanged | E::ReparsePoint | E::CaseSensitiveDirectory => {
            ProjectError::RootChanged
        }
        E::InvalidSnapshot => ProjectError::Corrupt,
        E::InvalidPath => ProjectError::Invalid,
        E::UnsupportedPlatform | E::UnsupportedIdentity => ProjectError::OperationUnsupported,
    }
}
fn parse<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|_| ProjectError::Corrupt.into())
}
fn digest<T: Serialize>(version: &str, value: &T) -> Result<String> {
    Ok(crate::workspace_changes::digest(&serde_json::to_vec(&(
        version, value,
    ))?))
}
impl ProjectConfig {
    pub(crate) fn normalized(mut self, settings: &Settings) -> Result<Self> {
        if self.allowed_route_ids.len() > 256
            || self.write_scopes.len() > 256
            || (!self.tools && (self.allow_commands || !self.write_scopes.is_empty()))
            || self
                .allowed_route_ids
                .iter()
                .any(|id| !settings.routes.iter().any(|r| &r.id == id))
            || self
                .default_route_id
                .as_ref()
                .is_some_and(|id| !self.allowed_route_ids.contains(id))
        {
            return Err(ProjectError::Invalid.into());
        }
        self.allowed_route_ids.sort();
        self.allowed_route_ids.dedup();
        self.write_scopes = self
            .write_scopes
            .iter()
            .map(|s| {
                if s == "*" {
                    Ok(s.clone())
                } else {
                    scope_path(s).map_err(|_| ProjectError::Invalid.into())
                }
            })
            .collect::<Result<_>>()?;
        self.write_scopes.sort();
        self.write_scopes.dedup();
        Ok(self)
    }
    pub(crate) fn permits(&self, spec: &TaskSpec, model: bool) -> bool {
        (!model || self.allowed_route_ids.contains(&spec.route_id))
            && (!spec.tools || self.tools)
            && (!spec.allow_commands || self.allow_commands)
            && spec
                .write_scopes
                .iter()
                .all(|requested| scope_allowed(&self.write_scopes, requested))
    }
}
pub(crate) fn scope_allowed(ceiling: &[String], requested: &str) -> bool {
    if ceiling.iter().any(|s| s == "*") {
        return true;
    }
    if requested == "*" {
        return false;
    }
    let Ok(requested) = scope_path(requested) else {
        return false;
    };
    ceiling
        .iter()
        .filter_map(|s| scope_path(s).ok())
        .any(|s| requested == s || requested.starts_with(&format!("{s}/")))
}
pub(crate) fn task_digest(spec: &TaskSpec) -> Result<String> {
    let mut scopes = spec
        .write_scopes
        .iter()
        .map(|s| {
            if s == "*" {
                Ok(s.clone())
            } else {
                scope_path(s)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    scopes.sort();
    scopes.dedup();
    digest(
        "task-project-permissions-v1",
        &(&spec.route_id, spec.tools, scopes, spec.allow_commands),
    )
}
pub(crate) fn config_tx(db: &Connection, id: &ProjectId) -> Result<ProjectConfigSnapshot> {
    let row: Option<(u64, String, String)> = db
        .query_row(
            "SELECT revision,value,permission_digest FROM project_configs WHERE project_id=?1",
            [&id.0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (revision, value, permission_digest) = row.ok_or(ProjectError::NotFound)?;
    let config: ProjectConfig = parse(&value)?;
    if revision == 0 || digest("project-config-v1", &config)? != permission_digest {
        return Err(ProjectError::Corrupt.into());
    }
    Ok(ProjectConfigSnapshot {
        project_id: id.clone(),
        revision,
        config,
        permission_digest,
    })
}
pub(crate) fn root_tx(db: &Connection, id: &ProjectId) -> Result<RootIdentity> {
    let value: Option<String> = db
        .query_row(
            "SELECT identity FROM project_root_bindings WHERE project_id=?1",
            [&id.0],
            |r| r.get(0),
        )
        .optional()?;
    parse(&value.ok_or(ProjectError::LegacyUnbound)?)
}
pub(crate) fn workspace_tx(db: &Connection, id: &ProjectId) -> Result<PathBuf> {
    let root = root_tx(db, id)?;
    let verified = root.verify().map_err(root_error)?;
    let project = Repository::new(db).project(id)?;
    let current =
        std::fs::canonicalize(&project.root_path).map_err(|_| ProjectError::RootUnavailable)?;
    if current != verified.path() {
        return Err(ProjectError::RootChanged.into());
    }
    Ok(verified.path().to_owned())
}
pub(crate) fn binding_tx(db: &Connection, task_id: &str) -> Result<Option<TaskProjectBinding>> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT project_id,value FROM task_project_bindings WHERE task_id=?1",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(project, value)| {
        let binding: TaskProjectBinding = parse(&value)?;
        if binding.binding_version != 1
            || binding.task_id != task_id
            || binding.project_id.0 != project
            || binding.config_revision == 0
        {
            return Err(ProjectError::Corrupt.into());
        }
        Ok(binding)
    })
    .transpose()
}
/// Does not grant permissions; it validates the durable Task -> Session -> Project identity chain.
pub(crate) fn validate_task_tx(
    db: &Connection,
    task: &Task,
    permissions: bool,
    model: bool,
) -> Result<TaskProjectBinding> {
    let binding = binding_tx(db, &task.id)?.ok_or(ProjectError::LegacyUnbound)?;
    let mapped: Option<String> = db.query_row("SELECT s.project_id FROM turn_tasks tt JOIN sessions s ON s.id=tt.session_id JOIN turns tr ON tr.id=tt.turn_id WHERE tt.legacy_task_id=?1 AND tr.project_id=s.project_id AND tr.session_id=s.id",[&task.id],|r|r.get(0)).optional()?;
    if mapped.as_deref() != Some(&binding.project_id.0)
        || binding.permission_digest != task_digest(&task.spec)?
        || serde_json::to_value(&binding.root_identity)?
            != serde_json::to_value(root_tx(db, &binding.project_id)?)?
    {
        return Err(ProjectError::Corrupt.into());
    }
    let root = workspace_tx(db, &binding.project_id)?;
    let task_root =
        std::fs::canonicalize(&task.workspace).map_err(|_| ProjectError::RootUnavailable)?;
    if root != task_root {
        return Err(ProjectError::RootChanged.into());
    }
    let config = config_tx(db, &binding.project_id)?;
    if config.revision < binding.config_revision {
        return Err(ProjectError::Corrupt.into());
    }
    if permissions && !config.config.permits(&task.spec, model) {
        return Err(ProjectError::PermissionRevoked.into());
    }
    Ok(binding)
}
pub(crate) fn bind_task_tx(
    db: &Connection,
    task: &Task,
    project: &ProjectId,
    expected: u64,
) -> Result<()> {
    let config = config_tx(db, project)?;
    if config.revision != expected {
        return Err(ProjectError::RevisionConflict.into());
    }
    if !config.config.permits(&task.spec, true) {
        return Err(ProjectError::PermissionRevoked.into());
    }
    let root = workspace_tx(db, project)?;
    if std::fs::canonicalize(&task.workspace).map_err(|_| ProjectError::RootUnavailable)? != root {
        return Err(ProjectError::RootChanged.into());
    }
    let binding = TaskProjectBinding {
        binding_version: 1,
        task_id: task.id.clone(),
        project_id: project.clone(),
        root_identity: root_tx(db, project)?,
        config_revision: expected,
        permission_digest: task_digest(&task.spec)?,
    };
    db.execute("INSERT INTO task_project_bindings(task_id,project_id,value,provenance) VALUES (?1,?2,?3,'native_v1')",params![task.id,project.0,serde_json::to_string(&binding)?])?;
    validate_task_tx(db, task, true, true)?;
    Ok(())
}
impl Store {
    pub fn project_config(&self, id: &ProjectId) -> Result<ProjectConfigSnapshot> {
        config_tx(&self.db.lock().unwrap(), id)
    }
    pub fn current_project(&self) -> Result<ProjectSelection> {
        Ok(self.db.lock().unwrap().query_row(
            "SELECT project_id,revision FROM project_selection WHERE id=1",
            [],
            |r| {
                Ok(ProjectSelection {
                    project_id: r.get::<_, Option<String>>(0)?.map(ProjectId),
                    revision: r.get(1)?,
                })
            },
        )?)
    }
    pub fn task_project_binding(&self, id: &str) -> Result<Option<TaskProjectBinding>> {
        self.task(id)?;
        binding_tx(&self.db.lock().unwrap(), id)
    }
    pub fn project_workspace(&self, id: &ProjectId) -> Result<PathBuf> {
        workspace_tx(&self.db.lock().unwrap(), id)
    }
    pub(crate) fn validate_project_task(
        &self,
        task: &Task,
        permissions: bool,
        model: bool,
    ) -> Result<TaskProjectBinding> {
        validate_task_tx(&self.db.lock().unwrap(), task, permissions, model)
    }
    pub fn project_runs(&self, id: &ProjectId) -> Result<Vec<Run>> {
        self.project(id)?;
        let ids: Vec<String> = {
            let db = self.db.lock().unwrap();
            let mut s=db.prepare("SELECT legacy_run_id FROM sessions WHERE project_id=?1 ORDER BY created_at DESC,id LIMIT 100")?;
            s.query_map([&id.0], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        ids.iter().map(|id| self.run(id)).collect()
    }
    pub(crate) fn register_project(&self, request: RegisterProject) -> Result<Project> {
        secrets::validate_idempotency_key(&request.idempotency_key)
            .map_err(|_| ProjectError::Invalid)?;
        let hash = digest("register-project-v1", &(&request.name, &request.root_path))?;
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let replay: Option<(String, String)> = tx
            .query_row(
                "SELECT request_hash,project_id FROM project_registration_keys WHERE key=?1",
                [&request.idempotency_key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((old, project)) = replay {
            if old != hash {
                return Err(crate::store::IdempotencyConflict.into());
            }
            return Repository::new(&tx).project(&ProjectId(project));
        }
        if request.name.trim().is_empty()
            || request.name.len() > 500
            || secrets::safe_metadata_text("project_name", &request.name).is_err()
            || secrets::safe_metadata_path("project_root", &request.root_path).is_err()
        {
            return Err(ProjectError::Invalid.into());
        }
        let root = RootIdentity::capture(Path::new(&request.root_path)).map_err(root_error)?;
        let path = root
            .verify()
            .map_err(root_error)?
            .path()
            .to_string_lossy()
            .into_owned();
        let projects = Repository::new(&tx).projects()?;
        let mut legacy = None;
        for project in projects {
            let bound: Option<String> = tx
                .query_row(
                    "SELECT identity FROM project_root_bindings WHERE project_id=?1",
                    [&project.id.0],
                    |r| r.get(0),
                )
                .optional()?;
            let existing = match &bound {
                Some(value) => parse(value)?,
                None => RootIdentity::capture(Path::new(&project.root_path)).map_err(root_error)?,
            };
            match root.overlap(&existing).map_err(root_error)? {
                RootOverlap::Disjoint => {}
                RootOverlap::Same
                    if bound.is_none() && legacy.is_none() && project.name == request.name =>
                {
                    legacy = Some(project)
                }
                RootOverlap::Same => return Err(ProjectError::DuplicateRoot.into()),
                _ => return Err(ProjectError::OverlappingRoot.into()),
            }
        }
        let project = legacy.unwrap_or_else(|| Project {
            id: ProjectId(id()),
            name: request.name,
            root_path: path,
            created_at: now(),
            updated_at: now(),
        });
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
            [&project.id.0],
            |r| r.get::<_, bool>(0),
        )? {
            Repository::new(&tx).insert_project(&project)?;
        }
        let config = ProjectConfig::default();
        tx.execute(
            "INSERT INTO project_root_bindings(project_id,identity) VALUES (?1,?2)",
            params![project.id.0, serde_json::to_string(&root)?],
        )?;
        tx.execute("INSERT INTO project_configs(project_id,revision,value,permission_digest) VALUES (?1,1,?2,?3)",params![project.id.0,serde_json::to_string(&config)?,digest("project-config-v1",&config)?])?;
        tx.execute(
            "INSERT INTO project_registration_keys(key,project_id,request_hash) VALUES (?1,?2,?3)",
            params![request.idempotency_key, project.id.0, hash],
        )?;
        tx.commit()?;
        Ok(project)
    }
    pub(crate) fn configure_project(
        &self,
        request: ConfigureProject,
    ) -> Result<ProjectConfigSnapshot> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old = config_tx(&tx, &request.project_id)?;
        if old.revision != request.expected_revision {
            return Err(ProjectError::RevisionConflict.into());
        }
        workspace_tx(&tx, &request.project_id)?;
        let settings: Settings = parse(&tx.query_row(
            "SELECT value FROM config WHERE id='settings'",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        let config = request.config.normalized(&settings)?;
        let revision = old
            .revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or(ProjectError::Corrupt)?;
        let permission_digest = digest("project-config-v1", &config)?;
        tx.execute("UPDATE project_configs SET revision=?1,value=?2,permission_digest=?3 WHERE project_id=?4",params![revision,serde_json::to_string(&config)?,permission_digest,request.project_id.0])?;
        tx.commit()?;
        Ok(ProjectConfigSnapshot {
            project_id: request.project_id,
            revision,
            config,
            permission_digest,
        })
    }
    pub(crate) fn select_project(&self, request: SelectProject) -> Result<ProjectSelection> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        config_tx(&tx, &request.project_id)?;
        let revision: u64 = tx.query_row(
            "SELECT revision FROM project_selection WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        if revision != request.expected_revision {
            return Err(ProjectError::RevisionConflict.into());
        }
        let revision = revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or(ProjectError::Corrupt)?;
        tx.execute(
            "UPDATE project_selection SET project_id=?1,revision=?2 WHERE id=1",
            params![request.project_id.0, revision],
        )?;
        tx.commit()?;
        Ok(ProjectSelection {
            project_id: Some(request.project_id),
            revision,
        })
    }
}
