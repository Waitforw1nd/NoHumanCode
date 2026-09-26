use clap::{Args, Subcommand, ValueEnum};
use futures_util::StreamExt;
use peachsh::network_authorization::{
    CallReceipt, CallResult, CallState, DispatchState, NetworkAuthorization, NetworkDecision,
    NetworkPreview, NetworkPurpose, NetworkSubject, NetworkSubjectKind,
};
use peachsh::{
    approval::{ApprovalStatus, CommandNetworkAck, CommandNetworkNotice},
    project_config::{ProjectConfig, ProjectConfigSnapshot, ProjectSelection, TaskProjectBinding},
    secrets,
};
use peachsh::{
    domain::{Event, Project, ProjectId, Run, Session, Task, Turn, TurnTask},
    workspace_changes::{RestoreReceipt, RestoreStatus, WorkspaceChange},
};
use reqwest::{Client, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    time::Duration,
};

const RESPONSE_LIMIT: usize = 1_048_576;

#[derive(Subcommand)]
pub enum Command {
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

fn valid_id(id: &str) -> bool {
    secrets::validate_persisted_id("id", id).is_ok() && !matches!(id, "." | "..")
}
fn valid_message(message: &str) -> bool {
    !message.trim().is_empty() && message.len() <= 100_000
}
impl Command {
    pub fn valid_ids(&self) -> bool {
        match self {
            Self::Account(_) => true,
            Self::Project(args) => match &args.command {
                ProjectCommand::List | ProjectCommand::Current => true,
                ProjectCommand::Get { id } | ProjectCommand::Select { id, .. } => valid_id(id),
                ProjectCommand::Create { name, root, key } => {
                    !name.trim().is_empty()
                        && !root.trim().is_empty()
                        && secrets::validate_idempotency_key(key).is_ok()
                        && !key.contains(',')
                }
                ProjectCommand::Config(args) => match &args.command {
                    ProjectConfigCommand::Get { id } | ProjectConfigCommand::Set { id, .. } => {
                        valid_id(id)
                    }
                },
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
                        && valid_digest(binding)
                        && secrets::validate_idempotency_key(key).is_ok()
                        && !key.contains(',')
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
                            .all(|scope| peachsh::domain::scope_path(scope).is_ok())
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
                        && secrets::validate_idempotency_key(key).is_ok()
                        && !key.contains(',')
                }
            },
            Self::Task(task) => match &task.command {
                TaskCommand::Approvals { task_id } => valid_id(task_id),
                TaskCommand::NetworkCalls { id } | TaskCommand::Project { id } => valid_id(id),
                TaskCommand::Diff { id, path, .. } => {
                    valid_id(id) && peachsh::domain::scope_path(path).is_ok()
                }
                TaskCommand::Checkpoint { id, key } => {
                    valid_id(id)
                        && secrets::validate_idempotency_key(key).is_ok()
                        && !key.contains(',')
                }
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
#[derive(Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
}
#[derive(Subcommand)]
enum ProjectCommand {
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
        file: std::path::PathBuf,
    },
}
#[derive(Args)]
pub struct RunArgs {
    #[command(subcommand)]
    command: RunCommand,
}
#[derive(Subcommand)]
enum RunCommand {
    /// Create in an explicit project, or use the Host default when both project flags are omitted.
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
    /// Explicitly authorize requesting local commands; every command still needs approval.
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
    /// Review a real Git patch; redacted patches are not applicable.
    Diff {
        id: String,
        #[arg(long)]
        path: String,
        #[arg(long, value_enum, default_value = "head")]
        view: DiffView,
    },
    /// Save this completed task's original file bytes, not a directory snapshot.
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
        /// 此命令可能访问任意网络，当前没有OS出站沙箱；明确确认此风险。
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
#[derive(Args)]
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
        config_revision: u64,
        #[arg(long)]
        credential_revision: u64,
        #[arg(long)]
        binding: String,
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

