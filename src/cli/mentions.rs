//! Expansion of workspace file, line-range, and directory mentions in user prompts.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MENTION_MAX_FILE_BYTES: usize = 64 * 1024;
const MENTION_MAX_PROMPT_BYTES: usize = 95_000;
const MENTION_MAX_DIR_ENTRIES: usize = 200;

fn is_skipped_context_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
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
    ) || lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
}

fn extract_mention_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut prev_char: Option<char> = None;
    for (idx, ch) in text.char_indices() {
        if ch == '@' && prev_char.is_none_or(|p| !p.is_alphanumeric() && p != '_') {
            let after = &text[idx + ch.len_utf8()..];
            let end_rel = after
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            ')' | ']'
                                | '}'
                                | '>'
                                | '"'
                                | '\''
                                | '`'
                                | ','
                                | ';'
                                | '!'
                                | '?'
                                | '<'
                                | '('
                                | '['
                                | '{'
                        )
                })
                .unwrap_or(after.len());
            let mut candidate = &after[..end_rel];
            while candidate.len() > 1 && candidate.ends_with('.') && !candidate.ends_with("..") {
                candidate = &candidate[..candidate.len() - 1];
            }
            if !candidate.is_empty() {
                tokens.push(candidate.to_owned());
            }
        }
        prev_char = Some(ch);
    }
    tokens
}

struct ParsedMention {
    raw_path: String,
    range: Option<(usize, usize)>,
    explicit_dir: bool,
}

fn parse_mention_token(token: &str) -> Option<ParsedMention> {
    if let Some((path_part, range_part)) = token.rsplit_once(':') {
        if path_part.is_empty() {
            return None;
        }
        let range = if let Some((s_str, e_str)) = range_part.split_once('-') {
            let start = s_str.parse::<usize>().ok()?;
            let end = e_str.parse::<usize>().ok()?;
            if start == 0 || start > end {
                return None;
            }
            (start, end)
        } else {
            let single = range_part.parse::<usize>().ok()?;
            if single == 0 {
                return None;
            }
            (single, single)
        };
        let explicit_dir = path_part.ends_with('/') || path_part.ends_with('\\');
        Some(ParsedMention {
            raw_path: path_part.to_owned(),
            range: Some(range),
            explicit_dir,
        })
    } else {
        let explicit_dir = token.ends_with('/') || token.ends_with('\\');
        Some(ParsedMention {
            raw_path: token.to_owned(),
            range: None,
            explicit_dir,
        })
    }
}

