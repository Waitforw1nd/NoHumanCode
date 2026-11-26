mod exploration;
mod parallel_exploration;
pub(crate) use exploration::{ExplorationRequest, exploration_definition};

pub(crate) use parallel_exploration::{
    ParallelExploration, SearchBranch, SearchEvidence, parallel_exploration_definition,
};

use crate::domain::{Task, scope_path};
use anyhow::{Context, Result, bail, ensure};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_READ_MAX_LINES: usize = 800;
const DIFF_PREVIEW_MAX_BYTES: usize = 64 * 1024;
const MAX_GITIGNORE_BYTES: u64 = 256 * 1024;
const MAX_TODOS_PER_RUN: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
}

static RUN_TODOS: OnceLock<Mutex<HashMap<String, Vec<TodoItem>>>> = OnceLock::new();

fn todo_store() -> &'static Mutex<HashMap<String, Vec<TodoItem>>> {
    RUN_TODOS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn todos_for_run(run_id: &str) -> Vec<TodoItem> {
    let guard = todo_store().lock().unwrap_or_else(|e| e.into_inner());
    guard.get(run_id).cloned().unwrap_or_default()
}

pub fn clear_todo_cache_for_run(run_id: &str) {
    let mut guard = todo_store().lock().unwrap_or_else(|e| e.into_inner());
    guard.remove(run_id);
}

pub fn todos_for_task(task: &Task) -> Vec<TodoItem> {
    {
        let guard = todo_store().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(items) = guard.get(&task.run_id) {
            return items.clone();
        }
    }
    for msg in task.messages.iter().rev() {
        if msg["role"] != "assistant" {
            continue;
        }
        let Some(calls) = msg["tool_calls"].as_array() else {
            continue;
        };
        for call in calls.iter().rev() {
            if call["function"]["name"].as_str() != Some("todo_write") {
                continue;
            }
            let parsed = match &call["function"]["arguments"] {
                Value::String(raw) => serde_json::from_str::<Value>(raw).ok(),
                other if other.is_object() => Some(other.clone()),
                _ => None,
            };
            if let Some(args) = parsed
                && let Ok(items) = parse_todo_items(&args)
            {
                let mut guard = todo_store().lock().unwrap_or_else(|e| e.into_inner());
                guard.insert(task.run_id.clone(), items.clone());
                return items;
            }
        }
    }
    Vec::new()
}

fn parse_todo_items(args: &Value) -> Result<Vec<TodoItem>> {
    let list = args
        .get("todos")
        .and_then(Value::as_array)
        .context("缺少 todos 数组")?;
    ensure!(
        list.len() <= MAX_TODOS_PER_RUN,
        "todos 项数量超出上限 ({MAX_TODOS_PER_RUN})"
    );
    let mut items = Vec::with_capacity(list.len());
    for raw in list {
        let id = raw["id"]
            .as_str()
            .context("todo 项缺少 id")?
            .trim()
            .to_owned();
        ensure!(!id.is_empty(), "todo 项 id 不能为空");
        let content = raw["content"]
            .as_str()
            .context("todo 项缺少 content")?
            .trim()
            .to_owned();
        ensure!(!content.is_empty(), "todo 项 content 不能为空");
        let status = raw["status"]
            .as_str()
            .context("todo 项缺少 status")?
            .trim()
            .to_owned();
        ensure!(
            matches!(status.as_str(), "pending" | "in_progress" | "completed"),
            "无效的 todo status：{status}（允许 pending | in_progress | completed）"
        );
        items.push(TodoItem {
            id,
            content,
            status,
        });
    }
    Ok(items)
}

fn todo_response(run_id: &str, items: &[TodoItem]) -> Value {
    let pending = items.iter().filter(|t| t.status == "pending").count();
    let in_progress = items.iter().filter(|t| t.status == "in_progress").count();
    let completed = items.iter().filter(|t| t.status == "completed").count();
    json!({
        "run_id": run_id,
        "todos": items,
        "summary": {
            "total": items.len(),
            "pending": pending,
            "in_progress": in_progress,
            "completed": completed
        }
    })
}

pub struct FileMutationPreview {
    pub target: PathBuf,
    pub display_path: String,
    pub before: Option<Vec<u8>>,
    pub after: Vec<u8>,
    pub replacements: usize,
    pub diff: String,
}

pub(crate) fn read_safe_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(!metadata.file_type().is_symlink(), "拒绝链接文件");
    ensure!(metadata.is_file(), "目标不是普通文件");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "拒绝 reparse point"
        );
    }
    ensure!(metadata.len() <= MAX_FILE_BYTES as u64, "文件大于 4 MiB");
    let bytes = std::fs::read(path)?;
    std::str::from_utf8(&bytes).context("文件不是 UTF-8")?;
    Ok(Some(bytes))
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<()> {
    struct TempCleanup(PathBuf);
    impl Drop for TempCleanup {
        fn drop(&mut self) {
            if !self.0.as_os_str().is_empty() {
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }
    let parent = path.parent().context("文件没有父目录")?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".coda-{}.tmp", uuid::Uuid::new_v4()));
    let mut cleanup = TempCleanup(temp.clone());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    use std::io::Write as _;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if path.exists() {
        let _ = read_safe_file(path)?;
    }
    replace_path(&temp, path)?;
    cleanup.0.clear();
    Ok(())
}

#[cfg(not(windows))]
fn replace_path(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
fn replace_path(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn skipped_dir(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        ".git" | "node_modules" | "target" | ".cargo-target"
    )
}

fn forbidden(relative: &str) -> bool {
    relative.replace('\\', "/").split('/').any(|part| {
        let p = part.to_lowercase();
        matches!(
            p.as_str(),
            ".git"
                | ".ssh"
                | ".aws"
                | "node_modules"
                | "target"
                | ".cargo-target"
                | ".credentials.yaml"
                | "coda.sqlite3"
                | "coda.sqlite3-wal"
                | "coda.sqlite3-shm"
        ) || p == ".env"
            || p.starts_with(".env.")
            || p.ends_with(".pem")
            || p.ends_with(".key")
    })
}

pub fn within_write_scopes(normalized: &str, scopes: &[String]) -> bool {
    scopes.iter().any(|s| {
        s == "*"
            || scope_path(s).ok().is_some_and(|scope| {
                normalized == scope || normalized.starts_with(&format!("{scope}/"))
            })
    })
}

pub fn resolve(root: &Path, relative: &str, writing: bool, scopes: &[String]) -> Result<PathBuf> {
    let normalized = scope_path(relative)?;
    ensure!(!forbidden(relative), "此路径不允许由文件工具访问");
    if writing {
        ensure!(
            within_write_scopes(&normalized, scopes),
            "写入超出当前成员范围"
        );
    }
    crate::workspace_identity::verify_directory_policy(root)?;
    let root = root.canonicalize().context("工作目录不存在")?;
    let relative = relative.replace('\\', "/");
    let mut target = root.clone();
    for part in relative.split('/') {
        target.push(part);
        let meta = match std::fs::symlink_metadata(&target) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("无法检查文件路径"),
        };
        if meta.is_dir() {
            crate::workspace_identity::verify_directory_policy(&target)?;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            ensure!(
                meta.file_attributes() & 0x400 == 0,
                "不允许经过目录联接或符号链接"
            );
        }
        ensure!(!meta.file_type().is_symlink(), "不允许经过符号链接");
        let canonical = target.canonicalize()?;
        let actual_relative = canonical.strip_prefix(&root).context("路径超出工作目录")?;
        ensure!(
            !forbidden(actual_relative.to_str().context("文件路径不是 UTF-8")?),
            "此路径不允许由文件工具访问"
        );
        target = canonical;
    }
    if writing {
        let actual = scope_path(
            target
                .strip_prefix(&root)?
                .to_str()
                .context("文件路径不是 UTF-8")?,
        )?;
        ensure!(within_write_scopes(&actual, scopes), "写入超出当前成员范围");
    }
    Ok(target)
}

