//! Embedded session compaction (`codaKernel/1 §8`): §8.3 exclusions, atomic
//! `(assistant toolCall + toolResult)` preservation, and LLM summarization via `modelProxy`.

use crate::kernel_embedded::credential_source::{
    EmbeddedCredentialSource, EmbeddedModelError, SharedEmbeddedProvider,
};
use crate::kernel_embedded::projection::estimate_message_tokens;
use crate::kernel_embedded::{ModelProxyConfig, new_entry_id};
use crate::messages::{
    KernelContent, NeutralMessage, SessionBlock, SessionEntry, SessionMessageRole,
};
use crate::provider_runtime::CompletionInput;
use crate::provider_transport::ProviderProtocol;
use crate::runtime_seams::KernelUsageBreakdown;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const DEFAULT_COMPACTION_SYSTEM_PROMPT: &str = "You are a session summarization assistant. Produce a concise, accurate, structured checkpoint of the conversation history so work can resume seamlessly. Preserve exact file paths, symbol names, tool results, user constraints, decisions made, and remaining work. Do not invent facts.";

#[derive(Clone, Debug)]
pub struct CompactionPreparation {
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    pub entry_ids_to_summarize: Vec<String>,
    pub prior_summary: Option<String>,
    pub entries_to_summarize: Vec<SessionEntry>,
}

