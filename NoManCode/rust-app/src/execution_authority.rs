//! A single, real data-domain owner shared by Store, controller and process work.
//! Schema17 lifetime slice: this is not the schema18 transaction/epoch fence.
use crate::process_lifetime::{DomainLease, Outcome, ProcessSpec, Supervisor};
use anyhow::{Context, Result, ensure};
use std::{
    future::Future,
    num::NonZeroUsize,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Admission {
    controller: bool,
    open: bool,
    in_flight: usize,
    unknown: usize,
}
pub(crate) struct ExecutionAuthority {
    domain: platform::Domain,
    pub(crate) initialization: Mutex<bool>,
    admission: Mutex<Admission>,
    pub(crate) supervisor: Supervisor,
}
impl ExecutionAuthority {
    pub(crate) fn acquire(path: &Path) -> Result<Arc<Self>> {
        platform::acquire(path)
    }
    pub(crate) fn connection(&self) -> Result<rusqlite::Connection> {
        self.domain.connection()
    }
    pub(crate) fn instance_lock_handle(&self) -> Result<std::fs::File> {
        self.domain.instance_lock_handle()
    }
    pub(crate) fn claim(self: &Arc<Self>) -> Result<ExecutionController> {
        self.domain.verify()?;
        let mut state = self
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
        ensure!(
            !state.controller && state.in_flight == 0 && state.unknown == 0,
            "data domain controller is busy or unknown"
        );
        state.controller = true;
        state.open = true;
        Ok(ExecutionController(
            self.clone(),
            std::sync::atomic::AtomicBool::new(false),
        ))
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.domain.verify()?;
        let state = self
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
        ensure!(
            state.controller && state.open,
            "data domain admission is closed"
        );
        Ok(())
    }
    pub(crate) fn close(&self) {
        self.admission
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .open = false;
    }
    pub(crate) fn status(&self) -> (bool, usize, usize) {
        let state = self.admission.lock().unwrap_or_else(|p| p.into_inner());
        (state.open, state.in_flight, state.unknown)
    }
    pub(crate) fn operation(
        self: &Arc<Self>,
        cancel: CancellationToken,
    ) -> Result<ExecutionOperation> {
        self.domain.verify()?;
        let mut state = self
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
        ensure!(
            state.controller && state.open,
            "data domain admission is closed"
        );
        state.in_flight = state
            .in_flight
            .checked_add(1)
            .context("domain ticket count exhausted")?;
        Ok(ExecutionOperation {
            cancel_on_drop: true,
            scope: Arc::new(OperationScope {
                authority: self.clone(),
                cancel: cancel.child_token(),
                state: Mutex::new(OperationState {
                    bodies: 1,
                    generation: 0,
                    active: None,
                    unknown: false,
                    settled: false,
                }),
            }),
        })
    }
}
pub(crate) struct ExecutionController(Arc<ExecutionAuthority>, std::sync::atomic::AtomicBool);
impl ExecutionController {
    pub(crate) fn activate(&self) {
        self.1.store(true, std::sync::atomic::Ordering::Release);
    }
    pub(crate) fn can_recover(&self) -> bool {
        !self.1.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(crate) fn owns(&self, authority: &Arc<ExecutionAuthority>) -> bool {
        Arc::ptr_eq(&self.0, authority)
    }
}
impl Drop for ExecutionController {
    fn drop(&mut self) {
        let mut state = self.0.admission.lock().unwrap_or_else(|p| p.into_inner());
        state.open = false;
        state.controller = false;
    }
}

// One original operation ticket, not one domain ticket per Git subprocess.
// Lock order is operation -> admission; no admission lock calls a callback.
struct OperationState {
    bodies: usize,
    generation: u64,
    active: Option<u64>,
    unknown: bool,
    settled: bool,
}
pub(crate) struct OperationScope {
    authority: Arc<ExecutionAuthority>,
    cancel: CancellationToken,
    state: Mutex<OperationState>,
}
pub(crate) struct ExecutionOperation {
    scope: Arc<OperationScope>,
    cancel_on_drop: bool,
}
tokio::task_local! { static CURRENT: Arc<OperationScope>; }
impl ExecutionOperation {
    pub(crate) fn hold_body(&self) -> Self {
        let mut state = self.scope.state.lock().unwrap_or_else(|p| p.into_inner());
        state.bodies = state
            .bodies
            .checked_add(1)
            .expect("operation body count exhausted");
        Self {
            scope: self.scope.clone(),
            cancel_on_drop: false,
        }
    }

    pub(crate) async fn run<F: Future>(self, work: F) -> F::Output {
        CURRENT.scope(self.scope.clone(), work).await
    }
}
impl Drop for ExecutionOperation {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.scope.cancel.cancel();
        }
        let mut state = self.scope.state.lock().unwrap_or_else(|p| p.into_inner());
        state.bodies = state
            .bodies
            .checked_sub(1)
            .expect("operation body accounting");
        self.scope.finish_if_ready(&mut state);
    }
}
impl OperationScope {
    fn finish_if_ready(&self, state: &mut OperationState) {
        if state.bodies == 0 && state.active.is_none() && !state.settled {
            let mut admission = self
                .authority
                .admission
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            admission.in_flight = admission
                .in_flight
                .checked_sub(1)
                .expect("domain ticket accounting");
            // Unknown is sticky for this owner, even after resource settlement.
            state.settled = true;
        }
    }
    fn generation(self: &Arc<Self>) -> Result<Box<dyn DomainLease>> {
        self.authority.domain.verify()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("operation poisoned"))?;
        let admission = self
            .authority
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
        ensure!(
            admission.controller && admission.open,
            "data domain admission is closed"
        );
        ensure!(!self.cancel.is_cancelled(), "operation was cancelled");
        ensure!(
            state.bodies != 0 && !state.unknown && state.active.is_none(),
            "operation generation is closed or busy"
        );
        let id = state
            .generation
            .checked_add(1)
            .context("operation generation exhausted")?;
        state.generation = id;
        state.active = Some(id);
        Ok(Box::new(Generation {
            scope: self.clone(),
            id,
        }))
    }
}
struct Generation {
    scope: Arc<OperationScope>,
    id: u64,
}
impl DomainLease for Generation {
    fn seal_unknown(&mut self) {
        let mut state = self.scope.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.active == Some(self.id) && !state.unknown {
            let mut admission = self
                .scope
                .authority
                .admission
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            admission.open = false;
            admission.unknown = admission.unknown.saturating_add(1);
            state.unknown = true;
        }
    }
    fn settle(self: Box<Self>) {
        let mut state = self.scope.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.active == Some(self.id) {
            state.active = None;
            self.scope.finish_if_ready(&mut state);
        }
    }
}