pub(crate) fn change_target(
    root: &Path,
    relative: &str,
    scopes: &[String],
) -> Result<(PathBuf, String, String)> {
    let target = resolve(root, relative, true, scopes)?;
    let canonical_root = root.canonicalize()?;
    let identity = if target.exists() {
        target.canonicalize()?
    } else if let Some(parent) = target.parent().filter(|parent| parent.exists()) {
        parent
            .canonicalize()?
            .join(target.file_name().context("文件名无效")?)
    } else {
        target.clone()
    };
    ensure!(
        identity.starts_with(&canonical_root),
        "路径身份超出工作目录"
    );
    let display = identity
        .strip_prefix(&canonical_root)?
        .to_string_lossy()
        .trim_start_matches(['/', '\\'])
        .replace('\\', "/");
    ensure!(!display.is_empty(), "文件路径无效");
    let key = display.to_lowercase();
    Ok((target, display, key))
}

fn bounded_diff(display_path: &str, before: &str, after: &str) -> String {
    let diff = crate::git_diff::unified_diff(display_path, before, after, 3);
    if diff.len() <= DIFF_PREVIEW_MAX_BYTES {
        return diff;
    }
    let mut cut = DIFF_PREVIEW_MAX_BYTES;
    while cut > 0 && !diff.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n... [diff truncated] ...\n", &diff[..cut])
}

struct EditSpec {
    old_string: String,
    new_string: String,
    replace_all: bool,
}

fn parse_edit_specs(args: &Value) -> Result<Vec<EditSpec>> {
    if let Some(edits) = args.get("edits").filter(|v| !v.is_null()) {
        let list = edits.as_array().context("edits 必须是数组")?;
        ensure!(!list.is_empty(), "edits 不能为空");
        let mut parsed = Vec::with_capacity(list.len());
        for item in list {
            let old_string = item["old_string"]
                .as_str()
                .context("edits 项缺少 old_string")?
                .to_owned();
            let new_string = item["new_string"]
                .as_str()
                .context("edits 项缺少 new_string")?
                .to_owned();
            let replace_all = item["replace_all"].as_bool().unwrap_or(false);
            ensure!(!old_string.is_empty(), "old_string 不能为空");
            parsed.push(EditSpec {
                old_string,
                new_string,
                replace_all,
            });
        }
        return Ok(parsed);
    }
    let old_string = args["old_string"]
        .as_str()
        .context("缺少 old_string、edits 数组或 patch")?
        .to_owned();
    let new_string = args["new_string"]
        .as_str()
        .context("缺少 new_string")?
        .to_owned();
    let replace_all = args["replace_all"].as_bool().unwrap_or(false);
    ensure!(!old_string.is_empty(), "old_string 不能为空");
    Ok(vec![EditSpec {
        old_string,
        new_string,
        replace_all,
    }])
}

#[derive(Clone, Debug)]
enum PatchOp {
    Context { implicit_blank: bool },
    Delete { no_newline: bool },
    Insert { text: String, no_newline: bool },
}

#[derive(Clone, Debug)]
struct PatchHunk {
    old_start: Option<usize>,
    old_lines: Vec<String>,
    ops: Vec<PatchOp>,
}

fn strip_markdown_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```")
        && let Some(newline_pos) = rest.find('\n')
        && let Some(inner) = rest[newline_pos + 1..].strip_suffix("```")
    {
        return inner;
    }
    raw
}

fn parse_hunk_header(line: &str) -> Option<Option<usize>> {
    if line.trim() == "@@" {
        return Some(None);
    }
    let rest = line.strip_prefix("@@")?;
    let end = rest.find("@@")?;
    let inner = rest[..end].trim();
    if inner.is_empty() {
        return Some(None);
    }
    for token in inner.split_whitespace() {
        if let Some(after_minus) = token.strip_prefix('-') {
            let start_str = after_minus.split(',').next().unwrap_or(after_minus);
            if let Ok(num) = start_str.parse::<usize>() {
                return Some(Some(num));
            }
        }
    }
    Some(None)
}

fn finalize_hunk(mut hunk: PatchHunk, out: &mut Vec<PatchHunk>) -> Result<()> {
    while matches!(
        hunk.ops.last(),
        Some(PatchOp::Context {
            implicit_blank: true
        })
    ) {
        hunk.ops.pop();
        hunk.old_lines.pop();
    }
    ensure!(!hunk.ops.is_empty(), "patch 包含空的 @@ 差异块");
    out.push(hunk);
    Ok(())
}

fn parse_unified_hunks(raw_patch: &str) -> Result<Vec<PatchHunk>> {
    let cleaned = strip_markdown_fence(raw_patch);
    let mut hunks = Vec::new();
    let mut current: Option<PatchHunk> = None;

    for raw_line in cleaned.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if (line.starts_with("@@ ") || line.trim() == "@@")
            && let Some(old_start) = parse_hunk_header(line)
        {
            if let Some(prev) = current.take() {
                finalize_hunk(prev, &mut hunks)?;
            }
            current = Some(PatchHunk {
                old_start,
                old_lines: Vec::new(),
                ops: Vec::new(),
            });
            continue;
        }

        let Some(ref mut hunk) = current else {
            continue;
        };

        if line.starts_with("diff --git ") {
            let finished = current.take().unwrap();
            finalize_hunk(finished, &mut hunks)?;
            continue;
        }

        if let Some(rest) = line.strip_prefix(' ') {
            hunk.old_lines.push(rest.to_owned());
            hunk.ops.push(PatchOp::Context {
                implicit_blank: false,
            });
        } else if let Some(rest) = line.strip_prefix('-') {
            hunk.old_lines.push(rest.to_owned());
            hunk.ops.push(PatchOp::Delete { no_newline: false });
        } else if let Some(rest) = line.strip_prefix('+') {
            hunk.ops.push(PatchOp::Insert {
                text: rest.to_owned(),
                no_newline: false,
            });
        } else if line.starts_with('\\') {
            if let Some(PatchOp::Delete { no_newline } | PatchOp::Insert { no_newline, .. }) =
                hunk.ops.last_mut()
            {
                *no_newline = true;
            }
        } else if line.is_empty() {
            hunk.old_lines.push(String::new());
            hunk.ops.push(PatchOp::Context {
                implicit_blank: true,
            });
        } else {
            bail!("patch 包含无法识别的差异行（缺少 ' '、'+' 或 '-' 前缀）：{line}");
        }
    }

    if let Some(last) = current {
        finalize_hunk(last, &mut hunks)?;
    }

    ensure!(
        !hunks.is_empty(),
        "patch 中未找到有效的统一差异块（缺少 @@ ... @@ hunk 头）"
    );
    Ok(hunks)
}

