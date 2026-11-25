use super::{Event, SteeringMessage, mpsc, watch};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

const MAX_ITEMS: usize = 1024;
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub enum EnqueueError {
    Full(SteeringMessage),
    Closed(SteeringMessage),
}

impl EnqueueError {
    pub fn into_message(self) -> SteeringMessage {
        match self {
            Self::Full(message) | Self::Closed(message) => message,
        }
    }
}

/// An interrupted append whose outcome must be checked against the host's store.
#[derive(Debug, PartialEq)]
pub struct UnconfirmedInput {
    pub expected_revision: String,
    pub messages: Vec<SteeringMessage>,
}

struct PayloadCounter(usize);

impl Write for PayloadCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES - self.0 {
            return Err(io::Error::other("steering payload exceeds capacity"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct RunningTurn {
    pub input: TurnInput,
    pub events: mpsc::UnboundedReceiver<Event>,
}

/// A FIFO bound to one run. Keep a clone until undelivered input has been reclaimed.
#[derive(Clone)]
pub struct TurnInput {
    queue: Arc<Mutex<Queue>>,
    finished: watch::Receiver<bool>,
}

#[derive(Default)]
struct Queue {
    pending: Vec<SteeringMessage>,
    pending_bytes: usize,
    // Includes pending, claimed and unconfirmed inputs until ownership leaves the queue.
    total_items: usize,
    total_bytes: usize,
    unconfirmed: Option<(UnconfirmedInput, usize)>,
    closed: bool,
}

impl TurnInput {
    pub(super) fn new(finished: watch::Receiver<bool>) -> Self {
        Self {
            queue: Arc::default(),
            finished,
        }
    }

    /// Accept up to 1024 outstanding inputs and 64 MiB of combined serialized JSON.
    pub fn enqueue(&self, message: SteeringMessage) -> Result<(), EnqueueError> {
        let mut size = PayloadCounter(0);
        let measured = serde_json::to_writer(&mut size, &message);
        let mut queue = self.queue.lock().expect("input queue poisoned");
        if queue.closed {
            return Err(EnqueueError::Closed(message));
        }
        if measured.is_err()
            || queue.total_items == MAX_ITEMS
            || size.0 > MAX_BYTES - queue.total_bytes
        {
            return Err(EnqueueError::Full(message));
        }
        queue.total_items += 1;
        queue.total_bytes += size.0;
        queue.pending_bytes += size.0;
        queue.pending.push(message);
        Ok(())
    }

    /// Snapshot of unclaimed messages in enqueue order.
    pub fn pending(&self) -> Vec<SteeringMessage> {
        self.queue
            .lock()
            .expect("input queue poisoned")
            .pending
            .clone()
    }

    /// Take currently unclaimed messages. An admitted batch belongs to the worker until CAS settles.
    pub fn clear_queue(&self) -> Vec<SteeringMessage> {
        let mut queue = self.queue.lock().expect("input queue poisoned");
        queue.total_bytes -= queue.pending_bytes;
        queue.total_items -= queue.pending.len();
        queue.pending_bytes = 0;
        std::mem::take(&mut queue.pending)
    }

    /// Wait for this run to release its session and take all remaining input once across clones.
    pub async fn take_remaining(&self) -> Vec<SteeringMessage> {
        let mut done = self.finished.clone();
        let _ = done.wait_for(|finished| *finished).await;
        self.clear_queue()
    }

    /// After the run finishes, take its uncertain append once across all handle clones.
    pub async fn take_unconfirmed(&self) -> Option<UnconfirmedInput> {
        let mut done = self.finished.clone();
        let _ = done.wait_for(|finished| *finished).await;
        let mut queue = self.queue.lock().expect("input queue poisoned");
        let (receipt, bytes) = queue.unconfirmed.take()?;
        queue.total_items -= receipt.messages.len();
        queue.total_bytes -= bytes;
        Some(receipt)
    }

    pub(super) fn close(&self) {
        self.queue.lock().expect("input queue poisoned").closed = true;
    }

    /// After a text reply, admit a continuation or atomically close an empty queue.
    pub(super) fn claim(&self, request_required: bool) -> Option<InputBatch<'_>> {
        let mut queue = self.queue.lock().expect("input queue poisoned");
        if !request_required && queue.pending.is_empty() {
            queue.closed = true;
            return None;
        }
        Some(InputBatch {
            input: self,
            bytes: std::mem::take(&mut queue.pending_bytes),
            expected_revision: None,
            messages: std::mem::take(&mut queue.pending),
        })
    }
}

/// Own the admitted prefix while the host validates and commits it. Restore it ahead of later
/// arrivals on definite failure. An interrupted append is retained for host reconciliation.
pub(super) struct InputBatch<'a> {
    input: &'a TurnInput,
    bytes: usize,
    expected_revision: Option<String>,
    pub messages: Vec<SteeringMessage>,
}

impl InputBatch<'_> {
    pub fn begin_append(&mut self, expected_revision: &str) {
        if !self.messages.is_empty() {
            self.expected_revision = Some(expected_revision.to_owned());
        }
    }

    pub fn not_committed(&mut self) {
        self.expected_revision = None;
    }

    pub fn commit(mut self) -> Vec<SteeringMessage> {
        let mut queue = self.input.queue.lock().expect("input queue poisoned");
        queue.total_items -= self.messages.len();
        queue.total_bytes -= self.bytes;
        std::mem::take(&mut self.messages)
    }
}

