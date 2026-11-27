//! Run-scoped `modelProxy` `CredentialSource` (`codaKernel/1 §5`, zero DPAPI/budget/`network_subjects`).

use crate::domain::Route;
use crate::kernel_embedded::ModelProxyConfig;
use crate::kernel_embedded::event_hub::RunEventHub;
use crate::provider_runtime::{CompletionInput, Emit, ProviderRuntime};
use crate::provider_transport::{
    ProviderClient, ProviderError, ProviderPermit, ProviderProtocol, RetryAfter, TransportError,
};
use crate::redaction::CredentialSet;
use crate::runtime_seams::{
    CompletionTurnResult, CredentialSource, DetailedCompletionTurnResult, SlotGuard,
};
use anyhow::Result;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

struct NoopSlotGuard;
impl SlotGuard for NoopSlotGuard {
    fn release(&mut self) {}
}

#[derive(Clone, Debug)]
pub struct EmbeddedModelError {
    pub code: &'static str,
    pub status: Option<u16>,
    pub upstream_code: Option<String>,
    pub retryable: bool,
    pub message: String,
}

impl std::fmt::Display for EmbeddedModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EmbeddedModelError {}

pub fn is_context_overflow_error(status: Option<u16>, code: Option<&str>, message: &str) -> bool {
    if let Some(s) = status
        && !matches!(s, 400 | 413 | 422)
    {
        return false;
    }
    let hay = format!("{} {}", code.unwrap_or_default(), message).to_ascii_lowercase();
    [
        "context_length_exceeded",
        "maximum context length",
        "prompt is too long",
        "too many tokens",
        "context_window",
        "input is too long",
        "exceeds the maximum number of tokens",
    ]
    .iter()
    .any(|pat| hay.contains(pat))
}

fn bound_upstream_code(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(1024).collect())
}

pub fn classify_transport_error(
    err: TransportError,
    scrub: &dyn Fn(&str) -> String,
) -> EmbeddedModelError {
    match err {
        TransportError::Provider(ProviderError {
            status,
            upstream_code,
            message,
            retryable,
        }) => {
            let clean_msg = scrub(
                message
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("upstream model request failed"),
            );
            let clean_code = upstream_code.and_then(|c| bound_upstream_code(&scrub(&c)));
            if matches!(status, 401 | 403 | 409) {
                return EmbeddedModelError {
                    code: "MODEL_PROXY_REJECTED",
                    status: Some(status),
                    upstream_code: clean_code,
                    retryable: false,
                    message: clean_msg,
                };
            }
            if is_context_overflow_error(Some(status), clean_code.as_deref(), &clean_msg) {
                return EmbeddedModelError {
                    code: "CONTEXT_OVERFLOW",
                    status: Some(status),
                    upstream_code: clean_code,
                    retryable: false,
                    message: clean_msg,
                };
            }
            EmbeddedModelError {
                code: "MODEL_UPSTREAM_ERROR",
                status: Some(status),
                upstream_code: clean_code,
                retryable: retryable || status == 429 || (500..=599).contains(&status),
                message: clean_msg,
            }
        }
        TransportError::Response { status } => EmbeddedModelError {
            code: "MODEL_UPSTREAM_ERROR",
            status: Some(status),
            upstream_code: None,
            retryable: status == 429 || (500..=599).contains(&status),
            message: format!("upstream returned HTTP {status}"),
        },
        TransportError::InvalidResponse { status } => EmbeddedModelError {
            code: "PROTOCOL_DECODE_ERROR",
            status: Some(status),
            upstream_code: None,
            retryable: false,
            message: format!("invalid upstream response ({status})"),
        },
        TransportError::Invalid => EmbeddedModelError {
            code: "INTERNAL",
            status: None,
            upstream_code: None,
            retryable: false,
            message: "invalid embedded model proxy request".into(),
        },
        TransportError::Unknown => EmbeddedModelError {
            code: "MODEL_UPSTREAM_ERROR",
            status: None,
            upstream_code: None,
            retryable: true,
            message: "upstream stream interrupted or decode failed".into(),
        },
    }
}

pub struct EmbeddedCredentialSource {
    client: ProviderClient,
    runtime: ProviderRuntime,
    proxy: ModelProxyConfig,
    protocol: ProviderProtocol,
    hub: Option<Arc<RunEventHub>>,
    thinking_level: Arc<Mutex<Option<String>>>,
    scrub_keys: CredentialSet,
    allow_empty_text: bool,
}

impl EmbeddedCredentialSource {
    pub fn new(
        proxy: ModelProxyConfig,
        protocol: ProviderProtocol,
        hub: Option<Arc<RunEventHub>>,
        thinking_level: Arc<Mutex<Option<String>>>,
    ) -> Result<Self> {
        let scrub_keys = CredentialSet::new(vec![proxy.token.clone()]);
        Ok(Self {
            client: ProviderClient::new()?,
            runtime: ProviderRuntime::builtin_with_state(true)?,
            proxy,
            protocol,
            hub,
            thinking_level,
            scrub_keys,
            allow_empty_text: false,
        })
    }

    pub fn with_allow_empty_text(mut self, allow: bool) -> Self {
        self.allow_empty_text = allow;
        self
    }

