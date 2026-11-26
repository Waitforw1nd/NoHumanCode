//! CLI argument definitions, validation, and local workbench dispatch.

mod computer;
mod http_admin;
mod init_route;
mod jobs;
mod mentions;
mod repl;

use clap::{Args, Subcommand, ValueEnum};
use coda::budget::BudgetConfig;
use coda::domain::ProjectId;
use coda::engine::Engine;
use coda::network_authorization::{NetworkSubject, NetworkSubjectKind};
use coda::secrets;
use coda::store::{Store, fresh_settings};
pub use http_admin::execute;
use init_route::run_init;
use repl::{run_ask, run_chat};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Subcommand)]
pub enum Command {
    /// One-step interactive or flag-driven provider & API key setup.
    Init(InitArgs),
    /// Interactive in-process AI coding workbench (zero separate server needed).
    Chat(ChatArgs),
    /// One-shot in-process coding execution in the target directory.
    Ask(AskArgs),
    /// Submit and inspect durable read-only exploration jobs in a chat session.
    Jobs(jobs::JobsArgs),
    /// Inspect the registered Windows application without contacting a model.
    Computer {
        #[command(subcommand)]
        command: computer::ComputerCommand,
    },
    /// Manage registered remote MCP servers.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Explicitly start the local HTTP harness server.
    Serve,
    /// Configure this local Host; saving does not authorize or contact a Provider.
    Host(HostArgs),
    /// Read process-local runtime diagnostics.
    Runtime(RuntimeArgs),
    Plugin(PluginArgs),
    Project(ProjectArgs),
    Run(RunArgs),
    Session(SessionArgs),
    Task(TaskArgs),
    Approval(ApprovalArgs),
    Checkpoint(CheckpointArgs),
    Network(NetworkArgs),
    Route(RouteArgs),
    Account(AccountArgs),
}

#[derive(Subcommand)]
pub enum McpCommand {
    /// Authorize a registered OAuth server in the browser.
    Login {
        server: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Registered loopback redirect port. Zero chooses an available port.
        #[arg(long, default_value_t = 0)]
        callback_port: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum InitProvider {
    Openai,
    Anthropic,
    Gemini,
}

#[derive(Args)]
pub struct InitArgs {
    /// Provider preset for default base URL, model, and API key environment variable.
    #[arg(long, value_enum)]
    pub provider: Option<InitProvider>,
    /// Route ID to create or update.
    #[arg(long, default_value = "default")]
    pub route: String,
    /// Display name for the route.
    #[arg(long, default_value = "Default")]
    pub name: String,
    /// Provider API base URL (defaults to provider's *_BASE_URL env var or preset URL).
    #[arg(long)]
    pub base_url: Option<String>,
    /// Default model name (defaults to CODA_MODEL or provider's preset model).
    #[arg(long)]
    pub model: Option<String>,
    /// Maximum output tokens per completion call (16..=65536).
    #[arg(long, default_value_t = 16384)]
    pub max_tokens: u32,
    /// Per-route parallel request limit (1..=8).
    #[arg(long, default_value_t = 4)]
    pub parallel_limit: usize,
    /// Read the API key as a single line from stdin.
    #[arg(long)]
    pub api_key_stdin: bool,
    /// Override user data directory (defaults to %LOCALAPPDATA%\coda).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct ChatArgs {
    /// Optional initial prompt to run immediately upon entering the session.
    pub prompt: Vec<String>,
    /// Target workspace directory (defaults to current working directory).
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Route ID to use (defaults to first configured route or 'default').
    #[arg(long)]
    pub route: Option<String>,
    /// Override the active route's model for this session.
    #[arg(long)]
    pub model: Option<String>,
    /// Continue the most recent chat session in this workspace.
    #[arg(short = 'c', long = "continue", conflicts_with = "resume")]
    pub continue_session: bool,
    /// Resume a specific session ID or run ID.
    #[arg(
        long = "resume",
        value_name = "ID",
        conflicts_with = "continue_session"
    )]
    pub resume: Option<String>,
    /// Automatically approve file edits and shell commands in this session.
    #[arg(short = 'y', long = "auto-approve")]
    pub auto_approve: bool,
    /// Override user data directory (defaults to %LOCALAPPDATA%\coda).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct AskArgs {
    /// Coding instruction or question to execute.
    #[arg(required = true)]
    pub prompt: Vec<String>,
    /// Target workspace directory (defaults to current working directory).
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Route ID to use (defaults to first configured route or 'default').
    #[arg(long)]
    pub route: Option<String>,
    /// Override the active route's model.
    #[arg(long)]
    pub model: Option<String>,
    /// Continue the most recent chat session in this workspace.
    #[arg(short = 'c', long = "continue", conflicts_with = "resume")]
    pub continue_session: bool,
    /// Resume a specific session ID or run ID.
    #[arg(
        long = "resume",
        value_name = "ID",
        conflicts_with = "continue_session"
    )]
    pub resume: Option<String>,
    /// Automatically approve file edits and shell commands without prompting.
    #[arg(short = 'y', long = "auto-approve")]
    pub auto_approve: bool,
    /// Override user data directory (defaults to %LOCALAPPDATA%\coda).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct HostArgs {
    #[command(subcommand)]
    command: HostCommand,
}

