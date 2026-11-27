//! Engine 3: durable asynchronous media task lifecycle (submit -> poll -> download / cancel).

use super::engine2::{
    Engine2HybridResponse, Engine2Request, MediaHttpClient, check_standard_business_status,
    decode_base64_bytes,
};
use super::{
    CreateMediaTaskResult, MediaArtifactRef, MediaArtifactStore, MediaError, MediaErrorCode,
    MediaKind, MediaTaskRecord, MediaTaskRepository, MediaTaskStatus, MediaTaskUpdate,
    NewMediaTask, crypto, is_loopback_host, unix_now_secs,
};
use crate::redaction::{CredentialSet, validate_idempotency_key};
use bytes::Bytes;
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct Engine3SubmitInput {
    pub idempotency_key: Option<String>,
    pub session_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub model: String,
    pub route_id: Option<String>,
    pub payload: Value,
}

#[derive(Clone, Debug)]
pub enum MediaOutputSource {
    DirectDownload {
        request: Engine2Request,
        suggested_ext: String,
        mime_type: String,
    },
    TwoStepDownload {
        resolve_request: Engine2Request,
        url_pointer: String,
        download_headers: Vec<(String, String)>,
        download_sensitive_headers: Vec<String>,
        suggested_ext: String,
        mime_type: String,
    },
    InlineBase64 {
        b64: String,
        suggested_ext: String,
        mime_type: String,
    },
    InlineBytes {
        bytes: Bytes,
        suggested_ext: String,
        mime_type: String,
    },
    AdapterResolve {
        resolve_request: Engine2Request,
        context: Value,
    },
}

#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    Accepted {
        remote_task_id: String,
        poll_cursor: Option<String>,
    },
    Completed {
        remote_task_id: Option<String>,
        outputs: Vec<MediaOutputSource>,
    },
}

#[derive(Clone, Debug)]
pub enum PollOutcome {
    Pending {
        status: MediaTaskStatus,
        poll_cursor: Option<String>,
        retry_after_hint: Option<Duration>,
    },
    Succeeded {
        outputs: Vec<MediaOutputSource>,
        poll_cursor: Option<String>,
    },
    Failed {
        upstream_code: Option<String>,
        message: String,
        retryable: bool,
    },
    Cancelled {
        message: String,
    },
}

pub trait Engine3Adapter: Send + Sync {
    fn vendor(&self) -> &str;
    fn kind(&self) -> MediaKind;

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError>;

    /// Must enforce vendor business status checks even when HTTP status is 200.
    fn parse_submit_response(
        &self,
        status: u16,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError>;

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError>;

    /// Must inspect BOTH HTTP headers and JSON body for vendor business status codes.
    fn parse_poll_response(
        &self,
        status: u16,
        headers: &HeaderMap,
        body: &Value,
        task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError>;

    fn build_cancel(&self, _task: &MediaTaskRecord) -> Result<Option<Engine2Request>, MediaError> {
        Ok(None)
    }

    fn parse_cancel_response(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        _body: &Value,
    ) -> Result<(), MediaError> {
        Ok(())
    }

    fn accepts_hybrid_submit(&self) -> bool {
        false
    }

    fn parse_submit_bytes(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        _bytes: &Bytes,
    ) -> Result<SubmitOutcome, MediaError> {
        Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "adapter does not support binary submit responses",
        ))
    }

    fn parse_resolved_outputs(
        &self,
        _status: u16,
        _headers: &HeaderMap,
        _body: &Value,
        _context: &Value,
        _task: &MediaTaskRecord,
    ) -> Result<Vec<MediaOutputSource>, MediaError> {
        Err(MediaError::new(
            MediaErrorCode::DecodeError,
            "adapter does not support AdapterResolve outputs",
        ))
    }
}

#[derive(Clone, Debug)]
pub struct Engine3Completion {
    pub record: MediaTaskRecord,
    pub payloads: Vec<(MediaArtifactRef, Bytes)>,
}

#[derive(Clone, Debug)]
pub struct Engine3PollConfig {
    pub initial_interval: Duration,
    pub max_interval: Duration,
    pub backoff_factor: f64,
    pub max_attempts: u32,
    pub max_wall_time: Duration,
}

impl Default for Engine3PollConfig {
    fn default() -> Self {
        Self {
            initial_interval: Duration::from_secs(1),
            max_interval: Duration::from_secs(15),
            backoff_factor: 1.5,
            max_attempts: 120,
            max_wall_time: Duration::from_secs(20 * 60),
        }
    }
}

pub struct Engine3Runner<R, A> {
    http: MediaHttpClient,
    repo: Arc<R>,
    artifacts: Arc<A>,
    poll_config: Engine3PollConfig,
    allow_http_loopback_download: bool,
}

impl<R: MediaTaskRepository, A: MediaArtifactStore> Engine3Runner<R, A> {
    pub fn new(http: MediaHttpClient, repo: Arc<R>, artifacts: Arc<A>) -> Self {
        Self {
            http,
            repo,
            artifacts,
            poll_config: Engine3PollConfig::default(),
            allow_http_loopback_download: false,
        }
    }

    pub fn with_poll_config(mut self, poll_config: Engine3PollConfig) -> Self {
        self.poll_config = poll_config;
        self
    }

    pub fn with_allow_http_loopback_download(mut self, allow: bool) -> Self {
        self.allow_http_loopback_download = allow;
        self
    }

    pub fn artifacts(&self) -> &Arc<A> {
        &self.artifacts
    }

    async fn load_persisted_payloads(
        &self,
        record: &MediaTaskRecord,
    ) -> Result<Vec<(MediaArtifactRef, Bytes)>, MediaError> {
        let mut payloads = Vec::with_capacity(record.artifacts.len());
        for artifact in &record.artifacts {
            let bytes = self.artifacts.read_artifact(artifact).await?;
            payloads.push((artifact.clone(), bytes));
        }
        Ok(payloads)
    }

    pub async fn submit_and_persist<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        input: &Engine3SubmitInput,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<(MediaTaskRecord, Option<Vec<MediaOutputSource>>), MediaError> {
        let (record, immediate_outputs, _) = self
            .submit_and_persist_with_origin(adapter, input, keys, cancel)
            .await?;
        Ok((record, immediate_outputs))
    }

    async fn submit_and_persist_with_origin<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        input: &Engine3SubmitInput,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<(MediaTaskRecord, Option<Vec<MediaOutputSource>>, bool), MediaError> {
        if let Some(key) = input.idempotency_key.as_deref() {
            validate_idempotency_key(key)
                .map_err(|e| MediaError::invalid_request(e.to_string()))?;
        }
        let canonical = serde_json::to_vec(&serde_json::json!({
            "kind": adapter.kind().as_str(),
            "vendor": adapter.vendor(),
            "model": input.model,
            "payload": input.payload,
        }))
        .map_err(|e| MediaError::invalid_request(e.to_string()))?;
        let request_hash = crypto::sha256_hex(&canonical);

        if let Some(key) = input.idempotency_key.as_deref()
            && let Some(existing) = self.repo.get_by_idempotency_key(key)?
        {
            if existing.request_hash != request_hash {
                return Err(MediaError::new(
                    MediaErrorCode::Conflict,
                    "idempotency key already used with different media request parameters",
                ));
            }
            return Ok((existing, None, false));
        }

        let submit_req = adapter.build_submit(input)?;
        let origin_is_loopback = is_loopback_url_str(&submit_req.url);
        let outcome = if adapter.accepts_hybrid_submit() {
            match self
                .http
                .execute_hybrid(&submit_req, keys, |_, _| Ok(()), cancel)
                .await?
            {
                Engine2HybridResponse::Json(resp) => adapter
                    .parse_submit_response(resp.status, &resp.headers, &resp.body)
                    .map_err(|e| e.scrubbed(keys))?,
                Engine2HybridResponse::Bytes(resp) => adapter
                    .parse_submit_bytes(resp.status, &resp.headers, &resp.bytes)
                    .map_err(|e| e.scrubbed(keys))?,
            }
        } else {
            let (status, headers, maybe_body) = self
                .http
                .execute_optional_json_with_headers(&submit_req, keys, |_, _| Ok(()), cancel)
                .await?;
            let body = maybe_body.unwrap_or(Value::Null);
            adapter
                .parse_submit_response(status, &headers, &body)
                .map_err(|e| e.scrubbed(keys))?
        };

        let now = unix_now_secs();
        let task_id = uuid::Uuid::now_v7().to_string();
        let (status, remote_task_id, poll_cursor, immediate_outputs) = match outcome {
            SubmitOutcome::Accepted {
                remote_task_id,
                poll_cursor,
            } => (
                MediaTaskStatus::Submitted,
                Some(remote_task_id),
                poll_cursor,
                None,
            ),
            SubmitOutcome::Completed {
                remote_task_id,
                outputs,
            } => (
                MediaTaskStatus::Running,
                remote_task_id,
                None,
                Some(outputs),
            ),
        };

        let new_task = NewMediaTask {
            id: task_id,
            idempotency_key: input.idempotency_key.clone(),
            request_hash,
            session_id: input.session_id.clone(),
            parent_task_id: input.parent_task_id.clone(),
            media_kind: adapter.kind(),
            vendor: adapter.vendor().to_owned(),
            model: input.model.clone(),
            route_id: input.route_id.clone(),
            status,
            remote_task_id,
            poll_cursor,
            request_summary: input.payload.clone(),
            artifacts: Vec::new(),
            error_code: None,
            error_message: None,
            created_at: now,
            updated_at: now,
        };

        let record = match self.repo.create_task(&new_task)? {
            CreateMediaTaskResult::Created(rec) | CreateMediaTaskResult::Replayed(rec) => rec,
            CreateMediaTaskResult::IdempotencyConflict => {
                return Err(MediaError::new(
                    MediaErrorCode::Conflict,
                    "idempotency key conflict during media task creation",
                ));
            }
        };
        Ok((record, immediate_outputs, origin_is_loopback))
    }

