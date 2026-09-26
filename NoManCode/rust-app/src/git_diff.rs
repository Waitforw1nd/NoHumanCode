//! Local, read-only Git review. Patches are review data, never restore inputs.
use crate::{secrets, workspace};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt};

const SIDE_LIMIT: usize = 256 * 1024;
const PATCH_LIMIT: usize = 512 * 1024;
const RESPONSE_LIMIT: usize = 1024 * 1024 - 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitDiffView {
    Staged,
    Unstaged,
    Head,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitDiffRequest {
    pub path: String,
    pub view: GitDiffView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitDiffStatus {
    Text,
    Unchanged,
    Binary,
    TooLarge,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiff {
    pub path: String,
    pub view: GitDiffView,
    pub base_oid: Option<String>,
    pub index_oid: Option<String>,
    pub before_digest: Option<String>,
    pub after_digest: Option<String>,
    pub status: GitDiffStatus,
    pub patch: Option<String>,
    pub redacted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitDiffError {
    InvalidPath,
    Unavailable,
    Unsupported,
    Conflict,
    Internal,
}
impl std::fmt::Display for GitDiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidPath => "Invalid Git review path",
            Self::Unavailable => "Local Git review is unavailable",
            Self::Unsupported => "Git review does not support this entry",
            Self::Conflict => "Git review inputs changed; request a fresh review",
            Self::Internal => "Git review failed",
        })
    }
}
impl std::error::Error for GitDiffError {}

pub(crate) struct GitOutput {
    code: Option<i32>,
    bytes: Vec<u8>,
}

/// Whitelist the OS runtime environment. In particular, do not inherit *any*
/// GIT_*, HOME, XDG_CONFIG_HOME, pager, SSH or credential helper variables.
fn git_command(root: &Path) -> Result<tokio::process::Command, GitDiffError> {
    // Resolve from absolute PATH entries before setting cwd. Windows otherwise
    // searches cwd for git.exe, which would execute a repository-owned binary.
    let executable = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .filter(|directory| directory.is_absolute())
                .map(|directory| directory.join(if cfg!(windows) { "git.exe" } else { "git" }))
                .filter_map(|candidate| candidate.canonicalize().ok())
                .find(|candidate| candidate.is_file() && !candidate.starts_with(root))
        })
        .ok_or(GitDiffError::Unavailable)?;
    let mut command = tokio::process::Command::new(executable);
    command.env_clear();
    for name in ["PATH", "SystemRoot", "WINDIR", "SYSTEMDRIVE", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C")
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=never",
            "-c",
            "protocol.http.allow=never",
            "-c",
            "protocol.https.allow=never",
            "-c",
            "protocol.ssh.allow=never",
            "-c",
            "protocol.git.allow=never",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "core.hooksPath=",
            "-c",
            "credential.helper=",
            "-c",
            "diff.external=",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(command)
}

async fn bounded_read(
    reader: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, GitDiffError> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| GitDiffError::Unavailable)?;
    if bytes.len() > limit {
        return Err(GitDiffError::Unavailable);
    }
    Ok(bytes)
}

pub(crate) async fn run_command(
    mut command: tokio::process::Command,
    limit: usize,
    timeout: Duration,
) -> Result<GitOutput, GitDiffError> {
    let mut child = command.spawn().map_err(|_| GitDiffError::Unavailable)?;
    let stdout = child.stdout.take().ok_or(GitDiffError::Internal)?;
    let stderr = child.stderr.take().ok_or(GitDiffError::Internal)?;
    let result = tokio::time::timeout(timeout, async {
        let (bytes, _, status) = tokio::try_join!(
            bounded_read(stdout, limit),
            bounded_read(stderr, 8192),
            async { child.wait().await.map_err(|_| GitDiffError::Unavailable) }
        )?;
        Ok::<GitOutput, GitDiffError>(GitOutput {
            code: status.code(),
            bytes,
        })
    })
    .await;
    match result {
        Ok(Ok(output)) => Ok(output),
        _ => {
            // Both timeout and output overflow must terminate AND reap the child.
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(GitDiffError::Unavailable)
        }
    }
}

async fn git(root: &Path, args: &[&str], limit: usize) -> Result<GitOutput, GitDiffError> {
    let mut command = git_command(root)?;
    // check-ignore takes literal filesystem names, not pathspecs, and Git
    // rejects GIT_LITERAL_PATHSPECS for this plumbing command.
    if args.first() == Some(&"check-ignore") {
        command.env_remove("GIT_LITERAL_PATHSPECS");
    }
    command.args(args);
    run_command(command, limit, Duration::from_secs(10)).await
}