#[derive(Subcommand)]
enum HostCommand {
    /// Read the assembled capabilities and each independent runtime observation.
    Composition,
    Config(HostConfigArgs),
    Route(HostRouteArgs),
}

#[derive(Args)]
struct HostConfigArgs {
    #[command(subcommand)]
    command: HostConfigCommand,
}

#[derive(Subcommand)]
enum HostConfigCommand {
    /// Read current local configuration, not an earlier operation receipt.
    Get,
    /// Read the original receipt; 404 does not resolve an in-flight PUT.
    Operation {
        #[arg(long)]
        key: String,
    },
}

#[derive(Args)]
struct HostRouteArgs {
    #[command(subcommand)]
    command: HostRouteCommand,
}

#[derive(Subcommand)]
enum HostRouteCommand {
    /// Save local configuration only; does not grant access or contact a Provider.
    Set(HostRouteSet),
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("credential").required(true).multiple(false)
    .args(["credential_prompt", "credential_stdin", "keep_credential"])))]
struct HostRouteSet {
    id: String,
    #[arg(long)]
    expected_revision: u64,
    #[arg(long)]
    name: String,
    #[arg(long)]
    base_url: String,
    #[arg(long)]
    model: String,
    #[arg(long)]
    max_tokens: u32,
    #[arg(long)]
    parallel_limit: usize,
    #[arg(long)]
    key: String,
    /// Read from a real console with echo disabled.
    #[arg(long)]
    credential_prompt: bool,
    /// Read one bounded UTF-8 line from redirected stdin; a TTY is rejected.
    #[arg(long)]
    credential_stdin: bool,
    /// Keep an existing usable credential; changing the API base requires replacement.
    #[arg(long)]
    keep_credential: bool,
}

fn host_key_valid(key: &str) -> bool {
    secrets::validate_idempotency_key(key).is_ok() && !key.contains(',')
}

fn host_route_id_valid(id: &str) -> bool {
    coda::domain::valid_name(id) && id != "newapi-account"
}

impl HostRouteSet {
    fn input(&self) -> coda::host_configuration::RouteInput {
        coda::host_configuration::RouteInput {
            name: self.name.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            parallel_limit: self.parallel_limit,
        }
    }
    fn valid(&self) -> bool {
        host_route_id_valid(&self.id)
            && self.expected_revision <= i64::MAX as u64
            && host_key_valid(&self.key)
            && secrets::safe_metadata_text("name", &self.name).is_ok()
            && secrets::safe_metadata_text("model", &self.model).is_ok()
            && self.name.len() <= 500
            && self.model.len() <= 500
            && !self.name.trim().is_empty()
            && !self.model.trim().is_empty()
            && self.input().normalized().is_ok()
            && (16..=65536).contains(&self.max_tokens)
            && (1..=8).contains(&self.parallel_limit)
            && [
                self.credential_prompt,
                self.credential_stdin,
                self.keep_credential,
            ]
            .into_iter()
            .filter(|enabled| *enabled)
            .count()
                == 1
    }
    fn route(&self) -> Result<Value, Failure> {
        self.input()
            .normalized()
            .map(|route| json!(route))
            .map_err(|_| {
                Failure::fixed("host_config_invalid", "配置参数无效，配置PUT未发送", false)
            })
    }
}

