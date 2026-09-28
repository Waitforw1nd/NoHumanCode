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

#[cfg(test)]
fn unknown_faults() -> &'static std::sync::Mutex<std::collections::HashMap<String, bool>> {
    static FAULTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(Default::default)
}
/// Test-only, task-scoped, single-use injection after a real process spawn.
#[cfg(test)]
pub(crate) struct UnknownAfterSpawnGuard(String);
#[cfg(test)]
impl Drop for UnknownAfterSpawnGuard {
    fn drop(&mut self) {
        unknown_faults().lock().unwrap().remove(&self.0);
    }
}
#[cfg(test)]
pub(crate) fn unknown_after_spawn_once(task_id: &str) -> UnknownAfterSpawnGuard {
    let inserted = {
        let mut faults = unknown_faults().lock().unwrap();
        if let std::collections::hash_map::Entry::Vacant(entry) = faults.entry(task_id.to_owned()) {
            entry.insert(true);
            true
        } else {
            false
        }
    };
    // Assert outside the lock: rejecting duplicate registration must not poison it.
    assert!(inserted, "duplicate task-scoped command fault registration");
    UnknownAfterSpawnGuard(task_id.to_owned())
}
#[cfg(test)]
fn take_unknown_fault(task_id: &str) -> bool {
    unknown_faults()
        .lock()
        .unwrap()
        .get_mut(task_id)
        .is_some_and(std::mem::take)
}

