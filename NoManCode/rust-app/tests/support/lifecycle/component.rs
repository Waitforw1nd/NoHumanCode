//! Independent component invariants, not evidence of Store/Engine fencing.
use super::{
    fixture::{Case, until},
    process_lifetime::*,
};
use anyhow::{Context, Result, ensure};
use std::{
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct Domain {
    sealed: AtomicBool,
    seals: AtomicUsize,
    settlements: AtomicUsize,
}
struct Lease(Arc<Domain>);
impl DomainLease for Lease {
    fn seal_unknown(&mut self) {
        self.0.sealed.store(true, Ordering::Release);
        self.0.seals.fetch_add(1, Ordering::AcqRel);
    }
    fn settle(self: Box<Self>) {
        self.0.settlements.fetch_add(1, Ordering::AcqRel);
    }
}

struct Hooks(Arc<TestHooks>);
impl Drop for Hooks {
    fn drop(&mut self) {
        self.0.release_created.store(true, Ordering::Release);
        self.0.release_stdout_reader.store(true, Ordering::Release);
        self.0.release_watchdog.store(true, Ordering::Release);
        self.0.release_reader_join.store(true, Ordering::Release);
        self.0.fail_job_query.store(false, Ordering::Release);
        self.0.fail_termination.store(false, Ordering::Release);
    }
}
struct Cleanup {
    supervisor: Arc<Supervisor>,
    hooks: Arc<TestHooks>,
    root: PathBuf,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        drop(Hooks(self.hooks.clone()));
        let deadline = Instant::now() + Duration::from_secs(6);
        while self.supervisor.active_count() != 0 {
            if Instant::now() >= deadline {
                super::fixture::retain_unknown(
                    &self.root,
                    "component holder did not terminate after fixture release",
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn supervised(case: &Case, hooks: &Hooks) -> (Arc<Supervisor>, Cleanup) {
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let cleanup = Cleanup {
        supervisor: supervisor.clone(),
        hooks: hooks.0.clone(),
        root: case.root.clone(),
    };
    (supervisor, cleanup)
}
fn executable() -> Result<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").context("PATH missing")?)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("pwsh.exe"))
        .find(|p| p.is_file())
        .context("real pwsh.exe required")
}
fn spec(case: &Case, command: &str, hooks: &Hooks) -> Result<ProcessSpec> {
    let mut environment: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            !key.to_string_lossy().eq_ignore_ascii_case("TEMP")
                && !key.to_string_lossy().eq_ignore_ascii_case("TMP")
        })
        .collect();
    environment.push(("TEMP".into(), case.root.as_os_str().to_owned()));
    environment.push(("TMP".into(), case.root.as_os_str().to_owned()));
    let mut spec = ProcessSpec::new(
        executable()?,
        case.root.clone(),
        environment,
        (64, 64),
        OverflowPolicy::TruncateAndDrain,
        Instant::now() + Duration::from_secs(30),
        Duration::from_secs(5),
    );
    spec.args = vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        command.into(),
    ];
    spec.test_hooks = Some(hooks.0.clone());
    Ok(spec)
}
fn submit(supervisor: &Supervisor, spec: ProcessSpec, domain: &Arc<Domain>) -> Result<Operation> {
    ensure!(
        !domain.sealed.load(Ordering::Acquire),
        "domain admission sealed"
    );
    supervisor
        .submit(spec, Box::new(Lease(domain.clone())))
        .map_err(|failure| {
            failure.lease.settle();
            anyhow::anyhow!("process submission rejected: {}", failure.reason)
        })
}

#[tokio::test]
async fn component_eof_keeps_lease_until_original_descendant_exits() -> Result<()> {
    let mut case = Case::new("component-eof-")?;
    let hooks = Hooks(Arc::new(TestHooks::default()));
    let domain = Arc::new(Domain::default());
    let (supervisor, _cleanup) = supervised(&case, &hooks);
    let operation = submit(
        &supervisor,
        spec(&case, &case.command(false, false)?, &hooks)?,
        &domain,
    )?;
    case.observe().await?;
    until(|| {
        Ok(case.parent_exited()?
            && hooks.0.stdout_eof_joined.load(Ordering::Acquire)
            && hooks.0.stderr_eof_joined.load(Ordering::Acquire))
    })
    .await?;
    ensure!(
        !case.descendant_exited()?,
        "original descendant not alive at real EOF"
    );
    ensure!(
        domain.settlements.load(Ordering::Acquire) == 0,
        "real EOF released tree lease"
    );
    ensure!(
        supervisor.active_count() == 1,
        "live descendant lost capacity"
    );
    case.release()?;
    case.prove_exited().await?;
    ensure!(
        matches!(operation.wait().await, Outcome::Settled(_)),
        "controlled completion not settled"
    );
    until(|| Ok(supervisor.active_count() == 0)).await?;
    ensure!(
        domain.settlements.load(Ordering::Acquire) == 1 && !domain.sealed.load(Ordering::Acquire),
        "normal tree exit misclassified"
    );
    Ok(())
}

#[tokio::test]
async fn component_unknown_seals_before_return_and_keeps_reader_and_capacity() -> Result<()> {
    let case = Case::new("component-unknown-")?;
    let hooks = Hooks(Arc::new(TestHooks::default()));
    hooks.0.hold_stdout_reader.store(true, Ordering::Release);
    hooks.0.fail_job_query.store(true, Ordering::Release);
    let domain = Arc::new(Domain::default());
    let (supervisor, _cleanup) = supervised(&case, &hooks);
    let operation = submit(&supervisor, spec(&case, "exit 0", &hooks)?, &domain)?;
    until(|| Ok(hooks.0.stdout_reader_held.load(Ordering::Acquire))).await?;
    let outcome = tokio::time::timeout(Duration::from_secs(8), operation.wait()).await?;
    ensure!(
        matches!(outcome, Outcome::Unknown),
        "query failure must be unknown"
    );
    ensure!(
        domain.sealed.load(Ordering::Acquire) && domain.seals.load(Ordering::Acquire) == 1,
        "unknown published before sealing exactly once"
    );
    ensure!(
        domain.settlements.load(Ordering::Acquire) == 0 && supervisor.active_count() == 1,
        "unknown prematurely released pending reader"
    );
    let independent = Arc::new(Domain::default());
    ensure!(
        submit(&supervisor, spec(&case, "exit 0", &hooks)?, &independent).is_err(),
        "unknown occupied slot was reused"
    );
    ensure!(
        submit(&supervisor, spec(&case, "exit 0", &hooks)?, &domain).is_err(),
        "sealed domain readmitted work"
    );
    hooks.0.fail_job_query.store(false, Ordering::Release);
    hooks.0.release_stdout_reader.store(true, Ordering::Release);
    until(|| Ok(domain.settlements.load(Ordering::Acquire) == 1 && supervisor.active_count() == 0))
        .await?;
    ensure!(
        domain.sealed.load(Ordering::Acquire),
        "late cleanup reopened business admission"
    );
    std::fs::write(
        case.root.join("unknown.json"),
        "{\"business\":\"unknown\",\"late_resources_settled\":true,\"admission_reopened\":false}",
    )?;
    Ok(())
}

#[tokio::test]
async fn component_failed_reader_join_releases_resources_but_preserves_unknown() -> Result<()> {
    let case = Case::new("reader-failed-")?;
    let hooks = Hooks(Arc::new(TestHooks::default()));
    hooks.0.fail_stdout_reader.store(true, Ordering::Release);
    let domain = Arc::new(Domain::default());
    let (supervisor, _cleanup) = supervised(&case, &hooks);
    let operation = submit(&supervisor, spec(&case, "exit 0", &hooks)?, &domain)?;
    ensure!(
        matches!(operation.wait().await, Outcome::Unknown),
        "reader failure fabricated successful capture"
    );
    until(|| {
        Ok(hooks.0.stdout_failed_joined.load(Ordering::Acquire)
            && hooks.0.root_signaled.load(Ordering::Acquire)
            && hooks.0.job_empty.load(Ordering::Acquire)
            && domain.settlements.load(Ordering::Acquire) == 1
            && supervisor.active_count() == 0)
    })
    .await?;
    ensure!(
        domain.sealed.load(Ordering::Acquire),
        "failed reader's late join erased unknown"
    );
    Ok(())
}

#[tokio::test]
async fn component_cancel_token_requests_real_tree_cleanup_without_early_drop() -> Result<()> {
    let mut case = Case::new("component-cancel-")?;
    let hooks = Hooks(Arc::new(TestHooks::default()));
    let domain = Arc::new(Domain::default());
    let (supervisor, _cleanup) = supervised(&case, &hooks);
    let operation = submit(
        &supervisor,
        spec(&case, &case.command(true, false)?, &hooks)?,
        &domain,
    )?;
    case.observe().await?;
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    let outcome = tokio::time::timeout(
        Duration::from_secs(8),
        operation.wait_with_cancel(&cancellation),
    )
    .await?;
    ensure!(
        matches!(
            outcome,
            Outcome::Settled(Settled {
                reason: CompletionReason::Cancelled,
                ..
            })
        ),
        "proved cancellation not a settled cancellation"
    );
    ensure!(
        case.parent_exited()? && case.descendant_exited()?,
        "cancellation returned before original tree exited"
    );
    case.prove_exited().await?;
    until(|| Ok(supervisor.active_count() == 0)).await?;
    ensure!(
        !domain.sealed.load(Ordering::Acquire) && domain.settlements.load(Ordering::Acquire) == 1,
        "normal cancellation sealed domain"
    );
    Ok(())
}

#[tokio::test]
async fn component_final_read_overflow_cannot_be_adopted_as_success() -> Result<()> {
    let case = Case::new("final-read-overflow-")?;
    let hooks = Hooks(Arc::new(TestHooks::default()));
    hooks.0.hold_stdout_reader.store(true, Ordering::Release);
    hooks
        .0
        .hold_before_reader_join
        .store(true, Ordering::Release);
    let domain = Arc::new(Domain::default());
    let (supervisor, _cleanup) = supervised(&case, &hooks);
    let mut spec = spec(&case, "[Console]::Out.Write(('x' * 128)); exit 0", &hooks)?;
    spec.overflow = OverflowPolicy::Terminate;
    let operation = submit(&supervisor, spec, &domain)?;
    until(|| {
        Ok(hooks.0.stdout_reader_held.load(Ordering::Acquire)
            && hooks.0.before_reader_join_held.load(Ordering::Acquire))
    })
    .await?;
    ensure!(
        domain.settlements.load(Ordering::Acquire) == 0,
        "parent signal bypassed unjoined reader"
    );
    hooks.0.release_stdout_reader.store(true, Ordering::Release);
    until(|| Ok(hooks.0.stdout_reader_finished.load(Ordering::Acquire))).await?;
    hooks.0.release_reader_join.store(true, Ordering::Release);
    let outcome = operation.wait().await;
    let Outcome::Settled(settled) = outcome else {
        anyhow::bail!("known overflow was not settled: {outcome:?}")
    };
    ensure!(
        settled.reason == CompletionReason::OutputLimit,
        "final read overflow adopted as normal exit"
    );
    ensure!(
        settled.stdout.truncated && settled.stdout.bytes.len() == 64,
        "retention limit changed"
    );
    until(|| Ok(supervisor.active_count() == 0)).await?;
    ensure!(
        domain.settlements.load(Ordering::Acquire) == 1 && !domain.sealed.load(Ordering::Acquire),
        "proved overflow misclassified unknown"
    );
    Ok(())
}
