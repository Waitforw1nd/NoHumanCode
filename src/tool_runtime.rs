//! Lightweight builtin file and shell tool runtime without PluginHost ceremony.
use crate::{domain::Task, workspace};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub(crate) const FILES_PLUGIN_VERSION: &str = "1.2.0";
pub(crate) const SHELL_PLUGIN_VERSION: &str = "1.0.0";
pub(crate) const SHELL_TOOLS: [&str; 1] = ["run_command"];
pub(crate) const FILE_TOOLS: [&str; 4] = ["read_file", "write_file", "list_files", "search_files"];
pub(crate) const COMPAT_TOOLS: [&str; 1] = ["run_wasm"];

fn is_file_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "write_file" | "edit_file" | "list_files" | "search_files" | "glob_files"
    )
}

fn is_shell_tool(name: &str) -> bool {
    name == "run_command"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesRuntimeState {
    Active,
    Stopped,
    Unloaded,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesRuntimeStatus {
    pub state: FilesRuntimeState,
    pub admission_open: bool,
    pub in_flight: u64,
    pub draining: bool,
    pub effects_count: u64,
    pub subscriptions_count: u64,
}

pub(crate) struct FilesControl<'a> {
    enabled: &'a AtomicBool,
    unloaded: &'a AtomicBool,
    in_flight: &'a Arc<AtomicU64>,
    effect_count: u64,
}

impl FilesControl<'_> {
    pub(crate) fn snapshot(&self) -> FilesRuntimeStatus {
        let admission_open = self.enabled.load(Ordering::SeqCst);
        let unloaded = self.unloaded.load(Ordering::SeqCst);
        let in_flight = self.in_flight.load(Ordering::SeqCst);
        let state = if admission_open {
            FilesRuntimeState::Active
        } else if unloaded {
            FilesRuntimeState::Unloaded
        } else {
            FilesRuntimeState::Stopped
        };
        FilesRuntimeStatus {
            state,
            admission_open,
            in_flight,
            draining: !admission_open && in_flight > 0,
            effects_count: if admission_open { self.effect_count } else { 0 },
            subscriptions_count: 0,
        }
    }

    pub(crate) fn reconcile(&mut self, enabled: bool) -> FilesRuntimeStatus {
        self.enabled.store(enabled, Ordering::SeqCst);
        if enabled {
            self.unloaded.store(false, Ordering::SeqCst);
        }
        self.snapshot()
    }
}

pub struct ToolRuntime {
    files_enabled: AtomicBool,
    shell_enabled: AtomicBool,
    files_unloaded: AtomicBool,
    shell_unloaded: AtomicBool,
    in_flight: Arc<AtomicU64>,
    shell_in_flight: Arc<AtomicU64>,
}

struct LeaseCount(Arc<AtomicU64>);
impl Drop for LeaseCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct ToolLease {
    name: String,
    _count: LeaseCount,
    task_id: String,
    workspace: String,
    spec: Value,
}

impl ToolRuntime {
    pub fn builtin() -> Result<Self> {
        Self::builtin_with_plugin_states(true, true)
    }

    pub fn builtin_with_enabled(enabled: bool) -> Result<Self> {
        Self::builtin_with_plugin_states(enabled, true)
    }

    pub fn builtin_with_plugin_states(files_enabled: bool, shell_enabled: bool) -> Result<Self> {
        Ok(Self {
            files_enabled: AtomicBool::new(files_enabled),
            shell_enabled: AtomicBool::new(shell_enabled),
            files_unloaded: AtomicBool::new(false),
            shell_unloaded: AtomicBool::new(false),
            in_flight: Arc::new(AtomicU64::new(0)),
            shell_in_flight: Arc::new(AtomicU64::new(0)),
        })
    }