fn select_closest_candidate(
    mut candidates: Vec<usize>,
    expected_pos: Option<usize>,
    display_path: &str,
    hunk_no: usize,
) -> Result<usize> {
    if candidates.len() == 1 {
        return Ok(candidates[0]);
    }
    if let Some(exp) = expected_pos {
        candidates.sort_by_key(|c| c.abs_diff(exp));
        if candidates[0].abs_diff(exp) < candidates[1].abs_diff(exp) {
            return Ok(candidates[0]);
        }
    }
    bail!(
        "patch 第 {hunk_no} 个 hunk 在 {display_path} 中匹配到 {} 处，请提供行号或更多唯一上下文行",
        candidates.len()
    )
}

fn find_hunk_position(
    lines: &[String],
    old_lines: &[String],
    min_start: usize,
    expected_pos: Option<usize>,
    display_path: &str,
    hunk_no: usize,
) -> Result<usize> {
    ensure!(
        min_start + old_lines.len() <= lines.len(),
        "patch 第 {hunk_no} 个 hunk 超出 {display_path} 剩余行数"
    );
    let max_start = lines.len() - old_lines.len();

    // Pass 1: exact line match
    let exact: Vec<usize> = (min_start..=max_start)
        .filter(|&pos| {
            old_lines
                .iter()
                .enumerate()
                .all(|(k, pat)| lines[pos + k] == *pat)
        })
        .collect();
    if !exact.is_empty() {
        return select_closest_candidate(exact, expected_pos, display_path, hunk_no);
    }

    // Pass 2: trailing-whitespace-tolerant match
    let trim_end_matches: Vec<usize> = (min_start..=max_start)
        .filter(|&pos| {
            old_lines
                .iter()
                .enumerate()
                .all(|(k, pat)| lines[pos + k].trim_end() == pat.trim_end())
        })
        .collect();
    if !trim_end_matches.is_empty() {
        return select_closest_candidate(trim_end_matches, expected_pos, display_path, hunk_no);
    }

    // Pass 3: indentation/whitespace-tolerant match (only when hunk has non-blank anchor lines)
    if old_lines.iter().any(|l| !l.trim().is_empty()) {
        let trimmed: Vec<usize> = (min_start..=max_start)
            .filter(|&pos| {
                old_lines
                    .iter()
                    .enumerate()
                    .all(|(k, pat)| lines[pos + k].trim() == pat.trim())
            })
            .collect();
        if !trimmed.is_empty() {
            return select_closest_candidate(trimmed, expected_pos, display_path, hunk_no);
        }
    }

    bail!(
        "patch 第 {hunk_no} 个 hunk 无法在 {display_path} 中匹配上下文；请先调用 read_file 检查最新文件内容"
    )
}

fn apply_unified_patch(
    display_path: &str,
    original: &str,
    raw_patch: &str,
) -> Result<(String, usize)> {
    let hunks = parse_unified_hunks(raw_patch)?;
    let use_crlf = original.contains("\r\n");
    let had_trailing_newline = original.ends_with('\n');
    let mut eof_newline = had_trailing_newline;
    let mut lines: Vec<String> = original.lines().map(str::to_owned).collect();
    let mut min_start = 0usize;
    let mut line_delta: isize = 0;

    for (idx, hunk) in hunks.iter().enumerate() {
        let hunk_no = idx + 1;
        let expected_pos = hunk.old_start.map(|s| {
            let base0 = s.saturating_sub(1) as isize;
            (base0 + line_delta).clamp(0, lines.len() as isize) as usize
        });

        let pos = if hunk.old_lines.is_empty() {
            match hunk.old_start {
                Some(0) => min_start,
                Some(s) => {
                    ((s.saturating_sub(1) as isize + line_delta)
                        .clamp(min_start as isize, lines.len() as isize))
                        as usize
                }
                None => lines.len(),
            }
        } else {
            find_hunk_position(
                &lines,
                &hunk.old_lines,
                min_start,
                expected_pos,
                display_path,
                hunk_no,
            )?
        };

        let touches_eof = pos + hunk.old_lines.len() == lines.len();
        if touches_eof {
            let last_new_op = hunk
                .ops
                .iter()
                .rev()
                .find(|op| matches!(op, PatchOp::Context { .. } | PatchOp::Insert { .. }));
            let deleted_eof_no_nl = hunk
                .ops
                .iter()
                .any(|op| matches!(op, PatchOp::Delete { no_newline: true }));
            match last_new_op {
                Some(PatchOp::Insert {
                    no_newline: true, ..
                }) => {
                    eof_newline = false;
                }
                Some(PatchOp::Insert {
                    no_newline: false, ..
                }) if original.is_empty() || (!had_trailing_newline && deleted_eof_no_nl) => {
                    eof_newline = true;
                }
                _ => {}
            }
        }

        let mut replacement = Vec::new();
        let mut old_cursor = 0usize;
        for op in &hunk.ops {
            match op {
                PatchOp::Context { .. } => {
                    replacement.push(lines[pos + old_cursor].clone());
                    old_cursor += 1;
                }
                PatchOp::Delete { .. } => {
                    old_cursor += 1;
                }
                PatchOp::Insert { text, .. } => {
                    replacement.push(text.clone());
                }
            }
        }

        let old_len = hunk.old_lines.len();
        let new_len = replacement.len();
        lines.splice(pos..pos + old_len, replacement);
        line_delta += new_len as isize - old_len as isize;
        min_start = pos + new_len;
    }

    let eol = if use_crlf { "\r\n" } else { "\n" };
    let mut updated = lines.join(eol);
    if !lines.is_empty() && eof_newline {
        updated.push_str(eol);
    }
    ensure!(updated.len() <= MAX_FILE_BYTES, "编辑后的文件大于 4 MiB");
    Ok((updated, hunks.len()))
}

fn adapt_line_endings(text: &str, use_crlf: bool) -> String {
    let lf = text.replace("\r\n", "\n");
    if use_crlf {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

fn find_eol_tolerant_ranges(haystack: &str, needle_lf: &str) -> Vec<(usize, usize)> {
    // Map each byte of `haystack_lf` (with `\r\n` collapsed to `\n`) back to original byte offsets.
    let bytes = haystack.as_bytes();
    let mut lf = Vec::with_capacity(bytes.len());
    let mut map_start = Vec::with_capacity(bytes.len() + 1);
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            map_start.push(i);
            lf.push(b'\n');
            i += 2;
        } else {
            map_start.push(i);
            lf.push(bytes[i]);
            i += 1;
        }
    }
    map_start.push(bytes.len());
    let Ok(lf_str) = std::str::from_utf8(&lf) else {
        return Vec::new();
    };
    lf_str
        .match_indices(needle_lf)
        .map(|(start, matched)| {
            let end = start + matched.len();
            (map_start[start], map_start[end])
        })
        .collect()
}

