//! Async subprocess supervision with Windows JobObject process-tree containment.

use std::{
    ffi::OsString,
    num::NonZeroUsize,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

pub(crate) trait DomainLease: Send + Sync + 'static {
    #[allow(dead_code)]
    fn seal_unknown(&mut self);
    fn settle(self: Box<Self>);
}

struct NoopLease;
impl DomainLease for NoopLease {
    fn seal_unknown(&mut self) {}
    fn settle(self: Box<Self>) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverflowPolicy {
    TruncateAndDrain,
    Terminate,
}

pub(crate) struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
    pub overflow: OverflowPolicy,
    pub deadline: Instant,
    pub cleanup_budget: Duration,
}

impl ProcessSpec {
    pub(crate) fn new(
        program: PathBuf,
        cwd: PathBuf,
        environment: Vec<(OsString, OsString)>,
        limits: (usize, usize),
        overflow: OverflowPolicy,
        deadline: Instant,
        cleanup_budget: Duration,
    ) -> Self {
        Self {
            program,
            args: Vec::new(),
            cwd,
            environment,
            stdout_limit: limits.0,
            stderr_limit: limits.1,
            overflow,
            deadline,
            cleanup_budget,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Capture {
    pub bytes: Vec<u8>,
    #[allow(dead_code)]
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionReason {
    Exited,
    Deadline,
    Cancelled,
    OutputLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub pid: u32,
    pub creation_filetime: u64,
}

#[derive(Debug)]
pub(crate) struct Settled {
    pub reason: CompletionReason,
    #[allow(dead_code)]
    pub identity: ProcessIdentity,
    pub exit_code: u32,
    pub stdout: Capture,
    pub stderr: Capture,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    NotStarted {
        #[allow(dead_code)]
        stage: &'static str,
        #[allow(dead_code)]
        os_error: Option<i32>,
    },
    Settled(Settled),
    #[allow(dead_code)]
    Unknown,
}

pub(crate) struct NotStarted {
    pub reason: &'static str,
    pub lease: Box<dyn DomainLease>,
}

struct Capacity {
    limit: NonZeroUsize,
    in_flight: usize,
}

struct SlotGuard {
    capacity: Arc<Mutex<Capacity>>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut state = self.capacity.lock().unwrap_or_else(|e| e.into_inner());
        state.in_flight = state.in_flight.saturating_sub(1);
    }
}

pub(crate) struct Supervisor {
    capacity: Arc<Mutex<Capacity>>,
}

impl Supervisor {
    pub(crate) fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity: Arc::new(Mutex::new(Capacity {
                limit: capacity,
                in_flight: 0,
            })),
        }
    }

    pub(crate) fn resize(&self, capacity: NonZeroUsize) {
        let mut state = self.capacity.lock().unwrap_or_else(|e| e.into_inner());
        state.limit = capacity;
    }

    #[allow(dead_code)]
    pub(crate) fn active_count(&self) -> usize {
        self.capacity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .in_flight
    }

    fn reserve(&self) -> Option<SlotGuard> {
        let mut state = self.capacity.lock().unwrap_or_else(|e| e.into_inner());
        if state.in_flight >= state.limit.get() {
            return None;
        }
        state.in_flight += 1;
        Some(SlotGuard {
            capacity: Arc::clone(&self.capacity),
        })
    }

    pub(crate) fn submit(
        &self,
        spec: ProcessSpec,
        lease: Box<dyn DomainLease>,
    ) -> Result<Operation, NotStarted> {
        if spec.cleanup_budget.is_zero() || spec.cleanup_budget > Duration::from_secs(5) {
            return Err(NotStarted {
                reason: "cleanup budget must be nonzero and at most five seconds",
                lease,
            });
        }
        if spec.deadline <= Instant::now() {
            return Err(NotStarted {
                reason: "deadline expired before submission",
                lease,
            });
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return Err(NotStarted {
                reason: "tokio runtime required",
                lease,
            });
        };
        let Some(slot) = self.reserve() else {
            return Err(NotStarted {
                reason: "process supervisor capacity exhausted",
                lease,
            });
        };
        let cancel_token = CancellationToken::new();
        let worker_cancel = cancel_token.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        handle.spawn(async move {
            let _slot = slot;
            let outcome = execute_process(spec, worker_cancel).await;
            lease.settle();
            let _ = tx.send(outcome);
        });
        Ok(Operation {
            rx,
            cancel_token,
            done: false,
        })
    }
}

pub(crate) struct Operation {
    rx: tokio::sync::oneshot::Receiver<Outcome>,
    cancel_token: CancellationToken,
    done: bool,
}

impl Operation {
    pub(crate) async fn wait_with_cancel(mut self, cancellation: &CancellationToken) -> Outcome {
        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                self.cancel_token.cancel();
                (&mut self.rx).await.unwrap_or(Outcome::NotStarted {
                    stage: "cancelled",
                    os_error: None,
                })
            }
            res = &mut self.rx => res.unwrap_or(Outcome::NotStarted {
                stage: "channel_closed",
                os_error: None,
            }),
        };
        self.done = true;
        outcome
    }

    pub(crate) async fn wait(mut self) -> Outcome {
        let outcome = (&mut self.rx).await.unwrap_or(Outcome::NotStarted {
            stage: "channel_closed",
            os_error: None,
        });
        self.done = true;
        outcome
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.done {
            self.cancel_token.cancel();
        }
    }
}

