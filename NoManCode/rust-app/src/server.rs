use crate::{
    approval,
    checkpoint::CheckpointError,
    domain::*,
    engine::Engine,
    git_diff::{GitDiffError, GitDiffRequest, GitDiffView},
    provider, secrets, wasm,
    workspace_changes::{RestoreStatus, WorkspaceChangeError},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, FromRequest, Path, Request, State,
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
use peachsh_protocol::{ErrorBody, ErrorCode};
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
        let message = secrets::scrub(&self.error.to_string(), "");
        let body = ErrorBody {
            code: self.code,
            message: message.clone(),
            retryable: self.retryable,
        };
        let mut value = serde_json::to_value(body).unwrap_or_else(|_| json!({}));
        // Keep the pre-v1 `error` field for the migration UI while new
        // clients use code/message/retryable.
        value["error"] = Value::String(message);
        (self.status, Json(value)).into_response()
    }
}

/// JSON body extractor whose failures stay inside the structured error contract.
///
/// Axum's built-in `Json` rejection is plain text and uses 422 for a type
/// mismatch. Callers of this adapter classify invalid JSON and field types as
/// 400, a missing or unsupported content type as 415, and the existing 1 MiB
/// limit as 413.
pub struct ContractJson<T>(pub T);

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

/// A completed store read is not a request-validation boundary.
///
/// Missing rows stay 404. Busy or locked storage is retryable. Every other
/// failure of that read, including a plain consistency check, is a safe 500.
/// Callers must classify settings lookup separately from a later "missing
/// settings" or unknown-route validation error.
fn read_error(error: anyhow::Error) -> ApiError {
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
        .route("/api/settings", get(settings).put(save_settings))
        .route("/api/routes/{id}/models", get(models))
        .route("/api/routes/{id}/check", post(check))
        .route("/api/newapi", put(account))
        .route("/api/newapi/balance", get(balance))
        .route("/api/runs", get(runs).post(start))
        .route("/api/runs/{id}", get(run))
        .route("/api/runs/{id}/context", get(run_context))
        .route("/api/runs/{id}/events", get(events))
        .route("/api/projects", get(projects))
        .route("/api/sessions/{id}", get(session))
        .route(
            "/api/sessions/{id}/turns",
            get(session_turns).post(post_session_turn),
        )
        .route("/api/sessions/{id}/events", get(session_events))
        .route("/api/turns/{id}", get(turn))
        .route("/api/wasm/plugins", get(wasm_plugins))
        .route("/api/wasm/run", post(wasm_run))
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
    if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(expected_host) {
        return forbidden("请通过启动地址访问本机服务");
    }
    if let Some(origin) = headers.get(header::ORIGIN)
        && origin.to_str().ok() != Some(&app.origin)
    {
        return forbidden("Origin 与本机服务地址不一致");
    }
    if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site") {
        return forbidden("拒绝跨站请求");
    }
    if !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && headers.get("x-peachsh-token").and_then(|v| v.to_str().ok()) != Some(&app.token)
    {
        return forbidden("写请求令牌缺失或不正确");
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
        json!({"ok":true,"name":"🍑sh harness","version":env!("CARGO_PKG_VERSION"),"runtime":"rust","schema_version":schema_version}),
    ))
}
async fn bootstrap(State(app): State<App>) -> Json<Value> {
    Json(json!({"token":app.token,"version":env!("CARGO_PKG_VERSION")}))
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
fn route(app: &App, id: &str) -> Result<Route> {
    app.engine
        .store
        .settings()
        .map_err(read_error)?
        .ok_or_else(|| anyhow::anyhow!("缺少设置"))?
        .routes
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| anyhow::anyhow!("路由不存在").into())
}
async fn models(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Value>> {
    let id = object_id(id)?;
    let route = route(&app, &id)?;
    let key = app.engine.key(&route)?;
    Ok(Json(
        json!({"models":provider::models(&app.engine.client,&route,&key).await?}),
    ))
}
async fn check(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Value>> {
    let id = object_id(id)?;
    let mut route = route(&app, &id)?;
    route.max_tokens = 128;
    let key = app.engine.key(&route)?;
    let started = std::time::Instant::now();
    let list = provider::models(&app.engine.client, &route, &key).await?;
    if !list.contains(&route.model) {
        return Err(anyhow::anyhow!(
            "模型列表中没有 {}，可用模型：{}",
            route.model,
            list.join(", ")
        )
        .into());
    }
    let (message, usage) = provider::complete(
        &app.engine.client,
        &route,
        &key,
        &[json!({"role":"user","content":"Return only the result of 17 * 19."})],
        &[],
        |_| Ok(()),
    )
    .await?;
    let correct = message["content"]
        .as_str()
        .is_some_and(|s| s.trim() == "323");
    Ok(Json(
        json!({"ok":correct,"model":route.model,"answer":message["content"],"usage":usage,"elapsed_ms":started.elapsed().as_millis()}),
    ))
}
#[derive(Deserialize)]
struct Bind {
    account: Account,
    token: Option<String>,
}
fn select_account_token(
    previous: Option<&Account>,
    next: &Account,
    provided: Option<String>,
    stored: Option<String>,
) -> anyhow::Result<String> {
    if let Some(token) = provided.filter(|s| !s.is_empty()) {
        return Ok(token);
    }
    if previous != Some(next) {
        anyhow::bail!("更换 New API 地址或用户后必须重新填写账号令牌");
    }
    stored.ok_or_else(|| anyhow::anyhow!("请填写 New API 账号访问令牌"))
}
async fn account(
    State(app): State<App>,
    ContractJson(body): ContractJson<Bind>,
) -> Result<Json<Value>> {
    let mut settings = app
        .engine
        .store
        .settings()
        .map_err(read_error)?
        .ok_or_else(|| anyhow::anyhow!("缺少设置"))?;
    let previous_account = settings.newapi.clone();
    settings.newapi = Some(body.account);
    settings.validate()?;
    let token = select_account_token(
        previous_account.as_ref(),
        settings.newapi.as_ref().unwrap(),
        body.token,
        app.engine.store.secret("newapi-account")?,
    )?;
    let result = provider::balance(
        &app.engine.client,
        settings.newapi.as_ref().unwrap(),
        &token,
    )
    .await?;
    app.engine.configure(settings).await?;
    app.engine.store.put_secret("newapi-account", &token)?;
    Ok(Json(result))
}
async fn balance(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app
        .engine
        .store
        .settings()
        .map_err(read_error)?
        .ok_or_else(|| anyhow::anyhow!("缺少设置"))?;
    let account = settings
        .newapi
        .ok_or_else(|| anyhow::anyhow!("请先绑定 New API 账号"))?;
    let token = app
        .engine
        .store
        .secret("newapi-account")?
        .ok_or_else(|| anyhow::anyhow!("缺少账号访问令牌"))?;
    Ok(Json(
        provider::balance(&app.engine.client, &account, &token).await?,
    ))
}
async fn runs(State(app): State<App>) -> Result<Json<Value>> {
    Ok(Json(json!(app.engine.store.runs().map_err(read_error)?)))
}
async fn projects(State(app): State<App>) -> Result<Json<Vec<Project>>> {
    Ok(Json(app.engine.store.projects().map_err(read_error)?))
}
async fn session(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Session>> {
    let id = object_id(id)?;
    app.engine
        .store
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
    app.engine.store.session(&id).map_err(read_error)?;
    Ok(Json(
        app.engine
            .store
            .turns_for_session(&id)
            .map_err(read_error)?,
    ))
}
/// Strict body for `POST /api/sessions/{id}/turns`.
///
/// The session comes only from the path; the agent, predecessor, and message
/// are required strings. Unknown or duplicated fields, nulls, and wrong types
/// are 400 before the command reaches the engine.
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

/// This endpoint requires exactly one visible-ASCII Idempotency-Key.
///
/// Unlike the optional run key it is mandatory, refuses repeated or
/// comma-merged header values, and cannot be supplied by the body or query.
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

/// Dedicated classification for `send_chat_turn` failures.
///
/// Order: explicit idempotency conflict, then the stable ChatTurnError
/// variants, then transient storage, then a safe 500. ChatTurnError text is
/// already fixed and free of request data; every other failure collapses to a
/// local fixed message instead of the raw anyhow text. Only the typed
/// NotFound maps to 404 — a raw missing row inside the command is a 500.
fn turn_command_error(error: anyhow::Error) -> ApiError {
    // anyhow::Error::downcast_ref also sees through `context()` layers; a
    // `chain()` element for a context is a wrapper that fails `is::<T>`.
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
        .store
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
    app.engine.store.run(&id).map(Json).map_err(read_error)
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
        .store
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
    Ok(Json(app.engine.resume(&id, &body.message).await?))
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
    if let Some(typed) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<approval::ApprovalError>())
    {
        let (status, code, message) = match typed {
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
    app.engine.store.task(&task_id).map_err(read_error)?;
    let approvals: Vec<ApprovalDto> = app
        .engine
        .store
        .approvals_for_task(&task_id)
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
        .store
        .approval(&id)
        .map(ApprovalDto::from)
        .map(Json)
        .map_err(approval_error)
}

async fn decide_approval(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
    ContractJson(body): ContractJson<ApprovalDecisionBody>,
) -> Result<Json<ApprovalDto>> {
    let id = approval_id(id)?;
    let approved = matches!(body.decision, ApprovalDecision::Approve);
    app.engine
        .decide_approval(&id, approved, Some("user"))
        .map(ApprovalDto::from)
        .map(Json)
        .map_err(approval_error)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitDiffBody {
    path: String,
    view: GitDiffView,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyBody {}
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
                Some(WorkspaceChangeError::Conflict { receipt } | WorkspaceChangeError::Unknown { receipt }) => receipt.clone(), _ => None,
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
    if error.downcast_ref::<WorkspaceChangeError>().is_some() {
        return workspace_change_error(error);
    }
    let (status, code, message) = match error.downcast_ref::<GitDiffError>() {
        Some(GitDiffError::InvalidPath) => (
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            "差异路径无效",
        ),
        Some(GitDiffError::Unavailable) => {
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
            let receipt = match typed { Some(WorkspaceChangeError::Conflict { receipt }) | Some(WorkspaceChangeError::Unknown { receipt }) => receipt.clone(), _ => None };
            if let Some(receipt) = receipt {
                let message = typed.map(ToString::to_string).unwrap_or_else(|| "文件恢复失败".into());
                return Ok((StatusCode::CONFLICT, Json(json!({"code":ErrorCode::Conflict,"message":message,"error":message,"retryable":false,"ok":false,"restored":receipt.restored,"status":receipt.status,"receipt":receipt}))).into_response());
            }
            Err(workspace_change_error(error))
        }
    }
}

fn workspace_change_error(error: anyhow::Error) -> ApiError {
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
        Some(WorkspaceChangeError::Corrupt | WorkspaceChangeError::Internal) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "文件变更服务失败",
        ),
        None => (
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
async fn wasm_plugins(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app
        .engine
        .store
        .settings()
        .map_err(read_error)?
        .ok_or_else(|| anyhow::anyhow!("缺少设置"))?;
    let directory = std::path::Path::new(&settings.workspace)
        .join(".peachsh")
        .join("plugins");
    Ok(Json(
        json!({"abi":wasm::ABI_VERSION,"plugins":wasm::list(&directory)?}),
    ))
}
#[derive(Deserialize)]
struct WasmRun {
    plugin: String,
    input: Value,
}
async fn wasm_run(
    State(app): State<App>,
    ContractJson(body): ContractJson<WasmRun>,
) -> Result<Json<Value>> {
    let settings = app
        .engine
        .store
        .settings()
        .map_err(read_error)?
        .ok_or_else(|| anyhow::anyhow!("缺少设置"))?;
    let directory = std::path::Path::new(&settings.workspace)
        .join(".peachsh")
        .join("plugins");
    let report = tokio::task::spawn_blocking(move || {
        let plugin = wasm::load(&directory, &body.plugin)?;
        wasm::run(&plugin, &body.input)
    })
    .await
    .map_err(anyhow::Error::from)??;
    Ok(Json(
        serde_json::to_value(report).map_err(anyhow::Error::from)?,
    ))
}
/// Shared cursor for Run and Session SSE.
///
/// `after` and `Last-Event-ID` are parsed before this struct is filled. Both
/// must be absent or a single ASCII decimal in `0..=i64::MAX`. When both are
/// present and valid, the header wins. An invalid value is never replaced by
/// zero or by the other source.
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
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("事件游标必须是 0 到 i64::MAX 的十进制数字"),
        ));
    }
    raw.parse::<i64>().map_err(|_| {
        ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("事件游标必须是 0 到 i64::MAX 的十进制数字"),
        )
    })
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
    first.to_str().map(Some).map_err(|_| {
        ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("事件游标必须是 0 到 i64::MAX 的十进制数字"),
        )
    })
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
                return Err(ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    ErrorCode::RequestFailed,
                    false,
                    anyhow::anyhow!("事件游标必须是 0 到 i64::MAX 的十进制数字"),
                ));
            }
            decoded.push((hex_value(bytes[index + 1]) << 4) | hex_value(bytes[index + 2]));
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).map_err(|_| {
        ApiError::with_code(
            StatusCode::BAD_REQUEST,
            ErrorCode::RequestFailed,
            false,
            anyhow::anyhow!("事件游标必须是 0 到 i64::MAX 的十进制数字"),
        )
    })
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
        .store
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
    app.engine.store.run(&id).map_err(read_error)?;
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
            match app.engine.store.events(&id, sent) {
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

    #[test]
    fn changing_newapi_origin_requires_new_token() {
        let old = account("https://old.example", "1");
        let next = account("https://new.example", "1");
        assert!(select_account_token(Some(&old), &next, None, Some("old".into())).is_err());
        assert_eq!(
            select_account_token(Some(&old), &next, Some("new".into()), Some("old".into()))
                .unwrap(),
            "new"
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
    fn run_context_error_preserves_typed_status_without_context_leaks() {
        for (variant, status, code) in [
            (
                RunContextError::NotFound,
                StatusCode::NOT_FOUND,
                ErrorCode::NotFound,
            ),
            (
                RunContextError::Unmapped,
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
            ),
            (
                RunContextError::CorruptState,
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
            ),
        ] {
            let error = context_error(anyhow::Error::new(variant).context("private database path"));
            assert_eq!(error.status, status);
            assert_eq!(error.code, code);
            assert!(!error.retryable);
            assert!(!error.error.to_string().contains("private"));
        }
        let unknown = context_error(anyhow::anyhow!("SELECT secret FROM private"));
        assert_eq!(unknown.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(unknown.error.to_string(), "运行身份读取失败");
    }

    #[test]
    fn turn_command_error_keeps_typed_classification_inside_a_chain() {
        let conflict = turn_command_error(
            anyhow::Error::new(crate::store::IdempotencyConflict).context("outer context"),
        );
        assert_eq!(conflict.status, StatusCode::CONFLICT);
        assert_eq!(conflict.code, ErrorCode::Conflict);
        assert!(!conflict.retryable);

        let corrupt = turn_command_error(
            anyhow::Error::from(rusqlite::Error::InvalidColumnName("x".into()))
                .context(ChatTurnError::CorruptState),
        );
        assert_eq!(corrupt.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(corrupt.code, ErrorCode::Internal);
        assert!(!corrupt.retryable);

        let busy_corrupt = turn_command_error(
            anyhow::Error::from(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            ))
            .context(ChatTurnError::CorruptState),
        );
        assert_eq!(busy_corrupt.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(busy_corrupt.code, ErrorCode::Internal);
        // downcast_ref sees the CorruptState through the context layer, so the
        // typed arm wins over the BUSY cause.
        assert!(!busy_corrupt.retryable);
        assert_eq!(
            busy_corrupt.error.to_string(),
            ChatTurnError::CorruptState.to_string()
        );
    }

    #[test]
    fn turn_command_error_never_turns_unknown_failures_into_4xx() {
        let missing_row =
            turn_command_error(anyhow::Error::from(rusqlite::Error::QueryReturnedNoRows));
        assert_eq!(missing_row.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(missing_row.code, ErrorCode::Internal);
        assert!(!missing_row.retryable);

        for sqlite in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let transient = turn_command_error(anyhow::Error::from(
                rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(sqlite), None),
            ));
            assert_eq!(transient.status, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(transient.code, ErrorCode::Internal);
            assert!(transient.retryable);
        }

        let plain = turn_command_error(anyhow::anyhow!("SELECT path FROM secret_config"));
        assert_eq!(plain.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(plain.code, ErrorCode::Internal);
        assert!(!plain.retryable);
        assert_eq!(plain.error.to_string(), "连续对话请求处理失败");
        assert!(!plain.error.to_string().contains("secret_config"));
    }

    #[test]
    fn required_idempotency_key_accepts_exactly_one_clean_value() {
        let mut headers = HeaderMap::new();
        assert!(required_idempotency_key(&headers).is_err());

        headers.insert("idempotency-key", HeaderValue::from_static("turn-1"));
        assert_eq!(required_idempotency_key(&headers).unwrap(), "turn-1");

        let mut doubled = HeaderMap::new();
        doubled.append("idempotency-key", HeaderValue::from_static("same"));
        doubled.append("idempotency-key", HeaderValue::from_static("same"));
        assert!(required_idempotency_key(&doubled).is_err());

        let mut merged = HeaderMap::new();
        merged.insert("idempotency-key", HeaderValue::from_static("key-a,key-b"));
        assert!(required_idempotency_key(&merged).is_err());

        let mut spaced = HeaderMap::new();
        spaced.insert("idempotency-key", HeaderValue::from_static("has space"));
        assert!(required_idempotency_key(&spaced).is_err());
    }

    #[test]
    fn approval_error_maps_typed_variants_through_context_and_source() {
        use anyhow::Context as _;

        let context = Err::<(), _>(approval::ApprovalError::BindingConflict {
            id: "approval".into(),
        })
        .context("outer context")
        .unwrap_err();
        let mapped = approval_error(context);
        assert_eq!(mapped.status, StatusCode::CONFLICT);
        assert_eq!(mapped.code, ErrorCode::Conflict);
        assert!(!mapped.retryable);
        assert_eq!(mapped.error.to_string(), "审批绑定不匹配");

        #[derive(Debug)]
        struct Wrapped(approval::ApprovalError);
        impl std::fmt::Display for Wrapped {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("wrapped")
            }
        }
        impl std::error::Error for Wrapped {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let mapped = approval_error(anyhow::Error::new(Wrapped(
            approval::ApprovalError::UnknownResult {
                id: "approval".into(),
            },
        )));
        assert_eq!(mapped.status, StatusCode::CONFLICT);
        assert_eq!(mapped.code, ErrorCode::Conflict);
        assert!(!mapped.retryable);
        assert_eq!(mapped.error.to_string(), "工具执行结果未知");
    }

    #[test]
    fn approval_error_keeps_corruption_500_and_busy_retryable() {
        let corrupt = approval_error(anyhow::Error::new(approval::ApprovalError::CorruptState {
            id: "approval".into(),
        }));
        assert_eq!(corrupt.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!corrupt.retryable);

        let busy = approval_error(anyhow::Error::from(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        )));
        assert_eq!(busy.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(busy.retryable);
    }
}
