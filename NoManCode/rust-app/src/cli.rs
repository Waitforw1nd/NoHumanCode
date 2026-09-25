use clap::{Args, Subcommand};
use futures_util::StreamExt;
use peachsh::{approval::ApprovalStatus, secrets};
use reqwest::{Client, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const RESPONSE_LIMIT: usize = 1_048_576;

#[derive(Subcommand)]
pub enum Command {
    Task(TaskArgs),
    Approval(ApprovalArgs),
}

impl Command {
    pub fn valid_ids(&self) -> bool {
        let id = match self {
            Self::Task(task) => match &task.command {
                TaskCommand::Approvals { task_id } => task_id,
            },
            Self::Approval(approval) => match &approval.command {
                ApprovalCommand::Get { approval_id }
                | ApprovalCommand::Approve { approval_id }
                | ApprovalCommand::Deny { approval_id } => approval_id,
            },
        };
        secrets::validate_persisted_id("id", id).is_ok() && !matches!(id.as_str(), "." | "..")
    }
}

#[derive(Args)]
pub struct TaskArgs {
    #[command(subcommand)]
    command: TaskCommand,
}

#[derive(Subcommand)]
enum TaskCommand {
    Approvals { task_id: String },
}

#[derive(Args)]
pub struct ApprovalArgs {
    #[command(subcommand)]
    command: ApprovalCommand,
}

#[derive(Subcommand)]
enum ApprovalCommand {
    Get { approval_id: String },
    Approve { approval_id: String },
    Deny { approval_id: String },
}

#[derive(Debug)]
pub struct Failure {
    code: &'static str,
    message: String,
    retryable: bool,
}

impl Failure {
    fn fixed(code: &'static str, message: &'static str, retryable: bool) -> Self {
        Self {
            code,
            message: message.to_owned(),
            retryable,
        }
    }

    pub fn print(&self, json_output: bool) {
        if json_output {
            eprintln!(
                "{}",
                json!({
                    "code": self.code,
                    "message": self.message,
                    "retryable": self.retryable,
                    "error": self.message,
                })
            );
        } else {
            eprintln!("{}: {}", self.code, self.message);
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

    let value = match command {
        Command::Task(task) => match task.command {
            TaskCommand::Approvals { task_id } => {
                let url = endpoint(&origin, &["api", "tasks", &task_id, "approvals"])?;
                request_json(client.get(url), false).await?
            }
        },
        Command::Approval(approval) => match approval.command {
            ApprovalCommand::Get { approval_id } => {
                let url = endpoint(&origin, &["api", "approvals", &approval_id])?;
                request_json(client.get(url), false).await?
            }
            ApprovalCommand::Approve { approval_id } => {
                decide(&client, &origin, &approval_id, "approve").await?
            }
            ApprovalCommand::Deny { approval_id } => {
                decide(&client, &origin, &approval_id, "deny").await?
            }
        },
    };

    let value = normalize_success(value)?;
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

async fn decide(client: &Client, origin: &Url, id: &str, decision: &str) -> Result<Value, Failure> {
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
    let result = request_json(
        client
            .post(url)
            .header("x-peachsh-token", &bootstrap.token)
            .json(&json!({"decision":decision})),
        true,
    )
    .await;
    match result {
        Ok(value) if value.to_string().contains(&bootstrap.token) => {
            Err(Failure::fixed("internal", "本机服务返回不安全响应", false))
        }
        Err(error) if error.message.contains(&bootstrap.token) => Err(Failure::fixed(
            "internal",
            "本机服务返回不安全错误响应",
            false,
        )),
        other => other,
    }
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
    if status.is_success() {
        return Ok(value);
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

fn normalize_success(value: Value) -> Result<Value, Failure> {
    if value.get("approvals").is_some() {
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
    println!(
        "{}\t{}\t{}",
        value["id"].as_str().unwrap(),
        value["status"].as_str().unwrap(),
        value["execution_state"].as_str().unwrap()
    );
    Ok(())
}
