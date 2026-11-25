use anyhow::{Result, bail, ensure};
use coda::embedded::*;
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, mpsc::UnboundedReceiver};

pub(super) const LIMIT: Duration = Duration::from_secs(5);

// Embedded-only builds enable Tokio's runtime and channels without its timer driver.
pub(super) async fn timeout<F: Future>(limit: Duration, operation: F) -> Result<F::Output> {
    let (stop, wait) = std::sync::mpsc::channel::<()>();
    let watchdog = tokio::task::spawn_blocking(move || wait.recv_timeout(limit));
    let result = tokio::select! {
        value = operation => Ok(value),
        _ = watchdog => Err(anyhow::anyhow!("operation timed out after {limit:?}")),
    };
    drop(stop);
    result
}

pub(super) fn model() -> Model {
    Model {
        id: "injected".into(),
        provider: Some("custom".into()),
    }
}
pub(super) fn text(s: &str) -> Content {
    Content::Text { text: s.into() }
}
pub(super) fn answer(s: &str) -> Block {
    Block::Text { text: s.into() }
}
pub(super) fn options() -> RunOptions {
    RunOptions {
        session_id: "session".into(),
        run_id: "run".into(),
        model: model(),
        system_prompt: "rules".into(),
        content: vec![text("question")],
        max_requests: 4,
        settings: Default::default(),
    }
}
pub(super) fn call(id: &str) -> Block {
    Block::ToolCall {
        call: ToolCall {
            id: id.into(),
            name: "lookup".into(),
            args: Default::default(),
        },
    }
}

#[derive(Default)]
pub(super) struct Transport {
    pub(super) replies: Mutex<VecDeque<Vec<Block>>>,
    pub(super) requests: Mutex<Vec<ModelRequest>>,
    pub(super) reject: bool,
    pub(super) pending: bool,
    pub(super) usage: Option<Usage>,
    pub(super) started: Notify,
    pub(super) release: Notify,
    pub(super) cancelled: Notify,
    pub(super) cleanup: Notify,
}
impl ModelTransport for Transport {
    fn preflight(&self, _: &ModelRequest) -> Result<()> {
        ensure!(!self.reject, "preflight rejected");
        Ok(())
    }
    fn complete<'a>(
        &'a self,
        request: &'a ModelRequest,
        emit: &'a (dyn Fn(ModelDelta) -> Result<()> + Send + Sync),
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ModelReply>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            self.started.notify_one();
            if self.pending {
                tokio::select! {
                    () = self.release.notified() => {},
                    () = cancel.cancelled() => { self.cancelled.notify_one(); self.cleanup.notified().await; bail!("cancelled"); }
                }
            }
            let blocks = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected model request");
            for block in &blocks {
                if let Block::Thinking { text } = block {
                    emit(ModelDelta::Thinking {
                        block_id: "thinking".into(),
                        delta: text.clone(),
                    })?;
                }
                if let Block::Text { text } = block {
                    emit(ModelDelta::Text {
                        block_id: "block".into(),
                        delta: text.clone(),
                    })?;
                }
            }
            Ok(ModelReply {
                blocks,
                usage: self.usage,
            })
        })
    }
}