    pub fn print(&self, json_output: bool) {
        let mut value = json!({"code":self.code,"message":self.message,"retryable":self.retryable,"error":self.message});
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
    error: String,
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

pub async fn execute(port: u16, json_output: bool, command: Command) -> Result<(), Failure> {
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|_| Failure::fixed("internal", "无法初始化本机客户端", false))?;
    let origin = Url::parse(&format!("http://127.0.0.1:{port}/"))
        .map_err(|_| Failure::fixed("internal", "本机服务地址无效", false))?;

    let command = match command {
        Command::Network(_)
        | Command::Route(_)
        | Command::Account(_)
        | Command::Task(TaskArgs {
            command: TaskCommand::NetworkCalls { .. },
        }) => return network_workflow(&client, &origin, json_output, command).await,
        Command::Approval(args) => Command::Approval(args),
        Command::Task(TaskArgs {
            command: TaskCommand::Approvals { task_id },
        }) => Command::Task(TaskArgs {
            command: TaskCommand::Approvals { task_id },
        }),
        other => return workflow(&client, &origin, json_output, other).await,
    };
    let (value, shape) = match command {
        Command::Task(task) => match task.command {
            TaskCommand::Approvals { task_id } => {
                let url = endpoint(&origin, &["api", "tasks", &task_id, "approvals"])?;
                (
                    request_json(client.get(url), false, None).await?,
                    SuccessShape::List,
                )
            }
            _ => unreachable!(),
        },
        Command::Approval(approval) => match approval.command {
            ApprovalCommand::Get { approval_id } => {
                let url = endpoint(&origin, &["api", "approvals", &approval_id])?;
                (
                    request_json(client.get(url), false, None).await?,
                    SuccessShape::Single,
                )
            }
            ApprovalCommand::Approve {
                approval_id,
                acknowledge_command_network,
            } => (
                decide(
                    &client,
                    &origin,
                    &approval_id,
                    "approve",
                    acknowledge_command_network,
                )
                .await?,
                SuccessShape::Single,
            ),
            ApprovalCommand::Deny { approval_id } => (
                decide(&client, &origin, &approval_id, "deny", false).await?,
                SuccessShape::Single,
            ),
        },
        _ => unreachable!(),
    };

    let value = normalize_success(value, shape)?;
    if json_output {
        println!("{value}");
    } else if let Some(approvals) = value.get("approvals").and_then(Value::as_array) {
        if approvals.is_empty() {
            println!("没有审批请求");
        } else {
            for approval in approvals {
                print_approval(approval)?;
            }
        }
    } else {
        print_approval(&value)?;
    }
    Ok(())
}

async fn decide(
    client: &Client,
    origin: &Url,
    id: &str,
    decision: &str,
    acknowledge_command_network: bool,
) -> Result<Value, Failure> {
    let bootstrap = endpoint(origin, &["api", "bootstrap"])?;
    let value = request_bootstrap(client.get(bootstrap)).await?;
    let bootstrap: Bootstrap = serde_json::from_value(value)
        .map_err(|_| Failure::fixed("internal", "本机服务 bootstrap 响应无效", false))?;
    if bootstrap.token.is_empty() || bootstrap.token.len() > 4096 {
        return Err(Failure::fixed(
            "internal",
            "本机服务 bootstrap 响应无效",
            false,
        ));
    }
    let url = endpoint(origin, &["api", "approvals", id, "decision"])?;
    request_json(
        client
            .post(url)
            .header("x-peachsh-token", &bootstrap.token)
            .json(&json!({"decision":decision,"acknowledge_command_network":acknowledge_command_network})),
        true,
        Some(&bootstrap.token),
    )
    .await
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

fn endpoint(origin: &Url, segments: &[&str]) -> Result<Url, Failure> {
    let mut url = origin.clone();
    url.path_segments_mut()
        .map_err(|_| Failure::fixed("internal", "本机服务地址无效", false))?
        .clear()
        .extend(segments);
    Ok(url)
}

async fn request_json(
    request: reqwest::RequestBuilder,
    decision_post: bool,
    known_token: Option<&str>,
) -> Result<Value, Failure> {
    let response = request.send().await.map_err(|error| {
        if decision_post {
            return decision_unknown();
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
            decision_unknown()
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
    if value.get("error").is_some_and(Value::is_object) {
        return Err(http_failure(value)?);
    }
    let error: HttpError = serde_json::from_value(value)
        .map_err(|_| Failure::fixed("internal", "本机服务返回无效错误响应", false))?;
    if error.error != error.message || !error.code.is_string() {
        return Err(Failure::fixed(
            "internal",
            "本机服务返回无效错误响应",
            false,
        ));
    }
    if secrets::scrub(&error.message, "") != error.message {
        return Err(Failure::fixed(
            "internal",
            "本机服务返回不安全错误响应",
            false,
        ));
    }
    let code = match error.code.as_str().unwrap() {
        "request_failed" => "request_failed",
        "not_found" => "not_found",
        "conflict" => "conflict",
        "forbidden" => "forbidden",
        "unavailable" => "unavailable",
        "internal" => "internal",
        _ => {
            return Err(Failure::fixed(
                "internal",
                "本机服务返回无效错误响应",
                false,
            ));
        }
    };
    let _ = status;
    Err(Failure {
        code,
        message: error.message,
        retryable: error.retryable,
        details: None,
        after: None,
    })
}

fn decision_unknown() -> Failure {
    Failure::fixed(
        "decision_result_unknown",
        "审批决定结果未知，请用 approval get 查询",
        false,
    )
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

#[derive(Clone, Copy)]
enum SuccessShape {
    List,
    Single,
}

fn normalize_success(value: Value, shape: SuccessShape) -> Result<Value, Failure> {
    if matches!(shape, SuccessShape::List) {
        let list: ApprovalList = serde_json::from_value(value)
            .map_err(|_| Failure::fixed("internal", "本机服务审批响应无效", false))?;
        for approval in &list.approvals {
            validate_approval_fields(approval)?;
        }
        return serde_json::to_value(list)
            .map_err(|_| Failure::fixed("internal", "本机服务审批响应无效", false));
    }
    let approval: ApprovalDto = serde_json::from_value(value)
        .map_err(|_| Failure::fixed("internal", "本机服务审批响应无效", false))?;
    validate_approval_fields(&approval)?;
    serde_json::to_value(approval)
        .map_err(|_| Failure::fixed("internal", "本机服务审批响应无效", false))
}

fn contains_string(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.contains(needle),
        Value::Array(values) => values.iter().any(|value| contains_string(value, needle)),
        Value::Object(values) => {
            values.keys().any(|key| key.contains(needle))
                || values.values().any(|value| contains_string(value, needle))
        }
        _ => false,
    }
}

fn validate_approval(value: &Value) -> Result<(), Failure> {
    let approval: ApprovalDto = serde_json::from_value(value.clone())
        .map_err(|_| Failure::fixed("internal", "本机服务审批响应无效", false))?;
    validate_approval_fields(&approval)
}

fn validate_approval_fields(value: &ApprovalDto) -> Result<(), Failure> {
    if secrets::validate_persisted_id("approval_id", &value.id).is_err()
        || secrets::validate_persisted_id("task_id", &value.task_id).is_err()
        || secrets::validate_persisted_id("tool_call_id", &value.tool_call_id).is_err()
        || value
            .session_id
            .as_deref()
            .is_some_and(|id| secrets::validate_persisted_id("session_id", id).is_err())
        || value
            .turn_id
            .as_deref()
            .is_some_and(|id| secrets::validate_persisted_id("turn_id", id).is_err())
        || secrets::scrub(&value.preview, "") != value.preview
        || secrets::scrub(&value.tool_name, "") != value.tool_name
        || value
            .decided_by
            .as_deref()
            .is_some_and(|by| secrets::scrub(by, "") != by)
        || ApprovalStatus::parse(&value.status).is_none()
        || !matches!(
            value.execution_state.as_str(),
            "not_started" | "claimed" | "finished" | "unknown" | "cancelled"
        )
    {
        return Err(Failure::fixed("internal", "本机服务审批响应无效", false));
    }
    Ok(())
}

fn print_approval(value: &Value) -> Result<(), Failure> {
    validate_approval(value)?;
    if value["tool_name"] == "run_command" {
        println!(
            "此命令可能访问任意网络，当前没有OS出站沙箱；确认状态：{}",
            value["command_network_ack"]
        );
    }
    println!(
        "{}\t{}\t{}",
        value["id"].as_str().unwrap(),
        value["status"].as_str().unwrap(),
        value["execution_state"].as_str().unwrap()
    );
    Ok(())
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
fn safe_strings(value: &Value) -> bool {
    match value {
        Value::String(_) => secrets::redact_persisted(value) == *value,
        Value::Array(items) => items.iter().all(safe_strings),
        Value::Object(items) => items.iter().all(|(key, item)| {
            secrets::redact_persisted(&json!(key)) == json!(key) && safe_strings(item)
        }),
        _ => true,
    }
}
fn print_value(value: Value, json_output: bool) -> Result<(), Failure> {
    if !safe_strings(&value) {
        return Err(invalid_response());
    }
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
fn task_projection(task: Task) -> Result<Value, Failure> {
    if !valid_id(&task.id) || !valid_id(&task.run_id) {
        return Err(invalid_response());
    }
    Ok(
        json!({"id":task.id,"run_id":task.run_id,"name":task.spec.name,"role":task.spec.role,
        "status":task.status,"output":task.output,"error":task.error,"workspace":task.workspace,
        "route_id":task.route.id,"tools":task.spec.tools,"write_scopes":task.spec.write_scopes,
        "allow_commands":task.spec.allow_commands,"created_at":task.created_at,"updated_at":task.updated_at}),
    )
}
fn run_projection(run: Run) -> Result<Value, Failure> {
    if !valid_id(&run.id) || run.tasks.iter().any(|task| task.run_id != run.id) {
        return Err(invalid_response());
    }
    let tasks: Vec<_> = run
        .tasks
        .into_iter()
        .map(task_projection)
        .collect::<Result<_, _>>()?;
    Ok(
        json!({"id":run.id,"run_id":run.id,"title":run.title,"kind":run.kind,"created_at":run.created_at,"tasks":tasks}),
    )
}
#[derive(Deserialize, Serialize)]
struct RunSummary {
    id: String,
    title: String,
    kind: peachsh::domain::SessionKind,
    created_at: u64,
}
// Wire projection, not a second domain command or a Store implementation.
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
#[derive(Deserialize, Serialize)]
struct DiffViewResponse {
    diff: DiffDto,
}
#[derive(Deserialize, Serialize)]
struct DiffDto {
    path: String,
    view: DiffView,
    base_oid: Option<String>,
    index_oid: Option<String>,
    before_digest: Option<String>,
    after_digest: Option<String>,
    status: String,
    patch: Option<String>,
    redacted: bool,
}
#[derive(Deserialize, Serialize)]
struct CheckpointDto {
    checkpoint_id: String,
    task_id: String,
    kind: String,
    generation: u32,
    created_at: u64,
    manifest_digest: String,
    entries: Vec<CheckpointEntryDto>,
}
#[derive(Deserialize, Serialize)]
struct CheckpointEntryDto {
    path: String,
    before_digest: Option<String>,
    after_digest: String,
}
#[derive(Deserialize, Serialize)]
struct CheckpointView {
    checkpoint: CheckpointDto,
}
#[derive(Deserialize, Serialize)]
struct CheckpointCreatedView {
    checkpoint: CheckpointDto,
    replayed: bool,
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn validate_checkpoint(cp: &CheckpointDto) -> Result<(), Failure> {
    if !valid_id(&cp.checkpoint_id)
        || !valid_id(&cp.task_id)
        || cp.kind != "task_before"
        || cp.generation != 1
        || !valid_digest(&cp.manifest_digest)
        || cp.entries.is_empty()
        || cp.entries.len() > 128
        || cp.entries.iter().any(|entry| {
            peachsh::domain::scope_path(&entry.path).is_err()
                || !valid_digest(&entry.after_digest)
                || entry
                    .before_digest
                    .as_deref()
                    .is_some_and(|v| !valid_digest(v))
        })
    {
        return Err(invalid_response());
    }
    Ok(())
}
async fn get_checkpoint(
    client: &Client,
    origin: &Url,
    id: &str,
) -> Result<CheckpointView, Failure> {
    let result: CheckpointView =
        decode(workflow_request(client, origin, &["api", "checkpoints", id], None, None).await?)?;
    validate_checkpoint(&result.checkpoint)?;
    if result.checkpoint.checkpoint_id != id {
        return Err(invalid_response());
    }
    Ok(result)
}
fn validate_receipt(receipt: &RestoreReceipt, task: &str) -> Result<(), Failure> {
    if receipt.task_id != task
        || !valid_id(task)
        || receipt.restore_id.as_ref().is_some_and(|id| !valid_id(id))
        || receipt.restored
            != receipt
                .outcomes
                .iter()
                .filter(|item| item.status == RestoreStatus::Complete)
                .count()
        || receipt.outcomes.iter().any(|item| {
            !valid_id(&item.change_id) || peachsh::domain::scope_path(&item.path).is_err()
        })
        || (receipt.status == RestoreStatus::Complete
            && receipt
                .outcomes
                .iter()
                .any(|item| item.status != RestoreStatus::Complete))
        || (receipt.restore_id.is_none()
            && (!receipt.outcomes.is_empty() || receipt.status != RestoreStatus::Complete))
    {
        return Err(invalid_response());
    }
    Ok(())
}
fn restore_projection(value: Value, task: &str, success: bool) -> Result<Value, Failure> {
    let view: RestoreView = decode(value)?;
    validate_receipt(&view.receipt, task)?;
    if view.status != view.receipt.status
        || view.restored != view.receipt.restored
        || view.ok != (view.status == RestoreStatus::Complete)
        || view.ok != success
    {
        return Err(invalid_response());
    }
    encoded(view)
}
fn operation_unknown(operation: &str) -> Failure {
    let message = match operation {
        "start" => "启动结果未知，请查询 run list；仅可用原参数和原 key 手动查询式重放",
        "send" => "新回合结果未知，请查询 session turns；仅可用原参数和原 key 手动重放",
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
fn project_error_code(code: &str) -> Option<&'static str> {
    match code {
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
fn http_failure(value: Value) -> Result<Failure, Failure> {
    if value.get("error").is_some_and(Value::is_object) {
        return network_http_failure(value);
    }
    let error: HttpError = decode(value)?;
    if error.error != error.message || !safe_strings(&json!(error.message)) {
        return Err(invalid_response());
    }
    let code = match error.code.as_str() {
        Some("request_failed") => "request_failed",
        Some("not_found") => "not_found",
        Some("conflict") => "conflict",
        Some("forbidden") => "forbidden",
        Some("unavailable") => "unavailable",
        Some("internal") => "internal",
        other => project_error_code(other.unwrap_or("")).ok_or_else(invalid_response)?,
    };
    Ok(Failure {
        code,
        message: error.message,
        retryable: error.retryable,
        details: None,
        after: None,
    })
}
async fn workflow_request(
    client: &Client,
    origin: &Url,
    segments: &[&str],
    write: Option<(&str, Value)>,
    key: Option<&str>,
) -> Result<Value, Failure> {
    workflow_request_bound(client, origin, segments, write, key, None).await
}
async fn workflow_request_bound(
    client: &Client,
    origin: &Url,
    segments: &[&str],
    write: Option<(&str, Value)>,
    key: Option<&str>,
    restore_task: Option<&str>,
) -> Result<Value, Failure> {
    let mut token = None;
    let operation = write
        .as_ref()
        .map(|(operation, _)| *operation)
        .filter(|op| *op != "diff");
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
        let bootstrap: Bootstrap =
            decode(request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?)).await?)?;
        if bootstrap.token.is_empty() || bootstrap.token.len() > 4096 {
            return Err(invalid_response());
        }
        let mut request = if body.is_none() {
            client.get(url)
        } else if matches!(operation, Some("select-project" | "configure-project")) {
            client.put(url)
        } else {
            client.post(url)
        };
        request = request.header("x-peachsh-token", &bootstrap.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        token = Some(bootstrap.token);
        request
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
        .is_some_and(|token| contains_string(&value, token))
    {
        return Err(invalid_response());
    }
    if status.is_success() {
        return Ok(value);
    }
    let mut failure = http_failure(value.clone())?;
    if matches!(operation, Some("restore" | "restore-checkpoint")) && value.get("receipt").is_some()
    {
        if status != reqwest::StatusCode::CONFLICT
            || failure.code != "conflict"
            || failure.retryable
        {
            return Err(invalid_response());
        }
        failure.details = Some(restore_projection(
            value,
            restore_task.unwrap_or(segments[2]),
            false,
        )?);
        if !safe_strings(failure.details.as_ref().unwrap()) {
            return Err(invalid_response());
        }
    }
    Err(failure)
}
async fn workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: Command,
) -> Result<(), Failure> {
    let operation = match &command {
        Command::Run(RunArgs {
            command: RunCommand::Start(_),
        }) => Some("start"),
        Command::Session(SessionArgs {
            command: SessionCommand::Send { .. },
        }) => Some("send"),
        Command::Task(TaskArgs {
            command: TaskCommand::Resume { .. },
        }) => Some("resume"),
        Command::Task(TaskArgs {
            command: TaskCommand::Cancel { .. },
        }) => Some("cancel"),
        Command::Task(TaskArgs {
            command: TaskCommand::Restore { .. },
        }) => Some("restore"),
        Command::Task(TaskArgs {
            command: TaskCommand::Checkpoint { .. },
        }) => Some("checkpoint"),
        Command::Checkpoint(CheckpointArgs {
            command: CheckpointCommand::Restore { .. },
        }) => Some("restore-checkpoint"),
        _ => None,
    };
    workflow_inner(client, origin, json_output, command)
        .await
        .map_err(|error| {
            // An invalid/unprintable reply cannot prove whether a write took effect.
            // Known bootstrap/HTTP failures retain their classification; no request is replayed.
            if (error.code == "output_failed"
                || (error.code == "internal" && error.message == invalid_response().message))
                && error.details.is_none()
                && let Some(operation) = operation
            {
                return operation_unknown(operation);
            }
            error
        })
}
fn validate_project(project: &Project) -> Result<(), Failure> {
    if !valid_id(&project.id.0)
        || project.name.trim().is_empty()
        || project.root_path.is_empty()
        || !safe_strings(&encoded(project)?)
    {
        return Err(invalid_response());
    }
    Ok(())
}
fn validate_selection(selection: &ProjectSelection) -> Result<(), Failure> {
    if selection
        .project_id
        .as_ref()
        .is_some_and(|p| !valid_id(&p.0))
        || (selection.revision == 0) != selection.project_id.is_none()
    {
        return Err(invalid_response());
    }
    Ok(())
}
async fn workflow_inner(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: Command,
) -> Result<(), Failure> {
    let value = match command {
        Command::Project(args) => match args.command {
            ProjectCommand::List => {
                let projects: Vec<Project> = decode(
                    workflow_request(client, origin, &["api", "projects"], None, None).await?,
                )?;
                for project in &projects {
                    validate_project(project)?;
                }
                encoded(projects)?
            }
            ProjectCommand::Create { name, root, key } => {
                let project: Project = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "projects"],
                        Some(("register-project", json!({"name":name,"root_path":root}))),
                        Some(&key),
                    )
                    .await?,
                )?;
                validate_project(&project)?;
                encoded(project)?
            }
            ProjectCommand::Get { id } => {
                let project: Project = decode(
                    workflow_request(client, origin, &["api", "projects", &id], None, None).await?,
                )?;
                validate_project(&project)?;
                if project.id.0 != id {
                    return Err(invalid_response());
                }
                encoded(project)?
            }
            ProjectCommand::Current => {
                let selection: ProjectSelection = decode(
                    workflow_request(client, origin, &["api", "project-selection"], None, None)
                        .await?,
                )?;
                validate_selection(&selection)?;
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
                            json!({"project_id":id,"expected_revision":expected_revision}),
                        )),
                        None,
                    )
                    .await?,
                )?;
                validate_selection(&selection)?;
                if selection.project_id.as_ref().map(|p| p.0.as_str()) != Some(id.as_str())
                    || expected_revision.checked_add(1) != Some(selection.revision)
                {
                    return Err(invalid_response());
                }
                encoded(selection)?
            }
            ProjectCommand::Config(args) => {
                let (id, write, expected) = match args.command {
                    ProjectConfigCommand::Get { id } => (id, None, None),
                    ProjectConfigCommand::Set {
                        id,
                        expected_revision,
                        file,
                    } => {
                        let bytes = std::fs::read(file).map_err(|_| {
                            Failure::fixed("request_failed", "无法读取项目配置文件", false)
                        })?;
                        if bytes.len() > RESPONSE_LIMIT {
                            return Err(Failure::fixed(
                                "request_failed",
                                "项目配置文件过大",
                                false,
                            ));
                        }
                        let shape: Value = serde_json::from_slice(&bytes).map_err(|_| {
                            Failure::fixed("request_failed", "项目配置JSON无效", false)
                        })?;
                        if shape.as_object().is_none_or(|object| {
                            object.len() != 5
                                || ![
                                    "allowed_route_ids",
                                    "default_route_id",
                                    "tools",
                                    "write_scopes",
                                    "allow_commands",
                                ]
                                .iter()
                                .all(|key| object.contains_key(*key))
                        }) {
                            return Err(Failure::fixed(
                                "request_failed",
                                "项目配置必须为完整对象",
                                false,
                            ));
                        }
                        let config: ProjectConfig =
                            serde_json::from_slice(&bytes).map_err(|_| {
                                Failure::fixed(
                                    "request_failed",
                                    "项目配置JSON无效或字段不完整",
                                    false,
                                )
                            })?;
                        (
                            id,
                            Some((
                                "configure-project",
                                json!({"expected_revision":expected_revision,"config":config}),
                            )),
                            Some(expected_revision),
                        )
                    }
                };
                let snapshot: ProjectConfigSnapshot = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "projects", &id, "config"],
                        write,
                        None,
                    )
                    .await?,
                )?;
                if snapshot.project_id.0 != id
                    || snapshot.revision == 0
                    || !valid_digest(&snapshot.permission_digest)
                    || expected.is_some_and(|r| r.checked_add(1) != Some(snapshot.revision))
                {
                    return Err(invalid_response());
                }
                encoded(snapshot)?
            }
        },
        Command::Run(args) => match args.command {
            RunCommand::Start(args) => {
                let body = json!({"title":args.title,"kind":"chat","tasks":[{"name":"agent","role":"coding agent",
                    "route_id":args.route,"prompt":args.message,"depends_on":[],"write_scopes":args.scope,
                    "tools":args.tools,"allow_commands":args.allow_commands,"max_rounds":12}]});
                let segments = match args.project.as_deref() {
                    Some(id) => vec!["api", "projects", id, "runs"],
                    None => vec!["api", "runs"],
                };
                let body = match args.expected_config_revision {
                    Some(revision) => json!({"expected_config_revision":revision,"request":body}),
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
                run_projection(run)?
            }
            RunCommand::List { project } => {
                let segments = match project.as_deref() {
                    Some(id) => vec!["api", "runs", "project-filter", id],
                    None => vec!["api", "runs"],
                };
                let runs: Vec<RunSummary> =
                    decode(workflow_request(client, origin, &segments, None, None).await?)?;
                if runs.iter().any(|run| !valid_id(&run.id)) {
                    return Err(invalid_response());
                }
                encoded(runs)?
            }
            RunCommand::Status { id } => {
                let run: Run = decode(
                    workflow_request(client, origin, &["api", "runs", &id], None, None).await?,
                )?;
                if run.id != id {
                    return Err(invalid_response());
                }
                run_projection(run)?
            }
            RunCommand::Context { id } => {
                let context: ContextView = decode(
                    workflow_request(client, origin, &["api", "runs", &id, "context"], None, None)
                        .await?,
                )?;
                if context.run_id != id
                    || context.session.legacy_run_id != id
                    || context.session.project_id != context.project.id
                    || context.latest_turn.session_id != context.session.id
                    || context.latest_turn.project_id != context.project.id
                    || context.tasks.is_empty()
                    || context.tasks.iter().any(|task| {
                        task.turn_id != context.latest_turn.id
                            || task.session_id != context.session.id
                    })
                {
                    return Err(invalid_response());
                }
                encoded(context)?
            }
            RunCommand::Events(args) => return observe(origin, "runs", args, json_output).await,
        },
        Command::Session(args) => match args.command {
            SessionCommand::Status { id } => {
                let session: Session = decode(
                    workflow_request(client, origin, &["api", "sessions", &id], None, None).await?,
                )?;
                if session.id.0 != id {
                    return Err(invalid_response());
                }
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
                if turns.iter().any(|turn| turn.session_id.0 != id) {
                    return Err(invalid_response());
                }
                encoded(turns)?
            }
            SessionCommand::Send {
                id,
                agent,
                after_turn,
                message,
                key,
            } => {
                let body =
                    json!({"agent_id":agent,"expected_last_turn_id":after_turn,"message":message});
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
                if receipt.turn.session_id.0 != id
                    || receipt.task.session_id.0 != id
                    || receipt.task.agent_id.0 != agent
                    || receipt.task.turn_id != receipt.turn.id
                {
                    return Err(invalid_response());
                }
                encoded(receipt)?
            }
            SessionCommand::Events(args) => {
                return observe(origin, "sessions", args, json_output).await;
            }
        },
        Command::Checkpoint(args) => match args.command {
            CheckpointCommand::Get { id } => encoded(get_checkpoint(client, origin, &id).await?)?,
            CheckpointCommand::Restore { id } => {
                let checkpoint = get_checkpoint(client, origin, &id).await?;
                let task = &checkpoint.checkpoint.task_id;
                restore_projection(
                    workflow_request_bound(
                        client,
                        origin,
                        &["api", "checkpoints", &id, "restore"],
                        Some(("restore-checkpoint", json!({}))),
                        None,
                        Some(task),
                    )
                    .await?,
                    task,
                    true,
                )?
            }
        },
        Command::Task(args) => match args.command {
            TaskCommand::Project { id } => {
                #[derive(Deserialize, Serialize)]
                #[serde(deny_unknown_fields)]
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
                if match &view.binding {
                    Some(b) => {
                        view.status != "bound"
                            || b.task_id != id
                            || !valid_id(&b.project_id.0)
                            || b.config_revision == 0
                            || !valid_digest(&b.permission_digest)
                    }
                    None => view.status != "legacy_unbound",
                } {
                    return Err(invalid_response());
                }
                encoded(view)?
            }
            TaskCommand::Diff { id, path, view } => {
                let result: DiffViewResponse = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "tasks", &id, "git-diff"],
                        Some(("diff", json!({"path":path,"view":view}))),
                        None,
                    )
                    .await?,
                )?;
                let diff = &result.diff;
                if diff.path != path.replace('\\', "/")
                    || diff.view != view
                    || !matches!(
                        diff.status.as_str(),
                        "text" | "unchanged" | "binary" | "too_large" | "missing"
                    )
                    || (diff.status == "text") != diff.patch.is_some()
                    || diff.patch.as_ref().is_some_and(|p| p.len() > 524288)
                {
                    return Err(invalid_response());
                }
                encoded(result)?
            }
            TaskCommand::Checkpoint { id, key } => {
                let result: CheckpointCreatedView = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "tasks", &id, "checkpoints"],
                        Some(("checkpoint", json!({}))),
                        Some(&key),
                    )
                    .await?,
                )?;
                validate_checkpoint(&result.checkpoint)?;
                if result.checkpoint.task_id != id {
                    return Err(invalid_response());
                }
                encoded(result)?
            }
            TaskCommand::Resume { id, message } => {
                let task: Task = decode(
                    workflow_request(
                        client,
                        origin,
                        &["api", "tasks", &id, "resume"],
                        Some(("resume", json!({"message":message}))),
                        None,
                    )
                    .await?,
                )?;
                if task.id != id {
                    return Err(invalid_response());
                }
                task_projection(task)?
            }
            TaskCommand::Cancel { id } => {
                let value = workflow_request(
                    client,
                    origin,
                    &["api", "tasks", &id, "cancel"],
                    Some(("cancel", json!({}))),
                    None,
                )
                .await?;
                if value.get("ok") != Some(&json!(true)) {
                    return Err(invalid_response());
                }
                json!({"ok":true})
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
                if view.changes.iter().any(|change| {
                    change.task_id != id
                        || !valid_id(&change.change_id)
                        || peachsh::domain::scope_path(&change.path).is_err()
                }) {
                    return Err(invalid_response());
                }
                if let Some(receipt) = &view.restore {
                    validate_receipt(receipt, &id)?;
                }
                encoded(view)?
            }
            TaskCommand::Restore { id } => restore_projection(
                workflow_request(
                    client,
                    origin,
                    &["api", "tasks", &id, "restore"],
                    Some(("restore", json!({}))),
                    None,
                )
                .await?,
                &id,
                true,
            )?,
            TaskCommand::Approvals { .. } | TaskCommand::NetworkCalls { .. } => unreachable!(),
        },
        Command::Approval(_) | Command::Network(_) | Command::Route(_) | Command::Account(_) => {
            unreachable!()
        }
    };
    print_value(value, json_output)
}

