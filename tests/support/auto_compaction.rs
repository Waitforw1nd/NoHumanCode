use super::fixture::*;
use anyhow::Result;
use coda::embedded::*;
use std::sync::{Arc, Mutex};

const TOKENS: u64 = u32::MAX as u64 + 17;
const INSTRUCTIONS: &str = "Keep the lookup outcome.";
const GENERATION_USAGE: Usage = Usage {
    input: 23,
    output: 7,
    cache_read: 3,
    cache_write: 1,
};
const SUMMARY_USAGE: Usage = Usage {
    input: 47,
    output: 13,
    cache_read: 5,
    cache_write: 2,
};

fn user(value: &str) -> Message {
    Message::User {
        content: vec![text(value)],
    }
}

#[derive(Default)]
struct HostStore {
    inner: Store,
    batches: Mutex<Vec<Vec<Entry>>>,
}

impl SessionStore for HostStore {
    fn load<'a>(&'a self, session: &'a str) -> BoxFuture<'a, Result<Snapshot>> {
        Box::pin(async move {
            let mut snapshot = self.inner.load(session).await?;
            // Only a real reload exposes the host's selected branch projection.
            for message in &mut snapshot.messages {
                if let Message::Summary { text } = message {
                    *text = format!("host projection of {text}");
                }
            }
            Ok(snapshot)
        })
    }

    fn append<'a>(
        &'a self,
        session: &'a str,
        revision: &'a str,
        entries: &'a [Entry],
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let next = self.inner.append(session, revision, entries).await?;
            self.batches.lock().unwrap().push(entries.to_vec());
            Ok(next)
        })
    }
}

struct HostTools(Arc<HostStore>, Tools);

impl ToolHost for HostTools {
    fn definitions(&self, session: &str, run: &str) -> Result<Vec<ToolDefinition>> {
        let mut tools = self.1.definitions(session, run)?;
        let summaries = self
            .0
            .inner
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Entry::Compaction { .. }))
            .count();
        tools[0].description = format!("lookup version {summaries}");
        Ok(tools)
    }

    fn review<'a>(
        &'a self,
        call: &'a Invocation,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<bool>> {
        self.1.review(call, cancel)
    }

    fn execute<'a>(
        &'a self,
        call: &'a Invocation,
        update: &'a (dyn Fn(ToolResult) -> Result<()> + Send + Sync),
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolResult>> {
        self.1.execute(call, update, cancel)
    }
}

struct ModelProbe {
    store: Arc<HostStore>,
    decisions: Mutex<Vec<(ModelRequest, Vec<Entry>)>>,
    order: Mutex<Vec<&'static str>>,
    generation: Transport,
    summary: Transport,
}

impl ModelTransport for ModelProbe {
    fn before_request<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: CancellationToken,
    ) -> BoxFuture<'a, Result<CompactionDecision>> {
        Box::pin(async move {
            self.order.lock().unwrap().push("before");
            let mut seen = self.decisions.lock().unwrap();
            seen.push((
                request.clone(),
                self.store.inner.entries.lock().unwrap().clone(),
            ));
            Ok(CompactionDecision::Compact {
                instructions: INSTRUCTIONS.into(),
                tokens_before: TOKENS + seen.len() as u64,
            })
        })
    }

    fn preflight(&self, request: &ModelRequest) -> Result<()> {
        if request.purpose == RequestPurpose::Generation {
            self.order.lock().unwrap().push("generation preflight");
        }
        Ok(())
    }

    fn complete<'a>(
        &'a self,
        request: &'a ModelRequest,
        emit: &'a (dyn Fn(ModelDelta) -> Result<()> + Send + Sync),
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ModelReply>> {
        match request.purpose {
            RequestPurpose::Generation => self.generation.complete(request, emit, cancel),
            RequestPurpose::Compaction => self.summary.complete(request, emit, cancel),
        }
    }
}

fn summary_projection(round: usize) -> Message {
    Message::Summary {
        text: format!("host projection of summary {round}"),
    }
}

fn assert_event_order(events: &[Event], steer: &SteeringMessage, note: &SteeringMessage) {
    let order: Vec<_> = events
        .iter()
        .map(|event| match event {
            Event::Start { .. } => "start",
            Event::CompactionStart { tokens_before } if *tokens_before == TOKENS + 1 => "start 1",
            Event::CompactionStart { tokens_before } if *tokens_before == TOKENS + 2 => "start 2",
            Event::CompactionEnd {
                reason: DoneReason::Completed,
            } => "end",
            Event::InputDelivered { message } if message == steer => "input",
            Event::InputDelivered { message } if message == note => "custom",
            Event::ToolCall { .. } => "call",
            Event::ToolResult { .. } => "result",
            Event::Text { delta, .. } => delta,
            Event::Done {
                reason: DoneReason::Completed,
            } => "done",
            Event::Usage { usage } if *usage == GENERATION_USAGE => "usage",
            _ => panic!("unexpected event {event:?}"),
        })
        .collect();
    assert_eq!(
        order,
        [
            "start", "start 1", "end", "input", "usage", "call", "result", "start 2", "end",
            "custom", "finished", "usage", "done"
        ]
    );
}