fn text(output: GitOutput) -> Result<String, GitDiffError> {
    if output.code != Some(0) {
        return Err(GitDiffError::Unavailable);
    }
    String::from_utf8(output.bytes)
        .map(|v| v.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|_| GitDiffError::Unavailable)
}

fn valid_oid(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn head(root: &Path) -> Result<Option<String>, GitDiffError> {
    let value = git(root, &["rev-parse", "--verify", "--quiet", "HEAD"], 128).await?;
    if value.code == Some(0) {
        let oid = text(value)?;
        if !valid_oid(&oid) {
            return Err(GitDiffError::Unavailable);
        }
        return Ok(Some(oid));
    }
    // A failed rev-parse is only an empty base for a genuinely unborn branch.
    let reference = text(git(root, &["symbolic-ref", "--quiet", "HEAD"], 4096).await?)?;
    if !reference.starts_with("refs/heads/") {
        return Err(GitDiffError::Unavailable);
    }
    let lookup = git(root, &["show-ref", "--verify", "--quiet", &reference], 128).await?;
    if lookup.code == Some(1) {
        Ok(None)
    } else {
        Err(GitDiffError::Unavailable)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    mode: String,
    oid: String,
}

fn entry(bytes: &[u8], path: &str, index: bool) -> Result<Option<Entry>, GitDiffError> {
    let mut records = bytes.split(|b| *b == 0).filter(|v| !v.is_empty());
    let Some(record) = records.next() else {
        return Ok(None);
    };
    if records.next().is_some() {
        return Err(GitDiffError::Unsupported);
    }
    let (header, filename) = record.split_at(
        record
            .iter()
            .position(|b| *b == b'\t')
            .ok_or(GitDiffError::Unavailable)?,
    );
    if &filename[1..] != path.as_bytes() {
        return Err(GitDiffError::Unsupported);
    }
    let header = std::str::from_utf8(header).map_err(|_| GitDiffError::Unavailable)?;
    let parts: Vec<_> = header.split(' ').collect();
    if parts.len() != 3 {
        return Err(GitDiffError::Unavailable);
    }
    let (mode, oid) = if index {
        if parts[2] != "0" {
            return Err(GitDiffError::Unsupported);
        }
        (parts[0], parts[1])
    } else {
        if parts[1] != "blob" {
            return Err(GitDiffError::Unsupported);
        }
        (parts[0], parts[2])
    };
    if !matches!(mode, "100644" | "100755") {
        return Err(GitDiffError::Unsupported);
    }
    if !valid_oid(oid) {
        return Err(GitDiffError::Unavailable);
    }
    Ok(Some(Entry {
        mode: mode.to_owned(),
        oid: oid.to_owned(),
    }))
}

async fn index_entry(root: &Path, path: &str) -> Result<Option<Entry>, GitDiffError> {
    let result = git(root, &["ls-files", "--stage", "-z", "--", path], 16384).await?;
    if result.code != Some(0) {
        return Err(GitDiffError::Unavailable);
    }
    entry(&result.bytes, path, true)
}

async fn base_entry(
    root: &Path,
    path: &str,
    oid: Option<&str>,
) -> Result<Option<Entry>, GitDiffError> {
    let Some(oid) = oid else {
        return Ok(None);
    };
    let result = git(root, &["ls-tree", "-z", oid, "--", path], 16384).await?;
    if result.code != Some(0) {
        return Err(GitDiffError::Unavailable);
    }
    entry(&result.bytes, path, false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Bytes {
    Missing,
    Present(Vec<u8>),
    Large,
}
impl Bytes {
    fn digest(&self) -> Option<String> {
        match self {
            Self::Present(bytes) => Some(format!("{:x}", Sha256::digest(bytes))),
            _ => None,
        }
    }
}

async fn blob(root: &Path, entry: Option<&Entry>) -> Result<Bytes, GitDiffError> {
    let Some(entry) = entry else {
        return Ok(Bytes::Missing);
    };
    let size: usize = text(git(root, &["cat-file", "-s", &entry.oid], 64).await?)?
        .parse()
        .map_err(|_| GitDiffError::Unavailable)?;
    if size > SIDE_LIMIT {
        return Ok(Bytes::Large);
    }
    let result = git(root, &["cat-file", "blob", &entry.oid], SIDE_LIMIT).await?;
    if result.code != Some(0) || result.bytes.len() != size {
        return Err(GitDiffError::Unavailable);
    }
    Ok(Bytes::Present(result.bytes))
}

fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

/// Only the bounded prefix is read for an oversized file. Its metadata and
/// prefix are compared again; oversized bodies are never returned or retained.
#[derive(Debug, PartialEq, Eq)]
struct Work {
    bytes: Bytes,
    len: u64,
    modified: Option<std::time::SystemTime>,
    prefix_digest: Option<String>,
}

async fn work(root: &Path, path: &str) -> Result<Work, GitDiffError> {
    let target =
        workspace::resolve(root, path, false, &[]).map_err(|_| GitDiffError::InvalidPath)?;
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Work {
                bytes: Bytes::Missing,
                len: 0,
                modified: None,
                prefix_digest: None,
            });
        }
        Err(_) => return Err(GitDiffError::Unavailable),
    };
    if is_link(&metadata) {
        return Err(GitDiffError::InvalidPath);
    }
    if !metadata.is_file() {
        return Err(GitDiffError::Unsupported);
    }
    let file = tokio::fs::File::open(target)
        .await
        .map_err(|_| GitDiffError::Unavailable)?;
    let mut bytes = Vec::new();
    file.take(SIDE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| GitDiffError::Unavailable)?;
    let large = bytes.len() > SIDE_LIMIT || metadata.len() > SIDE_LIMIT as u64;
    let prefix_digest = large.then(|| format!("{:x}", Sha256::digest(&bytes)));
    Ok(Work {
        bytes: if large {
            Bytes::Large
        } else {
            Bytes::Present(bytes)
        },
        len: metadata.len(),
        modified: metadata.modified().ok(),
        prefix_digest,
    })
}

async fn validate_root(root: &Path, path: &str) -> Result<(), GitDiffError> {
    let git_marker =
        std::fs::symlink_metadata(root.join(".git")).map_err(|_| GitDiffError::Unavailable)?;
    if is_link(&git_marker) {
        return Err(GitDiffError::Unavailable);
    }
    let top = text(git(root, &["rev-parse", "--show-toplevel"], 16384).await?)?;
    if Path::new(&top)
        .canonicalize()
        .map_err(|_| GitDiffError::Unavailable)?
        != root
    {
        return Err(GitDiffError::Unavailable);
    }
    if text(git(root, &["rev-parse", "--is-bare-repository"], 16).await?)? != "false" {
        return Err(GitDiffError::Unavailable);
    }
    // Do not silently treat a nested repository's bytes as this repository.
    let mut parent = root.to_path_buf();
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        parent.push(part);
        if std::fs::symlink_metadata(parent.join(".git")).is_ok() {
            return Err(GitDiffError::Unavailable);
        }
    }
    Ok(())
}

