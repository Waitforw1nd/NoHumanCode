use super::{partial::PartialReply, *};

pub(super) enum Failure {
    Aborted,
    Failed(&'static str),
}
use Failure::{Aborted, Failed};
type TurnResult<T> = std::result::Result<T, Failure>;

// Producers never wait on an observer. Bound the total per-turn data, including tiny events.
const EVENT_BUDGET: usize = 64 * 1024 * 1024;
pub(super) struct Events<'a> {
    channel: &'a mpsc::UnboundedSender<Event>,
    bytes: Mutex<usize>,
}
impl<'a> Events<'a> {
    pub(super) fn new(channel: &'a mpsc::UnboundedSender<Event>) -> Self {
        Self {
            channel,
            bytes: Mutex::new(0),
        }
    }
    fn check_budget(&self) -> TurnResult<()> {
        if *self.bytes.lock().expect("event budget poisoned") > EVENT_BUDGET {
            Err(Failed("EVENT_LIMIT"))
        } else {
            Ok(())
        }
    }
    fn send(&self, event: Event) {
        let mut total = self.bytes.lock().expect("event budget poisoned");
        if *total > EVENT_BUDGET {
            return;
        }
        let len = serde_json::to_vec(&event).map_or(EVENT_BUDGET, |bytes| bytes.len());
        *total = total.saturating_add(len).saturating_add(128);
        if *total <= EVENT_BUDGET {
            let _ = self.channel.send(event);
        }
    }
}

fn running(cancel: &CancellationToken) -> TurnResult<()> {
    if cancel.is_cancelled() {
        Err(Aborted)
    } else {
        Ok(())
    }
}

/// Reject ambiguous, repeated or incomplete intents before admission of new side effects.
pub(super) fn validate_history(history: &[Message]) -> TurnResult<HashSet<String>> {
    let mut seen = HashSet::new();
    let mut pending = HashMap::new();
    for message in history {
        match message {
            Message::Tool { call_id, name, .. } => {
                if pending.remove(call_id) != Some(name) {
                    return Err(Failed("INVALID_TOOL_HISTORY"));
                }
            }
            _ if !pending.is_empty() => return Err(Failed("UNFINISHED_TOOL_HISTORY")),
            Message::Assistant { blocks } => {
                for block in blocks {
                    if let Block::ToolCall { call } = block {
                        if call.id.trim().is_empty()
                            || call.name.trim().is_empty()
                            || !seen.insert(call.id.clone())
                        {
                            return Err(Failed("INVALID_TOOL_HISTORY"));
                        }
                        pending.insert(&call.id, &call.name);
                    }
                }
            }
            _ => (),
        }
    }
    if pending.is_empty() {
        Ok(seen)
    } else {
        Err(Failed("UNFINISHED_TOOL_HISTORY"))
    }
}

fn projected(mut message: Message) -> Message {
    if let Message::Assistant { blocks } = &mut message {
        blocks.retain(|b| !matches!(b, Block::Thinking { .. }));
    }
    message
}
pub(super) fn model_projection(history: Vec<Message>) -> Vec<Message> {
    let mut messages = Vec::with_capacity(history.len());
    for message in history {
        if let Message::Aborted { .. } = message {
            let marker = Block::Text { text: "The user cancelled the previous task. Do not resume it unless the user explicitly asks again.".into() };
            // A late abort can arrive while a complete assistant message is being committed.
            if let Some(Message::Assistant { blocks }) = messages.last_mut() {
                blocks.push(marker);
            } else {
                messages.push(Message::Assistant {
                    blocks: vec![marker],
                });
            }
        } else {
            messages.push(projected(message));
        }
    }
    messages
}

