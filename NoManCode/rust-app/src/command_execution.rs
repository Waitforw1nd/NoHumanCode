//! Bounded ownership of one approved command. This is not an OS process sandbox.
use crate::domain::Task;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
#[cfg(test)]
use std::process::Stdio;
use std::{fmt, time::Duration};
#[cfg(test)]
use tokio::{
    io::{AsyncRead, AsyncReadExt},
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
#[cfg(test)]
struct ReaderTask(tokio::task::JoinHandle<Result<Vec<u8>>>);
#[cfg(test)]
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

const OUTPUT_LIMIT: usize = 65_536;

/// PowerShell exposes a verbatim cwd as a provider-qualified `$PWD`, which
/// breaks Join-Path arguments passed to .NET file APIs. Only the launch spelling
/// changes; authority and executable checks continue to use the canonical root.
#[cfg(windows)]
fn shell_cwd(root: &std::path::Path) -> Result<std::path::PathBuf> {
    use std::path::{Component, PathBuf, Prefix};
    let mut components = root.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => bail!("命令工作区必须是本地 drive 路径"),
        },
        _ => bail!("命令工作区必须是绝对路径"),
    };
    ensure!(
        matches!(components.next(), Some(Component::RootDir)),
        "命令工作区必须是绝对路径"
    );
    let mut cwd = PathBuf::from(format!("{}:\\", char::from(drive)));
    for component in components {
        let Component::Normal(name) = component else {
            bail!("命令工作区包含非标准路径组件");
        };
        cwd.push(name);
    }
    ensure!(
        cwd.canonicalize().context("命令启动目录不可用")? == root,
        "命令启动目录与真实工作区不一致"
    );
    Ok(cwd)
}

#[cfg(not(windows))]
fn shell_cwd(root: &std::path::Path) -> Result<std::path::PathBuf> {
    Ok(root.to_path_buf())
}