pub(crate) async fn run_direct(spec: ProcessSpec) -> Outcome {
    if spec.deadline <= Instant::now() {
        return Outcome::NotStarted {
            stage: "deadline",
            os_error: None,
        };
    }
    let supervisor = Supervisor::new(NonZeroUsize::new(1).unwrap());
    match supervisor.submit(spec, Box::new(NoopLease)) {
        Ok(op) => op.wait().await,
        Err(_) => Outcome::NotStarted {
            stage: "submit",
            os_error: None,
        },
    }
}

async fn read_pipe<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
    overflow: OverflowPolicy,
    overflow_signal: CancellationToken,
) -> Capture {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let remaining = limit.saturating_sub(bytes.len());
                if n <= remaining {
                    bytes.extend_from_slice(&buf[..n]);
                } else {
                    if remaining > 0 {
                        bytes.extend_from_slice(&buf[..remaining]);
                    }
                    truncated = true;
                    if overflow == OverflowPolicy::Terminate {
                        overflow_signal.cancel();
                        break;
                    }
                }
            }
        }
    }
    Capture { bytes, truncated }
}

async fn execute_process(spec: ProcessSpec, cancel: CancellationToken) -> Outcome {
    #[cfg(windows)]
    let job = match win_job::JobGuard::create() {
        Ok(job) => Some(job),
        Err(err) => {
            return Outcome::NotStarted {
                stage: "create_job",
                os_error: Some(err),
            };
        }
    };

    let mut cmd = tokio::process::Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.environment.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return Outcome::NotStarted {
                stage: "spawn",
                os_error: err.raw_os_error(),
            };
        }
    };

    let pid = child.id().unwrap_or(0);
    #[cfg(windows)]
    let creation_filetime = win_job::creation_time(&child);
    #[cfg(not(windows))]
    let creation_filetime = 0u64;

    #[cfg(windows)]
    if let Some(ref job_guard) = job
        && let Err(err) = job_guard.assign(&mut child)
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Outcome::NotStarted {
            stage: "assign_job",
            os_error: Some(err),
        };
    }

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let overflow_signal = CancellationToken::new();

    let out_signal = overflow_signal.clone();
    let stdout_limit = spec.stdout_limit;
    let overflow_policy = spec.overflow;
    let stdout_task = tokio::spawn(async move {
        match stdout_pipe {
            Some(pipe) => read_pipe(pipe, stdout_limit, overflow_policy, out_signal).await,
            None => Capture {
                bytes: Vec::new(),
                truncated: false,
            },
        }
    });

    let err_signal = overflow_signal.clone();
    let stderr_limit = spec.stderr_limit;
    let stderr_task = tokio::spawn(async move {
        match stderr_pipe {
            Some(pipe) => read_pipe(pipe, stderr_limit, overflow_policy, err_signal).await,
            None => Capture {
                bytes: Vec::new(),
                truncated: false,
            },
        }
    });

    let deadline_instant = tokio::time::Instant::from_std(spec.deadline);
    let (mut reason, mut exit_code) = tokio::select! {
        biased;
        _ = cancel.cancelled() => (CompletionReason::Cancelled, 1u32),
        _ = overflow_signal.cancelled() => (CompletionReason::OutputLimit, 1u32),
        _ = tokio::time::sleep_until(deadline_instant) => (CompletionReason::Deadline, 1u32),
        status = child.wait() => match status {
            Ok(status) => (CompletionReason::Exited, status.code().unwrap_or(1) as u32),
            Err(_) => (CompletionReason::Exited, 1u32),
        },
    };

    if reason != CompletionReason::Exited {
        #[cfg(windows)]
        drop(job);
        let _ = child.start_kill();
        if let Ok(Ok(status)) = tokio::time::timeout(spec.cleanup_budget, child.wait()).await
            && let Some(code) = status.code()
        {
            exit_code = code as u32;
        }
    } else {
        // Ensure pipes finish before deadline; if a descendant kept pipes open past deadline,
        // dropping the JobObject kills the remaining process tree cleanly.
        let remaining = spec.deadline.saturating_duration_since(Instant::now());
        let pipe_wait = remaining.max(Duration::from_millis(50));
        if !stdout_task.is_finished() || !stderr_task.is_finished() {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    reason = CompletionReason::Cancelled;
                }
                _ = overflow_signal.cancelled() => {
                    reason = CompletionReason::OutputLimit;
                }
                _ = tokio::time::sleep(pipe_wait) => {
                    if !stdout_task.is_finished() || !stderr_task.is_finished() {
                        reason = CompletionReason::Deadline;
                    }
                }
                _ = async {
                    while !stdout_task.is_finished() || !stderr_task.is_finished() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                } => {}
            }
        }
        #[cfg(windows)]
        drop(job);
    }

    let stdout = match tokio::time::timeout(spec.cleanup_budget, stdout_task).await {
        Ok(Ok(capture)) => capture,
        _ => Capture {
            bytes: Vec::new(),
            truncated: true,
        },
    };
    let stderr = match tokio::time::timeout(spec.cleanup_budget, stderr_task).await {
        Ok(Ok(capture)) => capture,
        _ => Capture {
            bytes: Vec::new(),
            truncated: true,
        },
    };

    Outcome::Settled(Settled {
        reason,
        identity: ProcessIdentity {
            pid,
            creation_filetime,
        },
        exit_code,
        stdout,
        stderr,
    })
}