    pub async fn run_to_completion<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        input: &Engine3SubmitInput,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<MediaTaskRecord, MediaError> {
        let completion = self
            .run_to_completion_with_outputs(adapter, input, keys, cancel)
            .await?;
        Ok(completion.record)
    }

    pub async fn run_to_completion_with_outputs<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        input: &Engine3SubmitInput,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<Engine3Completion, MediaError> {
        let (record, immediate_outputs, origin_is_loopback) = self
            .submit_and_persist_with_origin(adapter, input, keys, cancel)
            .await?;
        if record.status.is_terminal() {
            let payloads =
                if record.status == MediaTaskStatus::Succeeded && !record.artifacts.is_empty() {
                    self.load_persisted_payloads(&record).await?
                } else {
                    Vec::new()
                };
            return Ok(Engine3Completion { record, payloads });
        }
        if let Some(outputs) = immediate_outputs {
            return self
                .finalize_outputs_or_fail_with_payloads(
                    adapter,
                    FinalizeTarget {
                        task: &record,
                        poll_cursor: None,
                        origin_is_loopback,
                    },
                    outputs,
                    keys,
                    cancel,
                )
                .await;
        }
        self.poll_until_done_with_outputs(adapter, record, keys, cancel)
            .await
    }

    pub async fn poll_until_done<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        task: MediaTaskRecord,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<MediaTaskRecord, MediaError> {
        let completion = self
            .poll_until_done_with_outputs(adapter, task, keys, cancel)
            .await?;
        Ok(completion.record)
    }

    pub async fn poll_until_done_with_outputs<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        mut task: MediaTaskRecord,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<Engine3Completion, MediaError> {
        if task.status.is_terminal() {
            return Ok(Engine3Completion {
                record: task,
                payloads: Vec::new(),
            });
        }

        let started = Instant::now();
        let mut interval = self.poll_config.initial_interval;

        for attempt in 0..self.poll_config.max_attempts {
            if cancel.is_cancelled() {
                let _ = self.repo.mark_cancelled(&task.id, Some("cancelled"));
                return Err(MediaError::new(
                    MediaErrorCode::Cancelled,
                    "media polling cancelled",
                ));
            }
            if started.elapsed() >= self.poll_config.max_wall_time {
                return Err(MediaError::new(
                    MediaErrorCode::Timeout,
                    "media task polling exceeded max wall time",
                ));
            }

            let poll_req = match adapter.build_poll(&task) {
                Ok(req) => req,
                Err(err) => {
                    let scrubbed = err.scrubbed(keys);
                    self.persist_terminal_error_if_needed(&task.id, &scrubbed, keys);
                    return Err(scrubbed);
                }
            };
            let origin_is_loopback = is_loopback_url_str(&poll_req.url);
            let (status, headers, maybe_body) = match self
                .http
                .execute_optional_json_with_headers(&poll_req, keys, |_, _| Ok(()), cancel)
                .await
            {
                Ok(r) => r,
                Err(err) => {
                    let scrubbed = err.scrubbed(keys);
                    self.persist_terminal_error_if_needed(&task.id, &scrubbed, keys);
                    return Err(scrubbed);
                }
            };
            let body = maybe_body.unwrap_or(Value::Null);
            let outcome = match adapter.parse_poll_response(status, &headers, &body, &task) {
                Ok(o) => o,
                Err(err) => {
                    let scrubbed = err.scrubbed(keys);
                    self.persist_terminal_error_if_needed(&task.id, &scrubbed, keys);
                    return Err(scrubbed);
                }
            };

            match outcome {
                PollOutcome::Pending {
                    status,
                    poll_cursor,
                    retry_after_hint,
                } => {
                    task = self.repo.update_task_status(
                        &task.id,
                        &MediaTaskUpdate {
                            status,
                            poll_cursor,
                            updated_at: unix_now_secs(),
                            ..Default::default()
                        },
                    )?;
                    let sleep_for = retry_after_hint
                        .unwrap_or(interval)
                        .min(self.poll_config.max_interval);
                    if !sleep_for.is_zero() {
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                let _ = self.repo.mark_cancelled(&task.id, Some("cancelled"));
                                return Err(MediaError::new(MediaErrorCode::Cancelled, "media polling cancelled"));
                            }
                            _ = tokio::time::sleep(sleep_for) => {}
                        }
                    }
                    let next_secs = interval.as_secs_f64() * self.poll_config.backoff_factor;
                    interval = Duration::from_secs_f64(next_secs.max(0.001))
                        .min(self.poll_config.max_interval);
                }
                PollOutcome::Succeeded {
                    outputs,
                    poll_cursor,
                } => {
                    return self
                        .finalize_outputs_or_fail_with_payloads(
                            adapter,
                            FinalizeTarget {
                                task: &task,
                                poll_cursor,
                                origin_is_loopback,
                            },
                            outputs,
                            keys,
                            cancel,
                        )
                        .await;
                }
                PollOutcome::Failed {
                    upstream_code,
                    message,
                    retryable,
                } => {
                    let scrubbed_msg = keys.scrub(&message);
                    let scrubbed_code = upstream_code.as_deref().map(|c| keys.scrub(c));
                    self.repo.update_task_status(
                        &task.id,
                        &MediaTaskUpdate {
                            status: MediaTaskStatus::Failed,
                            error_code: scrubbed_code.clone(),
                            error_message: Some(scrubbed_msg.clone()),
                            updated_at: unix_now_secs(),
                            ..Default::default()
                        },
                    )?;
                    return Err(MediaError::new(MediaErrorCode::ProviderError, scrubbed_msg)
                        .with_upstream_code(scrubbed_code)
                        .with_retryable(retryable));
                }
                PollOutcome::Cancelled { message } => {
                    let scrubbed_msg = keys.scrub(&message);
                    let record = self.repo.mark_cancelled(&task.id, Some(&scrubbed_msg))?;
                    return Ok(Engine3Completion {
                        record,
                        payloads: Vec::new(),
                    });
                }
            }

