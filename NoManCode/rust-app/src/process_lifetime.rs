//! Internal, per-domain Windows process-tree lifetime supervision.
//!
//! This module does not authorize commands or own a data domain.  The caller
//! validates its program, arguments, working directory and environment, and
//! supplies the domain lease. A supervisor must be shared by all Shell/Git
//! work belonging to one controller; creating one per call defeats its bound.

use std::{
    ffi::OsString,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// C's adapter must synchronously close admission on `seal_unknown`; it must
/// not re-enter this state machine. `settle` consumes the adapter and actual
/// ticket exactly once, but may block (for example, a last-owner thread join).
/// It must not reopen admission or erase an already recorded unknown. Both
/// callbacks must be panic-free for this interface to uphold its contract.
pub(crate) trait DomainLease: Send + Sync + 'static {
    fn seal_unknown(&mut self);
    fn settle(self: Box<Self>);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverflowPolicy {
    /// Retain only the configured prefix, but continue reading to actual EOF.
    TruncateAndDrain,
    /// Terminate the job when either limit is exceeded; still prove cleanup.
    Terminate,
}

/// All fields are supplied by trusted, already-authorized internal callers.
/// `environment` is the complete child environment, not a list of overrides.
pub(crate) struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
    pub overflow: OverflowPolicy,
    pub deadline: Instant,
    /// The caller's existing cleanup budget; never greater than five seconds.
    pub cleanup_budget: Duration,
    #[cfg(all(test, windows))]
    pub(crate) test_hooks: Option<Arc<TestHooks>>,
}

impl ProcessSpec {
    /// Callers must supply their already-authorized complete environment and
    /// explicitly select their existing output limits and overflow behavior.
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
            #[cfg(all(test, windows))]
            test_hooks: None,
        }
    }
}

#[cfg(all(test, windows))]
#[derive(Default)]
pub(crate) struct TestHooks {
    pub fail_stdout_reader: AtomicBool,
    pub hold_stdout_reader: AtomicBool,
    pub stdout_reader_held: AtomicBool,
    pub release_stdout_reader: AtomicBool,
    pub fail_membership: AtomicBool,
    pub fail_job_query: AtomicBool,
    pub fail_exit_code: AtomicBool,
    pub fail_termination: AtomicBool,
    pub hold_created: AtomicBool,
    pub created_held: AtomicBool,
    pub release_created: AtomicBool,
    pub root_signaled: AtomicBool,
    pub job_empty: AtomicBool,
    pub stdout_failed_joined: AtomicBool,
    pub hold_watchdog_exit: AtomicBool,
    pub watchdog_held: AtomicBool,
    pub release_watchdog: AtomicBool,
    pub hold_before_reader_join: AtomicBool,
    pub before_reader_join_held: AtomicBool,
    pub release_reader_join: AtomicBool,
    pub stdout_reader_finished: AtomicBool,
}

#[derive(Debug)]
pub(crate) struct Capture {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionReason {
    Exited,
    Deadline,
    Cancelled,
    OutputLimit,
}

/// Constructed only after the original process signaled, its job was queried
/// with zero active processes, and both original readers observed actual EOF.
#[derive(Debug)]
pub(crate) struct Settled {
    pub reason: CompletionReason,
    pub identity: ProcessIdentity,
    pub exit_code: u32,
    pub stdout: Capture,
    pub stderr: Capture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub pid: u32,
    pub creation_filetime: u64,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    /// Proven failure before any target instruction could run.
    NotStarted {
        stage: &'static str,
        os_error: Option<i32>,
    },
    Settled(Settled),
    /// Admission was sealed before notification. The holder retains the lease
    /// and objects until their later cleanup is actually proved; this outcome
    /// remains unknown even if that cleanup has completed by the time it is read.
    Unknown,
}

/// A synchronous rejection before any worker or OS operation was submitted.
/// The caller receives its unchanged lease and must settle the unstarted work.
pub(crate) struct NotStarted {
    pub reason: &'static str,
    pub lease: Box<dyn DomainLease>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pending,
    Unknown,
    Settling,
    UnknownSettling,
    Settled,
}

struct LeaseState {
    phase: Phase,
    lease: Option<Box<dyn DomainLease>>,
    outcome: Option<Outcome>,
}

struct LeaseCell(Mutex<LeaseState>);

impl LeaseCell {
    fn decide_unknown(&self) -> bool {
        // A poisoned lock is recovered rather than allowing cancellation to
        // skip the fail-closed callback. C's callbacks must not panic.
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match state.phase {
            Phase::Pending => {
                // The mutex makes sealing and terminal adoption one decision.
                // The adapter's callback is required not to panic or re-enter.
                state.lease.as_mut().expect("pending lease").seal_unknown();
                state.phase = Phase::Unknown;
                true
            }
            Phase::Unknown | Phase::UnknownSettling => true,
            Phase::Settling | Phase::Settled => false,
        }
    }

    fn finish(&self, outcome: Outcome) {
        let lease = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            state.phase = match state.phase {
                Phase::Pending => Phase::Settling,
                Phase::Unknown if state.lease.is_some() => Phase::UnknownSettling,
                _ => return,
            };
            state.outcome = Some(outcome);
            state.lease.take().expect("terminal lease")
        };
        // C's final ticket drop can join its domain mutex-owner thread. Never
        // hold the decision mutex during that potentially blocking callback.
        // The holder and its capacity slot remain alive until this returns.
        lease.settle();
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = match state.phase {
            Phase::Settling => Phase::Settled,
            Phase::UnknownSettling => Phase::Unknown,
            _ => unreachable!("terminal phase changed during settlement"),
        };
    }

    fn take_outcome(&self) -> Outcome {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(state.phase, Phase::Unknown | Phase::UnknownSettling) {
            Outcome::Unknown
        } else {
            state.outcome.take().unwrap_or(Outcome::Unknown)
        }
    }
}

struct Capacity {
    limit: NonZeroUsize,
    in_flight: usize,
    #[cfg(windows)]
    completed_threads: Vec<std::os::windows::io::OwnedHandle>,
}

