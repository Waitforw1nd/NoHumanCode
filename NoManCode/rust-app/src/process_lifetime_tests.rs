//! Isolated Windows process tests. Each requires a signed, serial test window.
use crate::process_lifetime::{
    CompletionReason, DomainLease, Operation, Outcome, OverflowPolicy, ProcessSpec, Supervisor,
    TestHooks,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    num::NonZeroUsize,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{GetCurrentProcess, GetProcessId, GetProcessTimes, WaitForSingleObject},
};

#[derive(Default)]
struct Domain {
    sealed: AtomicBool,
    seals: AtomicUsize,
    settled: AtomicUsize,
}
struct Lease(Arc<Domain>);
impl DomainLease for Lease {
    fn seal_unknown(&mut self) {
        self.0.sealed.store(true, Ordering::Release);
        self.0.seals.fetch_add(1, Ordering::SeqCst);
    }
    fn settle(self: Box<Self>) {
        self.0.settled.fetch_add(1, Ordering::SeqCst);
    }
}

fn fixture_exe() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("NHC_PROCESS_LIFETIME_PROBE").expect("fixed probe path required"),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "fixed probe must exist"
    );
    let expected =
        std::env::var("NHC_PROCESS_LIFETIME_PROBE_SHA256").expect("fixed probe SHA256 required");
    let actual = format!(
        "{:x}",
        Sha256::digest(std::fs::read(&path).expect("read probe"))
    );
    assert_eq!(
        actual,
        expected.to_ascii_lowercase(),
        "probe identity changed"
    );
    path
}
fn fixture_dir() -> tempfile::TempDir {
    let root = PathBuf::from(
        std::env::var_os("NHC_PROCESS_TEST_ROOT").expect("isolated E test root required"),
    );
    assert!(
        root.is_absolute()
            && root
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("e:\\")
    );
    tempfile::tempdir_in(root).expect("fresh fixture")
}
fn env_for(dir: &Path) -> Vec<(OsString, OsString)> {
    let mut env = Vec::new();
    for key in ["SystemRoot", "WINDIR", "PATH"] {
        if let Some(value) = std::env::var_os(key) {
            env.push((key.into(), value));
        }
    }
    env.push(("TEMP".into(), dir.as_os_str().to_owned()));
    env.push(("TMP".into(), dir.as_os_str().to_owned()));
    env
}
fn spec(dir: &Path, duration: Duration, hooks: Option<Arc<TestHooks>>) -> ProcessSpec {
    let mut spec = ProcessSpec::new(
        fixture_exe(),
        dir.to_owned(),
        env_for(dir),
        (65_536, 65_536),
        OverflowPolicy::TruncateAndDrain,
        Instant::now() + duration,
        Duration::from_secs(5),
    );
    spec.test_hooks = hooks;
    spec
}
fn submit(supervisor: &Supervisor, spec: ProcessSpec, domain: &Arc<Domain>) -> Operation {
    match supervisor.submit(spec, Box::new(Lease(Arc::clone(domain)))) {
        Ok(op) => op,
        Err(failure) => {
            let reason = failure.reason;
            failure.lease.settle();
            panic!("submission failed: {reason}");
        }
    }
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(12), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture condition did not occur");
}
fn filetime(handle: windows_sys::Win32::Foundation::HANDLE) -> u64 {
    let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
        unsafe { std::mem::zeroed() };
    assert_ne!(
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) },
        0
    );
    ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64
}
fn harness_identity() -> (u32, u64) {
    (std::process::id(), filetime(unsafe { GetCurrentProcess() }))
}