            if attempt + 1 >= self.poll_config.max_attempts {
                break;
            }
        }

        Err(MediaError::new(
            MediaErrorCode::Timeout,
            "media task polling exceeded max attempts",
        ))
    }

    pub async fn resume_pending_tasks<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        session_id: Option<&str>,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<Vec<Result<MediaTaskRecord, MediaError>>, MediaError> {
        let tasks = self.repo.list_resumable_tasks(session_id)?;
        let mut results = Vec::new();
        for task in tasks {
            if task.vendor != adapter.vendor() || task.media_kind != adapter.kind() {
                continue;
            }
            results.push(self.poll_until_done(adapter, task, keys, cancel).await);
        }
        Ok(results)
    }

    pub async fn cancel_task<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        task_id: &str,
        reason: Option<&str>,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<MediaTaskRecord, MediaError> {
        let task = self
            .repo
            .get_task(task_id)?
            .ok_or_else(|| MediaError::new(MediaErrorCode::NotFound, "media task not found"))?;
        if task.status.is_terminal() {
            return Ok(task);
        }
        if let Some(cancel_req) = adapter.build_cancel(&task)?
            && let Some(resp) = self
                .http
                .execute_optional_json(
                    &cancel_req,
                    keys,
                    super::engine2::check_standard_business_status,
                    cancel,
                )
                .await?
        {
            adapter
                .parse_cancel_response(resp.status, &resp.headers, &resp.body)
                .map_err(|e| e.scrubbed(keys))?;
        }
        self.repo.mark_cancelled(&task.id, reason)
    }

    fn persist_terminal_error_if_needed(
        &self,
        task_id: &str,
        err: &MediaError,
        keys: &CredentialSet,
    ) {
        if err.code == MediaErrorCode::Cancelled {
            let _ = self.repo.mark_cancelled(task_id, Some("cancelled"));
            return;
        }
        if !err.retryable {
            let scrubbed_msg = keys.scrub(&err.message);
            let scrubbed_code = err
                .upstream_code
                .as_deref()
                .map(|c| keys.scrub(c))
                .or_else(|| Some(err.code.as_str().to_owned()));
            let _ = self.repo.update_task_status(
                task_id,
                &MediaTaskUpdate {
                    status: MediaTaskStatus::Failed,
                    error_code: scrubbed_code,
                    error_message: Some(scrubbed_msg),
                    updated_at: unix_now_secs(),
                    ..Default::default()
                },
            );
        }
    }

    async fn finalize_outputs_or_fail_with_payloads<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        target: FinalizeTarget<'_>,
        outputs: Vec<MediaOutputSource>,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<Engine3Completion, MediaError> {
        let task_id = target.task.id.clone();
        match self
            .finalize_outputs_with_payloads(adapter, target, outputs, keys, cancel)
            .await
        {
            Ok(completion) => Ok(completion),
            Err(err) => {
                let scrubbed = err.scrubbed(keys);
                self.persist_terminal_error_if_needed(&task_id, &scrubbed, keys);
                Err(scrubbed)
            }
        }
    }

    async fn finalize_outputs_with_payloads<Ad: Engine3Adapter>(
        &self,
        adapter: &Ad,
        target: FinalizeTarget<'_>,
        outputs: Vec<MediaOutputSource>,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<Engine3Completion, MediaError> {
        let FinalizeTarget {
            task,
            poll_cursor,
            origin_is_loopback,
        } = target;
        let mut refs: Vec<MediaArtifactRef> = Vec::with_capacity(outputs.len());
        let mut payloads: Vec<(MediaArtifactRef, Bytes)> = Vec::with_capacity(outputs.len());
        for source in outputs {
            match source {
                MediaOutputSource::AdapterResolve {
                    resolve_request,
                    context,
                } => {
                    let meta = self
                        .http
                        .execute_json(
                            &resolve_request,
                            keys,
                            check_standard_business_status,
                            cancel,
                        )
                        .await?;
                    let resolve_origin_is_loopback =
                        origin_is_loopback || is_loopback_url_str(&resolve_request.url);
                    let resolved = adapter
                        .parse_resolved_outputs(
                            meta.status,
                            &meta.headers,
                            &meta.body,
                            &context,
                            task,
                        )
                        .map_err(|e| e.scrubbed(keys))?;
                    for item in resolved {
                        if matches!(item, MediaOutputSource::AdapterResolve { .. }) {
                            return Err(MediaError::new(
                                MediaErrorCode::DecodeError,
                                "nested AdapterResolve outputs are not allowed",
                            ));
                        }
                        let (artifact, payload_bytes) = self
                            .persist_output_source(
                                task,
                                item,
                                resolve_origin_is_loopback,
                                keys,
                                cancel,
                            )
                            .await?;
                        refs.push(artifact.clone());
                        payloads.push((artifact, payload_bytes));
                    }
                }
                other => {
                    let (artifact, payload_bytes) = self
                        .persist_output_source(task, other, origin_is_loopback, keys, cancel)
                        .await?;
                    refs.push(artifact.clone());
                    payloads.push((artifact, payload_bytes));
                }
            }
        }

        let record = self.repo.update_task_status(
            &task.id,
            &MediaTaskUpdate {
                status: MediaTaskStatus::Succeeded,
                poll_cursor,
                artifacts: Some(refs),
                updated_at: unix_now_secs(),
                ..Default::default()
            },
        )?;
        Ok(Engine3Completion { record, payloads })
    }

    async fn persist_output_source(
        &self,
        task: &MediaTaskRecord,
        source: MediaOutputSource,
        origin_is_loopback: bool,
        keys: &CredentialSet,
        cancel: &CancellationToken,
    ) -> Result<(MediaArtifactRef, Bytes), MediaError> {
        match source {
            MediaOutputSource::DirectDownload {
                mut request,
                suggested_ext,
                mime_type,
            } => {
                let allow_loopback = self.allow_http_loopback_download || origin_is_loopback;
                let validated =
                    super::engine2::validate_download_hop_url(&request.url, allow_loopback)
                        .map_err(|e| e.scrubbed(keys))?;
                request.url = validated.to_string();
                let resp = if request.controlled_redirects {
                    self.http
                        .execute_bytes_with_controlled_redirects(
                            &request,
                            allow_loopback,
                            keys,
                            cancel,
                        )
                        .await?
                } else {
                    self.http.execute_bytes(&request, keys, cancel).await?
                };
                let artifact = self
                    .artifacts
                    .write_artifact(&task.id, &suggested_ext, &mime_type, &resp.bytes)
                    .await?;
                Ok((artifact, resp.bytes))
            }
            MediaOutputSource::TwoStepDownload {
                resolve_request,
                url_pointer,
                download_headers,
                download_sensitive_headers,
                suggested_ext,
                mime_type,
            } => {
                let allow_loopback = self.allow_http_loopback_download
                    || origin_is_loopback
                    || is_loopback_url_str(&resolve_request.url);
                let meta = self
                    .http
                    .execute_json(
                        &resolve_request,
                        keys,
                        check_standard_business_status,
                        cancel,
                    )
                    .await?;
                let download_url = meta
                    .body
                    .pointer(&url_pointer)
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        MediaError::new(
                            MediaErrorCode::DecodeError,
                            format!("missing download url at pointer {url_pointer}"),
                        )
                    })?;
                let validated =
                    super::engine2::validate_download_hop_url(download_url, allow_loopback)
                        .map_err(|e| e.scrubbed(keys))?;
                let max_bytes = if mime_type.to_ascii_lowercase().starts_with("video/") {
                    super::engine2::MEDIA_DEFAULT_VIDEO_LIMIT
                } else {
                    super::engine2::MEDIA_DEFAULT_ARTIFACT_LIMIT
                };
                let dl_req = Engine2Request::get(validated.as_str())
                    .with_headers(download_headers, download_sensitive_headers)
                    .with_max_response_bytes(max_bytes)
                    .with_controlled_redirects(super::engine2::MEDIA_MAX_DOWNLOAD_REDIRECTS);
                let resp = self
                    .http
                    .execute_bytes_with_controlled_redirects(&dl_req, allow_loopback, keys, cancel)
                    .await?;
                let artifact = self
                    .artifacts
                    .write_artifact(&task.id, &suggested_ext, &mime_type, &resp.bytes)
                    .await?;
                Ok((artifact, resp.bytes))
            }
            MediaOutputSource::InlineBase64 {
                b64,
                suggested_ext,
                mime_type,
            } => {
                let decoded = decode_base64_bytes(&b64)?;
                let artifact = self
                    .artifacts
                    .write_artifact(&task.id, &suggested_ext, &mime_type, &decoded)
                    .await?;
                Ok((artifact, Bytes::from(decoded)))
            }
            MediaOutputSource::InlineBytes {
                bytes,
                suggested_ext,
                mime_type,
            } => {
                let artifact = self
                    .artifacts
                    .write_artifact(&task.id, &suggested_ext, &mime_type, &bytes)
                    .await?;
                Ok((artifact, bytes))
            }
            MediaOutputSource::AdapterResolve { .. } => Err(MediaError::new(
                MediaErrorCode::DecodeError,
                "nested AdapterResolve outputs are not allowed",
            )),
        }
    }
}

struct FinalizeTarget<'a> {
    task: &'a MediaTaskRecord,
    poll_cursor: Option<String>,
    origin_is_loopback: bool,
}

fn is_loopback_url_str(raw: &str) -> bool {
    url::Url::parse(raw.trim())
        .ok()
        .is_some_and(|u| is_loopback_host(&u))
}

#[derive(Clone, Debug)]
pub struct EmbeddedMediaTaskAdapter {
    vendor: String,
    kind: MediaKind,
    base_url: String,
    headers: Vec<(String, String)>,
    sensitive: Vec<String>,
    embedded_proxy: bool,
}

