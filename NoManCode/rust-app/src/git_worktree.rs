//! Fast local linked-worktree creation supporting LFS, submodules, symlinks, SHA-256, and dirty trees.
use crate::workspace_identity::{RootIdentity, RootOverlap};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::Command;

const LIMIT: usize = 1024 * 1024;
type Result<T> = std::result::Result<T, GitWorktreeError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitWorktreeError {
    Invalid,
    Unsupported,
    #[allow(dead_code)]
    Dirty,
    Conflict,
    Unavailable,
    OutcomeUnknown,
}

impl std::fmt::Display for GitWorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "Invalid worktree request",
            Self::Unsupported => "Unsupported local Git repository",
            Self::Dirty => "Source worktree is not clean",
            Self::Conflict => "Worktree inputs changed or conflict",
            Self::Unavailable => "Local Git observation unavailable",
            Self::OutcomeUnknown => "Git worktree outcome unknown; inspect without retry",
        })
    }
}

impl std::error::Error for GitWorktreeError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepositorySnapshot {
    pub git_dir: RootIdentity,
    pub common_dir: RootIdentity,
    pub head_oid: String,
    pub head_ref: Option<String>,
    pub object_format: String,
    pub config_digest: String,
}

impl RepositorySnapshot {
    pub fn common_key(&self) -> Result<String> {
        self.common_dir
            .validate_snapshot()
            .map_err(|_| GitWorktreeError::Invalid)?;
        Ok(digest(
            path_key(self.common_dir.canonical_path()).as_bytes(),
        ))
    }

