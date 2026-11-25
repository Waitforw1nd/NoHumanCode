use crate::{
    approval,
    budget::{BudgetConfig, BudgetError, BudgetErrorKind, ConfigureBudget},
    checkpoint::CheckpointError,
    domain::*,
    engine::Engine,
    git_diff::{GitDiffError, GitDiffRequest, GitDiffView},
    host_configuration::{ConfigureHostRoute, HostConfigError},
    network_authorization::{ConfirmRequest, NetworkError, NetworkErrorKind, NetworkSubject},
    plugin_management::{PluginManagementError, SetPluginEnabled},
    project_config::{
        ConfigureProject, ProjectConfig, ProjectConfigSnapshot, ProjectError, ProjectSelection,
        RegisterProject, SelectProject, StartInProject,
    },
    secrets,
    workspace_changes::{RestoreStatus, WorkspaceChangeError},
    worktree_management::{CreateWorktree, WorktreeError, WorktreeState},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, FromRequest, Path, RawQuery, Request, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response, Sse,
        sse::{Event as SseEvent, KeepAlive},
    },
    routing::{get, post, put},
};
use coda_protocol::{ErrorBody, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct App {
    pub engine: Arc<Engine>,
    pub token: String,
    pub origin: String,
}

#[derive(Debug)]
pub struct ApiError {
    error: anyhow::Error,
    code: ErrorCode,
    retryable: bool,
    status: StatusCode,
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        classify_anyhow(error)
    }
}
impl ApiError {
    fn with_code(
        status: StatusCode,
        code: ErrorCode,
        retryable: bool,
        error: anyhow::Error,
    ) -> Self {
        Self {
            error,
            code,
            retryable,
            status,
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Some(error) = self.error.downcast_ref::<WorktreeError>() {
            return worktree_failure(error);
        }
        if let Some(error) = self.error.downcast_ref::<NetworkError>() {
            return network_error(error.clone());
        }
        if let Some(error) = self.error.downcast_ref::<ProjectError>() {
            return project_failure(error.code(), &error.to_string());
        }
        let message = secrets::scrub(&self.error.to_string(), "");
        let body = ErrorBody {
            code: self.code,
            message: message.clone(),
            retryable: self.retryable,
        };
        let mut value = serde_json::to_value(body).unwrap_or_else(|_| json!({}));
        value["error"] = Value::String(message);
        (self.status, Json(value)).into_response()
    }
}

pub struct ContractJson<T>(pub T);

struct StrictObject<T>(T);
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

impl<S, T> FromRequest<S> for ContractJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(
        request: Request,
        state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_rejection(rejection)),
        }
    }
}

fn json_rejection(rejection: JsonRejection) -> ApiError {
    let (status, message) = match &rejection {
        JsonRejection::MissingJsonContentType(_) => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "JSON 接口需要 application/json Content-Type",
        ),
        JsonRejection::JsonSyntaxError(_) | JsonRejection::JsonDataError(_) => {
            (StatusCode::BAD_REQUEST, "JSON 请求无效")
        }
        JsonRejection::BytesRejection(
            axum::extract::rejection::BytesRejection::FailedToBufferBody(
                axum::extract::rejection::FailedToBufferBody::LengthLimitError(_),
            ),
        ) => (StatusCode::PAYLOAD_TOO_LARGE, "请求体超过 1 MiB 限制"),
        JsonRejection::BytesRejection(_) => (StatusCode::BAD_REQUEST, "无法读取请求体"),
        _ => (StatusCode::BAD_REQUEST, "JSON 请求无效"),
    };
    let _ = rejection;
    ApiError::with_code(
        status,
        ErrorCode::RequestFailed,
        false,
        anyhow::anyhow!(message),
    )
}

const SECURITY_HEADERS: [(&str, &str); 3] = [
    ("cache-control", "no-store"),
    ("x-content-type-options", "nosniff"),
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
    ),
];

fn apply_security_headers(response: &mut Response) {
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        headers.insert(name, HeaderValue::from_static(value));
    }
}

fn forbidden(message: &'static str) -> Response {
    let mut response = ApiError::with_code(
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
        false,
        anyhow::anyhow!(message),
    )
    .into_response();
    apply_security_headers(&mut response);
    response
}

fn classify_anyhow(error: anyhow::Error) -> ApiError {
    if error
        .downcast_ref::<crate::store::IdempotencyConflict>()
        .is_some()
        || error.chain().any(|cause| {
            cause
                .downcast_ref::<crate::store::IdempotencyConflict>()
                .is_some()
        })
    {
        return ApiError::with_code(StatusCode::CONFLICT, ErrorCode::Conflict, false, error);
    }
    ApiError::with_code(
        StatusCode::BAD_REQUEST,
        ErrorCode::RequestFailed,
        false,
        error,
    )
}

fn is_missing_row(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::QueryReturnedNoRows)
        )
    })
}

fn is_transient_store(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .and_then(|sqlite| match sqlite {
                rusqlite::Error::SqliteFailure(code, _) => Some(code.code),
                _ => None,
            })
            .is_some_and(|code| {
                matches!(
                    code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
            })
    })
}

fn is_corrupt_store(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rusqlite::Error>(),
            Some(
                rusqlite::Error::FromSqlConversionFailure(..)
                    | rusqlite::Error::InvalidColumnType(..)
                    | rusqlite::Error::IntegralValueOutOfRange(..)
                    | rusqlite::Error::Utf8Error(_)
            )
        ) || cause.is::<serde_json::Error>()
    })
}

fn read_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<ProjectError>().is_some()
        || error.downcast_ref::<NetworkError>().is_some()
        || error.downcast_ref::<WorktreeError>().is_some()
    {
        return classify_anyhow(error);
    }
    if error
        .downcast_ref::<crate::store::IdempotencyConflict>()
        .is_some()
        || error
            .chain()
            .any(|cause| cause.is::<crate::store::IdempotencyConflict>())
    {
        return ApiError::with_code(StatusCode::CONFLICT, ErrorCode::Conflict, false, error);
    }
    if is_missing_row(&error) {
        return ApiError::with_code(StatusCode::NOT_FOUND, ErrorCode::NotFound, false, error);
    }
    let retryable = is_transient_store(&error);
    let message = if is_corrupt_store(&error) {
        "存储数据损坏"
    } else if retryable {
        "存储暂时不可用"
    } else {
        "存储读取失败"
    };
    ApiError::with_code(
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
        retryable,
        anyhow::anyhow!(message),
    )
}

fn path_rejection(rejection: PathRejection) -> ApiError {
    let _ = rejection;
    ApiError::with_code(
        StatusCode::BAD_REQUEST,
        ErrorCode::RequestFailed,
        false,
        anyhow::anyhow!("路径参数无效"),
    )
}

fn object_id(path: std::result::Result<Path<String>, PathRejection>) -> Result<String> {
    path.map(|Path(id)| id).map_err(path_rejection)
}

fn stream_failure(after: i64, error: &anyhow::Error) -> SseEvent {
    let retryable = is_transient_store(error);
    let message = if is_corrupt_store(error) {
        "存储数据损坏"
    } else if retryable {
        "存储暂时不可用"
    } else {
        "存储读取失败"
    };
    let body = ErrorBody {
        code: ErrorCode::Internal,
        message: secrets::scrub(message, ""),
        retryable,
    };
    let mut value = serde_json::to_value(body).unwrap_or_else(|_| json!({}));
    value["after"] = json!(after);
    SseEvent::default().event("error").data(value.to_string())
}

type Result<T> = std::result::Result<T, ApiError>;

