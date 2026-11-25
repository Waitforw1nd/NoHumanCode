use clap::{Args, Subcommand, ValueEnum};
use coda::approval::{ApprovalStatus, CommandNetworkAck, CommandNetworkNotice};
use coda::budget::{
    BudgetCall, BudgetConfig, BudgetConfigSnapshot, BudgetMutationResult, BudgetStatus,
};
use coda::credential_input::{CredentialInputError, CredentialInputMode, read_credential};
use coda::domain::{
    AgentId, Event, Project, ProjectId, Route, Run, RunRequest, SendChatTurn, Session, SessionId,
    SessionKind, Task, TaskSpec, Turn, TurnId, TurnTask,
};
use coda::engine::Engine;
use coda::network_authorization::{
    AuthorizationState, CallReceipt, ConfirmRequest, DispatchState, NetworkAuthorization,
    NetworkDecision, NetworkPreview, NetworkResponse, NetworkSubject, NetworkSubjectKind,
};
use coda::plugin_management::{PluginMutationResult, PluginStatus};
use coda::project_config::{
    ConfigureProject, ProjectConfig, ProjectConfigSnapshot, ProjectSelection, RegisterProject,
    SelectProject, StartInProject, TaskProjectBinding,
};
use coda::secrets;
use coda::store::{Store, fresh_settings};
use coda::workspace_changes::{RestoreReceipt, RestoreStatus, WorkspaceChange};
use coda::worktree_management::{WorktreeInspection, WorktreeReceipt, WorktreeState};
use futures_util::StreamExt;
use reqwest::{Client, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const RESPONSE_LIMIT: usize = 1_048_576;
const FRAME_LIMIT: usize = 1_048_576;
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4.1";
const WORKBENCH_MAX_ROUNDS: usize = 48;

#[derive(Subcommand)]
pub enum Command {
    /// One-step interactive or flag-driven provider & API key setup.
    Init(InitArgs),
    /// Interactive in-process AI coding workbench (zero separate server needed).
    Chat(ChatArgs),
    /// One-shot in-process coding execution in the target directory.
    Ask(AskArgs),
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
    /// Override the platform's user data directory or CODA_DATA_DIR.
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
    /// Automatically approve file edits and shell commands throughout this CLI process.
    #[arg(short = 'y', long = "auto-approve")]
    pub auto_approve: bool,
    /// Override the platform's user data directory or CODA_DATA_DIR.
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
    /// Override the platform's user data directory or CODA_DATA_DIR.
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
        matches!(self, Self::Init(_) | Self::Chat(_) | Self::Ask(_))
    }

    pub fn valid_ids(&self) -> bool {
        match self {
            Self::Init(args) => {
                host_route_id_valid(&args.route)
                    && !args.name.trim().is_empty()
                    && (16..=65536).contains(&args.max_tokens)
                    && (1..=8).contains(&args.parallel_limit)
            }
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

pub(super) fn resolve_user_data_dir(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir;
    }
    if let Some(env_dir) = std::env::var_os("CODA_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(env_dir);
    }
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::home_dir().map(|home| home.join("Library/Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".local/share")));
    base.map(|dir| dir.join("coda"))
        .unwrap_or_else(|| PathBuf::from(".coda-data"))
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

fn prompt_line_with_default(label: &str, default_val: &str) -> Result<String, Failure> {
    eprint!("\x1b[1m{label}\x1b[0m [{default_val}]: ");
    let _ = io::stderr().flush();
    let mut buf = String::new();
    io::stdin()
        .read_line(&mut buf)
        .map_err(|e| Failure::custom("input_error", format!("读取输入失败: {e}")))?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        Ok(default_val.to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

fn read_secret_from_stdin_line() -> Result<String, Failure> {
    read_credential(CredentialInputMode::Stdin)
        .map(Into::into)
        .map_err(|err| match err {
            CredentialInputError::Cancelled => {
                Failure::fixed("credential_cancelled", "API Key 输入已取消", false)
            }
            CredentialInputError::Unavailable => Failure::fixed(
                "credential_unavailable",
                "--api-key-stdin 需要通过管道或文件输入",
                false,
            ),
            CredentialInputError::Invalid => {
                Failure::fixed("credential_invalid", "API Key 输入无效", false)
            }
        })
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProviderPreset {
    provider: InitProvider,
    display_name: &'static str,
    cli_flag: &'static str,
    key_env: &'static str,
    base_env: &'static str,
    default_base_url: &'static str,
    default_model: &'static str,
}

impl InitProvider {
    fn preset(self) -> ProviderPreset {
        match self {
            Self::Openai => ProviderPreset {
                provider: Self::Openai,
                display_name: "OpenAI",
                cli_flag: "openai",
                key_env: "OPENAI_API_KEY",
                base_env: "OPENAI_BASE_URL",
                default_base_url: DEFAULT_BASE_URL,
                default_model: DEFAULT_MODEL,
            },
            Self::Anthropic => ProviderPreset {
                provider: Self::Anthropic,
                display_name: "Anthropic",
                cli_flag: "anthropic",
                key_env: "ANTHROPIC_API_KEY",
                base_env: "ANTHROPIC_BASE_URL",
                default_base_url: "https://api.anthropic.com/v1",
                default_model: "claude-sonnet-4-20250514",
            },
            Self::Gemini => ProviderPreset {
                provider: Self::Gemini,
                display_name: "Gemini",
                cli_flag: "gemini",
                key_env: "GEMINI_API_KEY",
                base_env: "GEMINI_BASE_URL",
                default_base_url: "https://generativelanguage.googleapis.com/v1beta",
                default_model: "gemini-2.5-pro",
            },
        }
    }

    fn from_protocol(protocol: coda::provider::ProviderProtocol) -> Self {
        match protocol {
            coda::provider::ProviderProtocol::OpenAi => Self::Openai,
            coda::provider::ProviderProtocol::Anthropic => Self::Anthropic,
            coda::provider::ProviderProtocol::Gemini => Self::Gemini,
        }
    }
}

fn non_empty_env_with<F>(env_fn: &F, name: &str) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    env_fn(name)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn resolve_init_provider<F>(
    explicit: Option<InitProvider>,
    base_url: Option<&str>,
    env_fn: F,
) -> InitProvider
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(provider) = explicit {
        return provider;
    }
    if let Some(url) = base_url.map(str::trim).filter(|s| !s.is_empty()) {
        return InitProvider::from_protocol(coda::provider::ProviderProtocol::detect(url));
    }
    [
        InitProvider::Openai,
        InitProvider::Anthropic,
        InitProvider::Gemini,
    ]
    .into_iter()
    .find(|&candidate| non_empty_env_with(&env_fn, candidate.preset().key_env).is_some())
    .unwrap_or(InitProvider::Openai)
}

struct ZeroInitPlan {
    preset: ProviderPreset,
    base_url: String,
    model: String,
    api_key: HostSecret,
}

fn resolve_zero_init_plan<F>(
    target_id: &str,
    requested_route: Option<&str>,
    model_override: Option<&str>,
    env_fn: F,
) -> Result<ZeroInitPlan, Failure>
where
    F: Fn(&str) -> Option<String>,
{
    let explicit_provider = match requested_route {
        Some(r) if r.eq_ignore_ascii_case("anthropic") => Some(InitProvider::Anthropic),
        Some(r) if r.eq_ignore_ascii_case("gemini") => Some(InitProvider::Gemini),
        Some(r) if r.eq_ignore_ascii_case("openai") => Some(InitProvider::Openai),
        _ => None,
    };

    let model_hint = model_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| non_empty_env_with(&env_fn, "CODA_MODEL"));

    let hint_provider = model_hint.as_deref().and_then(|hint| {
        let lower = hint.to_ascii_lowercase();
        if lower.starts_with("claude") && non_empty_env_with(&env_fn, "ANTHROPIC_API_KEY").is_some()
        {
            Some(InitProvider::Anthropic)
        } else if (lower.starts_with("gemini") || lower.starts_with("models/gemini"))
            && non_empty_env_with(&env_fn, "GEMINI_API_KEY").is_some()
        {
            Some(InitProvider::Gemini)
        } else if non_empty_env_with(&env_fn, "OPENAI_BASE_URL").is_some()
            && non_empty_env_with(&env_fn, "OPENAI_API_KEY").is_some()
        {
            Some(InitProvider::Openai)
        } else {
            None
        }
    });

    let fallback_by_env = || {
        [
            InitProvider::Openai,
            InitProvider::Anthropic,
            InitProvider::Gemini,
        ]
        .into_iter()
        .find(|&candidate| non_empty_env_with(&env_fn, candidate.preset().key_env).is_some())
    };

    let Some(chosen_provider) = explicit_provider.or(hint_provider).or_else(fallback_by_env) else {
        return Err(Failure::custom(
            "route_not_configured",
            format!(
                "未找到模型路由 `{target_id}`；请先运行 `coda init` 或设置环境变量 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `GEMINI_API_KEY`"
            ),
        ));
    };

    let preset = chosen_provider.preset();
    let Some(api_key) = non_empty_env_with(&env_fn, preset.key_env) else {
        return Err(Failure::custom(
            "route_not_configured",
            format!(
                "未找到模型路由 `{target_id}` 的有效凭据（`{}` 未设置或为空）；请运行 `coda init --provider {}` 或设置环境变量 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `GEMINI_API_KEY`",
                preset.key_env, preset.cli_flag
            ),
        ));
    };

    let base_url = non_empty_env_with(&env_fn, preset.base_env)
        .unwrap_or_else(|| preset.default_base_url.to_owned());
    let model = model_hint.unwrap_or_else(|| preset.default_model.to_owned());

    Ok(ZeroInitPlan {
        preset,
        base_url,
        model,
        api_key: HostSecret(api_key),
    })
}

async fn run_init(
    instance_lock: &mut Option<std::fs::File>,
    top_data_dir: Option<PathBuf>,
    json_output: bool,
    args: InitArgs,
) -> Result<(), Failure> {
    let data_dir = resolve_user_data_dir(args.data_dir.or(top_data_dir));
    let interactive = !args.api_key_stdin && io::stdin().is_terminal() && !json_output;

    let provider = resolve_init_provider(args.provider, args.base_url.as_deref(), |k| {
        std::env::var(k).ok()
    });
    let preset = provider.preset();

    let env_base = std::env::var(preset.base_env)
        .ok()
        .filter(|s| !s.trim().is_empty());
    let env_model = std::env::var("CODA_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let env_key = std::env::var(preset.key_env)
        .ok()
        .filter(|s| !s.trim().is_empty());

    let raw_base_url = match args.base_url {
        Some(url) => url,
        None if interactive => {
            let def = env_base.as_deref().unwrap_or(preset.default_base_url);
            prompt_line_with_default(&format!("{} API Base URL", preset.display_name), def)?
        }
        None => env_base.unwrap_or_else(|| preset.default_base_url.to_owned()),
    };

    let raw_model = match args.model {
        Some(m) => m,
        None if interactive => {
            let def = env_model.as_deref().unwrap_or(preset.default_model);
            prompt_line_with_default("Default Model", def)?
        }
        None => env_model.unwrap_or_else(|| preset.default_model.to_owned()),
    };

    let normalized = coda::host_configuration::RouteInput {
        name: args.name.clone(),
        base_url: raw_base_url,
        model: raw_model,
        max_tokens: args.max_tokens,
        parallel_limit: args.parallel_limit,
    }
    .normalized()
    .map_err(|_| {
        Failure::fixed(
            "invalid_config",
            "路由配置无效（请检查 HTTPS/loopback Base URL 与模型名称）",
            false,
        )
    })?;

    let api_key = HostSecret(if args.api_key_stdin {
        read_secret_from_stdin_line()?
    } else if let Some(key) = env_key.clone().filter(|_| !interactive) {
        key
    } else if interactive {
        if env_key.is_some() {
            eprintln!(
                "提示: 检测到环境变量 {}，请在隐藏提示中输入要持久保存的 API Key:",
                preset.key_env
            );
        }
        read_credential(CredentialInputMode::Prompt)
            .map_err(|err| match err {
                CredentialInputError::Cancelled => {
                    Failure::fixed("credential_cancelled", "API Key 输入已取消", false)
                }
                CredentialInputError::Unavailable => Failure::custom(
                    "credential_unavailable",
                    format!(
                        "无法打开安全终端输入；可使用 --api-key-stdin 或设置 {}",
                        preset.key_env
                    ),
                ),
                CredentialInputError::Invalid => {
                    Failure::fixed("credential_invalid", "API Key 输入无效", false)
                }
            })?
            .into_string()
    } else if let Some(key) = env_key {
        key
    } else {
        return Err(Failure::custom(
            "credential_required",
            format!(
                "需要提供 API Key：请在终端交互运行 coda init、传入 --api-key-stdin 或设置 {}（支持 OPENAI_API_KEY / ANTHROPIC_API_KEY / GEMINI_API_KEY）",
                preset.key_env
            ),
        ));
    });

    let engine = open_local_engine(instance_lock, &data_dir)?;
    let result = async {
        let mut settings = engine
            .store
            .settings()
            .map_err(|e| Failure::custom("storage_error", e.to_string()))?
            .ok_or_else(|| Failure::fixed("storage_error", "缺少初始设置", false))?;

        let route = Route {
            id: args.route.clone(),
            name: normalized.name,
            base_url: normalized.base_url,
            model: normalized.model,
            max_tokens: normalized.max_tokens,
            parallel_limit: normalized.parallel_limit,
            key_env: None,
        };

        if let Some(existing) = settings.routes.iter_mut().find(|r| r.id == route.id) {
            *existing = route.clone();
        } else {
            settings.routes.push(route.clone());
        }

        engine
            .configure_with_keys(
                settings,
                HashMap::from([(route.id.clone(), api_key.as_str().to_owned())]),
            )
            .await
            .map_err(|e| Failure::custom("config_error", format!("保存路由配置失败: {e}")))?;

        if json_output {
            println!(
                "{}",
                json!({
                    "ok": true,
                    "data_dir": data_dir.to_string_lossy(),
                    "route_id": route.id,
                    "base_url": route.base_url,
                    "model": route.model,
                    "max_tokens": route.max_tokens,
                })
            );
        } else {
            println!(
                "\x1b[32m✓\x1b[0m Coda CLI 初始化完成 ({})!\n  数据目录: {}\n  默认路由: {} ({})\n  模型名称: {}\n\n现在可以在任意代码目录直接运行 `\x1b[1mcoda chat\x1b[0m` 或 `\x1b[1mcoda ask \"...\"\x1b[0m`!",
                preset.display_name,
                data_dir.display(),
                route.id,
                route.base_url,
                route.model
            );
        }
        Ok(())
    }
    .await;

    let _ = engine.shutdown(Duration::from_secs(5)).await;
    result
}

async fn ensure_route_ready(
    engine: &Arc<Engine>,
    requested_route: Option<&str>,
    model_override: Option<&str>,
) -> Result<Route, Failure> {
    let mut settings = engine
        .store
        .settings()
        .map_err(|e| Failure::custom("storage_error", e.to_string()))?
        .ok_or_else(|| Failure::fixed("storage_error", "缺少系统配置", false))?;

    let target_id = requested_route
        .map(str::to_owned)
        .or_else(|| settings.routes.first().map(|r| r.id.clone()))
        .unwrap_or_else(|| "default".to_owned());

    if !settings.routes.iter().any(|r| r.id == target_id) {
        let plan = resolve_zero_init_plan(&target_id, requested_route, model_override, |k| {
            std::env::var(k).ok()
        })?;

        let normalized = coda::host_configuration::RouteInput {
            name: plan.preset.display_name.into(),
            base_url: plan.base_url,
            model: plan.model,
            max_tokens: 16384,
            parallel_limit: 4,
        }
        .normalized()
        .map_err(|_| {
            Failure::custom(
                "invalid_config",
                format!("环境变量 {} 或 CODA_MODEL 格式无效", plan.preset.base_env),
            )
        })?;

        let route = Route {
            id: target_id.clone(),
            name: normalized.name,
            base_url: normalized.base_url,
            model: normalized.model,
            max_tokens: normalized.max_tokens,
            parallel_limit: normalized.parallel_limit,
            key_env: Some(plan.preset.key_env.into()),
        };
        settings.routes.push(route.clone());
        engine
            .configure_with_keys(
                settings.clone(),
                HashMap::from([(target_id.clone(), plan.api_key.as_str().to_owned())]),
            )
            .await
            .map_err(|e| Failure::custom("config_error", format!("自动配置路由失败: {e}")))?;
        return Ok(route);
    }

    if let Some(new_model) = model_override {
        let current_model = settings
            .routes
            .iter()
            .find(|r| r.id == target_id)
            .map(|r| r.model.as_str())
            .unwrap_or_default();
        if current_model != new_model {
            if let Some(route) = settings.routes.iter_mut().find(|r| r.id == target_id) {
                route.model = new_model.trim().to_owned();
            }
            engine
                .configure(settings.clone())
                .await
                .map_err(|e| Failure::custom("config_error", format!("切换模型失败: {e}")))?;
        }
    }

    settings
        .routes
        .into_iter()
        .find(|r| r.id == target_id)
        .ok_or_else(|| Failure::fixed("route_not_found", "未找到指定模型路由", false))
}

async fn resolve_workbench_project(
    engine: &Arc<Engine>,
    target_dir: PathBuf,
) -> Result<Project, Failure> {
    if !target_dir.is_dir() {
        return Err(Failure::custom(
            "invalid_workspace",
            format!("工作目录不存在或不是文件夹: {}", target_dir.display()),
        ));
    }
    let canonical = std::fs::canonicalize(&target_dir).map_err(|e| {
        Failure::custom(
            "invalid_workspace",
            format!("无法解析目录 {}: {e}", target_dir.display()),
        )
    })?;

    let existing_projects = engine
        .store
        .projects()
        .map_err(|e| Failure::custom("project_error", e.to_string()))?;

    let project = if let Some(found) = existing_projects.iter().find(|p| {
        std::fs::canonicalize(&p.root_path)
            .map(|cp| cp == canonical)
            .unwrap_or(false)
    }) {
        found.clone()
    } else if let Some(containing) = existing_projects.iter().find(|p| {
        std::fs::canonicalize(&p.root_path)
            .map(|cp| canonical.starts_with(&cp))
            .unwrap_or(false)
    }) {
        containing.clone()
    } else {
        let folder_name = target_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "workspace".to_owned());
        engine
            .register_project(RegisterProject {
                name: folder_name,
                root_path: canonical.to_string_lossy().into_owned(),
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .map_err(|e| Failure::custom("project_error", format!("注册工作目录失败: {e}")))?
    };

    Ok(project)
}

async fn ensure_project_route_ready(
    engine: &Arc<Engine>,
    project: &Project,
    route_id: &str,
) -> Result<(), Failure> {
    let snap = engine
        .project_config(&project.id)
        .map_err(|e| Failure::custom("project_error", format!("读取项目配置失败: {e}")))?;

    let needs_update = !snap.config.tools
        || !snap.config.allow_commands
        || !snap.config.write_scopes.iter().any(|s| s == "*")
        || !snap.config.allowed_route_ids.iter().any(|r| r == route_id)
        || snap.config.default_route_id.as_deref() != Some(route_id);

    if needs_update {
        let mut allowed = snap.config.allowed_route_ids.clone();
        if !allowed.iter().any(|r| r == route_id) {
            allowed.push(route_id.to_owned());
        }
        let mut scopes = snap.config.write_scopes.clone();
        if !scopes.iter().any(|s| s == "*") {
            scopes = vec!["*".to_owned()];
        }
        engine
            .configure_project(ConfigureProject {
                project_id: project.id.clone(),
                expected_revision: snap.revision,
                config: ProjectConfig {
                    allowed_route_ids: allowed,
                    default_route_id: Some(route_id.to_owned()),
                    tools: true,
                    write_scopes: scopes,
                    allow_commands: true,
                },
            })
            .await
            .map_err(|e| Failure::custom("project_error", format!("更新项目权限失败: {e}")))?;
    }

    let current = engine
        .current_project()
        .map_err(|e| Failure::custom("project_error", e.to_string()))?;
    if current.project_id.as_ref() != Some(&project.id) {
        engine
            .select_project(SelectProject {
                project_id: project.id.clone(),
                expected_revision: current.revision,
            })
            .await
            .map_err(|e| Failure::custom("project_error", format!("选中当前项目失败: {e}")))?;
    }

    // Belt-and-suspenders: ensure network preview/authorization is warm for this project route.
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: route_id.to_owned(),
        project_id: Some(project.id.clone()),
    };
    if let Ok(preview) = engine.network_preview(subject.clone()).await
        && preview
            .authorization
            .as_ref()
            .is_none_or(|auth| auth.state != AuthorizationState::Active)
    {
        let _ = engine
            .network_confirm(
                subject,
                ConfirmRequest {
                    config_revision: preview.config_revision,
                    credential_revision: preview.credential_revision,
                    binding_digest: preview.binding_digest,
                    purposes: preview.purposes,
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .await;
    }

    Ok(())
}

const MENTION_MAX_FILE_BYTES: usize = 64 * 1024;
const MENTION_MAX_PROMPT_BYTES: usize = 95_000;
const MENTION_MAX_DIR_ENTRIES: usize = 200;

struct ActiveChatSession {
    session_id: SessionId,
    agent_id: AgentId,
    last_turn_id: TurnId,
    run_id: String,
    event_cursor: i64,
    turn_count: usize,
    task_history: Vec<String>,
}

struct ProjectSessionSummary {
    latest_task_id: String,
    session_id: SessionId,
    run_id: String,
    title: String,
    agent_id: AgentId,
    last_turn_id: TurnId,
    latest_status: String,
    turn_count: usize,
    task_history: Vec<String>,
    last_event_seq: i64,
    updated_at: u64,
}

impl ProjectSessionSummary {
    fn effective_route(&self, engine: &Engine) -> Result<Route, Failure> {
        // Match Engine::send_chat_turn: the configured route supersedes the task snapshot.
        let task = engine
            .store
            .task(&self.latest_task_id)
            .map_err(|e| Failure::custom("task_error", e.to_string()))?;
        let settings = engine
            .store
            .settings()
            .map_err(|e| Failure::custom("storage_error", e.to_string()))?;
        Ok(settings
            .and_then(|s| s.routes.into_iter().find(|r| r.id == task.spec.route_id))
            .unwrap_or(task.route))
    }

    fn into_active(self) -> ActiveChatSession {
        ActiveChatSession {
            session_id: self.session_id,
            agent_id: self.agent_id,
            last_turn_id: self.last_turn_id,
            run_id: self.run_id,
            event_cursor: self.last_event_seq,
            turn_count: self.turn_count,
            task_history: self.task_history,
        }
    }
}

fn same_workspace_path(left: &str, right: &str) -> bool {
    if left == right || (cfg!(windows) && left.eq_ignore_ascii_case(right)) {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn drain_latest_event_seq(engine: &Engine, run_id: &str) -> i64 {
    let mut cursor = 0i64;
    while let Ok(batch) = engine.store.events(run_id, cursor) {
        let Some(last) = batch.last() else {
            break;
        };
        if last.seq <= cursor {
            break;
        }
        cursor = last.seq;
        if batch.len() < 500 {
            break;
        }
    }
    cursor
}

fn list_project_chat_sessions(engine: &Engine, project: &Project) -> Vec<ProjectSessionSummary> {
    let Ok(runs) = engine.store.runs() else {
        return Vec::new();
    };
    let mut summaries = Vec::new();
    for item in runs {
        let Some(run_id) = item["id"].as_str() else {
            continue;
        };
        let Ok(ctx) = engine.store.run_context(run_id) else {
            continue;
        };
        if ctx.session.kind != SessionKind::Chat {
            continue;
        }
        if ctx.project.id != project.id
            && !same_workspace_path(&ctx.project.root_path, &project.root_path)
        {
            continue;
        }
        let [latest_task] = ctx.tasks.as_slice() else {
            continue;
        };
        if !latest_task.depends_on.is_empty() {
            continue;
        }
        let turns = engine
            .store
            .turns_for_session(&ctx.session.id)
            .unwrap_or_else(|_| vec![ctx.latest_turn.clone()]);
        let mut task_history = Vec::with_capacity(turns.len().max(1));
        for turn in &turns {
            if let Ok(tt) = engine.store.turn_tasks(&turn.id) {
                for entry in tt {
                    task_history.push(entry.legacy_task_id);
                }
            }
        }
        if task_history.is_empty() {
            task_history.push(latest_task.legacy_task_id.clone());
        }
        let last_event_seq = drain_latest_event_seq(engine, &ctx.run_id);
        let updated_at = ctx.latest_turn.updated_at.max(ctx.session.updated_at);
        let turn_count = turns.len().max(1);
        summaries.push(ProjectSessionSummary {
            latest_task_id: latest_task.legacy_task_id.clone(),
            session_id: ctx.session.id,
            run_id: ctx.run_id,
            title: ctx.session.title,
            agent_id: latest_task.agent_id.clone(),
            last_turn_id: ctx.latest_turn.id,
            latest_status: ctx.latest_turn.status.as_str().to_owned(),
            turn_count,
            task_history,
            last_event_seq,
            updated_at,
        });
    }
    summaries.sort_by_key(|a| std::cmp::Reverse((a.last_event_seq, a.updated_at)));
    summaries
}

fn resolve_session_by_id(
    engine: &Engine,
    project: &Project,
    target: &str,
) -> Result<ProjectSessionSummary, Failure> {
    let target = target.trim();
    let sessions = list_project_chat_sessions(engine, project);
    if let Some(exact) = sessions
        .iter()
        .position(|s| s.session_id.0 == target || s.run_id == target)
    {
        let mut sessions = sessions;
        return Ok(sessions.swap_remove(exact));
    }
    if target.len() >= 4 {
        let matches: Vec<usize> = sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.session_id.0.starts_with(target) || s.run_id.starts_with(target))
            .map(|(idx, _)| idx)
            .collect();
        if matches.len() == 1 {
            let mut sessions = sessions;
            return Ok(sessions.swap_remove(matches[0]));
        }
        if matches.len() > 1 {
            return Err(Failure::custom(
                "ambiguous_session",
                format!("会话前缀 `{target}` 匹配到多个历史会话，请提供完整 ID"),
            ));
        }
    }
    Err(Failure::custom(
        "session_not_found",
        format!("当前项目中未找到会话或运行 `{target}`"),
    ))
}

fn resolve_initial_session(
    engine: &Engine,
    project: &Project,
    continue_session: bool,
    resume_id: Option<&str>,
    json_output: bool,
) -> Result<Option<ProjectSessionSummary>, Failure> {
    if let Some(target) = resume_id {
        let summary = resolve_session_by_id(engine, project, target)?;
        if !json_output {
            eprintln!(
                "\x1b[90m  ↻ Resuming session {} ({} turns): {}\x1b[0m",
                summary.session_id.0, summary.turn_count, summary.title
            );
        }
        return Ok(Some(summary));
    }
    if continue_session {
        let Some(summary) = list_project_chat_sessions(engine, project)
            .into_iter()
            .next()
        else {
            return Err(Failure::fixed(
                "session_not_found",
                "当前工作目录尚无历史对话可继续；请去掉 --continue (-c) 开启新会话",
                false,
            ));
        };
        if !json_output {
            eprintln!(
                "\x1b[90m  ↻ Continuing session {} ({} turns): {}\x1b[0m",
                summary.session_id.0, summary.turn_count, summary.title
            );
        }
        return Ok(Some(summary));
    }
    Ok(None)
}

async fn open_workbench_context(
    engine: &Arc<Engine>,
    target_dir: PathBuf,
    requested_route: Option<&str>,
    model: Option<&str>,
    continue_session: bool,
    resume_id: Option<&str>,
    json_output: bool,
) -> Result<(Project, Route, Option<ActiveChatSession>), Failure> {
    let resuming = continue_session || resume_id.is_some();
    let startup_model = if resuming { None } else { model };
    let startup_route = ensure_route_ready(engine, requested_route, startup_model).await?;
    let project = resolve_workbench_project(engine, target_dir).await?;
    // As on a fresh launch, project/network grants follow the selected startup route.
    // Restoring history may change the effective conversation route but cannot grant it.
    ensure_project_route_ready(engine, &project, &startup_route.id).await?;
    let session =
        resolve_initial_session(engine, &project, continue_session, resume_id, json_output)?;
    let route = if let Some(summary) = &session {
        let effective = summary.effective_route(engine)?;
        if model.is_some() {
            ensure_route_ready(engine, Some(&effective.id), model).await?
        } else {
            effective
        }
    } else {
        startup_route
    };
    Ok((
        project,
        route,
        session.map(ProjectSessionSummary::into_active),
    ))
}

fn is_skipped_context_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        ".git"
            | ".ssh"
            | ".aws"
            | "node_modules"
            | "target"
            | ".cargo-target"
            | ".credentials.yaml"
            | "coda.sqlite3"
            | "coda.sqlite3-wal"
            | "coda.sqlite3-shm"
    ) || lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
}

fn extract_mention_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut prev_char: Option<char> = None;
    for (idx, ch) in text.char_indices() {
        if ch == '@' && prev_char.is_none_or(|p| !p.is_alphanumeric() && p != '_') {
            let after = &text[idx + ch.len_utf8()..];
            let end_rel = after
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            ')' | ']'
                                | '}'
                                | '>'
                                | '"'
                                | '\''
                                | '`'
                                | ','
                                | ';'
                                | '!'
                                | '?'
                                | '<'
                                | '('
                                | '['
                                | '{'
                        )
                })
                .unwrap_or(after.len());
            let mut candidate = &after[..end_rel];
            while candidate.len() > 1 && candidate.ends_with('.') && !candidate.ends_with("..") {
                candidate = &candidate[..candidate.len() - 1];
            }
            if !candidate.is_empty() {
                tokens.push(candidate.to_owned());
            }
        }
        prev_char = Some(ch);
    }
    tokens
}

struct ParsedMention {
    raw_path: String,
    range: Option<(usize, usize)>,
    explicit_dir: bool,
}

fn parse_mention_token(token: &str) -> Option<ParsedMention> {
    if let Some((path_part, range_part)) = token.rsplit_once(':') {
        if path_part.is_empty() {
            return None;
        }
        let range = if let Some((s_str, e_str)) = range_part.split_once('-') {
            let start = s_str.parse::<usize>().ok()?;
            let end = e_str.parse::<usize>().ok()?;
            if start == 0 || start > end {
                return None;
            }
            (start, end)
        } else {
            let single = range_part.parse::<usize>().ok()?;
            if single == 0 {
                return None;
            }
            (single, single)
        };
        let explicit_dir = path_part.ends_with('/') || path_part.ends_with('\\');
        Some(ParsedMention {
            raw_path: path_part.to_owned(),
            range: Some(range),
            explicit_dir,
        })
    } else {
        let explicit_dir = token.ends_with('/') || token.ends_with('\\');
        Some(ParsedMention {
            raw_path: token.to_owned(),
            range: None,
            explicit_dir,
        })
    }
}

fn expand_context_mentions(project_root: &Path, raw_prompt: &str, json_output: bool) -> String {
    let tokens = extract_mention_tokens(raw_prompt);
    if tokens.is_empty() {
        return raw_prompt.to_owned();
    }

    let mut seen: HashSet<(PathBuf, Option<(usize, usize)>)> = HashSet::new();
    let mut blocks = String::new();
    let base_overhead = raw_prompt.len() + 64;

    for token in tokens {
        let Some(ParsedMention {
            raw_path,
            range,
            explicit_dir,
        }) = parse_mention_token(&token)
        else {
            continue;
        };
        let slash_path = raw_path.replace('\\', "/");
        let (resolved, display_rel) = if slash_path == "." || slash_path == "./" {
            if range.is_some()
                || coda::workspace_identity::verify_directory_policy(project_root).is_err()
            {
                continue;
            }
            let Ok(canon) = project_root.canonicalize() else {
                continue;
            };
            (canon, ".".to_owned())
        } else {
            let trimmed = slash_path.strip_prefix("./").unwrap_or(&slash_path);
            let clean_rel = trimmed.trim_end_matches('/');
            if clean_rel.is_empty() {
                continue;
            }
            let Ok(path) = coda::workspace::resolve(project_root, clean_rel, false, &[]) else {
                continue;
            };
            (path, clean_rel.to_owned())
        };

        let Ok(meta) = std::fs::symlink_metadata(&resolved) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if !seen.insert((resolved.clone(), range)) {
            continue;
        }

        if meta.is_dir() {
            if range.is_some() {
                continue;
            }
            let Ok(read_dir) = std::fs::read_dir(&resolved) else {
                continue;
            };
            let mut entries = Vec::new();
            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_skipped_context_entry(&name) {
                    continue;
                }
                let Ok(ft) = entry.file_type() else {
                    continue;
                };
                if ft.is_symlink() {
                    continue;
                }
                entries.push(if ft.is_dir() {
                    format!("{name}/")
                } else {
                    name
                });
            }
            entries.sort();
            if entries.len() > MENTION_MAX_DIR_ENTRIES {
                entries.truncate(MENTION_MAX_DIR_ENTRIES);
                entries.push("... [truncated]".to_owned());
            }
            let dir_label = if display_rel == "." {
                "./".to_owned()
            } else {
                format!("{display_rel}/")
            };
            let listing = entries.join("\n");
            let block = if listing.is_empty() {
                format!("<directory path=\"{dir_label}\">\n(empty)\n</directory>\n")
            } else {
                format!("<directory path=\"{dir_label}\">\n{listing}\n</directory>\n")
            };
            if base_overhead + blocks.len() + block.len() > MENTION_MAX_PROMPT_BYTES {
                break;
            }
            blocks.push_str(&block);
            if !json_output {
                eprintln!("\x1b[90m  📎 Attached {dir_label}\x1b[0m");
            }
        } else if meta.is_file() {
            if explicit_dir || meta.len() > 4 * 1024 * 1024 {
                continue;
            }
            let Ok(bytes) = std::fs::read(&resolved) else {
                continue;
            };
            let Ok(content) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let lines: Vec<&str> = content.lines().collect();
            let (start_idx, end_excl, label, header) = match range {
                Some((start, end)) => {
                    let s = start.saturating_sub(1);
                    let e = end.min(lines.len());
                    (
                        s,
                        e,
                        format!("{display_rel}:{start}-{end}"),
                        format!("<file path=\"{display_rel}\" lines=\"{start}-{end}\">\n"),
                    )
                }
                None => (
                    0,
                    lines.len(),
                    display_rel.clone(),
                    format!("<file path=\"{display_rel}\">\n"),
                ),
            };

            let remaining_prompt = MENTION_MAX_PROMPT_BYTES
                .saturating_sub(base_overhead + blocks.len() + header.len() + 64);
            if remaining_prompt < 64 {
                break;
            }
            let file_budget = MENTION_MAX_FILE_BYTES.min(remaining_prompt);

            let mut numbered = String::new();
            let mut truncated = false;
            if start_idx < end_excl && start_idx < lines.len() {
                for (offset, line) in lines[start_idx..end_excl].iter().enumerate() {
                    let line_no = start_idx + offset + 1;
                    let formatted = format!("{line_no}: {line}\n");
                    if numbered.len() + formatted.len() > file_budget {
                        truncated = true;
                        break;
                    }
                    numbered.push_str(&formatted);
                }
            }
            if truncated {
                numbered.push_str("... [truncated at 64 KB] ...\n");
            }

            blocks.push_str(&header);
            blocks.push_str(&numbered);
            blocks.push_str("</file>\n");
            if !json_output {
                eprintln!("\x1b[90m  📎 Attached {label}\x1b[0m");
            }
        }
    }

    if blocks.is_empty() {
        raw_prompt.to_owned()
    } else {
        format!("{raw_prompt}\n\n<context_files>\n{blocks}</context_files>")
    }
}

fn summarize_tool_start(engine: &Engine, task_id: &str, name: &str, data: &Value) -> String {
    let args_val = if data["arguments"].is_object() {
        Some(data["arguments"].clone())
    } else if let Some(call_id) = data["tool_call_id"].as_str()
        && let Ok(task) = engine.store.task(task_id)
    {
        task.messages
            .iter()
            .filter_map(|m| m["tool_calls"].as_array())
            .flatten()
            .find(|c| c["id"].as_str() == Some(call_id))
            .and_then(|c| c["function"]["arguments"].as_str())
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
    } else {
        data["preview"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
    };
    if let Some(parsed) = args_val {
        match name {
            "read_file" => {
                if let Some(p) = parsed["path"].as_str() {
                    let start = parsed.get("start_line").and_then(Value::as_u64);
                    let end = parsed.get("end_line").and_then(Value::as_u64);
                    return match (start, end) {
                        (Some(s), Some(e)) => format!("{p}:{s}-{e}"),
                        (Some(s), None) => format!("{p}:{s}-"),
                        (None, Some(e)) => format!("{p}:1-{e}"),
                        (None, None) => p.to_owned(),
                    };
                }
            }
            "write_file" | "edit_file" | "list_files" => {
                if let Some(p) = parsed["path"].as_str() {
                    return p.to_owned();
                }
            }
            "glob_files" => {
                if let Some(pat) = parsed["pattern"].as_str() {
                    if let Some(sub) = parsed["path"]
                        .as_str()
                        .filter(|p| !p.is_empty() && *p != ".")
                    {
                        return format!("{pat} (in {sub})");
                    }
                    return pat.to_owned();
                }
            }
            "search_files" => {
                if let Some(q) = parsed["query"].as_str() {
                    if let Some(glob) = parsed["file_pattern"].as_str().filter(|g| !g.is_empty()) {
                        return format!("{q:?} ({glob})");
                    }
                    return format!("{q:?}");
                }
            }
            "run_command" => {
                if let Some(cmd) = parsed["command"].as_str() {
                    let one_line = cmd.trim().replace('\n', "; ");
                    return one_line.chars().take(90).collect();
                }
            }
            "git_status" => {
                if let Some(p) = parsed["path"]
                    .as_str()
                    .filter(|p| !p.is_empty() && *p != ".")
                {
                    return p.to_owned();
                }
                return String::new();
            }
            "todo_write" => {
                if let Some(items) = parsed
                    .get("todos")
                    .or_else(|| parsed.get("items"))
                    .and_then(Value::as_array)
                {
                    let total = items.len();
                    let completed = items
                        .iter()
                        .filter(|i| i["status"].as_str() == Some("completed"))
                        .count();
                    let in_progress = items
                        .iter()
                        .filter(|i| i["status"].as_str() == Some("in_progress"))
                        .count();
                    if let Some(active_item) = items
                        .iter()
                        .find(|i| i["status"].as_str() == Some("in_progress"))
                        .and_then(|i| {
                            i["content"]
                                .as_str()
                                .or_else(|| i["title"].as_str())
                                .or_else(|| i["task"].as_str())
                        })
                    {
                        let short: String = active_item.trim().chars().take(48).collect();
                        return format!("{completed}/{total} done · active: {short}");
                    }
                    return format!(
                        "{total} items ({completed} completed, {in_progress} in_progress)"
                    );
                }
            }
            other if other.starts_with("mcp__") => {
                if let Some(obj) = parsed.as_object() {
                    if obj.is_empty() {
                        return String::new();
                    }
                    let compact = Value::Object(obj.clone()).to_string();
                    return compact.chars().take(80).collect();
                }
            }
            _ => {}
        }
    }
    data["preview"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect()
}

fn render_ansi_unified_diff(diff_text: &str) {
    let mut stderr = io::stderr().lock();
    let max_lines = 120usize;
    let lines: Vec<&str> = diff_text.lines().collect();
    for line in lines.iter().take(max_lines) {
        if line.starts_with("+++") || line.starts_with("---") {
            let _ = writeln!(stderr, "  \x1b[1m{line}\x1b[0m");
        } else if line.starts_with("@@") {
            let _ = writeln!(stderr, "  \x1b[36m{line}\x1b[0m");
        } else if line.starts_with('+') {
            let _ = writeln!(stderr, "  \x1b[32m{line}\x1b[0m");
        } else if line.starts_with('-') {
            let _ = writeln!(stderr, "  \x1b[31m{line}\x1b[0m");
        } else {
            let _ = writeln!(stderr, "  \x1b[90m{line}\x1b[0m");
        }
    }
    if lines.len() > max_lines {
        let _ = writeln!(
            stderr,
            "  \x1b[90m... ({} more diff lines)\x1b[0m",
            lines.len() - max_lines
        );
    }
    let _ = stderr.flush();
}

fn display_approval_preview(engine: &Engine, record: &coda::approval::ApprovalRecord) {
    let Ok(task) = engine.store.task(&record.task_id) else {
        eprintln!(
            "\n\x1b[33m[approval]\x1b[0m {} {}",
            record.tool_name, record.preview
        );
        return;
    };

    let raw_args = task
        .messages
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .find(|c| c["id"].as_str() == Some(record.tool_call_id.as_str()))
        .and_then(|c| c["function"]["arguments"].as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .or_else(|| serde_json::from_str::<Value>(&record.preview).ok());

    if matches!(record.tool_name.as_str(), "write_file" | "edit_file")
        && let Some(args) = &raw_args
        && let Ok((_, before_bytes, after_bytes, rel_path)) = coda::workspace::preview_file_mutation(
            Path::new(&task.workspace),
            &record.tool_name,
            args,
            &task.spec.write_scopes,
        )
    {
        let before_str = before_bytes
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("");
        let after_str = std::str::from_utf8(&after_bytes).unwrap_or("");
        eprintln!(
            "\n\x1b[1;33m● {}\x1b[0m \x1b[1m{}\x1b[0m",
            record.tool_name, rel_path
        );
        let patch = coda::git_diff::unified_diff(&rel_path, before_str, after_str, 3);
        if !patch.trim().is_empty() {
            render_ansi_unified_diff(&patch);
        }
        return;
    }

    if record.tool_name == "run_command" {
        let cmd = raw_args
            .as_ref()
            .and_then(|a| a["command"].as_str())
            .unwrap_or(&record.preview);
        let shell = if cfg!(windows) { "PowerShell" } else { "sh" };
        eprintln!("\n\x1b[1;33m● run_command\x1b[0m ({shell}):\n  \x1b[1m{cmd}\x1b[0m");
        return;
    }

    eprintln!(
        "\n\x1b[1;33m● {}\x1b[0m {}",
        record.tool_name, record.preview
    );
}

struct AutoApproval {
    process_default: bool,
    session_override: Option<bool>,
}

impl AutoApproval {
    fn new(process_default: bool) -> Self {
        Self {
            process_default,
            session_override: None,
        }
    }

    fn enabled(&self) -> bool {
        self.session_override.unwrap_or(self.process_default)
    }

    fn status(&self) -> &'static str {
        match (self.session_override, self.process_default) {
            (Some(true), _) => "ON (session)",
            (Some(false), _) => "OFF (session)",
            (None, true) => "ON (process -y)",
            (None, false) => "OFF",
        }
    }

    fn reset_session(&mut self) {
        self.session_override = None;
    }
}

async fn prompt_inline_approval(auto_approve: &mut AutoApproval) -> bool {
    if !io::stdin().is_terminal() {
        eprintln!("非交互终端且当前自动批准已关闭，自动拒绝待审批操作。");
        return false;
    }
    eprint!("\x1b[1;36mApprove? [y]es / [n]o / [a]llow-session:\x1b[0m ");
    let _ = io::stderr().flush();
    let answer = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
        line
    })
    .await
    .unwrap_or_default();

    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "a" | "allow" | "allow-session" | "all" => {
            auto_approve.session_override = Some(true);
            eprintln!("\x1b[32m✓ 已开启当前会话自动批准 (输入 /auto 可随时切换)\x1b[0m");
            true
        }
        _ => false,
    }
}

async fn execute_turn_and_stream(
    engine: &Arc<Engine>,
    project: &Project,
    route: &Route,
    session_state: &mut Option<ActiveChatSession>,
    prompt: &str,
    auto_approve: &mut AutoApproval,
    json_output: bool,
) -> Result<Task, Failure> {
    let enriched_prompt =
        expand_context_mentions(Path::new(&project.root_path), prompt, json_output);

    let (task_id, run_id, mut cursor) = if let Some(active) = session_state.as_mut() {
        match engine
            .send_chat_turn(SendChatTurn {
                session_id: active.session_id.clone(),
                agent_id: active.agent_id.clone(),
                expected_last_turn_id: active.last_turn_id.clone(),
                message: enriched_prompt.clone(),
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            })
            .await
        {
            Ok(receipt) => {
                active.last_turn_id = receipt.turn.id;
                active.turn_count += 1;
                active
                    .task_history
                    .push(receipt.task.legacy_task_id.clone());
                (
                    receipt.task.legacy_task_id,
                    active.run_id.clone(),
                    active.event_cursor,
                )
            }
            Err(err) => {
                if !json_output {
                    eprintln!("\x1b[33m  ! 无法继续原会话 ({err})，已开启新会话\x1b[0m");
                }
                auto_approve.reset_session();
                *session_state = None;
                start_fresh_session(
                    engine,
                    project,
                    route,
                    prompt,
                    &enriched_prompt,
                    session_state,
                )
                .await?
            }
        }
    } else {
        start_fresh_session(
            engine,
            project,
            route,
            prompt,
            &enriched_prompt,
            session_state,
        )
        .await?
    };

    let mut handled_approvals = HashSet::new();
    let mut mid_line = false;
    let mut in_reasoning = false;
    let mut reasoning_ended_newline = false;

    let drain_events = |cursor: &mut i64,
                        mid_line: &mut bool,
                        in_reasoning: &mut bool,
                        reasoning_ended_newline: &mut bool| {
        while let Ok(events) = engine.store.events(&run_id, *cursor) {
            if events.is_empty() {
                break;
            }
            let batch_len = events.len();
            let prev_cursor = *cursor;
            for ev in events {
                *cursor = ev.seq;
                if ev.task_id != task_id || json_output {
                    continue;
                }
                match ev.kind.as_str() {
                    "reasoning_delta" => {
                        if let Some(text) = ev.data["text"].as_str()
                            && !text.is_empty()
                        {
                            if !*in_reasoning {
                                if *mid_line {
                                    println!();
                                    *mid_line = false;
                                }
                                eprint!("\x1b[90m  💭 ");
                                *in_reasoning = true;
                            }
                            eprint!("\x1b[90m{text}\x1b[0m");
                            let _ = io::stderr().flush();
                            *reasoning_ended_newline = text.ends_with('\n');
                        }
                    }
                    "delta" => {
                        if let Some(text) = ev.data["text"].as_str()
                            && !text.is_empty()
                        {
                            finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                            print!("{text}");
                            let _ = io::stdout().flush();
                            *mid_line = !text.ends_with('\n');
                        }
                    }
                    "tool_start" | "tool.started" => {
                        finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                        if *mid_line {
                            println!();
                            *mid_line = false;
                        }
                        let name = ev.data["name"]
                            .as_str()
                            .or_else(|| ev.data["tool_name"].as_str())
                            .unwrap_or("tool");
                        let detail = summarize_tool_start(engine, &task_id, name, &ev.data);
                        let badge = if name.starts_with("mcp__") {
                            "mcp"
                        } else {
                            "tool"
                        };
                        if detail.is_empty() {
                            eprintln!("\x1b[90m  ▸ [{badge}] {name}\x1b[0m");
                        } else {
                            eprintln!("\x1b[90m  ▸ [{badge}] {name} {detail}\x1b[0m");
                        }
                    }
                    "tool_result" | "tool.finished" => {
                        let name = ev.data["name"]
                            .as_str()
                            .or_else(|| ev.data["tool_name"].as_str())
                            .unwrap_or("tool");
                        let res = &ev.data["result"];
                        if let Some(err) = res.get("error").and_then(Value::as_str) {
                            finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                            eprintln!("\x1b[33m    ↳ [{name}] error: {err}\x1b[0m");
                        } else if let Some(code) = res.get("exit_code").and_then(Value::as_i64)
                            && code != 0
                        {
                            finish_reasoning_line(in_reasoning, reasoning_ended_newline);
                            eprintln!("\x1b[33m    ↳ [{name}] exit_code={code}\x1b[0m");
                        }
                    }
                    _ => {}
                }
            }
            if *cursor <= prev_cursor || batch_len < 500 {
                break;
            }
        }
    };

    loop {
        drain_events(
            &mut cursor,
            &mut mid_line,
            &mut in_reasoning,
            &mut reasoning_ended_newline,
        );

        if let Ok(approvals) = engine.store.approvals_for_task(&task_id) {
            for record in approvals {
                if record.status == ApprovalStatus::Pending
                    && handled_approvals.insert(record.id.clone())
                {
                    if !json_output {
                        finish_reasoning_line(&mut in_reasoning, &mut reasoning_ended_newline);
                    }
                    if mid_line && !json_output {
                        println!();
                        mid_line = false;
                    }
                    if !json_output {
                        display_approval_preview(engine, &record);
                    }
                    let automatic = auto_approve.enabled();
                    let approve = if automatic {
                        if !json_output {
                            eprintln!("\x1b[90m  ✓ [auto-approved] {}\x1b[0m", record.tool_name);
                        }
                        true
                    } else if json_output {
                        false
                    } else {
                        prompt_inline_approval(auto_approve).await
                    };
                    let ack_net = approve && record.tool_name == "run_command";
                    let _ = engine.decide_approval_with_network_ack(
                        &record.id,
                        approve,
                        Some(if automatic { "cli-auto" } else { "user" }),
                        ack_net,
                    );
                }
            }
        }

        let task = engine
            .store
            .task(&task_id)
            .map_err(|e| Failure::custom("task_error", format!("读取任务状态失败: {e}")))?;

        if !matches!(task.status.as_str(), "queued" | "running") && !engine.is_busy() {
            drain_events(
                &mut cursor,
                &mut mid_line,
                &mut in_reasoning,
                &mut reasoning_ended_newline,
            );
            if !json_output {
                finish_reasoning_line(&mut in_reasoning, &mut reasoning_ended_newline);
            }
            if mid_line && !json_output {
                println!();
            }
            if !json_output && task.usage.is_object() {
                let prompt_tokens = task.usage["prompt_tokens"].as_u64();
                let completion_tokens = task.usage["completion_tokens"].as_u64();
                let total_tokens = task.usage["total_tokens"].as_u64();
                if prompt_tokens.is_some() || completion_tokens.is_some() || total_tokens.is_some()
                {
                    let p = prompt_tokens.unwrap_or(0);
                    let c = completion_tokens.unwrap_or(0);
                    let t = total_tokens.unwrap_or(p + c);
                    eprintln!("\x1b[90m  [tokens: prompt={p}, completion={c}, total={t}]\x1b[0m");
                }
            }
            if let Some(active) = session_state.as_mut() {
                active.event_cursor = cursor;
            } else {
                auto_approve.reset_session();
            }
            return Ok(task);
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(25)) => {}
            _ = tokio::signal::ctrl_c() => {
                if !json_output {
                    finish_reasoning_line(&mut in_reasoning, &mut reasoning_ended_newline);
                }
                if mid_line && !json_output {
                    println!();
                    mid_line = false;
                }
                if !json_output {
                    eprintln!("\x1b[33m^C 正在停止当前回合...\x1b[0m");
                }
                let _ = engine.cancel(&task_id);
            }
        }
    }
}

fn finish_reasoning_line(in_reasoning: &mut bool, reasoning_ended_newline: &mut bool) {
    if *in_reasoning {
        if !*reasoning_ended_newline {
            eprintln!();
        }
        *in_reasoning = false;
        *reasoning_ended_newline = false;
    }
}

async fn start_fresh_session(
    engine: &Arc<Engine>,
    project: &Project,
    route: &Route,
    display_prompt: &str,
    enriched_prompt: &str,
    session_state: &mut Option<ActiveChatSession>,
) -> Result<(String, String, i64), Failure> {
    let snap = engine
        .project_config(&project.id)
        .map_err(|e| Failure::custom("project_error", e.to_string()))?;
    let first_line = display_prompt.lines().next().unwrap_or(display_prompt);
    let title: String = first_line.trim().chars().take(60).collect();
    let run = engine
        .start_in_project(StartInProject {
            project_id: project.id.clone(),
            expected_config_revision: snap.revision,
            request: RunRequest {
                title: if title.is_empty() {
                    "coding session".into()
                } else {
                    title
                },
                kind: SessionKind::Chat,
                tasks: vec![TaskSpec {
                    name: "coder".into(),
                    role: "autonomous coding agent".into(),
                    route_id: route.id.clone(),
                    prompt: enriched_prompt.to_owned(),
                    depends_on: vec![],
                    write_scopes: vec!["*".into()],
                    tools: true,
                    allow_commands: true,
                    max_rounds: WORKBENCH_MAX_ROUNDS,
                }],
            },
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        })
        .await
        .map_err(|e| Failure::custom("start_error", format!("启动会话失败: {e}")))?;

    let task_id = run
        .tasks
        .first()
        .map(|t| t.id.clone())
        .ok_or_else(|| Failure::fixed("start_error", "新运行未包含任务", false))?;

    if let Ok(ctx) = engine.store.run_context(&run.id)
        && let Some(first_task) = ctx.tasks.first()
    {
        *session_state = Some(ActiveChatSession {
            session_id: ctx.session.id,
            agent_id: first_task.agent_id.clone(),
            last_turn_id: ctx.latest_turn.id,
            run_id: run.id.clone(),
            event_cursor: 0,
            turn_count: 1,
            task_history: vec![task_id.clone()],
        });
    }

    Ok((task_id, run.id, 0))
}

fn run_quick_git(workspace: &Path, args: &[&str]) -> Option<String> {
    let git_exe = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .filter(|d| d.is_absolute())
            .map(|d| d.join(if cfg!(windows) { "git.exe" } else { "git" }))
            .find(|c| c.is_file() && !c.starts_with(workspace))
    })?;
    let mut cmd = std::process::Command::new(git_exe);
    cmd.current_dir(workspace)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["--no-pager"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
}

