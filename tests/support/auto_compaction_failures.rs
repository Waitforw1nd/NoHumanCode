use super::fixture::*;
use anyhow::{Result, bail};
use coda::embedded::*;
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc::UnboundedReceiver};

const TOKENS: u64 = 4096;
const SUMMARY: &str = "both lookups finished";

#[derive(Clone, Copy, PartialEq)]
enum Fault {
    Cancel,
    CancelDuringAppend,
    SummaryPreflight,
    Transport,
    InvalidBlock,
    Conflict,
    ChangedRevision,
    FinalPreflight,
}

struct ModelHost {
    fault: Fault,
    first_started: Notify,
    release_first: Notify,
    summary_started: Notify,
    decisions: Mutex<usize>,
    requests: Mutex<Vec<RequestPurpose>>,
    events: Mutex<Option<UnboundedReceiver<Event>>>,
    before_rejection: Mutex<Vec<Event>>,
}

impl ModelTransport for ModelHost {
    fn before_request<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: CancellationToken,
    ) -> BoxFuture<'a, Result<CompactionDecision>> {
        Box::pin(async move {
            assert_eq!(request.purpose, RequestPurpose::Generation);
            if self.requests.lock().unwrap().is_empty() {
                return Ok(CompactionDecision::Continue);
            }
            let mut decisions = self.decisions.lock().unwrap();
            *decisions += 1;
            assert_eq!(*decisions, 1, "same boundary evaluated twice");
            let ids: Vec<_> = request
                .messages
                .iter()
                .filter_map(|m| match m {
                    Message::Tool { call_id, .. } => Some(call_id.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(ids, ["one", "two"]);
            Ok(CompactionDecision::Compact {
                instructions: "summarize the lookups".into(),
                tokens_before: TOKENS,
            })
        })
    }

    fn preflight(&self, request: &ModelRequest) -> Result<()> {
        if self.fault == Fault::SummaryPreflight && request.purpose == RequestPurpose::Compaction {
            bail!("summary exceeds budget");
        }
        if self.fault == Fault::FinalPreflight
            && request.purpose == RequestPurpose::Generation
            && request
                .messages
                .iter()
                .any(|m| matches!(m, Message::Summary { text } if text == SUMMARY))
        {
            let mut events = self.events.lock().unwrap();
            let rx = events.as_mut().unwrap();
            let mut prefix = self.before_rejection.lock().unwrap();
            while let Ok(event) = rx.try_recv() {
                prefix.push(event);
            }
            bail!("compacted generation exceeds budget");
        }
        Ok(())
    }

    fn complete<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a (dyn Fn(ModelDelta) -> Result<()> + Send + Sync),
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ModelReply>> {
        Box::pin(async move {
            let count = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.purpose);
                requests.len()
            };
            let blocks = match request.purpose {
                RequestPurpose::Generation => {
                    assert_eq!(count, 1, "unexpected generation");
                    self.first_started.notify_one();
                    self.release_first.notified().await;
                    vec![call("one"), call("two")]
                }
                RequestPurpose::Compaction => {
                    assert_eq!(count, 2, "unexpected summary request");
                    self.summary_started.notify_one();
                    match self.fault {
                        Fault::Cancel => {
                            cancel.cancelled().await;
                            bail!("summary cancelled");
                        }
                        Fault::Transport => bail!("summary unavailable"),
                        Fault::InvalidBlock => vec![answer(SUMMARY), call("illegal")],
                        _ => vec![answer(SUMMARY)],
                    }
                }
            };
            Ok(ModelReply {
                blocks,
                usage: None,
            })
        })
    }
}

struct History {
    store: Store,
    fault: Fault,
    summary_attempts: Mutex<usize>,
}

impl SessionStore for History {
    fn load<'a>(&'a self, session: &'a str) -> BoxFuture<'a, Result<Snapshot>> {
        Box::pin(async move {
            let mut snapshot = self.store.load(session).await?;
            if self.fault == Fault::ChangedRevision && *self.summary_attempts.lock().unwrap() > 0 {
                snapshot.revision.push_str("-changed");
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
            if entries
                .iter()
                .any(|e| matches!(e, Entry::Compaction { .. }))
            {
                *self.summary_attempts.lock().unwrap() += 1;
                if self.fault == Fault::CancelDuringAppend {
                    self.store.append_started.notify_one();
                    self.store.release_append.notified().await;
                }
                if self.fault == Fault::Conflict {
                    bail!("summary CAS conflict");
                }
            }
            self.store.append(session, revision, entries).await
        })
    }
}

fn assert_history(entries: &[Entry], fault: Fault) {
    let mut expected = vec![
        Entry::Message(Message::User {
            content: options().content,
        }),
        Entry::Message(Message::Assistant {
            blocks: vec![call("one"), call("two")],
        }),
    ];
    for id in ["one", "two"] {
        expected.push(Entry::Message(Message::Tool {
            call_id: id.into(),
            name: "lookup".into(),
            result: ToolResult {
                content: vec![text("found")],
                details: serde_json::Value::Null,
                is_error: false,
            },
        }));
    }
    if matches!(
        fault,
        Fault::CancelDuringAppend | Fault::ChangedRevision | Fault::FinalPreflight
    ) {
        expected.push(Entry::Compaction {
            summary: SUMMARY.into(),
            tokens_before: Some(TOKENS),
        });
    }
    if matches!(fault, Fault::Cancel | Fault::CancelDuringAppend) {
        expected.push(Entry::Message(Message::Aborted {
            run_id: "run".into(),
            partial: vec![],
        }));
    }
    assert_eq!(entries, expected);
}

fn assert_events(events: &[Event], fault: Fault) {
    let done = if matches!(fault, Fault::Cancel | Fault::CancelDuringAppend) {
        DoneReason::Aborted
    } else {
        DoneReason::Failed
    };
    let end = if fault == Fault::FinalPreflight {
        DoneReason::Completed
    } else {
        done
    };
    let lifecycle: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::CompactionStart { .. }
                    | Event::CompactionEnd { .. }
                    | Event::Done { .. }
                    | Event::InputDelivered { .. }
            )
        })
        .cloned()
        .collect();
    assert_eq!(
        lifecycle,
        [
            Event::CompactionStart {
                tokens_before: TOKENS
            },
            Event::CompactionEnd { reason: end },
            Event::Done { reason: done }
        ]
    );
    assert_eq!(events.last(), lifecycle.last());
    let code = match fault {
        Fault::SummaryPreflight => "COMPACTION_PREFLIGHT",
        Fault::Conflict => "SESSION_APPEND",
        Fault::ChangedRevision => "SESSION_REVISION",
        Fault::FinalPreflight => "MODEL_PREFLIGHT",
        _ => return,
    };
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Error { code: actual, .. } if actual == code)),
        "{events:?}"
    );
}