fn apply_edits(display_path: &str, original: &str, edits: &[EditSpec]) -> Result<(String, usize)> {
    let use_crlf = original.contains("\r\n");
    let mut working = original.to_owned();
    let mut total_replacements = 0usize;

    for edit in edits {
        let replacement = adapt_line_endings(&edit.new_string, use_crlf);
        let ranges: Vec<(usize, usize)> = {
            let exact: Vec<(usize, usize)> = working
                .match_indices(&edit.old_string)
                .map(|(idx, s)| (idx, idx + s.len()))
                .collect();
            if !exact.is_empty() {
                exact
            } else {
                let adapted_old = adapt_line_endings(&edit.old_string, use_crlf);
                let adapted: Vec<(usize, usize)> = working
                    .match_indices(&adapted_old)
                    .map(|(idx, s)| (idx, idx + s.len()))
                    .collect();
                if !adapted.is_empty() {
                    adapted
                } else {
                    let needle_lf = edit.old_string.replace("\r\n", "\n");
                    find_eol_tolerant_ranges(&working, &needle_lf)
                }
            }
        };

        if ranges.is_empty() {
            bail!(
                "在 {display_path} 中未找到匹配的 old_string；请先调用 read_file 检查最新文件内容"
            );
        }
        if ranges.len() > 1 && !edit.replace_all {
            let lines: Vec<String> = ranges
                .iter()
                .map(|(start, _)| {
                    let line = 1 + working[..*start].bytes().filter(|&b| b == b'\n').count();
                    line.to_string()
                })
                .collect();
            bail!(
                "old_string 在 {display_path} 中匹配到 {} 处（第 {} 行）；请提供更多上下文行使其唯一，或设置 replace_all: true",
                ranges.len(),
                lines.join(", ")
            );
        }

        let mut updated = String::with_capacity(working.len() + replacement.len());
        let mut cursor = 0usize;
        for (start, end) in &ranges {
            updated.push_str(&working[cursor..*start]);
            updated.push_str(&replacement);
            cursor = *end;
        }
        updated.push_str(&working[cursor..]);
        total_replacements += ranges.len();
        working = updated;
    }

    ensure!(working.len() <= MAX_FILE_BYTES, "编辑后的文件大于 4 MiB");
    Ok((working, total_replacements))
}

pub fn preview_file_mutation_full(
    root: &Path,
    name: &str,
    args: &Value,
    scopes: &[String],
) -> Result<FileMutationPreview> {
    let relative = args["path"].as_str().context("缺少 path")?;
    let (target, display_path, _) = change_target(root, relative, scopes)?;
    let before = read_safe_file(&target)?;
    let before_str = match &before {
        Some(bytes) => std::str::from_utf8(bytes).context("原文件不是有效 UTF-8")?,
        None => "",
    };

    match name {
        "write_file" => {
            let content = args["content"].as_str().context("缺少 content")?;
            ensure!(content.len() <= MAX_FILE_BYTES, "单个文件写入大于 4 MiB");
            let diff = bounded_diff(&display_path, before_str, content);
            Ok(FileMutationPreview {
                target,
                display_path,
                before,
                after: content.as_bytes().to_vec(),
                replacements: 1,
                diff,
            })
        }
        "edit_file" => {
            ensure!(
                before.is_some(),
                "目标文件不存在，无法 edit_file：{display_path}"
            );
            let (updated, replacements) =
                if let Some(patch) = args.get("patch").filter(|v| !v.is_null()) {
                    let patch_str = patch.as_str().context("patch 必须是字符串")?;
                    apply_unified_patch(&display_path, before_str, patch_str)?
                } else {
                    let edits = parse_edit_specs(args)?;
                    apply_edits(&display_path, before_str, &edits)?
                };
            let diff = bounded_diff(&display_path, before_str, &updated);
            Ok(FileMutationPreview {
                target,
                display_path,
                before,
                after: updated.into_bytes(),
                replacements,
                diff,
            })
        }
        _ => bail!("不支持的文件变更预览工具：{name}"),
    }
}

/// Resolves target path within `scopes`, reads existing file bytes (`Option<Vec<u8>>`),
/// computes resulting `new_bytes: Vec<u8>`, and returns the normalized `display_path`
/// expected by `workspace_changes` (Checkpoint & Restore).
#[allow(clippy::type_complexity)]
pub fn preview_file_mutation(
    root: &Path,
    name: &str,
    args: &Value,
    scopes: &[String],
) -> Result<(PathBuf, Option<Vec<u8>>, Vec<u8>, String)> {
    let preview = preview_file_mutation_full(root, name, args, scopes)?;
    Ok((
        preview.target,
        preview.before,
        preview.after,
        preview.display_path,
    ))
}

/// Computes a bounded unified diff preview for `write_file` or `edit_file`.
pub fn preview_file_diff(
    root: &Path,
    name: &str,
    args: &Value,
    scopes: &[String],
) -> Result<String> {
    Ok(preview_file_mutation_full(root, name, args, scopes)?.diff)
}

fn glob_to_regex(pattern: &str) -> Result<Regex> {
    let normalized = pattern.trim().replace('\\', "/");
    ensure!(
        !normalized.is_empty() && normalized.len() <= 1024,
        "glob 模式为空或过长"
    );
    let chars: Vec<char> = normalized.chars().collect();
    let mut rx = String::from("^");
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if chars.get(i + 1) == Some(&'*') {
                    while chars.get(i + 1) == Some(&'*') {
                        i += 1;
                    }
                    if chars.get(i + 1) == Some(&'/') {
                        i += 1;
                        rx.push_str("(?:.*/)?");
                    } else {
                        rx.push_str(".*");
                    }
                } else {
                    rx.push_str("[^/]*");
                }
            }
            '?' => rx.push_str("[^/]"),
            '{' => {
                if let Some(end) = chars[i + 1..].iter().position(|&c| c == '}') {
                    let inner: String = chars[i + 1..i + 1 + end].iter().collect();
                    let parts: Vec<String> = inner.split(',').map(regex::escape).collect();
                    rx.push_str("(?:");
                    rx.push_str(&parts.join("|"));
                    rx.push(')');
                    i += end + 1;
                } else {
                    rx.push_str("\\{");
                }
            }
            '[' => {
                if let Some(end) = chars[i + 1..].iter().position(|&c| c == ']') {
                    rx.push('[');
                    for &ch in &chars[i + 1..i + 1 + end] {
                        if ch == '\\' {
                            rx.push_str("\\\\");
                        } else {
                            rx.push(ch);
                        }
                    }
                    rx.push(']');
                    i += end + 1;
                } else {
                    rx.push_str("\\[");
                }
            }
            ch => rx.push_str(&regex::escape(&ch.to_string())),
        }
        i += 1;
    }
    rx.push('$');
    RegexBuilder::new(&rx)
        .case_insensitive(cfg!(windows))
        .build()
        .context("无效的 glob 模式")
}

fn matches_glob(rx: &Regex, raw_pattern: &str, relative_path: &str, file_name: &str) -> bool {
    if rx.is_match(relative_path) {
        return true;
    }
    if !raw_pattern.contains('/') && !raw_pattern.contains('\\') {
        return rx.is_match(file_name);
    }
    false
}

struct GitignoreRule {
    base: String,
    negated: bool,
    dir_only: bool,
    anchored: bool,
    regex: Regex,
}

impl GitignoreRule {
    fn matches_candidate(&self, cand_path: &str, cand_is_dir: bool) -> bool {
        let sub_rel = if self.base.is_empty() {
            cand_path
        } else if let Some(rest) = cand_path.strip_prefix(&self.base) {
            match rest.strip_prefix('/') {
                Some(after_slash) if !after_slash.is_empty() => after_slash,
                _ => return false,
            }
        } else {
            return false;
        };
        if self.dir_only && !cand_is_dir {
            return false;
        }
        if self.anchored {
            self.regex.is_match(sub_rel)
        } else {
            let name = sub_rel.rsplit('/').next().unwrap_or(sub_rel);
            self.regex.is_match(name)
        }
    }
}