impl HostCommand {
    fn valid(&self) -> bool {
        match self {
            Self::Composition => true,
            Self::Config(HostConfigArgs {
                command: HostConfigCommand::Get,
            }) => true,
            Self::Config(HostConfigArgs {
                command: HostConfigCommand::Operation { key },
            }) => host_key_valid(key),
            Self::Route(HostRouteArgs {
                command: HostRouteCommand::Set(args),
            }) => args.valid(),
        }
    }
}

struct HostSecret(String);
impl HostSecret {
    fn as_str(&self) -> &str {
        &self.0
    }
}
impl Drop for HostSecret {
    fn drop(&mut self) {
        for byte in unsafe { self.0.as_bytes_mut() } {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Args)]
pub struct RuntimeArgs {
    #[command(subcommand)]
    command: RuntimeCommand,
}

#[derive(Subcommand)]
enum RuntimeCommand {
    Agent(AgentRuntimeArgs),
    Component(RuntimeComponentArgs),
}

#[derive(Args)]
struct RuntimeComponentArgs {
    #[command(subcommand)]
    command: RuntimeComponentCommand,
}

#[derive(Subcommand)]
enum RuntimeComponentCommand {
    List,
    Status {
        id: String,
    },
    SetEnabled {
        id: String,
        #[arg(long, required = true, action = clap::ArgAction::Set)]
        enabled: bool,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        key: String,
    },
}

#[derive(Args)]
struct AgentRuntimeArgs {
    #[command(subcommand)]
    command: AgentRuntimeCommand,
}

#[derive(Subcommand)]
enum AgentRuntimeCommand {
    Status,
}

#[derive(Args)]
pub struct PluginArgs {
    #[command(subcommand)]
    command: PluginCommand,
}

#[derive(Subcommand)]
enum PluginCommand {
    List,
    Status {
        id: String,
    },
    SetEnabled {
        id: String,
        #[arg(long, action = clap::ArgAction::Set)]
        enabled: bool,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        key: String,
    },
}

#[derive(Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
}

#[derive(Subcommand)]
enum ProjectCommand {
    Budget(BudgetArgs),
    /// Explicit linked-worktree Project creation and read-only diagnostics.
    Worktree(WorktreeArgs),
    List,
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        root: String,
        #[arg(long)]
        key: String,
    },
    Get {
        id: String,
    },
    Current,
    Select {
        id: String,
        #[arg(long)]
        expected_revision: u64,
    },
    Config(ProjectConfigArgs),
}

#[derive(Args)]
struct BudgetArgs {
    #[command(subcommand)]
    command: BudgetCommand,
}

#[derive(Subcommand)]
enum BudgetCommand {
    Config(BudgetConfigArgs),
    Status {
        #[arg(long)]
        id: String,
    },
    Call {
        #[arg(long)]
        id: String,
        #[arg(long)]
        call_id: String,
    },
}

#[derive(Args)]
struct BudgetConfigArgs {
    #[command(subcommand)]
    command: BudgetConfigCommand,
}

#[derive(Subcommand)]
enum BudgetConfigCommand {
    Get {
        #[arg(long)]
        id: String,
    },
    Set {
        #[arg(long)]
        id: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        limit_tokens: u64,
        #[arg(long)]
        per_call_reservation_tokens: u64,
        #[arg(long)]
        key: String,
    },
}