fn validate_subject(subject: &NetworkSubject) -> Result<(), Failure> {
    if !valid_id(&subject.id)
        || (subject.kind == NetworkSubjectKind::Account
            && (subject.id != "newapi" || subject.project_id.is_some()))
        || subject.project_id.as_ref().is_some_and(|p| !valid_id(&p.0))
    {
        return Err(invalid_response());
    }
    Ok(())
}
fn validate_authorization(value: &NetworkAuthorization) -> Result<(), Failure> {
    validate_subject(&value.subject)?;
    if !valid_id(&value.authorization_id)
        || !valid_digest(&value.binding_digest)
        || value.config_revision == 0
        || value.credential_revision == 0
        || value.purposes.is_empty()
    {
        return Err(invalid_response());
    }
    validate_purposes(&value.subject, &value.purposes)
}
fn validate_purposes(subject: &NetworkSubject, purposes: &[NetworkPurpose]) -> Result<(), Failure> {
    if purposes.iter().any(|p| match subject.kind {
        NetworkSubjectKind::Route => matches!(p, NetworkPurpose::Balance),
        NetworkSubjectKind::Account => !matches!(p, NetworkPurpose::Balance),
    }) {
        return Err(invalid_response());
    }
    Ok(())
}
fn validate_network_calls(calls: &[CallReceipt]) -> Result<(), Failure> {
    let mut ids = std::collections::HashSet::new();
    for call in calls {
        validate_subject(&call.subject)?;
        validate_purposes(&call.subject, &[call.purpose])?;
        let target = peachsh::provider_transport::validate_target(&call.origin, call.purpose)
            .map_err(|_| invalid_response())?;
        let origin_url = Url::parse(&call.origin).map_err(|_| invalid_response())?;
        let valid_state = match call.state {
            CallState::Claimed => {
                call.finished_at.is_none()
                    && call.success.is_none()
                    && call.result.is_none()
                    && call.http_status.is_none()
            }
            CallState::Finished => {
                call.finished_at.is_some()
                    && match (call.success, call.result, call.http_status) {
                        (Some(true), Some(CallResult::Success), None) => true,
                        (Some(true), Some(CallResult::Success), Some(status)) => {
                            (200..300).contains(&status)
                        }
                        (Some(false), Some(CallResult::HttpFailure), Some(status)) => {
                            (100..600).contains(&status) && !(200..300).contains(&status)
                        }
                        (Some(false), Some(CallResult::InvalidResponse), Some(status)) => {
                            (200..300).contains(&status)
                        }
                        _ => false,
                    }
            }
            CallState::Unknown => {
                call.http_status.is_none()
                    && call.success.is_none()
                    && matches!(
                        call.result,
                        Some(
                            CallResult::TransportUnknown
                                | CallResult::Interrupted
                                | CallResult::PersistenceUnknown
                        )
                    )
            }
            CallState::NotSent => {
                call.http_status.is_none()
                    && call.finished_at.is_some()
                    && call.success.is_none()
                    && call.result == Some(CallResult::PreflightRejected)
            }
        };
        if !valid_id(&call.call_id)
            || !ids.insert(&call.call_id)
            || !valid_id(&call.authorization_id)
            || !valid_digest(&call.endpoint_digest)
            || call.task_id.as_deref().is_some_and(|id| !valid_id(id))
            || target.origin() != call.origin
            || origin_url.path() != "/"
            || target.method() != call.method
            || call.finished_at.is_some_and(|t| t < call.claimed_at)
            || !valid_state
        {
            return Err(invalid_response());
        }
    }
    if !safe_strings(&encoded(calls)?) {
        return Err(invalid_response());
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkHttpError {
    code: String,
    message: String,
    dispatch_state: DispatchState,
    calls: Vec<CallReceipt>,
    retryable: bool,
}
fn network_http_failure(value: Value) -> Result<Failure, Failure> {
    let error: NetworkHttpError =
        decode(value.get("error").cloned().ok_or_else(invalid_response)?)?;
    validate_network_calls(&error.calls)?;
    if error.retryable
        || !safe_strings(&json!(error.message))
        || (error.dispatch_state == DispatchState::Unknown
            && !error
                .calls
                .iter()
                .any(|c| matches!(c.state, CallState::Unknown | CallState::Claimed)))
        || (error.dispatch_state == DispatchState::Finished && error.calls.is_empty())
    {
        return Err(invalid_response());
    }
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
        other => project_error_code(other).ok_or_else(invalid_response)?,
    };
    Ok(Failure {
        code,
        message: error.message,
        retryable: false,
        details: Some(json!({"dispatch_state":error.dispatch_state,"calls":error.calls})),
        after: None,
    })
}
async fn network_request(
    client: &Client,
    origin: &Url,
    segments: &[&str],
    body: Option<Value>,
    key: Option<&str>,
    mutation: bool,
) -> Result<Value, Failure> {
    let bootstrap: Bootstrap =
        decode(request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?)).await?)?;
    if bootstrap.token.is_empty() || bootstrap.token.len() > 4096 {
        return Err(invalid_response());
    }
    let url = endpoint(origin, segments)?;
    let mut request = match body {
        Some(body) => client.post(url).json(&body),
        None => client.get(url),
    }
    .header("x-peachsh-token", &bootstrap.token);
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
    let invalid = || {
        if mutation {
            operation_unknown("network")
        } else {
            invalid_response()
        }
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if contains_string(&value, &bootstrap.token) || !safe_strings(&value) {
        return Err(invalid());
    }
    if status.is_success() {
        return Ok(value);
    }
    let failure = http_failure(value).map_err(|_| invalid())?;
    let expected = match failure.code {
        "invalid" | "project_invalid" => 400,
        "not_found" | "project_not_found" => 404,
        "required" | "stale" | "revoked" | "conflict" | "unknown" => 409,
        "corrupt" | "internal" | "project_corrupt" | "project_internal" => 500,
        "upstream" => 502,
        code if project_error_code(code).is_some() => 409,
        _ => status.as_u16(),
    };
    if failure.details.is_some() && expected != status.as_u16() {
        return Err(invalid());
    }
    Err(failure)
}
async fn network_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: Command,
) -> Result<(), Failure> {
    let mutation = matches!(
        &command,
        Command::Network(NetworkArgs {
            command: NetworkCommand::Authorize { .. } | NetworkCommand::Revoke { .. }
        }) | Command::Route(_)
            | Command::Account(_)
    );
    let result = network_workflow_inner(client, origin, json_output, command).await;
    result.map_err(|e| {
        if mutation
            && e.details.is_none()
            && (e.code == "output_failed"
                || (e.code == "internal" && e.message == invalid_response().message))
        {
            operation_unknown("network")
        } else {
            e
        }
    })
}
async fn network_workflow_inner(
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
                validate_subject(&preview.subject)?;
                validate_purposes(&preview.subject, &preview.purposes)?;
                if preview.subject != subject
                    || !valid_digest(&preview.binding_digest)
                    || preview.config_revision == 0
                    || preview.credential_revision == 0
                {
                    return Err(invalid_response());
                }
                let purpose = match subject.kind {
                    NetworkSubjectKind::Route => NetworkPurpose::Models,
                    NetworkSubjectKind::Account => NetworkPurpose::Balance,
                };
                let target =
                    peachsh::provider_transport::validate_target(&preview.base_url, purpose)
                        .map_err(|_| invalid_response())?;
                if target.origin() != preview.origin {
                    return Err(invalid_response());
                }
                if let Some(auth) = &preview.authorization {
                    validate_authorization(auth)?;
                    if auth.subject != subject {
                        return Err(invalid_response());
                    }
                }
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
                let mut purposes: Vec<String> = purpose
                    .into_iter()
                    .map(|p| {
                        match p {
                            PurposeArg::Models => "models",
                            PurposeArg::Completion => "completion",
                            PurposeArg::Balance => "balance",
                        }
                        .to_owned()
                    })
                    .collect();
                purposes.sort();
                purposes.dedup();
                let request = json!({"subject":subject,"config_revision":config_revision,"credential_revision":credential_revision,"binding_digest":binding,"purposes":purposes});
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
                validate_authorization(&decision.authorization)?;
                let auth = &decision.authorization;
                let mut actual: Vec<String> = auth
                    .purposes
                    .iter()
                    .map(|p| {
                        serde_json::to_value(p)
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .to_owned()
                    })
                    .collect();
                actual.sort();
                actual.dedup();
                if auth.subject != subject
                    || auth.config_revision != config_revision
                    || auth.credential_revision != credential_revision
                    || auth.binding_digest != binding
                    || actual != purposes
                {
                    return Err(invalid_response());
                }
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
                validate_authorization(&auth)?;
                if auth.authorization_id != id {
                    return Err(invalid_response());
                }
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
                validate_authorization(&auth)?;
                if auth.authorization_id != id
                    || auth.state != peachsh::network_authorization::AuthorizationState::Revoked
                {
                    return Err(invalid_response());
                }
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
                validate_network_calls(std::slice::from_ref(&call))?;
                if call.call_id != id {
                    return Err(invalid_response());
                }
                encoded(call)?
            }
        },
        Command::Task(TaskArgs {
            command: TaskCommand::NetworkCalls { id },
        }) => {
            #[derive(Deserialize, Serialize)]
            #[serde(deny_unknown_fields)]
            struct Calls {
                calls: Vec<CallReceipt>,
            }
            let calls: Calls = decode(
                network_request(
                    client,
                    origin,
                    &["api", "tasks", &id, "network-calls"],
                    None,
                    None,
                    false,
                )
                .await?,
            )?;
            validate_network_calls(&calls.calls)?;
            if calls
                .calls
                .iter()
                .any(|c| c.task_id.as_deref() != Some(id.as_str()))
            {
                return Err(invalid_response());
            }
            encoded(calls)?
        }
        Command::Route(args) => {
            let (id, project, action, purposes) = match args.command {
                RouteCommand::Models { id, project } => {
                    (id, project, "models", vec![NetworkPurpose::Models])
                }
                RouteCommand::Check { id, project } => (
                    id,
                    project,
                    "check",
                    vec![NetworkPurpose::Models, NetworkPurpose::Completion],
                ),
            };
            let response: peachsh::network_authorization::NetworkResponse<Value> = decode(
                network_request(
                    client,
                    origin,
                    &["api", "routes", &id, action],
                    Some(json!({"project_id":project})),
                    None,
                    true,
                )
                .await?,
            )?;
            validate_network_calls(&response.calls)?;
            if response.calls.len() != purposes.len()
                || response.calls.iter().zip(purposes).any(|(c, p)| {
                    c.subject.kind != NetworkSubjectKind::Route
                        || c.subject.id != id
                        || c.subject.project_id.as_ref().map(|p| p.0.as_str())
                            != Some(project.as_str())
                        || c.purpose != p
                        || c.success != Some(true)
                        || c.task_id.is_some()
                })
            {
                return Err(invalid_response());
            }
            if action == "models" {
                let _: Vec<String> = decode(response.result.clone())?;
            }
            encoded(response)?
        }
        Command::Account(_) => {
            let response: peachsh::network_authorization::NetworkResponse<Value> = decode(
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
            validate_network_calls(&response.calls)?;
            if response.calls.len() != 1
                || response.calls.iter().any(|c| {
                    c.subject.kind != NetworkSubjectKind::Account
                        || c.purpose != NetworkPurpose::Balance
                        || c.success != Some(true)
                        || c.task_id.is_some()
                })
            {
                return Err(invalid_response());
            }
            encoded(response)?
        }
        _ => unreachable!(),
    };
    // Preserve validated facts even if stdout closes after the Host completed a call.
    // Management calls have no Task ID through which the user could rediscover them.
    let calls = value
        .get("calls")
        .cloned()
        .or_else(|| value.get("call_id").map(|_| json!([value.clone()])));
    let details = calls.map(|calls| {
        let states: Vec<_> = calls
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["state"].as_str())
            .collect();
        let dispatch_state = if states.iter().any(|s| matches!(*s, "unknown" | "claimed")) {
            "unknown"
        } else if states.is_empty() || states.iter().all(|s| *s == "not_sent") {
            "not_sent"
        } else {
            "finished"
        };
        json!({"calls":calls,"dispatch_state":dispatch_state})
    });
    print_value(value, json_output).map_err(|mut error| {
        if error.code == "output_failed" {
            error.details = details;
        }
        error
    })
}

