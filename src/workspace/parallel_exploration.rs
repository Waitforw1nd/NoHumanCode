use super::{collect_project_files, definition, resolve, search_file_text};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use tokio_util::sync::CancellationToken;

const MAX_BRANCHES: usize = 4;
const MAX_FILES: usize = 4;
const MAX_EXCERPT_BYTES: usize = 40 * 1024;
const CONTEXT_BYTES: usize = 384;

pub(crate) fn parallel_exploration_definition() -> Value {
    definition(
        "explore_parallel",
        "Answer 1–4 code questions by searching project directories for literal, case-sensitive text. Each branch returns a job_id, a summary when available and numbered evidence. To use a saved result in a later batch, pass its job_id in that branch's depends_on.",
        json!({
            "branches": {"type":"array","minItems":1,"maxItems":MAX_BRANCHES,
                "items":{"type":"object","additionalProperties":false,
                    "required":["question","path","query"],"properties":{
                        "question":{"type":"string","minLength":1,"maxLength":2048},
                        "path":{"type":"string","minLength":1,"maxLength":256,"description":"Project-relative directory, or . for the root"},
                        "query":{"type":"string","minLength":1,"maxLength":4096,"description":"Single-line literal, up to 4096 UTF-8 bytes"},
                        "depends_on":{"type":"array","maxItems":32,"items":{"type":"string"},
                            "description":"Job IDs from earlier exploration batches in this workspace and session; their saved results become evidence."}
                    }}}
        }),
        &["branches"],
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ParallelExploration {
    pub(crate) branches: Vec<SearchBranch>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchBranch {
    pub(crate) question: String,
    pub(crate) path: String,
    pub(crate) query: String,
    #[serde(default)]
    pub(crate) depends_on: Vec<String>,
}

impl ParallelExploration {
    pub(crate) fn parse(args: &Value, root: &Path) -> Result<Self> {
        let mut request = Self::deserialize(args).context("并行检索参数无效")?;
        ensure!(
            (1..=MAX_BRANCHES).contains(&request.branches.len()),
            "每次需要 1 至 4 个检索分支"
        );
        for branch in &mut request.branches {
            ensure!(branch.depends_on.len() <= 32, "每支最多依赖 32 个子任务");
            let mut seen = std::collections::HashSet::new();
            for id in &branch.depends_on {
                crate::secrets::validate_persisted_id("depends_on", id)?;
                ensure!(seen.insert(id), "子任务依赖重复");
            }
            ensure!(
                !branch.question.trim().is_empty() && branch.question.chars().count() <= 2048,
                "检索问题必须在 1 至 2048 字符内"
            );
            ensure!(
                !branch.query.is_empty()
                    && branch.query.len() <= 4096
                    && !branch.query.contains(['\r', '\n', '\0']),
                "检索词须为 1 至 4096 字节的单行文本"
            );
            ensure!(
                branch.path.chars().count() <= 256 && !branch.path.contains('*'),
                "检索需要明确的相对目录"
            );
            branch
                .path
                .truncate(branch.path.trim_end_matches(['/', '\\']).len());
            if branch.path == "." {
                crate::workspace_identity::verify_directory_policy(root)?;
            } else {
                ensure!(
                    resolve(root, &branch.path, false, &[])?.is_dir(),
                    "指定路径不是目录"
                );
            }
        }
        Ok(request)
    }
}

#[derive(Default)]
pub(crate) struct SearchEvidence {
    pub(crate) excerpts: String,
    pub(crate) evidence: Vec<Value>,
}

impl SearchBranch {
    pub(crate) fn search(&self, root: &Path, cancel: &CancellationToken) -> Result<SearchEvidence> {
        ensure!(!cancel.is_cancelled(), "任务已停止");
        let files = collect_project_files(root, &self.path, false, Some(cancel))?;
        let mut result = SearchEvidence::default();
        let mut matched_files = 0;
        for (canonical, relative, _) in files {
            ensure!(!cancel.is_cancelled(), "任务已停止");
            // Recheck the workspace spelling as well as the canonical target before reading.
            let Ok(path) = resolve(root, &relative, false, &[]) else {
                continue;
            };
            if path != canonical {
                continue;
            }
            let Some(text) = search_file_text(&path)? else {
                continue;
            };
            if text.contains('\0') {
                continue;
            }
            let mut lines = text.lines().enumerate().peekable();
            let mut previous = None;
            let mut last_end = 0;
            while let Some((idx, line)) = lines.next() {
                ensure!(!cancel.is_cancelled(), "任务已停止");
                if line.contains(&self.query) {
                    let mut window = String::new();
                    if last_end == 0 {
                        window.push_str(&format!("\nFile: {relative}\n"));
                    }
                    let start = idx.max(1);
                    let mut end = idx + 1;
                    for (offset, source) in previous
                        .into_iter()
                        .chain(Some((idx, line)))
                        .chain(lines.peek().copied())
                    {
                        end = offset + 1;
                        if end > last_end {
                            window.push_str(&format!("{end}: {}\n", self.snippet(source)));
                        }
                    }
                    if result.excerpts.len() + window.len() > MAX_EXCERPT_BYTES {
                        return Ok(result);
                    }
                    result.excerpts.push_str(&window);
                    if last_end > 0 && start <= last_end + 1 {
                        result.evidence.last_mut().unwrap()["end_line"] = json!(end);
                    } else {
                        result
                            .evidence
                            .push(json!({"path":relative,"start_line":start,"end_line":end}));
                    }
                    last_end = end;
                }
                previous = Some((idx, line));
            }
            if last_end > 0 {
                matched_files += 1;
                if matched_files == MAX_FILES {
                    break;
                }
            }
        }
        Ok(result)
    }

    // Keep a late literal visible even on a minified or generated line.
    fn snippet<'a>(&self, line: &'a str) -> &'a str {
        let hit = line.find(&self.query);
        let start = line.ceil_char_boundary(hit.unwrap_or(0).saturating_sub(CONTEXT_BYTES / 2));
        let end =
            line.floor_char_boundary(start + CONTEXT_BYTES + hit.map_or(0, |_| self.query.len()));
        &line[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn branch(path: &str) -> Value {
        json!({"question":"Where is the literal?","path":path,"query":"needle.*"})
    }

    fn search(root: &Path, path: &str) -> SearchEvidence {
        let request =
            ParallelExploration::parse(&json!({"branches":[branch(path)]}), root).unwrap();
        request.branches[0]
            .search(root, &CancellationToken::new())
            .unwrap()
    }

    #[test]
    fn validates_dependency_ids_before_submitting_any_branch() {
        let root = tempfile::tempdir().unwrap();
        let branch_with = |ids: Value| {
            let mut b = branch(".");
            b["depends_on"] = ids;
            json!({"branches":[branch("."), b]})
        };
        let ids = (0..32).map(|i| format!("job-{i}")).collect::<Vec<_>>();
        let request = ParallelExploration::parse(&branch_with(json!(ids)), root.path()).unwrap();
        assert!(request.branches[0].depends_on.is_empty());
        assert_eq!(request.branches[1].depends_on, ids);
        for invalid in [
            json!(["job-a", "job-a"]),
            json!([""]),
            json!(["id with space"]),
            json!(vec!["a"; 33]),
        ] {
            assert!(ParallelExploration::parse(&branch_with(invalid), root.path()).is_err());
        }
    }

    #[test]
    fn validates_every_directory_before_search() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(root.path().join("not-dir"), "").unwrap();
        for invalid in [
            "",
            "*",
            "../outside",
            "C:\\outside",
            "/outside",
            ".git",
            "missing",
            "not-dir",
        ] {
            let args = json!({"branches":[branch("."),branch(invalid)]});
            assert!(
                ParallelExploration::parse(&args, root.path()).is_err(),
                "{invalid}"
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path(), root.path().join("link")).unwrap();
            assert!(
                ParallelExploration::parse(&json!({"branches":[branch("link")]}), root.path())
                    .is_err()
            );
        }
        let request =
            ParallelExploration::parse(&json!({"branches":[branch(".")]}), root.path()).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(request.branches[0].search(root.path(), &cancel).is_err());
    }

    #[test]
    fn literal_evidence_respects_ignore_policy_file_order_and_line_numbers() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        fs::create_dir(path.join("src")).unwrap();
        fs::create_dir(path.join("ignore-dir")).unwrap();
        fs::write(path.join(".gitignore"), "ignore-dir/\nsrc/ignore*\n").unwrap();
        for name in [".env", "ignore-dir/file", "src/ignore-me", "src/test.key"] {
            fs::write(path.join(name), "needle.* PRIVATE").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(path.join(".env"), path.join("src/link")).unwrap();
        fs::write(path.join("src/binary"), b"needle.* PRIVATE\0").unwrap();
        fs::write(path.join("src/almost"), "needleXX").unwrap();
        fs::write(
            path.join("src/a"),
            "line\n".repeat(801) + "needle.*\nline\nline\nneedle.*\nend\nline\nline\nneedle.*",
        )
        .unwrap();
        for name in ["z", "d", "c", "b"] {
            fs::write(path.join("src").join(name), "needle.*").unwrap();
        }
        let result = search(path, ".");
        assert_eq!(
            result.evidence,
            vec![
                json!({"path":"src/a","start_line":801,"end_line":806}),
                json!({"path":"src/a","start_line":808,"end_line":809}),
                json!({"path":"src/b","start_line":1,"end_line":1}),
                json!({"path":"src/c","start_line":1,"end_line":1}),
                json!({"path":"src/d","start_line":1,"end_line":1}),
            ]
        );
        assert!(result.excerpts.contains("802: needle.*"));
        assert_eq!(result.excerpts.matches("804: line").count(), 1);
        assert!(!result.excerpts.contains("PRIVATE"));
        assert!(search(path, "ignore-dir/").excerpts.is_empty());
    }

    #[test]
    fn long_unicode_lines_keep_the_literal_within_the_total_budget() {
        let root = tempfile::tempdir().unwrap();
        let line = format!("{}needle.*{}\n", "界".repeat(500), "界".repeat(300));
        fs::write(root.path().join("source"), line.repeat(400)).unwrap();
        let result = search(root.path(), ".");
        assert!(result.excerpts.len() <= MAX_EXCERPT_BYTES);
        let end = result.evidence[0]["end_line"].as_u64().unwrap() as usize;
        assert!(end < 400);
        assert_eq!(end, result.excerpts.matches("needle.*").count());
        assert!(result.excerpts.contains(&format!("{end}: ")));
    }
}