impl BudgetCommand {
    fn valid(&self) -> bool {
        match self {
            Self::Status { id }
            | Self::Config(BudgetConfigArgs {
                command: BudgetConfigCommand::Get { id },
            }) => valid_id(id),
            Self::Call { id, call_id } => valid_id(id) && valid_id(call_id),
            Self::Config(BudgetConfigArgs {
                command:
                    BudgetConfigCommand::Set {
                        id,
                        expected_revision,
                        limit_tokens,
                        per_call_reservation_tokens,
                        key,
                    },
            }) => {
                valid_id(id)
                    && *expected_revision < i64::MAX as u64
                    && BudgetConfig {
                        limit_tokens: *limit_tokens,
                        per_call_reservation_tokens: *per_call_reservation_tokens,
                    }
                    .validate()
                    .is_ok()
                    && host_key_valid(key)
            }
        }
    }
}

#[derive(Args)]
struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeCommand,
}

#[derive(Subcommand)]
enum WorktreeCommand {
    Create {
        #[arg(long)]
        source_project: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        parent: String,
        #[arg(long)]
        expected_config_revision: u64,
        #[arg(long)]
        expected_head: String,
        #[arg(long)]
        key: String,
    },
    Status {
        #[arg(long)]
        source_project: String,
        #[arg(long, required_unless_present = "key", conflicts_with = "key")]
        id: Option<String>,
        #[arg(long, required_unless_present = "id", conflicts_with = "id")]
        key: Option<String>,
    },
    Inspect {
        #[arg(long)]
        source_project: String,
        #[arg(long)]
        id: String,
    },
}

fn canonical_uuid(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id)
}

fn commit_oid(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

impl WorktreeCommand {
    fn valid(&self) -> bool {
        match self {
            Self::Create {
                source_project,
                name,
                parent,
                expected_config_revision,
                expected_head,
                key,
            } => {
                valid_id(source_project)
                    && !name.trim().is_empty()
                    && name.len() <= 500
                    && !parent.trim().is_empty()
                    && parent.len() <= 32768
                    && (1..=i64::MAX as u64).contains(expected_config_revision)
                    && commit_oid(expected_head)
                    && host_key_valid(key)
            }
            Self::Status {
                source_project,
                id,
                key,
            } => {
                valid_id(source_project)
                    && (id.is_some() != key.is_some())
                    && id.as_deref().is_none_or(canonical_uuid)
                    && key.as_deref().is_none_or(host_key_valid)
            }
            Self::Inspect { source_project, id } => valid_id(source_project) && canonical_uuid(id),
        }
    }
}

#[derive(Args)]
struct ProjectConfigArgs {
    #[command(subcommand)]
    command: ProjectConfigCommand,
}

#[derive(Subcommand)]
enum ProjectConfigCommand {
    Get {
        id: String,
    },
    Set {
        id: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        file: PathBuf,
    },
}

#[derive(Args)]
pub struct RunArgs {
    #[command(subcommand)]
    command: RunCommand,
}

#[derive(Subcommand)]
enum RunCommand {
    Start(StartArgs),
    List {
        #[arg(long)]
        project: Option<String>,
    },
    Status {
        id: String,
    },
    Context {
        id: String,
    },
    Events(EventsArgs),
}

#[derive(Args)]
struct StartArgs {
    #[arg(long, requires = "expected_config_revision")]
    project: Option<String>,
    #[arg(long, requires = "project", value_parser = clap::value_parser!(u64).range(1..))]
    expected_config_revision: Option<u64>,
    #[arg(long)]
    title: String,
    #[arg(long)]
    route: String,
    #[arg(long)]
    message: String,
    #[arg(long)]
    key: String,
    #[arg(long)]
    tools: bool,
    #[arg(long, requires = "tools")]
    scope: Vec<String>,
    #[arg(long, requires = "tools")]
    allow_commands: bool,
}

#[derive(Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Subcommand)]
enum SessionCommand {
    Status {
        id: String,
    },
    Turns {
        id: String,
    },
    Send {
        id: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        after_turn: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        key: String,
    },
    Events(EventsArgs),
}

fn decimal_cursor(value: &str) -> Result<i64, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("游标必须为非负十进制整数".into());
    }
    value.parse::<i64>().map_err(|_| "游标超出范围".into())
}

