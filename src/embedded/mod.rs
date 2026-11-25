//! In-process model loop. All external effects are supplied by the embedding host.
mod partial;
mod turn;

use anyhow::{Context, Result, ensure};
pub use coda_protocol::embedded::*;
use futures_util::FutureExt;
pub use futures_util::future::BoxFuture;
use std::{
    collections::{HashMap, HashSet},
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, watch};
pub use tokio_util::sync::CancellationToken;

pub trait ModelTransport: Send + Sync {
    /// Local validation of the actual request, including route, capabilities and token budget.
    fn preflight(&self, request: &ModelRequest) -> Result<()>;
    /// Return after any network/stream work has stopped. Only return a complete reply on success.
    fn complete<'a>(
        &'a self,
        request: &'a ModelRequest,
        emit: &'a (dyn Fn(ModelDelta) -> Result<()> + Send + Sync),
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ModelReply>>;
}

pub trait ToolHost: Send + Sync {
    fn definitions(&self, session_id: &str, run_id: &str) -> Result<Vec<ToolDefinition>>;
    fn review<'a>(
        &'a self,
        call: &'a Invocation,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<bool>>;
    /// Return tool failures as `Ok` with `is_error` and diagnostic content for the model.
    /// Return `Err` if the host cannot deliver a tool result and the turn must stop.
    fn execute<'a>(
        &'a self,
        call: &'a Invocation,
        update: &'a (dyn Fn(ToolResult) -> Result<()> + Send + Sync),
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolResult>>;
}

pub trait SessionStore: Send + Sync {
    /// Load the selected branch projection, including tool errors and the latest summary.
    fn load<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Result<Snapshot>>;
    /// Atomically append the entire batch iff the revision still matches. Return the new leaf.
    fn append<'a>(
        &'a self,
        session_id: &'a str,
        expected_revision: &'a str,
        entries: &'a [Entry],
    ) -> BoxFuture<'a, Result<String>>;
}

#[derive(Clone)]
pub struct EmbeddedEngine {
    model: Arc<dyn ModelTransport>,
    tools: Arc<dyn ToolHost>,
    store: Arc<dyn SessionStore>,
    registry: Arc<Mutex<Registry>>,
}

#[derive(Default)]
struct Registry {
    active: HashMap<String, Active>,
    used: HashSet<String>,
}

struct Active {
    id: Option<String>,
    cancel: Option<CancellationToken>,
    finished: watch::Receiver<bool>,
}

/// Release even if a host panics or the Tokio runtime drops the worker.
struct Lease {
    engine: EmbeddedEngine,
    session: String,
    cancel: CancellationToken,
    finished: watch::Sender<bool>,
    terminal: Option<(mpsc::UnboundedSender<Event>, DoneReason)>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut registry = self.engine.registry.lock().expect("run registry poisoned");
        registry.active.remove(&self.session);
        if let Some((events, reason)) = self.terminal.take() {
            let _ = events.send(Event::Done { reason });
        }
        self.finished.send_replace(true);
    }
}

impl EmbeddedEngine {
    pub fn new(
        model: Arc<dyn ModelTransport>,
        tools: Arc<dyn ToolHost>,
        store: Arc<dyn SessionStore>,
    ) -> Self {
        Self {
            model,
            tools,
            store,
            registry: Default::default(),
        }
    }

    fn claim(&self, session: &str, run_id: Option<&str>) -> Result<Lease> {
        ensure!(!session.trim().is_empty(), "empty session id");
        let mut registry = self.registry.lock().expect("run registry poisoned");
        ensure!(!registry.active.contains_key(session), "session busy");
        if let Some(id) = run_id {
            ensure!(
                !id.trim().is_empty() && registry.used.insert(id.to_owned()),
                "empty or reused run id"
            );
        }
        let (tx, rx) = watch::channel(false);
        let cancel = CancellationToken::new();
        registry.active.insert(
            session.to_owned(),
            Active {
                id: run_id.map(str::to_owned),
                cancel: Some(cancel.clone()),
                finished: rx,
            },
        );
        Ok(Lease {
            engine: self.clone(),
            session: session.into(),
            cancel,
            finished: tx,
            terminal: None,
        })
    }

