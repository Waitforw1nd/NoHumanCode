//! Embedded kernel (`codaKernel/1`) adapters, in-memory session store, event hub,
//! projections, host-tool batching, turn primitives, and compaction.

pub mod compaction;
pub mod credential_source;
pub mod event_hub;
pub mod projection;
pub mod session_store;

use crate::messages::{CustomValue, KernelContent, KernelModel, SessionEntry};
use crate::provider_transport::ProviderProtocol;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub const KERNEL_PROTOCOL: &str = "codaKernel/1";
pub const MAX_BODY_BYTES: usize = 67_108_864; // 64 MiB
pub const MAX_TOOLS_PER_SESSION: usize = 512;
pub const EVENT_REPLAY_PER_RUN: usize = 4096;
pub const DEFAULT_TOOL_RESULT_MAX_CHARS: usize = 32_000;
pub const DEFAULT_PRUNE_MIN_LENGTH: usize = 12_000;
pub const DEFAULT_PRUNE_KEEP_LENGTH: usize = 2_000;
pub const IDLE_SESSION_TTL_SECS: u64 = 1800;

pub fn new_entry_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub fn utc_iso8601_now() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();
    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant's civil-from-days algorithm (UTC Gregorian calendar).
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    (year, m as u32, d as u32)
}

pub fn validate_kernel_id(field: &str, value: &str) -> Result<(), KernelError> {
    let len = value.chars().count();
    if value.is_empty() || len > 1024 {
        return Err(KernelError::invalid_request(format!(
            "{field} must be 1..=1024 Unicode scalar characters"
        )));
    }
    Ok(())
}

pub fn validate_thinking_level(level: &str) -> Result<(), KernelError> {
    if matches!(
        level,
        "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
    ) {
        Ok(())
    } else {
        Err(KernelError::invalid_request(format!(
            "invalid thinkingLevel: {level}"
        )))
    }
}

pub fn parse_embedded_protocol(raw: &str) -> Result<ProviderProtocol, KernelError> {
    ProviderProtocol::parse_embedded_model_protocol(raw).ok_or_else(|| {
        KernelError::new(
            400,
            "UNSUPPORTED_PROTOCOL",
            format!("unsupported model protocol: {raw}"),
            false,
        )
    })
}

pub mod double_option {
    use super::*;

    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

#[derive(Clone, Debug)]
pub struct KernelError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    pub details: Option<Value>,
}

impl KernelError {
    pub fn new(
        status: u16,
        code: &'static str,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retryable,
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(400, "INVALID_REQUEST", message, false)
    }

    pub fn unknown_tool(name: &str) -> Self {
        Self::new(400, "UNKNOWN_TOOL", format!("unknown tool: {name}"), false)
    }

    pub fn session_not_found(session_id: &str) -> Self {
        Self::new(
            404,
            "SESSION_NOT_FOUND",
            format!("session not found: {session_id}"),
            true,
        )
    }

    pub fn run_not_found(run_id: &str) -> Self {
        Self::new(
            404,
            "RUN_NOT_FOUND",
            format!("run not found: {run_id}"),
            false,
        )
    }

    pub fn session_diverged(revision: u64, leaf_id: Option<&str>) -> Self {
        Self::new(409, "SESSION_DIVERGED", "session revision mismatch", true).with_details(
            serde_json::json!({
                "revision": revision,
                "leafId": leaf_id,
            }),
        )
    }

    pub fn run_active(run_id: &str) -> Self {
        Self::new(
            409,
            "RUN_ACTIVE",
            format!("session already has active run {run_id}"),
            false,
        )
    }

    pub fn run_finished(run_id: &str) -> Self {
        Self::new(
            409,
            "RUN_FINISHED",
            format!("run already finished: {run_id}"),
            false,
        )
    }
}

impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for KernelError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HydrateModelConfig {
    pub model_id: String,
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunModelConfig {
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchModelConfig {
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionMode {
    Sequential,
    Parallel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KernelToolDescriptor {
    pub name: String,
    pub description: String,
    pub parameters: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<ExecutionMode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionConfig {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_recent_tokens: Option<u64>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            keep_recent_tokens: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SteeringMode {
    #[default]
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
    #[serde(rename = "all")]
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GateKind {
    BeforeModelTurn,
    AfterTurn,
    BeforeCompact,
}

impl GateKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeModelTurn => "beforeModelTurn",
            Self::AfterTurn => "afterTurn",
            Self::BeforeCompact => "beforeCompact",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextProjectionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune_boundary_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune_min_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune_keep_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_notes: Option<HashMap<String, String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelProxyConfig {
    pub base_url: String,
    pub token: String,
    pub protocol: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum AppendableSessionEntry {
    Model {
        model: KernelModel,
    },
    ThinkingLevel {
        level: String,
    },
    SessionInfo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Label {
        #[serde(rename = "targetId")]
        target_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Custom {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<CustomValue>,
    },
    CustomMessage {
        name: String,
        content: Vec<KernelContent>,
        display: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<CustomValue>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "includeInSummary"
        )]
        include_in_summary: Option<bool>,
    },
}

impl AppendableSessionEntry {
    pub fn validate(&self) -> Result<(), KernelError> {
        match self {
            Self::Model { model } => {
                validate_kernel_id("model.modelId", &model.model_id)?;
                if let Some(cp) = &model.custom_provider_id {
                    validate_kernel_id("model.customProviderId", cp)?;
                }
                Ok(())
            }
            Self::ThinkingLevel { level } => validate_kernel_id("level", level),
            Self::SessionInfo { .. } => Ok(()),
            Self::Label { target_id, .. } => validate_kernel_id("targetId", target_id),
            Self::Custom { name, .. } => validate_kernel_id("name", name),
            Self::CustomMessage { name, .. } => validate_kernel_id("name", name),
        }
    }

    pub fn into_session_entry(
        self,
        id: String,
        parent_id: Option<String>,
        timestamp: String,
        active_protocol: Option<ProviderProtocol>,
    ) -> SessionEntry {
        match self {
            Self::Model { model } => {
                let metadata = active_protocol.map(|proto| {
                    let mut map = Map::new();
                    map.insert(
                        "protocol".into(),
                        Value::String(proto.kernel_wire_name().to_owned()),
                    );
                    map
                });
                SessionEntry::Model {
                    id,
                    parent_id,
                    timestamp,
                    metadata,
                    model,
                }
            }
            Self::ThinkingLevel { level } => SessionEntry::ThinkingLevel {
                id,
                parent_id,
                timestamp,
                metadata: None,
                level,
            },
            Self::SessionInfo { name } => SessionEntry::SessionInfo {
                id,
                parent_id,
                timestamp,
                metadata: None,
                name,
            },
            Self::Label { target_id, label } => SessionEntry::Label {
                id,
                parent_id,
                timestamp,
                metadata: None,
                target_id,
                label,
            },
            Self::Custom { name, data } => SessionEntry::Custom {
                id,
                parent_id,
                timestamp,
                metadata: None,
                name,
                data,
            },
            Self::CustomMessage {
                name,
                content,
                display,
                details,
                include_in_summary,
            } => SessionEntry::CustomMessage {
                id,
                parent_id,
                timestamp,
                metadata: None,
                name,
                content,
                display,
                details,
                include_in_summary,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KernelToolResult {
    pub content: Vec<KernelContent>,
    pub details: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KernelUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode_ms: Option<u64>,
}

impl From<&crate::runtime_seams::KernelUsageBreakdown> for KernelUsage {
    fn from(b: &crate::runtime_seams::KernelUsageBreakdown) -> Self {
        Self {
            input: b.input,
            output: b.output,
            cache_read: b.cache_read,
            cache_write: b.cache_write,
            reasoning: b.reasoning,
            cache_write_1h: None,
            total_tokens: b.total_tokens,
            decode_ms: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SteerPayload {
    User {
        content: Vec<KernelContent>,
    },
    CustomMessage {
        name: String,
        content: Vec<KernelContent>,
        display: bool,
        details: Option<CustomValue>,
        include_in_summary: Option<bool>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GateAction {
    Continue,
    Finish,
    Cancel,
    Replace,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateDecision {
    pub action: GateAction,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsChangedPayload {
    pub active_tool_names: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}
