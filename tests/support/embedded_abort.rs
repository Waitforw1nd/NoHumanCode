use super::*;

#[tokio::test]
async fn streaming_abort_survives_reload_and_separates_the_next_user_message() -> Result<()> {
    let f = Fixture::new(
        Transport {
            pending: true,
            partial: vec![
                ModelDelta::Text {
                    block_id: "a".into(),
                    delta: "unfinished".into(),
                },
                ModelDelta::Thinking {
                    block_id: "b".into(),
                    delta: "private".into(),
                },
                ModelDelta::Text {
                    block_id: "a".into(),
                    delta: " draft".into(),
                },
            ],
            ..Default::default()
        },
        Store::default(),
    );
    let rx = f.engine.run(options())?;
    timeout(LIMIT, f.transport.started.notified()).await?;
    f.transport.cleanup.notify_one();
    timeout(LIMIT, f.engine.abort("session", "run")).await?;
    assert_aborted(&collect(rx).await);
    let saved = f.store.load("session").await?;
    assert_eq!(
        saved.messages.len(),
        2,
        "cancelled user input needs a durable assistant boundary"
    );
    assert!(
        matches!(&saved.messages[1], Message::Aborted { run_id, partial } if run_id == "run" && partial == &[answer("unfinished draft"), Block::Thinking { text: "private".into() }])
    );
    assert_reloaded_abort_projection(&f).await
}

#[tokio::test]
async fn abort_marker_conflict_reports_failure_instead_of_acknowledging_an_unsaved_stop()
-> Result<()> {
    let f = Fixture::new(
        Transport {
            pending: true,
            ..Default::default()
        },
        Store {
            conflict: Some(WriteKind::Abort),
            ..Default::default()
        },
    );
    let rx = f.engine.run(options())?;
    timeout(LIMIT, f.transport.started.notified()).await?;
    f.transport.cleanup.notify_one();
    timeout(LIMIT, f.engine.abort("session", "run")).await?;
    let events = collect(rx).await;
    assert_failed(&events);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Error { code, .. } if code == "SESSION_APPEND"))
    );
    assert_eq!(*f.store.rejected.lock().unwrap(), 1);
    assert!(matches!(
        f.store.load("session").await?.messages.as_slice(),
        [Message::User { .. }]
    ));
    Ok(())
}

pub(super) async fn assert_reloaded_abort_projection(f: &Fixture) -> Result<()> {
    let transport = std::sync::Arc::new(scripted(vec![vec![answer("new answer")]]));
    let persisted = serde_json::to_vec(&*f.store.entries.lock().unwrap())?;
    let reopened = Store {
        entries: std::sync::Mutex::new(serde_json::from_slice(&persisted)?),
        ..Default::default()
    };
    let engine = EmbeddedEngine::new(
        transport.clone(),
        f.tools.clone(),
        std::sync::Arc::new(reopened),
    );
    let next = RunOptions {
        run_id: "next".into(),
        content: vec![text("new question")],
        ..options()
    };
    assert!(matches!(
        collect(engine.run(next)?).await.last(),
        Some(Event::Done {
            reason: DoneReason::Completed
        })
    ));
    let mut history = transport.requests.lock().unwrap()[0].messages.clone();
    assert_eq!(
        history.pop(),
        Some(Message::User {
            content: vec![text("new question")]
        })
    );
    let Some(Message::Assistant { blocks }) = history.last_mut() else {
        panic!("missing assistant boundary")
    };
    assert!(
        matches!(blocks.pop(), Some(Block::Text {text}) if text.contains("cancelled") && text.contains("user"))
    );
    if blocks.is_empty() {
        history.pop();
    }
    // Removing the model-only marker must recover *all* committed history, including
    // a concurrently saved final reply or each tool intent and correlated outcome.
    let mut archived = f.store.load("session").await?.messages;
    assert!(matches!(archived.pop(), Some(Message::Aborted { .. })));
    assert_eq!(history, archived);
    Ok(())
}

#[tokio::test]
async fn abort_around_first_user_append_only_closes_committed_input() -> Result<()> {
    for pause_load in [true, false] {
        let f = Fixture::new(
            Transport::default(),
            Store {
                pause_load,
                pause_user_append: !pause_load,
                ..Default::default()
            },
        );
        let rx = f.engine.run(options())?;
        timeout(LIMIT, f.store.append_started.notified()).await?;
        let abort = f.engine.abort("session", "run");
        tokio::pin!(abort);
        assert!(futures_util::poll!(&mut abort).is_pending());
        f.store.release_append.notify_one();
        timeout(LIMIT, abort).await?;
        assert_aborted(&collect(rx).await);
        assert!(f.transport.requests.lock().unwrap().is_empty());
        let entries = f.store.entries.lock().unwrap();
        if pause_load {
            assert!(entries.is_empty());
        } else {
            assert!(
                matches!(entries.as_slice(), [Entry::Message(Message::User {..}), Entry::Message(Message::Aborted { run_id, partial })] if run_id == "run" && partial.is_empty())
            );
        }
    }
    Ok(())
}