    fn validate(&self) -> Result<()> {
        self.git_dir
            .validate_snapshot()
            .map_err(|_| GitWorktreeError::Invalid)?;
        self.common_dir
            .validate_snapshot()
            .map_err(|_| GitWorktreeError::Invalid)?;
        if !matches!(self.object_format.as_str(), "sha1" | "sha256")
            || !valid_oid(&self.head_oid)
            || !hex(&self.config_digest, 64)
        {
            return Err(GitWorktreeError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectGitObservation {
    NotGit,
    Repository(Box<RepositorySnapshot>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedWorktreePlan {
    pub version: u32,
    pub operation_id: String,
    pub source_root: RootIdentity,
    pub target_parent: RootIdentity,
    pub repository: RepositorySnapshot,
    pub expected_head: String,
    pub branch_ref: String,
    pub target_leaf: String,
    pub policy_version: u32,
}

impl PersistedWorktreePlan {
    pub fn validate(&self) -> Result<()> {
        self.source_root
            .validate_snapshot()
            .map_err(|_| GitWorktreeError::Invalid)?;
        self.target_parent
            .validate_snapshot()
            .map_err(|_| GitWorktreeError::Invalid)?;
        self.repository.validate()?;
        if self.version != 1
            || self.policy_version != 1
            || !canonical_uuid(&self.operation_id)
            || !valid_oid(&self.expected_head)
            || self.repository.head_oid != self.expected_head
            || self.branch_ref != format!("refs/heads/codex/workspace/{}", self.operation_id)
            || self.target_leaf != format!("nhc-{}", self.operation_id)
        {
            return Err(GitWorktreeError::Invalid);
        }
        Ok(())
    }

    pub fn target_path(&self) -> PathBuf {
        self.target_parent.canonical_path().join(&self.target_leaf)
    }
}

pub(crate) struct PreparedWorktree {
    plan: PersistedWorktreePlan,
}

impl PreparedWorktree {
    pub fn plan(&self) -> &PersistedWorktreePlan {
        &self.plan
    }

    pub fn into_persisted_plan(self) -> PersistedWorktreePlan {
        self.plan
    }
}

#[derive(Clone)]
pub(crate) struct GitContext {
    #[allow(dead_code)]
    project_roots: Vec<RootIdentity>,
}

impl GitContext {
    pub fn new(project_roots: Vec<RootIdentity>) -> Result<Self> {
        for root in &project_roots {
            root.validate_snapshot()
                .map_err(|_| GitWorktreeError::Invalid)?;
        }
        Ok(Self { project_roots })
    }
}

pub(crate) struct ClaimedWorktree {
    plan: PersistedWorktreePlan,
    #[allow(dead_code)]
    host_aux: PathBuf,
    #[allow(dead_code)]
    context: GitContext,
}

impl ClaimedWorktree {
    pub fn new(
        plan: PersistedWorktreePlan,
        host_aux: PathBuf,
        project_roots: Vec<RootIdentity>,
    ) -> Result<Self> {
        plan.validate()?;
        if !host_aux.is_absolute() {
            return Err(GitWorktreeError::Invalid);
        }
        Ok(Self {
            plan,
            host_aux,
            context: GitContext::new(project_roots)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreatedWorktree {
    pub root: RootIdentity,
    pub git_dir: RootIdentity,
    pub common_dir: RootIdentity,
    pub head_oid: String,
    pub branch_ref: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorktreeObservationState {
    AbsentObserved,
    PresentMatching,
    Partial,
    Conflicting,
    IdentityChanged,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorktreeObservationReason {
    Absent,
    MetadataMismatch,
    IdentityChanged,
    GitUnavailable,
    PartialArtifacts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorktreeObservation {
    pub operation_id: String,
    pub observed_at: i64,
    pub state: WorktreeObservationState,
    pub reason_codes: Vec<WorktreeObservationReason>,
    pub observed_head: Option<String>,
    pub observed_branch: Option<String>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}

pub(crate) fn valid_oid(value: &str) -> bool {
    hex(value, 40) || hex(value, 64)
}

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

fn verified(root: &RootIdentity) -> Result<PathBuf> {
    root.verify()
        .map(|r| r.path().to_owned())
        .map_err(|_| GitWorktreeError::Conflict)
}

fn git_command(cwd: &Path) -> Result<Command> {
    let executable = std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(if cfg!(windows) { "git.exe" } else { "git" }))
                .filter_map(|candidate| candidate.canonicalize().ok())
                .find(|candidate| candidate.is_file() && !candidate.starts_with(cwd))
        })
        .ok_or(GitWorktreeError::Unavailable)?;

    let mut cmd = Command::new(executable);
    cmd.env_clear();
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "SYSTEMDRIVE",
        "TEMP",
        "TMP",
        "HOME",
        "USERPROFILE",
    ] {
        if let Some(val) = std::env::var_os(name) {
            cmd.env(name, val);
        }
    }
    cmd.current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .args(["--no-pager", "-c", "core.hooksPath="])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(cmd)
}

struct Output {
    code: Option<i32>,
    bytes: Vec<u8>,
}

async fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Result<Output> {
    use crate::process_lifetime::{CompletionReason, Outcome};
    let mut cmd = git_command(cwd)?;
    cmd.args(args);
    let spec = crate::execution_authority::command_spec(&cmd, LIMIT, Instant::now() + timeout)
        .map_err(|_| GitWorktreeError::Unavailable)?;

    let outcome = if crate::execution_authority::current().is_some() {
        crate::execution_authority::run_process(spec)
            .await
            .map_err(|_| GitWorktreeError::Unavailable)?
    } else {
        crate::process_lifetime::run_direct(spec).await
    };

    match outcome {
        Outcome::Settled(result) if result.reason == CompletionReason::Exited => Ok(Output {
            code: Some(result.exit_code as i32),
            bytes: result.stdout.bytes,
        }),
        Outcome::Unknown => Err(GitWorktreeError::OutcomeUnknown),
        _ => Err(GitWorktreeError::Unavailable),
    }
}

fn resolve_relative_to(base: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn read_symbolic_head(git_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let trimmed = raw.trim_end_matches(['\r', '\n']);
    let target = trimmed.strip_prefix("ref: ")?;
    target.starts_with("refs/heads/").then(|| target.to_owned())
}

/// Command #1: Fast single `git rev-parse` to inspect repository toplevel, git-dir, common-dir, and HEAD.
pub(crate) async fn inspect(
    _ctx: &GitContext,
    source: &RootIdentity,
) -> Result<RepositorySnapshot> {
    let root = verified(source)?;
    let out = run_git(
        &root,
        &[
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
            "HEAD",
        ],
        Duration::from_secs(10),
    )
    .await?;
    if out.code != Some(0) {
        return Err(GitWorktreeError::Unsupported);
    }
    let text = String::from_utf8(out.bytes).map_err(|_| GitWorktreeError::Unsupported)?;
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    if lines.len() < 4 {
        return Err(GitWorktreeError::Unsupported);
    }

    let top_identity =
        RootIdentity::capture(Path::new(lines[0])).map_err(|_| GitWorktreeError::Unsupported)?;
    if top_identity != *source {
        return Err(GitWorktreeError::Unsupported);
    }

    let git_dir_path = resolve_relative_to(&root, lines[1]);
    let common_dir_path = resolve_relative_to(&root, lines[2]);
    let git_dir = RootIdentity::capture(&git_dir_path).map_err(|_| GitWorktreeError::Conflict)?;
    let common_dir =
        RootIdentity::capture(&common_dir_path).map_err(|_| GitWorktreeError::Conflict)?;

    let mut head_oid = lines[3].to_ascii_lowercase();
    if !valid_oid(&head_oid) {
        return Err(GitWorktreeError::Unsupported);
    }
    let object_format = if head_oid.len() == 64 {
        "sha256".to_owned()
    } else {
        head_oid.truncate(40);
        "sha1".to_owned()
    };

    let head_ref = read_symbolic_head(git_dir.canonical_path());
    let config_digest = digest(path_key(common_dir.canonical_path()).as_bytes());

    let snapshot = RepositorySnapshot {
        git_dir,
        common_dir,
        head_oid,
        head_ref,
        object_format,
        config_digest,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

/// Lightweight local check without spawning subprocesses or failing on unrelated projects.
pub(crate) async fn probe_project_git(
    _ctx: &GitContext,
    source: &RootIdentity,
) -> Result<ProjectGitObservation> {
    let root = source.canonical_path();
    let dot_git = root.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
        return Ok(ProjectGitObservation::NotGit);
    };

    let git_dir_path = if meta.is_dir() {
        dot_git
    } else if meta.is_file() {
        let Ok(raw) = std::fs::read_to_string(&dot_git) else {
            return Ok(ProjectGitObservation::NotGit);
        };
        let Some(rest) = raw.trim_end_matches(['\r', '\n']).strip_prefix("gitdir: ") else {
            return Ok(ProjectGitObservation::NotGit);
        };
        resolve_relative_to(root, rest)
    } else {
        return Ok(ProjectGitObservation::NotGit);
    };

    let common_dir_path = match std::fs::read_to_string(git_dir_path.join("commondir")) {
        Ok(raw) => resolve_relative_to(&git_dir_path, raw.trim_end_matches(['\r', '\n'])),
        Err(_) => git_dir_path.clone(),
    };

    let (Ok(git_dir), Ok(common_dir)) = (
        RootIdentity::capture(&git_dir_path),
        RootIdentity::capture(&common_dir_path),
    ) else {
        return Ok(ProjectGitObservation::NotGit);
    };

    let dummy_oid = "0".repeat(40);
    let config_digest = digest(path_key(common_dir.canonical_path()).as_bytes());
    Ok(ProjectGitObservation::Repository(Box::new(
        RepositorySnapshot {
            git_dir,
            common_dir,
            head_oid: dummy_oid,
            head_ref: read_symbolic_head(&git_dir_path),
            object_format: "sha1".into(),
            config_digest,
        },
    )))
}

pub(crate) async fn prepare(
    ctx: &GitContext,
    source: &RootIdentity,
    parent: &RootIdentity,
    operation_id: &str,
    expected_head: &str,
) -> Result<PreparedWorktree> {
    if !canonical_uuid(operation_id) || !valid_oid(expected_head) {
        return Err(GitWorktreeError::Invalid);
    }
    let repository = inspect(ctx, source).await?;
    if repository.head_oid != expected_head {
        return Err(GitWorktreeError::Conflict);
    }
    let parent_dir = verified(parent)?;
    let target = parent_dir.join(format!("nhc-{operation_id}"));
    if target.exists() {
        return Err(GitWorktreeError::Conflict);
    }
    if parent
        .overlap(source)
        .map_err(|_| GitWorktreeError::Conflict)?
        == RootOverlap::RightContainsLeft
    {
        return Err(GitWorktreeError::Conflict);
    }

    let plan = PersistedWorktreePlan {
        version: 1,
        operation_id: operation_id.into(),
        source_root: source.clone(),
        target_parent: parent.clone(),
        repository,
        expected_head: expected_head.into(),
        branch_ref: format!("refs/heads/codex/workspace/{operation_id}"),
        target_leaf: format!("nhc-{operation_id}"),
        policy_version: 1,
    };
    plan.validate()?;
    Ok(PreparedWorktree { plan })
}

/// Commands #2 and #3: `git worktree add -b <branch> <target> <head>` + `git rev-parse HEAD` in target.
pub(crate) async fn execute_claimed(claim: ClaimedWorktree) -> Result<CreatedWorktree> {
    claim.plan.validate()?;
    let source_root = verified(&claim.plan.source_root)?;
    let _parent_dir = verified(&claim.plan.target_parent)?;
    let target = claim.plan.target_path();
    if target.exists() {
        return Err(GitWorktreeError::Conflict);
    }

    let branch = claim
        .plan
        .branch_ref
        .strip_prefix("refs/heads/")
        .ok_or(GitWorktreeError::Invalid)?;
    let target_arg = git_path(&target)?;

    let add = run_git(
        &source_root,
        &[
            "worktree",
            "add",
            "--no-track",
            "-b",
            branch,
            "--",
            &target_arg,
            &claim.plan.expected_head,
        ],
        Duration::from_secs(30),
    )
    .await?;

    if add.code != Some(0) {
        if !target.exists() {
            return Err(GitWorktreeError::Conflict);
        }
        return Err(GitWorktreeError::OutcomeUnknown);
    }

    observe_created_worktree(&claim.plan, &target).await
}

async fn observe_created_worktree(
    plan: &PersistedWorktreePlan,
    target: &Path,
) -> Result<CreatedWorktree> {
    let root = RootIdentity::capture(target).map_err(|_| GitWorktreeError::Conflict)?;
    let out = run_git(
        root.canonical_path(),
        &[
            "rev-parse",
            "--absolute-git-dir",
            "--git-common-dir",
            "HEAD",
        ],
        Duration::from_secs(10),
    )
    .await?;
    if out.code != Some(0) {
        return Err(GitWorktreeError::Conflict);
    }
    let text = String::from_utf8(out.bytes).map_err(|_| GitWorktreeError::Conflict)?;
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    if lines.len() < 3 {
        return Err(GitWorktreeError::Conflict);
    }

    let git_dir_path = resolve_relative_to(root.canonical_path(), lines[0]);
    let common_dir_path = resolve_relative_to(root.canonical_path(), lines[1]);
    let head_oid = lines[2].to_ascii_lowercase();

    let git_dir = RootIdentity::capture(&git_dir_path).map_err(|_| GitWorktreeError::Conflict)?;
    let common_dir =
        RootIdentity::capture(&common_dir_path).map_err(|_| GitWorktreeError::Conflict)?;
    let branch_ref =
        read_symbolic_head(git_dir.canonical_path()).unwrap_or_else(|| plan.branch_ref.clone());

    if common_dir != plan.repository.common_dir
        || head_oid != plan.expected_head
        || branch_ref != plan.branch_ref
    {
        return Err(GitWorktreeError::Conflict);
    }

    Ok(CreatedWorktree {
        root,
        git_dir,
        common_dir,
        head_oid,
        branch_ref,
    })
}

pub(crate) async fn diagnose(
    _ctx: &GitContext,
    plan: &PersistedWorktreePlan,
    registered_result_root: Option<&RootIdentity>,
    registered_result_git_dir: Option<&RootIdentity>,
) -> Result<WorktreeObservation> {
    plan.validate()?;
    use WorktreeObservationReason as R;
    use WorktreeObservationState as S;

    let mut obs = WorktreeObservation {
        operation_id: plan.operation_id.clone(),
        observed_at: crate::domain::now() as i64,
        state: S::Unavailable,
        reason_codes: vec![R::GitUnavailable],
        observed_head: None,
        observed_branch: None,
    };

    if verified(&plan.source_root).is_err()
        || verified(&plan.target_parent).is_err()
        || verified(&plan.repository.common_dir).is_err()
        || verified(&plan.repository.git_dir).is_err()
    {
        obs.state = S::IdentityChanged;
        obs.reason_codes = vec![R::IdentityChanged];
        return Ok(obs);
    }

    let target = plan.target_path();
    match std::fs::symlink_metadata(&target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            obs.state = S::AbsentObserved;
            obs.reason_codes = vec![R::Absent];
        }
        Ok(meta) if !meta.is_dir() => {
            obs.state = S::Conflicting;
            obs.reason_codes = vec![R::MetadataMismatch];
        }
        Ok(_) => match observe_created_worktree(plan, &target).await {
            Ok(created) => {
                if registered_result_root.is_some_and(|saved| saved != &created.root)
                    || registered_result_git_dir.is_some_and(|saved| saved != &created.git_dir)
                {
                    obs.state = S::IdentityChanged;
                    obs.reason_codes = vec![R::IdentityChanged];
                } else {
                    obs.state = S::PresentMatching;
                    obs.reason_codes.clear();
                    obs.observed_head = Some(created.head_oid);
                    obs.observed_branch = Some(created.branch_ref);
                }
            }
            Err(_) => {
                obs.state = S::Partial;
                obs.reason_codes = vec![R::PartialArtifacts];
            }
        },
        Err(_) => {}
    }
    Ok(obs)
}

fn git_path(path: &Path) -> Result<String> {
    let value = path.to_str().ok_or(GitWorktreeError::Invalid)?;
    #[cfg(windows)]
    {
        let stripped = if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
            format!("//{unc}")
        } else {
            value.strip_prefix(r"\\?\").unwrap_or(value).to_owned()
        };
        Ok(stripped.replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        Ok(value.to_owned())
    }
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    #[cfg(windows)]
    {
        value
            .replace('/', "\\")
            .trim_start_matches("\\\\?\\")
            .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        value.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_both_sha1_and_sha256_oids() {
        assert!(valid_oid(&"a".repeat(40)));
        assert!(valid_oid(&"f".repeat(64)));
        assert!(!valid_oid(&"g".repeat(40)));
        assert!(!valid_oid(&"a".repeat(39)));
    }
}