fn assert_usage_batch(store: &HostStore, entry: Entry, usage: Usage) {
    let batches = store.batches.lock().unwrap();
    let batch = batches
        .iter()
        .find(|batch| batch.contains(&entry))
        .expect("missing entry");
    assert_eq!(batch.len(), 2);
    assert!(batch.contains(&Entry::Usage(usage)));
}

#[tokio::test]
async fn empty_history_preflights_the_first_question_without_summarizing_it() -> Result<()> {
    let store = Arc::new(HostStore::default());
    let probe = Arc::new(ModelProbe {
        store: store.clone(),
        decisions: Default::default(),
        order: Default::default(),
        generation: scripted(vec![vec![answer("reply")]]),
        summary: Transport::default(),
    });
    let engine = EmbeddedEngine::new(probe.clone(), Arc::new(Tools::default()), store);
    let events = collect(engine.run(options())?).await;
    assert_eq!(
        events.last(),
        Some(&Event::Done {
            reason: DoneReason::Completed
        })
    );
    assert!(!events.iter().any(|e| matches!(
        e,
        Event::CompactionStart { .. } | Event::CompactionEnd { .. }
    )));
    assert_eq!(
        *probe.order.lock().unwrap(),
        ["before", "generation preflight"]
    );
    assert!(probe.summary.requests.lock().unwrap().is_empty());
    assert_eq!(
        probe.generation.requests.lock().unwrap()[0].messages,
        [user("question")]
    );
    Ok(())
}

#[tokio::test]
async fn compacts_committed_history_at_initial_and_tool_boundaries() -> Result<()> {
    let old = user("earlier history");
    let (engine, probe) = engine_with_tool_round(old.clone()).await?;
    let manual = explicit_summary_request(old.clone()).await?;
    let custom = drive_tool_round(&engine, &probe).await?;
    assert_boundary_requests(&probe, old, &custom, &manual);
    assert_archived_turn(&probe.store, custom);
    Ok(())
}

async fn engine_with_tool_round(old: Message) -> Result<(EmbeddedEngine, Arc<ModelProbe>)> {
    let store = Arc::new(HostStore::default());
    store.append("session", "0", &[Entry::Message(old)]).await?;
    let probe = Arc::new(ModelProbe {
        store: store.clone(),
        decisions: Default::default(),
        order: Default::default(),
        generation: Transport {
            pending: true,
            usage: Some(GENERATION_USAGE),
            ..scripted(vec![vec![call("first")], vec![answer("finished")]])
        },
        summary: Transport {
            usage: Some(SUMMARY_USAGE),
            ..scripted(vec![vec![answer("summary 1")], vec![answer("summary 2")]])
        },
    });
    let engine = EmbeddedEngine::new(
        probe.clone(),
        Arc::new(HostTools(store.clone(), Tools::default())),
        store,
    );
    Ok((engine, probe))
}

async fn explicit_summary_request(old: Message) -> Result<ModelRequest> {
    let manual = Fixture::new(scripted(vec![vec![answer("summary")]]), Store::default());
    manual
        .store
        .append("session", "0", &[Entry::Message(old)])
        .await?;
    manual
        .engine
        .compact("session", model(), INSTRUCTIONS)
        .await?;
    Ok(manual.transport.requests.lock().unwrap()[0].clone())
}

async fn drive_tool_round(engine: &EmbeddedEngine, probe: &ModelProbe) -> Result<CustomMessage> {
    let mut opts = options();
    opts.settings.thinking_level = Some("medium".into());
    opts.settings.context_window = Some(4096);
    opts.settings.max_output_tokens = Some(101);
    opts.max_requests = 2;
    // The current-thread runtime cannot poll the spawned turn until the first await.
    let turn = engine.run_with_input(opts)?;
    let steer = SteeringMessage::User {
        content: vec![text("clarification")],
    };
    turn.input.enqueue(steer.clone()).unwrap();
    let custom = CustomMessage {
        custom_type: "host.followup".into(),
        display: false,
        details: Some(serde_json::json!({"origin": "private metadata"})),
        content: vec![text("include the citation"), text("and the date")],
    };
    let custom_steer = SteeringMessage::Custom(custom.clone());
    timeout(LIMIT, probe.generation.started.notified()).await?;
    turn.input.enqueue(custom_steer.clone()).unwrap();
    probe.generation.release.notify_one();
    timeout(LIMIT, probe.generation.started.notified()).await?;
    probe.generation.release.notify_one();
    let events = collect(turn.events).await;
    assert_event_order(&events, &steer, &custom_steer);
    assert_eq!(
        *probe.order.lock().unwrap(),
        [
            "before",
            "generation preflight",
            "before",
            "generation preflight"
        ]
    );

    Ok(custom)
}

fn settings(s: &ModelSettings) -> (Option<usize>, Option<usize>, Option<&str>) {
    (
        s.context_window,
        s.max_output_tokens,
        s.thinking_level.as_deref(),
    )
}