fn read_repl_input<R: BufRead, F: FnMut()>(
    reader: &mut R,
    mut on_continuation: F,
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    loop {
        let mut buf = String::new();
        match reader.read_line(&mut buf) {
            Ok(0) => {
                if lines.is_empty() {
                    return None;
                }
                break;
            }
            Ok(_) => {
                let trimmed_eol = buf.trim_end_matches(['\r', '\n']);
                if let Some(without_slash) = trimmed_eol.strip_suffix('\\') {
                    lines.push(without_slash.to_owned());
                    on_continuation();
                } else {
                    lines.push(trimmed_eol.to_owned());
                    break;
                }
            }
            Err(_) => {
                return if lines.is_empty() {
                    None
                } else {
                    Some(lines.join("\n"))
                };
            }
        }
    }
    Some(lines.join("\n"))
}

fn print_repl_help() {
    println!(
        "\x1b[1mREPL Slash Commands:\x1b[0m\n\
         \x20 \x1b[36m/sessions\x1b[0m             List recent chat sessions in this workspace\n\
         \x20 \x1b[36m/resume <id>\x1b[0m          Switch active context to continue a previous session\n\
         \x20 \x1b[36m/todos\x1b[0m                Show structured task checklist ([✓] / [▸] / [ ]) for the active session\n\
         \x20 \x1b[36m/checkpoint\x1b[0m           Create a durable checkpoint on the latest modified turn\n\
         \x20 \x1b[36m/checkpoints\x1b[0m          List durable checkpoints for the active session (or project)\n\
         \x20 \x1b[36m/restore <ckpt_id>\x1b[0m    Restore workspace files from a durable checkpoint\n\
         \x20 \x1b[36m/compact\x1b[0m              Compact message history and show tokens/KB saved\n\
         \x20 \x1b[36m/diff\x1b[0m                 Show workspace Git status/diff summary & recorded task changes\n\
         \x20 \x1b[36m/undo\x1b[0m                 Restore workspace file edits from the most recent modified turn\n\
         \x20 \x1b[36m/status\x1b[0m               Show active workspace, route, model, session turns & token usage\n\
         \x20 \x1b[36m/routes\x1b[0m               List configured provider routes (active marked with *)\n\
         \x20 \x1b[36m/route [id]\x1b[0m           Show routes or switch active route (resets session if changed)\n\
         \x20 \x1b[36m/model <name>\x1b[0m         Switch active route model seamlessly while keeping session context\n\
         \x20 \x1b[36m/auto\x1b[0m                 Toggle automatic approval for this session (switching restores the process default)\n\
         \x20 \x1b[36m/new\x1b[0m                  Start a fresh conversation session\n\
         \x20 \x1b[36m/help\x1b[0m                 Show this command reference\n\
         \x20 \x1b[36m/exit\x1b[0m                 Cleanly shut down and exit Coda CLI\n\
         \x1b[90mTip: Reference files with @path, @path:start-end, or @dir/; end a line with \\ for multiline input.\x1b[0m"
    );
}