#[derive(Debug)]
pub enum CompactionError {
    EmptySummary,
    Upstream(EmbeddedModelError),
    Internal(String),
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySummary => f.write_str("compaction model returned an empty summary"),
            Self::Upstream(e) => write!(f, "{e}"),
            Self::Internal(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for CompactionError {}

fn estimate_entry_tokens(entry: &SessionEntry, protocol: ProviderProtocol) -> u64 {
    match entry.to_neutral_message_for_protocol(Some(protocol)) {
        Ok(Some(msg)) => estimate_message_tokens(&msg),
        _ => match entry {
            SessionEntry::BranchSummary { summary, .. } => {
                (summary.chars().count() as u64).div_ceil(4)
            }
            _ => 0,
        },
    }
}

fn is_excluded_from_summary(entry: &SessionEntry) -> bool {
    match entry {
        SessionEntry::CustomMessage {
            name,
            include_in_summary,
            ..
        } => name == "codaRuntimeContext" || *include_in_summary == Some(false),
        SessionEntry::Message {
            role: SessionMessageRole::System,
            ..
        } => true,
        SessionEntry::ContextEdit { .. }
        | SessionEntry::Model { .. }
        | SessionEntry::ThinkingLevel { .. }
        | SessionEntry::Usage { .. }
        | SessionEntry::Compaction { .. }
        | SessionEntry::Label { .. }
        | SessionEntry::SessionInfo { .. }
        | SessionEntry::Custom { .. } => true,
        SessionEntry::Message { .. }
        | SessionEntry::ToolResult { .. }
        | SessionEntry::BranchSummary { .. } => false,
    }
}

pub fn prepare_compaction(
    branch: &[SessionEntry],
    protocol: ProviderProtocol,
    keep_recent_tokens: u64,
    new_compaction_entry_id: &str,
    last_usage_tokens: Option<u64>,
) -> CompactionPreparation {
    let mut edits: HashMap<&str, Option<&Vec<SessionBlock>>> = HashMap::new();
    for entry in branch {
        if let SessionEntry::ContextEdit {
            target_id,
            replacement,
            ..
        } = entry
        {
            edits.insert(target_id.as_str(), replacement.as_ref());
        }
    }

    let compaction_idx = branch
        .iter()
        .rposition(|e| matches!(e, SessionEntry::Compaction { .. }));

    let mut prior_summary: Option<String> = None;
    let mut active_window: Vec<SessionEntry> = Vec::new();
    if let Some(c_idx) = compaction_idx {
        if let SessionEntry::Compaction {
            id,
            summary,
            first_kept_entry_id,
            ..
        } = &branch[c_idx]
        {
            prior_summary = Some(summary.clone());
            if first_kept_entry_id != id
                && let Some(pos) = branch[..c_idx]
                    .iter()
                    .position(|e| e.id() == first_kept_entry_id)
            {
                active_window.extend(branch[pos..c_idx].iter().cloned());
            }
        }
        active_window.extend(branch[c_idx + 1..].iter().cloned());
    } else {
        active_window = branch.to_vec();
    }

    // Apply contextEdit(null) / content replacements and §8.3 exclusions.
    let mut summarizable: Vec<SessionEntry> = Vec::with_capacity(active_window.len());
    let mut window_tokens: u64 = 0;
    for mut entry in active_window {
        if let Some(edit) = edits.get(entry.id()) {
            match edit {
                None => continue,
                Some(blocks) => {
                    if let SessionEntry::Message {
                        ref mut content, ..
                    } = entry
                    {
                        *content = (*blocks).clone();
                    }
                }
            }
        }
        window_tokens = window_tokens.saturating_add(estimate_entry_tokens(&entry, protocol));
        if !is_excluded_from_summary(&entry) {
            summarizable.push(entry);
        }
    }

    let tokens_before = last_usage_tokens
        .filter(|v| *v > 0)
        .unwrap_or(window_tokens);

    // Group into indivisible atomic units: `(Assistant with ToolCalls + matching ToolResults)`.
    let mut units: Vec<Vec<SessionEntry>> = Vec::new();
    let mut idx = 0;
    while idx < summarizable.len() {
        let entry = &summarizable[idx];
        let call_ids: HashSet<String> = match entry {
            SessionEntry::Message {
                role: SessionMessageRole::Assistant,
                content,
                ..
            } => content
                .iter()
                .filter_map(|b| match b {
                    SessionBlock::ToolCall { call_id, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .collect(),
            _ => HashSet::new(),
        };
        if !call_ids.is_empty() {
            let mut group = vec![entry.clone()];
            idx += 1;
            while idx < summarizable.len() {
                if let SessionEntry::ToolResult { call_id, .. } = &summarizable[idx]
                    && call_ids.contains(call_id)
                {
                    group.push(summarizable[idx].clone());
                    idx += 1;
                    continue;
                }
                break;
            }
            units.push(group);
        } else if matches!(entry, SessionEntry::ToolResult { .. }) {
            if let Some(last) = units.last_mut() {
                last.push(entry.clone());
            } else {
                units.push(vec![entry.clone()]);
            }
            idx += 1;
        } else {
            units.push(vec![entry.clone()]);
            idx += 1;
        }
    }

    let (split_unit_idx, first_kept_entry_id) = if keep_recent_tokens == 0 || units.is_empty() {
        (units.len(), new_compaction_entry_id.to_owned())
    } else {
        let mut kept_tokens: u64 = 0;
        let mut split = units.len();
        for (u_idx, unit) in units.iter().enumerate().rev() {
            let unit_tokens: u64 = unit
                .iter()
                .map(|e| estimate_entry_tokens(e, protocol))
                .sum();
            let must_keep_one = split == units.len();
            if !must_keep_one && kept_tokens.saturating_add(unit_tokens) > keep_recent_tokens {
                break;
            }
            kept_tokens = kept_tokens.saturating_add(unit_tokens);
            split = u_idx;
            if kept_tokens >= keep_recent_tokens {
                break;
            }
        }
        let first_id = units
            .get(split)
            .and_then(|u| u.first())
            .map(|e| e.id().to_owned())
            .unwrap_or_else(|| new_compaction_entry_id.to_owned());
        (split, first_id)
    };

    let mut entries_to_summarize = Vec::new();
    let mut entry_ids_to_summarize = Vec::new();
    for unit in &units[..split_unit_idx] {
        for entry in unit {
            entry_ids_to_summarize.push(entry.id().to_owned());
            entries_to_summarize.push(entry.clone());
        }
    }

    CompactionPreparation {
        first_kept_entry_id,
        tokens_before,
        entry_ids_to_summarize,
        prior_summary,
        entries_to_summarize,
    }
}

fn format_transcript_for_summary(
    prep: &CompactionPreparation,
    instructions: Option<&str>,
) -> String {
    let mut out = String::new();
    if let Some(prior) = &prep.prior_summary
        && !prior.trim().is_empty()
    {
        out.push_str("<prior_summary>\n");
        out.push_str(prior.trim());
        out.push_str("\n</prior_summary>\n\n");
    }

    if prep.entries_to_summarize.is_empty() {
        out.push_str("<conversation>\n(No older entries to summarize beyond the prior summary.)\n</conversation>\n");
    } else {
        out.push_str("<conversation>\n");
        for entry in &prep.entries_to_summarize {
            match entry {
                SessionEntry::Message { role, content, .. } => {
                    let role_str = match role {
                        SessionMessageRole::System => "system",
                        SessionMessageRole::User => "user",
                        SessionMessageRole::Assistant => "assistant",
                    };
                    out.push_str(&format!("[{role_str}]\n"));
                    for block in content {
                        match block {
                            SessionBlock::Text { text, .. } => {
                                if !text.is_empty() {
                                    out.push_str(text);
                                    out.push('\n');
                                }
                            }
                            SessionBlock::Image { mime_type, .. } => {
                                out.push_str(&format!("[image:{mime_type}]\n"));
                            }
                            SessionBlock::Audio {
                                path, mime_type, ..
                            } => {
                                out.push_str(&format!("[audio:{path} ({mime_type})]\n"));
                            }
                            SessionBlock::Video {
                                path, mime_type, ..
                            } => {
                                out.push_str(&format!("[video:{path} ({mime_type})]\n"));
                            }
                            SessionBlock::Resource { path, .. } => {
                                out.push_str(&format!("[resource:{path}]\n"));
                            }
                            SessionBlock::Thinking { .. } => {}
                            SessionBlock::ToolCall {
                                call_id,
                                name,
                                args,
                                ..
                            } => {
                                out.push_str(&format!(
                                    "[toolCall id={call_id} name={name} args={}]\n",
                                    serde_json::Value::Object(args.clone())
                                ));
                            }
                        }
                    }
                    out.push('\n');
                }
                SessionEntry::ToolResult {
                    call_id,
                    name,
                    content,
                    is_error,
                    ..
                } => {
                    out.push_str(&format!(
                        "[toolResult id={call_id} name={name} isError={is_error}]\n"
                    ));
                    for part in content {
                        match part {
                            KernelContent::Text { text } => {
                                out.push_str(text);
                                out.push('\n');
                            }
                            KernelContent::Image { mime_type, .. } => {
                                out.push_str(&format!("[image:{mime_type}]\n"));
                            }
                            KernelContent::Audio {
                                path, mime_type, ..
                            } => {
                                out.push_str(&format!("[audio:{path} ({mime_type})]\n"));
                            }
                            KernelContent::Video {
                                path, mime_type, ..
                            } => {
                                out.push_str(&format!("[video:{path} ({mime_type})]\n"));
                            }
                            KernelContent::Resource { path, .. } => {
                                out.push_str(&format!("[resource:{path}]\n"));
                            }
                        }
                    }
                    out.push('\n');
                }
                SessionEntry::CustomMessage { name, content, .. } => {
                    out.push_str(&format!("[custom:{name}]\n"));
                    for part in content {
                        match part {
                            KernelContent::Text { text } => {
                                out.push_str(text);
                                out.push('\n');
                            }
                            KernelContent::Image { mime_type, .. } => {
                                out.push_str(&format!("[image:{mime_type}]\n"));
                            }
                            KernelContent::Audio {
                                path, mime_type, ..
                            } => {
                                out.push_str(&format!("[audio:{path} ({mime_type})]\n"));
                            }
                            KernelContent::Video {
                                path, mime_type, ..
                            } => {
                                out.push_str(&format!("[video:{path} ({mime_type})]\n"));
                            }
                            KernelContent::Resource { path, .. } => {
                                out.push_str(&format!("[resource:{path}]\n"));
                            }
                        }
                    }
                    out.push('\n');
                }
                SessionEntry::BranchSummary { summary, .. } => {
                    out.push_str(&format!("[branchSummary]\n{summary}\n\n"));
                }
                _ => {}
            }
        }
        out.push_str("</conversation>\n");
    }

    if let Some(inst) = instructions.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str("\n<instructions>\n");
        out.push_str(inst);
        out.push_str("\n</instructions>\n");
    }
    out.push_str("\nSummarize the conversation above into a structured checkpoint.");
    out
}

#[allow(clippy::too_many_arguments)]
pub async fn summarize_compaction(
    proxy: ModelProxyConfig,
    protocol: ProviderProtocol,
    model_id: &str,
    max_output_tokens: u32,
    thinking_level: Option<String>,
    prep: &CompactionPreparation,
    system_prompt: Option<&str>,
    instructions: Option<&str>,
    session_id: Option<&str>,
    cancel: &CancellationToken,
) -> Result<(String, Option<KernelUsageBreakdown>), CompactionError> {
    summarize_compaction_with_shared(
        None,
        proxy,
        protocol,
        model_id,
        max_output_tokens,
        thinking_level,
        prep,
        system_prompt,
        instructions,
        session_id,
        cancel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn summarize_compaction_with_shared(
    shared: Option<SharedEmbeddedProvider>,
    proxy: ModelProxyConfig,
    protocol: ProviderProtocol,
    model_id: &str,
    max_output_tokens: u32,
    thinking_level: Option<String>,
    prep: &CompactionPreparation,
    system_prompt: Option<&str>,
    instructions: Option<&str>,
    session_id: Option<&str>,
    cancel: &CancellationToken,
) -> Result<(String, Option<KernelUsageBreakdown>), CompactionError> {
    let mut creds = match shared {
        Some(sp) => EmbeddedCredentialSource::with_shared(
            sp,
            proxy,
            protocol,
            None,
            Arc::new(Mutex::new(thinking_level)),
        ),
        None => EmbeddedCredentialSource::new(
            proxy,
            protocol,
            None,
            Arc::new(Mutex::new(thinking_level)),
        )
        .map_err(|e| CompactionError::Internal(e.to_string()))?,
    }
    .with_allow_empty_text(true);
    if let Some(sid) = session_id {
        creds = creds.with_session_id(sid);
    }

    let sys = system_prompt
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_COMPACTION_SYSTEM_PROMPT);
    let user_prompt = format_transcript_for_summary(prep, instructions);
    let messages = vec![
        NeutralMessage::system(sys),
        NeutralMessage::user_text(user_prompt),
    ];
    let input = CompletionInput {
        model: model_id,
        max_tokens: max_output_tokens.max(1024),
        messages: &messages,
        tools: &[],
    };
    let mut sink = |_delta: &str| Ok(());
    let mut reasoning_sink = |_delta: &str| Ok(());

    let detailed = creds
        .execute_once_with_retries(&input, &mut sink, &mut reasoning_sink, cancel)
        .await
        .map_err(CompactionError::Upstream)?;

    let summary = detailed.turn.message.text_content().trim().to_owned();
    if summary.is_empty() {
        return Err(CompactionError::EmptySummary);
    }
    Ok((summary, detailed.usage_breakdown))
}

pub fn make_compaction_entry_id() -> String {
    new_entry_id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn user_entry(id: &str, text: &str) -> SessionEntry {
        SessionEntry::Message {
            id: id.into(),
            parent_id: None,
            timestamp: "2026-11-26T00:00:00Z".into(),
            metadata: None,
            role: SessionMessageRole::User,
            content: vec![SessionBlock::Text {
                text: text.into(),
                signature: None,
            }],
        }
    }

    fn assistant_tool_entry(id: &str, call_id: &str, text: &str) -> SessionEntry {
        SessionEntry::Message {
            id: id.into(),
            parent_id: None,
            timestamp: "2026-11-26T00:00:00Z".into(),
            metadata: None,
            role: SessionMessageRole::Assistant,
            content: vec![
                SessionBlock::Text {
                    text: text.into(),
                    signature: None,
                },
                SessionBlock::ToolCall {
                    call_id: call_id.into(),
                    name: "read".into(),
                    args: Map::new(),
                    thought_signature: None,
                    namespace: None,
                },
            ],
        }
    }

    fn tool_result_entry(id: &str, call_id: &str, text: &str) -> SessionEntry {
        SessionEntry::ToolResult {
            id: id.into(),
            parent_id: None,
            timestamp: "2026-11-26T00:00:00Z".into(),
            metadata: None,
            call_id: call_id.into(),
            name: "read".into(),
            content: vec![KernelContent::Text { text: text.into() }],
            is_error: false,
        }
    }

    #[test]
    fn compaction_excludes_runtime_context_include_in_summary_false_and_context_edit_null() {
        let branch = vec![
            user_entry("u1", &"a".repeat(400)),
            SessionEntry::CustomMessage {
                id: "rc1".into(),
                parent_id: None,
                timestamp: "2026-11-26T00:00:00Z".into(),
                metadata: None,
                name: "codaRuntimeContext".into(),
                content: vec![KernelContent::Text {
                    text: "secret-runtime-ctx".into(),
                }],
                display: false,
                details: None,
                include_in_summary: None,
            },
            SessionEntry::CustomMessage {
                id: "cm_no".into(),
                parent_id: None,
                timestamp: "2026-11-26T00:00:00Z".into(),
                metadata: None,
                name: "ephemeralNote".into(),
                content: vec![KernelContent::Text {
                    text: "exclude-me".into(),
                }],
                display: true,
                details: None,
                include_in_summary: Some(false),
            },
            user_entry("u_del", "should-be-deleted"),
            SessionEntry::ContextEdit {
                id: "ce1".into(),
                parent_id: None,
                timestamp: "2026-11-26T00:00:00Z".into(),
                metadata: None,
                target_id: "u_del".into(),
                replacement: None,
            },
            user_entry("u2", &"b".repeat(400)),
        ];

        let prep = prepare_compaction(&branch, ProviderProtocol::OpenAi, 0, "comp-1", None);
        assert_eq!(prep.entry_ids_to_summarize, vec!["u1", "u2"]);
        assert_eq!(prep.first_kept_entry_id, "comp-1");
    }

    #[test]
    fn compaction_never_splits_assistant_tool_call_and_tool_result_atomic_unit() {
        let branch = vec![
            user_entry("u1", &"old-turn ".repeat(50)),
            assistant_tool_entry("a1", "call_1", &"call ".repeat(40)),
            tool_result_entry("tr1", "call_1", &"result ".repeat(40)),
        ];

        // keep_recent_tokens is larger than tr1 alone (~70 tokens), smaller than a1+tr1 (~120 tokens),
        // or right at the boundary: because a1+tr1 is the newest unit, the entire atomic unit MUST stay together.
        let prep = prepare_compaction(&branch, ProviderProtocol::OpenAi, 80, "comp-1", None);
        assert_eq!(prep.first_kept_entry_id, "a1");
        assert_eq!(prep.entry_ids_to_summarize, vec!["u1"]);
    }
}
