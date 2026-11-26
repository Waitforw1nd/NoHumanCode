use super::Failure;
use anyhow::{Context, Result};
use clap::Subcommand;
use coda::computer::ComputerService;
use serde_json::{Value, json};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[derive(Subcommand)]
pub enum ComputerCommand {
    /// Report the foreground state, then each change until the idle timeout or Ctrl+C.
    Watch {
        /// Maximum idle time between changes in milliseconds.
        #[arg(long, default_value_t = 30_000, value_parser = clap::value_parser!(u32).range(1..=60_000))]
        timeout_ms: u32,
    },
}

pub(super) async fn run(
    command: ComputerCommand,
    workspace: Option<PathBuf>,
    json_output: bool,
) -> Result<(), Failure> {
    let ComputerCommand::Watch { timeout_ms } = command;
    let workspace = workspace.as_deref().unwrap_or_else(|| Path::new("."));
    let cancel = CancellationToken::new();
    let work = watch(workspace, timeout_ms, json_output, &cancel);
    tokio::pin!(work);
    let result = tokio::select! {
        result = &mut work => result,
        signal = tokio::signal::ctrl_c() => {
            cancel.cancel();
            let _ = work.await;
            signal.context("无法监听 Ctrl+C")
        }
    };
    result.map_err(|error| Failure::custom("computer_watch_error", format!("{error:#}")))
}

async fn watch(
    workspace: &Path,
    timeout_ms: u32,
    json_output: bool,
    cancel: &CancellationToken,
) -> Result<()> {
    let workspace = workspace.canonicalize().context("无法解析工作区目录")?;
    let computer = ComputerService::from_env()?;
    let session = uuid::Uuid::new_v4().to_string();
    let mut observation = computer
        .execute(&session, &workspace, "computer_observe", &json!({}), cancel)
        .await?;
    loop {
        write_observation(&observation, json_output)?;
        if observation["outcome"] == "timeout" || observation["state"] == "target_exited" {
            return Ok(());
        }
        let reference = observation["observation_ref"]
            .as_str()
            .context("缺少桌面观察引用")?;
        observation = computer
            .execute(
                &session,
                &workspace,
                "computer_wait",
                &json!({
                    "condition": "changed", "observation_ref": reference, "timeout_ms": timeout_ms,
                }),
                cancel,
            )
            .await?;
    }
}

fn write_observation(observation: &Value, json_output: bool) -> Result<()> {
    let mut out = io::stdout().lock();
    if json_output {
        writeln!(out, "{observation}")?;
    } else {
        let state = observation["state"].as_str().context("缺少桌面状态")?;
        write!(out, "{state}")?;
        if let Some(title) = observation["title"].as_str() {
            // Keep terminal control characters escaped when displaying window titles.
            write!(out, "  {}", json!(title))?;
        }
        if observation["outcome"] == "timeout" {
            write!(out, "  (timeout)")?;
        }
        writeln!(out)?;
    }
    out.flush()?;
    Ok(())
}
