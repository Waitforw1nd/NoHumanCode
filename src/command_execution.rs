//! Bounded command execution through the platform shell.
use crate::domain::Task;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fmt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub(crate) struct CommandOutcomeUnknown;

impl fmt::Display for CommandOutcomeUnknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("命令执行结果未知；请检查工作区后创建新任务")
    }
}

impl std::error::Error for CommandOutcomeUnknown {}

const OUTPUT_LIMIT: usize = 65_536;

#[cfg(windows)]
fn shell_cwd(root: &Path) -> Result<PathBuf> {
    let raw = root.to_str().context("命令工作区路径不是有效 UTF-8")?;
    let stripped = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        raw.to_owned()
    };
    let cwd = PathBuf::from(stripped);
    ensure!(cwd.is_absolute(), "命令工作区必须是绝对路径");
    ensure!(
        cwd.canonicalize().context("命令启动目录不可用")? == root,
        "命令启动目录与真实工作区不一致"
    );
    Ok(cwd)
}

#[cfg(not(windows))]
fn shell_cwd(root: &Path) -> Result<PathBuf> {
    Ok(root.to_path_buf())
}

#[cfg(windows)]
pub(crate) fn resolve_shell(root: &Path) -> Result<PathBuf> {
    let search_path = |binary: &str| -> Option<PathBuf> {
        let paths = std::env::var_os("PATH")?;
        std::env::split_paths(&paths)
            .filter(|dir| dir.is_absolute())
            .filter_map(|dir| dir.join(binary).canonicalize().ok())
            .find(|candidate| candidate.is_file() && !candidate.starts_with(root))
    };

    if let Some(pwsh) = search_path("pwsh.exe") {
        return Ok(pwsh);
    }
    if let Some(ps) = search_path("powershell.exe") {
        return Ok(ps);
    }
    let mut fallbacks = Vec::new();
    if let Some(sysroot) = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("WINDIR")) {
        fallbacks
            .push(PathBuf::from(sysroot).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    }
    fallbacks.push(PathBuf::from(
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
    ));
    for candidate in fallbacks {
        if let Ok(canonical) = candidate.canonicalize()
            && canonical.is_file()
            && !canonical.starts_with(root)
        {
            return Ok(canonical);
        }
    }
    bail!("无法解析可信 PowerShell 程序 (pwsh.exe / powershell.exe)")
}

#[cfg(not(windows))]
fn resolve_shell(root: &Path) -> Result<PathBuf> {
    let shell = Path::new(if cfg!(target_os = "android") {
        "/system/bin/sh"
    } else {
        "/bin/sh"
    });
    let canonical = shell.canonicalize().context("找不到系统 sh")?;
    ensure!(
        canonical.is_file() && !canonical.starts_with(root),
        "无法解析可信 sh 程序"
    );
    // Keep argv[0] as sh when /bin/sh is a symlink to another shell.
    Ok(shell.to_owned())
}

pub(crate) async fn execute(task: &Task, args: &Value) -> Result<Value> {
    use crate::process_lifetime::{CompletionReason, Outcome, OverflowPolicy, ProcessSpec};

    ensure!(task.spec.allow_commands, "当前任务未授权运行命令");
    let command = args["command"].as_str().context("缺少 command")?;
    ensure!(command.len() <= 8192, "命令过长");
    let timeout_seconds = args["timeout_seconds"].as_u64().unwrap_or(30);
    ensure!(
        (1..=600).contains(&timeout_seconds),
        "命令超时时间必须在 1–600 秒之间"
    );

    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let root = std::fs::canonicalize(&task.workspace).context("命令工作区不可用")?;
    let executable = resolve_shell(&root)?;
    let environment = std::env::vars_os()
        .filter(|(name, _)| {
            let upper = name.to_string_lossy().to_ascii_uppercase();
            !upper.contains("KEY") && !upper.contains("TOKEN") && !upper.contains("SECRET")
        })
        .collect();

    let mut spec = ProcessSpec::new(
        executable,
        shell_cwd(&root)?,
        environment,
        (OUTPUT_LIMIT, OUTPUT_LIMIT),
        OverflowPolicy::TruncateAndDrain,
        deadline,
        Duration::from_secs(5),
    );

    #[cfg(windows)]
    let args = ["-NoProfile", "-NonInteractive", "-Command", command];
    #[cfg(not(windows))]
    let args = ["-c", command];
    spec.args = args.into_iter().map(Into::into).collect();

    let outcome = if crate::execution_authority::current().is_some() {
        crate::execution_authority::run_process(spec).await?
    } else {
        crate::process_lifetime::run_direct(spec).await
    };

    match outcome {
        Outcome::NotStarted { stage, os_error } => {
            bail!("无法启动命令解释器 ({stage}, {os_error:?})")
        }
        Outcome::Unknown => Err(CommandOutcomeUnknown.into()),
        Outcome::Settled(result) => match result.reason {
            CompletionReason::Exited => Ok(json!({
                "exit_code": result.exit_code as i32,
                "stdout": String::from_utf8_lossy(&result.stdout.bytes),
                "stderr": String::from_utf8_lossy(&result.stderr.bytes),
            })),
            CompletionReason::Deadline => {
                bail!("命令运行超过 {timeout_seconds} 秒；进程树与输出收集已结束")
            }
            CompletionReason::Cancelled => bail!("命令已取消；进程树与输出收集已结束"),
            CompletionReason::OutputLimit => bail!("命令输出超过限制；进程树与输出收集已结束"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Route, TaskSpec};

    fn sample_task(workspace: &Path) -> Task {
        Task {
            id: "cmd-test".into(),
            run_id: "run-test".into(),
            spec: TaskSpec {
                name: "coder".into(),
                role: "coder".into(),
                route_id: "r1".into(),
                prompt: "test".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: true,
                allow_commands: true,
                max_rounds: 2,
            },
            route: Route {
                id: "r1".into(),
                name: "r1".into(),
                base_url: "https://example.invalid/v1".into(),
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 1,
                key_env: None,
            },
            workspace: workspace.to_string_lossy().into_owned(),
            status: "running".into(),
            output: String::new(),
            error: None,
            messages: vec![],
            usage: Value::Null,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[tokio::test]
    async fn executes_platform_shell() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(resolve_shell(&root).is_ok());
        let task = sample_task(&root);
        let command = if cfg!(windows) {
            "Write-Output 'ok-42'"
        } else {
            "printf '%s' 'ok-42'"
        };
        let out = execute(&task, &json!({"command": command})).await.unwrap();
        assert_eq!(out["exit_code"], 0);
        assert!(out["stdout"].as_str().unwrap().contains("ok-42"));
    }
}