pub fn router(app: App) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(javascript))
        .route("/style.css", get(stylesheet))
        .route("/api/health", get(health))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/plugins", get(plugins))
        .route("/api/plugins/{id}", get(plugin_status))
        .route("/api/plugins/{id}/enabled", put(set_plugin_enabled))
        .route("/api/runtime/agents/chat", get(agent_runtime_status))
        .route("/api/runtime/components", get(runtime_components))
        .route("/api/runtime/components/{id}", get(runtime_component))
        .route(
            "/api/runtime/components/{id}/enabled",
            put(set_runtime_component),
        )
        .route("/api/settings", get(settings).put(save_settings))
        .route("/api/host/composition", get(host_composition))
        .route("/api/host/config", get(host_config))
        .route("/api/host/routes/{id}", put(configure_host_route))
        .route("/api/host/config/lookup", post(host_config_lookup))
        .route(
            "/api/routes/{id}/models",
            get(network_get_moved).post(models),
        )
        .route("/api/routes/{id}/check", post(check))
        .route("/api/newapi", put(account))
        .route("/api/newapi/balance", get(network_get_moved).post(balance))
        .route("/api/network/preview", post(network_preview))
        .route("/api/network/authorizations", post(network_confirm))
        .route(
            "/api/network/authorizations/{id}",
            get(network_authorization),
        )
        .route(
            "/api/network/authorizations/{id}/revoke",
            post(network_revoke),
        )
        .route("/api/network/calls/{id}", get(network_call))
        .route("/api/tasks/{id}/network-calls", get(task_network_calls))
        .route("/api/runs", get(runs).post(start))
        .route("/api/runs/{id}", get(run))
        .route("/api/runs/{id}/context", get(run_context))
        .route("/api/runs/{id}/events", get(events))
        .route("/api/projects", get(projects).post(register_project))
        .route("/api/projects/{id}", get(project))
        .route(
            "/api/projects/{id}/config",
            get(project_config).put(configure_project),
        )
        .route(
            "/api/projects/{id}/budget",
            get(budget_config).put(configure_budget),
        )
        .route("/api/projects/{id}/budget/status", get(budget_status))
        .route("/api/projects/{id}/budget/calls/{call}", get(budget_call))
        .route("/api/projects/{id}/runs", post(start_project))
        .route("/api/projects/{id}/worktrees", post(create_worktree))
        .route("/api/projects/{id}/worktrees/lookup", post(lookup_worktree))
        .route("/api/projects/{id}/worktrees/{op}", get(worktree_status))
        .route(
            "/api/projects/{id}/worktrees/{op}/inspect",
            post(inspect_worktree),
        )
        .route(
            "/api/project-selection",
            get(project_selection).put(select_project),
        )
        .route("/api/tasks/{id}/project", get(task_project))
        .route("/api/sessions/{id}", get(session))
        .route(
            "/api/sessions/{id}/turns",
            get(session_turns).post(post_session_turn),
        )
        .route("/api/sessions/{id}/events", get(session_events))
        .route("/api/turns/{id}", get(turn))
        .route("/api/wasm/plugins", get(wasm_unsupported))
        .route("/api/wasm/run", post(wasm_unsupported))
        .route("/api/tasks/{id}/resume", post(resume))
        .route("/api/tasks/{id}/cancel", post(cancel))
        .route("/api/tasks/{id}/approvals", get(task_approvals))
        .route("/api/approvals/{id}", get(approval))
        .route("/api/approvals/{id}/decision", post(decide_approval))
        .route("/api/tasks/{id}/changes", get(changes))
        .route("/api/tasks/{id}/restore", post(restore))
        .route("/api/tasks/{id}/git-diff", post(git_diff))
        .route("/api/tasks/{id}/checkpoints", post(create_checkpoint))
        .route("/api/checkpoints/{id}", get(checkpoint))
        .route("/api/checkpoints/{id}/restore", post(restore_checkpoint))
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

async fn guard(State(app): State<App>, request: Request, next: Next) -> Response {
    let expected_host = app.origin.strip_prefix("http://").unwrap_or(&app.origin);
    let headers = request.headers();
    let host_configuration = request.uri().path().starts_with("/api/host/");
    let denied = |message| {
        if host_configuration {
            host_config_failure(HostConfigError::Forbidden)
        } else {
            forbidden(message)
        }
    };
    if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(expected_host) {
        return denied("请通过启动地址访问本机服务");
    }
    if let Some(origin) = headers.get(header::ORIGIN)
        && origin.to_str().ok() != Some(&app.origin)
    {
        return denied("Origin 与本机服务地址不一致");
    }
    if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site") {
        return denied("拒绝跨站请求");
    }
    let authenticated_read = request.uri().path() == "/api/host/composition"
        || host_configuration
        || request.uri().path() == "/api/runtime/agents/chat"
        || request.uri().path() == "/api/runtime/components"
        || request.uri().path().starts_with("/api/runtime/components/")
        || request.uri().path().starts_with("/api/network/")
        || request.uri().path() == "/api/plugins"
        || request.uri().path().starts_with("/api/plugins/")
        || request.uri().path().starts_with("/api/projects")
        || request.uri().path() == "/api/project-selection"
        || (request.uri().path().starts_with("/api/tasks/")
            && request.uri().path().ends_with("/project"))
        || (request.uri().path().starts_with("/api/tasks/")
            && request.uri().path().ends_with("/network-calls"));
    if (authenticated_read
        || !matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD
        ))
        && headers.get("x-coda-token").and_then(|v| v.to_str().ok()) != Some(&app.token)
    {
        return denied("写请求令牌缺失或不正确");
    }
    let mut response = next.run(request).await;
    apply_security_headers(&mut response);
    response
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}
async fn javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
}
async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/style.css"),
    )
}
async fn health(State(app): State<App>) -> Result<Json<Value>> {
    let schema_version = app.engine.store.schema_version().map_err(read_error)?;
    Ok(Json(
        json!({"ok":true,"name":"Coda CLI harness","version":env!("CARGO_PKG_VERSION"),"runtime":"rust","schema_version":schema_version}),
    ))
}
async fn bootstrap(State(app): State<App>) -> Json<Value> {
    Json(json!({"token":app.token,"version":env!("CARGO_PKG_VERSION")}))
}

fn host_config_error_response(status: StatusCode, error: HostConfigError) -> Response {
    let mut response = (
        status,
        Json(json!({"error":{
            "code":error.code(),"message":error.message(),"retryable":false
        }})),
    )
        .into_response();
    apply_security_headers(&mut response);
    response
}
fn host_config_failure(error: HostConfigError) -> Response {
    let status = StatusCode::from_u16(error.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    host_config_error_response(status, error)
}

struct HostConfigJson<T>(T);
impl<S, T> FromRequest<S> for HostConfigJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned + Send,
{
    type Rejection = Response;
    async fn from_request(
        request: Request,
        state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        match Json::<StrictObject<T>>::from_request(request, state).await {
            Ok(Json(StrictObject(body))) => Ok(Self(body)),
            Err(error) => Err(host_config_error_response(
                json_rejection(error).status,
                HostConfigError::Invalid,
            )),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostConfigLookup {
    key: String,
}
async fn host_config(State(app): State<App>) -> Response {
    match app.engine.host_config() {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => host_config_failure(error),
    }
}
async fn host_config_lookup(
    State(app): State<App>,
    HostConfigJson(body): HostConfigJson<HostConfigLookup>,
) -> Response {
    if secrets::validate_idempotency_key(&body.key).is_err() || body.key.contains(',') {
        return host_config_failure(HostConfigError::Invalid);
    }
    match app.engine.host_config_operation(&body.key) {
        Ok(result) => Json(result).into_response(),
        Err(error) => host_config_failure(error),
    }
}
async fn configure_host_route(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    HostConfigJson(body): HostConfigJson<ConfigureHostRoute>,
) -> Response {
    let Ok(Path(id)) = id else {
        return host_config_failure(HostConfigError::Invalid);
    };
    let mut keys = headers.get_all("idempotency-key").iter();
    let key = keys.next().and_then(|value| value.to_str().ok());
    let Some(key) =
        key.filter(|key| !key.contains(',') && secrets::validate_idempotency_key(key).is_ok())
    else {
        return host_config_failure(HostConfigError::Invalid);
    };
    if keys.next().is_some() {
        return host_config_failure(HostConfigError::Invalid);
    }
    match app.engine.configure_host_route(&id, body, key).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => host_config_failure(error),
    }
}

fn plugin_failure(code: &str) -> Response {
    let (status, code, message) = match code {
        "plugin_invalid" => (StatusCode::BAD_REQUEST, "plugin_invalid", "插件请求无效"),
        "plugin_not_found" => (StatusCode::NOT_FOUND, "plugin_not_found", "插件不存在"),
        "plugin_revision_conflict" => (
            StatusCode::CONFLICT,
            "plugin_revision_conflict",
            "插件状态版本冲突",
        ),
        "plugin_idempotency_conflict" => (
            StatusCode::CONFLICT,
            "plugin_idempotency_conflict",
            "插件请求幂等键冲突",
        ),
        "plugin_unavailable" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "plugin_unavailable",
            "插件管理暂时不可用",
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "plugin_internal",
            "插件管理失败；写入结果可能已提交，请查询状态或使用原键和原参数手动回放",
        ),
    };
    (
        status,
        Json(json!({"code":code,"message":message,"error":message,"retryable":false})),
    )
        .into_response()
}

fn plugin_error(error: anyhow::Error) -> Response {
    plugin_failure(
        error
            .downcast_ref::<PluginManagementError>()
            .map_or("plugin_internal", |e| e.code()),
    )
}

async fn host_composition(
    State(app): State<App>,
) -> Result<Json<crate::engine::CompositionSnapshot>> {
    app.engine.composition().map(Json).map_err(read_error)
}

async fn plugins(State(app): State<App>) -> Response {
    match app.engine.plugins() {
        Ok(plugins) => Json(json!({"plugins":plugins})).into_response(),
        Err(error) => plugin_error(error),
    }
}

async fn plugin_status(State(app): State<App>, Path(id): Path<String>) -> Response {
    match app.engine.plugin_status(&id) {
        Ok(plugin) => Json(json!({"plugin":plugin})).into_response(),
        Err(error) => plugin_error(error),
    }
}

fn runtime_failure(error: crate::runtime_management::RuntimeManagementError) -> Response {
    use crate::runtime_management::RuntimeManagementError as E;
    let (status, code, message) = match error {
        E::Invalid => (
            StatusCode::BAD_REQUEST,
            "runtime_invalid",
            "Invalid runtime management request",
        ),
        E::NotFound => (
            StatusCode::NOT_FOUND,
            "runtime_not_found",
            "Runtime component not found",
        ),
        E::RevisionConflict => (
            StatusCode::CONFLICT,
            "runtime_revision_conflict",
            "Runtime revision conflict",
        ),
        E::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "runtime_idempotency_conflict",
            "Runtime idempotency conflict",
        ),
        E::Busy => (
            StatusCode::CONFLICT,
            "runtime_busy",
            "Runtime transition busy",
        ),
        E::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime_unavailable",
            "Runtime component unavailable",
        ),
        E::Internal => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime_internal",
            "Runtime management outcome unavailable",
        ),
    };
    (
        status,
        Json(json!({"code":code,"message":message,"error":message,"retryable":false})),
    )
        .into_response()
}

