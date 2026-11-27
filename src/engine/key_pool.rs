use super::Engine;
use crate::{
    domain::{Route, Task},
    key_pool::SelectedKey,
    messages::NeutralMessage,
    network_authorization::{
        CallResult, DispatchState, NetworkError, NetworkErrorKind, NetworkPurpose, NetworkResponse,
        NetworkSubject, ProviderRetries, safe_error,
    },
    provider_runtime::{CompletionInput, CompletionLease, Emit},
    provider_transport::{
        ProviderPermit, RetryAfter, TransportError, validate_target_with_protocol,
    },
};
use serde_json::Value;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, Weak},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

// One rotation per logical request. Transient 5xx retries keep the same member.
// The slot protects the actual wire credential until its receipt and cooldown settle.
#[derive(Default)]
pub(super) struct RouteAttempts {
    retries: ProviderRetries,
    selected: Option<SelectedKey>,
    previous_429: Option<SelectedKey>,
    slot: Option<OwnedSemaphorePermit>,
}

impl RouteAttempts {
    pub(super) async fn acquire(
        &mut self,
        engine: &Engine,
        route: &Route,
        subject: &NetworkSubject,
        purpose: NetworkPurpose,
        task: Option<&Task>,
        cancel: &CancellationToken,
    ) -> Result<&SelectedKey, NetworkError> {
        loop {
            if cancel.is_cancelled() {
                return Err(NetworkError::not_sent(NetworkErrorKind::Internal));
            }
            if self
                .selected
                .as_ref()
                .is_none_or(|key| !engine.key_pool.is_ready(key))
            {
                let primary = engine.store.network_primary_key(subject, purpose, task)?;
                self.selected = Some(
                    engine
                        .key_pool
                        .select(route, &primary, self.previous_429.as_ref())
                        .map_err(|mut error| {
                            // Concurrent calls can cool all alternatives during our backoff.
                            // The last dispatched attempt still has a finished 429 receipt.
                            if self.previous_429.is_some()
                                && error.kind == NetworkErrorKind::Upstream
                            {
                                error.dispatch_state = DispatchState::Finished;
                            }
                            error
                        })?,
                );
            }
            let key = self.selected.as_ref().unwrap();
            let bucket = engine.key_semaphore(route, &key.credential)?;
            let permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(NetworkError::not_sent(NetworkErrorKind::Internal)),
                result = bucket.acquire_owned() => result.map_err(|_| NetworkError::not_sent(NetworkErrorKind::Internal))?,
            };
            if engine.key_pool.is_ready(key) {
                self.slot = Some(permit);
                self.previous_429 = None;
                break;
            }
            self.selected = None;
        }
        Ok(self.selected.as_ref().unwrap())
    }

    pub(super) async fn after_attempt<T>(
        &mut self,
        engine: &Engine,
        attempt: Result<NetworkResponse<T>, NetworkError>,
        hint: RetryAfter,
        cancel: &CancellationToken,
    ) -> Option<Result<NetworkResponse<T>, NetworkError>> {
        let mut can_failover = false;
        if let Err(error) = &attempt
            && error.kind == NetworkErrorKind::Upstream
            && error.dispatch_state == DispatchState::Finished
            && matches!(error.calls.as_slice(), [call] if call.result == Some(CallResult::HttpFailure) && call.http_status == Some(429))
            && let Some(key) = &self.selected
            && key.pooled
        {
            can_failover = engine.key_pool.rate_limited(key, hint.get().copied());
            self.previous_429 = self.selected.take();
            if !can_failover {
                self.slot.take();
                return Some(attempt.map_err(|error| self.retries.finish_error(error)));
            }
        }
        self.slot.take();
        self.retries
            .after_pool_attempt(attempt, hint, cancel, can_failover)
            .await
    }
}

