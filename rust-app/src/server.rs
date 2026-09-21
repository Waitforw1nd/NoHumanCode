use crate::{domain::*, engine::Engine, provider, secrets, wasm};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
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
    fn from(e: anyhow::Error) -> Self {
        Self::with_code(StatusCode::BAD_REQUEST, ErrorCode::RequestFailed, false, e)
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
        return (StatusCode::FORBIDDEN, "请通过启动地址访问本机服务").into_response();
    }
    if let Some(origin) = headers.get(header::ORIGIN)
        && origin.to_str().ok() != Some(&app.origin)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site") {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && headers.get("x-peachsh-token").and_then(|v| v.to_str().ok()) != Some(&app.token)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response.headers_mut().insert("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
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
async fn health(State(app): State<App>) -> Json<Value> {
    Json(
        json!({"ok":true,"name":"🍑sh harness","version":env!("CARGO_PKG_VERSION"),"runtime":"rust","schema_version":app.engine.store.schema_version().unwrap_or_default()}),
    )
}
async fn bootstrap(State(app): State<App>) -> Json<Value> {
    Json(json!({"token":app.token,"version":env!("CARGO_PKG_VERSION")}))
}
async fn settings(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app
        .engine
        .store
        .settings()?
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
    Json(body): Json<SaveSettings>,
) -> Result<Json<Value>> {
    if body.keys.contains_key("newapi-account") {
        return Err(anyhow::anyhow!("请使用账户绑定入口设置账号令牌").into());
    }
    app.engine
        .configure_with_keys(body.settings, body.keys)
        .await?;
    Ok(Json(json!({"ok":true})))
}
fn route(app: &App, id: &str) -> anyhow::Result<Route> {
    app.engine
        .store
        .settings()?
        .ok_or_else(|| anyhow::anyhow!("缺少设置"))?
        .routes
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| anyhow::anyhow!("路由不存在"))
}
async fn models(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Value>> {
    let route = route(&app, &id)?;
    let key = app.engine.key(&route)?;
    Ok(Json(
        json!({"models":provider::models(&app.engine.client,&route,&key).await?}),
    ))
}
async fn check(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Value>> {
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
async fn account(State(app): State<App>, Json(body): Json<Bind>) -> Result<Json<Value>> {
    let mut settings = app
        .engine
        .store
        .settings()?
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
        .settings()?
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
    Ok(Json(json!(app.engine.store.runs()?)))
}
fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| anyhow::anyhow!("Idempotency-Key 必须是 ASCII 文本"))?;
    if !(1..=200).contains(&value.len()) || value.chars().any(|c| c.is_control()) {
        return Err(anyhow::anyhow!("Idempotency-Key 长度必须在 1–200 字节之间").into());
    }
    Ok(Some(value.to_owned()))
}
async fn start(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<RunRequest>,
) -> Result<Json<Run>> {
    let run = match idempotency_key(&headers)? {
        Some(key) => app.engine.start_idempotent(body, &key).await?,
        None => app.engine.start(body).await?,
    };
    Ok(Json(run))
}
async fn run(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Run>> {
    app.engine.store.run(&id).map(Json).map_err(|error| {
        ApiError::with_code(StatusCode::NOT_FOUND, ErrorCode::NotFound, false, error)
    })
}
#[derive(Deserialize)]
struct Resume {
    message: String,
}
async fn resume(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Resume>,
) -> Result<Json<Task>> {
    Ok(Json(app.engine.resume(&id, &body.message).await?))
}
async fn cancel(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Value>> {
    app.engine.cancel(&id)?;
    Ok(Json(json!({"ok":true})))
}
async fn changes(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(json!({"changes":app.engine.changes(&id)?})))
}
async fn restore(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(json!({"ok":true,"restored":app.engine.restore(&id)?})))
}
async fn wasm_plugins(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app
        .engine
        .store
        .settings()?
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
async fn wasm_run(State(app): State<App>, Json(body): Json<WasmRun>) -> Result<Json<Value>> {
    let settings = app
        .engine
        .store
        .settings()?
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
#[derive(Deserialize)]
struct Cursor {
    #[serde(default)]
    after: i64,
}
async fn events(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(cursor): Query<Cursor>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::Stream<Item = std::result::Result<SseEvent, Infallible>>>> {
    app.engine.store.run(&id).map_err(|error| {
        ApiError::with_code(StatusCode::NOT_FOUND, ErrorCode::NotFound, false, error)
    })?;
    let mut after = headers
        .get("last-event-id")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(cursor.after);
    let stream = async_stream::stream! {
        loop {
            match app.engine.store.events(&id,after) {
                Ok(events)=>for event in events {
                    after=event.seq;
                    match serde_json::to_string(&event) {
                        Ok(data)=>yield Ok(SseEvent::default().id(after.to_string()).event(&event.kind).data(data)),
                        Err(error)=>{
                            let body = ErrorBody {
                                code: ErrorCode::Internal,
                                message: secrets::scrub(&error.to_string(), ""),
                                retryable: false,
                            };
                            let mut data = serde_json::to_value(body).unwrap_or_else(|_| json!({}));
                            data["after"] = json!(after);
                            yield Ok(SseEvent::default().id(after.to_string()).event("error").data(data.to_string()));
                            break;
                        }
                    }
                },
                Err(error)=>{
                    let body = ErrorBody {
                        code: ErrorCode::Internal,
                        message: secrets::scrub(&error.to_string(), ""),
                        retryable: true,
                    };
                    let mut value = serde_json::to_value(body).unwrap_or_else(|_| json!({}));
                    value["after"] = json!(after);
                    let data = value.to_string();
                    yield Ok(SseEvent::default().event("error").data(data));
                    break;
                },
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
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