/// Capture a counted body lease *before* spawning detached work. Dropping
/// only its waiting caller requests cancellation but cannot finish this body.
pub(crate) fn current() -> Option<ExecutionOperation> {
    CURRENT
        .try_with(|scope| {
            let mut state = scope.state.lock().unwrap_or_else(|p| p.into_inner());
            state.bodies = state
                .bodies
                .checked_add(1)
                .expect("operation body count exhausted");
            ExecutionOperation {
                scope: scope.clone(),
                cancel_on_drop: false,
            }
        })
        .ok()
}
pub(crate) async fn inherit<F: Future>(scope: Option<ExecutionOperation>, work: F) -> F::Output {
    match scope {
        Some(scope) => scope.run(work).await,
        None => work.await,
    }
}
pub(crate) async fn run_process(spec: ProcessSpec) -> Result<Outcome> {
    #[cfg(all(test, windows))]
    let spec = process_override::take(spec);
    let scope = current().context("process execution requires a live data-domain operation")?;
    let lease = scope.scope.generation()?;
    let operation = match scope.scope.authority.supervisor.submit(spec, lease) {
        Ok(operation) => operation,
        Err(rejected) => {
            rejected.lease.settle();
            anyhow::bail!("process was not submitted: {}", rejected.reason);
        }
    };
    Ok(operation.wait_with_cancel(&scope.scope.cancel).await)
}

