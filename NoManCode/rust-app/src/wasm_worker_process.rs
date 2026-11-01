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
static P01_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
#[cfg(all(test, windows))]
static P01_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
fn record_p01(value: &serde_json::Value) {
    use std::io::Write;
    let _guard = STAGE_LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut value = value.clone();
    value["sequence"] = serde_json::json!(P01_SEQUENCE.fetch_add(1, Ordering::Relaxed));
    value["monotonic_us"] =
        serde_json::json!(P01_START.get_or_init(Instant::now).elapsed().as_micros());
    if let Some(path) = std::env::var_os("NHC_W2_STAGE_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "p01={value}");
    }
}

#[cfg(all(test, windows))]
pub(crate) mod test_observer {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    static PROCESS: Mutex<Option<(u32, OwnedHandle)>> = Mutex::new(None);
    static ARMED: AtomicBool = AtomicBool::new(false);
    static PRESSURE: AtomicBool = AtomicBool::new(false);
    static IO_EVENT: Mutex<Option<OwnedHandle>> = Mutex::new(None);
    static JOB: Mutex<Option<Arc<OwnedHandle>>> = Mutex::new(None);
    static JOB_ARMED: AtomicBool = AtomicBool::new(false);

    pub(crate) fn arm_job_probe() {
        assert!(JOB.lock().unwrap().is_none());
        assert!(!JOB_ARMED.swap(true, Ordering::SeqCst));
    }
    pub(super) fn capture_job(job: &Arc<OwnedHandle>) {
        if JOB_ARMED.swap(false, Ordering::SeqCst) {
            *JOB.lock().unwrap_or_else(|e| e.into_inner()) = Some(job.clone());
        }
    }
    pub(crate) fn take_job() -> Option<Arc<OwnedHandle>> {
        JOB.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

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
        atomic::{AtomicBool, AtomicU8, Ordering},
    };
    use std::time::{Duration, Instant};

