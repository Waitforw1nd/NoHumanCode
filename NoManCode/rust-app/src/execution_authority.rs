//! Lightweight per-database instance lock, connection factory, and process supervisor owner.
use crate::process_lifetime::{DomainLease, Outcome, ProcessSpec, Supervisor};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    future::Future,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct NoopLease;
impl DomainLease for NoopLease {
    fn seal_unknown(&mut self) {}
    fn settle(self: Box<Self>) {}
}

pub(crate) struct ExecutionAuthority {
    path: PathBuf,
    key: String,
    instance_lock: File,
    pub(crate) initialization: Mutex<bool>,
    open: AtomicBool,
    pub(crate) supervisor: Supervisor,
}

impl ExecutionAuthority {
    pub(crate) fn acquire(path: &Path) -> Result<Arc<Self>> {
        let basename = path.file_name().context("database filename missing")?;
        let raw_parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(raw_parent)?;
        let parent = std::fs::canonicalize(raw_parent).unwrap_or_else(|_| raw_parent.to_path_buf());
        let db_path = parent.join(basename);
        let key = if cfg!(windows) {
            db_path.to_string_lossy().to_lowercase()
        } else {
            db_path.to_string_lossy().into_owned()
        };

        static REGISTRY: OnceLock<Mutex<HashMap<String, Weak<ExecutionAuthority>>>> =
            OnceLock::new();
        let mut owners = REGISTRY
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        owners.retain(|_, owner| owner.strong_count() > 0);
        if let Some(existing) = owners.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }

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
        let instance_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&instance_path)?;
        instance_lock
            .try_lock()
            .context("此数据目录已有一个 🍑sh 实例正在运行")?;

        let owner = Arc::new(Self {
            path: db_path,
            key: key.clone(),
            instance_lock,
            initialization: Mutex::new(false),
            open: AtomicBool::new(false),
            supervisor: Supervisor::new(NonZeroUsize::new(16).unwrap()),
        });
        owners.insert(key, Arc::downgrade(&owner));
        Ok(owner)
    }

    pub(crate) fn connection(&self) -> Result<rusqlite::Connection> {
        let db = rusqlite::Connection::open(&self.path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        Ok(db)
    }

    pub(crate) fn instance_lock_handle(&self) -> Result<File> {
        Ok(self.instance_lock.try_clone()?)
    }

    pub(crate) fn claim(self: &Arc<Self>) -> Result<ExecutionController> {
        Ok(ExecutionController {
            authority: self.clone(),
            recovered: AtomicBool::new(false),
        })
    }

    pub(crate) fn check(&self) -> Result<()> {
        Ok(())
    }

    pub(crate) fn identity_key(&self) -> Result<String> {
        Ok(self.key.clone())
    }

    pub(crate) fn close(&self) {
        self.open.store(false, Ordering::Release);
    }

    pub(crate) fn status(&self) -> (bool, usize, usize) {
        (self.open.load(Ordering::Acquire), 0, 0)
    }

    pub(crate) fn operation(
        self: &Arc<Self>,
        cancel: CancellationToken,
    ) -> Result<ExecutionOperation> {
        Ok(ExecutionOperation::new(Some(self.clone()), cancel))
    }
}

pub(crate) struct ExecutionController {
    authority: Arc<ExecutionAuthority>,
    recovered: AtomicBool,
}

