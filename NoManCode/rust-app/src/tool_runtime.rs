//! Production builtin file-tool assembly. The host owns admission; services
//! receive no Store, approval authority, credentials, or persistent lease.
use crate::plugin_catalog::{
    CatalogManifestV1, ExactVersion, InterfaceKey, InterfaceKind, LifecycleKind, RuntimeKind,
    ScopeKey, ScopeKind, Surface,
};
use crate::plugin_host::{BuiltinPlugin, InstanceState, PluginContext, PluginError, PluginHost};
use crate::{domain::Task, workspace};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
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

/// One registry shared by all calls through an Engine. No public raw registry
/// or service handles: revocation and admission serialize on this mutex.
pub struct ToolRuntime {
    host: Mutex<PluginHost>,
}

/// Single-use admitted service. Not Clone, not persistent, and not a public
/// tool execution API. Only Engine's existing approval/perform_tool path calls it.
pub struct ToolLease {
    service: Service,
    task_id: String,
    workspace: String,
    spec: Value,
}
impl ToolRuntime {
    pub fn builtin() -> Result<Self> {
        Self::assemble([
            Arc::new(WorkspaceService("read_file")),
            Arc::new(WorkspaceService("write_file")),
        ])
    }
    fn assemble(services: [Service; 2]) -> Result<Self> {
        let mut host = PluginHost::new(ScopeKey::Host)?;
        host.catalog_mut().register(manifest(FILES, true))?;
        host.catalog_mut().register(manifest(CONSUMER, false))?;
        host.with_factory(FILES, move || {
            Box::new(FilesPlugin {
                services: services.clone(),
            })
        });
        host.with_factory(CONSUMER, || Box::new(ConsumerPlugin));
        host.start(&[CONSUMER.to_owned()])?;
        Ok(Self {
            host: Mutex::new(host),
        })
    }
    fn lock(&self) -> Result<MutexGuard<'_, PluginHost>> {
        self.host
            .lock()
            .map_err(|_| anyhow::anyhow!("tool runtime lock poisoned"))
    }
    fn files_active(host: &PluginHost) -> bool {
        host.instances()
            .iter()
            .any(|instance| instance.id == CONSUMER && instance.state == InstanceState::Active)
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
        if Self::files_active(&host) {
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
        let service: Service = if FILE_TOOLS.contains(&name) {
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
        let lease = ToolLease {
            service,
            task_id: task.id.clone(),
            workspace: task.workspace.clone(),
            spec: serde_json::to_value(&task.spec)?,
        };
        let admitted = admit()?;
        drop(host);
        Ok((lease, admitted))
    }
    pub fn disable_builtin_files(&self) -> Result<()> {
        let mut host = self.lock()?;
        if Self::files_active(&host) {
            host.stop_subtree(FILES)?;
        }
        Ok(())
    }
    pub fn unload_builtin_files(&self) -> Result<()> {
        let mut host = self.lock()?;
        if Self::files_active(&host) {
            host.stop_subtree(FILES)?;
        }
        for id in [CONSUMER, FILES] {
            if host
                .instances()
                .iter()
                .any(|instance| instance.id == id && instance.state != InstanceState::Unloaded)
            {
                host.unload(id)?;
            }
        }
        Ok(())
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
        Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

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
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let acquired = {
            let runtime = runtime.clone();
            let task = task.clone();
            let entered = entered.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                runtime
                    .acquire(&task, "read_file", || {
                        entered.wait();
                        release.wait();
                        Ok("claimed")
                    })
                    .unwrap()
            })
        };
        entered.wait();
        let stopper = {
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                attempt_tx.send(()).unwrap();
                runtime.disable_builtin_files().unwrap();
                done_tx.send(()).unwrap();
            })
        };
        attempt_rx.recv().unwrap();
        // The admission callback still owns the runtime lock.
        assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release.wait();
        let (lease, claim) = acquired.join().unwrap();
        stopper.join().unwrap();
        done_rx.recv().unwrap();
        assert_eq!(claim, "claimed");
        assert!(runtime.lock().unwrap().effects().is_empty());
        assert_eq!(
            lease.execute(&task, &json!({})).await.unwrap(),
            json!({"marker":true})
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
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
        let stopped = Arc::new(Barrier::new(2));
        let worker = {
            let runtime = runtime.clone();
            let stopped = stopped.clone();
            std::thread::spawn(move || {
                stopped.wait();
                assert!(
                    runtime
                        .acquire(&task("unused"), "write_file", || -> Result<()> {
                            panic!("stopped service must never admit")
                        })
                        .is_err()
                );
            })
        };
        runtime.disable_builtin_files().unwrap();
        stopped.wait();
        worker.join().unwrap();
        runtime.unload_builtin_files().unwrap();
        runtime.unload_builtin_files().unwrap();
        let host = runtime.lock().unwrap();
        assert!(host.effects().is_empty());
        assert!(
            host.instances()
                .iter()
                .all(|i| i.state == InstanceState::Unloaded)
        );
    }
    #[test]
    fn n09_claim_failure_returns_no_lease_and_releases_lock_for_stop() {
        let (runtime, hits) = marker_runtime();
        let runtime = Arc::new(runtime);
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker = {
            let runtime = runtime.clone();
            let entered = entered.clone();
            let release = release.clone();
            std::thread::spawn(move || {
                runtime
                    .acquire(&task("unused"), "read_file", || -> Result<()> {
                        entered.wait();
                        release.wait();
                        bail!("claim conflict")
                    })
                    .err()
                    .unwrap()
                    .to_string()
            })
        };
        entered.wait();
        let stopper = {
            let runtime = runtime.clone();
            std::thread::spawn(move || runtime.disable_builtin_files().unwrap())
        };
        release.wait();
        assert_eq!(worker.join().unwrap(), "claim conflict");
        stopper.join().unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert!(runtime.lock().unwrap().effects().is_empty());
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
}
