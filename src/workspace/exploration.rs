use super::{DEFAULT_READ_MAX_LINES, definition, read_safe_file, resolve};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};
use tokio_util::sync::CancellationToken;

const MAX_FILES: usize = 4;
const MAX_QUESTION_CHARS: usize = 2048;
const MAX_PATH_CHARS: usize = 256;
const EXCERPT_BYTES: usize = 8192;

pub(crate) fn exploration_definition() -> Value {
    definition(
        "explore_files",
        "Ask a separate, read-only model to summarize numbered excerpts from 1–4 distinct UTF-8 project files. Only its summary enters this conversation. Each excerpt covers up to the first 800 lines and 8 KiB of numbered text.",
        json!({
            "question": {"type": "string", "minLength": 1, "maxLength": MAX_QUESTION_CHARS},
            "paths": {"type": "array", "minItems": 1, "maxItems": MAX_FILES,
                "items": {"type": "string", "minLength": 1, "maxLength": MAX_PATH_CHARS}}
        }),
        &["question", "paths"],
    )
}

pub(crate) struct ExplorationRequest {
    pub(crate) question: String,
    pub(crate) paths: Vec<String>,
}

impl ExplorationRequest {
    pub(crate) fn parse(args: &Value) -> Result<Self> {
        let object = args.as_object().context("探索参数必须是对象")?;
        let question = args["question"].as_str().context("缺少探索问题")?;
        let paths = args["paths"].as_array().context("缺少探索文件列表")?;
        ensure!(object.len() == 2, "探索只接受 question 和 paths");
        ensure!(
            !question.trim().is_empty() && question.chars().count() <= MAX_QUESTION_CHARS,
            "探索问题必须在 1 至 {MAX_QUESTION_CHARS} 字符内"
        );
        ensure!(
            (1..=MAX_FILES).contains(&paths.len()),
            "每次探索需要 1 至 {MAX_FILES} 个文件"
        );
        let mut seen = HashSet::new();
        let mut selected = Vec::with_capacity(paths.len());
        for path in paths {
            let path = path.as_str().context("探索路径必须是字符串")?;
            ensure!(
                path != "*" && path.chars().count() <= MAX_PATH_CHARS,
                "探索需要明确的相对文件路径，长度上限为 {MAX_PATH_CHARS} 字符"
            );
            let normalized = crate::domain::scope_path(path)?;
            ensure!(seen.insert(normalized), "探索文件列表包含重复路径");
            selected.push(path.to_owned());
        }
        Ok(Self {
            question: question.trim().to_owned(),
            paths: selected,
        })
    }

    pub(crate) fn read_excerpts(&self, root: &Path, cancel: &CancellationToken) -> Result<String> {
        let mut excerpts = String::new();
        for relative in &self.paths {
            ensure!(!cancel.is_cancelled(), "任务已停止");
            let path = resolve(root, relative, false, &[])?;
            let bytes =
                read_safe_file(&path)?.with_context(|| format!("文件不存在: {relative}"))?;
            let text = std::str::from_utf8(&bytes)?;
            ensure!(!text.contains('\0'), "探索文件不是文本: {relative}");
            excerpts.push_str(&format!("\nFile: {relative}\n"));
            let start = excerpts.len();
            for (index, line) in text.lines().enumerate() {
                let prefix = format!("{}: ", index + 1);
                let remaining = EXCERPT_BYTES - (excerpts.len() - start);
                if index == DEFAULT_READ_MAX_LINES || remaining < prefix.len() + 1 {
                    excerpts.push_str("[truncated]\n");
                    break;
                }
                excerpts.push_str(&prefix);
                let end = line.floor_char_boundary(remaining - prefix.len() - 1);
                excerpts.push_str(&line[..end]);
                excerpts.push('\n');
                if end < line.len() {
                    excerpts.push_str("[truncated]\n");
                    break;
                }
            }
        }
        Ok(excerpts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_paths_and_bounds_numbered_excerpts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "first\n".to_owned() + &"界".repeat(5000),
        )
        .unwrap();
        for paths in [
            json!([]),
            json!(["a", "b", "c", "d", "e"]),
            json!(["*"]),
            json!(["../outside"]),
            json!(["src/a", "src\\a"]),
        ] {
            assert!(
                ExplorationRequest::parse(&json!({"question":"inspect","paths":paths})).is_err()
            );
        }
        for question in [" ".to_owned(), "问".repeat(MAX_QUESTION_CHARS + 1)] {
            assert!(
                ExplorationRequest::parse(&json!({"question":question,"paths":["lib.rs"]}))
                    .is_err()
            );
        }
        let parse = |paths| {
            ExplorationRequest::parse(&json!({"question":"inspect","paths":paths})).unwrap()
        };
        let cancel = CancellationToken::new();
        let text = parse(json!(["lib.rs"]))
            .read_excerpts(dir.path(), &cancel)
            .unwrap();
        assert!(text.starts_with("\nFile: lib.rs\n1: first\n2: 界"));
        assert!(text.ends_with("[truncated]\n"));
        assert!(text.len() <= EXCERPT_BYTES + 40);
        let forbidden = parse(json!(["lib.rs", ".env"]));
        assert!(forbidden.read_excerpts(dir.path(), &cancel).is_err());
        for bytes in [vec![0xff], b"prefix\0binary".to_vec()] {
            std::fs::write(dir.path().join("binary"), bytes).unwrap();
            assert!(
                parse(json!(["binary"]))
                    .read_excerpts(dir.path(), &cancel)
                    .is_err()
            );
        }
        cancel.cancel();
        assert!(
            parse(json!(["lib.rs"]))
                .read_excerpts(dir.path(), &cancel)
                .is_err()
        );
    }
}
