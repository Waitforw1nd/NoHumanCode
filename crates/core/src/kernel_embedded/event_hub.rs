//! Per-run event broadcast and bounded 4096-entry replay ring (`codaKernel/1 §6.2–6.4`).

use crate::kernel_embedded::EVENT_REPLAY_PER_RUN;
use crate::runtime_seams::EventSink;
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::sync::broadcast;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequencedKernelEvent {
    pub seq: u64,
    pub session_id: String,
    pub run_id: String,
    pub event: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventReplayError {
    Expired,
}

struct HubInner {
    ring: VecDeque<SequencedKernelEvent>,
    evicted_through: u64,
    next_seq: u64,
    finished: bool,
}

pub struct RunEventHub {
    session_id: String,
    run_id: String,
    inner: Mutex<HubInner>,
    tx: broadcast::Sender<SequencedKernelEvent>,
}

impl RunEventHub {
    pub fn new(session_id: impl Into<String>, run_id: impl Into<String>) -> Self {
        let (tx, _) = broadcast::channel(EVENT_REPLAY_PER_RUN);
        Self {
            session_id: session_id.into(),
            run_id: run_id.into(),
            inner: Mutex::new(HubInner {
                ring: VecDeque::with_capacity(256),
                evicted_through: 0,
                next_seq: 1,
                finished: false,
            }),
            tx,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn emit(&self, event: Value) -> Option<SequencedKernelEvent> {
        let mut inner = self.inner.lock().unwrap();
        if inner.finished {
            return None;
        }
        let is_done = event.get("type").and_then(Value::as_str) == Some("done");
        let seq = inner.next_seq;
        inner.next_seq += 1;
        if is_done {
            inner.finished = true;
        }
        let envelope = SequencedKernelEvent {
            seq,
            session_id: self.session_id.clone(),
            run_id: self.run_id.clone(),
            event,
        };
        if inner.ring.len() >= EVENT_REPLAY_PER_RUN
            && let Some(popped) = inner.ring.pop_front()
        {
            inner.evicted_through = popped.seq;
        }
        inner.ring.push_back(envelope.clone());
        let _ = self.tx.send(envelope.clone());
        Some(envelope)
    }

    pub fn is_finished(&self) -> bool {
        self.inner.lock().unwrap().finished
    }

    pub fn subscribe(
        &self,
        after: u64,
    ) -> Result<
        (
            Vec<SequencedKernelEvent>,
            broadcast::Receiver<SequencedKernelEvent>,
            bool,
        ),
        EventReplayError,
    > {
        let inner = self.inner.lock().unwrap();
        if after < inner.evicted_through {
            return Err(EventReplayError::Expired);
        }
        let rx = self.tx.subscribe();
        let replay: Vec<SequencedKernelEvent> = inner
            .ring
            .iter()
            .filter(|ev| ev.seq > after)
            .cloned()
            .collect();
        let already_done = inner.finished
            && (replay
                .last()
                .is_some_and(|e| e.event.get("type").and_then(Value::as_str) == Some("done"))
                || inner.ring.back().is_some_and(|e| {
                    e.seq <= after && e.event.get("type").and_then(Value::as_str) == Some("done")
                }));
        Ok((replay, rx, already_done))
    }
}

/// Minimal `EventSink` bridge for `agent_loop` calls; embedded turn deltas/events
/// are emitted directly via `RunEventHub` by `EmbeddedTurnController`.
pub struct NoopEmbeddedEventSink;

impl EventSink for NoopEmbeddedEventSink {
    fn completion_delta(&self, _task_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }

    fn reasoning_delta(&self, _task_id: &str, _text: &str) -> Result<()> {
        Ok(())
    }

    fn emit_event(&self, _task_id: &str, _kind: &str, _data: Value) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_hub_numbers_monotonically_stops_after_done_and_evicts_past_4096() {
        let hub = RunEventHub::new("s-1", "run-1");
        for i in 1..=(EVENT_REPLAY_PER_RUN as u64 + 10) {
            let ev = hub
                .emit(json!({"type": "text", "blockId": "b-1", "delta": format!("{i}")}))
                .unwrap();
            assert_eq!(ev.seq, i);
        }
        assert!(matches!(hub.subscribe(0), Err(EventReplayError::Expired)));
        assert!(matches!(hub.subscribe(9), Err(EventReplayError::Expired)));
        let (replay, _rx, done) = hub.subscribe(10).unwrap();
        assert!(!done);
        assert_eq!(replay.len(), EVENT_REPLAY_PER_RUN);
        assert_eq!(replay.first().unwrap().seq, 11);

        let done_ev = hub
            .emit(json!({"type": "done", "reason": "completed"}))
            .unwrap();
        assert_eq!(done_ev.seq, EVENT_REPLAY_PER_RUN as u64 + 11);
        assert!(
            hub.emit(json!({"type": "text", "blockId": "b-1", "delta": "late"}))
                .is_none()
        );
        let (_replay2, _rx2, already_done) = hub.subscribe(done_ev.seq).unwrap();
        assert!(already_done);
    }
}
