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
const SHELL: &str = "builtin.shell";
const SHELL_CONSUMER: &str = "builtin.engine-shell-tools";
pub(crate) const FILES_PLUGIN_VERSION: &str = "1.2.0";
pub(crate) const SHELL_PLUGIN_VERSION: &str = "1.0.0";
pub(crate) const SHELL_TOOLS: [&str; 1] = ["run_command"];
pub(crate) const FILE_TOOLS: [&str; 4] = ["read_file", "write_file", "list_files", "search_files"];
pub(crate) const COMPAT_TOOLS: [&str; 1] = ["run_wasm"];
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
    let shell = id == SHELL || id == SHELL_CONSUMER;
    let names: &[&str] = if shell { &SHELL_TOOLS } else { &FILE_TOOLS };
    let keys = names.iter().map(|name| key(name)).collect();
    let version = ExactVersion {
        major: 1,
        minor: if shell { 0 } else { 2 },
        patch: 0,
    };
    debug_assert_eq!(
        version.to_string(),
        if shell {
            SHELL_PLUGIN_VERSION
        } else {
            FILES_PLUGIN_VERSION
        }
    );
    CatalogManifestV1 {
        manifest_version: 1,
        id: id.into(),
        display_name: id.into(),
        version,
        scope: ScopeKind::Host,
        runtime: RuntimeKind::Builtin,
        lifecycle: LifecycleKind::HostManaged,
        surfaces: vec![Surface::Cli, Surface::Headless],
        permissions: if provider && shell {
            vec!["process.exec".into()]
        } else if provider {
            vec!["filesystem.read".into(), "filesystem.write".into()]
        } else {
            vec![]
        },
        config_schema: serde_json::json!({}),
        provides: if provider { keys } else { vec![] },
        requires: if provider {
            vec![]
        } else {
            names.iter().map(|name| key(name)).collect()
        },
    }
}
struct FilesPlugin {
    services: [Service; 4],
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

struct ShellPlugin(Service);
impl BuiltinPlugin for ShellPlugin {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        ctx.register_effect(&key("run_command"), Box::new(self.0.clone()))
            .map_err(|error| PluginError::new(error.to_string()))?;
        Ok(())
    }
    fn deactivate(&mut self, _: &mut PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}
struct ShellConsumerPlugin;
impl BuiltinPlugin for ShellConsumerPlugin {
    fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
        let binding = ctx
            .bound(&key("run_command"))
            .map_err(|error| PluginError::new(error.to_string()))?;
        if binding.payload().downcast_ref::<Service>().is_none() {
            return Err(PluginError::new("shell tool payload type mismatch"));
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
    provider: &'static str,
    consumer: &'static str,
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
        let instances: Vec<_> = [self.state.provider, self.state.consumer]
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
        let active = ToolRuntime::group_active(self.state);
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
                for id in [self.state.consumer, self.state.provider] {
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
        if ToolRuntime::group_active(self.state) && !self.state.apply_failed {
            self.state.admission_open = true;
            return self.snapshot();
        }
        // Failed instances must be unloaded, never directly restarted.
        if !self.stop_active() {
            return self.snapshot();
        }
        for id in [self.state.consumer, self.state.provider] {
            if self.state.host.instances().iter().any(|i| i.id == id)
                && self.state.host.unload(id).is_err()
            {
                self.state.apply_failed = true;
                return self.snapshot();
            }
        }
        self.state.apply_failed = false;
        match self.state.host.start(&[self.state.consumer.to_owned()]) {
            Ok(_) => self.state.admission_open = ToolRuntime::group_active(self.state),
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
        for id in [self.state.consumer, self.state.provider] {
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
    shell_host: Mutex<RuntimeState>,
    shell_in_flight: Arc<AtomicU64>,
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
    shell: bool,
    _file_count: Option<FileLeaseCount>,
    service: Service,
    task_id: String,
    workspace: String,
    spec: Value,
}
impl ToolRuntime {
    pub(crate) fn composition_manifest(&self, id: &str) -> Result<CatalogManifestV1> {
        let state = match id {
            FILES => self
                .host
                .lock()
                .map_err(|_| anyhow::anyhow!("files catalog unavailable"))?,
            SHELL => self
                .shell_host
                .lock()
                .map_err(|_| anyhow::anyhow!("shell catalog unavailable"))?,
            _ => anyhow::bail!("unknown tool catalog"),
        };
        state
            .host
            .catalog()
            .descriptors()
            .into_iter()
            .find(|m| m.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("tool binding missing"))
    }

    pub fn builtin() -> Result<Self> {
        Self::builtin_with_enabled(true)
    }
    pub fn builtin_with_enabled(enabled: bool) -> Result<Self> {
        Self::builtin_with_plugin_states(enabled, true)
    }
    pub fn builtin_with_plugin_states(files_enabled: bool, shell_enabled: bool) -> Result<Self> {
        Self::assemble_states(
            [
                Arc::new(WorkspaceService("read_file")),
                Arc::new(WorkspaceService("write_file")),
                Arc::new(WorkspaceService("list_files")),
                Arc::new(WorkspaceService("search_files")),
            ],
            files_enabled,
            shell_enabled,
        )
    }
    #[cfg(test)]
    fn assemble(services: [Service; 4]) -> Result<Self> {
        Self::assemble_enabled(services, true)
    }
    #[cfg(test)]
    fn assemble_enabled(services: [Service; 4], enabled: bool) -> Result<Self> {
        Self::assemble_states(services, enabled, true)
    }
    fn assemble_states(services: [Service; 4], enabled: bool, shell_enabled: bool) -> Result<Self> {
        let mut host = PluginHost::new(ScopeKey::Host)?;
        host.catalog_mut().register(manifest(FILES, true))?;
        host.catalog_mut().register(manifest(CONSUMER, false))?;
        host.with_factory(FILES, move || {
            Box::new(FilesPlugin {
                services: services.clone(),
            })
        });
        host.with_factory(CONSUMER, || Box::new(ConsumerPlugin));
        Self::from_hosts(host, enabled, Self::shell_host()?, shell_enabled)
    }
    fn shell_host() -> Result<PluginHost> {
        let mut host = PluginHost::new(ScopeKey::Host)?;
        host.catalog_mut().register(manifest(SHELL, true))?;
        host.catalog_mut()
            .register(manifest(SHELL_CONSUMER, false))?;
        host.with_factory(SHELL, || {
            Box::new(ShellPlugin(Arc::new(WorkspaceService("run_command"))))
        });
        host.with_factory(SHELL_CONSUMER, || Box::new(ShellConsumerPlugin));
        Ok(host)
    }
    #[cfg(test)]
    fn from_host(host: PluginHost, enabled: bool) -> Result<Self> {
        Self::from_hosts(host, enabled, Self::shell_host()?, true)
    }
    fn from_hosts(
        host: PluginHost,
        enabled: bool,
        shell_host: PluginHost,
        shell_enabled: bool,
    ) -> Result<Self> {
        let runtime = Self {
            host: Mutex::new(RuntimeState {
                provider: FILES,
                consumer: CONSUMER,
                host,
                admission_open: false,
                apply_failed: false,
                activation_recovery: vec![],
            }),
            in_flight: Arc::new(AtomicU64::new(0)),
            shell_host: Mutex::new(RuntimeState {
                provider: SHELL,
                consumer: SHELL_CONSUMER,
                host: shell_host,
                admission_open: false,
                apply_failed: false,
                activation_recovery: vec![],
            }),
            shell_in_flight: Arc::new(AtomicU64::new(0)),
        };
        if enabled {
            runtime.with_files_control(|control| {
                control.reconcile(true);
                Ok(())
            })?;
        }
        if shell_enabled {
            runtime.with_shell_control(|control| {
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
    fn group_active(state: &RuntimeState) -> bool {
        [state.provider, state.consumer].iter().all(|id| {
            state
                .host
                .instances()
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
    pub(crate) fn with_shell_control<R>(
        &self,
        f: impl FnOnce(&mut FilesControl<'_>) -> Result<R>,
    ) -> Result<R> {
        let mut state = self
            .shell_host
            .lock()
            .map_err(|_| anyhow::anyhow!("shell runtime lock poisoned"))?;
        f(&mut FilesControl {
            state: &mut state,
            in_flight: &self.shell_in_flight,
        })
    }
    /// Each assembly is observed separately: never hold both runtime locks.
    pub fn definitions(&self, task: &Task) -> Result<Vec<Value>> {
        let allowed = workspace::definitions(task);
        let mut result = allowed
            .iter()
            .filter(|definition| {
                definition["function"]["name"]
                    .as_str()
                    .is_some_and(|name| COMPAT_TOOLS.contains(&name))
            })
            .cloned()
            .collect::<Vec<_>>();
        for (mutex, names) in [
            (&self.host, FILE_TOOLS.as_slice()),
            (&self.shell_host, SHELL_TOOLS.as_slice()),
        ] {
            let state = mutex
                .lock()
                .map_err(|_| anyhow::anyhow!("tool runtime lock poisoned"))?;
            if state.admission_open && !state.apply_failed && Self::group_active(&state) {
                for name in names {
                    if !allowed
                        .iter()
                        .any(|definition| definition["function"]["name"] == *name)
                    {
                        continue;
                    }
                    let service = state
                        .host
                        .resolve_bound::<Service>(state.consumer, &key(name))?
                        .payload;
                    if let Some(definition) = service.definition(task) {
                        validate_definition(&definition, name)?;
                        result.push(definition);
                    }
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
        let is_shell = SHELL_TOOLS.contains(&name);
        let (mutex, count) = if is_shell {
            (&self.shell_host, &self.shell_in_flight)
        } else {
            (&self.host, &self.in_flight)
        };
        let host = mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("tool runtime lock poisoned"))?;
        // Host permission ceiling is independent of plugin-provided metadata.
        ensure!(
            workspace::definitions(task)
                .iter()
                .any(|definition| definition["function"]["name"] == name),
            "工具未获任务授权"
        );
        let is_file = FILE_TOOLS.contains(&name);
        let service: Service = if is_file || is_shell {
            ensure!(
                host.admission_open && !host.apply_failed && Self::group_active(&host),
                "file tools unavailable"
            );
            ensure!(
                count.load(Ordering::SeqCst) < u64::MAX,
                "file lease count exhausted"
            );
            host.resolve_bound::<Service>(host.consumer, &key(name))?
                .payload
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
            shell: is_shell,
            _file_count: None,
            service,
            task_id: task.id.clone(),
            workspace: task.workspace.clone(),
            spec: serde_json::to_value(&task.spec)?,
        };
        let admitted = admit()?;
        if is_file || is_shell {
            count.fetch_add(1, Ordering::SeqCst);
            lease._file_count = Some(FileLeaseCount(count.clone()));
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
    /// In-memory shell control; durable management belongs to Engine/Store.
    pub fn disable_builtin_shell(&self) -> Result<()> {
        self.with_shell_control(|control| {
            ensure!(
                control.reconcile(false).state != FilesRuntimeState::Failed,
                "shell runtime stop failed"
            );
            Ok(())
        })
    }
    pub fn unload_builtin_shell(&self) -> Result<()> {
        self.with_shell_control(|control| {
            control.reconcile(false);
            for id in [SHELL_CONSUMER, SHELL] {
                if control.state.host.instances().iter().any(|i| i.id == id) {
                    control.state.host.unload(id)?;
                }
            }
            control.state.apply_failed = false;
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
        if self.shell {
            let task = task.clone();
            let args = args.clone();
            let (send, receive) = tokio::sync::oneshot::channel();
            // The worker, not the caller waiting on its channel, owns the lease.
            tokio::spawn(async move {
                let result = self.service.execute(&task, &args).await;
                drop(self);
                let _ = send.send(result);
            });
            receive
                .await
                .map_err(|_| crate::command_execution::CommandOutcomeUnknown)?
        } else {
            self.service.execute(task, args).await
        }
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
            Arc::new(WorkspaceService("list_files")),
            Arc::new(WorkspaceService("search_files")),
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
        assert_eq!((first.effects_count, first.subscriptions_count), (4, 0));
        assert_eq!(
            first
                .providers
                .iter()
                .map(|p| (&p.key.name, p.provider_id.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (&"tool.list_files".to_owned(), FILES),
                (&"tool.read_file".to_owned(), FILES),
                (&"tool.search_files".to_owned(), FILES),
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
        let mut command_task = task.clone();
        command_task.spec.allow_commands = true;
        let (compat, ()) = runtime
            .acquire(&command_task, "run_command", || Ok(()))
            .unwrap();
        assert_eq!(status(&runtime).in_flight, 0);
        // Component callback only; real approval/network acknowledgement is Engine's responsibility.
        let output = compat
            .execute(
                &command_task,
                &json!({"command":"Write-Output 'compat-marker'","timeout_seconds":5}),
            )
            .await
            .unwrap();
        assert_eq!(output["exit_code"], 0);
        assert!(output["stdout"].as_str().unwrap().contains("compat-marker"));
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
                    if self.mode.load(Ordering::SeqCst) == 4 && name == "search_files" {
                        continue;
                    }
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
            assert_eq!(active.effects_count, 4);
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
        assert_eq!(active.effects_count, 4);
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
            Arc::new(WorkspaceService("list_files")),
            Arc::new(WorkspaceService("search_files")),
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
    struct DiscoveryMarker {
        name: &'static str,
        hits: Arc<AtomicUsize>,
    }
    impl ToolServiceV1 for DiscoveryMarker {
        fn definition(&self, _: &Task) -> Option<Value> {
            Some(
                json!({"type":"function","function":{"name":self.name,"description":"discovery registry marker"}}),
            )
        }
        fn execute<'a>(&'a self, _: &'a Task, _: &'a Value) -> ToolFuture<'a> {
            Box::pin(async move {
                self.hits.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"source":self.name}))
            })
        }
    }
    fn discovery_runtime() -> (ToolRuntime, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let list = Arc::new(AtomicUsize::new(0));
        let search = Arc::new(AtomicUsize::new(0));
        let runtime = ToolRuntime::assemble([
            Arc::new(WorkspaceService("read_file")),
            Arc::new(WorkspaceService("write_file")),
            Arc::new(DiscoveryMarker {
                name: "list_files",
                hits: list.clone(),
            }),
            Arc::new(DiscoveryMarker {
                name: "search_files",
                hits: search.clone(),
            }),
        ])
        .unwrap();
        (runtime, list, search)
    }
    #[tokio::test]
    async fn l01_l02_four_registry_services_and_revocation_have_no_discovery_fallback() {
        // Four distinct registry payloads on an unusable workspace rule out
        // accidental direct dispatch even for the pre-existing read/write keys.
        let counts: [Arc<AtomicUsize>; 4] = std::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));
        let services: [Service; 4] = std::array::from_fn(|i| {
            Arc::new(DiscoveryMarker {
                name: FILE_TOOLS[i],
                hits: counts[i].clone(),
            }) as Service
        });
        let markers = ToolRuntime::assemble(services).unwrap();
        let marker_task = task("does-not-exist");
        for (i, name) in FILE_TOOLS.into_iter().enumerate() {
            let definitions = markers.definitions(&marker_task).unwrap();
            assert_eq!(
                definitions
                    .iter()
                    .find(|d| d["function"]["name"] == name)
                    .unwrap()["function"]["description"],
                "discovery registry marker"
            );
            let lease = markers.acquire(&marker_task, name, || Ok(())).unwrap().0;
            assert_eq!(
                lease.execute(&marker_task, &json!({})).await.unwrap(),
                json!({"source":name})
            );
            assert_eq!(counts[i].load(Ordering::SeqCst), 1);
        }
        let (runtime, list, search) = discovery_runtime();
        let mut task = task("does-not-exist");
        task.spec.write_scopes.clear(); // Discovery must not require a write grant.
        {
            let state = runtime.lock().unwrap();
            for id in [FILES, CONSUMER] {
                let manifests = state.catalog().descriptors();
                let declaration = manifests.iter().find(|m| m.id == id).unwrap();
                assert_eq!(declaration.version.to_string(), FILES_PLUGIN_VERSION);
                assert_eq!(declaration.manifest_version, 1);
                let interfaces = if id == FILES {
                    &declaration.provides
                } else {
                    &declaration.requires
                };
                assert_eq!(interfaces.len(), 4);
                assert!(interfaces.iter().all(|k| k.version.to_string() == "1.0.0"));
            }
        }
        let definitions = runtime.definitions(&task).unwrap();
        let names: std::collections::BTreeSet<_> = definitions
            .iter()
            .map(|d| d["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), definitions.len());
        assert_eq!(
            names,
            std::collections::BTreeSet::from([
                "list_files",
                "read_file",
                "search_files",
                "run_wasm"
            ])
        );
        for name in ["list_files", "search_files"] {
            assert_eq!(
                definitions
                    .iter()
                    .find(|d| d["function"]["name"] == name)
                    .unwrap()["function"]["description"],
                "discovery registry marker"
            );
            let (lease, ()) = runtime.acquire(&task, name, || Ok(())).unwrap();
            assert_eq!(
                lease.execute(&task, &json!({})).await.unwrap(),
                json!({"source":name})
            );
        }
        assert_eq!(
            (list.load(Ordering::SeqCst), search.load(Ordering::SeqCst)),
            (1, 1)
        );
        for unload in [false, true] {
            if unload {
                runtime.unload_builtin_files().unwrap();
            } else {
                runtime.disable_builtin_files().unwrap();
            }
            assert_eq!(
                runtime
                    .definitions(&task)
                    .unwrap()
                    .iter()
                    .map(|d| d["function"]["name"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["run_wasm"]
            );
            for name in FILE_TOOLS {
                assert!(
                    runtime
                        .acquire(&task, name, || -> Result<()> {
                            panic!("revoked admission")
                        })
                        .is_err()
                );
            }
            assert_eq!(status(&runtime).effects_count, 0);
            assert_eq!(
                (list.load(Ordering::SeqCst), search.load(Ordering::SeqCst)),
                (1, 1)
            );
        }
    }
    #[tokio::test]
    async fn l01_real_discovery_results_permissions_and_path_policy_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join("src/a.txt"), "first\ndiscovery-needle\n").unwrap();
        std::fs::write(root.path().join("src/b.txt"), "other").unwrap();
        std::fs::write(root.path().join(".git/secret.txt"), "discovery-needle").unwrap();
        let mut task = task(root.path().to_str().unwrap());
        task.spec.write_scopes.clear();
        let runtime = ToolRuntime::builtin().unwrap();
        for (name, args) in [
            ("list_files", json!({"path":"src"})),
            (
                "search_files",
                json!({"query":"discovery-needle","path":"src"}),
            ),
        ] {
            let expected = workspace::execute(&task, name, &args).await.unwrap();
            let (lease, ()) = runtime.acquire(&task, name, || Ok(())).unwrap();
            let actual = lease.execute(&task, &args).await.unwrap();
            assert_eq!(actual, expected);
            if name == "list_files" {
                let mut names = actual["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f["name"].as_str().unwrap())
                    .collect::<Vec<_>>();
                names.sort_unstable();
                assert_eq!(names, ["a.txt", "b.txt"]);
            } else {
                assert_eq!(
                    actual["matches"],
                    json!([{"path":"src/a.txt","line":2,"text":"discovery-needle"}])
                );
            }
            for bad_path in ["../outside", ".git"] {
                let (lease, ()) = runtime.acquire(&task, name, || Ok(())).unwrap();
                assert!(
                    lease
                        .execute(&task, &json!({"path":bad_path,"query":"discovery-needle"}))
                        .await
                        .is_err()
                );
                assert_eq!(status(&runtime).in_flight, 0);
            }
        }
        task.spec.tools = false;
        assert!(runtime.definitions(&task).unwrap().is_empty());
        for name in ["list_files", "search_files"] {
            assert!(
                runtime
                    .acquire(&task, name, || -> Result<()> { panic!("tools denied") })
                    .is_err()
            );
        }
    }
    #[tokio::test]
    async fn l03_each_discovery_admission_wins_bounded_stop_then_real_result_finishes() {
        for name in ["list_files", "search_files"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("proof.txt"), "lease-needle").unwrap();
            let task = task(root.path().to_str().unwrap());
            let runtime = Arc::new(ToolRuntime::builtin().unwrap());
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let (lease_tx, lease_rx) = mpsc::channel();
            let worker = {
                let runtime = runtime.clone();
                let task = task.clone();
                std::thread::spawn(move || {
                    let lease = runtime
                        .acquire(&task, name, || {
                            entered_tx.send(()).unwrap();
                            release_rx.recv_timeout(WAIT).unwrap();
                            Ok(())
                        })
                        .unwrap()
                        .0;
                    lease_tx.send(lease).unwrap();
                })
            };
            entered_rx.recv_timeout(WAIT).unwrap();
            let (attempt_tx, attempt_rx) = mpsc::channel();
            let (stopped_tx, stopped_rx) = mpsc::channel();
            let stopper = {
                let runtime = runtime.clone();
                std::thread::spawn(move || {
                    attempt_tx.send(()).unwrap();
                    let stopped = reconcile(&runtime, false);
                    stopped_tx.send(stopped).unwrap();
                })
            };
            attempt_rx.recv_timeout(WAIT).unwrap();
            assert!(matches!(
                stopped_rx.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
            release_tx.send(()).unwrap();
            let lease = lease_rx.recv_timeout(WAIT).unwrap();
            let stopped = stopped_rx.recv_timeout(WAIT).unwrap();
            for worker in [worker, stopper] {
                let deadline = std::time::Instant::now() + WAIT;
                while !worker.is_finished() && std::time::Instant::now() < deadline {
                    std::thread::yield_now();
                }
                assert!(worker.is_finished(), "discovery worker termination timeout");
                worker.join().unwrap();
            }
            assert!(stopped.draining && !stopped.admission_open);
            assert_eq!((stopped.in_flight, stopped.effects_count), (1, 0));
            assert!(
                runtime
                    .acquire(&task, name, || -> Result<()> {
                        panic!("post-disable admission")
                    })
                    .is_err()
            );
            let value = lease
                .execute(&task, &json!({"path":".","query":"lease-needle"}))
                .await
                .unwrap();
            if name == "list_files" {
                assert_eq!(value["files"][0]["name"], "proof.txt");
            } else {
                assert_eq!(value["matches"][0]["text"], "lease-needle");
            }
            assert_eq!(status(&runtime).in_flight, 0);
            assert!(!status(&runtime).draining);
        }
    }
    #[tokio::test]
    async fn l03_mixed_file_leases_drop_error_and_binding_rejection_release_exactly_once() {
        let root = tempfile::tempdir().unwrap();
        let task = task(root.path().to_str().unwrap());
        let runtime = ToolRuntime::builtin().unwrap();
        let mut leases = FILE_TOOLS
            .into_iter()
            .map(|name| runtime.acquire(&task, name, || Ok(())).unwrap().0)
            .collect::<Vec<_>>();
        assert_eq!(status(&runtime).in_flight, 4);
        let search = leases.pop().unwrap();
        let list = leases.pop().unwrap();
        let stopped = reconcile(&runtime, false);
        assert!(stopped.draining);
        assert_eq!(stopped.in_flight, 4);
        runtime
            .with_files_control(|control| {
                drop(leases);
                assert_eq!(control.snapshot().in_flight, 2);
                Ok(())
            })
            .unwrap();
        assert!(search.execute(&task, &json!({"query":""})).await.is_err());
        assert_eq!(status(&runtime).in_flight, 1);
        let mut other = task.clone();
        other.id = "different".into();
        assert!(list.execute(&other, &json!({"path":"."})).await.is_err());
        assert_eq!(status(&runtime).in_flight, 0);
        assert!(!status(&runtime).draining);
        reconcile(&runtime, true);
        for name in ["list_files", "search_files"] {
            assert!(
                runtime
                    .acquire(&task, name, || -> Result<()> {
                        bail!("rejected final admission")
                    })
                    .is_err()
            );
            let lease = runtime.acquire(&task, name, || Ok(())).unwrap().0;
            runtime
                .with_files_control(|control| {
                    drop(lease);
                    assert_eq!(control.snapshot().in_flight, 0);
                    Ok(())
                })
                .unwrap();
            let lease = runtime.acquire(&task, name, || Ok(())).unwrap().0;
            assert!(
                lease
                    .execute(&task, &json!({"path":"../invalid","query":"x"}))
                    .await
                    .is_err()
            );
            assert_eq!(status(&runtime).in_flight, 0);
        }
    }
    struct PendingDiscovery {
        name: &'static str,
        entered: Arc<tokio::sync::Notify>,
    }
    impl ToolServiceV1 for PendingDiscovery {
        fn definition(&self, _: &Task) -> Option<Value> {
            Some(json!({"type":"function","function":{"name":self.name}}))
        }
        fn execute<'a>(&'a self, _: &'a Task, _: &'a Value) -> ToolFuture<'a> {
            Box::pin(async move {
                self.entered.notify_one();
                std::future::pending().await
            })
        }
    }
    #[tokio::test]
    async fn l03_each_pending_discovery_abort_releases_lease() {
        for name in ["list_files", "search_files"] {
            let entered = Arc::new(tokio::sync::Notify::new());
            let mut services: [Service; 4] =
                FILE_TOOLS.map(|name| Arc::new(WorkspaceService(name)) as Service);
            let index = FILE_TOOLS
                .iter()
                .position(|candidate| *candidate == name)
                .unwrap();
            services[index] = Arc::new(PendingDiscovery {
                name,
                entered: entered.clone(),
            });
            let runtime = ToolRuntime::assemble(services).unwrap();
            let task = task("unused");
            let lease = runtime.acquire(&task, name, || Ok(())).unwrap().0;
            let running = tokio::spawn(async move { lease.execute(&task, &json!({})).await });
            tokio::time::timeout(WAIT, entered.notified())
                .await
                .unwrap();
            assert!(reconcile(&runtime, false).draining);
            running.abort();
            assert!(
                tokio::time::timeout(WAIT, running)
                    .await
                    .unwrap()
                    .unwrap_err()
                    .is_cancelled()
            );
            assert_eq!(status(&runtime).in_flight, 0);
            assert!(!status(&runtime).admission_open);
        }
    }
    #[test]
    fn l04_missing_fourth_registration_fails_closed_and_explicit_rebuild_recovers() {
        let mode = Arc::new(AtomicUsize::new(4));
        let runtime = probe_runtime(
            true,
            mode.clone(),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(vec![])),
        );
        let failed = status(&runtime);
        assert_eq!(failed.state, FilesRuntimeState::Failed);
        assert_eq!(failed.effects_count, 0);
        assert_eq!(
            failed.recovery[0].cause_code,
            FilesFailureCauseCode::ProvidesNotCovered
        );
        assert!(!failed.admission_open);
        for name in FILE_TOOLS {
            assert!(
                runtime
                    .acquire(&task("unused"), name, || -> Result<()> {
                        panic!("incomplete assembly")
                    })
                    .is_err()
            );
        }
        let disabled = reconcile(&runtime, false);
        assert_eq!(disabled.state, FilesRuntimeState::Unloaded);
        assert_eq!(disabled.recovery, failed.recovery);
        mode.store(0, Ordering::SeqCst);
        let active = reconcile(&runtime, true);
        assert_eq!(active.effects_count, 4);
        assert!(active.admission_open);
        assert_eq!(active.recovery, failed.recovery);
    }
    fn shell_status(runtime: &ToolRuntime) -> FilesRuntimeStatus {
        runtime.with_shell_control(|c| Ok(c.snapshot())).unwrap()
    }
    fn shell_reconcile(runtime: &ToolRuntime, enabled: bool) -> FilesRuntimeStatus {
        runtime
            .with_shell_control(|c| Ok(c.reconcile(enabled)))
            .unwrap()
    }
    fn command_task(root: &std::path::Path) -> Task {
        let mut value = task(root.to_str().unwrap());
        value.spec.allow_commands = true;
        value
    }
    async fn wait_until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(12), async {
            while !condition() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("bounded component watchdog expired");
    }
    async fn real_shell(runtime: &ToolRuntime, task: &Task) {
        let (lease, ()) = runtime.acquire(task, "run_command", || Ok(())).unwrap();
        let value = tokio::time::timeout(WAIT, lease.execute(task, &json!({
            "command":"[IO.File]::WriteAllText((Join-Path $PWD 'shell-marker.txt'), 'shell'); [Console]::Out.Write('shell-registry'); [Console]::Error.Write('stderr-marker'); exit 7",
            "timeout_seconds":3
        }))).await.unwrap().unwrap();
        assert_eq!(value["exit_code"], 7);
        assert_eq!(value["stdout"], "shell-registry");
        assert_eq!(value["stderr"], "stderr-marker");
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&task.workspace).join("shell-marker.txt"))
                .unwrap(),
            "shell"
        );
    }
    async fn real_files(runtime: &ToolRuntime, task: &Task) {
        for (name, args) in [
            (
                "write_file",
                json!({"path":"files-marker.txt", "content":"independent-files"}),
            ),
            ("read_file", json!({"path":"files-marker.txt"})),
            ("list_files", json!({"path":"."})),
            (
                "search_files",
                json!({"query":"independent-files", "path":"."}),
            ),
        ] {
            let (lease, ()) = runtime.acquire(task, name, || Ok(())).unwrap();
            let output = lease.execute(task, &args).await.unwrap();
            if name == "read_file" {
                assert_eq!(output["content"], "independent-files");
            }
            if name == "list_files" {
                assert!(output.to_string().contains("files-marker.txt"));
            }
            if name == "search_files" {
                assert!(output.to_string().contains("independent-files"));
            }
        }
    }
    #[tokio::test]
    async fn s01_s02_real_registry_and_independent_disable_unload() {
        let root = tempfile::tempdir().unwrap();
        let task = command_task(root.path());
        let runtime = ToolRuntime::builtin().unwrap();
        assert_eq!(
            (
                status(&runtime).effects_count,
                shell_status(&runtime).effects_count
            ),
            (4, 1)
        );
        let shell = shell_status(&runtime);
        assert_eq!(
            shell
                .instances
                .iter()
                .map(|i| i.plugin_id.as_str())
                .collect::<Vec<_>>(),
            [SHELL, SHELL_CONSUMER]
        );
        assert_eq!(shell.providers[0].provider_id, SHELL);
        assert_eq!(shell.providers[0].key, key("run_command"));
        assert_eq!(shell.subscriptions_count, 0);
        for (id, version) in [
            (FILES, "1.2.0"),
            (CONSUMER, "1.2.0"),
            (SHELL, "1.0.0"),
            (SHELL_CONSUMER, "1.0.0"),
        ] {
            assert_eq!(
                manifest(id, id == FILES || id == SHELL).version.to_string(),
                version
            );
        }
        assert_eq!(manifest(SHELL, true).permissions, ["process.exec"]);
        assert_eq!(COMPAT_TOOLS, ["run_wasm"]);
        real_shell(&runtime, &task).await;
        runtime.disable_builtin_files().unwrap();
        real_shell(&runtime, &task).await;
        runtime.unload_builtin_files().unwrap();
        real_shell(&runtime, &task).await;
        assert_eq!(shell_status(&runtime), shell);
        reconcile(&runtime, true);
        runtime.disable_builtin_shell().unwrap();
        real_files(&runtime, &task).await;
        runtime.unload_builtin_shell().unwrap();
        real_files(&runtime, &task).await;
        assert_eq!(shell_status(&runtime).effects_count, 0);
        assert_eq!(status(&runtime).effects_count, 4);
        assert!(
            runtime
                .acquire(&task, "run_command", || -> Result<()> {
                    panic!("disabled shell claimed")
                })
                .is_err()
        );
        assert!(
            !runtime
                .definitions(&task)
                .unwrap()
                .iter()
                .any(|d| d["function"]["name"] == "run_command")
        );
    }
    #[test]
    fn s01_all_desired_combinations_and_shell_unpolled_binding_counts() {
        let mut task = task("unused");
        task.spec.allow_commands = true;
        for (files, shell) in [(false, false), (false, true), (true, false), (true, true)] {
            let runtime = ToolRuntime::builtin_with_plugin_states(files, shell).unwrap();
            assert_eq!(status(&runtime).effects_count, if files { 4 } else { 0 });
            assert_eq!(shell_status(&runtime).effects_count, u64::from(shell));
            for state in [(!files, status(&runtime)), (!shell, shell_status(&runtime))] {
                if state.0 {
                    assert!(
                        state
                            .1
                            .instances
                            .iter()
                            .all(|i| i.state == FilesInstanceState::Unloaded)
                    );
                }
            }
            assert_eq!(
                runtime.definitions(&task).unwrap().len(),
                1 + usize::from(files) * 4 + usize::from(shell)
            );
            if shell {
                let (lease, ()) = runtime.acquire(&task, "run_command", || Ok(())).unwrap();
                assert_eq!(shell_status(&runtime).in_flight, 1);
                assert_eq!(status(&runtime).in_flight, 0);
                runtime
                    .with_shell_control(|control| {
                        drop(lease);
                        assert_eq!(control.snapshot().in_flight, 0);
                        Ok(())
                    })
                    .unwrap();
            }
        }
    }
    struct ShellProbe {
        mode: Arc<AtomicUsize>,
    }
    impl BuiltinPlugin for ShellProbe {
        fn activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError> {
            if self.mode.load(Ordering::SeqCst) != 4 {
                let service: Service = Arc::new(WorkspaceService("run_command"));
                ctx.register_effect(&key("run_command"), Box::new(service))
                    .unwrap();
            }
            Ok(())
        }
        fn deactivate(&mut self, _: &mut PluginContext) -> Result<(), PluginError> {
            if self.mode.load(Ordering::SeqCst) == 2 {
                Err(PluginError::new("private-shell-failure"))
            } else {
                Ok(())
            }
        }
    }
    fn install_shell_probe(runtime: &ToolRuntime, mode: Arc<AtomicUsize>) {
        runtime.unload_builtin_shell().unwrap();
        let mut state = runtime.shell_host.lock().unwrap();
        state
            .host
            .with_factory(SHELL, move || Box::new(ShellProbe { mode: mode.clone() }));
    }
    #[tokio::test]
    async fn s02_missing_registration_and_lifecycle_failure_do_not_cross_groups() {
        let root = tempfile::tempdir().unwrap();
        let task = command_task(root.path());
        let files_mode = Arc::new(AtomicUsize::new(4));
        let runtime = probe_runtime(
            true,
            files_mode.clone(),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(vec![])),
        );
        assert_eq!(status(&runtime).state, FilesRuntimeState::Failed);
        real_shell(&runtime, &task).await;
        assert!(
            runtime
                .definitions(&task)
                .unwrap()
                .iter()
                .any(|d| d["function"]["name"] == "run_command")
        );
        files_mode.store(0, Ordering::SeqCst);
        reconcile(&runtime, true);
        let shell_mode = Arc::new(AtomicUsize::new(4));
        install_shell_probe(&runtime, shell_mode.clone());
        let failed = shell_reconcile(&runtime, true);
        assert_eq!(failed.state, FilesRuntimeState::Failed);
        assert_eq!(failed.effects_count, 0);
        assert!(
            failed
                .recovery
                .iter()
                .all(|r| r.plugin_id == SHELL || r.plugin_id == SHELL_CONSUMER)
        );
        real_files(&runtime, &task).await;
        assert!(
            runtime
                .acquire(&task, "run_command", || -> Result<()> {
                    panic!("missing registry claim")
                })
                .is_err()
        );
        assert_eq!(runtime.definitions(&task).unwrap().len(), 5);
        shell_mode.store(0, Ordering::SeqCst);
        shell_reconcile(&runtime, true);
        let history = shell_status(&runtime).recovery;
        shell_mode.store(2, Ordering::SeqCst);
        assert_eq!(
            shell_reconcile(&runtime, false).state,
            FilesRuntimeState::Failed
        );
        real_files(&runtime, &task).await;
        let cleared = shell_reconcile(&runtime, false);
        assert_eq!(cleared.state, FilesRuntimeState::Unloaded);
        assert!(cleared.recovery.starts_with(&history));
        shell_mode.store(0, Ordering::SeqCst);
        shell_reconcile(&runtime, true);
        files_mode.store(2, Ordering::SeqCst);
        assert_eq!(reconcile(&runtime, false).state, FilesRuntimeState::Failed);
        real_shell(&runtime, &task).await;
        assert_eq!(
            reconcile(&runtime, false).state,
            FilesRuntimeState::Unloaded
        );
    }
    #[tokio::test]
    async fn s05_caller_abort_keeps_real_worker_lease_through_disable_and_completion() {
        let root = tempfile::tempdir().unwrap();
        let task = command_task(root.path());
        let runtime = ToolRuntime::builtin().unwrap();
        let (lease, ()) = runtime.acquire(&task, "run_command", || Ok(())).unwrap();
        let ready = root.path().join("ready");
        let release = root.path().join("release");
        let finished = root.path().join("finished");
        let command = "[IO.File]::WriteAllText((Join-Path $PWD 'ready'), [string]$PID); $deadline=[DateTime]::UtcNow.AddSeconds(8); while (!(Test-Path (Join-Path $PWD 'release'))) { if ([DateTime]::UtcNow -gt $deadline) { exit 98 }; Start-Sleep -Milliseconds 10 }; [IO.File]::WriteAllText((Join-Path $PWD 'finished'), 'completed'); 'worker-output'";
        let caller = tokio::spawn(async move {
            lease
                .execute(&task, &json!({"command":command, "timeout_seconds":10}))
                .await
        });
        wait_until(|| ready.exists()).await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert_eq!(shell_status(&runtime).in_flight, 1);
        assert_eq!(status(&runtime).in_flight, 0);
        runtime.disable_builtin_shell().unwrap();
        let draining = shell_status(&runtime);
        assert!(draining.draining);
        assert_eq!(draining.effects_count, 0);
        std::fs::write(release, "go").unwrap();
        wait_until(|| shell_status(&runtime).in_flight == 0).await;
        assert_eq!(std::fs::read_to_string(finished).unwrap(), "completed");
        assert!(!shell_status(&runtime).draining);
    }
    #[tokio::test]
    async fn s05_shell_binding_rejection_errors_and_admitted_after_disable() {
        let root = tempfile::tempdir().unwrap();
        let task = command_task(root.path());
        let runtime = ToolRuntime::builtin().unwrap();
        let (lease, ()) = runtime.acquire(&task, "run_command", || Ok(())).unwrap();
        let mut wrong = task.clone();
        wrong.spec.allow_commands = false;
        assert!(
            lease
                .execute(&wrong, &json!({"command":"'no'"}))
                .await
                .is_err()
        );
        assert_eq!(shell_status(&runtime).in_flight, 0);
        let (lease, ()) = runtime.acquire(&task, "run_command", || Ok(())).unwrap();
        assert!(
            lease
                .execute(&task, &json!({"command":"'no'", "timeout_seconds":0}))
                .await
                .is_err()
        );
        assert_eq!(shell_status(&runtime).in_flight, 0);
        let (lease, ()) = runtime.acquire(&task, "run_command", || Ok(())).unwrap();
        runtime.disable_builtin_shell().unwrap();
        assert_eq!(
            lease
                .execute(&task, &json!({"command":"'already-admitted'"}))
                .await
                .unwrap()["stdout"]
                .as_str()
                .unwrap()
                .trim(),
            "already-admitted"
        );
        assert_eq!(shell_status(&runtime).in_flight, 0);
    }
    struct DisappearingShellWorker;
    impl ToolServiceV1 for DisappearingShellWorker {
        fn definition(&self, task: &Task) -> Option<Value> {
            WorkspaceService("run_command").definition(task)
        }
        fn execute<'a>(&'a self, _: &'a Task, _: &'a Value) -> ToolFuture<'a> {
            Box::pin(async { panic!("controlled shell worker disappearance") })
        }
    }
    #[tokio::test]
    async fn s05_worker_channel_loss_is_unknown_and_releases_owned_count() {
        let runtime = ToolRuntime::builtin().unwrap();
        runtime.unload_builtin_shell().unwrap();
        {
            let mut state = runtime.shell_host.lock().unwrap();
            state.host.with_factory(SHELL, || {
                Box::new(ShellPlugin(Arc::new(DisappearingShellWorker)))
            });
        }
        shell_reconcile(&runtime, true);
        let mut task = task("unused");
        task.spec.allow_commands = true;
        let (lease, ()) = runtime.acquire(&task, "run_command", || Ok(())).unwrap();
        let error = lease.execute(&task, &json!({})).await.unwrap_err();
        assert!(error.is::<crate::command_execution::CommandOutcomeUnknown>());
        assert_eq!(shell_status(&runtime).in_flight, 0);
        runtime.shell_in_flight.store(u64::MAX, Ordering::SeqCst);
        assert!(
            runtime
                .acquire(&task, "run_command", || -> Result<()> {
                    panic!("overflow must precede claim")
                })
                .is_err()
        );
        runtime.shell_in_flight.store(0, Ordering::SeqCst);
        assert_eq!(status(&runtime).in_flight, 0);
    }
    #[test]
    fn s08_durable_acceptance_survives_real_shell_apply_failure_and_replay() {
        use crate::{engine::Engine, plugin_management::SetPluginEnabled, store::Store};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.sqlite");
        let store = Arc::new(Store::open(&path).unwrap());
        let runtime = ToolRuntime::builtin().unwrap();
        let mode = Arc::new(AtomicUsize::new(0));
        install_shell_probe(&runtime, mode.clone());
        shell_reconcile(&runtime, true);
        let engine = Engine::new_with_runtime_for_test(store.clone(), runtime).unwrap();
        let files = engine.plugin_status(FILES).unwrap();
        let sql = rusqlite::Connection::open(&path).unwrap();
        let files_row = || {
            sql.query_row("SELECT enabled,revision,updated_at FROM plugin_desired WHERE plugin_id='builtin.files'", [], |r| Ok((r.get::<_,bool>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).unwrap()
        };
        let original_files = files_row();
        let set = |enabled, revision, key| {
            engine
                .set_plugin_enabled(
                    SHELL,
                    SetPluginEnabled {
                        enabled,
                        expected_revision: revision,
                    },
                    key,
                )
                .unwrap()
        };
        mode.store(2, Ordering::SeqCst);
        let accepted = set(false, 0, "s08-stop-fails");
        assert!(!accepted.replayed);
        assert_eq!(accepted.receipt.revision, 1);
        assert!(!accepted.current.converged);
        assert_eq!(accepted.current.runtime.state, FilesRuntimeState::Failed);
        assert_eq!(accepted.current.runtime.effects_count, 0);
        assert!(!accepted.current.desired.enabled);
        let row: (bool,i64,i64) = sql.query_row("SELECT enabled,revision,updated_at FROM plugin_desired WHERE plugin_id='builtin.shell'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(row, (false, 1, accepted.receipt.committed_at));
        let receipt: (String,bool,i64,i64,i64) = sql.query_row("SELECT plugin_id,enabled,expected_revision,result_revision,committed_at FROM plugin_management_requests WHERE key='s08-stop-fails'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
        assert_eq!(
            receipt,
            (SHELL.into(), false, 0, 1, accepted.receipt.committed_at)
        );
        mode.store(0, Ordering::SeqCst);
        let replay = set(false, 0, "s08-stop-fails");
        assert!(replay.replayed);
        assert_eq!(replay.receipt, accepted.receipt);
        assert_eq!(replay.current, accepted.current); // No reconciliation on replay.
        let clean = set(false, 1, "s08-explicit-clean");
        assert_eq!(clean.receipt.revision, 2);
        assert!(clean.current.converged);
        assert_eq!(clean.current.runtime.state, FilesRuntimeState::Unloaded);
        mode.store(4, Ordering::SeqCst); // Genuine missing service registration.
        let failed_enable = set(true, 2, "s08-enable-fails");
        assert_eq!(failed_enable.receipt.revision, 3);
        assert!(failed_enable.current.desired.enabled);
        assert!(!failed_enable.current.converged);
        assert_eq!(
            failed_enable.current.runtime.state,
            FilesRuntimeState::Failed
        );
        mode.store(0, Ordering::SeqCst);
        assert_eq!(
            set(true, 2, "s08-enable-fails").current,
            failed_enable.current
        );
        let recovered = set(true, 3, "s08-explicit-recover");
        assert_eq!(recovered.receipt.revision, 4);
        assert!(recovered.current.converged);
        assert_eq!(recovered.current.runtime.effects_count, 1);
        assert!(!recovered.current.runtime.recovery.is_empty());
        assert_eq!(engine.plugin_status(FILES).unwrap(), files);
        assert_eq!(files_row(), original_files);
        let receipt_counts: (i64,i64) = sql.query_row("SELECT count(*),sum(CASE WHEN plugin_id='builtin.files' THEN 1 ELSE 0 END) FROM plugin_management_requests", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(receipt_counts, (4, 0));
        assert_eq!(store.plugin_desired_for(SHELL).unwrap().revision, 4);
    }
}