pub(super) fn expand_context_mentions(
    project_root: &Path,
    raw_prompt: &str,
    json_output: bool,
) -> String {
    let tokens = extract_mention_tokens(raw_prompt);
    if tokens.is_empty() {
        return raw_prompt.to_owned();
    }

    let mut seen: HashSet<(PathBuf, Option<(usize, usize)>)> = HashSet::new();
    let mut blocks = String::new();
    let base_overhead = raw_prompt.len() + 64;

    for token in tokens {
        let Some(ParsedMention {
            raw_path,
            range,
            explicit_dir,
        }) = parse_mention_token(&token)
        else {
            continue;
        };
        let slash_path = raw_path.replace('\\', "/");
        let (resolved, display_rel) = if slash_path == "." || slash_path == "./" {
            if range.is_some()
                || coda::workspace_identity::verify_directory_policy(project_root).is_err()
            {
                continue;
            }
            let Ok(canon) = project_root.canonicalize() else {
                continue;
            };
            (canon, ".".to_owned())
        } else {
            let trimmed = slash_path.strip_prefix("./").unwrap_or(&slash_path);
            let clean_rel = trimmed.trim_end_matches('/');
            if clean_rel.is_empty() {
                continue;
            }
            let Ok(path) = coda::workspace::resolve(project_root, clean_rel, false, &[]) else {
                continue;
            };
            (path, clean_rel.to_owned())
        };

        let Ok(meta) = std::fs::symlink_metadata(&resolved) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if !seen.insert((resolved.clone(), range)) {
            continue;
        }

        if meta.is_dir() {
            if range.is_some() {
                continue;
            }
            let Ok(read_dir) = std::fs::read_dir(&resolved) else {
                continue;
            };
            let mut entries = Vec::new();
            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_skipped_context_entry(&name) {
                    continue;
                }
                let Ok(ft) = entry.file_type() else {
                    continue;
                };
                if ft.is_symlink() {
                    continue;
                }
                entries.push(if ft.is_dir() {
                    format!("{name}/")
                } else {
                    name
                });
            }
            entries.sort();
            if entries.len() > MENTION_MAX_DIR_ENTRIES {
                entries.truncate(MENTION_MAX_DIR_ENTRIES);
                entries.push("... [truncated]".to_owned());
            }
            let dir_label = if display_rel == "." {
                "./".to_owned()
            } else {
                format!("{display_rel}/")
            };
            let listing = entries.join("\n");
            let block = if listing.is_empty() {
                format!("<directory path=\"{dir_label}\">\n(empty)\n</directory>\n")
            } else {
                format!("<directory path=\"{dir_label}\">\n{listing}\n</directory>\n")
            };
            if base_overhead + blocks.len() + block.len() > MENTION_MAX_PROMPT_BYTES {
                break;
            }
            blocks.push_str(&block);
            if !json_output {
                eprintln!("\x1b[90m  📎 Attached {dir_label}\x1b[0m");
            }
        } else if meta.is_file() {
            if explicit_dir || meta.len() > 4 * 1024 * 1024 {
                continue;
            }
            let Ok(bytes) = std::fs::read(&resolved) else {
                continue;
            };
            let Ok(content) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let lines: Vec<&str> = content.lines().collect();
            let (start_idx, end_excl, label, header) = match range {
                Some((start, end)) => {
                    let s = start.saturating_sub(1);
                    let e = end.min(lines.len());
                    (
                        s,
                        e,
                        format!("{display_rel}:{start}-{end}"),
                        format!("<file path=\"{display_rel}\" lines=\"{start}-{end}\">\n"),
                    )
                }
                None => (
                    0,
                    lines.len(),
                    display_rel.clone(),
                    format!("<file path=\"{display_rel}\">\n"),
                ),
            };

            let remaining_prompt = MENTION_MAX_PROMPT_BYTES
                .saturating_sub(base_overhead + blocks.len() + header.len() + 64);
            if remaining_prompt < 64 {
                break;
            }
            let file_budget = MENTION_MAX_FILE_BYTES.min(remaining_prompt);

            let mut numbered = String::new();
            let mut truncated = false;
            if start_idx < end_excl && start_idx < lines.len() {
                for (offset, line) in lines[start_idx..end_excl].iter().enumerate() {
                    let line_no = start_idx + offset + 1;
                    let formatted = format!("{line_no}: {line}\n");
                    if numbered.len() + formatted.len() > file_budget {
                        truncated = true;
                        break;
                    }
                    numbered.push_str(&formatted);
                }
            }
            if truncated {
                numbered.push_str("... [truncated at 64 KB] ...\n");
            }

            blocks.push_str(&header);
            blocks.push_str(&numbered);
            blocks.push_str("</file>\n");
            if !json_output {
                eprintln!("\x1b[90m  📎 Attached {label}\x1b[0m");
            }
        }
    }

    if blocks.is_empty() {
        raw_prompt.to_owned()
    } else {
        format!("{raw_prompt}\n\n<context_files>\n{blocks}</context_files>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_mention_tokens_ignores_emails_and_handles_punctuation() {
        let tokens = extract_mention_tokens(
            "Contact foo@bar.com or user_1@example.org, then inspect (@src/lib.rs:10-40) and @src/.",
        );
        assert_eq!(tokens, vec!["src/lib.rs:10-40", "src/"]);
    }

    #[test]
    fn expand_context_mentions_attaches_files_ranges_and_dirs_safely() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "line_one\nline_two\nline_three\nline_four\n",
        )
        .unwrap();
        std::fs::write(temp.path().join(".env"), "SECRET=123").unwrap();

        let enriched = expand_context_mentions(
            temp.path(),
            "Explain @src/lib.rs:2-3 and @src/ while ignoring @.env and @missing.rs and dev@team.io",
            true,
        );
        assert!(enriched.contains("<context_files>"));
        assert!(enriched.contains("<file path=\"src/lib.rs\" lines=\"2-3\">"));
        assert!(enriched.contains("2: line_two\n3: line_three\n"));
        assert!(!enriched.contains("1: line_one"));
        assert!(!enriched.contains("4: line_four"));
        assert!(enriched.contains("<directory path=\"src/\">\nlib.rs\n</directory>"));
        assert!(!enriched.contains("SECRET=123"));
    }
}
