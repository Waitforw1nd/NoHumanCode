pub mod embedded;
pub use embedded::EmbeddedEngine;
mod model_loop;

#[cfg(all(test, windows, feature = "standalone"))]
extern crate self as coda;

#[cfg(feature = "standalone")]
pub mod agent_runtime;
#[cfg(feature = "standalone")]
pub mod approval;
#[cfg(feature = "standalone")]
pub mod budget;
#[cfg(feature = "standalone")]
pub mod checkpoint;
#[cfg(feature = "standalone")]
mod command_execution;
#[cfg(feature = "standalone")]
mod composition;
#[cfg(feature = "standalone")]
pub mod credential_input;
#[cfg(feature = "standalone")]
pub mod domain;
#[cfg(feature = "standalone")]
pub mod engine;
#[cfg(feature = "standalone")]
mod execution_authority;
#[cfg(feature = "standalone")]
pub mod git_diff;
#[cfg(feature = "standalone")]
pub(crate) mod git_worktree;
#[cfg(feature = "standalone")]
pub mod host_configuration;
#[cfg(feature = "standalone")]
mod host_services;
#[cfg(feature = "standalone")]
pub mod mcp;
#[cfg(feature = "standalone")]
pub mod network_authorization;
#[cfg(feature = "standalone")]
pub mod plugin_management;
#[cfg(feature = "standalone")]
mod process_lifetime;
#[cfg(feature = "standalone")]
pub mod project_config;
#[cfg(feature = "standalone")]
pub mod provider;
#[cfg(feature = "standalone")]
pub mod provider_runtime;
#[cfg(feature = "standalone")]
pub mod provider_transport;
#[cfg(feature = "standalone")]
pub mod provider_usage;
#[cfg(feature = "standalone")]
pub mod repository;
#[cfg(feature = "standalone")]
pub mod runtime_management;
#[cfg(feature = "standalone")]
pub mod secrets;
#[cfg(feature = "standalone")]
pub mod server;
#[cfg(feature = "standalone")]
pub mod store;
#[cfg(feature = "standalone")]
mod tool_call_application;
#[cfg(feature = "standalone")]
mod tool_results;
#[cfg(feature = "standalone")]
pub mod tool_runtime;
#[cfg(feature = "standalone")]
pub mod workspace;
#[cfg(feature = "standalone")]
pub mod workspace_changes;
#[cfg(feature = "standalone")]
pub mod workspace_identity;
#[cfg(feature = "standalone")]
pub mod worktree_management;
