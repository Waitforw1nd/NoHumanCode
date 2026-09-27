//! Restricted local linked-worktree creation. Not an OS or repository sandbox.
use crate::workspace_identity::{RootIdentity, RootOverlap};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::{Instant, timeout_at},
};

const LIMIT: usize = 16 * 1024 * 1024;
type Result<T> = std::result::Result<T, GitWorktreeError>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitWorktreeError {
    Invalid,
    Unsupported,
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
            &serde_json::to_vec(&self.common_dir).map_err(|_| GitWorktreeError::Invalid)?,
        ))
    }
    fn validate(&self) -> Result<()> {
        self.git_dir
            .validate_snapshot()
            .map_err(|_| GitWorktreeError::Invalid)?;
        self.common_dir
            .validate_snapshot()
            .map_err(|_| GitWorktreeError::Invalid)?;
        if self.object_format != "sha1"
            || !oid(&self.head_oid)
            || !hex(&self.config_digest, 64)
            || self
                .head_ref
                .as_ref()
                .is_some_and(|r| !r.starts_with("refs/heads/") || r.contains(['\0', '\n', '\r']))
        {
            return Err(GitWorktreeError::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectGitObservation {
    NotGit,
    Repository(RepositorySnapshot),
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
            || !oid(&self.expected_head)
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
    host_aux: PathBuf,
    context: GitContext,
}
impl ClaimedWorktree {
    /// Only call after the durable claim commits. This constructor does not execute Git.
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
fn oid(value: &str) -> bool {
    hex(value, 40)
}
fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}
fn verified(root: &RootIdentity) -> Result<PathBuf> {
    root.verify()
        .map(|r| r.path().to_owned())
        .map_err(|_| GitWorktreeError::Conflict)
}
fn link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}
fn regular_bytes(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(GitWorktreeError::Unavailable),
    };
    if link(&meta) || !meta.is_file() || meta.len() > limit as u64 {
        return Err(GitWorktreeError::Unsupported);
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| GitWorktreeError::Unavailable)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| GitWorktreeError::Unavailable)?;
    if bytes.len() > limit {
        return Err(GitWorktreeError::Unsupported);
    }
    Ok(Some(bytes))
}
struct Output {
    code: Option<i32>,
    bytes: Vec<u8>,
}
async fn read_bound(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| GitWorktreeError::Unavailable)?;
    if bytes.len() > limit {
        return Err(GitWorktreeError::Unsupported);
    }
    Ok(bytes)
}
async fn run(
    mut command: Command,
    limit: usize,
    deadline: Instant,
    mutating: bool,
) -> Result<Output> {
    let mut child = command.spawn().map_err(|_| GitWorktreeError::Unavailable)?;
    let result = timeout_at(deadline, async {
        let stdout = child.stdout.take().ok_or(GitWorktreeError::Unavailable)?;
        let stderr = child.stderr.take().ok_or(GitWorktreeError::Unavailable)?;
        let (bytes, _, status) =
            tokio::try_join!(read_bound(stdout, limit), read_bound(stderr, 8192), async {
                child
                    .wait()
                    .await
                    .map_err(|_| GitWorktreeError::Unavailable)
            })?;
        Ok(Output {
            code: status.code(),
            bytes,
        })
    })
    .await;
    match result {
        Ok(Ok(out)) if !mutating || out.code == Some(0) => Ok(out),
        other => {
            let deadline = Instant::now() + Duration::from_secs(5);
            let _ = child.start_kill();
            let _ = timeout_at(deadline, child.wait()).await;
            if mutating {
                Err(GitWorktreeError::OutcomeUnknown)
            } else {
                match other {
                    Ok(Err(e)) => Err(e),
                    _ => Err(GitWorktreeError::Unavailable),
                }
            }
        }
    }
}
fn text(out: Output) -> Result<String> {
    if out.code != Some(0) {
        return Err(GitWorktreeError::Unavailable);
    }
    String::from_utf8(out.bytes)
        .map(|s| s.trim_end_matches(['\n', '\r']).to_owned())
        .map_err(|_| GitWorktreeError::Unsupported)
}
#[derive(Clone)]
struct Policy {
    hooks: PathBuf,
    attributes: PathBuf,
}
fn command(
    ctx: &GitContext,
    root: &Path,
    policy: Option<&Policy>,
    extra_roots: &[&Path],
) -> Result<Command> {
    let mut excluded = Vec::new();
    for identity in &ctx.project_roots {
        excluded.push(verified(identity)?);
    }
    excluded.push(root.to_owned());
    excluded.extend(extra_roots.iter().map(|p| p.to_path_buf()));
    let executable = std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(if cfg!(windows) { "git.exe" } else { "git" }))
                .filter_map(|p| p.canonicalize().ok())
                .find(|p| p.is_file() && !excluded.iter().any(|r| p.starts_with(r)))
        })
        .ok_or(GitWorktreeError::Unavailable)?;
    let mut cmd = Command::new(executable);
    cmd.env_clear();
    for name in ["PATH", "SystemRoot", "WINDIR", "SYSTEMDRIVE", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    cmd.current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C")
        .args(["--no-pager", "--no-optional-locks"]);
    for config in [
        "core.fsmonitor=false",
        "protocol.allow=never",
        "protocol.file.allow=never",
        "protocol.http.allow=never",
        "protocol.https.allow=never",
        "protocol.ssh.allow=never",
        "protocol.git.allow=never",
        "protocol.ext.allow=never",
        "credential.helper=",
        "diff.external=",
        "maintenance.auto=false",
        "gc.auto=0",
        "gc.autoDetach=false",
        "submodule.recurse=false",
        "fetch.recurseSubmodules=false",
        "checkout.recurseSubmodules=false",
        "branch.autoSetupMerge=false",
        "worktree.guessRemote=false",
        "core.sparseCheckout=false",
        "core.protectNTFS=true",
    ] {
        cmd.args(["-c", config]);
    }
    if let Some(p) = policy {
        verify_policy(p)?;
        cmd.arg("-c").arg(format!(
            "core.hooksPath={}",
            p.hooks.to_str().ok_or(GitWorktreeError::Invalid)?
        ));
        cmd.arg("-c").arg(format!(
            "core.attributesFile={}",
            p.attributes.to_str().ok_or(GitWorktreeError::Invalid)?
        ));
    } else {
        cmd.args([
            "-c",
            "core.hooksPath=",
            "-c",
            &format!("core.attributesFile={null}"),
        ]);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    Ok(cmd)
}
async fn git(
    ctx: &GitContext,
    root: &Path,
    policy: Option<&Policy>,
    args: &[&str],
    limit: usize,
) -> Result<Output> {
    let mut cmd = command(ctx, root, policy, &[])?;
    cmd.args(args);
    run(cmd, limit, Instant::now() + Duration::from_secs(10), false).await
}
fn verify_policy(policy: &Policy) -> Result<()> {
    RootIdentity::capture(&policy.hooks).map_err(|_| GitWorktreeError::Conflict)?;
    if std::fs::read_dir(&policy.hooks)
        .map_err(|_| GitWorktreeError::Unavailable)?
        .next()
        .is_some()
        || regular_bytes(&policy.attributes, 0)? != Some(vec![])
    {
        return Err(GitWorktreeError::Conflict);
    }
    Ok(())
}
fn aux_policy(claim: &ClaimedWorktree) -> Result<Policy> {
    let parent = claim.host_aux.parent().ok_or(GitWorktreeError::Invalid)?;
    let parent_identity = RootIdentity::capture(parent).map_err(|_| GitWorktreeError::Conflict)?;
    for root in claim
        .context
        .project_roots
        .iter()
        .chain([&claim.plan.source_root])
    {
        let overlap = parent_identity
            .overlap(root)
            .map_err(|_| GitWorktreeError::Conflict)?;
        if matches!(overlap, RootOverlap::Same | RootOverlap::RightContainsLeft) {
            return Err(GitWorktreeError::Conflict);
        }
    }
    for root in claim.context.project_roots.iter().chain([
        &claim.plan.source_root,
        &claim.plan.repository.git_dir,
        &claim.plan.repository.common_dir,
    ]) {
        let path = verified(root)?;
        if claim.host_aux.starts_with(&path) || path.starts_with(&claim.host_aux) {
            return Err(GitWorktreeError::Conflict);
        }
    }
    let target = claim.plan.target_path();
    if claim.host_aux.starts_with(&target) || target.starts_with(&claim.host_aux) {
        return Err(GitWorktreeError::Conflict);
    }
    if claim.host_aux.file_name().and_then(|v| v.to_str()) != Some(claim.plan.operation_id.as_str())
    {
        return Err(GitWorktreeError::Invalid);
    }
    std::fs::create_dir(&claim.host_aux).map_err(|_| GitWorktreeError::Conflict)?;
    let policy = Policy {
        hooks: claim.host_aux.join("hooks"),
        attributes: claim.host_aux.join("attributes"),
    };
    std::fs::create_dir(&policy.hooks).map_err(|_| GitWorktreeError::Unavailable)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&policy.attributes)
        .map_err(|_| GitWorktreeError::Unavailable)?;
    verify_policy(&policy)?;
    Ok(policy)
}
fn reject_config(bytes: &[u8]) -> Result<()> {
    for item in bytes.split(|v| *v == 0).filter(|v| !v.is_empty()) {
        let split = item
            .iter()
            .position(|v| *v == b'\n')
            .ok_or(GitWorktreeError::Unsupported)?;
        let key = std::str::from_utf8(&item[..split])
            .map_err(|_| GitWorktreeError::Unsupported)?
            .to_ascii_lowercase();
        let value = std::str::from_utf8(&item[split + 1..])
            .map_err(|_| GitWorktreeError::Unsupported)?
            .to_ascii_lowercase();
        if key.starts_with("include.")
            || key.starts_with("includeif.")
            || key.starts_with("filter.")
            || key.starts_with("extensions.")
            || key.starts_with("submodule.")
            || key.starts_with("sparse.")
            || key.ends_with(".promisor")
            || key == "core.worktree"
            || key.starts_with("core.sparsecheckout")
            || key == "checkout.recursesubmodules"
            || key == "core.repositoryformatversion" && value != "0"
            || key == "core.bare" && value != "false"
            || key == "core.protectntfs" && value != "true"
        {
            return Err(GitWorktreeError::Unsupported);
        }
    }
    Ok(())
}
async fn configuration(
    ctx: &GitContext,
    root: &Path,
    git_dir: &RootIdentity,
    common: &RootIdentity,
    policy: Option<&Policy>,
) -> Result<String> {
    let common_path = verified(common)?;
    let git_path = verified(git_dir)?;
    for path in [
        common_path.join("objects/info/alternates"),
        common_path.join("objects/info/http-alternates"),
        common_path.join("config.worktree"),
        git_path.join("config.worktree"),
    ] {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Err(GitWorktreeError::Unsupported),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(GitWorktreeError::Unavailable),
        }
    }
    let config = common_path.join("config");
    let before = regular_bytes(&config, LIMIT)?.ok_or(GitWorktreeError::Unsupported)?;
    let values = git(
        ctx,
        root,
        policy,
        &[
            "config",
            "--file",
            config.to_str().ok_or(GitWorktreeError::Invalid)?,
            "--no-includes",
            "--null",
            "--list",
        ],
        LIMIT,
    )
    .await?;
    if values.code != Some(0) {
        return Err(GitWorktreeError::Unsupported);
    }
    reject_config(&values.bytes)?;
    if regular_bytes(&config, LIMIT)?.as_ref() != Some(&before) {
        return Err(GitWorktreeError::Conflict);
    }
    Ok(digest(&before))
}
async fn inspect_with(
    ctx: &GitContext,
    source: &RootIdentity,
    policy: Option<&Policy>,
    clean: bool,
) -> Result<RepositorySnapshot> {
    let root = verified(source)?;
    let marker =
        std::fs::symlink_metadata(root.join(".git")).map_err(|_| GitWorktreeError::Unsupported)?;
    if link(&marker) || !(marker.is_file() || marker.is_dir()) {
        return Err(GitWorktreeError::Unsupported);
    }
    let top = text(git(ctx, &root, policy, &["rev-parse", "--show-toplevel"], 16384).await?)?;
    if RootIdentity::capture(Path::new(&top)).map_err(|_| GitWorktreeError::Conflict)? != *source {
        return Err(GitWorktreeError::Unsupported);
    }
    if text(
        git(
            ctx,
            &root,
            policy,
            &["rev-parse", "--is-bare-repository"],
            16,
        )
        .await?,
    )? != "false"
    {
        return Err(GitWorktreeError::Unsupported);
    }
    let git_dir = RootIdentity::capture(Path::new(&text(
        git(
            ctx,
            &root,
            policy,
            &["rev-parse", "--absolute-git-dir"],
            16384,
        )
        .await?,
    )?))
    .map_err(|_| GitWorktreeError::Conflict)?;
    let common_dir = RootIdentity::capture(Path::new(&text(
        git(
            ctx,
            &root,
            policy,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            16384,
        )
        .await?,
    )?))
    .map_err(|_| GitWorktreeError::Conflict)?;
    let config_digest = configuration(ctx, &root, &git_dir, &common_dir, policy).await?;
    let object_format = text(
        git(
            ctx,
            &root,
            policy,
            &["rev-parse", "--show-object-format"],
            32,
        )
        .await?,
    )?;
    if object_format != "sha1" {
        return Err(GitWorktreeError::Unsupported);
    }
    let head = git(
        ctx,
        &root,
        policy,
        &["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"],
        128,
    )
    .await?;
    if head.code != Some(0) {
        return Err(GitWorktreeError::Unsupported);
    }
    let head_oid = text(head)?;
    if !oid(&head_oid)
        || text(git(ctx, &root, policy, &["cat-file", "-t", &head_oid], 32).await?)? != "commit"
    {
        return Err(GitWorktreeError::Unsupported);
    }
    let symbolic = git(
        ctx,
        &root,
        policy,
        &["symbolic-ref", "--quiet", "HEAD"],
        4096,
    )
    .await?;
    let head_ref = match symbolic.code {
        Some(0) => Some(text(symbolic)?),
        Some(1) => None,
        _ => return Err(GitWorktreeError::Unavailable),
    };
    let tree = git(
        ctx,
        &root,
        policy,
        &["ls-tree", "-r", "-z", &head_oid],
        LIMIT,
    )
    .await?;
    if tree.code != Some(0) {
        return Err(GitWorktreeError::Unavailable);
    }
    for entry in tree.bytes.split(|v| *v == 0).filter(|v| !v.is_empty()) {
        if !(entry.starts_with(b"100644 ") || entry.starts_with(b"100755 ")) {
            return Err(GitWorktreeError::Unsupported);
        }
    }
    if clean {
        let state = git(
            ctx,
            &root,
            policy,
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            LIMIT,
        )
        .await?;
        if state.code != Some(0) {
            return Err(GitWorktreeError::Unavailable);
        }
        if !state.bytes.is_empty() {
            return Err(GitWorktreeError::Dirty);
        }
    }
    let current = text(
        git(
            ctx,
            &root,
            policy,
            &["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"],
            128,
        )
        .await?,
    )?;
    if current != head_oid
        || configuration(ctx, &root, &git_dir, &common_dir, policy).await? != config_digest
        || verified(source)? != root
    {
        return Err(GitWorktreeError::Conflict);
    }
    let result = RepositorySnapshot {
        git_dir,
        common_dir,
        head_oid,
        head_ref,
        object_format,
        config_digest,
    };
    result.validate()?;
    Ok(result)
}
pub(crate) async fn inspect(ctx: &GitContext, source: &RootIdentity) -> Result<RepositorySnapshot> {
    inspect_with(ctx, source, None, true).await
}
pub(crate) async fn probe_project_git(
    ctx: &GitContext,
    source: &RootIdentity,
) -> Result<ProjectGitObservation> {
    let root = verified(source)?;
    match std::fs::symlink_metadata(root.join(".git")) {
        Ok(_) => Ok(ProjectGitObservation::Repository(
            inspect_with(ctx, source, None, false).await?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Establish absence by walking verified ancestors; a Git exit code
            // 128 alone could also mean permission/configuration failure.
            for ancestor in root.ancestors() {
                RootIdentity::capture(ancestor).map_err(|_| GitWorktreeError::Unavailable)?;
                for marker in [
                    ancestor.join(".git"),
                    ancestor.join("HEAD"),
                    ancestor.join("objects"),
                ] {
                    match std::fs::symlink_metadata(marker) {
                        Ok(_) => return Err(GitWorktreeError::Unsupported),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => return Err(GitWorktreeError::Unavailable),
                    }
                }
            }
            // Resolve/validate host Git and all roots even for a NotGit observation.
            let _ = command(ctx, &root, None, &[])?;
            Ok(ProjectGitObservation::NotGit)
        }
        Err(_) => Err(GitWorktreeError::Unavailable),
    }
}
pub(crate) async fn prepare(
    ctx: &GitContext,
    source: &RootIdentity,
    parent: &RootIdentity,
    operation_id: &str,
    expected_head: &str,
) -> Result<PreparedWorktree> {
    if !canonical_uuid(operation_id) || !oid(expected_head) {
        return Err(GitWorktreeError::Invalid);
    }
    let repository = inspect(ctx, source).await?;
    if repository.head_oid != expected_head {
        return Err(GitWorktreeError::Conflict);
    }
    verified(parent)?;
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
    validate_target(ctx, &plan)?;
    branch_available(ctx, &plan, None).await?;
    Ok(PreparedWorktree { plan })
}
fn validate_target(ctx: &GitContext, plan: &PersistedWorktreePlan) -> Result<()> {
    let parent = verified(&plan.target_parent)?;
    let target = parent.join(&plan.target_leaf);
    match std::fs::symlink_metadata(&target) {
        Ok(_) => return Err(GitWorktreeError::Conflict),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(GitWorktreeError::Unavailable),
    }
    for root in ctx.project_roots.iter().chain([
        &plan.source_root,
        &plan.repository.git_dir,
        &plan.repository.common_dir,
    ]) {
        let path = verified(root)?;
        if target.starts_with(&path) || path.starts_with(&target) {
            return Err(GitWorktreeError::Conflict);
        }
    }
    Ok(())
}
async fn branch_available(
    ctx: &GitContext,
    plan: &PersistedWorktreePlan,
    policy: Option<&Policy>,
) -> Result<()> {
    let output = git(
        ctx,
        plan.source_root.canonical_path(),
        policy,
        &["show-ref", "--verify", "--quiet", &plan.branch_ref],
        128,
    )
    .await?;
    match output.code {
        Some(1) => Ok(()),
        Some(0) => Err(GitWorktreeError::Conflict),
        _ => Err(GitWorktreeError::Unavailable),
    }
}
async fn created(
    ctx: &GitContext,
    plan: &PersistedWorktreePlan,
    policy: Option<&Policy>,
) -> Result<CreatedWorktree> {
    let target = plan.target_path();
    let root = RootIdentity::capture(&target).map_err(|_| GitWorktreeError::Conflict)?;
    for prior in ctx.project_roots.iter().chain([
        &plan.source_root,
        &plan.repository.common_dir,
        &plan.repository.git_dir,
    ]) {
        if root
            .overlap(prior)
            .map_err(|_| GitWorktreeError::Conflict)?
            != RootOverlap::Disjoint
        {
            return Err(GitWorktreeError::Conflict);
        }
    }
    let marker = regular_bytes(&target.join(".git"), 16384)?.ok_or(GitWorktreeError::Conflict)?;
    if !marker.starts_with(b"gitdir: ") {
        return Err(GitWorktreeError::Conflict);
    }
    let observed = inspect_with(ctx, &root, policy, true).await?;
    if observed.common_dir != plan.repository.common_dir
        || observed.head_oid != plan.expected_head
        || observed.head_ref.as_ref() != Some(&plan.branch_ref)
    {
        return Err(GitWorktreeError::Conflict);
    }
    if !observed.git_dir.canonical_path().starts_with(
        plan.repository
            .common_dir
            .canonical_path()
            .join("worktrees"),
    ) {
        return Err(GitWorktreeError::Conflict);
    }
    let marker_path = std::str::from_utf8(&marker[8..])
        .map_err(|_| GitWorktreeError::Unsupported)?
        .trim_end_matches(['\r', '\n']);
    let marker_path = Path::new(marker_path);
    let resolved = if marker_path.is_absolute() {
        marker_path.to_owned()
    } else {
        target.join(marker_path)
    };
    if RootIdentity::capture(&resolved).map_err(|_| GitWorktreeError::Conflict)? != observed.git_dir
    {
        return Err(GitWorktreeError::Conflict);
    }
    let list = git(
        ctx,
        plan.source_root.canonical_path(),
        policy,
        &["worktree", "list", "--porcelain", "-z"],
        LIMIT,
    )
    .await?;
    if list.code != Some(0) {
        return Err(GitWorktreeError::Unavailable);
    }
    let mut matches = 0;
    let mut fields = Vec::new();
    for field in list.bytes.split(|v| *v == 0) {
        if field.is_empty() {
            if let Some(path) = fields
                .iter()
                .find_map(|v: &&[u8]| v.strip_prefix(b"worktree "))
            {
                let path = std::str::from_utf8(path).map_err(|_| GitWorktreeError::Unsupported)?;
                if Path::new(path).canonicalize().ok().as_deref() == Some(root.canonical_path()) {
                    if !fields.contains(&format!("HEAD {}", plan.expected_head).as_bytes())
                        || !fields.contains(&format!("branch {}", plan.branch_ref).as_bytes())
                    {
                        return Err(GitWorktreeError::Conflict);
                    }
                    matches += 1;
                }
            }
            fields.clear();
        } else {
            fields.push(field);
        }
    }
    if matches != 1 {
        return Err(GitWorktreeError::Conflict);
    }
    Ok(CreatedWorktree {
        root,
        git_dir: observed.git_dir,
        common_dir: observed.common_dir,
        head_oid: observed.head_oid,
        branch_ref: plan.branch_ref.clone(),
    })
}
pub(crate) async fn execute_claimed(claim: ClaimedWorktree) -> Result<CreatedWorktree> {
    claim.plan.validate()?;
    validate_target(&claim.context, &claim.plan)?;
    if inspect(&claim.context, &claim.plan.source_root).await? != claim.plan.repository {
        return Err(GitWorktreeError::Conflict);
    }
    branch_available(&claim.context, &claim.plan, None).await?;
    let policy = aux_policy(&claim)?;
    let root = verified(&claim.plan.source_root)?;
    let index = regular_bytes(
        &claim.plan.repository.git_dir.canonical_path().join("index"),
        LIMIT,
    )?;
    // Final pre-spawn observations follow policy preparation. No lock spans Git IO.
    validate_target(&claim.context, &claim.plan)?;
    if inspect_with(&claim.context, &claim.plan.source_root, Some(&policy), true).await?
        != claim.plan.repository
    {
        return Err(GitWorktreeError::Conflict);
    }
    let target = claim.plan.target_path();
    let mut cmd = command(&claim.context, &root, Some(&policy), &[&target])?;
    cmd.args([
        "worktree",
        "add",
        "--no-track",
        "-b",
        claim
            .plan
            .branch_ref
            .strip_prefix("refs/heads/")
            .ok_or(GitWorktreeError::Invalid)?,
        "--",
        target.to_str().ok_or(GitWorktreeError::Invalid)?,
        &claim.plan.expected_head,
    ]);
    run(cmd, LIMIT, Instant::now() + Duration::from_secs(30), true).await?;
    let result = async {
        if inspect_with(&claim.context, &claim.plan.source_root, Some(&policy), true).await?
            != claim.plan.repository
            || regular_bytes(
                &claim.plan.repository.git_dir.canonical_path().join("index"),
                LIMIT,
            )? != index
        {
            return Err(GitWorktreeError::Conflict);
        }
        created(&claim.context, &claim.plan, Some(&policy)).await
    }
    .await;
    result.map_err(|_| GitWorktreeError::OutcomeUnknown)
}
pub(crate) async fn diagnose(
    ctx: &GitContext,
    plan: &PersistedWorktreePlan,
) -> Result<WorktreeObservation> {
    plan.validate()?;
    use WorktreeObservationReason as R;
    use WorktreeObservationState as S;
    let mut result = WorktreeObservation {
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
        result.state = S::IdentityChanged;
        result.reason_codes = vec![R::IdentityChanged];
        return Ok(result);
    }
    match std::fs::symlink_metadata(plan.target_path()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match branch_available(ctx, plan, None).await {
                Ok(()) => {
                    result.state = S::AbsentObserved;
                    result.reason_codes = vec![R::Absent];
                }
                Err(GitWorktreeError::Conflict) => {
                    result.state = S::Partial;
                    result.reason_codes = vec![R::PartialArtifacts];
                }
                Err(_) => {}
            }
        }
        Ok(meta) => {
            if link(&meta) || !meta.is_dir() {
                result.state = S::Conflicting;
                result.reason_codes = vec![R::MetadataMismatch];
            } else {
                match created(ctx, plan, None).await {
                    Ok(value) => {
                        result.state = S::PresentMatching;
                        result.reason_codes.clear();
                        result.observed_head = Some(value.head_oid);
                        result.observed_branch = Some(value.branch_ref);
                    }
                    Err(
                        GitWorktreeError::Conflict
                        | GitWorktreeError::Unsupported
                        | GitWorktreeError::Dirty,
                    ) => {
                        result.state = S::Partial;
                        result.reason_codes = vec![R::PartialArtifacts];
                    }
                    Err(_) => {}
                }
            }
        }
        Err(_) => {}
    }
    Ok(result)
}