fn parse_gitignore_line(raw_line: &str, base: &str) -> Option<GitignoreRule> {
    let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
    let bytes = line.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b' ' {
        if end >= 2 && bytes[end - 2] == b'\\' {
            break;
        }
        end -= 1;
    }
    let trimmed_end = &line[..end];
    if trimmed_end.trim().is_empty() || trimmed_end.starts_with('#') {
        return None;
    }
    let (negated, rest) = if let Some(s) = trimmed_end.strip_prefix('!') {
        (true, s)
    } else if let Some(s) = trimmed_end.strip_prefix("\\!") {
        (false, s)
    } else if let Some(s) = trimmed_end.strip_prefix("\\#") {
        (false, s)
    } else {
        (false, trimmed_end)
    };
    let unescaped = rest.replace("\\ ", " ");
    let (dir_only, body) = match unescaped.strip_suffix('/') {
        Some(s) => (true, s),
        None => (false, unescaped.as_str()),
    };
    if body.is_empty() {
        return None;
    }
    let (anchored, pat) = if let Some(s) = body.strip_prefix('/') {
        (true, s)
    } else if body.contains('/') {
        (true, body)
    } else {
        (false, body)
    };
    if pat.is_empty() {
        return None;
    }
    let regex = glob_to_regex(pat).ok()?;
    Some(GitignoreRule {
        base: base.to_owned(),
        negated,
        dir_only,
        anchored,
        regex,
    })
}

fn read_gitignore_rules(dir: &Path, base: &str) -> Vec<Arc<GitignoreRule>> {
    let gitignore_path = dir.join(".gitignore");
    let Ok(meta) = std::fs::symlink_metadata(&gitignore_path) else {
        return Vec::new();
    };
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_GITIGNORE_BYTES {
        return Vec::new();
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Vec::new();
        }
    }
    let Ok(bytes) = std::fs::read(&gitignore_path) else {
        return Vec::new();
    };
    let content = String::from_utf8_lossy(&bytes);
    content
        .lines()
        .filter_map(|line| parse_gitignore_line(line, base).map(Arc::new))
        .collect()
}

#[derive(Clone, Default)]
struct GitignoreMatcher {
    rules: Vec<Arc<GitignoreRule>>,
}

impl GitignoreMatcher {
    fn for_directory(root_canonical: &Path, relative_dir: &str) -> Self {
        let mut rules = read_gitignore_rules(root_canonical, "");
        let normalized = relative_dir.replace('\\', "/");
        let trimmed = normalized.trim_matches('/');
        if !trimmed.is_empty() {
            let mut cur_path = root_canonical.to_path_buf();
            let mut cur_rel = String::new();
            for part in trimmed.split('/') {
                cur_path.push(part);
                cur_rel = if cur_rel.is_empty() {
                    part.to_owned()
                } else {
                    format!("{cur_rel}/{part}")
                };
                if let Ok(meta) = std::fs::symlink_metadata(&cur_path)
                    && !meta.file_type().is_symlink()
                    && meta.is_dir()
                {
                    rules.extend(read_gitignore_rules(&cur_path, &cur_rel));
                }
            }
        }
        Self { rules }
    }

    fn with_dir(&self, dir_canonical: &Path, rel_dir: &str) -> Self {
        let extra = read_gitignore_rules(dir_canonical, rel_dir);
        if extra.is_empty() {
            return self.clone();
        }
        let mut rules = self.rules.clone();
        rules.extend(extra);
        Self { rules }
    }

    fn is_single_path_ignored(&self, cand_path: &str, cand_is_dir: bool) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            if rule.matches_candidate(cand_path, cand_is_dir) {
                ignored = !rule.negated;
            }
        }
        ignored
    }

    fn is_ignored(&self, rel_path: &str, is_dir: bool) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        let normalized = rel_path.replace('\\', "/");
        let trimmed = normalized.trim_matches('/');
        if trimmed.is_empty() {
            return false;
        }
        for (idx, ch) in trimmed.char_indices() {
            if ch == '/' && self.is_single_path_ignored(&trimmed[..idx], true) {
                return true;
            }
        }
        self.is_single_path_ignored(trimmed, is_dir)
    }
}

// Read a size-limited UTF-8 candidate even if the file grows after the metadata check.
fn search_file_text(path: &Path) -> Result<Option<String>> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