/// Build a process spec only from an existing trusted command policy. Git
/// commands use env_clear; callers must not silently inherit the host env.
pub(crate) fn command_spec(
    command: &tokio::process::Command,
    limit: usize,
    deadline: std::time::Instant,
) -> Result<ProcessSpec> {
    let command = command.as_std();
    let program = std::path::PathBuf::from(command.get_program());
    ensure!(
        program.is_absolute() && program.is_file(),
        "trusted executable must be resolved to an absolute file"
    );
    let environment = command
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
        .collect();
    let mut spec = ProcessSpec::new(
        program,
        command
            .get_current_dir()
            .context("process cwd missing")?
            .to_owned(),
        environment,
        (limit, 8192),
        crate::process_lifetime::OverflowPolicy::Terminate,
        deadline,
        std::time::Duration::from_secs(5),
    );
    spec.args = command.get_args().map(ToOwned::to_owned).collect();
    Ok(spec)
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    pub(crate) struct Domain;
    pub(crate) fn acquire(_: &Path) -> Result<Arc<ExecutionAuthority>> {
        anyhow::bail!("data-domain execution authority requires Windows")
    }
    impl Domain {
        pub(crate) fn instance_lock_handle(&self) -> Result<std::fs::File> {
            anyhow::bail!("unsupported data domain")
        }
        pub(crate) fn connection(&self) -> Result<rusqlite::Connection> {
            anyhow::bail!("unsupported data domain")
        }
        pub(crate) fn verify(&self) -> Result<()> {
            anyhow::bail!("unsupported data domain")
        }
    }
}
#[cfg(windows)]
mod platform {
    use super::*;
    use anyhow::anyhow;
    use std::{
        collections::HashMap,
        fs::{File, OpenOptions},
        os::windows::{
            ffi::OsStrExt,
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
        path::{Component, PathBuf, Prefix},
        sync::{OnceLock, Weak, mpsc},
        thread::{self, JoinHandle},
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW,
            GetFileInformationByHandle, GetVolumeInformationW, OPEN_EXISTING,
        },
        System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
    };
    /// Identity of a file already opened and held by the caller.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    pub(super) struct FileIdentity {
        volume: u32,
        index: u64,
    }

    impl FileIdentity {
        pub(super) fn from_file(file: &File) -> Result<Self> {
            Self::from_info(&info(file)?)
        }

        fn from_info(info: &BY_HANDLE_FILE_INFORMATION) -> Result<Self> {
            ensure!(
                info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
                    == 0,
                "database must be a regular non-reparse file"
            );
            ensure!(
                info.nNumberOfLinks > 0,
                "database has no live directory link"
            );
            Ok(Self {
                volume: info.dwVolumeSerialNumber,
                index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            })
        }
    }

    /// A dedicated thread owns the named mutex: Windows mutexes are reentrant
    /// on their owning thread, so acquiring them on callers is insufficient.
    pub(super) struct IdentityMutex {
        release: Option<mpsc::Sender<()>>,
        thread: Option<JoinHandle<()>>,
    }

    impl IdentityMutex {
        pub(super) fn try_acquire(identity: FileIdentity) -> Result<Self> {
            let name = format!(
                "Global\\NoHumanCode-domain-{:08x}-{:016x}",
                identity.volume, identity.index
            );
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::channel();
            let thread = thread::Builder::new()
                .name("nhc-domain-mutex".into())
                .spawn(move || {
                    let wide: Vec<u16> = std::ffi::OsStr::new(&name)
                        .encode_wide()
                        .chain(Some(0))
                        .collect();
                    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
                    if handle.is_null() {
                        let _ = ready_tx.send(Err(std::io::Error::last_os_error().to_string()));
                        return;
                    }
                    let wait = unsafe { WaitForSingleObject(handle, 0) };
                    if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
                        let error = if wait == WAIT_TIMEOUT {
                            "database domain is busy".to_owned()
                        } else {
                            std::io::Error::last_os_error().to_string()
                        };
                        let _ = ready_tx.send(Err(error));
                        unsafe { CloseHandle(handle) };
                        return;
                    }
                    if ready_tx.send(Ok(())).is_ok() {
                        let _ = release_rx.recv();
                    }
                    unsafe {
                        ReleaseMutex(handle);
                        CloseHandle(handle);
                    }
                })
                .context("cannot start domain mutex owner")?;
            match ready_rx.recv().context("domain mutex owner exited")? {
                Ok(()) => Ok(Self {
                    release: Some(release_tx),
                    thread: Some(thread),
                }),
                Err(error) => {
                    let _ = thread.join();
                    Err(anyhow!(error))
                }
            }
        }
    }

    impl Drop for IdentityMutex {
        fn drop(&mut self) {
            drop(self.release.take());
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn info(file: &File) -> Result<BY_HANDLE_FILE_INFORMATION> {
        let mut info = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } != 0,
            "cannot inspect pinned file: {}",
            std::io::Error::last_os_error()
        );
        Ok(info)
    }

    fn open_directory(path: &Path) -> Result<File> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).context("cannot pin domain directory");
        }
        let file = unsafe { File::from_raw_handle(handle.cast()) };
        let attributes = info(&file)?.dwFileAttributes;
        ensure!(
            attributes & FILE_ATTRIBUTE_DIRECTORY != 0
                && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "domain directory must not be a reparse point"
        );
        Ok(file)
    }

    fn check_volume(root: &Path) -> Result<()> {
        let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
        // DRIVE_FIXED; removable and remote volumes do not meet this contract.
        ensure!(
            unsafe { GetDriveTypeW(wide.as_ptr()) } == 3,
            "unsupported volume"
        );
        let mut filesystem = [0u16; 32];
        ensure!(
            unsafe {
                GetVolumeInformationW(
                    wide.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    filesystem.as_mut_ptr(),
                    filesystem.len() as u32,
                )
            } != 0,
            "cannot identify volume filesystem: {}",
            std::io::Error::last_os_error()
        );
        let end = filesystem
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(filesystem.len());
        ensure!(
            String::from_utf16_lossy(&filesystem[..end]).eq_ignore_ascii_case("NTFS"),
            "unsupported volume filesystem"
        );
        Ok(())
    }

    fn pin_directories(path: &Path) -> Result<(PathBuf, Vec<File>)> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut current = PathBuf::new();
        let mut pins = Vec::new();
        for part in absolute.components() {
            match part {
                Component::Prefix(prefix) => {
                    ensure!(
                        matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)),
                        "unsupported domain path"
                    );
                    current.push(prefix.as_os_str());
                }
                Component::RootDir => {
                    current.push(part.as_os_str());
                    check_volume(&current)?;
                    pins.push(open_directory(&current)?);
                }
                Component::Normal(component) => {
                    current.push(component);
                    pins.push(open_directory(&current)?);
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    ensure!(current.pop(), "domain path escapes its root");
                    pins.pop();
                }
            }
        }
        ensure!(!pins.is_empty(), "domain directory has no rooted path");
        Ok((current, pins))
    }

    fn open_pinned_file(path: &Path) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let _ = FileIdentity::from_file(&file)?;
        Ok(file)
    }

    pub(crate) struct Domain {
        path: PathBuf,
        database: File,
        identity: FileIdentity,
        _directories: Vec<File>,
        _mutex: IdentityMutex,
        instance_lock: File,
    }
    pub(crate) fn acquire(path: &Path) -> Result<Arc<ExecutionAuthority>> {
        let basename = path.file_name().context("database filename missing")?;
        ensure!(
            !basename.to_string_lossy().contains(':'),
            "database stream names are unsupported"
        );
        let (parent, pins) = pin_directories(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        let path = parent.join(basename);
        let instance_path = if basename
            .to_string_lossy()
            .eq_ignore_ascii_case("peachsh.sqlite3")
        {
            parent.join("instance.lock")
        } else {
            let mut name = basename.to_os_string();
            name.push(".instance.lock");
            parent.join(name)
        };
        let lock_instance = || -> Result<File> {
            let file = open_pinned_file(&instance_path)?;
            ensure!(
                info(&file)?.nNumberOfLinks == 1,
                "instance lock aliases are unsupported"
            );
            file.try_lock().context("data domain instance is busy")?;
            Ok(file)
        };
        let mut initial_lock = None;
        if !path.try_exists()? {
            initial_lock = Some(lock_instance()?);
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut sidecar = path.as_os_str().to_os_string();
                sidecar.push(suffix);
                ensure!(
                    !Path::new(&sidecar).try_exists()?,
                    "missing database has existing SQLite sidecars"
                );
            }
        }
        let database = open_pinned_file(&path)?;
        let identity = FileIdentity::from_file(&database)?;
        ensure!(
            info(&database)?.nNumberOfLinks == 1,
            "database hard links are unsupported"
        );
        let path = std::fs::canonicalize(path)?;
        // This registry only shares an already-held OS owner. Cross-process
        // exclusion always comes from the original file identity named mutex.
        static REGISTRY: OnceLock<Mutex<HashMap<FileIdentity, Weak<ExecutionAuthority>>>> =
            OnceLock::new();
        let mut owners = REGISTRY
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| anyhow!("domain registry poisoned"))?;
        owners.retain(|_, owner| owner.strong_count() != 0);
        if let Some(owner) = owners.get(&identity).and_then(Weak::upgrade) {
            owner.domain.verify()?;
            ensure!(
                owner
                    .domain
                    .path
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&path.to_string_lossy()),
                "database alias has a different SQLite sidecar name"
            );
            return Ok(owner);
        }
        let instance_lock = match initial_lock {
            Some(lock) => lock,
            None => lock_instance()?,
        };
        let mutex = IdentityMutex::try_acquire(identity)?;
        let owner = Arc::new(ExecutionAuthority {
            domain: Domain {
                path,
                database,
                identity,
                _directories: pins,
                _mutex: mutex,
                instance_lock,
            },
            initialization: Mutex::new(false),
            admission: Mutex::new(Admission::default()),
            supervisor: Supervisor::new(NonZeroUsize::new(1).unwrap()),
        });
        owner.domain.verify()?;
        owners.insert(identity, Arc::downgrade(&owner));
        Ok(owner)
    }
    impl Domain {
        pub(crate) fn instance_lock_handle(&self) -> Result<File> {
            Ok(self.instance_lock.try_clone()?)
        }
        pub(crate) fn verify(&self) -> Result<()> {
            ensure!(
                FileIdentity::from_file(&self.database)? == self.identity
                    && info(&self.database)?.nNumberOfLinks == 1,
                "database identity changed or acquired hard links"
            );
            let reopened = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&self.path)?;
            ensure!(
                FileIdentity::from_file(&reopened)? == self.identity
                    && info(&reopened)?.nNumberOfLinks == 1,
                "database path was replaced or aliased"
            );
            Ok(())
        }
        pub(crate) fn connection(&self) -> Result<rusqlite::Connection> {
            self.verify()?;
            let db = rusqlite::Connection::open(&self.path)?;
            let mut handle: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
            let rc = unsafe {
                rusqlite::ffi::sqlite3_file_control(
                    db.handle(),
                    c"main".as_ptr(),
                    rusqlite::ffi::SQLITE_FCNTL_WIN32_GET_HANDLE,
                    (&mut handle as *mut windows_sys::Win32::Foundation::HANDLE).cast(),
                )
            };
            ensure!(
                rc == rusqlite::ffi::SQLITE_OK
                    && !handle.is_null()
                    && handle != INVALID_HANDLE_VALUE,
                "SQLite main handle unavailable: {rc}"
            );
            let mut actual: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            ensure!(
                unsafe { GetFileInformationByHandle(handle, &mut actual) } != 0,
                "cannot inspect SQLite main handle"
            );
            ensure!(
                FileIdentity::from_info(&actual)? == self.identity && actual.nNumberOfLinks == 1,
                "SQLite did not open the pinned database"
            );
            self.verify()?;
            Ok(db)
        }
    }
}