#[cfg(windows)]
mod win_job {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Threading::GetProcessTimes,
        },
    };

    pub(super) struct JobGuard(HANDLE);
    unsafe impl Send for JobGuard {}
    unsafe impl Sync for JobGuard {}

    impl JobGuard {
        pub(super) fn create() -> Result<Self, i32> {
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(1));
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(1);
                unsafe {
                    CloseHandle(handle);
                }
                return Err(err);
            }
            Ok(Self(handle))
        }

        pub(super) fn assign(&self, child: &mut tokio::process::Child) -> Result<(), i32> {
            let Some(raw) = child.raw_handle() else {
                return Ok(());
            };
            let ok = unsafe { AssignProcessToJobObject(self.0, raw.cast()) };
            if ok != 0 {
                return Ok(());
            }
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(1);
            if child.try_wait().ok().flatten().is_some() {
                return Ok(());
            }
            Err(err)
        }
    }

    impl Drop for JobGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = TerminateJobObject(self.0, 1);
                CloseHandle(self.0);
            }
        }
    }

    pub(super) fn creation_time(child: &tokio::process::Child) -> u64 {
        let Some(raw) = child.raw_handle() else {
            return 0;
        };
        let mut created: FILETIME = unsafe { std::mem::zeroed() };
        let mut exited: FILETIME = unsafe { std::mem::zeroed() };
        let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
        let mut user: FILETIME = unsafe { std::mem::zeroed() };
        if unsafe {
            GetProcessTimes(
                raw.cast(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return 0;
        }
        ((created.dwHighDateTime as u64) << 32) | (created.dwLowDateTime as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_and_kills_timed_out_process_cleanly() {
        let cwd = std::env::current_dir().unwrap();
        let cmd_exe = if cfg!(windows) {
            PathBuf::from(r"C:\Windows\System32\cmd.exe")
        } else {
            PathBuf::from("/bin/sh")
        };
        if !cmd_exe.is_file() {
            return;
        }
        let env: Vec<_> = std::env::vars_os().collect();
        let mut spec = ProcessSpec::new(
            cmd_exe.clone(),
            cwd.clone(),
            env.clone(),
            (4096, 4096),
            OverflowPolicy::TruncateAndDrain,
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(2),
        );
        spec.args = if cfg!(windows) {
            vec!["/C".into(), "echo hello".into()]
        } else {
            vec!["-c".into(), "echo hello".into()]
        };
        match run_direct(spec).await {
            Outcome::Settled(settled) => {
                assert_eq!(settled.reason, CompletionReason::Exited);
                assert_eq!(settled.exit_code, 0);
                assert!(String::from_utf8_lossy(&settled.stdout.bytes).contains("hello"));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }

        let mut slow = ProcessSpec::new(
            cmd_exe,
            cwd,
            env,
            (4096, 4096),
            OverflowPolicy::TruncateAndDrain,
            Instant::now() + Duration::from_millis(120),
            Duration::from_secs(2),
        );
        slow.args = if cfg!(windows) {
            vec!["/C".into(), "ping -n 10 127.0.0.1 >NUL".into()]
        } else {
            vec!["-c".into(), "sleep 10".into()]
        };
        match run_direct(slow).await {
            Outcome::Settled(settled) => {
                assert_eq!(settled.reason, CompletionReason::Deadline);
            }
            other => panic!("timeout must settle cleanly via JobObject, got {other:?}"),
        }
    }
}