fn collect_project_files(
    root: &Path,
    start_relative: &str,
    include_ignored: bool,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<Vec<(PathBuf, String, String)>> {
    let dir = if start_relative == "." || start_relative.is_empty() {
        root.canonicalize()?
    } else {
        resolve(root, start_relative, false, &[])?
    };
    ensure!(dir.is_dir(), "指定路径不是目录");
    let root_canonical = root.canonicalize()?;
    let prefix = if start_relative == "." || start_relative.is_empty() {
        String::new()
    } else {
        start_relative
            .replace('\\', "/")
            .trim_matches('/')
            .to_owned()
    };

    let initial_matcher = if include_ignored {
        GitignoreMatcher::default()
    } else {
        GitignoreMatcher::for_directory(&root_canonical, &prefix)
    };
    if !include_ignored && !prefix.is_empty() && initial_matcher.is_ignored(&prefix, true) {
        return Ok(Vec::new());
    }

    let mut stack = vec![(dir, prefix, initial_matcher)];
    let mut results = Vec::new();
    let mut visited = 0usize;
    while let Some((current_dir, current_prefix, current_matcher)) = stack.pop() {
        let entries = match std::fs::read_dir(&current_dir) {
            Ok(entries) => entries,
            Err(err) if cancel.is_some() => return Err(err).context("无法遍历检索目录"),
            Err(_) => continue,
        };
        let mut dir_entries = Vec::new();
        for entry in entries {
            if let Some(cancel) = cancel {
                ensure!(!cancel.is_cancelled(), "任务已停止");
                visited += 1;
                ensure!(visited <= 100_000, "检索超过 100000 项，请缩小目录范围");
            }
            if let Ok(entry) = entry {
                dir_entries.push(entry);
            }
        }
        dir_entries.sort_by_key(|e| e.file_name());
        for entry in dir_entries.into_iter().rev() {
            ensure!(!cancel.is_some_and(|c| c.is_cancelled()), "任务已停止");
            let name = entry.file_name().to_string_lossy().into_owned();
            if skipped_dir(&name) || forbidden(&name) {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if std::fs::symlink_metadata(&path)
                    .map_or(true, |m| m.file_attributes() & 0x400 != 0)
                {
                    continue;
                }
            }
            let rel = if current_prefix.is_empty() {
                name.clone()
            } else {
                format!("{current_prefix}/{name}")
            };
            if forbidden(&rel) {
                continue;
            }
            let is_dir = ft.is_dir();
            if !include_ignored && current_matcher.is_ignored(&rel, is_dir) {
                continue;
            }
            if is_dir
                && let Ok(canon) = path.canonicalize()
                && canon.starts_with(&root_canonical)
            {
                let next_matcher = if include_ignored {
                    current_matcher.clone()
                } else {
                    current_matcher.with_dir(&canon, &rel)
                };
                stack.push((canon, rel, next_matcher));
            } else if ft.is_file()
                && let Ok(canon) = path.canonicalize()
                && canon.starts_with(&root_canonical)
            {
                results.push((canon, rel, name));
            }
        }
    }
    results.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(results)
}

fn parse_porcelain_branch(header_rest: &str) -> String {
    let trimmed = header_rest.trim();
    if let Some(rest) = trimmed
        .strip_prefix("No commits yet on ")
        .or_else(|| trimmed.strip_prefix("Initial commit on "))
    {
        return rest.trim().to_owned();
    }
    if trimmed.starts_with("HEAD (") {
        return "HEAD".to_owned();
    }
    let before_ahead = trimmed.split(" [").next().unwrap_or(trimmed);
    before_ahead
        .split("...")
        .next()
        .unwrap_or(before_ahead)
        .trim()
        .to_owned()
}

fn normalize_porcelain_path(raw: &str) -> String {
    let target = raw.rsplit(" -> ").next().unwrap_or(raw).trim();
    let unquoted = target
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(target);
    unquoted.replace('\\', "/")
}

async fn run_git_readonly(
    executable: &Path,
    cwd: &Path,
    extra_args: &[&str],
) -> Result<(i32, String)> {
    let mut command = tokio::process::Command::new(executable);
    command.env_clear();
    for name in ["PATH", "SystemRoot", "WINDIR", "SYSTEMDRIVE", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=",
            "-c",
            "diff.external=",
            "-c",
            "core.quotepath=false",
        ])
        .args(extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    use crate::process_lifetime::{CompletionReason, Outcome};
    let spec = crate::execution_authority::command_spec(
        &command,
        256 * 1024,
        Instant::now() + Duration::from_secs(10),
    )?;
    let outcome = if crate::execution_authority::current().is_some() {
        crate::execution_authority::run_process(spec).await?
    } else {
        crate::process_lifetime::run_direct(spec).await
    };
    match outcome {
        Outcome::Settled(res) if res.reason == CompletionReason::Exited => {
            let stdout = String::from_utf8_lossy(&res.stdout.bytes).into_owned();
            Ok((res.exit_code as i32, stdout))
        }
        _ => bail!("执行 git 状态命令超时或中断"),
    }
}

async fn execute_git_status(root: &Path, args: &Value) -> Result<Value> {
    let root_canon = root.canonicalize().context("工作目录不存在")?;
    if let Some(rel) = args.get("path").and_then(Value::as_str)
        && rel != "."
        && !rel.is_empty()
    {
        let _ = resolve(root, rel, false, &[])?;
    }

    let executable = std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(if cfg!(windows) { "git.exe" } else { "git" }))
                .filter_map(|candidate| candidate.canonicalize().ok())
                .find(|candidate| candidate.is_file() && !candidate.starts_with(&root_canon))
        })
        .context("未找到系统 git 可执行文件")?;

    let (status_code, status_out) = run_git_readonly(
        &executable,
        &root_canon,
        &["status", "--porcelain=v1", "-b"],
    )
    .await?;
    ensure!(
        status_code == 0,
        "当前工作目录不是有效的 Git 仓库或无法读取 Git 状态"
    );

    let mut branch = String::from("HEAD");
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();

    for raw_line in status_out.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(rest) = line.strip_prefix("## ") {
            branch = parse_porcelain_branch(rest);
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        let path = normalize_porcelain_path(&line[3..]);
        if x == '?' && y == '?' {
            untracked.push(path);
        } else if x == '!' && y == '!' {
            continue;
        } else {
            if x != ' ' && x != '?' {
                staged.push(path.clone());
            }
            if y != ' ' && y != '?' {
                modified.push(path);
            }
        }
    }

    let (stat_code, stat_out) =
        run_git_readonly(&executable, &root_canon, &["diff", "--stat"]).await?;
    let mut diff_stat = if stat_code == 0 {
        stat_out.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        String::new()
    };
    if diff_stat.is_empty()
        && !staged.is_empty()
        && let Ok((0, cached_stat)) =
            run_git_readonly(&executable, &root_canon, &["diff", "--cached", "--stat"]).await
    {
        diff_stat = cached_stat.trim_end_matches(['\r', '\n']).to_owned();
    }

    let clean = staged.is_empty() && modified.is_empty() && untracked.is_empty();
    Ok(json!({
        "branch": branch,
        "clean": clean,
        "staged": staged,
        "modified": modified,
        "untracked": untracked,
        "diff_stat": diff_stat
    }))
}

fn definition(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false
            }
        }
    })
}

pub fn definitions(task: &Task) -> Vec<Value> {
    if !task.spec.tools {
        return vec![];
    }
    let mut tools = vec![
        definition(
            "list_files",
            "List up to 200 entries in a project directory. Respects .gitignore by default unless include_ignored is true.",
            json!({
                "path": { "type": "string" },
                "include_ignored": { "type": "boolean" }
            }),
            &["path"],
        ),
        definition(
            "glob_files",
            "Recursively discover project files matching a glob pattern (e.g. '**/*.rs', '*auth*', 'src/**/*.ts'). Respects .gitignore by default unless include_ignored is true.",
            json!({
                "pattern": { "type": "string" },
                "path": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500 },
                "include_ignored": { "type": "boolean" }
            }),
            &["pattern"],
        ),
        definition(
            "read_file",
            "Read a UTF-8 project file up to 4 MiB with optional 1-based inclusive start_line and end_line. Returns line-numbered content.",
            json!({
                "path": { "type": "string" },
                "start_line": { "type": "integer", "minimum": 1 },
                "end_line": { "type": "integer", "minimum": 1 }
            }),
            &["path"],
        ),
        definition(
            "search_files",
            "Search UTF-8 project files using regex (default, with literal fallback) or literal text, optional file_pattern glob, case_insensitive flag, and surrounding context_lines. Respects .gitignore by default unless include_ignored is true.",
            json!({
                "query": { "type": "string" },
                "path": { "type": "string" },
                "is_regex": { "type": "boolean" },
                "case_insensitive": { "type": "boolean" },
                "file_pattern": { "type": "string" },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 10 },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 100 },
                "include_ignored": { "type": "boolean" }
            }),
            &["query"],
        ),
        definition(
            "git_status",
            "Inspect read-only Git repository status: current branch, clean flag, staged/modified/untracked file lists, and diff --stat summary.",
            json!({
                "path": { "type": "string" }
            }),
            &[],
        ),
        definition(
            "todo_write",
            "Create or update the structured task checklist for the current run. Replaces the checklist with the provided todos array.",
            json!({
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "content": { "type": "string" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"]
                            }
                        },
                        "required": ["id", "content", "status"],
                        "additionalProperties": false
                    }
                }
            }),
            &["todos"],
        ),
        definition(
            "todo_read",
            "Read the current structured task checklist for this run.",
            json!({}),
            &[],
        ),
    ];
    if !task.spec.write_scopes.is_empty() {
        tools.push(definition(
            "edit_file",
            "Surgically edit an existing UTF-8 file within assigned write scopes. Provide (path, old_string, new_string, optional replace_all), batch (path, edits: [{old_string, new_string, replace_all}]), or unified diff hunks (path, patch: '@@ -a,b +c,d @@ ...'). Preserves CRLF/LF line endings and returns a unified diff.",
            json!({
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean" },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string" },
                            "new_string": { "type": "string" },
                            "replace_all": { "type": "boolean" }
                        },
                        "required": ["old_string", "new_string"],
                        "additionalProperties": false
                    }
                },
                "patch": { "type": "string" }
            }),
            &["path"],
        ));
        tools.push(definition(
            "write_file",
            "Create or replace a UTF-8 file strictly within assigned write scopes. Returns bytes written and a unified diff.",
            json!({
                "path": { "type": "string" },
                "content": { "type": "string" }
            }),
            &["path", "content"],
        ));
    }
    if task.spec.allow_commands {
        tools.push(definition(
            "run_command",
            "Run a PowerShell command in the project directory (pwsh.exe or powershell.exe). Default timeout is 30s (1-600s).",
            json!({
                "command": { "type": "string" },
                "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 600 }
            }),
            &["command"],
        ));
    }
    tools
}

