//! A single, real data-domain owner shared by Store, controller and process work.
//! Durable epoch fences complement the original process and I/O lifetime.
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
    epoch: Option<i64>,
}
pub(crate) struct ExecutionAuthority {
    domain: platform::Domain,
    pub(crate) initialization: Mutex<bool>,
    admission: Mutex<Admission>,
    retained_unknown: std::sync::atomic::AtomicBool,
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
        state.open = false;
        drop(state);
        let registration = (|| -> Result<i64> {
            let mut db = self.connection()?;
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            crate::repository::verify_execution_owner(&tx, &self.domain.identity_key()?)?;
            let old: i64 =
                tx.query_row("SELECT epoch FROM execution_owner WHERE id=1", [], |r| {
                    r.get(0)
                })?;
            let next = old.checked_add(1).context("execution epoch exhausted")?;
            ensure!(tx.execute("UPDATE execution_owner SET epoch=?1,phase='recovering' WHERE id=1 AND epoch=?2", rusqlite::params![next, old])? == 1, "execution epoch changed");
            self.domain.verify()?;
            tx.commit()?;
            Ok(next)
        })();
        let epoch = match registration {
            Ok(epoch) => epoch,
            Err(error) => {
                let mut state = self.admission.lock().unwrap_or_else(|p| p.into_inner());
                state.controller = false;
                state.open = false;
                state.unknown = state.unknown.saturating_add(1);
                drop(state);
                self.retain_unknown_owner();
                return Err(error);
            }
        };
        self.admission
            .lock()
            .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?
            .epoch = Some(epoch);
        #[cfg(test)]
        crate::engine::crash_probe::block("u18-owner", "epoch_after_commit", "");
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
    pub(crate) fn identity_key(&self) -> Result<String> {
        self.domain.identity_key()
    }
    pub(crate) fn fence(
        self: &Arc<Self>,
        tx: &rusqlite::Transaction<'_>,
        op: &ExecutionOperation,
        settlement: bool,
    ) -> Result<()> {
        self.fence_scope(tx, &op.scope, settlement)
    }
    fn fence_scope(
        self: &Arc<Self>,
        tx: &rusqlite::Transaction<'_>,
        scope: &OperationScope,
        settlement: bool,
    ) -> Result<()> {
        // Ordinary caller rejection must not poison another domain or turn a
        // clean cancellation into an unknown resource outcome.
        ensure!(
            Arc::ptr_eq(self, &scope.authority),
            "cross-domain operation"
        );
        {
            let operation = scope
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("operation poisoned"))?;
            ensure!(
                operation.bodies > 0
                    && !operation.unknown
                    && !operation.settled
                    && (settlement || !scope.cancel.is_cancelled()),
                "operation closed"
            );
            let state = self
                .admission
                .lock()
                .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
            ensure!(
                state.controller
                    && (settlement || state.open)
                    && (settlement || state.unknown == 0)
                    && state.epoch == Some(scope.epoch),
                "stale operation"
            );
        }
        self.domain.verify_connection(tx)?;
        // A verified same-owner ticket encountering corrupt or changed durable
        // state cannot establish a safe continuation: close this owner.
        let result = (|| {
            self.domain.verify()?;
            crate::repository::verify_execution_owner(tx, &self.domain.identity_key()?)?;
            let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_owner WHERE id=1 AND epoch=?1 AND phase='active')", [scope.epoch], |r| r.get(0))?;
            ensure!(valid, "execution epoch or phase mismatch");
            Ok(())
        })();
        if result.is_err() {
            self.seal_unknown();
        }
        result
    }
    // Keep the original owner and its OS locks alive until process exit when
    // an outcome cannot be confirmed. This does not settle the original I/O.
    fn retain_unknown_owner(self: &Arc<Self>) {
        if !self
            .retained_unknown
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            std::mem::forget(self.clone());
        }
    }
    pub(crate) fn seal_unknown(self: &Arc<Self>) {
        let mut state = self.admission.lock().unwrap_or_else(|p| p.into_inner());
        state.open = false;
        state.unknown = state.unknown.saturating_add(1);
        drop(state);
        self.retain_unknown_owner();
    }
    pub(crate) fn close(&self) {
        let mut state = self.admission.lock().unwrap_or_else(|p| p.into_inner());
        state.open = false;
    }
    pub(crate) fn unknown_terminal_fence(
        self: &Arc<Self>,
        tx: &rusqlite::Transaction<'_>,
        op: &ExecutionOperation,
    ) -> Result<()> {
        ensure!(
            Arc::ptr_eq(self, &op.scope.authority),
            "cross-domain operation"
        );
        {
            let operation = op
                .scope
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("operation poisoned"))?;
            let state = self
                .admission
                .lock()
                .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
            ensure!(
                operation.bodies > 0
                    && !operation.settled
                    && operation.unknown
                    && state.controller
                    && state.unknown > 0
                    && state.epoch == Some(op.scope.epoch),
                "unknown terminal operation required"
            );
        }
        self.domain.verify_connection(tx)?;
        let result = (|| {
            self.domain.verify()?;
            crate::repository::verify_execution_owner(tx, &self.domain.identity_key()?)?;
            let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_owner WHERE id=1 AND epoch=?1 AND phase='active')", [op.scope.epoch], |r| r.get(0))?;
            ensure!(valid, "execution epoch or phase mismatch");
            Ok(())
        })();
        if result.is_err() {
            self.seal_unknown();
        }
        result
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
            cancel_on_drop: Some(CancellationOwner::Caller),
            scope: Arc::new(OperationScope {
                authority: self.clone(),
                epoch: state.epoch.context("execution epoch missing")?,
                cancel: cancel.child_token(),
                state: Mutex::new(OperationState {
                    bodies: 1,
                    task_id: None,
                    network_calls: Default::default(),
                    worktree_operations: Default::default(),
                    cancellation_owner: CancellationOwner::Caller,
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
    pub(crate) fn recovery_fence(&self, tx: &rusqlite::Transaction<'_>) -> Result<()> {
        ensure!(self.can_recover(), "controller already activated");
        let epoch = {
            let state = self
                .0
                .admission
                .lock()
                .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
            ensure!(state.controller && !state.open, "controller cannot recover");
            state.epoch.context("execution epoch missing")?
        };
        self.0.domain.verify_connection(tx)?;
        let result = (|| {
            self.0.domain.verify()?;
            crate::repository::verify_execution_owner(tx, &self.0.domain.identity_key()?)?;
            let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM execution_owner WHERE id=1 AND epoch=?1 AND phase='recovering')", [epoch], |r| r.get(0))?;
            ensure!(valid, "recovery epoch or phase mismatch");
            Ok(())
        })();
        if result.is_err() {
            self.0.seal_unknown();
        }
        result
    }
    pub(crate) fn finish_recovery(&self) -> Result<()> {
        let mut state = self
            .0
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
        ensure!(
            state.controller && !state.open && state.unknown == 0,
            "controller cannot activate"
        );
        state.open = true;
        self.1.store(true, std::sync::atomic::Ordering::Release);
        Ok(())
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
    task_id: Option<String>,
    network_calls: std::collections::HashSet<String>,
    worktree_operations: std::collections::HashSet<(String, String)>,
    cancellation_owner: CancellationOwner,
    generation: u64,
    active: Option<u64>,
    unknown: bool,
    settled: bool,
}
pub(crate) struct OperationScope {
    authority: Arc<ExecutionAuthority>,
    cancel: CancellationToken,
    epoch: i64,
    state: Mutex<OperationState>,
}
pub(crate) struct ExecutionOperation {
    scope: Arc<OperationScope>,
    cancel_on_drop: Option<CancellationOwner>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CancellationOwner {
    Caller,
    Worker,
}
tokio::task_local! { static CURRENT: Arc<OperationScope>; }
impl ExecutionOperation {
    /// Bind identities at their trusted claim boundary. The bindings live only
    /// in this original operation scope and cannot be manufactured from an
    /// epoch or a caller-supplied receipt.
    pub(crate) fn bind_task(&self, task_id: &str) -> Result<()> {
        crate::secrets::validate_persisted_id("task_id", task_id)?;
        let mut state = self
            .scope
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("operation poisoned"))?;
        let admission = self
            .scope
            .authority
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
        ensure!(
            admission.controller
                && admission.open
                && admission.unknown == 0
                && admission.epoch == Some(self.scope.epoch)
                && state.bodies > 0
                && !state.unknown
                && !state.settled
                && !self.scope.cancel.is_cancelled(),
            "operation cannot bind task"
        );
        ensure!(state.task_id.is_none(), "operation task already bound");
        state.task_id = Some(task_id.to_owned());
        Ok(())
    }
    pub(crate) fn bind_network_call(&self, call_id: &str) -> Result<()> {
        crate::secrets::validate_persisted_id("call_id", call_id)?;
        let mut state = self
            .scope
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("operation poisoned"))?;
        let admission = self
            .scope
            .authority
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
        ensure!(
            admission.controller
                && admission.open
                && admission.unknown == 0
                && admission.epoch == Some(self.scope.epoch)
                && state.bodies > 0
                && !state.unknown
                && !state.settled
                && !self.scope.cancel.is_cancelled(),
            "operation cannot bind call"
        );
        ensure!(
            state.network_calls.insert(call_id.to_owned()),
            "operation call already bound"
        );
        Ok(())
    }
    pub(crate) fn bind_worktree_operation(
        &self,
        source_project_id: &str,
        operation_id: &str,
    ) -> Result<()> {
        crate::secrets::validate_persisted_id("project_id", source_project_id)?;
        crate::secrets::validate_persisted_id("operation_id", operation_id)?;
        let mut state = self
            .scope
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("operation poisoned"))?;
        let admission = self
            .scope
            .authority
            .admission
            .lock()
            .map_err(|_| anyhow::anyhow!("domain admission poisoned"))?;
        ensure!(
            admission.controller
                && admission.open
                && admission.unknown == 0
                && admission.epoch == Some(self.scope.epoch)
                && state.bodies > 0
                && !state.unknown
                && !state.settled
                && !self.scope.cancel.is_cancelled(),
            "operation cannot bind worktree"
        );
        // The same original operation may retry a confirmed rolled-back claim.
        state
            .worktree_operations
            .insert((source_project_id.to_owned(), operation_id.to_owned()));
        Ok(())
    }
    pub(crate) fn verify_worktree_operation(
        &self,
        source_project_id: &str,
        operation_id: &str,
    ) -> Result<()> {
        let state = self
            .scope
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("operation poisoned"))?;
        ensure!(
            state.bodies > 0
                && !state.settled
                && state
                    .worktree_operations
                    .contains(&(source_project_id.to_owned(), operation_id.to_owned())),
            "worktree belongs to another operation"
        );
        Ok(())
    }
    pub(crate) fn verify_network_call(&self, call_id: &str) -> Result<()> {
        let state = self
            .scope
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("operation poisoned"))?;
        ensure!(
            state.bodies > 0 && !state.settled && state.network_calls.contains(call_id),
            "network call belongs to another operation"
        );
        Ok(())
    }
    pub(crate) fn verify_task(&self, task_id: &str) -> Result<()> {
        let state = self
            .scope
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("operation poisoned"))?;
        ensure!(
            state.bodies > 0 && !state.settled && state.task_id.as_deref() == Some(task_id),
            "task belongs to another operation"
        );
        Ok(())
    }
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn test_sync<R>(&self, work: impl FnOnce() -> R) -> R {
        CURRENT.sync_scope(self.scope.clone(), work)
    }
    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.scope.cancel.clone()
    }
    /// Mark the original operation outcome indeterminate; this is sticky on
    /// the same authority even if the caller and all counted bodies disappear.
    pub(crate) fn seal_unknown(&self) {
        let mut state = self.scope.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.unknown {
            let mut admission = self
                .scope
                .authority
                .admission
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            state.unknown = true;
            admission.open = false;
            admission.unknown = admission.unknown.saturating_add(1);
            self.scope.authority.retain_unknown_owner();
        }
    }
    pub(crate) fn generation_for(
        &self,
        authority: &Arc<ExecutionAuthority>,
    ) -> Result<Box<dyn DomainLease>> {
        ensure!(
            Arc::ptr_eq(&self.scope.authority, authority),
            "cross-domain operation"
        );
        self.scope.generation()
    }
    pub(crate) fn hold_body(&self) -> Self {
        let mut state = self.scope.state.lock().unwrap_or_else(|p| p.into_inner());
        state.bodies = state
            .bodies
            .checked_add(1)
            .expect("operation body count exhausted");
        Self {
            scope: self.scope.clone(),
            cancel_on_drop: None,
        }
    }

    /// Transfer cancellation of this same operation after its durable claim.
    /// The caller can then disappear while the independently owned worker runs.
    /// The token remains a child of the original Host shutdown token.
    pub(crate) fn take_worker_cancellation(mut self) -> Self {
        {
            let mut state = self.scope.state.lock().unwrap_or_else(|p| p.into_inner());
            assert!(state.cancellation_owner == CancellationOwner::Caller);
            state.cancellation_owner = CancellationOwner::Worker;
            self.cancel_on_drop = Some(CancellationOwner::Worker);
        }
        self
    }

    pub(crate) async fn run<F: Future>(self, work: F) -> F::Output {
        CURRENT.scope(self.scope.clone(), work).await
    }
}
impl Drop for ExecutionOperation {
    fn drop(&mut self) {
        let mut state = self.scope.state.lock().unwrap_or_else(|p| p.into_inner());
        if self.cancel_on_drop == Some(state.cancellation_owner) {
            self.scope.cancel.cancel();
        }
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
        // Take the SQLite writer lock before any operation/admission mutex.
        // The same transaction verifies the pinned main handle and captured
        // epoch before reserving the generation. No external work starts until
        // its commit succeeds.
        let mut db = self.authority.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        self.authority.fence_scope(&tx, self, false)?;
        let id = {
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
                admission.controller
                    && admission.open
                    && admission.unknown == 0
                    && admission.epoch == Some(self.epoch),
                "data domain admission is closed"
            );
            ensure!(!self.cancel.is_cancelled(), "operation was cancelled");
            ensure!(
                state.bodies != 0 && !state.unknown && !state.settled && state.active.is_none(),
                "operation generation is closed or busy"
            );
            let id = state
                .generation
                .checked_add(1)
                .context("operation generation exhausted")?;
            state.generation = id;
            state.active = Some(id);
            id
        };
        if let Err(error) = tx.commit() {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.active == Some(id) {
                state.active = None;
            }
            drop(state);
            self.authority.seal_unknown();
            return Err(error.into());
        }
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
            self.scope.authority.retain_unknown_owner();
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
                cancel_on_drop: None,
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
        pub(crate) fn identity_key(&self) -> Result<String> {
            anyhow::bail!("unsupported data domain")
        }
        pub(crate) fn verify_connection(&self, _: &rusqlite::Connection) -> Result<()> {
            anyhow::bail!("unsupported data domain")
        }
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
            // Preserve the Host's existing diagnostic at the same legacy lock
            // acquisition boundary, now owned by the shared domain.
            file.try_lock()
                .context("此数据目录已有一个 🍑sh 实例正在运行")?;
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
            retained_unknown: std::sync::atomic::AtomicBool::new(false),
            supervisor: Supervisor::new(NonZeroUsize::new(1).unwrap()),
        });
        owner.domain.verify()?;
        owners.insert(identity, Arc::downgrade(&owner));
        Ok(owner)
    }
    impl Domain {
        pub(crate) fn identity_key(&self) -> Result<String> {
            Ok(format!(
                "{:08x}:{:016x}",
                self.identity.volume, self.identity.index
            ))
        }
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
        pub(crate) fn verify_connection(&self, db: &rusqlite::Connection) -> Result<()> {
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
            Ok(())
        }
        pub(crate) fn connection(&self) -> Result<rusqlite::Connection> {
            self.verify()?;
            let db = rusqlite::Connection::open(&self.path)?;
            self.verify_connection(&db)?;
            self.verify()?;
            Ok(db)
        }
    }
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

