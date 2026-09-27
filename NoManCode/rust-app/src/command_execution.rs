//! Bounded ownership of one approved command. This is not an OS process sandbox.
use crate::domain::Task;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{fmt, process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    time::{Instant, timeout_at},
};

/// Execution or cleanup could not be confirmed. Engine must preserve the claim.
#[derive(Debug)]
pub(crate) struct CommandOutcomeUnknown;
impl fmt::Display for CommandOutcomeUnknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("命令执行结果未知；请检查工作区后创建新任务")
    }
}
impl std::error::Error for CommandOutcomeUnknown {}

pub(crate) async fn execute(task: &Task, args: &Value) -> Result<Value> {
    ensure!(task.spec.allow_commands, "当前任务未授权运行命令");
    let command = args["command"].as_str().context("缺少 command")?;
    ensure!(command.len() <= 8192, "命令过长");
    let timeout_seconds = args["timeout_seconds"].as_u64().unwrap_or(30);
    ensure!(
        (1..=600).contains(&timeout_seconds),
        "命令超时时间必须在 1–600 秒之间"
    );
    let mut process = Command::new("pwsh.exe");
    process
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .current_dir(&task.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, _) in std::env::vars() {
        if name.contains("KEY") || name.contains("TOKEN") || name.contains("SECRET") {
            process.env_remove(name);
        }
    }
    #[cfg(windows)]
    process.creation_flags(0x08000000);
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let mut child = process.spawn().context("无法启动 PowerShell")?;
    // These are guaranteed by Stdio::piped, but still take the cleanup path if
    // an unexpected missing pipe is ever encountered after the side effect.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut out = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout
            .context("missing stdout")?
            .take(65536)
            .read_to_end(&mut bytes)
            .await?;
        Ok::<_, anyhow::Error>(bytes)
    });
    let mut err = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr
            .context("missing stderr")?
            .take(65536)
            .read_to_end(&mut bytes)
            .await?;
        Ok::<_, anyhow::Error>(bytes)
    });
    let mut out_joined = false;
    let mut err_joined = false;
    let completion = timeout_at(deadline, async {
        let mut status = None;
        let mut output = None;
        let mut errors = None;
        while status.is_none() || output.is_none() || errors.is_none() {
            tokio::select! {
                value = child.wait(), if status.is_none() => { status = Some(value?); }
                value = &mut out, if !out_joined => {
                    out_joined = true;
                    output = Some(value??);
                }
                value = &mut err, if !err_joined => {
                    err_joined = true;
                    errors = Some(value??);
                }
            }
        }
        Ok::<_, anyhow::Error>(json!({
            "exit_code": status.context("missing exit status")?.code(),
            "stdout": String::from_utf8_lossy(&output.context("missing stdout result")?),
            "stderr": String::from_utf8_lossy(&errors.context("missing stderr result")?)
        }))
    })
    .await;
    if let Ok(Ok(value)) = completion {
        return Ok(value);
    }
    // All failure branches converge here. There is one additional total budget,
    // including process termination, reaping, and both reader task joins.
    let cleanup_deadline = Instant::now() + Duration::from_secs(5);
    if !out_joined {
        out.abort();
    }
    if !err_joined {
        err.abort();
    }
    let cleanup = timeout_at(cleanup_deadline, async {
        let process_result: Result<()> = async {
            if child.try_wait()?.is_none() {
                child.start_kill()?;
                child.wait().await?;
            }
            Ok(())
        }
        .await;
        // Reap reader tasks even if process cleanup fails.
        if !out_joined {
            let _ = (&mut out).await;
        }
        if !err_joined {
            let _ = (&mut err).await;
        }
        process_result
    })
    .await;
    if !matches!(cleanup, Ok(Ok(()))) || completion.is_ok() {
        return Err(CommandOutcomeUnknown.into());
    }
    bail!("命令运行超过 {timeout_seconds} 秒；已结束直接子进程等待与输出收集")
}
