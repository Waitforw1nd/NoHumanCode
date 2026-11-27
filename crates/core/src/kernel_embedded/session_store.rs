//! In-memory `TaskRepository`, `ToolExecutor`, `RunTurnController`, per-session locking,
//! steer queue, `oneshot` gates, host-tool batching, and 30-min idle reaper (`codaKernel/1 §4–§8`).

use crate::agent_runtime::{AgentError, StrategyAction, StrategyInput, ToolVote};
use crate::domain::{Route, Task, TaskSpec};
use crate::kernel_embedded::compaction::{
    CompactionError, make_compaction_entry_id, prepare_compaction, summarize_compaction,
};
use crate::kernel_embedded::credential_source::{EmbeddedCredentialSource, EmbeddedModelError};
use crate::kernel_embedded::event_hub::{EventReplayError, NoopEmbeddedEventSink, RunEventHub};
use crate::kernel_embedded::projection::{
    ProjectionParams, build_model_view_messages, estimate_context_tokens,
};
use crate::kernel_embedded::{
    AppendableSessionEntry, CompactionConfig, ContextProjectionConfig, DEFAULT_PRUNE_KEEP_LENGTH,
    DEFAULT_PRUNE_MIN_LENGTH, DEFAULT_TOOL_RESULT_MAX_CHARS, GateAction, GateDecision, GateKind,
    HydrateModelConfig, IDLE_SESSION_TTL_SECS, KernelError, KernelToolDescriptor, KernelToolResult,
    KernelUsage, MAX_TOOLS_PER_SESSION, ModelProxyConfig, PatchModelConfig, RunModelConfig,
    SteerPayload, SteeringMode, ToolsChangedPayload, new_entry_id, parse_embedded_protocol,
    utc_iso8601_now, validate_kernel_id, validate_thinking_level,
};
use crate::messages::{
    ContentBlock, KernelContent, KernelModel, NeutralMessage, SessionBlock, SessionEntry,
    SessionEntryPayload, SessionMessageRole, ToolCallBlock,
};
use crate::provider_transport::ProviderProtocol;
use crate::redaction::CredentialSet;
use crate::runtime_seams::{
    BatchOutcome, DependencyState, EventSink, KernelUsageBreakdown, OverflowAction,
    PreparedModelContext, PreparedTools, RoundDirective, RunTurnController, SlotGuard,
    StrategyDecider, TaskRepository, ToolExecutor, TurnCommitOutcome,
};
use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast, oneshot, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct SessionModel {
    pub model_id: String,
    pub protocol: ProviderProtocol,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub thinking_level: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: String,
    pub revision: u64,
    pub leaf_id: Option<String>,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<String>,
    pub active_tool_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HydrateResponse {
    pub session_id: String,
    pub revision: u64,
    pub leaf_id: Option<String>,
    pub status: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendEntryResponse {
    pub entry: SessionEntry,
    pub revision: u64,
    pub leaf_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchResponse {
    pub revision: u64,
    pub leaf_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToolsResponse {
    pub active_tool_names: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResultPayload {
    pub entry_id: String,
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<KernelUsage>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactResponse {
    pub entry: SessionEntry,
    pub revision: u64,
    pub result: CompactionResultPayload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolResultSubmission {
    Accepted,
    Replayed,
}

#[derive(Clone, Debug)]
struct SubmittedToolResult {
    result: KernelToolResult,
    model_note: Option<String>,
}

#[derive(Clone, Debug)]
enum BatchSlot {
    PendingHost {
        call_id: String,
        name: String,
    },
    Submitted {
        call_id: String,
        name: String,
        result: KernelToolResult,
        model_note: Option<String>,
    },
    Synthesized {
        call_id: String,
        name: String,
        content: Vec<KernelContent>,
        is_error: bool,
    },
}

impl BatchSlot {
    fn is_resolved(&self) -> bool {
        !matches!(self, Self::PendingHost { .. })
    }
}

struct PendingToolBatch {
    slots: Vec<BatchSlot>,
    call_to_index: HashMap<String, usize>,
    batch_done: Arc<Notify>,
}

impl PendingToolBatch {
    fn all_resolved(&self) -> bool {
        self.slots.iter().all(BatchSlot::is_resolved)
    }
}

struct PendingGate {
    gate_id: String,
    kind: GateKind,
    tx: oneshot::Sender<GateDecision>,
}

pub struct RunStartConfig {
    pub run_id: String,
    pub base_revision: u64,
    pub system_prompt: Option<String>,
    pub model: Option<RunModelConfig>,
    pub append_entries: Vec<AppendableSessionEntry>,
    pub content: Vec<KernelContent>,
    pub thinking_level: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub leaf_id: Option<String>,
    pub max_requests: usize,
    pub runtime_context: Option<Vec<KernelContent>>,
    pub prune_boundary_call_id: Option<Option<String>>,
    pub model_proxy: ModelProxyConfig,
}

pub struct ActiveRunControl {
    pub run_id: String,
    pub hub: Arc<RunEventHub>,
    pub cancel: CancellationToken,
    done_tx: watch::Sender<bool>,
    done_rx: watch::Receiver<bool>,
    finished: AtomicBool,
    finish_requested: AtomicBool,
    steer_drained_for_next_round: AtomicBool,
    current_status: Mutex<Option<&'static str>>,
    steer_queue: Mutex<VecDeque<SteerPayload>>,
    pending_gate: Mutex<Option<PendingGate>>,
    pending_tools_changed: Mutex<Vec<ToolsChangedPayload>>,
    tool_batch: Mutex<Option<PendingToolBatch>>,
    answered_calls: Mutex<HashMap<String, SubmittedToolResult>>,
    pub run_system_prompt: Mutex<Option<String>>,
    pub run_model: Option<RunModelConfig>,
    pub run_thinking_level: Arc<Mutex<Option<String>>>,
    pub run_context_window: Option<u64>,
    pub run_max_output_tokens: Option<u32>,
    pub run_runtime_context: Mutex<Option<Vec<KernelContent>>>,
    pub run_prune_boundary_override: Mutex<Option<Option<String>>>,
    pub model_proxy: ModelProxyConfig,
    pub proxy_protocol: ProviderProtocol,
    pub user_entry_id: Option<String>,
    pub pending_assistant_entry_id: Mutex<Option<String>>,
    pub last_assistant_entry_id: Mutex<Option<String>>,
    pub last_usage_tokens: Mutex<Option<u64>>,
    streaming_block_state: Mutex<StreamingBlockState>,
}

impl ActiveRunControl {
    pub async fn wait_done(&self) {
        let mut rx = self.done_rx.clone();
        let _ = rx.wait_for(|done| *done).await;
    }

    pub fn mark_done_signal(&self) {
        let _ = self.done_tx.send(true);
    }
}

#[derive(Default)]
struct StreamingBlockState {
    next_block_num: u64,
    thinking_block_id: Option<String>,
    thinking_closed: bool,
    text_block_id: Option<String>,
    text_closed: bool,
}

impl ActiveRunControl {
    fn next_block_id(state: &mut StreamingBlockState) -> String {
        state.next_block_num += 1;
        format!("b-{}", state.next_block_num)
    }

    pub fn emit_thinking_delta(&self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let mut st = self.streaming_block_state.lock().unwrap();
        let block_id = if let Some(id) = &st.thinking_block_id {
            if st.thinking_closed {
                let id = Self::next_block_id(&mut st);
                st.thinking_block_id = Some(id.clone());
                st.thinking_closed = false;
                id
            } else {
                id.clone()
            }
        } else {
            let id = Self::next_block_id(&mut st);
            st.thinking_block_id = Some(id.clone());
            st.thinking_closed = false;
            id
        };
        drop(st);
        self.hub.emit(json!({
            "type": "thinking",
            "blockId": block_id,
            "delta": delta,
        }));
    }

    pub fn emit_text_delta(&self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let mut st = self.streaming_block_state.lock().unwrap();
        let close_thinking = if !st.thinking_closed {
            st.thinking_closed = true;
            st.thinking_block_id.clone()
        } else {
            None
        };
        let block_id = if let Some(id) = &st.text_block_id {
            if st.text_closed {
                let id = Self::next_block_id(&mut st);
                st.text_block_id = Some(id.clone());
                st.text_closed = false;
                id
            } else {
                id.clone()
            }
        } else {
            let id = Self::next_block_id(&mut st);
            st.text_block_id = Some(id.clone());
            st.text_closed = false;
            id
        };
        drop(st);
        if let Some(tid) = close_thinking {
            self.hub.emit(json!({
                "type": "thinking",
                "blockId": tid,
                "delta": "",
                "done": true,
            }));
        }
        self.hub.emit(json!({
            "type": "text",
            "blockId": block_id,
            "delta": delta,
        }));
    }

    pub fn close_streaming_blocks(&self) {
        let mut st = self.streaming_block_state.lock().unwrap();
        let close_thinking = if !st.thinking_closed {
            st.thinking_closed = true;
            st.thinking_block_id.clone()
        } else {
            None
        };
        let close_text = if !st.text_closed {
            st.text_closed = true;
            st.text_block_id.clone()
        } else {
            None
        };
        st.thinking_block_id = None;
        st.text_block_id = None;
        drop(st);
        if let Some(tid) = close_thinking {
            self.hub.emit(json!({
                "type": "thinking",
                "blockId": tid,
                "delta": "",
                "done": true,
            }));
        }
        if let Some(bid) = close_text {
            self.hub.emit(json!({
                "type": "text",
                "blockId": bid,
                "delta": "",
                "done": true,
            }));
        }
    }

    fn reset_streaming_blocks_for_retry(&self) {
        let mut st = self.streaming_block_state.lock().unwrap();
        st.thinking_block_id = None;
        st.thinking_closed = false;
        st.text_block_id = None;
        st.text_closed = false;
    }

    pub fn emit_status(&self, state: &'static str) {
        let mut cur = self.current_status.lock().unwrap();
        if *cur == Some(state) {
            return;
        }
        *cur = Some(state);
        drop(cur);
        self.hub.emit(json!({ "type": "status", "state": state }));
    }

    pub fn flush_tools_changed(&self) {
        let pending: Vec<ToolsChangedPayload> = {
            let mut q = self.pending_tools_changed.lock().unwrap();
            std::mem::take(&mut *q)
        };
        for item in pending {
            self.hub.emit(json!({
                "type": "toolsChanged",
                "activeToolNames": item.active_tool_names,
                "added": item.added,
                "removed": item.removed,
            }));
        }
    }
}

struct FinishedRunRecord {
    run_id: String,
    hub: Arc<RunEventHub>,
}

pub struct EmbeddedSessionState {
    pub session_id: String,
    pub cwd: String,
    pub system_prompt: String,
    pub model: SessionModel,
    pub tools: Vec<KernelToolDescriptor>,
    pub active_tool_names: Vec<String>,
    pub tool_result_max_chars: Option<usize>,
    pub compaction: CompactionConfig,
    pub steering_mode: SteeringMode,
    pub gates: HashSet<GateKind>,
    pub context_projection: ContextProjectionConfig,
    pub patched_runtime_context: Option<Vec<KernelContent>>,
    pub entries: Vec<SessionEntry>,
    pub entries_by_id: HashMap<String, usize>,
    pub revision: u64,
    pub leaf_id: Option<String>,
    pub active_run: Option<Arc<ActiveRunControl>>,
    last_finished_run: Option<FinishedRunRecord>,
    expired_run_ids: HashSet<String>,
    pub last_activity: Instant,
}

impl EmbeddedSessionState {
    fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    pub fn append_entry(&mut self, entry: SessionEntry) {
        let id = entry.id().to_owned();
        let idx = self.entries.len();
        self.entries.push(entry);
        self.entries_by_id.insert(id.clone(), idx);
        self.leaf_id = Some(id);
        self.revision = self.revision.saturating_add(1);
        self.touch();
    }

    pub fn get_branch(&self) -> Vec<SessionEntry> {
        let Some(start) = self.leaf_id.as_deref() else {
            return Vec::new();
        };
        let mut branch = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(start);
        while let Some(id) = current {
            if !seen.insert(id) {
                break;
            }
            let Some(&idx) = self.entries_by_id.get(id) else {
                break;
            };
            let entry = &self.entries[idx];
            branch.push(entry.clone());
            current = entry.parent_id();
        }
        branch.reverse();
        branch
    }

    fn summary(&self) -> SessionSummary {
        let (status, active_run_id) = match &self.active_run {
            Some(run) => ("running", Some(run.run_id.clone())),
            None => ("idle", None),
        };
        SessionSummary {
            session_id: self.session_id.clone(),
            revision: self.revision,
            leaf_id: self.leaf_id.clone(),
            status,
            active_run_id,
            active_tool_names: self.active_tool_names.clone(),
        }
    }

    fn finish_active_run(&mut self, run_id: &str) {
        self.patched_runtime_context = None;
        if let Some(active) = self.active_run.take() {
            if active.run_id == run_id {
                if let Some(prev) = self.last_finished_run.replace(FinishedRunRecord {
                    run_id: active.run_id.clone(),
                    hub: active.hub.clone(),
                }) {
                    self.expired_run_ids.insert(prev.run_id);
                }
            } else {
                self.active_run = Some(active);
            }
        }
        self.touch();
    }
}

pub struct EmbeddedSessionHandle {
    pub op_lock: Arc<AsyncMutex<()>>,
    pub state: Mutex<EmbeddedSessionState>,
}

#[derive(Default)]
pub struct EmbeddedSessionStore {
    sessions: RwLock<HashMap<String, Arc<EmbeddedSessionHandle>>>,
    media_enabled: bool,
}

#[derive(Clone)]
pub struct HydrateInput {
    pub cwd: String,
    pub system_prompt: String,
    pub model: HydrateModelConfig,
    pub tools: Vec<KernelToolDescriptor>,
    pub active_tool_names: Vec<String>,
    pub tool_result_max_chars: Option<Option<usize>>,
    pub compaction: CompactionConfig,
    pub steering_mode: SteeringMode,
    pub gates: Vec<GateKind>,
    pub context_projection: Option<ContextProjectionConfig>,
    pub leaf_id: Option<String>,
    pub entries: Vec<SessionEntry>,
}

#[derive(Clone, Default)]
pub struct PatchInput {
    pub system_prompt: Option<String>,
    pub model: Option<PatchModelConfig>,
    pub tools: Option<Vec<KernelToolDescriptor>>,
    pub active_tool_names: Option<Vec<String>>,
    pub compaction: Option<CompactionConfig>,
    pub gates: Option<Vec<GateKind>>,
    pub tool_result_max_chars: Option<Option<usize>>,
    pub runtime_context: Option<Option<Vec<KernelContent>>>,
    pub prune_boundary_call_id: Option<Option<String>>,
}

#[derive(Clone)]
pub struct CompactInput {
    pub base_revision: u64,
    pub system_prompt: Option<String>,
    pub instructions: Option<String>,
    pub keep_recent_tokens: Option<u64>,
    pub model: Option<RunModelConfig>,
    pub thinking_level: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub model_proxy: ModelProxyConfig,
}

fn validate_tools_and_active(
    tools: &[KernelToolDescriptor],
    active_names: &[String],
) -> Result<(), KernelError> {
    if tools.len() > MAX_TOOLS_PER_SESSION {
        return Err(KernelError::invalid_request(format!(
            "tools count {} exceeds maxToolsPerSession ({MAX_TOOLS_PER_SESSION})",
            tools.len()
        )));
    }
    let mut defined = HashSet::with_capacity(tools.len());
    for tool in tools {
        validate_kernel_id("tools[].name", &tool.name)?;
        if !defined.insert(tool.name.as_str()) {
            return Err(KernelError::invalid_request(format!(
                "duplicate tool descriptor: {}",
                tool.name
            )));
        }
    }
    for name in active_names {
        if !defined.contains(name.as_str()) {
            return Err(KernelError::unknown_tool(name));
        }
    }
    Ok(())
}

fn compute_tools_diff(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let old_set: HashSet<&str> = old.iter().map(String::as_str).collect();
    let new_set: HashSet<&str> = new.iter().map(String::as_str).collect();
    let added: Vec<String> = new
        .iter()
        .filter(|n| !old_set.contains(n.as_str()))
        .cloned()
        .collect();
    let removed: Vec<String> = old
        .iter()
        .filter(|n| !new_set.contains(n.as_str()))
        .cloned()
        .collect();
    (added, removed)
}

fn validate_hydrate_model(model: &HydrateModelConfig) -> Result<SessionModel, KernelError> {
    validate_kernel_id("model.modelId", &model.model_id)?;
    let protocol = parse_embedded_protocol(&model.protocol)?;
    if let Some(cw) = model.context_window
        && !(1..=10_485_760).contains(&cw)
    {
        return Err(KernelError::invalid_request(
            "model.contextWindow must be in 1..=10485760",
        ));
    }
    if let Some(max_out) = model.max_output_tokens
        && !(1..=1_048_576).contains(&max_out)
    {
        return Err(KernelError::invalid_request(
            "model.maxOutputTokens must be in 1..=1048576",
        ));
    }
    if let Some(tl) = &model.thinking_level {
        validate_thinking_level(tl)?;
    }
    Ok(SessionModel {
        model_id: model.model_id.clone(),
        protocol,
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        thinking_level: model.thinking_level.clone(),
    })
}

fn truncate_kernel_content(
    content: Vec<KernelContent>,
    max_chars: Option<usize>,
) -> Vec<KernelContent> {
    let Some(max) = max_chars else {
        return content;
    };
    let total: usize = content
        .iter()
        .filter_map(|part| match part {
            KernelContent::Text { text } => Some(text.chars().count()),
            KernelContent::Image { .. }
            | KernelContent::Audio { .. }
            | KernelContent::Video { .. }
            | KernelContent::Resource { .. } => None,
        })
        .sum();
    if total <= max {
        return content;
    }

    let (head_budget, tail_budget) = if max >= 4 {
        (max / 2, max / 4)
    } else {
        (max, 0)
    };
    let combined_text: String = content
        .iter()
        .filter_map(|part| match part {
            KernelContent::Text { text } => Some(text.as_str()),
            KernelContent::Image { .. }
            | KernelContent::Audio { .. }
            | KernelContent::Video { .. }
            | KernelContent::Resource { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let combined_chars = combined_text.chars().count();
    let head_end = combined_text
        .char_indices()
        .nth(head_budget)
        .map(|(i, _)| i)
        .unwrap_or(combined_text.len());
    let tail_start = combined_text
        .char_indices()
        .nth(combined_chars.saturating_sub(tail_budget))
        .map(|(i, _)| i)
        .unwrap_or(head_end)
        .max(head_end);
    let omitted = combined_chars.saturating_sub(head_budget + tail_budget);
    let truncated_text = format!(
        "{}\n[... truncated {} chars ...]\n{}",
        &combined_text[..head_end],
        omitted,
        &combined_text[tail_start..]
    );

    let mut out = vec![KernelContent::Text {
        text: truncated_text,
    }];
    for part in content {
        if !matches!(part, KernelContent::Text { .. }) {
            out.push(part);
        }
    }
    out
}

fn kernel_contents_to_neutral_blocks(content: &[KernelContent]) -> Vec<ContentBlock> {
    content
        .iter()
        .map(KernelContent::to_content_block)
        .collect()
}

fn kernel_usage_from_value(usage: &Value, breakdown: Option<&KernelUsageBreakdown>) -> KernelUsage {
    if let Some(b) = breakdown {
        return KernelUsage::from(b);
    }
    let obj = usage.as_object();
    let prompt = obj
        .and_then(|m| m.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = obj
        .and_then(|m| m.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = obj
        .and_then(|m| m.get("total_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| prompt.saturating_add(output));
    KernelUsage {
        input: prompt,
        output,
        cache_read: 0,
        cache_write: 0,
        reasoning: None,
        cache_write_1h: None,
        total_tokens: Some(total),
        decode_ms: None,
    }
}

fn validate_kernel_contents_for_media(
    content: &[KernelContent],
    media_enabled: bool,
) -> Result<(), KernelError> {
    for part in content {
        part.validate_for_embedded(media_enabled)
            .map_err(KernelError::invalid_request)?;
    }
    Ok(())
}

fn validate_session_blocks_for_media(
    blocks: &[SessionBlock],
    media_enabled: bool,
) -> Result<(), KernelError> {
    for block in blocks {
        block
            .validate_for_embedded(media_enabled)
            .map_err(KernelError::invalid_request)?;
    }
    Ok(())
}

fn validate_session_entry_for_media(
    entry: &SessionEntry,
    media_enabled: bool,
) -> Result<(), KernelError> {
    match entry {
        SessionEntry::Message { content, .. } => {
            validate_session_blocks_for_media(content, media_enabled)
        }
        SessionEntry::ToolResult { content, .. } | SessionEntry::CustomMessage { content, .. } => {
            validate_kernel_contents_for_media(content, media_enabled)
        }
        SessionEntry::ContextEdit {
            replacement: Some(blocks),
            ..
        } => validate_session_blocks_for_media(blocks, media_enabled),
        _ => Ok(()),
    }
}

fn validate_appendable_entry_for_media(
    entry: &AppendableSessionEntry,
    media_enabled: bool,
) -> Result<(), KernelError> {
    match entry {
        AppendableSessionEntry::CustomMessage { content, .. } => {
            validate_kernel_contents_for_media(content, media_enabled)
        }
        _ => Ok(()),
    }
}

impl EmbeddedSessionStore {
    pub fn new() -> Arc<Self> {
        Self::with_media_enabled(false)
    }

    pub fn with_media_enabled(media_enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            sessions: RwLock::new(HashMap::new()),
            media_enabled,
        })
    }

    pub fn media_enabled(&self) -> bool {
        self.media_enabled
    }

    fn get_handle(&self, session_id: &str) -> Result<Arc<EmbeddedSessionHandle>, KernelError> {
        self.sessions
            .read()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| KernelError::session_not_found(session_id))
    }

    pub async fn hydrate_session(
        &self,
        session_id: String,
        input: HydrateInput,
    ) -> Result<(HydrateResponse, bool), KernelError> {
        validate_kernel_id("sessionId", &session_id)?;
        if input.cwd.trim().is_empty() {
            return Err(KernelError::invalid_request("cwd cannot be empty"));
        }
        let model = validate_hydrate_model(&input.model)?;
        validate_tools_and_active(&input.tools, &input.active_tool_names)?;
        if let Some(Some(0)) = input.tool_result_max_chars {
            return Err(KernelError::invalid_request(
                "toolResultMaxChars must be positive",
            ));
        }

        let mut entries_by_id = HashMap::with_capacity(input.entries.len());
        for (idx, entry) in input.entries.iter().enumerate() {
            let eid = entry.id();
            validate_kernel_id("history.entries[].id", eid)?;
            validate_session_entry_for_media(entry, self.media_enabled)?;
            if entries_by_id.insert(eid.to_owned(), idx).is_some() {
                return Err(KernelError::invalid_request(format!(
                    "duplicate history entry id: {eid}"
                )));
            }
        }
        if let Some(ref leaf) = input.leaf_id {
            validate_kernel_id("history.leafId", leaf)?;
        }

        let tool_result_max_chars = match input.tool_result_max_chars {
            None => Some(DEFAULT_TOOL_RESULT_MAX_CHARS),
            Some(None) => None,
            Some(Some(n)) => Some(n),
        };
        let gates: HashSet<GateKind> = input.gates.into_iter().collect();
        let context_projection = input.context_projection.unwrap_or_default();

        let existing = { self.sessions.read().unwrap().get(&session_id).cloned() };
        if let Some(handle) = existing {
            let _op = handle.op_lock.lock().await;
            let mut st = handle.state.lock().unwrap();
            if let Some(active) = &st.active_run {
                return Err(KernelError::run_active(&active.run_id));
            }
            if let Some(ref leaf) = input.leaf_id
                && !entries_by_id.contains_key(leaf.as_str())
            {
                return Err(KernelError::invalid_request(format!(
                    "history.leafId not found in entries: {leaf}"
                )));
            }
            st.cwd = input.cwd;
            st.system_prompt = input.system_prompt;
            st.model = model;
            st.tools = input.tools;
            st.active_tool_names = input.active_tool_names;
            st.tool_result_max_chars = tool_result_max_chars;
            st.compaction = input.compaction;
            st.steering_mode = input.steering_mode;
            st.gates = gates;
            st.context_projection = context_projection;
            st.patched_runtime_context = None;
            st.entries = input.entries;
            st.entries_by_id = entries_by_id;
            st.revision = 1;
            st.leaf_id = input.leaf_id.clone();
            st.touch();
            return Ok((
                HydrateResponse {
                    session_id,
                    revision: 1,
                    leaf_id: input.leaf_id,
                    status: "idle",
                },
                false,
            ));
        }

        if let Some(ref leaf) = input.leaf_id
            && !entries_by_id.contains_key(leaf.as_str())
        {
            return Err(KernelError::invalid_request(format!(
                "history.leafId not found in entries: {leaf}"
            )));
        }

        let handle = Arc::new(EmbeddedSessionHandle {
            op_lock: Arc::new(AsyncMutex::new(())),
            state: Mutex::new(EmbeddedSessionState {
                session_id: session_id.clone(),
                cwd: input.cwd,
                system_prompt: input.system_prompt,
                model,
                tools: input.tools,
                active_tool_names: input.active_tool_names,
                tool_result_max_chars,
                compaction: input.compaction,
                steering_mode: input.steering_mode,
                gates,
                context_projection,
                patched_runtime_context: None,
                entries: input.entries,
                entries_by_id,
                revision: 1,
                leaf_id: input.leaf_id.clone(),
                active_run: None,
                last_finished_run: None,
                expired_run_ids: HashSet::new(),
                last_activity: Instant::now(),
            }),
        });
        let mut map = self.sessions.write().unwrap();
        if map.contains_key(&session_id) {
            // Lost race with concurrent PUT; reject or overwrite via recursive retry?
            return Err(KernelError::invalid_request(
                "concurrent session creation conflict",
            ));
        }
        map.insert(session_id.clone(), handle);
        Ok((
            HydrateResponse {
                session_id,
                revision: 1,
                leaf_id: input.leaf_id,
                status: "idle",
            },
            true,
        ))
    }

    pub fn get_session(&self, session_id: &str) -> Result<SessionSummary, KernelError> {
        let handle = self.get_handle(session_id)?;
        let mut st = handle.state.lock().unwrap();
        st.touch();
        Ok(st.summary())
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<(), KernelError> {
        let handle = self.get_handle(session_id)?;
        let active_to_abort = {
            let st = handle.state.lock().unwrap();
            st.active_run.clone()
        };
        if let Some(active) = active_to_abort {
            active.cancel.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(3), active.wait_done()).await;
        }
        self.sessions.write().unwrap().remove(session_id);
        Ok(())
    }

    pub fn patch_session(
        &self,
        session_id: &str,
        patch: PatchInput,
    ) -> Result<SessionSummary, KernelError> {
        let handle = self.get_handle(session_id)?;
        let mut st = handle.state.lock().unwrap();

        let next_tools = patch.tools.as_deref().unwrap_or(&st.tools);
        let next_active = patch
            .active_tool_names
            .as_deref()
            .unwrap_or(&st.active_tool_names);
        validate_tools_and_active(next_tools, next_active)?;

        if let Some(ref pm) = patch.model {
            validate_kernel_id("model.modelId", &pm.model_id)?;
            if let Some(ref p) = pm.protocol {
                parse_embedded_protocol(p)?;
            }
            if let Some(cw) = pm.context_window
                && !(1..=10_485_760).contains(&cw)
            {
                return Err(KernelError::invalid_request(
                    "model.contextWindow must be in 1..=10485760",
                ));
            }
            if let Some(max_out) = pm.max_output_tokens
                && !(1..=1_048_576).contains(&max_out)
            {
                return Err(KernelError::invalid_request(
                    "model.maxOutputTokens must be in 1..=1048576",
                ));
            }
            if let Some(ref tl) = pm.thinking_level {
                validate_thinking_level(tl)?;
            }
        }
        if let Some(Some(0)) = patch.tool_result_max_chars {
            return Err(KernelError::invalid_request(
                "toolResultMaxChars must be positive",
            ));
        }
        if let Some(Some(ref boundary)) = patch.prune_boundary_call_id {
            validate_kernel_id("pruneBoundaryCallId", boundary)?;
        }
        if let Some(Some(ref rc)) = patch.runtime_context {
            validate_kernel_contents_for_media(rc, self.media_enabled)?;
        }

        let has_rt_ctx_patch = patch.runtime_context.is_some();
        let has_prune_patch = patch.prune_boundary_call_id.is_some();
        let patched_thinking_level = patch
            .model
            .as_ref()
            .and_then(|pm| pm.thinking_level.clone());

        let old_active = st.active_tool_names.clone();
        let patched_system_prompt = patch.system_prompt.clone();
        if let Some(sp) = patch.system_prompt {
            st.system_prompt = sp;
        }
        if let Some(pm) = patch.model {
            let protocol = match pm.protocol {
                Some(ref raw) => parse_embedded_protocol(raw)?,
                None => st.model.protocol,
            };
            st.model = SessionModel {
                model_id: pm.model_id,
                protocol,
                context_window: pm.context_window.or(st.model.context_window),
                max_output_tokens: pm.max_output_tokens.or(st.model.max_output_tokens),
                thinking_level: pm
                    .thinking_level
                    .or_else(|| st.model.thinking_level.clone()),
            };
        }
        if let Some(tools) = patch.tools {
            st.tools = tools;
        }
        if let Some(active) = patch.active_tool_names {
            st.active_tool_names = active;
        }
        if let Some(comp) = patch.compaction {
            st.compaction = comp;
        }
        if let Some(gates) = patch.gates {
            st.gates = gates.into_iter().collect();
        }
        if let Some(max_chars) = patch.tool_result_max_chars {
            st.tool_result_max_chars = max_chars;
        }
        if let Some(rt_ctx) = patch.runtime_context {
            st.patched_runtime_context = rt_ctx;
        }
        if let Some(boundary) = patch.prune_boundary_call_id {
            st.context_projection.prune_boundary_call_id = boundary;
        }

        let (added, removed) = compute_tools_diff(&old_active, &st.active_tool_names);
        if let Some(active_run) = &st.active_run {
            if let Some(ref sp) = patched_system_prompt {
                *active_run.run_system_prompt.lock().unwrap() = Some(sp.clone());
            }
            if let Some(ref tl) = patched_thinking_level {
                *active_run.run_thinking_level.lock().unwrap() = Some(tl.clone());
            }
            if has_rt_ctx_patch {
                *active_run.run_runtime_context.lock().unwrap() = None;
            }
            if has_prune_patch {
                *active_run.run_prune_boundary_override.lock().unwrap() = None;
            }
            if !added.is_empty() || !removed.is_empty() {
                active_run
                    .pending_tools_changed
                    .lock()
                    .unwrap()
                    .push(ToolsChangedPayload {
                        active_tool_names: st.active_tool_names.clone(),
                        added,
                        removed,
                    });
            }
        }
        st.touch();
        Ok(st.summary())
    }

    pub fn set_active_tools(
        &self,
        session_id: &str,
        active_tool_names: Vec<String>,
    ) -> Result<ActiveToolsResponse, KernelError> {
        let handle = self.get_handle(session_id)?;
        let mut st = handle.state.lock().unwrap();
        validate_tools_and_active(&st.tools, &active_tool_names)?;
        let (added, removed) = compute_tools_diff(&st.active_tool_names, &active_tool_names);
        st.active_tool_names = active_tool_names.clone();
        if (!added.is_empty() || !removed.is_empty())
            && let Some(active_run) = &st.active_run
        {
            active_run
                .pending_tools_changed
                .lock()
                .unwrap()
                .push(ToolsChangedPayload {
                    active_tool_names: active_tool_names.clone(),
                    added: added.clone(),
                    removed: removed.clone(),
                });
        }
        st.touch();
        Ok(ActiveToolsResponse {
            active_tool_names,
            added,
            removed,
        })
    }

    pub async fn append_entry(
        &self,
        session_id: &str,
        base_revision: Option<u64>,
        entry: AppendableSessionEntry,
        deliver_as_steer: bool,
    ) -> Result<Option<AppendEntryResponse>, KernelError> {
        entry.validate()?;
        validate_appendable_entry_for_media(&entry, self.media_enabled)?;
        let handle = self.get_handle(session_id)?;

        // Fast-path steer enqueue when active run exists (atomic under `handle.state` lock).
        if deliver_as_steer {
            let mut st = handle.state.lock().unwrap();
            st.touch();
            if let Some(active) = &st.active_run {
                if active.finished.load(Ordering::Acquire) {
                    return Err(KernelError::run_finished(&active.run_id));
                }
                let AppendableSessionEntry::CustomMessage {
                    name,
                    content,
                    display,
                    details,
                    include_in_summary,
                } = entry
                else {
                    return Err(KernelError::invalid_request(
                        "deliverAs: 'steer' requires a customMessage entry",
                    ));
                };
                active
                    .steer_queue
                    .lock()
                    .unwrap()
                    .push_back(SteerPayload::CustomMessage {
                        name,
                        content,
                        display,
                        details,
                        include_in_summary,
                    });
                return Ok(None);
            }
        }

        let _op = handle.op_lock.lock().await;
        let mut st = handle.state.lock().unwrap();
        if let Some(active) = &st.active_run {
            if active.finished.load(Ordering::Acquire) {
                return Err(KernelError::run_finished(&active.run_id));
            }
            if deliver_as_steer {
                let AppendableSessionEntry::CustomMessage {
                    name,
                    content,
                    display,
                    details,
                    include_in_summary,
                } = entry
                else {
                    return Err(KernelError::invalid_request(
                        "deliverAs: 'steer' requires a customMessage entry",
                    ));
                };
                active
                    .steer_queue
                    .lock()
                    .unwrap()
                    .push_back(SteerPayload::CustomMessage {
                        name,
                        content,
                        display,
                        details,
                        include_in_summary,
                    });
                return Ok(None);
            }
            return Err(KernelError::run_active(&active.run_id));
        }

        if let Some(base) = base_revision
            && base != st.revision
        {
            return Err(KernelError::session_diverged(
                st.revision,
                st.leaf_id.as_deref(),
            ));
        }
        if base_revision.is_none() && !deliver_as_steer {
            return Err(KernelError::invalid_request(
                "baseRevision is required when deliverAs is 'append'",
            ));
        }

        let id = new_entry_id();
        let parent_id = st.leaf_id.clone();
        let ts = utc_iso8601_now();
        let active_proto = Some(st.model.protocol);
        let session_entry = entry.into_session_entry(id, parent_id, ts, active_proto);
        st.append_entry(session_entry.clone());
        Ok(Some(AppendEntryResponse {
            entry: session_entry,
            revision: st.revision,
            leaf_id: st.leaf_id.clone(),
        }))
    }

    pub async fn branch_session(
        &self,
        session_id: &str,
        base_revision: u64,
        leaf_id: Option<String>,
    ) -> Result<BranchResponse, KernelError> {
        if let Some(ref leaf) = leaf_id {
            validate_kernel_id("leafId", leaf)?;
        }
        let handle = self.get_handle(session_id)?;
        let _op = handle.op_lock.lock().await;
        let mut st = handle.state.lock().unwrap();
        if let Some(active) = &st.active_run {
            return Err(KernelError::run_active(&active.run_id));
        }
        if base_revision != st.revision {
            return Err(KernelError::session_diverged(
                st.revision,
                st.leaf_id.as_deref(),
            ));
        }
        if let Some(ref leaf) = leaf_id
            && !st.entries_by_id.contains_key(leaf.as_str())
        {
            return Err(KernelError::invalid_request(format!(
                "unknown leafId: {leaf}"
            )));
        }
        st.leaf_id = leaf_id.clone();
        st.revision = st.revision.saturating_add(1);
        st.touch();
        Ok(BranchResponse {
            revision: st.revision,
            leaf_id,
        })
    }

    pub async fn compact_session(
        &self,
        session_id: &str,
        input: CompactInput,
    ) -> Result<CompactResponse, KernelError> {
        let proxy_protocol = parse_embedded_protocol(&input.model_proxy.protocol)?;
        if let Some(ref m) = input.model {
            validate_kernel_id("model.modelId", &m.model_id)?;
        }
        if let Some(ref tl) = input.thinking_level {
            validate_thinking_level(tl)?;
        }
        if let Some(cw) = input.context_window
            && !(1..=10_485_760).contains(&cw)
        {
            return Err(KernelError::invalid_request(
                "contextWindow must be in 1..=10485760",
            ));
        }
        if let Some(max_out) = input.max_output_tokens
            && !(1..=1_048_576).contains(&max_out)
        {
            return Err(KernelError::invalid_request(
                "maxOutputTokens must be in 1..=1048576",
            ));
        }
        let handle = self.get_handle(session_id)?;
        let _op = handle.op_lock.lock().await;

        let (branch, model_id, max_out, thinking_level, keep_recent, last_usage_tokens) = {
            let mut st = handle.state.lock().unwrap();
            if let Some(active) = &st.active_run {
                return Err(KernelError::run_active(&active.run_id));
            }
            if input.base_revision != st.revision {
                return Err(KernelError::session_diverged(
                    st.revision,
                    st.leaf_id.as_deref(),
                ));
            }
            st.touch();
            let branch = st.get_branch();
            let model_id = input
                .model
                .as_ref()
                .map(|m| m.model_id.clone())
                .unwrap_or_else(|| st.model.model_id.clone());
            let max_out = input
                .max_output_tokens
                .or(st.model.max_output_tokens)
                .unwrap_or(4096);
            let tl = input
                .thinking_level
                .clone()
                .or_else(|| st.model.thinking_level.clone());
            let keep = input
                .keep_recent_tokens
                .or(st.compaction.keep_recent_tokens)
                .unwrap_or(20_000);
            (branch, model_id, max_out, tl, keep, None)
        };

        let new_id = make_compaction_entry_id();
        let prep = prepare_compaction(
            &branch,
            proxy_protocol,
            keep_recent,
            &new_id,
            last_usage_tokens,
        );
        let cancel = CancellationToken::new();
        let (summary, usage_bd) = summarize_compaction(
            input.model_proxy,
            proxy_protocol,
            &model_id,
            max_out,
            thinking_level,
            &prep,
            input.system_prompt.as_deref(),
            input.instructions.as_deref(),
            &cancel,
        )
        .await
        .map_err(|err| match err {
            CompactionError::EmptySummary => KernelError::new(
                502,
                "COMPACTION_EMPTY_SUMMARY",
                "compaction model returned an empty summary",
                true,
            ),
            CompactionError::Upstream(e) => {
                KernelError::new(502, "MODEL_UPSTREAM_ERROR", e.message, true)
            }
            CompactionError::Internal(msg) => {
                KernelError::new(502, "MODEL_UPSTREAM_ERROR", msg, true)
            }
        })?;

        let mut st = handle.state.lock().unwrap();
        if let Some(active) = &st.active_run {
            return Err(KernelError::run_active(&active.run_id));
        }
        if input.base_revision != st.revision {
            return Err(KernelError::session_diverged(
                st.revision,
                st.leaf_id.as_deref(),
            ));
        }
        let parent_id = st.leaf_id.clone();
        let entry = SessionEntry::Compaction {
            id: new_id.clone(),
            parent_id,
            timestamp: utc_iso8601_now(),
            metadata: None,
            summary: summary.clone(),
            first_kept_entry_id: prep.first_kept_entry_id.clone(),
            tokens_before: prep.tokens_before,
        };
        st.append_entry(entry.clone());
        let usage = usage_bd.as_ref().map(KernelUsage::from);
        Ok(CompactResponse {
            entry,
            revision: st.revision,
            result: CompactionResultPayload {
                entry_id: new_id,
                summary,
                first_kept_entry_id: prep.first_kept_entry_id,
                tokens_before: prep.tokens_before,
                usage,
            },
        })
    }

    pub async fn start_run(
        self: &Arc<Self>,
        session_id: &str,
        cfg: RunStartConfig,
    ) -> Result<
        (
            Arc<ActiveRunControl>,
            Vec<crate::kernel_embedded::event_hub::SequencedKernelEvent>,
            broadcast::Receiver<crate::kernel_embedded::event_hub::SequencedKernelEvent>,
            bool,
        ),
        KernelError,
    > {
        validate_kernel_id("runId", &cfg.run_id)?;
        let proxy_protocol = parse_embedded_protocol(&cfg.model_proxy.protocol)?;
        if let Some(ref m) = cfg.model {
            validate_kernel_id("model.modelId", &m.model_id)?;
        }
        if let Some(ref tl) = cfg.thinking_level {
            validate_thinking_level(tl)?;
        }
        if let Some(cw) = cfg.context_window
            && !(1..=10_485_760).contains(&cw)
        {
            return Err(KernelError::invalid_request(
                "contextWindow must be in 1..=10485760",
            ));
        }
        if let Some(max_out) = cfg.max_output_tokens
            && !(1..=1_048_576).contains(&max_out)
        {
            return Err(KernelError::invalid_request(
                "maxOutputTokens must be in 1..=1048576",
            ));
        }
        if let Some(ref leaf) = cfg.leaf_id {
            validate_kernel_id("leafId", leaf)?;
        }
        if let Some(Some(ref boundary)) = cfg.prune_boundary_call_id {
            validate_kernel_id("pruneBoundaryCallId", boundary)?;
        }
        for ae in &cfg.append_entries {
            ae.validate()?;
            validate_appendable_entry_for_media(ae, self.media_enabled)?;
        }
        validate_kernel_contents_for_media(&cfg.content, self.media_enabled)?;
        if let Some(ref rc) = cfg.runtime_context {
            validate_kernel_contents_for_media(rc, self.media_enabled)?;
        }
        if !(1..=65_535).contains(&cfg.max_requests) {
            return Err(KernelError::invalid_request(
                "maxRequests must be in 1..=65535",
            ));
        }

        let handle = self.get_handle(session_id)?;
        let _op = handle.op_lock.lock().await;

        let (run_ctrl, task, rx_tuple) = {
            let mut st = handle.state.lock().unwrap();
            if let Some(active) = &st.active_run {
                if active.run_id == cfg.run_id {
                    return Err(KernelError::new(
                        409,
                        "RUN_EXISTS",
                        format!("run already exists: {}", cfg.run_id),
                        false,
                    ));
                }
                return Err(KernelError::run_active(&active.run_id));
            }
            if st
                .last_finished_run
                .as_ref()
                .is_some_and(|f| f.run_id == cfg.run_id)
                || st.expired_run_ids.contains(&cfg.run_id)
            {
                return Err(KernelError::new(
                    409,
                    "RUN_EXISTS",
                    format!("run already exists: {}", cfg.run_id),
                    false,
                ));
            }
            if cfg.base_revision != st.revision {
                return Err(KernelError::session_diverged(
                    st.revision,
                    st.leaf_id.as_deref(),
                ));
            }
            if let Some(ref leaf) = cfg.leaf_id {
                if !st.entries_by_id.contains_key(leaf.as_str()) {
                    return Err(KernelError::invalid_request(format!(
                        "unknown leafId: {leaf}"
                    )));
                }
                st.leaf_id = Some(leaf.clone());
            }

            let hub = Arc::new(RunEventHub::new(session_id.to_owned(), cfg.run_id.clone()));
            hub.emit(json!({ "type": "status", "state": "running" }));

            for ae in cfg.append_entries {
                let id = new_entry_id();
                let parent_id = st.leaf_id.clone();
                let ts = utc_iso8601_now();
                let entry = ae.into_session_entry(id, parent_id, ts, Some(proxy_protocol));
                st.append_entry(entry.clone());
                hub.emit(json!({
                    "type": "entry",
                    "entry": entry,
                    "revision": st.revision,
                }));
            }

            let user_entry_id = if !cfg.content.is_empty() {
                let id = new_entry_id();
                let parent_id = st.leaf_id.clone();
                let ts = utc_iso8601_now();
                let blocks: Vec<SessionBlock> = cfg
                    .content
                    .iter()
                    .map(KernelContent::to_session_block)
                    .collect();
                let entry = SessionEntry::Message {
                    id: id.clone(),
                    parent_id,
                    timestamp: ts,
                    metadata: None,
                    role: SessionMessageRole::User,
                    content: blocks,
                };
                st.append_entry(entry.clone());
                hub.emit(json!({
                    "type": "entry",
                    "entry": entry,
                    "revision": st.revision,
                }));
                hub.emit(json!({
                    "type": "userMessage",
                    "entryId": id,
                }));
                Some(id)
            } else {
                None
            };

            let initial_thinking = cfg
                .thinking_level
                .clone()
                .or_else(|| st.model.thinking_level.clone());
            let effective_model_id = cfg
                .model
                .as_ref()
                .map(|m| m.model_id.clone())
                .unwrap_or_else(|| st.model.model_id.clone());
            let effective_max_tokens = cfg
                .max_output_tokens
                .or(st.model.max_output_tokens)
                .unwrap_or(16_384);
            let (done_tx, done_rx) = watch::channel(false);

            let run_ctrl = Arc::new(ActiveRunControl {
                run_id: cfg.run_id.clone(),
                hub: hub.clone(),
                cancel: CancellationToken::new(),
                done_tx,
                done_rx,
                finished: AtomicBool::new(false),
                finish_requested: AtomicBool::new(false),
                steer_drained_for_next_round: AtomicBool::new(false),
                current_status: Mutex::new(Some("running")),
                steer_queue: Mutex::new(VecDeque::new()),
                pending_gate: Mutex::new(None),
                pending_tools_changed: Mutex::new(Vec::new()),
                tool_batch: Mutex::new(None),
                answered_calls: Mutex::new(HashMap::new()),
                run_system_prompt: Mutex::new(cfg.system_prompt),
                run_model: cfg.model,
                run_thinking_level: Arc::new(Mutex::new(initial_thinking)),
                run_context_window: cfg.context_window,
                run_max_output_tokens: cfg.max_output_tokens,
                run_runtime_context: Mutex::new(cfg.runtime_context),
                run_prune_boundary_override: Mutex::new(cfg.prune_boundary_call_id),
                model_proxy: cfg.model_proxy,
                proxy_protocol,
                user_entry_id,
                pending_assistant_entry_id: Mutex::new(None),
                last_assistant_entry_id: Mutex::new(None),
                last_usage_tokens: Mutex::new(None),
                streaming_block_state: Mutex::new(StreamingBlockState::default()),
            });

            st.active_run = Some(run_ctrl.clone());
            st.touch();

            let rx_tuple = hub
                .subscribe(0)
                .map_err(|_| KernelError::new(500, "INTERNAL", "event hub expired", false))?;

            let task = Task {
                id: cfg.run_id.clone(),
                run_id: cfg.run_id.clone(),
                spec: TaskSpec {
                    name: "embedded-agent".into(),
                    role: "assistant".into(),
                    route_id: "embedded-proxy".into(),
                    prompt: "embedded".into(),
                    depends_on: Vec::new(),
                    write_scopes: Vec::new(),
                    tools: true,
                    allow_commands: false,
                    max_rounds: cfg.max_requests,
                },
                route: Route {
                    id: "embedded-proxy".into(),
                    name: "Embedded Proxy".into(),
                    base_url: run_ctrl.model_proxy.base_url.clone(),
                    model: effective_model_id,
                    max_tokens: effective_max_tokens,
                    parallel_limit: 1,
                    key_env: None,
                    protocol: Some(proxy_protocol),
                },
                workspace: st.cwd.clone(),
                status: "queued".into(),
                output: String::new(),
                error: None,
                messages: Vec::new(),
                usage: Value::Null,
                created_at: 0,
                updated_at: 0,
            };

            (run_ctrl, task, rx_tuple)
        };

        let handle_for_task = handle.clone();
        let run_for_task = run_ctrl.clone();
        tokio::spawn(async move {
            run_embedded_task(handle_for_task, run_for_task, task).await;
        });

        Ok((run_ctrl, rx_tuple.0, rx_tuple.1, rx_tuple.2))
    }

    pub fn subscribe_run_events(
        &self,
        session_id: &str,
        run_id: &str,
        after: u64,
    ) -> Result<
        (
            Vec<crate::kernel_embedded::event_hub::SequencedKernelEvent>,
            broadcast::Receiver<crate::kernel_embedded::event_hub::SequencedKernelEvent>,
            bool,
        ),
        KernelError,
    > {
        let handle = self.get_handle(session_id)?;
        let mut st = handle.state.lock().unwrap();
        st.touch();
        let hub = if let Some(active) = &st.active_run
            && active.run_id == run_id
        {
            active.hub.clone()
        } else if let Some(finished) = &st.last_finished_run
            && finished.run_id == run_id
        {
            finished.hub.clone()
        } else if st.expired_run_ids.contains(run_id) {
            return Err(KernelError::new(
                410,
                "EVENTS_EXPIRED",
                format!("events expired for run {run_id}"),
                false,
            ));
        } else {
            return Err(KernelError::run_not_found(run_id));
        };
        drop(st);

        hub.subscribe(after).map_err(|e| match e {
            EventReplayError::Expired => KernelError::new(
                410,
                "EVENTS_EXPIRED",
                format!("events prior to seq {after} have expired"),
                false,
            ),
        })
    }

    fn resolve_active_run(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<(Arc<EmbeddedSessionHandle>, Arc<ActiveRunControl>), KernelError> {
        let handle = self.get_handle(session_id)?;
        let mut st = handle.state.lock().unwrap();
        st.touch();
        if let Some(active) = &st.active_run
            && active.run_id == run_id
        {
            if active.finished.load(Ordering::Acquire) {
                return Err(KernelError::run_finished(run_id));
            }
            return Ok((handle.clone(), active.clone()));
        }
        if st
            .last_finished_run
            .as_ref()
            .is_some_and(|f| f.run_id == run_id)
            || st.expired_run_ids.contains(run_id)
        {
            return Err(KernelError::run_finished(run_id));
        }
        Err(KernelError::run_not_found(run_id))
    }

    pub async fn abort_run(&self, session_id: &str, run_id: &str) -> Result<(), KernelError> {
        let handle = self.get_handle(session_id)?;
        let active_to_cancel = {
            let mut st = handle.state.lock().unwrap();
            st.touch();
            if let Some(active) = &st.active_run
                && active.run_id == run_id
            {
                Some(active.clone())
            } else if st
                .last_finished_run
                .as_ref()
                .is_some_and(|f| f.run_id == run_id)
                || st.expired_run_ids.contains(run_id)
            {
                None
            } else {
                return Err(KernelError::run_not_found(run_id));
            }
        };
        if let Some(active) = active_to_cancel {
            active.cancel.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(3), active.wait_done()).await;
        }
        Ok(())
    }

    pub fn finish_run(&self, session_id: &str, run_id: &str) -> Result<(), KernelError> {
        let (_handle, active) = self.resolve_active_run(session_id, run_id)?;
        active.finish_requested.store(true, Ordering::Release);
        Ok(())
    }

    pub fn submit_steer(
        &self,
        session_id: &str,
        run_id: &str,
        content: Vec<KernelContent>,
    ) -> Result<(), KernelError> {
        validate_kernel_contents_for_media(&content, self.media_enabled)?;
        let handle = self.get_handle(session_id)?;
        let mut st = handle.state.lock().unwrap();
        st.touch();
        if let Some(active) = &st.active_run
            && active.run_id == run_id
        {
            if active.finished.load(Ordering::Acquire) {
                return Err(KernelError::run_finished(run_id));
            }
            active
                .steer_queue
                .lock()
                .unwrap()
                .push_back(SteerPayload::User { content });
            return Ok(());
        }
        if st
            .last_finished_run
            .as_ref()
            .is_some_and(|f| f.run_id == run_id)
            || st.expired_run_ids.contains(run_id)
        {
            return Err(KernelError::run_finished(run_id));
        }
        Err(KernelError::run_not_found(run_id))
    }

    pub fn submit_tool_result(
        &self,
        session_id: &str,
        run_id: &str,
        call_id: String,
        result: KernelToolResult,
        model_note: Option<String>,
    ) -> Result<ToolResultSubmission, KernelError> {
        validate_kernel_id("callId", &call_id)?;
        validate_kernel_contents_for_media(&result.content, self.media_enabled)?;
        let (_handle, active) = self.resolve_active_run(session_id, run_id)?;
        let mut answered = active.answered_calls.lock().unwrap();
        if let Some(prev) = answered.get(&call_id) {
            if prev.result == result && prev.model_note == model_note {
                return Ok(ToolResultSubmission::Replayed);
            }
            return Err(KernelError::new(
                409,
                "TOOL_RESULT_CONFLICT",
                format!("conflicting tool result for callId {call_id}"),
                false,
            ));
        }

        let mut batch_guard = active.tool_batch.lock().unwrap();
        let Some(batch) = batch_guard.as_mut() else {
            return Err(KernelError::new(
                404,
                "TOOL_CALL_NOT_FOUND",
                format!("unknown tool callId: {call_id}"),
                false,
            ));
        };
        let Some(&idx) = batch.call_to_index.get(&call_id) else {
            return Err(KernelError::new(
                404,
                "TOOL_CALL_NOT_FOUND",
                format!("unknown tool callId: {call_id}"),
                false,
            ));
        };

        let name = match &batch.slots[idx] {
            BatchSlot::PendingHost { name, .. } => name.clone(),
            BatchSlot::Submitted {
                result: prev_res,
                model_note: prev_note,
                ..
            } => {
                if *prev_res == result && *prev_note == model_note {
                    return Ok(ToolResultSubmission::Replayed);
                }
                return Err(KernelError::new(
                    409,
                    "TOOL_RESULT_CONFLICT",
                    format!("conflicting tool result for callId {call_id}"),
                    false,
                ));
            }
            BatchSlot::Synthesized { .. } => {
                return Err(KernelError::new(
                    409,
                    "TOOL_RESULT_CONFLICT",
                    format!("tool call {call_id} already synthesized locally"),
                    false,
                ));
            }
        };

        let submission = SubmittedToolResult {
            result: result.clone(),
            model_note: model_note.clone(),
        };
        answered.insert(call_id.clone(), submission);
        batch.slots[idx] = BatchSlot::Submitted {
            call_id,
            name,
            result,
            model_note,
        };
        if batch.all_resolved() {
            batch.batch_done.notify_waiters();
        }
        Ok(ToolResultSubmission::Accepted)
    }

    pub fn resolve_gate(
        &self,
        session_id: &str,
        run_id: &str,
        gate_id: &str,
        decision: GateDecision,
    ) -> Result<(), KernelError> {
        validate_kernel_id("gateId", gate_id)?;
        let (_handle, active) = self.resolve_active_run(session_id, run_id)?;
        let mut guard = active.pending_gate.lock().unwrap();
        let Some(pending) = guard.as_ref() else {
            return Err(KernelError::new(
                404,
                "GATE_NOT_FOUND",
                format!("gate not found: {gate_id}"),
                false,
            ));
        };
        if pending.gate_id != gate_id {
            return Err(KernelError::new(
                404,
                "GATE_NOT_FOUND",
                format!("gate not found: {gate_id}"),
                false,
            ));
        }

        // Validate action per gate kind (`protocol.md §7.3`).
        match (pending.kind, decision.action) {
            (
                GateKind::BeforeModelTurn | GateKind::AfterTurn,
                GateAction::Continue | GateAction::Finish,
            ) => {
                if decision.summary.is_some() {
                    return Err(KernelError::invalid_request(
                        "summary is only valid for action 'replace'",
                    ));
                }
            }
            (GateKind::BeforeCompact, GateAction::Continue | GateAction::Cancel) => {
                if decision.summary.is_some() {
                    return Err(KernelError::invalid_request(
                        "summary is only valid for action 'replace'",
                    ));
                }
            }
            (GateKind::BeforeCompact, GateAction::Replace) => {
                if decision.summary.is_none() {
                    return Err(KernelError::invalid_request(
                        "action 'replace' requires a summary",
                    ));
                }
            }
            _ => {
                return Err(KernelError::invalid_request(format!(
                    "action not allowed for gate kind {}",
                    pending.kind.as_str()
                )));
            }
        }

        let pending = guard.take().unwrap();
        let _ = pending.tx.send(decision);
        Ok(())
    }

    pub fn reap_idle_sessions(&self) -> usize {
        let mut to_remove = Vec::new();
        {
            let map = self.sessions.read().unwrap();
            for (id, handle) in map.iter() {
                if let Ok(st) = handle.state.try_lock()
                    && st.active_run.is_none()
                    && st.last_activity.elapsed() >= Duration::from_secs(IDLE_SESSION_TTL_SECS)
                {
                    to_remove.push(id.clone());
                }
            }
        }
        if to_remove.is_empty() {
            return 0;
        }
        let mut map = self.sessions.write().unwrap();
        let mut evicted = 0usize;
        for id in to_remove {
            let should_remove = map.get(&id).is_some_and(|handle| {
                let Ok(st) = handle.state.try_lock() else {
                    return false;
                };
                st.active_run.is_none()
                    && st.last_activity.elapsed() >= Duration::from_secs(IDLE_SESSION_TTL_SECS)
            });
            if should_remove {
                map.remove(&id);
                evicted += 1;
            }
        }
        evicted
    }

    pub async fn abort_all_runs(&self) {
        let active_runs: Vec<Arc<ActiveRunControl>> = {
            let map = self.sessions.read().unwrap();
            map.values()
                .filter_map(|h| h.state.lock().unwrap().active_run.clone())
                .collect()
        };
        for run in &active_runs {
            run.cancel.cancel();
        }
        if active_runs.is_empty() {
            return;
        }
        let wait_all =
            futures_util::future::join_all(active_runs.iter().map(|run| run.wait_done()));
        let _ = tokio::time::timeout(Duration::from_secs(3), wait_all).await;
    }
}

struct NoopStrategyDecider;
impl StrategyDecider for NoopStrategyDecider {
    fn decide(
        &self,
        input: StrategyInput<'_>,
        _messages: &[NeutralMessage],
        _round: usize,
        _max_rounds: usize,
    ) -> std::result::Result<StrategyAction, AgentError> {
        Ok(match input {
            StrategyInput::BeforeCompletion { tools } => {
                StrategyAction::UseTools((0..tools.len()).collect())
            }
            StrategyInput::AfterCompletion {
                outcome: crate::agent_runtime::CompletionKind::NoToolCalls,
            } => StrategyAction::Finish,
            StrategyInput::AfterCompletion {
                outcome: crate::agent_runtime::CompletionKind::ToolGroup(tools),
            } => StrategyAction::ToolVotes(vec![ToolVote::ApplyHostPolicy; tools.len()]),
        })
    }
}

struct EmbeddedTaskRepository;
impl TaskRepository for EmbeddedTaskRepository {
    fn check_dependencies(&self, _task: &Task) -> Result<DependencyState> {
        Ok(DependencyState::Ready(Vec::new()))
    }
    fn save_running_snapshot(&self, _task: &Task) -> Result<()> {
        Ok(())
    }
    fn save_assistant_message(&self, _task: &Task, _has_tool_calls: bool) -> Result<()> {
        Ok(())
    }
    fn consume_resume_inputs(&self, task: &Task) -> Result<Task> {
        Ok(task.clone())
    }
}

struct EmbeddedToolExecutor {
    handle: Arc<EmbeddedSessionHandle>,
    run: Arc<ActiveRunControl>,
}

impl ToolExecutor for EmbeddedToolExecutor {
    async fn reconcile_open_tools(
        &self,
        _task: &mut Task,
        _keys: &CredentialSet,
        _slot: &mut Box<dyn SlotGuard>,
        _cancel: &CancellationToken,
    ) -> Result<()> {
        Ok(())
    }

    async fn prepare_round_tools(
        &self,
        _task: &Task,
        _events: &dyn EventSink,
        _announced_logins: &mut HashSet<String>,
        _cancel: &CancellationToken,
    ) -> Result<PreparedTools> {
        let st = self.handle.state.lock().unwrap();
        let active_set: HashSet<&str> = st.active_tool_names.iter().map(String::as_str).collect();
        let definitions: Vec<Value> = st
            .tools
            .iter()
            .filter(|t| active_set.contains(t.name.as_str()))
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": Value::Object(t.parameters.clone()),
                    }
                })
            })
            .collect();
        Ok(PreparedTools {
            definitions,
            backend_state: Box::new(()),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_batch(
        &self,
        task: &mut Task,
        _calls: &[ToolCallBlock],
        _votes: &[ToolVote],
        _advertised_tools: &[Value],
        _prepared: &PreparedTools,
        _keys: &CredentialSet,
        _slot: &mut Box<dyn SlotGuard>,
        cancel: &CancellationToken,
    ) -> Result<BatchOutcome> {
        let wait_info = {
            let guard = self.run.tool_batch.lock().unwrap();
            guard
                .as_ref()
                .map(|b| (!b.all_resolved(), b.batch_done.clone()))
        };
        if let Some((needs_wait, notify)) = wait_info
            && needs_wait
        {
            self.run.emit_status("waitingTools");
            loop {
                let notified = notify.notified();
                tokio::pin!(notified);
                let done = {
                    self.run
                        .tool_batch
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_none_or(PendingToolBatch::all_resolved)
                };
                if done {
                    break;
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(BatchOutcome::Continue),
                    _ = &mut notified => {}
                }
            }
        }

        if cancel.is_cancelled() {
            return Ok(BatchOutcome::Continue);
        }

        let batch = self.run.tool_batch.lock().unwrap().take();
        let Some(batch) = batch else {
            return Ok(BatchOutcome::Continue);
        };

        let mut any_terminate = false;
        let max_chars = self.handle.state.lock().unwrap().tool_result_max_chars;

        for slot in batch.slots {
            let _provenance = crate::tool_results::Origin::HostTool;
            let (call_id, name, raw_content, is_error, terminate, model_note) = match slot {
                BatchSlot::Submitted {
                    call_id,
                    name,
                    result,
                    model_note,
                } => (
                    call_id,
                    name,
                    result.content,
                    result.is_error.unwrap_or(false),
                    result.terminate.unwrap_or(false),
                    model_note,
                ),
                BatchSlot::Synthesized {
                    call_id,
                    name,
                    content,
                    is_error,
                } => (call_id, name, content, is_error, false, None),
                BatchSlot::PendingHost { call_id, name } => (
                    call_id,
                    name,
                    vec![KernelContent::Text {
                        text: "aborted".into(),
                    }],
                    true,
                    false,
                    None,
                ),
            };
            if terminate {
                any_terminate = true;
            }
            let truncated = truncate_kernel_content(raw_content, max_chars);
            let mut result_json = json!({
                "content": truncated,
                "details": Value::Null,
                "isError": is_error,
            });
            if terminate {
                result_json["terminate"] = json!(true);
            }
            self.run.hub.emit(json!({
                "type": "toolResult",
                "callId": call_id,
                "result": result_json,
            }));

            let entry = {
                let mut st = self.handle.state.lock().unwrap();
                if let Some(note) = model_note.filter(|s| !s.is_empty()) {
                    st.context_projection
                        .tool_result_notes
                        .get_or_insert_with(HashMap::new)
                        .insert(call_id.clone(), note);
                }
                let entry = SessionEntry::ToolResult {
                    id: new_entry_id(),
                    parent_id: st.leaf_id.clone(),
                    timestamp: utc_iso8601_now(),
                    metadata: None,
                    call_id: call_id.clone(),
                    name: name.clone(),
                    content: truncated.clone(),
                    is_error,
                };
                st.append_entry(entry.clone());
                (entry, st.revision)
            };
            self.run.hub.emit(json!({
                "type": "entry",
                "entry": entry.0,
                "revision": entry.1,
            }));

            task.messages.push(NeutralMessage::tool_result_blocks(
                call_id,
                Some(name),
                kernel_contents_to_neutral_blocks(&truncated),
                is_error,
            ));
        }

        Ok(if any_terminate {
            BatchOutcome::TerminateRun
        } else {
            BatchOutcome::Continue
        })
    }
}

struct ProjectedContextSnapshot {
    messages: Vec<NeutralMessage>,
    model_id: String,
    max_tokens: u32,
    context_window: Option<u64>,
}

struct EmbeddedTurnController {
    handle: Arc<EmbeddedSessionHandle>,
    run: Arc<ActiveRunControl>,
}

impl EmbeddedTurnController {
    async fn open_gate(
        &self,
        kind: GateKind,
        turn: usize,
        last_entry_id: Option<String>,
        preparation: Option<Value>,
        cancel: &CancellationToken,
    ) -> Result<GateDecision> {
        let enabled = self.handle.state.lock().unwrap().gates.contains(&kind);
        if !enabled {
            return Ok(GateDecision {
                action: GateAction::Continue,
                summary: None,
            });
        }
        let gate_id = format!("gate_{}", new_entry_id());
        let (tx, rx) = oneshot::channel();
        {
            *self.run.pending_gate.lock().unwrap() = Some(PendingGate {
                gate_id: gate_id.clone(),
                kind,
                tx,
            });
        }
        let mut ev = json!({
            "type": "gate",
            "gateId": gate_id,
            "kind": kind.as_str(),
            "turn": turn,
        });
        if let Some(eid) = last_entry_id {
            ev["lastEntryId"] = json!(eid);
        }
        if let Some(prep) = preparation {
            ev["preparation"] = prep;
        }
        self.run.hub.emit(ev);
        self.run.emit_status("waitingGate");

        let decision = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                *self.run.pending_gate.lock().unwrap() = None;
                bail!("run aborted");
            }
            res = rx => {
                *self.run.pending_gate.lock().unwrap() = None;
                res.map_err(|_| anyhow::anyhow!("gate channel closed"))?
            }
        };
        if kind != GateKind::BeforeCompact && decision.action != GateAction::Finish {
            self.run.emit_status("running");
        }
        Ok(decision)
    }

    fn drain_steers_at_safe_point(&self, task: &mut Task) -> usize {
        let mode = self.handle.state.lock().unwrap().steering_mode;
        let drained: Vec<SteerPayload> = {
            let mut q = self.run.steer_queue.lock().unwrap();
            match mode {
                SteeringMode::OneAtATime => q.pop_front().into_iter().collect(),
                SteeringMode::All => q.drain(..).collect(),
            }
        };
        let count = drained.len();
        for item in drained {
            let entry = {
                let mut st = self.handle.state.lock().unwrap();
                let id = new_entry_id();
                let parent_id = st.leaf_id.clone();
                let ts = utc_iso8601_now();
                let entry = match item {
                    SteerPayload::User { content } => {
                        let blocks = content
                            .iter()
                            .map(KernelContent::to_session_block)
                            .collect();
                        SessionEntry::Message {
                            id,
                            parent_id,
                            timestamp: ts,
                            metadata: None,
                            role: SessionMessageRole::User,
                            content: blocks,
                        }
                    }
                    SteerPayload::CustomMessage {
                        name,
                        content,
                        display,
                        details,
                        include_in_summary,
                    } => SessionEntry::CustomMessage {
                        id,
                        parent_id,
                        timestamp: ts,
                        metadata: None,
                        name,
                        content,
                        display,
                        details,
                        include_in_summary,
                    },
                };
                st.append_entry(entry.clone());
                if let Ok(Some(nm)) =
                    entry.to_neutral_message_for_protocol(Some(self.run.proxy_protocol))
                {
                    task.messages.push(nm);
                }
                (entry, st.revision)
            };
            let entry_id = entry.0.id().to_owned();
            self.run.hub.emit(json!({
                "type": "entry",
                "entry": entry.0,
                "revision": entry.1,
            }));
            self.run.hub.emit(json!({
                "type": "userMessage",
                "entryId": entry_id,
                "steered": true,
            }));
        }
        count
    }

    fn build_projected_messages(&self) -> Result<ProjectedContextSnapshot> {
        let run_system_prompt = self.run.run_system_prompt.lock().unwrap().clone();
        let run_prune_override = self.run.run_prune_boundary_override.lock().unwrap().clone();
        let run_rt_ctx = self.run.run_runtime_context.lock().unwrap().clone();
        let st = self.handle.state.lock().unwrap();
        let branch = st.get_branch();
        let sys_prompt = run_system_prompt.as_deref().unwrap_or(&st.system_prompt);
        let prune_boundary = match run_prune_override.as_ref() {
            Some(override_val) => override_val.as_deref(),
            None => st.context_projection.prune_boundary_call_id.as_deref(),
        };
        let prune_min = st
            .context_projection
            .prune_min_length
            .unwrap_or(DEFAULT_PRUNE_MIN_LENGTH);
        let prune_keep = st
            .context_projection
            .prune_keep_length
            .unwrap_or(DEFAULT_PRUNE_KEEP_LENGTH);
        let notes = st
            .context_projection
            .tool_result_notes
            .clone()
            .unwrap_or_default();

        let messages = build_model_view_messages(
            &branch,
            &ProjectionParams {
                system_prompt: sys_prompt,
                active_protocol: self.run.proxy_protocol,
                patched_runtime_context: st.patched_runtime_context.as_deref(),
                run_runtime_context: run_rt_ctx.as_deref(),
                prune_boundary_call_id: prune_boundary,
                prune_min_length: prune_min,
                prune_keep_length: prune_keep,
                tool_result_notes: Some(&notes),
            },
        )?;
        let model_id = self
            .run
            .run_model
            .as_ref()
            .map(|m| m.model_id.clone())
            .unwrap_or_else(|| st.model.model_id.clone());
        let max_tokens = self
            .run
            .run_max_output_tokens
            .or(st.model.max_output_tokens)
            .unwrap_or(16_384);
        let context_window = self.run.run_context_window.or(st.model.context_window);
        Ok(ProjectedContextSnapshot {
            messages,
            model_id,
            max_tokens,
            context_window,
        })
    }

    async fn run_auto_compaction(
        &self,
        round: usize,
        force_overflow: bool,
        available_tools: &[Value],
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let (branch, enabled, context_window, max_out, keep_recent, model_id, thinking_level) = {
            let st = self.handle.state.lock().unwrap();
            let model_id = self
                .run
                .run_model
                .as_ref()
                .map(|m| m.model_id.clone())
                .unwrap_or_else(|| st.model.model_id.clone());
            let max_out = self
                .run
                .run_max_output_tokens
                .or(st.model.max_output_tokens)
                .unwrap_or(4096);
            let keep = st.compaction.keep_recent_tokens.unwrap_or(20_000);
            let tl = self.run.run_thinking_level.lock().unwrap().clone();
            (
                st.get_branch(),
                st.compaction.enabled,
                self.run.run_context_window.or(st.model.context_window),
                max_out,
                keep,
                model_id,
                tl,
            )
        };
        if !enabled && !force_overflow {
            return Ok(false);
        }

        if !force_overflow {
            let Some(cw) = context_window else {
                return Ok(false);
            };
            let snap = self.build_projected_messages()?;
            let est = estimate_context_tokens(&snap.messages, available_tools);
            let margin = (cw / 10).min(16_384);
            let threshold = cw.saturating_sub(u64::from(max_out)).saturating_sub(margin);
            if est <= threshold {
                return Ok(false);
            }
        }

        let new_id = make_compaction_entry_id();
        let last_usage = *self.run.last_usage_tokens.lock().unwrap();
        let prep = prepare_compaction(
            &branch,
            self.run.proxy_protocol,
            keep_recent,
            &new_id,
            last_usage,
        );
        if prep.entry_ids_to_summarize.is_empty() && prep.prior_summary.is_none() {
            return Ok(false);
        }

        self.run.emit_status("compacting");
        self.run.hub.emit(json!({
            "type": "compaction",
            "active": true,
            "reason": "auto",
        }));

        let gate_prep = json!({
            "firstKeptEntryId": prep.first_kept_entry_id,
            "tokensBefore": prep.tokens_before,
            "entryIdsToSummarize": prep.entry_ids_to_summarize,
        });
        let decision = self
            .open_gate(
                GateKind::BeforeCompact,
                round + 1,
                None,
                Some(gate_prep),
                cancel,
            )
            .await?;

        let (summary, usage_bd) = match decision.action {
            GateAction::Cancel => {
                self.run
                    .hub
                    .emit(json!({ "type": "compaction", "active": false }));
                self.run.emit_status("running");
                return Ok(false);
            }
            GateAction::Replace => {
                let s = decision.summary.unwrap_or_default();
                let trimmed = s.trim().to_owned();
                if trimmed.is_empty() {
                    self.run.hub.emit(json!({
                        "type": "compaction",
                        "active": false,
                        "error": "compaction summary is empty",
                    }));
                    self.run.emit_status("running");
                    return Ok(false);
                }
                (trimmed, None)
            }
            GateAction::Continue | GateAction::Finish => {
                match summarize_compaction(
                    self.run.model_proxy.clone(),
                    self.run.proxy_protocol,
                    &model_id,
                    max_out,
                    thinking_level,
                    &prep,
                    None,
                    None,
                    cancel,
                )
                .await
                {
                    Ok(res) => res,
                    Err(err) => {
                        self.run.hub.emit(json!({
                            "type": "compaction",
                            "active": false,
                            "error": err.to_string(),
                        }));
                        self.run.emit_status("running");
                        return Ok(false);
                    }
                }
            }
        };

        let (entry, rev) = {
            let mut st = self.handle.state.lock().unwrap();
            let entry = SessionEntry::Compaction {
                id: new_id.clone(),
                parent_id: st.leaf_id.clone(),
                timestamp: utc_iso8601_now(),
                metadata: None,
                summary: summary.clone(),
                first_kept_entry_id: prep.first_kept_entry_id.clone(),
                tokens_before: prep.tokens_before,
            };
            st.append_entry(entry.clone());
            (entry, st.revision)
        };
        self.run.hub.emit(json!({
            "type": "entry",
            "entry": entry,
            "revision": rev,
        }));
        let mut comp_ev = json!({
            "type": "compaction",
            "active": false,
            "reason": "auto",
            "entryId": new_id,
            "summary": summary,
            "tokensBefore": prep.tokens_before,
            "firstKeptEntryId": prep.first_kept_entry_id,
        });
        if let Some(ref bd) = usage_bd {
            comp_ev["usage"] = json!(KernelUsage::from(bd));
        }
        self.run.hub.emit(comp_ev);
        self.run.emit_status("running");
        *self.run.last_usage_tokens.lock().unwrap() = None;
        Ok(true)
    }

    fn emit_done_with_state(&self, revision: u64, leaf_id: Option<String>) {
        let assistant_entry_id = self.run.last_assistant_entry_id.lock().unwrap().clone();
        let mut done = json!({
            "type": "done",
            "reason": "completed",
            "stopReason": "stop",
            "revision": revision,
        });
        if let Some(leaf) = leaf_id {
            done["leafId"] = json!(leaf);
        }
        if let Some(uid) = &self.run.user_entry_id {
            done["userEntryId"] = json!(uid);
        }
        if let Some(aid) = assistant_entry_id {
            done["assistantEntryId"] = json!(aid);
        }
        self.run.hub.emit(done);
        self.run.mark_done_signal();
    }

    fn emit_done_completed(&self) {
        let (revision, leaf_id) = {
            let mut st = self.handle.state.lock().unwrap();
            if self.run.finished.swap(true, Ordering::SeqCst) {
                return;
            }
            let rev = st.revision;
            let leaf = st.leaf_id.clone();
            st.finish_active_run(&self.run.run_id);
            (rev, leaf)
        };
        self.emit_done_with_state(revision, leaf_id);
    }
}

impl RunTurnController for EmbeddedTurnController {
    fn is_embedded(&self) -> bool {
        true
    }

    async fn before_model_turn(
        &self,
        task: &mut Task,
        round: usize,
        cancel: &CancellationToken,
    ) -> Result<RoundDirective> {
        if self.run.finish_requested.load(Ordering::Acquire) {
            self.emit_done_completed();
            return Ok(RoundDirective::Finish);
        }

        let decision = self
            .open_gate(GateKind::BeforeModelTurn, round + 1, None, None, cancel)
            .await?;
        if decision.action == GateAction::Finish
            || self.run.finish_requested.load(Ordering::Acquire)
        {
            self.emit_done_completed();
            return Ok(RoundDirective::Finish);
        }

        // E2E `T3-K03`: flush `toolsChanged` AFTER `beforeModelTurn` gate resolves (if enabled)
        // and BEFORE `status("running")` when resuming after tools without `beforeModelTurn` (`runToolBatch.json`).
        self.run.flush_tools_changed();
        self.run.emit_status("running");

        // Also drain any steers queued while paused in `beforeModelTurn`, unless
        // `after_tool_batch` already injected one for this round under `one-at-a-time`.
        let already_drained = self
            .run
            .steer_drained_for_next_round
            .swap(false, Ordering::AcqRel);
        let mode = self.handle.state.lock().unwrap().steering_mode;
        if !already_drained || mode == SteeringMode::All {
            self.drain_steers_at_safe_point(task);
        }
        Ok(RoundDirective::Continue)
    }

    async fn prepare_model_context(
        &self,
        _task: &mut Task,
        available_tools: &[Value],
        round: usize,
        cancel: &CancellationToken,
    ) -> Result<Option<PreparedModelContext>> {
        self.run_auto_compaction(round, false, available_tools, cancel)
            .await?;
        self.run.reset_streaming_blocks_for_retry();

        let snap = self.build_projected_messages()?;
        if let Some(cw) = snap.context_window {
            let tokens = estimate_context_tokens(&snap.messages, available_tools);
            self.run.hub.emit(json!({
                "type": "contextUsage",
                "tokens": tokens,
                "contextWindow": cw,
            }));
        }

        let entry_id = new_entry_id();
        *self.run.last_assistant_entry_id.lock().unwrap() = None;
        *self.run.pending_assistant_entry_id.lock().unwrap() = Some(entry_id.clone());
        let model_json = json!({ "modelId": snap.model_id });
        self.run.hub.emit(json!({
            "type": "start",
            "model": model_json,
            "entryId": entry_id,
        }));

        Ok(Some(PreparedModelContext {
            messages: snap.messages,
            model_id: snap.model_id,
            max_tokens: snap.max_tokens,
        }))
    }

    fn emit_completion_delta(&self, _task_id: &str, delta: &str) -> Result<()> {
        self.run.emit_text_delta(delta);
        Ok(())
    }

    fn emit_reasoning_delta(&self, _task_id: &str, delta: &str) -> Result<()> {
        self.run.emit_thinking_delta(delta);
        Ok(())
    }

    fn finish_streaming_blocks(&self) -> Result<()> {
        self.run.close_streaming_blocks();
        Ok(())
    }

    async fn try_overflow_compaction(
        &self,
        _task: &mut Task,
        err: &anyhow::Error,
        available_tools: &[Value],
        round: usize,
        cancel: &CancellationToken,
    ) -> Result<OverflowAction> {
        let is_overflow = err
            .downcast_ref::<EmbeddedModelError>()
            .is_some_and(|e| e.code == "CONTEXT_OVERFLOW");
        if !is_overflow {
            return Ok(OverflowAction::NoRetry);
        }
        let compacted = self
            .run_auto_compaction(round, true, available_tools, cancel)
            .await?;
        if !compacted {
            return Ok(OverflowAction::NoRetry);
        }
        self.run.reset_streaming_blocks_for_retry();
        let snap = self.build_projected_messages()?;
        if let Some(cw) = snap.context_window {
            let tokens = estimate_context_tokens(&snap.messages, available_tools);
            self.run.hub.emit(json!({
                "type": "contextUsage",
                "tokens": tokens,
                "contextWindow": cw,
            }));
        }
        Ok(OverflowAction::RetryWithContext(PreparedModelContext {
            messages: snap.messages,
            model_id: snap.model_id,
            max_tokens: snap.max_tokens,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit_assistant_turn(
        &self,
        task: &mut Task,
        message: &mut NeutralMessage,
        usage: &Value,
        usage_breakdown: Option<&KernelUsageBreakdown>,
        response_id: Option<&str>,
        _available_tools: &[Value],
        _cancel: &CancellationToken,
    ) -> Result<()> {
        // Collect existing call IDs from branch history so model-omitted or duplicate call IDs
        // normalize to `call_<uuid>` per `protocol.md §3`.
        let mut seen_call_ids: HashSet<String> = {
            let st = self.handle.state.lock().unwrap();
            let mut ids = HashSet::new();
            for entry in st.get_branch() {
                if let SessionEntry::Message { content, .. } = entry {
                    for b in content {
                        if let SessionBlock::ToolCall { call_id, .. } = b {
                            ids.insert(call_id);
                        }
                    }
                }
            }
            ids
        };

        for tc in message.tool_calls_mut() {
            if tc.id.is_empty() || !seen_call_ids.insert(tc.id.clone()) {
                let fresh = format!("call_{}", uuid::Uuid::now_v7());
                seen_call_ids.insert(fresh.clone());
                tc.id = fresh;
            }
        }

        // Emit any RedactedThinking blocks not streamed as plain thinking deltas.
        for block in &message.content {
            if let ContentBlock::RedactedThinking { data } = block {
                let block_id = {
                    let mut st = self.run.streaming_block_state.lock().unwrap();
                    ActiveRunControl::next_block_id(&mut st)
                };
                self.run.hub.emit(json!({
                    "type": "thinking",
                    "blockId": block_id,
                    "delta": data,
                    "done": true,
                    "redacted": true,
                }));
            }
        }

        let calls: Vec<ToolCallBlock> = message.tool_calls().cloned().collect();
        let batch_size = calls.len();
        let batch_id = format!("batch_{}", uuid::Uuid::now_v7());
        let active_set: HashSet<String> = {
            self.handle
                .state
                .lock()
                .unwrap()
                .active_tool_names
                .iter()
                .cloned()
                .collect()
        };

        let mut slots = Vec::with_capacity(batch_size);
        let mut call_to_index = HashMap::with_capacity(batch_size);

        for (batch_index, call) in calls.iter().enumerate() {
            if !active_set.contains(&call.name) {
                slots.push(BatchSlot::Synthesized {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    content: vec![KernelContent::Text {
                        text: format!("工具未启用：{}", call.name),
                    }],
                    is_error: true,
                });
                continue;
            }
            match serde_json::from_str::<Value>(&call.arguments) {
                Ok(Value::Object(args_map)) => {
                    self.run.hub.emit(json!({
                        "type": "toolCall",
                        "callId": call.id,
                        "name": call.name,
                        "args": Value::Object(args_map),
                        "batchId": batch_id,
                        "batchIndex": batch_index,
                        "batchSize": batch_size,
                    }));
                    call_to_index.insert(call.id.clone(), batch_index);
                    slots.push(BatchSlot::PendingHost {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                    });
                }
                _ => {
                    slots.push(BatchSlot::Synthesized {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: vec![KernelContent::Text {
                            text: format!("工具 {} 参数不是合法 JSON 对象", call.name),
                        }],
                        is_error: true,
                    });
                }
            }
        }

        let k_usage = kernel_usage_from_value(usage, usage_breakdown);
        if let Some(total) = k_usage.total_tokens {
            *self.run.last_usage_tokens.lock().unwrap() = Some(total);
        }
        self.run.hub.emit(json!({
            "type": "usage",
            "usage": k_usage,
        }));

        let payload = message.to_session_entry_payload_lossy()?;
        let SessionEntryPayload::Message { role, content } = payload else {
            bail!("expected assistant message payload");
        };
        let entry_id = self
            .run
            .pending_assistant_entry_id
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(new_entry_id);
        let mut assistant_meta = Map::new();
        assistant_meta.insert(
            "stopReason".into(),
            json!(if calls.is_empty() { "stop" } else { "toolUse" }),
        );
        if let Some(rid) = response_id.filter(|s| !s.is_empty()) {
            assistant_meta.insert("responseId".into(), json!(rid));
        }
        let usage_map = match serde_json::to_value(&k_usage)? {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let (entry, rev, usage_entry, usage_rev) = {
            let mut st = self.handle.state.lock().unwrap();
            let entry = SessionEntry::Message {
                id: entry_id.clone(),
                parent_id: st.leaf_id.clone(),
                timestamp: utc_iso8601_now(),
                metadata: Some(assistant_meta),
                role,
                content,
            };
            st.append_entry(entry.clone());
            let rev = st.revision;

            let model_id = self
                .run
                .run_model
                .as_ref()
                .map(|m| m.model_id.clone())
                .unwrap_or_else(|| st.model.model_id.clone());
            let usage_entry = SessionEntry::Usage {
                id: new_entry_id(),
                parent_id: Some(entry_id.clone()),
                timestamp: utc_iso8601_now(),
                metadata: None,
                category: "chat".into(),
                model: KernelModel {
                    model_id,
                    custom_provider_id: None,
                },
                usage: usage_map,
            };
            st.append_entry(usage_entry.clone());
            let usage_rev = st.revision;
            (entry, rev, usage_entry, usage_rev)
        };
        *self.run.last_assistant_entry_id.lock().unwrap() = Some(entry_id);
        self.run.hub.emit(json!({
            "type": "entry",
            "entry": entry,
            "revision": rev,
        }));
        self.run.hub.emit(json!({
            "type": "entry",
            "entry": usage_entry,
            "revision": usage_rev,
        }));

        // Register pending batch ONLY AFTER `entry(assistant)` is emitted (`protocol.md §6.3` Rule 2).
        if !slots.is_empty() {
            *self.run.tool_batch.lock().unwrap() = Some(PendingToolBatch {
                slots,
                call_to_index,
                batch_done: Arc::new(Notify::new()),
            });
        }
        task.messages.push(message.clone());
        Ok(())
    }

    async fn after_tool_batch(
        &self,
        task: &mut Task,
        has_tool_calls: bool,
        batch_terminated: bool,
        round: usize,
        cancel: &CancellationToken,
    ) -> Result<TurnCommitOutcome> {
        if batch_terminated || self.run.finish_requested.load(Ordering::Acquire) {
            self.emit_done_completed();
            return Ok(TurnCommitOutcome::Finish);
        }

        let last_entry_id = self.handle.state.lock().unwrap().leaf_id.clone();
        let decision = self
            .open_gate(GateKind::AfterTurn, round + 1, last_entry_id, None, cancel)
            .await?;
        if decision.action == GateAction::Finish
            || self.run.finish_requested.load(Ordering::Acquire)
        {
            self.emit_done_completed();
            return Ok(TurnCommitOutcome::Finish);
        }

        let finish_info = {
            let mut st = self.handle.state.lock().unwrap();
            let has_steers = !self.run.steer_queue.lock().unwrap().is_empty();
            if has_tool_calls || has_steers {
                None
            } else if self.run.finished.swap(true, Ordering::SeqCst) {
                return Ok(TurnCommitOutcome::Finish);
            } else {
                let rev = st.revision;
                let leaf = st.leaf_id.clone();
                st.finish_active_run(&self.run.run_id);
                Some((rev, leaf))
            }
        };
        if let Some((revision, leaf_id)) = finish_info {
            self.emit_done_with_state(revision, leaf_id);
            Ok(TurnCommitOutcome::Finish)
        } else {
            let drained = self.drain_steers_at_safe_point(task);
            self.run
                .steer_drained_for_next_round
                .store(drained > 0, Ordering::Release);
            Ok(TurnCommitOutcome::Continue)
        }
    }

    async fn on_aborted(
        &self,
        _task: &mut Task,
        partial_text: &str,
        partial_thinking: &str,
    ) -> Result<()> {
        {
            let _st = self.handle.state.lock().unwrap();
            if self.run.finished.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
        }
        self.run.close_streaming_blocks();

        let pending_aid = self.run.pending_assistant_entry_id.lock().unwrap().take();

        // Commit partial assistant text/thinking if aborted mid-stream.
        if !partial_text.is_empty() || !partial_thinking.is_empty() {
            let mut blocks = Vec::new();
            if !partial_thinking.is_empty() {
                blocks.push(SessionBlock::Thinking {
                    text: partial_thinking.to_owned(),
                    signature: None,
                    redacted: None,
                });
            }
            if !partial_text.is_empty() {
                blocks.push(SessionBlock::Text {
                    text: partial_text.to_owned(),
                    signature: None,
                });
            }
            let mut meta = Map::new();
            meta.insert("stopReason".into(), json!("aborted"));
            let entry_id = pending_aid.unwrap_or_else(new_entry_id);
            let (entry, rev) = {
                let mut st = self.handle.state.lock().unwrap();
                let entry = SessionEntry::Message {
                    id: entry_id.clone(),
                    parent_id: st.leaf_id.clone(),
                    timestamp: utc_iso8601_now(),
                    metadata: Some(meta),
                    role: SessionMessageRole::Assistant,
                    content: blocks,
                };
                st.append_entry(entry.clone());
                (entry, st.revision)
            };
            *self.run.last_assistant_entry_id.lock().unwrap() = Some(entry_id);
            self.run.hub.emit(json!({
                "type": "entry",
                "entry": entry,
                "revision": rev,
            }));
        }

        // Complete any open tool batch with accepted results + synthetic `"aborted"` toolResults.
        if let Some(batch) = self.run.tool_batch.lock().unwrap().take() {
            let max_chars = self.handle.state.lock().unwrap().tool_result_max_chars;
            for slot in batch.slots {
                let (call_id, name, raw_content, is_error) = match slot {
                    BatchSlot::Submitted {
                        call_id,
                        name,
                        result,
                        ..
                    } => (
                        call_id,
                        name,
                        result.content,
                        result.is_error.unwrap_or(false),
                    ),
                    BatchSlot::Synthesized {
                        call_id,
                        name,
                        content,
                        is_error,
                    } => (call_id, name, content, is_error),
                    BatchSlot::PendingHost { call_id, name } => (
                        call_id,
                        name,
                        vec![KernelContent::Text {
                            text: "aborted".into(),
                        }],
                        true,
                    ),
                };
                let truncated = truncate_kernel_content(raw_content, max_chars);
                self.run.hub.emit(json!({
                    "type": "toolResult",
                    "callId": call_id,
                    "result": {
                        "content": truncated,
                        "details": Value::Null,
                        "isError": is_error,
                    },
                }));
                let (entry, rev) = {
                    let mut st = self.handle.state.lock().unwrap();
                    let entry = SessionEntry::ToolResult {
                        id: new_entry_id(),
                        parent_id: st.leaf_id.clone(),
                        timestamp: utc_iso8601_now(),
                        metadata: None,
                        call_id,
                        name,
                        content: truncated,
                        is_error,
                    };
                    st.append_entry(entry.clone());
                    (entry, st.revision)
                };
                self.run.hub.emit(json!({
                    "type": "entry",
                    "entry": entry,
                    "revision": rev,
                }));
            }
        }

        let (revision, leaf_id) = {
            let mut st = self.handle.state.lock().unwrap();
            let rev = st.revision;
            let leaf = st.leaf_id.clone();
            st.finish_active_run(&self.run.run_id);
            (rev, leaf)
        };
        let mut done = json!({
            "type": "done",
            "reason": "aborted",
            "stopReason": "aborted",
            "revision": revision,
        });
        if let Some(leaf) = leaf_id {
            done["leafId"] = json!(leaf);
        }
        if let Some(uid) = &self.run.user_entry_id {
            done["userEntryId"] = json!(uid);
        }
        if let Some(aid) = self.run.last_assistant_entry_id.lock().unwrap().clone() {
            done["assistantEntryId"] = json!(aid);
        }
        self.run.hub.emit(done);
        self.run.mark_done_signal();
        Ok(())
    }

    async fn on_run_error(&self, _task: &mut Task, err: &anyhow::Error) -> Result<()> {
        {
            let _st = self.handle.state.lock().unwrap();
            if self.run.finished.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
        }
        self.run.close_streaming_blocks();
        let (code, status, retryable, upstream_code, raw_message) =
            if let Some(me) = err.downcast_ref::<EmbeddedModelError>() {
                (
                    me.code,
                    me.status,
                    me.retryable,
                    me.upstream_code.clone(),
                    me.message.clone(),
                )
            } else {
                ("INTERNAL", None, false, None, err.to_string())
            };
        let message = crate::redaction::scrub(&raw_message, &self.run.model_proxy.token);

        let mut err_ev = json!({
            "type": "error",
            "code": code,
            "message": message,
            "retryable": retryable,
        });
        if let Some(s) = status {
            err_ev["status"] = json!(s);
        }
        if let Some(uc) = upstream_code
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let bounded: String = uc.chars().take(1024).collect();
            err_ev["upstreamCode"] = json!(bounded);
        }
        self.run.hub.emit(err_ev);

        let (revision, leaf_id) = {
            let mut st = self.handle.state.lock().unwrap();
            let rev = st.revision;
            let leaf = st.leaf_id.clone();
            st.finish_active_run(&self.run.run_id);
            (rev, leaf)
        };
        let mut done = json!({
            "type": "done",
            "reason": "failed",
            "stopReason": "error",
            "revision": revision,
        });
        if let Some(leaf) = leaf_id {
            done["leafId"] = json!(leaf);
        }
        if let Some(uid) = &self.run.user_entry_id {
            done["userEntryId"] = json!(uid);
        }
        if let Some(aid) = self.run.last_assistant_entry_id.lock().unwrap().clone() {
            done["assistantEntryId"] = json!(aid);
        }
        self.run.hub.emit(done);
        self.run.mark_done_signal();
        Ok(())
    }

    async fn on_max_rounds_reached(&self, _task: &mut Task) -> Result<()> {
        {
            let _st = self.handle.state.lock().unwrap();
            if self.run.finished.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
        }
        self.run.hub.emit(json!({
            "type": "error",
            "code": "MAX_REQUESTS_REACHED",
            "message": "maximum model requests reached for run",
            "retryable": false,
        }));
        let (revision, leaf_id) = {
            let mut st = self.handle.state.lock().unwrap();
            let rev = st.revision;
            let leaf = st.leaf_id.clone();
            st.finish_active_run(&self.run.run_id);
            (rev, leaf)
        };
        let mut done = json!({
            "type": "done",
            "reason": "failed",
            "stopReason": "error",
            "revision": revision,
        });
        if let Some(leaf) = leaf_id {
            done["leafId"] = json!(leaf);
        }
        if let Some(uid) = &self.run.user_entry_id {
            done["userEntryId"] = json!(uid);
        }
        if let Some(aid) = self.run.last_assistant_entry_id.lock().unwrap().clone() {
            done["assistantEntryId"] = json!(aid);
        }
        self.run.hub.emit(done);
        self.run.mark_done_signal();
        Ok(())
    }
}

async fn run_embedded_task(
    handle: Arc<EmbeddedSessionHandle>,
    run: Arc<ActiveRunControl>,
    mut task: Task,
) {
    let strategy = NoopStrategyDecider;
    let events = NoopEmbeddedEventSink;
    let repo = EmbeddedTaskRepository;
    let creds = match EmbeddedCredentialSource::new(
        run.model_proxy.clone(),
        run.proxy_protocol,
        Some(run.hub.clone()),
        run.run_thinking_level.clone(),
    ) {
        Ok(c) => c,
        Err(err) => {
            let ctrl = EmbeddedTurnController {
                handle,
                run: run.clone(),
            };
            let _ = ctrl.on_run_error(&mut task, &err).await;
            return;
        }
    };
    let tools = EmbeddedToolExecutor {
        handle: handle.clone(),
        run: run.clone(),
    };
    let controller = EmbeddedTurnController {
        handle,
        run: run.clone(),
    };
    let cancel = run.cancel.clone();
    if let Err(err) = crate::agent_loop::run_task_with_controller(
        &mut task,
        &strategy,
        &events,
        &repo,
        &creds,
        &tools,
        &controller,
        &cancel,
    )
    .await
    {
        if cancel.is_cancelled() {
            let _ = controller.on_aborted(&mut task, "", "").await;
        } else {
            let _ = controller.on_run_error(&mut task, &err).await;
        }
    }
    run.mark_done_signal();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel_embedded::ExecutionMode;

    fn basic_hydrate() -> HydrateInput {
        HydrateInput {
            cwd: ".".into(),
            system_prompt: "sys".into(),
            model: HydrateModelConfig {
                model_id: "gpt-5.2".into(),
                protocol: "openai-responses".into(),
                context_window: Some(400_000),
                max_output_tokens: Some(16_384),
                thinking_level: Some("medium".into()),
            },
            tools: vec![KernelToolDescriptor {
                name: "read".into(),
                description: "read file".into(),
                parameters: serde_json::from_value(json!({"type": "object"})).unwrap(),
                label: Some("Read".into()),
                execution_mode: Some(ExecutionMode::Parallel),
            }],
            active_tool_names: vec!["read".into()],
            tool_result_max_chars: Some(None),
            compaction: CompactionConfig {
                enabled: true,
                keep_recent_tokens: Some(20_000),
            },
            steering_mode: SteeringMode::OneAtATime,
            gates: vec![GateKind::BeforeModelTurn],
            context_projection: None,
            leaf_id: None,
            entries: vec![],
        }
    }

    #[tokio::test]
    async fn hydrate_append_branch_and_unknown_tool_validation() {
        let store = EmbeddedSessionStore::new();
        let (res, created) = store
            .hydrate_session("s-1".into(), basic_hydrate())
            .await
            .unwrap();
        assert!(created);
        assert_eq!(res.revision, 1);

        let e1 = store
            .append_entry(
                "s-1",
                Some(1),
                AppendableSessionEntry::CustomMessage {
                    name: "codaNote".into(),
                    content: vec![KernelContent::Text { text: "hi".into() }],
                    display: true,
                    details: None,
                    include_in_summary: Some(true),
                },
                false,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(e1.revision, 2);

        // Revision mismatch returns 409 SESSION_DIVERGED.
        let div = store.branch_session("s-1", 1, None).await.unwrap_err();
        assert_eq!(div.status, 409);
        assert_eq!(div.code, "SESSION_DIVERGED");

        // Valid branch bumps revision.
        let br = store.branch_session("s-1", 2, None).await.unwrap();
        assert_eq!(br.revision, 3);
        assert!(br.leaf_id.is_none());

        // Unknown tool rejected.
        let unk = store
            .set_active_tools("s-1", vec!["does_not_exist".into()])
            .unwrap_err();
        assert_eq!(unk.status, 400);
        assert_eq!(unk.code, "UNKNOWN_TOOL");
    }

    #[tokio::test]
    async fn tool_limit_512_and_truncation_behavior() {
        let store = EmbeddedSessionStore::new();
        let mut h = basic_hydrate();
        h.tools = (0..513)
            .map(|i| KernelToolDescriptor {
                name: format!("t{i}"),
                description: "d".into(),
                parameters: Map::new(),
                label: None,
                execution_mode: None,
            })
            .collect();
        h.active_tool_names = vec![];
        let err = store.hydrate_session("s-cap".into(), h).await.unwrap_err();
        assert_eq!(err.status, 400);

        let long_text = "a".repeat(40);
        let kept = truncate_kernel_content(
            vec![KernelContent::Text {
                text: long_text.clone(),
            }],
            None,
        );
        assert_eq!(
            kept,
            vec![KernelContent::Text {
                text: long_text.clone()
            }]
        );

        let trunc =
            truncate_kernel_content(vec![KernelContent::Text { text: long_text }], Some(20));
        match &trunc[0] {
            KernelContent::Text { text } => {
                assert!(text.starts_with(&"a".repeat(10)));
                assert!(text.contains("truncated"));
                assert!(text.ends_with(&"a".repeat(5)));
            }
            _ => panic!("expected text"),
        }
    }

    #[tokio::test]
    async fn idle_reaper_evicts_expired_idle_sessions_only() {
        let store = EmbeddedSessionStore::new();
        store
            .hydrate_session("s-old".into(), basic_hydrate())
            .await
            .unwrap();
        store
            .hydrate_session("s-fresh".into(), basic_hydrate())
            .await
            .unwrap();
        {
            let h = store.get_handle("s-old").unwrap();
            h.state.lock().unwrap().last_activity =
                Instant::now() - Duration::from_secs(IDLE_SESSION_TTL_SECS + 10);
        }
        let evicted = store.reap_idle_sessions();
        assert_eq!(evicted, 1);
        assert!(store.get_session("s-old").is_err());
        assert!(store.get_session("s-fresh").is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_run_and_abort_all_runs_complete_immediately_without_notify_lost_wakeup() {
        let store = EmbeddedSessionStore::new();
        for sid in ["s-ab-1", "s-ab-2", "s-ab-3"] {
            let mut h = basic_hydrate();
            h.gates = vec![GateKind::BeforeModelTurn];
            store.hydrate_session(sid.into(), h).await.unwrap();
            let (_ctrl, _replay, _rx, _done) = store
                .start_run(
                    sid,
                    RunStartConfig {
                        run_id: format!("run-{sid}"),
                        base_revision: 1,
                        system_prompt: None,
                        model: None,
                        append_entries: vec![],
                        content: vec![KernelContent::Text { text: "hi".into() }],
                        thinking_level: None,
                        context_window: None,
                        max_output_tokens: None,
                        leaf_id: None,
                        max_requests: 4,
                        runtime_context: None,
                        prune_boundary_call_id: None,
                        model_proxy: ModelProxyConfig {
                            base_url: "http://127.0.0.1:1/m/unused".into(),
                            token: "tok".into(),
                            protocol: "openai-responses".into(),
                        },
                    },
                )
                .await
                .unwrap();
        }

        // Wait briefly until all 3 runs enter waitingGate.
        for sid in ["s-ab-1", "s-ab-2", "s-ab-3"] {
            let rid = format!("run-{sid}");
            loop {
                let (replay, _, _) = store.subscribe_run_events(sid, &rid, 0).unwrap();
                if replay.iter().any(|e| e.event["type"] == "gate") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        // abort_all_runs MUST abort all 3 concurrently well under 1s (not 3s sequential stall).
        let t0 = Instant::now();
        store.abort_all_runs().await;
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "abort_all_runs took too long: {:?}",
            t0.elapsed()
        );

        // Calling abort_run again on an already-finished run MUST return immediately.
        let t1 = Instant::now();
        store.abort_run("s-ab-1", "run-s-ab-1").await.unwrap();
        assert!(t1.elapsed() < Duration::from_millis(200));

        // Steer after run finished MUST return 409 RUN_FINISHED.
        let err = store
            .submit_steer(
                "s-ab-1",
                "run-s-ab-1",
                vec![KernelContent::Text {
                    text: "late".into(),
                }],
            )
            .unwrap_err();
        assert_eq!(err.status, 409);
        assert_eq!(err.code, "RUN_FINISHED");

        // Deleting a session with a finished run returns immediately.
        let t2 = Instant::now();
        store.delete_session("s-ab-2").await.unwrap();
        assert!(t2.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn mid_run_patch_system_prompt_overrides_run_system_prompt_for_next_turn() {
        let store = EmbeddedSessionStore::new();
        let mut h = basic_hydrate();
        h.system_prompt = "session initial".into();
        h.gates = vec![GateKind::BeforeModelTurn];
        store.hydrate_session("s-sp".into(), h).await.unwrap();

        let (_ctrl, _replay, _rx, _done) = store
            .start_run(
                "s-sp",
                RunStartConfig {
                    run_id: "run-sp".into(),
                    base_revision: 1,
                    system_prompt: Some("run override initial".into()),
                    model: None,
                    append_entries: vec![],
                    content: vec![KernelContent::Text { text: "hi".into() }],
                    thinking_level: None,
                    context_window: None,
                    max_output_tokens: None,
                    leaf_id: None,
                    max_requests: 4,
                    runtime_context: None,
                    prune_boundary_call_id: None,
                    model_proxy: ModelProxyConfig {
                        base_url: "http://127.0.0.1:1/m/unused".into(),
                        token: "tok".into(),
                        protocol: "openai-responses".into(),
                    },
                },
            )
            .await
            .unwrap();

        let handle = store.get_handle("s-sp").unwrap();
        let active_run = handle.state.lock().unwrap().active_run.clone().unwrap();
        let controller = EmbeddedTurnController {
            handle: handle.clone(),
            run: active_run,
        };
        let before = controller.build_projected_messages().unwrap();
        assert_eq!(before.messages[0].text_content(), "run override initial");

        store
            .patch_session(
                "s-sp",
                PatchInput {
                    system_prompt: Some("patched mid-run system".into()),
                    model: None,
                    tools: None,
                    active_tool_names: None,
                    compaction: None,
                    gates: None,
                    tool_result_max_chars: None,
                    runtime_context: None,
                    prune_boundary_call_id: None,
                },
            )
            .unwrap();

        let after = controller.build_projected_messages().unwrap();
        assert_eq!(after.messages[0].text_content(), "patched mid-run system");

        store.abort_run("s-sp", "run-sp").await.unwrap();
    }
}
