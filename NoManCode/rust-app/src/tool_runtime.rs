//! Production builtin file-tool assembly. The host owns admission; services
//! receive no Store, approval authority, credentials, or persistent lease.
use crate::plugin_catalog::{
    CatalogManifestV1, ExactVersion, InterfaceKey, InterfaceKind, LifecycleKind, RuntimeKind,
    ScopeKey, ScopeKind, Surface,
};
use crate::plugin_host::{
    BuiltinPlugin, FailureStage, HostError, InstanceState, PluginContext, PluginError,
    PluginFailureCause, PluginHost,
};
use crate::{domain::Task, workspace};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};

const FILES: &str = "builtin.files";
const CONSUMER: &str = "builtin.engine-tools";
const FILE_TOOLS: [&str; 2] = ["read_file", "write_file"];
const COMPAT_TOOLS: [&str; 4] = ["list_files", "search_files", "run_command", "run_wasm"];
type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

// Private: only this assembly can create services or obtain their raw Arc.
trait ToolServiceV1: Send + Sync {
    fn definition(&self, task: &Task) -> Option<Value>;
    fn execute<'a>(&'a self, task: &'a Task, args: &'a Value) -> ToolFuture<'a>;
}
type Service = Arc<dyn ToolServiceV1 + Send + Sync>;

struct WorkspaceService(&'static str);
impl ToolServiceV1 for WorkspaceService {
    fn definition(&self, task: &Task) -> Option<Value> {
        workspace::definitions(task)
            .into_iter()
            .find(|definition| definition["function"]["name"] == self.0)
    }
    fn execute<'a>(&'a self, task: &'a Task, args: &'a Value) -> ToolFuture<'a> {
        Box::pin(workspace::execute(task, self.0, args))
    }
}
fn key(name: &str) -> InterfaceKey {
    InterfaceKey {
        kind: InterfaceKind::Service,
        name: format!("tool.{name}"),
        version: ExactVersion {
            major: 1,
            minor: 0,
            patch: 0,
        },
    }
}
fn manifest(id: &str, provider: bool) -> CatalogManifestV1 {
    let keys = FILE_TOOLS.iter().map(|name| key(name)).collect();
    CatalogManifestV1 {
        manifest_version: 1,
        id: id.into(),
        display_name: id.into(),
        version: ExactVersion {
            major: 1,
            minor: 0,
            patch: 0,
        },
        scope: ScopeKind::Host,
        runtime: RuntimeKind::Builtin,
        lifecycle: LifecycleKind::HostManaged,
        surfaces: vec![Surface::Cli, Surface::Headless],
        permissions: if provider {
            vec!["filesystem.read".into(), "filesystem.write".into()]
        } else {
            vec![]
        },
        config_schema: serde_json::json!({}),
        provides: if provider { keys } else { vec![] },
        requires: if provider {
            vec![]
        } else {
            FILE_TOOLS.iter().map(|name| key(name)).collect()
        },
    }
}
struct FilesPlugin {
    services: [Service; 2],
}
impl BuiltinPlugin for FilesPlugin {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        for (name, service) in FILE_TOOLS.iter().zip(&self.services) {
            ctx.register_effect(&key(name), Box::new(service.clone()))
                .map_err(|error| PluginError::new(error.to_string()))?;
        }
        Ok(())
    }
    fn deactivate(&mut self, _: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}
struct ConsumerPlugin;
impl BuiltinPlugin for ConsumerPlugin {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        for name in FILE_TOOLS {
            let binding = ctx
                .bound(&key(name))
                .map_err(|error| PluginError::new(error.to_string()))?;
            if binding.payload().downcast_ref::<Service>().is_none() {
                return Err(PluginError::new("file tool payload type mismatch"));
            }
        }
        Ok(())
    }
    fn deactivate(&mut self, _: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}