#[derive(Args)]
struct EventsArgs {
    id: String,
    #[arg(long, default_value = "0", value_parser = decimal_cursor)]
    after: i64,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    limit: Option<u64>,
}

#[derive(Args)]
pub struct TaskArgs {
    #[command(subcommand)]
    command: TaskCommand,
}

#[derive(Subcommand)]
enum TaskCommand {
    Project {
        id: String,
    },
    NetworkCalls {
        id: String,
    },
    Diff {
        id: String,
        #[arg(long)]
        path: String,
        #[arg(long, value_enum, default_value = "head")]
        view: DiffView,
    },
    Checkpoint {
        id: String,
        #[arg(long)]
        key: String,
    },
    Approvals {
        task_id: String,
    },
    Resume {
        id: String,
        #[arg(long)]
        message: String,
    },
    Cancel {
        id: String,
    },
    Changes {
        id: String,
    },
    Restore {
        id: String,
    },
}

#[derive(Clone, Copy, ValueEnum, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum DiffView {
    Staged,
    Unstaged,
    Head,
}

#[derive(Args)]
pub struct CheckpointArgs {
    #[command(subcommand)]
    command: CheckpointCommand,
}

#[derive(Subcommand)]
enum CheckpointCommand {
    Get { id: String },
    Restore { id: String },
}

#[derive(Args)]
pub struct ApprovalArgs {
    #[command(subcommand)]
    command: ApprovalCommand,
}

#[derive(Subcommand)]
enum ApprovalCommand {
    Get {
        approval_id: String,
    },
    Approve {
        approval_id: String,
        #[arg(long)]
        acknowledge_command_network: bool,
    },
    Deny {
        approval_id: String,
    },
}

#[derive(Args)]
pub struct NetworkArgs {
    #[command(subcommand)]
    command: NetworkCommand,
}

#[derive(Args, Clone)]
struct SubjectArgs {
    #[arg(long)]
    route: Option<String>,
    #[arg(long, requires = "route", conflicts_with = "account")]
    project: Option<String>,
    #[arg(long, conflicts_with = "route")]
    account: bool,
}

impl SubjectArgs {
    fn valid(&self) -> bool {
        self.route.as_deref().is_none_or(valid_id)
            && (self.route.is_some() != self.account)
            && (self.route.is_some() == self.project.is_some())
            && self.project.as_deref().is_none_or(valid_id)
    }
    fn subject(self) -> NetworkSubject {
        match self.route {
            Some(id) => NetworkSubject {
                kind: NetworkSubjectKind::Route,
                id,
                project_id: self.project.map(ProjectId),
            },
            None => NetworkSubject {
                kind: NetworkSubjectKind::Account,
                id: "newapi".into(),
                project_id: None,
            },
        }
    }
}

#[derive(Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
enum PurposeArg {
    Models,
    Completion,
    Balance,
}

#[derive(Subcommand)]
enum NetworkCommand {
    Preview(SubjectArgs),
    Authorize {
        #[command(flatten)]
        subject: SubjectArgs,
        #[arg(long)]
        config_revision: Option<u64>,
        #[arg(long)]
        credential_revision: Option<u64>,
        #[arg(long)]
        binding: Option<String>,
        #[arg(long, value_enum, required = true)]
        purpose: Vec<PurposeArg>,
        #[arg(long)]
        key: String,
    },
    Get {
        id: String,
    },
    Revoke {
        id: String,
    },
    Call {
        id: String,
    },
}

#[derive(Args)]
pub struct RouteArgs {
    #[command(subcommand)]
    command: RouteCommand,
}

#[derive(Subcommand)]
enum RouteCommand {
    Models {
        id: String,
        #[arg(long)]
        project: String,
    },
    Check {
        id: String,
        #[arg(long)]
        project: String,
    },
}

#[derive(Args)]
pub struct AccountArgs {
    #[command(subcommand)]
    command: AccountCommand,
}

#[derive(Subcommand)]
enum AccountCommand {
    Balance,
}

fn valid_id(id: &str) -> bool {
    secrets::validate_persisted_id("id", id).is_ok() && !matches!(id, "." | "..")
}

