//! Declarative model-view context projections and token estimation (`codaKernel/1 §4, §6, §7, §8.3`).

use crate::kernel_embedded::new_entry_id;
use crate::messages::{
    ContentBlock, KernelContent, NeutralMessage, NeutralRole, SessionBlock, SessionEntry,
    session_entries_to_neutral_messages,
};
use crate::provider_transport::ProviderProtocol;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

#[derive(Clone, Debug, Default)]
pub struct ProjectionParams<'a> {
    pub system_prompt: &'a str,
    pub active_protocol: ProviderProtocol,
    pub patched_runtime_context: Option<&'a [KernelContent]>,
    pub run_runtime_context: Option<&'a [KernelContent]>,
    pub prune_boundary_call_id: Option<&'a str>,
    pub prune_min_length: usize,
    pub prune_keep_length: usize,
    pub tool_result_notes: Option<&'a HashMap<String, String>>,
}

/// Applies the 4 declarative projections on a derived branch view without mutating
/// the stored `SessionEntry` list or `revision`.
pub fn build_model_view_messages(
    branch: &[SessionEntry],
    params: &ProjectionParams<'_>,
) -> Result<Vec<NeutralMessage>> {
    // Collect latest contextEdit per targetId across the branch.
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

    // Locate latest Compaction entry (if any) and slice active window.
    let compaction_idx = branch
        .iter()
        .rposition(|e| matches!(e, SessionEntry::Compaction { .. }));

    let mut compaction_summary: Option<String> = None;
    let mut prior_model: Option<SessionEntry> = None;
    let mut window: Vec<SessionEntry> = Vec::new();

    if let Some(c_idx) = compaction_idx {
        if let SessionEntry::Compaction {
            id,
            summary,
            first_kept_entry_id,
            ..
        } = &branch[c_idx]
        {
            compaction_summary = Some(summary.clone());
            if first_kept_entry_id != id
                && let Some(kept_start) = branch[..c_idx]
                    .iter()
                    .position(|e| e.id() == first_kept_entry_id)
            {
                // Preserve the active Model entry prior to `kept_start` so kept entries
                // retain accurate protocol metadata in `session_entries_to_neutral_messages`.
                prior_model = branch[..kept_start]
                    .iter()
                    .rev()
                    .find(|e| matches!(e, SessionEntry::Model { .. }))
                    .cloned();
                for entry in &branch[kept_start..c_idx] {
                    let is_system = matches!(
                        entry,
                        SessionEntry::Message {
                            role: crate::messages::SessionMessageRole::System,
                            ..
                        }
                    );
                    if !is_system {
                        window.push(entry.clone());
                    }
                }
            } else {
                prior_model = branch[..c_idx]
                    .iter()
                    .rev()
                    .find(|e| matches!(e, SessionEntry::Model { .. }))
                    .cloned();
            }
        }
        window.extend(branch[c_idx + 1..].iter().cloned());
    } else {
        window = branch.to_vec();
    }

    // Projection 2: Apply contextEdit (drop ContextEdit entries, drop `null` targets,
    // replace `Message` content when replacement is `Some(blocks)`).
    let mut after_edits: Vec<SessionEntry> = Vec::with_capacity(window.len());
    for mut entry in window {
        if matches!(entry, SessionEntry::ContextEdit { .. }) {
            continue;
        }
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
        after_edits.push(entry);
    }

    // Projection 1a: Keep only the latest `codaRuntimeContext` CustomMessage,
    // and apply `PATCH.runtimeContext` replacement in-place if present.
    let last_ctx_idx = after_edits.iter().rposition(|e| {
        matches!(
            e,
            SessionEntry::CustomMessage { name, .. } if name == "codaRuntimeContext"
        )
    });

    let mut projected_entries: Vec<SessionEntry> = Vec::with_capacity(after_edits.len() + 2);
    if let Some(pm) = prior_model {
        projected_entries.push(pm);
    }
    let effective_rc = params
        .patched_runtime_context
        .or(params.run_runtime_context);
    let mut replaced_in_place = false;
    let mut replaced_by_run_ctx = false;
    for (idx, mut entry) in after_edits.into_iter().enumerate() {
        if matches!(
            &entry,
            SessionEntry::CustomMessage { name, .. } if name == "codaRuntimeContext"
        ) {
            if Some(idx) != last_ctx_idx {
                continue;
            }
            if let Some(rc) = effective_rc
                && (params.patched_runtime_context.is_some() || !rc.is_empty())
                && let SessionEntry::CustomMessage {
                    ref mut content, ..
                } = entry
            {
                *content = rc.to_vec();
                replaced_in_place = true;
                if params.patched_runtime_context.is_none() {
                    replaced_by_run_ctx = true;
                }
            }
        }
        projected_entries.push(entry);
    }

    if let Some(patched) = params.patched_runtime_context
        && !replaced_in_place
        && !patched.is_empty()
    {
        projected_entries.push(SessionEntry::CustomMessage {
            id: new_entry_id(),
            parent_id: None,
            timestamp: String::new(),
            metadata: None,
            name: "codaRuntimeContext".into(),
            content: patched.to_vec(),
            display: false,
            details: None,
            include_in_summary: Some(false),
        });
    }

    // Build NeutralMessages using M2's protocol-aware converter (`session_entries_to_neutral_messages`).
    let mut messages = Vec::new();
    if !params.system_prompt.is_empty() {
        messages.push(NeutralMessage::system(params.system_prompt));
    }
    if let Some(summary) = compaction_summary {
        messages.push(NeutralMessage::user_text(format!(
            "{COMPACTION_SUMMARY_PREFIX}{summary}{COMPACTION_SUMMARY_SUFFIX}"
        )));
    }
    messages.extend(session_entries_to_neutral_messages(
        &projected_entries,
        Some(params.active_protocol),
    )?);

    // Projection 1b: Ephemeral Run-level `runtimeContext` appended at tail
    // (or overwriting trailing `codaRuntimeContext` in model view) unless it already
    // replaced the latest persisted `codaRuntimeContext` entry in-place.
    if !replaced_by_run_ctx
        && let Some(run_ctx) = params.run_runtime_context
        && !run_ctx.is_empty()
    {
        let blocks: Vec<ContentBlock> = run_ctx
            .iter()
            .map(KernelContent::to_content_block)
            .collect();
        if let Some(last) = messages.last_mut()
            && matches!(&last.role, NeutralRole::Custom { name, .. } if name == "codaRuntimeContext")
        {
            last.content = blocks;
        } else {
            messages.push(NeutralMessage {
                role: NeutralRole::Custom {
                    name: "codaRuntimeContext".into(),
                    display: false,
                    details: None,
                    include_in_summary: Some(false),
                },
                content: blocks,
            });
        }
    }

    // Projection 3: `pruneBoundaryCallId` truncation for older ToolResults strictly before boundary.
    if let Some(boundary_call_id) = params.prune_boundary_call_id
        && let Some(boundary_idx) = messages.iter().position(|m| {
            matches!(
                &m.role,
                NeutralRole::ToolResult { call_id, .. } if call_id == boundary_call_id
            )
        })
    {
        for msg in &mut messages[..boundary_idx] {
            if matches!(msg.role, NeutralRole::ToolResult { .. }) {
                prune_tool_result_message(msg, params.prune_min_length, params.prune_keep_length);
            }
        }
    }

    // Projection 4: Append `modelNote` / `toolResultNotes` (`\n\n${note}`) ONLY in model view.
    if let Some(notes) = params.tool_result_notes {
        for msg in &mut messages {
            if let NeutralRole::ToolResult { call_id, .. } = &msg.role
                && let Some(note) = notes.get(call_id)
                && !note.is_empty()
            {
                append_model_note_to_message(msg, note);
            }
        }
    }

    Ok(messages)
}