const FRAME_LIMIT: usize = 1_048_576;
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
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| value.split(';').next().unwrap_or("").trim() == "text/event-stream")
    {
        return Err(invalid_response());
    }
    let mut stream = response.bytes_stream();
    let mut reader = EventReader::new();
    let mut count = 0u64;
    loop {
        let next = tokio::select! {
            signal = tokio::signal::ctrl_c() => { signal.map_err(|_| invalid_response())?; return Ok(()); },
            next = tokio::time::timeout(Duration::from_secs(30),stream.next()) => next
                .map_err(|_| Failure::fixed("unavailable","事件流停滞，请手动按游标重连",true))?,
        };
        let chunk = next
            .ok_or_else(|| Failure::fixed("unavailable", "事件流断开，请手动按游标重连", true))?
            .map_err(|_| Failure::fixed("unavailable", "事件流读取失败，请手动按游标重连", true))?;
        for byte in chunk {
            if let Some(frame) = reader.byte(byte)? {
                let value: Value =
                    serde_json::from_str(&frame.data.join("\n")).map_err(|_| invalid_response())?;
                if !safe_strings(&value) {
                    return Err(invalid_response());
                }
                if frame.kind.as_deref() == Some("error") {
                    let code = value
                        .get("code")
                        .and_then(Value::as_str)
                        .ok_or_else(invalid_response)?;
                    let message = value
                        .get("message")
                        .and_then(Value::as_str)
                        .ok_or_else(invalid_response)?;
                    let retryable = value
                        .get("retryable")
                        .and_then(Value::as_bool)
                        .ok_or_else(invalid_response)?;
                    let after = value
                        .get("after")
                        .and_then(Value::as_i64)
                        .ok_or_else(invalid_response)?;
                    if frame.id.is_some() || code != "internal" || after != *last {
                        return Err(invalid_response());
                    }
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
                if id <= *last
                    || id != event.seq
                    || frame.kind.as_deref() != Some(&event.kind)
                    || (!event.cursor.is_empty() && event.cursor != id.to_string())
                    || peachsh::domain::validate_event_kind(&event.kind).is_err()
                    || !valid_id(&event.task_id)
                    || (collection == "sessions" && event.session_id.as_deref() != Some(&args.id))
                {
                    return Err(invalid_response());
                }
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
