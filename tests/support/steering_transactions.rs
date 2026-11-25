use super::fixture::*;
use anyhow::{Result, bail};
use coda::embedded::*;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

fn user(value: &str) -> SteeringMessage {
    SteeringMessage::User {
        content: vec![text(value)],
    }
}

fn custom() -> CustomMessage {
    CustomMessage {
        custom_type: "host.note".into(),
        content: vec![text("插话"), text("second block")],
        display: false,
        details: Some(serde_json::json!({"source": [1, "host"]})),
    }
}

fn selected() -> [SteeringMessage; 2] {
    [SteeringMessage::Custom(custom()), user("follow-up")]
}

fn first_round() -> Vec<Message> {
    vec![
        Message::User {
            content: options().content,
        },
        Message::Assistant {
            blocks: vec![answer("first reply")],
        },
    ]
}

#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
}

impl Gate {
    async fn pause(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

#[derive(Default)]
struct StagedStore {
    history: Store,
    steering: Gate,
    abort_marker: Gate,
    panic_after_commit: std::sync::atomic::AtomicBool,
}

impl SessionStore for StagedStore {
    fn load<'a>(&'a self, session: &'a str) -> BoxFuture<'a, Result<Snapshot>> {
        self.history.load(session)
    }

    fn append<'a>(
        &'a self,
        session: &'a str,
        revision: &'a str,
        entries: &'a [Entry],
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            if entries
                .iter()
                .any(|e| matches!(e, Entry::Message(Message::Custom(_))))
            {
                self.steering.pause().await;
            }
            if entries
                .iter()
                .any(|e| matches!(e, Entry::Message(Message::Aborted { .. })))
            {
                self.abort_marker.pause().await;
            }
            let committed = self.history.append(session, revision, entries).await?;
            assert!(
                !self
                    .panic_after_commit
                    .load(std::sync::atomic::Ordering::SeqCst),
                "lost store acknowledgement"
            );
            Ok(committed)
        })
    }
}

struct StagedModel {
    transport: Transport,
    reject_steering: bool,
}

impl ModelTransport for StagedModel {
    fn preflight(&self, request: &ModelRequest) -> Result<()> {
        if self.reject_steering && request.messages.len() > 1 {
            let mut expected = first_round();
            expected.extend([
                Message::User {
                    content: custom().content,
                },
                Message::User {
                    content: vec![text("follow-up")],
                },
            ]);
            assert_eq!(request.messages, expected);
            bail!("steering preflight rejected");
        }
        Ok(())
    }

    fn complete<'a>(
        &'a self,
        request: &'a ModelRequest,
        emit: &'a (dyn Fn(ModelDelta) -> Result<()> + Send + Sync),
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ModelReply>> {
        self.transport.complete(request, emit, cancel)
    }
}

async fn start(reject_steering: bool) -> Result<(EmbeddedEngine, RunningTurn, Arc<StagedStore>)> {
    let model = Arc::new(StagedModel {
        transport: Transport {
            pending: true,
            ..scripted(vec![vec![answer("first reply")]])
        },
        reject_steering,
    });
    let store = Arc::new(StagedStore::default());
    let engine = EmbeddedEngine::new(model.clone(), Arc::new(Tools::default()), store.clone());
    let turn = engine.run_with_input(options())?;
    timeout(LIMIT, model.transport.started.notified()).await?;
    for message in selected() {
        turn.input.enqueue(message).unwrap();
    }
    model.transport.release.notify_one();
    Ok((engine, turn, store))
}

struct InitialStore {
    history: Store,
    attempts: Mutex<Vec<(String, Vec<Entry>)>>,
}

impl SessionStore for InitialStore {
    fn load<'a>(&'a self, session: &'a str) -> BoxFuture<'a, Result<Snapshot>> {
        self.history.load(session)
    }

    fn append<'a>(
        &'a self,
        session: &'a str,
        revision: &'a str,
        entries: &'a [Entry],
    ) -> BoxFuture<'a, Result<String>> {
        self.attempts
            .lock()
            .unwrap()
            .push((revision.to_owned(), entries.to_vec()));
        self.history.append(session, revision, entries)
    }
}