impl Capacity {
    #[cfg(windows)]
    fn reap_completed(&mut self) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
        };
        let mut index = 0;
        while index < self.completed_threads.len() {
            if unsafe { WaitForSingleObject(self.completed_threads[index].as_raw_handle(), 0) }
                == WAIT_OBJECT_0
            {
                self.completed_threads.swap_remove(index);
                self.in_flight = self
                    .in_flight
                    .checked_sub(1)
                    .expect("process slot accounting");
            } else {
                // WAIT_TIMEOUT and WAIT_FAILED both retain the slot and handle.
                index += 1;
            }
        }
    }
}

struct Slot {
    capacity: Arc<Mutex<Capacity>>,
    #[cfg(windows)]
    holder_thread: Option<std::os::windows::io::OwnedHandle>,
    released: bool,
}
impl Slot {
    /// Only before a holder thread has been successfully created.
    fn release_unstarted(&mut self) {
        let mut capacity = self.capacity.lock().unwrap_or_else(|e| e.into_inner());
        capacity.in_flight = capacity
            .in_flight
            .checked_sub(1)
            .expect("process slot accounting");
        self.released = true;
    }

    #[cfg(windows)]
    fn attach_current_thread(&mut self) -> std::io::Result<()> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::{
            Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle},
            System::Threading::{GetCurrentProcess, GetCurrentThread},
        };
        let mut original = std::ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                GetCurrentThread(),
                GetCurrentProcess(),
                &mut original,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        self.holder_thread =
            Some(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(original) });
        Ok(())
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        #[cfg(windows)]
        if let Some(handle) = self.holder_thread.take() {
            let mut capacity = self.capacity.lock().unwrap_or_else(|e| e.into_inner());
            capacity.completed_threads.push(handle);
        }
        // If no original holder-thread handle was obtained, the slot remains
        // conservatively occupied. An unstarted reservation is released only
        // through release_unstarted before a thread exists.
    }
}

/// Per-controller/domain shared capacity. An unconfirmed operation keeps its
/// slot; dropping the caller or its Tokio runtime cannot create another slot.
pub(crate) struct Supervisor {
    capacity: Arc<Mutex<Capacity>>,
}

impl Supervisor {
    pub(crate) fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity: Arc::new(Mutex::new(Capacity {
                limit: capacity,
                in_flight: 0,
                #[cfg(windows)]
                completed_threads: Vec::new(),
            })),
        }
    }

    /// Shrinking never cancels holders; it only rejects later reservations
    /// until the live count is below the new limit. The caller validates its
    /// own configuration (currently C's 1..=8) before calling this method.
    pub(crate) fn resize(&self, capacity: NonZeroUsize) {
        let mut state = self.capacity.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(windows)]
        state.reap_completed();
        state.limit = capacity;
    }

    #[cfg(all(test, windows))]
    pub(crate) fn active_count(&self) -> usize {
        let mut state = self.capacity.lock().unwrap_or_else(|e| e.into_inner());
        state.reap_completed();
        state.in_flight
    }

    #[cfg(windows)]
    fn reserve(&self) -> Option<Slot> {
        let mut capacity = self.capacity.lock().unwrap_or_else(|e| e.into_inner());
        capacity.reap_completed();
        if capacity.in_flight >= capacity.limit.get() {
            return None;
        }
        capacity.in_flight += 1;
        Some(Slot {
            capacity: Arc::clone(&self.capacity),
            holder_thread: None,
            released: false,
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
        #[cfg(not(windows))]
        {
            let _ = spec;
            return Err(NotStarted {
                reason: "Windows process supervision unavailable",
                lease,
            });
        }
        #[cfg(windows)]
        {
            // Resize and reservation share one linearization point.
            let slot = match self.reserve() {
                Some(slot) => slot,
                None => {
                    return Err(NotStarted {
                        reason: "process supervisor capacity exhausted",
                        lease,
                    });
                }
            };
            let deadline = spec.deadline;
            let cleanup_budget = spec.cleanup_budget;
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancel_at = Arc::new(Mutex::new(None));
            let (send, receive) = tokio::sync::oneshot::channel();
            let cell = Arc::new(LeaseCell(Mutex::new(LeaseState {
                phase: Phase::Pending,
                lease: Some(lease),
                outcome: None,
            })));
            // Keep the lease outside the thread closure until thread creation
            // succeeds, so a thread-creation error can return it unchanged.
            let pending = Arc::new(Mutex::new(Some((spec, slot, send))));
            let worker_pending = Arc::clone(&pending);
            let worker_cancelled = Arc::clone(&cancelled);
            let worker_cancel_at = Arc::clone(&cancel_at);
            let worker_cell = Arc::clone(&cell);
            let spawn = std::thread::Builder::new()
                .name("nhc-process-holder".into())
                .spawn(move || {
                    let work = worker_pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take();
                    if let Some((spec, mut slot, send)) = work {
                        if let Err(error) = slot.attach_current_thread() {
                            worker_cell.finish(Outcome::NotStarted {
                                stage: "holder_thread_identity",
                                os_error: error.raw_os_error(),
                            });
                            let _ = send.send(());
                            // No thread signal handle exists; keep this one
                            // reservation conservatively occupied.
                            return;
                        }
                        platform::run(
                            spec,
                            worker_cell,
                            slot,
                            worker_cancelled,
                            worker_cancel_at,
                            send,
                        );
                    }
                });
            if spawn.is_err() {
                let (_, mut slot, _) = pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .expect("unstarted work remains");
                slot.release_unstarted();
                let lease = cell
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .lease
                    .take()
                    .expect("unstarted lease remains");
                return Err(NotStarted {
                    reason: "cannot create process holder thread",
                    lease,
                });
            }
            Ok(Operation {
                receive,
                lease: cell,
                cancelled,
                cancel_at,
                deadline,
                cleanup_budget,
                done: false,
            })
        }
    }
}

pub(crate) struct Operation {
    receive: tokio::sync::oneshot::Receiver<()>,
    lease: Arc<LeaseCell>,
    cancelled: Arc<AtomicBool>,
    cancel_at: Arc<Mutex<Option<Instant>>>,
    deadline: Instant,
    cleanup_budget: Duration,
    done: bool,
}

impl Operation {
    fn request_cancel(&self) {
        request_cancel(&self.cancelled, &self.cancel_at);
    }

    pub(crate) fn cancel(&self) {
        self.request_cancel();
    }

    /// Keep the same result future and holder when cancellation requests stop.
    /// Dropping this future still only requests stop through Operation::drop.
    pub(crate) async fn wait_with_cancel(
        self,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Outcome {
        let cancelled = Arc::clone(&self.cancelled);
        let cancel_at = Arc::clone(&self.cancel_at);
        let wait = self.wait();
        tokio::pin!(wait);
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                request_cancel(&cancelled, &cancel_at);
                wait.await
            }
            outcome = &mut wait => outcome,
        }
    }

    pub(crate) async fn wait(mut self) -> Outcome {
        let until = self
            .deadline
            .checked_add(self.cleanup_budget)
            .unwrap_or(self.deadline);
        let result =
            tokio::time::timeout_at(tokio::time::Instant::from_std(until), &mut self.receive).await;
        let outcome = match result {
            Ok(Ok(())) => self.lease.take_outcome(),
            _ => {
                self.request_cancel();
                if self.lease.decide_unknown() {
                    Outcome::Unknown
                } else {
                    self.lease.take_outcome()
                }
            }
        };
        self.done = true;
        outcome
    }
}