    struct State {
        phase: Option<&'static str>,
        next_phase: Option<&'static str>,
        reached: bool,
        released: bool,
        timed_out: bool,
    }
    static STATE: Mutex<State> = Mutex::new(State {
        phase: None,
        next_phase: None,
        reached: false,
        released: false,
        timed_out: false,
    });
    static WAKE: Condvar = Condvar::new();
    static ZERO: AtomicU8 = AtomicU8::new(0);
    static MEMBERSHIP: AtomicU8 = AtomicU8::new(0);
    static JOB_EVENTS: AtomicU8 = AtomicU8::new(0);
    static P01_EVENTS: AtomicU8 = AtomicU8::new(0);
    static PERSISTENT_ERROR: AtomicU8 = AtomicU8::new(0);
    static W05_STOPPER_STARTED: AtomicBool = AtomicBool::new(false);

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
            next_phase: None,
            reached: false,
            released: false,
            timed_out: false,
        };
        Guard
    }
    pub(crate) fn reached() -> bool {
        STATE.lock().unwrap_or_else(|e| e.into_inner()).reached
    }
    pub(crate) fn phase_reached(phase: &'static str) -> bool {
        let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.phase == Some(phase) && state.reached
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
    pub(crate) fn release_with_next(phase: &'static str) {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.next_phase = Some(phase);
        state.released = true;
        WAKE.notify_all();
    }
    pub(crate) fn w05_reset_stopper() {
        W05_STOPPER_STARTED.store(false, Ordering::SeqCst);
    }
    pub(crate) fn w05_stopper_started() -> bool {
        W05_STOPPER_STARTED.load(Ordering::SeqCst)
    }
    pub(super) fn w05_mark_stopper() {
        W05_STOPPER_STARTED.store(true, Ordering::SeqCst);
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
        state.phase = if released {
            state.next_phase.take()
        } else {
            None
        };
        if state.phase.is_some() {
            state.reached = false;
            state.released = false;
        }
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
    pub(crate) fn arm_p01_events() {
        P01_EVENTS.store(1, Ordering::SeqCst);
    }
    pub(super) fn take_p01_events() -> bool {
        P01_EVENTS.swap(0, Ordering::SeqCst) == 1
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
                CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessW,
                DETACHED_PROCESS, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
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

    #[cfg(test)]
    mod w06_artifact_tests {
        use super::*;
        use std::io::Write;

        fn digest(bytes: &[u8]) -> [u8; 32] {
            Sha256::digest(bytes).into()
        }

        #[test]
        #[ignore = "requires controlled isolated process or artifact fixture"]
        fn pin_file_counterexamples() {
            let root = PathBuf::from(std::env::var_os("NHC_W06_ROOT").expect("fresh W06 root"));
            let dir = tempfile::Builder::new()
                .prefix("w06-artifact-")
                .tempdir_in(root)
                .expect("W06 dedicated directory");
            let path = dir.path().join("worker.exe");
            let original = b"W06 original bytes";
            let replacement = b"W06 replacement bytes";
            std::fs::write(&path, original).expect("original artifact");
            let expected = digest(original);
            drop(Image::pin(&path, &expected).expect("valid digest control"));
            assert!(matches!(
                Image::pin(&path, &digest(b"wrong")),
                Err(WorkerError::IsolationUnavailable("image_digest", None))
            ));
            let absent = dir.path().join("absent.exe");
            assert!(matches!(
                Image::pin(&absent, &expected),
                Err(WorkerError::IsolationUnavailable("image_open", _))
            ));

            let backup = dir.path().join("before-pin-backup.exe");
            std::fs::rename(&path, &backup).expect("before-pin replacement rename control");
            std::fs::write(&path, replacement).expect("before-pin replacement write control");
            assert_eq!(std::fs::read(&path).unwrap(), replacement);
            assert!(matches!(
                Image::pin(&path, &expected),
                Err(WorkerError::IsolationUnavailable("image_digest", None))
            ));
            drop(Image::pin(&path, &digest(replacement)).expect("replacement digest control"));

            let pinned = Image::pin(&path, &digest(replacement)).expect("post-pin control");
            let rename = std::fs::rename(&path, dir.path().join("after-pin-moved.exe"));
            let write = OpenOptions::new().write(true).open(&path);
            assert!(rename.is_err(), "rename succeeded while image pinned");
            assert!(write.is_err(), "write handle opened while image pinned");
            assert_eq!(std::fs::read(&path).unwrap(), replacement);
            drop(pinned);
            std::fs::rename(&path, dir.path().join("after-unpin-moved.exe"))
                .expect("rename works after unpin");
            std::fs::write(&path, original).unwrap();

            let link = dir.path().join("hard-link.exe");
            match std::fs::hard_link(&path, &link) {
                Ok(()) => {
                    let metadata = std::fs::metadata(&link).unwrap();
                    assert!(metadata.is_file());
                    assert!(matches!(
                        Image::pin(&link, &expected),
                        Err(WorkerError::IsolationUnavailable("image_link", None))
                    ));
                    std::fs::remove_file(&link).unwrap();
                    drop(Image::pin(&path, &expected).expect("hard-link removed control"));
                }
                Err(error) => panic!("W06 hard-link attack not established: {error}"),
            }

            let symlink = dir.path().join("symbolic-link.exe");
            match std::os::windows::fs::symlink_file(&path, &symlink) {
                Ok(()) => {
                    let metadata = std::fs::symlink_metadata(&symlink).unwrap();
                    assert_ne!(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT, 0);
                    assert!(matches!(
                        Image::pin(&symlink, &expected),
                        Err(WorkerError::IsolationUnavailable(
                            "image_link" | "image_open",
                            _
                        ))
                    ));
                    std::fs::remove_file(&symlink).unwrap();
                }
                Err(error) => panic!("W06 reparse attack not established: {error}"),
            }

            let mut writer = OpenOptions::new()
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(&path)
                .expect("pre-held writer");
            writer.write_all(original).expect("writer attack condition");
            writer.flush().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(matches!(
                Image::pin(&path, &expected),
                Err(WorkerError::IsolationUnavailable("image_open", _))
            ));
            drop(writer);
            drop(Image::pin(&path, &expected).expect("writer closed control"));
        }

        #[test]
        #[ignore = "requires controlled isolated process or artifact fixture"]
        fn pin_existing_writable_mapping() {
            use windows_sys::Win32::System::Memory::{
                CreateFileMappingW, FILE_MAP_WRITE, FlushViewOfFile, MEMORY_MAPPED_VIEW_ADDRESS,
                MapViewOfFile, PAGE_READWRITE, UnmapViewOfFile,
            };
            struct View(MEMORY_MAPPED_VIEW_ADDRESS);
            impl Drop for View {
                fn drop(&mut self) {
                    unsafe { UnmapViewOfFile(self.0) };
                }
            }
            let root = PathBuf::from(std::env::var_os("NHC_W06_ROOT").expect("fresh W06 root"));
            let dir = tempfile::Builder::new()
                .prefix("w06-mapping-")
                .tempdir_in(root)
                .expect("W06 dedicated directory");
            let path = dir.path().join("worker.exe");
            let bytes = b"W06 mapping bytes";
            std::fs::write(&path, bytes).unwrap();
            drop(Image::pin(&path, &digest(bytes)).expect("mapping test valid control"));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(&path)
                .expect("mapping writer");
            let mapping_raw = unsafe {
                CreateFileMappingW(file.as_raw_handle(), null(), PAGE_READWRITE, 0, 0, null())
            };
            if mapping_raw.is_null() {
                panic!(
                    "W06 writable mapping not established: {:?}",
                    std::io::Error::last_os_error().raw_os_error()
                );
            }
            let mapping = unsafe { OwnedHandle::from_raw_handle(mapping_raw) };
            let mapped = unsafe { MapViewOfFile(raw(&mapping), FILE_MAP_WRITE, 0, 0, bytes.len()) };
            if mapped.Value.is_null() {
                panic!(
                    "W06 writable view not established: {:?}",
                    std::io::Error::last_os_error().raw_os_error()
                );
            }
            let view = View(mapped);
            drop(file);
            unsafe { view.0.Value.cast::<u8>().write(b'X') };
            assert_ne!(unsafe { FlushViewOfFile(view.0.Value, bytes.len()) }, 0);
            assert_eq!(
                std::fs::read(&path).unwrap()[0],
                b'X',
                "mapping attack control"
            );
            unsafe { view.0.Value.cast::<u8>().write(bytes[0]) };
            assert_ne!(unsafe { FlushViewOfFile(view.0.Value, bytes.len()) }, 0);
            match Image::pin(&path, &digest(bytes)) {
                Err(WorkerError::IsolationUnavailable("image_open", code)) => {
                    println!("W06 active writable mapping rejected: {code:?}");
                }
                Ok(pinned) => {
                    unsafe { view.0.Value.cast::<u8>().write(b'X') };
                    assert_ne!(unsafe { FlushViewOfFile(view.0.Value, bytes.len()) }, 0);
                    let changed = std::fs::read(&path).unwrap()[0] == b'X';
                    unsafe { view.0.Value.cast::<u8>().write(bytes[0]) };
                    let restored = unsafe { FlushViewOfFile(view.0.Value, bytes.len()) };
                    drop(pinned);
                    assert_ne!(restored, 0);
                    assert!(!changed, "writable mapping changed the pinned image");
                }
                Err(error) => panic!("mapping pin returned unexpected result: {error:?}"),
            }
            drop(view);
            drop(mapping);
            drop(Image::pin(&path, &digest(bytes)).expect("mapping released control"));
        }

        #[test]
        #[ignore = "requires controlled isolated process or artifact fixture"]
        fn process_image_identity_counterexample() {
            use std::{
                os::windows::process::CommandExt,
                process::{Child, Command, Stdio},
            };
            struct SuspendedChild(Child);
            impl Drop for SuspendedChild {
                fn drop(&mut self) {
                    if unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
                        let _ = self.0.kill();
                    }
                    if unsafe { WaitForSingleObject(self.0.as_raw_handle(), 5000) } != WAIT_OBJECT_0
                    {
                        eprintln!("W06 image process cleanup unknown");
                        loop {
                            thread::park_timeout(Duration::from_secs(1));
                        }
                    }
                }
            }
            let root = PathBuf::from(std::env::var_os("NHC_W06_ROOT").expect("fresh W06 root"));
            let source = PathBuf::from(
                std::env::var_os("NHC_W2_WORKER_SNAPSHOT").expect("fixed real worker"),
            );
            let expected = std::env::var("NHC_W2_WORKER_SHA256").expect("fixed worker digest");
            let bytes = std::fs::read(source).expect("fixed real worker bytes");
            assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
            let dir = tempfile::Builder::new()
                .prefix("w06-image-process-")
                .tempdir_in(root)
                .expect("W06 dedicated directory");
            let first = dir.path().join("first.exe");
            let second = dir.path().join("second.exe");
            std::fs::write(&first, &bytes).unwrap();
            std::fs::write(&second, &bytes).unwrap();
            let hash = digest(&bytes);
            let first_image = Image::pin(&first, &hash).expect("first image positive pin");
            let second_image = Image::pin(&second, &hash).expect("second image positive pin");
            let id = |image: &Image| {
                (
                    image.identity.dwVolumeSerialNumber,
                    image.identity.nFileIndexHigh,
                    image.identity.nFileIndexLow,
                )
            };
            assert_ne!(
                id(&first_image),
                id(&second_image),
                "different file identities"
            );
            let child = match Command::new(&second)
                .creation_flags(CREATE_SUSPENDED | DETACHED_PROCESS)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => SuspendedChild(child),
                Err(error) => {
                    panic!("W06 process identity attack not established: {error}");
                }
            };
            assert_eq!(
                unsafe { WaitForSingleObject(child.0.as_raw_handle(), 0) },
                WAIT_TIMEOUT
            );
            second_image
                .verify_process(child.0.as_raw_handle(), &hash)
                .expect("actual second process image positive control");
            assert!(matches!(
                first_image.verify_process(child.0.as_raw_handle(), &hash),
                Err(WorkerError::IsolationUnavailable("image_changed", None))
            ));
            println!("W06 image identity actual process pid={}", child.0.id());
        }
    }

    // This fixture deliberately passes a test Job handle to the helper. It is
    // separate from W05 and tests limit effects, not last-handle semantics.
    #[cfg(test)]
    mod w06_limit_tests {
        use super::*;
        use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;

        fn publish(path: &Path, value: &serde_json::Value) {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, value.to_string()).expect("limit receipt write");
            std::fs::rename(tmp, path).expect("limit receipt publish");
        }

        #[test]
        #[ignore = "requires isolated controlled Job and suspended process fixture"]
        fn limit_helper() {
            let path =
                PathBuf::from(std::env::var_os("NHC_W06_LIMIT_RECEIPT").expect("receipt path"));
            let job_number: usize = std::env::var("NHC_W06_LIMIT_JOB")
                .expect("job handle")
                .parse()
                .expect("job number");
            let breakaway = std::env::var("NHC_W06_LIMIT_BREAKAWAY").expect("attempt flag") == "1";
            let nonce = std::env::var("NHC_W06_LIMIT_NONCE").expect("limit nonce");
            let helper_pid = std::process::id();
            let exe = std::env::current_exe().expect("fixed test executable");
            let app = wide(exe.as_os_str());
            let mut command = wide(OsStr::new(&format!(
                "\"{}\" --exact wasm_worker_process::platform::w06_limit_tests::limit_helper --ignored",
                exe.display()
            )));
            let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
            startup.StartupInfo.cb =
                size_of::<windows_sys::Win32::System::Threading::STARTUPINFOW>() as u32;
            let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
            let flags = CREATE_SUSPENDED
                | DETACHED_PROCESS
                | if breakaway {
                    CREATE_BREAKAWAY_FROM_JOB
                } else {
                    0
                };
            let created = unsafe {
                CreateProcessW(
                    app.as_ptr(),
                    command.as_mut_ptr(),
                    null(),
                    null(),
                    0,
                    flags,
                    null_mut(),
                    null(),
                    &startup.StartupInfo,
                    &mut info,
                )
            };
            let error = if created == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            if created == 0 {
                publish(
                    &path,
                    &serde_json::json!({"created": false, "error": error, "flags": flags, "nonce": nonce, "helper_pid": helper_pid}),
                );
                return;
            }
            let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
            let _thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
            let alive = unsafe { WaitForSingleObject(raw(&process), 0) };
            let mut member = 0;
            let queried =
                unsafe { IsProcessInJob(raw(&process), job_number as HANDLE, &mut member) };
            let query_error = if queried == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            let killed = unsafe { TerminateProcess(raw(&process), 1) };
            let kill_error = if killed == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            let waited = unsafe { WaitForSingleObject(raw(&process), 5000) };
            if killed == 0 || waited != WAIT_OBJECT_0 {
                eprintln!("W06 limit child cleanup_unknown; preserving child handle");
                loop {
                    thread::park_timeout(Duration::from_secs(1));
                }
            }
            publish(
                &path,
                &serde_json::json!({"created": true, "pid": info.dwProcessId, "flags": flags, "nonce": nonce, "helper_pid": helper_pid,
                "alive_wait": alive, "membership_query": queried, "member": member,
                "query_error": query_error, "terminate": killed, "terminate_error": kill_error, "wait": waited}),
            );
        }

        fn case(
            root: &Path,
            label: &str,
            active: u32,
            breakaway_allowed: bool,
            attempt_breakaway: bool,
        ) -> serde_json::Value {
            let receipt = root.join(format!("{label}.json"));
            assert!(
                !receipt.exists() && !receipt.with_extension("tmp").exists(),
                "fresh limit fixture"
            );
            let job =
                unsafe { own(CreateJobObjectW(null(), null()), "limit_job") }.expect("test job");
            // The explicit handle list is test-control plumbing, not a production launch.
            assert_ne!(
                unsafe {
                    SetHandleInformation(raw(&job), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
                },
                0
            );
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_PROCESS_TIME
                | if breakaway_allowed {
                    JOB_OBJECT_LIMIT_BREAKAWAY_OK
                } else {
                    0
                };
            limits.BasicLimitInformation.ActiveProcessLimit = active;
            limits.BasicLimitInformation.PerProcessUserTimeLimit = 30_000_000;
            limits.ProcessMemoryLimit = 128 * 1024 * 1024;
            assert_ne!(
                unsafe {
                    SetInformationJobObject(
                        raw(&job),
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    )
                },
                0
            );
            let mut actual: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            assert_ne!(
                unsafe {
                    QueryInformationJobObject(
                        raw(&job),
                        JobObjectExtendedLimitInformation,
                        (&mut actual as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                        null_mut(),
                    )
                },
                0
            );
            assert_eq!(
                actual.BasicLimitInformation.LimitFlags,
                limits.BasicLimitInformation.LimitFlags
            );
            assert_eq!(actual.BasicLimitInformation.ActiveProcessLimit, active);
            assert_eq!(
                actual.BasicLimitInformation.PerProcessUserTimeLimit,
                limits.BasicLimitInformation.PerProcessUserTimeLimit
            );
            assert_eq!(actual.ProcessMemoryLimit, limits.ProcessMemoryLimit);
            let exe = std::env::current_exe().expect("fixed test executable");
            let app = wide(exe.as_os_str());
            let mut command = wide(OsStr::new(&format!(
                "\"{}\" --exact wasm_worker_process::platform::w06_limit_tests::limit_helper --ignored --nocapture",
                exe.display()
            )));
            let jobs = [raw(&job)];
            let handles = [raw(&job)];
            let mut attributes = Attributes::new().expect("limit attributes");
            attributes
                .set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)
                .expect("limit job list");
            attributes
                .set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)
                .expect("limit control handle list");
            let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.lpAttributeList = attributes.list;
            let keys = [
                "NHC_W06_LIMIT_RECEIPT",
                "NHC_W06_LIMIT_JOB",
                "NHC_W06_LIMIT_BREAKAWAY",
                "NHC_W06_LIMIT_NONCE",
            ];
            let mut vars: Vec<(OsString, OsString)> = std::env::vars_os()
                .filter(|(k, _)| {
                    !keys
                        .iter()
                        .any(|key| k.to_string_lossy().eq_ignore_ascii_case(key))
                })
                .collect();
            vars.push((keys[0].into(), receipt.as_os_str().to_owned()));
            vars.push((keys[1].into(), (raw(&job) as usize).to_string().into()));
            vars.push((
                keys[2].into(),
                if attempt_breakaway { "1" } else { "0" }.into(),
            ));
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            vars.push((keys[3].into(), nonce.clone().into()));
            vars.sort_by_key(|(k, _)| k.to_string_lossy().to_uppercase());
            let mut environment = Vec::<u16>::new();
            for (key, value) in vars {
                environment.extend(key.encode_wide());
                environment.push(b'=' as u16);
                environment.extend(value.encode_wide());
                environment.push(0);
            }
            environment.push(0);
            let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
            let created = unsafe {
                CreateProcessW(
                    app.as_ptr(),
                    command.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    CREATE_SUSPENDED
                        | DETACHED_PROCESS
                        | CREATE_UNICODE_ENVIRONMENT
                        | EXTENDED_STARTUPINFO_PRESENT,
                    environment.as_mut_ptr().cast(),
                    null(),
                    &startup.StartupInfo,
                    &mut info,
                )
            };
            assert_ne!(
                created,
                0,
                "test helper not established: {:?}",
                std::io::Error::last_os_error()
            );
            let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
            let thread_handle = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
            let mut member = 0;
            if unsafe { IsProcessInJob(raw(&process), raw(&job), &mut member) } == 0
                || member == 0
                || unsafe { ResumeThread(raw(&thread_handle)) } == u32::MAX
            {
                unsafe { TerminateProcess(raw(&process), 1) };
                if unsafe { WaitForSingleObject(raw(&process), 5000) } != WAIT_OBJECT_0 {
                    loop {
                        thread::park_timeout(Duration::from_secs(1));
                    }
                }
                panic!("test helper membership/resume not established");
            }
            // On timeout keep both helper and Job alive. A breakaway child may exist;
            // killing the helper would discard its only guaranteed child handle.
            if unsafe { WaitForSingleObject(raw(&process), 10000) } != WAIT_OBJECT_0 {
                eprintln!("W06 limit helper cleanup_unknown; retain Job and process");
                loop {
                    thread::park_timeout(Duration::from_secs(1));
                }
            }
            let mut exit_code = 0;
            if unsafe { GetExitCodeProcess(raw(&process), &mut exit_code) } == 0
                || exit_code != 0
                || !receipt.is_file()
            {
                // A helper failure may have occurred after a breakaway child was
                // created but before its cleanup receipt. Do not assert cleanup.
                eprintln!("W06 limit helper cleanup_unknown: missing trusted receipt");
                loop {
                    thread::park_timeout(Duration::from_secs(1));
                }
            }
            let value: serde_json::Value = match std::fs::read(&receipt)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            {
                Some(value) => value,
                None => {
                    eprintln!("W06 limit helper cleanup_unknown: unreadable receipt");
                    loop {
                        thread::park_timeout(Duration::from_secs(1));
                    }
                }
            };
            let expected_flags = CREATE_SUSPENDED
                | DETACHED_PROCESS
                | if attempt_breakaway {
                    CREATE_BREAKAWAY_FROM_JOB
                } else {
                    0
                };
            if value["nonce"] != nonce
                || value["helper_pid"].as_u64() != Some(info.dwProcessId as u64)
                || value["flags"].as_u64() != Some(expected_flags as u64)
            {
                eprintln!("W06 limit helper cleanup_unknown: receipt identity mismatch");
                loop {
                    thread::park_timeout(Duration::from_secs(1));
                }
            }
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
            assert_ne!(
                unsafe {
                    QueryInformationJobObject(
                        raw(&job),
                        JobObjectBasicAccountingInformation,
                        (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                        size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                        null_mut(),
                    )
                },
                0
            );
            assert_eq!(accounting.ActiveProcesses, 0, "test Job must be empty");
            println!(
                "W06 limit {label} active={active} breakaway_allowed={breakaway_allowed} attempt_breakaway={attempt_breakaway} actual_flags={} receipt={value}",
                actual.BasicLimitInformation.LimitFlags
            );
            value
        }

        fn success(value: &serde_json::Value, member: bool) {
            assert_eq!(
                value["created"], true,
                "positive creation was not established: {value}"
            );
            assert_eq!(value["alive_wait"], WAIT_TIMEOUT);
            assert_eq!(value["membership_query"], 1);
            assert_eq!(value["member"], if member { 1 } else { 0 });
            assert_eq!(value["terminate"], 1);
            assert_eq!(value["wait"], WAIT_OBJECT_0);
        }

        #[test]
        #[ignore = "requires controlled single-factor active-process Job fixture"]
        fn active_limit_single_factor() {
            let root = PathBuf::from(std::env::var_os("NHC_W06_ROOT").expect("fresh W06 root"));
            let control = case(&root, "active-control", 2, false, false);
            success(&control, true);
            let restricted = case(&root, "active-one", 1, false, false);
            assert_eq!(
                restricted["created"], false,
                "active1 allowed child creation: {restricted}"
            );
        }

        #[test]
        #[ignore = "requires controlled single-factor breakaway Job fixture"]
        fn breakaway_single_factor() {
            let root = PathBuf::from(std::env::var_os("NHC_W06_ROOT").expect("fresh W06 root"));
            // Active=2 in both cases removes active1 as a competing cause.
            let control = case(&root, "breakaway-control", 2, true, true);
            success(&control, false);
            let restricted = case(&root, "breakaway-denied", 2, false, true);
            assert_eq!(
                restricted["created"], false,
                "breakaway unexpectedly allowed: {restricted}"
            );
        }
    }

    #[cfg(test)]
    mod w06_handle_tests {
        use super::*;
        use std::io::{BufRead, Write};
        use windows_sys::Win32::{
            Foundation::{CompareObjectHandles, DUPLICATE_SAME_ACCESS, DuplicateHandle},
            System::Pipes::CreatePipe,
        };

        #[test]
        #[ignore = "requires controlled isolated process or artifact fixture"]
        fn positive_helper() {
            std::env::var_os("NHC_W06_POSITIVE_HELPER").expect("positive helper environment");
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .expect("sentinel request");
            let (nonce, number) = line.trim().split_once(':').expect("sentinel message");
            assert_eq!(nonce.len(), 32);
            let candidate: usize = number.parse().expect("sentinel numeric handle");
            assert_ne!(
                unsafe { windows_sys::Win32::System::Threading::SetEvent(candidate as HANDLE) },
                0
            );
            line.clear();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .expect("sentinel release");
            assert_eq!(line.trim(), "release");
        }

        struct PositiveProcess(OwnedHandle);
        impl Drop for PositiveProcess {
            fn drop(&mut self) {
                if unsafe { WaitForSingleObject(raw(&self.0), 0) } != WAIT_OBJECT_0 {
                    unsafe { TerminateProcess(raw(&self.0), 1) };
                }
                if unsafe { WaitForSingleObject(raw(&self.0), 5000) } != WAIT_OBJECT_0 {
                    eprintln!("W06 positive launcher cleanup unknown");
                    loop {
                        thread::park_timeout(Duration::from_secs(1));
                    }
                }
            }
        }

        #[test]
        #[ignore = "requires controlled isolated process or artifact fixture"]
        fn sentinel_explicit_inheritance_control() {
            let mut security = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: null_mut(),
                bInheritHandle: 1,
            };
            let event = unsafe { own(CreateEventW(&mut security, 1, 0, null()), "positive_event") }
                .expect("positive sentinel");
            let mut event_flags = 0;
            assert_ne!(
                unsafe {
                    windows_sys::Win32::Foundation::GetHandleInformation(
                        raw(&event),
                        &mut event_flags,
                    )
                },
                0
            );
            assert_ne!(event_flags & HANDLE_FLAG_INHERIT, 0);
            let mut child_in = null_mut();
            let mut parent_in = null_mut();
            let mut parent_out = null_mut();
            let mut child_out = null_mut();
            if unsafe { CreatePipe(&mut child_in, &mut parent_in, &mut security, 0) } == 0 {
                panic!(
                    "W06 positive stdin pipe not established: {:?}",
                    std::io::Error::last_os_error().raw_os_error()
                );
            }
            let child_in = unsafe { OwnedHandle::from_raw_handle(child_in) };
            let parent_in = unsafe { OwnedHandle::from_raw_handle(parent_in) };
            if unsafe { CreatePipe(&mut parent_out, &mut child_out, &mut security, 0) } == 0 {
                panic!(
                    "W06 positive stdout pipe not established: {:?}",
                    std::io::Error::last_os_error().raw_os_error()
                );
            }
            let parent_out = unsafe { OwnedHandle::from_raw_handle(parent_out) };
            let child_out = unsafe { OwnedHandle::from_raw_handle(child_out) };
            assert_ne!(
                unsafe { SetHandleInformation(raw(&parent_in), HANDLE_FLAG_INHERIT, 0) },
                0
            );
            assert_ne!(
                unsafe { SetHandleInformation(raw(&parent_out), HANDLE_FLAG_INHERIT, 0) },
                0
            );
            let handles = [raw(&event), raw(&child_in), raw(&child_out)];
            let mut attributes = Attributes::new().expect("positive attributes");
            attributes
                .set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)
                .expect("positive handle list");
            let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = raw(&child_in);
            startup.StartupInfo.hStdOutput = raw(&child_out);
            startup.StartupInfo.hStdError = raw(&child_out);
            startup.lpAttributeList = attributes.list;
            let exe = std::env::current_exe().unwrap();
            let app = wide(exe.as_os_str());
            let mut command = wide(OsStr::new(&format!(
                "\"{}\" --exact wasm_worker_process::platform::w06_handle_tests::positive_helper --ignored --nocapture",
                exe.display()
            )));
            let mut vars: Vec<(OsString, OsString)> = std::env::vars_os()
                .filter(|(key, _)| {
                    !key.to_string_lossy()
                        .eq_ignore_ascii_case("NHC_W06_POSITIVE_HELPER")
                })
                .collect();
            vars.push((
                OsString::from("NHC_W06_POSITIVE_HELPER"),
                OsString::from("1"),
            ));
            vars.sort_by_key(|(key, _)| key.to_string_lossy().to_uppercase());
            let mut environment = Vec::<u16>::new();
            for (key, value) in vars {
                environment.extend(key.encode_wide());
                environment.push(b'=' as u16);
                environment.extend(value.encode_wide());
                environment.push(0);
            }
            environment.push(0);
            let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
            let created = unsafe {
                CreateProcessW(
                    app.as_ptr(),
                    command.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    DETACHED_PROCESS | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                    environment.as_mut_ptr().cast(),
                    null(),
                    &startup.StartupInfo,
                    &mut info,
                )
            };
            if created == 0 {
                panic!(
                    "W06 positive launcher not established: {:?}",
                    std::io::Error::last_os_error().raw_os_error()
                );
            }
            let process = PositiveProcess(unsafe { OwnedHandle::from_raw_handle(info.hProcess) });
            let _thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
            drop(child_in);
            drop(child_out);
            let mut input = File::from(parent_in);
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            writeln!(input, "{nonce}:{}", raw(&event) as usize).unwrap();
            input.flush().unwrap();
            assert_eq!(
                unsafe { WaitForSingleObject(raw(&event), 3000) },
                WAIT_OBJECT_0,
                "explicit inheritance control did not signal"
            );
            let mut duplicated = null_mut();
            assert_ne!(
                unsafe {
                    DuplicateHandle(
                        raw(&process.0),
                        raw(&event),
                        GetCurrentProcess(),
                        &mut duplicated,
                        0,
                        0,
                        DUPLICATE_SAME_ACCESS,
                    )
                },
                0,
                "positive sentinel duplicate"
            );
            let duplicated = unsafe { OwnedHandle::from_raw_handle(duplicated) };
            assert_ne!(
                unsafe { CompareObjectHandles(raw(&event), raw(&duplicated)) },
                0,
                "positive control must compare same object"
            );
            writeln!(input, "release").unwrap();
            input.flush().unwrap();
            assert_eq!(
                unsafe { WaitForSingleObject(raw(&process.0), 3000) },
                WAIT_OBJECT_0
            );
            let mut exit_code = 0;
            assert_ne!(
                unsafe { GetExitCodeProcess(raw(&process.0), &mut exit_code) },
                0
            );
            assert_eq!(exit_code, 0, "positive helper failed");
            println!(
                "W06 explicit inheritance positive actual same-object event pid={}",
                info.dwProcessId
            );
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
                            {
                                test_hooks::w05_mark_stopper();
                                test_hooks::hold_stopper();
                            }
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
    struct P01Monitor {
        stop: Arc<AtomicBool>,
        target: Arc<Mutex<Option<OwnedHandle>>>,
        finished: std::sync::mpsc::Receiver<()>,
        thread: Option<thread::JoinHandle<()>>,
    }
    #[cfg(test)]
    impl P01Monitor {
        fn start(job: Arc<OwnedHandle>) -> Result<Self, WorkerError> {
            use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
            use windows_sys::Win32::System::IO::CreateIoCompletionPort;
            use windows_sys::Win32::System::JobObjects::{
                JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JobObjectAssociateCompletionPortInformation,
            };
            P01_START.get_or_init(Instant::now);
            let port = unsafe {
                own(
                    CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1),
                    "p01_port",
                )?
            };
            let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
                CompletionKey: std::ptr::dangling_mut::<c_void>(),
                CompletionPort: raw(&port),
            };
            if unsafe {
                SetInformationJobObject(
                    raw(&job),
                    JobObjectAssociateCompletionPortInformation,
                    (&association as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
                    size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
                )
            } == 0
            {
                return Err(failure("p01_associate"));
            }
            let stop = Arc::new(AtomicBool::new(false));
            let stop_thread = stop.clone();
            let target = Arc::new(Mutex::new(None));
            let target_thread = target.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
            let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
            let thread = thread::Builder::new()
                .name("nhc-w2-p01-observer".into())
                .spawn(move || {
                    use std::collections::HashMap;
                    use windows_sys::Win32::System::IO::GetQueuedCompletionStatus;
                    let mut handles = HashMap::<u32, OwnedHandle>::new();
                    let mut scan = 0u64;
                    record_stage("p01_collector_ready", None);
                    let _ = ready_tx.send(());
                    loop {
                        let mut message = 0u32;
                        let mut key = 0usize;
                        let mut data = null_mut();
                        let result = unsafe {
                            GetQueuedCompletionStatus(
                                raw(&port),
                                &mut message,
                                &mut key,
                                &mut data,
                                50,
                            )
                        };
                        let error = if result == 0 {
                            std::io::Error::last_os_error().raw_os_error()
                        } else {
                            None
                        };
                        record_p01(&serde_json::json!({
                            "kind":"dequeue", "ok":result != 0, "error":error,
                            "message":message, "key":key, "raw_data":data as usize,
                            "observed_unix_ms":p01_timestamp()
                        }));
                        if result != 0 && key == 1 {
                            let data_value = data as usize;
                            if matches!(message, 2 | 6 | 7 | 8 | 9) {
                                let pid = data_value as u32;
                                if message == 6 && pid != 0 {
                                    p01_open("notification", pid, &mut handles);
                                }
                                p01_observe(
                                    &job,
                                    "notification",
                                    message,
                                    pid,
                                    handles.get(&pid),
                                    &target_thread,
                                    0,
                                );
                            } else {
                                record_p01(&serde_json::json!({
                                    "kind":"nonprocess_notification", "message":message,
                                    "raw_data":data_value, "observed_unix_ms":p01_timestamp()
                                }));
                            }
                        } else if result == 0 {
                            if error != Some(WAIT_TIMEOUT as i32) {
                                record_stage("p01_dequeue_failed", None);
                                break;
                            }
                            if stop_thread.load(Ordering::Acquire) {
                                break;
                            }
                        } else {
                            record_stage("p01_bad_completion_key", None);
                            break;
                        }
                        scan += 1;
                        p01_scan_job(&job, &mut handles, &target_thread, scan);
                    }
                    record_stage("p01_collector_stopped", None);
                    let _ = done_tx.send(());
                })
                .map_err(|_| WorkerError::IsolationUnavailable("p01_thread", None))?;
            let monitor = Self {
                stop,
                target,
                finished: done_rx,
                thread: Some(thread),
            };
            if ready_rx.recv_timeout(Duration::from_secs(1)).is_err() {
                record_stage("p01_not_ready", None);
                drop(monitor);
                return Err(WorkerError::IsolationUnavailable("p01_not_ready", None));
            }
            Ok(monitor)
        }
        fn set_target(&self, process: &OwnedHandle) -> Result<(), WorkerError> {
            use windows_sys::Win32::Foundation::DuplicateHandle;
            let current = unsafe { GetCurrentProcess() };
            let mut duplicate = null_mut();
            if unsafe {
                DuplicateHandle(
                    current,
                    raw(process),
                    current,
                    &mut duplicate,
                    0x0010_1000,
                    0,
                    0,
                )
            } == 0
            {
                return Err(failure("p01_target_duplicate"));
            }
            let handle = unsafe { OwnedHandle::from_raw_handle(duplicate) };
            *self.target.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
            Ok(())
        }
    }
    #[cfg(test)]
    impl Drop for P01Monitor {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                match self.finished.recv_timeout(Duration::from_secs(1)) {
                    Ok(()) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        record_stage("p01_collector_failed", None)
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        record_stage("p01_collector_shutdown_unknown", None);
                        while self.finished.recv_timeout(Duration::from_secs(1)).is_err() {
                            if thread.is_finished() {
                                break;
                            }
                        }
                    }
                }
                if thread.join().is_err() {
                    record_stage("p01_collector_failed", None);
                }
            }
        }
    }

    #[cfg(test)]
    fn p01_timestamp() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    }

    #[cfg(test)]
    fn p01_open(
        origin: &'static str,
        pid: u32,
        handles: &mut std::collections::HashMap<u32, OwnedHandle>,
    ) {
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };
        if handles.contains_key(&pid) {
            return;
        }
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if handle.is_null() {
            let error = std::io::Error::last_os_error().raw_os_error();
            record_p01(
                &serde_json::json!({"kind":"open_failed", "origin":origin, "pid":pid, "error":error, "observed_unix_ms":p01_timestamp()}),
            );
        } else {
            handles.insert(pid, unsafe { OwnedHandle::from_raw_handle(handle) });
        }
    }

    #[cfg(test)]
    fn p01_scan_job(
        job: &OwnedHandle,
        handles: &mut std::collections::HashMap<u32, OwnedHandle>,
        target: &Arc<Mutex<Option<OwnedHandle>>>,
        scan: u64,
    ) {
        use windows_sys::Win32::System::JobObjects::JobObjectBasicProcessIdList;
        #[repr(C)]
        struct ProcessList {
            assigned: u32,
            count: u32,
            ids: [usize; 64],
        }
        let target_before = {
            let guard = target.lock().unwrap_or_else(|e| e.into_inner());
            guard.as_ref().map(|h| p01_status(job, h))
        };
        let mut list = ProcessList {
            assigned: 0,
            count: 0,
            ids: [0; 64],
        };
        let ok = unsafe {
            QueryInformationJobObject(
                raw(job),
                JobObjectBasicProcessIdList,
                (&mut list as *mut ProcessList).cast(),
                size_of::<ProcessList>() as u32,
                null_mut(),
            )
        };
        let error = if ok == 0 {
            std::io::Error::last_os_error().raw_os_error()
        } else {
            None
        };
        let ids = list.ids[..(list.count as usize).min(list.ids.len())].to_vec();
        record_p01(&serde_json::json!({
            "kind":"list_before_open", "scan":scan, "ok":ok != 0, "error":error,
            "assigned":list.assigned, "count":list.count, "ids":ids,
            "complete":ok != 0 && list.count as usize <= list.ids.len() && list.assigned <= list.count,
            "observed_unix_ms":p01_timestamp()
        }));
        if ok == 0 || list.count as usize > list.ids.len() || list.assigned > list.count {
            record_p01(
                &serde_json::json!({"kind":"list_failed", "error":error, "assigned":list.assigned, "count":list.count, "observed_unix_ms":p01_timestamp()}),
            );
            return;
        }
        for raw_pid in &list.ids[..list.count as usize] {
            let pid = *raw_pid as u32;
            p01_open("job_list", pid, handles);
            p01_observe(job, "job_list", 0, pid, handles.get(&pid), target, scan);
        }
        let target_after = {
            let guard = target.lock().unwrap_or_else(|e| e.into_inner());
            guard.as_ref().map(|h| p01_status(job, h))
        };
        record_p01(&serde_json::json!({
            "kind":"scan_complete", "scan":scan, "ids":ids,
            "target_before":target_before, "target_after":target_after,
            "observed_unix_ms":p01_timestamp()
        }));
    }

    #[cfg(test)]
    fn p01_status(job: &OwnedHandle, handle: &OwnedHandle) -> serde_json::Value {
        let wait = unsafe { WaitForSingleObject(raw(handle), 0) };
        let wait_error = if wait == windows_sys::Win32::Foundation::WAIT_FAILED {
            std::io::Error::last_os_error().raw_os_error()
        } else {
            None
        };
        let mut member = 0;
        let member_ok = unsafe { IsProcessInJob(raw(handle), raw(job), &mut member) };
        let member_error = if member_ok == 0 {
            std::io::Error::last_os_error().raw_os_error()
        } else {
            None
        };
        serde_json::json!({"wait":wait,"wait_error":wait_error,"member_ok":member_ok != 0,"member":member,"member_error":member_error})
    }

    #[cfg(test)]
    fn p01_observe(
        job: &OwnedHandle,
        origin: &'static str,
        message: u32,
        pid: u32,
        handle: Option<&OwnedHandle>,
        target: &Arc<Mutex<Option<OwnedHandle>>>,
        scan: u64,
    ) {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::JobObjects::JobObjectBasicProcessIdList;
        use windows_sys::Win32::System::Threading::{GetProcessId, GetProcessTimes};
        #[repr(C)]
        struct ProcessList {
            assigned: u32,
            count: u32,
            ids: [usize; 64],
        }
        let start_us = P01_START.get_or_init(Instant::now).elapsed().as_micros();
        let target_guard = target.lock().unwrap_or_else(|e| e.into_inner());
        let target_before = target_guard.as_ref().map(|h| p01_status(job, h));
        let process_before = handle.map(|h| p01_status(job, h));
        let observed_unix_ms = p01_timestamp();
        let mut list = ProcessList {
            assigned: 0,
            count: 0,
            ids: [0; 64],
        };
        let list_ok = unsafe {
            QueryInformationJobObject(
                raw(job),
                JobObjectBasicProcessIdList,
                (&mut list as *mut ProcessList).cast(),
                size_of::<ProcessList>() as u32,
                null_mut(),
            )
        };
        let list_error = if list_ok == 0 {
            std::io::Error::last_os_error().raw_os_error()
        } else {
            None
        };
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        let accounting_ok = unsafe {
            QueryInformationJobObject(
                raw(job),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        };
        let accounting_error = if accounting_ok == 0 {
            std::io::Error::last_os_error().raw_os_error()
        } else {
            None
        };
        let ids: Vec<usize> = list.ids[..(list.count as usize).min(list.ids.len())].to_vec();
        let mut observed = serde_json::json!({
            "kind":origin, "message":message, "pid":pid, "scan":scan, "observed_unix_ms":observed_unix_ms,
            "start_monotonic_us":start_us, "target_before":target_before, "process_before":process_before,
            "retained_handle":handle.is_some(), "list_ok":list_ok != 0, "list_error":list_error,
            "list_complete":list_ok != 0 && list.count as usize <= list.ids.len() && list.assigned <= list.count,
            "list_assigned":list.assigned, "list_count":list.count, "list_ids":ids,
            "accounting_ok":accounting_ok != 0, "accounting_error":accounting_error,
            "total":accounting.TotalProcesses, "active":accounting.ActiveProcesses, "terminated":accounting.TotalTerminatedProcesses,
        });
        if let Some(handle) = handle {
            let mut creation: FILETIME = unsafe { zeroed() };
            let mut exit: FILETIME = unsafe { zeroed() };
            let mut kernel: FILETIME = unsafe { zeroed() };
            let mut user: FILETIME = unsafe { zeroed() };
            let times_ok = unsafe {
                GetProcessTimes(
                    raw(handle),
                    &mut creation,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )
            };
            let times_error = if times_ok == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            let mut image = vec![0u16; 32768];
            let mut length = image.len() as u32;
            let image_ok = unsafe {
                QueryFullProcessImageNameW(raw(handle), 0, image.as_mut_ptr(), &mut length)
            };
            let image_error = if image_ok == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            let image_path = if image_ok != 0 {
                Some(
                    OsString::from_wide(&image[..length as usize])
                        .to_string_lossy()
                        .into_owned(),
                )
            } else {
                None
            };
            let mut member = 0;
            let member_ok = unsafe { IsProcessInJob(raw(handle), raw(job), &mut member) };
            let member_error = if member_ok == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            let mut any_job = 0;
            let any_ok = unsafe { IsProcessInJob(raw(handle), null_mut(), &mut any_job) };
            let any_error = if any_ok == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            let handle_pid = unsafe { GetProcessId(raw(handle)) };
            let handle_pid_error = if handle_pid == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            observed["handle_pid"] = serde_json::json!(handle_pid);
            observed["handle_pid_error"] = serde_json::json!(handle_pid_error);
            observed["creation_filetime"] = serde_json::json!(
                ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64
            );
            observed["times_ok"] = serde_json::json!(times_ok != 0);
            observed["times_error"] = serde_json::json!(times_error);
            observed["image"] = serde_json::json!(image_path);
            observed["image_error"] = serde_json::json!(image_error);
            observed["member_ok"] = serde_json::json!(member_ok != 0);
            observed["member"] = serde_json::json!(member);
            observed["member_error"] = serde_json::json!(member_error);
            observed["any_job_ok"] = serde_json::json!(any_ok != 0);
            observed["any_job"] = serde_json::json!(any_job);
            observed["any_job_error"] = serde_json::json!(any_error);
            observed["wait"] = serde_json::json!(unsafe { WaitForSingleObject(raw(handle), 0) });
        }
        observed["process_after"] = serde_json::json!(handle.map(|h| p01_status(job, h)));
        observed["target_after"] =
            serde_json::json!(target_guard.as_ref().map(|h| p01_status(job, h)));
        observed["end_monotonic_us"] =
            serde_json::json!(P01_START.get_or_init(Instant::now).elapsed().as_micros());
        record_p01(&observed);
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
                #[cfg(test)]
                if write.pending && test_hooks::armed("w05_stdin_pending") {
                    w05_checkpoint("w05_stdin_pending", Some(process.pid), gate, deadline);
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

    #[cfg(test)]
    fn w05_checkpoint(phase: &'static str, pid: Option<u32>, gate: &Gate, deadline: Instant) {
        if !test_hooks::armed(phase) {
            return;
        }
        if gate.stopped(deadline) || test_hooks::w05_stopper_started() {
            record_stage("w05_gate_or_stopper_closed", pid);
            return;
        }
        record_stage("w05_gate_open", pid);
        test_hooks::checkpoint(phase, pid);
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
        let p01_armed = test_hooks::take_p01_events();
        #[cfg(test)]
        let old_events_armed = test_hooks::take_job_events();
        #[cfg(test)]
        if p01_armed && old_events_armed {
            return Err(WorkerError::IsolationUnavailable(
                "p01_conflicting_observer",
                None,
            ));
        }
        #[cfg(test)]
        let _job_events = if old_events_armed {
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
        #[cfg(test)]
        test_observer::capture_job(&job);
        #[cfg(test)]
        if test_hooks::armed("w05_before_create") || test_hooks::armed("w05_suspended") {
            use windows_sys::Win32::Foundation::GetHandleInformation;
            let mut handle_flags = 0u32;
            if unsafe { GetHandleInformation(raw(&job), &mut handle_flags) } == 0 {
                return Err(failure("w05_job_handle_flags"));
            }
            record_count("w05_job_handle_flags", 0, handle_flags as usize);
            record_count(
                "w05_job_limit_flags",
                0,
                actual.BasicLimitInformation.LimitFlags as usize,
            );
            record_count(
                "w05_job_active_limit",
                0,
                actual.BasicLimitInformation.ActiveProcessLimit as usize,
            );
            let mut host_in_job = 0;
            if unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut host_in_job) } == 0 {
                return Err(failure("w05_host_job_query"));
            }
            record_count("w05_host_in_job", 0, host_in_job as usize);
        }
        #[cfg(test)]
        let _p01_monitor = if p01_armed {
            let mut host_in_job = 0;
            let host_job_ok =
                unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut host_in_job) };
            let host_job_error = if host_job_ok == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            record_p01(&serde_json::json!({
                "kind":"configuration", "host_pid":unsafe { GetCurrentProcessId() },
                "host_in_any_job_query_ok":host_job_ok != 0, "host_in_any_job":host_in_job,
                "host_in_any_job_error":host_job_error, "active_limit":actual.BasicLimitInformation.ActiveProcessLimit,
                "limit_flags":actual.BasicLimitInformation.LimitFlags, "observed_unix_ms":p01_timestamp()
            }));
            Some(P01Monitor::start(job.clone())?)
        } else {
            None
        };
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
        let creation_flags = CREATE_SUSPENDED
            | DETACHED_PROCESS
            | CREATE_UNICODE_ENVIRONMENT
            | EXTENDED_STARTUPINFO_PRESENT;
        record_count("create_flags", 0, creation_flags as usize);
        #[cfg(test)]
        w05_checkpoint("w05_before_create", None, gate, deadline);
        let created = unsafe {
            CreateProcessW(
                app.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                creation_flags,
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
        #[cfg(test)]
        if let Some(monitor) = &_p01_monitor {
            monitor.set_target(&process.process)?;
        }
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
        #[cfg(test)]
        if let Some(monitor) = &_p01_monitor {
            p01_observe(
                &job,
                "host_suspended",
                0,
                information.dwProcessId,
                Some(&process.process),
                &monitor.target,
                0,
            );
        }
        image.verify_process(raw(&process.process), &worker.digest)?;
        #[cfg(test)]
        w05_checkpoint(
            "w05_suspended",
            Some(information.dwProcessId),
            gate,
            deadline,
        );
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
        #[cfg(test)]
        w05_checkpoint("w05_resumed", Some(information.dwProcessId), gate, deadline);
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
            CREATE_NO_WINDOW, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
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
        #[ignore = "requires controlled isolated process or artifact fixture"]
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
