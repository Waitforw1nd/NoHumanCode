use super::fixture::*;
use anyhow::Result;
use coda::embedded::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;

fn delivered(events: &[Event]) -> Vec<SteeringMessage> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::InputDelivered { message } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

#[derive(Default)]
struct ChangingTools {
    inner: Tools,
    first_started: Notify,
    resume_first: Notify,
    replaced: AtomicBool,
}

impl ToolHost for ChangingTools {
    fn definitions(&self, session: &str, run: &str) -> Result<Vec<ToolDefinition>> {
        let mut definitions = self.inner.definitions(session, run)?;
        if self.replaced.load(Ordering::SeqCst) {
            definitions[0].name = "replacement".into();
        }
        Ok(definitions)
    }

    fn review<'a>(
        &'a self,
        call: &'a Invocation,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<bool>> {
        self.inner.review(call, cancel)
    }

    fn execute<'a>(
        &'a self,
        call: &'a Invocation,
        update: &'a (dyn Fn(ToolResult) -> Result<()> + Send + Sync),
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            if call.call.id == "first" {
                self.first_started.notify_one();
                self.resume_first.notified().await;
            }
            self.inner.execute(call, update, cancel).await
        })
    }
}

#[tokio::test]
async fn tool_batch_finishes_before_fifo_delivery_and_next_request_refreshes_tools() -> Result<()> {
    let f = Fixture::new(
        scripted(vec![
            vec![call("first"), call("second")],
            vec![answer("final")],
        ]),
        Store::default(),
    );
    let tools = Arc::new(ChangingTools::default());
    let engine = EmbeddedEngine::new(f.transport.clone(), tools.clone(), f.store.clone());
    let turn = engine.run_with_input(options())?;
    timeout(LIMIT, tools.first_started.notified()).await?;
    let custom = CustomMessage {
        custom_type: "host.note".into(),
        content: vec![text("context"), text("more")],
        display: false,
        details: Some(serde_json::json!({"source": "host"})),
    };
    let batch = [
        user("clarification"),
        SteeringMessage::Custom(custom.clone()),
    ];
    for message in &batch {
        turn.input.enqueue(message.clone()).unwrap();
    }
    tools.replaced.store(true, Ordering::SeqCst);
    tools.resume_first.notify_one();
    let events = collect(turn.events).await;
    assert!(matches!(
        events.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    assert_eq!(delivered(&events), batch);
    let order: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult { call_id, result } => {
                assert!(!result.is_error);
                Some(call_id.as_str())
            }
            Event::InputDelivered { .. } => Some("input"),
            Event::Text { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["first", "second", "input", "input", "final"]);
    let saved = f.store.load("session").await?.messages;
    let [
        Message::User { .. },
        Message::Assistant { .. },
        Message::Tool { call_id: a, .. },
        Message::Tool { call_id: b, .. },
        Message::User { content },
        Message::Custom(note),
        Message::Assistant { blocks },
    ] = saved.as_slice()
    else {
        panic!("unexpected archive {saved:?}")
    };
    assert_eq!((a.as_str(), b.as_str()), ("first", "second"));
    assert_eq!(content, &vec![text("clarification")]);
    assert_eq!(note, &custom);
    assert_eq!(blocks, &vec![answer("final")]);
    let mut projected = saved[..5].to_vec();
    projected.push(Message::User {
        content: custom.content,
    });
    let requests = f.transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages, saved[..1]);
    assert_eq!(requests[1].messages, projected);
    for (request, name) in requests.iter().zip(["lookup", "replacement"]) {
        assert!(matches!(request.tools.as_slice(), [tool] if tool.name == name));
    }
    Ok(())
}

#[tokio::test]
async fn reloaded_custom_message_contributes_only_content_to_generation_and_compaction()
-> Result<()> {
    let custom = CustomMessage {
        custom_type: "private.host.type".into(),
        content: vec![text("tool report")],
        display: false,
        details: Some(serde_json::json!({"internal_id": 42})),
    };
    let f = Fixture::new(
        scripted(vec![vec![answer("reply")], vec![answer("summary")]]),
        Store::default(),
    );
    f.store
        .append(
            "session",
            "0",
            &[Entry::Message(Message::Custom(custom.clone()))],
        )
        .await?;
    let events = collect(f.engine.run(options())?).await;
    assert_eq!(
        events.last(),
        Some(&Event::Done {
            reason: DoneReason::Completed
        })
    );
    f.engine.compact("session", model(), "summarize").await?;
    let requests = f.transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let projected = Message::User {
        content: custom.content,
    };
    for request in requests.iter() {
        assert_eq!(request.messages.first(), Some(&projected));
        assert!(
            !request
                .messages
                .iter()
                .any(|m| matches!(m, Message::Custom(_)))
        );
    }
    Ok(())
}

#[tokio::test]
async fn queued_input_after_text_continues_with_the_saved_reply_once() -> Result<()> {
    let f = Fixture::new(
        Transport {
            pending: true,
            ..scripted(vec![vec![answer("first")], vec![answer("second")]])
        },
        Store::default(),
    );
    let turn = f.engine.run_with_input(options())?;
    timeout(LIMIT, f.transport.started.notified()).await?;
    turn.input.enqueue(user("follow up")).unwrap();
    f.transport.release.notify_one();
    timeout(LIMIT, f.transport.started.notified()).await?;
    f.transport.release.notify_one();
    let events = collect(turn.events).await;
    assert_eq!(delivered(&events), [user("follow up")]);
    assert!(matches!(
        events.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    let saved = f.store.load("session").await?.messages;
    assert_eq!(
        saved,
        [
            Message::User {
                content: vec![text("question")]
            },
            Message::Assistant {
                blocks: vec![answer("first")]
            },
            Message::User {
                content: vec![text("follow up")]
            },
            Message::Assistant {
                blocks: vec![answer("second")]
            }
        ]
    );
    let remaining: Vec<SteeringMessage> = timeout(LIMIT, turn.input.take_remaining()).await?;
    assert!(remaining.is_empty());
    let requests = f.transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages, saved[..3]);
    Ok(())
}

#[tokio::test]
async fn tool_result_revision_conflict_leaves_input_recoverable() -> Result<()> {
    let f = Fixture::new(
        Transport {
            pending: true,
            ..scripted(vec![vec![call("lookup")]])
        },
        Store {
            conflict: Some(WriteKind::Result),
            ..Store::default()
        },
    );
    let turn = f.engine.run_with_input(options())?;
    timeout(LIMIT, f.transport.started.notified()).await?;
    turn.input.enqueue(user("recover me")).unwrap();
    f.transport.release.notify_one();
    let events = collect(turn.events).await;
    assert!(matches!(
        events.last(),
        Some(Event::Done {
            reason: DoneReason::Failed
        })
    ));
    assert_eq!(*f.store.rejected.lock().unwrap(), 1);
    assert!(delivered(&events).is_empty());
    assert_eq!(
        timeout(LIMIT, turn.input.take_remaining()).await?,
        [user("recover me")]
    );
    assert_eq!(f.store.load("session").await?.messages.len(), 2);
    Ok(())
}

fn user(value: &str) -> SteeringMessage {
    SteeringMessage::User {
        content: vec![text(value)],
    }
}

#[tokio::test]
async fn exhausted_budget_returns_unsent_input_once_and_closes_all_handles() -> Result<()> {
    let f = Fixture::new(
        Transport {
            pending: true,
            ..scripted(vec![vec![answer("finished")]])
        },
        Store::default(),
    );
    let turn = f.engine.run_with_input(RunOptions {
        max_requests: 1,
        ..options()
    })?;
    timeout(LIMIT, f.transport.started.notified()).await?;
    let other = turn.input.clone();
    other.enqueue(user("discard")).unwrap();
    assert_eq!(turn.input.clear_queue(), [user("discard")]);
    assert!(other.pending().is_empty());
    for value in ["one", "two"] {
        turn.input.enqueue(user(value)).unwrap();
    }
    assert_eq!(other.pending(), [user("one"), user("two")]);
    f.transport.release.notify_one();
    let events = collect(turn.events).await;
    assert_eq!(
        events.last(),
        Some(&Event::Done {
            reason: DoneReason::Completed
        })
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::InputDelivered { .. }))
    );
    assert!(
        matches!(other.enqueue(user("late")), Err(EnqueueError::Closed(value)) if value == user("late"))
    );
    assert_eq!(
        timeout(LIMIT, other.take_remaining()).await?,
        [user("one"), user("two")]
    );
    let remaining: Vec<SteeringMessage> = timeout(LIMIT, turn.input.take_remaining()).await?;
    assert!(remaining.is_empty());
    assert_eq!(f.transport.requests.lock().unwrap().len(), 1);
    assert_eq!(f.store.load("session").await?.messages.len(), 2);
    Ok(())
}