impl EmbeddedEngine {
    pub(super) async fn turn(
        &self,
        opts: &RunOptions,
        events: &Events<'_>,
        cancel: &CancellationToken,
    ) -> TurnResult<()> {
        running(cancel)?;
        let snapshot = self
            .store
            .load(&opts.session_id)
            .await
            .map_err(|_| Failed("SESSION_LOAD"))?;
        let seen = validate_history(&snapshot.messages)?;
        let tools = self
            .tools
            .definitions(&opts.session_id, &opts.run_id)
            .map_err(|_| Failed("TOOL_DEFINITIONS"))?;
        let mut names = HashSet::new();
        if tools
            .iter()
            .any(|t| t.name.trim().is_empty() || !names.insert(&t.name))
        {
            return Err(Failed("INVALID_TOOL_DEFINITIONS"));
        }
        let user = Message::User {
            content: opts.content.clone(),
        };
        let mut request = ModelRequest {
            session_id: opts.session_id.clone(),
            run_id: Some(opts.run_id.clone()),
            settings: opts.settings.clone(),
            model: opts.model.clone(),
            system_prompt: opts.system_prompt.clone(),
            tools,
            messages: model_projection(snapshot.messages),
        };
        request.messages.push(user.clone());
        let mut turn = Turn {
            engine: self,
            opts,
            revision: snapshot.revision,
            user_saved: false,
            partial: Mutex::default(),
            events,
            cancel,
        };
        let result = turn.drive(request, user, seen).await;
        if matches!(result, Ok(()) | Err(Aborted)) && self.close_abort_window(&opts.session_id) {
            if turn.user_saved {
                let partial =
                    std::mem::take(turn.partial.get_mut().expect("partial reply poisoned"))
                        .into_blocks();
                turn.save(&[Entry::Message(Message::Aborted {
                    run_id: opts.run_id.clone(),
                    partial,
                })])
                .await?;
            }
            return Err(Aborted);
        }
        result
    }
}

impl Turn<'_> {
    async fn drive(
        &mut self,
        mut request: ModelRequest,
        user: Message,
        mut seen: HashSet<String>,
    ) -> TurnResult<()> {
        let cancel = self.cancel;
        let events = self.events;
        let mut rounds = crate::model_loop::ModelLoop::new(self.opts.max_requests);
        loop {
            let round = rounds.next(cancel).map_err(|reason| match reason {
                crate::model_loop::Stop::Cancelled => Aborted,
                crate::model_loop::Stop::Limit => Failed("REQUEST_LIMIT"),
            })?;
            self.engine
                .model
                .preflight(&request)
                .map_err(|_| Failed("MODEL_PREFLIGHT"))?;
            running(cancel)?;
            if round == 0 {
                self.save(&[Entry::Message(user.clone())]).await?;
                self.user_saved = true;
            }
            running(cancel)?;
            let emit = |delta| {
                anyhow::ensure!(!cancel.is_cancelled(), "cancelled");
                // Serialize concurrent emits so the archive follows the same order as the stream.
                let mut draft = self.partial.lock().expect("partial reply poisoned");
                let event = match &delta {
                    ModelDelta::Text { block_id, delta } => Event::Text {
                        block_id: block_id.clone(),
                        delta: delta.clone(),
                    },
                    ModelDelta::Thinking { block_id, delta } => Event::Thinking {
                        block_id: block_id.clone(),
                        delta: delta.clone(),
                    },
                };
                events.send(event);
                events
                    .check_budget()
                    .map_err(|_| anyhow::anyhow!("event budget exceeded"))?;
                draft.push(&delta);
                Ok(())
            };
            let reply = self
                .engine
                .model
                .complete(&request, &emit, cancel.child_token())
                .await;
            running(cancel)?;
            events.check_budget()?;
            let reply = reply.map_err(|_| Failed("MODEL_COMPLETE"))?;
            let calls = validate_reply(&reply, &request.tools, &mut seen)?;
            let assistant = Message::Assistant {
                blocks: reply.blocks,
            };
            let mut entries = vec![Entry::Message(assistant.clone())];
            if let Some(usage) = reply.usage {
                entries.push(Entry::Usage(usage));
            }
            self.save(&entries).await?;
            *self.partial.get_mut().expect("partial reply poisoned") = PartialReply::default();
            request.messages.push(projected(assistant));
            if let Some(usage) = reply.usage {
                events.send(Event::Usage { usage });
            }
            if calls.is_empty() {
                running(cancel)?;
                return events.check_budget();
            }
            let failure = (!rounds.can_continue()).then_some("REQUEST_LIMIT");
            self.dispatch(calls, failure, &mut request.messages).await?;
        }
    }
}