async fn exercise(fault: Fault) -> Result<()> {
    let model = Arc::new(ModelHost {
        fault,
        first_started: Notify::new(),
        release_first: Notify::new(),
        summary_started: Notify::new(),
        decisions: Mutex::default(),
        requests: Mutex::default(),
        events: Mutex::default(),
        before_rejection: Mutex::default(),
    });
    let tools = Arc::new(Tools::default());
    let history = Arc::new(History {
        store: Store::default(),
        fault,
        summary_attempts: Mutex::default(),
    });
    let engine = EmbeddedEngine::new(model.clone(), tools.clone(), history.clone());
    let turn = engine.run_with_input(options())?;
    timeout(LIMIT, model.first_started.notified()).await?;
    let steering = SteeringMessage::User {
        content: vec![text("pending follow-up")],
    };
    turn.input.enqueue(steering.clone()).unwrap();
    *model.events.lock().unwrap() = Some(turn.events);
    model.release_first.notify_one();
    if fault == Fault::Cancel {
        timeout(LIMIT, model.summary_started.notified()).await?;
        timeout(LIMIT, engine.abort("session", "run")).await?;
    } else if fault == Fault::CancelDuringAppend {
        timeout(LIMIT, history.store.append_started.notified()).await?;
        let abort = engine.abort("session", "run");
        tokio::pin!(abort);
        tokio::select! {
            biased;
            () = &mut abort => panic!("abort returned before summary CAS resolved"),
            () = std::future::ready(()) => {}
        }
        history.store.release_append.notify_one();
        timeout(LIMIT, abort).await?;
    }
    assert_eq!(
        timeout(LIMIT, turn.input.take_remaining()).await?,
        [steering]
    );
    let mut events = std::mem::take(&mut *model.before_rejection.lock().unwrap());
    if fault == Fault::FinalPreflight {
        assert!(
            events.contains(&Event::CompactionEnd {
                reason: DoneReason::Completed
            }),
            "final preflight preceded compaction end"
        );
    }
    let rx = model.events.lock().unwrap().take().unwrap();
    events.extend(collect(rx).await);
    assert_events(&events, fault);
    assert_history(&history.store.entries.lock().unwrap(), fault);
    assert_eq!(*model.decisions.lock().unwrap(), 1);
    let expected_requests: &[RequestPurpose] = if fault == Fault::SummaryPreflight {
        &[RequestPurpose::Generation]
    } else {
        &[RequestPurpose::Generation, RequestPurpose::Compaction]
    };
    assert_eq!(model.requests.lock().unwrap().as_slice(), expected_requests);
    let attempted = matches!(
        fault,
        Fault::CancelDuringAppend
            | Fault::Conflict
            | Fault::ChangedRevision
            | Fault::FinalPreflight
    );
    assert_eq!(
        *history.summary_attempts.lock().unwrap(),
        usize::from(attempted)
    );
    assert_eq!(
        *tools.actions.lock().unwrap(),
        ["review one", "execute one", "review two", "execute two"]
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_during_summary_preserves_tool_results_and_abort_marker() -> Result<()> {
    exercise(Fault::Cancel).await
}

#[tokio::test]
async fn summary_transport_failure_returns_steering() -> Result<()> {
    exercise(Fault::Transport).await
}

#[tokio::test]
async fn illegal_summary_block_rejects_whole_reply() -> Result<()> {
    exercise(Fault::InvalidBlock).await
}

#[tokio::test]
async fn summary_cas_conflict_stops_generation() -> Result<()> {
    exercise(Fault::Conflict).await
}

#[tokio::test]
async fn reload_revision_change_stops_generation() -> Result<()> {
    exercise(Fault::ChangedRevision).await
}

#[tokio::test]
async fn completed_summary_precedes_rejected_generation_preflight() -> Result<()> {
    exercise(Fault::FinalPreflight).await
}

#[tokio::test]
async fn rejected_summary_preflight_returns_steering_without_request_or_append() -> Result<()> {
    exercise(Fault::SummaryPreflight).await
}

#[tokio::test]
async fn cancel_during_summary_append_uses_committed_revision_for_abort_marker() -> Result<()> {
    exercise(Fault::CancelDuringAppend).await
}
