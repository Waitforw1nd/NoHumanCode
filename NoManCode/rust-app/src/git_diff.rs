//! Local read-only Git diff and pure-Rust Myers unified diff generator.
use crate::{secrets, workspace};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

const SIDE_LIMIT: usize = 4 * 1024 * 1024;
const PATCH_LIMIT: usize = 512 * 1024;
const RESPONSE_LIMIT: usize = 1024 * 1024 - 4096;
const MAX_MYERS_LINES: usize = 5_000;
const MAX_MYERS_EDIT_DIST: usize = 2_000;

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
    OutcomeUnknown,
    InvalidPath,
    Unavailable,
    Unsupported,
    Conflict,
    Internal,
}

impl std::fmt::Display for GitDiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::OutcomeUnknown => "Local Git review outcome is unknown",
            Self::InvalidPath => "Invalid Git review path",
            Self::Unavailable => "Local Git review is unavailable",
            Self::Unsupported => "Git review does not support this entry",
            Self::Conflict => "Git review inputs changed; request a fresh review",
            Self::Internal => "Git review failed",
        })
    }
}

impl std::error::Error for GitDiffError {}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DiffLine<'a> {
    text: &'a str,
    has_lf: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditOp {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

fn split_diff_lines(input: &str) -> Vec<DiffLine<'_>> {
    if input.is_empty() {
        return Vec::new();
    }
    input
        .split_inclusive('\n')
        .map(|segment| match segment.strip_suffix('\n') {
            Some(stripped) => DiffLine {
                text: stripped,
                has_lf: true,
            },
            None => DiffLine {
                text: segment,
                has_lf: false,
            },
        })
        .collect()
}

