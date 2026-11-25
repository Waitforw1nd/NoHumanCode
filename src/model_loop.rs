use tokio_util::sync::CancellationToken;

/// Request admission shared by both model loops. Each driver owns its host I/O.
pub(crate) struct ModelLoop {
    rounds: std::ops::Range<usize>,
}

pub(crate) enum Stop {
    Cancelled,
    Limit,
}

impl ModelLoop {
    pub(crate) fn new(max_requests: usize) -> Self {
        Self {
            rounds: 0..max_requests,
        }
    }

    pub(crate) fn next(&mut self, cancel: &CancellationToken) -> Result<usize, Stop> {
        if cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        self.rounds.next().ok_or(Stop::Limit)
    }

    pub(crate) fn can_continue(&self) -> bool {
        !self.rounds.is_empty()
    }
}