// Even an unexpected unwind/runtime teardown must not detach readers. Normal
// completion and the bounded finalizer still explicitly join each task.
struct ReaderTask(tokio::task::JoinHandle<Result<Vec<u8>>>);
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

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
    #[cfg(test)]
    let probe = tests::PROBE.try_with(std::sync::Arc::clone).ok();
    #[cfg(test)]
    if let Some(probe) = &probe {
        probe
            .pid
            .store(child.id().unwrap(), std::sync::atomic::Ordering::SeqCst);
        #[cfg(windows)]
        probe.capture_process(&child);
    }
    #[cfg(test)]
    let forced_unknown = take_unknown_fault(&task.id);
    // These are guaranteed by Stdio::piped, but still take the cleanup path if
    // an unexpected missing pipe is ever encountered after the side effect.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    #[cfg(test)]
    let out_probe = probe.clone();
    let mut out = ReaderTask(tokio::spawn(async move {
        #[cfg(test)]
        let _reader = tests::ReaderGuard::new(out_probe.clone());
        #[cfg(test)]
        if out_probe.as_ref().is_some_and(|p| p.reader_error) {
            bail!("test reader failure");
        }
        let mut bytes = Vec::new();
        stdout
            .context("missing stdout")?
            .take(65536)
            .read_to_end(&mut bytes)
            .await?;
        Ok::<_, anyhow::Error>(bytes)
    }));
    #[cfg(test)]
    let err_probe = probe.clone();
    let mut err = ReaderTask(tokio::spawn(async move {
        #[cfg(test)]
        let _reader = tests::ReaderGuard::new(err_probe);
        let mut bytes = Vec::new();
        stderr
            .context("missing stderr")?
            .take(65536)
            .read_to_end(&mut bytes)
            .await?;
        Ok::<_, anyhow::Error>(bytes)
    }));
    let mut out_joined = false;
    let mut err_joined = false;
    let completion = timeout_at(deadline, async {
        #[cfg(test)]
        if forced_unknown { bail!("test unknown outcome after real spawn"); }
        let mut status = None;
        let mut output = None;
        let mut errors = None;
        while status.is_none() || output.is_none() || errors.is_none() {
            tokio::select! {
                value = child.wait(), if status.is_none() => {
                    status = Some(value?);
                    #[cfg(test)]
                    if probe.as_ref().is_some_and(|p| p.wait_error) { bail!("test wait failure"); }
                    #[cfg(test)]
                    if let Some(probe) = &probe { probe.parent_waited.store(true, std::sync::atomic::Ordering::SeqCst); }
                }
                value = &mut out.0, if !out_joined => {
                    out_joined = true;
                    output = Some(value??);
                }
                value = &mut err.0, if !err_joined => {
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
        out.0.abort();
    }
    if !err_joined {
        err.0.abort();
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
            let _ = (&mut out.0).await;
        }
        if !err_joined {
            let _ = (&mut err.0).await;
        }
        process_result
    })
    .await;
    #[cfg(test)]
    if probe.as_ref().is_some_and(|p| p.cleanup_unknown) {
        return Err(CommandOutcomeUnknown.into());
    }
    if !matches!(cleanup, Ok(Ok(()))) || completion.is_ok() {
        return Err(CommandOutcomeUnknown.into());
    }
    bail!("命令运行超过 {timeout_seconds} 秒；已结束直接子进程等待与输出收集")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    };
    tokio::task_local! { pub(super) static PROBE: Arc<Probe>; }
    #[derive(Default)]
    pub(super) struct Probe {
        pub case: &'static str,
        pub reader_error: bool,
        pub wait_error: bool,
        pub cleanup_unknown: bool,
        pub pid: AtomicU32,
        pub parent_waited: AtomicBool,
        readers: AtomicUsize,
        #[cfg(windows)]
        original: std::sync::Mutex<Option<std::result::Result<ProcessObservation, String>>>,
    }
    #[cfg(windows)]
    struct ProcessObservation {
        handle: std::os::windows::io::OwnedHandle,
        pid: u32,
        created: u64,
    }
    #[cfg(windows)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetProcessTimes(
            handle: *mut std::ffi::c_void,
            created: *mut u64,
            exited: *mut u64,
            kernel: *mut u64,
            user: *mut u64,
        ) -> i32;
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn GetProcessId(handle: *mut std::ffi::c_void) -> u32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn DuplicateHandle(
            source_process: *mut std::ffi::c_void,
            source: *mut std::ffi::c_void,
            target_process: *mut std::ffi::c_void,
            target: *mut *mut std::ffi::c_void,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
        fn TerminateProcess(handle: *mut std::ffi::c_void, exit_code: u32) -> i32;
    }
    #[cfg(windows)]
    #[derive(Debug)]
    struct ExitSignal {
        wait_status: u32,
        query_error: Option<i32>,
    }
    #[cfg(windows)]
    impl ExitSignal {
        fn require_exited(&self) -> Result<()> {
            ensure!(
                self.wait_status == 0 && self.query_error.is_none(),
                "original process exit not confirmed: {self:?}"
            );
            Ok(())
        }
    }
    #[cfg(windows)]
    impl ProcessObservation {
        fn capture(child: &tokio::process::Child) -> Result<Self> {
            use std::os::windows::io::BorrowedHandle;
            let pid = child.id().context("original child has no process id")?;
            let raw = child.raw_handle().context("original child has no handle")?;
            // The actual Child still owns this handle before any wait is polled.
            let handle = unsafe { BorrowedHandle::borrow_raw(raw) }.try_clone_to_owned()?;
            Self::from_handle(handle, pid)
        }
        fn from_handle(handle: std::os::windows::io::OwnedHandle, pid: u32) -> Result<Self> {
            use std::os::windows::io::AsRawHandle;
            let actual_pid = unsafe { GetProcessId(handle.as_raw_handle()) };
            if actual_pid == 0 {
                return Err(std::io::Error::last_os_error()).context("GetProcessId");
            }
            ensure!(actual_pid == pid, "original handle PID mismatch");
            let created = Self::creation_time(handle.as_raw_handle())?;
            Ok(Self {
                handle,
                pid,
                created,
            })
        }
        fn creation_time(handle: *mut std::ffi::c_void) -> Result<u64> {
            let (mut created, mut exited, mut kernel, mut user) = (0, 0, 0, 0);
            if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
                == 0
            {
                return Err(std::io::Error::last_os_error()).context("GetProcessTimes");
            }
            Ok(created)
        }
        fn signal(&self, case: &str, phase: &str) -> ExitSignal {
            use std::os::windows::io::AsRawHandle;
            let wait_status = unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) };
            let query_error = (wait_status == u32::MAX)
                .then(|| std::io::Error::last_os_error().raw_os_error())
                .flatten();
            let signal = ExitSignal {
                wait_status,
                query_error,
            };
            eprintln!(
                "s06-process {}",
                json!({
                    "case":case, "phase":phase, "pid":self.pid, "created":self.created,
                    "wait_status":wait_status, "query_error":query_error,
                    "exit_confirmed":signal.require_exited().is_ok()
                })
            );
            signal
        }
        async fn terminate_fixture(&self, case: &str) -> Result<()> {
            use std::os::windows::io::AsRawHandle;
            let deadline = Instant::now() + Duration::from_secs(6);
            let signal = self.signal(case, "before-fixture-cleanup");
            if signal.wait_status == 0 {
                return signal.require_exited();
            }
            ensure!(
                signal.wait_status == 258,
                "fixture query failed: {signal:?}"
            );
            let terminated = unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) };
            let error = (terminated == 0).then(std::io::Error::last_os_error);
            eprintln!(
                "s06-process {}",
                json!({
                    "case":case, "phase":"terminate-original-fixture", "pid":self.pid,
                    "created":self.created, "terminate_result":terminated,
                    "terminate_error":error.as_ref().and_then(std::io::Error::raw_os_error)
                })
            );
            // A concurrent natural exit needs the same original handle signaled.
            if let Some(error) = error {
                return self
                    .signal(case, "terminate-failed-query")
                    .require_exited()
                    .with_context(|| format!("TerminateProcess failed: {error}"));
            }
            let handle = self.handle.try_clone()?;
            let signal = timeout_at(
                deadline,
                tokio::task::spawn_blocking(move || {
                    use std::os::windows::io::AsRawHandle;
                    let wait_status = unsafe { WaitForSingleObject(handle.as_raw_handle(), 6000) };
                    let query_error = (wait_status == u32::MAX)
                        .then(|| std::io::Error::last_os_error().raw_os_error())
                        .flatten();
                    ExitSignal {
                        wait_status,
                        query_error,
                    }
                }),
            )
            .await??;
            eprintln!(
                "s06-process {}",
                json!({
                    "case":case, "phase":"fixture-cleanup-wait", "pid":self.pid,
                    "created":self.created, "wait_status":signal.wait_status,
                    "query_error":signal.query_error
                })
            );
            signal.require_exited()
        }
    }
    #[cfg(windows)]
    impl Probe {
        pub(super) fn capture_process(&self, child: &tokio::process::Child) {
            use std::io::Write;
            // Retain failures for the test assertion; never interrupt product cleanup.
            let captured = ProcessObservation::capture(child).map_err(|error| format!("{error:#}"));
            let _ = writeln!(
                std::io::stderr().lock(),
                "s06-process {}",
                json!({
                    "case":self.case, "phase":"capture-original-child",
                    "pid":child.id(), "created":captured.as_ref().ok().map(|p| p.created),
                    "probe_error":captured.as_ref().err(), "reader_error":self.reader_error,
                    "cleanup_unknown":self.cleanup_unknown, "wait_error":self.wait_error
                })
            );
            *self.original.lock().unwrap() = Some(captured);
        }
    }
    pub(super) struct ReaderGuard(Option<Arc<Probe>>);
    impl ReaderGuard {
        pub fn new(probe: Option<Arc<Probe>>) -> Self {
            if let Some(probe) = &probe {
                probe.readers.fetch_add(1, Ordering::SeqCst);
            }
            Self(probe)
        }
    }
    impl Drop for ReaderGuard {
        fn drop(&mut self) {
            if let Some(probe) = &self.0 {
                probe.readers.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
    fn task(root: &std::path::Path) -> Task {
        serde_json::from_value(json!({"id":"command-test","run_id":"run",
            "spec":{"name":"worker","role":"test","route_id":"route","prompt":"test",
                "depends_on":[],"write_scopes":[],"tools":true,"allow_commands":true,"max_rounds":1},
            "route":{"id":"route","name":"route","base_url":"http://127.0.0.1","model":"mock",
                "max_tokens":16,"parallel_limit":1,"key_env":null},
            "workspace":root.to_str().unwrap(),"status":"running","output":"","error":null,"messages":[],
            "usage":null,"created_at":0,"updated_at":0})).unwrap()
    }
    fn original_exit(probe: &Probe) -> Result<()> {
        #[cfg(windows)]
        {
            let captured = probe.original.lock().unwrap();
            let observation = captured
                .as_ref()
                .with_context(|| format!("{}: original process probe missing", probe.case))?
                .as_ref()
                .map_err(|error| {
                    anyhow::anyhow!("{}: original process capture failed: {error}", probe.case)
                })?;
            ensure!(
                observation.pid == probe.pid.load(Ordering::SeqCst),
                "probe PID mismatch"
            );
            observation
                .signal(probe.case, "assert-original-exited")
                .require_exited()
        }
        #[cfg(not(windows))]
        bail!(
            "{}: stable original process observation requires Windows (pid={})",
            probe.case,
            probe.pid.load(Ordering::SeqCst)
        )
    }
    #[cfg(windows)]
    #[derive(serde::Serialize, serde::Deserialize)]
    struct TransferredProcess {
        handle: usize,
        pid: u32,
        created: u64,
    }
    #[cfg(windows)]
    fn receive_descendant(root: &std::path::Path) -> Result<ProcessObservation> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        let transfer: TransferredProcess =
            serde_json::from_slice(&std::fs::read(root.join("descendant-ready"))?)?;
        ensure!(
            transfer.handle != 0 && transfer.handle != usize::MAX,
            "invalid transferred handle"
        );
        // The fixture duplicates its own live process handle into this test process.
        // The atomic ready file transfers ownership exactly once; no PID is reopened.
        let handle =
            unsafe { OwnedHandle::from_raw_handle(transfer.handle as *mut std::ffi::c_void) };
        let observation = ProcessObservation::from_handle(handle, transfer.pid)?;
        ensure!(
            observation.created == transfer.created,
            "transferred creation time mismatch"
        );
        eprintln!(
            "s06-process {}",
            json!({
                "case":"inherited-pipe", "phase":"receive-original-descendant",
                "pid":observation.pid, "created":observation.created, "probe_error":null
            })
        );
        Ok(observation)
    }
    #[cfg(windows)]
    #[test]
    fn s06_inherited_pipe_child() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        let Ok(observer_pid) = std::env::var("NHC_S06_OBSERVER_PID") else {
            return;
        };
        let observer_pid: u32 = observer_pid.parse().unwrap();
        let root = std::path::PathBuf::from(std::env::var_os("NHC_S06_OBSERVATION_ROOT").unwrap());
        // Only the still-running test observer is opened by PID. The observed
        // descendant is this actual process, never a later numeric PID lookup.
        let observer = unsafe { OpenProcess(0x0040, 0, observer_pid) };
        assert!(
            !observer.is_null(),
            "OpenProcess(observer): {}",
            std::io::Error::last_os_error()
        );
        let observer = unsafe { OwnedHandle::from_raw_handle(observer) };
        let current = unsafe { GetCurrentProcess() };
        let created = ProcessObservation::creation_time(current).unwrap();
        let mut transferred = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                DuplicateHandle(
                    current,
                    current,
                    observer.as_raw_handle(),
                    &mut transferred,
                    0x00100000 | 0x1000 | 0x0001,
                    0,
                    0,
                )
            },
            0,
            "DuplicateHandle(descendant): {}",
            std::io::Error::last_os_error()
        );
        let transfer = TransferredProcess {
            handle: transferred as usize,
            pid: std::process::id(),
            created,
        };
        let publish: Result<()> = (|| {
            std::fs::write(
                root.join("descendant-pending"),
                serde_json::to_vec(&transfer)?,
            )?;
            std::fs::rename(
                root.join("descendant-pending"),
                root.join("descendant-ready"),
            )?;
            Ok(())
        })();
        if let Err(error) = publish {
            // Publication failed: recover and close the unconsumed remote handle.
            let mut recovered = std::ptr::null_mut();
            let closed = unsafe {
                DuplicateHandle(
                    observer.as_raw_handle(),
                    transferred,
                    current,
                    &mut recovered,
                    0,
                    0,
                    0x0001 | 0x0002,
                )
            };
            if closed != 0 {
                drop(unsafe { OwnedHandle::from_raw_handle(recovered) });
            }
            panic!("descendant identity publication failed: {error:#}; recovered={closed}");
        }
        eprintln!(
            "s06-process {}",
            json!({
                "case":"inherited-pipe", "phase":"transfer-live-self",
                "pid":transfer.pid, "created":created, "observer_pid":observer_pid,
                "duplicate_result":true
            })
        );
        // Same five-second fixture fallback as before, not product cleanup.
        std::thread::sleep(Duration::from_secs(5));
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn s06_original_handle_rejects_live_and_query_failure_accepts_exit() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        let mut command = Command::new("pwsh.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 20",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .creation_flags(0x08000000);
        let mut child = command.spawn().unwrap();
        let observed = ProcessObservation::capture(&child);
        let checks: Result<(ExitSignal, ExitSignal)> = (|| {
            let observation = observed
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error:#}"))?;
            let live = observation.signal("identity-controls", "live-original");
            let current = unsafe { GetCurrentProcess() };
            let mut query_only = std::ptr::null_mut();
            // A valid original-process handle without SYNCHRONIZE must fail a wait.
            if unsafe {
                DuplicateHandle(
                    current,
                    observation.handle.as_raw_handle(),
                    current,
                    &mut query_only,
                    0x1000,
                    0,
                    0,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error()).context("DuplicateHandle(query-only)");
            }
            let query_only = ProcessObservation::from_handle(
                unsafe { OwnedHandle::from_raw_handle(query_only) },
                observation.pid,
            )?;
            let failed = query_only.signal("identity-controls", "query-without-synchronize");
            Ok((live, failed))
        })();
        // Clean the actual Child before asserting the controls, even on capture failure.
        child.start_kill().unwrap();
        let status = timeout_at(Instant::now() + Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        eprintln!(
            "s06-process {}",
            json!({"case":"identity-controls",
            "phase":"original-child-wait", "exit_code":status.code()})
        );
        let observation = observed.unwrap();
        let (live, failed) = checks.unwrap();
        assert_eq!(live.wait_status, 258);
        assert!(live.require_exited().is_err(), "a live original must fail");
        assert_eq!(failed.wait_status, u32::MAX);
        assert_eq!(failed.query_error, Some(5));
        assert!(
            failed.require_exited().is_err(),
            "a query failure must fail"
        );
        observation
            .signal("identity-controls", "waited-original")
            .require_exited()
            .unwrap();
    }
    #[tokio::test]
    async fn s06_real_timeout_reaps_child_and_readers_without_false_unknown() {
        let root = tempfile::tempdir().unwrap();
        let task = task(root.path());
        let probe = Arc::new(Probe {
            case: "real-timeout",
            ..Probe::default()
        });
        let start = Instant::now();
        let error = PROBE.scope(probe.clone(), execute(&task, &json!({
            "command":"[IO.File]::WriteAllText((Join-Path $PWD 'ready'), 'ready'); Start-Sleep -Seconds 20",
            "timeout_seconds":1
        }))).await.unwrap_err();
        let original_exit = original_exit(&probe);
        assert!(root.path().join("ready").exists());
        assert!(!error.is::<CommandOutcomeUnknown>(), "{error:#}");
        assert!(error.to_string().contains("超过 1 秒"));
        assert!(start.elapsed() < Duration::from_secs(7));
        assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
        original_exit.unwrap();
    }
    #[tokio::test]
    async fn s06_reader_error_and_unconfirmed_cleanup_are_typed_unknown() {
        let root = tempfile::tempdir().unwrap();
        let task = task(root.path());
        for (case, reader_error, cleanup_unknown, wait_error) in [
            ("reader-error", true, false, false),
            ("cleanup-unknown", false, true, false),
            ("wait-error", false, false, true),
        ] {
            let probe = Arc::new(Probe {
                case,
                reader_error,
                cleanup_unknown,
                wait_error,
                ..Probe::default()
            });
            let error = PROBE.scope(probe.clone(), execute(&task, &json!({
                "command": if wait_error { "exit 0" } else { "Start-Sleep -Seconds 20" }, "timeout_seconds":1
            }))).await.unwrap_err();
            let original_exit = original_exit(&probe);
            assert!(error.is::<CommandOutcomeUnknown>());
            assert!(!error.to_string().contains("已停止"));
            assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
            original_exit.unwrap();
        }
    }
    #[tokio::test]
    async fn s06_deadline_covers_inherited_pipe_after_direct_parent_exit() {
        #[cfg(windows)]
        {
            let root = tempfile::tempdir().unwrap();
            let task = task(root.path());
            // No stdout/stderr redirect: the real helper inherits the parent's pipes.
            // It transfers its own original handle before publishing readiness.
            let executable = std::env::current_exe()
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\'', "''");
            let observer_pid = std::process::id();
            let command = format!(
                "$p=[Diagnostics.ProcessStartInfo]::new('{executable}'); $p.UseShellExecute=$false; $p.CreateNoWindow=$true; $p.ArgumentList.Add('--exact'); $p.ArgumentList.Add('command_execution::tests::s06_inherited_pipe_child'); $p.ArgumentList.Add('--nocapture'); $p.Environment['NHC_S06_OBSERVER_PID']='{observer_pid}'; $p.Environment['NHC_S06_OBSERVATION_ROOT']=[string]$PWD; $child=[Diagnostics.Process]::Start($p); $deadline=[DateTime]::UtcNow.AddSeconds(1); while (!(Test-Path (Join-Path $PWD 'descendant-ready'))) {{ if ([DateTime]::UtcNow -gt $deadline) {{ exit 97 }}; Start-Sleep -Milliseconds 10 }}; [Console]::Out.Write('parent-exit')"
            );
            let probe = Arc::new(Probe {
                case: "inherited-pipe",
                ..Probe::default()
            });
            let begin = Instant::now();
            let result = PROBE
                .scope(
                    probe.clone(),
                    execute(&task, &json!({"command":command,"timeout_seconds":2})),
                )
                .await;
            let parent_waited = probe.parent_waited.load(Ordering::SeqCst);
            let elapsed = begin.elapsed();
            // Snapshot now; fixture cleanup must not provide extra time for the parent.
            let original_exit = original_exit(&probe);
            // Cleanup is bound to the transferred original handle, including failure paths.
            let descendant = receive_descendant(root.path())
                .inspect_err(|error| {
                    eprintln!(
                        "s06-process {}",
                        json!({"case":"inherited-pipe",
                "phase":"receive-original-descendant", "probe_error":format!("{error:#}")})
                    );
                })
                .unwrap();
            descendant
                .terminate_fixture("inherited-pipe")
                .await
                .unwrap();
            assert!(
                parent_waited,
                "fixture did not observe direct parent exit before deadline"
            );
            let error = result.unwrap_err();
            assert!(!error.is::<CommandOutcomeUnknown>(), "{error:#}");
            assert!(
                elapsed < Duration::from_secs(4),
                "pipe did not obey execution deadline: {elapsed:?}"
            );
            assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
            original_exit.unwrap();
            descendant
                .signal("inherited-pipe", "assert-descendant-exited")
                .require_exited()
                .unwrap();
        }
        #[cfg(not(windows))]
        panic!("stable inherited-pipe process observation requires Windows");
    }
    #[tokio::test]
    async fn s06_task_scoped_unknown_hook_is_once_guarded_and_reaps_real_spawn() {
        let root = tempfile::tempdir().unwrap();
        let mut task = task(root.path());
        task.id = "d-s06-once-hook".into();
        let guard = unknown_after_spawn_once(&task.id);
        assert!(std::panic::catch_unwind(|| unknown_after_spawn_once(&task.id)).is_err());
        let mut other = task.clone();
        other.id = "d-s06-unaffected".into();
        assert_eq!(
            execute(&other, &json!({"command":"[Console]::Out.Write('other')"}))
                .await
                .unwrap()["stdout"],
            "other"
        );
        let probe = Arc::new(Probe {
            case: "task-scoped-unknown",
            ..Probe::default()
        });
        let error = PROBE
            .scope(
                probe.clone(),
                execute(&task, &json!({"command":"Start-Sleep -Seconds 20"})),
            )
            .await
            .unwrap_err();
        assert!(error.is::<CommandOutcomeUnknown>());
        assert_ne!(probe.pid.load(Ordering::SeqCst), 0);
        original_exit(&probe).unwrap();
        assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
        assert_eq!(
            execute(
                &task,
                &json!({"command":"[Console]::Out.Write('consumed')"})
            )
            .await
            .unwrap()["stdout"],
            "consumed"
        );
        assert!(std::panic::catch_unwind(|| unknown_after_spawn_once(&task.id)).is_err());
        drop(guard);
        let unused = unknown_after_spawn_once(&task.id);
        drop(unused);
        assert_eq!(
            execute(&task, &json!({"command":"[Console]::Out.Write('cleared')"}))
                .await
                .unwrap()["stdout"],
            "cleared"
        );
    }
}
