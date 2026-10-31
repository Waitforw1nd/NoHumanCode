#[cfg(test)]
use crate::engine::restore_probe;
use crate::{host_services, host_services::Active};
use crate::{store::Store, tool_runtime::ToolRuntime};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;

pub(crate) struct HostComposition {
    pub(crate) store: Arc<Store>,
    pub(crate) gate: Arc<AsyncMutex<()>>,
    pub(crate) active: Arc<Mutex<HashMap<String, Active>>>,
    pub(crate) tools: Arc<ToolRuntime>,
    pub(crate) provider: Arc<crate::provider_runtime::ProviderRuntime>,
    pub(crate) agent: Arc<crate::agent_runtime::AgentRuntime>,
    pub(crate) projects: Arc<host_services::ProjectService>,
    pub(crate) approval: Arc<host_services::ApprovalService>,
    pub(crate) sessions: host_services::SessionService,
    pub(crate) workspace: Arc<host_services::WorkspaceSafety>,
    pub(crate) recovery: host_services::RecoveryService,
    pub(crate) review_recovery: host_services::ReviewRecovery,
    #[cfg(test)]
    pub(crate) restore_probe: Arc<Mutex<Option<restore_probe::Hook>>>,
}
impl HostComposition {
    pub(crate) fn builtin(
        store: Arc<Store>,
        recovery: host_services::RecoveryService,
    ) -> anyhow::Result<Self> {
        let (files, shell) = store.builtin_plugin_states()?;
        let tools = ToolRuntime::builtin_with_plugin_states(files.enabled, shell.enabled)?;
        let agent = store.plugin_desired_for("builtin.chat-agent")?;
        let provider = store.plugin_desired_for("builtin.provider-xpeach")?;
        Ok(Self::new(
            store,
            tools,
            crate::provider_runtime::ProviderRuntime::builtin_with_state(provider.enabled)?,
            crate::agent_runtime::AgentRuntime::builtin_with_state(agent.enabled)?,
            recovery,
        ))
    }
    pub(crate) fn new(
        store: Arc<Store>,
        tools: ToolRuntime,
        provider: crate::provider_runtime::ProviderRuntime,
        agent: crate::agent_runtime::AgentRuntime,
        recovery: host_services::RecoveryService,
    ) -> Self {
        let gate = Arc::new(AsyncMutex::new(()));
        let active = Arc::new(Mutex::new(HashMap::new()));
        #[cfg(test)]
        let restore_probe = Arc::new(Mutex::new(None));
        let workspace = Arc::new(host_services::WorkspaceSafety::new(gate.clone()));
        let projects = Arc::new(host_services::ProjectService::new(
            store.clone(),
            gate.clone(),
        ));
        let approval = Arc::new(host_services::ApprovalService::new(store.clone()));
        let sessions = host_services::SessionService::new(store.clone());
        let review_recovery = host_services::ReviewRecovery::new(
            store.clone(),
            workspace.clone(),
            projects.clone(),
            active.clone(),
            #[cfg(test)]
            restore_probe.clone(),
        );
        Self {
            store,
            gate,
            active,
            tools: Arc::new(tools),
            provider: Arc::new(provider),
            agent: Arc::new(agent),
            projects,
            approval,
            sessions,
            workspace,
            recovery,
            review_recovery,
            #[cfg(test)]
            restore_probe,
        }
    }
}

#[derive(serde::Serialize)]
pub struct CompositionInterface {
    pub name: String,
    pub version: String,
}
#[derive(serde::Serialize)]
pub struct CompositionCapability {
    pub id: &'static str,
    pub binding_kind: &'static str,
    pub provider_id: String,
    pub provider_version: String,
    pub interfaces: Vec<CompositionInterface>,
    pub lifecycle: &'static str,
    pub dependencies: Vec<&'static str>,
}
#[derive(serde::Serialize)]
pub struct CompositionSnapshot {
    pub version: u8,
    pub consistency: &'static str,
    pub capabilities: Vec<CompositionCapability>,
    pub plugins: Vec<crate::plugin_management::PluginStatus>,
    pub runtimes: Vec<crate::runtime_management::RuntimeComponentStatus>,
}
fn host(
    id: &'static str,
    provider: &str,
    version: &str,
    dependencies: &[&'static str],
) -> CompositionCapability {
    CompositionCapability {
        id,
        binding_kind: "host_service",
        provider_id: provider.into(),
        provider_version: version.into(),
        interfaces: vec![CompositionInterface {
            name: format!("host.{id}"),
            version: version.into(),
        }],
        lifecycle: "host_required",
        dependencies: dependencies.to_vec(),
    }
}
fn plugin(
    id: &'static str,
    manifest: crate::plugin_catalog::CatalogManifestV1,
    names: &[&str],
    dependencies: &[&'static str],
) -> CompositionCapability {
    let interfaces = manifest
        .provides
        .into_iter()
        .filter(|k| names.contains(&k.name.as_str()))
        .map(|k| CompositionInterface {
            name: k.name,
            version: k.version.to_string(),
        })
        .collect();
    CompositionCapability {
        id,
        binding_kind: "plugin_runtime",
        provider_id: manifest.id,
        provider_version: manifest.version.to_string(),
        interfaces,
        lifecycle: "managed",
        dependencies: dependencies.to_vec(),
    }
}
impl HostComposition {
    pub(crate) fn snapshot(
        &self,
        plugins: Vec<crate::plugin_management::PluginStatus>,
        runtimes: Vec<crate::runtime_management::RuntimeComponentStatus>,
    ) -> CompositionSnapshot {
        use host_services::{
            ApprovalService as A, ProjectService as P, RecoveryService as R, ReviewRecovery as RR,
            SessionService as S, WorkspaceSafety as W,
        };
        let capabilities = vec![
            host("core", "host.core", "1.0.0", &[]),
            host("session-log", S::ID, S::VERSION, &["core"]),
            host("local-project", P::ID, P::VERSION, &["core", "workspace"]),
            host("workspace", W::ID, W::VERSION, &["core"]),
            plugin(
                "provider-xpeach",
                crate::provider_runtime::composition_manifest(),
                &["provider.completion-codec"],
                &["core"],
            ),
            plugin(
                "file-read-search",
                crate::tool_runtime::composition_manifest("builtin.files"),
                &["tool.read_file", "tool.list_files", "tool.search_files"],
                &["workspace", "local-project", "approval-policy"],
            ),
            host(
                "approval-policy",
                A::ID,
                A::VERSION,
                &["core", "local-project"],
            ),
            plugin(
                "file-write",
                crate::tool_runtime::composition_manifest("builtin.files"),
                &["tool.write_file"],
                &["workspace", "local-project", "approval-policy"],
            ),
            plugin(
                "shell",
                crate::tool_runtime::composition_manifest("builtin.shell"),
                &["tool.run_command"],
                &["workspace", "local-project", "approval-policy"],
            ),
            host(
                "git-diff",
                RR::ID,
                RR::VERSION,
                &["workspace", "local-project"],
            ),
            host(
                "checkpoint-restore",
                RR::ID,
                RR::VERSION,
                &["workspace", "local-project", "approval-policy"],
            ),
            host("recovery", R::ID, R::VERSION, &["core"]),
            plugin(
                "chat-agent",
                crate::agent_runtime::composition_manifest(),
                &["agent.strategy"],
                &["core", "approval-policy"],
            ),
        ];
        CompositionSnapshot {
            version: 1,
            consistency: "per_component",
            capabilities,
            plugins,
            runtimes,
        }
    }
}