impl EmbeddedMediaTaskAdapter {
    pub fn new(
        vendor: impl Into<String>,
        kind: MediaKind,
        base_url: impl Into<String>,
        headers: Vec<(String, String)>,
        sensitive: Vec<String>,
        embedded_proxy: bool,
    ) -> Result<Self, MediaError> {
        let base_url_raw = base_url.into();
        let validated = super::validate_media_url(&base_url_raw, embedded_proxy)?;
        Ok(Self {
            vendor: vendor.into(),
            kind,
            base_url: validated.as_str().trim_end_matches('/').to_owned(),
            headers,
            sensitive,
            embedded_proxy,
        })
    }

    fn tasks_base_url(&self) -> String {
        if self.base_url.to_ascii_lowercase().ends_with("/media/tasks") {
            self.base_url.clone()
        } else {
            format!("{}/media/tasks", self.base_url)
        }
    }

    fn default_output_mime_and_ext(&self, body: &Value) -> (String, String) {
        if let Some(mime) = body
            .get("mimeType")
            .or_else(|| body.get("contentType"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let ext = match mime.split(';').next().unwrap_or(mime).trim() {
                "video/mp4" => "mp4",
                "video/webm" => "webm",
                "audio/mpeg" | "audio/mp3" => "mp3",
                "audio/wav" | "audio/x-wav" => "wav",
                "audio/ogg" => "ogg",
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                _ => "bin",
            };
            return (mime.to_owned(), ext.to_owned());
        }
        match self.kind {
            MediaKind::Video => ("video/mp4".to_owned(), "mp4".to_owned()),
            MediaKind::Music | MediaKind::Tts | MediaKind::Stt => {
                ("audio/mpeg".to_owned(), "mp3".to_owned())
            }
            MediaKind::Image => ("image/png".to_owned(), "png".to_owned()),
            _ => ("application/octet-stream".to_owned(), "bin".to_owned()),
        }
    }
}

impl Engine3Adapter for EmbeddedMediaTaskAdapter {
    fn vendor(&self) -> &str {
        &self.vendor
    }

    fn kind(&self) -> MediaKind {
        self.kind
    }

    fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
        let _ = super::validate_media_url(&self.base_url, self.embedded_proxy)?;
        let mut body = input.payload.clone();
        if let Some(obj) = body.as_object_mut()
            && !input.model.trim().is_empty()
            && !obj.contains_key("model")
        {
            obj.insert("model".to_owned(), Value::String(input.model.clone()));
        }
        Ok(Engine2Request::post_json(self.tasks_base_url(), body)
            .with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_submit_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<SubmitOutcome, MediaError> {
        check_standard_business_status(headers, body)?;
        let raw_id = body
            .get("taskId")
            .or_else(|| body.get("id"))
            .or_else(|| body.get("task_id"))
            .or_else(|| body.pointer("/data/task_id"))
            .or_else(|| body.pointer("/data/id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                MediaError::new(
                    MediaErrorCode::DecodeError,
                    "embedded media task submit response missing taskId",
                )
            })?;
        let task_id = super::validate_path_segment("taskId", raw_id)?;
        Ok(SubmitOutcome::Accepted {
            remote_task_id: task_id,
            poll_cursor: None,
        })
    }

    fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
        let raw_id = task
            .remote_task_id
            .as_deref()
            .ok_or_else(|| MediaError::invalid_request("missing remote_task_id for task poll"))?;
        let task_id = super::validate_path_segment("taskId", raw_id)?;
        let url = format!("{}/{}", self.tasks_base_url(), task_id);
        Ok(Engine2Request::get(url).with_headers(self.headers.clone(), self.sensitive.clone()))
    }