async fn run_chat(
    instance_lock: &mut Option<std::fs::File>,
    top_data_dir: Option<PathBuf>,
    top_workspace: Option<PathBuf>,
    json_output: bool,
    args: ChatArgs,
) -> Result<(), Failure> {
    let data_dir = resolve_user_data_dir(args.data_dir.or(top_data_dir));
    let target_dir = args
        .dir
        .or(top_workspace)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let engine = open_local_engine(instance_lock, &data_dir)?;
    let result = async {
        let (project, mut route, mut session_state) = open_workbench_context(
            &engine, target_dir, args.route.as_deref(), args.model.as_deref(),
            args.continue_session, args.resume.as_deref(), json_output,
        ).await?;
        let mut auto_approve = AutoApproval::new(args.auto_approve);

        if !json_output {
            println!(
                "\x1b[1;35m⚡ Coda CLI\x1b[0m · model=\x1b[1m{}\x1b[0m · route=\x1b[36m{}\x1b[0m · auto-approve={}\n\
                 \x1b[90mWorkspace: {}  (type /help for commands, /exit to quit)\x1b[0m",
                route.model,
                route.id,
                auto_approve.status(),
                project.root_path
            );
        }

        let initial = args.prompt.join(" ");
        if !initial.trim().is_empty() {
            let task = execute_turn_and_stream(
                &engine,
                &project,
                &route,
                &mut session_state,
                initial.trim(),
                &mut auto_approve,
                json_output,
            )
            .await?;
            if task.status != "completed"
                && let Some(err) = &task.error
            {
                eprintln!("\x1b[31m[{}] {}\x1b[0m", task.status, err);
            }
        }

        loop {
            if !json_output {
                print!("\n\x1b[1;32mcoda>\x1b[0m ");
                let _ = io::stdout().flush();
            }
            let maybe_line = tokio::task::spawn_blocking(move || {
                let stdin = io::stdin();
                let mut lock = stdin.lock();
                read_repl_input(&mut lock, || {
                    if !json_output {
                        print!("... ");
                        let _ = io::stdout().flush();
                    }
                })
            })
            .await
            .unwrap_or(None);

            let Some(raw_line) = maybe_line else {
                break;
            };
            let input = raw_line.trim();
            if input.is_empty() {
                continue;
            }

            if !input.contains('\n')
                && let Some(cmd_rest) = input.strip_prefix('/')
            {
                let (slash_cmd, arg_str) = cmd_rest
                    .split_once(char::is_whitespace)
                    .map(|(c, a)| (c, a.trim()))
                    .unwrap_or((cmd_rest, ""));
                match slash_cmd {
                    "exit" | "quit" | "q" => break,
                    "help" | "h" | "?" => print_repl_help(),
                    "auto" => {
                        auto_approve.session_override = Some(!auto_approve.enabled());
                        println!("Auto-approve is now {}", auto_approve.status());
                    }
                    "new" | "clear" => {
                        auto_approve.reset_session();
                        session_state = None;
                        println!("\x1b[32m✓ 已重置上下文；下一条消息将开启全新会话。\x1b[0m");
                    }
                    "sessions" => {
                        let sessions = list_project_chat_sessions(&engine, &project);
                        if sessions.is_empty() {
                            println!("当前项目暂无历史会话记录。");
                        } else {
                            let active_id = session_state.as_ref().map(|a| a.session_id.0.as_str());
                            println!("\x1b[1mRecent Sessions ({}):\x1b[0m", project.name);
                            for item in sessions.iter().take(20) {
                                let marker = if active_id == Some(item.session_id.0.as_str()) {
                                    "\x1b[32m*\x1b[0m"
                                } else {
                                    " "
                                };
                                println!(
                                    " {marker} \x1b[36m{}\x1b[0m  [{}]  ({} turn{})  {}",
                                    item.session_id.0,
                                    item.latest_status,
                                    item.turn_count,
                                    if item.turn_count == 1 { "" } else { "s" },
                                    item.title
                                );
                            }
                        }
                    }
                    "resume" => {
                        if arg_str.is_empty() {
                            println!("用法: /resume <session_id_or_run_id> (输入 /sessions 查看列表)");
                        } else {
                            match resolve_session_by_id(&engine, &project, arg_str).and_then(|summary| {
                                let effective = summary.effective_route(&engine)?;
                                Ok((summary, effective))
                            }) {
                                Ok((summary, effective)) => {
                                    route = effective;
                                    println!(
                                        "\x1b[32m✓ 已切换到会话 {} ({} turns, status={}): {}\x1b[0m",
                                        summary.session_id.0,
                                        summary.turn_count,
                                        summary.latest_status,
                                        summary.title
                                    );
                                    if session_state.as_ref().map(|s| &s.session_id) != Some(&summary.session_id) {
                                        auto_approve.reset_session();
                                    }
                                    session_state = Some(summary.into_active());
                                }
                                Err(err) => err.print(false),
                            }
                        }
                    }
                    "todos" | "todo" => {
                        let latest_tid = session_state
                            .as_ref()
                            .and_then(|active| active.task_history.last())
                            .cloned();
                        match latest_tid {
                            Some(tid) => match engine.store.task(&tid) {
                                Ok(task) => {
                                    let items = coda::workspace::todos_for_task(&task);
                                    if items.is_empty() {
                                        println!("当前会话暂无 Todo 任务项。");
                                    } else {
                                        let completed = items
                                            .iter()
                                            .filter(|i| i.status == "completed")
                                            .count();
                                        let in_progress = items
                                            .iter()
                                            .filter(|i| i.status == "in_progress")
                                            .count();
                                        let pending = items
                                            .iter()
                                            .filter(|i| i.status == "pending")
                                            .count();
                                        println!(
                                            "\x1b[1mSession Todos\x1b[0m ({} total: \x1b[32m{} completed\x1b[0m, \x1b[36m{} in_progress\x1b[0m, {} pending):",
                                            items.len(),
                                            completed,
                                            in_progress,
                                            pending
                                        );
                                        for item in &items {
                                            let badge = match item.status.as_str() {
                                                "completed" => "\x1b[32m[✓]\x1b[0m",
                                                "in_progress" => "\x1b[36m[▸]\x1b[0m",
                                                _ => "\x1b[90m[ ]\x1b[0m",
                                            };
                                            println!(
                                                "  {badge} {} \x1b[90m({})\x1b[0m",
                                                item.content, item.id
                                            );
                                        }
                                    }
                                }
                                Err(err) => eprintln!("\x1b[31m读取会话任务失败: {err}\x1b[0m"),
                            },
                            None => println!("当前没有活跃会话可查看 Todo 清单。"),
                        }
                    }
                    "checkpoint" => {
                        let target_task = session_state.as_ref().and_then(|active| {
                            active
                                .task_history
                                .iter()
                                .rev()
                                .find(|tid| {
                                    engine
                                        .changes(tid)
                                        .map(|c| !c.is_empty())
                                        .unwrap_or(false)
                                })
                                .or_else(|| active.task_history.last())
                                .cloned()
                        });
                        match target_task {
                            Some(tid) => {
                                let key = format!("cli-ckpt-{tid}");
                                match engine.create_checkpoint(&tid, &key).await {
                                    Ok(created) => {
                                        println!(
                                            "\x1b[32m✓ Checkpoint created:\x1b[0m \x1b[1m{}\x1b[0m (task={}, files={}, replayed={})",
                                            created.checkpoint.checkpoint_id,
                                            created.checkpoint.task_id,
                                            created.checkpoint.entries.len(),
                                            created.replayed
                                        );
                                        for entry in &created.checkpoint.entries {
                                            println!("  - {}", entry.path);
                                        }
                                    }
                                    Err(err) => {
                                        eprintln!("\x1b[31m创建检查点失败: {err}\x1b[0m");
                                    }
                                }
                            }
                            None => println!("当前会话尚无已完成任务可创建检查点。"),
                        }
                    }
                    "checkpoints" => {
                        let task_ids: Vec<String> = if let Some(active) = &session_state {
                            active.task_history.clone()
                        } else {
                            list_project_chat_sessions(&engine, &project)
                                .into_iter()
                                .flat_map(|s| s.task_history)
                                .collect()
                        };
                        if task_ids.is_empty() {
                            println!("当前项目暂无可用检查点记录。");
                        } else {
                            match engine.checkpoints_for_tasks(&task_ids) {
                                Ok(checkpoints) if checkpoints.is_empty() => {
                                    println!(
                                        "当前会话暂无已创建的检查点（输入 /checkpoint 创建检查点）。"
                                    );
                                }
                                Ok(checkpoints) => {
                                    println!(
                                        "\x1b[1mCheckpoints ({}):\x1b[0m",
                                        checkpoints.len()
                                    );
                                    for ckpt in checkpoints {
                                        let paths: Vec<&str> =
                                            ckpt.entries.iter().map(|e| e.path.as_str()).collect();
                                        let paths_summary = if paths.is_empty() {
                                            "(no file entries)".to_owned()
                                        } else {
                                            paths.join(", ")
                                        };
                                        println!(
                                            "  \x1b[36m{}\x1b[0m  task={}  ({} file{})  {}",
                                            ckpt.checkpoint_id,
                                            ckpt.task_id,
                                            ckpt.entries.len(),
                                            if ckpt.entries.len() == 1 { "" } else { "s" },
                                            paths_summary
                                        );
                                    }
                                }
                                Err(err) => {
                                    eprintln!("\x1b[31m查询检查点列表失败: {err}\x1b[0m");
                                }
                            }
                        }
                    }
                    "restore" => {
                        if arg_str.is_empty() {
                            println!("用法: /restore <checkpoint_id>");
                        } else {
                            match engine.restore_checkpoint(arg_str).await {
                                Ok(receipt) => {
                                    println!(
                                        "\x1b[32m✓ 已恢复检查点 {arg_str} ({} 处文件, status={:?}):\x1b[0m",
                                        receipt.restored, receipt.status
                                    );
                                    for item in &receipt.outcomes {
                                        println!("  - {} ({:?})", item.path, item.status);
                                    }
                                }
                                Err(err) => {
                                    eprintln!("\x1b[31m恢复检查点失败: {err}\x1b[0m");
                                }
                            }
                        }
                    }
                    "compact" => {
                        let latest_tid = session_state
                            .as_ref()
                            .and_then(|active| active.task_history.last())
                            .cloned();
                        match latest_tid {
                            Some(tid) => match engine.store.task(&tid) {
                                Ok(mut task) => {
                                    let before_count = task.messages.len();
                                    let before_bytes = serde_json::to_vec(&task.messages)
                                        .map(|b| b.len())
                                        .unwrap_or(0);
                                    let compacted =
                                        coda::engine::compact_messages(&task.messages);
                                    let after_count = compacted.len();
                                    let after_bytes = serde_json::to_vec(&compacted)
                                        .map(|b| b.len())
                                        .unwrap_or(0);
                                    let saved_kb =
                                        before_bytes.saturating_sub(after_bytes) as f64 / 1024.0;
                                    if compacted != task.messages {
                                        task.messages = compacted;
                                        task.updated_at = coda::domain::now();
                                        let _ = engine.store.save_task(&task);
                                        println!(
                                            "\x1b[32m✓ Context compacted:\x1b[0m {before_count} -> {after_count} messages ({saved_kb:.1} KB saved)"
                                        );
                                    } else {
                                        println!(
                                            "\x1b[90m✓ Context already compact:\x1b[0m {before_count} -> {after_count} messages ({saved_kb:.1} KB saved)"
                                        );
                                    }
                                }
                                Err(err) => eprintln!("\x1b[31m读取任务消息失败: {err}\x1b[0m"),
                            },
                            None => println!("当前没有活跃会话可压缩。"),
                        }
                    }
                    "routes" | "route" => {
                        if arg_str.is_empty() {
                            match engine.store.settings() {
                                Ok(Some(settings)) if !settings.routes.is_empty() => {
                                    println!("\x1b[1mConfigured Routes:\x1b[0m");
                                    for item in &settings.routes {
                                        let marker = if item.id == route.id {
                                            "\x1b[32m*\x1b[0m"
                                        } else {
                                            " "
                                        };
                                        println!(
                                            " {marker} \x1b[36m{}\x1b[0m  model=\x1b[1m{}\x1b[0m  ({})",
                                            item.id, item.model, item.base_url
                                        );
                                    }
                                }
                                Ok(_) => println!("当前尚未配置任何模型路由。"),
                                Err(err) => eprintln!("\x1b[31m读取路由配置失败: {err}\x1b[0m"),
                            }
                        } else if !host_route_id_valid(arg_str) {
                            eprintln!("无效的路由 ID `{arg_str}`。");
                        } else {
                            match ensure_route_ready(&engine, Some(arg_str), None).await {
                                Ok(selected) => {
                                    let changed = selected.id != route.id;
                                    match ensure_project_route_ready(
                                        &engine,
                                        &project,
                                        &selected.id,
                                    )
                                    .await
                                    {
                                        Ok(_) => {
                                            route = selected;
                                            if changed {
                                                auto_approve.reset_session();
                                                session_state = None;
                                                println!(
                                                    "\x1b[32m✓ 已切换到路由 `{}` (model=`{}`, base=`{}`)；已重置当前会话上下文。\x1b[0m",
                                                    route.id, route.model, route.base_url
                                                );
                                            } else {
                                                println!(
                                                    "\x1b[32m✓ 当前已在路由 `{}` (model=`{}`, base=`{}`)\x1b[0m",
                                                    route.id, route.model, route.base_url
                                                );
                                            }
                                        }
                                        Err(err) => err.print(false),
                                    }
                                }
                                Err(err) => err.print(false),
                            }
                        }
                    }
                    "model" => {
                        if arg_str.is_empty() {
                            println!("当前路由 `{}` 使用模型: \x1b[1m{}\x1b[0m", route.id, route.model);
                        } else {
                            match ensure_route_ready(&engine, Some(&route.id), Some(arg_str)).await
                            {
                                Ok(updated) => {
                                    route = updated;
                                    let _ = ensure_project_route_ready(
                                        &engine,
                                        &project,
                                        &route.id,
                                    )
                                    .await;
                                    if session_state.is_some() {
                                        println!(
                                            "\x1b[32m✓ 已切换模型为 `{}` (保留当前会话上下文，下一轮直接生效)\x1b[0m",
                                            route.model
                                        );
                                    } else {
                                        println!(
                                            "\x1b[32m✓ 已切换模型为 `{}`\x1b[0m",
                                            route.model
                                        );
                                    }
                                }
                                Err(err) => err.print(false),
                            }
                        }
                    }
                    "status" => {
                        println!("Workspace   : {}", project.root_path);
                        println!("Project ID  : {}", project.id.0);
                        println!("Route/Model : {} / {}", route.id, route.model);
                        println!("Auto-Approve: {}", auto_approve.status());
                        if let Some(active) = &session_state {
                            let mut total_tokens = 0u64;
                            for tid in &active.task_history {
                                if let Ok(t) = engine.store.task(tid) {
                                    total_tokens +=
                                        t.usage["total_tokens"].as_u64().unwrap_or(0);
                                }
                            }
                            println!(
                                "Session     : {} ({} turns, {} total tokens)",
                                active.session_id.0, active.turn_count, total_tokens
                            );
                        } else {
                            println!("Session     : (none yet)");
                        }
                    }
                    "diff" => {
                        let root = Path::new(&project.root_path);
                        if let Some(status) = run_quick_git(root, &["status", "--short"]) {
                            if status.is_empty() {
                                println!("Git working tree is clean.");
                            } else {
                                println!("\x1b[1mGit Status:\x1b[0m\n{status}");
                                if let Some(stat) = run_quick_git(root, &["diff", "--stat"])
                                    && !stat.is_empty()
                                {
                                    println!("\n\x1b[1mGit Diff Stat:\x1b[0m\n{stat}");
                                }
                            }
                        }
                        if let Some(active) = &session_state
                            && let Some(last_tid) = active.task_history.last()
                            && let Ok(changes) = engine.changes(last_tid)
                            && !changes.is_empty()
                        {
                            println!("\n\x1b[1mLatest Turn Tracked File Edits:\x1b[0m");
                            for ch in changes {
                                println!(
                                    "  - {} ({:?}, restorable={})",
                                    ch.path, ch.kind, ch.restorable
                                );
                            }
                        }
                    }
                    "undo" => {
                        let target_task = session_state.as_ref().and_then(|active| {
                            active.task_history.iter().rev().find(|tid| {
                                engine
                                    .changes(tid)
                                    .map(|c| c.iter().any(|item| item.restorable))
                                    .unwrap_or(false)
                            })
                        }).cloned();
                        match target_task {
                            Some(tid) => match engine.restore(&tid).await {
                                Ok(receipt) => {
                                    println!(
                                        "\x1b[32m✓ 已回滚任务 {} 的 {} 处文件修改 (status={:?}):\x1b[0m",
                                        tid, receipt.restored, receipt.status
                                    );
                                    for item in &receipt.outcomes {
                                        println!("  - {} ({:?})", item.path, item.status);
                                    }
                                }
                                Err(err) => eprintln!("\x1b[31m回滚失败: {err}\x1b[0m"),
                            },
                            None => println!("当前会话没有可回滚的文件修改。"),
                        }
                    }
                    other => {
                        eprintln!("未知命令 `/{other}`；输入 `/help` 查看支持的命令。");
                    }
                }
                continue;
            }

            let task = execute_turn_and_stream(
                &engine,
                &project,
                &route,
                &mut session_state,
                input,
                &mut auto_approve,
                json_output,
            )
            .await?;
            if task.status != "completed"
                && let Some(err) = &task.error
            {
                eprintln!("\x1b[31m[{}] {}\x1b[0m", task.status, err);
            }
        }
        Ok(())
    }
    .await;

    let _ = engine.shutdown(Duration::from_secs(10)).await;
    result
}