/// Run test fixtures under the same controller/recovery/operation path as production.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn test_with_store_sync<R>(
    store: &crate::store::Store,
    work: impl FnOnce() -> R,
) -> Result<R> {
    let controller = store.authority.claim()?;
    store.recover_with_controller(&controller)?;
    let operation = store.execution_operation(CancellationToken::new())?;
    Ok(CURRENT.sync_scope(operation.scope.clone(), || {
        let _operation = operation;
        work()
    }))
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

    struct ReaderRelease(Arc<TestHooks>);
    impl Drop for ReaderRelease {
        fn drop(&mut self) {
            // Also run on any early assertion/fixture error. The independent
            // holder can then drain and retain its real objects as necessary.
            self.0.release_stdout_reader.store(true, Ordering::SeqCst);
        }
    }

    async fn native_git_case(mode: &str) -> Result<()> {
        let mut case = Case::new(&format!("native-git-{mode}-"))?;
        let host = Host::new(&case).await?;
        let (task, _) = host.shell(r#"git init --quiet; git config user.name lifecycle; git config user.email lifecycle@example.invalid; [IO.File]::WriteAllText((Join-Path $PWD 'a.txt'), "old`n"); git add a.txt; git commit --quiet -m fixture; [IO.File]::WriteAllText((Join-Path $PWD 'a.txt'), "new`n")"#, 20).await?;
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
        let _reader_release = ReaderRelease(hooks.clone());
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
        let remaining_before_shutdown = usize::from(mode == "shutdown");
        until(
            || Ok(host.engine.store.execution_admission_status()?.1 == remaining_before_shutdown),
        )
        .await?;
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
            let status = store.execution_admission_status()?;
            ensure!(
                !status.0 && status.1 == 0 && status.2 == 0,
                "shutdown ticket or unknown remains"
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
    fn worktree_handoff_keeps_original_ticket_and_parent_cancellation() {
        for stop_host in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store =
                Arc::new(crate::store::Store::open(&root.path().join("handoff.db")).unwrap());
            let _engine = Engine::new(store.clone(), 1).unwrap();
            let shutdown = CancellationToken::new();
            let original = store.execution_operation(shutdown.clone()).unwrap();
            let cancelled = original.scope.cancel.clone();
            let worker = original.hold_body().take_worker_cancellation();
            drop(original);
            assert!(
                !cancelled.is_cancelled(),
                "waiter drop cancelled committed worker"
            );
            assert_eq!(store.execution_admission_status().unwrap().1, 1);
            assert!(Engine::new(store.clone(), 1).is_err());
            if stop_host {
                shutdown.cancel();
                assert!(
                    cancelled.is_cancelled(),
                    "parent stop lost original token chain"
                );
            }
            drop(worker);
            assert!(
                cancelled.is_cancelled(),
                "worker drop must cancel remaining process work"
            );
            assert_eq!(store.execution_admission_status().unwrap().1, 0);
        }
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