impl ExecutionController {
    pub(crate) fn finish_recovery(&self) -> Result<()> {
        self.authority.open.store(true, Ordering::Release);
        self.recovered.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn can_recover(&self) -> bool {
        true
    }

    pub(crate) fn owns(&self, authority: &Arc<ExecutionAuthority>) -> bool {
        Arc::ptr_eq(&self.authority, authority)
    }
}

impl Drop for ExecutionController {
    fn drop(&mut self) {
        self.authority.open.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct ExecutionOperation {
    authority: Option<Arc<ExecutionAuthority>>,
    cancel: CancellationToken,
}

tokio::task_local! {
    static CURRENT: ExecutionOperation;
}

impl ExecutionOperation {
    pub(crate) fn new(
        authority: Option<Arc<ExecutionAuthority>>,
        cancel: CancellationToken,
    ) -> Self {
        Self { authority, cancel }
    }

    pub(crate) fn detached() -> Self {
        Self {
            authority: None,
            cancel: CancellationToken::new(),
        }
    }

    pub(crate) fn bind_task(&self, _task_id: &str) -> Result<()> {
        Ok(())
    }

    pub(crate) fn bind_worktree_operation(
        &self,
        _source_project_id: &str,
        _operation_id: &str,
    ) -> Result<()> {
        Ok(())
    }

    pub(crate) fn verify_worktree_operation(
        &self,
        _source_project_id: &str,
        _operation_id: &str,
    ) -> Result<()> {
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn test_sync<R>(&self, work: impl FnOnce() -> R) -> R {
        CURRENT.sync_scope(self.clone(), work)
    }

    #[allow(dead_code)]
    pub(crate) fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub(crate) fn seal_unknown(&self) {}

    pub(crate) fn generation_for(
        &self,
        _authority: &Arc<ExecutionAuthority>,
    ) -> Result<Box<dyn DomainLease>> {
        Ok(Box::new(NoopLease))
    }

    pub(crate) fn hold_body(&self) -> Self {
        self.clone()
    }

    pub(crate) fn take_worker_cancellation(self) -> Self {
        self
    }

    pub(crate) async fn run<F: Future>(self, work: F) -> F::Output {
        CURRENT.scope(self, work).await
    }
}

pub(crate) fn current() -> Option<ExecutionOperation> {
    Some(
        CURRENT
            .try_with(Clone::clone)
            .unwrap_or_else(|_| ExecutionOperation::detached()),
    )
}

pub(crate) async fn inherit<F: Future>(scope: Option<ExecutionOperation>, work: F) -> F::Output {
    match scope {
        Some(scope) => scope.run(work).await,
        None => work.await,
    }
}

pub(crate) async fn run_process(spec: ProcessSpec) -> Result<Outcome> {
    static DEFAULT_SUPERVISOR: OnceLock<Supervisor> = OnceLock::new();
    let op = current().unwrap_or_else(ExecutionOperation::detached);
    let supervisor = match &op.authority {
        Some(authority) => &authority.supervisor,
        None => DEFAULT_SUPERVISOR.get_or_init(|| Supervisor::new(NonZeroUsize::new(16).unwrap())),
    };
    let lease: Box<dyn DomainLease> = Box::new(NoopLease);
    let operation = match supervisor.submit(spec, lease) {
        Ok(operation) => operation,
        Err(rejected) => {
            rejected.lease.settle();
            anyhow::bail!("process was not submitted: {}", rejected.reason);
        }
    };
    Ok(operation.wait_with_cancel(&op.cancel).await)
}

pub(crate) fn command_spec(
    command: &tokio::process::Command,
    limit: usize,
    deadline: std::time::Instant,
) -> Result<ProcessSpec> {
    let command = command.as_std();
    let program = PathBuf::from(command.get_program());
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
        Duration::from_secs(5),
    );
    spec.args = command.get_args().map(ToOwned::to_owned).collect();
    Ok(spec)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn test_with_active_store_sync<R>(
    store: &crate::store::Store,
    work: impl FnOnce() -> R,
) -> Result<R> {
    let operation = store.execution_operation(CancellationToken::new())?;
    Ok(operation.test_sync(work))
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn test_with_store_sync<R>(
    store: &crate::store::Store,
    work: impl FnOnce() -> R,
) -> Result<R> {
    let controller = store.authority.claim()?;
    store.recover_with_controller(&controller)?;
    let operation = store.execution_operation(CancellationToken::new())?;
    Ok(operation.test_sync(work))
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) async fn test_with_store<F: Future>(
    store: &crate::store::Store,
    work: F,
) -> Result<F::Output> {
    let controller = store.authority.claim()?;
    store.recover_with_controller(&controller)?;
    let operation = store.execution_operation(CancellationToken::new())?;
    Ok(operation.run(work).await)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) async fn test_scope<F: Future>(work: F) -> F::Output {
    let operation = ExecutionOperation::detached();
    operation.run(work).await
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn test_spawn<F>(work: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let scope = current();
    tokio::spawn(inherit(scope, work))
}