async fn run_ask(
    instance_lock: &mut Option<std::fs::File>,
    top_data_dir: Option<PathBuf>,
    top_workspace: Option<PathBuf>,
    json_output: bool,
    args: AskArgs,
) -> Result<(), Failure> {
    let data_dir = resolve_user_data_dir(args.data_dir.or(top_data_dir));
    let target_dir = args
        .dir
        .or(top_workspace)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let prompt = args.prompt.join(" ");

    let engine = open_local_engine(instance_lock, &data_dir)?;
    let outcome = async {
        let (project, route, mut session_state) = open_workbench_context(
            &engine,
            target_dir,
            args.route.as_deref(),
            args.model.as_deref(),
            args.continue_session,
            args.resume.as_deref(),
            json_output,
        )
        .await?;
        let mut auto_approve = AutoApproval::new(args.auto_approve);

        let task = execute_turn_and_stream(
            &engine,
            &project,
            &route,
            &mut session_state,
            prompt.trim(),
            &mut auto_approve,
            json_output,
        )
        .await?;

        if json_output {
            println!(
                "{}",
                json!({
                    "ok": task.status == "completed",
                    "status": task.status,
                    "task_id": task.id,
                    "run_id": task.run_id,
                    "session_id": session_state.as_ref().map(|s| &s.session_id.0),
                    "output": task.output,
                    "error": task.error,
                    "usage": task.usage,
                })
            );
        }

        if task.status == "completed" {
            Ok(())
        } else {
            Err(Failure::custom(
                "task_failed",
                task.error
                    .unwrap_or_else(|| format!("任务未完成，结束状态: {}", task.status)),
            ))
        }
    }
    .await;

    let _ = engine.shutdown(Duration::from_secs(10)).await;
    outcome
}