async fn start_during_load(
    conflict: Option<WriteKind>,
) -> Result<(RunningTurn, Arc<InitialStore>, Arc<Transport>)> {
    let store = Arc::new(InitialStore {
        history: Store {
            pause_load: true,
            conflict,
            ..Store::default()
        },
        attempts: Mutex::default(),
    });
    let model = Arc::new(scripted(vec![vec![answer("reply")]]));
    let engine = EmbeddedEngine::new(model.clone(), Arc::new(Tools::default()), store.clone());
    let turn = engine.run_with_input(options())?;
    timeout(LIMIT, store.history.append_started.notified()).await?;
    for message in selected() {
        turn.input.enqueue(message).unwrap();
    }
    store.history.release_append.notify_one();
    Ok((turn, store, model))
}

fn initial_entries() -> Vec<Entry> {
    vec![
        Entry::Message(Message::User {
            content: options().content,
        }),
        Entry::Message(Message::Custom(custom())),
        Entry::Message(Message::User {
            content: vec![text("follow-up")],
        }),
    ]
}

#[tokio::test]
async fn initial_request_commits_prompt_and_steering_together_and_acknowledges_only_steering()
-> Result<()> {
    let (turn, store, model) = start_during_load(None).await?;
    let events = collect(turn.events).await;
    assert_eq!(
        events.last(),
        Some(&Event::Done {
            reason: DoneReason::Completed
        })
    );
    let delivered: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event {
            Event::InputDelivered { message } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(delivered, selected());
    let first = initial_entries();
    let reply = Entry::Message(Message::Assistant {
        blocks: vec![answer("reply")],
    });
    assert_eq!(
        *store.attempts.lock().unwrap(),
        [
            ("0".to_owned(), first.clone()),
            (first.len().to_string(), vec![reply.clone()])
        ]
    );
    assert_eq!(
        *store.history.entries.lock().unwrap(),
        [first, vec![reply]].concat()
    );
    assert!(
        timeout(LIMIT, turn.input.take_remaining())
            .await?
            .is_empty()
    );
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].messages,
        [
            Message::User {
                content: options().content
            },
            Message::User {
                content: custom().content
            },
            Message::User {
                content: vec![text("follow-up")]
            }
        ]
    );
    Ok(())
}

#[tokio::test]
async fn initial_append_conflict_leaves_history_empty_and_returns_only_steering_once() -> Result<()>
{
    let (turn, store, model) = start_during_load(Some(WriteKind::User)).await?;
    assert_failed_before_delivery(&collect(turn.events).await, "SESSION_APPEND");
    assert_eq!(
        *store.attempts.lock().unwrap(),
        [("0".to_owned(), initial_entries())]
    );
    assert!(store.history.entries.lock().unwrap().is_empty());
    assert!(model.requests.lock().unwrap().is_empty());
    assert_eq!(
        timeout(LIMIT, turn.input.take_remaining()).await?,
        selected()
    );
    assert!(
        timeout(LIMIT, turn.input.take_remaining())
            .await?
            .is_empty()
    );
    Ok(())
}

fn assert_failed_before_delivery(events: &[Event], expected_code: &str) {
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Error { code, .. } if code == expected_code)),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::InputDelivered { .. }))
    );
    assert_eq!(
        events.last(),
        Some(&Event::Done {
            reason: DoneReason::Failed
        })
    );
}

