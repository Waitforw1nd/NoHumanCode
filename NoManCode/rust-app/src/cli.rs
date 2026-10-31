use clap::{Args, Subcommand, ValueEnum};
use futures_util::StreamExt;
use peachsh::budget::{
    BudgetBlockedReason, BudgetCall, BudgetCallReason, BudgetCallState, BudgetConfig,
    BudgetConfigSnapshot, BudgetMode, BudgetMutationResult, BudgetStatus,
};
use peachsh::network_authorization::{
    CallReceipt, CallResult, CallState, DispatchState, NetworkAuthorization, NetworkDecision,
    NetworkPreview, NetworkPurpose, NetworkSubject, NetworkSubjectKind,
};
use peachsh::{
    approval::{ApprovalStatus, CommandNetworkAck, CommandNetworkNotice},
    plugin_management::{PluginMutationResult, PluginStatus},
    project_config::{ProjectConfig, ProjectConfigSnapshot, ProjectSelection, TaskProjectBinding},
    secrets,
    worktree_management::{
        WorktreeInspection, WorktreeObservationState, WorktreeReceipt, WorktreeState,
    },
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
    peachsh::domain::valid_name(id) && id != "newapi-account"
}
impl HostRouteSet {
    fn input(&self) -> peachsh::host_configuration::RouteInput {
        peachsh::host_configuration::RouteInput {
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
fn host_unknown() -> Failure {
    Failure::fixed(
        "operation_result_unknown",
        "配置结果未知，可能已提交；先用 host config operation --key <原key> 查询原回执，再用 host config get 查询当前配置。lookup 404不能排除原请求仍在途；不会自动重发或换key。",
        false,
    )
}
// No Debug/Display/Serialize. reqwest/serde and the OS may retain other copies;
// this only clears the plaintext buffer owned by this CLI scope, best effort.
struct HostSecret(String);
impl HostSecret {
    fn as_str(&self) -> &str {
        &self.0
    }
}
impl Drop for HostSecret {
    fn drop(&mut self) {
        // SAFETY: exclusively borrowed live bytes; zero is valid UTF-8 and this
        // String is never read again before deallocation.
        for byte in unsafe { self.0.as_bytes_mut() } {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}
fn host_error_code(code: &str) -> Option<(&'static str, u16, &'static str)> {
    Some(match code {
        "host_config_invalid" => ("host_config_invalid", 400, "配置请求无效"),
        "forbidden" => ("forbidden", 403, "本机Host访问被拒绝"),
        "host_config_not_found" => (
            "host_config_not_found",
            404,
            "查询快照未见原回执；原PUT可能仍在途，不要因此换key或自动重发。请随后查询原回执及当前配置",
        ),
        "host_config_revision_conflict" => (
            "host_config_revision_conflict",
            409,
            "Host配置版本冲突，请查询当前配置",
        ),
        "host_config_idempotency_conflict" => (
            "host_config_idempotency_conflict",
            409,
            "原key与配置请求不一致",
        ),
        "host_config_busy" => ("host_config_busy", 409, "Host仍有活动任务或网络调用"),
        "host_config_credential_required" => (
            "host_config_credential_required",
            409,
            "此配置需要重新输入凭据",
        ),
        "host_config_corrupt" => ("host_config_corrupt", 500, "Host配置持久状态损坏"),
        "host_config_unavailable" => ("host_config_unavailable", 500, "Host配置或凭据不可用"),
        "host_config_internal" => ("host_config_internal", 500, "Host配置操作失败"),
        _ => return None,
    })
}

// Transport parsing only: duplicates at any depth are rejected before projection.
struct HostWireValue(Value);
impl<'de> Deserialize<'de> for HostWireValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = HostWireValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("unambiguous JSON")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(HostWireValue(json!(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(HostWireValue(json!(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(HostWireValue(json!(v)))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|v| HostWireValue(Value::Number(v)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(HostWireValue(json!(v)))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(HostWireValue(Value::String(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(HostWireValue(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(HostWireValue(value)) = seq.next_element()? {
                    items.push(value);
                }
                Ok(HostWireValue(Value::Array(items)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate member"));
                    }
                    let HostWireValue(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(HostWireValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
fn host_revision(value: &Value) -> bool {
    value.as_u64().is_some_and(|n| n <= i64::MAX as u64)
}
fn host_view(value: &Value) -> Result<(), Failure> {
    if !worktree_fields(value, &["id","name","base_url","model","max_tokens","parallel_limit","key_env","credential_source"])
        || !value["id"].as_str().is_some_and(host_route_id_valid)
        || !["name","model"].into_iter().all(|field| value[field].as_str().is_some_and(|v|
            !v.trim().is_empty() && v.len() <= 500 && secrets::safe_metadata_text(field,v).is_ok()))
        // Legacy routes remain visible even if they use non-loopback HTTP.
        || !value["base_url"].as_str().is_some_and(|v| peachsh::domain::validate_url(v).is_ok()
            && secrets::safe_metadata_path("base_url",v).is_ok())
        || !value["max_tokens"].as_u64().is_some_and(|n| (16..=65536).contains(&n))
        || !value["parallel_limit"].as_u64().is_some_and(|n| (1..=8).contains(&n))
        || !(value["key_env"].is_null() || value["key_env"].as_str().is_some_and(|v|
            !v.is_empty() && v.len() <= 500 && secrets::safe_metadata_text("key_env",v).is_ok()
                && v.bytes().enumerate().all(|(i,b)| b==b'_' || b.is_ascii_alphabetic() || (i>0 && b.is_ascii_digit()))))
        || !matches!(value["credential_source"].as_str(),Some("stored"|"environment"|"legacy"|"missing"|"unavailable"))
    {
        return Err(invalid_response());
    }
    Ok(())
}
fn host_snapshot(value: &Value) -> Result<(), Failure> {
    let _: peachsh::host_configuration::HostConfigSnapshot = decode(value.clone())?;
    if !worktree_fields(value, &["revision", "routes"]) || !host_revision(&value["revision"]) {
        return Err(invalid_response());
    }
    let mut ids = std::collections::HashSet::new();
    for route in value["routes"].as_array().ok_or_else(invalid_response)? {
        host_view(route)?;
        if !ids.insert(route["id"].as_str().unwrap()) {
            return Err(invalid_response());
        }
    }
    Ok(())
}
fn host_result(value: &Value, write: Option<&HostRouteSet>) -> Result<(), Failure> {
    let _: peachsh::host_configuration::RouteConfigResult = decode(value.clone())?;
    let receipt = &value["receipt"];
    if !worktree_fields(value, &["receipt", "replayed"])
        || !value["replayed"].is_boolean()
        || !worktree_fields(
            receipt,
            &[
                "operation_id",
                "route_id",
                "revision",
                "route",
                "changed",
                "committed_at",
            ],
        )
        || !receipt["operation_id"]
            .as_str()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok_and(|v| v.to_string() == id))
        || !host_revision(&receipt["revision"])
        || !receipt["changed"].is_boolean()
        || !receipt["committed_at"].as_u64().is_some_and(|n| n > 0)
        || receipt["route_id"] != receipt["route"]["id"]
    {
        return Err(invalid_response());
    }
    host_view(&receipt["route"])?;
    if let Some(write) = write {
        let expected = write.route()?;
        let revision = if receipt["changed"] == true {
            write.expected_revision.checked_add(1)
        } else {
            Some(write.expected_revision)
        };
        if receipt["route_id"] != write.id
            || receipt["revision"].as_u64() != revision
            || expected
                .as_object()
                .unwrap()
                .iter()
                .any(|(key, v)| receipt["route"][key] != *v)
            || (!write.keep_credential
                && (receipt["route"]["credential_source"] != "stored"
                    || !receipt["route"]["key_env"].is_null()))
            || (write.keep_credential
                && !matches!(
                    receipt["route"]["credential_source"].as_str(),
                    Some("stored" | "environment" | "legacy")
                ))
        {
            return Err(invalid_response());
        }
    } else if value["replayed"] != true {
        return Err(invalid_response());
    }
    Ok(())
}
fn composition_version(value: &Value) -> bool {
    value.as_str().is_some_and(|version| {
        let parts: Vec<_> = version.split('.').collect();
        parts.len() == 3
            && parts.iter().all(|part| {
                !part.is_empty()
                    && (part.len() == 1 || !part.starts_with('0'))
                    && part.bytes().all(|b| b.is_ascii_digit())
                    && part.parse::<u32>().is_ok()
            })
    })
}

fn validate_composition(value: &Value) -> Result<(), Failure> {
    use std::collections::BTreeSet;
    if !worktree_fields(
        value,
        &[
            "version",
            "consistency",
            "capabilities",
            "plugins",
            "runtimes",
        ],
    ) || value["version"] != 1
        || value["consistency"] != "per_component"
    {
        return Err(invalid_response());
    }
    let plugins = value["plugins"].as_array().ok_or_else(invalid_response)?;
    let runtimes = value["runtimes"].as_array().ok_or_else(invalid_response)?;
    if plugins.len() != 2 || runtimes.len() != 2 {
        return Err(invalid_response());
    }
    let mut plugin_ids = BTreeSet::new();
    for item in plugins {
        let plugin: PluginStatus = decode(item.clone())?;
        if encoded(&plugin)? != *item || !plugin_ids.insert(plugin.plugin_id.clone()) {
            return Err(invalid_response());
        }
        validate_plugin(&plugin)?;
    }
    if plugin_ids != BTreeSet::from(["builtin.files".to_owned(), "builtin.shell".to_owned()]) {
        return Err(invalid_response());
    }
    let mut runtime_ids = BTreeSet::new();
    for item in runtimes {
        validate_runtime_component(item)?;
        if !runtime_ids.insert(item["id"].as_str().ok_or_else(invalid_response)?) {
            return Err(invalid_response());
        }
    }
    if runtime_ids != BTreeSet::from(["builtin.chat-agent", "builtin.provider-xpeach"]) {
        return Err(invalid_response());
    }
    let caps = value["capabilities"]
        .as_array()
        .ok_or_else(invalid_response)?;
    if caps.len() != 13 {
        return Err(invalid_response());
    }
    let mut ids = BTreeSet::new();
    for cap in caps {
        if !worktree_fields(
            cap,
            &[
                "id",
                "binding_kind",
                "provider_id",
                "provider_version",
                "interfaces",
                "lifecycle",
                "dependencies",
            ],
        ) {
            return Err(invalid_response());
        }
        let id = cap["id"].as_str().ok_or_else(invalid_response)?;
        if !ids.insert(id) {
            return Err(invalid_response());
        }
        let (provider, managed, deps, names): (&str, bool, &[&str], &[&str]) = match id {
            "core" => ("host.core", false, &[], &["host.core"]),
            "session-log" => ("host.session-log", false, &["core"], &["host.session-log"]),
            "local-project" => (
                "host.local-project",
                false,
                &["core", "workspace"],
                &["host.local-project"],
            ),
            "workspace" => ("host.workspace", false, &["core"], &["host.workspace"]),
            "provider-xpeach" => (
                "builtin.provider-xpeach",
                true,
                &["core"],
                &["provider.completion-codec"],
            ),
            "file-read-search" => (
                "builtin.files",
                true,
                &["workspace", "local-project", "approval-policy"],
                &["tool.read_file", "tool.list_files", "tool.search_files"],
            ),
            "approval-policy" => (
                "host.approval-policy",
                false,
                &["core", "local-project"],
                &["host.approval-policy"],
            ),
            "file-write" => (
                "builtin.files",
                true,
                &["workspace", "local-project", "approval-policy"],
                &["tool.write_file"],
            ),
            "shell" => (
                "builtin.shell",
                true,
                &["workspace", "local-project", "approval-policy"],
                &["tool.run_command"],
            ),
            "git-diff" => (
                "host.review-recovery",
                false,
                &["workspace", "local-project"],
                &["host.git-diff"],
            ),
            "checkpoint-restore" => (
                "host.review-recovery",
                false,
                &["workspace", "local-project"],
                &["host.checkpoint-restore"],
            ),
            "recovery" => ("host.recovery", false, &["core"], &["host.recovery"]),
            "chat-agent" => (
                "builtin.chat-agent",
                true,
                &["core", "approval-policy"],
                &["agent.strategy"],
            ),
            _ => return Err(invalid_response()),
        };
        if cap["provider_id"] != provider
            || cap["binding_kind"]
                != if managed {
                    "plugin_runtime"
                } else {
                    "host_service"
                }
            || cap["lifecycle"] != if managed { "managed" } else { "host_required" }
            || !composition_version(&cap["provider_version"])
            || (!managed && cap["provider_version"] != "1.0.0")
        {
            return Err(invalid_response());
        }
        if let Some(plugin) = plugins.iter().find(|p| p["plugin_id"] == provider) {
            if cap["provider_version"] != plugin["version"] {
                return Err(invalid_response());
            }
        }
        let actual_deps = cap["dependencies"]
            .as_array()
            .ok_or_else(invalid_response)?;
        let actual: BTreeSet<_> = actual_deps
            .iter()
            .map(|v| v.as_str().ok_or_else(invalid_response))
            .collect::<Result<_, _>>()?;
        if actual.len() != actual_deps.len() || actual != deps.iter().copied().collect() {
            return Err(invalid_response());
        }
        let interfaces = cap["interfaces"].as_array().ok_or_else(invalid_response)?;
        let mut found = BTreeSet::new();
        for interface in interfaces {
            if !worktree_fields(interface, &["name", "version"])
                || interface["version"] != "1.0.0"
                || !found.insert(interface["name"].as_str().ok_or_else(invalid_response)?)
            {
                return Err(invalid_response());
            }
        }
        if found != names.iter().copied().collect() {
            return Err(invalid_response());
        }
    }
    Ok(())
}

async fn composition_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
) -> Result<(), Failure> {
    let unavailable = || Failure::fixed("unavailable", "无法读取本机组合状态", true);
    let bootstrap: Bootstrap = decode(
        request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?))
            .await
            .map_err(|_| unavailable())?,
    )
    .map_err(|_| unavailable())?;
    if bootstrap.token.is_empty()
        || bootstrap.token.len() > 4096
        || reqwest::header::HeaderValue::from_str(&bootstrap.token).is_err()
    {
        return Err(unavailable());
    }
    let response = client
        .get(endpoint(origin, &["api", "host", "composition"])?)
        .header("x-peachsh-token", &bootstrap.token)
        .send()
        .await
        .map_err(|_| unavailable())?;
    let status = response.status().as_u16();
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return Err(invalid_response());
    }
    let bytes = limited_body(response).await?;
    let HostWireValue(value) = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
    if !safe_strings(&value)
        || contains_string(&value, &bootstrap.token)
        || value.to_string().contains(&bootstrap.token)
    {
        return Err(invalid_response());
    }
    if status != 200 {
        if status == 403
            && worktree_fields(&value, &["error"])
            && worktree_fields(&value["error"], &["code", "message", "retryable"])
            && value["error"]["code"] == "forbidden"
            && value["error"]["message"].is_string()
            && value["error"]["retryable"] == false
        {
            return Err(Failure::fixed("forbidden", "本机Host访问被拒绝", false));
        }
        if worktree_fields(&value, &["code", "message", "retryable", "error"])
            && value["message"].is_string()
            && value["error"] == value["message"]
            && value["retryable"].is_boolean()
        {
            return match (status, value["code"].as_str(), value["retryable"].as_bool()) {
                (404, Some("not_found"), Some(false)) => {
                    Err(Failure::fixed("not_found", "组合状态未找到", false))
                }
                (500, Some("internal"), Some(retryable)) => {
                    Err(Failure::fixed("internal", "组合状态读取失败", retryable))
                }
                _ => Err(invalid_response()),
            };
        }
        return Err(invalid_response());
    }
    validate_composition(&value)?;
    print_value(value, json_output)?;
    if !json_output {
        let mut out = io::stdout().lock();
        writeln!(out, "能力绑定、期望状态、进程状态与实际授权不同；host_required 不可热关闭，files 多项能力共享开关。per_component 不保证跨域原子快照。")
            .and_then(|()| out.flush()).map_err(|_| Failure::fixed("output_failed", "无法写入标准输出", false))?;
    }
    Ok(())
}

async fn host_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: HostCommand,
) -> Result<(), Failure> {
    use peachsh::credential_input::{CredentialInputError, CredentialInputMode, read_credential};
    let write = match &command {
        HostCommand::Route(HostRouteArgs {
            command: HostRouteCommand::Set(args),
        }) => Some(args),
        _ => None,
    };
    // Only the controlled wire body below contains the secret; never Debug it.
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
    let bootstrap: Bootstrap = decode(
        request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?))
            .await
            .map_err(|_| {
                Failure::fixed(
                    "host_config_unavailable",
                    "bootstrap失败，配置PUT未发送",
                    false,
                )
            })?,
    )
    .map_err(|_| {
        Failure::fixed(
            "host_config_unavailable",
            "bootstrap无效，配置PUT未发送",
            false,
        )
    })?;
    if bootstrap.token.is_empty()
        || bootstrap.token.len() > 4096
        || reqwest::header::HeaderValue::from_str(&bootstrap.token).is_err()
    {
        return Err(Failure::fixed(
            "host_config_unavailable",
            "bootstrap无效，配置PUT未发送",
            false,
        ));
    }
    let request=match &command {
        HostCommand::Composition => return Err(invalid_response()),
        HostCommand::Config(HostConfigArgs { command: HostConfigCommand::Get }) => client.get(endpoint(origin,&["api","host","config"])?),
        HostCommand::Config(HostConfigArgs { command: HostConfigCommand::Operation { key } }) => client.post(endpoint(origin,&["api","host","config","lookup"])?).json(&json!({"key":key})),
        HostCommand::Route(HostRouteArgs { command: HostRouteCommand::Set(args) }) => {
            let credential=match &secret { Some(secret)=>json!({"mode":"replace","value":secret.as_str()}), None=>json!({"mode":"keep"}) };
            client.put(endpoint(origin,&["api","host","routes",&args.id])?).header("Idempotency-Key",&args.key)
                .json(&json!({"expected_revision":args.expected_revision,"route":args.route()?,"credential":credential}))
        }
    }.header("x-peachsh-token",&bootstrap.token);
    let result: Result<(),Failure>=async {
        let response=request.send().await.map_err(|_| Failure::fixed("host_transport_unavailable","无法完成本机Host请求",false))?;
        let status=response.status().as_u16();
        if !response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v|v.to_str().ok())
            .is_some_and(|v|v.split(';').next().is_some_and(|v|v.trim().eq_ignore_ascii_case("application/json"))) { return Err(invalid_response()); }
        let bytes=limited_body(response).await?;
        let HostWireValue(value)=serde_json::from_slice(&bytes).map_err(|_|invalid_response())?;
        let serialized=value.to_string();
        if !safe_strings(&value) || contains_string(&value,&bootstrap.token) || serialized.contains(&bootstrap.token)
            || secret.as_ref().is_some_and(|s| contains_string(&value,s.as_str()) || serialized.contains(s.as_str())) { return Err(invalid_response()); }
        if status!=200 {
            let error=&value["error"];
            if !worktree_fields(&value,&["error"]) || !worktree_fields(error,&["code","message","retryable"])
                || error["retryable"]!=false || !error["message"].is_string() { return Err(invalid_response()); }
            let (code,expected,message)=error["code"].as_str().and_then(host_error_code).ok_or_else(invalid_response)?;
            if expected!=status && !(code=="host_config_invalid" && matches!(status,413|415)) { return Err(invalid_response()); }
            return Err(Failure::fixed(code,message,false));
        }
        match &command {
            HostCommand::Config(HostConfigArgs { command: HostConfigCommand::Get }) => host_snapshot(&value)?,
            _ => host_result(&value,write)?,
        }
        let unavailable=value["routes"].as_array().is_some_and(|routes|routes.iter().any(|v|v["credential_source"]=="unavailable"));
        print_value(value,json_output)?;
        if !json_output {
            let message=if matches!(command,HostCommand::Config(HostConfigArgs{command:HostConfigCommand::Get})) {
                if unavailable { "当前配置；凭据来源unavailable表示无法读取，不等于missing或Provider验证失败。" }
                else { "这是当前本地配置；凭据来源只表示本地状态，不表示Provider已经验证或联网已获授权。" }
            } else { "这是提交时的原回执，可能早于当前配置；仅保存本地配置，未授权联网、未验证Provider。当前值请查 host config get。" };
            let mut output=io::stdout().lock();
            writeln!(output,"{message}").and_then(|()|output.flush()).map_err(|_|Failure::fixed("output_failed","无法写入标准输出",false))?;
        }
        Ok(())
    }.await;
    result.map_err(|error| {
        if write.is_some() && host_error_code(error.code).is_none() {
            host_unknown()
        } else {
            error
        }
    })
}

#[derive(Subcommand)]
pub enum Command {
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

fn valid_id(id: &str) -> bool {
    secrets::validate_persisted_id("id", id).is_ok() && !matches!(id, "." | "..")
}
fn valid_message(message: &str) -> bool {
    !message.trim().is_empty() && message.len() <= 100_000
}
impl Command {
    pub fn valid_ids(&self) -> bool {
        match self {
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
                        valid_id(id)
                            && *expected_revision <= i64::MAX as u64
                            && secrets::validate_idempotency_key(key).is_ok()
                            && !key.contains(',')
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
                } => {
                    valid_id(id)
                        && *expected_revision <= i64::MAX as u64
                        && secrets::validate_idempotency_key(key).is_ok()
                        && !key.contains(',')
                }
            },
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
                    && secrets::validate_idempotency_key(key).is_ok()
                    && !key.contains(',')
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
fn worktree_key(value: &str) -> bool {
    secrets::validate_idempotency_key(value).is_ok() && !value.contains(',')
}
// Lexical wire comparison only. RootIdentity validation belongs to the Host.
fn worktree_path(value: &str) -> Option<String> {
    if value.len() > 32768 || value.chars().any(|c| c.is_control()) {
        return None;
    }
    let mut path = value.replace('\\', "/");
    if path
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("//?/UNC/"))
    {
        path = format!("//{}", &path[8..]);
    } else if path.starts_with("//?/") {
        path = path[4..].to_owned();
    }
    let drive = path.as_bytes().get(1) == Some(&b':')
        && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && path.as_bytes().get(2) == Some(&b'/');
    let unc = path.starts_with("//") && path[2..].split('/').filter(|s| !s.is_empty()).count() >= 2;
    if !drive && !unc {
        return None;
    }
    for (index, part) in path.split('/').enumerate() {
        if part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part.contains(['*', '?', '"', '<', '>', '|'])
            || (part.contains(':') && !(drive && index == 0))
        {
            return None;
        }
    }
    Some(path.trim_end_matches('/').to_ascii_lowercase())
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
                    && secrets::safe_metadata_text("name", name).is_ok()
                    && secrets::safe_metadata_path("parent", parent).is_ok()
                    && worktree_path(parent).is_some()
                    && (1..=i64::MAX as u64).contains(expected_config_revision)
                    && commit_oid(expected_head)
                    && worktree_key(key)
            }
            Self::Status {
                source_project,
                id,
                key,
            } => {
                valid_id(source_project)
                    && (id.is_some() != key.is_some())
                    && id.as_deref().is_none_or(canonical_uuid)
                    && key.as_deref().is_none_or(worktree_key)
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
        if self.code == "worktree_not_succeeded" {
            if !json_output {
                eprintln!("工作树未成功完成；请按原 key 查询。不会自动重试、接纳或清理。");
            }
            return;
        }
        // The successful mutation response has already been printed intact.
        // This sentinel controls only the process exit status, not the wire shape.
        if self.code == "plugin_not_converged" {
            if !json_output {
                eprintln!("持久命令已接受，当前运行时尚未收敛；请查询状态。");
            }
            return;
        }
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
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|_| Failure::fixed("internal", "无法初始化本机客户端", false))?;
    let origin = Url::parse(&format!("http://127.0.0.1:{port}/"))
        .map_err(|_| Failure::fixed("internal", "本机服务地址无效", false))?;

    let command = match command {
        Command::Host(args) => {
            return match args.command {
                HostCommand::Composition => {
                    composition_workflow(&client, &origin, json_output).await
                }
                other => host_workflow(&client, &origin, json_output, other).await,
            };
        }
        Command::Runtime(args) => match args.command {
            RuntimeCommand::Agent(agent) => {
                return agent_runtime_workflow(
                    &client,
                    &origin,
                    json_output,
                    RuntimeArgs {
                        command: RuntimeCommand::Agent(agent),
                    },
                )
                .await;
            }
            RuntimeCommand::Component(args) => {
                return runtime_component_workflow(&client, &origin, json_output, args.command)
                    .await;
            }
        },
        Command::Project(ProjectArgs {
            command: ProjectCommand::Budget(args),
        }) => return budget_workflow(&client, &origin, json_output, args.command).await,
        Command::Project(ProjectArgs {
            command: ProjectCommand::Worktree(args),
        }) => {
            return worktree_workflow(&client, &origin, json_output, args.command).await;
        }
        Command::Plugin(args) => return plugin_workflow(&client, &origin, json_output, args).await,
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

fn plugin_unknown() -> Failure {
    Failure::fixed(
        "operation_result_unknown",
        "插件写入结果未知，可能已提交。请查询 plugin status，并保留原 --key、--enabled 和 --expected-revision 手动回放；不会自动重试。",
        false,
    )
}
fn worktree_unknown() -> Failure {
    Failure::fixed(
        "operation_result_unknown",
        "工作树创建结果未知；请用 project worktree status --source-project <原项目> --key <原key> 查询。不会自动重发、接纳或清理。",
        false,
    )
}
fn worktree_fields(value: &Value, fields: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == fields.len() && fields.iter().all(|field| object.contains_key(*field))
    })
}
fn validate_worktree(
    receipt: &WorktreeReceipt,
    source: &str,
    id: Option<&str>,
) -> Result<(), Failure> {
    let parent = worktree_path(&receipt.parent_path).ok_or_else(invalid_response)?;
    let target = format!("{parent}/nhc-{}", receipt.operation_id);
    let succeeded = receipt.state == WorktreeState::Succeeded;
    let has_cause = matches!(
        receipt.state,
        WorktreeState::Rejected | WorktreeState::Unknown
    );
    if receipt.schema_version != 1
        || !canonical_uuid(&receipt.operation_id)
        || receipt.source_project_id.0 != source
        || !valid_id(source)
        || id.is_some_and(|id| id != receipt.operation_id)
        || receipt.name.trim().is_empty()
        || receipt.name.len() > 500
        || worktree_path(&receipt.target_path).as_deref() != Some(target.as_str())
        || receipt.branch != format!("codex/workspace/{}", receipt.operation_id)
        || !commit_oid(&receipt.expected_head)
        || receipt.expected_head != receipt.expected_head.to_ascii_lowercase()
        || !valid_digest(&receipt.request_digest)
        || !(1..=i64::MAX as u64).contains(&receipt.expected_config_revision)
        || !(1..=i64::MAX as u64).contains(&receipt.revision)
        || receipt.created_at < 0
        || receipt.updated_at < receipt.created_at
        || succeeded != receipt.project_id.is_some()
        || has_cause != receipt.cause.is_some()
        || receipt
            .project_id
            .as_ref()
            .is_some_and(|p| !valid_id(&p.0) || p.0 == source)
        || !safe_strings(&encoded(receipt)?)
    {
        return Err(invalid_response());
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreeWireError {
    code: String,
    message: String,
    error: String,
    retryable: bool,
}
fn worktree_http_error(bytes: &[u8], status: u16) -> Result<Failure, Failure> {
    let error: WorktreeWireError = serde_json::from_slice(bytes).map_err(|_| invalid_response())?;
    let (code, expected) = match error.code.as_str() {
        "worktree_invalid" => ("worktree_invalid", 400),
        "worktree_not_found" => ("worktree_not_found", 404),
        "worktree_revision_conflict" => ("worktree_revision_conflict", 409),
        "worktree_idempotency_conflict" => ("worktree_idempotency_conflict", 409),
        "worktree_busy" => ("worktree_busy", 409),
        "worktree_root_changed" => ("worktree_root_changed", 409),
        "worktree_conflict" => ("worktree_conflict", 409),
        "worktree_unsupported" => ("worktree_unsupported", 422),
        "worktree_unavailable" => ("worktree_unavailable", 503),
        "worktree_corrupt" => ("worktree_corrupt", 500),
        "forbidden" => ("forbidden", 403),
        "request_failed" if matches!(status, 400 | 413 | 415) => ("request_failed", status),
        _ => return Err(invalid_response()),
    };
    if expected != status || error.retryable || error.message != error.error {
        return Err(invalid_response());
    }
    Ok(Failure {
        code,
        message: error.message,
        retryable: false,
        details: None,
        after: None,
    })
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
    let bootstrap: Bootstrap =
        decode(request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?)).await?)?;
    if bootstrap.token.is_empty() || bootstrap.token.len() > 4096 {
        return Err(invalid_response());
    }
    let mut segments = vec!["api", "projects", source, "worktrees"];
    let request = match &command {
        WorktreeCommand::Create { name, parent, expected_config_revision, expected_head, key, .. } => {
            client.post(endpoint(origin, &segments)?).header("idempotency-key", key)
                .json(&json!({"name":name,"parent_path":parent,"expected_config_revision":expected_config_revision,"expected_head":expected_head}))
        }
        WorktreeCommand::Status { key: Some(key), .. } => {
            segments.push("lookup");
            client.post(endpoint(origin, &segments)?).json(&json!({"idempotency_key":key}))
        }
        WorktreeCommand::Status { .. } => {
            segments.push(operation_id.expect("validated id"));
            client.get(endpoint(origin, &segments)?)
        }
        WorktreeCommand::Inspect { id, .. } => {
            segments.extend([id.as_str(), "inspect"]);
            client.post(endpoint(origin, &segments)?).json(&json!({}))
        }
    }.header("x-peachsh-token", &bootstrap.token);
    let response = request.send().await.map_err(|_| {
        if is_create {
            worktree_unknown()
        } else {
            Failure::fixed(
                "unavailable",
                "无法读取工作树持久状态或观测；未发起创建",
                false,
            )
        }
    })?;
    let status = response.status().as_u16();
    let process = async {
        let bytes = limited_body(response).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
        if !safe_strings(&value) || contains_string(&value, &bootstrap.token) { return Err(invalid_response()); }
        // Read-only POSTs are intentionally not classified as creation requests.
        if value.get("schema_version").is_none() && !is_inspect
            || is_inspect && value.get("receipt").is_none()
        {
            return Err(worktree_http_error(&bytes, status)?);
        }
        let receipt = if is_inspect {
            if !worktree_fields(&value, &["receipt", "observation"])
                || !worktree_fields(&value["observation"], &["operation_id", "observed_at", "state", "reason_codes", "observed_head", "observed_branch"])
            { return Err(invalid_response()); }
            let inspection: WorktreeInspection = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
            let observation = &inspection.observation;
            if status != 200 || observation.operation_id != inspection.receipt.operation_id
                || observation.observed_at < 0
                || observation.observed_head.as_deref().is_some_and(|head| !commit_oid(head) || head != head.to_ascii_lowercase())
                || observation.observed_branch.as_deref().is_some_and(|branch| branch.is_empty() || branch.contains(['\r','\n','\0']))
                || (observation.state == WorktreeObservationState::PresentMatching
                    && (observation.observed_head.as_deref() != Some(inspection.receipt.expected_head.as_str())
                        || !matches!(observation.observed_branch.as_deref(), Some(branch) if branch == inspection.receipt.branch || branch == format!("refs/heads/{}", inspection.receipt.branch))))
            { return Err(invalid_response()); }
            inspection.receipt
        } else {
            serde_json::from_slice::<WorktreeReceipt>(&bytes).map_err(|_| invalid_response())?
        };
        let receipt_value = if is_inspect { &value["receipt"] } else { &value };
        if !worktree_fields(receipt_value, &["schema_version", "operation_id", "source_project_id", "name", "parent_path", "target_path", "branch", "expected_config_revision", "expected_head", "request_digest", "revision", "state", "project_id", "cause", "created_at", "updated_at"]) {
            return Err(invalid_response());
        }
        validate_worktree(&receipt, source, operation_id)?;
        if let WorktreeCommand::Create { name, parent, expected_config_revision, expected_head, .. } = &command {
            let expected_status = match receipt.state {
                WorktreeState::Succeeded => 200,
                WorktreeState::Claimed | WorktreeState::Unknown => 202,
                WorktreeState::Rejected => 409,
            };
            if status != expected_status || receipt.name != *name || receipt.parent_path != *parent
                || receipt.expected_config_revision != *expected_config_revision
                || receipt.expected_head != expected_head.to_ascii_lowercase()
            { return Err(invalid_response()); }
        } else if status != 200 { return Err(invalid_response()); }
        print_value(value, json_output)?;
        if !json_output {
            let mut output = io::stdout().lock();
            writeln!(output, "linked worktree 共享 Git objects/refs/config，不是 OS 沙箱；新 Project 需独立配置与网络授权。unknown 仅诊断，不自动重试、接纳或清理。")
                .and_then(|()| output.flush())
                .map_err(|_| Failure::fixed("output_failed", "无法写入标准输出", false))?;
        }
        if is_create && receipt.state != WorktreeState::Succeeded {
            return Err(Failure::fixed("worktree_not_succeeded", "工作树未成功完成", false));
        }
        Ok(())
    }.await;
    process.map_err(|error| {
        if is_create
            && matches!(
                error.code,
                "internal" | "output_failed" | "unavailable" | "worktree_corrupt"
            )
        {
            worktree_unknown()
        } else {
            error
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentRuntimeView {
    plugin_id: String,
    service: String,
    interface_version: String,
    scope: String,
    active: bool,
    admission_open: bool,
    in_flight: u64,
    generation: u64,
    draining: bool,
    transitioning: bool,
    effects: u64,
    recovery: u64,
    last_failure: Option<AgentRuntimeFailure>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum AgentRuntimeFailure {
    Unavailable,
    InvalidDecision,
    Failed,
    Panicked,
    Stopped,
    Busy,
    Lifecycle,
    Internal,
}

fn validate_agent_runtime(value: &Value) -> Result<(), Failure> {
    if !worktree_fields(
        value,
        &[
            "plugin_id",
            "service",
            "interface_version",
            "scope",
            "active",
            "admission_open",
            "in_flight",
            "generation",
            "draining",
            "transitioning",
            "effects",
            "recovery",
            "last_failure",
        ],
    ) {
        return Err(invalid_response());
    }
    let view: AgentRuntimeView = decode(value.clone())?;
    if view.plugin_id != "builtin.chat-agent"
        || view.service != "agent.strategy"
        || view.interface_version != "1.0.0"
        || view.scope != "process"
        || view.generation == 0
        || view.draining != (!view.admission_open && view.in_flight > 0)
        || (view.admission_open && (!view.active || view.transitioning))
        || (view.draining && (!view.active || view.transitioning))
        || (view.transitioning && (view.active || view.in_flight != 0))
        || (view.active && !view.transitioning && view.effects == 0)
    {
        return Err(invalid_response());
    }
    let _ = (view.recovery, view.last_failure);
    Ok(())
}

async fn agent_runtime_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    args: RuntimeArgs,
) -> Result<(), Failure> {
    match args.command {
        RuntimeCommand::Agent(AgentRuntimeArgs {
            command: AgentRuntimeCommand::Status,
        }) => {}
        RuntimeCommand::Component(_) => return Err(invalid_response()),
    }
    let bootstrap: Bootstrap = decode(
        request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?))
            .await
            .map_err(|_| Failure::fixed("agent_runtime_unavailable", "Agent运行态不可用", false))?,
    )?;
    if bootstrap.token.is_empty()
        || bootstrap.token.len() > 4096
        || reqwest::header::HeaderValue::from_str(&bootstrap.token).is_err()
    {
        return Err(invalid_response());
    }
    let response = client
        .get(endpoint(origin, &["api", "runtime", "agents", "chat"])?)
        .header("x-peachsh-token", &bootstrap.token)
        .send()
        .await
        .map_err(|_| Failure::fixed("agent_runtime_unavailable", "Agent运行态不可用", false))?;
    let status = response.status().as_u16();
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return Err(invalid_response());
    }
    let bytes = limited_body(response)
        .await
        .map_err(|_| invalid_response())?;
    let HostWireValue(value) = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
    if !safe_strings(&value)
        || contains_string(&value, &bootstrap.token)
        || value.to_string().contains(&bootstrap.token)
    {
        return Err(invalid_response());
    }
    if status != 200 {
        if status == 503
            && worktree_fields(&value, &["code", "message", "retryable", "error"])
            && value["code"] == "agent_runtime_unavailable"
            && value["message"] == "Agent运行态不可用"
            && value["error"] == value["message"]
            && value["retryable"] == false
        {
            return Err(Failure::fixed(
                "agent_runtime_unavailable",
                "Agent运行态不可用",
                false,
            ));
        }
        return Err(invalid_response());
    }
    validate_agent_runtime(&value)?;
    print_value(value, json_output)
}

fn runtime_unknown() -> Failure {
    Failure::fixed(
        "operation_result_unknown",
        "Runtime management outcome unknown; use the same key and parameters for an explicit replay.",
        false,
    )
}

fn validate_runtime_component(value: &Value) -> Result<(), Failure> {
    if !worktree_fields(
        value,
        &["id", "capability", "desired", "runtime", "converged"],
    ) {
        return Err(invalid_response());
    }
    let id = value["id"].as_str().ok_or_else(invalid_response)?;
    let agent = match (id, value["capability"].as_str()) {
        ("builtin.chat-agent", Some("agent.strategy@1.0.0")) => true,
        ("builtin.provider-xpeach", Some("provider.completion-codec@1.0.0")) => false,
        _ => return Err(invalid_response()),
    };
    let d = &value["desired"];
    if !worktree_fields(d, &["scope", "enabled", "revision", "updated_at"])
        || d["scope"] != "host"
        || !d["enabled"].is_boolean()
        || d["revision"].as_u64().is_none_or(|x| x > i64::MAX as u64)
        || d["updated_at"].as_i64().is_none_or(|x| x < 0)
    {
        return Err(invalid_response());
    }
    let r = &value["runtime"];
    let fields: &[&str] = if agent {
        &[
            "kind",
            "scope",
            "active",
            "admission_open",
            "in_flight",
            "generation",
            "draining",
            "transitioning",
            "effects",
            "recovery",
            "last_failure",
        ]
    } else {
        &[
            "kind",
            "scope",
            "active",
            "admission_open",
            "in_flight",
            "generation",
            "transitioning",
            "effects",
            "recovery",
            "lifecycle_failed",
        ]
    };
    if !worktree_fields(r, fields)
        || r["scope"] != "process"
        || r["kind"]
            != if agent {
                "agent"
            } else {
                "completion_provider"
            }
    {
        return Err(invalid_response());
    }
    let active = r["active"].as_bool().ok_or_else(invalid_response)?;
    let admission = r["admission_open"].as_bool().ok_or_else(invalid_response)?;
    let transition = r["transitioning"].as_bool().ok_or_else(invalid_response)?;
    let flight = r["in_flight"].as_u64().ok_or_else(invalid_response)?;
    let effects = r["effects"].as_u64().ok_or_else(invalid_response)?;
    let recovery = r["recovery"].as_u64().ok_or_else(invalid_response)?;
    if r["generation"].as_u64().is_none_or(|n| n == 0)
        || (transition && admission)
        || (!transition && admission && !active)
    {
        return Err(invalid_response());
    }
    let (failure, draining) = if agent {
        let draining = r["draining"].as_bool().ok_or_else(invalid_response)?;
        if draining != (!admission && flight > 0)
            || (draining && (!active || transition))
            || (transition && (active || flight != 0))
            || (active && !transition && effects == 0)
        {
            return Err(invalid_response());
        }
        let failure = if r["last_failure"].is_null() {
            false
        } else {
            if !matches!(
                r["last_failure"].as_str(),
                Some(
                    "unavailable"
                        | "invalid_decision"
                        | "failed"
                        | "panicked"
                        | "stopped"
                        | "busy"
                        | "lifecycle"
                        | "internal"
                )
            ) {
                return Err(invalid_response());
            }
            true
        };
        (failure, draining)
    } else {
        (
            r["lifecycle_failed"]
                .as_bool()
                .ok_or_else(invalid_response)?,
            false,
        )
    };
    let desired = d["enabled"].as_bool().ok_or_else(invalid_response)?;
    let converged = !transition
        && !failure
        && recovery == 0
        && if desired {
            active && admission && (!agent || !draining)
        } else {
            !active && !admission && effects == 0 && (!agent || (!draining && flight == 0))
        };
    if value["converged"].as_bool() != Some(converged) {
        return Err(invalid_response());
    }
    Ok(())
}

async fn runtime_body(response: reqwest::Response) -> Result<Vec<u8>, Failure> {
    const LIMIT: usize = 256 * 1024;
    if response.content_length().is_some_and(|n| n > LIMIT as u64) {
        return Err(invalid_response());
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| invalid_response())?;
        if body.len().saturating_add(chunk.len()) > LIMIT {
            return Err(invalid_response());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn runtime_component_workflow(
    client: &Client,
    origin: &Url,
    json_output: bool,
    command: RuntimeComponentCommand,
) -> Result<(), Failure> {
    let bootstrap: Bootstrap =
        decode(request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?)).await?)?;
    if bootstrap.token.is_empty()
        || bootstrap.token.len() > 4096
        || reqwest::header::HeaderValue::from_str(&bootstrap.token).is_err()
    {
        return Err(invalid_response());
    }
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
            .json(&json!({"enabled":enabled,"expected_revision":revision})),
        None => client.get(url),
    }
    .header("x-peachsh-token", &bootstrap.token);
    let writing = mutation.is_some();
    let response = request.send().await.map_err(|_| {
        if writing {
            runtime_unknown()
        } else {
            invalid_response()
        }
    })?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
        });
    let process = async {
        if !content_type {
            return Err(invalid_response());
        }
        let bytes = runtime_body(response).await?;
        let HostWireValue(value) =
            serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
        if !safe_strings(&value)
            || contains_string(&value, &bootstrap.token)
            || value.to_string().contains(&bootstrap.token)
        {
            return Err(invalid_response());
        }
        if status != 200 {
            if !worktree_fields(&value, &["code", "message", "error", "retryable"])
                || value["retryable"] != false
                || value["message"] != value["error"]
            {
                return Err(invalid_response());
            }
            let (code, expected, message) = match value["code"].as_str() {
                Some("runtime_invalid") => {
                    ("runtime_invalid", 400, "Invalid runtime management request")
                }
                Some("runtime_not_found") => {
                    ("runtime_not_found", 404, "Runtime component not found")
                }
                Some("runtime_revision_conflict") => (
                    "runtime_revision_conflict",
                    409,
                    "Runtime revision conflict",
                ),
                Some("runtime_idempotency_conflict") => (
                    "runtime_idempotency_conflict",
                    409,
                    "Runtime idempotency conflict",
                ),
                Some("runtime_busy") => ("runtime_busy", 409, "Runtime transition busy"),
                Some("runtime_unavailable") => {
                    ("runtime_unavailable", 503, "Runtime component unavailable")
                }
                Some("runtime_internal") => (
                    "runtime_internal",
                    500,
                    "Runtime management outcome unavailable",
                ),
                _ => return Err(invalid_response()),
            };
            if status != expected || value["message"] != message {
                return Err(invalid_response());
            }
            if writing && code == "runtime_internal" {
                return Err(runtime_unknown());
            }
            return Err(Failure::fixed(code, message, false));
        }
        match &command {
            RuntimeComponentCommand::List => {
                if !worktree_fields(&value, &["components"]) {
                    return Err(invalid_response());
                }
                let components = value["components"]
                    .as_array()
                    .ok_or_else(invalid_response)?;
                if components.len() != 2
                    || components[0]["id"] != "builtin.chat-agent"
                    || components[1]["id"] != "builtin.provider-xpeach"
                {
                    return Err(invalid_response());
                }
                for item in components {
                    validate_runtime_component(item)?;
                }
            }
            RuntimeComponentCommand::Status { id } => {
                validate_runtime_component(&value)?;
                if value["id"] != *id {
                    return Err(invalid_response());
                }
            }
            RuntimeComponentCommand::SetEnabled {
                id,
                enabled,
                expected_revision,
                ..
            } => {
                if !worktree_fields(&value, &["receipt", "replayed", "current"])
                    || !value["replayed"].is_boolean()
                {
                    return Err(invalid_response());
                }
                let receipt = &value["receipt"];
                if !worktree_fields(
                    receipt,
                    &[
                        "plugin_id",
                        "enabled",
                        "previous_revision",
                        "revision",
                        "committed_at",
                    ],
                ) || receipt["plugin_id"] != *id
                    || receipt["enabled"].as_bool() != Some(*enabled)
                    || receipt["previous_revision"].as_u64() != Some(*expected_revision)
                    || receipt["revision"].as_u64() != expected_revision.checked_add(1)
                    || receipt["revision"]
                        .as_u64()
                        .is_none_or(|n| n > i64::MAX as u64)
                    || receipt["committed_at"].as_i64().is_none_or(|n| n <= 0)
                {
                    return Err(invalid_response());
                }
                validate_runtime_component(&value["current"])?;
                if value["current"]["id"] != *id {
                    return Err(invalid_response());
                }
            }
        }
        print_value(value, json_output)
    }
    .await;
    process.map_err(|error| {
        if writing && matches!(error.code, "internal" | "output_failed" | "unavailable") {
            runtime_unknown()
        } else {
            error
        }
    })
}

fn validate_plugin(plugin: &PluginStatus) -> Result<(), Failure> {
    let value = encoded(plugin)?;
    let (affected, compat, dependent) = match (plugin.plugin_id.as_str(), plugin.version.as_str()) {
        ("builtin.files", "1.0.0") => (
            json!(["read_file", "write_file"]),
            json!(["list_files", "search_files", "run_command", "run_wasm"]),
            "builtin.engine-tools",
        ),
        ("builtin.files", "1.1.0") => (
            json!(["read_file", "write_file", "list_files", "search_files"]),
            json!(["run_command", "run_wasm"]),
            "builtin.engine-tools",
        ),
        ("builtin.files", "1.2.0") => (
            json!(["read_file", "write_file", "list_files", "search_files"]),
            json!(["run_wasm"]),
            "builtin.engine-tools",
        ),
        ("builtin.shell", "1.0.0") => (
            json!(["run_command"]),
            json!(["run_wasm"]),
            "builtin.engine-shell-tools",
        ),
        _ => return Err(invalid_response()),
    };
    if plugin.scope != "host"
        || plugin.desired.plugin_id != plugin.plugin_id
        || plugin.desired.revision > i64::MAX as u64
        || value["affected_tools"] != affected
        || value["compat_tools"] != compat
        || value["affected_dependents"] != json!([dependent])
        || !safe_strings(&value)
    {
        return Err(invalid_response());
    }
    Ok(())
}

fn print_plugin(plugin: &PluginStatus) {
    let runtime = serde_json::to_value(&plugin.runtime).expect("runtime DTO serializes");
    println!(
        "{}@{} scope={} desired={} revision={} runtime={} admission={} in-flight={} draining={} converged={}",
        plugin.plugin_id,
        plugin.version,
        plugin.scope,
        plugin.desired.enabled,
        plugin.desired.revision,
        runtime["state"].as_str().unwrap_or("unknown"),
        runtime["admission_open"],
        runtime["in_flight"],
        runtime["draining"],
        plugin.converged
    );
    if plugin.plugin_id == "builtin.shell" {
        println!("命令不受文件 write_scopes 限制，无 OS 出站或目录沙箱；禁用不取消已准入命令。");
    }
    println!(
        "影响 {}；{} 为 compat，仍受原有权限与项目策略约束。",
        plugin.affected_tools.join("/"),
        plugin.compat_tools.join("/")
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
    let bootstrap: Bootstrap =
        decode(request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?)).await?)?;
    if bootstrap.token.is_empty() || bootstrap.token.len() > 4096 {
        return Err(invalid_response());
    }
    let url = endpoint(origin, &segments)?;
    let request = match mutation {
        Some((enabled, expected_revision, key)) => client
            .put(url)
            .header("idempotency-key", key)
            .json(&json!({"enabled":enabled,"expected_revision":expected_revision})),
        None => client.get(url),
    }
    .header("x-peachsh-token", &bootstrap.token);
    let is_mutation = mutation.is_some();
    let response = request.send().await.map_err(|_| {
        if is_mutation {
            plugin_unknown()
        } else {
            Failure::fixed("unavailable", "无法连接或读取本机服务", true)
        }
    })?;
    let status = response.status();
    let process = async {
        let bytes = limited_body(response).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
        if contains_string(&value, &bootstrap.token) || !safe_strings(&value) {
            return Err(invalid_response());
        }
        if status != reqwest::StatusCode::OK {
            let error: HttpError = decode(value)?;
            let (code, expected) = match error.code.as_str() {
                Some("plugin_invalid") => ("plugin_invalid", 400),
                Some("plugin_not_found") => ("plugin_not_found", 404),
                Some("plugin_revision_conflict") => ("plugin_revision_conflict", 409),
                Some("plugin_idempotency_conflict") => ("plugin_idempotency_conflict", 409),
                Some("plugin_unavailable") => ("plugin_unavailable", 503),
                Some("plugin_internal") => ("plugin_internal", 500),
                Some("forbidden") => ("forbidden", 403),
                Some("request_failed") => ("request_failed", status.as_u16()),
                _ => return Err(invalid_response()),
            };
            if status.as_u16() != expected || error.retryable || error.error != error.message {
                return Err(invalid_response());
            }
            if is_mutation && code == "plugin_internal" {
                return Err(plugin_unknown());
            }
            return Err(Failure {
                code,
                message: error.message,
                retryable: false,
                details: None,
                after: None,
            });
        }
        match args.command {
            PluginCommand::List => {
                #[derive(Deserialize)]
                struct List {
                    plugins: Vec<PluginStatus>,
                }
                let list: List = decode(value.clone())?;
                for plugin in &list.plugins {
                    validate_plugin(plugin)?;
                }
                let directory = list
                    .plugins
                    .iter()
                    .map(|p| (p.plugin_id.as_str(), p.version.as_str()))
                    .collect::<Vec<_>>();
                if !matches!(
                    directory.as_slice(),
                    [("builtin.files", "1.0.0" | "1.1.0")]
                        | [("builtin.files", "1.2.0"), ("builtin.shell", "1.0.0")]
                ) {
                    return Err(invalid_response());
                }
                if json_output {
                    print_value(value, true)?;
                } else {
                    for plugin in &list.plugins {
                        print_plugin(plugin);
                    }
                }
            }
            PluginCommand::Status { id } => {
                #[derive(Deserialize)]
                struct Status {
                    plugin: PluginStatus,
                }
                let result: Status = decode(value.clone())?;
                validate_plugin(&result.plugin)?;
                if result.plugin.plugin_id != id {
                    return Err(invalid_response());
                }
                if json_output {
                    print_value(value, true)?;
                } else {
                    print_plugin(&result.plugin);
                }
            }
            PluginCommand::SetEnabled {
                id,
                enabled,
                expected_revision,
                ..
            } => {
                let result: PluginMutationResult = decode(value.clone())?;
                validate_plugin(&result.current)?;
                if result.receipt.plugin_id != id
                    || result.current.plugin_id != id
                    || result.receipt.enabled != enabled
                    || result.receipt.previous_revision != expected_revision
                    || expected_revision.checked_add(1) != Some(result.receipt.revision)
                    || result.current.desired.revision < result.receipt.revision
                {
                    return Err(invalid_response());
                }
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
    .await;
    process.map_err(|error| {
        if is_mutation && matches!(error.code, "internal" | "output_failed" | "unavailable") {
            plugin_unknown()
        } else {
            error
        }
    })
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
        "worktree_busy" if status == reqwest::StatusCode::CONFLICT && !error.retryable => {
            "worktree_busy"
        }
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
fn http_failure(value: Value) -> Result<Failure, Failure> {
    if value.get("error").is_some_and(Value::is_object) {
        return network_http_failure(value);
    }
    let error: HttpError = decode(value)?;
    if error.error != error.message
        || !safe_strings(&json!(error.message))
        || (error.code.as_str() == Some("worktree_busy") && error.retryable)
    {
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
    if failure.code == "worktree_busy" && status != reqwest::StatusCode::CONFLICT {
        return Err(invalid_response());
    }
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
            ProjectCommand::Worktree(_) | ProjectCommand::Budget(_) => unreachable!(),
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
        Command::Host(_)
        | Command::Runtime(_)
        | Command::Plugin(_)
        | Command::Approval(_)
        | Command::Network(_)
        | Command::Route(_)
        | Command::Account(_) => {
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
        other => budget_error_code(other)
            .map(|(code, _)| code)
            .or_else(|| project_error_code(other))
            .ok_or_else(invalid_response)?,
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
        code if budget_error_code(code).is_some() => budget_error_code(code).unwrap().1,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetWireError {
    error: NetworkHttpError,
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
fn budget_snapshot(
    value: &Value,
    snapshot: &BudgetConfigSnapshot,
    id: &str,
) -> Result<(), Failure> {
    if !worktree_fields(value, &["project_id", "revision", "config"])
        || snapshot.project_id.0 != id
        || !valid_id(id)
        || snapshot.revision > i64::MAX as u64
        || (snapshot.revision == 0) != snapshot.config.is_none()
    {
        return Err(invalid_response());
    }
    if let Some(config) = &snapshot.config
        && (!worktree_fields(
            &value["config"],
            &["limit_tokens", "per_call_reservation_tokens"],
        ) || config.validate().is_err())
    {
        return Err(invalid_response());
    }
    Ok(())
}
fn validate_budget_status(value: &Value, status: &BudgetStatus, id: &str) -> Result<(), Failure> {
    if !worktree_fields(
        value,
        &[
            "project_id",
            "snapshot",
            "mode",
            "spent_tokens",
            "held_tokens",
            "available_tokens",
            "claimed_count",
            "unresolved_count",
            "overrun_count",
            "blocked_reason",
            "coverage",
            "coverage_start_call_rowid",
        ],
    ) || status.project_id.0 != id
        || status.coverage != "since_configuration"
        || [
            status.spent_tokens,
            status.held_tokens,
            status.claimed_count,
            status.unresolved_count,
            status.overrun_count,
        ]
        .iter()
        .any(|n| *n > i64::MAX as u64)
        || status
            .coverage_start_call_rowid
            .is_some_and(|n| n > i64::MAX as u64)
    {
        return Err(invalid_response());
    }
    budget_snapshot(&value["snapshot"], &status.snapshot, id)?;
    let occupied = status
        .spent_tokens
        .checked_add(status.held_tokens)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(invalid_response)?;
    let held_count = status
        .claimed_count
        .checked_add(status.unresolved_count)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(invalid_response)?;
    if status.held_tokens < held_count
        || (held_count == 0) != (status.held_tokens == 0)
        || (status.overrun_count > 0 && status.spent_tokens == 0)
    {
        return Err(invalid_response());
    }
    match status.snapshot.config {
        None => {
            if status.mode != BudgetMode::Unconfigured
                || occupied != 0
                || held_count != 0
                || status.overrun_count != 0
                || status.available_tokens.is_some()
                || status.coverage_start_call_rowid.is_some()
                || status.blocked_reason.is_some()
            {
                return Err(invalid_response());
            }
        }
        Some(config) => {
            let available = config.limit_tokens.saturating_sub(occupied);
            let blocked = if status.unresolved_count > 0 {
                Some(BudgetBlockedReason::Unresolved)
            } else if status.overrun_count > 0 {
                Some(BudgetBlockedReason::Overrun)
            } else if available < config.per_call_reservation_tokens {
                Some(BudgetBlockedReason::Exceeded)
            } else {
                None
            };
            if status.mode != BudgetMode::Configured
                || status.coverage_start_call_rowid.is_none()
                || status.available_tokens != Some(available)
                || status.blocked_reason != blocked
            {
                return Err(invalid_response());
            }
        }
    }
    Ok(())
}
fn validate_budget_call(
    value: &Value,
    budget: &BudgetCall,
    id: &str,
    call_id: &str,
) -> Result<(), Failure> {
    if !worktree_fields(
        value,
        &[
            "project_id",
            "call_id",
            "call",
            "managed",
            "budget_revision",
            "reserved_tokens",
            "state",
            "usage",
            "reason",
        ],
    ) || !worktree_fields(
        &value["call"],
        &[
            "call_id",
            "authorization_id",
            "subject",
            "task_id",
            "purpose",
            "method",
            "origin",
            "endpoint_digest",
            "state",
            "claimed_at",
            "finished_at",
            "success",
            "http_status",
            "result",
        ],
    ) || budget.project_id.0 != id
        || budget.call_id != call_id
        || budget.call.call_id != call_id
        || budget
            .call
            .subject
            .project_id
            .as_ref()
            .map(|p| p.0.as_str())
            != Some(id)
        || budget.call.purpose != NetworkPurpose::Completion
    {
        return Err(invalid_response());
    }
    validate_network_calls(std::slice::from_ref(&budget.call))?;
    if !budget.managed {
        if budget.budget_revision.is_some()
            || budget.reserved_tokens.is_some()
            || budget.state.is_some()
            || budget.usage.is_some()
            || budget.reason.is_some()
        {
            return Err(invalid_response());
        }
        return Ok(());
    }
    if !matches!(budget.budget_revision, Some(1..=9223372036854775807))
        || !matches!(budget.reserved_tokens, Some(1..=1_000_000_000))
    {
        return Err(invalid_response());
    }
    let reservation = budget.reserved_tokens.unwrap();
    let success = budget.call.state == CallState::Finished && budget.call.success == Some(true);
    let valid = match budget.state {
        Some(BudgetCallState::Claimed) => {
            budget.call.state == CallState::Claimed
                && budget.reason.is_none()
                && budget.usage.is_none()
        }
        Some(BudgetCallState::Settled | BudgetCallState::Overrun) => {
            let usage = budget.usage.as_ref().ok_or_else(invalid_response)?;
            if !worktree_fields(
                &value["usage"],
                &["prompt_tokens", "completion_tokens", "total_tokens"],
            ) || usage.validate().is_err()
            {
                return Err(invalid_response());
            }
            success
                && if budget.state == Some(BudgetCallState::Settled) {
                    usage.total_tokens <= reservation && budget.reason.is_none()
                } else {
                    usage.total_tokens > reservation
                        && budget.reason == Some(BudgetCallReason::ReservationOverrun)
                }
        }
        Some(BudgetCallState::NotSent) => {
            budget.call.state == CallState::NotSent
                && budget.usage.is_none()
                && budget.reason == Some(BudgetCallReason::PreflightRejected)
        }
        Some(BudgetCallState::UsageMissing) => {
            success
                && budget.usage.is_none()
                && budget.reason == Some(BudgetCallReason::UsageMissing)
        }
        Some(BudgetCallState::UsageInvalid) => {
            success
                && budget.usage.is_none()
                && budget.reason == Some(BudgetCallReason::UsageInvalid)
        }
        Some(BudgetCallState::Unknown) => {
            budget.usage.is_none()
                && match (budget.call.state, budget.call.result, budget.reason) {
                    (
                        CallState::Unknown,
                        Some(CallResult::TransportUnknown),
                        Some(BudgetCallReason::TransportUnknown),
                    )
                    | (
                        CallState::Unknown,
                        Some(CallResult::Interrupted),
                        Some(BudgetCallReason::Interrupted),
                    )
                    | (
                        CallState::Unknown,
                        Some(CallResult::PersistenceUnknown),
                        Some(BudgetCallReason::PersistenceUnknown),
                    ) => true,
                    (
                        CallState::Finished,
                        Some(CallResult::HttpFailure | CallResult::InvalidResponse),
                        Some(BudgetCallReason::TransportUnknown),
                    ) => budget.call.success == Some(false),
                    _ => false,
                }
        }
        None => false,
    };
    if !valid {
        return Err(invalid_response());
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
    let bootstrap: Bootstrap =
        decode(request_bootstrap(client.get(endpoint(origin, &["api", "bootstrap"])?)).await?)?;
    if bootstrap.token.is_empty() || bootstrap.token.len() > 4096 {
        return Err(invalid_response());
    }
    let mut segments = vec!["api", "projects", id, "budget"];
    if matches!(command, BudgetCommand::Status { .. }) {
        segments.push("status");
    }
    if let Some(call) = call_id {
        segments.extend(["calls", call]);
    }
    let url = endpoint(origin, &segments)?;
    let request = if let Some((revision, config, key)) = write {
        client
            .put(url)
            .json(&json!({"expected_revision":revision,"idempotency_key":key,"config":config}))
    } else {
        client.get(url)
    }
    .header("x-peachsh-token", &bootstrap.token);
    let result: Result<(), Failure> = async {
        let response = request.send().await.map_err(|_| Failure::fixed("unavailable", "无法连接本机服务", true))?;
        let status = response.status().as_u16();
        let bytes = limited_body(response).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
        if contains_string(&value, &bootstrap.token) || !safe_strings(&value) { return Err(invalid_response()); }
        if status != 200 {
            if let Some(error) = value.get("error").filter(|v| v.is_object()) {
                if !worktree_fields(&value, &["error"]) || !worktree_fields(error, &["code", "message", "dispatch_state", "calls", "retryable"]) { return Err(invalid_response()); }
                let parsed = serde_json::from_slice::<BudgetWireError>(&bytes).map_err(|_| invalid_response())?.error;
                if !(budget_error_code(&parsed.code).map(|(_, expected)| expected) == Some(status) || (parsed.code == "budget_invalid" && matches!(status, 413 | 415))) || parsed.dispatch_state != DispatchState::NotSent || !parsed.calls.is_empty() { return Err(invalid_response()); }
                return Err(network_http_failure(value)?);
            }
            let error = http_failure(value)?;
            if error.code != "forbidden" || status != 403 { return Err(invalid_response()); }
            return Err(error);
        }
        if let Some((revision, config, _)) = write {
            let result: BudgetMutationResult = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
            if !worktree_fields(&value, &["snapshot", "replayed"]) { return Err(invalid_response()); }
            budget_snapshot(&value["snapshot"], &result.snapshot, id)?;
            if result.snapshot.config != Some(config) || revision.checked_add(1) != Some(result.snapshot.revision) { return Err(invalid_response()); }
        } else { match &command {
            BudgetCommand::Status { .. } => {
                let status: BudgetStatus = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
                validate_budget_status(&value, &status, id)?;
            }
            BudgetCommand::Call { call_id, .. } => {
                let call: BudgetCall = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
                validate_budget_call(&value, &call, id, call_id)?;
            }
            BudgetCommand::Config(_) => {
                let snapshot: BudgetConfigSnapshot = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
                budget_snapshot(&value, &snapshot, id)?;
            }
        }}
        print_value(value, json_output)?;
        if !json_output {
            let mut output = io::stdout().lock();
            writeln!(output, "token准入不是真实费用或实际消耗硬上限；unconfigured表示未启用且历史未覆盖。unknown/usage_missing/usage_invalid保留预约，overrun已计实际用量；均无自动解除或重试。")
                .and_then(|()| output.flush()).map_err(|_| Failure::fixed("output_failed", "无法写入标准输出", false))?;
        }
        Ok(())
    }.await;
    result.map_err(|error| {
        if write.is_some() && budget_error_code(error.code).is_none() && error.code != "forbidden" {
            operation_unknown("budget-config")
        } else {
            error
        }
    })
}