pub(super) enum CompletionOutput<'a> {
    Stream {
        text: &'a mut Emit<'a>,
        reasoning: &'a mut Emit<'a>,
    },
    Exploration {
        stop_dispatch: &'a CancellationToken,
    },
}

impl CompletionOutput<'_> {
    async fn execute(
        &mut self,
        lease: CompletionLease,
        engine: &Engine,
        permit: ProviderPermit,
        route: &Route,
    ) -> Result<(NeutralMessage, Value), TransportError> {
        match self {
            Self::Stream { text, reasoning } => {
                lease
                    .execute_with_reasoning(&engine.client, permit, route, *text, *reasoning)
                    .await
            }
            Self::Exploration { .. } => {
                lease
                    .execute_exploration(&engine.client, permit, route)
                    .await
            }
        }
    }
}

impl Engine {
    fn key_semaphore(
        &self,
        route: &Route,
        credential: &str,
    ) -> Result<Arc<Semaphore>, NetworkError> {
        let target = validate_target_with_protocol(
            &route.base_url,
            NetworkPurpose::Completion,
            route.protocol,
        )
        .map_err(|_| NetworkError::not_sent(NetworkErrorKind::Invalid))?;
        let mut hash = DefaultHasher::new();
        target.base_url().hash(&mut hash);
        credential.hash(&mut hash);
        let id = hash.finish();
        let mut limit = route.parallel_limit;
        if let Some(settings) = self.store.settings().map_err(safe_error)? {
            for other in &settings.routes {
                if other.parallel_limit < limit
                    && validate_target_with_protocol(
                        &other.base_url,
                        NetworkPurpose::Completion,
                        other.protocol,
                    )
                    .is_ok_and(|t| t.base_url() == target.base_url())
                    && self.key(other).is_ok_and(|primary| {
                        crate::key_pool::contains_member(other, &primary, credential)
                    })
                {
                    limit = other.parallel_limit;
                }
            }
        }
        let mut slots = self.key_slots.lock().unwrap();
        if let Some(bucket) = slots.get(&id).and_then(Weak::upgrade) {
            return Ok(bucket);
        }
        slots.retain(|_, bucket| bucket.strong_count() > 0);
        let bucket = Arc::new(Semaphore::new(limit));
        slots.insert(id, Arc::downgrade(&bucket));
        Ok(bucket)
    }

    pub(super) async fn complete_route(
        &self,
        subject: &NetworkSubject,
        task: Option<&Task>,
        route: &Route,
        input: &CompletionInput<'_>,
        mut output: CompletionOutput<'_>,
        cancel: &CancellationToken,
    ) -> Result<NetworkResponse<(NeutralMessage, Value)>, NetworkError> {
        // A sibling can stop queued admissions while an already admitted call settles.
        let stop_dispatch = match &output {
            CompletionOutput::Exploration { stop_dispatch } => *stop_dispatch,
            CompletionOutput::Stream { .. } => cancel,
        };
        let mut attempts = RouteAttempts::default();
        loop {
            let hint = RetryAfter::default();
            let attempt = async {
                let key = attempts
                    .acquire(
                        self,
                        route,
                        subject,
                        NetworkPurpose::Completion,
                        task,
                        stop_dispatch,
                    )
                    .await?;
                if matches!(output, CompletionOutput::Exploration { .. })
                    && let Some(task) = task
                {
                    self.composition
                        .projects
                        .validate_files_read(task)
                        .map_err(crate::network_authorization::safe_error)?;
                }
                let (lease, (permit, receipt)) = self
                    .admit_completion(subject, task, input, stop_dispatch, key)
                    .await?;
                let call =
                    output.execute(lease, self, permit.track_retry_after(hint.clone()), route);
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(TransportError::Unknown),
                    result = call => result,
                };
                self.store.finish_completion(receipt, result)
            }
            .await;
            if let Some(done) = attempts
                .after_attempt(self, attempt, hint, stop_dispatch)
                .await
            {
                return done;
            }
        }
    }
}