fn request_cancel(cancelled: &AtomicBool, cancel_at: &Mutex<Option<Instant>>) {
    let mut time = cancel_at.lock().unwrap_or_else(|e| e.into_inner());
    if time.is_none() {
        *time = Some(Instant::now());
    }
    cancelled.store(true, Ordering::Release);
}

impl Drop for Operation {
    fn drop(&mut self) {
        // Drop requests stop; the independent holder and watchdog keep the
        // lease until cleanup is proven or unknown is actually decided.
        if !self.done {
            self.request_cancel();
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        ffi::{OsStr, c_void},
        io::{self, Read},
        mem::{self, size_of, zeroed},
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        ptr::{null, null_mut},
        thread::{self, JoinHandle},
    };
    use windows_sys::Win32::{
        Foundation::{
            ERROR_BROKEN_PIPE, FILETIME, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
            SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Globalization::CompareStringOrdinal,
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::{
            JobObjects::{
                CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
                JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                SetInformationJobObject, TerminateJobObject,
            },
            Pipes::CreatePipe,
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
                DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
                GetProcessTimes, InitializeProcThreadAttributeList,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
                PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
                TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    };

    type Signal = Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>;
    fn notify(signal: &Signal) {
        if let Some(tx) = signal.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
        }
    }
    fn unknown(lease: &LeaseCell, signal: &Signal) {
        if lease.decide_unknown() {
            notify(signal);
        }
    }
    struct WatchdogExit {
        lease: Arc<LeaseCell>,
        signal: Signal,
        normal: bool,
    }
    impl Drop for WatchdogExit {
        fn drop(&mut self) {
            if !self.normal {
                unknown(&self.lease, &self.signal);
            }
        }
    }
    fn raw(handle: &OwnedHandle) -> HANDLE {
        handle.as_raw_handle()
    }
    fn last_error() -> Option<i32> {
        io::Error::last_os_error().raw_os_error()
    }
    unsafe fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
        }
    }

    struct Holder {
        lease: Arc<LeaseCell>,
        slot: Option<Slot>,
        finished: bool,
    }
    impl Holder {
        fn finish(&mut self, outcome: Outcome, signal: &Signal) {
            self.lease.finish(outcome);
            self.finished = true;
            notify(signal);
        }
    }
    impl Drop for Holder {
        fn drop(&mut self) {
            if !self.finished {
                self.lease.decide_unknown();
                mem::forget(Arc::clone(&self.lease));
                // A panic or permanently unconfirmed OS call must not return a
                // slot which could start additional work under the same owner.
                if let Some(slot) = self.slot.take() {
                    mem::forget(slot);
                }
            }
        }
    }

    /// During an unwind, retain original objects rather than treating a close
    /// or kill-on-close as proof that the tree and its I/O have settled.
    struct Evidence {
        lease: Arc<LeaseCell>,
        job: Option<OwnedHandle>,
        process: Option<OwnedHandle>,
        thread: Option<OwnedHandle>,
        resolved: bool,
    }
    impl Drop for Evidence {
        fn drop(&mut self) {
            if !self.resolved {
                self.lease.decide_unknown();
                if let Some(h) = self.job.take() {
                    mem::forget(h);
                }
                if let Some(h) = self.process.take() {
                    mem::forget(h);
                }
                if let Some(h) = self.thread.take() {
                    mem::forget(h);
                }
            }
        }
    }