// ============================================================================
// HTTP Harness Client Subcommands (`host`, `runtime`, `plugin`, `project`, ...)
// ============================================================================

#[derive(Deserialize)]
struct Bootstrap {
    token: String,
}

#[derive(Deserialize)]
struct HttpError {
    code: Value,
    message: String,
    #[serde(default)]
    retryable: bool,
    #[serde(rename = "error")]
    _error: String,
}

#[derive(Deserialize, Serialize)]
struct ApprovalDto {
    id: String,
    task_id: String,
    tool_call_id: String,
    tool_name: String,
    preview: String,
    session_id: Option<String>,
    turn_id: Option<String>,
    decided_by: Option<String>,
    status: String,
    execution_state: String,
    created_at: u64,
    decided_at: Option<u64>,
    #[serde(default)]
    network_notice: Option<CommandNetworkNotice>,
    #[serde(default)]
    command_network_ack: CommandNetworkAck,
}

#[derive(Deserialize, Serialize)]
struct ApprovalList {
    approvals: Vec<ApprovalDto>,
}

fn invalid_response() -> Failure {
    Failure::fixed("internal", "本机服务返回无效或不安全响应", false)
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Failure> {
    serde_json::from_value(value).map_err(|_| invalid_response())
}

fn encoded<T: Serialize>(value: T) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|_| invalid_response())
}

fn contains_string(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.contains(needle),
        Value::Array(values) => values.iter().any(|v| contains_string(v, needle)),
        Value::Object(values) => {
            values.keys().any(|k| k.contains(needle))
                || values.values().any(|v| contains_string(v, needle))
        }
        _ => false,
    }
}

fn print_value(value: Value, json_output: bool) -> Result<(), Failure> {
    let mut output = io::stdout().lock();
    let text = if json_output {
        value.to_string()
    } else {
        serde_json::to_string_pretty(&value).map_err(|_| invalid_response())?
    };
    writeln!(output, "{text}")
        .and_then(|()| output.flush())
        .map_err(|_| Failure::fixed("output_failed", "无法写入标准输出", false))
}

fn endpoint(origin: &Url, segments: &[&str]) -> Result<Url, Failure> {
    let mut url = origin.clone();
    url.path_segments_mut()
        .map_err(|_| Failure::fixed("internal", "本机服务地址无效", false))?
        .clear()
        .extend(segments);
    Ok(url)
}

async fn limited_body(response: reqwest::Response) -> Result<Vec<u8>, Failure> {
    if response
        .content_length()
        .is_some_and(|length| length > RESPONSE_LIMIT as u64)
    {
        return Err(Failure::fixed("internal", "本机服务响应过大", false));
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| Failure::fixed("unavailable", "读取本机服务响应失败", true))?;
        if body.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            return Err(Failure::fixed("internal", "本机服务响应过大", false));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn request_bootstrap(request: reqwest::RequestBuilder) -> Result<Value, Failure> {
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            Failure::fixed("unavailable", "本机服务请求超时", true)
        } else {
            Failure::fixed("unavailable", "无法连接本机服务", true)
        }
    })?;
    let success = response.status().is_success();
    let bytes = limited_body(response).await?;
    if !success {
        return Err(Failure::fixed(
            "internal",
            "本机服务 bootstrap 响应无效",
            false,
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Failure::fixed("internal", "本机服务 bootstrap 响应无效", false))
}

async fn fetch_token(client: &Client, origin: &Url) -> Result<String, Failure> {
    let bootstrap: Bootstrap =
        decode(request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?)).await?)?;
    if bootstrap.token.is_empty()
        || bootstrap.token.len() > 4096
        || reqwest::header::HeaderValue::from_str(&bootstrap.token).is_err()
    {
        return Err(invalid_response());
    }
    Ok(bootstrap.token)
}

pub async fn execute(port: u16, json_output: bool, command: Command) -> Result<(), Failure> {
    let client = Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| Failure::fixed("internal", "无法初始化本机客户端", false))?;
    let origin = Url::parse(&format!("http://127.0.0.1:{port}/"))
        .map_err(|_| Failure::fixed("internal", "本机服务地址无效", false))?;

    match command {
        Command::Host(args) => match args.command {
            HostCommand::Composition => composition_workflow(&client, &origin, json_output).await,
            other => host_workflow(&client, &origin, json_output, other).await,
        },
        Command::Runtime(args) => match args.command {
            RuntimeCommand::Agent(_) => agent_runtime_workflow(&client, &origin, json_output).await,
            RuntimeCommand::Component(args) => {
                runtime_component_workflow(&client, &origin, json_output, args.command).await
            }
        },
        Command::Plugin(args) => plugin_workflow(&client, &origin, json_output, args).await,
        Command::Project(ProjectArgs {
            command: ProjectCommand::Budget(args),
        }) => budget_workflow(&client, &origin, json_output, args.command).await,
        Command::Project(ProjectArgs {
            command: ProjectCommand::Worktree(args),
        }) => worktree_workflow(&client, &origin, json_output, args.command).await,
        Command::Network(_)
        | Command::Route(_)
        | Command::Account(_)
        | Command::Task(TaskArgs {
            command: TaskCommand::NetworkCalls { .. },
        }) => network_workflow(&client, &origin, json_output, command).await,
        Command::Approval(approval) => {
            approval_workflow(&client, &origin, json_output, approval.command).await
        }
        Command::Task(TaskArgs {
            command: TaskCommand::Approvals { task_id },
        }) => {
            let url = endpoint(&origin, &["api", "tasks", &task_id, "approvals"])?;
            let value = request_json(client.get(url), false, None).await?;
            let list: ApprovalList = decode(value)?;
            if json_output {
                print_value(encoded(&list)?, true)?;
            } else if list.approvals.is_empty() {
                println!("没有审批请求");
            } else {
                for item in &list.approvals {
                    print_approval(&encoded(item)?)?;
                }
            }
            Ok(())
        }
        other => workflow(&client, &origin, json_output, other).await,
    }
}

fn host_error_code(code: &str) -> Option<(&'static str, &'static str)> {
    Some(match code {
        "host_config_invalid" => ("host_config_invalid", "配置请求无效"),
        "forbidden" => ("forbidden", "本机Host访问被拒绝"),
        "host_config_not_found" => (
            "host_config_not_found",
            "查询快照未见原回执；原PUT可能仍在途，不要因此换key或自动重发。请随后查询原回执及当前配置",
        ),
        "host_config_revision_conflict" => (
            "host_config_revision_conflict",
            "Host配置版本冲突，请查询当前配置",
        ),
        "host_config_idempotency_conflict" => {
            ("host_config_idempotency_conflict", "原key与配置请求不一致")
        }
        "host_config_busy" => ("host_config_busy", "Host仍有活动任务或网络调用"),
        "host_config_credential_required" => {
            ("host_config_credential_required", "此配置需要重新输入凭据")
        }
        "host_config_corrupt" => ("host_config_corrupt", "Host配置持久状态损坏"),
        "host_config_unavailable" => ("host_config_unavailable", "Host配置或凭据不可用"),
        "host_config_internal" => ("host_config_internal", "Host配置操作失败"),
        _ => return None,
    })
}

async fn composition_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
) -> Result<(), Failure> {
    let token = fetch_token(client, origin)
        .await
        .map_err(|_| Failure::fixed("unavailable", "无法读取本机组合状态", true))?;
    let response = client
        .get(endpoint(origin, &["api", "host", "composition"])?)
        .header("x-coda-token", &token)
        .send()
        .await
        .map_err(|_| Failure::fixed("unavailable", "无法读取本机组合状态", true))?;
    let status = response.status();
    let bytes = limited_body(response).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
    if contains_string(&value, &token) {
        return Err(invalid_response());
    }
    if !status.is_success() {
        return Err(http_failure(value)?);
    }
    print_value(value, json_output)
}