fn review(
    before: &Bytes,
    after: &Bytes,
    path: &str,
) -> Result<(GitDiffStatus, Option<String>, bool), GitDiffError> {
    use GitDiffStatus as S;
    if matches!(before, Bytes::Large) || matches!(after, Bytes::Large) {
        return Ok((S::TooLarge, None, false));
    }
    if before == &Bytes::Missing && after == &Bytes::Missing {
        return Ok((S::Missing, None, false));
    }
    if before == after {
        return Ok((S::Unchanged, None, false));
    }
    let a = match before {
        Bytes::Present(v) => v.as_slice(),
        _ => &[],
    };
    let b = match after {
        Bytes::Present(v) => v.as_slice(),
        _ => &[],
    };
    let (Ok(a), Ok(b)) = (std::str::from_utf8(a), std::str::from_utf8(b)) else {
        return Ok((S::Binary, None, false));
    };
    if a.contains('\0') || b.contains('\0') {
        return Ok((S::Binary, None, false));
    }
    // A full-file unified hunk has linear cost, preserves CRLF and missing EOF
    // newlines, and avoids quadratic worst cases on hostile 256 KiB text.
    let old_name = if before == &Bytes::Missing {
        "/dev/null".to_owned()
    } else {
        serde_json::to_string(&format!("a/{path}")).map_err(|_| GitDiffError::Internal)?
    };
    let new_name = if after == &Bytes::Missing {
        "/dev/null".to_owned()
    } else {
        serde_json::to_string(&format!("b/{path}")).map_err(|_| GitDiffError::Internal)?
    };
    // Redact original text before adding diff prefixes. A leading '-' would
    // otherwise hide a token/assignment boundary from the shared redactor.
    let redact = |content: &str| -> Result<String, GitDiffError> {
        secrets::redact_persisted(&serde_json::Value::String(content.to_owned()))
            .as_str()
            .map(str::to_owned)
            .ok_or(GitDiffError::Internal)
    };
    let safe_a = redact(a)?;
    let safe_b = redact(b)?;
    let changed = safe_a != a || safe_b != b;
    let a_count = a.split_inclusive('\n').count();
    let b_count = b.split_inclusive('\n').count();
    let mut patch = format!(
        "--- {old_name}\n+++ {new_name}\n@@ -{},{} +{},{} @@\n",
        usize::from(a_count != 0),
        a_count,
        usize::from(b_count != 0),
        b_count
    );
    for (prefix, content) in [('-', safe_a.as_str()), ('+', safe_b.as_str())] {
        for line in content.split_inclusive('\n') {
            patch.push(prefix);
            patch.push_str(line);
            if !line.ends_with('\n') {
                patch.push_str("\n\\ No newline at end of file\n");
            }
            if patch.len() > PATCH_LIMIT {
                return Ok((S::TooLarge, None, false));
            }
        }
    }
    Ok((S::Text, Some(patch), changed))
}

