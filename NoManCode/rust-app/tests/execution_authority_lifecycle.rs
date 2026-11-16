#![cfg(windows)]
//! Manager-only serial real-process matrix; no live provider is used.
#[path = "support/lifecycle/fixture.rs"]
mod fixture;
#[path = "support/lifecycle/host.rs"]
mod host;
// Compile the actual component source with its test hooks in this independent
// integration target. Public Host cases above/below still use the library.
#[path = "support/lifecycle/component.rs"]
mod component;
#[allow(dead_code)]
#[path = "../src/process_lifetime.rs"]
mod process_lifetime;

use anyhow::{Context, Result, ensure};
use fixture::{Case, until};
use host::Host;
use peachsh::{
    approval::ExecutionState,
    engine::Engine,
    git_diff::{GitDiffRequest, GitDiffStatus, GitDiffView},
    store::Store,
};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn real_git_alias_descendant_keeps_shell_approval_claimed() -> Result<()> {
    let mut case = Case::new("git-descendant-")?;
    let host = Host::new(&case).await?;
    let (task, approval) = host.shell(&case.git_command()?, 30).await?;
    case.observe().await?;
    until(|| case.parent_exited()).await?;
    ensure!(
        !case.descendant_exited()?,
        "Git-launched descendant exited before release"
    );
    for _ in 0..20 {
        ensure!(
            host.engine.store.approval(&approval.id)?.execution_state == ExecutionState::Claimed,
            "Git/alias exit prematurely completed the live process tree"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    case.release()?;
    case.prove_exited().await?;
    host.finished(&task).await?;
    host.success().await?;
    host.close().await
}

#[tokio::test]
async fn shell_eof_and_parent_exit_do_not_release_live_descendant() -> Result<()> {
    let mut case = Case::new("shell-eof-")?;
    let host = Host::new(&case).await?;
    let (task, approval) = host.shell(&case.command(false, true)?, 30).await?;
    case.observe().await?;
    until(|| case.parent_exited()).await?;
    ensure!(
        case.root.join("flood-finished").exists(),
        "both streams must drain beyond retention limit"
    );
    ensure!(
        !case.descendant_exited()?,
        "descendant must remain alive with separate stdio"
    );
    let (open, in_flight, unknown) = host.engine.store.execution_admission_status()?;
    ensure!(
        open && in_flight > 0 && unknown == 0,
        "live Shell must retain a real domain operation"
    );
    // Observe a stable interval after the original parent has signaled. The
    // descendant cannot exit normally until this test releases its barrier.
    for _ in 0..20 {
        ensure!(
            host.engine.store.approval(&approval.id)?.execution_state == ExecutionState::Claimed,
            "parent exit and EOF prematurely completed the real approval"
        );
        ensure!(
            host.engine.is_busy(),
            "Host reported idle with a live descendant"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let contender = Engine::new(host.engine.store.clone(), 1);
    ensure!(
        contender.is_err(),
        "second controller accepted during live work"
    );
    case.release()?;
    case.prove_exited().await?;
    host.finished(&task).await?;
    until(|| Ok(host.engine.store.execution_admission_status()?.1 == 0)).await?;
    ensure!(
        host.engine.store.approval(&approval.id)?.execution_state == ExecutionState::Finished,
        "proved completion was not adopted"
    );
    host.success().await?;
    host.close().await
}

#[tokio::test]
async fn shell_cancel_reaps_original_tree_then_allows_next_command() -> Result<()> {
    let mut case = Case::new("shell-cancel-")?;
    let host = Host::new(&case).await?;
    let (task, approval) = host.shell(&case.command(true, false)?, 30).await?;
    case.observe().await?;
    host.engine.cancel(&task.id)?;
    // No fixture release/kill precedes this proof: collector cancellation must
    // reach the real Job, and must not wait for the 30-second command deadline.
    tokio::time::timeout(Duration::from_secs(8), case.prove_exited()).await??;
    let task = host.finished(&task).await?;
    ensure!(
        task.status == "cancelled",
        "task cancellation not persisted: {}",
        task.status
    );
    ensure!(
        host.engine.store.approval(&approval.id)?.execution_state != ExecutionState::Unknown,
        "proved cancellation incorrectly sealed the domain"
    );
    host.success().await?;
    host.close().await
}

#[tokio::test]
async fn shell_deadline_reaps_descendant_without_false_unknown() -> Result<()> {
    let mut case = Case::new("shell-deadline-")?;
    let host = Host::new(&case).await?;
    // Startup must reach the handle barrier before the deadline; a failed
    // fixture preparation is a failure, never evidence of correct timeout.
    let (task, approval) = host.shell(&case.command(true, false)?, 12).await?;
    case.observe().await?;
    tokio::time::timeout(Duration::from_secs(18), case.prove_exited()).await??;
    host.finished(&task).await?;
    ensure!(
        host.engine.store.approval(&approval.id)?.execution_state != ExecutionState::Unknown,
        "proved timeout incorrectly sealed the domain"
    );
    host.success().await?;
    host.close().await
}

#[tokio::test]
async fn host_shutdown_cancels_real_shell_before_reporting_settled() -> Result<()> {
    let mut case = Case::new("host-shutdown-")?;
    let host = Host::new(&case).await?;
    host.shell(&case.command(true, false)?, 30).await?;
    case.observe().await?;
    let report = host.engine.shutdown(Duration::from_secs(8)).await;
    ensure!(
        report.settled(),
        "bounded shutdown must prove real cleanup: {report:?}"
    );
    ensure!(
        case.parent_exited()? && case.descendant_exited()?,
        "settled Host still owns live processes"
    );
    case.prove_exited().await?;
    ensure!(
        host.engine.start(Host::request()).await.is_err(),
        "closed Host accepted a new task"
    );
    Ok(())
}

#[tokio::test]
async fn ordinary_shell_and_git_failures_release_and_path_commands_work() -> Result<()> {
    let case = Case::new("ordinary-failure-with space-")?;
    let host = Host::new(&case).await?;
    let (task, approval) = host.shell("exit 23", 10).await?;
    let done = host.finished(&task).await?;
    ensure!(
        host.engine.store.approval(&approval.id)?.execution_state == ExecutionState::Finished,
        "nonzero exit is a known result"
    );
    ensure!(done.messages.iter().any(|m| m["role"] == "tool" && m["content"].as_str().is_some_and(|s| s.contains("23"))), "actual nonzero exit missing");
    let request = || GitDiffRequest {
        path: "file.txt".into(),
        view: GitDiffView::Head,
    };
    ensure!(
        host.engine.git_diff(&task.id, request()).await.is_err(),
        "non-repository Git unexpectedly succeeded"
    );
    // Both names are resolved by production callers through PATH. An absolute
    // process probe alone cannot catch CreateProcessW's search-path regression.
    let command = "$ErrorActionPreference='Stop'; git init --quiet; if($LASTEXITCODE -ne 0){exit $LASTEXITCODE}; [IO.File]::WriteAllText((Join-Path $PWD 'file.txt'), 'before'); git add -- file.txt; if($LASTEXITCODE -ne 0){exit $LASTEXITCODE}; git -c user.name=Lifecycle -c user.email=lifecycle@example.invalid -c core.hooksPath= commit --quiet -m fixture; if($LASTEXITCODE -ne 0){exit $LASTEXITCODE}; [IO.File]::WriteAllText((Join-Path $PWD 'file.txt'), 'after'); exit 0";
    let (git_task, approval) = host.shell(command, 20).await?;
    let setup = host.finished(&git_task).await?;
    ensure!(
        host.engine.store.approval(&approval.id)?.execution_state == ExecutionState::Finished,
        "Git setup not settled"
    );
    let outputs: Vec<_> = setup
        .messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect();
    ensure!(
        outputs.len() == 1,
        "Git setup must have exactly one tool result: task_status={}, outputs={outputs:?}",
        setup.status,
    );
    let output = outputs[0]["content"]
        .as_str()
        .with_context(|| format!("Git setup tool content is not text: {outputs:?}"))?;
    let output: serde_json::Value = serde_json::from_str(output)
        .with_context(|| format!("Git setup tool output is not JSON: {output}"))?;
    ensure!(
        output["exit_code"].as_i64() == Some(0),
        "Git setup failed: task_status={}, tool_output={output}",
        setup.status,
    );
    let diff = host.engine.git_diff(&git_task.id, request()).await?;
    ensure!(
        diff.status == GitDiffStatus::Text,
        "real Git diff not text: status={:?}, patch={:?}",
        diff.status,
        diff.patch,
    );
    let patch = diff.patch.unwrap_or_default();
    ensure!(
        patch.contains("-before") && patch.contains("+after"),
        "Git PATH positive control missing actual diff: status={:?}, patch={patch:?}",
        diff.status,
    );
    host.success().await?;
    host.close().await
}

#[tokio::test]
async fn actual_domain_alias_cannot_create_second_controller_and_reopens_after_close() -> Result<()>
{
    let case = Case::new("domain-alias-")?;
    let host = Host::new(&case).await?;
    let before = host.engine.store.runs()?;
    let shared = Arc::new(host.engine.store.open_shared()?);
    ensure!(
        shared.settings()?.is_some(),
        "legitimate same-domain connection unavailable"
    );
    ensure!(
        Engine::new(shared.clone(), 1).is_err(),
        "shared Store became second controller"
    );
    ensure!(
        Engine::new_recovered(shared.clone(), 1).is_err(),
        "recovery bypassed active controller"
    );
    ensure!(
        host.engine.store.runs()? == before,
        "rejected recovery changed tasks"
    );
    let alias = case.root.join(".").join("LIFECYCLE.SQLITE3");
    match Store::open(&alias) {
        Ok(other) => ensure!(
            Engine::new(Arc::new(other), 1).is_err(),
            "case/path alias acquired the same actual domain"
        ),
        Err(error) => ensure!(
            format!("{error:#}").contains("domain"),
            "alias failed for an unrelated reason: {error:#}"
        ),
    }
    host.success().await?;
    host.close().await?;
    let database = host.database.clone();
    drop(shared);
    drop(host);
    let next = Engine::new_recovered(Arc::new(Store::open(&database)?), 1)?.0;
    let report = next.shutdown(Duration::from_secs(5)).await;
    ensure!(
        report.settled(),
        "next controller did not cleanly close: {report:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_controllers_on_two_store_connections_have_one_winner() -> Result<()> {
    let case = Case::new("domain-concurrent-")?;
    let host = Host::new(&case).await?;
    let one = Arc::new(host.engine.store.open_shared()?);
    let two = Arc::new(host.engine.store.open_shared()?);
    host.close().await?;
    drop(host);
    let start = Arc::new(tokio::sync::Barrier::new(3));
    let handles: Vec<_> = [one, two]
        .into_iter()
        .map(|store| {
            let start = start.clone();
            tokio::spawn(async move {
                start.wait().await;
                // The JoinHandle retains the winner until the caller collects it.
                Engine::new(store, 1)
            })
        })
        .collect();
    tokio::time::timeout(Duration::from_secs(5), start.wait()).await?;
    let mut winner = None;
    let mut rejected = 0;
    for handle in handles {
        match tokio::time::timeout(Duration::from_secs(10), handle).await?? {
            Ok(engine) => {
                ensure!(
                    winner.is_none(),
                    "two Store connections acquired simultaneous controllers"
                );
                winner = Some(engine);
            }
            Err(error) => {
                ensure!(
                    format!("{error:#}").contains("controller"),
                    "contender failed for an unrelated reason: {error:#}"
                );
                rejected += 1;
            }
        }
    }
    ensure!(
        rejected == 1,
        "exactly one same-domain contender must be rejected"
    );
    let winner = winner.expect("one controller must succeed");
    ensure!(
        winner.shutdown(Duration::from_secs(5)).await.settled(),
        "winner shutdown failed"
    );
    Ok(())
}