async fn host_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: HostCommand,
) -> Result<(), Failure> {
    let write = match &command {
        HostCommand::Route(HostRouteArgs {
            command: HostRouteCommand::Set(args),
        }) => Some(args),
        _ => None,
    };
    let secret = if let Some(args) = write.filter(|args| !args.keep_credential) {
        let mode = if args.credential_prompt {
            CredentialInputMode::Prompt
        } else {
            CredentialInputMode::Stdin
        };
        Some(HostSecret(
            read_credential(mode)
                .map_err(|error| match error {
                    CredentialInputError::Cancelled => Failure::fixed(
                        "credential_cancelled",
                        "秘密输入已取消，配置PUT未发送",
                        false,
                    ),
                    CredentialInputError::Unavailable => Failure::fixed(
                        "credential_unavailable",
                        "安全秘密输入不可用，配置PUT未发送",
                        false,
                    ),
                    CredentialInputError::Invalid => {
                        Failure::fixed("credential_invalid", "秘密输入无效，配置PUT未发送", false)
                    }
                })?
                .into_string(),
        ))
    } else {
        None
    };

    let token = fetch_token(client, origin).await.map_err(|_| {
        Failure::fixed(
            "host_config_unavailable",
            "bootstrap失败，配置PUT未发送",
            false,
        )
    })?;

    let request = match &command {
        HostCommand::Composition => return Err(invalid_response()),
        HostCommand::Config(HostConfigArgs {
            command: HostConfigCommand::Get,
        }) => client.get(endpoint(origin, &["api", "host", "config"])?),
        HostCommand::Config(HostConfigArgs {
            command: HostConfigCommand::Operation { key },
        }) => client
            .post(endpoint(origin, &["api", "host", "config", "lookup"])?)
            .json(&json!({"key": key})),
        HostCommand::Route(HostRouteArgs {
            command: HostRouteCommand::Set(args),
        }) => {
            let credential = match &secret {
                Some(secret) => json!({"mode": "replace", "value": secret.as_str()}),
                None => json!({"mode": "keep"}),
            };
            client
                .put(endpoint(origin, &["api", "host", "routes", &args.id])?)
                .header("Idempotency-Key", &args.key)
                .json(&json!({
                    "expected_revision": args.expected_revision,
                    "route": args.route()?,
                    "credential": credential,
                }))
        }
    }
    .header("x-coda-token", &token);

    let result: Result<(), Failure> = async {
        let response = request.send().await.map_err(|_| {
            Failure::fixed("host_transport_unavailable", "无法完成本机Host请求", false)
        })?;
        let status = response.status();
        let bytes = limited_body(response).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
        if contains_string(&value, &token)
            || secret
                .as_ref()
                .is_some_and(|s| contains_string(&value, s.as_str()))
        {
            return Err(invalid_response());
        }
        if !status.is_success() {
            if let Some(code) = value["error"]["code"].as_str()
                && let Some((mapped, message)) = host_error_code(code)
            {
                return Err(Failure::fixed(mapped, message, false));
            }
            return Err(http_failure(value)?);
        }
        match &command {
            HostCommand::Config(HostConfigArgs {
                command: HostConfigCommand::Get,
            }) => {
                let _: coda::host_configuration::HostConfigSnapshot = decode(value.clone())?;
            }
            _ => {
                let _: coda::host_configuration::RouteConfigResult = decode(value.clone())?;
            }
        }
        print_value(value, json_output)
    }
    .await;

    result.map_err(|error| {
        if write.is_some() && host_error_code(error.code).is_none() {
            Failure::fixed(
                "operation_result_unknown",
                "配置结果未知，可能已提交；请用 host config operation --key <原key> 或 host config get 查询",
                false,
            )
        } else {
            error
        }
    })
}

async fn agent_runtime_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
) -> Result<(), Failure> {
    let token = fetch_token(client, origin)
        .await
        .map_err(|_| Failure::fixed("agent_runtime_unavailable", "Agent运行态不可用", false))?;
    let response = client
        .get(endpoint(origin, &["api", "runtime", "agents", "chat"])?)
        .header("x-coda-token", &token)
        .send()
        .await
        .map_err(|_| Failure::fixed("agent_runtime_unavailable", "Agent运行态不可用", false))?;
    let status = response.status();
    let bytes = limited_body(response).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
    if !status.is_success() {
        if value["code"] == "agent_runtime_unavailable" {
            return Err(Failure::fixed(
                "agent_runtime_unavailable",
                "Agent运行态不可用",
                false,
            ));
        }
        return Err(http_failure(value)?);
    }
    print_value(value, json_output)
}

async fn runtime_component_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: RuntimeComponentCommand,
) -> Result<(), Failure> {
    let token = fetch_token(client, origin).await?;
    let (segments, mutation) = match &command {
        RuntimeComponentCommand::List => (vec!["api", "runtime", "components"], None),
        RuntimeComponentCommand::Status { id } => {
            (vec!["api", "runtime", "components", id.as_str()], None)
        }
        RuntimeComponentCommand::SetEnabled {
            id,
            enabled,
            expected_revision,
            key,
        } => (
            vec!["api", "runtime", "components", id.as_str(), "enabled"],
            Some((*enabled, *expected_revision, key.as_str())),
        ),
    };
    let url = endpoint(origin, &segments)?;
    let request = match mutation {
        Some((enabled, revision, key)) => client
            .put(url)
            .header("idempotency-key", key)
            .json(&json!({"enabled": enabled, "expected_revision": revision})),
        None => client.get(url),
    }
    .header("x-coda-token", &token);

    let response = request.send().await.map_err(|_| {
        if mutation.is_some() {
            Failure::fixed(
                "operation_result_unknown",
                "Runtime management outcome unknown; use the same key and parameters for an explicit replay.",
                false,
            )
        } else {
            Failure::fixed("unavailable", "无法连接本机服务", true)
        }
    })?;
    let status = response.status();
    let bytes = limited_body(response).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
    if !status.is_success() {
        if let Some(code) = value["code"].as_str() {
            let mapped = match code {
                "runtime_invalid" => Some("runtime_invalid"),
                "runtime_not_found" => Some("runtime_not_found"),
                "runtime_revision_conflict" => Some("runtime_revision_conflict"),
                "runtime_idempotency_conflict" => Some("runtime_idempotency_conflict"),
                "runtime_busy" => Some("runtime_busy"),
                "runtime_unavailable" => Some("runtime_unavailable"),
                "runtime_internal" => Some("runtime_internal"),
                _ => None,
            };
            if let Some(mapped) = mapped {
                if mutation.is_some() && mapped == "runtime_internal" {
                    return Err(Failure::fixed(
                        "operation_result_unknown",
                        "Runtime management outcome unknown; use the same key and parameters for an explicit replay.",
                        false,
                    ));
                }
                return Err(Failure::custom(
                    mapped,
                    value["message"].as_str().unwrap_or(mapped),
                ));
            }
        }
        return Err(http_failure(value)?);
    }
    let converged = value
        .get("current")
        .and_then(|c| c.get("converged"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    print_value(value, json_output)?;
    if mutation.is_some() && !converged {
        return Err(Failure::fixed(
            "plugin_not_converged",
            "持久命令已接受，当前运行时尚未收敛",
            false,
        ));
    }
    Ok(())
}

fn print_plugin(plugin: &PluginStatus) {
    let runtime = encoded(&plugin.runtime).unwrap_or_else(|_| json!({}));
    println!(
        "{}\t{}\tdesired={}\trev={}\tstate={}\tactive_calls={}\tdraining={}\tconverged={}",
        plugin.plugin_id,
        plugin.version,
        plugin.desired.enabled,
        plugin.desired.revision,
        runtime["state"].as_str().unwrap_or("unknown"),
        runtime["active_calls"],
        runtime["draining"],
        plugin.converged
    );
}

async fn plugin_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    args: PluginArgs,
) -> Result<(), Failure> {
    let (segments, mutation) = match &args.command {
        PluginCommand::List => (vec!["api", "plugins"], None),
        PluginCommand::Status { id } => (vec!["api", "plugins", id], None),
        PluginCommand::SetEnabled {
            id,
            enabled,
            expected_revision,
            key,
        } => (
            vec!["api", "plugins", id, "enabled"],
            Some((*enabled, *expected_revision, key.as_str())),
        ),
    };
    let token = fetch_token(client, origin).await?;
    let url = endpoint(origin, &segments)?;
    let request = match mutation {
        Some((enabled, expected_revision, key)) => client
            .put(url)
            .header("idempotency-key", key)
            .json(&json!({"enabled": enabled, "expected_revision": expected_revision})),
        None => client.get(url),
    }
    .header("x-coda-token", &token);
    let is_mutation = mutation.is_some();
    let response = request.send().await.map_err(|_| {
        if is_mutation {
            Failure::fixed(
                "operation_result_unknown",
                "插件写入结果未知，可能已提交；请查询 plugin status",
                false,
            )
        } else {
            Failure::fixed("unavailable", "无法连接或读取本机服务", true)
        }
    })?;
    let status = response.status();
    let bytes = limited_body(response).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
    if !status.is_success() {
        if let Some(code) = value["code"].as_str() {
            let mapped = match code {
                "plugin_invalid" => Some("plugin_invalid"),
                "plugin_not_found" => Some("plugin_not_found"),
                "plugin_revision_conflict" => Some("plugin_revision_conflict"),
                "plugin_idempotency_conflict" => Some("plugin_idempotency_conflict"),
                "plugin_unavailable" => Some("plugin_unavailable"),
                "plugin_internal" => Some("plugin_internal"),
                _ => None,
            };
            if let Some(mapped) = mapped {
                if is_mutation && mapped == "plugin_internal" {
                    return Err(Failure::fixed(
                        "operation_result_unknown",
                        "插件写入结果未知，可能已提交；请查询 plugin status",
                        false,
                    ));
                }
                return Err(Failure::custom(
                    mapped,
                    value["message"].as_str().unwrap_or(mapped),
                ));
            }
        }
        return Err(http_failure(value)?);
    }

    match args.command {
        PluginCommand::List => {
            #[derive(Deserialize)]
            struct List {
                plugins: Vec<PluginStatus>,
            }
            let list: List = decode(value.clone())?;
            if json_output {
                print_value(value, true)?;
            } else {
                for plugin in &list.plugins {
                    print_plugin(plugin);
                }
            }
        }
        PluginCommand::Status { .. } => {
            #[derive(Deserialize)]
            struct Status {
                plugin: PluginStatus,
            }
            let result: Status = decode(value.clone())?;
            if json_output {
                print_value(value, true)?;
            } else {
                print_plugin(&result.plugin);
            }
        }
        PluginCommand::SetEnabled { .. } => {
            let result: PluginMutationResult = decode(value.clone())?;
            if json_output {
                print_value(value, true)?;
            } else {
                println!(
                    "持久命令已接受：receipt={} replayed={}",
                    encoded(&result.receipt)?,
                    result.replayed
                );
                print_plugin(&result.current);
            }
            if !result.current.converged {
                return Err(Failure::fixed(
                    "plugin_not_converged",
                    "持久命令已接受，当前运行时尚未收敛",
                    false,
                ));
            }
        }
    }
    Ok(())
}

async fn approval_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: ApprovalCommand,
) -> Result<(), Failure> {
    let value = match command {
        ApprovalCommand::Get { approval_id } => {
            let url = endpoint(origin, &["api", "approvals", &approval_id])?;
            request_json(client.get(url), false, None).await?
        }
        ApprovalCommand::Approve {
            approval_id,
            acknowledge_command_network,
        } => {
            decide_http(
                client,
                origin,
                &approval_id,
                "approve",
                acknowledge_command_network,
            )
            .await?
        }
        ApprovalCommand::Deny { approval_id } => {
            decide_http(client, origin, &approval_id, "deny", false).await?
        }
    };
    let dto: ApprovalDto = decode(value)?;
    let encoded_dto = encoded(&dto)?;
    if json_output {
        println!("{encoded_dto}");
    } else {
        print_approval(&encoded_dto)?;
    }
    Ok(())
}

async fn decide_http(
    client: &Client,
    origin: &Url,
    id: &str,
    decision: &str,
    acknowledge_command_network: bool,
) -> Result<Value, Failure> {
    let token = fetch_token(client, origin).await?;
    let url = endpoint(origin, &["api", "approvals", id, "decision"])?;
    request_json(
        client
            .post(url)
            .header("x-coda-token", &token)
            .json(&json!({
                "decision": decision,
                "acknowledge_command_network": acknowledge_command_network,
            })),
        true,
        Some(&token),
    )
    .await
}

fn print_approval(value: &Value) -> Result<(), Failure> {
    if value["tool_name"] == "run_command" {
        println!(
            "此命令可能访问任意网络，当前没有OS出站沙箱；确认状态：{}",
            value["command_network_ack"]
        );
    }
    println!(
        "{}\t{}\t{}",
        value["id"].as_str().unwrap_or_default(),
        value["status"].as_str().unwrap_or_default(),
        value["execution_state"].as_str().unwrap_or_default()
    );
    Ok(())
}

async fn request_json(
    request: reqwest::RequestBuilder,
    decision_post: bool,
    known_token: Option<&str>,
) -> Result<Value, Failure> {
    let response = request.send().await.map_err(|error| {
        if decision_post {
            return Failure::fixed(
                "decision_result_unknown",
                "审批决定结果未知，请用 approval get 查询",
                false,
            );
        }
        if error.is_timeout() {
            Failure::fixed("unavailable", "本机服务请求超时", true)
        } else {
            Failure::fixed("unavailable", "无法连接本机服务", true)
        }
    })?;
    let status = response.status();
    let bytes = limited_body(response).await.map_err(|error| {
        if decision_post && error.code == "unavailable" {
            Failure::fixed(
                "decision_result_unknown",
                "审批决定结果未知，请用 approval get 查询",
                false,
            )
        } else {
            error
        }
    })?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Failure::fixed("internal", "本机服务返回无效 JSON", false))?;
    if known_token.is_some_and(|token| contains_string(&value, token)) {
        return Err(Failure::fixed("internal", "本机服务返回不安全响应", false));
    }
    if status.is_success() {
        return Ok(value);
    }
    Err(http_failure(value)?)
}

fn budget_error_code(code: &str) -> Option<(&'static str, u16)> {
    Some(match code {
        "budget_invalid" => ("budget_invalid", 400),
        "budget_not_found" => ("budget_not_found", 404),
        "budget_revision_conflict" => ("budget_revision_conflict", 409),
        "budget_conflict" => ("budget_conflict", 409),
        "budget_busy" => ("budget_busy", 409),
        "budget_exceeded" => ("budget_exceeded", 409),
        "budget_unresolved" => ("budget_unresolved", 409),
        "budget_overrun" => ("budget_overrun", 409),
        "budget_corrupt" => ("budget_corrupt", 500),
        "budget_internal" => ("budget_internal", 500),
        _ => return None,
    })
}

fn project_error_code(code: &str) -> Option<&'static str> {
    match code {
        "worktree_busy" => Some("worktree_busy"),
        "project_invalid" => Some("project_invalid"),
        "project_not_found" => Some("project_not_found"),
        "project_revision_conflict" => Some("project_revision_conflict"),
        "project_duplicate_root" => Some("project_duplicate_root"),
        "project_overlapping_root" => Some("project_overlapping_root"),
        "project_selection_required" => Some("project_selection_required"),
        "project_legacy_unbound" => Some("project_legacy_unbound"),
        "project_root_changed" => Some("project_root_changed"),
        "project_root_unavailable" => Some("project_root_unavailable"),
        "project_permission_revoked" => Some("project_permission_revoked"),
        "project_operation_unsupported" => Some("project_operation_unsupported"),
        "project_corrupt" => Some("project_corrupt"),
        "project_internal" => Some("project_internal"),
        "workspace_selection_managed" => Some("workspace_selection_managed"),
        _ => None,
    }
}

#[derive(Deserialize)]
struct NetworkHttpError {
    code: String,
    message: String,
    dispatch_state: DispatchState,
    calls: Vec<CallReceipt>,
    #[serde(default)]
    retryable: bool,
}

fn network_http_failure(value: Value) -> Result<Failure, Failure> {
    let error: NetworkHttpError =
        decode(value.get("error").cloned().ok_or_else(invalid_response)?)?;
    let code = match error.code.as_str() {
        "invalid" => "invalid",
        "not_found" => "not_found",
        "required" => "required",
        "stale" => "stale",
        "revoked" => "revoked",
        "conflict" => "conflict",
        "corrupt" => "corrupt",
        "unknown" => "unknown",
        "internal" => "internal",
        "upstream" => "upstream",
        "invalid_tool_arguments" => "invalid_tool_arguments",
        other => budget_error_code(other)
            .map(|(code, _)| code)
            .or_else(|| project_error_code(other))
            .unwrap_or("request_failed"),
    };
    Ok(Failure {
        code,
        message: error.message,
        retryable: error.retryable,
        details: Some(json!({"dispatch_state": error.dispatch_state, "calls": error.calls})),
        after: None,
    })
}

fn http_failure(value: Value) -> Result<Failure, Failure> {
    if value.get("error").is_some_and(Value::is_object) {
        if value["error"].get("dispatch_state").is_some() {
            return network_http_failure(value);
        }
        let code_str = value["error"]["code"].as_str().unwrap_or("request_failed");
        let message = value["error"]["message"]
            .as_str()
            .unwrap_or("请求失败")
            .to_owned();
        let retryable = value["error"]["retryable"].as_bool().unwrap_or(false);
        let mapped = match code_str {
            "forbidden" => "forbidden",
            other => project_error_code(other).unwrap_or("request_failed"),
        };
        return Ok(Failure {
            code: mapped,
            message,
            retryable,
            details: None,
            after: None,
        });
    }
    let error: HttpError = decode(value)?;
    let code = match error.code.as_str() {
        Some("request_failed") => "request_failed",
        Some("not_found") => "not_found",
        Some("conflict") => "conflict",
        Some("forbidden") => "forbidden",
        Some("unavailable") => "unavailable",
        Some("internal") => "internal",
        other => project_error_code(other.unwrap_or("")).unwrap_or("request_failed"),
    };
    Ok(Failure {
        code,
        message: error.message,
        retryable: error.retryable,
        details: None,
        after: None,
    })
}

