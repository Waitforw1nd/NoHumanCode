//! Supervision for the fixed, trusted WASM policy worker.
//!
//! This is a lifecycle boundary, not an OS sandbox for native code. The worker
//! and the no-import WASM interpreter remain part of the trusted computing base.
#[cfg(windows)]
use crate::wasm_worker_protocol::Outcome;
use crate::wasm_worker_protocol::{self as protocol, Decision, Input, Manifest, Request};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::path::Path;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const CALL_LIMIT: Duration = Duration::from_secs(5);
const REAP_LIMIT: Duration = Duration::from_secs(2);
static SLOT: AtomicBool = AtomicBool::new(false);

#[cfg(all(test, windows))]
static STAGE_LOG_LOCK: Mutex<()> = Mutex::new(());

#[cfg(all(test, windows))]
fn record_stage(phase: &'static str, pid: Option<u32>) {
    use std::io::Write;
    let _guard = STAGE_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(path) = std::env::var_os("NHC_W2_STAGE_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "phase={phase} pid={pid:?}");
    }
}
#[cfg(not(all(test, windows)))]
fn record_stage(_phase: &'static str, _pid: Option<u32>) {}

#[cfg(all(test, windows))]
fn record_count(phase: &'static str, pid: u32, count: usize) {
    use std::io::Write;
    let _guard = STAGE_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(path) = std::env::var_os("NHC_W2_STAGE_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "phase={phase} pid=Some({pid}) count={count}");
    }
}
#[cfg(not(all(test, windows)))]
fn record_count(_phase: &'static str, _pid: u32, _count: usize) {}

#[cfg(all(test, windows))]
pub(crate) mod test_observer {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    static PROCESS: Mutex<Option<(u32, OwnedHandle)>> = Mutex::new(None);
    static ARMED: AtomicBool = AtomicBool::new(false);
    static PRESSURE: AtomicBool = AtomicBool::new(false);
    static IO_EVENT: Mutex<Option<OwnedHandle>> = Mutex::new(None);

    pub(crate) fn arm(pressure: bool) {
        assert!(PROCESS.lock().unwrap().is_none());
        PRESSURE.store(pressure, Ordering::SeqCst);
        assert!(!ARMED.swap(true, Ordering::SeqCst));
    }

    pub(super) fn take_pressure() -> bool {
        PRESSURE.swap(false, Ordering::SeqCst)
    }

    pub(crate) fn capture(pid: u32) -> Result<(), super::WorkerError> {
        if !ARMED.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return Err(super::WorkerError::IsolationUnavailable(
                "test_observer_open",
                std::io::Error::last_os_error().raw_os_error(),
            ));
        }
        let owned = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut slot = PROCESS.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err(super::WorkerError::IsolationUnavailable(
                "test_observer_busy",
                None,
            ));
        }
        *slot = Some((pid, owned));
        Ok(())
    }

    pub(crate) fn take() -> Option<(u32, OwnedHandle)> {
        PROCESS.lock().unwrap().take()
    }
    pub(super) fn capture_event(event: &OwnedHandle) -> Result<(), super::WorkerError> {
        use windows_sys::Win32::Foundation::DuplicateHandle;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let current = unsafe { GetCurrentProcess() };
        let mut duplicate = std::ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                current,
                event.as_raw_handle(),
                current,
                &mut duplicate,
                0x0010_0000,
                0,
                0,
            )
        } == 0
        {
            return Err(super::WorkerError::IsolationUnavailable(
                "test_event_duplicate",
                std::io::Error::last_os_error().raw_os_error(),
            ));
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(duplicate) };
        let mut slot = IO_EVENT.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err(super::WorkerError::IsolationUnavailable(
                "test_event_busy",
                None,
            ));
        }
        *slot = Some(handle);
        Ok(())
    }
    pub(crate) fn take_event() -> Option<OwnedHandle> {
        IO_EVENT.lock().unwrap().take()
    }
}

#[cfg(all(test, windows))]
pub(crate) mod test_hooks {
    use std::sync::{
        Condvar, Mutex,
        atomic::{AtomicU8, Ordering},
    };
    use std::time::{Duration, Instant};

    struct State {
        phase: Option<&'static str>,
        reached: bool,
        released: bool,
        timed_out: bool,
    }
    static STATE: Mutex<State> = Mutex::new(State {
        phase: None,
        reached: false,
        released: false,
        timed_out: false,
    });
    static WAKE: Condvar = Condvar::new();
    static ZERO: AtomicU8 = AtomicU8::new(0);
    static MEMBERSHIP: AtomicU8 = AtomicU8::new(0);
    static JOB_EVENTS: AtomicU8 = AtomicU8::new(0);
    static PERSISTENT_ERROR: AtomicU8 = AtomicU8::new(0);