#[tokio::test]
async fn conflicted_append_returns_selected_batch_before_new_pending_input() -> Result<()> {
    let (_engine, turn, store) = start(false).await?;
    timeout(LIMIT, store.steering.entered.notified()).await?;
    turn.input.enqueue(user("clear me")).unwrap();
    assert_eq!(turn.input.pending(), [user("clear me")]);
    assert_eq!(turn.input.clear_queue(), [user("clear me")]);
    turn.input.enqueue(user("still queued")).unwrap();

    // A competing writer advances the actual revision while the engine's append is suspended.
    let snapshot = store.load("session").await?;
    store
        .history
        .append(
            "session",
            &snapshot.revision,
            &[Entry::Usage(Usage::default())],
        )
        .await?;
    store.steering.release.notify_one();
    assert_failed_before_delivery(&collect(turn.events).await, "SESSION_APPEND");
    assert_eq!(store.load("session").await?.messages, first_round());
    let mut expected = selected().to_vec();
    expected.push(user("still queued"));
    assert_eq!(timeout(LIMIT, turn.input.take_remaining()).await?, expected);
    assert!(
        timeout(LIMIT, turn.input.take_remaining())
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn panic_after_commit_preserves_batch_for_reconciliation_without_requeue() -> Result<()> {
    let (_engine, turn, store) = start(false).await?;
    timeout(LIMIT, store.steering.entered.notified()).await?;
    let base = store.load("session").await?.revision;
    turn.input.enqueue(user("later")).unwrap();
    store
        .panic_after_commit
        .store(true, std::sync::atomic::Ordering::SeqCst);
    store.steering.release.notify_one();
    assert_failed_before_delivery(&collect(turn.events).await, "HOST_PANIC");
    let mut history = first_round();
    history.extend(selected().map(Message::from));
    assert_eq!(store.load("session").await?.messages, history);
    assert_eq!(turn.input.take_remaining().await, [user("later")]);
    let uncertain = turn
        .input
        .take_unconfirmed()
        .await
        .expect("missing unconfirmed batch");
    assert_eq!(uncertain.expected_revision, base);
    assert_eq!(uncertain.messages, selected());
    assert!(turn.input.take_unconfirmed().await.is_none());
    Ok(())
}

#[tokio::test]
async fn abort_during_successful_append_delivers_once_before_terminal_history_and_done()
-> Result<()> {
    let (engine, mut turn, store) = start(false).await?;
    timeout(LIMIT, store.steering.entered.notified()).await?;
    // Poll abort to its first suspension before allowing the store to commit.
    let abort = engine.abort("session", "run");
    tokio::pin!(abort);
    tokio::select! { biased; () = &mut abort => panic!("abort finished during append"), () = std::future::ready(()) => {} }
    while let Ok(event) = turn.events.try_recv() {
        assert!(!matches!(
            event,
            Event::InputDelivered { .. } | Event::Done { .. }
        ));
    }
    store.steering.release.notify_one();
    timeout(LIMIT, store.abort_marker.entered.notified()).await?;
    let mut expected_history = first_round();
    expected_history.extend([
        Message::Custom(custom()),
        Message::User {
            content: vec![text("follow-up")],
        },
    ]);
    assert_eq!(store.load("session").await?.messages, expected_history);
    let mut delivered = Vec::new();
    while let Ok(event) = turn.events.try_recv() {
        match event {
            Event::InputDelivered { message } => delivered.push(message),
            Event::Done { .. } => panic!("Done preceded the abort marker commit"),
            _ => {}
        }
    }
    store.abort_marker.release.notify_one();
    let events = collect(turn.events).await;
    for event in &events {
        if let Event::InputDelivered { message } = event {
            delivered.push(message.clone());
        }
    }
    assert_eq!(delivered, selected());
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::Done { .. }))
            .count(),
        1
    );
    assert_eq!(
        events.last(),
        Some(&Event::Done {
            reason: DoneReason::Aborted
        })
    );
    expected_history.push(Message::Aborted {
        run_id: "run".into(),
        partial: vec![],
    });
    assert_eq!(store.load("session").await?.messages, expected_history);
    assert!(
        timeout(LIMIT, turn.input.take_remaining())
            .await?
            .is_empty()
    );
    timeout(LIMIT, abort).await?;
    Ok(())
}

#[tokio::test]
async fn rejected_steering_preflight_returns_original_batch_without_appending_it() -> Result<()> {
    let (_engine, turn, store) = start(true).await?;
    assert_failed_before_delivery(&collect(turn.events).await, "MODEL_PREFLIGHT");
    assert_eq!(store.load("session").await?.messages, first_round());
    assert_eq!(
        timeout(LIMIT, turn.input.take_remaining()).await?,
        selected()
    );
    Ok(())
}
