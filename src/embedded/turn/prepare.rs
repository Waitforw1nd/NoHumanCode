use super::*;
use crate::embedded::compaction::{Summary, SummaryError, append_summary};

impl Turn<'_> {
    pub(super) async fn prepare_request(
        &mut self,
        request: &mut ModelRequest,
        user: &Message,
        batch: input::InputBatch<'_>,
        seen: &mut HashSet<String>,
    ) -> TurnResult<()> {
        request.tools = self.definitions()?;
        let committed_len = request.messages.len();
        let mut entries = Vec::new();
        if !self.user_saved {
            request.messages.push(user.clone());
            entries.push(Entry::Message(user.clone()));
        }
        for message in &batch.messages {
            let archived = Message::from(message.clone());
            request.messages.push(projected(archived.clone()));
            entries.push(Entry::Message(archived));
            self.events.reserve(&Event::InputDelivered {
                message: message.clone(),
            })?;
        }

        let decision = self
            .engine
            .model
            .before_request(request, self.cancel.child_token())
            .await;
        running(self.cancel)?;
        if let CompactionDecision::Compact {
            instructions,
            tokens_before,
        } = decision.map_err(|_| Failed("COMPACTION_DECISION"))?
            && committed_len > 0
        {
            let pending = request.messages.split_off(committed_len);
            let history = std::mem::take(&mut request.messages);
            request.messages = self
                .compact_history(history, &instructions, tokens_before, seen)
                .await?;
            request.messages.extend(pending);
            request.tools = self.definitions()?;
        }
        let preflight = self.engine.model.preflight(request);
        running(self.cancel)?;
        preflight.map_err(|_| Failed("MODEL_PREFLIGHT"))?;
        self.commit_input(entries, batch).await
    }

    fn definitions(&self) -> TurnResult<Vec<ToolDefinition>> {
        let definitions = self
            .engine
            .tools
            .definitions(&self.opts.session_id, &self.opts.run_id);
        running(self.cancel)?;
        let tools = definitions.map_err(|_| Failed("TOOL_DEFINITIONS"))?;
        let mut names = HashSet::new();
        if tools
            .iter()
            .any(|t| t.name.trim().is_empty() || !names.insert(&t.name))
        {
            return Err(Failed("INVALID_TOOL_DEFINITIONS"));
        }
        Ok(tools)
    }

    async fn commit_input(
        &mut self,
        entries: Vec<Entry>,
        mut batch: input::InputBatch<'_>,
    ) -> TurnResult<()> {
        if !entries.is_empty() {
            batch.begin_append(&self.revision);
            if let Err(failure) = self.save(&entries).await {
                batch.not_committed();
                return Err(failure);
            }
            self.user_saved = true;
        }
        for message in batch.commit() {
            let _ = self.events.channel.send(Event::InputDelivered { message });
        }
        Ok(())
    }

    async fn compact_history(
        &mut self,
        history: Vec<Message>,
        instructions: &str,
        tokens_before: u64,
        seen: &mut HashSet<String>,
    ) -> TurnResult<Vec<Message>> {
        let mut events = CompactionEvents::start(self.events, tokens_before)?;
        let result = async {
            let summary = Summary {
                session_id: &self.opts.session_id,
                run_id: Some(&self.opts.run_id),
                model: &self.opts.model,
                settings: self.opts.settings.clone(),
                revision: &self.revision,
                messages: history,
                instructions,
                tokens_before: Some(tokens_before),
            };
            self.revision = append_summary(self.engine, summary, self.cancel)
                .await
                .map_err(|e| match e {
                    SummaryError("ABORTED", _) => Aborted,
                    SummaryError(code, _) => Failed(code),
                })?;
            running(self.cancel)?;
            let snapshot = self.engine.store.load(&self.opts.session_id).await;
            running(self.cancel)?;
            let snapshot = snapshot.map_err(|_| Failed("SESSION_LOAD"))?;
            if snapshot.revision != self.revision {
                return Err(Failed("SESSION_REVISION"));
            }
            seen.extend(validate_history(&snapshot.messages)?);
            Ok(model_projection(snapshot.messages))
        }
        .await;
        events.reason = match result {
            Ok(_) => DoneReason::Completed,
            Err(Aborted) => DoneReason::Aborted,
            Err(Failed(_)) => DoneReason::Failed,
        };
        result
    }
}

/// Reserve the pair before emitting Start. A panicking host still gets a matching End(Failed).
struct CompactionEvents<'a> {
    channel: &'a mpsc::UnboundedSender<Event>,
    reason: DoneReason,
}

impl<'a> CompactionEvents<'a> {
    fn start(events: &Events<'a>, tokens_before: u64) -> TurnResult<Self> {
        let start = Event::CompactionStart { tokens_before };
        events.reserve(&start)?;
        events.reserve(&Event::CompactionEnd {
            reason: DoneReason::Completed,
        })?;
        let _ = events.channel.send(start);
        Ok(Self {
            channel: events.channel,
            reason: DoneReason::Failed,
        })
    }
}

impl Drop for CompactionEvents<'_> {
    fn drop(&mut self) {
        let _ = self.channel.send(Event::CompactionEnd {
            reason: self.reason,
        });
    }
}