    /// Start on the calling Tokio runtime. Dropping the event receiver does not cancel the run.
    pub fn run(&self, options: RunOptions) -> Result<mpsc::UnboundedReceiver<Event>> {
        let runtime =
            tokio::runtime::Handle::try_current().context("a Tokio runtime is required")?;
        ensure!(options.max_requests > 0, "max_requests must be positive");
        let mut lease = self.claim(&options.session_id, Some(&options.run_id))?;
        let (tx, rx) = mpsc::unbounded_channel();
        runtime.spawn(async move {
            let _ = tx.send(Event::Start {
                model: options.model.clone(),
            });
            let events = turn::Events::new(&tx);
            let outcome = AssertUnwindSafe(lease.engine.turn(&options, &events, &lease.cancel))
                .catch_unwind()
                .await;
            let reason = match outcome {
                Ok(Ok(())) => DoneReason::Completed,
                Ok(Err(turn::Failure::Aborted)) => DoneReason::Aborted,
                failure => {
                    let code = match failure {
                        Ok(Err(turn::Failure::Failed(code))) => code,
                        _ => "HOST_PANIC",
                    };
                    let _ = tx.send(Event::Error {
                        code: code.to_owned(),
                        message: code.replace('_', " ").to_lowercase(),
                    });
                    DoneReason::Failed
                }
            };
            lease.terminal = Some((tx, reason));
            drop(lease);
        });
        Ok(rx)
    }

    // Linearize success vs. abort under the same lock as abort(). Later callers still wait
    // for the final history append and lease release, but cannot change the decided outcome.
    fn close_abort_window(&self, session_id: &str) -> bool {
        self.registry
            .lock()
            .expect("run registry poisoned")
            .active
            .get_mut(session_id)
            .and_then(|active| active.cancel.take())
            .is_some_and(|cancel| cancel.is_cancelled())
    }

    /// Signal the matching run and wait for its callbacks, writes and session claim to finish.
    pub async fn abort(&self, session_id: &str, run_id: &str) {
        let mut done = {
            let registry = self.registry.lock().expect("run registry poisoned");
            let Some(active) = registry
                .active
                .get(session_id)
                .filter(|active| active.id.as_deref() == Some(run_id))
            else {
                return;
            };
            if let Some(cancel) = &active.cancel {
                cancel.cancel();
            }
            active.finished.clone()
        };
        let _ = done.wait_for(|finished| *finished).await;
    }

    /// Summarize the complete selected projection with the host model. Success returns the appended leaf.
    pub async fn compact(
        &self,
        session_id: &str,
        model: Model,
        instructions: &str,
    ) -> Result<String> {
        let runtime =
            tokio::runtime::Handle::try_current().context("a Tokio runtime is required")?;
        let lease = self.claim(session_id, None)?;
        let instructions = instructions.to_owned();
        runtime
            .spawn(summarize(lease, model, instructions))
            .await
            .context("compaction worker stopped")?
    }
}

async fn summarize(lease: Lease, model: Model, instructions: String) -> Result<String> {
    let snapshot = lease.engine.store.load(&lease.session).await?;
    turn::validate_history(&snapshot.messages)
        .map_err(|_| anyhow::anyhow!("invalid or unfinished tool history"))?;
    ensure!(!snapshot.messages.is_empty(), "empty session");
    let mut messages = turn::model_projection(snapshot.messages);
    let prompt = format!(
        "Summarize this conversation for continuation. Preserve decisions, tool outcomes and remaining work.\n{instructions}"
    );
    messages.push(Message::User {
        content: vec![Content::Text { text: prompt }],
    });
    let request = ModelRequest {
        session_id: lease.session.clone(),
        run_id: None,
        settings: ModelSettings::default(),
        model,
        system_prompt: "Produce a factual conversation summary.".into(),
        messages,
        tools: vec![],
    };
    lease.engine.model.preflight(&request)?;
    let reply = lease
        .engine
        .model
        .complete(&request, &|_| Ok(()), lease.cancel.child_token())
        .await?;
    let mut summary = String::new();
    for block in reply.blocks {
        match block {
            Block::Text { text } => summary.push_str(&text),
            Block::Thinking { .. } => (),
            Block::ToolCall { .. } | Block::Image { .. } => {
                anyhow::bail!("expected a text summary")
            }
        }
    }
    ensure!(!summary.trim().is_empty(), "empty compaction summary");
    let mut entries = Vec::new();
    if let Some(usage) = reply.usage {
        entries.push(Entry::Usage(usage));
    }
    entries.push(Entry::Compaction { summary });
    lease
        .engine
        .store
        .append(&lease.session, &snapshot.revision, &entries)
        .await
}