fn valid_message(message: &str) -> bool {
    !message.trim().is_empty() && message.len() <= 100_000
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Command {
    pub fn is_local_workbench(&self) -> bool {
        matches!(
            self,
            Self::Init(_)
                | Self::Chat(_)
                | Self::Ask(_)
                | Self::Jobs(_)
                | Self::Computer { .. }
                | Self::Mcp { .. }
        )
    }

    pub fn valid_ids(&self) -> bool {
        match self {
            Self::Init(args) => {
                host_route_id_valid(&args.route)
                    && !args.name.trim().is_empty()
                    && (16..=65536).contains(&args.max_tokens)
                    && (1..=8).contains(&args.parallel_limit)
            }
            Self::Jobs(args) => valid_id(&args.session),
            Self::Computer { .. } => true,
            Self::Chat(args) => {
                args.route.as_deref().is_none_or(host_route_id_valid)
                    && args
                        .model
                        .as_deref()
                        .is_none_or(|m| !m.trim().is_empty() && m.len() <= 500)
                    && args.resume.as_deref().is_none_or(valid_id)
            }
            Self::Ask(args) => {
                let text = args.prompt.join(" ");
                valid_message(&text)
                    && args.route.as_deref().is_none_or(host_route_id_valid)
                    && args
                        .model
                        .as_deref()
                        .is_none_or(|m| !m.trim().is_empty() && m.len() <= 500)
                    && args.resume.as_deref().is_none_or(valid_id)
            }
            Self::Mcp {
                command: McpCommand::Login { server, .. },
            } => !server.is_empty(),
            Self::Serve => true,
            Self::Host(args) => args.command.valid(),
            Self::Runtime(args) => match &args.command {
                RuntimeCommand::Agent(_) => true,
                RuntimeCommand::Component(args) => match &args.command {
                    RuntimeComponentCommand::List => true,
                    RuntimeComponentCommand::Status { id } => valid_id(id),
                    RuntimeComponentCommand::SetEnabled {
                        id,
                        expected_revision,
                        key,
                        ..
                    } => {
                        valid_id(id) && *expected_revision <= i64::MAX as u64 && host_key_valid(key)
                    }
                },
            },
            Self::Plugin(args) => match &args.command {
                PluginCommand::List => true,
                PluginCommand::Status { id } => valid_id(id),
                PluginCommand::SetEnabled {
                    id,
                    expected_revision,
                    key,
                    ..
                } => valid_id(id) && *expected_revision <= i64::MAX as u64 && host_key_valid(key),
            },
            Self::Account(_) => true,
            Self::Project(args) => match &args.command {
                ProjectCommand::List | ProjectCommand::Current => true,
                ProjectCommand::Get { id } | ProjectCommand::Select { id, .. } => valid_id(id),
                ProjectCommand::Create { name, root, key } => {
                    !name.trim().is_empty() && !root.trim().is_empty() && host_key_valid(key)
                }
                ProjectCommand::Config(args) => match &args.command {
                    ProjectConfigCommand::Get { id } | ProjectConfigCommand::Set { id, .. } => {
                        valid_id(id)
                    }
                },
                ProjectCommand::Worktree(args) => args.command.valid(),
                ProjectCommand::Budget(args) => args.command.valid(),
            },
            Self::Route(args) => match &args.command {
                RouteCommand::Models { id, project } | RouteCommand::Check { id, project } => {
                    valid_id(id) && valid_id(project)
                }
            },
            Self::Network(args) => match &args.command {
                NetworkCommand::Preview(subject) => subject.valid(),
                NetworkCommand::Authorize {
                    subject,
                    binding,
                    key,
                    ..
                } => {
                    subject.valid()
                        && binding.as_deref().is_none_or(valid_digest)
                        && host_key_valid(key)
                }
                NetworkCommand::Get { id }
                | NetworkCommand::Revoke { id }
                | NetworkCommand::Call { id } => valid_id(id),
            },
            Self::Run(args) => match &args.command {
                RunCommand::List { project } => project.as_deref().is_none_or(valid_id),
                RunCommand::Start(start) => {
                    valid_id(&start.route)
                        && start.project.as_deref().is_none_or(valid_id)
                        && (start.project.is_some() == start.expected_config_revision.is_some())
                        && !start.title.trim().is_empty()
                        && start.title.len() <= 500
                        && valid_message(&start.message)
                        && secrets::validate_idempotency_key(&start.key).is_ok()
                        && start
                            .scope
                            .iter()
                            .all(|scope| coda::domain::scope_path(scope).is_ok())
                }
                RunCommand::Status { id } | RunCommand::Context { id } => valid_id(id),
                RunCommand::Events(events) => valid_id(&events.id),
            },
            Self::Session(args) => match &args.command {
                SessionCommand::Status { id } | SessionCommand::Turns { id } => valid_id(id),
                SessionCommand::Events(events) => valid_id(&events.id),
                SessionCommand::Send {
                    id,
                    agent,
                    after_turn,
                    message,
                    key,
                } => {
                    [id, agent, after_turn].iter().all(|id| valid_id(id))
                        && valid_message(message)
                        && host_key_valid(key)
                }
            },
            Self::Task(task) => match &task.command {
                TaskCommand::Approvals { task_id } => valid_id(task_id),
                TaskCommand::NetworkCalls { id } | TaskCommand::Project { id } => valid_id(id),
                TaskCommand::Diff { id, path, .. } => {
                    valid_id(id) && coda::domain::scope_path(path).is_ok()
                }
                TaskCommand::Checkpoint { id, key } => valid_id(id) && host_key_valid(key),
                TaskCommand::Cancel { id }
                | TaskCommand::Changes { id }
                | TaskCommand::Restore { id } => valid_id(id),
                TaskCommand::Resume { id, message } => valid_id(id) && valid_message(message),
            },
            Self::Checkpoint(args) => match &args.command {
                CheckpointCommand::Get { id } | CheckpointCommand::Restore { id } => valid_id(id),
            },
            Self::Approval(approval) => match &approval.command {
                ApprovalCommand::Get { approval_id }
                | ApprovalCommand::Approve { approval_id, .. }
                | ApprovalCommand::Deny { approval_id } => valid_id(approval_id),
            },
        }
    }
}

#[derive(Debug)]
pub struct Failure {
    code: &'static str,
    message: String,
    retryable: bool,
    details: Option<Value>,
    after: Option<i64>,
}

impl Failure {
    fn fixed(code: &'static str, message: &'static str, retryable: bool) -> Self {
        Self {
            code,
            message: message.to_owned(),
            retryable,
            details: None,
            after: None,
        }
    }

    fn custom(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            details: None,
            after: None,
        }
    }

    pub fn print(&self, json_output: bool) {
        if self.code == "worktree_not_succeeded" {
            if !json_output {
                eprintln!("工作树未成功完成；请按原 key 查询。不会自动重试、接纳或清理。");
            }
            return;
        }
        if self.code == "plugin_not_converged" {
            if !json_output {
                eprintln!("持久命令已接受，当前运行时尚未收敛；请查询状态。");
            }
            return;
        }
        let mut value = json!({
            "code": self.code,
            "message": self.message,
            "retryable": self.retryable,
            "error": self.message,
        });
        if let Some(details) = &self.details {
            for field in [
                "receipt",
                "status",
                "restored",
                "ok",
                "dispatch_state",
                "calls",
            ] {
                if let Some(item) = details.get(field) {
                    value[field] = item.clone();
                }
            }
        }
        if let Some(after) = self.after {
            value["after"] = json!(after);
        }
        if json_output {
            eprintln!("{value}");
        } else {
            eprintln!("{}: {}", self.code, self.message);
            if let Some(details) = &self.details {
                eprintln!("{details}");
            }
            if let Some(after) = self.after {
                eprintln!("最后已输出游标：{after}；使用 --after {after} 手动继续观察");
            }
        }
    }
}