fn validate_reply(
    reply: &ModelReply,
    tools: &[ToolDefinition],
    seen: &mut HashSet<String>,
) -> TurnResult<Vec<ToolCall>> {
    let mut calls = Vec::new();
    for block in &reply.blocks {
        if let Block::ToolCall { call } = block {
            if call.id.trim().is_empty()
                || !seen.insert(call.id.clone())
                || !tools.iter().any(|t| t.name == call.name)
            {
                return Err(Failed("INVALID_TOOL_CALL"));
            }
            calls.push(call.clone());
        }
    }
    Ok(calls)
}

struct Turn<'a> {
    engine: &'a EmbeddedEngine,
    opts: &'a RunOptions,
    revision: String,
    user_saved: bool,
    partial: Mutex<PartialReply>,
    events: &'a Events<'a>,
    cancel: &'a CancellationToken,
}
impl Turn<'_> {
    async fn save(&mut self, entries: &[Entry]) -> TurnResult<()> {
        self.revision = self
            .engine
            .store
            .append(&self.opts.session_id, &self.revision, entries)
            .await
            .map_err(|_| Failed("SESSION_APPEND"))?;
        Ok(())
    }
    async fn dispatch(
        &mut self,
        calls: Vec<ToolCall>,
        mut failure: Option<&'static str>,
        messages: &mut Vec<Message>,
    ) -> TurnResult<()> {
        for call in calls {
            self.events.send(Event::ToolCall { call: call.clone() });
            if self.events.check_budget().is_err() {
                failure.get_or_insert("EVENT_LIMIT");
            }
            let invocation = Invocation {
                session_id: self.opts.session_id.clone(),
                run_id: self.opts.run_id.clone(),
                call,
            };
            let result = if self.cancel.is_cancelled() {
                Err("ABORTED")
            } else if failure.is_some() {
                Err("TOOL_NOT_EXECUTED")
            } else {
                self.invoke(&invocation).await
            };
            let result = result.unwrap_or_else(|code| {
                failure.get_or_insert(code);
                ToolResult {
                    content: vec![Content::Text { text: code.into() }],
                    details: serde_json::Value::Null,
                    is_error: true,
                }
            });
            if result.is_error {
                failure.get_or_insert("TOOL_FAILED");
            }
            let id = invocation.call.id;
            let message = Message::Tool {
                call_id: id.clone(),
                name: invocation.call.name,
                result: result.clone(),
            };
            self.save(&[Entry::Message(message.clone())]).await?;
            messages.push(message);
            self.events.send(Event::ToolResult {
                call_id: id,
                result,
            });
        }
        running(self.cancel)?;
        self.events.check_budget()?;
        failure.map_or(Ok(()), |code| Err(Failed(code)))
    }
    async fn invoke(&self, call: &Invocation) -> std::result::Result<ToolResult, &'static str> {
        let approved = self
            .engine
            .tools
            .review(call, self.cancel.child_token())
            .await;
        if self.cancel.is_cancelled() {
            return Err("ABORTED");
        }
        let approved = approved.map_err(|_| "TOOL_REVIEW")?;
        if !approved {
            return Err("TOOL_DENIED");
        }
        let update = |result| {
            anyhow::ensure!(!self.cancel.is_cancelled(), "cancelled");
            self.events.send(Event::ToolUpdate {
                call_id: call.call.id.clone(),
                result,
            });
            self.events
                .check_budget()
                .map_err(|_| anyhow::anyhow!("event budget exceeded"))
        };
        self.engine
            .tools
            .execute(call, &update, self.cancel.child_token())
            .await
            .map_err(|_| {
                if self.cancel.is_cancelled() {
                    "ABORTED"
                } else {
                    "TOOL_EXECUTION"
                }
            })
    }
}