pub async fn execute(task: &Task, name: &str, args: &Value) -> Result<Value> {
    execute_builtin(task, name, args).await
}

pub(crate) async fn execute_builtin(task: &Task, name: &str, args: &Value) -> Result<Value> {
    ensure!(task.spec.tools, "当前任务未启用项目工具");
    let root = Path::new(&task.workspace);
    crate::workspace_identity::verify_directory_policy(root)?;

    match name {
        "list_files" => {
            let relative = args["path"].as_str().unwrap_or(".");
            let include_ignored = args["include_ignored"].as_bool().unwrap_or(false);
            let root_canonical = root.canonicalize()?;
            let (dir, prefix) = if relative == "." || relative.is_empty() {
                (root_canonical.clone(), String::new())
            } else {
                let resolved = resolve(root, relative, false, &[])?;
                let norm = relative.replace('\\', "/").trim_matches('/').to_owned();
                (resolved, norm)
            };
            let gitignore = if include_ignored {
                GitignoreMatcher::default()
            } else {
                GitignoreMatcher::for_directory(&root_canonical, &prefix)
            };
            let mut raw_entries: Vec<_> =
                std::fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
            raw_entries.sort_by_key(|e| e.file_name());
            let mut entries_out = Vec::new();
            for entry in raw_entries {
                if entries_out.len() >= 200 {
                    break;
                }
                let entry_name = entry.file_name().to_string_lossy().into_owned();
                if skipped_dir(&entry_name) || forbidden(&entry_name) {
                    continue;
                }
                let ft = entry.file_type()?;
                if ft.is_symlink() {
                    continue;
                }
                let rel = if prefix.is_empty() {
                    entry_name.clone()
                } else {
                    format!("{prefix}/{entry_name}")
                };
                if forbidden(&rel) {
                    continue;
                }
                let is_dir = ft.is_dir();
                if !include_ignored && gitignore.is_ignored(&rel, is_dir) {
                    continue;
                }
                entries_out.push(json!({
                    "name": entry_name,
                    "directory": is_dir
                }));
            }
            Ok(json!({ "files": entries_out, "limit": 200 }))
        }
        "glob_files" => {
            let pattern = args["pattern"].as_str().context("缺少 pattern")?;
            let sub_path = args["path"].as_str().unwrap_or(".");
            let include_ignored = args["include_ignored"].as_bool().unwrap_or(false);
            let limit = args["limit"].as_u64().unwrap_or(200).clamp(1, 500) as usize;
            let rx = glob_to_regex(pattern)?;
            let all_files = collect_project_files(root, sub_path, include_ignored, None)?;
            let mut matched: Vec<String> = all_files
                .into_iter()
                .filter(|(_, rel, file_name)| matches_glob(&rx, pattern, rel, file_name))
                .map(|(_, rel, _)| rel)
                .collect();
            matched.sort();
            let total = matched.len();
            let truncated = total > limit;
            matched.truncate(limit);
            Ok(json!({
                "files": matched,
                "count": matched.len(),
                "total_matched": total,
                "truncated": truncated
            }))
        }
        "read_file" => {
            let rel = args["path"].as_str().context("缺少 path")?;
            let path = resolve(root, rel, false, &[])?;
            let bytes = read_safe_file(&path)?.context("文件不存在")?;
            let full_content = String::from_utf8(bytes).context("文件不是有效 UTF-8")?;
            let all_lines: Vec<&str> = full_content.split_inclusive('\n').collect();
            let total_lines = all_lines.len();

            let explicit_range = args.get("start_line").is_some() || args.get("end_line").is_some();
            let start_line = args["start_line"].as_u64().unwrap_or(1).max(1) as usize;
            let default_end = if explicit_range {
                total_lines.max(1)
            } else {
                total_lines.clamp(1, DEFAULT_READ_MAX_LINES)
            };
            let end_line = args["end_line"]
                .as_u64()
                .map(|v| v.max(1) as usize)
                .unwrap_or(default_end);
            ensure!(start_line <= end_line, "start_line 不能大于 end_line");

            if total_lines == 0 {
                return Ok(json!({
                    "content": "",
                    "numbered_content": "",
                    "total_lines": 0,
                    "shown_range": [0, 0],
                    "truncated": false
                }));
            }

            let clamped_start = start_line.min(total_lines);
            let clamped_end = end_line.min(total_lines);
            let slice = if start_line > total_lines {
                &[][..]
            } else {
                &all_lines[clamped_start - 1..clamped_end]
            };
            let content: String = slice.concat();
            let mut numbered = String::with_capacity(content.len() + slice.len() * 8);
            for (idx, line) in slice.iter().enumerate() {
                let line_no = clamped_start + idx;
                let clean = line.trim_end_matches(['\r', '\n']);
                numbered.push_str(&format!("{line_no}: {clean}\n"));
            }
            let truncated = clamped_start > 1 || clamped_end < total_lines;
            Ok(json!({
                "content": content,
                "numbered_content": numbered,
                "total_lines": total_lines,
                "shown_range": if slice.is_empty() { [0, 0] } else { [clamped_start, clamped_end] },
                "truncated": truncated
            }))
        }
        "search_files" => {
            let query = args["query"].as_str().context("缺少 query")?;
            ensure!(
                !query.is_empty() && query.len() <= 4096,
                "搜索内容为空或过长"
            );
            let sub_path = args["path"].as_str().unwrap_or(".");
            let is_regex = args["is_regex"].as_bool().unwrap_or(true);
            let case_insensitive = args["case_insensitive"].as_bool().unwrap_or(false);
            let context_lines = args["context_lines"].as_u64().unwrap_or(1).min(10) as usize;
            let max_results = args["max_results"].as_u64().unwrap_or(50).clamp(1, 100) as usize;
            let file_glob = match args.get("file_pattern").and_then(Value::as_str) {
                Some(pat) if !pat.is_empty() => Some((pat, glob_to_regex(pat)?)),
                _ => None,
            };

            let matcher = {
                let try_pattern = if is_regex {
                    RegexBuilder::new(query)
                        .case_insensitive(case_insensitive)
                        .build()
                } else {
                    RegexBuilder::new(&regex::escape(query))
                        .case_insensitive(case_insensitive)
                        .build()
                };
                match try_pattern {
                    Ok(rx) => rx,
                    Err(_) => RegexBuilder::new(&regex::escape(query))
                        .case_insensitive(case_insensitive)
                        .build()
                        .context("无法构建搜索模式")?,
                }
            };

            let include_ignored = args["include_ignored"].as_bool().unwrap_or(false);
            let files = collect_project_files(root, sub_path, include_ignored, None)?;
            let mut matches = Vec::new();
            let mut truncated = false;

            for (canon_path, rel_path, file_name) in files {
                if let Some((raw_pat, ref rx)) = file_glob
                    && !matches_glob(rx, raw_pat, &rel_path, &file_name)
                {
                    continue;
                }
                let Ok(Some(content)) = search_file_text(&canon_path) else {
                    continue;
                };
                let lines: Vec<&str> = content.lines().collect();
                for (idx, line) in lines.iter().enumerate() {
                    if matcher.is_match(line) {
                        if matches.len() >= max_results {
                            truncated = true;
                            break;
                        }
                        let ctx_start = idx.saturating_sub(context_lines);
                        let ctx_end = (idx + 1 + context_lines).min(lines.len());
                        let context: Vec<String> = (ctx_start..ctx_end)
                            .map(|i| {
                                let snippet: String = lines[i].chars().take(300).collect();
                                format!("{}: {}", i + 1, snippet)
                            })
                            .collect();
                        matches.push(json!({
                            "path": rel_path,
                            "line": idx + 1,
                            "text": line.chars().take(400).collect::<String>(),
                            "context": context
                        }));
                    }
                }
                if truncated {
                    break;
                }
            }

            Ok(json!({
                "matches": matches,
                "truncated": truncated,
                "limit": max_results
            }))
        }
        "git_status" => execute_git_status(root, args).await,
        "todo_write" => {
            let items = parse_todo_items(args)?;
            {
                let mut guard = todo_store().lock().unwrap_or_else(|e| e.into_inner());
                guard.insert(task.run_id.clone(), items.clone());
            }
            Ok(todo_response(&task.run_id, &items))
        }
        "todo_read" => {
            let items = todos_for_task(task);
            Ok(todo_response(&task.run_id, &items))
        }
        "write_file" => {
            ensure!(!task.spec.write_scopes.is_empty(), "当前任务无文件写入权限");
            let preview =
                preview_file_mutation_full(root, "write_file", args, &task.spec.write_scopes)?;
            atomic_replace(&preview.target, &preview.after)?;
            Ok(json!({
                "path": preview.display_path,
                "written": args["path"].as_str().unwrap_or(&preview.display_path),
                "bytes": preview.after.len(),
                "diff": preview.diff
            }))
        }
        "edit_file" => {
            ensure!(!task.spec.write_scopes.is_empty(), "当前任务无文件写入权限");
            let preview =
                preview_file_mutation_full(root, "edit_file", args, &task.spec.write_scopes)?;
            atomic_replace(&preview.target, &preview.after)?;
            Ok(json!({
                "path": preview.display_path,
                "replacements": preview.replacements,
                "bytes": preview.after.len(),
                "diff": preview.diff
            }))
        }
        "run_command" => crate::command_execution::execute(task, args).await,
        _ => bail!("未知工具：{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Route, TaskSpec};

    fn test_task(workspace: &Path, scopes: Vec<String>) -> Task {
        Task {
            id: "task".into(),
            run_id: "run".into(),
            spec: TaskSpec {
                name: "worker".into(),
                role: "coder".into(),
                route_id: "route".into(),
                prompt: "work".into(),
                depends_on: vec![],
                write_scopes: scopes,
                tools: true,
                allow_commands: false,
                max_rounds: 2,
            },
            route: Route {
                id: "route".into(),
                name: "route".into(),
                base_url: "http://127.0.0.1".into(),
                model: "model".into(),
                max_tokens: 16,
                parallel_limit: 1,
                key_env: None,
            },
            workspace: workspace.to_string_lossy().into(),
            status: "queued".into(),
            output: String::new(),
            error: None,
            messages: vec![],
            usage: Value::Null,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn standalone_workspace_does_not_advertise_engine_exploration() {
        let task = test_task(Path::new("."), vec![]);
        assert!(
            definitions(&task)
                .iter()
                .all(|tool| tool["function"]["name"] != "explore_files")
        );
    }

    #[test]
    fn file_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        assert!(resolve(temp.path(), "../escape", false, &[]).is_err());
        assert!(resolve(temp.path(), ".env", false, &[]).is_err());
        assert!(resolve(temp.path(), "src/a.rs", true, &["src".into()]).is_ok());
        assert!(resolve(temp.path(), "src2/a.rs", true, &["src".into()]).is_err());
        assert!(resolve(temp.path(), "src/a.rs", true, &[]).is_err());
    }

    #[tokio::test]
    async fn read_edit_glob_and_search_work_end_to_end() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::create_dir_all(temp.path().join("target")).unwrap();
        std::fs::write(temp.path().join("target/ignored.rs"), "fn skip() {}\n").unwrap();
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "fn alpha() {\r\n    let x = 1;\r\n}\r\nfn beta() {\r\n    let x = 1;\r\n}\r\n",
        )
        .unwrap();

        let task = test_task(temp.path(), vec!["src".into()]);

        // 1. glob_files skips target/ and returns sorted paths
        let globbed = execute(&task, "glob_files", &json!({"pattern": "**/*.rs"}))
            .await
            .unwrap();
        assert_eq!(globbed["files"], json!(["src/lib.rs"]));

        // 2. read_file with start_line / end_line
        let read_out = execute(
            &task,
            "read_file",
            &json!({"path": "src/lib.rs", "start_line": 1, "end_line": 3}),
        )
        .await
        .unwrap();
        assert!(
            read_out["numbered_content"]
                .as_str()
                .unwrap()
                .contains("2:     let x = 1;")
        );
        assert_eq!(read_out["total_lines"], 6);
        assert_eq!(read_out["truncated"], true);

        // 3. edit_file rejects ambiguous old_string with exact line numbers
        let err = execute(
            &task,
            "edit_file",
            &json!({"path": "src/lib.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("2, 5"),
            "expected line numbers 2, 5 in error: {err}"
        );

        // 4. edit_file matches LF input against CRLF file and preserves CRLF!
        let edited = execute(
            &task,
            "edit_file",
            &json!({
                "path": "src/lib.rs",
                "old_string": "fn alpha() {\n    let x = 1;\n}",
                "new_string": "fn alpha() {\n    let x = 42;\n}"
            }),
        )
        .await
        .unwrap();
        assert_eq!(edited["replacements"], 1);
        assert!(
            edited["diff"]
                .as_str()
                .unwrap()
                .contains("+    let x = 42;")
        );
        let disk = std::fs::read_to_string(temp.path().join("src/lib.rs")).unwrap();
        assert!(disk.contains("fn alpha() {\r\n    let x = 42;\r\n}\r\n"));

        // 5. search_files with regex + context
        let searched = execute(
            &task,
            "search_files",
            &json!({"query": "let x = \\d+", "file_pattern": "*.rs", "context_lines": 1}),
        )
        .await
        .unwrap();
        assert_eq!(searched["matches"].as_array().unwrap().len(), 2);
        assert_eq!(searched["matches"][0]["path"], "src/lib.rs");
        assert_eq!(searched["matches"][0]["line"], 2);
    }
}