pub async fn read_git_diff(root: &Path, request: GitDiffRequest) -> Result<GitDiff, GitDiffError> {
    read_with_barrier(root, request, || async {}).await
}

pub(crate) async fn read_with_barrier<F, Fut>(
    root: &Path,
    request: GitDiffRequest,
    barrier: F,
) -> Result<GitDiff, GitDiffError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    if request.path.len() > 4096 || request.path == "*" {
        return Err(GitDiffError::InvalidPath);
    }
    secrets::safe_metadata_path("path", &request.path).map_err(|_| GitDiffError::InvalidPath)?;
    workspace::resolve(root, &request.path, false, &[]).map_err(|_| GitDiffError::InvalidPath)?;
    let path = request.path.replace('\\', "/");
    let root = root.canonicalize().map_err(|_| GitDiffError::Unavailable)?;
    validate_root(&root, &path).await?;
    let base_oid = head(&root).await?;
    let index = index_entry(&root, &path).await?;
    let base = base_entry(&root, &path, base_oid.as_deref()).await?;
    if index.is_none() && base.is_none() {
        let ignored = git(&root, &["check-ignore", "--quiet", "--", &path], 16).await?;
        match ignored.code {
            Some(0) => return Err(GitDiffError::Unsupported),
            Some(1) => {}
            _ => return Err(GitDiffError::Unavailable),
        }
    }
    let working = if request.view != GitDiffView::Staged {
        Some(work(&root, &path).await?)
    } else {
        None
    };
    let before = match request.view {
        GitDiffView::Staged | GitDiffView::Head => blob(&root, base.as_ref()).await?,
        GitDiffView::Unstaged => blob(&root, index.as_ref()).await?,
    };
    let after = match request.view {
        GitDiffView::Staged => blob(&root, index.as_ref()).await?,
        _ => working
            .as_ref()
            .ok_or(GitDiffError::Internal)?
            .bytes
            .clone(),
    };
    barrier().await;
    // Re-resolve paths even for staged queries: a new link is never accepted.
    workspace::resolve(&root, &path, false, &[]).map_err(|_| GitDiffError::Conflict)?;
    validate_root(&root, &path)
        .await
        .map_err(|_| GitDiffError::Conflict)?;
    if head(&root).await.map_err(|_| GitDiffError::Conflict)? != base_oid
        || index_entry(&root, &path)
            .await
            .map_err(|_| GitDiffError::Conflict)?
            != index
    {
        return Err(GitDiffError::Conflict);
    }
    if let Some(original) = working
        && work(&root, &path)
            .await
            .map_err(|_| GitDiffError::Conflict)?
            != original
    {
        return Err(GitDiffError::Conflict);
    }
    let (status, patch, redacted) = review(&before, &after, &path)?;
    let mut result = GitDiff {
        path,
        view: request.view,
        base_oid,
        index_oid: index.map(|entry| entry.oid),
        before_digest: before.digest(),
        after_digest: after.digest(),
        status,
        patch,
        redacted,
    };
    if serde_json::to_vec(&result)
        .map_err(|_| GitDiffError::Internal)?
        .len()
        > RESPONSE_LIMIT
    {
        result.status = GitDiffStatus::TooLarge;
        result.patch = None;
        result.redacted = false;
    }
    Ok(result)
}