    struct Attributes {
        buffer: Vec<usize>,
        list: *mut c_void,
    }
    impl Attributes {
        fn new() -> io::Result<Self> {
            let mut bytes = 0;
            unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes) };
            if bytes == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buffer = vec![0usize; bytes.div_ceil(size_of::<usize>())];
            let list = buffer.as_mut_ptr().cast();
            if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut bytes) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { buffer, list })
        }
        fn set<T>(&mut self, attribute: u32, value: &T) -> io::Result<()> {
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
                Err(io::Error::last_os_error())
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

    fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
        let mut data: Vec<u16> = value.encode_wide().collect();
        if data.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"));
        }
        data.push(0);
        Ok(data)
    }

    // Windows CommandLineToArgvW / CRT quoting: always quote, double runs of
    // backslashes before quotes and before the closing quote.
    fn append_argument(line: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
        let units: Vec<u16> = arg.encode_wide().collect();
        if units.contains(&0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "argument NUL"));
        }
        line.push(b'"' as u16);
        let mut slashes = 0usize;
        for c in units {
            if c == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            if c == b'"' as u16 {
                line.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
            } else {
                line.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            }
            slashes = 0;
            line.push(c);
        }
        line.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        line.push(b'"' as u16);
        Ok(())
    }

    fn environment(entries: &[(OsString, OsString)]) -> io::Result<Vec<u16>> {
        let mut vars = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let k: Vec<u16> = key.encode_wide().collect();
            let v: Vec<u16> = value.encode_wide().collect();
            if k.is_empty()
                || k.contains(&0)
                || v.contains(&0)
                || k.len() > i32::MAX as usize
                || k.iter().skip(1).any(|x| *x == b'=' as u16)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid environment",
                ));
            }
            vars.push((k, v));
        }
        vars.sort_by(|a, b| {
            let cmp = unsafe {
                CompareStringOrdinal(
                    a.0.as_ptr(),
                    a.0.len() as i32,
                    b.0.as_ptr(),
                    b.0.len() as i32,
                    1,
                )
            };
            match cmp {
                1 => std::cmp::Ordering::Less,
                3 => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            }
        });
        for pair in vars.windows(2) {
            let cmp = unsafe {
                CompareStringOrdinal(
                    pair[0].0.as_ptr(),
                    pair[0].0.len() as i32,
                    pair[1].0.as_ptr(),
                    pair[1].0.len() as i32,
                    1,
                )
            };
            if cmp == 0 || cmp == 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate or invalid environment name",
                ));
            }
        }
        let mut block = Vec::new();
        for (key, value) in vars {
            block.extend(key);
            block.push(b'=' as u16);
            block.extend(value);
            block.push(0);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        Ok(block)
    }

    fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1,
        };
        let mut read = null_mut();
        let mut write = null_mut();
        if unsafe { CreatePipe(&mut read, &mut write, &security, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let read = unsafe { owned(read)? };
        let write = unsafe { owned(write)? };
        if unsafe { SetHandleInformation(raw(&read), HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((read, write))
    }

    fn reader(
        handle: OwnedHandle,
        limit: usize,
        overflow: Arc<AtomicBool>,
        #[cfg(test)] hooks: Option<Arc<TestHooks>>,
    ) -> io::Result<JoinHandle<io::Result<Capture>>> {
        thread::Builder::new()
            .name("nhc-process-reader".into())
            .spawn(move || {
                let mut file = std::fs::File::from(handle);
                #[cfg(test)]
                if let Some(hooks) = &hooks {
                    if hooks.fail_stdout_reader.load(Ordering::Acquire) {
                        return Err(io::Error::other("injected reader failure"));
                    }
                    if hooks.hold_stdout_reader.load(Ordering::Acquire) {
                        hooks.stdout_reader_held.store(true, Ordering::Release);
                        while !hooks.release_stdout_reader.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(5));
                        }
                    }
                }
                let mut data = Vec::new();
                let mut truncated = false;
                let mut buffer = [0u8; 8192];
                loop {
                    let n = match file.read(&mut buffer) {
                        Ok(n) => n,
                        // For a Windows anonymous pipe ReadFile reports
                        // ERROR_BROKEN_PIPE when the final writer is closed.
                        Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => 0,
                        Err(e) => return Err(e),
                    };
                    if n == 0 {
                        break;
                    }
                    let retain = n.min(limit.saturating_sub(data.len()));
                    data.extend_from_slice(&buffer[..retain]);
                    if retain != n {
                        truncated = true;
                        overflow.store(true, Ordering::Release);
                    }
                }
                #[cfg(test)]
                if let Some(hooks) = &hooks {
                    hooks.stdout_reader_finished.store(true, Ordering::Release);
                }
                Ok(Capture {
                    bytes: data,
                    truncated,
                })
            })
    }

    fn accounting(job: &OwnedHandle) -> io::Result<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION> {
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        if unsafe {
            QueryInformationJobObject(
                raw(job),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(info)
        }
    }

    fn prestart(holder: &mut Holder, signal: &Signal, stage: &'static str, error: Option<i32>) {
        holder.finish(
            Outcome::NotStarted {
                stage,
                os_error: error,
            },
            signal,
        );
    }

    pub(super) fn run(
        spec: ProcessSpec,
        lease: Arc<LeaseCell>,
        slot: Slot,
        cancelled: Arc<AtomicBool>,
        cancel_at: Arc<Mutex<Option<Instant>>>,
        send: tokio::sync::oneshot::Sender<()>,
    ) {
        let signal = Arc::new(Mutex::new(Some(send)));
        let mut holder = Holder {
            lease: Arc::clone(&lease),
            slot: Some(slot),
            finished: false,
        };
        let stopped = Arc::new(AtomicBool::new(false));
        let watcher_done = Arc::clone(&stopped);
        let watcher_lease = Arc::clone(&lease);
        let watcher_cancel = Arc::clone(&cancelled);
        let watcher_at = Arc::clone(&cancel_at);
        let watcher_signal = Arc::clone(&signal);
        #[cfg(test)]
        let watcher_hooks = spec.test_hooks.clone();
        let deadline = spec.deadline;
        let budget = spec.cleanup_budget;
        let watchdog = thread::Builder::new()
            .name("nhc-process-watchdog".into())
            .spawn(move || {
                let mut exit = WatchdogExit {
                    lease: Arc::clone(&watcher_lease),
                    signal: Arc::clone(&watcher_signal),
                    normal: false,
                };
                while !watcher_done.load(Ordering::Acquire) {
                    let cancelled_at = *watcher_at.lock().unwrap_or_else(|e| e.into_inner());
                    let limit = cancelled_at
                        .unwrap_or(deadline)
                        .min(deadline)
                        .checked_add(budget)
                        .unwrap_or(deadline);
                    if Instant::now() >= limit {
                        watcher_cancel.store(true, Ordering::Release);
                        unknown(&watcher_lease, &watcher_signal);
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                #[cfg(test)]
                if let Some(hooks) = &watcher_hooks
                    && hooks.hold_watchdog_exit.load(Ordering::Acquire)
                {
                    hooks.watchdog_held.store(true, Ordering::Release);
                    while !hooks.release_watchdog.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
                exit.normal = true;
            });
        let watchdog = match watchdog {
            Ok(handle) => handle,
            Err(_) => {
                prestart(&mut holder, &signal, "watchdog_thread", None);
                return;
            }
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(spec, &lease, &cancelled, &signal, &mut holder)
        }));
        stopped.store(true, Ordering::Release);
        // A slot is never released while its watchdog is merely asleep.
        // If run_inner panicked, Holder retains its slot and lease on Drop.
        if watchdog.join().is_err() || result.is_err() {
            unknown(&lease, &signal);
        }
    }

    fn run_inner(
        spec: ProcessSpec,
        lease: &Arc<LeaseCell>,
        cancelled: &AtomicBool,
        signal: &Signal,
        holder: &mut Holder,
    ) {
        // Before CreateProcess is called, failures are unambiguously unstarted.
        if cancelled.load(Ordering::Acquire) || Instant::now() >= spec.deadline {
            let stage = if Instant::now() >= spec.deadline {
                "deadline_before_create"
            } else {
                "cancelled_before_create"
            };
            prestart(holder, signal, stage, None);
            return;
        }
        let prepare = || -> io::Result<_> {
            if !spec.program.is_absolute() || !spec.cwd.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "program and cwd must be absolute",
                ));
            }
            let app = wide(spec.program.as_os_str())?;
            let cwd = wide(spec.cwd.as_os_str())?;
            let mut command = Vec::new();
            append_argument(&mut command, spec.program.as_os_str())?;
            for arg in &spec.args {
                command.push(b' ' as u16);
                append_argument(&mut command, arg)?;
            }
            command.push(0);
            let env = environment(&spec.environment)?;
            let job = unsafe { owned(CreateJobObjectW(null(), null()))? };
            if unsafe { SetHandleInformation(raw(&job), HANDLE_FLAG_INHERIT, 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if unsafe {
                SetInformationJobObject(
                    raw(&job),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
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
                return Err(io::Error::last_os_error());
            }
            let flags = actual.BasicLimitInformation.LimitFlags;
            if flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE == 0
                || flags
                    & (JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                        | JOB_OBJECT_LIMIT_BREAKAWAY_OK
                        | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK)
                    != 0
            {
                return Err(io::Error::other("job limits mismatch"));
            }
            let (out_read, out_write) = pipe()?;
            let (err_read, err_write) = pipe()?;
            let security = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: null_mut(),
                bInheritHandle: 1,
            };
            let nul = wide(OsStr::new("NUL"))?;
            let stdin = unsafe {
                owned(CreateFileW(
                    nul.as_ptr(),
                    0x8000_0000,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    &security,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    null_mut(),
                ))?
            };
            Ok((
                app, cwd, command, env, job, out_read, out_write, err_read, err_write, stdin,
            ))
        };
        let (app, cwd, mut command, env, job, out_read, out_write, err_read, err_write, stdin) =
            match prepare() {
                Ok(v) => v,
                Err(e) => {
                    prestart(holder, signal, "prepare", e.raw_os_error());
                    return;
                }
            };
        let mut evidence = Evidence {
            lease: Arc::clone(lease),
            job: Some(job),
            process: None,
            thread: None,
            resolved: false,
        };
        let jobs = [raw(evidence.job.as_ref().expect("job"))];
        let handles = [raw(&stdin), raw(&out_write), raw(&err_write)];
        let attrs = (|| -> io::Result<Attributes> {
            let mut a = Attributes::new()?;
            a.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
            a.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
            Ok(a)
        })();
        let attrs = match attrs {
            Ok(v) => v,
            Err(e) => {
                evidence.resolved = true;
                prestart(holder, signal, "attributes", e.raw_os_error());
                return;
            }
        };
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = attrs.list;
        if cancelled.load(Ordering::Acquire) || Instant::now() >= spec.deadline {
            evidence.resolved = true;
            let stage = if Instant::now() >= spec.deadline {
                "deadline_before_create"
            } else {
                "cancelled_before_create"
            };
            prestart(holder, signal, stage, None);
            return;
        }
        let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
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
                env.as_ptr().cast(),
                cwd.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            )
        };
        if created == 0 {
            let error = last_error();
            match accounting(evidence.job.as_ref().expect("job")) {
                Ok(a) if a.ActiveProcesses == 0 && a.TotalProcesses == 0 => {
                    evidence.resolved = true;
                    prestart(holder, signal, "create_returned_no_process", error);
                    return;
                }
                _ => {
                    unknown(lease, signal);
                    // The API returned without a trustworthy original process
                    // handle. Stop what is in the owned job and keep observing
                    // it, but never manufacture a process-signal proof.
                    let job = evidence.job.as_ref().expect("job");
                    let _ = unsafe { TerminateJobObject(raw(job), 1) };
                    loop {
                        let _ = accounting(job);
                        thread::park_timeout(Duration::from_secs(1));
                    }
                }
            }
        }
        evidence.process = Some(unsafe { OwnedHandle::from_raw_handle(info.hProcess) });
        evidence.thread = Some(unsafe { OwnedHandle::from_raw_handle(info.hThread) });
        #[cfg(test)]
        if let Some(hooks) = &spec.test_hooks
            && hooks.hold_created.load(Ordering::Acquire)
        {
            hooks.created_held.store(true, Ordering::Release);
            while !hooks.release_created.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(5));
            }
        }
        drop(attrs);
        drop(stdin);
        drop(out_write);
        drop(err_write);
        let job = evidence.job.as_ref().expect("job");
        let process = evidence.process.as_ref().expect("process");
        let mut created_at: FILETIME = unsafe { zeroed() };
        let mut exited_at: FILETIME = unsafe { zeroed() };
        let mut kernel_time: FILETIME = unsafe { zeroed() };
        let mut user_time: FILETIME = unsafe { zeroed() };
        let has_identity = unsafe {
            GetProcessTimes(
                raw(process),
                &mut created_at,
                &mut exited_at,
                &mut kernel_time,
                &mut user_time,
            )
        } != 0;
        let identity = has_identity.then_some(ProcessIdentity {
            pid: info.dwProcessId,
            creation_filetime: ((created_at.dwHighDateTime as u64) << 32)
                | created_at.dwLowDateTime as u64,
        });
        let mut inside = 0;
        let membership_ok =
            unsafe { IsProcessInJob(raw(process), raw(job), &mut inside) } != 0 && inside != 0;
        #[cfg(test)]
        let membership_ok = membership_ok
            && !spec
                .test_hooks
                .as_ref()
                .is_some_and(|h| h.fail_membership.load(Ordering::Acquire));
        let mut must_stop_direct = !membership_ok || identity.is_none();
        let mut reason = None;
        if !membership_ok || identity.is_none() {
            unknown(lease, signal);
            reason = Some(CompletionReason::Cancelled);
        }
        let overflow = Arc::new(AtomicBool::new(false));
        let mut io_failed = false;
        let mut out = match reader(
            out_read,
            spec.stdout_limit,
            Arc::clone(&overflow),
            #[cfg(test)]
            spec.test_hooks.clone(),
        ) {
            Ok(v) => Some(v),
            Err(_) => {
                unknown(lease, signal);
                io_failed = true;
                must_stop_direct = true;
                reason = Some(CompletionReason::Cancelled);
                None
            }
        };
        let mut err = match reader(
            err_read,
            spec.stderr_limit,
            Arc::clone(&overflow),
            #[cfg(test)]
            None,
        ) {
            Ok(v) => Some(v),
            Err(_) => {
                unknown(lease, signal);
                io_failed = true;
                must_stop_direct = true;
                reason = Some(CompletionReason::Cancelled);
                None
            }
        };
        // A reader which could not be started owns no outstanding read task;
        // its failed result stays unknown but is different from a live reader.
        let mut out_done = out.is_none();
        let mut err_done = err.is_none();
        let mut cleanup_until = None;
        if reason.is_none() && !cancelled.load(Ordering::Acquire) && Instant::now() < spec.deadline
        {
            if unsafe { ResumeThread(raw(evidence.thread.as_ref().expect("thread"))) } != 1 {
                unknown(lease, signal);
                must_stop_direct = true;
                reason = Some(CompletionReason::Cancelled);
            }
        } else if reason.is_none() {
            reason = Some(if Instant::now() >= spec.deadline {
                CompletionReason::Deadline
            } else {
                CompletionReason::Cancelled
            });
        }
        let mut out_value = None;
        let mut err_value = None;
        let mut signaled = false;
        let mut exit_code = 0u32;
        let mut exit_code_unavailable = false;
        let mut direct_termination_attempted = false;
        let mut job_termination_confirmed = false;
        loop {
            if reason.is_none() {
                if cancelled.load(Ordering::Acquire) {
                    reason = Some(CompletionReason::Cancelled);
                } else if Instant::now() >= spec.deadline {
                    reason = Some(CompletionReason::Deadline);
                } else if spec.overflow == OverflowPolicy::Terminate
                    && overflow.load(Ordering::Acquire)
                {
                    reason = Some(CompletionReason::OutputLimit);
                }
            }
            if reason.is_some() && cleanup_until.is_none() {
                cleanup_until = Some(
                    Instant::now()
                        .checked_add(spec.cleanup_budget)
                        .unwrap_or_else(Instant::now),
                );
            }
            if reason.is_some()
                && !job_termination_confirmed
                && cleanup_until.is_some_and(|until| Instant::now() <= until)
            {
                #[cfg(test)]
                let inject_failure = spec
                    .test_hooks
                    .as_ref()
                    .is_some_and(|h| h.fail_termination.load(Ordering::Acquire));
                #[cfg(not(test))]
                let inject_failure = false;
                if inject_failure || unsafe { TerminateJobObject(raw(job), 1) } == 0 {
                    unknown(lease, signal);
                    must_stop_direct = true;
                } else {
                    job_termination_confirmed = true;
                }
            }
            if must_stop_direct && !direct_termination_attempted {
                // Membership is not proved, or job termination failed:
                // target the original process object as well, never a PID.
                direct_termination_attempted = true;
                #[cfg(test)]
                let inject_failure = spec
                    .test_hooks
                    .as_ref()
                    .is_some_and(|h| h.fail_termination.load(Ordering::Acquire));
                #[cfg(not(test))]
                let inject_failure = false;
                if !inject_failure {
                    let _ = unsafe { TerminateProcess(raw(process), 1) };
                }
            }
            if !signaled {
                match unsafe { WaitForSingleObject(raw(process), 0) } {
                    WAIT_OBJECT_0 => {
                        signaled = true;
                        #[cfg(test)]
                        if let Some(hooks) = &spec.test_hooks {
                            hooks.root_signaled.store(true, Ordering::Release);
                        }
                        let exit_code_ok =
                            unsafe { GetExitCodeProcess(raw(process), &mut exit_code) } != 0;
                        #[cfg(test)]
                        let exit_code_ok = exit_code_ok
                            && !spec
                                .test_hooks
                                .as_ref()
                                .is_some_and(|h| h.fail_exit_code.load(Ordering::Acquire));
                        if !exit_code_ok {
                            unknown(lease, signal);
                            exit_code_unavailable = true;
                            reason.get_or_insert(CompletionReason::Cancelled);
                        }
                    }
                    WAIT_TIMEOUT => {}
                    _ => {
                        unknown(lease, signal);
                        reason.get_or_insert(CompletionReason::Cancelled);
                        must_stop_direct = true;
                    }
                }
            }
            #[cfg(test)]
            if signaled
                && let Some(hooks) = &spec.test_hooks
                && hooks.hold_before_reader_join.load(Ordering::Acquire)
            {
                hooks.before_reader_join_held.store(true, Ordering::Release);
                while !hooks.release_reader_join.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
                // This barrier forces the final read to occur between the
                // initial stop decision and this iteration's completed join.
                while out.as_ref().is_some_and(|reader| !reader.is_finished()) {
                    thread::sleep(Duration::from_millis(5));
                }
            }
            if out.as_ref().is_some_and(JoinHandle::is_finished) {
                out_done = true;
                match out.take().expect("stdout reader").join() {
                    Ok(Ok(v)) => out_value = Some(v),
                    _ => {
                        unknown(lease, signal);
                        io_failed = true;
                        #[cfg(test)]
                        if let Some(hooks) = &spec.test_hooks {
                            hooks.stdout_failed_joined.store(true, Ordering::Release);
                        }
                        reason.get_or_insert(CompletionReason::Cancelled);
                        must_stop_direct = true;
                    }
                }
            }
            if err.as_ref().is_some_and(JoinHandle::is_finished) {
                err_done = true;
                match err.take().expect("stderr reader").join() {
                    Ok(Ok(v)) => err_value = Some(v),
                    _ => {
                        unknown(lease, signal);
                        io_failed = true;
                        reason.get_or_insert(CompletionReason::Cancelled);
                        must_stop_direct = true;
                    }
                }
            }
            let observed = accounting(job);
            #[cfg(test)]
            let observed = if spec
                .test_hooks
                .as_ref()
                .is_some_and(|h| h.fail_job_query.load(Ordering::Acquire))
            {
                Err(io::Error::other("injected job query failure"))
            } else {
                observed
            };
            let active = match observed {
                Ok(v) => Some(v.ActiveProcesses),
                Err(_) => {
                    unknown(lease, signal);
                    reason.get_or_insert(CompletionReason::Cancelled);
                    None
                }
            };
            #[cfg(test)]
            if active == Some(0)
                && let Some(hooks) = &spec.test_hooks
            {
                hooks.job_empty.store(true, Ordering::Release);
            }
            if signaled && active == Some(0) && out_done && err_done {
                // The final read may cross the limit after this iteration's
                // initial overflow check. Joining synchronizes that write;
                // do not adopt a successful exit for Terminate policy.
                if reason.is_none()
                    && spec.overflow == OverflowPolicy::Terminate
                    && overflow.load(Ordering::Acquire)
                {
                    reason = Some(CompletionReason::OutputLimit);
                }
                // Reader errors have no in-flight I/O after successful join,
                // but cannot be represented as successful EOF or output.
                // Unknown was sealed earlier and stays sticky on late cleanup.
                let outcome = match identity {
                    Some(identity) if !io_failed && !exit_code_unavailable => {
                        Outcome::Settled(Settled {
                            reason: reason.unwrap_or(CompletionReason::Exited),
                            identity,
                            exit_code,
                            stdout: out_value.take().expect("stdout"),
                            stderr: err_value.take().expect("stderr"),
                        })
                    }
                    _ => Outcome::Unknown,
                };
                evidence.resolved = true;
                holder.finish(outcome, signal);
                return;
            }
            if cleanup_until.is_some_and(|until| Instant::now() >= until) {
                unknown(lease, signal);
            }
            // A blocked reader still owns its real I/O and keeps the slot.
            thread::sleep(if io_failed && signaled && active == Some(0) {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(20)
            });
        }
    }

    /// Independent test-control Job; its Job handle is intentionally inherited
    /// by the helper solely to compare the child's actual membership. It is
    /// not the production holder's Job and proves nothing about last-close.
    #[cfg(test)]
    pub(super) fn breakaway_control(
        probe: &std::path::Path,
        receipt: &std::path::Path,
        allow: bool,
    ) -> String {
        let job = unsafe { owned(CreateJobObjectW(null(), null())) }.expect("control Job");
        assert_ne!(
            unsafe { SetHandleInformation(raw(&job), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) },
            0
        );
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | if allow {
                JOB_OBJECT_LIMIT_BREAKAWAY_OK
            } else {
                0
            };
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
        let jobs = [raw(&job)];
        let mut attrs = Attributes::new().expect("control attributes");
        attrs
            .set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)
            .expect("control job list");
        attrs
            .set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &jobs)
            .expect("control handle list");
        let app = wide(probe.as_os_str()).expect("control probe path");
        let mut command = Vec::new();
        append_argument(&mut command, probe.as_os_str()).unwrap();
        for arg in [
            OsString::from("job-breakaway-control"),
            OsString::from((raw(&job) as usize).to_string()),
            receipt.as_os_str().to_owned(),
        ] {
            command.push(b' ' as u16);
            append_argument(&mut command, &arg).unwrap();
        }
        command.push(0);
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = attrs.list;
        let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
        let created = unsafe {
            CreateProcessW(
                app.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                null(),
                null(),
                &startup.StartupInfo,
                &mut info,
            )
        };
        if created == 0 {
            if accounting(&job).is_ok_and(|a| a.ActiveProcesses == 0 && a.TotalProcesses == 0) {
                panic!("control helper was not created");
            }
            eprintln!("NHC_TERMINAL_UNKNOWN: control helper creation lacks process proof");
            loop {
                thread::park_timeout(Duration::from_secs(1));
            }
        }
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        let thread_handle = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        let mut member = 0;
        if unsafe { IsProcessInJob(raw(&process), raw(&job), &mut member) } == 0
            || member == 0
            || unsafe { ResumeThread(raw(&thread_handle)) } != 1
        {
            let _ = unsafe { TerminateProcess(raw(&process), 1) };
            let _ = unsafe { TerminateJobObject(raw(&job), 1) };
            if unsafe { WaitForSingleObject(raw(&process), 5000) } != WAIT_OBJECT_0
                || !accounting(&job).is_ok_and(|a| a.ActiveProcesses == 0)
            {
                eprintln!("NHC_TERMINAL_UNKNOWN: control helper cleanup unavailable");
                loop {
                    thread::park_timeout(Duration::from_secs(1));
                }
            }
            panic!("control helper membership/resume failed");
        }
        if unsafe { WaitForSingleObject(raw(&process), 10000) } != WAIT_OBJECT_0 {
            eprintln!("NHC_TERMINAL_UNKNOWN: control helper still owns possible breakaway child");
            loop {
                thread::park_timeout(Duration::from_secs(1));
            }
        }
        // A signaled process does not make the Job's accounting synchronous.
        // Observe the same retained Job within the existing five-second cleanup
        // budget, and record every field independently (old case16 lost these
        // distinctions in one short-circuit condition). This is a new fixture;
        // it neither reclassifies nor operates on the historical unknown.
        let cleanup_until = Instant::now() + Duration::from_secs(5);
        let mut observations = Vec::new();
        let job_empty = loop {
            let observation = accounting(&job);
            match observation {
                Ok(info) => {
                    observations.push(serde_json::json!({
                        "active": info.ActiveProcesses,
                        "total": info.TotalProcesses,
                        "terminated": info.TotalTerminatedProcesses,
                    }));
                    if info.ActiveProcesses == 0 {
                        break true;
                    }
                }
                Err(error) => observations.push(serde_json::json!({
                    "query_error": error.raw_os_error(),
                })),
            }
            if Instant::now() >= cleanup_until {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let mut code = 0;
        let exit_code_ok = unsafe { GetExitCodeProcess(raw(&process), &mut code) } != 0;
        let exit_code_error = (!exit_code_ok).then(last_error).flatten();
        let receipt_result = std::fs::read_to_string(receipt);
        // A successful helper receipt is also the proof for the possible
        // breakaway child, which by definition is not covered by this Job.
        let child_proved = receipt_result.as_ref().is_ok_and(|text| {
            text.trim()
                .strip_prefix("denied=")
                .is_some_and(|e| e.parse::<i32>().is_ok())
                || text.starts_with("created=1 query=")
                    && text.split_whitespace().any(|field| field == "wait=0")
        });
        let observation = serde_json::json!({
            "allow": allow, "helper_pid": info.dwProcessId, "helper_wait": WAIT_OBJECT_0,
            "exit_code_ok": exit_code_ok, "exit_code": code, "exit_code_error": exit_code_error,
            "job_empty": job_empty, "job_observations": observations,
            "receipt_error": receipt_result.as_ref().err().map(|e| e.to_string()),
            "child_proved": child_proved,
        });
        let snapshot = receipt.with_extension("observation.json");
        let saved = std::fs::write(&snapshot, observation.to_string());
        if !job_empty || !child_proved {
            eprintln!("NHC_TERMINAL_UNKNOWN: control cleanup {observation}; snapshot={saved:?}");
            loop {
                thread::park_timeout(Duration::from_secs(1));
            }
        }
        // A missing exit code/nonzero status is an assertion failure only after
        // both the Job and possible escaped child have actual cleanup proof.
        assert!(
            exit_code_ok && code == 0,
            "control helper status: {observation}"
        );
        saved.expect("persist independent control observations");
        receipt_result.expect("proved control receipt")
    }
}