// ============================================================================
// Flagship In-Process Coding Workbench (`init`, `chat`, `ask`)
// ============================================================================

fn enable_windows_ansi() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE,
            STD_OUTPUT_HANDLE, SetConsoleMode,
        };
        for std_id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = GetStdHandle(std_id);
            if !handle.is_null() {
                let mut mode = 0u32;
                if GetConsoleMode(handle, &mut mode) != 0 {
                    let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                }
            }
        }
    }
}

fn resolve_user_data_dir(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir;
    }
    if let Some(env_dir) = std::env::var_os("CODA_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(env_dir);
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        return PathBuf::from(local_app_data).join("coda");
    }
    PathBuf::from(".coda-data")
}

fn open_local_engine(
    instance_lock: &mut Option<std::fs::File>,
    data_dir: &Path,
) -> Result<Arc<Engine>, Failure> {
    std::fs::create_dir_all(data_dir).map_err(|e| {
        Failure::custom(
            "storage_error",
            format!("无法创建数据目录 {}: {e}", data_dir.display()),
        )
    })?;
    let default_ws = data_dir.join("workspace");
    let _ = std::fs::create_dir_all(&default_ws);

    let store = Arc::new(
        Store::open(&data_dir.join("coda.sqlite3"))
            .map_err(|e| Failure::custom("storage_error", format!("无法打开本地数据库: {e}")))?,
    );
    let lock = store
        .instance_lock_handle()
        .map_err(|e| Failure::custom("storage_error", format!("无法获取数据目录实例锁: {e}")))?;
    *instance_lock = Some(lock);

    let settings = match store
        .settings()
        .map_err(|e| Failure::custom("storage_error", format!("读取本地配置失败: {e}")))?
    {
        Some(mut existing) => {
            if !Path::new(&existing.workspace).is_dir() {
                let _ = std::fs::create_dir_all(&existing.workspace);
            }
            if !Path::new(&existing.workspace).is_dir() {
                existing.workspace = default_ws.to_string_lossy().into_owned();
            }
            existing
        }
        None => fresh_settings(&store, &default_ws)
            .map_err(|e| Failure::custom("storage_error", format!("初始化配置失败: {e}")))?,
    };
    let concurrency = settings.max_concurrency.clamp(1, 8);
    Engine::new(store, concurrency)
        .map_err(|e| Failure::custom("engine_error", format!("启动本地引擎失败: {e}")))
}