fn assert_generation_candidate(candidate: &ModelRequest) {
    assert_eq!(candidate.purpose, RequestPurpose::Generation);
    assert_eq!(candidate.session_id, "session");
    assert_eq!(candidate.run_id.as_deref(), Some("run"));
    assert_eq!(candidate.model, model());
    assert_eq!(candidate.system_prompt, "rules");
    assert_eq!(candidate.settings.thinking_level.as_deref(), Some("medium"));
    assert_eq!(candidate.settings.context_window, Some(4096));
    assert_eq!(candidate.settings.max_output_tokens, Some(101));
}

fn assert_summary_request(
    summary: &ModelRequest,
    manual: &ModelRequest,
    candidate_settings: &ModelSettings,
    committed: &[Message],
) {
    assert_eq!(settings(&summary.settings), settings(candidate_settings));
    assert_eq!(summary.run_id.as_deref(), Some("run"));
    assert_eq!(summary.purpose, RequestPurpose::Compaction);
    assert_eq!(summary.system_prompt, manual.system_prompt);
    assert!(summary.tools.is_empty());
    let (instruction, history) = summary.messages.split_last().unwrap();
    assert_eq!(Some(instruction), manual.messages.last());
    assert!(
        matches!(instruction, Message::User { content } if matches!(content.as_slice(), [Content::Text { text }] if text.contains(INSTRUCTIONS)))
    );
    assert_eq!(history, committed);
}

fn tool_intent() -> Message {
    Message::Assistant {
        blocks: vec![call("first")],
    }
}

fn assert_boundary_requests(
    probe: &ModelProbe,
    old: Message,
    custom: &CustomMessage,
    manual: &ModelRequest,
) {
    let inputs = [user("question"), user("clarification")];
    let intent = tool_intent();
    let result = Message::Tool {
        call_id: "first".into(),
        name: "lookup".into(),
        result: ToolResult {
            is_error: false,
            details: Default::default(),
            content: vec![text("found")],
        },
    };
    let initial = [vec![old.clone()], inputs.to_vec()].concat();
    let regenerated = [vec![summary_projection(1)], inputs.to_vec()].concat();
    let continuation = [regenerated.clone(), vec![intent.clone(), result.clone()]].concat();
    let body = Message::User {
        content: custom.content.clone(),
    };
    let next_candidate = [continuation.clone(), vec![body.clone()]].concat();
    let decisions = probe.decisions.lock().unwrap();
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0].1, [Entry::Message(old.clone())]);
    assert!(decisions[1].1.ends_with(&[Entry::Message(result)]));
    assert!(decisions[1].1.contains(&Entry::Message(intent.clone())));
    assert!(
        !decisions[1]
            .1
            .contains(&Entry::Message(Message::Custom(custom.clone())))
    );
    let generations = probe.generation.requests.lock().unwrap();
    let summaries = probe.summary.requests.lock().unwrap();
    assert_eq!((generations.len(), summaries.len()), (2, 2));
    assert_eq!(
        settings(&manual.settings),
        settings(&ModelSettings::default())
    );
    for (i, (candidate, _)) in decisions.iter().enumerate() {
        assert_generation_candidate(candidate);
        assert_eq!(
            candidate.messages,
            [&initial, &next_candidate][i].as_slice()
        );
        for (request, version) in [(candidate, i), (&generations[i], i + 1)] {
            assert!(
                matches!(request.tools.as_slice(), [tool] if tool.name == "lookup" && tool.description == format!("lookup version {version}"))
            );
        }
        let committed = [std::slice::from_ref(&old), continuation.as_slice()][i];
        assert_summary_request(&summaries[i], manual, &candidate.settings, committed);
    }
    assert_eq!(generations[0].messages, regenerated);
    assert_eq!(generations[1].messages, [summary_projection(2), body]);
}

fn assert_archived_turn(store: &HostStore, custom: CustomMessage) {
    for round in 1..=2 {
        let entry = Entry::Compaction {
            summary: format!("summary {round}"),
            tokens_before: Some(TOKENS + round),
        };
        assert_usage_batch(store, entry, SUMMARY_USAGE);
    }
    assert_usage_batch(store, Entry::Message(tool_intent()), GENERATION_USAGE);
    assert_usage_batch(
        store,
        Entry::Message(Message::Assistant {
            blocks: vec![answer("finished")],
        }),
        GENERATION_USAGE,
    );
    let archive = store.inner.entries.lock().unwrap();
    for input in [user("question"), user("clarification")]
        .into_iter()
        .chain([Message::Custom(custom)])
        .map(Entry::Message)
    {
        assert_eq!(archive.iter().filter(|entry| **entry == input).count(), 1);
    }
    for usage in [GENERATION_USAGE, SUMMARY_USAGE] {
        assert_eq!(
            archive
                .iter()
                .filter(|entry| **entry == Entry::Usage(usage))
                .count(),
            2
        );
    }
}