    pub(crate) async fn execute_once_with_retries(
        &self,
        input: &CompletionInput<'_>,
        emit_text: &mut Emit<'_>,
        emit_reasoning: &mut Emit<'_>,
        cancel: &CancellationToken,
    ) -> std::result::Result<DetailedCompletionTurnResult, EmbeddedModelError> {
        let thinking = self.thinking_level.lock().unwrap().clone();
        let proxy_route = Route {
            id: "embedded-proxy".into(),
            name: "Embedded Proxy".into(),
            base_url: self.proxy.base_url.clone(),
            model: input.model.to_owned(),
            max_tokens: input.max_tokens,
            parallel_limit: 1,
            key_env: None,
            protocol: Some(self.protocol),
        };
        let scrub = |s: &str| self.scrub_keys.scrub(s);

        const MAX_RETRIES: u32 = 3;
        for attempt in 0..=MAX_RETRIES {
            if cancel.is_cancelled() {
                return Err(EmbeddedModelError {
                    code: "INTERNAL",
                    status: None,
                    upstream_code: None,
                    retryable: false,
                    message: "run aborted".into(),
                });
            }

            let hint = RetryAfter::default();
            let call_id = format!("embed-{}", uuid::Uuid::now_v7());
            let permit = ProviderPermit::new_embedded_proxy(
                &self.proxy.base_url,
                self.protocol,
                self.proxy.token.clone(),
                call_id,
                input.model.to_owned(),
            )
            .map_err(|e| classify_transport_error(e, &scrub))?
            .with_scrub_keys(vec![self.proxy.token.clone()])
            .track_retry_after(hint.clone());

            let prepared = self
                .runtime
                .prepare(input)
                .map_err(|_| EmbeddedModelError {
                    code: "INTERNAL",
                    status: None,
                    upstream_code: None,
                    retryable: false,
                    message: "failed to prepare completion request".into(),
                })?;
            let lease =
                self.runtime
                    .acquire_unpersisted(prepared)
                    .map_err(|_| EmbeddedModelError {
                        code: "INTERNAL",
                        status: None,
                        upstream_code: None,
                        retryable: false,
                        message: "failed to acquire completion lease".into(),
                    })?;
            let lease = lease
                .with_thinking_level(thinking.clone())
                .with_allow_empty_text(self.allow_empty_text);

            let fut = lease.execute_with_reasoning_detailed(
                &self.client,
                permit,
                &proxy_route,
                emit_text,
                emit_reasoning,
            );
            let res = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Err(EmbeddedModelError {
                        code: "INTERNAL",
                        status: None,
                        upstream_code: None,
                        retryable: false,
                        message: "run aborted".into(),
                    });
                }
                r = fut => r,
            };

            match res {
                Ok((output, breakdown, response_id)) => {
                    if attempt > 0
                        && let Some(hub) = &self.hub
                    {
                        hub.emit(json!({ "type": "retryCleared" }));
                    }
                    return Ok(DetailedCompletionTurnResult {
                        turn: CompletionTurnResult {
                            message: output.0,
                            usage: output.1,
                        },
                        usage_breakdown: breakdown,
                        response_id,
                    });
                }
                Err(transport_err) => {
                    // 401/403/409 from proxy MUST NEVER retry (`protocol.md §5`).
                    if let TransportError::Provider(ref pe) = transport_err
                        && matches!(pe.status, 401 | 403 | 409)
                    {
                        return Err(classify_transport_error(transport_err, &scrub));
                    }

                    let retry_status = match &transport_err {
                        TransportError::Response { status }
                            if *status == 429 || (500..=599).contains(status) =>
                        {
                            Some(*status)
                        }
                        TransportError::Provider(pe)
                            if pe.status == 429 || (500..=599).contains(&pe.status) =>
                        {
                            Some(pe.status)
                        }
                        _ => None,
                    };

                    if let Some(status) = retry_status
                        && attempt < MAX_RETRIES
                    {
                        let retry_num = attempt + 1;
                        if let Some(hub) = &self.hub {
                            hub.emit(json!({
                                "type": "retry",
                                "attempt": retry_num,
                                "code": format!("HTTP_{status}"),
                            }));
                        }
                        let backoff = Duration::from_millis(250 * (1u64 << attempt));
                        let delay = hint
                            .get()
                            .copied()
                            .unwrap_or(backoff)
                            .max(backoff)
                            .min(Duration::from_secs(10));
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => {
                                return Err(EmbeddedModelError {
                                    code: "INTERNAL",
                                    status: None,
                                    upstream_code: None,
                                    retryable: false,
                                    message: "run aborted".into(),
                                });
                            }
                            _ = tokio::time::sleep(delay) => {}
                        }
                        continue;
                    }

                    return Err(classify_transport_error(transport_err, &scrub));
                }
            }
        }

        Err(EmbeddedModelError {
            code: "MODEL_UPSTREAM_ERROR",
            status: None,
            upstream_code: None,
            retryable: true,
            message: "retries exhausted".into(),
        })
    }
}

impl CredentialSource for EmbeddedCredentialSource {
    fn credential_set(&self, _route: &Route) -> Result<CredentialSet> {
        Ok(self.scrub_keys.clone())
    }

    async fn acquire_slot(&self, _cancel: &CancellationToken) -> Result<Box<dyn SlotGuard>> {
        Ok(Box::new(NoopSlotGuard))
    }

