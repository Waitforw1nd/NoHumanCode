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
    time::{timeout_at, Instant},
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
        let mut identity =
            serde_json::to_value(&self.common_dir).map_err(|_| GitWorktreeError::Invalid)?;
        identity["canonical_path"] =
            serde_json::Value::String(path_key(self.common_dir.canonical_path()));
        Ok(digest(
            &serde_json::to_vec(&identity).map_err(|_| GitWorktreeError::Invalid)?,
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
        excluded.push(verified(identity).map_err(|_| GitWorktreeError::Unavailable)?);
    }
    excluded.push(root.to_owned());
    excluded.extend(extra_roots.iter().map(|p| p.to_path_buf()));
    let executable = std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(if cfg!(windows) { "git.exe" } else { "git" }))
                .filter_map(|p| p.canonicalize().ok())
                .find(|p| p.is_file() && !excluded.iter().any(|r| within(p, r)))
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
        "branch.autoSetupRebase=never",
        "worktree.guessRemote=false",
        "core.sparseCheckout=false",
        "core.protectNTFS=true",
    ] {
        cmd.args(["-c", config]);
    }
    if let Some(p) = policy {
        verify_policy(p)?;
        cmd.arg("-c")
            .arg(format!("core.hooksPath={}", git_path(&p.hooks)?));
        cmd.arg("-c")
            .arg(format!("core.attributesFile={}", git_path(&p.attributes)?));
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
        if within(&claim.host_aux, &path) || within(&path, &claim.host_aux) {
            return Err(GitWorktreeError::Conflict);
        }
    }
    let target = claim.plan.target_path();
    if within(&claim.host_aux, &target) || within(&target, &claim.host_aux) {
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
    let admin_path = verified(git_dir)?;
    for path in [
        common_path.join("objects/info/alternates"),
        common_path.join("objects/info/http-alternates"),
        common_path.join("config.worktree"),
        admin_path.join("config.worktree"),
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
            &git_path(&config)?,
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
        // Git's assume-unchanged/skip-worktree bits can conceal dirty bytes.
        let flags = git(ctx, &root, policy, &["ls-files", "-v", "-z"], LIMIT).await?;
        if flags.code != Some(0) {
            return Err(GitWorktreeError::Unavailable);
        }
        if flags
            .bytes
            .split(|b| *b == 0)
            .filter(|v| !v.is_empty())
            .any(|v| v.first() != Some(&b'H'))
        {
            return Err(GitWorktreeError::Unsupported);
        }
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
        if within(&target, &path) || within(&path, &target) {
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
    registered_result_root: Option<&RootIdentity>,
    require_clean: bool,
) -> Result<CreatedWorktree> {
    let target = plan.target_path();
    let root = RootIdentity::capture(&target).map_err(|_| GitWorktreeError::Conflict)?;
    for prior in ctx.project_roots.iter().chain([
        &plan.source_root,
        &plan.repository.common_dir,
        &plan.repository.git_dir,
    ]) {
        if registered_result_root == Some(prior) && prior == &root {
            continue;
        }
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
    let observed = inspect_with(ctx, &root, policy, require_clean).await?;
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
        &git_path(&target)?,
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
        created(&claim.context, &claim.plan, Some(&policy), None, true).await
    }
    .await;
    result.map_err(|_| GitWorktreeError::OutcomeUnknown)
}
pub(crate) async fn diagnose(
    ctx: &GitContext,
    plan: &PersistedWorktreePlan,
    registered_result_root: Option<&RootIdentity>,
    registered_result_git_dir: Option<&RootIdentity>,
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
    // No artifact observation is trustworthy without every executable exclusion root.
    if ctx.project_roots.iter().any(|root| verified(root).is_err()) {
        return Ok(result);
    }
    match std::fs::symlink_metadata(plan.target_path()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match branch_available(ctx, plan, None).await {
                Ok(()) => match absent_registration(ctx, plan).await {
                    Ok(true) => {
                        result.state = S::AbsentObserved;
                        result.reason_codes = vec![R::Absent];
                    }
                    Ok(false) => {
                        result.state = S::Partial;
                        result.reason_codes = vec![R::PartialArtifacts];
                    }
                    Err(_) => {}
                },
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
                match created(ctx, plan, None, registered_result_root, false).await {
                    Ok(value) => {
                        if registered_result_git_dir.is_some_and(|saved| saved != &value.git_dir) {
                            result.state = S::IdentityChanged;
                            result.reason_codes = vec![R::IdentityChanged];
                            return Ok(result);
                        }
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

async fn absent_registration(ctx: &GitContext, plan: &PersistedWorktreePlan) -> Result<bool> {
    let common = verified(&plan.repository.common_dir)?;
    let target = plan.target_path();
    let list = git(
        ctx,
        plan.source_root.canonical_path(),
        None,
        &["worktree", "list", "--porcelain", "-z"],
        LIMIT,
    )
    .await?;
    if list.code != Some(0) {
        return Err(GitWorktreeError::Unavailable);
    }
    for field in list.bytes.split(|v| *v == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            let path = std::str::from_utf8(path).map_err(|_| GitWorktreeError::Unsupported)?;
            if same_path(Path::new(path), &target) {
                return Ok(false);
            }
        }
    }
    let admin = common.join("worktrees");
    match std::fs::symlink_metadata(&admin) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(_) => return Err(GitWorktreeError::Unavailable),
        Ok(meta) if link(&meta) || !meta.is_dir() => return Err(GitWorktreeError::Unsupported),
        Ok(_) => {}
    }
    let entries = std::fs::read_dir(admin).map_err(|_| GitWorktreeError::Unavailable)?;
    for (count, entry) in entries.enumerate() {
        if count >= 4096 {
            return Err(GitWorktreeError::Unsupported);
        }
        let path = entry.map_err(|_| GitWorktreeError::Unavailable)?.path();
        RootIdentity::capture(&path).map_err(|_| GitWorktreeError::Conflict)?;
        let bytes =
            regular_bytes(&path.join("gitdir"), 16384)?.ok_or(GitWorktreeError::Unsupported)?;
        let value = std::str::from_utf8(&bytes)
            .map_err(|_| GitWorktreeError::Unsupported)?
            .trim_end_matches(['\r', '\n']);
        if same_path(Path::new(value), &target.join(".git")) {
            return Ok(false);
        }
    }
    Ok(true)
}
fn git_path(path: &Path) -> Result<String> {
    let value = path.to_str().ok_or(GitWorktreeError::Invalid)?;
    #[cfg(windows)]
    {
        Ok(value
            .strip_prefix(r"\\?\")
            .unwrap_or(value)
            .replace('\\', "/"))
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
fn within(path: &Path, root: &Path) -> bool {
    let path = path_key(path);
    let mut root = path_key(root);
    if path == root {
        return true;
    }
    root.push(if cfg!(windows) { '\\' } else { '/' });
    path.starts_with(&root)
}
fn same_path(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as SyncCommand;
    struct Fixture {
        _temp: tempfile::TempDir,
        source: PathBuf,
        parent: PathBuf,
        aux: PathBuf,
        root: RootIdentity,
        context: GitContext,
    }
    fn raw(root: &Path, args: &[&str]) -> Vec<u8> {
        let output = SyncCommand::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let parent = temp.path().join("worktrees");
            let aux = temp.path().join("host-data");
            for path in [&source, &parent, &aux] {
                std::fs::create_dir(path).unwrap();
            }
            raw(&source, &["init", "--quiet"]);
            raw(&source, &["config", "user.name", "Fixture"]);
            raw(
                &source,
                &["config", "user.email", "fixture@example.invalid"],
            );
            raw(&source, &["config", "core.autocrlf", "false"]);
            std::fs::write(source.join("a.txt"), "baseline\n").unwrap();
            raw(&source, &["add", "--", "a.txt"]);
            raw(&source, &["commit", "--quiet", "-m", "baseline"]);
            let root = RootIdentity::capture(&source).unwrap();
            let context = GitContext::new(vec![root.clone()]).unwrap();
            Self {
                _temp: temp,
                source,
                parent,
                aux,
                root,
                context,
            }
        }
        async fn plan(&self) -> PreparedWorktree {
            let snapshot = inspect(&self.context, &self.root).await.unwrap();
            prepare(
                &self.context,
                &self.root,
                &RootIdentity::capture(&self.parent).unwrap(),
                &uuid::Uuid::new_v4().to_string(),
                &snapshot.head_oid,
            )
            .await
            .unwrap()
        }
    }
    #[tokio::test]
    async fn w10_present_result_with_unrelated_offline_root_is_unavailable() {
        for replace in [false, true] {
            let f = Fixture::new();
            let plan = f.plan().await.into_persisted_plan();
            let made = execute_claimed(
                ClaimedWorktree::new(
                    plan.clone(),
                    f.aux.join(&plan.operation_id),
                    vec![f.root.clone()],
                )
                .unwrap(),
            )
            .await
            .unwrap();
            let other = f._temp.path().join("other-project");
            std::fs::create_dir(&other).unwrap();
            let identity = RootIdentity::capture(&other).unwrap();
            let ctx = GitContext::new(vec![f.root.clone(), made.root.clone(), identity]).unwrap();
            let index = std::fs::read(made.git_dir.canonical_path().join("index")).unwrap();
            std::fs::rename(&other, f._temp.path().join("retained-original")).unwrap();
            if replace {
                std::fs::create_dir(&other).unwrap();
            }
            assert_eq!(
                diagnose(&ctx, &plan, Some(&made.root), Some(&made.git_dir))
                    .await
                    .unwrap()
                    .state,
                WorktreeObservationState::Unavailable
            );
            assert_eq!(
                std::fs::read(made.git_dir.canonical_path().join("index")).unwrap(),
                index
            );
            assert_eq!(
                std::fs::read(made.root.canonical_path().join("a.txt")).unwrap(),
                b"baseline\n"
            );
            std::fs::rename(&f.source, f._temp.path().join("source-moved")).unwrap();
            assert_eq!(
                diagnose(&ctx, &plan, Some(&made.root), Some(&made.git_dir))
                    .await
                    .unwrap()
                    .state,
                WorktreeObservationState::IdentityChanged
            );
        }
    }
    #[tokio::test]
    async fn w10_unrelated_missing_or_replaced_root_is_unavailable() {
        for replace in [false, true] {
            let f = Fixture::new();
            let plan = f.plan().await.into_persisted_plan();
            let other = f._temp.path().join("other-project");
            std::fs::create_dir(&other).unwrap();
            let identity = RootIdentity::capture(&other).unwrap();
            let ctx = GitContext::new(vec![f.root.clone(), identity]).unwrap();
            std::fs::rename(&other, f._temp.path().join("retained-original")).unwrap();
            if replace {
                std::fs::create_dir(&other).unwrap();
            }
            assert!(matches!(
                command(&ctx, &f.source, None, &[]),
                Err(GitWorktreeError::Unavailable)
            ));
            assert_eq!(
                diagnose(&ctx, &plan, None, None).await.unwrap().state,
                WorktreeObservationState::Unavailable
            );
            assert!(!plan.target_path().exists());
            assert!(!f.source.join(".git/worktrees").exists());
        }
    }
    #[tokio::test]
    async fn w04_real_config_over_limit_is_rejected_without_effects() {
        let f = Fixture::new();
        let config = f.source.join(".git/config");
        let mut bytes = std::fs::read(&config).unwrap();
        bytes.extend_from_slice(b"\n#");
        bytes.extend(std::iter::repeat_n(b'x', LIMIT));
        std::fs::write(&config, &bytes).unwrap();
        assert_eq!(
            inspect(&f.context, &f.root).await.unwrap_err(),
            GitWorktreeError::Unsupported
        );
        assert_eq!(std::fs::read(config).unwrap(), bytes);
        assert!(!f.source.join(".git/worktrees").exists());
    }
    #[tokio::test]
    async fn w01_dirty_result_diagnosis_preserves_content_and_index() {
        for mode in ["staged", "unstaged", "untracked"] {
            let f = Fixture::new();
            let plan = f.plan().await.into_persisted_plan();
            let made = execute_claimed(
                ClaimedWorktree::new(
                    plan.clone(),
                    f.aux.join(&plan.operation_id),
                    vec![f.root.clone()],
                )
                .unwrap(),
            )
            .await
            .unwrap();
            let target = made.root.canonical_path();
            let file = target.join(if mode == "untracked" {
                "new.txt"
            } else {
                "a.txt"
            });
            std::fs::write(&file, "user change\n").unwrap();
            if mode == "staged" {
                raw(target, &["add", "a.txt"]);
            }
            let index = std::fs::read(made.git_dir.canonical_path().join("index")).unwrap();
            let ctx = GitContext::new(vec![f.root.clone(), made.root.clone()]).unwrap();
            assert_eq!(
                diagnose(&ctx, &plan, Some(&made.root), Some(&made.git_dir))
                    .await
                    .unwrap()
                    .state,
                WorktreeObservationState::PresentMatching
            );
            assert_eq!(std::fs::read(&file).unwrap(), b"user change\n");
            assert_eq!(
                std::fs::read(made.git_dir.canonical_path().join("index")).unwrap(),
                index
            );
            assert_eq!(
                created(&ctx, &plan, None, Some(&made.root), true)
                    .await
                    .unwrap_err(),
                GitWorktreeError::Dirty
            );
            assert_eq!(std::fs::read(&file).unwrap(), b"user change\n");
            assert_eq!(
                std::fs::read(made.git_dir.canonical_path().join("index")).unwrap(),
                index
            );
        }
    }
    #[tokio::test]
    async fn w01_real_worktree_and_registered_result_diagnosis_are_read_only() {
        let f = Fixture::new();
        let plan = f.plan().await.into_persisted_plan();
        let old_index = std::fs::read(f.source.join(".git/index")).unwrap();
        let old_head = raw(&f.source, &["rev-parse", "HEAD"]);
        let old_refs = raw(&f.source, &["show-ref"]);
        let initial = diagnose(&f.context, &plan, None, None).await.unwrap();
        assert_eq!(initial.state, WorktreeObservationState::AbsentObserved);
        let value = execute_claimed(
            ClaimedWorktree::new(
                plan.clone(),
                f.aux.join(&plan.operation_id),
                vec![f.root.clone()],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(value.head_oid, plan.expected_head);
        assert_eq!(value.common_dir, plan.repository.common_dir);
        assert_ne!(value.git_dir, plan.repository.git_dir);
        assert_eq!(
            std::fs::read(value.root.canonical_path().join("a.txt")).unwrap(),
            b"baseline\n"
        );
        assert_eq!(
            std::fs::read(f.source.join(".git/index")).unwrap(),
            old_index
        );
        assert_eq!(raw(&f.source, &["rev-parse", "HEAD"]), old_head);
        assert_eq!(
            std::fs::read(f.source.join("a.txt")).unwrap(),
            b"baseline\n"
        );
        let refs = raw(&f.source, &["show-ref"]);
        assert_eq!(
            refs.split(|v| *v == b'\n')
                .filter(|s| !s.is_empty())
                .count(),
            old_refs
                .split(|v| *v == b'\n')
                .filter(|s| !s.is_empty())
                .count()
                + 1
        );
        let context = GitContext::new(vec![f.root.clone(), value.root.clone()]).unwrap();
        for _ in 0..2 {
            assert_eq!(
                diagnose(&context, &plan, Some(&value.root), Some(&value.git_dir))
                    .await
                    .unwrap()
                    .state,
                WorktreeObservationState::PresentMatching
            );
        }
        assert_ne!(
            diagnose(&context, &plan, None, None).await.unwrap().state,
            WorktreeObservationState::PresentMatching
        );
        assert_eq!(
            std::fs::read(f.source.join(".git/index")).unwrap(),
            old_index
        );
    }
    #[tokio::test]
    async fn w04_dirty_expected_head_and_existing_target_are_rejected() {
        for mode in ["unstaged", "staged", "untracked"] {
            let f = Fixture::new();
            if mode == "untracked" {
                std::fs::write(f.source.join("new.txt"), "new").unwrap();
            } else {
                std::fs::write(f.source.join("a.txt"), "change").unwrap();
                if mode == "staged" {
                    raw(&f.source, &["add", "a.txt"]);
                }
            }
            assert_eq!(
                inspect(&f.context, &f.root).await.unwrap_err(),
                GitWorktreeError::Dirty
            );
        }
        let f = Fixture::new();
        let plan = f.plan().await.into_persisted_plan();
        assert!(prepare(
            &f.context,
            &f.root,
            &plan.target_parent,
            &plan.operation_id,
            "HEAD~1"
        )
        .await
        .is_err());
        assert!(prepare(
            &f.context,
            &f.root,
            &plan.target_parent,
            &plan.operation_id,
            &"0".repeat(40)
        )
        .await
        .is_err());
        std::fs::create_dir(plan.target_path()).unwrap();
        assert!(prepare(
            &f.context,
            &f.root,
            &plan.target_parent,
            &plan.operation_id,
            &plan.expected_head
        )
        .await
        .is_err());
        assert!(std::fs::read_dir(f.aux).unwrap().next().is_none());
    }
    #[tokio::test]
    async fn w04_unsupported_configs_and_tree_modes_fail_before_worktree_effects() {
        for (key, value) in [
            ("filter.evil.smudge", "exit 99"),
            ("include.path", "absent"),
            ("core.sparseCheckout", "true"),
            ("extensions.worktreeConfig", "true"),
            ("remote.x.promisor", "true"),
            ("core.protectNTFS", "false"),
            ("submodule.x.url", "https://example.invalid"),
        ] {
            let f = Fixture::new();
            raw(&f.source, &["config", key, value]);
            assert_eq!(
                inspect(&f.context, &f.root).await.unwrap_err(),
                GitWorktreeError::Unsupported,
                "{key}"
            );
            assert!(!f.source.join(".git/worktrees").exists());
        }
        for mode in ["120000", "160000"] {
            let f = Fixture::new();
            let object = if mode == "160000" {
                String::from_utf8(raw(&f.source, &["rev-parse", "HEAD"])).unwrap()
            } else {
                String::from_utf8(raw(&f.source, &["rev-parse", "HEAD:a.txt"])).unwrap()
            };
            raw(
                &f.source,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("{mode},{},entry", object.trim()),
                ],
            );
            raw(&f.source, &["commit", "--quiet", "-m", "unsupported"]);
            assert_eq!(
                inspect(&f.context, &f.root).await.unwrap_err(),
                GitWorktreeError::Unsupported
            );
        }
    }
    #[tokio::test]
    async fn w10_missing_target_with_admin_residue_is_not_absent() {
        let f = Fixture::new();
        let plan = f.plan().await.into_persisted_plan();
        let admin = f.source.join(".git/worktrees/residue");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(
            admin.join("gitdir"),
            format!("{}\n", plan.target_path().join(".git").display()),
        )
        .unwrap();
        assert_ne!(
            diagnose(&f.context, &plan, None, None).await.unwrap().state,
            WorktreeObservationState::AbsentObserved
        );
        assert!(admin.exists());
    }
    #[tokio::test]
    async fn w05_real_hooks_and_fsmonitor_markers_are_not_executed() {
        let f = Fixture::new();
        let marker = f._temp.path().join("external-marker");
        let hooks = f._temp.path().join("evil-hooks");
        std::fs::create_dir(&hooks).unwrap();
        let script = format!(
            "#!/bin/sh\necho bad > '{}'\n",
            marker.to_string_lossy().replace('\\', "/")
        );
        for name in ["post-checkout", "reference-transaction"] {
            std::fs::write(hooks.join(name), &script).unwrap();
        }
        let monitor = f._temp.path().join("monitor.sh");
        std::fs::write(&monitor, &script).unwrap();
        raw(
            &f.source,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        raw(
            &f.source,
            &[
                "config",
                "core.fsmonitor",
                &format!("sh '{}'", monitor.to_string_lossy().replace('\\', "/")),
            ],
        );
        raw(&f.source, &["hook", "run", "post-checkout"]);
        assert!(marker.exists(), "hook positive control must execute");
        std::fs::remove_file(&marker).unwrap();
        raw(&f.source, &["status", "--porcelain"]);
        assert!(marker.exists(), "fsmonitor positive control must execute");
        std::fs::remove_file(&marker).unwrap();
        let plan = f.plan().await.into_persisted_plan();
        execute_claimed(
            ClaimedWorktree::new(
                plan.clone(),
                f.aux.join(&plan.operation_id),
                vec![f.root.clone()],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(!marker.exists());
    }
    #[tokio::test]
    async fn w06_nonzero_and_timeout_are_unknown_after_spawn() {
        let f = Fixture::new();
        for (script, timeout) in [
            (
                "[IO.File]::WriteAllText('pid',[string]$PID); [IO.File]::WriteAllText('partial','present'); exit 7",
                Duration::from_secs(3),
            ),
            (
                "[IO.File]::WriteAllText('pid',[string]$PID); [IO.File]::WriteAllText('partial','present'); Start-Sleep -Seconds 20",
                Duration::from_secs(1),
            ),
        ] {
            let mut cmd = Command::new("pwsh.exe");
            cmd.current_dir(&f.aux)
                .args(["-NoProfile", "-NonInteractive", "-Command", script])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            cmd.creation_flags(0x08000000);
            let start = Instant::now();
            let error = run(cmd, LIMIT, Instant::now() + timeout, true)
                .await
                .err()
                .unwrap();
            assert_eq!(error, GitWorktreeError::OutcomeUnknown);
            assert!(start.elapsed() < Duration::from_secs(7));
            assert_eq!(
                std::fs::read_to_string(f.aux.join("partial")).unwrap(),
                "present"
            );
            let pid = std::fs::read_to_string(f.aux.join("pid")).unwrap();
            assert_pid_absent(pid.trim().parse().unwrap()).await;
        }
    }
    #[tokio::test]
    async fn w04_special_repository_states_and_parent_replacement() {
        for mode in ["unborn", "bare", "sha256"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("repo");
            std::fs::create_dir(&root).unwrap();
            if mode == "bare" {
                raw(&root, &["init", "--bare", "--quiet"]);
            } else if mode == "sha256" {
                raw(&root, &["init", "--object-format=sha256", "--quiet"]);
            } else {
                raw(&root, &["init", "--quiet"]);
            }
            let root = RootIdentity::capture(&root).unwrap();
            let ctx = GitContext::new(vec![root.clone()]).unwrap();
            assert!(inspect(&ctx, &root).await.is_err(), "{mode}");
        }
        let f = Fixture::new();
        let plan = f.plan().await.into_persisted_plan();
        let saved = f.parent.with_file_name("old-parent");
        std::fs::rename(&f.parent, &saved).unwrap();
        std::fs::create_dir(&f.parent).unwrap();
        assert_eq!(
            execute_claimed(
                ClaimedWorktree::new(
                    plan.clone(),
                    f.aux.join(&plan.operation_id),
                    vec![f.root.clone()]
                )
                .unwrap()
            )
            .await
            .unwrap_err(),
            GitWorktreeError::Conflict
        );
        assert!(!plan.target_path().exists());
        let f = Fixture::new();
        raw(&f.source, &["update-index", "--assume-unchanged", "a.txt"]);
        std::fs::write(f.source.join("a.txt"), "hidden-dirty").unwrap();
        assert_eq!(
            inspect(&f.context, &f.root).await.unwrap_err(),
            GitWorktreeError::Unsupported
        );
    }
    #[tokio::test]
    async fn w01_sibling_parent_and_w10_real_deleted_target_admin_residue() {
        let f = Fixture::new();
        let snapshot = inspect(&f.context, &f.root).await.unwrap();
        let parent = RootIdentity::capture(f._temp.path()).unwrap();
        let plan = prepare(
            &f.context,
            &f.root,
            &parent,
            &uuid::Uuid::new_v4().to_string(),
            &snapshot.head_oid,
        )
        .await
        .unwrap()
        .into_persisted_plan();
        let made = execute_claimed(
            ClaimedWorktree::new(
                plan.clone(),
                f.aux.join(&plan.operation_id),
                vec![f.root.clone()],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            diagnose(&f.context, &plan, None, None).await.unwrap().state,
            WorktreeObservationState::PresentMatching
        );
        // Only this freshly-created disposable fixture tree is removed. Keep the
        // real Git admin entry, then delete only its fixture branch to expose D02.
        let target = plan.target_path();
        assert!(target.starts_with(f._temp.path().canonicalize().unwrap()));
        std::fs::remove_dir_all(&target).unwrap();
        raw(&f.source, &["update-ref", "-d", &plan.branch_ref]);
        assert!(made.git_dir.canonical_path().exists());
        assert!(!target.exists());
        assert_eq!(
            diagnose(&f.context, &plan, None, None).await.unwrap().state,
            WorktreeObservationState::Partial
        );
        assert!(made.git_dir.canonical_path().exists());
    }
    #[test]
    fn w04_structural_plan_validation_and_windows_path_aliases() {
        #[cfg(windows)]
        {
            assert!(same_path(
                Path::new(r"\\?\E:\Code\Repo"),
                Path::new("e:/code/repo")
            ));
            assert!(within(
                Path::new(r"\\?\E:\Code\Repo\git.exe"),
                Path::new("e:/code/repo")
            ));
            assert!(!within(
                Path::new("E:/code/repository"),
                Path::new("E:/code/repo")
            ));
        }
    }
    #[tokio::test]
    async fn w04_common_key_stable_across_windows_case_and_detached_head() {
        let f = Fixture::new();
        let snapshot = inspect(&f.context, &f.root).await.unwrap();
        #[cfg(windows)]
        {
            let alias =
                RootIdentity::capture(Path::new(&f.source.to_string_lossy().to_uppercase()))
                    .unwrap();
            let mut same = snapshot.clone();
            same.common_dir = RootIdentity::capture(Path::new(
                &snapshot
                    .common_dir
                    .canonical_path()
                    .to_string_lossy()
                    .to_uppercase(),
            ))
            .unwrap();
            assert_eq!(snapshot.common_key().unwrap(), same.common_key().unwrap());
            assert_eq!(alias.overlap(&f.root).unwrap(), RootOverlap::Same);
        }
        raw(&f.source, &["checkout", "--detach", "--quiet"]);
        let detached = inspect(&f.context, &f.root).await.unwrap();
        assert_eq!(detached.head_ref, None);
        let plan = f.plan().await.into_persisted_plan();
        execute_claimed(
            ClaimedWorktree::new(
                plan.clone(),
                f.aux.join(&plan.operation_id),
                vec![f.root.clone()],
            )
            .unwrap(),
        )
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn w05_filters_include_and_alternates_leave_external_marker_absent() {
        for mode in ["smudge", "process", "include", "alternates"] {
            let f = Fixture::new();
            let marker = f._temp.path().join("filter-marker");
            let script = f._temp.path().join("filter.sh");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\necho bad > '{}'\ncat\n",
                    marker.to_string_lossy().replace('\\', "/")
                ),
            )
            .unwrap();
            if mode == "smudge" || mode == "process" {
                std::fs::write(f.source.join(".gitattributes"), "a.txt filter=evil\n").unwrap();
                raw(&f.source, &["add", ".gitattributes"]);
                raw(&f.source, &["commit", "--quiet", "-m", "attributes"]);
            }
            if mode == "include" {
                let config = f._temp.path().join("included");
                std::fs::write(
                    &config,
                    format!(
                        "[filter \"evil\"]\n smudge = {}\n",
                        script.to_string_lossy().replace('\\', "/")
                    ),
                )
                .unwrap();
                raw(
                    &f.source,
                    &["config", "include.path", config.to_str().unwrap()],
                );
            } else if mode == "alternates" {
                std::fs::write(
                    f.source.join(".git/objects/info/alternates"),
                    "unavailable-object-pool",
                )
                .unwrap();
            } else {
                raw(
                    &f.source,
                    &[
                        "config",
                        &format!("filter.evil.{mode}"),
                        &format!("sh '{}'", script.to_string_lossy().replace('\\', "/")),
                    ],
                );
            }
            if mode == "smudge" || mode == "process" {
                let mut positive = Command::new("git");
                positive
                    .current_dir(&f.source)
                    .args(["cat-file", "--filters", "HEAD:a.txt"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .stdin(Stdio::null())
                    .kill_on_drop(true);
                let _ = run(
                    positive,
                    8192,
                    Instant::now() + Duration::from_secs(3),
                    false,
                )
                .await;
                assert!(
                    marker.exists(),
                    "Git filter {mode} positive control must launch script"
                );
                std::fs::remove_file(&marker).unwrap();
            }
            assert_eq!(
                inspect(&f.context, &f.root).await.unwrap_err(),
                GitWorktreeError::Unsupported
            );
            assert!(!marker.exists());
            assert!(!f.source.join(".git/worktrees").exists());
        }
    }

    async fn assert_pid_absent(pid: u32) {
        let mut query = Command::new("pwsh.exe");
        query
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("if(Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 9 }}"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        query.creation_flags(0x08000000);
        assert_eq!(
            run(query, 1024, Instant::now() + Duration::from_secs(5), false)
                .await
                .unwrap()
                .code,
            Some(0),
            "fixture process {pid} survived"
        );
    }
    #[tokio::test]
    async fn w06_output_overflow_terminates_and_reaps_real_child() {
        let f = Fixture::new();
        let mut cmd = Command::new("pwsh.exe");
        cmd.current_dir(&f.aux).args(["-NoProfile","-NonInteractive","-Command","[IO.File]::WriteAllText('pid',[string]$PID); [Console]::Out.Write(('x'*65536)); Start-Sleep -Seconds 20"]).stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        assert!(matches!(
            run(cmd, 1024, Instant::now() + Duration::from_secs(5), true).await,
            Err(GitWorktreeError::OutcomeUnknown)
        ));
        assert_pid_absent(
            std::fs::read_to_string(f.aux.join("pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        )
        .await;
    }
    #[tokio::test]
    async fn w06_inherited_pipe_deadline_keeps_partial_outcome_unknown() {
        let f = Fixture::new();
        std::fs::write(f.aux.join("descendant.ps1"),"[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'descendant-pid'),[string]$PID); Start-Sleep -Seconds 6").unwrap();
        let script = "[IO.File]::WriteAllText('parent-pid',[string]$PID); $p=[Diagnostics.ProcessStartInfo]::new('pwsh.exe'); $p.UseShellExecute=$false; $p.CreateNoWindow=$true; $p.ArgumentList.Add('-NoProfile'); $p.ArgumentList.Add('-File'); $p.ArgumentList.Add((Join-Path $PWD 'descendant.ps1')); $c=[Diagnostics.Process]::Start($p); $d=[DateTime]::UtcNow.AddSeconds(1); while(!(Test-Path 'descendant-pid')) { if([DateTime]::UtcNow -gt $d){exit 97}; Start-Sleep -Milliseconds 10 }; exit 0";
        let mut cmd = Command::new("pwsh.exe");
        cmd.current_dir(&f.aux)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        let begin = Instant::now();
        let result = run(cmd, 1024, Instant::now() + Duration::from_secs(2), true).await;
        let elapsed = begin.elapsed();
        let descendant: u32 = std::fs::read_to_string(f.aux.join("descendant-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut cleanup = Command::new("pwsh.exe");
        cleanup.args(["-NoProfile","-NonInteractive","-Command",&format!("$p=Get-Process -Id {descendant} -ErrorAction SilentlyContinue; if($p){{$p.Kill();$p.WaitForExit()}}")]).stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        cleanup.creation_flags(0x08000000);
        assert_eq!(
            run(
                cleanup,
                1024,
                Instant::now() + Duration::from_secs(5),
                false
            )
            .await
            .unwrap()
            .code,
            Some(0)
        );
        assert!(matches!(result, Err(GitWorktreeError::OutcomeUnknown)));
        assert!(elapsed < Duration::from_secs(4));
        assert_pid_absent(
            std::fs::read_to_string(f.aux.join("parent-pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        )
        .await;
        assert_pid_absent(descendant).await;
    }
}