impl Drop for InputBatch<'_> {
    fn drop(&mut self) {
        if !self.messages.is_empty() {
            let mut queue = self.input.queue.lock().expect("input queue poisoned");
            if let Some(expected_revision) = self.expected_revision.take() {
                // A run has one worker. An interrupted append terminates that worker.
                let receipt = UnconfirmedInput {
                    expected_revision,
                    messages: std::mem::take(&mut self.messages),
                };
                queue.unconfirmed = Some((receipt, self.bytes));
                return;
            }
            queue.pending_bytes += self.bytes;
            self.messages.append(&mut queue.pending);
            queue.pending = std::mem::take(&mut self.messages);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coda_protocol::embedded::Content;

    fn input() -> TurnInput {
        TurnInput::new(watch::channel(false).1)
    }

    fn text(value: String) -> SteeringMessage {
        SteeringMessage::User {
            content: vec![Content::Text { text: value }],
        }
    }

    fn assert_full(input: &TurnInput, value: &str) {
        let err = input.enqueue(text(value.into())).unwrap_err();
        assert!(matches!(err, EnqueueError::Full(_)));
        assert_eq!(err.into_message(), text(value.into()));
    }

    #[test]
    fn claimed_items_keep_slots_until_committed_or_reclaimed() {
        let input = input();
        for _ in 0..1023 {
            input.enqueue(text(String::new())).unwrap();
        }
        let batch = input.claim(true).unwrap();
        input.enqueue(text("later".into())).unwrap();
        assert_full(&input, "overflow");
        assert_eq!(input.clear_queue(), vec![text("later".into())]);
        input.enqueue(text("last".into())).unwrap();
        drop(batch);
        assert_full(&input, "after rollback");
        let committed = input.claim(true).unwrap().commit();
        assert_eq!(committed.len(), 1024);
        assert_eq!(committed.last(), Some(&text("last".into())));
        for _ in 0..1024 {
            input.enqueue(text(String::new())).unwrap();
        }
        assert_full(&input, "after refill");
    }

    #[test]
    fn byte_limit_counts_serialized_claimed_and_pending_payloads() {
        let input = input();
        let small = text("\n雪".into());
        let small_bytes = serde_json::to_vec(&small).unwrap().len();
        let overhead = serde_json::to_vec(&text(String::new())).unwrap().len();
        let large = text("x".repeat(64 * 1024 * 1024 - overhead - small_bytes));
        input.enqueue(large).unwrap();
        let batch = input.claim(true).unwrap();
        input.enqueue(small.clone()).unwrap();
        assert_full(&input, "");
        assert_eq!(input.clear_queue(), vec![small.clone()]);
        assert_full(&input, "too long for gap");
        drop(batch);
        input.enqueue(small.clone()).unwrap();
        assert_full(&input, "");
        let mut committed = input.claim(true).unwrap().commit();
        assert_eq!(committed.pop(), Some(small.clone()));
        input.enqueue(committed.pop().unwrap()).unwrap();
        input.enqueue(small).unwrap();
    }

    #[tokio::test]
    async fn uncertain_append_is_reclaimed_separately_once_after_finish() {
        let (done, rx) = watch::channel(false);
        let input = TurnInput::new(rx);
        let msg = text("claimed".into());
        input.enqueue(msg.clone()).unwrap();
        let mut batch = input.claim(true).unwrap();
        batch.begin_append("rev");
        input.enqueue(text("later".into())).unwrap();
        drop(batch);
        assert!(futures_util::poll!(Box::pin(input.take_unconfirmed())).is_pending());
        done.send(true).unwrap();
        assert_eq!(input.take_remaining().await, vec![text("later".into())]);
        let uncertain = input.take_unconfirmed().await.unwrap();
        assert_eq!(uncertain.expected_revision, "rev");
        assert_eq!(uncertain.messages, vec![msg]);
        assert!(input.clone().take_unconfirmed().await.is_none());
    }

    #[tokio::test]
    async fn definite_failure_restores_prefix_and_empty_append_leaves_no_receipt() {
        let (done, rx) = watch::channel(false);
        let input = TurnInput::new(rx);
        let mut empty = input.claim(true).unwrap();
        empty.begin_append("empty");
        drop(empty);
        input.enqueue(text("first".into())).unwrap();
        let mut batch = input.claim(true).unwrap();
        batch.begin_append("rev");
        batch.not_committed();
        input.enqueue(text("second".into())).unwrap();
        drop(batch);
        assert_eq!(
            input.clear_queue(),
            vec![text("first".into()), text("second".into())]
        );
        done.send(true).unwrap();
        assert!(input.take_unconfirmed().await.is_none());
    }

    #[test]
    fn oversized_and_closed_errors_return_original_payload() {
        let input = input();
        let size = 64 * 1024 * 1024 + 1 - serde_json::to_vec(&text(String::new())).unwrap().len();
        let original = "a".repeat(size);
        let ptr = original.as_ptr();
        let err = input.enqueue(text(original)).unwrap_err();
        assert!(matches!(err, EnqueueError::Full(_)));
        let msg = err.into_message();
        let SteeringMessage::User { content } = &msg else {
            panic!()
        };
        assert!(
            matches!(&content[..], [Content::Text { text }] if text.as_ptr() == ptr && text.len() == size)
        );
        input.close();
        let err = input.enqueue(msg).unwrap_err();
        assert!(matches!(err, EnqueueError::Closed(_)));
        let SteeringMessage::User { content } = err.into_message() else {
            panic!()
        };
        assert!(matches!(&content[..], [Content::Text { text }] if text.as_ptr() == ptr));
    }
}