async fn runtime_empty_request(request: Request) -> bool {
    request.uri().query().is_none()
        && matches!(axum::body::to_bytes(request.into_body(), 0).await, Ok(body) if body.is_empty())
}

async fn runtime_components(State(app): State<App>, request: Request) -> Response {
    use crate::runtime_management::RuntimeManagementError as E;
    if !runtime_empty_request(request).await {
        return runtime_failure(E::Invalid);
    }
    match app.engine.runtime_components() {
        Ok(components) => Json(json!({"components":components})).into_response(),
        Err(error) => runtime_failure(error),
    }
}

async fn runtime_component(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    request: Request,
) -> Response {
    use crate::runtime_management::{RuntimeId, RuntimeManagementError as E};
    if !runtime_empty_request(request).await {
        return runtime_failure(E::Invalid);
    }
    let Ok(Path(id)) = id else {
        return runtime_failure(E::Invalid);
    };
    let id = match RuntimeId::parse(&id) {
        Ok(id) => id,
        Err(error) => return runtime_failure(error),
    };
    match app.engine.runtime_component_status(id) {
        Ok(component) => Json(component).into_response(),
        Err(error) => runtime_failure(error),
    }
}

async fn set_runtime_component(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    request: Request,
) -> Response {
    use crate::runtime_management::{RuntimeId, RuntimeManagementError as E};
    if request.uri().query().is_some() {
        return runtime_failure(E::Invalid);
    }
    let Ok(Path(id)) = id else {
        return runtime_failure(E::Invalid);
    };
    let id = match RuntimeId::parse(&id) {
        Ok(id) => id,
        Err(error) => return runtime_failure(error),
    };
    let key = match required_idempotency_key(request.headers()) {
        Ok(key) => key,
        Err(_) => return runtime_failure(E::Invalid),
    };
    let is_json = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
        });
    if !is_json {
        return runtime_failure(E::Invalid);
    }
    let Ok(body) = axum::body::to_bytes(request.into_body(), 4096).await else {
        return runtime_failure(E::Invalid);
    };
    let Ok(StrictObject(body)) = serde_json::from_slice::<StrictObject<SetPluginEnabled>>(&body)
    else {
        return runtime_failure(E::Invalid);
    };
    if body.expected_revision > i64::MAX as u64 {
        return runtime_failure(E::Invalid);
    }
    match app.engine.set_runtime_enabled(id, body, &key) {
        Ok(result) => Json(result).into_response(),
        Err(error) => runtime_failure(error),
    }
}

async fn agent_runtime_status(State(app): State<App>, RawQuery(query): RawQuery) -> Response {
    if query.is_some() {
        return ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("Agent运行态查询不接受参数"),
        )
        .into_response();
    }
    match app.engine.builtin_agent_status() {
        Ok(status) => Json(status).into_response(),
        Err(_) => {
            let message = "Agent运行态不可用";
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "code": "agent_runtime_unavailable",
                    "message": message,
                    "retryable": false,
                    "error": message
                })),
            )
                .into_response()
        }
    }
}

async fn set_plugin_enabled(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ContractJson(StrictObject(request)): ContractJson<StrictObject<SetPluginEnabled>>,
) -> Response {
    let key = match required_idempotency_key(&headers) {
        Ok(key) => key,
        Err(_) => return plugin_failure("plugin_invalid"),
    };
    match app.engine.set_plugin_enabled(&id, request, &key) {
        Ok(result) => Json(result).into_response(),
        Err(error) => plugin_error(error),
    }
}

async fn settings(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app
        .engine
        .store
        .settings()
        .map_err(read_error)?
        .ok_or_else(|| anyhow::anyhow!("缺少设置"))?;
    let keys: HashMap<_, _> = settings
        .routes
        .iter()
        .map(|r| (r.id.clone(), app.engine.key(r).is_ok()))
        .collect();
    Ok(Json(
        json!({"settings":settings,"keys":keys,"newapi_key":app.engine.store.secret("newapi-account")?.is_some()}),
    ))
}

#[derive(Deserialize)]
struct SaveSettings {
    settings: Settings,
    #[serde(default)]
    keys: HashMap<String, String>,
}
async fn save_settings(
    State(app): State<App>,
    ContractJson(body): ContractJson<SaveSettings>,
) -> Result<Json<Value>> {
    if body.keys.contains_key("newapi-account") {
        return Err(anyhow::anyhow!("请使用账户绑定入口设置账号令牌").into());
    }
    app.engine
        .configure_with_keys(body.settings, body.keys)
        .await?;
    Ok(Json(json!({"ok":true})))
}

