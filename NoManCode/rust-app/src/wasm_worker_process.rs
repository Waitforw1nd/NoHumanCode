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
        let request = Request {
            manifest,
            input,
            module,
        };
        let gate = Arc::new(Gate::new());
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
                    Ok(Ok(success)) if gate.accept(&cancel, deadline) => Ok(success),
                    Ok(_) if cancel.is_cancelled() => Err(WorkerError::Cancelled),
                    Ok(Err(error)) => Err(error),
                    Ok(Ok(_)) => Err(WorkerError::Deadline),
                    Err(_) => Err(WorkerError::CleanupUnknown),
                }
            },
        };
        if matches!(result, Err(WorkerError::Cancelled | WorkerError::Deadline)) {
            gate.stop();
            if !finish_cleanup_wait(&mut rx, received_completion).await {
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
}
impl Gate {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
            admission: Mutex::new(()),
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
        self.state.load(Ordering::Acquire) == 1 || Instant::now() >= deadline
    }
    fn accept(&self, cancel: &CancellationToken, deadline: Instant) -> bool {
        let _guard = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        if cancel.is_cancelled() || Instant::now() >= deadline {
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
        self.state.load(Ordering::Acquire) == 0 && Instant::now() < deadline
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
            let gate = Arc::new(Gate::new());
            let barrier = Arc::new(Barrier::new(2));
            let other_gate = gate.clone();
            let other_barrier = barrier.clone();
            let thread = std::thread::spawn(move || {
                other_barrier.wait();
                other_gate.stop();
            });
            barrier.wait();
            let accepted = gate.accept(
                &CancellationToken::new(),
                Instant::now() + Duration::from_secs(1),
            );
            thread.join().unwrap();
            assert_eq!(
                gate.state.load(Ordering::Acquire),
                if accepted { 2 } else { 1 }
            );
            assert!(!gate.accept(
                &CancellationToken::new(),
                Instant::now() + Duration::from_secs(1)
            ));
        }
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
            ERROR_BROKEN_PIPE, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED,
            GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, LocalFree,
            SetHandleInformation, WAIT_OBJECT_0,
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
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
                JOB_OBJECT_LIMIT_PROCESS_TIME, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
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
                STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
                WaitForSingleObject,
            },
        },
    };

    const CHUNK: usize = 16 * 1024;
    const MAX_STDERR: usize = 64 * 1024;

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
                        let mut transferred = 0;
                        if unsafe { GetOverlappedResult(raw(&server), &*ov, &mut transferred, 0) }
                            != 0
                        {
                            break;
                        }
                        let err = std::io::Error::last_os_error().raw_os_error();
                        if err != Some(ERROR_IO_INCOMPLETE as i32) {
                            return Err(WorkerError::IsolationUnavailable("pipe_connect", err));
                        }
                        if gate.stopped(deadline) {
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
                            if let Some(job) = job {
                                unsafe { TerminateJobObject(raw(&job), 1) };
                            }
                            for pipe in pipes {
                                unsafe { CancelIoEx(raw(&pipe), null()) };
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
    }
    impl Operation {
        fn new(pipe: Arc<OwnedHandle>, write: bool) -> Result<Self, WorkerError> {
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
            if !gate.authorize(deadline) {
                self.pending = false;
                return Err(WorkerError::Deadline);
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
            if result == 0 {
                let code = std::io::Error::last_os_error().raw_os_error();
                if code != Some(ERROR_IO_PENDING as i32) {
                    self.pending = false;
                    return Err(WorkerError::IsolationUnavailable("io_submit", code));
                }
            }
            Ok(())
        }
        fn poll(&mut self) -> Result<Option<u32>, WorkerError> {
            if !self.pending {
                return Ok(None);
            }
            let mut transferred = 0;
            if unsafe { GetOverlappedResult(raw(&self.pipe), &*self.ov, &mut transferred, 0) } != 0
            {
                self.pending = false;
                return Ok(Some(transferred));
            }
            let code = std::io::Error::last_os_error().raw_os_error();
            if code == Some(ERROR_IO_INCOMPLETE as i32) {
                return Ok(None);
            }
            self.pending = false;
            Err(WorkerError::IsolationUnavailable("io_completion", code))
        }
    }
    impl Drop for Operation {
        fn drop(&mut self) {
            while self.pending {
                unsafe { CancelIoEx(raw(&self.pipe), &*self.ov) };
                let _ = self.poll();
                if self.pending {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    struct ProcessGuard {
        job: Arc<OwnedHandle>,
        process: OwnedHandle,
        complete: bool,
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
            if unsafe { WaitForSingleObject(raw(&self.process), 0) } != WAIT_OBJECT_0 {
                return false;
            }
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
            unsafe {
                QueryInformationJobObject(
                    raw(&self.job),
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    null_mut(),
                ) != 0
                    && info.ActiveProcesses == 0
            }
        }
        fn wait_reaped(&mut self) {
            while !self.reaped() {
                unsafe { TerminateJobObject(raw(&self.job), 1) };
                thread::sleep(Duration::from_millis(5));
            }
            self.complete = true;
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
        stdin: Arc<OwnedHandle>,
        stdout: Arc<OwnedHandle>,
        stderr: Arc<OwnedHandle>,
        process: &mut ProcessGuard,
        stopper: &Stopper,
        gate: &Gate,
        deadline: Instant,
    ) -> Result<Vec<u8>, WorkerError> {
        let mut writer = Some(Operation::new(stdin, true)?);
        let mut out = Operation::new(stdout, false)?;
        let mut err = Operation::new(stderr, false)?;
        let mut fail_stop = StopOnFailure {
            gate,
            success: false,
        };
        let mut offset = 0usize;
        let mut response = Vec::new();
        let mut stderr_count = 0usize;
        let mut stdout_eof = false;
        let mut stderr_eof = false;
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
                if offset == input.len() && !write.pending {
                    stopper.remove_pipe(&write.pipe);
                    writer = None;
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
                            continue;
                        }
                        other => other?,
                    }
                }
                match op.poll() {
                    Ok(Some(0)) => *eof = true,
                    Ok(Some(n)) => {
                        if target {
                            response.extend_from_slice(&op.buffer[..n as usize]);
                            if response.len() > protocol::MAX_RESPONSE + 4 {
                                return Err(WorkerError::Protocol);
                            }
                        } else {
                            stderr_count += n as usize;
                            if stderr_count > MAX_STDERR {
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
                        *eof = true
                    }
                    Err(error) => return Err(error),
                }
            }
            if writer.is_none() && stdout_eof && stderr_eof && process.reaped() {
                process.complete = true;
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
            return Err(failure("worker_create"));
        }
        let process_handle = match unsafe { own(information.hProcess, "worker_process_handle") } {
            Ok(handle) => handle,
            Err(_) => {
                // A successful CreateProcess without an observable process handle
                // cannot satisfy the cleanup proof. Keep this one slot occupied.
                gate.stop();
                loop {
                    thread::park_timeout(Duration::from_secs(1));
                }
            }
        };
        let mut process = ProcessGuard {
            job: job.clone(),
            process: process_handle,
            complete: false,
        };
        let thread_handle = unsafe { own(information.hThread, "worker_thread_handle")? };
        drop(child_stdin);
        drop(child_stdout);
        drop(child_stderr);
        let mut inside = 0;
        if unsafe { IsProcessInJob(raw(&process.process), raw(&job), &mut inside) } == 0 {
            let error = failure("worker_job_membership");
            gate.stop();
            return Err(error);
        }
        if inside == 0 {
            gate.stop();
            return Err(WorkerError::IsolationUnavailable("worker_not_in_job", None));
        }
        image.verify_process(raw(&process.process), &worker.digest)?;
        if !gate.authorize(deadline) {
            return Err(WorkerError::Deadline);
        }
        if unsafe { ResumeThread(raw(&thread_handle)) } == u32::MAX {
            let error = failure("worker_resume");
            gate.stop();
            return Err(error);
        }
        let response = io_loop(
            frame,
            stdin,
            stdout,
            stderr,
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
}