#[derive(Default)]
pub(super) struct Tools {
    pub(super) actions: Mutex<Vec<String>>,
    pub(super) blocked: Notify,
}
impl ToolHost for Tools {
    fn definitions(&self, _: &str, _: &str) -> Result<Vec<ToolDefinition>> {
        Ok(vec![ToolDefinition {
            name: "lookup".into(),
            description: "test tool".into(),
            parameters: Default::default(),
        }])
    }
    fn review<'a>(
        &'a self,
        invocation: &'a Invocation,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            assert_eq!(invocation.session_id, "session");
            assert_eq!(invocation.run_id, "run");
            self.actions
                .lock()
                .unwrap()
                .push(format!("review {}", invocation.call.id));
            if invocation.call.id == "wait-review" {
                self.blocked.notify_one();
                cancel.cancelled().await;
                bail!("review interrupted");
            }
            Ok(invocation.call.id != "denied")
        })
    }
    fn execute<'a>(
        &'a self,
        invocation: &'a Invocation,
        update: &'a (dyn Fn(ToolResult) -> Result<()> + Send + Sync),
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolResult>> {
        Box::pin(async move {
            self.actions
                .lock()
                .unwrap()
                .push(format!("execute {}", invocation.call.id));
            if invocation.call.id == "wait-execute" {
                self.blocked.notify_one();
                cancel.cancelled().await;
                bail!("execute interrupted");
            }
            if invocation.call.id == "flood" {
                let progress = ToolResult {
                    content: vec![Content::Text {
                        text: "p".repeat(1024 * 1024),
                    }],
                    details: Value::Null,
                    is_error: false,
                };
                let mut reached_limit = false;
                for _ in 0..128 {
                    if update(progress.clone()).is_err() {
                        reached_limit = true;
                        break;
                    }
                    // Let the receiver discard each large progress event immediately.
                    tokio::task::yield_now().await;
                }
                assert!(reached_limit, "updates never exhausted the event budget");
            }
            ensure!(invocation.call.id != "broken", "tool unavailable");
            Ok(ToolResult {
                content: vec![text(if invocation.call.id == "reported-error" {
                    "tool reported an error"
                } else {
                    "found"
                })],
                details: Value::Null,
                is_error: invocation.call.id == "reported-error",
            })
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum WriteKind {
    User,
    Intent,
    Result,
}
fn write_kind(entry: &Entry) -> Option<WriteKind> {
    match entry {
        Entry::Message(Message::User { .. }) => Some(WriteKind::User),
        Entry::Message(Message::Assistant { blocks })
            if blocks.iter().any(|b| matches!(b, Block::ToolCall { .. })) =>
        {
            Some(WriteKind::Intent)
        }
        Entry::Message(Message::Tool { .. }) => Some(WriteKind::Result),
        _ => None,
    }
}
#[derive(Default)]
pub(super) struct Store {
    pub(super) entries: Mutex<Vec<Entry>>,
    pub(super) conflict: Option<WriteKind>,
    pub(super) rejected: Mutex<usize>,
    pub(super) assistant_saved: Notify,
    pub(super) pause_final_reply: bool,
    pub(super) append_started: Notify,
    pub(super) release_append: Notify,
}
impl SessionStore for Store {
    fn load<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Snapshot>> {
        Box::pin(async move {
            assert_eq!(session_id, "session");
            let entries = self.entries.lock().unwrap();
            let mut messages = vec![];
            for entry in entries.iter() {
                match entry {
                    Entry::Message(message) => messages.push(message.clone()),
                    Entry::Compaction { summary } => {
                        messages = vec![Message::Summary {
                            text: summary.clone(),
                        }]
                    }
                    _ => {}
                }
            }
            Ok(Snapshot {
                revision: entries.len().to_string(),
                messages,
            })
        })
    }
    fn append<'a>(
        &'a self,
        session_id: &'a str,
        expected_revision: &'a str,
        entries: &'a [Entry],
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            assert_eq!(session_id, "session");
            if self.pause_final_reply && entries.iter().any(|entry| matches!(entry,Entry::Message(Message::Assistant{blocks}) if blocks.iter().all(|block| matches!(block,Block::Text{..})))) {
                self.append_started.notify_one();
                self.release_append.notified().await;
            }
            let mut saved = self.entries.lock().unwrap();
            if self
                .conflict
                .is_some_and(|kind| entries.iter().any(|e| write_kind(e) == Some(kind)))
            {
                *self.rejected.lock().unwrap() += 1;
                bail!("revision conflict");
            }
            ensure!(
                expected_revision == saved.len().to_string(),
                "revision conflict"
            );
            saved.extend_from_slice(entries);
            if entries
                .iter()
                .any(|e| matches!(e, Entry::Message(Message::Assistant { .. })))
            {
                self.assistant_saved.notify_one();
            }
            Ok(saved.len().to_string())
        })
    }
}

pub(super) struct Fixture {
    pub(super) engine: EmbeddedEngine,
    pub(super) transport: Arc<Transport>,
    pub(super) tools: Arc<Tools>,
    pub(super) store: Arc<Store>,
}
impl Fixture {
    pub(super) fn new(transport: Transport, store: Store) -> Self {
        let transport = Arc::new(transport);
        let tools = Arc::new(Tools::default());
        let store = Arc::new(store);
        let engine = EmbeddedEngine::new(transport.clone(), tools.clone(), store.clone());
        Self {
            engine,
            transport,
            tools,
            store,
        }
    }
}
pub(super) fn scripted(replies: Vec<Vec<Block>>) -> Transport {
    Transport {
        replies: Mutex::new(replies.into()),
        ..Default::default()
    }
}
pub(super) async fn collect(mut rx: UnboundedReceiver<Event>) -> Vec<Event> {
    timeout(LIMIT, async {
        let mut events = vec![];
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    })
    .await
    .expect("event stream did not close")
}