pub async fn execute_local(
    instance_lock: &mut Option<std::fs::File>,
    top_data_dir: Option<PathBuf>,
    top_workspace: Option<PathBuf>,
    json_output: bool,
    command: Command,
) -> Result<(), Failure> {
    enable_windows_ansi();
    match command {
        Command::Computer { command } => computer::run(command, top_workspace, json_output).await,
        Command::Mcp {
            command:
                McpCommand::Login {
                    server,
                    dir,
                    data_dir,
                    callback_port,
                },
        } => {
            let dir = dir.or(top_workspace).unwrap_or_else(|| PathBuf::from("."));
            let data_dir = resolve_user_data_dir(data_dir.or(top_data_dir));
            let store = Store::open(&data_dir.join("coda.sqlite3"))
                .map_err(|error| Failure::custom("storage_error", error.to_string()))?;
            coda::mcp::oauth::login(&store, &dir, &server, callback_port)
                .await
                .map_err(|error| Failure::custom("mcp_login_failed", error.to_string()))?;
            if json_output {
                println!("{}", json!({"ok": true, "server": server}));
            } else {
                println!("MCP OAuth 登录成功。");
            }
            Ok(())
        }
        Command::Jobs(args) => {
            jobs::run(
                instance_lock,
                top_data_dir,
                top_workspace,
                json_output,
                args,
            )
            .await
        }
        Command::Init(args) => run_init(instance_lock, top_data_dir, json_output, args).await,
        Command::Chat(args) => {
            run_chat(
                instance_lock,
                top_data_dir,
                top_workspace,
                json_output,
                args,
            )
            .await
        }
        Command::Ask(args) => {
            run_ask(
                instance_lock,
                top_data_dir,
                top_workspace,
                json_output,
                args,
            )
            .await
        }
        _ => unreachable!(),
    }
}
