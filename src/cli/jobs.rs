//! Local durable exploration commands for a single workspace and chat session.

use super::{Failure, open_local_engine, resolve_user_data_dir};
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use coda::{
    domain::{SessionId, SessionKind},
    engine::Engine,
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Args)]
pub struct JobsArgs {
    /// Exact chat session ID (shown by coda --json ask or chat /sessions).
    #[arg(long)]
    pub session: String,
    /// Registered workspace root, defaults to the current directory.
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Local Coda data directory used for the session.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: JobsCommand,
}

#[derive(Subcommand)]
enum JobsCommand {
    /// List jobs in this session with status, evidence and attempt receipts.
    List,
    /// Inspect a job or batch by ID.
    Show { id: String },
    /// Save a batch and return stable IDs before any model requests.
    Submit {
        /// Stable caller key for repeated submission of this batch in the session.
        #[arg(long)]
        key: String,
        /// JSON document with a branches array, as for explore_parallel.
        #[arg(long)]
        file: PathBuf,
    },
    /// Run eligible work for a job or batch.
    Continue { id: String },
    /// Explicitly start a new attempt for one retryable job.
    Retry { id: String },
}

fn failure(error: anyhow::Error) -> Failure {
    Failure::custom("exploration_job_error", format!("{error:#}"))
}

async fn execute(
    engine: &Engine,
    session_id: &SessionId,
    command: JobsCommand,
    json_output: bool,
) -> Result<()> {
    let session = engine.store.session(session_id)?;
    ensure!(session.kind == SessionKind::Chat, "请选择聊天会话");
    let context = engine.store.run_context(&session.legacy_run_id)?;
    let [parent] = context.tasks.as_slice() else {
        bail!("当前聊天回合没有唯一的主任务")
    };
    let parent = &parent.legacy_task_id;
    let cancel = CancellationToken::new();
    let result = match command {
        JobsCommand::List => json!(engine.exploration_jobs(parent)?),
        JobsCommand::Submit { key, file } => {
            coda::secrets::validate_idempotency_key(&key)?;
            let bytes = std::fs::read(&file)
                .with_context(|| format!("无法读取批次文件 {}", file.display()))?;
            // Windows PowerShell 5 writes a UTF-8 BOM with Set-Content -Encoding UTF8.
            let utf8 = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
            let input: Value = serde_json::from_slice(utf8).context("批次文件 JSON 无效")?;
            json!(engine.submit_exploration(parent, &key, &input)?)
        }
        JobsCommand::Retry { id } => {
            json!(
                engine
                    .continue_exploration(parent, &id, true, &cancel)
                    .await?
            )
        }
        JobsCommand::Show { id } if id.starts_with("batch-") => {
            json!(engine.exploration_batch(parent, &id)?)
        }
        JobsCommand::Show { id } => json!(engine.exploration_job(parent, &id)?),
        JobsCommand::Continue { id } if id.starts_with("batch-") => {
            json!(
                engine
                    .continue_exploration_batch(parent, &id, &cancel)
                    .await?
            )
        }
        JobsCommand::Continue { id } => {
            json!(
                engine
                    .continue_exploration(parent, &id, false, &cancel)
                    .await?
            )
        }
    };
    if json_output {
        println!("{result}");
    } else {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}

pub(super) async fn run(
    instance_lock: &mut Option<std::fs::File>,
    top_data_dir: Option<PathBuf>,
    top_workspace: Option<PathBuf>,
    json_output: bool,
    args: JobsArgs,
) -> Result<(), Failure> {
    let data_dir = resolve_user_data_dir(args.data_dir.or(top_data_dir));
    let engine = open_local_engine(instance_lock, &data_dir)?;
    let outcome = async {
        let session_id = SessionId(args.session);
        let session = engine.store.session(&session_id)?;
        let workspace = args
            .dir
            .or(top_workspace)
            .unwrap_or_else(|| PathBuf::from("."));
        ensure!(
            workspace.canonicalize()? == engine.project_workspace(&session.project_id)?,
            "会话不属于指定的工作区"
        );
        execute(&engine, &session_id, args.command, json_output).await
    }
    .await;
    let _ = engine.shutdown(Duration::from_secs(10)).await;
    outcome.map_err(failure)
}

pub(super) async fn slash(
    engine: &Engine,
    session: &SessionId,
    args: &str,
    json_output: bool,
) -> Result<(), Failure> {
    let (verb, arg) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let arg = arg.trim();
    let command = match (verb, arg) {
        ("" | "list", "") => JobsCommand::List,
        ("show", id) if !id.is_empty() => JobsCommand::Show { id: id.into() },
        ("continue", id) if !id.is_empty() => JobsCommand::Continue { id: id.into() },
        ("retry", id) if !id.is_empty() => JobsCommand::Retry { id: id.into() },
        ("submit", arg) if let Some((key, file)) = arg.split_once(char::is_whitespace) => {
            JobsCommand::Submit {
                key: key.into(),
                file: file.trim().into(),
            }
        }
        _ => {
            return Err(Failure::custom(
                "invalid_command",
                "用法：/jobs [list | show ID | submit KEY FILE | continue ID | retry JOB_ID]",
            ));
        }
    };
    execute(engine, session, command, json_output)
        .await
        .map_err(failure)
}
