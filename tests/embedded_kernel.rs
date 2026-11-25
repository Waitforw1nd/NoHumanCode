#[path = "support/embedded_abort.rs"]
mod abort_history;
use abort_history::assert_reloaded_abort_projection;

#[path = "support/embedded_fixture.rs"]
mod fixture;

use anyhow::Result;
use coda::embedded::*;
use fixture::*;
use std::time::Duration;

#[tokio::test]
async fn injected_model_streams_text_and_persists_conversation() -> Result<()> {
    let f = Fixture::new(scripted(vec![vec![answer("reply")]]), Store::default());
    let mut opts = options();
    opts.settings.max_output_tokens = Some(128);
    let events = collect(f.engine.run(opts)?).await;
    assert!(matches!(events.first(), Some(Event::Start{model}) if model.id == "injected"));
    assert!(events.iter().any(|event| matches!(event, Event::Text{block_id,delta} if block_id == "block" && delta == "reply")));
    assert!(matches!(
        events.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    let saved = f.store.load("session").await?;
    assert!(
        matches!(saved.messages.as_slice(), [Message::User{content}, Message::Assistant{blocks}] if matches!(content.as_slice(), [Content::Text{text}] if text == "question") && matches!(blocks.as_slice(), [Block::Text{text}] if text == "reply"))
    );
    let requests = f.transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!(r.settings.max_output_tokens, Some(128));
    assert_eq!(r.session_id, "session");
    assert_eq!(r.run_id.as_deref(), Some("run"));
    assert_eq!(r.system_prompt, "rules");
    assert_eq!(r.model.provider.as_deref(), Some("custom"));
    assert_eq!(r.tools[0].name, "lookup");
    Ok(())
}

#[tokio::test]
async fn batch_reviews_calls_and_returns_correlated_results_to_model() -> Result<()> {
    let ids = ["first", "second"];
    let f = Fixture::new(
        scripted(vec![
            ids.iter().map(|id| call(id)).collect(),
            vec![answer("finished")],
        ]),
        Store::default(),
    );
    let events = collect(f.engine.run(options())?).await;
    assert!(matches!(
        events.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    assert_eq!(
        f.tools.actions.lock().unwrap().as_slice(),
        [
            "review first",
            "execute first",
            "review second",
            "execute second"
        ]
    );
    let calls: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Event::ToolCall { call } = e {
                Some(call.id.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(calls, ids);
    let expected = [("first", false), ("second", false)];
    let results: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Event::ToolResult {
                call_id, result, ..
            } = e
            {
                Some((call_id.as_str(), result.is_error))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(results, expected);
    let requests = f.transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let messages: Vec<_> = requests[1]
        .messages
        .iter()
        .filter_map(|m| {
            if let Message::Tool {
                call_id, result, ..
            } = m
            {
                Some((call_id.as_str(), result.is_error))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(messages, expected);
    Ok(())
}

fn pending_fixture() -> Fixture {
    Fixture::new(
        Transport {
            pending: true,
            ..scripted(vec![vec![answer("reply")], vec![answer("second reply")]])
        },
        Store::default(),
    )
}

#[tokio::test]
async fn abort_waits_for_inflight_model_and_emits_aborted() -> Result<()> {
    let f = pending_fixture();
    let rx = f.engine.run(options())?;
    timeout(LIMIT, f.transport.started.notified()).await?;
    let engine = f.engine.clone();
    let abort = tokio::spawn(async move { engine.abort("session", "run").await });
    timeout(LIMIT, f.transport.cancelled.notified()).await?;
    assert!(!abort.is_finished());
    f.transport.cleanup.notify_one();
    timeout(LIMIT, abort).await??;
    assert!(matches!(
        collect(rx).await.last(),
        Some(Event::Done {
            reason: DoneReason::Aborted
        })
    ));
    Ok(())
}

#[tokio::test]
async fn dropping_event_receiver_still_persists_reply() -> Result<()> {
    let f = pending_fixture();
    let rx = f.engine.run(options())?;
    timeout(LIMIT, f.transport.started.notified()).await?;
    drop(rx);
    f.transport.release.notify_one();
    timeout(LIMIT, f.store.assistant_saved.notified()).await?;
    assert!(
        matches!(f.store.load("session").await?.messages.last(), Some(Message::Assistant{blocks}) if blocks == &[answer("reply")])
    );
    Ok(())
}

#[tokio::test]
async fn compaction_persists_model_summary_and_uses_session_history() -> Result<()> {
    let f = Fixture::new(scripted(vec![vec![answer("summary")]]), Store::default());
    f.store
        .append(
            "session",
            "0",
            &[Entry::Message(Message::User {
                content: vec![text("old question")],
            })],
        )
        .await?;
    let _ = timeout(
        LIMIT,
        f.engine.compact("session", model(), "summarize history"),
    )
    .await??;
    assert!(
        matches!(f.store.entries.lock().unwrap().last(),Some(Entry::Compaction{summary}) if summary == "summary")
    );
    let requests = f.transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].session_id, "session");
    assert!(requests[0].run_id.is_none());
    assert!(requests[0].messages.iter().any(|m| matches!(m,Message::User{content} if content.iter().any(|c| matches!(c,Content::Text{text} if text.contains("old question"))))));
    Ok(())
}

#[tokio::test]
async fn preflight_failure_has_no_model_or_storage_side_effects() -> Result<()> {
    let f = Fixture::new(
        Transport {
            reject: true,
            ..Default::default()
        },
        Store::default(),
    );
    assert_failed(&collect(f.engine.run(options())?).await);
    assert!(f.transport.requests.lock().unwrap().is_empty());
    assert!(f.store.entries.lock().unwrap().is_empty());
    Ok(())
}

fn assert_failed(events: &[Event]) {
    assert!(
        matches!(events, [prefix @ .., Event::Error{code,message}, Event::Done{reason: DoneReason::Failed}] if !code.is_empty() && !message.is_empty() && !prefix.iter().any(|e| matches!(e,Event::Done{..}|Event::Error{..}))),
        "unexpected terminal sequence {events:?}"
    );
}

#[tokio::test]
async fn rejected_revision_fails_run_and_leaves_store_intact() -> Result<()> {
    let f = Fixture::new(
        scripted(vec![vec![answer("reply")]]),
        Store {
            conflict: Some(WriteKind::User),
            ..Default::default()
        },
    );
    assert_failed(&collect(f.engine.run(options())?).await);
    assert!(f.store.entries.lock().unwrap().is_empty());
    Ok(())
}

async fn failed_batch(id: &str, actions: &[&str]) -> Result<()> {
    let f = Fixture::new(
        scripted(vec![vec![call(id), call("skipped")]]),
        Store::default(),
    );
    let events = collect(f.engine.run(options())?).await;
    assert_failed(&events);
    assert_eq!(f.tools.actions.lock().unwrap().as_slice(), actions);
    assert_eq!(f.transport.requests.lock().unwrap().len(), 1);
    let saved = f.store.load("session").await?;
    let results: Vec<_> = saved
        .messages
        .iter()
        .filter_map(|m| {
            if let Message::Tool {
                call_id, result, ..
            } = m
            {
                Some((call_id.as_str(), result.is_error))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(results, [(id, true), ("skipped", true)]);
    Ok(())
}
#[tokio::test]
async fn denial_skips_remaining_calls() -> Result<()> {
    failed_batch("denied", &["review denied"]).await
}
#[tokio::test]
async fn execution_failure_skips_remaining_calls() -> Result<()> {
    failed_batch("broken", &["review broken", "execute broken"]).await
}

#[tokio::test]
async fn active_run_excludes_other_writers_and_stale_abort_spares_next_run() -> Result<()> {
    let f = pending_fixture();
    let first = f.engine.run(options())?;
    timeout(LIMIT, f.transport.started.notified()).await?;
    let second = RunOptions {
        run_id: "next".into(),
        ..options()
    };
    assert!(f.engine.run(second.clone()).is_err());
    assert!(
        timeout(LIMIT, f.engine.compact("session", model(), "summary"))
            .await?
            .is_err()
    );
    f.transport.release.notify_one();
    assert!(matches!(
        collect(first).await.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    assert!(f.engine.run(options()).is_err());
    let rx = f.engine.run(second)?;
    timeout(LIMIT, f.transport.started.notified()).await?;
    timeout(LIMIT, f.engine.abort("session", "run")).await?;
    f.transport.release.notify_one();
    assert!(matches!(
        collect(rx).await.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    Ok(())
}

async fn reject_tool_write(kind: WriteKind, expected_actions: &[&str]) -> Result<()> {
    let f = Fixture::new(
        scripted(vec![vec![call("one"), call("two")]]),
        Store {
            conflict: Some(kind),
            ..Default::default()
        },
    );
    assert_failed(&collect(f.engine.run(options())?).await);
    assert_eq!(f.tools.actions.lock().unwrap().as_slice(), expected_actions);
    assert_eq!(*f.store.rejected.lock().unwrap(), 1);
    assert_eq!(f.transport.requests.lock().unwrap().len(), 1);
    let saved = f.store.load("session").await?;
    assert_eq!(
        saved.messages.len(),
        if kind == WriteKind::Intent { 1 } else { 2 }
    );
    if kind == WriteKind::Result {
        assert!(
            matches!(saved.messages.last(),Some(Message::Assistant{blocks}) if blocks == &[call("one"),call("two")])
        );
        let retry = RunOptions {
            run_id: "retry".into(),
            ..options()
        };
        assert_failed(&collect(f.engine.run(retry)?).await);
        assert_eq!(f.tools.actions.lock().unwrap().as_slice(), expected_actions);
        assert_eq!(f.transport.requests.lock().unwrap().len(), 1);
        assert_eq!(f.store.load("session").await?.messages, saved.messages);
    }
    Ok(())
}
#[tokio::test]
async fn intent_conflict_prevents_tool_dispatch() -> Result<()> {
    reject_tool_write(WriteKind::Intent, &[]).await
}
#[tokio::test]
async fn result_conflict_never_repeats_side_effect_or_dispatches_next_call() -> Result<()> {
    reject_tool_write(WriteKind::Result, &["review one", "execute one"]).await
}

#[tokio::test]
async fn unfinished_tool_history_blocks_run_and_compaction() -> Result<()> {
    let f = Fixture::new(Transport::default(), Store::default());
    let history = [
        Entry::Message(Message::User {
            content: vec![text("before")],
        }),
        Entry::Message(Message::Assistant {
            blocks: vec![call("unfinished")],
        }),
    ];
    f.store.append("session", "0", &history).await?;
    assert_failed(&collect(f.engine.run(options())?).await);
    assert!(
        timeout(LIMIT, f.engine.compact("session", model(), "summary"))
            .await?
            .is_err()
    );
    assert!(f.transport.requests.lock().unwrap().is_empty());
    assert!(f.tools.actions.lock().unwrap().is_empty());
    assert_eq!(f.store.entries.lock().unwrap().as_slice(), &history);
    Ok(())
}

#[tokio::test]
async fn duplicate_final_call_invalidates_whole_batch_before_intent_or_review() -> Result<()> {
    let f = Fixture::new(
        scripted(vec![vec![call("duplicate"), call("duplicate")]]),
        Store::default(),
    );
    assert_failed(&collect(f.engine.run(options())?).await);
    assert!(f.tools.actions.lock().unwrap().is_empty());
    assert!(matches!(
        f.store.load("session").await?.messages.as_slice(),
        [Message::User { .. }]
    ));
    Ok(())
}

fn assert_aborted(events: &[Event]) {
    assert!(
        matches!(events, [prefix @ .., Event::Done{reason: DoneReason::Aborted}] if !prefix.iter().any(|event| matches!(event, Event::Done{..}|Event::Error{..}))),
        "unexpected events {events:?}"
    );
}

#[tokio::test]
async fn abort_during_final_reply_append_waits_and_persists_reply_once() -> Result<()> {
    let f = Fixture::new(
        scripted(vec![vec![answer("reply")]]),
        Store {
            pause_final_reply: true,
            ..Default::default()
        },
    );
    let rx = f.engine.run(options())?;
    timeout(LIMIT, f.store.append_started.notified()).await?;
    let abort = f.engine.abort("session", "run");
    tokio::pin!(abort);
    // Poll on this task so the abort signal precedes release of the append.
    assert!(futures_util::poll!(&mut abort).is_pending());
    f.store.release_append.notify_one();
    timeout(LIMIT, abort).await?;
    assert_aborted(&collect(rx).await);
    assert_eq!(
        f.store.load("session").await?.messages,
        [
            Message::User {
                content: options().content
            },
            Message::Assistant {
                blocks: vec![answer("reply")]
            },
            Message::Aborted {
                run_id: "run".into(),
                partial: vec![]
            }
        ]
    );
    assert_eq!(f.transport.requests.lock().unwrap().len(), 1);
    assert_reloaded_abort_projection(&f).await
}

async fn abort_during_tool_callback(id: &str, expected_actions: &[&str]) -> Result<()> {
    let f = Fixture::new(
        scripted(vec![vec![call(id), call("later")]]),
        Store::default(),
    );
    let rx = f.engine.run(options())?;
    timeout(LIMIT, f.tools.blocked.notified()).await?;
    timeout(LIMIT, f.engine.abort("session", "run")).await?;
    assert_aborted(&collect(rx).await);
    assert_eq!(f.tools.actions.lock().unwrap().as_slice(), expected_actions);
    assert_eq!(f.transport.requests.lock().unwrap().len(), 1);
    let saved = f.store.load("session").await?;
    let results: Vec<_> = saved
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Tool {
                call_id, result, ..
            } => Some((call_id.as_str(), result)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    for ((actual_id, result), expected_id) in results.into_iter().zip([id, "later"]) {
        assert_eq!(actual_id, expected_id);
        assert!(result.is_error);
        assert_eq!(result.content, [text("ABORTED")]);
    }
    assert!(
        matches!(saved.messages.last(), Some(Message::Aborted { run_id, partial }) if run_id == "run" && partial.is_empty())
    );
    assert_reloaded_abort_projection(&f).await
}

#[tokio::test]
async fn abort_during_review_records_cancellation_and_stops_dispatch() -> Result<()> {
    abort_during_tool_callback("wait-review", &["review wait-review"]).await
}

#[tokio::test]
async fn abort_during_execute_records_cancellation_and_stops_dispatch() -> Result<()> {
    abort_during_tool_callback(
        "wait-execute",
        &["review wait-execute", "execute wait-execute"],
    )
    .await
}

#[tokio::test]
async fn exhausted_update_budget_closes_batch_and_allows_continuation() -> Result<()> {
    let f = Fixture::new(
        scripted(vec![
            vec![call("flood"), call("later")],
            vec![answer("continued")],
        ]),
        Store::default(),
    );
    let mut rx = f.engine.run(options())?;
    let events = timeout(Duration::from_secs(30), async {
        let mut events = vec![];
        while let Some(event) = rx.recv().await {
            if !matches!(event, Event::ToolUpdate { .. }) {
                events.push(event);
            }
        }
        events
    })
    .await?;
    assert_failed(&events);
    assert_eq!(
        f.tools.actions.lock().unwrap().as_slice(),
        ["review flood", "execute flood"]
    );
    assert_eq!(f.transport.requests.lock().unwrap().len(), 1);
    let saved = f.store.load("session").await?;
    assert!(
        matches!(saved.messages.as_slice(),[Message::User{..},Message::Assistant{blocks}, Message::Tool{call_id:first,result,..},Message::Tool{call_id:later,result:skipped,..}] if blocks == &[call("flood"),call("later")] && first == "flood" && !result.is_error && result.content == [text("found")] && later == "later" && skipped.is_error),
        "unexpected history {:?}",
        saved.messages
    );
    let next = RunOptions {
        run_id: "continue".into(),
        content: vec![text("continue")],
        ..options()
    };
    let events = collect(f.engine.run(next)?).await;
    assert!(matches!(
        events.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    let requests = f.transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let mut expected = saved.messages;
    expected.push(Message::User {
        content: vec![text("continue")],
    });
    assert_eq!(requests[1].messages, expected);
    Ok(())
}

#[tokio::test]
async fn final_request_records_tool_intent_and_skips_dispatch() -> Result<()> {
    let f = Fixture::new(
        scripted(vec![vec![call("one"), call("two")]]),
        Store::default(),
    );
    let events = collect(f.engine.run(RunOptions {
        max_requests: 1,
        ..options()
    })?)
    .await;
    assert_failed(&events);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Error{code,..} if code == "REQUEST_LIMIT"))
    );
    assert!(f.tools.actions.lock().unwrap().is_empty());
    assert_eq!(f.transport.requests.lock().unwrap().len(), 1);
    let saved = f.store.load("session").await?;
    assert!(
        matches!(saved.messages.as_slice(),[Message::User{..}, Message::Assistant{blocks}, Message::Tool{call_id:first,result:a,..}, Message::Tool{call_id:second,result:b,..}] if blocks == &[call("one"),call("two")] && first == "one" && second == "two" && a.is_error && b.is_error)
    );
    Ok(())
}

#[tokio::test]
async fn returned_tool_error_preserves_result_and_stops_batch() -> Result<()> {
    let f = Fixture::new(
        scripted(vec![vec![call("reported-error"), call("later")]]),
        Store::default(),
    );
    assert_failed(&collect(f.engine.run(options())?).await);
    assert_eq!(
        f.tools.actions.lock().unwrap().as_slice(),
        ["review reported-error", "execute reported-error"]
    );
    assert_eq!(f.transport.requests.lock().unwrap().len(), 1);
    let saved = f.store.load("session").await?;
    assert!(
        matches!(saved.messages.as_slice(),[Message::User{..}, Message::Assistant{..},Message::Tool{call_id,result,..},Message::Tool{call_id:later,result:skipped,..}] if call_id == "reported-error" && result.is_error && result.content == [text("tool reported an error")] && later == "later" && skipped.is_error)
    );
    Ok(())
}

#[tokio::test]
async fn usage_and_thinking_are_archived_and_images_survive_history_projection() -> Result<()> {
    let usage = Usage {
        input: 31,
        output: 11,
        cache_read: 5,
        cache_write: 2,
    };
    let thinking = Block::Thinking {
        text: "scratch".into(),
    };
    let image = Block::Image {
        mime_type: "image/png".into(),
        data: "aW1n".into(),
    };
    let blocks = vec![thinking, image.clone(), answer("reply")];
    let f = Fixture::new(
        Transport {
            usage: Some(usage),
            ..scripted(vec![blocks.clone(), vec![answer("continued")]])
        },
        Store::default(),
    );
    let system = Message::System {
        content: vec![
            text("system"),
            Content::Image {
                mime_type: "image/png".into(),
                data: "c3lz".into(),
            },
        ],
    };
    f.store
        .append("session", "0", &[Entry::Message(system.clone())])
        .await?;
    let events = collect(f.engine.run(options())?).await;
    assert!(matches!(
        events.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    assert!(events.contains(&Event::Usage { usage }));
    assert!(events.contains(&Event::Thinking {
        block_id: "thinking".into(),
        delta: "scratch".into()
    }));
    let saved = f.store.entries.lock().unwrap().clone();
    assert_eq!(
        saved.iter().filter(|e| **e == Entry::Usage(usage)).count(),
        1
    );
    assert!(saved.contains(&Entry::Message(Message::Assistant { blocks })));
    let rx = f.engine.run(RunOptions {
        run_id: "next".into(),
        ..options()
    })?;
    assert!(matches!(
        collect(rx).await.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    let requests = f.transport.requests.lock().unwrap();
    let user = Message::User {
        content: options().content,
    };
    assert_eq!(
        requests[1].messages,
        [
            system,
            user.clone(),
            Message::Assistant {
                blocks: vec![image, answer("reply")]
            },
            user
        ]
    );
    Ok(())
}