/// The retention bound is not an EOF: keep the pipe open and drain the rest.
/// Completion still requires the real EOF (or the caller's cleanup deadline).
#[cfg(test)]
async fn read_output(mut reader: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(bytes);
        }
        let retain = count.min(OUTPUT_LIMIT.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..retain]);
    }
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
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_seconds);
    let root = std::fs::canonicalize(&task.workspace).context("命令工作区不可用")?;
    // CreateProcessW does not search PATH when lpApplicationName is non-null.
    // Resolve only absolute PATH entries, never a workspace-provided executable.
    let executable = std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .filter(|path| path.is_absolute())
                .filter_map(|path| path.join("pwsh.exe").canonicalize().ok())
                .find(|path| path.is_file() && !path.starts_with(&root))
        })
        .context("无法解析可信 PowerShell 程序")?;
    let environment = std::env::vars_os()
        .filter(|(name, _)| {
            let name = name.to_string_lossy().to_ascii_uppercase();
            !name.contains("KEY") && !name.contains("TOKEN") && !name.contains("SECRET")
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
    spec.args = ["-NoProfile", "-NonInteractive", "-Command", command]
        .into_iter()
        .map(Into::into)
        .collect();
    #[cfg(all(test, windows))]
    let hooks = {
        let hooks = std::sync::Arc::new(crate::process_lifetime::TestHooks::default());
        let forced = take_unknown_fault(&task.id);
        let probe = tests::PROBE.try_with(std::sync::Arc::clone).ok();
        use std::sync::atomic::Ordering::SeqCst;
        hooks.fail_stdout_reader.store(
            forced || probe.as_ref().is_some_and(|p| p.reader_error),
            SeqCst,
        );
        hooks
            .fail_exit_code
            .store(probe.as_ref().is_some_and(|p| p.wait_error), SeqCst);
        hooks
            .hold_stdout_reader
            .store(probe.as_ref().is_some_and(|p| p.cleanup_unknown), SeqCst);
        spec.test_hooks = Some(hooks.clone());
        hooks
    };
    let outcome = crate::execution_authority::run_process(spec).await?;
    #[cfg(all(test, windows))]
    {
        use std::sync::atomic::Ordering::SeqCst;
        hooks.release_stdout_reader.store(true, SeqCst);
        hooks.fail_exit_code.store(false, SeqCst);
        let until = tokio::time::Instant::now() + Duration::from_secs(5);
        while !(hooks.root_signaled.load(SeqCst)
            && hooks.job_empty.load(SeqCst)
            && hooks.stdout_reader_joined.load(SeqCst)
            && hooks.stderr_reader_joined.load(SeqCst))
            && tokio::time::Instant::now() < until
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if let Ok(probe) = tests::PROBE.try_with(std::sync::Arc::clone) {
            probe.capture_supervised(&hooks);
        }
    }
    match outcome {
        Outcome::NotStarted { stage, os_error } => {
            bail!("无法启动 PowerShell ({stage}, {os_error:?})")
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
async fn collect(
    mut child: tokio::process::Child,
    deadline: Instant,
    timeout_seconds: u64,
    #[cfg(test)] probe: Option<std::sync::Arc<tests::Probe>>,
    #[cfg(test)] forced_unknown: bool,
) -> Result<Value> {
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
        read_output(stdout.context("missing stdout")?).await
    }));
    #[cfg(test)]
    let err_probe = probe.clone();
    let mut err = ReaderTask(tokio::spawn(async move {
        #[cfg(test)]
        let _reader = tests::ReaderGuard::new(err_probe);
        read_output(stderr.context("missing stderr")?).await
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
        // This proves direct-child exit and real pipe EOF, not process-tree
        // exit: a descendant can redirect/close both pipes and keep running.
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
    async fn execute(task: &Task, args: &Value) -> Result<Value> {
        let data = tempfile::tempdir()?;
        let store = Arc::new(crate::store::Store::open(&data.path().join("shell.db"))?);
        let _engine = crate::engine::Engine::new(store.clone(), 1)?;
        let operation = store.execution_operation(tokio_util::sync::CancellationToken::new())?;
        operation.run(super::execute(task, args)).await
    }

    #[tokio::test]
    async fn output_reader_drains_beyond_limit_and_keeps_only_prefix() {
        let input: Vec<u8> = (0..OUTPUT_LIMIT * 3).map(|i| (i % 251) as u8).collect();
        let mut source = input.as_slice();
        let output = read_output(&mut source).await.unwrap();
        assert_eq!(output, input[..OUTPUT_LIMIT]);
        assert!(
            source.is_empty(),
            "truncating capture must still drain all input"
        );
    }

    #[tokio::test]
    async fn output_reader_waits_for_real_eof_after_limit() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(OUTPUT_LIMIT + 1024);
        writer
            .write_all(&vec![b'x'; OUTPUT_LIMIT + 1])
            .await
            .unwrap();
        let mut capture = Box::pin(read_output(reader));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut capture)
                .await
                .is_err(),
            "capture limit must not manufacture EOF while a writer is alive"
        );
        writer.write_all(b"discarded suffix").await.unwrap();
        drop(writer);
        let output = tokio::time::timeout(Duration::from_secs(1), capture)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output, vec![b'x'; OUTPUT_LIMIT]);
    }

    #[tokio::test]
    async fn output_reader_reports_errors_after_capture_limit() {
        struct FailingReader;
        impl AsyncRead for FailingReader {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::other("synthetic late reader failure")))
            }
        }
        let prefix = vec![b'x'; OUTPUT_LIMIT + 1];
        let error = read_output(prefix.as_slice().chain(FailingReader))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("synthetic late reader failure"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn s16_large_output_keeps_both_pipes_open_until_command_finishes() {
        let root = tempfile::tempdir().unwrap();
        let probe = Arc::new(Probe {
            case: "large-output",
            ..Probe::default()
        });
        let result = PROBE.scope(probe.clone(), execute(&task(root.path()), &json!({
            "command": "[Console]::Out.Write(('o' * 131072)); [Console]::Error.Write(('e' * 131072)); [IO.File]::WriteAllText((Join-Path $PWD 'finished'), 'done'); exit 0",
            "timeout_seconds": 10
        }))).await;
        // Observe the original process object before unwrapping the result.
        original_exit(&probe).unwrap();
        assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
        let result = result.unwrap();
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["stdout"], "o".repeat(OUTPUT_LIMIT));
        assert_eq!(result["stderr"], "e".repeat(OUTPUT_LIMIT));
        assert_eq!(
            std::fs::read_to_string(root.path().join("finished")).unwrap(),
            "done"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn shell_pwd_join_path_preserves_dotnet_paths_with_spaces() {
        let root = tempfile::Builder::new()
            .prefix("shell cwd with spaces ")
            .tempdir()
            .unwrap();
        let canonical = root.path().canonicalize().unwrap();
        let probe = Arc::new(Probe {
            case: "ordinary-shell-cwd",
            ..Probe::default()
        });
        let result = PROBE
            .scope(
                probe.clone(),
                execute(
                    &task(&canonical),
                    &json!({
                        "command": "$ErrorActionPreference = 'Stop'; [IO.File]::WriteAllText((Join-Path $PWD 'file with spaces.txt'), 'written'); [Console]::Out.Write($PWD.Path)",
                        "timeout_seconds": 10
                    }),
                ),
            )
            .await;
        original_exit(&probe).unwrap();
        let result = result.unwrap();
        assert_eq!(result["exit_code"], 0, "{result}");
        assert_eq!(result["stderr"], "");
        let pwd = result["stdout"].as_str().unwrap();
        assert!(!pwd.contains("::"), "provider-qualified cwd: {pwd}");
        assert!(!pwd.starts_with(r"\\?\"), "verbatim cwd: {pwd}");
        assert_eq!(std::path::Path::new(pwd).canonicalize().unwrap(), canonical);
        assert_eq!(
            std::fs::read_to_string(root.path().join("file with spaces.txt")).unwrap(),
            "written"
        );
    }

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
        fn DuplicateHandle(
            source_process: *mut std::ffi::c_void,
            source: *mut std::ffi::c_void,
            target_process: *mut std::ffi::c_void,
            target: *mut *mut std::ffi::c_void,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
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
    }
    #[cfg(windows)]
    impl Probe {
        pub(super) fn capture_supervised(&self, hooks: &crate::process_lifetime::TestHooks) {
            let captured = hooks
                .process_handle
                .lock()
                .unwrap()
                .take()
                .context("supervisor original process handle missing")
                .and_then(|handle| {
                    let expected = hooks
                        .process_identity
                        .lock()
                        .unwrap()
                        .context("creator process identity missing")?;
                    self.pid.store(expected.pid, Ordering::SeqCst);
                    let observation = ProcessObservation::from_handle(handle, expected.pid)?;
                    ensure!(
                        observation.created == expected.creation_filetime,
                        "original process creation identity mismatch"
                    );
                    Ok(observation)
                })
                .map_err(|error| format!("{error:#}"));
            self.parent_waited
                .store(hooks.root_signaled.load(Ordering::SeqCst), Ordering::SeqCst);
            if self.reader_error {
                assert!(
                    hooks.stdout_failed_joined.load(Ordering::SeqCst),
                    "reader failure branch missing"
                );
            }
            if self.cleanup_unknown {
                assert!(
                    hooks.stdout_reader_held.load(Ordering::SeqCst),
                    "cleanup obstruction branch missing"
                );
            }
            if self.wait_error {
                assert!(
                    hooks.exit_code_failed.load(Ordering::SeqCst),
                    "exit query failure branch missing"
                );
            }
            assert!(
                hooks.job_empty.load(Ordering::SeqCst),
                "original Job not empty"
            );
            assert!(
                hooks.stdout_reader_joined.load(Ordering::SeqCst),
                "original stdout reader not joined"
            );
            assert!(
                hooks.stderr_reader_joined.load(Ordering::SeqCst),
                "original stderr reader not joined"
            );
            assert!(hooks.stderr_eof_joined.load(Ordering::SeqCst));
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
    #[cfg(windows)]
    #[tokio::test]
    async fn s06_deadline_covers_inherited_pipe_after_direct_parent_exit() {
        let mut fixture = crate::process_test_fixture::prepare("s06-inherited-pipe")
            .await
            .expect("fixture preparation failed; retained path printed");
        let probe = Arc::new(Probe {
            case: "inherited-pipe",
            ..Probe::default()
        });
        let begin = Instant::now();
        let deadline = begin + Duration::from_secs(2);
        let result = collect(
            fixture.child.take().unwrap(),
            deadline,
            2,
            Some(probe.clone()),
            false,
        )
        .await;
        let elapsed = begin.elapsed();
        let parent_waited = probe.parent_waited.load(Ordering::SeqCst);
        eprintln!(
            "s06 case=inherited-pipe phase=collector result={} elapsed_ms={} parent_waited={} readers={} fixture={}",
            if result.is_ok() {
                "success"
            } else if result
                .as_ref()
                .err()
                .is_some_and(|e| e.is::<CommandOutcomeUnknown>())
            {
                "unknown"
            } else {
                "timeout"
            },
            elapsed.as_millis(),
            parent_waited,
            probe.readers.load(Ordering::SeqCst),
            fixture.root.display()
        );
        let category = if result.is_ok() {
            "success"
        } else if result
            .as_ref()
            .err()
            .is_some_and(|e| e.is::<CommandOutcomeUnknown>())
        {
            "unknown"
        } else {
            "timeout"
        };
        std::fs::write(fixture.root.join("collector.txt"), format!("case=s06-inherited-pipe result={category} elapsed_ms={} parent_waited={} readers={}\n", elapsed.as_millis(), parent_waited, probe.readers.load(Ordering::SeqCst))).expect("persist collector evidence");
        let cleanup = fixture.cleanup("s06-inherited-pipe");
        cleanup.expect("fixture cleanup");
        let error = result.expect_err("inherited pipe must reach deadline");
        assert!(!error.is::<CommandOutcomeUnknown>(), "{error:#}");
        assert!(error.to_string().contains("超过 2 秒"));
        assert!(parent_waited);
        assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
        assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn s06_execute_two_second_deadline_starts_before_spawn() {
        let root = tempfile::tempdir().unwrap();
        let task = task(root.path());
        let probe = Arc::new(Probe {
            case: "execute-two-second",
            ..Probe::default()
        });
        let begin = Instant::now();
        let result = PROBE
            .scope(
                probe.clone(),
                execute(
                    &task,
                    &json!({"command":"Start-Sleep -Seconds 20", "timeout_seconds":2}),
                ),
            )
            .await;
        let elapsed = begin.elapsed();
        let exited = original_exit(&probe);
        let error = result.expect_err("long-running command must time out");
        assert!(!error.is::<CommandOutcomeUnknown>(), "{error:#}");
        assert!(error.to_string().contains("超过 2 秒"));
        assert!(elapsed < Duration::from_secs(7));
        assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
        exited.unwrap();
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