fn prune_tool_result_message(
    msg: &mut NeutralMessage,
    prune_min_length: usize,
    prune_keep_length: usize,
) {
    let total_chars: usize = msg
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.chars().count()),
            _ => None,
        })
        .sum();
    if total_chars <= prune_min_length {
        return;
    }

    let mut remaining = prune_keep_length;
    let mut new_content = Vec::with_capacity(msg.content.len() + 1);
    let mut inserted_notice = false;
    for block in msg.content.drain(..) {
        match block {
            ContentBlock::Text { text, signature } => {
                if remaining > 0 {
                    let count = text.chars().count();
                    if count <= remaining {
                        remaining -= count;
                        new_content.push(ContentBlock::Text { text, signature });
                    } else {
                        let cut_byte = text
                            .char_indices()
                            .nth(remaining)
                            .map(|(i, _)| i)
                            .unwrap_or(text.len());
                        remaining = 0;
                        let omitted = total_chars.saturating_sub(prune_keep_length);
                        new_content.push(ContentBlock::Text {
                            text: format!(
                                "{}\n\n[... pruned {omitted} chars before pruneBoundaryCallId ...]",
                                &text[..cut_byte]
                            ),
                            signature,
                        });
                        inserted_notice = true;
                    }
                } else if !inserted_notice {
                    let omitted = total_chars.saturating_sub(prune_keep_length);
                    new_content.push(ContentBlock::Text {
                        text: format!(
                            "[... pruned {omitted} chars before pruneBoundaryCallId ...]"
                        ),
                        signature: None,
                    });
                    inserted_notice = true;
                }
            }
            other => new_content.push(other),
        }
    }
    if !inserted_notice {
        let omitted = total_chars.saturating_sub(prune_keep_length);
        new_content.push(ContentBlock::Text {
            text: format!("\n\n[... pruned {omitted} chars before pruneBoundaryCallId ...]"),
            signature: None,
        });
    }
    msg.content = new_content;
}