/// Safe current observation, without payloads or arbitrary plugin error messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesRuntimeStatus {
    pub state: FilesRuntimeState,
    pub admission_open: bool,
    pub in_flight: u64,
    pub draining: bool,
    pub instances: Vec<FilesInstanceStatus>,
    pub providers: Vec<FilesProviderStatus>,
    pub effects_count: u64,
    pub subscriptions_count: u64,
    pub recovery: Vec<FilesRecoveryStatus>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesRuntimeState {
    Active,
    Stopped,
    Unloaded,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesInstanceState {
    Registered,
    Loaded,
    Active,
    Stopped,
    Unloaded,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesFailureStage {
    Activate,
    Deactivate,
    Deliver,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesFailureCauseCode {
    PluginError,
    Panic,
    ProvidesNotCovered,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesInstanceStatus {
    pub plugin_id: String,
    pub state: FilesInstanceState,
    pub failure_stage: Option<FilesFailureStage>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesProviderStatus {
    pub key: InterfaceKey,
    pub provider_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesRecoveryStatus {
    pub plugin_id: String,
    pub stage: FilesFailureStage,
    pub cause_code: FilesFailureCauseCode,
}
impl From<FailureStage> for FilesFailureStage {
    fn from(stage: FailureStage) -> Self {
        match stage {
            FailureStage::Activate => Self::Activate,
            FailureStage::Deactivate => Self::Deactivate,
            FailureStage::Deliver => Self::Deliver,
        }
    }
}
fn cause_code(cause: &PluginFailureCause) -> FilesFailureCauseCode {
    match cause {
        PluginFailureCause::PluginError { .. } => FilesFailureCauseCode::PluginError,
        PluginFailureCause::Panic => FilesFailureCauseCode::Panic,
        PluginFailureCause::ProvidesNotCovered { .. } => FilesFailureCauseCode::ProvidesNotCovered,
    }
}
struct RuntimeState {
    host: PluginHost,
    admission_open: bool,
    apply_failed: bool,
    activation_recovery: Vec<FilesRecoveryStatus>,
}
impl std::ops::Deref for RuntimeState {
    type Target = PluginHost;
    fn deref(&self) -> &Self::Target {
        &self.host
    }
}
/// Restricted management view. Registry and execution handles never escape.
pub(crate) struct FilesControl<'a> {
    state: &'a mut RuntimeState,
    in_flight: &'a Arc<AtomicU64>,
}
impl FilesControl<'_> {
    pub(crate) fn snapshot(&self) -> FilesRuntimeStatus {
        let actual = self.state.host.instances();
        let instances: Vec<_> = [FILES, CONSUMER]
            .into_iter()
            .map(|id| {
                let state = actual
                    .iter()
                    .find(|i| i.id == id)
                    .map(|i| i.state)
                    .unwrap_or(InstanceState::Unloaded);
                let (state, failure_stage) = match state {
                    InstanceState::Registered => (FilesInstanceState::Registered, None),
                    InstanceState::Loaded => (FilesInstanceState::Loaded, None),
                    InstanceState::Active => (FilesInstanceState::Active, None),
                    InstanceState::Stopped => (FilesInstanceState::Stopped, None),
                    InstanceState::Unloaded => (FilesInstanceState::Unloaded, None),
                    InstanceState::Failed { stage } => {
                        (FilesInstanceState::Failed, Some(stage.into()))
                    }
                };
                FilesInstanceStatus {
                    plugin_id: id.into(),
                    state,
                    failure_stage,
                }
            })
            .collect();
        let active = ToolRuntime::files_active(&self.state.host);
        let state = if self.state.apply_failed
            || instances
                .iter()
                .any(|i| i.state == FilesInstanceState::Failed)
        {
            FilesRuntimeState::Failed
        } else if active {
            FilesRuntimeState::Active
        } else if instances
            .iter()
            .all(|i| i.state == FilesInstanceState::Unloaded)
        {
            FilesRuntimeState::Unloaded
        } else {
            FilesRuntimeState::Stopped
        };
        let effects = self.state.host.effects();
        let providers = effects
            .iter()
            .filter(|e| !e.is_subscription)
            .map(|e| FilesProviderStatus {
                key: e.key.clone(),
                provider_id: e.plugin_id.clone(),
            })
            .collect();
        let mut recovery = self.state.activation_recovery.clone();
        recovery.extend(self.state.host.pending_recovery().into_iter().map(|r| {
            FilesRecoveryStatus {
                plugin_id: r.plugin_id,
                stage: r.stage.into(),
                cause_code: cause_code(&r.cause),
            }
        }));
        let in_flight = self.in_flight.load(Ordering::SeqCst);
        let admission_open = self.state.admission_open && active && !self.state.apply_failed;
        FilesRuntimeStatus {
            state,
            admission_open,
            in_flight,
            draining: !admission_open && in_flight > 0,
            instances,
            providers,
            effects_count: effects.len() as u64,
            subscriptions_count: effects.iter().filter(|e| e.is_subscription).count() as u64,
            recovery,
        }
    }
    /// Only explicit new intent reconciles. Lifecycle failure is observed state,
    /// not an error suggesting that the preceding durable commit was rolled back.
    pub(crate) fn reconcile(&mut self, enabled: bool) -> FilesRuntimeStatus {
        self.state.admission_open = false;
        if !enabled {
            let previously_failed = self.state.apply_failed
                || self
                    .state
                    .host
                    .instances()
                    .iter()
                    .any(|i| matches!(i.state, InstanceState::Failed { .. }));
            // A new disabled intent can explicitly finish cleanup from an older
            // failure. Never conceal a failure produced by this very stop.
            if self.stop_active() && previously_failed {
                let mut unloaded = true;
                for id in [CONSUMER, FILES] {
                    if self.state.host.instances().iter().any(|i| i.id == id)
                        && self.state.host.unload(id).is_err()
                    {
                        unloaded = false;
                    }
                }
                self.state.apply_failed = !unloaded;
            }
            return self.snapshot();
        }
        if ToolRuntime::files_active(&self.state.host) && !self.state.apply_failed {
            self.state.admission_open = true;
            return self.snapshot();
        }
        // Failed instances must be unloaded, never directly restarted.
        if !self.stop_active() {
            return self.snapshot();
        }
        for id in [CONSUMER, FILES] {
            if self.state.host.instances().iter().any(|i| i.id == id)
                && self.state.host.unload(id).is_err()
            {
                self.state.apply_failed = true;
                return self.snapshot();
            }
        }
        self.state.apply_failed = false;
        match self.state.host.start(&[CONSUMER.to_owned()]) {
            Ok(_) => self.state.admission_open = ToolRuntime::files_active(&self.state.host),
            Err(error) => {
                self.state.apply_failed = true;
                let record = match error {
                    HostError::PluginStartError {
                        plugin_id,
                        stage,
                        cause,
                        ..
                    } => Some(FilesRecoveryStatus {
                        plugin_id,
                        stage: stage.into(),
                        cause_code: cause_code(&cause),
                    }),
                    HostError::PluginPanic { plugin_id, stage } => Some(FilesRecoveryStatus {
                        plugin_id,
                        stage: stage.into(),
                        cause_code: FilesFailureCauseCode::Panic,
                    }),
                    HostError::ProvidesNotCovered { plugin_id, .. } => Some(FilesRecoveryStatus {
                        plugin_id,
                        stage: FilesFailureStage::Activate,
                        cause_code: FilesFailureCauseCode::ProvidesNotCovered,
                    }),
                    _ => None,
                };
                if let Some(record) = record {
                    self.state.activation_recovery.push(record);
                }
            }
        }
        self.snapshot()
    }
    fn stop_active(&mut self) -> bool {
        let mut success = true;
        // Consumer failure must not leave the provider active.
        for id in [CONSUMER, FILES] {
            if self
                .state
                .host
                .instances()
                .iter()
                .any(|i| i.id == id && i.state == InstanceState::Active)
                && self.state.host.stop(id).is_err()
            {
                success = false;
            }
        }
        if !success {
            self.state.apply_failed = true;
        }
        success
    }
}
/// Admission and management share this mutex; lease release never acquires it.
pub struct ToolRuntime {
    host: Mutex<RuntimeState>,
    in_flight: Arc<AtomicU64>,
}
struct FileLeaseCount(Arc<AtomicU64>);
impl Drop for FileLeaseCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Single-use admitted service. Not Clone, not persistent, and not a public
/// tool execution API. Only Engine's existing approval/perform_tool path calls it.
pub struct ToolLease {
    _file_count: Option<FileLeaseCount>,
    service: Service,
    task_id: String,
    workspace: String,
    spec: Value,
}
impl ToolRuntime {
    pub fn builtin() -> Result<Self> {
        Self::builtin_with_enabled(true)
    }
    pub fn builtin_with_enabled(enabled: bool) -> Result<Self> {
        Self::assemble_enabled(
            [
                Arc::new(WorkspaceService("read_file")),
                Arc::new(WorkspaceService("write_file")),
            ],
            enabled,
        )
    }
    #[cfg(test)]
    fn assemble(services: [Service; 2]) -> Result<Self> {
        Self::assemble_enabled(services, true)
    }
    fn assemble_enabled(services: [Service; 2], enabled: bool) -> Result<Self> {
        let mut host = PluginHost::new(ScopeKey::Host)?;
        host.catalog_mut().register(manifest(FILES, true))?;
        host.catalog_mut().register(manifest(CONSUMER, false))?;
        host.with_factory(FILES, move || {
            Box::new(FilesPlugin {
                services: services.clone(),
            })
        });
        host.with_factory(CONSUMER, || Box::new(ConsumerPlugin));
        Self::from_host(host, enabled)
    }
    fn from_host(host: PluginHost, enabled: bool) -> Result<Self> {
        let runtime = Self {
            host: Mutex::new(RuntimeState {
                host,
                admission_open: false,
                apply_failed: false,
                activation_recovery: vec![],
            }),
            in_flight: Arc::new(AtomicU64::new(0)),
        };
        if enabled {
            runtime.with_files_control(|control| {
                control.reconcile(true);
                Ok(())
            })?;
        }
        Ok(runtime)
    }
    fn lock(&self) -> Result<MutexGuard<'_, RuntimeState>> {
        self.host
            .lock()
            .map_err(|_| anyhow::anyhow!("tool runtime lock poisoned"))
    }
    fn files_active(host: &PluginHost) -> bool {
        [FILES, CONSUMER].iter().all(|id| {
            host.instances()
                .iter()
                .any(|instance| instance.id == *id && instance.state == InstanceState::Active)
        })
    }
    /// Synchronous short callback only: runtime -> Store; no await, Engine gate,
    /// runtime re-entry, or filesystem execution.
    pub(crate) fn with_files_control<R>(
        &self,
        f: impl FnOnce(&mut FilesControl<'_>) -> Result<R>,
    ) -> Result<R> {
        let mut state = self.lock()?;
        f(&mut FilesControl {
            state: &mut state,
            in_flight: &self.in_flight,
        })
    }
    fn file_service(host: &PluginHost, name: &str) -> Result<Service> {
        Ok(host.resolve_bound::<Service>(CONSUMER, &key(name))?.payload)
    }
    /// Definitions from real registry payloads, with explicitly unconverted
    /// compatibility tools. Revoked file tools never enter the fallback list.
    pub fn definitions(&self, task: &Task) -> Result<Vec<Value>> {
        let host = self.lock()?;
        let mut result = workspace::definitions(task)
            .into_iter()
            .filter(|definition| {
                definition["function"]["name"]
                    .as_str()
                    .is_some_and(|name| COMPAT_TOOLS.contains(&name))
            })
            .collect::<Vec<_>>();
        if host.admission_open && !host.apply_failed && Self::files_active(&host) {
            for name in FILE_TOOLS {
                if !workspace::definitions(task)
                    .iter()
                    .any(|definition| definition["function"]["name"] == name)
                {
                    continue;
                }
                if let Some(definition) = Self::file_service(&host, name)?.definition(task) {
                    validate_definition(&definition, name)?;
                    result.push(definition);
                }
            }
        }
        Ok(result)
    }
    /// `admit` must be short and synchronous (e.g. Store::claim_approval), must
    /// not re-enter this runtime, await, or execute a filesystem tool. Never
    /// acquire the runtime while holding Store's lock. No lease exists during
    /// approval waiting. A successful admission may finish after revocation.
    pub fn acquire<A>(
        &self,
        task: &Task,
        name: &str,
        admit: impl FnOnce() -> Result<A>,
    ) -> Result<(ToolLease, A)> {
        let host = self.lock()?;
        // Host permission ceiling is independent of plugin-provided metadata.
        ensure!(
            workspace::definitions(task)
                .iter()
                .any(|definition| definition["function"]["name"] == name),
            "工具未获任务授权"
        );
        let is_file = FILE_TOOLS.contains(&name);
        let service: Service = if is_file {
            ensure!(
                host.admission_open && !host.apply_failed && Self::files_active(&host),
                "file tools unavailable"
            );
            ensure!(
                self.in_flight.load(Ordering::SeqCst) < u64::MAX,
                "file lease count exhausted"
            );
            Self::file_service(&host, name)?
        } else if let Some(&name) = COMPAT_TOOLS.iter().find(|candidate| **candidate == name) {
            Arc::new(WorkspaceService(name))
        } else {
            bail!("未知工具");
        };
        let definition = service.definition(task).context("工具当前不可用")?;
        validate_definition(&definition, name)?;
        // Do all fallible snapshot work before claim, so an admitted call cannot
        // disappear because creating its lease failed afterwards.
        let mut lease = ToolLease {
            _file_count: None,
            service,
            task_id: task.id.clone(),
            workspace: task.workspace.clone(),
            spec: serde_json::to_value(&task.spec)?,
        };
        let admitted = admit()?;
        if is_file {
            self.in_flight.fetch_add(1, Ordering::SeqCst);
            lease._file_count = Some(FileLeaseCount(self.in_flight.clone()));
        }
        drop(host);
        Ok((lease, admitted))
    }
    /// Legacy in-memory control only, not persistent management.
    pub fn disable_builtin_files(&self) -> Result<()> {
        self.with_files_control(|control| {
            let status = control.reconcile(false);
            ensure!(
                status.state != FilesRuntimeState::Failed,
                "file runtime stop failed"
            );
            Ok(())
        })
    }
    /// Legacy in-memory unload; preserves recovery history.
    pub fn unload_builtin_files(&self) -> Result<()> {
        self.with_files_control(|control| {
            control.reconcile(false);
            for id in [CONSUMER, FILES] {
                if control.state.host.instances().iter().any(|i| i.id == id) {
                    control.state.host.unload(id)?;
                }
            }
            control.state.apply_failed = false;
            Ok(())
        })
    }
}
fn validate_definition(definition: &Value, name: &str) -> Result<()> {
    ensure!(
        definition["type"] == "function" && definition["function"]["name"] == name,
        "tool service returned a mismatched definition"
    );
    Ok(())
}
impl ToolLease {
    // Used by Engine after D's candidate is integrated into B. Until then the
    // isolated module has no production caller, but unit tests execute it.
    #[allow(dead_code)]
    pub(crate) async fn execute(self, task: &Task, args: &Value) -> Result<Value> {
        ensure!(
            self.task_id == task.id
                && self.workspace == task.workspace
                && self.spec == serde_json::to_value(&task.spec)?,
            "tool lease task binding changed"
        );
        self.service.execute(task, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    const WAIT: std::time::Duration = std::time::Duration::from_secs(5);
    fn task(root: &str) -> Task {
        serde_json::from_value(json!({"id":"task","run_id":"run",
            "spec":{"name":"worker","role":"test","route_id":"route","prompt":"test",
                "depends_on":[],"write_scopes":["*"],"tools":true,"allow_commands":false,"max_rounds":1},
            "route":{"id":"route","name":"route","base_url":"http://127.0.0.1","model":"mock",
                "max_tokens":16,"parallel_limit":1,"key_env":null},
            "workspace":root,"status":"running","output":"","error":null,"messages":[],
            "usage":null,"created_at":0,"updated_at":0})).unwrap()
    }
    struct Marker(Arc<AtomicUsize>);
    impl ToolServiceV1 for Marker {
        fn definition(&self, _: &Task) -> Option<Value> {
            Some(
                json!({"type":"function","function":{"name":"read_file","description":"registry marker"}}),
            )
        }
        fn execute<'a>(&'a self, _: &'a Task, _: &'a Value) -> ToolFuture<'a> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"marker":true}))
            })
        }
    }
    fn marker_runtime() -> (ToolRuntime, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let runtime = ToolRuntime::assemble([
            Arc::new(Marker(hits.clone())),
            Arc::new(WorkspaceService("write_file")),
        ])
        .unwrap();
        (runtime, hits)
    }
    #[tokio::test]
    async fn n08_definition_and_execution_use_actual_registry_payload() {
        let (runtime, hits) = marker_runtime();
        let task = task("unused");
        assert!(
            runtime
                .definitions(&task)
                .unwrap()
                .iter()
                .any(|d| d["function"]["description"] == "registry marker")
        );
        let (lease, admitted) = runtime.acquire(&task, "read_file", || Ok(41)).unwrap();
        assert_eq!(admitted, 41);
        assert_eq!(
            lease.execute(&task, &json!({})).await.unwrap(),
            json!({"marker":true})
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn n09_admit_first_can_finish_after_stop_without_retaining_registry_effects() {
        let (runtime, hits) = marker_runtime();
        let runtime = Arc::new(runtime);
        let task = task("unused");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let acquired = {
            let runtime = runtime.clone();
            let task = task.clone();
            std::thread::spawn(move || {
                runtime
                    .acquire(&task, "read_file", || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv_timeout(WAIT).unwrap();
                        Ok("claimed")
                    })
                    .unwrap()
            })
        };
        entered_rx.recv_timeout(WAIT).unwrap();
        let stopper = {
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                attempt_tx.send(()).unwrap();
                runtime.disable_builtin_files().unwrap();
                done_tx.send(()).unwrap();
            })
        };
        attempt_rx.recv_timeout(WAIT).unwrap();
        assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(WAIT).unwrap();
        let (lease, claim) = acquired.join().unwrap();
        stopper.join().unwrap();
        assert_eq!(claim, "claimed");
        assert!(runtime.lock().unwrap().effects().is_empty());
        assert_eq!(status(&runtime).in_flight, 1);
        assert!(status(&runtime).draining);
        assert_eq!(
            lease.execute(&task, &json!({})).await.unwrap(),
            json!({"marker":true})
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(status(&runtime).in_flight, 0);
        assert!(!status(&runtime).draining);
        assert!(
            runtime
                .acquire(&task, "read_file", || -> Result<()> {
                    panic!("must not claim after stop")
                })
                .is_err()
        );
    }
    #[test]
    fn n09_stop_first_rejects_admission_and_unload_leaves_no_effects() {
        let runtime = Arc::new(ToolRuntime::builtin().unwrap());
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = {
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                stopped_rx.recv_timeout(WAIT).unwrap();
                assert!(
                    runtime
                        .acquire(&task("unused"), "write_file", || -> Result<()> {
                            panic!("stopped must never admit")
                        })
                        .is_err()
                );
                done_tx.send(()).unwrap();
            })
        };
        runtime.disable_builtin_files().unwrap();
        stopped_tx.send(()).unwrap();
        done_rx.recv_timeout(WAIT).unwrap();
        worker.join().unwrap();
        runtime.unload_builtin_files().unwrap();
        runtime.unload_builtin_files().unwrap();
        let snapshot = status(&runtime);
        assert_eq!(snapshot.effects_count, 0);
        assert_eq!(snapshot.instances.len(), 2);
        assert!(
            snapshot
                .instances
                .iter()
                .all(|i| i.state == FilesInstanceState::Unloaded)
        );
    }
    #[test]
    fn n09_claim_failure_returns_no_lease_and_releases_lock_for_stop() {
        let (runtime, hits) = marker_runtime();
        let runtime = Arc::new(runtime);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = {
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                runtime
                    .acquire(&task("unused"), "read_file", || -> Result<()> {
                        entered_tx.send(()).unwrap();
                        release_rx.recv_timeout(WAIT).unwrap();
                        bail!("claim conflict")
                    })
                    .err()
                    .unwrap()
                    .to_string()
            })
        };
        entered_rx.recv_timeout(WAIT).unwrap();
        let stopper = {
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                runtime.disable_builtin_files().unwrap();
                done_tx.send(()).unwrap();
            })
        };
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(worker.join().unwrap(), "claim conflict");
        stopper.join().unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert_eq!(status(&runtime).in_flight, 0);
        assert_eq!(status(&runtime).effects_count, 0);
    }
    #[tokio::test]
    async fn builtin_executes_existing_file_implementation_and_preserves_path_checks() {
        let root = tempfile::tempdir().unwrap();
        let task = task(root.path().to_str().unwrap());
        let runtime = ToolRuntime::builtin().unwrap();
        let (lease, ()) = runtime.acquire(&task, "write_file", || Ok(())).unwrap();
        lease
            .execute(&task, &json!({"path":"a.txt","content":"hello"}))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
            "hello"
        );
        let (lease, ()) = runtime.acquire(&task, "read_file", || Ok(())).unwrap();
        assert_eq!(
            lease
                .execute(&task, &json!({"path":"a.txt"}))
                .await
                .unwrap()["content"],
            "hello"
        );
        let (lease, ()) = runtime.acquire(&task, "write_file", || Ok(())).unwrap();
        assert!(
            lease
                .execute(&task, &json!({"path":"../escape.txt","content":"bad"}))
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
            "hello"
        );
    }
    #[tokio::test]
    async fn lease_cannot_be_retargeted_to_another_task_or_permission_snapshot() {
        let (runtime, hits) = marker_runtime();
        let mut task = task("unused");
        let (lease, ()) = runtime.acquire(&task, "read_file", || Ok(())).unwrap();
        task.spec.allow_commands = true;
        assert!(lease.execute(&task, &json!({})).await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    fn status(runtime: &ToolRuntime) -> FilesRuntimeStatus {
        runtime
            .with_files_control(|control| Ok(control.snapshot()))
            .unwrap()
    }
    fn reconcile(runtime: &ToolRuntime, enabled: bool) -> FilesRuntimeStatus {
        runtime
            .with_files_control(|control| Ok(control.reconcile(enabled)))
            .unwrap()
    }
    #[test]
    fn f02_f10_snapshot_is_read_only_and_repeated_control_has_exact_registry() {
        let (runtime, hits) = marker_runtime();
        let first = status(&runtime);
        assert_eq!(first.state, FilesRuntimeState::Active);
        assert!(first.admission_open);
        assert_eq!((first.effects_count, first.subscriptions_count), (2, 0));
        assert_eq!(
            first
                .providers
                .iter()
                .map(|p| (&p.key.name, p.provider_id.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (&"tool.read_file".to_owned(), FILES),
                (&"tool.write_file".to_owned(), FILES)
            ]
        );
        assert_eq!(first.instances.len(), 2);
        assert!(
            first
                .instances
                .iter()
                .all(|i| i.state == FilesInstanceState::Active && i.failure_stage.is_none())
        );
        assert_eq!(
            serde_json::from_value::<FilesRuntimeStatus>(serde_json::to_value(&first).unwrap())
                .unwrap(),
            first
        );
        for _ in 0..3 {
            assert_eq!(status(&runtime), first);
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        for _ in 0..3 {
            let stopped = reconcile(&runtime, false);
            assert_eq!(stopped.state, FilesRuntimeState::Stopped);
            assert!(!stopped.admission_open);
            assert_eq!((stopped.effects_count, stopped.subscriptions_count), (0, 0));
            assert!(stopped.providers.is_empty());
            assert_eq!(reconcile(&runtime, false), stopped);
            assert_eq!(reconcile(&runtime, true), first);
            assert_eq!(reconcile(&runtime, true), first);
        }
    }
    #[tokio::test]
    async fn f04_real_write_lease_finishes_after_disable_and_all_release_paths_count() {
        let root = tempfile::tempdir().unwrap();
        let task = task(root.path().to_str().unwrap());
        let runtime = ToolRuntime::builtin().unwrap();
        let (lease, receipt) = runtime
            .acquire(&task, "write_file", || Ok(json!({"claimed":17})))
            .unwrap();
        assert_eq!(status(&runtime).in_flight, 1);
        let stopped = reconcile(&runtime, false);
        assert!(stopped.draining);
        assert_eq!(stopped.in_flight, 1);
        assert_eq!(stopped.effects_count, 0);
        assert_eq!(receipt, json!({"claimed":17}));
        lease
            .execute(&task, &json!({"path":"real.txt","content":"once"}))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("real.txt")).unwrap(),
            "once"
        );
        assert_eq!(status(&runtime).in_flight, 0);
        assert!(!status(&runtime).draining);
        assert!(
            runtime
                .acquire(&task, "write_file", || -> Result<()> {
                    panic!("disabled claim")
                })
                .is_err()
        );
        // Compat remains available under the same original task permissions.
        let (compat, ()) = runtime.acquire(&task, "list_files", || Ok(())).unwrap();
        assert_eq!(status(&runtime).in_flight, 0);
        compat.execute(&task, &json!({"path":"."})).await.unwrap();
        reconcile(&runtime, true);
        let (dropped, ()) = runtime.acquire(&task, "read_file", || Ok(())).unwrap();
        runtime
            .with_files_control(|control| {
                assert_eq!(control.snapshot().in_flight, 1);
                drop(dropped); // Drop while the runtime lock is held must not lock it.
                assert_eq!(control.snapshot().in_flight, 0);
                Ok(())
            })
            .unwrap();
        let (bad, ()) = runtime.acquire(&task, "write_file", || Ok(())).unwrap();
        assert!(
            bad.execute(&task, &json!({"path":"../outside","content":"no"}))
                .await
                .is_err()
        );
        assert_eq!(status(&runtime).in_flight, 0);
        assert!(
            runtime
                .acquire(&task, "read_file", || -> Result<()> {
                    bail!("claim rejected")
                })
                .is_err()
        );
        assert_eq!(status(&runtime).in_flight, 0);
        let (bound, ()) = runtime.acquire(&task, "read_file", || Ok(())).unwrap();
        let mut wrong = task.clone();
        wrong.id = "other".into();
        assert!(
            bound
                .execute(&wrong, &json!({"path":"real.txt"}))
                .await
                .is_err()
        );
        assert_eq!(status(&runtime).in_flight, 0);
    }
    struct ProbePlugin {
        id: &'static str,
        mode: Arc<AtomicUsize>,
        events: Arc<Mutex<Vec<String>>>,
    }
    impl BuiltinPlugin for ProbePlugin {
        fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
            self.events
                .lock()
                .unwrap()
                .push(format!("start:{}", self.id));
            if self.id == FILES {
                for name in FILE_TOOLS {
                    let service: Service = Arc::new(WorkspaceService(name));
                    ctx.register_effect(&key(name), Box::new(service)).unwrap();
                }
            } else {
                for name in FILE_TOOLS {
                    assert_eq!(ctx.bound(&key(name)).unwrap().provider_id(), FILES);
                }
            }
            match self.mode.load(Ordering::SeqCst) {
                1 => Err(PluginError::new(
                    "sensitive activation payload /private/token",
                )),
                3 => panic!("sensitive panic payload"),
                _ => Ok(()),
            }
        }
        fn deactivate(&mut self, _: &mut PluginContext) -> Result<(), PluginError> {
            self.events
                .lock()
                .unwrap()
                .push(format!("stop:{}", self.id));
            if self.mode.load(Ordering::SeqCst) == 2 {
                return Err(PluginError::new("sensitive stop payload"));
            }
            Ok(())
        }
    }
    fn probe_runtime(
        enabled: bool,
        provider_mode: Arc<AtomicUsize>,
        consumer_mode: Arc<AtomicUsize>,
        events: Arc<Mutex<Vec<String>>>,
    ) -> ToolRuntime {
        let mut host = PluginHost::new(ScopeKey::Host).unwrap();
        host.catalog_mut().register(manifest(FILES, true)).unwrap();
        host.catalog_mut()
            .register(manifest(CONSUMER, false))
            .unwrap();
        for (id, mode) in [(FILES, provider_mode), (CONSUMER, consumer_mode)] {
            let events = events.clone();
            host.with_factory(id, move || {
                Box::new(ProbePlugin {
                    id,
                    mode: mode.clone(),
                    events: events.clone(),
                })
            });
        }
        ToolRuntime::from_host(host, enabled).unwrap()
    }
    #[test]
    fn f07_f10_disabled_constructor_never_activates_and_lifecycle_order_is_exact() {
        let events = Arc::new(Mutex::new(vec![]));
        let mode = Arc::new(AtomicUsize::new(0));
        let runtime = probe_runtime(false, mode.clone(), mode, events.clone());
        let initial = status(&runtime);
        assert_eq!(initial.state, FilesRuntimeState::Unloaded);
        assert_eq!(initial.instances.len(), 2);
        assert!(
            initial
                .instances
                .iter()
                .all(|i| i.state == FilesInstanceState::Unloaded)
        );
        assert_eq!(initial.effects_count, 0);
        assert!(!initial.admission_open);
        assert!(events.lock().unwrap().is_empty());
        assert_eq!(reconcile(&runtime, false), initial);
        assert!(
            runtime
                .acquire(&task("unused"), "read_file", || -> Result<()> {
                    panic!("initial disabled")
                })
                .is_err()
        );
        for _ in 0..2 {
            reconcile(&runtime, true);
            reconcile(&runtime, false);
        }
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "start:builtin.files",
                "start:builtin.engine-tools",
                "stop:builtin.engine-tools",
                "stop:builtin.files",
                "start:builtin.files",
                "start:builtin.engine-tools",
                "stop:builtin.engine-tools",
                "stop:builtin.files"
            ]
        );
    }
    #[test]
    fn f07_activation_failure_is_queryable_clean_and_explicit_rebuild_preserves_recovery() {
        for (provider_failure, mode_value) in [(true, 1), (false, 1), (true, 3)] {
            let events = Arc::new(Mutex::new(vec![]));
            let provider = Arc::new(AtomicUsize::new(if provider_failure {
                mode_value
            } else {
                0
            }));
            let consumer = Arc::new(AtomicUsize::new(if provider_failure {
                0
            } else {
                mode_value
            }));
            let runtime = probe_runtime(true, provider.clone(), consumer.clone(), events.clone());
            let failed = status(&runtime);
            assert_eq!(failed.state, FilesRuntimeState::Failed);
            assert!(!failed.admission_open);
            assert_eq!((failed.effects_count, failed.subscriptions_count), (0, 0));
            assert_eq!(failed.recovery.len(), 1);
            assert_eq!(failed.recovery[0].stage, FilesFailureStage::Activate);
            assert_eq!(
                failed.recovery[0].plugin_id,
                if provider_failure { FILES } else { CONSUMER }
            );
            assert_eq!(
                failed.recovery[0].cause_code,
                if mode_value == 3 {
                    FilesFailureCauseCode::Panic
                } else {
                    FilesFailureCauseCode::PluginError
                }
            );
            assert!(
                !serde_json::to_string(&failed)
                    .unwrap()
                    .contains("sensitive")
            );
            let old_events = events.lock().unwrap().clone();
            assert_eq!(status(&runtime), failed);
            assert_eq!(*events.lock().unwrap(), old_events);
            assert!(
                runtime
                    .acquire(&task("unused"), "read_file", || -> Result<()> {
                        panic!("failed must not admit")
                    })
                    .is_err()
            );
            provider.store(0, Ordering::SeqCst);
            consumer.store(0, Ordering::SeqCst);
            let active = reconcile(&runtime, true);
            assert_eq!(active.state, FilesRuntimeState::Active);
            assert!(active.admission_open);
            assert_eq!(active.effects_count, 2);
            assert_eq!(active.recovery, failed.recovery);
        }
    }
    #[test]
    fn f07_stop_failure_closes_admission_cleans_both_members_and_keeps_history() {
        let events = Arc::new(Mutex::new(vec![]));
        let provider = Arc::new(AtomicUsize::new(0));
        let consumer = Arc::new(AtomicUsize::new(0));
        let runtime = probe_runtime(true, provider.clone(), consumer.clone(), events.clone());
        provider.store(2, Ordering::SeqCst);
        consumer.store(2, Ordering::SeqCst);
        let failed = reconcile(&runtime, false);
        assert_eq!(failed.state, FilesRuntimeState::Failed);
        assert!(!failed.admission_open);
        assert_eq!(failed.effects_count, 0);
        assert!(failed.providers.is_empty());
        assert_eq!(failed.recovery.len(), 2);
        assert!(
            failed
                .instances
                .iter()
                .all(|i| i.state == FilesInstanceState::Failed
                    && i.failure_stage == Some(FilesFailureStage::Deactivate))
        );
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "start:builtin.files",
                "start:builtin.engine-tools",
                "stop:builtin.engine-tools",
                "stop:builtin.files"
            ]
        );
        assert!(
            !serde_json::to_string(&failed)
                .unwrap()
                .contains("sensitive")
        );
        provider.store(0, Ordering::SeqCst);
        consumer.store(0, Ordering::SeqCst);
        let active = reconcile(&runtime, true);
        assert_eq!(active.state, FilesRuntimeState::Active);
        assert_eq!(active.recovery, failed.recovery);
        assert_eq!(active.effects_count, 2);
    }
    #[test]
    fn f07_new_disabled_intent_finishes_old_failed_cleanup_without_retrying_hooks() {
        let events = Arc::new(Mutex::new(vec![]));
        let provider = Arc::new(AtomicUsize::new(0));
        let consumer = Arc::new(AtomicUsize::new(0));
        let runtime = probe_runtime(true, provider.clone(), consumer.clone(), events.clone());
        provider.store(2, Ordering::SeqCst);
        consumer.store(2, Ordering::SeqCst);
        let failed = reconcile(&runtime, false);
        assert_eq!(failed.state, FilesRuntimeState::Failed);
        assert_eq!(failed.recovery.len(), 2);
        let hooks = events.lock().unwrap().clone();
        assert_eq!(status(&runtime), failed); // Observing/replaying never cleans.
        let cleaned = reconcile(&runtime, false); // A separate explicit intent.
        assert_eq!(cleaned.state, FilesRuntimeState::Unloaded);
        assert!(!cleaned.admission_open);
        assert_eq!((cleaned.effects_count, cleaned.subscriptions_count), (0, 0));
        assert!(
            cleaned
                .instances
                .iter()
                .all(|i| i.state == FilesInstanceState::Unloaded)
        );
        assert_eq!(cleaned.recovery, failed.recovery);
        assert_eq!(*events.lock().unwrap(), hooks); // No repeated deactivate.
        assert_eq!(reconcile(&runtime, false), cleaned);
        // Failed initial activation can likewise be explicitly disabled.
        let mode = Arc::new(AtomicUsize::new(1));
        let runtime = probe_runtime(true, mode.clone(), mode, Arc::new(Mutex::new(vec![])));
        let failed = status(&runtime);
        assert_eq!(failed.state, FilesRuntimeState::Failed);
        let disabled = reconcile(&runtime, false);
        assert_eq!(disabled.state, FilesRuntimeState::Unloaded);
        assert_eq!(disabled.recovery, failed.recovery);
    }
    #[test]
    fn f07_failed_persistence_callback_does_not_apply_and_poison_is_fail_closed() {
        let runtime = Arc::new(ToolRuntime::builtin().unwrap());
        let before = status(&runtime);
        let failed: Result<()> =
            runtime.with_files_control(|_| bail!("simulated Store transaction failure"));
        assert!(failed.is_err());
        assert_eq!(status(&runtime), before);
        let other = runtime.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = other.lock().unwrap();
                panic!("poison test");
            })
            .join()
            .is_err()
        );
        assert!(runtime.with_files_control(|c| Ok(c.snapshot())).is_err());
        assert!(runtime.definitions(&task("unused")).is_err());
        assert!(
            runtime
                .acquire(&task("unused"), "read_file", || -> Result<()> {
                    panic!("poisoned admit")
                })
                .is_err()
        );
    }
    struct PendingService(Arc<tokio::sync::Notify>);
    impl ToolServiceV1 for PendingService {
        fn definition(&self, _: &Task) -> Option<Value> {
            Some(json!({"type":"function","function":{"name":"read_file"}}))
        }
        fn execute<'a>(&'a self, _: &'a Task, _: &'a Value) -> ToolFuture<'a> {
            Box::pin(async move {
                self.0.notify_one();
                std::future::pending().await
            })
        }
    }
    #[tokio::test]
    async fn f04_cancelling_in_progress_future_releases_count_without_reopening_admission() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let runtime = ToolRuntime::assemble([
            Arc::new(PendingService(entered.clone())),
            Arc::new(WorkspaceService("write_file")),
        ])
        .unwrap();
        let task = task("unused");
        let (lease, ()) = runtime.acquire(&task, "read_file", || Ok(())).unwrap();
        let running = tokio::spawn(async move { lease.execute(&task, &json!({})).await });
        tokio::time::timeout(WAIT, entered.notified())
            .await
            .unwrap();
        assert_eq!(status(&runtime).in_flight, 1);
        assert!(reconcile(&runtime, false).draining);
        running.abort();
        assert!(
            tokio::time::timeout(WAIT, running)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
        let stopped = status(&runtime);
        assert_eq!(stopped.in_flight, 0);
        assert!(!stopped.draining);
        assert!(!stopped.admission_open);
        assert_eq!(stopped.effects_count, 0);
    }
}