    fn parse_poll_response(
        &self,
        _status: u16,
        headers: &HeaderMap,
        body: &Value,
        task: &MediaTaskRecord,
    ) -> Result<PollOutcome, MediaError> {
        check_standard_business_status(headers, body)?;
        let state = body
            .get("status")
            .or_else(|| body.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("running")
            .trim()
            .to_ascii_lowercase();
        match state.as_str() {
            "queued" | "pending" | "submitted" => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Queued,
                poll_cursor: None,
                retry_after_hint: None,
            }),
            "running" | "in_progress" | "processing" => Ok(PollOutcome::Pending {
                status: MediaTaskStatus::Running,
                poll_cursor: None,
                retry_after_hint: None,
            }),
            "cancelled" | "canceled" => Ok(PollOutcome::Cancelled {
                message: body
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("media task cancelled")
                    .to_owned(),
            }),
            "failed" | "error" => {
                let code = body
                    .pointer("/error/code")
                    .or_else(|| body.get("code"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let message = body
                    .pointer("/error/message")
                    .or_else(|| body.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("embedded media task failed")
                    .to_owned();
                Ok(PollOutcome::Failed {
                    upstream_code: code,
                    message,
                    retryable: false,
                })
            }
            "succeeded" | "completed" | "done" => {
                let (mime_type, suggested_ext) = self.default_output_mime_and_ext(body);
                if let Some(b64) = body
                    .get("b64_json")
                    .or_else(|| body.get("base64"))
                    .and_then(Value::as_str)
                {
                    return Ok(PollOutcome::Succeeded {
                        outputs: vec![MediaOutputSource::InlineBase64 {
                            b64: b64.to_owned(),
                            suggested_ext,
                            mime_type,
                        }],
                        poll_cursor: None,
                    });
                }
                let raw_id = task.remote_task_id.as_deref().ok_or_else(|| {
                    MediaError::invalid_request("missing remote_task_id for content download")
                })?;
                let task_id = super::validate_path_segment("taskId", raw_id)?;
                let content_url = format!("{}/{}/content", self.tasks_base_url(), task_id);
                let req = Engine2Request::get(content_url)
                    .with_headers(self.headers.clone(), self.sensitive.clone())
                    .with_max_response_bytes(super::engine2::MEDIA_DEFAULT_VIDEO_LIMIT);
                Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::DirectDownload {
                        request: req,
                        suggested_ext,
                        mime_type,
                    }],
                    poll_cursor: None,
                })
            }
            other => Err(MediaError::new(
                MediaErrorCode::DecodeError,
                format!("unexpected embedded media task status: {other}"),
            )),
        }
    }

    fn build_cancel(&self, task: &MediaTaskRecord) -> Result<Option<Engine2Request>, MediaError> {
        let Some(raw_id) = task.remote_task_id.as_deref() else {
            return Ok(None);
        };
        let task_id = super::validate_path_segment("taskId", raw_id)?;
        let url = format!("{}/{}/cancel", self.tasks_base_url(), task_id);
        Ok(Some(
            Engine2Request::post_raw(url, "application/json", Bytes::new())
                .with_headers(self.headers.clone(), self.sensitive.clone()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::InMemoryMediaArtifactStore;
    use crate::media::InMemoryMediaTaskRepository;
    use axum::{
        Router,
        extract::Path as AxumPath,
        http::StatusCode,
        routing::{delete, get, post},
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct MockVideoAdapter {
        base_url: String,
    }

    impl Engine3Adapter for MockVideoAdapter {
        fn vendor(&self) -> &str {
            "mock-video"
        }

        fn kind(&self) -> MediaKind {
            MediaKind::Video
        }

        fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
            Ok(Engine2Request::post_json(
                format!("{}/submit", self.base_url),
                input.payload.clone(),
            ))
        }

        fn parse_submit_response(
            &self,
            _status: u16,
            _headers: &HeaderMap,
            body: &Value,
        ) -> Result<SubmitOutcome, MediaError> {
            if let Some(code) = body
                .pointer("/base_resp/status_code")
                .and_then(Value::as_i64)
                && code != 0
            {
                return Err(MediaError::business(
                    Some(code.to_string()),
                    body.pointer("/base_resp/status_msg")
                        .and_then(Value::as_str)
                        .unwrap_or("submit rejected"),
                    false,
                ));
            }
            let remote_id = body["task_id"]
                .as_str()
                .ok_or_else(|| MediaError::new(MediaErrorCode::DecodeError, "missing task_id"))?;
            Ok(SubmitOutcome::Accepted {
                remote_task_id: remote_id.to_owned(),
                poll_cursor: None,
            })
        }

        fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
            let remote_id = task.remote_task_id.as_deref().unwrap_or("");
            Ok(Engine2Request::get(format!(
                "{}/tasks/{remote_id}",
                self.base_url
            )))
        }

        fn parse_poll_response(
            &self,
            _status: u16,
            headers: &HeaderMap,
            body: &Value,
            _task: &MediaTaskRecord,
        ) -> Result<PollOutcome, MediaError> {
            if let Some(hdr_code) = headers
                .get("x-api-status-code")
                .and_then(|v| v.to_str().ok())
                && hdr_code != "20000000"
            {
                return Err(MediaError::business(
                    Some(hdr_code.to_owned()),
                    "header business error",
                    false,
                ));
            }
            match body["status"].as_str().unwrap_or("unknown") {
                "running" => Ok(PollOutcome::Pending {
                    status: MediaTaskStatus::Running,
                    poll_cursor: Some("step-1".into()),
                    retry_after_hint: Some(Duration::from_millis(5)),
                }),
                "succeeded" => Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(format!(
                            "{}{}",
                            self.base_url,
                            body["download_path"].as_str().unwrap_or("/files/out.mp4")
                        )),
                        suggested_ext: "mp4".into(),
                        mime_type: "video/mp4".into(),
                    }],
                    poll_cursor: Some("done".into()),
                }),
                "failed" => Ok(PollOutcome::Failed {
                    upstream_code: Some("render_err".into()),
                    message: "render failed".into(),
                    retryable: false,
                }),
                _ => Ok(PollOutcome::Pending {
                    status: MediaTaskStatus::Unknown,
                    poll_cursor: None,
                    retry_after_hint: None,
                }),
            }
        }

        fn build_cancel(
            &self,
            task: &MediaTaskRecord,
        ) -> Result<Option<Engine2Request>, MediaError> {
            let remote_id = task.remote_task_id.as_deref().unwrap_or("");
            Ok(Some(Engine2Request::delete(format!(
                "{}/tasks/{remote_id}",
                self.base_url
            ))))
        }
    }

    #[tokio::test]
    async fn engine3_idempotency_resume_business_error_and_cancel() {
        let submit_calls = Arc::new(AtomicUsize::new(0));
        let poll_calls = Arc::new(AtomicUsize::new(0));
        let cancelled_remotely = Arc::new(AtomicBool::new(false));

        let submit_c = submit_calls.clone();
        let poll_c = poll_calls.clone();
        let cancel_c = cancelled_remotely.clone();

        let app = Router::new()
            .route(
                "/submit",
                post(move |axum::Json(body): axum::Json<Value>| {
                    let count = submit_c.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if body.get("biz_fail").and_then(Value::as_bool) == Some(true) {
                            return (
                                StatusCode::OK,
                                r#"{"base_resp":{"status_code":2013,"status_msg":"invalid input"}}"#
                                    .to_owned(),
                            );
                        }
                        (StatusCode::OK, format!(r#"{{"task_id":"remote-{count}"}}"#))
                    }
                }),
            )
            .route(
                "/tasks/{id}",
                get(move |AxumPath(id): AxumPath<String>| {
                    let n = poll_c.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if id == "remote-hdr-err" {
                            let mut h = axum::http::HeaderMap::new();
                            h.insert("x-api-status-code", "55000001".parse().unwrap());
                            return (StatusCode::OK, h, r#"{"status":"running"}"#.to_owned());
                        }
                        let mut h = axum::http::HeaderMap::new();
                        h.insert("x-api-status-code", "20000000".parse().unwrap());
                        if n == 0 {
                            (StatusCode::OK, h, r#"{"status":"running"}"#.to_owned())
                        } else {
                            (
                                StatusCode::OK,
                                h,
                                r#"{"status":"succeeded","download_path":"/files/video.mp4"}"#
                                    .to_owned(),
                            )
                        }
                    }
                }),
            )
            .route(
                "/tasks/{id}",
                delete(move || {
                    cancel_c.store(true, Ordering::SeqCst);
                    async { (StatusCode::OK, r#"{"cancelled":true}"#) }
                }),
            )
            .route(
                "/files/video.mp4",
                get(|| async { (StatusCode::OK, b"FAKE-MP4-BYTES".to_vec()) }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner = Engine3Runner::new(
            MediaHttpClient::new().unwrap(),
            repo.clone(),
            artifacts.clone(),
        )
        .with_poll_config(Engine3PollConfig {
            initial_interval: Duration::from_millis(5),
            max_interval: Duration::from_millis(20),
            ..Default::default()
        });
        let adapter = MockVideoAdapter {
            base_url: format!("http://{addr}"),
        };
        let keys = CredentialSet::new(vec![]);
        let cancel = CancellationToken::new();

        // 1. Business error on HTTP 200 submit rejected before persistence
        let biz_err = runner
            .submit_and_persist(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("k-biz".into()),
                    session_id: Some("s1".into()),
                    parent_task_id: None,
                    model: "seedance".into(),
                    route_id: None,
                    payload: serde_json::json!({"biz_fail": true}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(biz_err.code, MediaErrorCode::BusinessError);

        // 2. Submit persists remote_task_id immediately, then resume_pending_tasks completes it
        let input = Engine3SubmitInput {
            idempotency_key: Some("k-video-1".into()),
            session_id: Some("s1".into()),
            parent_task_id: None,
            model: "seedance".into(),
            route_id: None,
            payload: serde_json::json!({"prompt": "ocean waves"}),
        };
        let (submitted, _) = runner
            .submit_and_persist(&adapter, &input, &keys, &cancel)
            .await
            .unwrap();
        assert_eq!(submitted.status, MediaTaskStatus::Submitted);
        assert!(submitted.remote_task_id.is_some());

        // Idempotent replay with same key & payload returns existing without extra submit
        let before_submits = submit_calls.load(Ordering::SeqCst);
        let (replayed, _) = runner
            .submit_and_persist(&adapter, &input, &keys, &cancel)
            .await
            .unwrap();
        assert_eq!(replayed.id, submitted.id);
        assert_eq!(submit_calls.load(Ordering::SeqCst), before_submits);

        // Different payload with same idempotency_key -> Conflict
        let mut diff_input = input.clone();
        diff_input.payload = serde_json::json!({"prompt": "mountains"});
        let conflict = runner
            .submit_and_persist(&adapter, &diff_input, &keys, &cancel)
            .await
            .unwrap_err();
        assert_eq!(conflict.code, MediaErrorCode::Conflict);

        // Resume pending tasks finishes polling and saves artifact
        let resumed = runner
            .resume_pending_tasks(&adapter, Some("s1"), &keys, &cancel)
            .await
            .unwrap();
        assert_eq!(resumed.len(), 1);
        let completed = resumed.into_iter().next().unwrap().unwrap();
        assert_eq!(completed.status, MediaTaskStatus::Succeeded);
        assert_eq!(completed.artifacts.len(), 1);
        assert_eq!(completed.artifacts[0].size_bytes, 14);
        assert_eq!(artifacts.snapshot().len(), 1);

        // 3. Submit another and cancel remotely + locally
        let (to_cancel, _) = runner
            .submit_and_persist(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("k-cancel".into()),
                    session_id: Some("s2".into()),
                    parent_task_id: None,
                    model: "seedance".into(),
                    route_id: None,
                    payload: serde_json::json!({"prompt": "forest"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        let cancelled = runner
            .cancel_task(
                &adapter,
                &to_cancel.id,
                Some("user aborted"),
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status, MediaTaskStatus::Cancelled);
        assert!(cancelled_remotely.load(Ordering::SeqCst));
    }

    struct RemediationAdapter {
        base_url: String,
        mode: &'static str,
    }

    impl Engine3Adapter for RemediationAdapter {
        fn vendor(&self) -> &str {
            "remediation-mock"
        }

        fn kind(&self) -> MediaKind {
            MediaKind::Video
        }

        fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
            Ok(Engine2Request::post_json(
                format!("{}/submit", self.base_url),
                input.payload.clone(),
            ))
        }

        fn parse_submit_response(
            &self,
            _status: u16,
            _headers: &HeaderMap,
            body: &Value,
        ) -> Result<SubmitOutcome, MediaError> {
            let id = body["task_id"].as_str().unwrap_or("r-1").to_owned();
            Ok(SubmitOutcome::Accepted {
                remote_task_id: id,
                poll_cursor: None,
            })
        }

        fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
            let remote_id = task.remote_task_id.as_deref().unwrap_or("");
            Ok(Engine2Request::get(format!(
                "{}/poll/{remote_id}",
                self.base_url
            )))
        }

        fn parse_poll_response(
            &self,
            _status: u16,
            _headers: &HeaderMap,
            body: &Value,
            _task: &MediaTaskRecord,
        ) -> Result<PollOutcome, MediaError> {
            if let Some(code) = body
                .pointer("/base_resp/status_code")
                .and_then(Value::as_i64)
                && code != 0
            {
                let msg = body
                    .pointer("/base_resp/status_msg")
                    .and_then(Value::as_str)
                    .unwrap_or("poll biz fail");
                return Err(MediaError::business(Some(code.to_string()), msg, false));
            }
            match self.mode {
                "direct" => Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(body["url"].as_str().unwrap_or("")),
                        suggested_ext: "mp4".into(),
                        mime_type: "video/mp4".into(),
                    }],
                    poll_cursor: None,
                }),
                "two_step" => Ok(PollOutcome::Succeeded {
                    outputs: vec![MediaOutputSource::TwoStepDownload {
                        resolve_request: Engine2Request::get(format!(
                            "{}/resolve/{}",
                            self.base_url,
                            body["file_id"].as_str().unwrap_or("ok")
                        )),
                        url_pointer: "/file/download_url".into(),
                        download_headers: vec![],
                        download_sensitive_headers: vec![],
                        suggested_ext: "mp4".into(),
                        mime_type: "video/mp4".into(),
                    }],
                    poll_cursor: None,
                }),
                _ => Ok(PollOutcome::Failed {
                    upstream_code: None,
                    message: "unexpected".into(),
                    retryable: false,
                }),
            }
        }

        fn build_cancel(
            &self,
            task: &MediaTaskRecord,
        ) -> Result<Option<Engine2Request>, MediaError> {
            let remote_id = task.remote_task_id.as_deref().unwrap_or("");
            Ok(Some(Engine2Request::delete(format!(
                "{}/cancel/{remote_id}",
                self.base_url
            ))))
        }
    }

    #[tokio::test]
    async fn engine3_cancel_204_poll_non_retryable_fail_and_download_url_validation() {
        let dl_hits = Arc::new(AtomicUsize::new(0));
        let dl_hits_c = dl_hits.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let base_for_srv = base_url.clone();

        let app = Router::new()
            .route(
                "/submit",
                post(|axum::Json(body): axum::Json<Value>| async move {
                    let id = body["id"].as_str().unwrap_or("t-1");
                    (StatusCode::OK, format!(r#"{{"task_id":"{id}"}}"#))
                }),
            )
            .route(
                "/cancel/{id}",
                delete(|AxumPath(id): AxumPath<String>| async move {
                    match id.as_str() {
                        "c-204" => (StatusCode::NO_CONTENT, String::new()),
                        "c-200-empty" => (StatusCode::OK, String::new()),
                        "c-biz-err" => (
                            StatusCode::OK,
                            r#"{"base_resp":{"status_code":1004,"status_msg":"cannot cancel"}}"#
                                .to_owned(),
                        ),
                        _ => (StatusCode::OK, r#"{"cancelled":true}"#.to_owned()),
                    }
                }),
            )
            .route(
                "/poll/{id}",
                get(move |AxumPath(id): AxumPath<String>| {
                    let base = base_for_srv.clone();
                    async move {
                        match id.as_str() {
                            "p-biz-err" => (
                                StatusCode::OK,
                                r#"{"base_resp":{"status_code":4002,"status_msg":"rejected secret-tok"}}"#.to_owned(),
                            ),
                            "p-401" => (
                                StatusCode::UNAUTHORIZED,
                                r#"{"error":{"message":"bad secret-tok"}}"#.to_owned(),
                            ),
                            "d-cred" => (
                                StatusCode::OK,
                                format!(r#"{{"url":"http://u:p@127.0.0.1:{}/dl/video.mp4"}}"#, addr.port()),
                            ),
                            "d-nonloop" => (
                                StatusCode::OK,
                                r#"{"url":"http://example.com/dl/video.mp4"}"#.to_owned(),
                            ),
                            "t-cred" => (StatusCode::OK, r#"{"file_id":"f-cred"}"#.to_owned()),
                            "t-nonloop" => (StatusCode::OK, r#"{"file_id":"f-nonloop"}"#.to_owned()),
                            "t-ok" => (StatusCode::OK, r#"{"file_id":"f-ok"}"#.to_owned()),
                            _ => (StatusCode::OK, format!(r#"{{"url":"{base}/dl/video.mp4"}}"#)),
                        }
                    }
                }),
            )
            .route(
                "/resolve/{fid}",
                get(move |AxumPath(fid): AxumPath<String>| async move {
                    match fid.as_str() {
                        "f-cred" => (
                            StatusCode::OK,
                            format!(
                                r#"{{"file":{{"download_url":"http://secretuser:secretpass@127.0.0.1:{}/dl/video.mp4"}}}}"#,
                                addr.port()
                            ),
                        ),
                        "f-nonloop" => (
                            StatusCode::OK,
                            r#"{"file":{"download_url":"http://evil.example.com/dl/video.mp4"}}"#
                                .to_owned(),
                        ),
                        _ => (
                            StatusCode::OK,
                            format!(
                                r#"{{"file":{{"download_url":"http://127.0.0.1:{}/dl/video.mp4"}}}}"#,
                                addr.port()
                            ),
                        ),
                    }
                }),
            )
            .route(
                "/dl/video.mp4",
                get(move || {
                    dl_hits_c.fetch_add(1, Ordering::SeqCst);
                    async { (StatusCode::OK, b"VIDEO-BYTES".to_vec()) }
                }),
            );

        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner = Engine3Runner::new(
            MediaHttpClient::new().unwrap(),
            repo.clone(),
            artifacts.clone(),
        )
        .with_poll_config(Engine3PollConfig {
            initial_interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(5),
            ..Default::default()
        });
        let keys = CredentialSet::new(vec!["secret-tok".into()]);
        let cancel = CancellationToken::new();

        // 1. Cancel 204 No Content & 200 empty body succeed; 200 business error fails
        let two_step_adapter = RemediationAdapter {
            base_url: base_url.clone(),
            mode: "two_step",
        };
        for (idem, remote_id) in [("c-204-k", "c-204"), ("c-200-k", "c-200-empty")] {
            let (rec, _) = runner
                .submit_and_persist(
                    &two_step_adapter,
                    &Engine3SubmitInput {
                        idempotency_key: Some(idem.into()),
                        session_id: Some("s-cancel".into()),
                        parent_task_id: None,
                        model: "v1".into(),
                        route_id: None,
                        payload: serde_json::json!({"id": remote_id}),
                    },
                    &keys,
                    &cancel,
                )
                .await
                .unwrap();
            let cancelled = runner
                .cancel_task(&two_step_adapter, &rec.id, Some("stop"), &keys, &cancel)
                .await
                .unwrap();
            assert_eq!(cancelled.status, MediaTaskStatus::Cancelled);
        }
        let (rec_biz, _) = runner
            .submit_and_persist(
                &two_step_adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("c-biz-k".into()),
                    session_id: Some("s-cancel-biz".into()),
                    parent_task_id: None,
                    model: "v1".into(),
                    route_id: None,
                    payload: serde_json::json!({"id": "c-biz-err"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        let cancel_err = runner
            .cancel_task(&two_step_adapter, &rec_biz.id, Some("stop"), &keys, &cancel)
            .await
            .unwrap_err();
        assert_eq!(cancel_err.code, MediaErrorCode::BusinessError);
        assert_eq!(
            repo.get_task(&rec_biz.id).unwrap().unwrap().status,
            MediaTaskStatus::Submitted
        );

        // 2. Non-retryable poll errors (BusinessError & 401) mark task Failed and non-resumable
        for (idem, remote_id, expected_code) in [
            ("p-biz-k", "p-biz-err", MediaErrorCode::BusinessError),
            ("p-401-k", "p-401", MediaErrorCode::AuthRejected),
        ] {
            let err = runner
                .run_to_completion(
                    &two_step_adapter,
                    &Engine3SubmitInput {
                        idempotency_key: Some(idem.into()),
                        session_id: Some("s-poll-fail".into()),
                        parent_task_id: None,
                        model: "v1".into(),
                        route_id: None,
                        payload: serde_json::json!({"id": remote_id}),
                    },
                    &keys,
                    &cancel,
                )
                .await
                .unwrap_err();
            assert_eq!(err.code, expected_code);
            assert!(!err.message.contains("secret-tok"));
        }
        assert!(
            repo.list_resumable_tasks(Some("s-poll-fail"))
                .unwrap()
                .is_empty()
        );

        // 3. DirectDownload & TwoStepDownload reject embedded credentials & non-loopback http://
        let direct_adapter = RemediationAdapter {
            base_url: base_url.clone(),
            mode: "direct",
        };
        for (adapter, idem, remote_id) in [
            (&direct_adapter, "d-cred-k", "d-cred"),
            (&direct_adapter, "d-nonloop-k", "d-nonloop"),
            (&two_step_adapter, "t-cred-k", "t-cred"),
            (&two_step_adapter, "t-nonloop-k", "t-nonloop"),
        ] {
            let err = runner
                .run_to_completion(
                    adapter,
                    &Engine3SubmitInput {
                        idempotency_key: Some(idem.into()),
                        session_id: Some("s-url-reject".into()),
                        parent_task_id: None,
                        model: "v1".into(),
                        route_id: None,
                        payload: serde_json::json!({"id": remote_id}),
                    },
                    &keys,
                    &cancel,
                )
                .await
                .unwrap_err();
            assert_eq!(err.code, MediaErrorCode::InvalidRequest);
        }
        assert_eq!(dl_hits.load(Ordering::SeqCst), 0);

        // 4. Valid loopback TwoStepDownload succeeds
        let ok = runner
            .run_to_completion(
                &two_step_adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("t-ok-k".into()),
                    session_id: Some("s-url-ok".into()),
                    parent_task_id: None,
                    model: "v1".into(),
                    route_id: None,
                    payload: serde_json::json!({"id": "t-ok"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(ok.status, MediaTaskStatus::Succeeded);
        assert_eq!(dl_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn engine3_twostep_checks_business_status_and_returns_payloads() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");

        let app = Router::new()
            .route(
                "/submit",
                post(|axum::Json(body): axum::Json<Value>| async move {
                    let id = body["id"].as_str().unwrap_or("ok");
                    (StatusCode::OK, format!(r#"{{"task_id":"{id}"}}"#))
                }),
            )
            .route(
                "/poll/{id}",
                get(|AxumPath(id): AxumPath<String>| async move {
                    (StatusCode::OK, format!(r#"{{"file_id":"{id}"}}"#))
                }),
            )
            .route(
                "/resolve/{fid}",
                get(move |AxumPath(fid): AxumPath<String>| async move {
                    if fid == "biz-err" {
                        (
                            StatusCode::OK,
                            r#"{"base_resp":{"status_code":1004,"status_msg":"file expired"}}"#
                                .to_owned(),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            format!(
                                r#"{{"file":{{"download_url":"http://127.0.0.1:{}/dl/video.mp4"}}}}"#,
                                addr.port()
                            ),
                        )
                    }
                }),
            )
            .route(
                "/dl/video.mp4",
                get(|| async { (StatusCode::OK, b"OUT-PAYLOAD".to_vec()) }),
            );
        let _server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner = Engine3Runner::new(MediaHttpClient::new().unwrap(), repo, artifacts);
        let adapter = RemediationAdapter {
            base_url,
            mode: "two_step",
        };
        let keys = CredentialSet::new(vec![]);
        let cancel = CancellationToken::new();

        let err = runner
            .run_to_completion_with_outputs(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("k-2step-biz".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "v1".into(),
                    route_id: None,
                    payload: serde_json::json!({"id": "biz-err"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, MediaErrorCode::BusinessError);

        let completed = runner
            .run_to_completion_with_outputs(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("k-2step-ok".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "v1".into(),
                    route_id: None,
                    payload: serde_json::json!({"id": "ok"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(completed.record.status, MediaTaskStatus::Succeeded);
        assert_eq!(completed.payloads.len(), 1);
        assert_eq!(&completed.payloads[0].1[..], b"OUT-PAYLOAD");

        // Idempotent replay of terminal Succeeded task loads persisted payloads without extra HTTP
        let replayed = runner
            .run_to_completion_with_outputs(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("k-2step-ok".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "v1".into(),
                    route_id: None,
                    payload: serde_json::json!({"id": "ok"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(replayed.record.id, completed.record.id);
        assert_eq!(replayed.record.status, MediaTaskStatus::Succeeded);
        assert_eq!(replayed.payloads.len(), 1);
        assert_eq!(&replayed.payloads[0].1[..], b"OUT-PAYLOAD");
    }

    struct MultiResolveAdapter {
        base_url: String,
        nested_resolve: bool,
    }

    impl Engine3Adapter for MultiResolveAdapter {
        fn vendor(&self) -> &str {
            "multi-resolve"
        }

        fn kind(&self) -> MediaKind {
            MediaKind::Music
        }

        fn build_submit(&self, input: &Engine3SubmitInput) -> Result<Engine2Request, MediaError> {
            Ok(Engine2Request::post_json(
                format!("{}/submit", self.base_url),
                input.payload.clone(),
            ))
        }

        fn parse_submit_response(
            &self,
            _status: u16,
            _headers: &HeaderMap,
            body: &Value,
        ) -> Result<SubmitOutcome, MediaError> {
            Ok(SubmitOutcome::Accepted {
                remote_task_id: body["task_id"].as_str().unwrap_or("m1").to_owned(),
                poll_cursor: None,
            })
        }

        fn build_poll(&self, task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
            let id = task.remote_task_id.as_deref().unwrap_or("");
            Ok(Engine2Request::get(format!("{}/poll/{id}", self.base_url)))
        }

        fn parse_poll_response(
            &self,
            _status: u16,
            _headers: &HeaderMap,
            body: &Value,
            task: &MediaTaskRecord,
        ) -> Result<PollOutcome, MediaError> {
            let id = task.remote_task_id.as_deref().unwrap_or("");
            Ok(PollOutcome::Succeeded {
                outputs: vec![MediaOutputSource::AdapterResolve {
                    resolve_request: Engine2Request::get(format!(
                        "{}/artifacts/{id}",
                        self.base_url
                    )),
                    context: body.clone(),
                }],
                poll_cursor: None,
            })
        }

        fn parse_resolved_outputs(
            &self,
            _status: u16,
            _headers: &HeaderMap,
            body: &Value,
            _context: &Value,
            _task: &MediaTaskRecord,
        ) -> Result<Vec<MediaOutputSource>, MediaError> {
            if self.nested_resolve {
                return Ok(vec![MediaOutputSource::AdapterResolve {
                    resolve_request: Engine2Request::get(format!("{}/nested", self.base_url)),
                    context: Value::Null,
                }]);
            }
            let items = body["artifacts"].as_array().ok_or_else(|| {
                MediaError::new(MediaErrorCode::DecodeError, "missing artifacts array")
            })?;
            Ok(items
                .iter()
                .map(|item| MediaOutputSource::DirectDownload {
                    request: Engine2Request::get(item["url"].as_str().unwrap_or(""))
                        .with_controlled_redirects(5),
                    suggested_ext: item["ext"].as_str().unwrap_or("mp3").to_owned(),
                    mime_type: item["mime"].as_str().unwrap_or("audio/mpeg").to_owned(),
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn engine3_adapter_resolve_multi_artifacts_and_controlled_redirects() {
        let listener_cdn = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr_cdn = listener_cdn.local_addr().unwrap();
        let app_cdn = Router::new().route(
            "/cdn/clip1.mp3",
            get(|headers: axum::http::HeaderMap| async move {
                assert!(!headers.contains_key("authorization"));
                (StatusCode::OK, b"CLIP1-MP3".to_vec())
            }),
        );
        tokio::spawn(async move { axum::serve(listener_cdn, app_cdn).await.unwrap() });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let base_srv = base_url.clone();

        let app = Router::new()
            .route(
                "/submit",
                post(|axum::Json(body): axum::Json<Value>| async move {
                    let id = body["id"].as_str().unwrap_or("m1");
                    (StatusCode::OK, format!(r#"{{"task_id":"{id}"}}"#))
                }),
            )
            .route(
                "/poll/{id}",
                get(|| async { (StatusCode::OK, r#"{"status":"succeeded"}"#) }),
            )
            .route(
                "/artifacts/{id}",
                get(move |AxumPath(id): AxumPath<String>| {
                    let base = base_srv.clone();
                    async move {
                        if id == "bad-ssrf" {
                            (
                                StatusCode::OK,
                                serde_json::json!({
                                    "artifacts": [{ "url": "http://evil.example.com/x.mp3", "ext": "mp3", "mime": "audio/mpeg" }]
                                })
                                .to_string(),
                            )
                        } else {
                            (
                                StatusCode::OK,
                                serde_json::json!({
                                    "artifacts": [
                                        { "url": format!("{base}/redir"), "ext": "mp3", "mime": "audio/mpeg" },
                                        { "url": format!("{base}/clip2.png"), "ext": "png", "mime": "image/png" }
                                    ]
                                })
                                .to_string(),
                            )
                        }
                    }
                }),
            )
            .route(
                "/redir",
                get(move || async move {
                    let mut h = axum::http::HeaderMap::new();
                    h.insert(
                        "location",
                        format!("http://{addr_cdn}/cdn/clip1.mp3").parse().unwrap(),
                    );
                    (StatusCode::FOUND, h)
                }),
            )
            .route(
                "/clip2.png",
                get(|| async { (StatusCode::OK, b"PNGCOVER".to_vec()) }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner = Engine3Runner::new(MediaHttpClient::new().unwrap(), repo, artifacts);
        let keys = CredentialSet::new(vec![]);
        let cancel = CancellationToken::new();

        let adapter = MultiResolveAdapter {
            base_url: base_url.clone(),
            nested_resolve: false,
        };
        let completed = runner
            .run_to_completion_with_outputs(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("k-multi-1".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "suno-v4".into(),
                    route_id: None,
                    payload: serde_json::json!({"id": "ok"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(completed.record.status, MediaTaskStatus::Succeeded);
        assert_eq!(completed.payloads.len(), 2);
        assert_eq!(&completed.payloads[0].1[..], b"CLIP1-MP3");
        assert_eq!(&completed.payloads[1].1[..], b"PNGCOVER");

        // SSRF URL inside resolved manifest is rejected
        let ssrf_err = runner
            .run_to_completion_with_outputs(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("k-multi-ssrf".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "suno-v4".into(),
                    route_id: None,
                    payload: serde_json::json!({"id": "bad-ssrf"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(ssrf_err.code, MediaErrorCode::InvalidRequest);

        // Nested AdapterResolve is rejected
        let nested_adapter = MultiResolveAdapter {
            base_url,
            nested_resolve: true,
        };
        let nested_err = runner
            .run_to_completion_with_outputs(
                &nested_adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("k-multi-nested".into()),
                    session_id: None,
                    parent_task_id: None,
                    model: "suno-v4".into(),
                    route_id: None,
                    payload: serde_json::json!({"id": "ok"}),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(nested_err.code, MediaErrorCode::DecodeError);
    }

    #[tokio::test]
    async fn engine3_direct_download_without_redirects_blocks_private_and_ipv4_mapped_loopback() {
        struct DirectSsrfAdapter {
            target_url: String,
        }
        impl Engine3Adapter for DirectSsrfAdapter {
            fn vendor(&self) -> &str {
                "mock-ssrf"
            }
            fn kind(&self) -> MediaKind {
                MediaKind::Video
            }
            fn build_submit(
                &self,
                _input: &Engine3SubmitInput,
            ) -> Result<Engine2Request, MediaError> {
                Ok(Engine2Request::post_json(
                    "https://api.example.com/v1/submit",
                    serde_json::json!({}),
                ))
            }
            fn parse_submit_response(
                &self,
                _status: u16,
                _headers: &HeaderMap,
                _body: &Value,
            ) -> Result<SubmitOutcome, MediaError> {
                Ok(SubmitOutcome::Completed {
                    remote_task_id: None,
                    outputs: vec![MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(&self.target_url),
                        suggested_ext: "mp4".into(),
                        mime_type: "video/mp4".into(),
                    }],
                })
            }
            fn build_poll(&self, _task: &MediaTaskRecord) -> Result<Engine2Request, MediaError> {
                unreachable!()
            }
            fn parse_poll_response(
                &self,
                _status: u16,
                _headers: &HeaderMap,
                _body: &Value,
                _task: &MediaTaskRecord,
            ) -> Result<PollOutcome, MediaError> {
                unreachable!()
            }
        }

        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner = Engine3Runner::new(MediaHttpClient::new().unwrap(), repo, artifacts);
        let keys = CredentialSet::new(vec![]);
        let cancel = CancellationToken::new();
        let dummy_task = MediaTaskRecord {
            id: "t-ssrf-1".into(),
            idempotency_key: None,
            request_hash: "0".repeat(64),
            session_id: None,
            parent_task_id: None,
            media_kind: MediaKind::Video,
            vendor: "mock-ssrf".into(),
            model: "m".into(),
            route_id: None,
            status: MediaTaskStatus::Running,
            remote_task_id: None,
            poll_cursor: None,
            request_summary: Value::Null,
            artifacts: vec![],
            error_code: None,
            error_message: None,
            created_at: 0,
            updated_at: 0,
        };

        for bad_url in [
            "https://169.254.169.254/latest/meta-data",
            "https://10.0.0.1/secret.mp4",
            "https://[::ffff:127.0.0.1]/internal.mp4",
        ] {
            let err = runner
                .persist_output_source(
                    &dummy_task,
                    MediaOutputSource::DirectDownload {
                        request: Engine2Request::get(bad_url),
                        suggested_ext: "mp4".into(),
                        mime_type: "video/mp4".into(),
                    },
                    false,
                    &keys,
                    &cancel,
                )
                .await
                .unwrap_err();
            assert_eq!(err.code, MediaErrorCode::InvalidRequest, "url={bad_url}");
        }
        let _ = DirectSsrfAdapter {
            target_url: String::new(),
        };
    }

    #[tokio::test]
    async fn embedded_media_task_adapter_lifecycle_round_trip_and_cancel() {
        let polls = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let polls_clone = polls.clone();
        let cancelled_clone = cancelled.clone();

        let app = Router::new()
            .route(
                "/media/tasks",
                post(|| async {
                    axum::Json(serde_json::json!({
                        "taskId": "task-123",
                        "status": "queued"
                    }))
                }),
            )
            .route(
                "/media/tasks/{id}",
                get(move |AxumPath(_id): AxumPath<String>| {
                    let count = polls_clone.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if count == 0 {
                            axum::Json(serde_json::json!({ "status": "running" }))
                        } else {
                            axum::Json(serde_json::json!({
                                "status": "succeeded",
                                "mimeType": "video/mp4"
                            }))
                        }
                    }
                }),
            )
            .route(
                "/media/tasks/{id}/content",
                get(|| async {
                    (
                        StatusCode::OK,
                        [("content-type", "video/mp4")],
                        b"FAKE_MP4_BYTES".to_vec(),
                    )
                }),
            )
            .route(
                "/media/tasks/{id}/cancel",
                post(move |AxumPath(_id): AxumPath<String>| {
                    cancelled_clone.store(true, Ordering::SeqCst);
                    async { axum::Json(serde_json::json!({ "status": "cancelled" })) }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let adapter = EmbeddedMediaTaskAdapter::new(
            "embedded-media",
            MediaKind::Video,
            format!("http://{addr}"),
            vec![("Authorization".into(), "Bearer tok-1".into())],
            vec!["Authorization".into()],
            true,
        )
        .unwrap();
        let http = MediaHttpClient::new().unwrap();
        let repo = Arc::new(InMemoryMediaTaskRepository::new());
        let artifacts = Arc::new(InMemoryMediaArtifactStore::new());
        let runner = Engine3Runner::new(http, repo.clone(), artifacts)
            .with_allow_http_loopback_download(true)
            .with_poll_config(Engine3PollConfig {
                initial_interval: Duration::from_millis(5),
                max_interval: Duration::from_millis(10),
                backoff_factor: 1.0,
                max_attempts: 5,
                max_wall_time: Duration::from_secs(5),
            });
        let keys = CredentialSet::new(vec!["tok-1".into()]);
        let cancel = CancellationToken::new();

        let completion = runner
            .run_to_completion_with_outputs(
                &adapter,
                &Engine3SubmitInput {
                    idempotency_key: Some("idem-1".into()),
                    session_id: Some("sess-1".into()),
                    parent_task_id: None,
                    model: "sora-2".into(),
                    route_id: None,
                    payload: serde_json::json!({ "prompt": "a sunset" }),
                },
                &keys,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(completion.record.status, MediaTaskStatus::Succeeded);
        assert_eq!(completion.payloads.len(), 1);
        assert_eq!(completion.payloads[0].1.as_ref(), b"FAKE_MP4_BYTES");

        // Cancel flow against a submitted queued task.
        let queued = MediaTaskRecord {
            id: "local-2".into(),
            idempotency_key: None,
            request_hash: "0".repeat(64),
            session_id: None,
            parent_task_id: None,
            media_kind: MediaKind::Video,
            vendor: "embedded-media".into(),
            model: "sora-2".into(),
            route_id: None,
            status: MediaTaskStatus::Running,
            remote_task_id: Some("task-123".into()),
            poll_cursor: None,
            request_summary: Value::Null,
            artifacts: vec![],
            error_code: None,
            error_message: None,
            created_at: 0,
            updated_at: 0,
        };
        repo.create_task(&NewMediaTask {
            id: queued.id.clone(),
            idempotency_key: None,
            request_hash: queued.request_hash.clone(),
            session_id: None,
            parent_task_id: None,
            media_kind: queued.media_kind,
            vendor: queued.vendor.clone(),
            model: queued.model.clone(),
            route_id: None,
            status: MediaTaskStatus::Running,
            remote_task_id: queued.remote_task_id.clone(),
            poll_cursor: None,
            request_summary: Value::Null,
            artifacts: vec![],
            error_code: None,
            error_message: None,
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
        let rec = runner
            .cancel_task(&adapter, "local-2", Some("user cancel"), &keys, &cancel)
            .await
            .unwrap();
        assert_eq!(rec.status, MediaTaskStatus::Cancelled);
        assert!(cancelled.load(Ordering::SeqCst));

        srv.abort();
    }
}