fn append_model_note_to_message(msg: &mut NeutralMessage, note: &str) {
    for block in msg.content.iter_mut().rev() {
        if let ContentBlock::Text { text, .. } = block {
            if text.is_empty() {
                text.push_str(note);
            } else {
                text.push_str("\n\n");
                text.push_str(note);
            }
            return;
        }
    }
    msg.content.push(ContentBlock::Text {
        text: format!("\n\n{note}"),
        signature: None,
    });
}

pub fn estimate_message_tokens(msg: &NeutralMessage) -> u64 {
    let mut chars: u64 = 0;
    for block in &msg.content {
        match block {
            ContentBlock::Text { text, .. } => {
                chars = chars.saturating_add(text.chars().count() as u64);
            }
            ContentBlock::Thinking { text, .. } => {
                chars = chars.saturating_add(text.chars().count() as u64);
            }
            ContentBlock::RedactedThinking { data } => {
                chars = chars.saturating_add(data.len() as u64);
            }
            ContentBlock::ToolCall(tc) => {
                chars = chars
                    .saturating_add(tc.name.len() as u64)
                    .saturating_add(tc.arguments.len() as u64);
            }
            ContentBlock::Image { .. } => {
                chars = chars.saturating_add(4800);
            }
            ContentBlock::Audio { .. } => {
                chars = chars.saturating_add(1600);
            }
            ContentBlock::Video { .. } => {
                chars = chars.saturating_add(3200);
            }
            ContentBlock::Resource { path, .. } => {
                chars = chars.saturating_add(128).saturating_add(path.len() as u64);
            }
        }
    }
    chars.div_ceil(4)
}