async fn network_get_moved() -> Response {
    let mut response = ApiError::with_code(
        StatusCode::METHOD_NOT_ALLOWED,
        ErrorCode::RequestFailed,
        false,
        anyhow::anyhow!("此接口需要使用带令牌的POST显式请求网络访问"),
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteProjectBody {
    project_id: Option<ProjectId>,
}
async fn models(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<RouteProjectBody>>,
) -> Response {
    let id = match workspace_task_id(id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let result = match body.project_id {
        Some(project) => app.engine.project_route_models(&project, &id).await,
        None => app.engine.route_models(&id).await,
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => network_error(error),
    }
}
async fn check(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<RouteProjectBody>>,
) -> Response {
    let id = match workspace_task_id(id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let result = match body.project_id {
        Some(project) => app.engine.project_route_check(&project, &id).await,
        None => app.engine.route_check(&id).await,
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => network_error(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bind {
    account: BindAccount,
    token: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindAccount {
    base_url: String,
    user_id: String,
    #[serde(default = "default_quota_unit")]
    quota_per_unit: f64,
}
fn default_quota_unit() -> f64 {
    500_000.0
}
impl From<BindAccount> for Account {
    fn from(value: BindAccount) -> Self {
        Self {
            base_url: value.base_url,
            user_id: value.user_id,
            quota_per_unit: value.quota_per_unit,
        }
    }
}
async fn account(
    State(app): State<App>,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<Bind>>,
) -> Response {
    match app
        .engine
        .bind_account(body.account.into(), body.token)
        .await
    {
        Ok(()) => Json(json!({"ok":true,"network_authorization_required":true})).into_response(),
        Err(error) => network_error(error),
    }
}
async fn balance(State(app): State<App>, ContractJson(_): ContractJson<EmptyBody>) -> Response {
    match app.engine.account_balance().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => network_error(error),
    }
}

fn network_error(error: NetworkError) -> Response {
    if let Some(code) = error.kind.budget_code() {
        let status = match code {
            "budget_invalid" => StatusCode::BAD_REQUEST,
            "budget_not_found" => StatusCode::NOT_FOUND,
            "budget_corrupt" | "budget_internal" => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::CONFLICT,
        };
        return (status, Json(json!({"error":{"code":code,"message":"预算操作被拒绝，请查询项目预算与调用诊断","dispatch_state":error.dispatch_state,"calls":error.calls,"retryable":false}}))).into_response();
    }
    if let Some(code) = error.kind.project_code() {
        return (project_status(code), Json(json!({"error":{"code":code,"message":"项目网络操作被拒绝，请检查项目状态与权限","dispatch_state":error.dispatch_state,"calls":error.calls,"retryable":false}}))).into_response();
    }
    let (status, code, message) = match error.kind {
        NetworkErrorKind::Invalid => (StatusCode::BAD_REQUEST, "invalid", "网络请求无效"),
        NetworkErrorKind::NotFound => (StatusCode::NOT_FOUND, "not_found", "网络对象不存在"),
        NetworkErrorKind::Required => (StatusCode::CONFLICT, "required", "需要明确网络授权"),
        NetworkErrorKind::Stale => (
            StatusCode::CONFLICT,
            "stale",
            "网络授权版本已失效，请重新查看并确认",
        ),
        NetworkErrorKind::Revoked => (StatusCode::CONFLICT, "revoked", "网络授权已撤销"),
        NetworkErrorKind::Conflict => (StatusCode::CONFLICT, "conflict", "网络授权绑定冲突"),
        NetworkErrorKind::Corrupt => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "corrupt",
            "网络持久状态损坏",
        ),
        NetworkErrorKind::Unknown => (
            StatusCode::CONFLICT,
            "unknown",
            "网络调用结果未知，请查询回执，不要自动重试",
        ),
        NetworkErrorKind::Internal => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "网络服务处理失败",
        ),
        NetworkErrorKind::InvalidToolArguments => (
            StatusCode::BAD_GATEWAY,
            "invalid_tool_arguments",
            "模型返回了无效的工具参数 JSON",
        ),
        NetworkErrorKind::Upstream => (StatusCode::BAD_GATEWAY, "upstream", "上游请求未成功完成"),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "网络服务处理失败",
        ),
    };
    (status,Json(json!({"error":{"code":code,"message":message,"dispatch_state":error.dispatch_state,"calls":error.calls,"retryable":false}}))).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkConfirmBody {
    subject: NetworkSubject,
    config_revision: u64,
    credential_revision: u64,
    binding_digest: String,
    purposes: Vec<crate::provider_transport::NetworkPurpose>,
}
async fn network_preview(
    State(app): State<App>,
    ContractJson(StrictObject(subject)): ContractJson<StrictObject<NetworkSubject>>,
) -> Response {
    match app.engine.network_preview(subject).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => network_error(error),
    }
}
async fn network_confirm(
    State(app): State<App>,
    headers: HeaderMap,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<NetworkConfirmBody>>,
) -> Response {
    let key = match required_idempotency_key(&headers) {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    let request = ConfirmRequest {
        config_revision: body.config_revision,
        credential_revision: body.credential_revision,
        binding_digest: body.binding_digest,
        purposes: body.purposes,
    };
    match app
        .engine
        .network_confirm(body.subject, request, &key)
        .await
    {
        Ok(value) => (
            if value.replayed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            },
            Json(value),
        )
            .into_response(),
        Err(error) => network_error(error),
    }
}
async fn network_authorization(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Response {
    let id = match workspace_task_id(id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    match app.engine.network_authorization(&id) {
        Ok(value) => Json(value).into_response(),
        Err(error) => network_error(error),
    }
}
async fn network_revoke(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    ContractJson(_): ContractJson<EmptyBody>,
) -> Response {
    let id = match workspace_task_id(id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    match app.engine.network_revoke(&id).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => network_error(error),
    }
}
async fn network_call(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Response {
    let id = match workspace_task_id(id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    match app.engine.network_call(&id) {
        Ok(value) => Json(value).into_response(),
        Err(error) => network_error(error),
    }
}
async fn task_network_calls(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Response {
    let id = match workspace_task_id(id) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    match app.engine.network_calls_for_task(&id) {
        Ok(value) => Json(json!({"calls":value})).into_response(),
        Err(error) => network_error(error),
    }
}

async fn runs(State(app): State<App>, RawQuery(query): RawQuery) -> Result<Json<Value>> {
    if let Some(id) = project_query(query)? {
        let runs = app.engine.project_runs(&id).map_err(read_error)?;
        return Ok(Json(json!(
            runs.into_iter()
                .map(|r| json!({"id":r.id,"title":r.title,"kind":r.kind,"created_at":r.created_at}))
                .collect::<Vec<_>>()
        )));
    }
    Ok(Json(json!(
        app.engine.sessions().runs().map_err(read_error)?
    )))
}
fn project_query(query: Option<String>) -> Result<Option<ProjectId>> {
    let mut project = None;
    for pair in query
        .as_deref()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key)?;
        let value = percent_decode(value)?;
        if key != "project_id" || project.is_some() {
            return Err(anyhow::anyhow!("项目查询参数无效").into());
        }
        secrets::validate_persisted_id("project_id", &value)?;
        project = Some(ProjectId(value));
    }
    Ok(project)
}

async fn projects(State(app): State<App>) -> Result<Json<Vec<Project>>> {
    Ok(Json(app.engine.projects().list().map_err(read_error)?))
}

fn worktree_failure(error: &WorktreeError) -> Response {
    let (status, code) = match error {
        WorktreeError::Invalid => (StatusCode::BAD_REQUEST, "worktree_invalid"),
        WorktreeError::NotFound => (StatusCode::NOT_FOUND, "worktree_not_found"),
        WorktreeError::RevisionConflict => (StatusCode::CONFLICT, "worktree_revision_conflict"),
        WorktreeError::IdempotencyConflict => {
            (StatusCode::CONFLICT, "worktree_idempotency_conflict")
        }
        WorktreeError::Busy => (StatusCode::CONFLICT, "worktree_busy"),
        WorktreeError::RootChanged => (StatusCode::CONFLICT, "worktree_root_changed"),
        WorktreeError::Conflict => (StatusCode::CONFLICT, "worktree_conflict"),
        WorktreeError::Unsupported => (StatusCode::UNPROCESSABLE_ENTITY, "worktree_unsupported"),
        WorktreeError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "worktree_unavailable"),
        WorktreeError::Corrupt => (StatusCode::INTERNAL_SERVER_ERROR, "worktree_corrupt"),
    };
    (
        status,
        Json(json!({"code":code,"message":code,"error":code,"retryable":false})),
    )
        .into_response()
}
fn worktree_command_error(error: anyhow::Error) -> ApiError {
    let typed = error
        .downcast_ref::<WorktreeError>()
        .copied()
        .unwrap_or(WorktreeError::Corrupt);
    ApiError::from(anyhow::Error::from(typed))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWorktreeBody {
    name: String,
    parent_path: String,
    expected_config_revision: u64,
    expected_head: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupWorktreeBody {
    idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectWorktreeBody {}

fn worktree_operation_id(id: String) -> Result<String> {
    if !uuid::Uuid::parse_str(&id).is_ok_and(|u| u.to_string() == id) {
        return Err(anyhow::Error::from(WorktreeError::Invalid).into());
    }
    Ok(id)
}
async fn create_worktree(
    State(app): State<App>,
    source: std::result::Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<CreateWorktreeBody>>,
) -> Result<Response> {
    let receipt = app
        .engine
        .create_project_worktree(CreateWorktree {
            source_project_id: ProjectId(workspace_task_id(source)?),
            name: body.name,
            parent_path: body.parent_path,
            expected_config_revision: body.expected_config_revision,
            expected_head: body.expected_head,
            idempotency_key: required_idempotency_key(&headers)?,
        })
        .await
        .map_err(worktree_command_error)?;
    let status = match receipt.state {
        WorktreeState::Succeeded => StatusCode::OK,
        WorktreeState::Claimed | WorktreeState::Unknown => StatusCode::ACCEPTED,
        WorktreeState::Rejected => StatusCode::CONFLICT,
    };
    Ok((status, Json(receipt)).into_response())
}
async fn lookup_worktree(
    State(app): State<App>,
    source: std::result::Result<Path<String>, PathRejection>,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<LookupWorktreeBody>>,
) -> Result<Response> {
    let source = ProjectId(workspace_task_id(source)?);
    secrets::validate_idempotency_key(&body.idempotency_key)
        .map_err(|_| anyhow::Error::from(WorktreeError::Invalid))?;
    Ok(Json(
        app.engine
            .lookup_project_worktree(&source, &body.idempotency_key)
            .map_err(worktree_command_error)?,
    )
    .into_response())
}
async fn worktree_status(
    State(app): State<App>,
    path: std::result::Result<Path<(String, String)>, PathRejection>,
) -> Result<Response> {
    let Path((source, op)) = path.map_err(|_| anyhow::Error::from(WorktreeError::Invalid))?;
    secrets::validate_persisted_id("project_id", &source)
        .map_err(|_| anyhow::Error::from(WorktreeError::Invalid))?;
    let op = worktree_operation_id(op)?;
    Ok(Json(
        app.engine
            .worktree_receipt(&ProjectId(source), &op)
            .map_err(worktree_command_error)?,
    )
    .into_response())
}
async fn inspect_worktree(
    State(app): State<App>,
    path: std::result::Result<Path<(String, String)>, PathRejection>,
    ContractJson(StrictObject(_)): ContractJson<StrictObject<InspectWorktreeBody>>,
) -> Result<Response> {
    let Path((source, op)) = path.map_err(|_| anyhow::Error::from(WorktreeError::Invalid))?;
    secrets::validate_persisted_id("project_id", &source)
        .map_err(|_| anyhow::Error::from(WorktreeError::Invalid))?;
    let op = worktree_operation_id(op)?;
    Ok(Json(
        app.engine
            .inspect_project_worktree(&ProjectId(source), &op)
            .await
            .map_err(worktree_command_error)?,
    )
    .into_response())
}

fn project_status(code: &str) -> StatusCode {
    match code {
        "project_invalid" => StatusCode::BAD_REQUEST,
        "project_not_found" => StatusCode::NOT_FOUND,
        "project_internal" | "project_corrupt" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::CONFLICT,
    }
}
fn project_failure(code: &str, message: &str) -> Response {
    let message = secrets::scrub(message, "");
    (
        project_status(code),
        Json(json!({"code":code,"message":message,"error":message,"retryable":false})),
    )
        .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterProjectBody {
    name: String,
    root_path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureProjectBody {
    expected_revision: u64,
    #[serde(deserialize_with = "strict_project_config")]
    config: ProjectConfig,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectProjectBody {
    project_id: ProjectId,
    expected_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartProjectBody {
    expected_config_revision: u64,
    #[serde(deserialize_with = "strict_project_run")]
    request: RunRequest,
}
fn object_fields<'a, E: serde::de::Error>(
    value: &'a Value,
    allowed: &[&str],
) -> std::result::Result<&'a serde_json::Map<String, Value>, E> {
    let object = value
        .as_object()
        .ok_or_else(|| E::custom("JSON object required"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(E::custom("unknown object field"));
    }
    Ok(object)
}
fn strict_project_config<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<ProjectConfig, D::Error> {
    let value = Value::deserialize(d)?;
    let fields = [
        "allowed_route_ids",
        "default_route_id",
        "tools",
        "write_scopes",
        "allow_commands",
    ];
    let object = object_fields::<D::Error>(&value, &fields)?;
    if object.len() != fields.len() {
        return Err(serde::de::Error::custom("complete project config required"));
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}
fn strict_project_run<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<RunRequest, D::Error> {
    let value = Value::deserialize(d)?;
    let object = object_fields::<D::Error>(&value, &["title", "kind", "tasks"])?;
    let tasks = object
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or_else(|| serde::de::Error::custom("tasks array required"))?;
    for task in tasks {
        object_fields::<D::Error>(
            task,
            &[
                "name",
                "role",
                "route_id",
                "prompt",
                "depends_on",
                "write_scopes",
                "tools",
                "allow_commands",
                "max_rounds",
            ],
        )?;
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}
async fn register_project(
    State(app): State<App>,
    headers: HeaderMap,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<RegisterProjectBody>>,
) -> Result<(StatusCode, Json<Project>)> {
    let idempotency_key = required_idempotency_key(&headers)?;
    let project = app
        .engine
        .register_project(RegisterProject {
            name: body.name,
            root_path: body.root_path,
            idempotency_key,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(project)))
}
async fn project(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Response> {
    let id = ProjectId(workspace_task_id(id)?);
    match app.engine.projects().get(&id) {
        Ok(project) => Ok(Json(project).into_response()),
        Err(error) if is_missing_row(&error) => {
            Ok(project_failure("project_not_found", "项目不存在"))
        }
        Err(error) => Err(read_error(error)),
    }
}

fn budget_failure(error: BudgetError) -> Response {
    let (status, message) = match error.kind {
        BudgetErrorKind::Invalid => (StatusCode::BAD_REQUEST, "预算请求无效"),
        BudgetErrorKind::NotFound => (StatusCode::NOT_FOUND, "预算对象不存在"),
        BudgetErrorKind::RevisionConflict => {
            (StatusCode::CONFLICT, "预算配置版本已变化，请重新查询")
        }
        BudgetErrorKind::Conflict => (StatusCode::CONFLICT, "预算幂等请求或持久回执冲突"),
        BudgetErrorKind::Busy => (
            StatusCode::CONFLICT,
            "项目仍有未结束调用，暂不能首次配置预算",
        ),
        BudgetErrorKind::Exceeded => (StatusCode::CONFLICT, "预算不足以预约下一次模型调用"),
        BudgetErrorKind::Unresolved => (StatusCode::CONFLICT, "预算存在未决用量，请查询调用诊断"),
        BudgetErrorKind::Overrun => (StatusCode::CONFLICT, "实际用量已超过预约，后续调用已阻止"),
        BudgetErrorKind::Corrupt => (StatusCode::INTERNAL_SERVER_ERROR, "预算持久状态损坏"),
        BudgetErrorKind::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "预算操作失败"),
    };
    budget_error_response(status, error.kind.code(), message)
}
fn budget_error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error":{"code":code,"message":message,"dispatch_state":"not_sent","calls":[],"retryable":false}}))).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureBudgetBody {
    expected_revision: u64,
    idempotency_key: String,
    config: BudgetConfig,
}
struct BudgetJson(ConfigureBudgetBody);
impl<S: Send + Sync> FromRequest<S> for BudgetJson {
    type Rejection = Response;
    async fn from_request(
        request: Request,
        state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        match Json::<StrictObject<ConfigureBudgetBody>>::from_request(request, state).await {
            Ok(Json(StrictObject(body))) => Ok(Self(body)),
            Err(error) => {
                let rejection = json_rejection(error);
                let message = match rejection.status {
                    StatusCode::UNSUPPORTED_MEDIA_TYPE => {
                        "预算接口需要 application/json Content-Type"
                    }
                    StatusCode::PAYLOAD_TOO_LARGE => "预算请求体超过 1 MiB 限制",
                    _ => "预算请求无效",
                };
                Err(budget_error_response(
                    rejection.status,
                    "budget_invalid",
                    message,
                ))
            }
        }
    }
}
fn budget_id(
    id: std::result::Result<Path<String>, PathRejection>,
) -> std::result::Result<ProjectId, BudgetError> {
    let Path(id) = id.map_err(|_| BudgetError::from(BudgetErrorKind::Invalid))?;
    secrets::validate_persisted_id("project", &id)
        .map_err(|_| BudgetError::from(BudgetErrorKind::Invalid))?;
    if matches!(id.as_str(), "." | "..") {
        return Err(BudgetErrorKind::Invalid.into());
    }
    Ok(ProjectId(id))
}
async fn budget_config(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Response {
    match budget_id(id).and_then(|id| app.engine.budget_config(&id)) {
        Ok(value) => Json(value).into_response(),
        Err(error) => budget_failure(error),
    }
}
async fn configure_budget(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    BudgetJson(body): BudgetJson,
) -> Response {
    let result = budget_id(id).and_then(|project_id| {
        if body.expected_revision >= i64::MAX as u64
            || secrets::validate_idempotency_key(&body.idempotency_key).is_err()
            || body.idempotency_key.contains(',')
        {
            return Err(BudgetErrorKind::Invalid.into());
        }
        body.config.validate()?;
        app.engine.configure_budget(ConfigureBudget {
            project_id,
            expected_revision: body.expected_revision,
            idempotency_key: body.idempotency_key,
            config: body.config,
        })
    });
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => budget_failure(error),
    }
}
async fn budget_status(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Response {
    match budget_id(id).and_then(|id| app.engine.budget_status(&id)) {
        Ok(value) => Json(value).into_response(),
        Err(error) => budget_failure(error),
    }
}
async fn budget_call(
    State(app): State<App>,
    path: std::result::Result<Path<(String, String)>, PathRejection>,
) -> Response {
    let result = (|| {
        let Path((project, call)) =
            path.map_err(|_| BudgetError::from(BudgetErrorKind::Invalid))?;
        let id = budget_id(Ok(Path(project)))?;
        secrets::validate_persisted_id("call", &call)
            .map_err(|_| BudgetError::from(BudgetErrorKind::Invalid))?;
        if matches!(call.as_str(), "." | "..") {
            return Err(BudgetErrorKind::Invalid.into());
        }
        app.engine.budget_call(&id, &call)
    })();
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => budget_failure(error),
    }
}

async fn project_config(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<ProjectConfigSnapshot>> {
    app.engine
        .project_config(&ProjectId(workspace_task_id(id)?))
        .map(Json)
        .map_err(read_error)
}
async fn configure_project(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<ConfigureProjectBody>>,
) -> Result<Json<ProjectConfigSnapshot>> {
    Ok(Json(
        app.engine
            .configure_project(ConfigureProject {
                project_id: ProjectId(workspace_task_id(id)?),
                expected_revision: body.expected_revision,
                config: body.config,
            })
            .await?,
    ))
}
async fn project_selection(State(app): State<App>) -> Result<Json<ProjectSelection>> {
    app.engine.current_project().map(Json).map_err(read_error)
}
async fn select_project(
    State(app): State<App>,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<SelectProjectBody>>,
) -> Result<Json<ProjectSelection>> {
    Ok(Json(
        app.engine
            .select_project(SelectProject {
                project_id: body.project_id,
                expected_revision: body.expected_revision,
            })
            .await?,
    ))
}
async fn start_project(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<StartProjectBody>>,
) -> Result<Json<Run>> {
    Ok(Json(
        app.engine
            .start_in_project(StartInProject {
                project_id: ProjectId(workspace_task_id(id)?),
                expected_config_revision: body.expected_config_revision,
                request: body.request,
                idempotency_key: required_idempotency_key(&headers)?,
            })
            .await?,
    ))
}
async fn task_project(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Value>> {
    let id = workspace_task_id(id)?;
    app.engine.sessions().task(&id).map_err(read_error)?;
    let binding = app.engine.task_project_binding(&id).map_err(read_error)?;
    Ok(Json(
        json!({"status":if binding.is_some() {"bound"} else {"legacy_unbound"},"binding":binding}),
    ))
}
async fn session(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Session>> {
    let id = object_id(id)?;
    app.engine
        .sessions()
        .session(&SessionId(id))
        .map(Json)
        .map_err(read_error)
}
async fn session_turns(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Vec<Turn>>> {
    let id = object_id(id)?;
    let id = SessionId(id);
    app.engine.sessions().session(&id).map_err(read_error)?;
    Ok(Json(
        app.engine
            .sessions()
            .turns_for_session(&id)
            .map_err(read_error)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatTurnBody {
    agent_id: String,
    expected_last_turn_id: String,
    message: String,
}

fn bad_turn_request(message: &'static str) -> ApiError {
    ApiError::with_code(
        StatusCode::BAD_REQUEST,
        ErrorCode::RequestFailed,
        false,
        anyhow::anyhow!(message),
    )
}

fn required_idempotency_key(headers: &HeaderMap) -> Result<String> {
    let mut values = headers.get_all("idempotency-key").iter();
    let Some(first) = values.next() else {
        return Err(bad_turn_request("缺少 Idempotency-Key 请求头"));
    };
    if values.next().is_some() {
        return Err(bad_turn_request("Idempotency-Key 只能提供一个"));
    }
    let value = first
        .to_str()
        .map_err(|_| bad_turn_request("Idempotency-Key 必须是可见 ASCII 文本"))?;
    if value.contains(',') {
        return Err(bad_turn_request("Idempotency-Key 不能包含逗号"));
    }
    if secrets::validate_idempotency_key(value).is_err() {
        return Err(bad_turn_request("Idempotency-Key 无效"));
    }
    Ok(value.to_owned())
}

fn turn_command_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<ProjectError>().is_some()
        || error.downcast_ref::<NetworkError>().is_some()
        || error.downcast_ref::<WorktreeError>().is_some()
    {
        return classify_anyhow(error);
    }
    if error.is::<NetworkError>() {
        return ApiError::from(error);
    }
    if error.is::<crate::store::IdempotencyConflict>() {
        return ApiError::with_code(
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
            false,
            anyhow::anyhow!("同一 Idempotency-Key 已绑定不同的连续对话请求"),
        );
    }
    if let Some(typed) = error.downcast_ref::<ChatTurnError>() {
        let (status, code) = match typed {
            ChatTurnError::InvalidInput | ChatTurnError::UnsupportedSession => {
                (StatusCode::BAD_REQUEST, ErrorCode::RequestFailed)
            }
            ChatTurnError::NotFound => (StatusCode::NOT_FOUND, ErrorCode::NotFound),
            ChatTurnError::StalePredecessor
            | ChatTurnError::SessionBusy
            | ChatTurnError::UnresolvedEffects
            | ChatTurnError::PredecessorChanged => (StatusCode::CONFLICT, ErrorCode::Conflict),
            ChatTurnError::CorruptState => (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal),
        };
        return ApiError::with_code(status, code, false, anyhow::anyhow!("{typed}"));
    }
    if is_transient_store(&error) {
        return ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            true,
            anyhow::anyhow!("存储暂时不可用"),
        );
    }
    ApiError::with_code(
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
        false,
        anyhow::anyhow!("连续对话请求处理失败"),
    )
}

async fn post_session_turn(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    ContractJson(body): ContractJson<ChatTurnBody>,
) -> Result<Response> {
    let session_id = object_id(id)?;
    for (field, value) in [
        ("session_id", session_id.as_str()),
        ("agent_id", body.agent_id.as_str()),
        ("expected_last_turn_id", body.expected_last_turn_id.as_str()),
    ] {
        if secrets::validate_persisted_id(field, value).is_err() {
            return Err(bad_turn_request("标识字段无效"));
        }
    }
    if body.message.trim().is_empty() || body.message.len() > 100_000 {
        return Err(bad_turn_request("消息为空或超过 100000 字节"));
    }
    let key = required_idempotency_key(&headers)?;
    let receipt = app
        .engine
        .send_chat_turn(SendChatTurn {
            session_id: SessionId(session_id),
            agent_id: AgentId(body.agent_id),
            expected_last_turn_id: TurnId(body.expected_last_turn_id),
            message: body.message,
            idempotency_key: key,
        })
        .await
        .map_err(turn_command_error)?;
    let status = if receipt.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(json!({
            "turn": receipt.turn,
            "task": receipt.task,
            "replayed": receipt.replayed,
        })),
    )
        .into_response())
}
async fn turn(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Turn>> {
    let id = object_id(id)?;
    app.engine
        .sessions()
        .turn(&TurnId(id))
        .map(Json)
        .map_err(read_error)
}
fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| anyhow::anyhow!("Idempotency-Key 必须是 ASCII 文本"))?;
    secrets::validate_idempotency_key(value)?;
    Ok(Some(value.to_owned()))
}
async fn start(
    State(app): State<App>,
    headers: HeaderMap,
    ContractJson(body): ContractJson<RunRequest>,
) -> Result<Json<Run>> {
    let run = match idempotency_key(&headers)? {
        Some(key) => app.engine.start_idempotent(body, &key).await?,
        None => app.engine.start(body).await?,
    };
    Ok(Json(run))
}
async fn run(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Run>> {
    let id = object_id(id)?;
    app.engine.sessions().run(&id).map(Json).map_err(read_error)
}
async fn run_context(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<RunContext>> {
    let id = object_id(id)?;
    if secrets::validate_persisted_id("run_id", &id).is_err() {
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("运行标识无效"),
        ));
    }
    app.engine
        .sessions()
        .run_context(&id)
        .map(Json)
        .map_err(context_error)
}
fn context_error(error: anyhow::Error) -> ApiError {
    if let Some(typed) = error.downcast_ref::<RunContextError>() {
        let (status, code, message) = match typed {
            RunContextError::NotFound => (StatusCode::NOT_FOUND, ErrorCode::NotFound, "运行不存在"),
            RunContextError::Unmapped => (
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "旧运行没有领域身份映射",
            ),
            RunContextError::CorruptState => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "运行身份数据损坏",
            ),
        };
        return ApiError::with_code(status, code, false, anyhow::anyhow!(message));
    }
    let retryable = is_transient_store(&error);
    ApiError::with_code(
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
        retryable,
        anyhow::anyhow!(if retryable {
            "存储暂时不可用"
        } else {
            "运行身份读取失败"
        }),
    )
}

#[derive(Deserialize)]
struct Resume {
    message: String,
}
async fn resume(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    ContractJson(body): ContractJson<Resume>,
) -> Result<Json<Task>> {
    let id = object_id(id)?;
    let task = app
        .engine
        .resume(&id, &body.message)
        .await
        .map_err(|error| {
            if matches!(
                error.downcast_ref::<crate::store::ResumeError>(),
                Some(crate::store::ResumeError::Sealed)
            ) {
                ApiError::with_code(
                    StatusCode::CONFLICT,
                    ErrorCode::Conflict,
                    false,
                    anyhow::anyhow!("任务已有恢复记录，不能继续原任务"),
                )
            } else if matches!(
                error.downcast_ref::<crate::checkpoint::CheckpointError>(),
                Some(crate::checkpoint::CheckpointError::Conflict)
            ) {
                ApiError::with_code(
                    StatusCode::CONFLICT,
                    ErrorCode::Conflict,
                    false,
                    anyhow::anyhow!("任务已有检查点或恢复记录，不能继续原任务"),
                )
            } else {
                ApiError::from(error)
            }
        })?;
    Ok(Json(task))
}
async fn cancel(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Value>> {
    let id = object_id(id)?;
    app.engine.cancel(&id)?;
    Ok(Json(json!({"ok":true})))
}

#[derive(Serialize)]
struct ApprovalDto {
    id: String,
    task_id: String,
    tool_call_id: String,
    tool_name: String,
    preview: String,
    session_id: Option<String>,
    turn_id: Option<String>,
    decided_by: Option<String>,
    status: approval::ApprovalStatus,
    execution_state: approval::ExecutionState,
    created_at: u64,
    decided_at: Option<u64>,
    network_notice: Option<approval::CommandNetworkNotice>,
    command_network_ack: approval::CommandNetworkAck,
}
impl From<approval::ApprovalRecord> for ApprovalDto {
    fn from(record: approval::ApprovalRecord) -> Self {
        Self {
            id: record.id,
            task_id: record.task_id,
            tool_call_id: record.tool_call_id,
            tool_name: record.tool_name,
            preview: record.preview,
            session_id: record.session_id,
            turn_id: record.turn_id,
            decided_by: record.decided_by,
            status: record.status,
            execution_state: record.execution_state,
            created_at: record.created_at,
            decided_at: record.decided_at,
            network_notice: record.network_notice,
            command_network_ack: record.command_network_ack,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApprovalDecision {
    Approve,
    Deny,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalDecisionBody {
    decision: ApprovalDecision,
    #[serde(default)]
    acknowledge_command_network: bool,
}

fn approval_id(path: std::result::Result<Path<String>, PathRejection>) -> Result<String> {
    let id = object_id(path)?;
    if secrets::validate_persisted_id("approval_id", &id).is_err() {
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("审批标识无效"),
        ));
    }
    Ok(id)
}

fn approval_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<ProjectError>().is_some()
        || error.downcast_ref::<NetworkError>().is_some()
        || error.downcast_ref::<WorktreeError>().is_some()
    {
        return classify_anyhow(error);
    }
    if let Some(typed) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<approval::ApprovalError>())
    {
        let (status, code, message) = match typed {
            approval::ApprovalError::InvalidNetworkAcknowledgement { .. } => (
                StatusCode::BAD_REQUEST,
                ErrorCode::RequestFailed,
                "此审批不接受命令联网确认",
            ),
            approval::ApprovalError::NetworkAcknowledgementRequired { .. } => (
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "此命令可能访问任意网络，当前没有OS出站沙箱；请显式确认",
            ),
            approval::ApprovalError::NotFound { .. } => {
                (StatusCode::NOT_FOUND, ErrorCode::NotFound, "审批不存在")
            }
            approval::ApprovalError::Conflict { .. } => (
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "审批当前状态不能执行此操作",
            ),
            approval::ApprovalError::BindingConflict { .. } => {
                (StatusCode::CONFLICT, ErrorCode::Conflict, "审批绑定不匹配")
            }
            approval::ApprovalError::UnknownResult { .. } => (
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "工具执行结果未知",
            ),
            approval::ApprovalError::CorruptState { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "审批持久状态损坏",
            ),
        };
        return ApiError::with_code(status, code, false, anyhow::anyhow!(message));
    }
    let retryable = is_transient_store(&error);
    ApiError::with_code(
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
        retryable,
        anyhow::anyhow!(if retryable {
            "存储暂时不可用"
        } else {
            "审批请求处理失败"
        }),
    )
}

async fn task_approvals(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Value>> {
    let task_id = approval_id(id)?;
    app.engine.sessions().task(&task_id).map_err(read_error)?;
    let approvals: Vec<ApprovalDto> = app
        .engine
        .approvals()
        .for_task(&task_id)
        .map_err(approval_error)?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(Json(json!({"approvals":approvals})))
}

async fn approval(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<ApprovalDto>> {
    let id = approval_id(id)?;
    app.engine
        .approvals()
        .get(&id)
        .map(ApprovalDto::from)
        .map(Json)
        .map_err(approval_error)
}

async fn decide_approval(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    ContractJson(StrictObject(body)): ContractJson<StrictObject<ApprovalDecisionBody>>,
) -> Result<Json<ApprovalDto>> {
    let id = approval_id(id)?;
    let approved = matches!(body.decision, ApprovalDecision::Approve);
    if body.acknowledge_command_network {
        let approval = app.engine.approvals().get(&id).map_err(approval_error)?;
        if approval.tool_name != "run_command" || !approved {
            return Err(ApiError::with_code(
                StatusCode::BAD_REQUEST,
                ErrorCode::RequestFailed,
                false,
                anyhow::anyhow!("此审批不接受命令联网确认"),
            ));
        }
    }
    app.engine
        .decide_approval_with_network_ack(
            &id,
            approved,
            Some("user"),
            body.acknowledge_command_network,
        )
        .map(ApprovalDto::from)
        .map(Json)
        .map_err(approval_error)
}

struct GitDiffBody {
    path: String,
    view: GitDiffView,
}
impl<'de> Deserialize<'de> for GitDiffBody {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct BodyVisitor;
        impl<'de> serde::de::Visitor<'de> for BodyVisitor {
            type Value = GitDiffBody;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Git diff request object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let (mut path, mut view) = (None, None);
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "path" if path.is_none() => path = Some(map.next_value()?),
                        "view" if view.is_none() => view = Some(map.next_value()?),
                        _ => {
                            return Err(serde::de::Error::custom(
                                "unknown or duplicate Git diff field",
                            ));
                        }
                    }
                }
                Ok(GitDiffBody {
                    path: path.ok_or_else(|| serde::de::Error::missing_field("path"))?,
                    view: view.ok_or_else(|| serde::de::Error::missing_field("view"))?,
                })
            }
        }
        deserializer.deserialize_map(BodyVisitor)
    }
}

struct EmptyBody;
impl<'de> Deserialize<'de> for EmptyBody {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if value.as_object().is_some_and(|object| object.is_empty()) {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom("expected an empty JSON object"))
        }
    }
}

async fn git_diff(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    ContractJson(body): ContractJson<GitDiffBody>,
) -> Result<Json<Value>> {
    let id = workspace_task_id(id)?;
    let diff = app
        .engine
        .git_diff(
            &id,
            GitDiffRequest {
                path: body.path,
                view: body.view,
            },
        )
        .await
        .map_err(git_diff_error)?;
    Ok(Json(json!({"diff":diff})))
}

async fn create_checkpoint(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    ContractJson(_body): ContractJson<EmptyBody>,
) -> Result<Response> {
    let id = workspace_task_id(id)?;
    let key = required_idempotency_key(&headers)?;
    let created = app
        .engine
        .create_checkpoint(&id, &key)
        .await
        .map_err(checkpoint_error)?;
    let status = if created.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(created)).into_response())
}

async fn checkpoint(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Value>> {
    let id = checkpoint_id(id)?;
    let checkpoint = app.engine.checkpoint(&id).map_err(checkpoint_error)?;
    Ok(Json(json!({"checkpoint":checkpoint})))
}

fn checkpoint_id(path: std::result::Result<Path<String>, PathRejection>) -> Result<String> {
    let id = object_id(path)?;
    if secrets::validate_persisted_id("checkpoint_id", &id).is_err() {
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("检查点标识无效"),
        ));
    }
    Ok(id)
}

async fn restore_checkpoint(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    ContractJson(_body): ContractJson<EmptyBody>,
) -> Result<Response> {
    let id = checkpoint_id(id)?;
    match app.engine.restore_checkpoint(&id).await {
        Ok(receipt) => Ok(Json(json!({"ok":receipt.status == RestoreStatus::Complete,"restored":receipt.restored,"status":receipt.status,"receipt":receipt})).into_response()),
        Err(error) => {
            let receipt = match error.downcast_ref::<WorkspaceChangeError>() {
                Some(WorkspaceChangeError::Conflict { receipt } | WorkspaceChangeError::Unknown { receipt }) => receipt.clone(),
                _ => None,
            };
            if let Some(receipt) = receipt {
                let message = "检查点恢复未完整完成";
                return Ok((StatusCode::CONFLICT, Json(json!({"code":ErrorCode::Conflict,"message":message,"error":message,"retryable":false,"ok":false,"restored":receipt.restored,"status":receipt.status,"receipt":receipt}))).into_response());
            }
            Err(checkpoint_error(error))
        }
    }
}

fn git_diff_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<ProjectError>().is_some()
        || error.downcast_ref::<NetworkError>().is_some()
        || error.downcast_ref::<WorktreeError>().is_some()
    {
        return classify_anyhow(error);
    }
    if error.downcast_ref::<WorkspaceChangeError>().is_some() {
        return workspace_change_error(error);
    }
    let (status, code, message) = match error.downcast_ref::<GitDiffError>() {
        Some(GitDiffError::InvalidPath) => (
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            "差异路径无效",
        ),
        Some(GitDiffError::Unavailable | GitDiffError::OutcomeUnknown) => {
            (StatusCode::CONFLICT, ErrorCode::Conflict, "Git差异不可用")
        }
        Some(GitDiffError::Unsupported) => {
            (StatusCode::CONFLICT, ErrorCode::Conflict, "不支持此Git差异")
        }
        Some(GitDiffError::Conflict) => (
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
            "Git状态已变化，请重新查询",
        ),
        Some(GitDiffError::Internal) | None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "Git差异服务失败",
        ),
    };
    ApiError::with_code(status, code, false, anyhow::anyhow!(message))
}

fn checkpoint_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<ProjectError>().is_some()
        || error.downcast_ref::<NetworkError>().is_some()
        || error.downcast_ref::<WorktreeError>().is_some()
    {
        return classify_anyhow(error);
    }
    if error.downcast_ref::<WorkspaceChangeError>().is_some() {
        return workspace_change_error(error);
    }
    let (status, code, message) = match error.downcast_ref::<CheckpointError>() {
        Some(CheckpointError::Invalid) => (
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            "检查点请求无效",
        ),
        Some(CheckpointError::NotFound) => (
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "检查点或任务不存在",
        ),
        Some(CheckpointError::Conflict) => {
            (StatusCode::CONFLICT, ErrorCode::Conflict, "检查点状态冲突")
        }
        Some(CheckpointError::Unrestorable) => (
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
            "任务不可创建或恢复检查点",
        ),
        Some(CheckpointError::Corrupt | CheckpointError::Internal) | None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "检查点服务失败",
        ),
    };
    ApiError::with_code(status, code, false, anyhow::anyhow!(message))
}

async fn changes(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Value>> {
    let id = workspace_task_id(id)?;
    let changes = app.engine.changes(&id).map_err(workspace_change_error)?;
    let restore = app
        .engine
        .latest_restore(&id)
        .map_err(workspace_change_error)?;
    Ok(Json(json!({"changes":changes,"restore":restore})))
}

async fn restore(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Response> {
    let id = workspace_task_id(id)?;
    match app.engine.restore(&id).await {
        Ok(receipt) => Ok(Json(json!({"ok":receipt.status == RestoreStatus::Complete,"restored":receipt.restored,"status":receipt.status,"receipt":receipt})).into_response()),
        Err(error) => {
            let typed = error.chain().find_map(|cause| cause.downcast_ref::<WorkspaceChangeError>());
            let receipt = match typed {
                Some(WorkspaceChangeError::Conflict { receipt }) | Some(WorkspaceChangeError::Unknown { receipt }) => receipt.clone(),
                _ => None,
            };
            if let Some(receipt) = receipt {
                let message = typed.map(ToString::to_string).unwrap_or_else(|| "文件恢复失败".into());
                return Ok((StatusCode::CONFLICT, Json(json!({"code":ErrorCode::Conflict,"message":message,"error":message,"retryable":false,"ok":false,"restored":receipt.restored,"status":receipt.status,"receipt":receipt}))).into_response());
            }
            Err(workspace_change_error(error))
        }
    }
}

fn workspace_change_error(error: anyhow::Error) -> ApiError {
    if error.downcast_ref::<ProjectError>().is_some()
        || error.downcast_ref::<NetworkError>().is_some()
        || error.downcast_ref::<WorktreeError>().is_some()
    {
        return classify_anyhow(error);
    }
    let typed = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<WorkspaceChangeError>());
    let (status, code, message) = match typed {
        Some(WorkspaceChangeError::NotFound) => {
            (StatusCode::NOT_FOUND, ErrorCode::NotFound, "任务不存在")
        }
        Some(
            WorkspaceChangeError::Active
            | WorkspaceChangeError::Conflict { .. }
            | WorkspaceChangeError::Unrestorable
            | WorkspaceChangeError::Unknown { .. },
        ) => (
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
            "文件变更当前不可恢复",
        ),
        Some(WorkspaceChangeError::Corrupt | WorkspaceChangeError::Internal) | None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "文件变更服务失败",
        ),
    };
    ApiError::with_code(status, code, false, anyhow::anyhow!(message))
}

fn workspace_task_id(path: std::result::Result<Path<String>, PathRejection>) -> Result<String> {
    let id = object_id(path)?;
    if secrets::validate_persisted_id("task_id", &id).is_err() {
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("任务标识无效"),
        ));
    }
    Ok(id)
}

async fn wasm_unsupported() -> Response {
    ApiError::with_code(
        StatusCode::NOT_FOUND,
        ErrorCode::NotFound,
        false,
        anyhow::anyhow!("WASM 插件运行已移除"),
    )
    .into_response()
}

struct EventCursor {
    after: i64,
}

fn cursor_error() -> ApiError {
    ApiError::with_code(
        StatusCode::BAD_REQUEST,
        ErrorCode::RequestFailed,
        false,
        anyhow::anyhow!("事件游标必须是 0 到 i64::MAX 的十进制数字"),
    )
}

fn cursor_value(raw: &str) -> std::result::Result<i64, ApiError> {
    if raw.is_empty()
        || raw.as_bytes().first() == Some(&b'+')
        || raw.as_bytes().first() == Some(&b'-')
        || !raw.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(cursor_error());
    }
    raw.parse::<i64>().map_err(|_| cursor_error())
}

fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> std::result::Result<Option<&'a str>, ApiError> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("事件游标不能重复提供"),
        ));
    }
    first.to_str().map(Some).map_err(|_| cursor_error())
}