#[cfg(test)]
pub(crate) async fn test_scope<F: Future>(work: F) -> F::Output {
    let data = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::store::Store::open(&data.path().join("scope.db")).unwrap());
    let _engine = crate::engine::Engine::new(store.clone(), 8).unwrap();
    store
        .execution_operation(CancellationToken::new())
        .unwrap()
        .run(work)
        .await
}
#[cfg(test)]
pub(crate) fn test_spawn<F>(work: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let scope = current();
    tokio::spawn(inherit(scope, work))
}

#[cfg(all(test, windows))]
mod process_override {
    use super::*;
    use std::{collections::HashMap, path::PathBuf, sync::OnceLock};
    fn overrides() -> &'static Mutex<HashMap<PathBuf, ProcessSpec>> {
        static VALUES: OnceLock<Mutex<HashMap<PathBuf, ProcessSpec>>> = OnceLock::new();
        VALUES.get_or_init(Default::default)
    }
    pub(crate) struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            overrides().lock().unwrap().remove(&self.0);
        }
    }
    pub(super) fn once(root: &Path, spec: ProcessSpec) -> Guard {
        let path = root.canonicalize().unwrap();
        assert!(
            overrides()
                .lock()
                .unwrap()
                .insert(path.clone(), spec)
                .is_none()
        );
        Guard(path)
    }
    pub(super) fn take(spec: ProcessSpec) -> ProcessSpec {
        let Ok(root) = spec.cwd.canonicalize() else {
            return spec;
        };
        overrides().lock().unwrap().remove(&root).unwrap_or(spec)
    }
}
#[cfg(all(test, windows))]
mod lifecycle_tests {
    use super::*;
    use crate::git_diff::{GitDiffError, GitDiffRequest, GitDiffStatus, GitDiffView};
    use crate::process_lifetime::{OverflowPolicy, TestHooks};
    use crate::{
        fixture::{Case, until},
        lifecycle_host::Host,
    };
    use std::{
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };

