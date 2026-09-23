use crate::{domain::*, engine::Engine, provider, secrets, wasm};
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
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct App {
    pub engine: Arc<Engine>,
    pub token: String,
    pub origin: String,
}
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
        .route("/api/runs/{id}/events", get(events))
        .route("/api/projects", get(projects))
        .route("/api/sessions/{id}", get(session))
        .route("/api/sessions/{id}/turns", get(session_turns))
        .route("/api/sessions/{id}/events", get(session_events))
        .route("/api/turns/{id}", get(turn))
        .route("/api/wasm/plugins", get(wasm_plugins))
        .route("/api/wasm/run", post(wasm_run))
        .route("/api/tasks/{id}/resume", post(resume))
        .route("/api/tasks/{id}/cancel", post(cancel))
        .route("/api/tasks/{id}/changes", get(changes))
        .route("/api/tasks/{id}/restore", post(restore))
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
async fn changes(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Value>> {
    let id = object_id(id)?;
    Ok(Json(json!({"changes":app.engine.changes(&id)?})))
}
async fn restore(
    State(app): State<App>,
    id: std::result::Result<Path<String>, PathRejection>,
) -> Result<Json<Value>> {
    let id = object_id(id)?;
    Ok(Json(json!({"ok":true,"restored":app.engine.restore(&id)?})))
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
}