fn event_cursor(query: &str, headers: &HeaderMap) -> std::result::Result<EventCursor, ApiError> {
    let after = query_cursor(query)?;
    let header = single_header(headers, "last-event-id")?
        .map(cursor_value)
        .transpose()?;
    match (header, after) {
        (Some(after), _) | (None, Some(after)) => Ok(EventCursor { after }),
        (None, None) => Ok(EventCursor { after: 0 }),
    }
}

fn query_cursor(query: &str) -> std::result::Result<Option<i64>, ApiError> {
    let mut found = None;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let name = percent_decode(name)?;
        if name.contains('%') {
            return Err(cursor_error());
        }
        if name != "after" {
            continue;
        }
        if found.is_some() {
            return Err(ApiError::with_code(
                StatusCode::BAD_REQUEST,
                ErrorCode::RequestFailed,
                false,
                anyhow::anyhow!("事件游标不能重复提供"),
            ));
        }
        let value = percent_decode(value)?;
        found = Some(cursor_value(&value)?);
    }
    Ok(found)
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
    }
}

fn percent_decode(value: &str) -> std::result::Result<String, ApiError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'+' {
            decoded.push(b' ');
            index += 1;
            continue;
        }
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(cursor_error());
            }
            decoded.push((hex_value(bytes[index + 1]) << 4) | hex_value(bytes[index + 2]));
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).map_err(|_| cursor_error())
}

