#[cfg(test)]
extern crate self as coda;

pub(crate) use codacli_core::agent_loop;
pub use codacli_core::{
    agent_runtime, domain, kernel_embedded, media, messages, provider, provider_runtime,
    provider_transport, provider_usage, redaction,
};
pub(crate) use codacli_core::{runtime_seams, tool_results};

pub mod approval;
pub mod budget;
pub mod checkpoint;
mod command_execution;
mod composition;
pub mod computer;
pub mod credential_input;
pub mod engine;
mod execution_authority;
pub mod exploration_jobs;
pub mod git_diff;
pub(crate) mod git_worktree;
pub mod host_configuration;
mod host_services;
pub mod kernel_server;
mod key_pool;
mod key_pool_config;
pub mod mcp;
pub mod media_adapters;
pub mod network_authorization;
pub mod plugin_management;
mod process_lifetime;
pub mod project_config;
pub mod repository;
pub mod runtime_management;
pub mod secrets;
pub mod server;
pub mod store;
mod tool_call_application;
pub mod tool_runtime;
pub mod workspace;
pub mod workspace_changes;
pub mod workspace_identity;
pub mod worktree_management;