    async fn native_git_case(mode: &str) -> Result<()> {
        let mut case = Case::new(&format!("native-git-{mode}-"))?;
        let host = Host::new(&case).await?;
        let (task, _) = host.shell("git init --quiet; git config user.name lifecycle; git config user.email lifecycle@example.invalid; [IO.File]::WriteAllText((Join-Path $PWD 'a.txt'), \"old`n\"); git add a.txt; git commit --quiet -m fixture; [IO.File]::WriteAllText((Join-Path $PWD 'a.txt'), \"new`n\")", 20).await?;
        host.finished(&task).await?;
        let request = || GitDiffRequest {
            path: "a.txt".into(),
            view: GitDiffView::Head,
        };
        assert_eq!(
            host.engine.git_diff(&task.id, request()).await?.status,
            GitDiffStatus::Text
        );
        let program = std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths)
                    .filter(|path| path.is_absolute())
                    .filter_map(|path| path.join("pwsh.exe").canonicalize().ok())
                    .find(|path| path.is_file() && !path.starts_with(&case.root))
            })
            .context("trusted pwsh fixture")?;
        let hooks = Arc::new(TestHooks::default());
        hooks
            .hold_stdout_reader
            .store(mode == "unknown", Ordering::SeqCst);
        let mut spec = ProcessSpec::new(
            program,
            case.root.clone(),
            std::env::vars_os().collect(),
            (65_536, 65_536),
            OverflowPolicy::TruncateAndDrain,
            Instant::now() + Duration::from_secs(10),
            Duration::from_secs(5),
        );
        spec.args = vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            case.command(false, false)?.into(),
        ];
        spec.test_hooks = Some(hooks.clone());
        let _hook = process_override::once(&case.root, spec);
        let engine = host.engine.clone();
        let id = task.id.clone();
        let request_value = request();
        let pending = tokio::spawn(async move { engine.git_diff(&id, request_value).await });
        case.observe().await?;
        until(|| case.parent_exited()).await?;
        ensure!(
            !case.descendant_exited()?,
            "original descendant must remain live after parent/EOF"
        );
        let status = host.engine.store.execution_admission_status()?;
        ensure!(
            status.0 && status.1 > 0 && status.2 == 0,
            "native Git lost its domain ticket"
        );
        ensure!(
            Engine::new(host.engine.store.clone(), 1).is_err(),
            "second controller entered while Git alive"
        );
        ensure!(
            !pending.is_finished(),
            "native Git returned before descendant settled"
        );
        if mode == "shutdown" {
            host.engine.request_shutdown();
        } else {
            case.release()?;
        }
        if mode == "unknown" {
            until(|| Ok(hooks.stdout_reader_held.load(Ordering::SeqCst))).await?;
        }
        let wait = if mode == "shutdown" {
            Duration::from_secs(8)
        } else {
            Duration::from_secs(18)
        };
        let result = tokio::time::timeout(wait, pending).await??;
        // The deliberately substituted transport output is not valid Git data.
        // This is an error-path probe through the actual Engine/Git collector.
        ensure!(
            result.is_err(),
            "substituted transport unexpectedly parsed as Git"
        );
        if mode == "unknown" {
            ensure!(
                result.unwrap_err().downcast_ref::<GitDiffError>()
                    == Some(&GitDiffError::OutcomeUnknown),
                "typed Git unknown missing"
            );
            let status = host.engine.store.execution_admission_status()?;
            ensure!(
                !status.0 && status.1 > 0 && status.2 > 0,
                "unknown did not seal/retain original ticket"
            );
            ensure!(
                host.engine.start(Host::request()).await.is_err(),
                "unknown admitted new work"
            );
            hooks.release_stdout_reader.store(true, Ordering::SeqCst);
        }
        case.prove_exited().await?;
        until(|| Ok(host.engine.store.execution_admission_status()?.1 == 0)).await?;
        ensure!(
            hooks.stdout_reader_joined.load(Ordering::SeqCst)
                && hooks.stderr_reader_joined.load(Ordering::SeqCst),
            "real readers not joined"
        );
        let store = host.engine.store.clone();
        if mode == "normal" {
            ensure!(
                store.execution_admission_status()?.0,
                "ordinary Git parse error sealed domain"
            );
            assert_eq!(
                host.engine.git_diff(&task.id, request()).await?.status,
                GitDiffStatus::Text
            );
            host.close().await?;
        } else if mode == "shutdown" {
            ensure!(
                host.engine.shutdown(Duration::from_secs(5)).await.settled(),
                "settled native Git shutdown reported unknown"
            );
        } else {
            let status = store.execution_admission_status()?;
            ensure!(
                !status.0 && status.2 > 0,
                "late cleanup reopened the same owner"
            );
            ensure!(
                !host.engine.shutdown(Duration::from_secs(5)).await.settled(),
                "shutdown erased Git unknown"
            );
        }
        assert!(host.database.exists());
        drop(host);
        if mode != "unknown" {
            drop(crate::engine::Engine::new(store, 1)?);
        }
        Ok(())
    }
    use crate::engine::Engine;
    #[tokio::test]
    async fn native_git_descendant_retains_ticket_then_normal_git_succeeds() -> Result<()> {
        native_git_case("normal").await
    }
    #[tokio::test]
    async fn native_git_shutdown_cancels_collector_and_settles_ticket() -> Result<()> {
        native_git_case("shutdown").await
    }
    #[tokio::test]
    async fn native_git_unknown_seals_admission_until_and_after_reader_join() -> Result<()> {
        native_git_case("unknown").await
    }

    #[test]
    fn original_ticket_covers_detached_body_between_generations() {
        let root = tempfile::tempdir().unwrap();
        let store =
            Arc::new(crate::store::Store::open(&root.path().join("peachsh.sqlite3")).unwrap());
        let engine = Engine::new(store.clone(), 1).unwrap();
        let original = store.execution_operation(CancellationToken::new()).unwrap();
        let body = original.hold_body();
        drop(original);
        assert_eq!(store.execution_admission_status().unwrap().1, 1);
        drop(engine);
        assert!(Engine::new(store.clone(), 1).is_err());
        let old_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.path().join("instance.lock"))
            .unwrap();
        assert!(old_lock.try_lock().is_err());
        drop(body);
        assert_eq!(store.execution_admission_status().unwrap().1, 0);
        drop(Engine::new(store.clone(), 1).unwrap());
        drop(store);
        old_lock.try_lock().unwrap();
    }
}

#[cfg(all(test, windows))]
pub(crate) fn test_process_once(root: &Path, spec: ProcessSpec) -> process_override::Guard {
    process_override::once(root, spec)
}