    pub(crate) struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            release();
        }
    }
    pub(crate) struct PersistentGuard;
    impl Drop for PersistentGuard {
        fn drop(&mut self) {
            release_persistent_error();
        }
    }
    pub(crate) fn arm_persistent_error(result_error: bool) -> PersistentGuard {
        assert_eq!(
            PERSISTENT_ERROR.swap(if result_error { 3 } else { 1 }, Ordering::SeqCst),
            0
        );
        PersistentGuard
    }
    pub(crate) fn persistent_reached() -> bool {
        matches!(PERSISTENT_ERROR.load(Ordering::SeqCst), 2 | 4)
    }
    pub(crate) fn release_persistent_error() {
        PERSISTENT_ERROR.store(0, Ordering::SeqCst);
    }
    pub(super) fn persistent_event_active() -> bool {
        PERSISTENT_ERROR.load(Ordering::SeqCst) == 2
    }
    pub(super) fn start_persistent_event() -> bool {
        PERSISTENT_ERROR
            .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
    pub(super) fn persistent_result_active() -> bool {
        PERSISTENT_ERROR.load(Ordering::SeqCst) == 4
    }
    pub(super) fn start_persistent_result() -> bool {
        PERSISTENT_ERROR
            .compare_exchange(3, 4, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
    pub(crate) fn arm(phase: &'static str) -> Guard {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        assert!(state.phase.is_none());
        *state = State {
            phase: Some(phase),
            reached: false,
            released: false,
            timed_out: false,
        };
        Guard
    }
    pub(crate) fn reached() -> bool {
        STATE.lock().unwrap_or_else(|e| e.into_inner()).reached
    }
    pub(super) fn armed(phase: &'static str) -> bool {
        STATE.lock().unwrap_or_else(|e| e.into_inner()).phase == Some(phase)
    }
    pub(super) fn hold_stopper() {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != Some("observation_injected") {
            return;
        }
        super::record_stage("injected_stopper_held", None);
        let deadline = Instant::now() + Duration::from_secs(15);
        while !state.released {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.timed_out = true;
                super::record_stage("injection_timeout", None);
                break;
            }
            state = WAKE
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    pub(crate) fn completed_without_timeout() -> bool {
        let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.reached && state.released && !state.timed_out && state.phase.is_none()
    }
    pub(crate) fn slot_free() -> bool {
        !super::SLOT.load(Ordering::Acquire)
    }
    pub(crate) fn mark(phase: &'static str) {
        super::record_stage(phase, None);
    }
    pub(crate) fn release() {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.released = true;
        WAKE.notify_all();
    }
    pub(super) fn checkpoint(phase: &'static str, pid: Option<u32>) -> bool {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != Some(phase) {
            return false;
        }
        state.reached = true;
        super::record_stage(phase, pid);
        WAKE.notify_all();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !state.released {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let result = WAKE
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner());
            state = result.0;
        }
        let released = state.released;
        state.timed_out |= !released;
        state.phase = None;
        drop(state);
        super::record_stage(
            if released {
                "injection_released"
            } else {
                "injection_timeout"
            },
            pid,
        );
        true
    }
    pub(crate) fn arm_zero(stdout: bool) {
        ZERO.store(if stdout { 1 } else { 2 }, Ordering::SeqCst);
    }
    pub(super) fn inject_zero(stdout: bool) -> bool {
        let expected = if stdout { 1 } else { 2 };
        ZERO.compare_exchange(expected, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
    pub(crate) fn arm_membership_failure() {
        MEMBERSHIP.store(1, Ordering::SeqCst);
    }
    pub(super) fn inject_membership_failure() -> bool {
        MEMBERSHIP.swap(0, Ordering::SeqCst) == 1
    }
    pub(crate) fn arm_job_events() {
        JOB_EVENTS.store(1, Ordering::SeqCst);
    }
    pub(super) fn take_job_events() -> bool {
        JOB_EVENTS.swap(0, Ordering::SeqCst) == 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerError {
    Busy,
    Unsupported,
    InvalidRequest,
    Cancelled,
    Deadline,
    CleanupUnknown,
    IsolationUnavailable(&'static str, Option<i32>),
    Protocol,
    WorkerRejected,
}
#[derive(Debug, Clone)]
pub struct WorkerSuccess {
    pub decision: Decision,
    pub process_id: u32,
    pub exit_code: u32,
    pub process_signaled: bool,
    pub job_active_processes: u32,
    pub io_completed: bool,
}

#[derive(Clone)]
pub(crate) struct TrustedWorker {
    path: PathBuf,
    digest: [u8; 32],
}

impl TrustedWorker {
    /// The caller is the trusted installation boundary, never a plugin request.
    #[allow(dead_code)] // The authenticated installation boundary is connected later.
    pub(crate) fn from_host_install(path: PathBuf, digest: [u8; 32]) -> Self {
        Self { path, digest }
    }
}

#[derive(Clone)]
pub struct WorkerService {
    worker: Arc<TrustedWorker>,
}

impl WorkerService {
    #[allow(dead_code)] // No product entry point is wired in this component slice.
    pub(crate) fn from_trusted(worker: TrustedWorker) -> Self {
        Self {
            worker: Arc::new(worker),
        }
    }

    /// Execute one policy request using the fixed worker; no executable is
    /// selected by the caller. A process-wide slot prevents rebuilding the
    /// service from bypassing unresolved cleanup.
    pub async fn call(
        &self,
        mut manifest: Manifest,
        input: Input,
        module: Vec<u8>,
        cancel: CancellationToken,
    ) -> Result<WorkerSuccess, WorkerError> {
        let deadline = Instant::now() + CALL_LIMIT;
        if cancel.is_cancelled() {
            return Err(WorkerError::Cancelled);
        }
        if SLOT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(WorkerError::Busy);
        }
        let lease = SlotLease;
        if module.is_empty() || module.len() > protocol::MAX_MODULE {
            return Err(WorkerError::InvalidRequest);
        }
        manifest.request_id = uuid::Uuid::new_v4().simple().to_string();
        manifest.nonce = uuid::Uuid::new_v4().simple().to_string();
        manifest.digest = format!("{:x}", Sha256::digest(&module));
        manifest
            .validate()
            .map_err(|_| WorkerError::InvalidRequest)?;
        input.validate().map_err(|_| WorkerError::InvalidRequest)?;
        if Instant::now() >= deadline {
            return Err(WorkerError::Deadline);
        }
        if cancel.is_cancelled() {
            return Err(WorkerError::Cancelled);
        }
        let request = Request {
            manifest,
            input,
            module,
        };
        let gate = Arc::new(Gate::new(cancel.clone()));
        let mut cancellation = CancelOnDrop(Some(gate.clone()));
        let (tx, mut rx) = oneshot::channel();
        let worker = self.worker.clone();
        let gate_for_thread = gate.clone();
        std::thread::Builder::new()
            .name("nhc-wasm-worker-supervision".into())
            .spawn(move || {
                let result = platform::run(&worker, request, deadline, &gate_for_thread);
                // The slot is released only when the platform has returned after
                // confirmed cleanup. Pending kernel calls retain this thread.
                drop(lease);
                let _ = tx.send(result);
            })
            .map_err(|_| WorkerError::IsolationUnavailable("supervisor_thread", None))?;

        let mut received_completion = false;
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(WorkerError::Cancelled),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(WorkerError::Deadline),
            received = &mut rx => {
                received_completion = true;
                match received {
                    Ok(Ok(success)) => {
                        #[cfg(all(test, windows))]
                        test_hooks::checkpoint("accept_before", Some(success.process_id));
                        if gate.accept(deadline) {
                            #[cfg(all(test, windows))]
                            test_hooks::checkpoint("accept_after", Some(success.process_id));
                            Ok(success)
                        } else if cancel.is_cancelled() {
                            Err(WorkerError::Cancelled)
                        } else {
                            Err(WorkerError::Deadline)
                        }
                    },
                    Ok(_) if cancel.is_cancelled() => Err(WorkerError::Cancelled),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(WorkerError::CleanupUnknown),
                }
            },
        };
        if matches!(result, Err(WorkerError::Cancelled | WorkerError::Deadline)) {
            gate.stop();
            if !finish_cleanup_wait(&mut rx, received_completion).await {
                record_stage("cleanup_unknown", None);
                cancellation.0 = None;
                return Err(WorkerError::CleanupUnknown);
            }
        }
        cancellation.0 = None;
        result
    }
}

async fn finish_cleanup_wait(
    receiver: &mut oneshot::Receiver<Result<WorkerSuccess, WorkerError>>,
    received_completion: bool,
) -> bool {
    if received_completion {
        return true;
    }
    match tokio::time::timeout(REAP_LIMIT, receiver).await {
        Ok(Ok(Err(WorkerError::CleanupUnknown))) | Ok(Err(_)) | Err(_) => false,
        Ok(Ok(_)) => true,
    }
}

struct SlotLease;
impl Drop for SlotLease {
    fn drop(&mut self) {
        SLOT.store(false, Ordering::Release);
    }
}

struct Gate {
    state: AtomicU8,
    admission: Mutex<()>,
    cancel: CancellationToken,
}
impl Gate {
    fn new(cancel: CancellationToken) -> Self {
        Self {
            state: AtomicU8::new(0),
            admission: Mutex::new(()),
            cancel,
        }
    }
    fn stop(&self) {
        let _guard = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }
    #[cfg(windows)]
    fn stopped(&self, deadline: Instant) -> bool {
        self.cancel.is_cancelled()
            || self.state.load(Ordering::Acquire) == 1
            || Instant::now() >= deadline
    }
    fn accept(&self, deadline: Instant) -> bool {
        let _guard = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        if self.cancel.is_cancelled() || Instant::now() >= deadline {
            let _ = self
                .state
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
            return false;
        }
        self.state
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    #[cfg(windows)]
    fn authorize(&self, deadline: Instant) -> bool {
        let _guard = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        if self.cancel.is_cancelled() || Instant::now() >= deadline {
            let _ = self
                .state
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
            return false;
        }
        self.state.load(Ordering::Acquire) == 0
    }
}
struct CancelOnDrop(Option<Arc<Gate>>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(gate) = self.0.take() {
            gate.stop();
        }
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use std::sync::Barrier;

    #[tokio::test]
    async fn already_received_deadline_does_not_poll_receiver_again() {
        let (sender, mut receiver) = oneshot::channel();
        sender.send(Err(WorkerError::Deadline)).unwrap();
        assert!(matches!(
            (&mut receiver).await,
            Ok(Err(WorkerError::Deadline))
        ));
        assert!(finish_cleanup_wait(&mut receiver, true).await);
    }

    #[test]
    fn cancel_and_accept_have_one_winner() {
        for _ in 0..256 {
            let gate = Arc::new(Gate::new(CancellationToken::new()));
            let barrier = Arc::new(Barrier::new(2));
            let other_gate = gate.clone();
            let other_barrier = barrier.clone();
            let thread = std::thread::spawn(move || {
                other_barrier.wait();
                other_gate.stop();
            });
            barrier.wait();
            let accepted = gate.accept(Instant::now() + Duration::from_secs(1));
            thread.join().unwrap();
            assert_eq!(
                gate.state.load(Ordering::Acquire),
                if accepted { 2 } else { 1 }
            );
            assert!(!gate.accept(Instant::now() + Duration::from_secs(1)));
        }
    }

    #[cfg(windows)]
    #[test]
    fn background_gate_observes_cancel_without_caller_poll() {
        let cancel = CancellationToken::new();
        let gate = Gate::new(cancel.clone());
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(gate.authorize(deadline));
        cancel.cancel();
        assert!(gate.stopped(deadline));
        assert!(!gate.authorize(deadline));
        assert!(!gate.accept(deadline));
        assert_eq!(gate.state.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn already_cancelled_never_starts_supervisor() {
        let service =
            WorkerService::from_trusted(TrustedWorker::from_host_install(PathBuf::new(), [0; 32]));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = service
            .call(
                Manifest {
                    version: 1,
                    request_id: "unused".into(),
                    nonce: "0".repeat(32),
                    binding_generation: 0,
                    plugin_id: "test".into(),
                    plugin_version: "1.0.0".into(),
                    digest: "0".repeat(64),
                    fuel: 1,
                    memory_pages: 1,
                },
                Input {
                    round: 0,
                    max_rounds: 1,
                    decision: protocol::Phase::AfterNoTools {},
                },
                vec![0],
                cancel,
            )
            .await;
        assert!(matches!(result, Err(WorkerError::Cancelled)));
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    pub(super) fn run(
        worker: &TrustedWorker,
        _request: Request,
        _deadline: Instant,
        _gate: &Arc<Gate>,
    ) -> Result<WorkerSuccess, WorkerError> {
        let _ = (&worker.path, &worker.digest);
        Err(WorkerError::Unsupported)
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        ffi::{OsStr, OsString, c_void},
        fs::{File, OpenOptions},
        io::{Read, Seek, SeekFrom},
        mem::{size_of, zeroed},
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt},
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::{Component, PathBuf},
        ptr::{null, null_mut},
        thread,
    };
    use windows_sys::Win32::{
        Foundation::{
            ERROR_BROKEN_PIPE, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_NO_DATA,
            ERROR_OPERATION_ABORTED, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ,
            GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, LocalFree, SetHandleInformation,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        Security::{GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED,
            FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING,
            PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, ReadFile, WriteFile,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            JobObjects::{
                CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
                JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,
                JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                SetInformationJobObject, TerminateJobObject,
            },
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
            },
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW,
                CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
                GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess,
                InitializeProcThreadAttributeList, OpenProcessToken,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
                PROCESS_INFORMATION, QueryFullProcessImageNameW, ResetEvent, ResumeThread,
                STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
                WaitForSingleObject,
            },
        },
    };

    const CHUNK: usize = 16 * 1024;
    const MAX_STDERR: usize = 64 * 1024;

    #[cfg(test)]
    fn pipe_pressure(pipe: &OwnedHandle) -> Result<(u32, u32), WorkerError> {
        use windows_sys::Win32::System::Pipes::{GetNamedPipeInfo, PeekNamedPipe};
        let mut capacity = 0;
        let mut available = 0;
        if unsafe { GetNamedPipeInfo(raw(pipe), null_mut(), null_mut(), &mut capacity, null_mut()) }
            == 0
        {
            return Err(failure("test_pipe_info"));
        }
        if unsafe {
            PeekNamedPipe(
                raw(pipe),
                null_mut(),
                0,
                null_mut(),
                &mut available,
                null_mut(),
            )
        } == 0
        {
            return Err(failure("test_pipe_peek"));
        }
        Ok((capacity, available))
    }

    fn failure(stage: &'static str) -> WorkerError {
        WorkerError::IsolationUnavailable(stage, std::io::Error::last_os_error().raw_os_error())
    }
    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }
    fn raw(h: &OwnedHandle) -> HANDLE {
        h.as_raw_handle()
    }
    unsafe fn own(h: HANDLE, stage: &'static str) -> Result<OwnedHandle, WorkerError> {
        if h.is_null() || h as isize == -1 {
            Err(failure(stage))
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(h) })
        }
    }
    fn ensure_open(gate: &Gate, deadline: Instant) -> Result<(), WorkerError> {
        if gate.stopped(deadline) {
            Err(WorkerError::Deadline)
        } else {
            Ok(())
        }
    }
    fn known_terminal_error(code: Option<i32>) -> bool {
        matches!(
            code,
            Some(code)
                if code == ERROR_OPERATION_ABORTED as i32
                    || code == ERROR_BROKEN_PIPE as i32
                    || code == ERROR_NO_DATA as i32
                    || code == ERROR_PIPE_NOT_CONNECTED as i32
        )
    }

    struct Image {
        file: File,
        _parents: Vec<File>,
        path: PathBuf,
        identity: BY_HANDLE_FILE_INFORMATION,
    }
    impl Image {
        fn pin(path: &Path, digest: &[u8; 32]) -> Result<Self, WorkerError> {
            if !path.is_absolute() {
                return Err(WorkerError::IsolationUnavailable("image_absolute", None));
            }
            let parent = path
                .parent()
                .ok_or(WorkerError::IsolationUnavailable("image_parent", None))?;
            let mut current = PathBuf::new();
            let mut parents = Vec::new();
            for part in parent.components() {
                match part {
                    Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                        current.push(part.as_os_str());
                    }
                    _ => return Err(WorkerError::IsolationUnavailable("image_path", None)),
                }
                if matches!(part, Component::Prefix(_)) {
                    continue;
                }
                let directory = OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(&current)
                    .map_err(|_| failure("image_parent_open"))?;
                if directory
                    .metadata()
                    .map_err(|_| failure("image_parent_metadata"))?
                    .file_attributes()
                    & FILE_ATTRIBUTE_REPARSE_POINT
                    != 0
                {
                    return Err(WorkerError::IsolationUnavailable(
                        "image_parent_reparse",
                        None,
                    ));
                }
                parents.push(directory);
            }
            let mut file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
                .map_err(|_| failure("image_open"))?;
            let mut identity: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut identity) } == 0 {
                return Err(failure("image_identity"));
            }
            if identity.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || identity.nNumberOfLinks != 1
            {
                return Err(WorkerError::IsolationUnavailable("image_link", None));
            }
            let mut hash = Sha256::new();
            let mut buf = [0u8; CHUNK];
            loop {
                let n = file.read(&mut buf).map_err(|_| failure("image_read"))?;
                if n == 0 {
                    break;
                }
                hash.update(&buf[..n]);
            }
            let actual: [u8; 32] = hash.finalize().into();
            if &actual != digest {
                return Err(WorkerError::IsolationUnavailable("image_digest", None));
            }
            file.seek(SeekFrom::Start(0))
                .map_err(|_| failure("image_seek"))?;
            Ok(Self {
                file,
                _parents: parents,
                path: path.to_owned(),
                identity,
            })
        }
        fn verify_process(&self, process: HANDLE, digest: &[u8; 32]) -> Result<(), WorkerError> {
            let mut name = vec![0u16; 32768];
            let mut size = name.len() as u32;
            if unsafe { QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut size) } == 0
            {
                return Err(failure("image_process_path"));
            }
            let actual = PathBuf::from(OsString::from_wide(&name[..size as usize]));
            let checked = Self::pin(&actual, digest)?;
            let a = &self.identity;
            let b = &checked.identity;
            if (a.dwVolumeSerialNumber, a.nFileIndexHigh, a.nFileIndexLow)
                != (b.dwVolumeSerialNumber, b.nFileIndexHigh, b.nFileIndexLow)
            {
                return Err(WorkerError::IsolationUnavailable("image_changed", None));
            }
            let _ = &self.file;
            Ok(())
        }
    }

    struct Attributes {
        buffer: Vec<usize>,
        list: *mut c_void,
    }
    impl Attributes {
        fn new() -> Result<Self, WorkerError> {
            let mut size = 0;
            unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut size) };
            if size == 0 {
                return Err(failure("attribute_size"));
            }
            let mut buffer = vec![0usize; size.div_ceil(size_of::<usize>())];
            let list = buffer.as_mut_ptr().cast();
            if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut size) } == 0 {
                return Err(failure("attribute_init"));
            }
            Ok(Self { buffer, list })
        }
        fn set<T>(&mut self, attribute: u32, value: &T) -> Result<(), WorkerError> {
            if unsafe {
                UpdateProcThreadAttribute(
                    self.list,
                    0,
                    attribute as usize,
                    (value as *const T).cast(),
                    size_of::<T>(),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                Err(failure("attribute_set"))
            } else {
                Ok(())
            }
        }
    }
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe { DeleteProcThreadAttributeList(self.list) };
            let _ = &self.buffer;
        }
    }

    fn current_user_sid() -> Result<String, WorkerError> {
        let mut token = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(failure("current_token"));
        }
        let token = unsafe { own(token, "current_token")? };
        let mut size = 0u32;
        unsafe { GetTokenInformation(raw(&token), TokenUser, null_mut(), 0, &mut size) };
        if size == 0 {
            return Err(failure("current_user_size"));
        }
        let mut bytes = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                raw(&token),
                TokenUser,
                bytes.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        } == 0
        {
            return Err(failure("current_user"));
        }
        let sid = unsafe { (*(bytes.as_ptr() as *const TOKEN_USER)).User.Sid };
        if sid.is_null() {
            return Err(WorkerError::IsolationUnavailable("current_user_sid", None));
        }
        let mut text = null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(failure("current_sid_text"));
        }
        let mut len = 0;
        unsafe {
            while *text.add(len) != 0 {
                len += 1;
            }
        }
        let result = String::from_utf16(unsafe { std::slice::from_raw_parts(text, len) })
            .map_err(|_| WorkerError::IsolationUnavailable("current_sid_utf16", None));
        unsafe { LocalFree(text.cast()) };
        result
    }

    struct Pipe {
        server: Arc<OwnedHandle>,
        client: OwnedHandle,
    }
    impl Pipe {
        fn create(
            user_sid: &str,
            inbound: bool,
            gate: &Gate,
            deadline: Instant,
        ) -> Result<Self, WorkerError> {
            ensure_open(gate, deadline)?;
            let name = format!(r"\\.\pipe\nhc-wasm-{}", uuid::Uuid::new_v4().simple());
            let name = wide(OsStr::new(&name));
            let descriptor = format!("D:P(A;;GA;;;SY)(A;;GA;;;{user_sid})");
            let descriptor = wide(OsStr::new(&descriptor));
            let mut sd = null_mut();
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    descriptor.as_ptr(),
                    1,
                    &mut sd,
                    null_mut(),
                )
            } == 0
            {
                return Err(failure("pipe_descriptor"));
            }
            let server_attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            };
            let access = if inbound {
                PIPE_ACCESS_INBOUND
            } else {
                PIPE_ACCESS_OUTBOUND
            };
            let server_handle = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    access | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    CHUNK as u32,
                    CHUNK as u32,
                    0,
                    &server_attributes,
                )
            };
            let server_error = if server_handle.is_null() || server_handle as isize == -1 {
                Some(failure("pipe_server"))
            } else {
                None
            };
            unsafe { LocalFree(sd.cast()) };
            if let Some(error) = server_error {
                return Err(error);
            }
            let server = unsafe { own(server_handle, "pipe_server")? };
            let client_attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: null_mut(),
                bInheritHandle: 1,
            };
            let desired = if inbound { GENERIC_WRITE } else { GENERIC_READ };
            let client = unsafe {
                own(
                    CreateFileW(
                        name.as_ptr(),
                        desired,
                        0,
                        &client_attributes,
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL,
                        null_mut(),
                    ),
                    "pipe_client",
                )?
            };
            let event = unsafe { own(CreateEventW(null(), 1, 0, null()), "connect_event")? };
            let mut ov: Box<OVERLAPPED> = Box::new(unsafe { zeroed() });
            ov.hEvent = raw(&event);
            let connected = unsafe { ConnectNamedPipe(raw(&server), &mut *ov) };
            if connected == 0 {
                let code = std::io::Error::last_os_error().raw_os_error();
                if code == Some(ERROR_IO_PENDING as i32) {
                    loop {
                        let state = unsafe { WaitForSingleObject(raw(&event), 0) };
                        if state == WAIT_OBJECT_0 {
                            let mut transferred = 0;
                            if unsafe {
                                GetOverlappedResult(raw(&server), &*ov, &mut transferred, 0)
                            } != 0
                            {
                                break;
                            }
                            let err = std::io::Error::last_os_error().raw_os_error();
                            if known_terminal_error(err) {
                                return Err(WorkerError::IsolationUnavailable("pipe_connect", err));
                            }
                        }
                        // A failed observation or contradictory incomplete result
                        // cannot release the pending OVERLAPPED or its event.
                        if state != WAIT_TIMEOUT || gate.stopped(deadline) {
                            unsafe { CancelIoEx(raw(&server), &*ov) };
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                } else if code != Some(ERROR_PIPE_CONNECTED as i32) {
                    return Err(WorkerError::IsolationUnavailable("pipe_connect", code));
                }
            }
            let mut client_pid = 0;
            if unsafe { GetNamedPipeClientProcessId(raw(&server), &mut client_pid) } == 0
                || client_pid != unsafe { GetCurrentProcessId() }
            {
                return Err(WorkerError::IsolationUnavailable("pipe_peer", None));
            }
            ensure_open(gate, deadline)?;
            Ok(Self {
                server: Arc::new(server),
                client,
            })
        }
    }

    #[derive(Default)]
    struct StopState {
        job: Option<Arc<OwnedHandle>>,
        pipes: Vec<Arc<OwnedHandle>>,
        finished: bool,
    }
    struct Stopper {
        state: Arc<Mutex<StopState>>,
        thread: Option<thread::JoinHandle<()>>,
    }
    impl Stopper {
        fn start(gate: Arc<Gate>, deadline: Instant) -> Result<Self, WorkerError> {
            let state = Arc::new(Mutex::new(StopState::default()));
            let shared = state.clone();
            let thread = thread::Builder::new()
                .name("nhc-wasm-stop".into())
                .spawn(move || {
                    #[cfg(test)]
                    let mut reported_cancel = false;
                    loop {
                        let (job, pipes, finished) = {
                            let state = shared
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            (state.job.clone(), state.pipes.clone(), state.finished)
                        };
                        if finished {
                            break;
                        }
                        if gate.stopped(deadline) {
                            #[cfg(test)]
                            test_hooks::hold_stopper();
                            if let Some(job) = job {
                                unsafe { TerminateJobObject(raw(&job), 1) };
                            }
                            for pipe in pipes {
                                let _cancelled = unsafe { CancelIoEx(raw(&pipe), null()) };
                                #[cfg(test)]
                                if !reported_cancel {
                                    record_count(
                                        "stopper_cancel_io_return",
                                        0,
                                        _cancelled as usize,
                                    );
                                    reported_cancel = true;
                                }
                            }
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                })
                .map_err(|_| WorkerError::IsolationUnavailable("stop_thread", None))?;
            Ok(Self {
                state,
                thread: Some(thread),
            })
        }
        fn set_job(&self, job: Arc<OwnedHandle>) {
            self.state.lock().unwrap_or_else(|p| p.into_inner()).job = Some(job);
        }
        fn set_pipes(&self, pipes: Vec<Arc<OwnedHandle>>) {
            self.state.lock().unwrap_or_else(|p| p.into_inner()).pipes = pipes;
        }
        fn remove_pipe(&self, pipe: &Arc<OwnedHandle>) {
            self.state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .pipes
                .retain(|other| !Arc::ptr_eq(other, pipe));
        }
    }
    impl Drop for Stopper {
        fn drop(&mut self) {
            self.state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .finished = true;
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    struct Operation {
        pipe: Arc<OwnedHandle>,
        event: OwnedHandle,
        ov: Box<OVERLAPPED>,
        buffer: Vec<u8>,
        pending: bool,
        write: bool,
        #[cfg(test)]
        label: &'static str,
        #[cfg(test)]
        pid: u32,
    }
    impl Operation {
        fn new(
            pipe: Arc<OwnedHandle>,
            write: bool,
            _label: &'static str,
            _pid: u32,
        ) -> Result<Self, WorkerError> {
            let event = unsafe { own(CreateEventW(null(), 1, 0, null()), "io_event")? };
            let mut ov: Box<OVERLAPPED> = Box::new(unsafe { zeroed() });
            ov.hEvent = raw(&event);
            Ok(Self {
                pipe,
                event,
                ov,
                buffer: vec![0; CHUNK],
                pending: false,
                write,
                #[cfg(test)]
                label: _label,
                #[cfg(test)]
                pid: _pid,
            })
        }
        fn issue(
            &mut self,
            input: &[u8],
            gate: &Gate,
            deadline: Instant,
        ) -> Result<(), WorkerError> {
            if self.pending {
                return Err(WorkerError::IsolationUnavailable("io_double_submit", None));
            }
            if unsafe { ResetEvent(raw(&self.event)) } == 0 {
                return Err(failure("io_reset"));
            }
            *self.ov = unsafe { zeroed() };
            self.ov.hEvent = raw(&self.event);
            if self.write {
                self.buffer[..input.len()].copy_from_slice(input);
            }
            // Register before admission so a cancellation after admission
            // cannot release storage while the OS call is still returning.
            self.pending = true;
            #[cfg(test)]
            if self.write {
                test_hooks::checkpoint("submit_before_ticket", Some(self.pid));
            }
            if !gate.authorize(deadline) {
                self.pending = false;
                return Err(WorkerError::Deadline);
            }
            #[cfg(test)]
            if self.write {
                record_stage("submit_ticket_granted", Some(self.pid));
                test_hooks::checkpoint("submit_after_ticket", Some(self.pid));
            }
            let result = unsafe {
                if self.write {
                    WriteFile(
                        raw(&self.pipe),
                        self.buffer.as_ptr(),
                        input.len() as u32,
                        null_mut(),
                        &mut *self.ov,
                    )
                } else {
                    ReadFile(
                        raw(&self.pipe),
                        self.buffer.as_mut_ptr(),
                        self.buffer.len() as u32,
                        null_mut(),
                        &mut *self.ov,
                    )
                }
            };
            let code = if result == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            #[cfg(test)]
            if self.write && code == Some(ERROR_IO_PENDING as i32) {
                test_hooks::checkpoint("submit_returned_pending", Some(self.pid));
            }
            if result == 0 && code != Some(ERROR_IO_PENDING as i32) {
                self.pending = false;
                return Err(WorkerError::IsolationUnavailable("io_submit", code));
            }
            Ok(())
        }
        fn poll(&mut self) -> Result<Option<u32>, WorkerError> {
            if !self.pending {
                return Ok(None);
            }
            let state = unsafe { WaitForSingleObject(raw(&self.event), 0) };
            #[cfg(test)]
            let (state, injected_error) = {
                let first =
                    self.write && state == WAIT_TIMEOUT && test_hooks::start_persistent_event();
                if first {
                    record_stage("persistent_injected_event_error", Some(self.pid));
                }
                if first || (self.write && test_hooks::persistent_event_active()) {
                    (windows_sys::Win32::Foundation::WAIT_FAILED, true)
                } else {
                    (state, false)
                }
            };
            #[cfg(not(test))]
            let injected_error = false;
            if state == WAIT_TIMEOUT {
                #[cfg(test)]
                if self.write && test_hooks::armed("observation_injected") {
                    record_stage("kernel_write_event_unsignaled", Some(self.pid));
                    test_observer::capture_event(&self.event)?;
                    if test_hooks::checkpoint("observation_injected", Some(self.pid)) {
                        return Err(WorkerError::IsolationUnavailable(
                            "test_observation_injected",
                            None,
                        ));
                    }
                }
                return Ok(None);
            }
            if state != WAIT_OBJECT_0 {
                let error = if injected_error {
                    WorkerError::IsolationUnavailable("io_event_observation", Some(6))
                } else {
                    failure("io_event_observation")
                };
                return Err(error);
            }
            #[cfg(test)]
            if self.write && test_hooks::checkpoint("result_observation_injected", Some(self.pid)) {
                return Err(WorkerError::IsolationUnavailable("io_completion", Some(6)));
            }
            let mut transferred = 0;
            #[cfg(test)]
            let injected_result_error = if self.write {
                let first = test_hooks::start_persistent_result();
                if first {
                    record_stage("persistent_injected_result_error", Some(self.pid));
                }
                first || test_hooks::persistent_result_active()
            } else {
                false
            };
            #[cfg(not(test))]
            let injected_result_error = false;
            let completed = if injected_result_error {
                0
            } else {
                unsafe { GetOverlappedResult(raw(&self.pipe), &*self.ov, &mut transferred, 0) }
            };
            if completed != 0 {
                self.pending = false;
                return Ok(Some(transferred));
            }
            let code = if injected_result_error {
                Some(6)
            } else {
                std::io::Error::last_os_error().raw_os_error()
            };
            if code == Some(ERROR_IO_INCOMPLETE as i32) {
                return Ok(None);
            }
            if known_terminal_error(code) {
                self.pending = false;
            }
            Err(WorkerError::IsolationUnavailable("io_completion", code))
        }
    }
    impl Drop for Operation {
        fn drop(&mut self) {
            while self.pending {
                let _cancelled = unsafe { CancelIoEx(raw(&self.pipe), &*self.ov) };
                #[cfg(test)]
                {
                    record_count("cancel_io_return", self.pid, _cancelled as usize);
                    if self.write {
                        record_stage("stdin_cancel_returned", Some(self.pid));
                        test_hooks::checkpoint("cancel_after_api", Some(self.pid));
                    }
                }
                let _ = self.poll();
                if self.pending {
                    thread::sleep(Duration::from_millis(5));
                }
            }
            #[cfg(test)]
            record_stage(self.label, Some(self.pid));
        }
    }

    struct ProcessGuard {
        job: Arc<OwnedHandle>,
        process: OwnedHandle,
        pid: u32,
        complete: bool,
    }
    #[cfg(test)]
    struct JobEvents(OwnedHandle);
    #[cfg(test)]
    impl JobEvents {
        fn new(job: &OwnedHandle) -> Result<Self, WorkerError> {
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::System::IO::CreateIoCompletionPort;
            use windows_sys::Win32::System::JobObjects::{
                JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JobObjectAssociateCompletionPortInformation,
            };
            let port = unsafe {
                own(
                    CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1),
                    "test_job_port",
                )?
            };
            let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
                CompletionKey: std::ptr::dangling_mut::<c_void>(),
                CompletionPort: raw(&port),
            };
            if unsafe {
                SetInformationJobObject(
                    raw(job),
                    JobObjectAssociateCompletionPortInformation,
                    (&association as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
                    size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
                )
            } == 0
            {
                return Err(failure("test_job_port_associate"));
            }
            Ok(Self(port))
        }
    }
    #[cfg(test)]
    impl Drop for JobEvents {
        fn drop(&mut self) {
            use windows_sys::Win32::System::IO::GetQueuedCompletionStatus;
            loop {
                let mut message = 0;
                let mut key = 0usize;
                let mut process = null_mut();
                if unsafe {
                    GetQueuedCompletionStatus(
                        raw(&self.0),
                        &mut message,
                        &mut key,
                        &mut process,
                        100,
                    )
                } == 0
                {
                    break;
                }
                if key == 1 {
                    record_count(
                        "job_notification",
                        process as usize as u32,
                        message as usize,
                    );
                }
            }
        }
    }
    #[cfg(test)]
    struct RunScope(Option<u32>);
    #[cfg(test)]
    impl Drop for RunScope {
        fn drop(&mut self) {
            if let Some(pid) = self.0 {
                record_stage("run_handles_released", Some(pid));
            }
        }
    }
    struct StopOnFailure<'a> {
        gate: &'a Gate,
        success: bool,
    }
    impl Drop for StopOnFailure<'_> {
        fn drop(&mut self) {
            if !self.success {
                self.gate.stop();
            }
        }
    }
    impl ProcessGuard {
        fn reaped(&self) -> bool {
            let wait = unsafe { WaitForSingleObject(raw(&self.process), 0) };
            if wait != WAIT_OBJECT_0 {
                return false;
            }
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
            let queried = unsafe {
                QueryInformationJobObject(
                    raw(&self.job),
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    null_mut(),
                )
            };
            if queried != 0 && info.ActiveProcesses == 0 {
                record_count("process_wait", self.pid, wait as usize);
                record_count("job_query", self.pid, queried as usize);
                record_count("job_active", self.pid, info.ActiveProcesses as usize);
                #[cfg(test)]
                {
                    record_count("job_user_100ns", self.pid, info.TotalUserTime as usize);
                    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
                    let ok = unsafe {
                        QueryInformationJobObject(
                            raw(&self.job),
                            JobObjectExtendedLimitInformation,
                            (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                            null_mut(),
                        )
                    };
                    record_count("job_extended_query", self.pid, ok as usize);
                    if ok != 0 {
                        record_count("job_memory_limit", self.pid, limits.ProcessMemoryLimit);
                        record_count(
                            "job_peak_process_memory",
                            self.pid,
                            limits.PeakProcessMemoryUsed,
                        );
                        record_count(
                            "job_cpu_limit_100ns",
                            self.pid,
                            limits.BasicLimitInformation.PerProcessUserTimeLimit as usize,
                        );
                    }
                }
                true
            } else {
                false
            }
        }
        fn wait_reaped(&mut self) {
            while !self.reaped() {
                unsafe {
                    let _job_result = TerminateJobObject(raw(&self.job), 1);
                    let _process_result = TerminateProcess(raw(&self.process), 1);
                    #[cfg(test)]
                    {
                        record_count("terminate_job_return", self.pid, _job_result as usize);
                        record_count(
                            "terminate_process_return",
                            self.pid,
                            _process_result as usize,
                        );
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            self.complete = true;
            record_stage("failure_reaped", Some(self.pid));
        }
    }
    impl Drop for ProcessGuard {
        fn drop(&mut self) {
            if !self.complete {
                self.wait_reaped();
            }
        }
    }

    fn io_loop(
        input: Vec<u8>,
        pipes: [Arc<OwnedHandle>; 3],
        process: &mut ProcessGuard,
        stopper: &Stopper,
        gate: &Gate,
        deadline: Instant,
    ) -> Result<Vec<u8>, WorkerError> {
        let [stdin, stdout, stderr] = pipes;
        let mut writer = Some(Operation::new(
            stdin,
            true,
            "stdin_io_settled",
            process.pid,
        )?);
        let mut out = Operation::new(stdout, false, "stdout_io_settled", process.pid)?;
        let mut err = Operation::new(stderr, false, "stderr_io_settled", process.pid)?;
        let mut fail_stop = StopOnFailure {
            gate,
            success: false,
        };
        let mut offset = 0usize;
        let mut response = Vec::new();
        let mut stderr_count = 0usize;
        let mut stdout_eof = false;
        let mut stderr_eof = false;
        let mut stdout_zero_observed = false;
        #[cfg(test)]
        let mut stderr_zero_observed = false;
        #[cfg(test)]
        let mut stdin_pending_observed = false;
        #[cfg(test)]
        let mut half_closed_observed = false;
        #[cfg(test)]
        let mut hold_reads = test_observer::take_pressure();
        #[cfg(test)]
        let pressure_start = Instant::now();
        loop {
            if gate.stopped(deadline) {
                gate.stop();
                return Err(WorkerError::Deadline);
            }
            if let Some(write) = &mut writer {
                if !write.pending && offset < input.len() {
                    let end = (offset + CHUNK).min(input.len());
                    write.issue(&input[offset..end], gate, deadline)?;
                }
                if let Some(n) = write.poll()? {
                    if n == 0 || offset + n as usize > input.len() {
                        return Err(WorkerError::Protocol);
                    }
                    offset += n as usize;
                }
                #[cfg(test)]
                if write.pending && !stdin_pending_observed {
                    record_stage("stdin_pending", Some(process.pid));
                    record_count("stdin_pending_bytes", process.pid, offset);
                    stdin_pending_observed = true;
                }
                if offset == input.len() && !write.pending {
                    stopper.remove_pipe(&write.pipe);
                    writer = None;
                }
            }
            #[cfg(test)]
            if hold_reads {
                let (out_capacity, out_available) = pipe_pressure(&out.pipe)?;
                let (err_capacity, err_available) = pipe_pressure(&err.pipe)?;
                let pending = writer.as_ref().is_some_and(|write| write.pending);
                if pending
                    && out_capacity > 0
                    && err_capacity > 0
                    && out_available >= out_capacity
                    && err_available >= err_capacity
                {
                    for (phase, value) in [
                        ("pressure_stdout_capacity", out_capacity),
                        ("pressure_stdout_available", out_available),
                        ("pressure_stderr_capacity", err_capacity),
                        ("pressure_stderr_available", err_available),
                    ] {
                        record_count(phase, process.pid, value as usize);
                    }
                    record_stage("dual_pressure_release", Some(process.pid));
                    hold_reads = false;
                } else if pressure_start.elapsed() >= Duration::from_secs(3) {
                    for (phase, value) in [
                        ("pressure_stdout_capacity", out_capacity),
                        ("pressure_stdout_available", out_available),
                        ("pressure_stderr_capacity", err_capacity),
                        ("pressure_stderr_available", err_available),
                    ] {
                        record_count(phase, process.pid, value as usize);
                    }
                    return Err(WorkerError::IsolationUnavailable(
                        "test_pressure_timeout",
                        None,
                    ));
                } else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
            }
            for (op, eof, target) in [
                (&mut out, &mut stdout_eof, true),
                (&mut err, &mut stderr_eof, false),
            ] {
                if *eof {
                    continue;
                }
                if !op.pending {
                    match op.issue(&[], gate, deadline) {
                        Err(WorkerError::IsolationUnavailable(_, Some(code)))
                            if code == ERROR_BROKEN_PIPE as i32 =>
                        {
                            *eof = true;
                            if target {
                                record_stage("stdout_eof", Some(process.pid));
                            } else {
                                record_stage("stderr_eof", Some(process.pid));
                            }
                            continue;
                        }
                        other => other?,
                    }
                }
                #[cfg(test)]
                let injected = test_hooks::inject_zero(target);
                #[cfg(not(test))]
                let injected = false;
                if injected {
                    record_stage(
                        if target {
                            "injected_stdout_zero"
                        } else {
                            "injected_stderr_zero"
                        },
                        Some(process.pid),
                    );
                }
                let polled = if injected { Ok(Some(0)) } else { op.poll() };
                match polled {
                    Ok(Some(0)) => {
                        if target && !stdout_zero_observed {
                            record_stage("stdout_zero_read", Some(process.pid));
                            stdout_zero_observed = true;
                        } else {
                            #[cfg(test)]
                            if !target && !stderr_zero_observed {
                                record_stage("stderr_zero_read", Some(process.pid));
                                stderr_zero_observed = true;
                            }
                        }
                    }
                    Ok(Some(n)) => {
                        if target {
                            response.extend_from_slice(&op.buffer[..n as usize]);
                            record_stage("stdout_data", Some(process.pid));
                            record_count("stdout_bytes", process.pid, response.len());
                            if response.len() > protocol::MAX_RESPONSE + 4 {
                                record_stage("stdout_limit", Some(process.pid));
                                return Err(WorkerError::Protocol);
                            }
                        } else {
                            stderr_count += n as usize;
                            record_stage("stderr_data", Some(process.pid));
                            record_count("stderr_bytes", process.pid, stderr_count);
                            if stderr_count > MAX_STDERR {
                                record_stage("stderr_limit", Some(process.pid));
                                return Err(WorkerError::IsolationUnavailable(
                                    "stderr_limit",
                                    None,
                                ));
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(WorkerError::IsolationUnavailable(_, Some(code)))
                        if code == ERROR_BROKEN_PIPE as i32 =>
                    {
                        *eof = true;
                        if target {
                            record_stage("stdout_eof", Some(process.pid));
                        } else {
                            record_stage("stderr_eof", Some(process.pid));
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            #[cfg(test)]
            if !half_closed_observed
                && stdout_eof
                && !stderr_eof
                && unsafe { WaitForSingleObject(raw(&process.process), 0) } == WAIT_TIMEOUT
            {
                record_stage("stdout_eof_process_alive_stderr_open", Some(process.pid));
                half_closed_observed = true;
            }
            if writer.is_none() && stdout_eof && stderr_eof && process.reaped() {
                process.complete = true;
                record_stage("io_complete_reaped", Some(process.pid));
                fail_stop.success = true;
                return Ok(response);
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    pub(super) fn run(
        worker: &TrustedWorker,
        request: Request,
        deadline: Instant,
        gate: &Arc<Gate>,
    ) -> Result<WorkerSuccess, WorkerError> {
        #[cfg(test)]
        let mut run_scope = RunScope(None);
        record_stage("supervision_started", None);
        let stopper = Stopper::start(gate.clone(), deadline)?;
        ensure_open(gate, deadline)?;
        let mut frame = Vec::with_capacity(
            request.module.len() + protocol::MAX_MANIFEST + protocol::MAX_INPUT + 12,
        );
        protocol::write_request(&mut frame, &request).map_err(|_| WorkerError::InvalidRequest)?;
        ensure_open(gate, deadline)?;
        let image = Image::pin(&worker.path, &worker.digest)?;
        ensure_open(gate, deadline)?;
        let user_sid = current_user_sid()?;
        let Pipe {
            server: stdin,
            client: child_stdin,
        } = Pipe::create(&user_sid, false, gate, deadline)?;
        let Pipe {
            server: stdout,
            client: child_stdout,
        } = Pipe::create(&user_sid, true, gate, deadline)?;
        let Pipe {
            server: stderr,
            client: child_stderr,
        } = Pipe::create(&user_sid, true, gate, deadline)?;
        stopper.set_pipes(vec![stdin.clone(), stdout.clone(), stderr.clone()]);
        ensure_open(gate, deadline)?;
        let job = Arc::new(unsafe { own(CreateJobObjectW(null(), null()), "job_create")? });
        #[cfg(test)]
        let _job_events = if test_hooks::take_job_events() {
            Some(JobEvents::new(&job)?)
        } else {
            None
        };
        if unsafe { SetHandleInformation(raw(&job), HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(failure("job_inherit"));
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_TIME;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        limits.BasicLimitInformation.PerProcessUserTimeLimit = 30_000_000;
        limits.ProcessMemoryLimit = 128 * 1024 * 1024;
        if unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(failure("job_limits"));
        }
        let mut actual: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        if unsafe {
            QueryInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                (&mut actual as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(failure("job_limits_query"));
        }
        if actual.BasicLimitInformation.ActiveProcessLimit != 1
            || actual.ProcessMemoryLimit != limits.ProcessMemoryLimit
            || actual.BasicLimitInformation.PerProcessUserTimeLimit != 30_000_000
            || actual.BasicLimitInformation.LimitFlags & limits.BasicLimitInformation.LimitFlags
                != limits.BasicLimitInformation.LimitFlags
            || actual.BasicLimitInformation.LimitFlags
                & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK)
                != 0
        {
            return Err(WorkerError::IsolationUnavailable(
                "job_limits_mismatch",
                None,
            ));
        }
        stopper.set_job(job.clone());
        let jobs = [raw(&job)];
        let handles = [raw(&child_stdin), raw(&child_stdout), raw(&child_stderr)];
        let mut attributes = Attributes::new()?;
        attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = attributes.list;
        let app = wide(image.path.as_os_str());
        let mut command = wide(OsStr::new(&format!("\"{}\"", image.path.display())));
        let dir = image
            .path
            .parent()
            .ok_or(WorkerError::IsolationUnavailable("worker_dir", None))?;
        let dir = wide(dir.as_os_str());
        let system_root = std::env::var_os("SystemRoot")
            .ok_or(WorkerError::IsolationUnavailable("system_root", None))?;
        let mut environment: Vec<u16> = OsStr::new("SystemRoot=")
            .encode_wide()
            .chain(system_root.encode_wide())
            .chain([0, 0])
            .collect();
        let mut information: PROCESS_INFORMATION = unsafe { zeroed() };
        ensure_open(gate, deadline)?;
        record_stage("before_create", None);
        #[cfg(test)]
        test_hooks::checkpoint("create_before_ticket", None);
        if !gate.authorize(deadline) {
            return Err(WorkerError::Deadline);
        }
        #[cfg(test)]
        {
            record_stage("create_ticket_granted", None);
            test_hooks::checkpoint("create_after_ticket", None);
        }
        let created = unsafe {
            CreateProcessW(
                app.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_SUSPENDED
                    | CREATE_NO_WINDOW
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT,
                environment.as_mut_ptr().cast(),
                dir.as_ptr(),
                &startup.StartupInfo,
                &mut information,
            )
        };
        if created == 0 {
            let error = failure("worker_create");
            record_stage("create_failed", None);
            return Err(error);
        }
        let process_handle = match unsafe { own(information.hProcess, "worker_process_handle") } {
            Ok(handle) => handle,
            Err(_) => {
                // A successful CreateProcess without an observable process handle
                // cannot satisfy the cleanup proof. Keep this one slot occupied.
                record_stage(
                    "created_process_handle_unknown",
                    Some(information.dwProcessId),
                );
                gate.stop();
                loop {
                    thread::park_timeout(Duration::from_secs(1));
                }
            }
        };
        let mut process = ProcessGuard {
            job: job.clone(),
            process: process_handle,
            pid: information.dwProcessId,
            complete: false,
        };
        #[cfg(test)]
        {
            run_scope.0 = Some(information.dwProcessId);
        }
        let thread_handle = unsafe { own(information.hThread, "worker_thread_handle")? };
        record_stage("created_suspended", Some(information.dwProcessId));
        #[cfg(test)]
        test_observer::capture(information.dwProcessId)?;
        #[cfg(test)]
        test_hooks::checkpoint("create_returned_suspended", Some(information.dwProcessId));
        drop(child_stdin);
        drop(child_stdout);
        drop(child_stderr);
        let mut inside = 0;
        if unsafe { IsProcessInJob(raw(&process.process), raw(&job), &mut inside) } == 0 {
            let error = failure("worker_job_membership");
            gate.stop();
            return Err(error);
        }
        #[cfg(test)]
        if test_hooks::inject_membership_failure() {
            record_count(
                "membership_actual",
                information.dwProcessId,
                inside as usize,
            );
            record_stage("membership_failure_injected", Some(information.dwProcessId));
            inside = 0;
        }
        if inside == 0 {
            gate.stop();
            return Err(WorkerError::IsolationUnavailable("worker_not_in_job", None));
        }
        image.verify_process(raw(&process.process), &worker.digest)?;
        #[cfg(test)]
        test_hooks::checkpoint("resume_before_ticket", Some(information.dwProcessId));
        if !gate.authorize(deadline) {
            return Err(WorkerError::Deadline);
        }
        #[cfg(test)]
        {
            record_stage("resume_ticket_granted", Some(information.dwProcessId));
            test_hooks::checkpoint("resume_after_ticket", Some(information.dwProcessId));
        }
        let resume_result = unsafe { ResumeThread(raw(&thread_handle)) };
        let resume_error = if resume_result == u32::MAX {
            Some(failure("worker_resume"))
        } else {
            None
        };
        #[cfg(test)]
        record_count(
            "resume_api_return",
            information.dwProcessId,
            resume_result as usize,
        );
        if let Some(error) = resume_error {
            gate.stop();
            return Err(error);
        }
        record_stage("resumed", Some(information.dwProcessId));
        let response = io_loop(
            frame,
            [stdin, stdout, stderr],
            &mut process,
            &stopper,
            gate,
            deadline,
        )?;
        ensure_open(gate, deadline)?;
        let mut exit_code = 0;
        if unsafe { GetExitCodeProcess(raw(&process.process), &mut exit_code) } == 0 {
            return Err(failure("worker_exit_code"));
        }
        if exit_code != 0 {
            return Err(WorkerError::IsolationUnavailable(
                "worker_exit",
                Some(exit_code as i32),
            ));
        }
        let response =
            protocol::read_response(&mut response.as_slice(), &request.manifest, &request.input)
                .map_err(|_| WorkerError::Protocol)?;
        match response.outcome {
            Outcome::Ok { decision } => Ok(WorkerSuccess {
                decision,
                process_id: information.dwProcessId,
                exit_code,
                process_signaled: true,
                job_active_processes: 0,
                io_completed: true,
            }),
            Outcome::Error { .. } => Err(WorkerError::WorkerRejected),
        }
    }

    #[cfg(test)]
    mod r04_tests {
        use super::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };

        struct UnwindCleanup(Arc<OwnedHandle>);
        impl Drop for UnwindCleanup {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    unsafe {
                        TerminateJobObject(raw(&self.0), 1);
                    }
                }
            }
        }

        #[test]
        fn suspended_process_in_real_job_requires_process_fallback() {
            let path =
                PathBuf::from(std::env::var_os("NHC_W2_WORKER_SNAPSHOT").expect("worker snapshot"));
            let hash = Sha256::digest(std::fs::read(&path).expect("worker bytes"));
            assert_eq!(
                format!("{hash:x}"),
                std::env::var("NHC_W2_WORKER_SHA256").unwrap()
            );
            let digest: [u8; 32] = hash.into();
            let image = Image::pin(&path, &digest).expect("pin worker");
            let real_job =
                Arc::new(unsafe { own(CreateJobObjectW(null(), null()), "real_job") }.unwrap());
            let empty_job =
                Arc::new(unsafe { own(CreateJobObjectW(null(), null()), "empty_job") }.unwrap());
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_PROCESS_TIME;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            limits.BasicLimitInformation.PerProcessUserTimeLimit = 30_000_000;
            limits.ProcessMemoryLimit = 128 * 1024 * 1024;
            assert_ne!(
                unsafe {
                    SetInformationJobObject(
                        raw(&real_job),
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    )
                },
                0
            );
            let mut attributes = Attributes::new().unwrap();
            let jobs = [raw(&real_job)];
            attributes
                .set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)
                .unwrap();
            let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.lpAttributeList = attributes.list;
            let application = wide(image.path.as_os_str());
            let mut command = wide(OsStr::new(&format!("\"{}\"", image.path.display())));
            let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
            assert_ne!(
                unsafe {
                    CreateProcessW(
                        application.as_ptr(),
                        command.as_mut_ptr(),
                        null(),
                        null(),
                        0,
                        CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                        null_mut(),
                        null(),
                        &startup.StartupInfo,
                        &mut info,
                    )
                },
                0
            );
            let process = unsafe { own(info.hProcess, "real_process") }.unwrap();
            let thread_handle = unsafe { own(info.hThread, "real_thread") }.unwrap();
            let mut guard = ProcessGuard {
                job: empty_job,
                process,
                pid: info.dwProcessId,
                complete: false,
            };
            let _unwind_cleanup = UnwindCleanup(real_job.clone());
            image
                .verify_process(raw(&guard.process), &digest)
                .expect("same snapshot");
            let mut in_real_job = 0;
            assert_ne!(
                unsafe { IsProcessInJob(raw(&guard.process), raw(&real_job), &mut in_real_job) },
                0
            );
            assert_ne!(in_real_job, 0);
            let observed = Arc::new(
                unsafe {
                    own(
                        OpenProcess(
                            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                            0,
                            info.dwProcessId,
                        ),
                        "observer",
                    )
                }
                .unwrap(),
            );
            let fallback_used = Arc::new(AtomicBool::new(false));
            let watchdog = {
                let real_job = real_job.clone();
                let observed = observed.clone();
                let fallback_used = fallback_used.clone();
                thread::spawn(move || {
                    if unsafe { WaitForSingleObject(raw(&observed), 2500) } != WAIT_OBJECT_0 {
                        fallback_used.store(true, Ordering::SeqCst);
                        unsafe {
                            TerminateJobObject(raw(&real_job), 1);
                        }
                    }
                })
            };
            let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
            let cleanup = thread::spawn(move || {
                guard.wait_reaped();
                let _ = done_tx.send(());
            });
            let done = done_rx.recv_timeout(Duration::from_secs(6)).is_ok();
            watchdog.join().unwrap();
            assert!(
                done,
                "R04 cleanup remains unknown; stop and inspect retained process"
            );
            cleanup.join().unwrap();
            let wait = unsafe { WaitForSingleObject(raw(&observed), 0) };
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
            let deadline = Instant::now() + Duration::from_secs(2);
            let queried = loop {
                let query = unsafe {
                    QueryInformationJobObject(
                        raw(&real_job),
                        JobObjectBasicAccountingInformation,
                        (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                        size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                        null_mut(),
                    )
                };
                if query == 0 || accounting.ActiveProcesses == 0 || Instant::now() >= deadline {
                    break query;
                }
                thread::sleep(Duration::from_millis(5));
            };
            println!(
                "R04 pid={} observed_wait={wait} real_job_query={queried} real_job_active={} watchdog_used={}",
                info.dwProcessId,
                accounting.ActiveProcesses,
                fallback_used.load(Ordering::SeqCst)
            );
            assert!(
                !fallback_used.load(Ordering::SeqCst),
                "empty Job could not terminate real worker"
            );
            assert_eq!(wait, WAIT_OBJECT_0);
            assert_ne!(queried, 0);
            assert_eq!(accounting.ActiveProcesses, 0);
            drop(thread_handle);
        }
    }
}