    pub(crate) fn with_files_control<R>(
        &self,
        f: impl FnOnce(&mut FilesControl<'_>) -> Result<R>,
    ) -> Result<R> {
        f(&mut FilesControl {
            enabled: &self.files_enabled,
            unloaded: &self.files_unloaded,
            in_flight: &self.in_flight,
            effect_count: FILE_TOOLS.len() as u64,
        })
    }

    pub(crate) fn with_shell_control<R>(
        &self,
        f: impl FnOnce(&mut FilesControl<'_>) -> Result<R>,
    ) -> Result<R> {
        f(&mut FilesControl {
            enabled: &self.shell_enabled,
            unloaded: &self.shell_unloaded,
            in_flight: &self.shell_in_flight,
            effect_count: SHELL_TOOLS.len() as u64,
        })
    }

    pub fn definitions(&self, task: &Task) -> Result<Vec<Value>> {
        let files_on = self.files_enabled.load(Ordering::SeqCst);
        let shell_on = self.shell_enabled.load(Ordering::SeqCst);
        let tools = workspace::definitions(task)
            .into_iter()
            .filter(|def| match def["function"]["name"].as_str() {
                Some(name) if is_file_tool(name) => files_on,
                Some(name) if is_shell_tool(name) => shell_on,
                _ => false,
            })
            .collect();
        Ok(tools)
    }

    pub fn acquire<A>(
        &self,
        task: &Task,
        name: &str,
        admit: impl FnOnce() -> Result<A>,
    ) -> Result<(ToolLease, A)> {
        let is_file = is_file_tool(name);
        let is_shell = is_shell_tool(name);
        if !is_file && !is_shell {
            bail!("未知工具");
        }
        ensure!(
            workspace::definitions(task)
                .iter()
                .any(|def| def["function"]["name"] == name),
            "工具未获任务授权"
        );
        let (enabled, count) = if is_shell {
            (&self.shell_enabled, &self.shell_in_flight)
        } else {
            (&self.files_enabled, &self.in_flight)
        };
        ensure!(enabled.load(Ordering::SeqCst), "tool runtime unavailable");
        let spec = serde_json::to_value(&task.spec)?;
        let admitted = admit()?;
        count.fetch_add(1, Ordering::SeqCst);
        Ok((
            ToolLease {
                name: name.to_owned(),
                _count: LeaseCount(count.clone()),
                task_id: task.id.clone(),
                workspace: task.workspace.clone(),
                spec,
            },
            admitted,
        ))
    }

    pub fn disable_builtin_files(&self) -> Result<()> {
        self.files_enabled.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn unload_builtin_files(&self) -> Result<()> {
        self.files_enabled.store(false, Ordering::SeqCst);
        self.files_unloaded.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn disable_builtin_shell(&self) -> Result<()> {
        self.shell_enabled.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn unload_builtin_shell(&self) -> Result<()> {
        self.shell_enabled.store(false, Ordering::SeqCst);
        self.shell_unloaded.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl ToolLease {
    pub(crate) async fn execute(self, task: &Task, args: &Value) -> Result<Value> {
        ensure!(
            self.task_id == task.id
                && self.workspace == task.workspace
                && self.spec == serde_json::to_value(&task.spec)?,
            "tool lease task binding changed"
        );
        workspace::execute_builtin(task, &self.name, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_task(root: &str) -> Task {
        serde_json::from_value(json!({
            "id": "task",
            "run_id": "run",
            "spec": {
                "name": "worker",
                "role": "test",
                "route_id": "route",
                "prompt": "test",
                "depends_on": [],
                "write_scopes": ["*"],
                "tools": true,
                "allow_commands": true,
                "max_rounds": 4
            },
            "route": {
                "id": "route",
                "name": "route",
                "base_url": "http://127.0.0.1",
                "model": "mock",
                "max_tokens": 16,
                "parallel_limit": 1,
                "key_env": null
            },
            "workspace": root,
            "status": "running",
            "output": "",
            "error": null,
            "messages": [],
            "usage": null,
            "created_at": 0,
            "updated_at": 0
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn definitions_and_direct_execution_respect_toggles() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "peach").unwrap();
        let task = sample_task(&dir.path().to_string_lossy());
        let runtime = ToolRuntime::builtin().unwrap();

        let defs = runtime.definitions(&task).unwrap();
        assert!(defs.iter().all(|d| d["function"]["name"] != "run_wasm"));
        assert!(defs.iter().any(|d| d["function"]["name"] == "read_file"));
        assert!(defs.iter().any(|d| d["function"]["name"] == "run_command"));

        let (lease, tag) = runtime.acquire(&task, "read_file", || Ok(42)).unwrap();
        assert_eq!(tag, 42);
        let out = lease
            .execute(&task, &json!({"path": "hello.txt"}))
            .await
            .unwrap();
        assert!(out["content"].as_str().unwrap().contains("peach"));

        runtime.disable_builtin_files().unwrap();
        assert!(runtime.acquire(&task, "read_file", || Ok(())).is_err());
        assert!(runtime.acquire(&task, "run_command", || Ok(())).is_ok());
    }
}
