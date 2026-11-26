//! Local Host HTTP administration commands and SSE event observation.

use super::{
    ApprovalCommand, BudgetCommand, BudgetConfigArgs, BudgetConfigCommand, CheckpointCommand,
    Command, EventsArgs, Failure, HostCommand, HostConfigArgs, HostConfigCommand, HostRouteArgs,
    HostRouteCommand, HostSecret, NetworkCommand, PluginArgs, PluginCommand, ProjectArgs,
    ProjectCommand, ProjectConfigCommand, PurposeArg, RouteCommand, RunCommand, RuntimeCommand,
    RuntimeComponentCommand, SessionCommand, TaskArgs, TaskCommand, WorktreeCommand,
    decimal_cursor,
};
use coda::approval::{CommandNetworkAck, CommandNetworkNotice};
use coda::budget::{
    BudgetCall, BudgetConfig, BudgetConfigSnapshot, BudgetMutationResult, BudgetStatus,
};
use coda::credential_input::{CredentialInputError, CredentialInputMode, read_credential};
use coda::domain::{Event, Project, Run, Session, SessionKind, Task, Turn, TurnTask};
use coda::network_authorization::{
    CallReceipt, DispatchState, NetworkAuthorization, NetworkDecision, NetworkPreview,
    NetworkResponse,
};
use coda::plugin_management::{PluginMutationResult, PluginStatus};
use coda::project_config::{
    ProjectConfig, ProjectConfigSnapshot, ProjectSelection, TaskProjectBinding,
};
use coda::workspace_changes::{RestoreReceipt, RestoreStatus, WorkspaceChange};
use coda::worktree_management::{WorktreeInspection, WorktreeReceipt, WorktreeState};
use futures_util::StreamExt;
use reqwest::redirect::Policy;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{self, Write};
use std::time::Duration;

const RESPONSE_LIMIT: usize = 1_048_576;
const FRAME_LIMIT: usize = 1_048_576;

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