fn operation_unknown(operation: &str) -> Failure {
    let message = match operation {
        "start" => "启动结果未知，请查询 run list；仅可用原参数和原 key 手动查询式重放",
        "send" => "新回合结果未知，请查询 session turns；仅可用原参数和原 key 手动重放",
        "budget-config" => {
            "预算配置结果未知，请 GET 查询配置或使用原 key 原参数显式重放，不要自动重复写入"
        }
        "resume" => "继续结果未知，请查询原任务所在 run status，不要自动重复继续",
        "cancel" => "取消结果未知，请查询原任务所在 run status",
        "checkpoint" => "检查点创建结果未知，请仅用原任务和原 key 手动查询式重放",
        "restore-checkpoint" => {
            "检查点恢复结果未知，请用 task changes 查询原任务回执，不要自动重做"
        }
        "restore" => "恢复结果未知，请用 task changes 查询持久回执，不要自动重做",
        _ => "操作结果未知，请查询持久状态",
    };
    Failure::fixed("operation_result_unknown", message, false)
}

async fn worktree_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: WorktreeCommand,
) -> Result<(), Failure> {
    let (source, operation_id, is_create, is_inspect) = match &command {
        WorktreeCommand::Create { source_project, .. } => {
            (source_project.as_str(), None, true, false)
        }
        WorktreeCommand::Status {
            source_project, id, ..
        } => (source_project.as_str(), id.as_deref(), false, false),
        WorktreeCommand::Inspect { source_project, id } => {
            (source_project.as_str(), Some(id.as_str()), false, true)
        }
    };
    let token = fetch_token(client, origin).await?;
    let mut segments = vec!["api", "projects", source, "worktrees"];
    let request = match &command {
        WorktreeCommand::Create {
            name,
            parent,
            expected_config_revision,
            expected_head,
            key,
            ..
        } => client
            .post(endpoint(origin, &segments)?)
            .header("idempotency-key", key)
            .json(&json!({
                "name": name,
                "parent_path": parent,
                "expected_config_revision": expected_config_revision,
                "expected_head": expected_head,
            })),
        WorktreeCommand::Status { key: Some(key), .. } => {
            segments.push("lookup");
            client
                .post(endpoint(origin, &segments)?)
                .json(&json!({"idempotency_key": key}))
        }
        WorktreeCommand::Status { .. } => {
            segments.push(operation_id.expect("validated id"));
            client.get(endpoint(origin, &segments)?)
        }
        WorktreeCommand::Inspect { id, .. } => {
            segments.extend([id.as_str(), "inspect"]);
            client.post(endpoint(origin, &segments)?).json(&json!({}))
        }
    }
    .header("x-coda-token", &token);

    let response = request.send().await.map_err(|_| {
        if is_create {
            Failure::fixed(
                "operation_result_unknown",
                "工作树创建结果未知；请用 project worktree status 查询",
                false,
            )
        } else {
            Failure::fixed("unavailable", "无法读取工作树状态", false)
        }
    })?;
    let status = response.status().as_u16();
    let bytes = limited_body(response).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;

    if (value.get("schema_version").is_none() && !is_inspect)
        || (is_inspect && value.get("receipt").is_none())
    {
        let code = match value["code"].as_str().unwrap_or("request_failed") {
            "worktree_invalid" => "worktree_invalid",
            "worktree_not_found" => "worktree_not_found",
            "worktree_revision_conflict" => "worktree_revision_conflict",
            "worktree_idempotency_conflict" => "worktree_idempotency_conflict",
            "worktree_busy" => "worktree_busy",
            "worktree_root_changed" => "worktree_root_changed",
            "worktree_conflict" => "worktree_conflict",
            "worktree_unsupported" => "worktree_unsupported",
            "worktree_unavailable" => "worktree_unavailable",
            "worktree_corrupt" => "worktree_corrupt",
            "forbidden" => "forbidden",
            _ if status >= 400 => "request_failed",
            _ => return Err(invalid_response()),
        };
        return Err(Failure::custom(
            code,
            value["message"].as_str().unwrap_or(code),
        ));
    }

    let receipt = if is_inspect {
        let inspection: WorktreeInspection = decode(value.clone())?;
        inspection.receipt
    } else {
        decode::<WorktreeReceipt>(value.clone())?
    };

    print_value(value, json_output)?;
    if is_create && receipt.state != WorktreeState::Succeeded {
        return Err(Failure::fixed(
            "worktree_not_succeeded",
            "工作树未成功完成",
            false,
        ));
    }
    Ok(())
}

async fn budget_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: BudgetCommand,
) -> Result<(), Failure> {
    let (id, call_id, write) = match &command {
        BudgetCommand::Status { id }
        | BudgetCommand::Config(BudgetConfigArgs {
            command: BudgetConfigCommand::Get { id },
        }) => (id.as_str(), None, None),
        BudgetCommand::Call { id, call_id } => (id.as_str(), Some(call_id.as_str()), None),
        BudgetCommand::Config(BudgetConfigArgs {
            command:
                BudgetConfigCommand::Set {
                    id,
                    expected_revision,
                    limit_tokens,
                    per_call_reservation_tokens,
                    key,
                },
        }) => (
            id.as_str(),
            None,
            Some((
                *expected_revision,
                BudgetConfig {
                    limit_tokens: *limit_tokens,
                    per_call_reservation_tokens: *per_call_reservation_tokens,
                },
                key.as_str(),
            )),
        ),
    };
    let token = fetch_token(client, origin).await?;
    let mut segments = vec!["api", "projects", id, "budget"];
    if matches!(command, BudgetCommand::Status { .. }) {
        segments.push("status");
    }
    if let Some(call) = call_id {
        segments.extend(["calls", call]);
    }
    let url = endpoint(origin, &segments)?;
    let request = if let Some((revision, config, key)) = write {
        client.put(url).json(&json!({
            "expected_revision": revision,
            "idempotency_key": key,
            "config": config,
        }))
    } else {
        client.get(url)
    }
    .header("x-coda-token", &token);

    let response = request.send().await.map_err(|_| {
        if write.is_some() {
            operation_unknown("budget-config")
        } else {
            Failure::fixed("unavailable", "无法连接本机服务", true)
        }
    })?;
    let status = response.status();
    let bytes = limited_body(response).await?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
    if !status.is_success() {
        return Err(http_failure(value)?);
    }
    if write.is_some() {
        let _: BudgetMutationResult = decode(value.clone())?;
    } else {
        match &command {
            BudgetCommand::Status { .. } => {
                let _: BudgetStatus = decode(value.clone())?;
            }
            BudgetCommand::Call { .. } => {
                let _: BudgetCall = decode(value.clone())?;
            }
            BudgetCommand::Config(_) => {
                let _: BudgetConfigSnapshot = decode(value.clone())?;
            }
        }
    }
    print_value(value, json_output)
}

async fn network_request(
    client: &Client,
    origin: &Url,
    segments: &[&str],
    body: Option<Value>,
    key: Option<&str>,
    mutation: bool,
) -> Result<Value, Failure> {
    let token = fetch_token(client, origin).await?;
    let url = endpoint(origin, segments)?;
    let mut request = match body {
        Some(body) => client.post(url).json(&body),
        None => client.get(url),
    }
    .header("x-coda-token", &token);
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    let unknown = || {
        if mutation {
            operation_unknown("network")
        } else {
            Failure::fixed("unavailable", "无法连接或读取本机服务", true)
        }
    };
    let response = request.send().await.map_err(|_| unknown())?;
    let status = response.status();
    let bytes = limited_body(response).await.map_err(|_| unknown())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| unknown())?;
    if contains_string(&value, &token) {
        return Err(invalid_response());
    }
    if status.is_success() {
        return Ok(value);
    }
    Err(http_failure(value)?)
}

async fn network_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: Command,
) -> Result<(), Failure> {
    let value = match command {
        Command::Network(args) => match args.command {
            NetworkCommand::Preview(subject) => {
                let subject = subject.subject();
                let preview: NetworkPreview = decode(
                    network_request(
                        client,
                        origin,
                        &["api", "network", "preview"],
                        Some(json!(subject)),
                        None,
                        false,
                    )
                    .await?,
                )?;
                encoded(preview)?
            }
            NetworkCommand::Authorize {
                subject,
                config_revision,
                credential_revision,
                binding,
                purpose,
                key,
            } => {
                let subject = subject.subject();
                let (cfg_rev, cred_rev, binding_digest) =
                    match (config_revision, credential_revision, binding) {
                        (Some(c), Some(cr), Some(b)) => (c, cr, b),
                        (c_opt, cr_opt, b_opt) => {
                            let preview: NetworkPreview = decode(
                                network_request(
                                    client,
                                    origin,
                                    &["api", "network", "preview"],
                                    Some(json!(subject)),
                                    None,
                                    false,
                                )
                                .await?,
                            )?;
                            (
                                c_opt.unwrap_or(preview.config_revision),
                                cr_opt.unwrap_or(preview.credential_revision),
                                b_opt.unwrap_or(preview.binding_digest),
                            )
                        }
                    };
                let mut purposes: Vec<String> = purpose
                    .into_iter()
                    .map(|p| match p {
                        PurposeArg::Models => "models".to_owned(),
                        PurposeArg::Completion => "completion".to_owned(),
                        PurposeArg::Balance => "balance".to_owned(),
                    })
                    .collect();
                purposes.sort();
                purposes.dedup();
                let request = json!({
                    "subject": subject,
                    "config_revision": cfg_rev,
                    "credential_revision": cred_rev,
                    "binding_digest": binding_digest,
                    "purposes": purposes,
                });
                let decision: NetworkDecision = decode(
                    network_request(
                        client,
                        origin,
                        &["api", "network", "authorizations"],
                        Some(request),
                        Some(&key),
                        true,
                    )
                    .await?,
                )?;
                encoded(decision)?
            }
            NetworkCommand::Get { id } => {
                let auth: NetworkAuthorization = decode(
                    network_request(
                        client,
                        origin,
                        &["api", "network", "authorizations", &id],
                        None,
                        None,
                        false,
                    )
                    .await?,
                )?;
                encoded(auth)?
            }
            NetworkCommand::Revoke { id } => {
                let auth: NetworkAuthorization = decode(
                    network_request(
                        client,
                        origin,
                        &["api", "network", "authorizations", &id, "revoke"],
                        Some(json!({})),
                        None,
                        true,
                    )
                    .await?,
                )?;
                encoded(auth)?
            }
            NetworkCommand::Call { id } => {
                let call: CallReceipt = decode(
                    network_request(
                        client,
                        origin,
                        &["api", "network", "calls", &id],
                        None,
                        None,
                        false,
                    )
                    .await?,
                )?;
                encoded(call)?
            }
        },
        Command::Task(TaskArgs {
            command: TaskCommand::NetworkCalls { id },
        }) => {
            network_request(
                client,
                origin,
                &["api", "tasks", &id, "network-calls"],
                None,
                None,
                false,
            )
            .await?
        }
        Command::Route(args) => {
            let (id, project, action) = match args.command {
                RouteCommand::Models { id, project } => (id, project, "models"),
                RouteCommand::Check { id, project } => (id, project, "check"),
            };
            let response: NetworkResponse<Value> = decode(
                network_request(
                    client,
                    origin,
                    &["api", "routes", &id, action],
                    Some(json!({"project_id": project})),
                    None,
                    true,
                )
                .await?,
            )?;
            encoded(response)?
        }
        Command::Account(_) => {
            let response: NetworkResponse<Value> = decode(
                network_request(
                    client,
                    origin,
                    &["api", "newapi", "balance"],
                    Some(json!({})),
                    None,
                    true,
                )
                .await?,
            )?;
            encoded(response)?
        }
        _ => unreachable!(),
    };
    print_value(value, json_output)
}

fn task_projection(task: Task) -> Value {
    json!({
        "id": task.id,
        "run_id": task.run_id,
        "name": task.spec.name,
        "role": task.spec.role,
        "status": task.status,
        "output": task.output,
        "error": task.error,
        "workspace": task.workspace,
        "route_id": task.route.id,
        "tools": task.spec.tools,
        "write_scopes": task.spec.write_scopes,
        "allow_commands": task.spec.allow_commands,
        "created_at": task.created_at,
        "updated_at": task.updated_at,
    })
}

fn run_projection(run: Run) -> Value {
    let tasks: Vec<_> = run.tasks.into_iter().map(task_projection).collect();
    json!({
        "id": run.id,
        "run_id": run.id,
        "title": run.title,
        "kind": run.kind,
        "created_at": run.created_at,
        "tasks": tasks,
    })
}

#[derive(Deserialize, Serialize)]
struct RunSummary {
    id: String,
    title: String,
    kind: SessionKind,
    created_at: u64,
}

#[derive(Deserialize, Serialize)]
struct ContextView {
    run_id: String,
    project: Project,
    session: Session,
    latest_turn: Turn,
    tasks: Vec<TurnTask>,
}

#[derive(Deserialize, Serialize)]
struct TurnReceiptView {
    turn: Turn,
    task: TurnTask,
    replayed: bool,
}

#[derive(Deserialize, Serialize)]
struct ChangesView {
    changes: Vec<WorkspaceChange>,
    restore: Option<RestoreReceipt>,
}

#[derive(Deserialize, Serialize)]
struct RestoreView {
    ok: bool,
    restored: usize,
    status: RestoreStatus,
    receipt: RestoreReceipt,
}

async fn workflow_request(
    client: &Client,
    origin: &Url,
    segments: &[&str],
    write: Option<(&str, Value)>,
    key: Option<&str>,
) -> Result<Value, Failure> {
    let mut token = None;
    let operation = write.as_ref().map(|(op, _)| *op).filter(|op| *op != "diff");
    let mut url = endpoint(origin, segments)?;
    if segments.len() == 4 && segments[..3] == ["api", "runs", "project-filter"] {
        url = endpoint(origin, &["api", "runs"])?;
        url.query_pairs_mut().append_pair("project_id", segments[3]);
    }
    let authenticated_read = segments
        .get(1)
        .is_some_and(|s| matches!(*s, "projects" | "project-selection"))
        || segments.last() == Some(&"project");

    let request = if write.is_some() || authenticated_read {
        let body = write.as_ref().map(|(_, body)| body);
        let tok = fetch_token(client, origin).await?;
        let mut req = if body.is_none() {
            client.get(url)
        } else if matches!(operation, Some("select-project" | "configure-project")) {
            client.put(url)
        } else {
            client.post(url)
        };
        req = req.header("x-coda-token", &tok);
        if let Some(body) = body {
            req = req.json(body);
        }
        if let Some(key) = key {
            req = req.header("idempotency-key", key);
        }
        token = Some(tok);
        req
    } else {
        client.get(url)
    };

    let response = request.send().await.map_err(|_| {
        operation
            .map(operation_unknown)
            .unwrap_or_else(|| Failure::fixed("unavailable", "无法连接或读取本机服务", true))
    })?;
    let status = response.status();
    let bytes = limited_body(response)
        .await
        .map_err(|error| operation.map(operation_unknown).unwrap_or(error))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
    if token
        .as_deref()
        .is_some_and(|tok| contains_string(&value, tok))
    {
        return Err(invalid_response());
    }
    if status.is_success() {
        return Ok(value);
    }
    let mut failure = http_failure(value.clone())?;
    if matches!(operation, Some("restore" | "restore-checkpoint")) && value.get("receipt").is_some()
    {
        let view: RestoreView = decode(value)?;
        failure.details = Some(encoded(view)?);
    }
    Err(failure)
}