/// Owns only duplicate handles to the original child objects, never a Job.
/// If cleanup cannot be verified the test process stays alive with its proof.
struct FixtureGuard {
    dir: tempfile::TempDir,
    supervisor: Arc<Supervisor>,
    domain: Arc<Domain>,
    children: Vec<OwnedHandle>,
    abort: Option<tokio::task::AbortHandle>,
    hooks: Option<Arc<TestHooks>>,
    active: bool,
}
impl FixtureGuard {
    fn new(
        supervisor: Arc<Supervisor>,
        domain: Arc<Domain>,
        hooks: Option<Arc<TestHooks>>,
    ) -> Self {
        Self {
            dir: fixture_dir(),
            supervisor,
            domain,
            children: Vec::new(),
            abort: None,
            hooks,
            active: false,
        }
    }
    fn path(&self) -> &Path {
        self.dir.path()
    }
    fn track(&mut self, task: &tokio::task::JoinHandle<Outcome>) {
        self.abort = Some(task.abort_handle());
        self.active = true;
    }
    fn take_children(&mut self, ready: &Path, expected_count: usize) {
        let text = std::fs::read_to_string(ready).expect("child receipt");
        for line in text.lines() {
            let parts: Vec<_> = line.split_whitespace().collect();
            assert_eq!(parts.len(), 3);
            let raw: usize = parts[0].parse().unwrap();
            let pid: u32 = parts[1].parse().unwrap();
            let created: u64 = parts[2].parse().unwrap();
            let handle = unsafe { OwnedHandle::from_raw_handle(raw as _) };
            assert_eq!(unsafe { GetProcessId(handle.as_raw_handle()) }, pid);
            assert_eq!(
                filetime(handle.as_raw_handle()),
                created,
                "same original child object"
            );
            self.children.push(handle);
        }
        assert_eq!(self.children.len(), expected_count);
    }
}
impl Drop for FixtureGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(abort) = &self.abort {
            abort.abort();
        }
        if let Some(h) = &self.hooks {
            h.release_created.store(true, Ordering::Release);
            h.release_stdout_reader.store(true, Ordering::Release);
            h.release_watchdog.store(true, Ordering::Release);
            h.release_reader_join.store(true, Ordering::Release);
            h.fail_job_query.store(false, Ordering::Release);
            h.fail_termination.store(false, Ordering::Release);
        }
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let children_done = self
                .children
                .iter()
                .all(|h| unsafe { WaitForSingleObject(h.as_raw_handle(), 0) } == WAIT_OBJECT_0);
            if self.domain.settled.load(Ordering::Acquire) != 0
                && self.supervisor.active_count() == 0
                && children_done
            {
                return;
            }
            if Instant::now() >= deadline {
                eprintln!(
                    "NHC_TERMINAL_UNKNOWN: process-lifetime fixture retains original child handles and directory"
                );
                loop {
                    std::thread::park_timeout(Duration::from_secs(1));
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn parent_spec(
    dir: &Path,
    duration: Duration,
    children: usize,
    stay: bool,
    noisy: bool,
    hooks: Option<Arc<TestHooks>>,
) -> ProcessSpec {
    let mut s = spec(dir, duration, hooks);
    let (pid, created) = harness_identity();
    s.args = vec![
        "parent".into(),
        dir.join("ready").into_os_string(),
        dir.join("release").into_os_string(),
        dir.join("marker").into_os_string(),
        pid.to_string().into(),
        created.to_string().into(),
        children.to_string().into(),
        (stay as u8).to_string().into(),
        (noisy as u8).to_string().into(),
    ];
    s
}
async fn result(task: tokio::task::JoinHandle<Outcome>) -> Outcome {
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .expect("operation deadline")
        .expect("operation task")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn normal_nonzero_and_prestart_rejection() {
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(2).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(Arc::clone(&supervisor), Arc::clone(&domain), None);
    let mut s = spec(guard.path(), Duration::from_secs(10), None);
    s.args = vec!["exit".into(), "7".into()];
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    let Outcome::Settled(settled) = result(task).await else {
        panic!("expected settled");
    };
    assert_eq!(settled.reason, CompletionReason::Exited);
    assert_eq!(settled.exit_code, 7);
    assert_eq!(settled.stdout.bytes, b"stdout-marker");
    assert_eq!(settled.stderr.bytes, b"stderr-marker");
    assert!(!domain.sealed.load(Ordering::Acquire));
    until(|| supervisor.active_count() == 0).await;
    let mut identity_spec = spec(guard.path(), Duration::from_secs(10), None);
    identity_spec.args = vec!["identity".into()];
    let task = tokio::spawn(submit(&supervisor, identity_spec, &domain).wait());
    guard.track(&task);
    let Outcome::Settled(identity_result) = result(task).await else {
        panic!("identity probe must settle");
    };
    assert_eq!(identity_result.exit_code, 0);
    let receipt = std::str::from_utf8(&identity_result.stdout.bytes).unwrap();
    let mut parts = receipt.split_whitespace();
    assert_eq!(
        parts.next().unwrap().parse::<u32>().unwrap(),
        identity_result.identity.pid
    );
    assert_eq!(
        parts.next().unwrap().parse::<u64>().unwrap(),
        identity_result.identity.creation_filetime
    );
    assert!(parts.next().is_none());
    until(|| supervisor.active_count() == 0).await;
    let mut missing = spec(guard.path(), Duration::from_secs(10), None);
    missing.program = guard.path().join("missing.exe");
    let task = tokio::spawn(submit(&supervisor, missing, &domain).wait());
    guard.track(&task);
    let Outcome::NotStarted { stage, os_error } = result(task).await else {
        panic!("missing program must be proven unstarted");
    };
    assert_eq!(stage, "create_returned_no_process");
    assert_eq!(os_error, Some(2)); // ERROR_FILE_NOT_FOUND for a missing file in an existing directory.
    assert!(!domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn detached_multi_child_waits_for_real_tree_zero() {
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(2).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(Arc::clone(&supervisor), Arc::clone(&domain), None);
    let s = parent_spec(guard.path(), Duration::from_secs(10), 2, false, true, None);
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    let ready = guard.path().join("ready");
    until(|| ready.is_file()).await;
    guard.take_children(&ready, 2);
    assert!(
        guard
            .children
            .iter()
            .all(|h| unsafe { WaitForSingleObject(h.as_raw_handle(), 0) } == WAIT_TIMEOUT)
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !task.is_finished(),
        "direct parent/stdio completion is not tree completion"
    );
    std::fs::write(guard.path().join("release"), b"go").unwrap();
    let Outcome::Settled(value) = result(task).await else {
        panic!("expected settled");
    };
    assert_eq!(value.reason, CompletionReason::Exited);
    assert!(value.stdout.truncated);
    assert!(
        guard
            .children
            .iter()
            .all(|h| unsafe { WaitForSingleObject(h.as_raw_handle(), 0) } == WAIT_OBJECT_0)
    );
    assert!(!domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn timeout_and_cancel_reap_original_tree_without_sealing() {
    for cancel in [false, true] {
        let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
        let domain = Arc::new(Domain::default());
        let mut guard = FixtureGuard::new(Arc::clone(&supervisor), Arc::clone(&domain), None);
        let s = parent_spec(guard.path(), Duration::from_secs(5), 1, true, false, None);
        let op = submit(&supervisor, s, &domain);
        guard.active = true;
        let ready = guard.path().join("ready");
        until(|| ready.is_file()).await;
        guard.take_children(&ready, 1);
        if cancel {
            op.cancel();
        }
        let task = tokio::spawn(op.wait());
        guard.track(&task);
        let Outcome::Settled(value) = result(task).await else {
            panic!("confirmed cleanup expected");
        };
        assert_eq!(
            value.reason,
            if cancel {
                CompletionReason::Cancelled
            } else {
                CompletionReason::Deadline
            }
        );
        assert_eq!(
            unsafe { WaitForSingleObject(guard.children[0].as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
        assert!(!domain.sealed.load(Ordering::Acquire));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn dropped_caller_requests_cleanup_without_immediate_unknown() {
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(Arc::clone(&supervisor), Arc::clone(&domain), None);
    let s = parent_spec(guard.path(), Duration::from_secs(10), 1, true, false, None);
    let op = submit(&supervisor, s, &domain);
    guard.active = true;
    let ready = guard.path().join("ready");
    until(|| ready.is_file()).await;
    guard.take_children(&ready, 1);
    drop(op);
    until(|| domain.settled.load(Ordering::Acquire) == 1 && supervisor.active_count() == 0).await;
    assert_eq!(
        unsafe { WaitForSingleObject(guard.children[0].as_raw_handle(), 0) },
        WAIT_OBJECT_0
    );
    assert!(!domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn token_cancel_waits_for_original_holder_and_reader_before_settlement() {
    let hooks = Arc::new(TestHooks::default());
    hooks.hold_stdout_reader.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let s = parent_spec(
        guard.path(),
        Duration::from_secs(10),
        1,
        true,
        false,
        Some(Arc::clone(&hooks)),
    );
    let op = submit(&supervisor, s, &domain);
    let cancellation = tokio_util::sync::CancellationToken::new();
    let waiter_token = cancellation.clone();
    let task = tokio::spawn(async move { op.wait_with_cancel(&waiter_token).await });
    guard.track(&task);
    let ready = guard.path().join("ready");
    until(|| ready.is_file() && hooks.stdout_reader_held.load(Ordering::Acquire)).await;
    guard.take_children(&ready, 1);
    cancellation.cancel();
    until(|| {
        hooks.root_signaled.load(Ordering::Acquire) && hooks.job_empty.load(Ordering::Acquire)
    })
    .await;
    assert!(
        !task.is_finished(),
        "cancellation must continue waiting for the same I/O"
    );
    assert_eq!(domain.settled.load(Ordering::Acquire), 0);
    assert_eq!(supervisor.active_count(), 1);
    assert!(!domain.sealed.load(Ordering::Acquire));
    hooks.release_stdout_reader.store(true, Ordering::Release);
    let Outcome::Settled(value) = result(task).await else {
        panic!("confirmed cancel must settle")
    };
    assert_eq!(value.reason, CompletionReason::Cancelled);
    until(|| supervisor.active_count() == 0).await;
    assert_eq!(domain.settled.load(Ordering::Acquire), 1);
    assert!(!domain.sealed.load(Ordering::Acquire));
    assert_eq!(
        unsafe { WaitForSingleObject(guard.children[0].as_raw_handle(), 0) },
        WAIT_OBJECT_0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn final_read_overflow_cannot_be_adopted_as_ordinary_exit() {
    let hooks = Arc::new(TestHooks::default());
    hooks.hold_stdout_reader.store(true, Ordering::Release);
    hooks.hold_before_reader_join.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let mut s = spec(
        guard.path(),
        Duration::from_secs(10),
        Some(Arc::clone(&hooks)),
    );
    s.args = vec!["exit".into(), "0".into()];
    s.stdout_limit = 1;
    s.overflow = OverflowPolicy::Terminate;
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    until(|| {
        hooks.stdout_reader_held.load(Ordering::Acquire)
            && hooks.before_reader_join_held.load(Ordering::Acquire)
    })
    .await;
    // The holder already made this iteration's stop decision while no bytes
    // had been read. Let the final read cross the limit before that same join.
    hooks.release_stdout_reader.store(true, Ordering::Release);
    until(|| hooks.stdout_reader_finished.load(Ordering::Acquire)).await;
    hooks.release_reader_join.store(true, Ordering::Release);
    let Outcome::Settled(value) = result(task).await else {
        panic!("completed I/O must settle")
    };
    assert_eq!(value.reason, CompletionReason::OutputLimit);
    assert!(value.stdout.truncated);
    assert_eq!(value.stdout.bytes, b"s");
    assert!(!domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn token_cancel_unknown_keeps_reader_and_stays_sealed_after_late_cleanup() {
    let hooks = Arc::new(TestHooks::default());
    hooks.hold_stdout_reader.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let mut s = spec(
        guard.path(),
        Duration::from_secs(10),
        Some(Arc::clone(&hooks)),
    );
    s.args = vec!["exit".into(), "0".into()];
    s.cleanup_budget = Duration::from_millis(500);
    let op = submit(&supervisor, s, &domain);
    let cancellation = tokio_util::sync::CancellationToken::new();
    let waiter_token = cancellation.clone();
    let task = tokio::spawn(async move { op.wait_with_cancel(&waiter_token).await });
    guard.track(&task);
    until(|| {
        hooks.stdout_reader_held.load(Ordering::Acquire)
            && hooks.root_signaled.load(Ordering::Acquire)
            && hooks.job_empty.load(Ordering::Acquire)
    })
    .await;
    cancellation.cancel();
    assert!(matches!(result(task).await, Outcome::Unknown));
    assert!(domain.sealed.load(Ordering::Acquire));
    assert_eq!(domain.seals.load(Ordering::Acquire), 1);
    assert_eq!(domain.settled.load(Ordering::Acquire), 0);
    assert_eq!(supervisor.active_count(), 1);
    hooks.release_stdout_reader.store(true, Ordering::Release);
    until(|| domain.settled.load(Ordering::Acquire) == 1 && supervisor.active_count() == 0).await;
    assert!(domain.sealed.load(Ordering::Acquire));
    assert_eq!(domain.seals.load(Ordering::Acquire), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn output_limit_termination_keeps_tree_until_real_cleanup() {
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(Arc::clone(&supervisor), Arc::clone(&domain), None);
    let mut s = parent_spec(guard.path(), Duration::from_secs(10), 1, true, true, None);
    s.stdout_limit = 1024;
    s.overflow = OverflowPolicy::Terminate;
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    let ready = guard.path().join("ready");
    until(|| ready.is_file()).await;
    guard.take_children(&ready, 1);
    let Outcome::Settled(value) = result(task).await else {
        panic!("expected proven cleanup");
    };
    assert_eq!(value.reason, CompletionReason::OutputLimit);
    assert_eq!(value.stdout.bytes.len(), 1024);
    assert!(value.stdout.truncated);
    assert_eq!(
        unsafe { WaitForSingleObject(guard.children[0].as_raw_handle(), 0) },
        WAIT_OBJECT_0
    );
    assert!(!domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn failed_job_observation_stays_unknown_until_late_real_cleanup() {
    let hooks = Arc::new(TestHooks::default());
    hooks.fail_job_query.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let mut s = spec(
        guard.path(),
        Duration::from_secs(10),
        Some(Arc::clone(&hooks)),
    );
    s.args = vec!["exit".into(), "0".into()];
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    assert!(matches!(result(task).await, Outcome::Unknown));
    assert!(domain.sealed.load(Ordering::Acquire));
    assert_eq!(supervisor.active_count(), 1);
    hooks.fail_job_query.store(false, Ordering::Release);
    until(|| {
        hooks.root_signaled.load(Ordering::Acquire)
            && hooks.job_empty.load(Ordering::Acquire)
            && domain.settled.load(Ordering::Acquire) == 1
            && supervisor.active_count() == 0
    })
    .await;
    assert!(
        domain.sealed.load(Ordering::Acquire),
        "late cleanup does not reopen controller"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn reader_error_joins_and_cleans_but_unknown_is_sticky() {
    let hooks = Arc::new(TestHooks::default());
    hooks.fail_stdout_reader.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let mut s = spec(
        guard.path(),
        Duration::from_secs(10),
        Some(Arc::clone(&hooks)),
    );
    s.args = vec!["exit".into(), "0".into()];
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    assert!(matches!(result(task).await, Outcome::Unknown));
    assert!(domain.sealed.load(Ordering::Acquire));
    until(|| {
        hooks.stdout_failed_joined.load(Ordering::Acquire)
            && hooks.root_signaled.load(Ordering::Acquire)
            && hooks.job_empty.load(Ordering::Acquire)
            && domain.settled.load(Ordering::Acquire) == 1
            && supervisor.active_count() == 0
    })
    .await;
    assert!(
        domain.sealed.load(Ordering::Acquire),
        "late cleanup must not reopen unknown"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn live_reader_keeps_slot_even_after_process_and_job_finish() {
    let hooks = Arc::new(TestHooks::default());
    hooks.hold_stdout_reader.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let mut s = spec(
        guard.path(),
        Duration::from_secs(10),
        Some(Arc::clone(&hooks)),
    );
    s.args = vec!["exit".into(), "0".into()];
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    until(|| {
        hooks.stdout_reader_held.load(Ordering::Acquire)
            && hooks.root_signaled.load(Ordering::Acquire)
            && hooks.job_empty.load(Ordering::Acquire)
    })
    .await;
    assert!(!task.is_finished());
    assert_eq!(supervisor.active_count(), 1);
    assert_eq!(domain.settled.load(Ordering::Acquire), 0);
    hooks.release_stdout_reader.store(true, Ordering::Release);
    assert!(matches!(result(task).await, Outcome::Settled(_)));
    assert!(!domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn unknown_on_membership_and_created_hold_retains_then_cleans() {
    for hold_created in [false, true] {
        let hooks = Arc::new(TestHooks::default());
        hooks
            .fail_membership
            .store(!hold_created, Ordering::Release);
        hooks.hold_created.store(hold_created, Ordering::Release);
        let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
        let domain = Arc::new(Domain::default());
        let mut guard = FixtureGuard::new(
            Arc::clone(&supervisor),
            Arc::clone(&domain),
            Some(Arc::clone(&hooks)),
        );
        let marker = guard.path().join("must-not-run");
        let mut s = spec(
            guard.path(),
            if hold_created {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(10)
            },
            Some(Arc::clone(&hooks)),
        );
        s.cleanup_budget = Duration::from_millis(500);
        s.args = vec!["touch".into(), marker.clone().into_os_string()];
        let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
        guard.track(&task);
        if hold_created {
            until(|| hooks.created_held.load(Ordering::Acquire)).await;
        }
        assert!(matches!(result(task).await, Outcome::Unknown));
        assert!(domain.sealed.load(Ordering::Acquire));
        assert!(!marker.exists());
        if hold_created {
            assert_eq!(supervisor.active_count(), 1);
            hooks.release_created.store(true, Ordering::Release);
        }
        until(|| domain.settled.load(Ordering::Acquire) == 1 && supervisor.active_count() == 0)
            .await;
        assert!(!marker.exists());
        assert!(domain.sealed.load(Ordering::Acquire));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn watchdog_real_exit_is_covered_by_slot() {
    let hooks = Arc::new(TestHooks::default());
    hooks.hold_watchdog_exit.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let mut s = spec(
        guard.path(),
        Duration::from_secs(10),
        Some(Arc::clone(&hooks)),
    );
    s.args = vec!["exit".into(), "0".into()];
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    until(|| hooks.watchdog_held.load(Ordering::Acquire)).await;
    assert_eq!(supervisor.active_count(), 1, "watchdog has not exited");
    assert!(
        !task.is_finished(),
        "known result waits for the original holder thread"
    );
    hooks.release_watchdog.store(true, Ordering::Release);
    assert!(matches!(result(task).await, Outcome::Settled(_)));
    until(|| supervisor.active_count() == 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn known_result_allows_immediate_next_submit_at_capacity_one() {
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(Arc::clone(&supervisor), Arc::clone(&domain), None);
    for mode in ["missing", "0", "7"] {
        let mut s = spec(guard.path(), Duration::from_secs(10), None);
        s.args = vec!["exit".into(), mode.into()];
        if mode == "missing" {
            s.program = guard.path().join("missing.exe");
        }
        // Intentionally no active_count polling or delay between submissions.
        let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
        guard.track(&task);
        match result(task).await {
            Outcome::NotStarted { stage, .. } if mode == "missing" => {
                assert_eq!(stage, "create_returned_no_process")
            }
            Outcome::Settled(value) if mode != "missing" => {
                assert_eq!(value.exit_code, mode.parse::<u32>().unwrap());
                assert_eq!(value.reason, CompletionReason::Exited);
            }
            other => panic!("unexpected known result: {other:?}"),
        }
    }
    assert_eq!(domain.settled.load(Ordering::Acquire), 3);
    assert!(!domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn exit_code_failure_preserves_signal_and_waits_for_io() {
    let hooks = Arc::new(TestHooks::default());
    hooks.fail_exit_code.store(true, Ordering::Release);
    hooks.hold_stdout_reader.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let s = parent_spec(
        guard.path(),
        Duration::from_secs(10),
        1,
        true,
        false,
        Some(Arc::clone(&hooks)),
    );
    let ready = guard.path().join("ready");
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    until(|| ready.is_file() && hooks.stdout_reader_held.load(Ordering::Acquire)).await;
    guard.take_children(&ready, 1);
    assert!(!hooks.root_signaled.load(Ordering::Acquire));
    assert!(!domain.sealed.load(Ordering::Acquire));
    assert_eq!(domain.settled.load(Ordering::Acquire), 0);
    task.abort(); // Drop requests cancellation; holder retains all evidence.
    until(|| hooks.root_signaled.load(Ordering::Acquire) && domain.sealed.load(Ordering::Acquire))
        .await;
    assert_eq!(supervisor.active_count(), 1);
    assert_eq!(domain.settled.load(Ordering::Acquire), 0);
    hooks.release_stdout_reader.store(true, Ordering::Release);
    until(|| {
        hooks.job_empty.load(Ordering::Acquire)
            && domain.settled.load(Ordering::Acquire) == 1
            && supervisor.active_count() == 0
    })
    .await;
    assert!(domain.sealed.load(Ordering::Acquire));
    assert_eq!(domain.seals.load(Ordering::Acquire), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn exit_code_failure_returns_unknown_then_releases_once() {
    let hooks = Arc::new(TestHooks::default());
    hooks.fail_exit_code.store(true, Ordering::Release);
    hooks.hold_stdout_reader.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let mut s = spec(
        guard.path(),
        Duration::from_secs(10),
        Some(Arc::clone(&hooks)),
    );
    s.args = vec!["exit".into(), "0".into()];
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    assert!(matches!(result(task).await, Outcome::Unknown));
    until(|| {
        hooks.root_signaled.load(Ordering::Acquire)
            && hooks.stdout_reader_held.load(Ordering::Acquire)
    })
    .await;
    assert!(domain.sealed.load(Ordering::Acquire));
    assert_eq!(domain.settled.load(Ordering::Acquire), 0);
    assert_eq!(supervisor.active_count(), 1);
    hooks.release_stdout_reader.store(true, Ordering::Release);
    until(|| {
        hooks.job_empty.load(Ordering::Acquire)
            && domain.settled.load(Ordering::Acquire) == 1
            && supervisor.active_count() == 0
    })
    .await;
    assert_eq!(domain.seals.load(Ordering::Acquire), 1);
    assert!(domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn termination_failure_retains_live_tree_then_recovers_without_reopening() {
    let hooks = Arc::new(TestHooks::default());
    hooks.fail_termination.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let s = parent_spec(
        guard.path(),
        Duration::from_secs(10),
        1,
        true,
        false,
        Some(Arc::clone(&hooks)),
    );
    let op = submit(&supervisor, s, &domain);
    guard.active = true;
    let ready = guard.path().join("ready");
    until(|| ready.is_file()).await;
    guard.take_children(&ready, 1);
    op.cancel();
    let task = tokio::spawn(op.wait());
    guard.track(&task);
    assert!(matches!(result(task).await, Outcome::Unknown));
    assert!(domain.sealed.load(Ordering::Acquire));
    assert_eq!(
        unsafe { WaitForSingleObject(guard.children[0].as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
    assert_eq!(supervisor.active_count(), 1);
    assert_eq!(domain.settled.load(Ordering::Acquire), 0);
    let isolated = Arc::new(Domain::default());
    let failure = match supervisor.submit(
        spec(guard.path(), Duration::from_secs(10), None),
        Box::new(Lease(Arc::clone(&isolated))),
    ) {
        Ok(_) => panic!("live unknown must keep capacity"),
        Err(failure) => failure,
    };
    // This is a distinct unstarted lease, accounted separately.
    failure.lease.settle();
    assert_eq!(isolated.settled.load(Ordering::Acquire), 1);
    hooks.fail_termination.store(false, Ordering::Release);
    until(|| domain.settled.load(Ordering::Acquire) == 1 && supervisor.active_count() == 0).await;
    assert_eq!(
        unsafe { WaitForSingleObject(guard.children[0].as_raw_handle(), 0) },
        WAIT_OBJECT_0
    );
    assert!(domain.sealed.load(Ordering::Acquire));
    assert_eq!(domain.seals.load(Ordering::Acquire), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn resize_races_submit_without_erasing_existing_slots() {
    let hooks = Arc::new(TestHooks::default());
    hooks.hold_stdout_reader.store(true, Ordering::Release);
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(2).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(
        Arc::clone(&supervisor),
        Arc::clone(&domain),
        Some(Arc::clone(&hooks)),
    );
    let mut first = spec(
        guard.path(),
        Duration::from_secs(10),
        Some(Arc::clone(&hooks)),
    );
    first.args = vec!["exit".into(), "0".into()];
    let first_task = tokio::spawn(submit(&supervisor, first, &domain).wait());
    guard.track(&first_task);
    until(|| hooks.stdout_reader_held.load(Ordering::Acquire)).await;
    let mut second = spec(
        guard.path(),
        Duration::from_secs(10),
        Some(Arc::clone(&hooks)),
    );
    second.args = vec!["exit".into(), "0".into()];
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let race_result = std::thread::scope(|scope| {
        let b = Arc::clone(&barrier);
        let sup = Arc::clone(&supervisor);
        let resize = scope.spawn(move || {
            b.wait();
            sup.resize(NonZeroUsize::new(1).unwrap());
        });
        barrier.wait();
        let result = supervisor.submit(second, Box::new(Lease(Arc::clone(&domain))));
        resize.join().unwrap();
        result
    });
    let second_op = match race_result {
        Ok(op) => {
            assert_eq!(supervisor.active_count(), 2);
            Some(op)
        }
        Err(failure) => {
            failure.lease.settle();
            assert_eq!(supervisor.active_count(), 1);
            None
        }
    };
    let third = spec(guard.path(), Duration::from_secs(10), None);
    let rejection = match supervisor.submit(third, Box::new(Lease(Arc::clone(&domain)))) {
        Ok(_) => panic!("shrink must reject at or above limit"),
        Err(value) => value,
    };
    rejection.lease.settle();
    hooks.release_stdout_reader.store(true, Ordering::Release);
    assert!(matches!(result(first_task).await, Outcome::Settled(_)));
    if let Some(op) = second_op {
        assert!(matches!(op.wait().await, Outcome::Settled(_)));
    }
    until(|| supervisor.active_count() == 0).await;
    let mut last = spec(guard.path(), Duration::from_secs(10), None);
    last.args = vec!["exit".into(), "0".into()];
    let task = tokio::spawn(submit(&supervisor, last, &domain).wait());
    guard.track(&task);
    assert!(matches!(result(task).await, Outcome::Settled(_)));
    assert!(!domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn argv_cwd_environment_roundtrip_and_invalid_prestart() {
    fn append(out: &mut Vec<u8>, value: &std::ffi::OsStr) {
        let wide: Vec<u16> = value.encode_wide().collect();
        out.extend_from_slice(&(wide.len() as u32).to_le_bytes());
        for unit in wide {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(Arc::clone(&supervisor), Arc::clone(&domain), None);
    let sentinel = "NHC_PARENT_SENTINEL_B23";
    let old = std::env::var_os(sentinel);
    unsafe { std::env::set_var(sentinel, "parent-only") };
    let values: Vec<OsString> = [
        "",
        "space and 中文",
        "quoted\"part",
        "tail\\",
        "quoted \" and tail\\\\",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let receipt = guard.path().join("roundtrip.bin");
    let mut s = spec(guard.path(), Duration::from_secs(10), None);
    s.environment
        .push(("NHC_ROUNDTRIP_VALUE".into(), "value 空格 \\".into()));
    s.args = vec!["roundtrip".into(), receipt.clone().into_os_string()];
    s.args.extend(values.iter().cloned());
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    let result_value = result(task).await;
    unsafe {
        if let Some(v) = old {
            std::env::set_var(sentinel, v)
        } else {
            std::env::remove_var(sentinel)
        }
    };
    assert!(matches!(result_value, Outcome::Settled(ref v) if v.exit_code == 0));
    let mut expected = Vec::new();
    for value in &values {
        append(&mut expected, value);
    }
    append(&mut expected, guard.path().as_os_str());
    append(&mut expected, std::ffi::OsStr::new("value 空格 \\"));
    expected.push(0);
    assert_eq!(std::fs::read(receipt).unwrap(), expected);
    until(|| supervisor.active_count() == 0).await;
    for mode in ["nul-arg", "nul-env", "duplicate"] {
        let marker = guard.path().join(format!("{mode}-ran"));
        let mut s = spec(guard.path(), Duration::from_secs(10), None);
        s.args = vec!["touch".into(), marker.clone().into_os_string()];
        if mode == "duplicate" {
            s.environment.extend([
                ("NHC_DUP".into(), "a".into()),
                ("nhc_dup".into(), "b".into()),
            ]);
        } else if mode == "nul-env" {
            s.environment.push((
                "NHC_BAD".into(),
                OsString::from_wide(&[b'x' as u16, 0, b'y' as u16]),
            ));
        } else {
            s.args
                .push(OsString::from_wide(&[b'x' as u16, 0, b'y' as u16]));
        }
        let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
        guard.track(&task);
        assert!(matches!(result(task).await, Outcome::NotStarted { .. }));
        assert!(!marker.exists());
        until(|| supervisor.active_count() == 0).await;
    }
    assert!(!domain.sealed.load(Ordering::Acquire));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated fixed executable and serial process supervision"]
async fn breakaway_independent_controls_and_component_denial() {
    let supervisor = Arc::new(Supervisor::new(NonZeroUsize::new(1).unwrap()));
    let domain = Arc::new(Domain::default());
    let mut guard = FixtureGuard::new(Arc::clone(&supervisor), Arc::clone(&domain), None);
    let allowed = crate::process_lifetime::breakaway_control(
        &fixture_exe(),
        &guard.path().join("allow"),
        true,
    );
    assert!(allowed.contains("created=1 query=1 member=0"), "{allowed}");
    let denied = crate::process_lifetime::breakaway_control(
        &fixture_exe(),
        &guard.path().join("deny"),
        false,
    );
    assert!(denied.starts_with("denied="), "{denied}");
    let mut s = spec(guard.path(), Duration::from_secs(10), None);
    s.args = vec!["breakaway".into()];
    let task = tokio::spawn(submit(&supervisor, s, &domain).wait());
    guard.track(&task);
    let Outcome::Settled(value) = result(task).await else {
        panic!("component not settled")
    };
    assert_eq!(value.exit_code, 0);
    assert!(String::from_utf8_lossy(&value.stdout.bytes).contains("breakaway-denied="));
    assert!(!domain.sealed.load(Ordering::Acquire));
}
