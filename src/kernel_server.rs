//! Embedded kernel HTTP/SSE server & stdio handshake (`codaKernel/1 §2–§9`).

use crate::kernel_embedded::event_hub::SequencedKernelEvent;
use crate::kernel_embedded::session_store::{
    CompactInput, EmbeddedSessionStore, HydrateInput, PatchInput, RunStartConfig,
    ToolResultSubmission,
};
use crate::kernel_embedded::{
    AppendableSessionEntry, CompactionConfig, ContextProjectionConfig, EVENT_REPLAY_PER_RUN,
    GateDecision, GateKind, HydrateModelConfig, KERNEL_PROTOCOL, KernelError, KernelToolDescriptor,
    KernelToolResult, MAX_BODY_BYTES, MAX_TOOLS_PER_SESSION, ModelProxyConfig, PatchModelConfig,
    RunModelConfig, SteeringMode, double_option,
};
use crate::messages::{KernelContent, SessionEntry};
use crate::redaction::scrub;
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, FromRequest, Path, RawQuery, Request, State,
        rejection::{BytesRejection, PathRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event as SseEvent, KeepAlive},
    },
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::future::IntoFuture;
use std::io::{BufRead, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct KernelServeConfig {
    pub listen: SocketAddr,
    pub data_dir: Option<PathBuf>,
    pub token_stdin: bool,
    pub token_env: Option<String>,
    pub embedded: bool,
    pub media_proxy: bool,
}

#[derive(Clone)]
pub struct KernelAppState {
    pub store: Arc<EmbeddedSessionStore>,
    pub token: String,
    pub expected_host: String,
    pub shutting_down: Arc<AtomicBool>,
    pub media_proxy: bool,
}

impl KernelAppState {
    pub fn new(token: String, expected_host: String) -> Self {
        Self::with_media_proxy(token, expected_host, false)
    }

    pub fn with_media_proxy(token: String, expected_host: String, media_proxy: bool) -> Self {
        Self {
            store: EmbeddedSessionStore::with_media_enabled(media_proxy),
            token,
            expected_host,
            shutting_down: Arc::new(AtomicBool::new(false)),
            media_proxy,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KernelErrorPayload {
    code: &'static str,
    message: String,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

struct KernelApiError {
    error: KernelError,
    scrub_token: String,
}

impl KernelApiError {
    fn from_kernel(error: KernelError, scrub_token: &str) -> Self {
        Self {
            error,
            scrub_token: scrub_token.to_owned(),
        }
    }
}

impl IntoResponse for KernelApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let msg = scrub(&self.error.message, &self.scrub_token);
        let body = json!({
            "error": KernelErrorPayload {
                code: self.error.code,
                message: msg,
                retryable: self.error.retryable,
                details: self.error.details,
            }
        });
        (status, Json(body)).into_response()
    }
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StrictValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value without duplicate object keys")
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| StrictValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("invalid JSON number"))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v.to_owned())))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictValue(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate JSON object key"));
                    }
                    let StrictValue(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

struct StrictObject<T>(T);

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for StrictObject<T> {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let StrictValue(value) = StrictValue::deserialize(deserializer)?;
        if !value.is_object() {
            return Err(serde::de::Error::custom("JSON object required"));
        }
        serde_json::from_value(value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

struct KernelJson<T>(T);

impl<T> FromRequest<KernelAppState> for KernelJson<T>
where
    T: serde::de::DeserializeOwned,
{
    type Rejection = KernelApiError;

    async fn from_request(
        request: Request,
        state: &KernelAppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let ct = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let is_json = ct
            .split(';')
            .next()
            .map(str::trim)
            .is_some_and(|mime| mime.eq_ignore_ascii_case("application/json"));
        if !is_json {
            return Err(KernelApiError::from_kernel(
                KernelError::invalid_request("Content-Type must be application/json"),
                &state.token,
            ));
        }

        let bytes = match Bytes::from_request(request, state).await {
            Ok(b) => b,
            Err(BytesRejection::FailedToBufferBody(
                axum::extract::rejection::FailedToBufferBody::LengthLimitError(_),
            )) => {
                return Err(KernelApiError::from_kernel(
                    KernelError::new(
                        413,
                        "BODY_TOO_LARGE",
                        "request body exceeds 64 MiB limit",
                        false,
                    ),
                    &state.token,
                ));
            }
            Err(_) => {
                return Err(KernelApiError::from_kernel(
                    KernelError::invalid_request("failed to read request body"),
                    &state.token,
                ));
            }
        };

        match serde_json::from_slice::<StrictObject<T>>(&bytes) {
            Ok(StrictObject(val)) => Ok(Self(val)),
            Err(err) => Err(KernelApiError::from_kernel(
                KernelError::invalid_request(format!("invalid JSON payload: {err}")),
                &state.token,
            )),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyObject {}

struct OptionalEmptyBody;

impl FromRequest<KernelAppState> for OptionalEmptyBody {
    type Rejection = KernelApiError;

    async fn from_request(
        request: Request,
        state: &KernelAppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let bytes = match Bytes::from_request(request, state).await {
            Ok(b) => b,
            Err(BytesRejection::FailedToBufferBody(
                axum::extract::rejection::FailedToBufferBody::LengthLimitError(_),
            )) => {
                return Err(KernelApiError::from_kernel(
                    KernelError::new(
                        413,
                        "BODY_TOO_LARGE",
                        "request body exceeds 64 MiB limit",
                        false,
                    ),
                    &state.token,
                ));
            }
            Err(_) => {
                return Err(KernelApiError::from_kernel(
                    KernelError::invalid_request("failed to read request body"),
                    &state.token,
                ));
            }
        };
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(Self);
        }
        serde_json::from_slice::<StrictObject<EmptyObject>>(&bytes).map_err(|err| {
            KernelApiError::from_kernel(
                KernelError::invalid_request(format!("invalid JSON payload: {err}")),
                &state.token,
            )
        })?;
        Ok(Self)
    }
}

struct KernelPath<T>(T);

impl<S, T> axum::extract::FromRequestParts<S> for KernelPath<T>
where
    Path<T>: axum::extract::FromRequestParts<S, Rejection = PathRejection>,
    S: Send + Sync,
{
    type Rejection = KernelApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(v)) => Ok(Self(v)),
            Err(_) => Err(KernelApiError::from_kernel(
                KernelError::invalid_request("invalid path parameter"),
                "",
            )),
        }
    }
}

fn constant_time_eq(provided: &[u8], expected: &[u8]) -> bool {
    let mut diff = provided.len() ^ expected.len();
    for (i, &b) in expected.iter().enumerate() {
        let a = provided.get(i).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

fn single_header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut iter = headers.get_all(name).iter();
    let first = iter.next()?;
    if iter.next().is_some() {
        return None;
    }
    first.to_str().ok()
}

async fn kernel_guard(
    State(state): State<KernelAppState>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();

    // 1. Strict loopback IP literal `Host` check (`403 FORBIDDEN`).
    if single_header_str(headers, header::HOST.as_str()) != Some(state.expected_host.as_str()) {
        return KernelApiError::from_kernel(
            KernelError::new(
                403,
                "FORBIDDEN",
                "Host header must match loopback IP literal",
                false,
            ),
            &state.token,
        )
        .into_response();
    }

    // 2. Reject ANY `Origin` header or `sec-fetch-site: cross-site` (`403 FORBIDDEN`).
    if headers.contains_key(header::ORIGIN) {
        return KernelApiError::from_kernel(
            KernelError::new(403, "FORBIDDEN", "Origin header is forbidden", false),
            &state.token,
        )
        .into_response();
    }
    if headers.get_all("sec-fetch-site").iter().any(|v| {
        v.to_str()
            .is_ok_and(|s| s.trim().eq_ignore_ascii_case("cross-site"))
    }) {
        return KernelApiError::from_kernel(
            KernelError::new(403, "FORBIDDEN", "cross-site requests are forbidden", false),
            &state.token,
        )
        .into_response();
    }

    // 3. Constant-time `x-coda-token` validation (`401 UNAUTHORIZED`).
    let token_ok = single_header_str(headers, "x-coda-token")
        .is_some_and(|provided| constant_time_eq(provided.as_bytes(), state.token.as_bytes()));
    if !token_ok {
        return KernelApiError::from_kernel(
            KernelError::new(
                401,
                "UNAUTHORIZED",
                "invalid or missing x-coda-token",
                false,
            ),
            &state.token,
        )
        .into_response();
    }

    // 4. Shutdown drain (`503 KERNEL_SHUTTING_DOWN`).
    if state.shutting_down.load(Ordering::Acquire) {
        let mut response = KernelApiError::from_kernel(
            KernelError::new(503, "KERNEL_SHUTTING_DOWN", "kernel is shutting down", true),
            &state.token,
        )
        .into_response();
        let resp_headers = response.headers_mut();
        resp_headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
        resp_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        resp_headers.insert(
            header::HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        );
        return response;
    }

    let mut response = next.run(request).await;
    let resp_headers = response.headers_mut();
    resp_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp_headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn kernel_not_found(State(state): State<KernelAppState>) -> Response {
    KernelApiError::from_kernel(
        KernelError::new(404, "NOT_FOUND", "unknown kernel endpoint", false),
        &state.token,
    )
    .into_response()
}

async fn kernel_method_not_allowed(State(state): State<KernelAppState>) -> Response {
    KernelApiError::from_kernel(
        KernelError::new(
            405,
            "METHOD_NOT_ALLOWED",
            "method not allowed for kernel endpoint",
            false,
        ),
        &state.token,
    )
    .into_response()
}

pub fn kernel_router(state: KernelAppState) -> Router {
    Router::new()
        .route("/api/kernel/capabilities", get(capabilities))
        .route(
            "/api/kernel/sessions/{sessionId}",
            put(hydrate_session)
                .get(get_session)
                .delete(delete_session)
                .patch(patch_session),
        )
        .route(
            "/api/kernel/sessions/{sessionId}/entries",
            post(append_entry),
        )
        .route(
            "/api/kernel/sessions/{sessionId}/branch",
            post(branch_session),
        )
        .route(
            "/api/kernel/sessions/{sessionId}/active-tools",
            put(set_active_tools),
        )
        .route(
            "/api/kernel/sessions/{sessionId}/compact",
            post(compact_session),
        )
        .route("/api/kernel/sessions/{sessionId}/runs", post(start_run))
        .route(
            "/api/kernel/sessions/{sessionId}/runs/{runId}/events",
            get(reconnect_run_events),
        )
        .route(
            "/api/kernel/sessions/{sessionId}/runs/{runId}/abort",
            post(abort_run),
        )
        .route(
            "/api/kernel/sessions/{sessionId}/runs/{runId}/finish",
            post(finish_run),
        )
        .route(
            "/api/kernel/sessions/{sessionId}/runs/{runId}/tool-results",
            post(submit_tool_result),
        )
        .route(
            "/api/kernel/sessions/{sessionId}/runs/{runId}/steer",
            post(steer_run),
        )
        .route(
            "/api/kernel/sessions/{sessionId}/runs/{runId}/gates/{gateId}",
            post(resolve_gate),
        )
        .fallback(kernel_not_found)
        .method_not_allowed_fallback(kernel_method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), kernel_guard))
        .with_state(state)
}

async fn capabilities(State(state): State<KernelAppState>) -> impl IntoResponse {
    let mut features = vec![
        "hydrate",
        "hostTools",
        "steer",
        "activeTools",
        "gates",
        "compact",
        "reconnect",
        "finish",
    ];
    if state.media_proxy {
        features.push("media");
    }
    let mut payload = serde_json::Map::new();
    payload.insert("protocol".to_owned(), json!(KERNEL_PROTOCOL));
    payload.insert("version".to_owned(), json!(env!("CARGO_PKG_VERSION")));
    payload.insert("features".to_owned(), json!(features));
    payload.insert(
        "gates".to_owned(),
        json!(["beforeModelTurn", "afterTurn", "beforeCompact"]),
    );
    payload.insert(
        "modelProtocols".to_owned(),
        json!([
            "openai-completions",
            "openai-responses",
            "anthropic-messages"
        ]),
    );
    if state.media_proxy {
        payload.insert(
            "mediaProtocols".to_owned(),
            json!([
                "openai-embeddings",
                "gemini-embeddings",
                "jina-rerank",
                "openai-images",
                "gemini-image",
                "volcengine-images",
                "openai-speech",
                "gemini-tts",
                "elevenlabs-tts",
                "azure-speech",
                "minimax-tts",
                "volcengine-tts",
                "dashscope-tts",
                "openai-stt",
                "elevenlabs-stt",
                "gemini-audio",
                "dashscope-stt",
                "volcengine-stt",
                "minimax-music",
                "elevenlabs-music",
                "lyria-music",
                "suno-music",
                "openai-videos",
                "veo-videos",
                "seedance-videos",
                "hailuo-videos",
                "kling-videos",
                "wanx-videos",
                "runway-videos",
                "openai-realtime",
                "gemini-live"
            ]),
        );
        payload.insert(
            "mediaRoutes".to_owned(),
            json!([
                "embeddings",
                "rerank",
                "images/generations",
                "images/edits",
                "audio/speech",
                "audio/transcriptions",
                "audio/translations",
                "media/tasks",
                "media/tasks/status",
                "media/tasks/content",
                "media/tasks/cancel",
                "gemini"
            ]),
        );
    }
    payload.insert(
        "events".to_owned(),
        json!([
            "start",
            "text",
            "thinking",
            "toolCall",
            "toolResult",
            "compaction",
            "retry",
            "retryCleared",
            "usage",
            "contextUsage",
            "userMessage",
            "error",
            "done",
            "entry",
            "gate",
            "status",
            "toolsChanged"
        ]),
    );
    payload.insert(
        "limits".to_owned(),
        json!({
            "maxBodyBytes": MAX_BODY_BYTES,
            "maxToolsPerSession": MAX_TOOLS_PER_SESSION,
            "eventReplayPerRun": EVENT_REPLAY_PER_RUN
        }),
    );
    Json(Value::Object(payload))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HydrateHistoryReq {
    leaf_id: Option<String>,
    entries: Vec<SessionEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HydrateSessionReq {
    cwd: String,
    system_prompt: String,
    model: HydrateModelConfig,
    tools: Vec<KernelToolDescriptor>,
    active_tool_names: Vec<String>,
    #[serde(default, deserialize_with = "double_option::deserialize")]
    tool_result_max_chars: Option<Option<usize>>,
    #[serde(default)]
    compaction: CompactionConfig,
    #[serde(default)]
    steering_mode: SteeringMode,
    #[serde(default)]
    gates: Vec<GateKind>,
    #[serde(default)]
    context_projection: Option<ContextProjectionConfig>,
    history: HydrateHistoryReq,
}

async fn hydrate_session(
    State(state): State<KernelAppState>,
    KernelPath(session_id): KernelPath<String>,
    KernelJson(req): KernelJson<HydrateSessionReq>,
) -> std::result::Result<impl IntoResponse, KernelApiError> {
    let (res, created) = state
        .store
        .hydrate_session(
            session_id,
            HydrateInput {
                cwd: req.cwd,
                system_prompt: req.system_prompt,
                model: req.model,
                tools: req.tools,
                active_tool_names: req.active_tool_names,
                tool_result_max_chars: req.tool_result_max_chars,
                compaction: req.compaction,
                steering_mode: req.steering_mode,
                gates: req.gates,
                context_projection: req.context_projection,
                leaf_id: req.history.leaf_id,
                entries: req.history.entries,
            },
        )
        .await
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(res)))
}

async fn get_session(
    State(state): State<KernelAppState>,
    KernelPath(session_id): KernelPath<String>,
) -> std::result::Result<impl IntoResponse, KernelApiError> {
    let summary = state
        .store
        .get_session(&session_id)
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(Json(summary))
}

async fn delete_session(
    State(state): State<KernelAppState>,
    KernelPath(session_id): KernelPath<String>,
) -> std::result::Result<StatusCode, KernelApiError> {
    state
        .store
        .delete_session(&session_id)
        .await
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PatchSessionReq {
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    model: Option<PatchModelConfig>,
    #[serde(default)]
    tools: Option<Vec<KernelToolDescriptor>>,
    #[serde(default)]
    active_tool_names: Option<Vec<String>>,
    #[serde(default)]
    compaction: Option<CompactionConfig>,
    #[serde(default)]
    gates: Option<Vec<GateKind>>,
    #[serde(default, deserialize_with = "double_option::deserialize")]
    tool_result_max_chars: Option<Option<usize>>,
    #[serde(default, deserialize_with = "double_option::deserialize")]
    runtime_context: Option<Option<Vec<KernelContent>>>,
    #[serde(default, deserialize_with = "double_option::deserialize")]
    prune_boundary_call_id: Option<Option<String>>,
}

async fn patch_session(
    State(state): State<KernelAppState>,
    KernelPath(session_id): KernelPath<String>,
    KernelJson(req): KernelJson<PatchSessionReq>,
) -> std::result::Result<impl IntoResponse, KernelApiError> {
    let summary = state
        .store
        .patch_session(
            &session_id,
            PatchInput {
                system_prompt: req.system_prompt,
                model: req.model,
                tools: req.tools,
                active_tool_names: req.active_tool_names,
                compaction: req.compaction,
                gates: req.gates,
                tool_result_max_chars: req.tool_result_max_chars,
                runtime_context: req.runtime_context,
                prune_boundary_call_id: req.prune_boundary_call_id,
            },
        )
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(Json(summary))
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum DeliverAs {
    Append,
    Steer,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AppendEntryReq {
    #[serde(default)]
    base_revision: Option<u64>,
    entry: AppendableSessionEntry,
    #[serde(default)]
    deliver_as: Option<DeliverAs>,
}

async fn append_entry(
    State(state): State<KernelAppState>,
    KernelPath(session_id): KernelPath<String>,
    KernelJson(req): KernelJson<AppendEntryReq>,
) -> std::result::Result<Response, KernelApiError> {
    let deliver_as_steer = req.deliver_as == Some(DeliverAs::Steer);
    match state
        .store
        .append_entry(&session_id, req.base_revision, req.entry, deliver_as_steer)
        .await
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?
    {
        Some(res) => Ok((StatusCode::OK, Json(res)).into_response()),
        None => Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))).into_response()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BranchReq {
    base_revision: u64,
    leaf_id: Option<String>,
}

async fn branch_session(
    State(state): State<KernelAppState>,
    KernelPath(session_id): KernelPath<String>,
    KernelJson(req): KernelJson<BranchReq>,
) -> std::result::Result<impl IntoResponse, KernelApiError> {
    let res = state
        .store
        .branch_session(&session_id, req.base_revision, req.leaf_id)
        .await
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(Json(res))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActiveToolsReq {
    active_tool_names: Vec<String>,
}

async fn set_active_tools(
    State(state): State<KernelAppState>,
    KernelPath(session_id): KernelPath<String>,
    KernelJson(req): KernelJson<ActiveToolsReq>,
) -> std::result::Result<impl IntoResponse, KernelApiError> {
    let res = state
        .store
        .set_active_tools(&session_id, req.active_tool_names)
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(Json(res))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompactReq {
    base_revision: u64,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    keep_recent_tokens: Option<u64>,
    #[serde(default)]
    model: Option<RunModelConfig>,
    #[serde(default)]
    thinking_level: Option<String>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    model_proxy: ModelProxyConfig,
}

async fn compact_session(
    State(state): State<KernelAppState>,
    KernelPath(session_id): KernelPath<String>,
    KernelJson(req): KernelJson<CompactReq>,
) -> std::result::Result<impl IntoResponse, KernelApiError> {
    let res = state
        .store
        .compact_session(
            &session_id,
            CompactInput {
                base_revision: req.base_revision,
                system_prompt: req.system_prompt,
                instructions: req.instructions,
                keep_recent_tokens: req.keep_recent_tokens,
                model: req.model,
                thinking_level: req.thinking_level,
                context_window: req.context_window,
                max_output_tokens: req.max_output_tokens,
                model_proxy: req.model_proxy,
            },
        )
        .await
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(Json(res))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartRunReq {
    run_id: String,
    base_revision: u64,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    model: Option<RunModelConfig>,
    #[serde(default)]
    append_entries: Option<Vec<AppendableSessionEntry>>,
    content: Vec<KernelContent>,
    #[serde(default)]
    thinking_level: Option<String>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    leaf_id: Option<String>,
    #[serde(default)]
    max_requests: Option<usize>,
    #[serde(default)]
    runtime_context: Option<Vec<KernelContent>>,
    #[serde(default, deserialize_with = "double_option::deserialize")]
    prune_boundary_call_id: Option<Option<String>>,
    model_proxy: ModelProxyConfig,
}

fn build_sse_response(
    replay: Vec<SequencedKernelEvent>,
    mut rx: broadcast::Receiver<SequencedKernelEvent>,
    already_done: bool,
) -> Sse<impl futures_util::Stream<Item = std::result::Result<SseEvent, Infallible>>> {
    let stream = async_stream::stream! {
        let mut last_seq = 0u64;
        let mut finished = false;
        for ev in replay {
            last_seq = ev.seq;
            let is_done = ev.event.get("type").and_then(Value::as_str) == Some("done");
            let event_name = ev
                .event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("message")
                .to_owned();
            let payload = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
            yield Ok(SseEvent::default()
                .id(ev.seq.to_string())
                .event(event_name)
                .data(payload));
            if is_done {
                finished = true;
                break;
            }
        }
        if !finished && !already_done {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if ev.seq <= last_seq {
                            continue;
                        }
                        last_seq = ev.seq;
                        let is_done = ev.event.get("type").and_then(Value::as_str) == Some("done");
                        let event_name = ev
                            .event
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("message")
                            .to_owned();
                        let payload = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
                        yield Ok(SseEvent::default()
                            .id(ev.seq.to_string())
                            .event(event_name)
                            .data(payload));
                        if is_done {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    };
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text("ping"),
    )
}

async fn start_run(
    State(state): State<KernelAppState>,
    KernelPath(session_id): KernelPath<String>,
    KernelJson(req): KernelJson<StartRunReq>,
) -> std::result::Result<Response, KernelApiError> {
    let (_run, replay, rx, already_done) = state
        .store
        .start_run(
            &session_id,
            RunStartConfig {
                run_id: req.run_id,
                base_revision: req.base_revision,
                system_prompt: req.system_prompt,
                model: req.model,
                append_entries: req.append_entries.unwrap_or_default(),
                content: req.content,
                thinking_level: req.thinking_level,
                context_window: req.context_window,
                max_output_tokens: req.max_output_tokens,
                leaf_id: req.leaf_id,
                max_requests: req.max_requests.unwrap_or(64),
                runtime_context: req.runtime_context,
                prune_boundary_call_id: req.prune_boundary_call_id,
                model_proxy: req.model_proxy,
            },
        )
        .await
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(build_sse_response(replay, rx, already_done).into_response())
}

fn parse_after_cursor(raw_query: Option<&str>, headers: &HeaderMap) -> Result<u64, KernelError> {
    let mut query_after = None;
    if let Some(q) = raw_query {
        for pair in q.split('&').filter(|s| !s.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if k == "after" {
                let parsed = v.parse::<u64>().map_err(|_| {
                    KernelError::invalid_request(
                        "query parameter 'after' must be a non-negative integer",
                    )
                })?;
                query_after = Some(parsed);
            } else {
                return Err(KernelError::invalid_request(format!(
                    "unknown query parameter: {k}"
                )));
            }
        }
    }
    if let Some(after) = query_after {
        return Ok(after);
    }
    if let Some(last_id) = headers.get("last-event-id").and_then(|v| v.to_str().ok()) {
        return last_id.trim().parse::<u64>().map_err(|_| {
            KernelError::invalid_request("Last-Event-ID must be a non-negative integer")
        });
    }
    Ok(0)
}

async fn reconnect_run_events(
    State(state): State<KernelAppState>,
    KernelPath((session_id, run_id)): KernelPath<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> std::result::Result<Response, KernelApiError> {
    let after = parse_after_cursor(query.as_deref(), &headers)
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    let (replay, rx, already_done) = state
        .store
        .subscribe_run_events(&session_id, &run_id, after)
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(build_sse_response(replay, rx, already_done).into_response())
}

async fn abort_run(
    State(state): State<KernelAppState>,
    KernelPath((session_id, run_id)): KernelPath<(String, String)>,
    _: OptionalEmptyBody,
) -> std::result::Result<StatusCode, KernelApiError> {
    state
        .store
        .abort_run(&session_id, &run_id)
        .await
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn finish_run(
    State(state): State<KernelAppState>,
    KernelPath((session_id, run_id)): KernelPath<(String, String)>,
    _: OptionalEmptyBody,
) -> std::result::Result<StatusCode, KernelApiError> {
    state
        .store
        .finish_run(&session_id, &run_id)
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolResultPostReq {
    call_id: String,
    result: KernelToolResult,
    #[serde(default)]
    model_note: Option<String>,
}

async fn submit_tool_result(
    State(state): State<KernelAppState>,
    KernelPath((session_id, run_id)): KernelPath<(String, String)>,
    KernelJson(req): KernelJson<ToolResultPostReq>,
) -> std::result::Result<Response, KernelApiError> {
    match state
        .store
        .submit_tool_result(
            &session_id,
            &run_id,
            req.call_id,
            req.result,
            req.model_note,
        )
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?
    {
        ToolResultSubmission::Accepted => {
            Ok((StatusCode::ACCEPTED, Json(json!({ "accepted": true }))).into_response())
        }
        ToolResultSubmission::Replayed => {
            Ok((StatusCode::OK, Json(json!({ "replayed": true }))).into_response())
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SteerReq {
    content: Vec<KernelContent>,
}

async fn steer_run(
    State(state): State<KernelAppState>,
    KernelPath((session_id, run_id)): KernelPath<(String, String)>,
    KernelJson(req): KernelJson<SteerReq>,
) -> std::result::Result<impl IntoResponse, KernelApiError> {
    state
        .store
        .submit_steer(&session_id, &run_id, req.content)
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))))
}

async fn resolve_gate(
    State(state): State<KernelAppState>,
    KernelPath((session_id, run_id, gate_id)): KernelPath<(String, String, String)>,
    KernelJson(decision): KernelJson<GateDecision>,
) -> std::result::Result<impl IntoResponse, KernelApiError> {
    state
        .store
        .resolve_gate(&session_id, &run_id, &gate_id, decision)
        .map_err(|e| KernelApiError::from_kernel(e, &state.token))?;
    Ok((StatusCode::OK, Json(json!({ "accepted": true }))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandshakeTokenLine {
    token: String,
}

fn spawn_stdin_handshake_thread(
    eof_cancel: CancellationToken,
) -> std::sync::mpsc::Receiver<Result<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("kernel-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            let mut buf = Vec::with_capacity(256);
            let first = read_bounded_line(&mut lock, &mut buf, 8192);
            let token_res = first.and_then(|bytes| {
                ensure!(!bytes.is_empty(), "stdin closed before token handshake");
                let text = std::str::from_utf8(bytes).context("handshake line must be UTF-8")?;
                let parsed: HandshakeTokenLine =
                    serde_json::from_str(text.trim_end_matches(['\r', '\n']))
                        .context("invalid stdin token handshake JSON")?;
                ensure!(!parsed.token.is_empty(), "handshake token cannot be empty");
                ensure!(
                    HeaderValue::from_str(&parsed.token).is_ok(),
                    "handshake token contains invalid header characters"
                );
                Ok(parsed.token)
            });
            let ok = token_res.is_ok();
            let _ = tx.send(token_res);
            if !ok {
                eof_cancel.cancel();
                return;
            }

            // Keep stdin open; wait for EOF to trigger graceful exit (`protocol.md §2.2`).
            let mut sink = [0u8; 1024];
            loop {
                match std::io::Read::read(&mut lock, &mut sink) {
                    Ok(0) | Err(_) => {
                        eof_cancel.cancel();
                        break;
                    }
                    Ok(_) => {}
                }
            }
        });
    rx
}

fn read_bounded_line<'a>(
    reader: &mut impl BufRead,
    buf: &'a mut Vec<u8>,
    max_bytes: usize,
) -> Result<&'a [u8]> {
    buf.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(buf.as_slice());
        }
        if let Some(pos) = available.iter().position(|b| *b == b'\n') {
            ensure!(
                buf.len() + pos < max_bytes,
                "stdin handshake line exceeds limit"
            );
            buf.extend_from_slice(&available[..=pos]);
            reader.consume(pos + 1);
            return Ok(buf.as_slice());
        }
        ensure!(
            buf.len() + available.len() <= max_bytes,
            "stdin handshake line exceeds limit"
        );
        let len = available.len();
        buf.extend_from_slice(available);
        reader.consume(len);
    }
}

fn read_token_from_env_with(
    var_name: &str,
    lookup: impl FnOnce(&str) -> std::result::Result<String, std::env::VarError>,
) -> Result<String> {
    let trimmed_name = var_name.trim();
    ensure!(
        !trimmed_name.is_empty(),
        "--token-env variable name cannot be empty"
    );
    let raw = lookup(trimmed_name)
        .with_context(|| format!("missing token environment variable: {trimmed_name}"))?;
    let token = raw.trim_end_matches(['\r', '\n']).to_owned();
    ensure!(
        !token.trim().is_empty(),
        "token environment variable {trimmed_name} cannot be empty or whitespace-only"
    );
    ensure!(
        HeaderValue::from_str(&token).is_ok(),
        "token environment variable {trimmed_name} contains invalid header characters"
    );
    Ok(token)
}

pub async fn run_embedded(config: KernelServeConfig) -> Result<()> {
    run_embedded_inner(config, |var_name| std::env::var(var_name), None, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

async fn run_embedded_inner(
    config: KernelServeConfig,
    env_lookup: impl FnOnce(&str) -> std::result::Result<String, std::env::VarError>,
    ready_tx: Option<tokio::sync::oneshot::Sender<(u16, String)>>,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    run_embedded_with_eof(
        config,
        env_lookup,
        ready_tx,
        CancellationToken::new(),
        shutdown,
    )
    .await
}

async fn run_embedded_with_eof(
    config: KernelServeConfig,
    env_lookup: impl FnOnce(&str) -> std::result::Result<String, std::env::VarError>,
    ready_tx: Option<tokio::sync::oneshot::Sender<(u16, String)>>,
    eof_cancel: CancellationToken,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    ensure!(config.embedded, "only --embedded mode is supported");
    ensure!(
        config.token_stdin ^ config.token_env.is_some(),
        "exactly one of --token-stdin or --token-env is required in embedded mode"
    );
    ensure!(
        config.listen.ip().is_loopback(),
        "--listen must be a loopback IP literal (127.0.0.1 or ::1)"
    );
    if let Some(ref data_dir) = config.data_dir {
        std::fs::create_dir_all(data_dir).context("failed to create ephemeral --data-dir")?;
    }

    let token = if let Some(ref var_name) = config.token_env {
        read_token_from_env_with(var_name, env_lookup)?
    } else {
        let token_rx = spawn_stdin_handshake_thread(eof_cancel.clone());
        tokio::task::spawn_blocking(move || {
            token_rx
                .recv()
                .context("stdin handshake thread terminated unexpectedly")?
        })
        .await
        .context("stdin handshake task failed")??
    };

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("failed to bind {}", config.listen))?;
    let local_addr = listener.local_addr()?;
    let expected_host = match local_addr.ip() {
        IpAddr::V4(ip) => format!("{ip}:{}", local_addr.port()),
        IpAddr::V6(ip) => format!("[{ip}]:{}", local_addr.port()),
    };

    let state = KernelAppState::with_media_proxy(token, expected_host, config.media_proxy);
    let app = kernel_router(state.clone());

    let ready_line = serde_json::to_string(&json!({
        "type": "ready",
        "protocol": KERNEL_PROTOCOL,
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "port": local_addr.port(),
    }))?;
    {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        writeln!(out, "{ready_line}")?;
        out.flush()?;
    }
    if let Some(tx) = ready_tx {
        let _ = tx.send((local_addr.port(), ready_line));
    }

    // 30-min idle session reaper (`protocol.md §4.2`).
    let reaper_store = state.store.clone();
    let reaper_cancel = eof_cancel.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = reaper_cancel.cancelled() => break,
                _ = interval.tick() => {
                    reaper_store.reap_idle_sessions();
                }
            }
        }
    });

    let http_stop = CancellationToken::new();
    let serving = axum::serve(listener, app)
        .with_graceful_shutdown(http_stop.clone().cancelled_owned())
        .into_future();
    tokio::pin!(serving);
    tokio::pin!(shutdown);

    tokio::select! {
        res = &mut serving => {
            eof_cancel.cancel();
            res.context("embedded kernel HTTP server exited with error")?;
            return Ok(());
        }
        _ = eof_cancel.cancelled() => {}
        _ = &mut shutdown => {
            eof_cancel.cancel();
        }
    }

    // `503 KERNEL_SHUTTING_DOWN` drain window + abort all active runs (`<= 5s` total).
    // Keep polling `&mut serving` concurrently so pending/new HTTP requests during the
    // drain window are accepted and answered with `503 KERNEL_SHUTTING_DOWN` instead of
    // hitting `ECONNRESET` when `http_stop` closes the listener.
    state.shutting_down.store(true, Ordering::Release);
    let drain = async {
        state.store.abort_all_runs().await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    tokio::pin!(drain);

    tokio::select! {
        res = &mut serving => {
            res.context("embedded kernel HTTP server exited with error")?;
            return Ok(());
        }
        _ = &mut drain => {}
    }

    http_stop.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), &mut serving).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn test_state() -> KernelAppState {
        KernelAppState::new("secret-token-123".into(), "127.0.0.1:53817".into())
    }

    #[tokio::test]
    async fn security_guard_enforces_host_origin_token_body_limit_strict_json_and_503_drain() {
        let state = test_state();
        let app = kernel_router(state.clone());

        // 1. Missing/wrong token -> 401 UNAUTHORIZED
        let req = Request::builder()
            .method("GET")
            .uri("/api/kernel/capabilities")
            .header(header::HOST, "127.0.0.1:53817")
            .header("x-coda-token", "wrong-token")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(v["error"]["code"], "UNAUTHORIZED");

        // 2. Host: localhost:<port> -> 403 FORBIDDEN
        let req = Request::builder()
            .method("GET")
            .uri("/api/kernel/capabilities")
            .header(header::HOST, "localhost:53817")
            .header("x-coda-token", "secret-token-123")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // 3. Any Origin header (even matching loopback) -> 403 FORBIDDEN
        let req = Request::builder()
            .method("GET")
            .uri("/api/kernel/capabilities")
            .header(header::HOST, "127.0.0.1:53817")
            .header(header::ORIGIN, "http://127.0.0.1:53817")
            .header("x-coda-token", "secret-token-123")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // 4. sec-fetch-site: cross-site -> 403 FORBIDDEN
        let req = Request::builder()
            .method("GET")
            .uri("/api/kernel/capabilities")
            .header(header::HOST, "127.0.0.1:53817")
            .header("sec-fetch-site", "cross-site")
            .header("x-coda-token", "secret-token-123")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // 5. Strict JSON unknown field / duplicate key -> 400 INVALID_REQUEST
        let req = Request::builder()
            .method("PUT")
            .uri("/api/kernel/sessions/s1/active-tools")
            .header(header::HOST, "127.0.0.1:53817")
            .header("x-coda-token", "secret-token-123")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"activeToolNames":[],"extra":1}"#))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(v["error"]["code"], "INVALID_REQUEST");

        let req_dup = Request::builder()
            .method("PUT")
            .uri("/api/kernel/sessions/s1/active-tools")
            .header(header::HOST, "127.0.0.1:53817")
            .header("x-coda-token", "secret-token-123")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"activeToolNames":[],"activeToolNames":[]}"#))
            .unwrap();
        let resp_dup = app.clone().oneshot(req_dup).await.unwrap();
        assert_eq!(resp_dup.status(), StatusCode::BAD_REQUEST);

        // 6. Shutdown drain -> 503 KERNEL_SHUTTING_DOWN
        state.shutting_down.store(true, Ordering::Release);
        let req = Request::builder()
            .method("GET")
            .uri("/api/kernel/capabilities")
            .header(header::HOST, "127.0.0.1:53817")
            .header("x-coda-token", "secret-token-123")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(v["error"]["code"], "KERNEL_SHUTTING_DOWN");
        assert_eq!(v["error"]["retryable"], true);
    }

    #[tokio::test]
    async fn full_embedded_session_run_tool_batch_steer_gate_t3_k03_abort_and_compact() {
        use axum::routing::post as axum_post;
        use std::sync::atomic::AtomicUsize;

        // Spawn a mock modelProxy HTTP+SSE server on loopback.
        let call_count = Arc::new(AtomicUsize::new(0));
        let last_bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let cc = call_count.clone();
        let lb = last_bodies.clone();

        let mock_proxy = Router::new().route(
            "/m/run-1/responses",
            axum_post(move |Json(body): Json<Value>| {
                let cc = cc.clone();
                let lb = lb.clone();
                async move {
                    lb.lock().unwrap().push(body);
                    let n = cc.fetch_add(1, Ordering::SeqCst);
                    let sse = if n == 0 {
                        // Round 1: return a tool call + an inactive tool call
                        concat!(
                            "event: response.output_item.added\n",
                            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_host_1\",\"name\":\"read\",\"arguments\":\"\"}}\n\n",
                            "event: response.function_call_arguments.delta\n",
                            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"path\\\":\\\"a.ts\\\"}\"}\n\n",
                            "event: response.output_item.done\n",
                            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_host_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.ts\\\"}\"}}\n\n",
                            "event: response.completed\n",
                            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15}}}\n\n",
                            "data: [DONE]\n\n"
                        )
                    } else {
                        // Round 2: return text
                        concat!(
                            "event: response.output_text.delta\n",
                            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done!\"}\n\n",
                            "event: response.completed\n",
                            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_2\",\"usage\":{\"input_tokens\":20,\"output_tokens\":4,\"total_tokens\":24}}}\n\n",
                            "data: [DONE]\n\n"
                        )
                    };
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        sse,
                    )
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let _ = axum::serve(listener, mock_proxy).await;
        });

        let state = test_state();
        let app = kernel_router(state.clone());

        let req_with = |method: &str, uri: &str, body: Option<Value>| {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "127.0.0.1:53817")
                .header("x-coda-token", "secret-token-123");
            if body.is_some() {
                b = b.header(header::CONTENT_TYPE, "application/json");
            }
            let body = match body {
                Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
                None => Body::empty(),
            };
            b.body(body).unwrap()
        };

        // 1. Hydrate session with beforeModelTurn gate enabled.
        let hydrate_body = json!({
            "cwd": ".",
            "systemPrompt": "initial system",
            "model": {
                "modelId": "gpt-5.2",
                "protocol": "openai-responses",
                "contextWindow": 400000,
                "maxOutputTokens": 16384,
                "thinkingLevel": "medium"
            },
            "tools": [
                { "name": "read", "description": "read", "parameters": { "type": "object" }, "executionMode": "parallel" },
                { "name": "write", "description": "write", "parameters": { "type": "object" }, "executionMode": "sequential" }
            ],
            "activeToolNames": ["read"],
            "toolResultMaxChars": null,
            "compaction": { "enabled": true, "keepRecentTokens": 20000 },
            "steeringMode": "one-at-a-time",
            "gates": ["beforeModelTurn"],
            "history": { "leafId": null, "entries": [] }
        });
        let resp = app
            .clone()
            .oneshot(req_with(
                "PUT",
                "/api/kernel/sessions/s-e2e",
                Some(hydrate_body),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // 2. Start Run.
        let run_body = json!({
            "runId": "run-1",
            "baseRevision": 1,
            "content": [{ "type": "text", "text": "hello" }],
            "modelProxy": {
                "baseUrl": format!("http://{proxy_addr}/m/run-1"),
                "token": "proxy-tok",
                "protocol": "openai-responses"
            }
        });
        let run_resp = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s-e2e/runs",
                Some(run_body),
            ))
            .await
            .unwrap();
        assert_eq!(run_resp.status(), StatusCode::OK);

        // Poll replay events until beforeModelTurn gate appears.
        let gate_id_1 = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, _) = state
                .store
                .subscribe_run_events("s-e2e", "run-1", 0)
                .unwrap();
            if let Some(ev) = replay.iter().find(|e| e.event["type"] == "gate") {
                break ev.event["gateId"].as_str().unwrap().to_owned();
            }
        };

        // T3-K03 check: while beforeModelTurn gate is held, send PUT /active-tools & PATCH.
        let resp = app
            .clone()
            .oneshot(req_with(
                "PUT",
                "/api/kernel/sessions/s-e2e/active-tools",
                Some(json!({ "activeToolNames": ["read", "write"] })),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(req_with(
                "PATCH",
                "/api/kernel/sessions/s-e2e",
                Some(json!({ "systemPrompt": "updated system" })),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Resolve gate 1 -> toolsChanged MUST appear AFTER gate resolution and BEFORE start/model request.
        let resp = app
            .clone()
            .oneshot(req_with(
                "POST",
                &format!("/api/kernel/sessions/s-e2e/runs/run-1/gates/{gate_id_1}"),
                Some(json!({ "action": "continue" })),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Wait until toolCall arrives.
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, _) = state
                .store
                .subscribe_run_events("s-e2e", "run-1", 0)
                .unwrap();
            if replay.iter().any(|e| e.event["type"] == "toolCall") {
                break;
            }
        }

        // Verify T3-K03 ordering: gate -> toolsChanged -> start -> toolCall -> usage -> entry(assistant).
        {
            let (replay, _, _) = state
                .store
                .subscribe_run_events("s-e2e", "run-1", 0)
                .unwrap();
            let gate_seq = replay
                .iter()
                .find(|e| e.event["type"] == "gate")
                .unwrap()
                .seq;
            let tc_changed_seq = replay
                .iter()
                .find(|e| e.event["type"] == "toolsChanged")
                .unwrap()
                .seq;
            let start_seq = replay
                .iter()
                .find(|e| e.event["type"] == "start")
                .unwrap()
                .seq;
            assert!(gate_seq < tc_changed_seq && tc_changed_seq < start_seq);
        }

        // Submit tool result, test idempotent replay and conflict.
        let tr_payload = json!({
            "callId": "call_host_1",
            "result": {
                "content": [{ "type": "text", "text": "file body" }],
                "details": { "bytes": 9 },
                "isError": false
            },
            "modelNote": "note-for-model-only"
        });
        let resp = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s-e2e/runs/run-1/tool-results",
                Some(tr_payload.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let resp_replay = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s-e2e/runs/run-1/tool-results",
                Some(tr_payload),
            ))
            .await
            .unwrap();
        assert_eq!(resp_replay.status(), StatusCode::OK);

        let resp_conflict = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s-e2e/runs/run-1/tool-results",
                Some(json!({
                    "callId": "call_host_1",
                    "result": { "content": [{ "type": "text", "text": "different" }], "details": null }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp_conflict.status(), StatusCode::CONFLICT);

        // Wait for round 2 gate, resolve it, and wait for `done`.
        let gate_id_2 = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, _) = state
                .store
                .subscribe_run_events("s-e2e", "run-1", 0)
                .unwrap();
            let gates: Vec<_> = replay
                .iter()
                .filter(|e| e.event["type"] == "gate")
                .collect();
            if gates.len() >= 2 {
                break gates[1].event["gateId"].as_str().unwrap().to_owned();
            }
        };
        let resp = app
            .clone()
            .oneshot(req_with(
                "POST",
                &format!("/api/kernel/sessions/s-e2e/runs/run-1/gates/{gate_id_2}"),
                Some(json!({ "action": "continue" })),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let replay = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, done) = state
                .store
                .subscribe_run_events("s-e2e", "run-1", 0)
                .unwrap();
            if done {
                break replay;
            }
        };
        assert!(
            replay
                .iter()
                .any(|e| e.event["type"] == "done" && e.event["reason"] == "completed")
        );

        // Verify modelNote is NOT in entry(toolResult) or toolResult SSE event,
        // and IS present in the second upstream model request!
        let tr_ev = replay
            .iter()
            .find(|e| e.event["type"] == "toolResult")
            .unwrap();
        let tr_text = tr_ev.event["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(tr_text, "file body");
        let bodies = last_bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["instructions"], "updated system");
        assert!(bodies[0]["tools"].as_array().unwrap().len() == 2);
        let round2_str = bodies[1].to_string();
        assert!(round2_str.contains("note-for-model-only"));

        proxy_task.abort();
    }

    #[tokio::test]
    async fn steer_abort_finish_inactive_malformed_tools_reconnect_and_compact_endpoints() {
        use axum::routing::post as axum_post;
        use std::sync::atomic::AtomicUsize;

        let mock_proxy = Router::new()
            .route(
                "/m/run-steer/responses",
                axum_post({
                    let n = Arc::new(AtomicUsize::new(0));
                    move |Json(_body): Json<Value>| {
                        let n = n.clone();
                        async move {
                            let idx = n.fetch_add(1, Ordering::SeqCst);
                            let sse = if idx == 0 {
                                // Emit 1 valid active tool call, 1 inactive tool call, 1 non-object arg tool call
                                concat!(
                                    "event: response.output_item.done\n",
                                    "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc0\",\"call_id\":\"c_ok\",\"name\":\"read\",\"arguments\":\"{\\\"p\\\":1}\"}}\n\n",
                                    "event: response.output_item.done\n",
                                    "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"fc1\",\"call_id\":\"c_inactive\",\"name\":\"disabled_tool\",\"arguments\":\"{\\\"p\\\":1}\"}}\n\n",
                                    "event: response.output_item.done\n",
                                    "data: {\"type\":\"response.output_item.done\",\"output_index\":2,\"item\":{\"type\":\"function_call\",\"id\":\"fc2\",\"call_id\":\"c_bad\",\"name\":\"read\",\"arguments\":\"[1,2]\"}}\n\n",
                                    "event: response.completed\n",
                                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r0\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15}}}\n\n",
                                    "data: [DONE]\n\n"
                                )
                            } else if idx == 1 {
                                // Non-tool turn, but steer queued during round 1 will trigger round 3!
                                concat!(
                                    "event: response.output_text.delta\n",
                                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"round2\"}\n\n",
                                    "event: response.completed\n",
                                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"total_tokens\":12}}}\n\n",
                                    "data: [DONE]\n\n"
                                )
                            } else {
                                concat!(
                                    "event: response.output_text.delta\n",
                                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"after-steer\"}\n\n",
                                    "event: response.completed\n",
                                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r2\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"total_tokens\":12}}}\n\n",
                                    "data: [DONE]\n\n"
                                )
                            };
                            (
                                StatusCode::OK,
                                [(header::CONTENT_TYPE, "text/event-stream")],
                                sse,
                            )
                        }
                    }
                }),
            )
            .route(
                "/m/run-abort/responses",
                axum_post(|| async {
                    let sse = concat!(
                        "event: response.output_item.done\n",
                        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_a\",\"call_id\":\"c_wait_1\",\"name\":\"read\",\"arguments\":\"{}\"}}\n\n",
                        "event: response.output_item.done\n",
                        "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"id\":\"fc_b\",\"call_id\":\"c_wait_2\",\"name\":\"read\",\"arguments\":\"{}\"}}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"ra\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}\n\n",
                        "data: [DONE]\n\n"
                    );
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        sse,
                    )
                }),
            )
            .route(
                "/m/compact-empty/responses",
                axum_post(|| async {
                    let sse = concat!(
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"   \"}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"rc1\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1,\"total_tokens\":6}}}\n\n",
                        "data: [DONE]\n\n"
                    );
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        sse,
                    )
                }),
            )
            .route(
                "/m/compact-ok/responses",
                axum_post(|| async {
                    let sse = concat!(
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"summary ok\"}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"rc2\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}\n\n",
                        "data: [DONE]\n\n"
                    );
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        sse,
                    )
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let _ = axum::serve(listener, mock_proxy).await;
        });

        let state = test_state();
        let app = kernel_router(state.clone());

        let req_with = |method: &str, uri: &str, body: Option<Value>| {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "127.0.0.1:53817")
                .header("x-coda-token", "secret-token-123");
            if body.is_some() {
                b = b.header(header::CONTENT_TYPE, "application/json");
            }
            let body = match body {
                Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
                None => Body::empty(),
            };
            b.body(body).unwrap()
        };

        // Hydrate session.
        let hydrate_body = json!({
            "cwd": ".",
            "systemPrompt": "sys",
            "model": {
                "modelId": "gpt-5.2",
                "protocol": "openai-responses",
                "contextWindow": 400000,
                "maxOutputTokens": 16384
            },
            "tools": [
                { "name": "read", "description": "read", "parameters": { "type": "object" } },
                { "name": "disabled_tool", "description": "off", "parameters": { "type": "object" } }
            ],
            "activeToolNames": ["read"],
            "toolResultMaxChars": null,
            "compaction": { "enabled": false, "keepRecentTokens": 20000 },
            "steeringMode": "one-at-a-time",
            "gates": [],
            "history": { "leafId": null, "entries": [] }
        });
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "PUT",
                    "/api/kernel/sessions/s2",
                    Some(hydrate_body)
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );

        // Start run-steer.
        let run1 = json!({
            "runId": "run-steer",
            "baseRevision": 1,
            "content": [{ "type": "text", "text": "go" }],
            "modelProxy": {
                "baseUrl": format!("http://{proxy_addr}/m/run-steer"),
                "token": "ptok",
                "protocol": "openai-responses"
            }
        });
        assert_eq!(
            app.clone()
                .oneshot(req_with("POST", "/api/kernel/sessions/s2/runs", Some(run1)))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // Wait for waitingTools.
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, _) = state
                .store
                .subscribe_run_events("s2", "run-steer", 0)
                .unwrap();
            if replay
                .iter()
                .any(|e| e.event["type"] == "status" && e.event["state"] == "waitingTools")
            {
                break;
            }
        }

        // Queue TWO steers while in waitingTools (one customMessage via /entries + one /steer).
        // With steeringMode = "one-at-a-time", 1st injects after batch closes, 2nd triggers round 3 after round 2 non-tool turn!
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s2/entries",
                    Some(json!({
                        "entry": {
                            "type": "customMessage",
                            "name": "codaGroupPost",
                            "content": [{ "type": "text", "text": "group steer" }],
                            "display": true,
                            "details": { "value": { "from": "alice" } },
                            "includeInSummary": false
                        },
                        "deliverAs": "steer"
                    }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s2/runs/run-steer/steer",
                    Some(json!({ "content": [{ "type": "text", "text": "second steer" }] }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );

        // Submit c_ok tool result.
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s2/runs/run-steer/tool-results",
                    Some(json!({
                        "callId": "c_ok",
                        "result": { "content": [{ "type": "text", "text": "ok" }], "details": null }
                    }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );

        let replay = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, done) = state
                .store
                .subscribe_run_events("s2", "run-steer", 0)
                .unwrap();
            if done {
                break replay;
            }
        };

        // Only 1 toolCall event (`c_ok`) was emitted; inactive & non-object got synthesized toolResult events!
        let tool_calls: Vec<_> = replay
            .iter()
            .filter(|e| e.event["type"] == "toolCall")
            .collect();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].event["callId"], "c_ok");

        let tool_results: Vec<_> = replay
            .iter()
            .filter(|e| e.event["type"] == "toolResult")
            .collect();
        assert_eq!(tool_results.len(), 3);
        assert_eq!(tool_results[0].event["callId"], "c_ok");
        assert_eq!(tool_results[1].event["callId"], "c_inactive");
        assert_eq!(tool_results[1].event["result"]["isError"], true);
        assert_eq!(tool_results[2].event["callId"], "c_bad");
        assert_eq!(tool_results[2].event["result"]["isError"], true);

        // Both steers were committed (first preserving customMessage name/details/includeInSummary, second triggering round 3).
        let steered: Vec<_> = replay
            .iter()
            .filter(|e| e.event["type"] == "userMessage" && e.event["steered"] == true)
            .collect();
        assert_eq!(steered.len(), 2);
        assert!(replay.iter().any(|e| {
            e.event["type"] == "entry"
                && e.event["entry"]["type"] == "customMessage"
                && e.event["entry"]["name"] == "codaGroupPost"
                && e.event["entry"]["includeInSummary"] == false
        }));
        assert!(
            replay
                .iter()
                .any(|e| e.event["type"] == "text" && e.event["delta"] == "after-steer")
        );

        // Test manual compact: empty summary -> 502 COMPACTION_EMPTY_SUMMARY without bumping revision.
        let rev_before_compact = state.store.get_session("s2").unwrap().revision;
        let resp_empty = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s2/compact",
                Some(json!({
                    "baseRevision": rev_before_compact,
                    "keepRecentTokens": 0,
                    "modelProxy": {
                        "baseUrl": format!("http://{proxy_addr}/m/compact-empty"),
                        "token": "ptok",
                        "protocol": "openai-responses"
                    }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp_empty.status(), StatusCode::BAD_GATEWAY);
        let v_empty: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp_empty.into_body(), 1024 * 64)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v_empty["error"]["code"], "COMPACTION_EMPTY_SUMMARY");
        assert_eq!(
            state.store.get_session("s2").unwrap().revision,
            rev_before_compact
        );

        // Valid manual compact -> 200 and revision + 1.
        let resp_ok = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s2/compact",
                Some(json!({
                    "baseRevision": rev_before_compact,
                    "systemPrompt": "custom compactor",
                    "keepRecentTokens": 0,
                    "modelProxy": {
                        "baseUrl": format!("http://{proxy_addr}/m/compact-ok"),
                        "token": "ptok",
                        "protocol": "openai-responses"
                    }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp_ok.status(), StatusCode::OK);
        let rev_after_compact = state.store.get_session("s2").unwrap().revision;
        assert_eq!(rev_after_compact, rev_before_compact + 1);

        // Start run-abort, submit 1 of 2 tool results, then POST /abort -> synthetic `"aborted"` for c_wait_2.
        let run2 = json!({
            "runId": "run-abort",
            "baseRevision": rev_after_compact,
            "content": [{ "type": "text", "text": "abort me" }],
            "modelProxy": {
                "baseUrl": format!("http://{proxy_addr}/m/run-abort"),
                "token": "ptok",
                "protocol": "openai-responses"
            }
        });
        assert_eq!(
            app.clone()
                .oneshot(req_with("POST", "/api/kernel/sessions/s2/runs", Some(run2)))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, _) = state
                .store
                .subscribe_run_events("s2", "run-abort", 0)
                .unwrap();
            if replay
                .iter()
                .any(|e| e.event["type"] == "status" && e.event["state"] == "waitingTools")
            {
                break;
            }
        }
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s2/runs/run-abort/tool-results",
                    Some(json!({
                        "callId": "c_wait_1",
                        "result": { "content": [{ "type": "text", "text": "partial ok" }], "details": null }
                    }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s2/runs/run-abort/abort",
                    None
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        let (abort_replay, _, abort_done) = state
            .store
            .subscribe_run_events("s2", "run-abort", 0)
            .unwrap();
        assert!(abort_done);
        assert!(
            abort_replay
                .iter()
                .any(|e| e.event["type"] == "done" && e.event["reason"] == "aborted")
        );
        assert!(abort_replay.iter().any(|e| {
            e.event["type"] == "toolResult"
                && e.event["callId"] == "c_wait_2"
                && e.event["result"]["isError"] == true
                && e.event["result"]["content"][0]["text"] == "aborted"
        }));

        // Run-steer was evicted from last_finished_run by run-abort -> GET /events returns 410 EVENTS_EXPIRED,
        // while unknown runId returns 404 RUN_NOT_FOUND.
        let resp_410 = app
            .clone()
            .oneshot(req_with(
                "GET",
                "/api/kernel/sessions/s2/runs/run-steer/events?after=0",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp_410.status(), StatusCode::GONE);
        let resp_404 = app
            .clone()
            .oneshot(req_with(
                "GET",
                "/api/kernel/sessions/s2/runs/run-never/events?after=0",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp_404.status(), StatusCode::NOT_FOUND);

        proxy_task.abort();
    }

    #[tokio::test]
    async fn before_compact_gate_overflow_retry_terminate_finish_and_per_session_concurrency() {
        use axum::routing::post as axum_post;
        use std::sync::atomic::AtomicUsize;

        let overflow_calls = Arc::new(AtomicUsize::new(0));
        let ov_c = overflow_calls.clone();

        let mock_proxy = Router::new()
            .route(
                "/m/run-ov/responses",
                axum_post(move |Json(_body): Json<Value>| {
                    let ov = ov_c.clone();
                    async move {
                        let n = ov.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            (
                                StatusCode::BAD_REQUEST,
                                [(header::CONTENT_TYPE, "application/json")],
                                r#"{"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}"#,
                            )
                                .into_response()
                        } else {
                            let sse = concat!(
                                "event: response.output_item.done\n",
                                "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_t\",\"call_id\":\"c_term\",\"name\":\"ask\",\"arguments\":\"{}\"}}\n\n",
                                "event: response.completed\n",
                                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"rov\",\"usage\":{\"input_tokens\":8,\"output_tokens\":2,\"total_tokens\":10}}}\n\n",
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
                "/m/run-fast/responses",
                axum_post(|| async {
                    let sse = concat!(
                        "event: response.output_text.delta\n",
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"fast\"}\n\n",
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"rf\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1,\"total_tokens\":6}}}\n\n",
                        "data: [DONE]\n\n"
                    );
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        sse,
                    )
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let _ = axum::serve(listener, mock_proxy).await;
        });

        let state = test_state();
        let app = kernel_router(state.clone());

        let req_with = |method: &str, uri: &str, body: Option<Value>| {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "127.0.0.1:53817")
                .header("x-coda-token", "secret-token-123");
            if body.is_some() {
                b = b.header(header::CONTENT_TYPE, "application/json");
            }
            let body = match body {
                Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
                None => Body::empty(),
            };
            b.body(body).unwrap()
        };

        // Session A: overflow triggers beforeCompact gate -> host replaces summary -> retries -> tool with terminate: true.
        let h_a = json!({
            "cwd": ".",
            "systemPrompt": "sys",
            "model": { "modelId": "gpt-5.2", "protocol": "openai-responses", "contextWindow": 400000, "maxOutputTokens": 16384 },
            "tools": [{ "name": "ask", "description": "ask", "parameters": { "type": "object" } }],
            "activeToolNames": ["ask"],
            "toolResultMaxChars": null,
            "compaction": { "enabled": true, "keepRecentTokens": 0 },
            "steeringMode": "all",
            "gates": ["beforeCompact"],
            "history": {
                "leafId": "u_old",
                "entries": [{
                    "type": "message",
                    "id": "u_old",
                    "parentId": null,
                    "timestamp": "2026-11-26T00:00:00Z",
                    "role": "user",
                    "content": [{ "type": "text", "text": "old message" }]
                }]
            }
        });
        assert_eq!(
            app.clone()
                .oneshot(req_with("PUT", "/api/kernel/sessions/s-a", Some(h_a)))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );

        let h_b = json!({
            "cwd": ".",
            "systemPrompt": "sys",
            "model": { "modelId": "gpt-5.2", "protocol": "openai-responses" },
            "tools": [],
            "activeToolNames": [],
            "compaction": { "enabled": false },
            "steeringMode": "all",
            "gates": [],
            "history": { "leafId": null, "entries": [] }
        });
        assert_eq!(
            app.clone()
                .oneshot(req_with("PUT", "/api/kernel/sessions/s-b", Some(h_b)))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );

        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s-a/runs",
                    Some(json!({
                        "runId": "run-a",
                        "baseRevision": 1,
                        "content": [{ "type": "text", "text": "trigger overflow" }],
                        "modelProxy": { "baseUrl": format!("http://{proxy_addr}/m/run-ov"), "token": "p", "protocol": "openai-responses" }
                    }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // Wait for beforeCompact gate on Session A.
        let gate_id = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, _) = state.store.subscribe_run_events("s-a", "run-a", 0).unwrap();
            if let Some(ev) = replay
                .iter()
                .find(|e| e.event["type"] == "gate" && e.event["kind"] == "beforeCompact")
            {
                break ev.event["gateId"].as_str().unwrap().to_owned();
            }
        };

        // While Session A is blocked on gate, Session B MUST run and complete concurrently (per-session locking!).
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s-b/runs",
                    Some(json!({
                        "runId": "run-b",
                        "baseRevision": 1,
                        "content": [{ "type": "text", "text": "concurrent b" }],
                        "modelProxy": { "baseUrl": format!("http://{proxy_addr}/m/run-fast"), "token": "p", "protocol": "openai-responses" }
                    }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let replay_b = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, done) = state.store.subscribe_run_events("s-b", "run-b", 0).unwrap();
            if done {
                break replay;
            }
        };
        assert!(
            replay_b
                .iter()
                .any(|e| e.event["type"] == "done" && e.event["reason"] == "completed")
        );

        // Resolve Session A's beforeCompact gate with `replace{summary}`.
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    &format!("/api/kernel/sessions/s-a/runs/run-a/gates/{gate_id}"),
                    Some(json!({ "action": "replace", "summary": "host replaced summary" }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // Wait for toolCall `c_term`, then submit `terminate: true` -> run completes immediately without 2nd model call.
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, _) = state.store.subscribe_run_events("s-a", "run-a", 0).unwrap();
            if replay.iter().any(|e| e.event["type"] == "toolCall") {
                break;
            }
        }
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s-a/runs/run-a/tool-results",
                    Some(json!({
                        "callId": "c_term",
                        "result": { "content": [{ "type": "text", "text": "asked user" }], "details": null, "terminate": true }
                    }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );
        let replay_a = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, done) = state.store.subscribe_run_events("s-a", "run-a", 0).unwrap();
            if done {
                break replay;
            }
        };
        assert_eq!(overflow_calls.load(Ordering::SeqCst), 2);
        assert!(replay_a.iter().any(|e| {
            e.event["type"] == "compaction"
                && e.event["active"] == false
                && e.event["summary"] == "host replaced summary"
        }));
        assert!(
            replay_a
                .iter()
                .any(|e| e.event["type"] == "done" && e.event["reason"] == "completed")
        );

        proxy_task.abort();
    }

    #[tokio::test]
    async fn wire_schema_defaults_optional_fields_leaf_id_and_rejects_custom_provider_id() {
        use axum::routing::post as axum_post;

        let captured_bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let cb1 = captured_bodies.clone();
        let cb2 = captured_bodies.clone();

        let mock_proxy = Router::new()
            .route(
                "/m/run-opts/responses",
                axum_post(move |Json(body): Json<Value>| {
                    let cb = cb1.clone();
                    async move {
                        cb.lock().unwrap().push(body);
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
                    }
                }),
            )
            .route(
                "/m/compact-opts/responses",
                axum_post(move |Json(body): Json<Value>| {
                    let cb = cb2.clone();
                    async move {
                        cb.lock().unwrap().push(body);
                        let sse = concat!(
                            "event: response.output_text.delta\n",
                            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"compacted\"}\n\n",
                            "event: response.completed\n",
                            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"rc\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}\n\n",
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
        let proxy_addr = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let _ = axum::serve(listener, mock_proxy).await;
        });

        let state = test_state();
        let app = kernel_router(state.clone());

        let req_with = |method: &str, uri: &str, body: Option<Value>| {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "127.0.0.1:53817")
                .header("x-coda-token", "secret-token-123");
            if body.is_some() {
                b = b.header(header::CONTENT_TYPE, "application/json");
            }
            let body = match body {
                Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
                None => Body::empty(),
            };
            b.body(body).unwrap()
        };

        // 1. Hydrate omitting `compaction`, `steeringMode`, `gates` -> 201 with defaults.
        let resp = app
            .clone()
            .oneshot(req_with(
                "PUT",
                "/api/kernel/sessions/s-wire",
                Some(json!({
                    "cwd": ".",
                    "systemPrompt": "sys",
                    "model": {
                        "modelId": "gpt-5.2",
                        "protocol": "openai-responses",
                        "contextWindow": 200000,
                        "maxOutputTokens": 8192
                    },
                    "tools": [],
                    "activeToolNames": [],
                    "history": {
                        "leafId": "e-1",
                        "entries": [{
                            "type": "message",
                            "id": "e-1",
                            "parentId": null,
                            "timestamp": "2026-11-26T00:00:00Z",
                            "role": "user",
                            "content": [{ "type": "text", "text": "prior" }]
                        }]
                    }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // 2. Reject `customProviderId` in PUT and PATCH model configs.
        let resp_bad_put = app
            .clone()
            .oneshot(req_with(
                "PUT",
                "/api/kernel/sessions/s-bad",
                Some(json!({
                    "cwd": ".",
                    "systemPrompt": "sys",
                    "model": {
                        "modelId": "gpt-5.2",
                        "customProviderId": "forbidden",
                        "protocol": "openai-responses"
                    },
                    "tools": [],
                    "activeToolNames": [],
                    "history": { "leafId": null, "entries": [] }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp_bad_put.status(), StatusCode::BAD_REQUEST);

        let resp_bad_patch = app
            .clone()
            .oneshot(req_with(
                "PATCH",
                "/api/kernel/sessions/s-wire",
                Some(json!({
                    "model": { "modelId": "gpt-5.2", "customProviderId": "forbidden" }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp_bad_patch.status(), StatusCode::BAD_REQUEST);

        // 3. StartRunReq with unknown leafId -> 400 INVALID_REQUEST.
        let resp_bad_leaf = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s-wire/runs",
                Some(json!({
                    "runId": "run-bad-leaf",
                    "baseRevision": 1,
                    "model": { "modelId": "gpt-5.2" },
                    "content": [],
                    "leafId": "does-not-exist",
                    "modelProxy": {
                        "baseUrl": format!("http://{proxy_addr}/m/run-opts"),
                        "token": "p",
                        "protocol": "openai-responses"
                    }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp_bad_leaf.status(), StatusCode::BAD_REQUEST);

        // 4. StartRunReq with contextWindow, maxOutputTokens, valid leafId -> 200.
        let resp_ok_run = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s-wire/runs",
                Some(json!({
                    "runId": "run-opts",
                    "baseRevision": 1,
                    "model": { "modelId": "gpt-5.2" },
                    "content": [{ "type": "text", "text": "go" }],
                    "contextWindow": 123456,
                    "maxOutputTokens": 2048,
                    "leafId": "e-1",
                    "modelProxy": {
                        "baseUrl": format!("http://{proxy_addr}/m/run-opts"),
                        "token": "p",
                        "protocol": "openai-responses"
                    }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp_ok_run.status(), StatusCode::OK);

        let replay = loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, done) = state
                .store
                .subscribe_run_events("s-wire", "run-opts", 0)
                .unwrap();
            if done {
                break replay;
            }
        };
        let cu = replay
            .iter()
            .find(|e| e.event["type"] == "contextUsage")
            .unwrap();
        assert_eq!(cu.event["contextWindow"], 123456);

        // 5. CompactReq with contextWindow & maxOutputTokens -> 200.
        let rev = state.store.get_session("s-wire").unwrap().revision;
        let resp_compact = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s-wire/compact",
                Some(json!({
                    "baseRevision": rev,
                    "keepRecentTokens": 0,
                    "model": { "modelId": "gpt-5.2" },
                    "contextWindow": 123456,
                    "maxOutputTokens": 3072,
                    "modelProxy": {
                        "baseUrl": format!("http://{proxy_addr}/m/compact-opts"),
                        "token": "p",
                        "protocol": "openai-responses"
                    }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp_compact.status(), StatusCode::OK);

        {
            let bodies = captured_bodies.lock().unwrap();
            assert_eq!(bodies[0]["max_output_tokens"], 2048);
            assert_eq!(bodies[1]["max_output_tokens"], 3072);
        }

        // 6. Unmatched path -> 404 JSON error; wrong method -> 405 JSON error.
        let resp_404 = app
            .clone()
            .oneshot(req_with("GET", "/api/kernel/nonexistent", None))
            .await
            .unwrap();
        assert_eq!(resp_404.status(), StatusCode::NOT_FOUND);
        let v_404: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp_404.into_body(), 1024 * 64)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v_404["error"]["code"], "NOT_FOUND");

        let resp_405 = app
            .oneshot(req_with("POST", "/api/kernel/capabilities", None))
            .await
            .unwrap();
        assert_eq!(resp_405.status(), StatusCode::METHOD_NOT_ALLOWED);
        let v_405: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp_405.into_body(), 1024 * 64)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v_405["error"]["code"], "METHOD_NOT_ALLOWED");

        proxy_task.abort();
    }

    #[tokio::test]
    async fn prune_boundary_null_override_mid_run_patch_precedence_and_zero_token_abort() {
        use axum::routing::post as axum_post;
        use std::sync::atomic::AtomicUsize;

        let captured = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let cap_c = captured.clone();
        let turn_n = Arc::new(AtomicUsize::new(0));
        let tn_c = turn_n.clone();

        let mock_proxy = Router::new()
            .route(
                "/m/run-patch/responses",
                axum_post(move |Json(body): Json<Value>| {
                    let cb = cap_c.clone();
                    let tn = tn_c.clone();
                    async move {
                        cb.lock().unwrap().push(body);
                        let n = tn.fetch_add(1, Ordering::SeqCst);
                        let sse = if n == 0 {
                            concat!(
                                "event: response.output_item.done\n",
                                "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc1\",\"call_id\":\"call_turn1\",\"name\":\"read\",\"arguments\":\"{}\"}}\n\n",
                                "event: response.completed\n",
                                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15}}}\n\n",
                                "data: [DONE]\n\n"
                            )
                        } else {
                            concat!(
                                "event: response.output_text.delta\n",
                                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
                                "event: response.completed\n",
                                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r2\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"total_tokens\":12}}}\n\n",
                                "data: [DONE]\n\n"
                            )
                        };
                        (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            sse,
                        )
                    }
                }),
            )
            .route(
                "/m/run-zero-abort/responses",
                axum_post(|| async {
                    let stream = async_stream::stream! {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        yield Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"data: [DONE]\n\n"));
                    };
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        Body::from_stream(stream),
                    )
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(async move {
            let _ = axum::serve(listener, mock_proxy).await;
        });

        let state = test_state();
        let app = kernel_router(state.clone());

        let req_with = |method: &str, uri: &str, body: Option<Value>| {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "127.0.0.1:53817")
                .header("x-coda-token", "secret-token-123");
            if body.is_some() {
                b = b.header(header::CONTENT_TYPE, "application/json");
            }
            let body = match body {
                Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
                None => Body::empty(),
            };
            b.body(body).unwrap()
        };

        let long_old = "Z".repeat(200);
        let hydrate = json!({
            "cwd": ".",
            "systemPrompt": "sys",
            "model": {
                "modelId": "gpt-5.2",
                "protocol": "openai-responses",
                "contextWindow": 400000,
                "maxOutputTokens": 16384
            },
            "tools": [{ "name": "read", "description": "read", "parameters": { "type": "object" } }],
            "activeToolNames": ["read"],
            "contextProjection": {
                "pruneBoundaryCallId": "call_keep",
                "pruneMinLength": 50,
                "pruneKeepLength": 10
            },
            "history": {
                "leafId": "e4",
                "entries": [
                    {
                        "type": "message",
                        "id": "e1",
                        "parentId": null,
                        "timestamp": "2026-11-26T00:00:00Z",
                        "role": "assistant",
                        "content": [{ "type": "toolCall", "callId": "call_old", "name": "read", "args": {} }]
                    },
                    {
                        "type": "toolResult",
                        "id": "e2",
                        "parentId": "e1",
                        "timestamp": "2026-11-26T00:00:01Z",
                        "callId": "call_old",
                        "name": "read",
                        "content": [{ "type": "text", "text": long_old }],
                        "isError": false
                    },
                    {
                        "type": "message",
                        "id": "e3",
                        "parentId": "e2",
                        "timestamp": "2026-11-26T00:00:02Z",
                        "role": "assistant",
                        "content": [{ "type": "toolCall", "callId": "call_keep", "name": "read", "args": {} }]
                    },
                    {
                        "type": "toolResult",
                        "id": "e4",
                        "parentId": "e3",
                        "timestamp": "2026-11-26T00:00:03Z",
                        "callId": "call_keep",
                        "name": "read",
                        "content": [{ "type": "text", "text": "kept-result" }],
                        "isError": false
                    }
                ]
            }
        });
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "PUT",
                    "/api/kernel/sessions/s-proj",
                    Some(hydrate)
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );

        // Start Run with explicit `"pruneBoundaryCallId": null` and initial `"runtimeContext"`.
        let run_resp = app
            .clone()
            .oneshot(req_with(
                "POST",
                "/api/kernel/sessions/s-proj/runs",
                Some(json!({
                    "runId": "run-patch",
                    "baseRevision": 1,
                    "content": [{ "type": "text", "text": "check" }],
                    "runtimeContext": [{ "type": "text", "text": "RUN_EPHEMERAL_CTX" }],
                    "pruneBoundaryCallId": null,
                    "modelProxy": {
                        "baseUrl": format!("http://{proxy_addr}/m/run-patch"),
                        "token": "p",
                        "protocol": "openai-responses"
                    }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(run_resp.status(), StatusCode::OK);

        // Wait for waitingTools on run-patch.
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, _) = state
                .store
                .subscribe_run_events("s-proj", "run-patch", 0)
                .unwrap();
            if replay
                .iter()
                .any(|e| e.event["type"] == "status" && e.event["state"] == "waitingTools")
            {
                break;
            }
        }

        // Mid-run PATCH updating BOTH `pruneBoundaryCallId` and `runtimeContext`.
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "PATCH",
                    "/api/kernel/sessions/s-proj",
                    Some(json!({
                        "pruneBoundaryCallId": "call_keep",
                        "runtimeContext": [{ "type": "text", "text": "PATCHED_MID_RUN_CTX" }]
                    }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // Submit tool result -> Turn 2 sees mid-run PATCH overrides.
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s-proj/runs/run-patch/tool-results",
                    Some(json!({
                        "callId": "call_turn1",
                        "result": { "content": [{ "type": "text", "text": "t1" }], "details": null }
                    }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::ACCEPTED
        );

        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (_replay, _, done) = state
                .store
                .subscribe_run_events("s-proj", "run-patch", 0)
                .unwrap();
            if done {
                break;
            }
        }

        {
            let bodies = captured.lock().unwrap();
            assert_eq!(bodies.len(), 2);
            let turn1 = bodies[0].to_string();
            let turn2 = bodies[1].to_string();
            // Turn 1: `pruneBoundaryCallId: null` disabled pruning (full 200 Zs), and used RUN_EPHEMERAL_CTX.
            assert!(turn1.contains(&"Z".repeat(200)));
            assert!(!turn1.contains("pruned"));
            assert!(turn1.contains("RUN_EPHEMERAL_CTX"));
            // Turn 2: mid-run PATCH took precedence over both!
            assert!(!turn2.contains(&"Z".repeat(200)));
            assert!(turn2.contains("pruned 190 chars before pruneBoundaryCallId"));
            assert!(turn2.contains("PATCHED_MID_RUN_CTX"));
            assert!(!turn2.contains("RUN_EPHEMERAL_CTX"));
        }

        // Start Run 2 that aborts at 0 tokens -> `done` must omit `assistantEntryId`.
        let rev = state.store.get_session("s-proj").unwrap().revision;
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s-proj/runs",
                    Some(json!({
                        "runId": "run-zero-abort",
                        "baseRevision": rev,
                        "content": [{ "type": "text", "text": "abort 0" }],
                        "modelProxy": {
                            "baseUrl": format!("http://{proxy_addr}/m/run-zero-abort"),
                            "token": "p",
                            "protocol": "openai-responses"
                        }
                    }))
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (replay, _, _) = state
                .store
                .subscribe_run_events("s-proj", "run-zero-abort", 0)
                .unwrap();
            if replay.iter().any(|e| e.event["type"] == "start") {
                break;
            }
        }
        assert_eq!(
            app.clone()
                .oneshot(req_with(
                    "POST",
                    "/api/kernel/sessions/s-proj/runs/run-zero-abort/abort",
                    None
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        let (abort_replay, _, abort_done) = state
            .store
            .subscribe_run_events("s-proj", "run-zero-abort", 0)
            .unwrap();
        assert!(abort_done);
        let done_ev = abort_replay
            .iter()
            .find(|e| e.event["type"] == "done")
            .unwrap();
        assert_eq!(done_ev.event["reason"], "aborted");
        assert!(done_ev.event.get("assistantEntryId").is_none());

        proxy_task.abort();
    }

    #[test]
    fn read_token_from_env_validates_missing_empty_and_invalid_without_leaking_secret() {
        let missing =
            read_token_from_env_with("CODA_MISSING_TOK", |_| Err(std::env::VarError::NotPresent))
                .unwrap_err();
        assert!(missing.to_string().contains("CODA_MISSING_TOK"));

        let empty_name = read_token_from_env_with("   ", |_| Ok("unused".into())).unwrap_err();
        assert!(empty_name.to_string().contains("cannot be empty"));

        let empty_val =
            read_token_from_env_with("CODA_EMPTY_TOK", |_| Ok("\r\n".into())).unwrap_err();
        assert!(empty_val.to_string().contains("CODA_EMPTY_TOK"));

        let secret_invalid = "secret-val\x07bad";
        let invalid_hdr =
            read_token_from_env_with("CODA_BAD_TOK", |_| Ok(secret_invalid.into())).unwrap_err();
        let err_str = format!("{invalid_hdr:#}");
        assert!(err_str.contains("CODA_BAD_TOK"));
        assert!(!err_str.contains("secret-val"));

        let valid =
            read_token_from_env_with("CODA_GOOD_TOK", |_| Ok("tok-env-abc123\n".into())).unwrap();
        assert_eq!(valid, "tok-env-abc123");
    }

    #[test]
    fn read_token_from_env_rejects_whitespace_only_values() {
        for (var_name, raw) in [
            ("CODA_WS_SPACES", "   "),
            ("CODA_WS_TAB", "\t"),
            ("CODA_WS_CRLF_SPACE", "\r\n "),
            ("CODA_WS_MIXED", " \t \r\n"),
        ] {
            let err = read_token_from_env_with(var_name, |_| Ok(raw.into())).unwrap_err();
            let err_str = format!("{err:#}");
            assert!(
                err_str.contains(var_name),
                "expected error to mention {var_name}, got: {err_str}"
            );
            assert!(
                err_str.contains("cannot be empty"),
                "expected empty/whitespace error message for {var_name}, got: {err_str}"
            );
        }
    }

    #[tokio::test]
    async fn run_embedded_with_token_env_serves_capabilities_and_shuts_down() {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let stop = CancellationToken::new();
        let stop_fut = stop.clone();

        let cfg = KernelServeConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            data_dir: None,
            token_stdin: false,
            token_env: Some("CODA_TEST_ENV_TOKEN".into()),
            embedded: true,
            media_proxy: false,
        };
        let server_task = tokio::spawn(async move {
            run_embedded_inner(
                cfg,
                |_| Ok("env-secret-token-xyz".into()),
                Some(ready_tx),
                async move {
                    stop_fut.cancelled().await;
                },
            )
            .await
        });

        let (port, ready_line) = ready_rx.await.unwrap();
        let ready_json: Value = serde_json::from_str(&ready_line).unwrap();
        assert_eq!(ready_json["type"], "ready");
        assert_eq!(ready_json["protocol"], KERNEL_PROTOCOL);
        assert_eq!(ready_json["port"], port);

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/api/kernel/capabilities"))
            .header("x-coda-token", "env-secret-token-xyz")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let caps: Value = resp.json().await.unwrap();
        assert_eq!(caps["protocol"], KERNEL_PROTOCOL);

        stop.cancel();
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shutdown_drain_returns_503_across_repeated_concurrent_eof_and_signal_trials() {
        for trial in 0..12 {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let eof_cancel = CancellationToken::new();
            let stop = CancellationToken::new();
            let eof_for_server = eof_cancel.clone();
            let stop_for_server = stop.clone();

            let cfg = KernelServeConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                data_dir: None,
                token_stdin: false,
                token_env: Some("CODA_TEST_ENV_TOKEN".into()),
                embedded: true,
                media_proxy: false,
            };
            let server_task = tokio::spawn(async move {
                run_embedded_with_eof(
                    cfg,
                    |_| Ok("drain-secret-token".into()),
                    Some(ready_tx),
                    eof_for_server,
                    async move {
                        stop_for_server.cancelled().await;
                    },
                )
                .await
            });

            let (port, _) = ready_rx.await.unwrap();
            let url = format!("http://127.0.0.1:{port}/api/kernel/capabilities");

            // Warm up a keep-alive client and verify server is ready (`200 OK`).
            let keepalive_client = reqwest::Client::new();
            let warm_resp = keepalive_client
                .get(&url)
                .header("x-coda-token", "drain-secret-token")
                .send()
                .await
                .unwrap_or_else(|e| panic!("trial {trial} warmup failed: {e}"));
            assert_eq!(warm_resp.status(), StatusCode::OK);
            let _ = warm_resp.bytes().await.unwrap();

            let fresh_client = reqwest::Client::builder()
                .pool_max_idle_per_host(0)
                .build()
                .unwrap();

            let started = std::time::Instant::now();
            // Exercise both `--token-stdin` EOF path (10 trials) and signal `shutdown` path (2 trials).
            if trial < 10 {
                eof_cancel.cancel();
            } else {
                stop.cancel();
            }
            // Yield once so `server_task` enters the drain `select!` branch.
            tokio::task::yield_now().await;

            // Fire concurrent + staggered HTTP probes across the drain window (`0ms`, `30ms`, `100ms`).
            for wave in 0..3 {
                if wave == 1 {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                } else if wave == 2 {
                    tokio::time::sleep(Duration::from_millis(70)).await;
                }
                let futs = (0..4).map(|idx| {
                    let client = if idx % 2 == 0 {
                        keepalive_client.clone()
                    } else {
                        fresh_client.clone()
                    };
                    let url = url.clone();
                    async move {
                        let resp = client
                            .get(&url)
                            .header("x-coda-token", "drain-secret-token")
                            .send()
                            .await
                            .unwrap_or_else(|e| {
                                panic!("trial {trial} wave {wave} req {idx} transport error: {e}")
                            });
                        let status = resp.status();
                        let conn_hdr = resp
                            .headers()
                            .get(header::CONNECTION)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned);
                        let body: Value = resp.json().await.unwrap_or_else(|e| {
                            panic!("trial {trial} wave {wave} req {idx} body error: {e}")
                        });
                        (status, conn_hdr, body)
                    }
                });
                let results = futures_util::future::join_all(futs).await;
                for (idx, (status, conn_hdr, body)) in results.into_iter().enumerate() {
                    assert_eq!(
                        status,
                        StatusCode::SERVICE_UNAVAILABLE,
                        "trial {trial} wave {wave} req {idx} expected 503, got {status}: {body}"
                    );
                    assert_eq!(
                        conn_hdr.as_deref(),
                        Some("close"),
                        "trial {trial} wave {wave} req {idx} missing Connection: close"
                    );
                    assert_eq!(body["error"]["code"], "KERNEL_SHUTTING_DOWN");
                    assert_eq!(body["error"]["retryable"], true);
                }
            }

            let exit_res = tokio::time::timeout(Duration::from_secs(5), server_task)
                .await
                .unwrap_or_else(|_| panic!("trial {trial} server did not exit within 5s"))
                .unwrap();
            assert!(
                exit_res.is_ok(),
                "trial {trial} server exited with error: {exit_res:?}"
            );
            assert!(
                started.elapsed() <= Duration::from_secs(5),
                "trial {trial} exceeded 5s exit budget: {:?}",
                started.elapsed()
            );
        }
    }
}