fn myers_middle(a: &[DiffLine<'_>], b: &[DiffLine<'_>]) -> Vec<EditOp> {
    if a.is_empty() {
        return (0..b.len()).map(EditOp::Insert).collect();
    }
    if b.is_empty() {
        return (0..a.len()).map(EditOp::Delete).collect();
    }
    if a.len() > MAX_MYERS_LINES || b.len() > MAX_MYERS_LINES {
        let mut fallback = Vec::with_capacity(a.len() + b.len());
        fallback.extend((0..a.len()).map(EditOp::Delete));
        fallback.extend((0..b.len()).map(EditOp::Insert));
        return fallback;
    }

    let n = a.len() as isize;
    let m = b.len() as isize;
    let max_d = ((n + m) as usize).min(MAX_MYERS_EDIT_DIST);
    let offset = n + m;
    let mut v = vec![0isize; (2 * offset + 1) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut reached = false;

    for d in 0..=(max_d as isize) {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d
                || (k != d && v[(offset + k - 1) as usize] < v[(offset + k + 1) as usize])
            {
                v[(offset + k + 1) as usize]
            } else {
                v[(offset + k - 1) as usize] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(offset + k) as usize] = x;
            if x >= n && y >= m {
                reached = true;
                break;
            }
            k += 2;
        }
        if reached {
            break;
        }
    }

    if !reached {
        let mut fallback = Vec::with_capacity(a.len() + b.len());
        fallback.extend((0..a.len()).map(EditOp::Delete));
        fallback.extend((0..b.len()).map(EditOp::Insert));
        return fallback;
    }

    let mut ops = Vec::with_capacity(a.len() + b.len());
    let mut x = n;
    let mut y = m;
    for d in (0..trace.len()).rev() {
        let d_isize = d as isize;
        if d_isize == 0 {
            while x > 0 && y > 0 {
                x -= 1;
                y -= 1;
                ops.push(EditOp::Equal(x as usize, y as usize));
            }
            break;
        }
        let v_prev = &trace[d];
        let k = x - y;
        let prev_k = if k == -d_isize
            || (k != d_isize
                && v_prev[(offset + k - 1) as usize] < v_prev[(offset + k + 1) as usize])
        {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v_prev[(offset + prev_k) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(EditOp::Equal(x as usize, y as usize));
        }
        if prev_k == k + 1 {
            y -= 1;
            ops.push(EditOp::Insert(y as usize));
        } else {
            x -= 1;
            ops.push(EditOp::Delete(x as usize));
        }
    }
    ops.reverse();
    ops
}

/// Generate a standard line-level unified diff (`--- a/{path}`, `+++ b/{path}`)
/// using Myers shortest-edit-script with bounded fallback for >5,000-line rewrites.
pub fn unified_diff(path: &str, before: &str, after: &str, context_lines: usize) -> String {
    if before == after {
        return String::new();
    }
    let a = split_diff_lines(before);
    let b = split_diff_lines(after);

    let mut prefix = 0usize;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < (a.len() - prefix)
        && suffix < (b.len() - prefix)
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let mid_ops = myers_middle(&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut ops = Vec::with_capacity(prefix + mid_ops.len() + suffix);
    for i in 0..prefix {
        ops.push(EditOp::Equal(i, i));
    }
    for op in mid_ops {
        ops.push(match op {
            EditOp::Equal(ai, bi) => EditOp::Equal(prefix + ai, prefix + bi),
            EditOp::Delete(ai) => EditOp::Delete(prefix + ai),
            EditOp::Insert(bi) => EditOp::Insert(prefix + bi),
        });
    }
    for i in 0..suffix {
        let ai = a.len() - suffix + i;
        let bi = b.len() - suffix + i;
        ops.push(EditOp::Equal(ai, bi));
    }

    let mut op_positions = Vec::with_capacity(ops.len() + 1);
    let mut cur_a = 0usize;
    let mut cur_b = 0usize;
    for op in &ops {
        op_positions.push((cur_a, cur_b));
        match op {
            EditOp::Equal(_, _) => {
                cur_a += 1;
                cur_b += 1;
            }
            EditOp::Delete(_) => cur_a += 1,
            EditOp::Insert(_) => cur_b += 1,
        }
    }
    op_positions.push((cur_a, cur_b));

    let mut change_ranges: Vec<(usize, usize)> = Vec::new();
    let mut idx = 0usize;
    while idx < ops.len() {
        if matches!(ops[idx], EditOp::Equal(_, _)) {
            idx += 1;
            continue;
        }
        let start = idx;
        while idx < ops.len() && !matches!(ops[idx], EditOp::Equal(_, _)) {
            idx += 1;
        }
        let end = idx;
        if let Some(last) = change_ranges.last_mut()
            && start.saturating_sub(last.1) <= context_lines.saturating_mul(2)
        {
            last.1 = end;
        } else {
            change_ranges.push((start, end));
        }
    }

    if change_ranges.is_empty() {
        return String::new();
    }

    let clean_path = path.trim_start_matches(['/', '\\']).replace('\\', "/");
    let mut out = format!("--- a/{clean_path}\n+++ b/{clean_path}\n");

    for (first_change, last_change) in change_ranges {
        let hunk_start = first_change.saturating_sub(context_lines);
        let hunk_end = (last_change + context_lines).min(ops.len());
        let (start_a, start_b) = op_positions[hunk_start];
        let (end_a, end_b) = op_positions[hunk_end];
        let old_count = end_a - start_a;
        let new_count = end_b - start_b;
        let old_line = if old_count == 0 { start_a } else { start_a + 1 };
        let new_line = if new_count == 0 { start_b } else { start_b + 1 };

        out.push_str(&format!(
            "@@ -{old_line},{old_count} +{new_line},{new_count} @@\n"
        ));
        for op in &ops[hunk_start..hunk_end] {
            let (prefix_ch, line) = match *op {
                EditOp::Equal(ai, _) => (' ', a[ai]),
                EditOp::Delete(ai) => ('-', a[ai]),
                EditOp::Insert(bi) => ('+', b[bi]),
            };
            out.push(prefix_ch);
            out.push_str(line.text);
            out.push('\n');
            if !line.has_lf {
                out.push_str("\\ No newline at end of file\n");
            }
        }
    }
    out
}

pub(crate) struct GitOutput {
    pub(crate) code: Option<i32>,
    pub(crate) bytes: Vec<u8>,
}

fn git_command(root: &Path) -> Result<tokio::process::Command, GitDiffError> {
    let executable = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(if cfg!(windows) { "git.exe" } else { "git" }))
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
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("LC_ALL", "C")
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=",
            "-c",
            "diff.external=",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(command)
}

#[cfg(test)]
#[allow(dead_code)]
pub fn transport_fixture_command(root: &Path) -> Result<tokio::process::Command, GitDiffError> {
    git_command(root)
}

pub(crate) async fn run_command(
    command: tokio::process::Command,
    limit: usize,
    timeout: Duration,
) -> Result<GitOutput, GitDiffError> {
    use crate::process_lifetime::{CompletionReason, Outcome};
    let spec = crate::execution_authority::command_spec(
        &command,
        limit,
        std::time::Instant::now() + timeout,
    )
    .map_err(|_| GitDiffError::Unavailable)?;

    let outcome = if crate::execution_authority::current().is_some() {
        crate::execution_authority::run_process(spec)
            .await
            .map_err(|_| GitDiffError::Unavailable)?
    } else {
        crate::process_lifetime::run_direct(spec).await
    };

    match outcome {
        Outcome::Settled(result) if result.reason == CompletionReason::Exited => Ok(GitOutput {
            code: Some(result.exit_code as i32),
            bytes: result.stdout.bytes,
        }),
        Outcome::Unknown => Err(GitDiffError::OutcomeUnknown),
        _ => Err(GitDiffError::Unavailable),
    }
}

async fn git(root: &Path, args: &[&str], limit: usize) -> Result<GitOutput, GitDiffError> {
    let mut command = git_command(root)?;
    if args.first() == Some(&"check-ignore") {
        command.env_remove("GIT_LITERAL_PATHSPECS");
    }
    command.args(args);
    run_command(command, limit, Duration::from_secs(10)).await
}

fn text_out(output: GitOutput) -> Result<String, GitDiffError> {
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

async fn head_oid(root: &Path) -> Result<Option<String>, GitDiffError> {
    let value = git(root, &["rev-parse", "--verify", "--quiet", "HEAD"], 256).await?;
    if value.code == Some(0) {
        let oid = text_out(value)?;
        if !valid_oid(&oid) {
            return Err(GitDiffError::Unavailable);
        }
        return Ok(Some(oid));
    }
    let sym = git(root, &["symbolic-ref", "--quiet", "HEAD"], 4096).await?;
    if sym.code == Some(0) {
        return Ok(None);
    }
    Err(GitDiffError::Unavailable)
}

fn parse_entry_oid(bytes: &[u8], path: &str, index: bool) -> Result<Option<String>, GitDiffError> {
    let mut records = bytes.split(|b| *b == 0).filter(|v| !v.is_empty());
    let Some(record) = records.next() else {
        return Ok(None);
    };
    if records.next().is_some() {
        return Err(GitDiffError::Unsupported);
    }
    let tab = record
        .iter()
        .position(|b| *b == b'\t')
        .ok_or(GitDiffError::Unavailable)?;
    let (header, filename) = record.split_at(tab);
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
    Ok(Some(oid.to_owned()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SideBytes {
    Missing,
    Present(Vec<u8>),
    Large,
}

impl SideBytes {
    fn digest(&self) -> Option<String> {
        match self {
            Self::Present(bytes) => Some(format!("{:x}", Sha256::digest(bytes))),
            _ => None,
        }
    }
}

async fn read_blob(root: &Path, oid: Option<&str>) -> Result<SideBytes, GitDiffError> {
    let Some(oid) = oid else {
        return Ok(SideBytes::Missing);
    };
    let size: usize = text_out(git(root, &["cat-file", "-s", oid], 64).await?)?
        .parse()
        .map_err(|_| GitDiffError::Unavailable)?;
    if size > SIDE_LIMIT {
        return Ok(SideBytes::Large);
    }
    let result = git(root, &["cat-file", "blob", oid], SIDE_LIMIT).await?;
    if result.code != Some(0) || result.bytes.len() != size {
        return Ok(SideBytes::Large);
    }
    Ok(SideBytes::Present(result.bytes))
}

async fn read_worktree_file(root: &Path, path: &str) -> Result<SideBytes, GitDiffError> {
    let target =
        workspace::resolve(root, path, false, &[]).map_err(|_| GitDiffError::InvalidPath)?;
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SideBytes::Missing),
        Err(_) => return Err(GitDiffError::Unavailable),
    };
    if metadata.file_type().is_symlink() {
        return Err(GitDiffError::InvalidPath);
    }
    if !metadata.is_file() {
        return Err(GitDiffError::Unsupported);
    }
    if metadata.len() > SIDE_LIMIT as u64 {
        return Ok(SideBytes::Large);
    }
    let file = tokio::fs::File::open(target)
        .await
        .map_err(|_| GitDiffError::Unavailable)?;
    let mut bytes = Vec::new();
    file.take(SIDE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| GitDiffError::Unavailable)?;
    if bytes.len() > SIDE_LIMIT {
        Ok(SideBytes::Large)
    } else {
        Ok(SideBytes::Present(bytes))
    }
}

fn build_patch(
    before: &SideBytes,
    after: &SideBytes,
    path: &str,
) -> (GitDiffStatus, Option<String>) {
    if matches!(before, SideBytes::Large) || matches!(after, SideBytes::Large) {
        return (GitDiffStatus::TooLarge, None);
    }
    if before == &SideBytes::Missing && after == &SideBytes::Missing {
        return (GitDiffStatus::Missing, None);
    }
    if before == after {
        return (GitDiffStatus::Unchanged, None);
    }
    let a = match before {
        SideBytes::Present(v) => v.as_slice(),
        _ => &[],
    };
    let b = match after {
        SideBytes::Present(v) => v.as_slice(),
        _ => &[],
    };
    let (Ok(a_str), Ok(b_str)) = (std::str::from_utf8(a), std::str::from_utf8(b)) else {
        return (GitDiffStatus::Binary, None);
    };
    if a_str.contains('\0') || b_str.contains('\0') {
        return (GitDiffStatus::Binary, None);
    }
    let patch = unified_diff(path, a_str, b_str, 3);
    if patch.is_empty() {
        return (GitDiffStatus::Unchanged, None);
    }
    if patch.len() > PATCH_LIMIT {
        return (GitDiffStatus::TooLarge, None);
    }
    (GitDiffStatus::Text, Some(patch))
}

pub async fn read(root: &Path, request: GitDiffRequest) -> Result<GitDiff, GitDiffError> {
    read_with_barrier(root, request, || async {}).await
}

pub async fn read_git_diff(root: &Path, request: GitDiffRequest) -> Result<GitDiff, GitDiffError> {
    read(root, request).await
}

pub async fn read_with_barrier<F, Fut>(
    root: &Path,
    request: GitDiffRequest,
    barrier: F,
) -> Result<GitDiff, GitDiffError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    if request.path.is_empty() || request.path.len() > 4096 || request.path == "*" {
        return Err(GitDiffError::InvalidPath);
    }
    secrets::safe_metadata_path("path", &request.path).map_err(|_| GitDiffError::InvalidPath)?;
    workspace::resolve(root, &request.path, false, &[]).map_err(|_| GitDiffError::InvalidPath)?;

    let path = request.path.replace('\\', "/");
    let root = root.canonicalize().map_err(|_| GitDiffError::Unavailable)?;
    let top = text_out(git(&root, &["rev-parse", "--show-toplevel"], 16384).await?)?;
    if Path::new(&top)
        .canonicalize()
        .map_err(|_| GitDiffError::Unavailable)?
        != root
    {
        return Err(GitDiffError::Unavailable);
    }

    let base_oid = head_oid(&root).await?;
    let idx_out = git(&root, &["ls-files", "--stage", "-z", "--", &path], 16384).await?;
    if idx_out.code != Some(0) {
        return Err(GitDiffError::Unavailable);
    }
    let index_oid = parse_entry_oid(&idx_out.bytes, &path, true)?;

    let tree_oid = if let Some(ref head) = base_oid {
        let tree_out = git(&root, &["ls-tree", "-z", head, "--", &path], 16384).await?;
        if tree_out.code != Some(0) {
            return Err(GitDiffError::Unavailable);
        }
        parse_entry_oid(&tree_out.bytes, &path, false)?
    } else {
        None
    };

    if index_oid.is_none() && tree_oid.is_none() {
        let ignored = git(&root, &["check-ignore", "--quiet", "--", &path], 16).await?;
        if ignored.code == Some(0) {
            return Err(GitDiffError::Unsupported);
        }
    }

    let before = match request.view {
        GitDiffView::Staged | GitDiffView::Head => read_blob(&root, tree_oid.as_deref()).await?,
        GitDiffView::Unstaged => read_blob(&root, index_oid.as_deref()).await?,
    };
    let after = match request.view {
        GitDiffView::Staged => read_blob(&root, index_oid.as_deref()).await?,
        GitDiffView::Unstaged | GitDiffView::Head => read_worktree_file(&root, &path).await?,
    };

    barrier().await;

    let (status, patch) = build_patch(&before, &after, &path);
    let mut result = GitDiff {
        path,
        view: request.view,
        base_oid,
        index_oid,
        before_digest: before.digest(),
        after_digest: after.digest(),
        status,
        patch,
        redacted: false,
    };
    if serde_json::to_vec(&result)
        .map_err(|_| GitDiffError::Internal)?
        .len()
        > RESPONSE_LIMIT
    {
        result.status = GitDiffStatus::TooLarge;
        result.patch = None;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_diff_produces_context_hunks_not_full_file_dump() {
        let before: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let mut after_lines: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        after_lines[9] = "line 10 modified\n".into();
        let after = after_lines.concat();

        let diff = unified_diff("src/main.rs", &before, &after, 3);
        assert!(diff.starts_with("--- a/src/main.rs\n+++ b/src/main.rs\n"));
        assert!(diff.contains("@@ -7,7 +7,7 @@\n"));
        assert!(diff.contains("-line 10\n+line 10 modified\n"));
        assert!(!diff.contains("line 1\n"));
        assert!(!diff.contains("line 20\n"));
    }

    #[test]
    fn unified_diff_handles_multiple_hunks_and_missing_eof_newline() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj";
        let after = "A\nb\nc\nd\ne\nf\ng\nh\ni\nJ\n";
        let diff = unified_diff("test.txt", before, after, 1);
        assert_eq!(diff.matches("@@ ").count(), 2);
        assert!(diff.contains("\\ No newline at end of file"));
    }

    #[test]
    fn unified_diff_falls_back_cleanly_on_huge_files() {
        let before: String = (0..6_000).map(|i| format!("old {i}\n")).collect();
        let after: String = (0..6_000).map(|i| format!("new {i}\n")).collect();
        let diff = unified_diff("huge.txt", &before, &after, 3);
        assert!(diff.starts_with("--- a/huge.txt\n+++ b/huge.txt\n@@ -1,6000 +1,6000 @@\n"));
    }
}