    async fn complete_streaming<'a>(
        &'a self,
        task: &'a crate::domain::Task,
        route: &'a Route,
        input: &'a CompletionInput<'a>,
        emit_text: &'a mut Emit<'a>,
        emit_reasoning: &'a mut Emit<'a>,
        cancel: &'a CancellationToken,
    ) -> Result<CompletionTurnResult> {
        let detailed = self
            .complete_streaming_detailed(task, route, input, emit_text, emit_reasoning, cancel)
            .await?;
        Ok(detailed.turn)
    }

    async fn complete_streaming_detailed<'a>(
        &'a self,
        _task: &'a crate::domain::Task,
        _route: &'a Route,
        input: &'a CompletionInput<'a>,
        emit_text: &'a mut Emit<'a>,
        emit_reasoning: &'a mut Emit<'a>,
        cancel: &'a CancellationToken,
    ) -> Result<DetailedCompletionTurnResult> {
        self.execute_once_with_retries(input, emit_text, emit_reasoning, cancel)
            .await
            .map_err(anyhow::Error::new)
    }
}

pub fn embedded_media_base_url(proxy_base_url: &str) -> String {
    proxy_base_url.trim().trim_end_matches('/').to_owned()
}

pub fn classify_media_error(
    err: crate::media::MediaError,
    scrub: &dyn Fn(&str) -> String,
) -> EmbeddedModelError {
    let clean_msg = scrub(if err.message.trim().is_empty() {
        "upstream media proxy request failed"
    } else {
        err.message.trim()
    });
    let clean_code = err
        .upstream_code
        .as_deref()
        .and_then(|c| bound_upstream_code(&scrub(c)));

    if matches!(
        err.code,
        crate::media::MediaErrorCode::AuthRejected
            | crate::media::MediaErrorCode::Forbidden
            | crate::media::MediaErrorCode::Conflict
    ) || matches!(err.status, Some(401 | 403 | 409))
    {
        return EmbeddedModelError {
            code: "MODEL_PROXY_REJECTED",
            status: err.status,
            upstream_code: clean_code,
            retryable: false,
            message: clean_msg,
        };
    }

    let code = match err.code {
        crate::media::MediaErrorCode::InvalidRequest => "INVALID_REQUEST",
        crate::media::MediaErrorCode::DecodeError => "PROTOCOL_DECODE_ERROR",
        crate::media::MediaErrorCode::BusinessError
        | crate::media::MediaErrorCode::RateLimited
        | crate::media::MediaErrorCode::ProviderError
        | crate::media::MediaErrorCode::Timeout
        | crate::media::MediaErrorCode::Transport => "MODEL_UPSTREAM_ERROR",
        crate::media::MediaErrorCode::AuthRejected
        | crate::media::MediaErrorCode::Forbidden
        | crate::media::MediaErrorCode::Conflict => "MODEL_PROXY_REJECTED",
        crate::media::MediaErrorCode::Cancelled
        | crate::media::MediaErrorCode::ArtifactWriteFailed
        | crate::media::MediaErrorCode::PersistenceFailed
        | crate::media::MediaErrorCode::NotFound => "INTERNAL",
    };

    EmbeddedModelError {
        code,
        status: err.status,
        upstream_code: clean_code,
        retryable: err.retryable,
        message: clean_msg,
    }
}

impl crate::media::MediaCredentialSource for EmbeddedCredentialSource {
    async fn resolve_media_endpoint<'a>(
        &'a self,
        route_id: Option<&'a str>,
        vendor: &'a str,
        model: &'a str,
    ) -> std::result::Result<
        (
            crate::media::MediaEndpointConfig,
            crate::media::MediaCredential,
        ),
        crate::media::MediaError,
    > {
        let base_url = embedded_media_base_url(&self.proxy.base_url);
        let _ = crate::media::validate_media_url(&base_url, true)?;
        let mut cfg = crate::media::MediaEndpointConfig::new(vendor, base_url, model);
        cfg.route_id = route_id.map(ToOwned::to_owned);
        cfg.embedded_proxy = true;
        let cred = crate::media::MediaCredential::proxy_token(self.proxy.token.clone())?;
        Ok((cfg, cred))
    }
}

pub struct EmbeddedMediaProxyClient {
    http: crate::media::engine2::MediaHttpClient,
    repo: Arc<crate::media::InMemoryMediaTaskRepository>,
    artifacts: Arc<crate::media::InMemoryMediaArtifactStore>,
    base_url: String,
    token: String,
    scrub_keys: CredentialSet,
}

