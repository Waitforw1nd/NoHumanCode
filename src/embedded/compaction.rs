use super::*;

pub(super) struct Summary<'a> {
    pub session_id: &'a str,
    pub run_id: Option<&'a str>,
    pub model: &'a Model,
    pub settings: ModelSettings,
    pub revision: &'a str,
    pub messages: Vec<Message>,
    pub instructions: &'a str,
    pub tokens_before: Option<u64>,
}

/// Preserve the host's diagnostic for explicit compact and a stable event code for runs.
#[derive(Debug)]
pub(super) struct SummaryError(pub &'static str, pub anyhow::Error);

fn running(cancel: &CancellationToken) -> Result<(), SummaryError> {
    if cancel.is_cancelled() {
        return Err(SummaryError(
            "ABORTED",
            anyhow::anyhow!("compaction cancelled"),
        ));
    }
    Ok(())
}

fn summary_text(blocks: Vec<Block>) -> Result<String> {
    let mut summary = String::new();
    for block in blocks {
        match block {
            Block::Text { text } => summary.push_str(&text),
            Block::Thinking { .. } => (),
            Block::ToolCall { .. } | Block::Image { .. } => {
                anyhow::bail!("expected a text summary")
            }
        }
    }
    ensure!(!summary.trim().is_empty(), "empty compaction summary");
    Ok(summary)
}

pub(super) async fn append_summary(
    engine: &EmbeddedEngine,
    mut summary: Summary<'_>,
    cancel: &CancellationToken,
) -> Result<String, SummaryError> {
    running(cancel)?;
    let prompt = format!(
        "Summarize this conversation for continuation. Preserve decisions, tool outcomes and remaining work.\n{}",
        summary.instructions
    );
    summary.messages.push(Message::User {
        content: vec![Content::Text { text: prompt }],
    });
    let request = ModelRequest {
        purpose: RequestPurpose::Compaction,
        session_id: summary.session_id.to_owned(),
        run_id: summary.run_id.map(str::to_owned),
        settings: summary.settings,
        model: summary.model.clone(),
        system_prompt: "Produce a factual conversation summary.".into(),
        messages: summary.messages,
        tools: vec![],
    };
    let preflight = engine.model.preflight(&request);
    running(cancel)?;
    preflight.map_err(|e| SummaryError("COMPACTION_PREFLIGHT", e))?;
    let reply = engine
        .model
        .complete(&request, &|_| Ok(()), cancel.child_token())
        .await;
    running(cancel)?;
    let reply = reply.map_err(|e| SummaryError("COMPACTION_MODEL", e))?;
    let text = summary_text(reply.blocks).map_err(|e| SummaryError("COMPACTION_SUMMARY", e))?;
    let mut entries = Vec::new();
    if let Some(usage) = reply.usage {
        entries.push(Entry::Usage(usage));
    }
    entries.push(Entry::Compaction {
        summary: text,
        tokens_before: summary.tokens_before,
    });
    running(cancel)?;
    // Always return the actual CAS outcome, including when cancellation races the write.
    engine
        .store
        .append(summary.session_id, summary.revision, &entries)
        .await
        .map_err(|e| SummaryError("SESSION_APPEND", e))
}