async fn session_events(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    request: Request,
) -> Result<Sse<impl futures_util::Stream<Item = std::result::Result<SseEvent, Infallible>>>> {
    let id = object_id(id)?;
    let cursor = event_cursor(request.uri().query().unwrap_or(""), request.headers())?;
    let session = app
        .engine
        .sessions()
        .session(&SessionId(id))
        .map_err(read_error)?;
    open_event_stream(app, session.legacy_run_id, cursor.after)
}

async fn events(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    request: Request,
) -> Result<Sse<impl futures_util::Stream<Item = std::result::Result<SseEvent, Infallible>>>> {
    let id = object_id(id)?;
    let cursor = event_cursor(request.uri().query().unwrap_or(""), request.headers())?;
    app.engine.sessions().run(&id).map_err(read_error)?;
    open_event_stream(app, id, cursor.after)
}

fn sse_frame(event: &Event) -> anyhow::Result<SseEvent> {
    crate::domain::validate_event_kind(&event.kind)?;
    let id = event.seq.to_string();
    if id.bytes().any(|byte| matches!(byte, b'\0' | b'\r' | b'\n'))
        || event.kind.bytes().any(|byte| matches!(byte, b'\r' | b'\n'))
    {
        anyhow::bail!("存储数据损坏");
    }
    let data = serde_json::to_string(event)?;
    Ok(SseEvent::default().id(id).event(&event.kind).data(data))
}

