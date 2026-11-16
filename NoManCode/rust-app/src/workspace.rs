use crate::{
    domain::{Task, scope_path},
    wasm,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

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
    ensure!(metadata.len() <= 262_144, "文件大于 256 KiB");
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
    let temp = parent.join(format!(".peachsh-{}.tmp", uuid::Uuid::new_v4()));
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
                | ".credentials.yaml"
                | "peachsh.sqlite3"
                | "peachsh.sqlite3-wal"
                | "peachsh.sqlite3-shm"
        ) || p == ".env"
            || p.starts_with(".env.")
            || p.ends_with(".pem")
            || p.ends_with(".key")
    })
}

fn within_write_scopes(normalized: &str, scopes: &[String]) -> bool {
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
        // Windows 8.3 names can hide both protected leaves and directories.
        // Check every existing component's real spelling, not just the input.
        ensure!(
            !forbidden(actual_relative.to_str().context("文件路径不是 UTF-8")?),
            "此路径不允许由文件工具访问"
        );
        // Preserve the resolved ancestor when a later component is absent.
        // Returning the canonical spelling also avoids reopening the alias.
        target = canonical;
    }
    if writing {
        // Admission/conflict checks use declared lexical scopes. An alias-only
        // scope must not silently authorize a different canonical namespace.
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
fn definition(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}})
}
pub fn definitions(task: &Task) -> Vec<Value> {
    if !task.spec.tools {
        return vec![];
    }
    let mut tools = vec![
        definition(
            "list_files",
            "List up to 200 project files. path is a relative directory, or '.' for project root.",
            json!({"path":{"type":"string"}}),
            &["path"],
        ),
        definition(
            "read_file",
            "Read a UTF-8 project file up to 256 KiB.",
            json!({"path":{"type":"string"}}),
            &["path"],
        ),
        definition(
            "search_files",
            "Search UTF-8 project files for a literal text fragment. Returns matching paths and line numbers.",
            json!({"query":{"type":"string"},"path":{"type":"string"},"max_results":{"type":"integer","minimum":1,"maximum":100}}),
            &["query"],
        ),
    ];
    if !task.spec.write_scopes.is_empty() {
        tools.push(definition("write_file","Create or replace a UTF-8 file strictly within assigned write scopes. Protected recovery data is stored separately; events contain safe metadata only.",json!({"path":{"type":"string"},"content":{"type":"string"}}),&["path","content"]));
    }
    if task.spec.allow_commands {
        tools.push(definition("run_command","Run a PowerShell command in the project directory. Default limit is 30 seconds; timeout_seconds may be raised up to 600 when the user explicitly enabled command access. Command access is not restricted by file write scopes.",json!({"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600}}),&["command"]));
    }
    tools.push(definition("run_wasm","Run a pre-installed JSON-only WASM plugin. It has no host imports, filesystem, network, clock or secrets.",json!({"plugin":{"type":"string","pattern":"^[a-z0-9][a-z0-9-]*$"},"input":{"type":"object"}}),&["plugin","input"]));
    tools
}
pub async fn execute(task: &Task, name: &str, args: &Value) -> Result<Value> {
    ensure!(task.spec.tools, "当前任务未启用项目工具");
    let root = Path::new(&task.workspace);
    crate::workspace_identity::verify_directory_policy(root)?;
    match name {
        "list_files" => {
            let relative = args["path"].as_str().unwrap_or(".");
            let dir = if relative == "." {
                root.canonicalize()?
            } else {
                resolve(root, relative, false, &[])?
            };
            let mut files = vec![];
            for entry in std::fs::read_dir(dir)?.take(200) {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if forbidden(&name) {
                    continue;
                }
                files.push(json!({"name":name,"directory":entry.file_type()?.is_dir()}));
            }
            Ok(json!({"files":files,"limit":200}))
        }
        "read_file" => {
            let path = resolve(
                root,
                args["path"].as_str().context("缺少 path")?,
                false,
                &[],
            )?;
            ensure!(
                std::fs::metadata(&path)?.len() <= 262_144,
                "文件大于 256 KiB"
            );
            Ok(json!({"content":std::fs::read_to_string(path)?}))
        }
        "search_files" => {
            let query = args["query"].as_str().context("缺少 query")?;
            ensure!(
                !query.is_empty() && query.len() <= 4096,
                "搜索内容为空或过长"
            );
            let relative = args["path"].as_str().unwrap_or(".");
            let dir = if relative == "." {
                root.canonicalize()?
            } else {
                resolve(root, relative, false, &[])?
            };
            ensure!(dir.is_dir(), "搜索路径不是目录");
            let max_results = args["max_results"].as_u64().unwrap_or(50).clamp(1, 100) as usize;
            let root_canonical = root.canonicalize()?;
            let prefix = if relative == "." {
                String::new()
            } else {
                relative.trim_matches('/').to_owned()
            };
            let mut pending = vec![(dir, prefix)];
            let mut matches = vec![];
            while let Some((directory, prefix)) = pending.pop() {
                crate::workspace_identity::verify_directory_policy(&directory)?;
                for entry in std::fs::read_dir(&directory)? {
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if forbidden(&name) {
                        continue;
                    }
                    let path = entry.path();
                    let relative_path = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}/{name}")
                    };
                    let file_type = entry.file_type()?;
                    if file_type.is_dir() {
                        crate::workspace_identity::verify_directory_policy(&path)?;
                        pending.push((path, relative_path));
                        continue;
                    }
                    if !file_type.is_file()
                        || std::fs::metadata(&path)?.len() > 512 * 1024
                        || !path.canonicalize()?.starts_with(&root_canonical)
                    {
                        continue;
                    }
                    resolve(root, &relative_path, false, &[])?;
                    let Ok(content) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    for (line_number, line) in content.lines().enumerate() {
                        if line.contains(query) {
                            matches.push(json!({
                                "path": relative_path,
                                "line": line_number + 1,
                                "text": line.chars().take(400).collect::<String>()
                            }));
                            if matches.len() >= max_results {
                                break;
                            }
                        }
                    }
                    if matches.len() >= max_results {
                        break;
                    }
                }
                if matches.len() >= max_results {
                    break;
                }
            }
            Ok(json!({"matches":matches,"limit":max_results}))
        }
        "write_file" => {
            let relative = args["path"].as_str().context("缺少 path")?;
            let content = args["content"].as_str().context("缺少 content")?;
            ensure!(content.len() <= 262_144, "单个文件写入大于 256 KiB");
            let path = resolve(root, relative, true, &task.spec.write_scopes)?;
            // Avoid replacing a hard link that could mutate a file outside the project.
            let parent = path.parent().context("文件没有父目录")?;
            std::fs::create_dir_all(parent)?;
            resolve(root, relative, true, &task.spec.write_scopes)?;
            atomic_replace(&path, content.as_bytes())?;
            Ok(json!({"written":relative,"bytes":content.len()}))
        }
        "run_command" => crate::command_execution::execute(task, args).await,
        "run_wasm" => {
            let plugin = args["plugin"].as_str().context("缺少 plugin")?.to_owned();
            let input = args.get("input").cloned().unwrap_or(Value::Null);
            let plugin_dir = root.join(".peachsh").join("plugins");
            let report = tokio::task::spawn_blocking(move || {
                let plugin = wasm::load(&plugin_dir, &plugin)?;
                wasm::run(&plugin, &input)
            })
            .await??;
            Ok(serde_json::to_value(report)?)
        }
        _ => bail!("未知工具：{name}"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Route, TaskSpec};
    #[test]
    fn file_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        assert!(resolve(temp.path(), "../escape", false, &[]).is_err());
        assert!(resolve(temp.path(), ".env", false, &[]).is_err());
        assert!(resolve(temp.path(), "src/a.rs", true, &["src".into()]).is_ok());
        assert!(resolve(temp.path(), "src2/a.rs", true, &["src".into()]).is_err());
        assert!(resolve(temp.path(), "src/a.rs", true, &[]).is_err());
    }

    #[cfg(windows)]
    fn short_leaf(path: &Path) -> String {
        use std::os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::OpenOptionsExt,
            io::AsRawHandle,
        };
        use std::sync::atomic::{AtomicU32, Ordering};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, GetShortPathNameW, SetFileShortNameW,
        };
        static NEXT_ALIAS: AtomicU32 = AtomicU32::new(1);
        let sequence = NEXT_ALIAS.fetch_add(1, Ordering::Relaxed);
        assert!(sequence <= 0xfffff, "synthetic alias counter exhausted");
        let alias = format!("NHC{sequence:05X}");
        let alias_wide: Vec<u16> = alias.encode_utf16().chain(Some(0)).collect();
        // Assign a real alias to this test's own object. This works when the
        // volume disables automatic 8.3 creation; no global setting is changed.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .access_mode(0x10000000) // GENERIC_ALL on this synthetic object only
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .expect("cannot open synthetic short-name fixture");
        assert!(
            unsafe { SetFileShortNameW(file.as_raw_handle(), alias_wide.as_ptr()) } != 0,
            "cannot assign synthetic 8.3 alias: {}",
            std::io::Error::last_os_error()
        );
        drop(file);
        let input: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut output = vec![0u16; 32_768];
        let count =
            unsafe { GetShortPathNameW(input.as_ptr(), output.as_mut_ptr(), output.len() as u32) }
                as usize;
        assert!(count > 0 && count < output.len(), "8.3 query failed");
        let short = PathBuf::from(std::ffi::OsString::from_wide(&output[..count]));
        let leaf = short.file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(leaf, alias, "Windows must publish the assigned real alias");
        assert_eq!(short.canonicalize().unwrap(), path.canonicalize().unwrap());
        leaf
    }

    #[cfg(windows)]
    #[test]
    fn windows_short_names_cannot_hide_protected_files() {
        let temp = tempfile::tempdir().unwrap();
        for name in [".credentials.yaml", ".env.production", "peachsh.sqlite3"] {
            let path = temp.path().join(name);
            std::fs::write(&path, "synthetic-non-secret").unwrap();
            let alias = short_leaf(&path);
            // This is the old input-only check's actual counterexample.
            assert!(!forbidden(&alias), "fixture must bypass lexical policy");
            for writing in [false, true] {
                let error = resolve(temp.path(), &alias, writing, &["*".into()]).unwrap_err();
                assert!(error.to_string().contains("此路径不允许"), "{error:#}");
            }
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                "synthetic-non-secret"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_short_directories_cannot_hide_protected_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        for name in [".ssh", ".git", "node_modules"] {
            let directory = temp.path().join(name);
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join("existing.txt"), "synthetic-non-secret").unwrap();
            let alias = short_leaf(&directory);
            assert!(!forbidden(&alias), "fixture must bypass lexical policy");
            for suffix in ["", "/existing.txt", "/missing/nested.txt"] {
                for writing in [false, true] {
                    let error = resolve(
                        temp.path(),
                        &format!("{alias}{suffix}"),
                        writing,
                        &["*".into()],
                    )
                    .unwrap_err();
                    assert!(error.to_string().contains("此路径不允许"), "{error:#}");
                }
            }
            assert!(!directory.join("missing").exists());
            assert_eq!(
                std::fs::read_to_string(directory.join("existing.txt")).unwrap(),
                "synthetic-non-secret"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_allowed_aliases_preserve_existing_and_missing_targets() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("ordinary-long-directory");
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("ordinary-long-file.txt");
        std::fs::write(&file, "safe fixture").unwrap();
        let directory_alias = short_leaf(&directory);
        let file_alias = short_leaf(&file);
        for writing in [false, true] {
            let existing = resolve(
                temp.path(),
                &format!("{directory_alias}/{file_alias}"),
                writing,
                &["*".into()],
            )
            .unwrap();
            assert_eq!(existing, file.canonicalize().unwrap());
            let missing = resolve(
                temp.path(),
                &format!("{directory_alias}/new/nested.txt"),
                writing,
                &["*".into()],
            )
            .unwrap();
            assert_eq!(
                missing,
                directory.canonicalize().unwrap().join("new/nested.txt")
            );
            assert!(!missing.exists());
        }
        assert!(
            resolve(
                temp.path(),
                &format!("{directory_alias}/{file_alias}"),
                true,
                &["elsewhere".into()],
            )
            .is_err()
        );
        let (_, display, key) = change_target(
            temp.path(),
            &format!("{directory_alias}/new/nested.txt"),
            &["*".into()],
        )
        .unwrap();
        assert_eq!(display, "ordinary-long-directory/new/nested.txt");
        assert_eq!(key, display);
    }

    #[cfg(windows)]
    #[test]
    fn windows_alias_only_scope_cannot_authorize_another_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("ordinary-long-directory");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("existing.txt"), "scope sentinel").unwrap();
        let alias = short_leaf(&directory);
        let scopes = vec![alias.to_lowercase()];
        // Engine's lexical conflict detector considers these scopes disjoint.
        assert!(!crate::domain::overlaps(
            &scopes[0],
            "ordinary-long-directory"
        ));
        for suffix in ["existing.txt", "missing/nested.txt"] {
            let request = format!("{alias}/{suffix}");
            assert!(within_write_scopes(&scope_path(&request).unwrap(), &scopes));
            let error = resolve(temp.path(), &request, true, &scopes).unwrap_err();
            assert!(
                error.to_string().contains("写入超出当前成员范围"),
                "{error:#}"
            );
            assert!(resolve(temp.path(), &request, false, &[]).is_ok());
        }
        assert!(!directory.join("missing").exists());
        assert_eq!(
            std::fs::read_to_string(directory.join("existing.txt")).unwrap(),
            "scope sentinel"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_alias_below_declared_long_scope_remains_writable() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("src/ordinary-long-directory");
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("ordinary-long-file.txt");
        std::fs::write(&file, "scope sentinel").unwrap();
        let alias = short_leaf(&directory);
        let file_alias = short_leaf(&file);
        let scopes = vec!["src".into()];
        let existing = resolve(
            temp.path(),
            &format!("src/{alias}/{file_alias}"),
            true,
            &scopes,
        )
        .unwrap();
        assert_eq!(existing, file.canonicalize().unwrap());
        let missing = resolve(
            temp.path(),
            &format!("src/{alias}/new/nested.txt"),
            true,
            &scopes,
        )
        .unwrap();
        assert_eq!(
            missing,
            directory.canonicalize().unwrap().join("new/nested.txt")
        );
        assert!(!missing.exists());
        assert!(resolve(temp.path(), "src/plain/new.txt", true, &scopes).is_ok());
    }

    #[tokio::test]
    async fn search_reports_project_matches() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "fn peach() {}\n").unwrap();
        let task = Task {
            id: "task".into(),
            run_id: "run".into(),
            spec: TaskSpec {
                name: "worker".into(),
                role: "search".into(),
                route_id: "route".into(),
                prompt: "search".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: true,
                allow_commands: false,
                max_rounds: 1,
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
            workspace: temp.path().to_string_lossy().into(),
            status: "queued".into(),
            output: String::new(),
            error: None,
            messages: vec![],
            usage: Value::Null,
            created_at: 0,
            updated_at: 0,
        };
        let result = execute(&task, "search_files", &json!({"query":"peach"}))
            .await
            .unwrap();
        assert_eq!(result["matches"][0]["path"], "src/main.rs");
        assert_eq!(result["matches"][0]["line"], 1);
    }
}