#[cfg(all(test, windows))]
pub(crate) fn breakaway_control(
    probe: &std::path::Path,
    receipt: &std::path::Path,
    allow: bool,
) -> String {
    platform::breakaway_control(probe, receipt, allow)
}

#[cfg(all(test, windows))]
mod state_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    struct Lease {
        sealed: Arc<AtomicUsize>,
        settled: Arc<AtomicUsize>,
        entered: Option<mpsc::Sender<()>>,
        release: Option<Mutex<mpsc::Receiver<()>>>,
    }
    impl DomainLease for Lease {
        fn seal_unknown(&mut self) {
            self.sealed.fetch_add(1, Ordering::SeqCst);
        }
        fn settle(self: Box<Self>) {
            if let Some(entered) = &self.entered {
                let _ = entered.send(());
            }
            if let Some(release) = &self.release {
                let _ = release.lock().unwrap().recv();
            }
            self.settled.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn cell(sealed: &Arc<AtomicUsize>, settled: &Arc<AtomicUsize>) -> Arc<LeaseCell> {
        Arc::new(LeaseCell(Mutex::new(LeaseState {
            phase: Phase::Pending,
            lease: Some(Box::new(Lease {
                sealed: Arc::clone(sealed),
                settled: Arc::clone(settled),
                entered: None,
                release: None,
            })),
            outcome: None,
        })))
    }
    fn prestart() -> Outcome {
        Outcome::NotStarted {
            stage: "test",
            os_error: None,
        }
    }

    #[test]
    fn unknown_is_sticky_and_ticket_consumed_once_after_cleanup() {
        let sealed = Arc::new(AtomicUsize::new(0));
        let settled = Arc::new(AtomicUsize::new(0));
        let cell = cell(&sealed, &settled);
        assert!(cell.decide_unknown());
        assert!(cell.decide_unknown());
        assert_eq!(sealed.load(Ordering::SeqCst), 1);
        cell.finish(prestart());
        cell.finish(prestart());
        assert_eq!(settled.load(Ordering::SeqCst), 1);
        assert!(matches!(cell.take_outcome(), Outcome::Unknown));
    }

    #[test]
    fn blocking_settle_does_not_hold_decision_lock_or_release_slot() {
        let supervisor = Supervisor::new(NonZeroUsize::new(1).unwrap());
        let slot = supervisor.reserve().unwrap();
        let sealed = Arc::new(AtomicUsize::new(0));
        let settled = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let cell = Arc::new(LeaseCell(Mutex::new(LeaseState {
            phase: Phase::Pending,
            outcome: None,
            lease: Some(Box::new(Lease {
                sealed: Arc::clone(&sealed),
                settled: Arc::clone(&settled),
                entered: Some(entered_tx),
                release: Some(Mutex::new(release_rx)),
            })),
        })));
        let holder = Arc::clone(&cell);
        let thread = std::thread::spawn(move || {
            let mut slot = slot;
            slot.attach_current_thread().unwrap();
            holder.finish(prestart());
            drop(slot);
        });
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("settle entered");
        let observer = Arc::clone(&cell);
        let (observed_tx, observed_rx) = mpsc::channel();
        let watcher = std::thread::spawn(move || {
            let _ = observed_tx.send(observer.decide_unknown());
        });
        let observed = observed_rx.recv_timeout(Duration::from_secs(2));
        let still_full = supervisor.reserve().is_none();
        release_tx.send(()).unwrap();
        thread.join().unwrap();
        watcher.join().unwrap();
        assert!(
            !observed.unwrap(),
            "real terminal proof wins before callback"
        );
        assert!(still_full, "slot must cover callback completion");
        assert_eq!(sealed.load(Ordering::SeqCst), 0);
        assert_eq!(settled.load(Ordering::SeqCst), 1);
        assert!(supervisor.reserve().is_some());
    }

    #[test]
    fn resize_preserves_existing_slots_and_uses_new_limit() {
        let supervisor = Supervisor::new(NonZeroUsize::new(3).unwrap());
        let mut one = supervisor.reserve().unwrap();
        let mut two = supervisor.reserve().unwrap();
        supervisor.resize(NonZeroUsize::new(1).unwrap());
        assert!(supervisor.reserve().is_none());
        one.release_unstarted();
        assert!(supervisor.reserve().is_none());
        two.release_unstarted();
        let mut one = supervisor.reserve().unwrap();
        supervisor.resize(NonZeroUsize::new(2).unwrap());
        let mut two = supervisor.reserve().unwrap();
        assert!(supervisor.reserve().is_none());
        one.release_unstarted();
        two.release_unstarted();
    }

    #[test]
    fn caller_drop_only_requests_cancel_until_holder_decides() {
        let sealed = Arc::new(AtomicUsize::new(0));
        let settled = Arc::new(AtomicUsize::new(0));
        let cell = cell(&sealed, &settled);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_at = Arc::new(Mutex::new(None));
        let (_send, receive) = tokio::sync::oneshot::channel();
        let operation = Operation {
            receive,
            lease: Arc::clone(&cell),
            cancelled: Arc::clone(&cancelled),
            cancel_at: Arc::clone(&cancel_at),
            deadline: Instant::now() + Duration::from_secs(10),
            cleanup_budget: Duration::from_secs(1),
            done: false,
        };
        drop(operation);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(cancel_at.lock().unwrap().is_some());
        assert_eq!(sealed.load(Ordering::SeqCst), 0);
        cell.finish(prestart());
        assert_eq!(settled.load(Ordering::SeqCst), 1);
        assert_eq!(sealed.load(Ordering::SeqCst), 0);
    }
}