fn open_event_stream(
    app: App,
    id: String,
    after: i64,
) -> Result<Sse<impl futures_util::Stream<Item = std::result::Result<SseEvent, Infallible>>>> {
    let stream = async_stream::stream! {
        let mut sent = after;
        let mut failed = false;
        loop {
            if failed {
                break;
            }
            match app.engine.sessions().events(&id, sent) {
                Ok(events) => {
                    for event in events {
                        match sse_frame(&event) {
                            Ok(frame) => {
                                yield Ok(frame);
                                sent = event.seq;
                            }
                            Err(error) => {
                                yield Ok(stream_failure(sent, &error));
                                failed = true;
                                break;
                            }
                        }
                    }
                }
                Err(error) => {
                    yield Ok(stream_failure(sent, &error));
                    failed = true;
                }
            }
            if failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(base_url: &str, user_id: &str) -> Account {
        Account {
            base_url: base_url.into(),
            user_id: user_id.into(),
            quota_per_unit: 500_000.0,
        }
    }

    #[tokio::test]
    async fn changing_newapi_origin_requires_new_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::store::Store::open(&dir.path().join("account.db")).unwrap());
        store
            .save_settings(&Settings {
                workspace: dir.path().to_string_lossy().into(),
                max_concurrency: 1,
                routes: vec![],
                newapi: None,
            })
            .unwrap();
        let engine = Engine::new(store, 1).unwrap();
        let old = account("https://old.example", "1");
        engine
            .bind_account(old.clone(), Some("old".into()))
            .await
            .unwrap();
        let next = account("https://new.example", "1");
        assert!(engine.bind_account(next.clone(), None).await.is_err());
        assert_eq!(engine.store.settings().unwrap().unwrap().newapi, Some(old));
        assert_eq!(
            engine.store.secret("newapi-account").unwrap().as_deref(),
            Some("old")
        );
        engine
            .bind_account(next.clone(), Some("new".into()))
            .await
            .unwrap();
        assert_eq!(
            engine.store.secret("newapi-account").unwrap().as_deref(),
            Some("new")
        );
        let mut quota = next;
        quota.quota_per_unit = 100_000.0;
        engine.bind_account(quota.clone(), None).await.unwrap();
        assert_eq!(
            engine.store.settings().unwrap().unwrap().newapi,
            Some(quota)
        );
    }

    #[test]
    fn turn_command_error_maps_every_stable_variant() {
        let cases = [
            (
                ChatTurnError::InvalidInput,
                StatusCode::BAD_REQUEST,
                ErrorCode::RequestFailed,
                false,
            ),
            (
                ChatTurnError::NotFound,
                StatusCode::NOT_FOUND,
                ErrorCode::NotFound,
                false,
            ),
            (
                ChatTurnError::UnsupportedSession,
                StatusCode::BAD_REQUEST,
                ErrorCode::RequestFailed,
                false,
            ),
            (
                ChatTurnError::StalePredecessor,
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                false,
            ),
            (
                ChatTurnError::SessionBusy,
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                false,
            ),
            (
                ChatTurnError::UnresolvedEffects,
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                false,
            ),
            (
                ChatTurnError::PredecessorChanged,
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                false,
            ),
            (
                ChatTurnError::CorruptState,
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                false,
            ),
        ];
        for (variant, status, code, retryable) in cases {
            let error = turn_command_error(anyhow::Error::new(variant));
            assert_eq!(error.status, status, "{variant}");
            assert_eq!(error.code, code, "{variant}");
            assert_eq!(error.retryable, retryable, "{variant}");
            assert_eq!(error.error.to_string(), variant.to_string(), "{variant}");
        }
    }

    #[test]
    fn review_json_requires_exact_objects() {
        for bad in ["[]", "null", "{\"extra\":1}", "true"] {
            assert!(serde_json::from_str::<EmptyBody>(bad).is_err());
        }
        assert!(serde_json::from_str::<EmptyBody>("{}").is_ok());
        for bad in [
            r#"["src/a","head"]"#,
            r#"{"path":"src/a","view":"head","extra":1}"#,
            r#"{"path":"src/a","path":"src/b","view":"head"}"#,
            r#"{"path":"src/a","view":"head","view":"staged"}"#,
        ] {
            assert!(serde_json::from_str::<GitDiffBody>(bad).is_err());
        }
    }
}