async fn workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: Command,
) -> Result<(), Failure> {
    let value = match command {
        Command::Project(args) => match args.command {
            ProjectCommand::Worktree(_) | ProjectCommand::Budget(_) => unreachable!(),
            ProjectCommand::List => {
                let projects: Vec<Project> = decode(
                    workflow_request(client, origin, &["api", "projects"], None, None).await?,
                )?;
                encoded(projects)?
            }
            ProjectCommand::Create { name, root, key } => {
                let project: Project = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "projects"],
                        Some(("register-project", json!({"name": name, "root_path": root}))),
                        Some(&key),
                    )
                    .await?,
                )?;
                encoded(project)?
            }
            ProjectCommand::Get { id } => {
                let project: Project = decode(
                    workflow_request(client, origin, &["api", "projects", &id], None, None).await?,
                )?;
                encoded(project)?
            }
            ProjectCommand::Current => {
                let selection: ProjectSelection = decode(
                    workflow_request(client, origin, &["api", "project-selection"], None, None)
                        .await?,
                )?;
                encoded(selection)?
            }
            ProjectCommand::Select {
                id,
                expected_revision,
            } => {
                let selection: ProjectSelection = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "project-selection"],
                        Some((
                            "select-project",
                            json!({"project_id": id, "expected_revision": expected_revision}),
                        )),
                        None,
                    )
                    .await?,
                )?;
                encoded(selection)?
            }
            ProjectCommand::Config(args) => match args.command {
                ProjectConfigCommand::Get { id } => {
                    let snap: ProjectConfigSnapshot = decode(
                        workflow_request(
                            client,
                            origin,
                            &["api", "projects", &id, "config"],
                            None,
                            None,
                        )
                        .await?,
                    )?;
                    encoded(snap)?
                }
                ProjectConfigCommand::Set {
                    id,
                    expected_revision,
                    file,
                } => {
                    let bytes = std::fs::read(file).map_err(|_| {
                        Failure::fixed("request_failed", "无法读取项目配置文件", false)
                    })?;
                    let config: ProjectConfig = serde_json::from_slice(&bytes).map_err(|_| {
                        Failure::fixed("request_failed", "项目配置JSON无效或字段不完整", false)
                    })?;
                    let snap: ProjectConfigSnapshot = decode(
                        workflow_request(
                            client,
                            origin,
                            &["api", "projects", &id, "config"],
                            Some((
                                "configure-project",
                                json!({"expected_revision": expected_revision, "config": config}),
                            )),
                            None,
                        )
                        .await?,
                    )?;
                    encoded(snap)?
                }
            },
        },
        Command::Run(args) => match args.command {
            RunCommand::Start(args) => {
                let body = json!({
                    "title": args.title,
                    "kind": "chat",
                    "tasks": [{
                        "name": "agent",
                        "role": "coding agent",
                        "route_id": args.route,
                        "prompt": args.message,
                        "depends_on": [],
                        "write_scopes": args.scope,
                        "tools": args.tools,
                        "allow_commands": args.allow_commands,
                        "max_rounds": 30
                    }]
                });
                let segments = match args.project.as_deref() {
                    Some(id) => vec!["api", "projects", id, "runs"],
                    None => vec!["api", "runs"],
                };
                let body = match args.expected_config_revision {
                    Some(revision) => {
                        json!({"expected_config_revision": revision, "request": body})
                    }
                    None => body,
                };
                let run: Run = decode(
                    workflow_request(
                        client,
                        origin,
                        &segments,
                        Some(("start", body)),
                        Some(&args.key),
                    )
                    .await?,
                )?;
                run_projection(run)
            }
            RunCommand::List { project } => {
                let segments = match project.as_deref() {
                    Some(id) => vec!["api", "runs", "project-filter", id],
                    None => vec!["api", "runs"],
                };
                let runs: Vec<RunSummary> =
                    decode(workflow_request(client, origin, &segments, None, None).await?)?;
                encoded(runs)?
            }
            RunCommand::Status { id } => {
                let run: Run = decode(
                    workflow_request(client, origin, &["api", "runs", &id], None, None).await?,
                )?;
                run_projection(run)
            }
            RunCommand::Context { id } => {
                let context: ContextView = decode(
                    workflow_request(client, origin, &["api", "runs", &id, "context"], None, None)
                        .await?,
                )?;
                encoded(context)?
            }
            RunCommand::Events(args) => return observe(origin, "runs", args, json_output).await,
        },
        Command::Session(args) => match args.command {
            SessionCommand::Status { id } => {
                let session: Session = decode(
                    workflow_request(client, origin, &["api", "sessions", &id], None, None).await?,
                )?;
                encoded(session)?
            }
            SessionCommand::Turns { id } => {
                let turns: Vec<Turn> = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "sessions", &id, "turns"],
                        None,
                        None,
                    )
                    .await?,
                )?;
                encoded(turns)?
            }
            SessionCommand::Send {
                id,
                agent,
                after_turn,
                message,
                key,
            } => {
                let body = json!({
                    "agent_id": agent,
                    "expected_last_turn_id": after_turn,
                    "message": message,
                });
                let receipt: TurnReceiptView = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "sessions", &id, "turns"],
                        Some(("send", body)),
                        Some(&key),
                    )
                    .await?,
                )?;
                encoded(receipt)?
            }
            SessionCommand::Events(args) => {
                return observe(origin, "sessions", args, json_output).await;
            }
        },
        Command::Checkpoint(args) => match args.command {
            CheckpointCommand::Get { id } => {
                workflow_request(client, origin, &["api", "checkpoints", &id], None, None).await?
            }
            CheckpointCommand::Restore { id } => {
                let view: RestoreView = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "checkpoints", &id, "restore"],
                        Some(("restore-checkpoint", json!({}))),
                        None,
                    )
                    .await?,
                )?;
                encoded(view)?
            }
        },
        Command::Task(args) => match args.command {
            TaskCommand::Project { id } => {
                #[derive(Deserialize, Serialize)]
                struct BindingView {
                    binding: Option<TaskProjectBinding>,
                    status: String,
                }
                let view: BindingView = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "tasks", &id, "project"],
                        None,
                        None,
                    )
                    .await?,
                )?;
                encoded(view)?
            }
            TaskCommand::Diff { id, path, view } => {
                workflow_request(
                    client,
                    origin,
                    &["api", "tasks", &id, "git-diff"],
                    Some(("diff", json!({"path": path, "view": view}))),
                    None,
                )
                .await?
            }
            TaskCommand::Checkpoint { id, key } => {
                workflow_request(
                    client,
                    origin,
                    &["api", "tasks", &id, "checkpoints"],
                    Some(("checkpoint", json!({}))),
                    Some(&key),
                )
                .await?
            }
            TaskCommand::Resume { id, message } => {
                let task: Task = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "tasks", &id, "resume"],
                        Some(("resume", json!({"message": message}))),
                        None,
                    )
                    .await?,
                )?;
                task_projection(task)
            }
            TaskCommand::Cancel { id } => {
                workflow_request(
                    client,
                    origin,
                    &["api", "tasks", &id, "cancel"],
                    Some(("cancel", json!({}))),
                    None,
                )
                .await?
            }
            TaskCommand::Changes { id } => {
                let view: ChangesView = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "tasks", &id, "changes"],
                        None,
                        None,
                    )
                    .await?,
                )?;
                encoded(view)?
            }
            TaskCommand::Restore { id } => {
                let view: RestoreView = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "tasks", &id, "restore"],
                        Some(("restore", json!({}))),
                        None,
                    )
                    .await?,
                )?;
                encoded(view)?
            }
            TaskCommand::Approvals { .. } | TaskCommand::NetworkCalls { .. } => unreachable!(),
        },
        _ => unreachable!(),
    };
    print_value(value, json_output)
}

#[derive(Default)]
struct EventFrame {
    id: Option<String>,
    kind: Option<String>,
    data: Vec<String>,
    size: usize,
}

struct EventReader {
    line: Vec<u8>,
    frame: EventFrame,
    last_cr: bool,
}

impl EventReader {
    fn new() -> Self {
        Self {
            line: vec![],
            frame: EventFrame::default(),
            last_cr: false,
        }
    }
    fn byte(&mut self, byte: u8) -> Result<Option<EventFrame>, Failure> {
        if self.last_cr && byte == b'\n' {
            self.last_cr = false;
            return Ok(None);
        }
        self.last_cr = byte == b'\r';
        if byte != b'\r' && byte != b'\n' {
            self.line.push(byte);
            if self.line.len().saturating_add(self.frame.size) > FRAME_LIMIT {
                return Err(invalid_response());
            }
            return Ok(None);
        }
        let bytes = std::mem::take(&mut self.line);
        let line = std::str::from_utf8(&bytes).map_err(|_| invalid_response())?;
        if line.is_empty() {
            let frame = std::mem::take(&mut self.frame);
            return Ok((!frame.data.is_empty()).then_some(frame));
        }
        self.frame.size += bytes.len() + 1;
        if self.frame.size > FRAME_LIMIT {
            return Err(invalid_response());
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match name {
            "id" if self.frame.id.is_none() && decimal_cursor(value).is_ok() => {
                self.frame.id = Some(value.into())
            }
            "event" if self.frame.kind.is_none() => self.frame.kind = Some(value.into()),
            "data" => self.frame.data.push(value.into()),
            "id" | "event" => return Err(invalid_response()),
            _ => {}
        }
        Ok(None)
    }
}

async fn observe(
    origin: &Url,
    collection: &str,
    args: EventsArgs,
    json_output: bool,
) -> Result<(), Failure> {
    let mut last = args.after;
    let result = observe_inner(origin, collection, &args, json_output, &mut last).await;
    result.map_err(|mut error| {
        error.after = Some(last);
        error
    })
}

async fn observe_inner(
    origin: &Url,
    collection: &str,
    args: &EventsArgs,
    json_output: bool,
    last: &mut i64,
) -> Result<(), Failure> {
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| invalid_response())?;
    let mut url = endpoint(origin, &["api", collection, &args.id, "events"])?;
    url.query_pairs_mut()
        .append_pair("after", &args.after.to_string());
    let response = tokio::time::timeout(Duration::from_secs(4), client.get(url).send())
        .await
        .map_err(|_| Failure::fixed("unavailable", "连接事件流超时", true))?
        .map_err(|_| Failure::fixed("unavailable", "无法连接事件流", true))?;
    if !response.status().is_success() {
        let bytes = tokio::time::timeout(Duration::from_secs(4), limited_body(response))
            .await
            .map_err(|_| invalid_response())??;
        let value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
        return Err(http_failure(value)?);
    }
    let mut stream = response.bytes_stream();
    let mut reader = EventReader::new();
    let mut count = 0u64;
    loop {
        let next = tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|_| invalid_response())?;
                return Ok(());
            },
            next = tokio::time::timeout(Duration::from_secs(30), stream.next()) => next
                .map_err(|_| Failure::fixed("unavailable", "事件流停滞，请手动按游标重连", true))?,
        };
        let chunk = next
            .ok_or_else(|| Failure::fixed("unavailable", "事件流断开，请手动按游标重连", true))?
            .map_err(|_| Failure::fixed("unavailable", "事件流读取失败，请手动按游标重连", true))?;
        for byte in chunk {
            if let Some(frame) = reader.byte(byte)? {
                let value: Value =
                    serde_json::from_str(&frame.data.join("\n")).map_err(|_| invalid_response())?;
                if frame.kind.as_deref() == Some("error") {
                    let message = value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("事件流错误");
                    let retryable = value
                        .get("retryable")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    return Err(Failure {
                        code: "internal",
                        message: message.into(),
                        retryable,
                        details: None,
                        after: Some(*last),
                    });
                }
                let id = decimal_cursor(frame.id.as_deref().ok_or_else(invalid_response)?)
                    .map_err(|_| invalid_response())?;
                let event: Event = decode(value)?;
                print_value(encoded(event)?, json_output)?;
                *last = id;
                count += 1;
                if args.limit.is_some_and(|limit| count >= limit) {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod workbench_unit_tests {
    use super::*;

    #[test]
    fn extract_mention_tokens_ignores_emails_and_handles_punctuation() {
        let tokens = extract_mention_tokens(
            "Contact foo@bar.com or user_1@example.org, then inspect (@src/lib.rs:10-40) and @src/.",
        );
        assert_eq!(tokens, vec!["src/lib.rs:10-40", "src/"]);
    }

    #[test]
    fn expand_context_mentions_attaches_files_ranges_and_dirs_safely() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "line_one\nline_two\nline_three\nline_four\n",
        )
        .unwrap();
        std::fs::write(temp.path().join(".env"), "SECRET=123").unwrap();

        let enriched = expand_context_mentions(
            temp.path(),
            "Explain @src/lib.rs:2-3 and @src/ while ignoring @.env and @missing.rs and dev@team.io",
            true,
        );
        assert!(enriched.contains("<context_files>"));
        assert!(enriched.contains("<file path=\"src/lib.rs\" lines=\"2-3\">"));
        assert!(enriched.contains("2: line_two\n3: line_three\n"));
        assert!(!enriched.contains("1: line_one"));
        assert!(!enriched.contains("4: line_four"));
        assert!(enriched.contains("<directory path=\"src/\">\nlib.rs\n</directory>"));
        assert!(!enriched.contains("SECRET=123"));
    }

    #[test]
    fn resolve_init_provider_handles_explicit_base_url_and_env_fallbacks() {
        let empty_env = |_: &str| -> Option<String> { None };
        assert_eq!(
            resolve_init_provider(Some(InitProvider::Anthropic), None, empty_env),
            InitProvider::Anthropic
        );
        assert_eq!(
            resolve_init_provider(None, Some("https://api.anthropic.com/v1"), |_| Some(
                "sk-openai".into()
            )),
            InitProvider::Anthropic
        );
        assert_eq!(
            resolve_init_provider(None, Some("http://127.0.0.1:9000/v1beta"), |_| Some(
                "sk-openai".into()
            )),
            InitProvider::Gemini
        );
        assert_eq!(
            resolve_init_provider(None, None, |k| match k {
                "ANTHROPIC_API_KEY" => Some("sk-ant-123".into()),
                "GEMINI_API_KEY" => Some("AIza-123".into()),
                _ => None,
            }),
            InitProvider::Anthropic
        );
        assert_eq!(
            resolve_init_provider(None, None, |k| match k {
                "GEMINI_API_KEY" => Some("AIza-123".into()),
                _ => None,
            }),
            InitProvider::Gemini
        );
        assert_eq!(
            resolve_init_provider(None, None, empty_env),
            InitProvider::Openai
        );
    }

    #[test]
    fn resolve_zero_init_plan_supports_all_three_providers_and_model_hints() {
        // 1. Explicit route selects provider preset
        let ant = resolve_zero_init_plan("anthropic", Some("anthropic"), None, |k| match k {
            "OPENAI_API_KEY" => Some("sk-openai".into()),
            "ANTHROPIC_API_KEY" => Some("sk-ant-key".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(ant.preset.provider, InitProvider::Anthropic);
        assert_eq!(ant.preset.key_env, "ANTHROPIC_API_KEY");
        assert_eq!(ant.base_url, "https://api.anthropic.com/v1");
        assert_eq!(ant.model, "claude-sonnet-4-20250514");
        assert_eq!(ant.api_key.as_str(), "sk-ant-key");

        // 2. Model hint `claude-*` or `models/gemini-*` overrides OPENAI_API_KEY fallback
        let claude_hint = resolve_zero_init_plan(
            "default",
            None,
            Some("Claude-Opus-4-20250514"),
            |k| match k {
                "OPENAI_API_KEY" => Some("sk-openai".into()),
                "ANTHROPIC_API_KEY" => Some("sk-ant-hint".into()),
                "ANTHROPIC_BASE_URL" => Some("http://127.0.0.1:4100/anthropic/v1".into()),
                _ => None,
            },
        )
        .unwrap();
        assert_eq!(claude_hint.preset.provider, InitProvider::Anthropic);
        assert_eq!(claude_hint.base_url, "http://127.0.0.1:4100/anthropic/v1");
        assert_eq!(claude_hint.model, "Claude-Opus-4-20250514");

        let gemini_env_hint = resolve_zero_init_plan("default", None, None, |k| match k {
            "OPENAI_API_KEY" => Some("sk-openai".into()),
            "GEMINI_API_KEY" => Some("AIza-gem".into()),
            "CODA_MODEL" => Some("models/gemini-2.5-flash".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(gemini_env_hint.preset.provider, InitProvider::Gemini);
        assert_eq!(gemini_env_hint.preset.key_env, "GEMINI_API_KEY");
        assert_eq!(
            gemini_env_hint.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(gemini_env_hint.model, "models/gemini-2.5-flash");

        // 3. Fallback priority: OPENAI_API_KEY -> ANTHROPIC_API_KEY -> GEMINI_API_KEY
        let fallback_gem = resolve_zero_init_plan("default", None, None, |k| match k {
            "GEMINI_API_KEY" => Some("AIza-only".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(fallback_gem.preset.provider, InitProvider::Gemini);
        assert_eq!(fallback_gem.model, "gemini-2.5-pro");

        // 4. Missing keys produce actionable error mentioning coda init and all three env vars
        let err = resolve_zero_init_plan("default", None, None, |_| None)
            .err()
            .unwrap();
        assert_eq!(err.code, "route_not_configured");
        assert!(err.message.contains("coda init"));
        assert!(err.message.contains("OPENAI_API_KEY"));
        assert!(err.message.contains("ANTHROPIC_API_KEY"));
        assert!(err.message.contains("GEMINI_API_KEY"));
    }

    #[test]
    fn read_repl_input_joins_backslash_multiline_continuations() {
        let mut cursor = io::Cursor::new(
            b"Explain this:\\\r\nfn main() {\\\n    println!(\"hi\");\n}\n" as &[u8],
        );
        let mut continuations = 0usize;
        let joined = read_repl_input(&mut cursor, || {
            continuations += 1;
        })
        .unwrap();
        assert_eq!(continuations, 2);
        assert_eq!(joined, "Explain this:\nfn main() {\n    println!(\"hi\");");

        let closing = read_repl_input(&mut cursor, || {}).unwrap();
        assert_eq!(closing, "}");

        assert!(read_repl_input(&mut cursor, || {}).is_none());
    }
}