pub fn estimate_context_tokens(messages: &[NeutralMessage], tools: &[Value]) -> u64 {
    let msg_tokens: u64 = messages.iter().map(estimate_message_tokens).sum();
    let tools_chars: u64 = tools
        .iter()
        .map(|t| {
            serde_json::to_string(t)
                .map(|s| s.len() as u64)
                .unwrap_or(0)
        })
        .sum();
    msg_tokens.saturating_add(tools_chars.div_ceil(4))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{KernelModel, SessionMessageRole};
    use serde_json::{Map, json};

    fn text_msg(id: &str, role: SessionMessageRole, text: &str) -> SessionEntry {
        SessionEntry::Message {
            id: id.into(),
            parent_id: None,
            timestamp: "2026-11-26T00:00:00Z".into(),
            metadata: None,
            role,
            content: vec![SessionBlock::Text {
                text: text.into(),
                signature: None,
            }],
        }
    }

    fn runtime_ctx(id: &str, text: &str) -> SessionEntry {
        SessionEntry::CustomMessage {
            id: id.into(),
            parent_id: None,
            timestamp: "2026-11-26T00:00:00Z".into(),
            metadata: None,
            name: "codaRuntimeContext".into(),
            content: vec![KernelContent::Text { text: text.into() }],
            display: false,
            details: None,
            include_in_summary: Some(false),
        }
    }

    fn tool_res(id: &str, call_id: &str, text: &str) -> SessionEntry {
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
    fn four_declarative_projections_do_not_mutate_stored_entries() {
        let mut notes = HashMap::new();
        notes.insert("c2".into(), "step 2 progress".into());

        let branch = vec![
            runtime_ctx("e1", "old owner ctx"),
            text_msg("e2", SessionMessageRole::User, "deleted user msg"),
            SessionEntry::ContextEdit {
                id: "e3".into(),
                parent_id: Some("e2".into()),
                timestamp: "2026-11-26T00:00:01Z".into(),
                metadata: None,
                target_id: "e2".into(),
                replacement: None,
            },
            runtime_ctx("e4", "latest ctx"),
            tool_res("e5", "c1", &"x".repeat(500)),
            tool_res("e6", "c2", &"y".repeat(500)),
        ];

        let patched = vec![KernelContent::Text {
            text: "patched ctx".into(),
        }];
        let ephemeral = vec![KernelContent::Text {
            text: "ephemeral run ctx".into(),
        }];

        let projected = build_model_view_messages(
            &branch,
            &ProjectionParams {
                system_prompt: "sys",
                active_protocol: ProviderProtocol::OpenAiResponses,
                patched_runtime_context: Some(&patched),
                run_runtime_context: Some(&ephemeral),
                prune_boundary_call_id: Some("c2"),
                prune_min_length: 100,
                prune_keep_length: 20,
                tool_result_notes: Some(&notes),
            },
        )
        .unwrap();

        // Stored entries remain completely intact.
        assert_eq!(branch.len(), 6);
        assert!(matches!(
            &branch[4],
            SessionEntry::ToolResult { content, .. }
            if matches!(&content[0], KernelContent::Text { text } if text.len() == 500)
        ));

        // 1. Deleted user msg removed; 2. only 1 historical codaRuntimeContext kept & patched;
        // 3. c1 pruned to 20 chars + notice, c2 NOT pruned; 4. c2 gets modelNote; 5. ephemeral appended at tail.
        let texts: Vec<(String, String)> = projected
            .iter()
            .map(|m| (m.role_name().to_owned(), m.text_content()))
            .collect();
        assert_eq!(texts[0], ("system".into(), "sys".into()));
        assert_eq!(texts[1], ("custom".into(), "patched ctx".into()));
        assert_eq!(texts[2].0, "tool");
        assert!(texts[2].1.starts_with(&"x".repeat(20)));
        assert!(texts[2].1.contains("pruned 480 chars"));
        assert_eq!(texts[3].0, "tool");
        assert!(texts[3].1.starts_with(&"y".repeat(500)));
        assert!(texts[3].1.ends_with("\n\nstep 2 progress"));
        assert_eq!(texts[4], ("custom".into(), "ephemeral run ctx".into()));
    }

    #[test]
    fn responses_hydration_isolates_prior_anthropic_signature_and_promotes_responses_signature() {
        let mut resp_meta = Map::new();
        resp_meta.insert("protocol".into(), json!("openai-responses"));
        let branch = vec![
            SessionEntry::Model {
                id: "m1".into(),
                parent_id: None,
                timestamp: "2026-11-26T00:00:00Z".into(),
                metadata: None,
                model: KernelModel {
                    model_id: "claude-sonnet-4-5".into(),
                    custom_provider_id: None,
                },
            },
            SessionEntry::Message {
                id: "a1".into(),
                parent_id: Some("m1".into()),
                timestamp: "2026-11-26T00:00:01Z".into(),
                metadata: None,
                role: SessionMessageRole::Assistant,
                content: vec![SessionBlock::Thinking {
                    text: "anth".into(),
                    signature: Some("anth-sig".into()),
                    redacted: None,
                }],
            },
            SessionEntry::Model {
                id: "m2".into(),
                parent_id: Some("a1".into()),
                timestamp: "2026-11-26T00:00:02Z".into(),
                metadata: Some(resp_meta),
                model: KernelModel {
                    model_id: "gpt-5.2".into(),
                    custom_provider_id: None,
                },
            },
            SessionEntry::Message {
                id: "a2".into(),
                parent_id: Some("m2".into()),
                timestamp: "2026-11-26T00:00:03Z".into(),
                metadata: None,
                role: SessionMessageRole::Assistant,
                content: vec![SessionBlock::Thinking {
                    text: "resp".into(),
                    signature: Some("resp-enc".into()),
                    redacted: None,
                }],
            },
        ];

        let projected = build_model_view_messages(
            &branch,
            &ProjectionParams {
                system_prompt: "",
                active_protocol: ProviderProtocol::OpenAiResponses,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(projected.len(), 2);
        assert_eq!(
            projected[0].content[0],
            ContentBlock::Thinking {
                text: "anth".into(),
                signature: Some("anth-sig".into()),
                encrypted_content: None,
            }
        );
        assert_eq!(
            projected[1].content[0],
            ContentBlock::Thinking {
                text: "resp".into(),
                signature: None,
                encrypted_content: Some("resp-enc".into()),
            }
        );
    }

    #[test]
    fn compaction_slice_injects_summary_and_keeps_from_first_kept_entry_id() {
        let branch = vec![
            text_msg("u0", SessionMessageRole::User, "ancient"),
            text_msg("s0", SessionMessageRole::System, "dropped system"),
            text_msg("u1", SessionMessageRole::User, "kept turn"),
            SessionEntry::Compaction {
                id: "c1".into(),
                parent_id: Some("u1".into()),
                timestamp: "2026-11-26T00:00:02Z".into(),
                metadata: None,
                summary: "concise summary".into(),
                first_kept_entry_id: "u1".into(),
                tokens_before: 100,
            },
            text_msg("u2", SessionMessageRole::User, "after compact"),
        ];
        let projected = build_model_view_messages(
            &branch,
            &ProjectionParams {
                system_prompt: "sys",
                active_protocol: ProviderProtocol::OpenAi,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(projected.len(), 4);
        assert_eq!(projected[0].text_content(), "sys");
        assert!(projected[1].text_content().contains("concise summary"));
        assert_eq!(projected[2].text_content(), "kept turn");
        assert_eq!(projected[3].text_content(), "after compact");
    }

    #[test]
    fn lossy_session_block_degrades_non_object_tool_args() {
        let block = ContentBlock::ToolCall(crate::messages::ToolCallBlock {
            id: "c1".into(),
            name: "read".into(),
            arguments: "[1, 2, 3]".into(),
            thought_signature: None,
            namespace: None,
        });
        assert!(SessionBlock::from_content_block(&block).is_err());
        let lossy = SessionBlock::from_content_block_lossy(&block).unwrap();
        assert!(matches!(lossy, SessionBlock::ToolCall { args, .. } if args.is_empty()));
    }

    #[test]
    fn run_runtime_context_replaces_latest_persisted_runtime_context_in_place() {
        let branch = vec![
            runtime_ctx("c0", "<system-reminder>ROLE=OLD</system-reminder>"),
            runtime_ctx(
                "c1",
                "<system-reminder>ROLE=OWNER_FULL_TOOLS</system-reminder>",
            ),
            text_msg("u1", SessionMessageRole::User, "Ask Alice and Bob"),
            text_msg("a1", SessionMessageRole::Assistant, "Answered"),
            text_msg("u2", SessionMessageRole::User, "Guest query"),
        ];
        let run_ctx = vec![KernelContent::Text {
            text: "<system-reminder>ROLE=RESTRICTED_GUEST</system-reminder>".into(),
        }];

        let projected = build_model_view_messages(
            &branch,
            &ProjectionParams {
                system_prompt: "sys",
                active_protocol: ProviderProtocol::OpenAiResponses,
                patched_runtime_context: None,
                run_runtime_context: Some(&run_ctx),
                ..Default::default()
            },
        )
        .unwrap();

        let texts: Vec<(String, String)> = projected
            .iter()
            .map(|m| (m.role_name().to_owned(), m.text_content()))
            .collect();
        assert_eq!(
            texts,
            vec![
                ("system".into(), "sys".into()),
                (
                    "custom".into(),
                    "<system-reminder>ROLE=RESTRICTED_GUEST</system-reminder>".into()
                ),
                ("user".into(), "Ask Alice and Bob".into()),
                ("assistant".into(), "Answered".into()),
                ("user".into(), "Guest query".into()),
            ]
        );
    }

    #[test]
    fn run_runtime_context_appends_at_tail_when_no_persisted_runtime_context_exists() {
        let branch = vec![text_msg("u1", SessionMessageRole::User, "hello")];
        let run_ctx = vec![KernelContent::Text {
            text: "<system-reminder>EPHEMERAL</system-reminder>".into(),
        }];
        let projected = build_model_view_messages(
            &branch,
            &ProjectionParams {
                system_prompt: "sys",
                active_protocol: ProviderProtocol::OpenAiResponses,
                patched_runtime_context: None,
                run_runtime_context: Some(&run_ctx),
                ..Default::default()
            },
        )
        .unwrap();
        let texts: Vec<(String, String)> = projected
            .iter()
            .map(|m| (m.role_name().to_owned(), m.text_content()))
            .collect();
        assert_eq!(
            texts,
            vec![
                ("system".into(), "sys".into()),
                ("user".into(), "hello".into()),
                (
                    "custom".into(),
                    "<system-reminder>EPHEMERAL</system-reminder>".into()
                ),
            ]
        );
    }
}