impl EmbeddedMediaProxyClient {
    pub fn new(
        proxy_base_url: impl Into<String>,
        token: impl Into<String>,
    ) -> std::result::Result<Self, EmbeddedModelError> {
        let raw_base = proxy_base_url.into();
        let token = token.into();
        let scrub_keys = CredentialSet::new(vec![token.clone()]);
        let scrub = |s: &str| scrub_keys.scrub(s);

        if token.trim().is_empty() {
            return Err(EmbeddedModelError {
                code: "INVALID_REQUEST",
                status: None,
                upstream_code: None,
                retryable: false,
                message: "embedded media proxy token must be non-empty".into(),
            });
        }
        let media_base = embedded_media_base_url(&raw_base);
        let validated = crate::media::validate_media_url(&media_base, true)
            .map_err(|e| classify_media_error(e, &scrub))?;
        let http = crate::media::engine2::MediaHttpClient::new()
            .map_err(|e| classify_media_error(e, &scrub))?;

        Ok(Self {
            http,
            repo: Arc::new(crate::media::InMemoryMediaTaskRepository::new()),
            artifacts: Arc::new(crate::media::InMemoryMediaArtifactStore::new()),
            base_url: validated.as_str().trim_end_matches('/').to_owned(),
            token,
            scrub_keys,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn repository(&self) -> &Arc<crate::media::InMemoryMediaTaskRepository> {
        &self.repo
    }

    pub fn artifacts(&self) -> &Arc<crate::media::InMemoryMediaArtifactStore> {
        &self.artifacts
    }

    fn scrub(&self, raw: &str) -> String {
        self.scrub_keys.scrub(raw)
    }

    fn auth_headers(&self) -> (Vec<(String, String)>, Vec<String>) {
        (
            vec![("Authorization".to_owned(), format!("Bearer {}", self.token))],
            vec!["Authorization".to_owned()],
        )
    }

    fn map_err(&self, err: crate::media::MediaError) -> EmbeddedModelError {
        classify_media_error(err, &|s| self.scrub(s))
    }

    fn clean_rel_suffix(&self, suffix: &str) -> std::result::Result<String, EmbeddedModelError> {
        let trimmed = suffix.trim().trim_start_matches('/');
        if trimmed.is_empty()
            || trimmed.contains("..")
            || trimmed.contains('\\')
            || trimmed.contains('#')
        {
            return Err(EmbeddedModelError {
                code: "INVALID_REQUEST",
                status: None,
                upstream_code: None,
                retryable: false,
                message: "invalid media proxy route suffix".into(),
            });
        }
        Ok(trimmed.to_owned())
    }

    async fn post_json_route(
        &self,
        route: &str,
        payload: serde_json::Value,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        let clean = self.clean_rel_suffix(route)?;
        let (headers, sensitive) = self.auth_headers();
        let req = crate::media::engine2::Engine2Request::post_json(
            format!("{}/{}", self.base_url, clean),
            payload,
        )
        .with_headers(headers, sensitive);
        let resp = self
            .http
            .execute_json(
                &req,
                &self.scrub_keys,
                crate::media::engine2::check_standard_business_status,
                cancel,
            )
            .await
            .map_err(|e| self.map_err(e))?;
        Ok(resp.body)
    }

    async fn post_multipart_route(
        &self,
        route: &str,
        form: crate::media::engine2::MultipartForm,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        let clean = self.clean_rel_suffix(route)?;
        let (headers, sensitive) = self.auth_headers();
        let req = crate::media::engine2::Engine2Request::post_multipart(
            format!("{}/{}", self.base_url, clean),
            form,
        )
        .with_headers(headers, sensitive);
        let resp = self
            .http
            .execute_json(
                &req,
                &self.scrub_keys,
                crate::media::engine2::check_standard_business_status,
                cancel,
            )
            .await
            .map_err(|e| self.map_err(e))?;
        Ok(resp.body)
    }

    pub async fn embeddings(
        &self,
        payload: serde_json::Value,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        self.post_json_route("embeddings", payload, cancel).await
    }

    pub async fn rerank(
        &self,
        payload: serde_json::Value,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        self.post_json_route("rerank", payload, cancel).await
    }

    pub async fn images_generations(
        &self,
        payload: serde_json::Value,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        self.post_json_route("images/generations", payload, cancel)
            .await
    }

    pub async fn images_edits_multipart(
        &self,
        form: crate::media::engine2::MultipartForm,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        self.post_multipart_route("images/edits", form, cancel)
            .await
    }

    pub async fn audio_speech(
        &self,
        payload: serde_json::Value,
        cancel: &CancellationToken,
    ) -> std::result::Result<bytes::Bytes, EmbeddedModelError> {
        let (headers, sensitive) = self.auth_headers();
        let req = crate::media::engine2::Engine2Request::post_json(
            format!("{}/audio/speech", self.base_url),
            payload,
        )
        .with_headers(headers, sensitive)
        .with_max_response_bytes(crate::media::engine2::MEDIA_DEFAULT_ARTIFACT_LIMIT);
        let resp = self
            .http
            .execute_bytes(&req, &self.scrub_keys, cancel)
            .await
            .map_err(|e| self.map_err(e))?;
        Ok(resp.bytes)
    }

    pub async fn audio_transcriptions_multipart(
        &self,
        form: crate::media::engine2::MultipartForm,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        self.post_multipart_route("audio/transcriptions", form, cancel)
            .await
    }

    pub async fn audio_translations_multipart(
        &self,
        form: crate::media::engine2::MultipartForm,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        self.post_multipart_route("audio/translations", form, cancel)
            .await
    }

    pub async fn gemini_post(
        &self,
        suffix_path: &str,
        payload: serde_json::Value,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        let clean = self.clean_rel_suffix(suffix_path)?;
        self.post_json_route(&format!("gemini/{clean}"), payload, cancel)
            .await
    }

    pub async fn gemini_sse(
        &self,
        suffix_path: &str,
        payload: serde_json::Value,
        cancel: &CancellationToken,
    ) -> std::result::Result<Vec<crate::media::engine2::SseEvent>, EmbeddedModelError> {
        let clean = self.clean_rel_suffix(suffix_path)?;
        let (headers, sensitive) = self.auth_headers();
        let req = crate::media::engine2::Engine2Request::post_json(
            format!("{}/gemini/{clean}", self.base_url),
            payload,
        )
        .with_headers(headers, sensitive);
        let mut events = Vec::new();
        self.http
            .execute_sse_stream(
                &req,
                &self.scrub_keys,
                |ev| {
                    events.push(ev);
                    Ok(crate::media::engine2::StreamControl::Continue)
                },
                cancel,
            )
            .await
            .map_err(|e| self.map_err(e))?;
        Ok(events)
    }

    pub async fn gemini_get_operation(
        &self,
        operation_name: &str,
        cancel: &CancellationToken,
    ) -> std::result::Result<serde_json::Value, EmbeddedModelError> {
        let clean = self.clean_rel_suffix(operation_name)?;
        let (headers, sensitive) = self.auth_headers();
        let req =
            crate::media::engine2::Engine2Request::get(format!("{}/gemini/{clean}", self.base_url))
                .with_headers(headers, sensitive);
        let resp = self
            .http
            .execute_json(
                &req,
                &self.scrub_keys,
                crate::media::engine2::check_standard_business_status,
                cancel,
            )
            .await
            .map_err(|e| self.map_err(e))?;
        Ok(resp.body)
    }

    pub fn engine3_runner(
        &self,
    ) -> crate::media::engine3::Engine3Runner<
        crate::media::InMemoryMediaTaskRepository,
        crate::media::InMemoryMediaArtifactStore,
    > {
        crate::media::engine3::Engine3Runner::new(
            self.http.clone(),
            self.repo.clone(),
            self.artifacts.clone(),
        )
        .with_allow_http_loopback_download(true)
    }

    pub fn build_task_adapter(
        &self,
        vendor: &str,
        kind: crate::media::MediaKind,
    ) -> std::result::Result<crate::media::engine3::EmbeddedMediaTaskAdapter, EmbeddedModelError>
    {
        let (headers, sensitive) = self.auth_headers();
        crate::media::engine3::EmbeddedMediaTaskAdapter::new(
            vendor,
            kind,
            &self.base_url,
            headers,
            sensitive,
            true,
        )
        .map_err(|e| self.map_err(e))
    }

    pub async fn run_media_task(
        &self,
        vendor: &str,
        kind: crate::media::MediaKind,
        model: &str,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        cancel: &CancellationToken,
    ) -> std::result::Result<crate::media::engine3::Engine3Completion, EmbeddedModelError> {
        let adapter = self.build_task_adapter(vendor, kind)?;
        let runner = self.engine3_runner();
        let input = crate::media::engine3::Engine3SubmitInput {
            idempotency_key,
            session_id: None,
            parent_task_id: None,
            model: model.to_owned(),
            route_id: None,
            payload,
        };
        runner
            .run_to_completion_with_outputs(&adapter, &input, &self.scrub_keys, cancel)
            .await
            .map_err(|e| self.map_err(e))
    }

    pub async fn cancel_media_task(
        &self,
        vendor: &str,
        kind: crate::media::MediaKind,
        task_id: &str,
        reason: Option<&str>,
        cancel: &CancellationToken,
    ) -> std::result::Result<crate::media::MediaTaskRecord, EmbeddedModelError> {
        let adapter = self.build_task_adapter(vendor, kind)?;
        let runner = self.engine3_runner();
        runner
            .cancel_task(&adapter, task_id, reason, &self.scrub_keys, cancel)
            .await
            .map_err(|e| self.map_err(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_transport::NetworkPurpose;

    #[test]
    fn embedded_proxy_permit_does_not_strip_v1_or_guess_protocol() {
        let permit = ProviderPermit::new_embedded_proxy(
            "http://127.0.0.1:61234/m/run-1/v1",
            ProviderProtocol::Anthropic,
            "tok-secret".into(),
            "c1".into(),
            "claude-sonnet-4-5".into(),
        )
        .unwrap();

        // Succeeds with explicit Anthropic protocol without stripping `/v1`.
        permit
            .matches_with_protocol(
                "http://127.0.0.1:61234/m/run-1/v1",
                NetworkPurpose::Completion,
                Some("claude-sonnet-4-5"),
                None,
                Some(ProviderProtocol::Anthropic),
            )
            .unwrap();

        // Rejects missing protocol (no URL-path protocol guessing in embedded mode).
        assert!(
            permit
                .matches_with_protocol(
                    "http://127.0.0.1:61234/m/run-1/v1",
                    NetworkPurpose::Completion,
                    Some("claude-sonnet-4-5"),
                    None,
                    None,
                )
                .is_err()
        );
    }

    #[test]
    fn proxy_401_403_409_map_to_model_proxy_rejected_non_retryable() {
        for status in [401, 403, 409] {
            let err = classify_transport_error(
                TransportError::Provider(ProviderError {
                    status,
                    upstream_code: Some("CUSTOM_PROVIDER_DISABLED".into()),
                    message: Some("rejected with tok-secret".into()),
                    retryable: false,
                }),
                &|s| s.replace("tok-secret", "[redacted]"),
            );
            assert_eq!(err.code, "MODEL_PROXY_REJECTED");
            assert_eq!(err.status, Some(status));
            assert_eq!(
                err.upstream_code.as_deref(),
                Some("CUSTOM_PROVIDER_DISABLED")
            );
            assert!(!err.retryable);
            assert!(!err.message.contains("tok-secret"));
        }
        let long_code = "X".repeat(2048);
        let bounded = classify_transport_error(
            TransportError::Provider(ProviderError {
                status: 403,
                upstream_code: Some(long_code),
                message: Some("denied".into()),
                retryable: false,
            }),
            &|s| s.to_owned(),
        );
        assert_eq!(bounded.upstream_code.as_deref().map(str::len), Some(1024));
    }

    #[tokio::test]
    async fn embedded_credential_source_retries_429_rejects_403_and_forwards_thinking_level() {
        use axum::{Json, Router, http::StatusCode, http::header, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let attempts = Arc::new(AtomicUsize::new(0));
        let captured_body = Arc::new(Mutex::new(None::<serde_json::Value>));
        let att_c = attempts.clone();
        let cap_c = captured_body.clone();

        let app = Router::new()
            .route(
                "/m/run-retry/responses",
                post(move |Json(body): Json<serde_json::Value>| {
                    let att = att_c.clone();
                    let cap = cap_c.clone();
                    async move {
                        *cap.lock().unwrap() = Some(body);
                        let n = att.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            (
                                StatusCode::TOO_MANY_REQUESTS,
                                [(header::RETRY_AFTER, "0")],
                                "rate limited",
                            )
                                .into_response()
                        } else {
                            let sse = concat!(
                                "event: response.output_text.delta\n",
                                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
                                "event: response.completed\n",
                                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}\n\n",
                                "data: [DONE]\n\n"
                            );
                            (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "text/event-stream")],
                                sse,
                            )
                                .into_response()
                        }
                    }
                }),
            )
            .route(
                "/m/run-403/responses",
                post(|| async {
                    (
                        StatusCode::FORBIDDEN,
                        [(header::CONTENT_TYPE, "application/json")],
                        r#"{"error":{"code":"CLOUD_RELAY_FORBIDDEN","message":"blocked tok-secret"}}"#,
                    )
                }),
            );
        use axum::response::IntoResponse;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let hub = Arc::new(RunEventHub::new("s1", "run-retry"));
        let creds = EmbeddedCredentialSource::new(
            ModelProxyConfig {
                base_url: format!("http://{addr}/m/run-retry"),
                token: "tok-secret".into(),
                protocol: "openai-responses".into(),
            },
            ProviderProtocol::OpenAiResponses,
            Some(hub.clone()),
            Arc::new(Mutex::new(Some("high".into()))),
        )
        .unwrap();

        let msgs = vec![crate::messages::NeutralMessage::user_text("hi")];
        let input = CompletionInput {
            model: "gpt-5.2",
            max_tokens: 1024,
            messages: &msgs,
            tools: &[],
        };
        let cancel = CancellationToken::new();
        let mut t = |_d: &str| Ok(());
        let mut r = |_d: &str| Ok(());
        let res = creds
            .execute_once_with_retries(&input, &mut t, &mut r, &cancel)
            .await
            .unwrap();
        assert_eq!(res.turn.message.text_content(), "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let req_body = captured_body.lock().unwrap().clone().unwrap();
        assert_eq!(req_body["reasoning"]["effort"], "high");

        let (replay, _, _) = hub.subscribe(0).unwrap();
        assert!(replay.iter().any(|e| e.event["type"] == "retry"));
        assert!(replay.iter().any(|e| e.event["type"] == "retryCleared"));

        // Now verify 403 does 0 retries and returns MODEL_PROXY_REJECTED with scrubbed message.
        let creds_403 = EmbeddedCredentialSource::new(
            ModelProxyConfig {
                base_url: format!("http://{addr}/m/run-403"),
                token: "tok-secret".into(),
                protocol: "openai-responses".into(),
            },
            ProviderProtocol::OpenAiResponses,
            None,
            Arc::new(Mutex::new(None)),
        )
        .unwrap();
        let err = creds_403
            .execute_once_with_retries(&input, &mut t, &mut r, &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.code, "MODEL_PROXY_REJECTED");
        assert_eq!(err.status, Some(403));
        assert_eq!(err.upstream_code.as_deref(), Some("CLOUD_RELAY_FORBIDDEN"));
        assert!(!err.retryable);
        assert!(!err.message.contains("tok-secret"));

        srv.abort();
    }

    #[tokio::test]
    async fn embedded_credential_source_forwards_thinking_level_for_anthropic_and_openai_completions()
     {
        use axum::{Json, Router, http::StatusCode, http::header, routing::post};

        let captured = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let cap1 = captured.clone();
        let cap2 = captured.clone();

        let app = Router::new()
            .route(
                "/m/anth/v1/messages",
                post(move |Json(body): Json<serde_json::Value>| {
                    let cap = cap1.clone();
                    async move {
                        cap.lock().unwrap().push(body);
                        let sse = concat!(
                            "event: message_start\n",
                            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
                            "event: content_block_start\n",
                            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                            "event: content_block_delta\n",
                            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
                            "event: content_block_stop\n",
                            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                            "event: message_delta\n",
                            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
                            "event: message_stop\n",
                            "data: {\"type\":\"message_stop\"}\n\n"
                        );
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            sse,
                        )
                    }
                }),
            )
            .route(
                "/m/oa/chat/completions",
                post(move |Json(body): Json<serde_json::Value>| {
                    let cap = cap2.clone();
                    async move {
                        cap.lock().unwrap().push(body);
                        let sse = concat!(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
                            "data: [DONE]\n\n"
                        );
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            sse,
                        )
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let msgs = vec![crate::messages::NeutralMessage::user_text("hi")];
        let input = CompletionInput {
            model: "m",
            max_tokens: 4096,
            messages: &msgs,
            tools: &[],
        };
        let cancel = CancellationToken::new();
        let mut t = |_d: &str| Ok(());
        let mut r = |_d: &str| Ok(());

        let anth = EmbeddedCredentialSource::new(
            ModelProxyConfig {
                base_url: format!("http://{addr}/m/anth"),
                token: "tok".into(),
                protocol: "anthropic-messages".into(),
            },
            ProviderProtocol::Anthropic,
            None,
            Arc::new(Mutex::new(Some("medium".into()))),
        )
        .unwrap();
        anth.execute_once_with_retries(&input, &mut t, &mut r, &cancel)
            .await
            .unwrap();

        let oa = EmbeddedCredentialSource::new(
            ModelProxyConfig {
                base_url: format!("http://{addr}/m/oa"),
                token: "tok".into(),
                protocol: "openai-completions".into(),
            },
            ProviderProtocol::OpenAi,
            None,
            Arc::new(Mutex::new(Some("high".into()))),
        )
        .unwrap();
        oa.execute_once_with_retries(&input, &mut t, &mut r, &cancel)
            .await
            .unwrap();

        let bodies = captured.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["thinking"]["type"], "enabled");
        assert!(bodies[0]["thinking"]["budget_tokens"].as_u64().unwrap() < 4096);
        assert_eq!(bodies[1]["reasoning_effort"], "high");

        srv.abort();
    }

    #[tokio::test]
    async fn media_credential_source_and_client_cover_routes_and_zero_retry_rejections() {
        use crate::media::engine2::MultipartForm;
        use crate::media::{
            MediaAuthKind, MediaCredentialSource, MediaError, MediaErrorCode, MediaKind,
        };
        use crate::provider_transport::validate_embedded_proxy_target_v11;
        use axum::{
            Router,
            body::Bytes,
            extract::Path as AxumPath,
            http::{HeaderMap, StatusCode},
            routing::{get, post},
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        // 1.0 vs 1.1 Gemini target validation.
        assert!(
            validate_embedded_proxy_target_v11(
                "http://127.0.0.1:4100/m/r1",
                ProviderProtocol::Gemini,
                false
            )
            .is_err()
        );
        let gemini_target = validate_embedded_proxy_target_v11(
            "http://127.0.0.1:4100/m/r1",
            ProviderProtocol::Gemini,
            true,
        )
        .unwrap();
        assert_eq!(
            gemini_target
                .resolved_endpoint(Some("gemini-2.5-flash"))
                .unwrap(),
            "http://127.0.0.1:4100/m/r1/gemini/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );

        // classify_media_error zero-retry MODEL_PROXY_REJECTED + scrub.
        for status in [401u16, 403, 409] {
            let err = MediaError::new(MediaErrorCode::ProviderError, "bad proxy tok-secret")
                .with_status(status)
                .with_retryable(true);
            let classified = classify_media_error(err, &|s| s.replace("tok-secret", "[REDACTED]"));
            assert_eq!(classified.code, "MODEL_PROXY_REJECTED");
            assert!(!classified.retryable);
            assert!(!classified.message.contains("tok-secret"));
        }

        // MediaCredentialSource on EmbeddedCredentialSource.
        let cred_src = EmbeddedCredentialSource::new(
            ModelProxyConfig {
                base_url: "http://127.0.0.1:4100/m/r1".into(),
                token: "tok-proxy-1".into(),
                protocol: "openai-completions".into(),
            },
            ProviderProtocol::OpenAi,
            None,
            Arc::new(Mutex::new(None)),
        )
        .unwrap();
        let (ep, cred) = cred_src
            .resolve_media_endpoint(Some("route-1"), "openai", "gpt-image-1")
            .await
            .unwrap();
        assert!(ep.embedded_proxy);
        assert_eq!(ep.base_url, "http://127.0.0.1:4100/m/r1");
        assert_eq!(cred.kind, MediaAuthKind::ProxyToken);

        // EmbeddedMediaProxyClient exercises all 12 media routes + zero-retry 401/403/409.
        let auth_fail_hits = Arc::new(AtomicUsize::new(0));
        let auth_fail_hits_srv = auth_fail_hits.clone();
        let expected_auth = "Bearer secret-run-tok";

        let app = Router::new()
            .route(
                "/m/r1/embeddings",
                post(move |headers: HeaderMap| {
                    let hits = auth_fail_hits_srv.clone();
                    async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");
                        if auth != expected_auth {
                            hits.fetch_add(1, Ordering::SeqCst);
                            return (
                                StatusCode::UNAUTHORIZED,
                                axum::Json(serde_json::json!({
                                    "error": { "message": format!("rejected {auth}") }
                                })),
                            );
                        }
                        (
                            StatusCode::OK,
                            axum::Json(serde_json::json!({ "data": [{ "embedding": [0.1, 0.2] }] })),
                        )
                    }
                }),
            )
            .route(
                "/m/r1/rerank",
                post(|| async { axum::Json(serde_json::json!({ "results": [{ "index": 0 }] })) }),
            )
            .route(
                "/m/r1/images/generations",
                post(|| async { axum::Json(serde_json::json!({ "data": [{ "b64_json": "AA==" }] })) }),
            )
            .route(
                "/m/r1/images/edits",
                post(|headers: HeaderMap, body: Bytes| async move {
                    let ct = headers
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    assert!(ct.starts_with("multipart/form-data; boundary="));
                    assert!(!body.is_empty());
                    axum::Json(serde_json::json!({ "data": [{ "b64_json": "AQ==" }] }))
                }),
            )
            .route(
                "/m/r1/audio/speech",
                post(|| async {
                    (
                        StatusCode::OK,
                        [("content-type", "audio/mpeg")],
                        b"MP3BYTES".to_vec(),
                    )
                }),
            )
            .route(
                "/m/r1/audio/transcriptions",
                post(|| async { axum::Json(serde_json::json!({ "text": "hello world" })) }),
            )
            .route(
                "/m/r1/audio/translations",
                post(|| async { axum::Json(serde_json::json!({ "text": "translated" })) }),
            )
            .route(
                "/m/r1/gemini/models/gemini-2.5-flash:generateContent",
                post(|| async {
                    axum::Json(serde_json::json!({
                        "candidates": [{ "content": { "parts": [{ "text": "ok" }] } }]
                    }))
                }),
            )
            .route(
                "/m/r1/gemini/models/gemini-2.5-flash:streamGenerateContent",
                post(|| async {
                    (
                        StatusCode::OK,
                        [("content-type", "text/event-stream")],
                        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}\n\n",
                    )
                }),
            )
            .route(
                "/m/r1/gemini/operations/op-1",
                get(|| async { axum::Json(serde_json::json!({ "name": "operations/op-1", "done": true })) }),
            )
            .route(
                "/m/r1/media/tasks",
                post(|| async {
                    axum::Json(serde_json::json!({ "taskId": "mt-1", "status": "queued" }))
                }),
            )
            .route(
                "/m/r1/media/tasks/{id}",
                get(|AxumPath(id): AxumPath<String>| async move {
                    axum::Json(serde_json::json!({ "taskId": id, "status": "succeeded", "mimeType": "audio/mpeg" }))
                }),
            )
            .route(
                "/m/r1/media/tasks/{id}/content",
                get(|| async {
                    (
                        StatusCode::OK,
                        [("content-type", "audio/mpeg")],
                        b"TASK_AUDIO".to_vec(),
                    )
                }),
            )
            .route(
                "/m/r1/media/tasks/{id}/cancel",
                post(|| async { axum::Json(serde_json::json!({ "status": "cancelled" })) }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client =
            EmbeddedMediaProxyClient::new(format!("http://{addr}/m/r1"), "secret-run-tok").unwrap();
        let cancel = CancellationToken::new();

        assert_eq!(
            client
                .embeddings(
                    json!({"model": "text-embedding-3-small", "input": "hi"}),
                    &cancel
                )
                .await
                .unwrap()["data"][0]["embedding"][0],
            0.1
        );
        assert_eq!(
            client
                .rerank(json!({"query": "q", "documents": ["d"]}), &cancel)
                .await
                .unwrap()["results"][0]["index"],
            0
        );
        assert_eq!(
            client
                .images_generations(json!({"prompt": "cat"}), &cancel)
                .await
                .unwrap()["data"][0]["b64_json"],
            "AA=="
        );
        let img_form = MultipartForm::new().text("prompt", "edit").unwrap();
        assert_eq!(
            client
                .images_edits_multipart(img_form, &cancel)
                .await
                .unwrap()["data"][0]["b64_json"],
            "AQ=="
        );
        let speech = client
            .audio_speech(json!({"model": "tts-1", "input": "hi"}), &cancel)
            .await
            .unwrap();
        assert_eq!(speech.as_ref(), b"MP3BYTES");
        let stt_form = MultipartForm::new().text("model", "whisper-1").unwrap();
        assert_eq!(
            client
                .audio_transcriptions_multipart(stt_form, &cancel)
                .await
                .unwrap()["text"],
            "hello world"
        );
        let tr_form = MultipartForm::new().text("model", "whisper-1").unwrap();
        assert_eq!(
            client
                .audio_translations_multipart(tr_form, &cancel)
                .await
                .unwrap()["text"],
            "translated"
        );
        assert_eq!(
            client
                .gemini_post(
                    "models/gemini-2.5-flash:generateContent",
                    json!({"contents": []}),
                    &cancel
                )
                .await
                .unwrap()["candidates"][0]["content"]["parts"][0]["text"],
            "ok"
        );
        let sse = client
            .gemini_sse(
                "models/gemini-2.5-flash:streamGenerateContent?alt=sse",
                json!({"contents": []}),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(sse.len(), 1);
        assert!(sse[0].data.contains("hi"));
        assert_eq!(
            client
                .gemini_get_operation("operations/op-1", &cancel)
                .await
                .unwrap()["done"],
            true
        );

        let task_res = client
            .run_media_task(
                "embedded-media",
                MediaKind::Music,
                "music-1",
                json!({"prompt": "jazz"}),
                Some("k-1".into()),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(task_res.payloads[0].1.as_ref(), b"TASK_AUDIO");

        // Verify bad token causes 401 -> zero-retry MODEL_PROXY_REJECTED with token scrubbed.
        let bad_client =
            EmbeddedMediaProxyClient::new(format!("http://{addr}/m/r1"), "wrong-secret-tok")
                .unwrap();
        let err = bad_client
            .embeddings(json!({"input": "x"}), &cancel)
            .await
            .unwrap_err();
        assert_eq!(err.code, "MODEL_PROXY_REJECTED");
        assert!(!err.retryable);
        assert!(!err.message.contains("wrong-secret-tok"));
        assert_eq!(auth_fail_hits.load(Ordering::SeqCst), 1);

        srv.abort();
    }
}
