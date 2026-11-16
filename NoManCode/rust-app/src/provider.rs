use crate::{
    domain::*,
    provider_transport::{
        NetworkPurpose, ProviderClient, ProviderPermit, RESPONSE_LIMIT, TransportError,
    },
    provider_usage::UsageObservation,
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn client() -> Result<ProviderClient> {
    Ok(ProviderClient::new()?)
}
pub(crate) async fn json_response(
    response: reqwest::Response,
) -> std::result::Result<(Value, u16), TransportError> {
    let status = response.status();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| TransportError::Unknown)?;
        if chunk.len() > RESPONSE_LIMIT.saturating_sub(bytes.len()) {
            return Err(TransportError::Unknown);
        }
        bytes.extend_from_slice(&chunk);
    }
    let status_code = status.as_u16();
    if !status.is_success() {
        return Err(TransportError::Response {
            status: status_code,
        });
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransportError::InvalidResponse {
            status: status_code,
        })?;
    if value.is_null() {
        return Err(TransportError::InvalidResponse {
            status: status_code,
        });
    }
    Ok((value, status_code))
}
pub async fn models(
    client: &ProviderClient,
    permit: ProviderPermit,
    route: &Route,
) -> std::result::Result<Vec<String>, TransportError> {
    permit.matches(
        &route.base_url,
        NetworkPurpose::Models,
        Some(&route.model),
        None,
    )?;
    let response = client.send(permit, None).await?;
    let (value, status) = json_response(response).await?;
    let list = value["data"]
        .as_array()
        .ok_or(TransportError::InvalidResponse { status })?;
    list.iter()
        .map(|v| {
            v["id"]
                .as_str()
                .or(v.as_str())
                .map(String::from)
                .ok_or(TransportError::InvalidResponse { status })
        })
        .collect()
}

#[derive(Default)]
struct ToolParts {
    id: String,
    name: String,
    arguments: String,
}
#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    event: String,
    size: usize,
    done: bool,
    finish: Option<String>,
    pub text: String,
    tools: BTreeMap<usize, ToolParts>,
    pub usage: Value,
    usage_observation: UsageObservation,
}
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        ensure!(
            bytes.len() <= RESPONSE_LIMIT.saturating_sub(self.size),
            "模型输出超过安全上限"
        );
        self.size += bytes.len();
        self.buffer.extend_from_slice(bytes);
        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(emit)?;
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.event.is_empty() {
                    self.event.push('\n');
                }
                self.event.push_str(data.trim_start());
            }
        }
        Ok(())
    }
    fn flush(&mut self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        let data = std::mem::take(&mut self.event);
        if data.is_empty() {
            return Ok(());
        }
        ensure!(!self.done, "流式结束后收到额外事件");
        if data == "[DONE]" {
            ensure!(self.finish.is_some(), "流式响应缺少完成原因");
            self.done = true;
            return Ok(());
        }
        let chunk: Value = serde_json::from_str(&data).context("无效的流式响应")?;
        if !chunk["error"].is_null() {
            bail!("模型流返回错误");
        }
        self.usage_observation.observe(&chunk["usage"]);
        self.usage = self.usage_observation.into_value();
        if let Some(choice) = chunk["choices"].as_array().and_then(|v| v.first()) {
            let delta = &choice["delta"];
            if let Some(text) = delta["content"].as_str() {
                self.text.push_str(text);
                emit(text)?;
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                self.finish = Some(reason.into());
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for call in calls {
                    let index = call["index"].as_u64().context("工具调用缺少 index")? as usize;
                    ensure!(index < 64, "单轮工具调用过多");
                    let part = self.tools.entry(index).or_default();
                    if let Some(s) = call["id"].as_str() {
                        part.id.push_str(s);
                    }
                    if let Some(s) = call["function"]["name"].as_str() {
                        part.name.push_str(s);
                    }
                    if let Some(s) = call["function"]["arguments"].as_str() {
                        part.arguments.push_str(s);
                    }
                }
            }
        }
        Ok(())
    }
    pub fn finish(self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<Value> {
        let _ = emit;
        ensure!(
            self.buffer.is_empty() && self.event.is_empty(),
            "流式连接缺少完整结束事件"
        );
        ensure!(
            self.done && self.finish.is_some(),
            "流式连接提前中断，请继续此成员重试"
        );
        ensure!(
            self.finish.as_deref() != Some("length"),
            "模型输出达到上限，请提高路由的输出上限后重试"
        );
        ensure!(
            self.finish.as_deref() != Some("content_filter"),
            "模型拒绝生成此内容"
        );
        let mut message = json!({"role":"assistant","content":self.text});
        if !self.tools.is_empty() {
            let calls: Result<Vec<Value>> = self.tools.into_values().map(|part| {
                ensure!(!part.id.is_empty() && !part.name.is_empty(),"不完整的工具调用");
                let _: Value = serde_json::from_str(&part.arguments).context("工具参数不完整")?;
                Ok(json!({"id":part.id,"type":"function","function":{"name":part.name,"arguments":part.arguments}}))
            }).collect();
            message["tool_calls"] = json!(calls?);
        } else {
            ensure!(!self.text.trim().is_empty(), "模型没有返回内容");
        }
        Ok(message)
    }
}

pub(crate) struct BuiltinCompletionCodec;

impl crate::provider_runtime::CompletionCodec for BuiltinCompletionCodec {
    fn encode(
        &self,
        input: &crate::provider_runtime::CompletionInput<'_>,
    ) -> std::result::Result<Value, crate::provider_runtime::CodecError> {
        Ok(crate::provider_runtime::expected_body(input))
    }

    fn decoder(
        &self,
    ) -> std::result::Result<
        Box<dyn crate::provider_runtime::CompletionDecoder>,
        crate::provider_runtime::CodecError,
    > {
        Ok(Box::new(Decoder::default()))
    }
}

impl crate::provider_runtime::CompletionDecoder for Decoder {
    fn feed(
        &mut self,
        bytes: &[u8],
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<(), crate::provider_runtime::CodecError> {
        Decoder::feed(self, bytes, &mut |text| emit(text))
            .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn finish(
        self: Box<Self>,
        emit: &mut crate::provider_runtime::Emit<'_>,
    ) -> std::result::Result<
        crate::provider_runtime::CompletionOutput,
        crate::provider_runtime::CodecError,
    > {
        let usage = self.usage.clone();
        Decoder::finish(*self, &mut |text| emit(text))
            .map(|message| (message, usage))
            .map_err(|_| crate::provider_runtime::CodecError::Failed)
    }
}
pub async fn complete(
    client: &ProviderClient,
    permit: ProviderPermit,
    route: &Route,
    messages: &[Value],
    tools: &[Value],
    mut emit: impl FnMut(&str) -> Result<()> + Send,
) -> std::result::Result<(Value, Value), TransportError> {
    permit.matches(
        &route.base_url,
        NetworkPurpose::Completion,
        Some(&route.model),
        None,
    )?;
    let mut body = json!({"model":permit.model(),"messages":messages,"max_tokens":route.max_tokens,"stream":true,"stream_options":{"include_usage":true}});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
    }
    let response = client.send(permit, Some(&body)).await?;
    if !response.status().is_success() {
        json_response(response).await?;
        return Err(TransportError::Unknown);
    }
    let mut decoder = Decoder::default();
    let mut bytes = response.bytes_stream();
    let mut redactor = crate::secrets::StreamRedactor::default();
    let (message, usage) = {
        let mut filtered_emit = |text: &str| {
            let safe = redactor.push(text);
            if safe.is_empty() { Ok(()) } else { emit(&safe) }
        };
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|_| TransportError::Unknown)?;
            decoder
                .feed(&chunk, &mut filtered_emit)
                .map_err(|_| TransportError::Unknown)?;
            if decoder.done {
                break;
            }
        }
        let usage = decoder.usage.clone();
        let message = decoder
            .finish(&mut filtered_emit)
            .map_err(|_| TransportError::Unknown)?;
        (message, usage)
    };
    let tail = redactor.finish();
    if !tail.is_empty() {
        emit(&tail).map_err(|_| TransportError::Unknown)?;
    }
    Ok((message, usage))
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .filter(|n| n.is_finite())
}
pub fn normalize_balance(value: Value, account: &Account) -> Result<Value> {
    ensure!(value["success"] != false, "New API 账号验证失败");
    let data = value.get("data").unwrap_or(&value);
    let quota = number(&data["quota"]).context("余额响应缺少有效 quota")?;
    let used = number(&data["used_quota"]);
    Ok(
        json!({"username":data["username"],"display_name":data["display_name"],"group":data["group"],"quota_raw":quota,"balance":quota/account.quota_per_unit,"used":used.map(|v|v/account.quota_per_unit),"quota_per_unit":account.quota_per_unit,"checked_at":now()}),
    )
}
pub async fn balance(
    client: &ProviderClient,
    permit: ProviderPermit,
    account: &Account,
) -> std::result::Result<Value, TransportError> {
    permit.matches(
        &account.base_url,
        NetworkPurpose::Balance,
        None,
        Some(&account.user_id),
    )?;
    let response = client.send(permit, None).await?;
    let (value, status) = json_response(response).await?;
    normalize_balance(value, account).map_err(|_| TransportError::InvalidResponse { status })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf8_sse_and_fragmented_tools() {
        let data = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"桃子\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.txt\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"
        );
        let mut decoder = Decoder::default();
        let mut text = String::new();
        for byte in data.as_bytes() {
            decoder
                .feed(&[*byte], &mut |s| {
                    text.push_str(s);
                    Ok(())
                })
                .unwrap();
        }
        let message = decoder.finish(&mut |_| Ok(())).unwrap();
        assert_eq!(text, "桃子");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a.txt\"}"
        );
    }
    #[test]
    fn truncated_stream_fails() {
        let mut d = Decoder::default();
        d.feed(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            &mut |_| Ok(()),
        )
        .unwrap();
        assert!(d.finish(&mut |_| Ok(())).is_err());
    }
    #[test]
    fn balance_validation() {
        let a = Account {
            base_url: "https://example.com".into(),
            user_id: "1".into(),
            quota_per_unit: 500_000.0,
        };
        assert_eq!(
            normalize_balance(
                json!({"success":true,"data":{"quota":"1000000","used_quota":500000}}),
                &a
            )
            .unwrap()["balance"],
            2.0
        );
        assert!(normalize_balance(json!({"success":false}), &a).is_err());
        assert!(normalize_balance(json!({"data":{}}), &a).is_err());
    }

    #[tokio::test]
    async fn done_does_not_wait_for_connection_close() {
        use axum::{
            Router,
            body::{Body, Bytes},
            routing::post,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let app=Router::new().route("/chat/completions",post(||async{
                let first=Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");
                let stream=futures_util::stream::iter([Ok::<_,std::io::Error>(first)]).chain(futures_util::stream::pending());
                ([("content-type","text/event-stream")],Body::from_stream(stream))
            }));
            axum::serve(listener, app).await.unwrap();
        });
        let route = Route {
            id: "mock".into(),
            name: "mock".into(),
            base_url: format!("http://{address}"),
            model: "mock".into(),
            max_tokens: 128,
            parallel_limit: 1,
            key_env: None,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            complete(
                &client().unwrap(),
                ProviderPermit::new(
                    crate::provider_transport::validate_target(
                        &route.base_url,
                        NetworkPurpose::Completion,
                    )
                    .unwrap(),
                    "fake".into(),
                    "call-test".into(),
                    Some(route.model.clone()),
                    None,
                )
                .unwrap(),
                &route,
                &[],
                &[],
                |_| Ok(()),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.0["content"], "OK");
        server.abort();
        let _ = server.await;
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use crate::provider_transport::validate_target;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    struct Mock {
        base: String,
        count: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<String>>>,
        task: JoinHandle<()>,
    }
    impl Mock {
        async fn start(response: Vec<u8>, hold_open: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let count = Arc::new(AtomicUsize::new(0));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let c = count.clone();
            let r = requests.clone();
            let task = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    c.fetch_add(1, Ordering::SeqCst);
                    let mut request = Vec::new();
                    loop {
                        let mut buf = [0; 4096];
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            break;
                        }
                        request.extend_from_slice(&buf[..n]);
                        assert!(request.len() < 1_000_000);
                        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&request[..end]);
                            let length = header
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if request.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    r.lock().unwrap().push(String::from_utf8(request).unwrap());
                    if socket.write_all(&response).await.is_ok() && hold_open {
                        std::future::pending::<()>().await;
                    }
                }
            });
            Self {
                base,
                count,
                requests,
                task,
            }
        }
        async fn stop(self) {
            self.task.abort();
            let _ = self.task.await;
        }
        fn hits(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
        fn request(&self) -> String {
            self.requests.lock().unwrap().last().unwrap().clone()
        }
    }
    fn response(status: u16, body: &[u8], extra: &str) -> Vec<u8> {
        let mut bytes = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
            body.len()
        )
        .into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }
    fn route(base: &str) -> Route {
        Route {
            id: "mock".into(),
            name: "mock".into(),
            base_url: base.into(),
            model: "m".into(),
            max_tokens: 128,
            parallel_limit: 1,
            key_env: None,
        }
    }
    fn permit(
        base: &str,
        purpose: NetworkPurpose,
        model: Option<&str>,
        user: Option<&str>,
    ) -> ProviderPermit {
        ProviderPermit::new(
            validate_target(base, purpose).unwrap(),
            "fixture-secret-key".into(),
            "fixture-call".into(),
            model.map(String::from),
            user.map(String::from),
        )
        .unwrap()
    }
    async fn list(mock: &Mock) -> std::result::Result<Vec<String>, TransportError> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            models(
                &client().unwrap(),
                permit(&mock.base, NetworkPurpose::Models, Some("m"), None),
                &route(&mock.base),
            ),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn all_redirect_statuses_do_not_reach_second_origin_or_replay_post() {
        let b = Mock::start(response(200, br#"{"data":[{"id":"m"}]}"#, ""), false).await;
        for status in [301, 302, 303, 307, 308] {
            let a = Mock::start(
                response(
                    status,
                    b"secret upstream error",
                    &format!("Location: {}/private?token=fixture-secret-key\r\n", b.base),
                ),
                false,
            )
            .await;
            let result = complete(
                &client().unwrap(),
                permit(&a.base, NetworkPurpose::Completion, Some("m"), None),
                &route(&a.base),
                &[],
                &[],
                |_| Ok(()),
            )
            .await;
            assert_eq!(result.unwrap_err(), TransportError::Response { status });
            assert_eq!(a.hits(), 1);
            assert_eq!(b.hits(), 0);
            assert!(a.request().starts_with("POST /chat/completions "));
            assert!(a.request().contains("\"model\":\"m\""));
            a.stop().await;
        }
        assert_eq!(list(&b).await.unwrap(), vec!["m"]);
        assert_eq!(b.hits(), 1);
        b.stop().await;
    }

    #[tokio::test]
    async fn mismatched_target_model_user_or_purpose_is_invalid_and_zero_connections() {
        let a = Mock::start(response(200, br#"{"data":[{"id":"m"}]}"#, ""), false).await;
        let b = Mock::start(response(200, br#"{"data":[{"id":"m"}]}"#, ""), false).await;
        let c = client().unwrap();
        assert_eq!(
            models(
                &c,
                permit(&a.base, NetworkPurpose::Models, Some("m"), None),
                &route(&b.base)
            )
            .await,
            Err(TransportError::Invalid)
        );
        let mut changed = route(&a.base);
        changed.model = "other".into();
        assert_eq!(
            models(
                &c,
                permit(&a.base, NetworkPurpose::Models, Some("m"), None),
                &changed
            )
            .await,
            Err(TransportError::Invalid)
        );
        assert_eq!(
            models(
                &c,
                permit(&a.base, NetworkPurpose::Completion, Some("m"), None),
                &route(&a.base)
            )
            .await,
            Err(TransportError::Invalid)
        );
        let account = Account {
            base_url: a.base.clone(),
            user_id: "2".into(),
            quota_per_unit: 1.0,
        };
        assert_eq!(
            balance(
                &c,
                permit(&a.base, NetworkPurpose::Balance, None, Some("1")),
                &account
            )
            .await,
            Err(TransportError::Invalid)
        );
        assert_eq!((a.hits(), b.hits()), (0, 0));
        assert_eq!(list(&a).await.unwrap(), vec!["m"]);
        assert_eq!(list(&b).await.unwrap(), vec!["m"]);
        assert_eq!((a.hits(), b.hits()), (1, 1));
        a.stop().await;
        b.stop().await;
    }

    #[tokio::test]
    async fn account_uses_only_bound_header_and_target() {
        let mock = Mock::start(
            response(200, br#"{"success":true,"data":{"quota":10}}"#, ""),
            false,
        )
        .await;
        let account = Account {
            base_url: mock.base.clone(),
            user_id: "42".into(),
            quota_per_unit: 2.0,
        };
        let value = balance(
            &client().unwrap(),
            permit(&mock.base, NetworkPurpose::Balance, None, Some("42")),
            &account,
        )
        .await
        .unwrap();
        assert_eq!(value["balance"], 5.0);
        let request = mock.request().to_ascii_lowercase();
        assert!(request.starts_with("get /api/user/self "));
        assert!(request.contains("new-api-user: 42\r\n"));
        assert!(request.contains("authorization: bearer fixture-secret-key\r\n"));
        mock.stop().await;
    }

    #[tokio::test]
    async fn json_limit_is_enforced_before_eof_and_exact_limit_succeeds() {
        let mut body = br#"{"data":[{"id":"m"}],"padding":""#.to_vec();
        body.resize(RESPONSE_LIMIT - 2, b'x');
        body.extend_from_slice(b"\"}");
        assert_eq!(body.len(), RESPONSE_LIMIT);
        let exact = Mock::start(response(200, &body, ""), false).await;
        assert_eq!(list(&exact).await.unwrap(), vec!["m"]);
        exact.stop().await;
        let mut oversized = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            RESPONSE_LIMIT + 1000
        )
        .into_bytes();
        oversized.resize(oversized.len() + RESPONSE_LIMIT + 1, b'x');
        let large = Mock::start(oversized, true).await;
        assert_eq!(list(&large).await, Err(TransportError::Unknown));
        assert_eq!(large.hits(), 1);
        large.stop().await;
    }

    #[tokio::test]
    async fn sse_exact_limit_finishes_and_over_limit_fails_before_eof() {
        let finish = b"data: {\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\n";
        let done = b"data: [DONE]\n\n";
        let mut exact_body = finish.to_vec();
        exact_body.push(b':');
        exact_body.resize(RESPONSE_LIMIT - done.len() - 1, b'x');
        exact_body.push(b'\n');
        exact_body.extend_from_slice(done);
        assert_eq!(exact_body.len(), RESPONSE_LIMIT);
        let exact = Mock::start(response(200, &exact_body, ""), false).await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            complete(
                &client().unwrap(),
                permit(&exact.base, NetworkPurpose::Completion, Some("m"), None),
                &route(&exact.base),
                &[],
                &[],
                |_| Ok(()),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.0["content"], "OK");
        assert_eq!(exact.hits(), 1);
        exact.stop().await;

        let mut wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/event-stream\r\n\r\n",
            RESPONSE_LIMIT + 1000
        )
        .into_bytes();
        wire.push(b':');
        wire.resize(wire.len() + RESPONSE_LIMIT, b'x');
        let oversized = Mock::start(wire, true).await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            complete(
                &client().unwrap(),
                permit(&oversized.base, NetworkPurpose::Completion, Some("m"), None),
                &route(&oversized.base),
                &[],
                &[],
                |_| Ok(()),
            ),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err(), TransportError::Unknown);
        assert_eq!(oversized.hits(), 1);
        oversized.stop().await;
    }

    #[tokio::test]
    async fn response_errors_distinguish_complete_invalid_and_truncated() {
        for (wire, expected) in [
            (
                response(200, b"not JSON fixture-secret-key", ""),
                TransportError::InvalidResponse { status: 200 },
            ),
            (
                response(200, br#"{"data":[{"bad":true}]}"#, ""),
                TransportError::InvalidResponse { status: 200 },
            ),
            (
                response(401, b"fixture-secret-key", ""),
                TransportError::Response { status: 401 },
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{}".to_vec(),
                TransportError::Unknown,
            ),
        ] {
            let mock = Mock::start(wire, false).await;
            let error = list(&mock).await.unwrap_err();
            assert_eq!(error, expected);
            assert!(!format!("{error:?} {error}").contains("fixture-secret-key"));
            assert_eq!(mock.hits(), 1);
            mock.stop().await;
        }
    }

    #[tokio::test]
    async fn sse_requires_finish_and_complete_done_and_never_ignores_read_failure() {
        let finished =
            "data: {\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\n";
        for body in [
            finished.to_string(),
            format!("{finished}data: [DONE]"),
            "data: [DONE]\n\n".into(),
            format!("{finished}data: broken\n\n"),
        ] {
            let mock = Mock::start(response(200, body.as_bytes(), ""), false).await;
            let result = complete(
                &client().unwrap(),
                permit(&mock.base, NetworkPurpose::Completion, Some("m"), None),
                &route(&mock.base),
                &[],
                &[],
                |_| Ok(()),
            )
            .await;
            assert_eq!(result.unwrap_err(), TransportError::Unknown);
            assert_eq!(mock.hits(), 1);
            mock.stop().await;
        }
        let mut truncated = b"HTTP/1.1 200 OK\r\nContent-Length: 9999\r\n\r\n".to_vec();
        truncated.extend_from_slice(finished.as_bytes());
        let mock = Mock::start(truncated, false).await;
        assert_eq!(
            complete(
                &client().unwrap(),
                permit(&mock.base, NetworkPurpose::Completion, Some("m"), None),
                &route(&mock.base),
                &[],
                &[],
                |_| Ok(())
            )
            .await
            .unwrap_err(),
            TransportError::Unknown
        );
        mock.stop().await;
        let good = format!("{finished}data: [DONE]\n\n");
        let mock = Mock::start(response(200, good.as_bytes(), ""), false).await;
        assert_eq!(
            complete(
                &client().unwrap(),
                permit(&mock.base, NetworkPurpose::Completion, Some("m"), None),
                &route(&mock.base),
                &[],
                &[],
                |_| Err(anyhow::anyhow!("fixture-secret-key"))
            )
            .await
            .unwrap_err(),
            TransportError::Unknown
        );
        mock.stop().await;
    }

    fn usage_stream(observations: &[Value], finish: &str, done: bool) -> String {
        let mut body = format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"content":"桃子"}}]})
        );
        for usage in observations {
            body.push_str(&format!(
                "data: {}\r\n\r\n",
                json!({"choices":[],"usage":usage})
            ));
        }
        body.push_str(&format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{},"finish_reason":finish}]})
        ));
        if done {
            body.push_str("data: [DONE]\n\n");
        }
        body
    }

    async fn usage_complete(mock: &Mock) -> std::result::Result<(Value, Value), TransportError> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            complete(
                &client().unwrap(),
                permit(&mock.base, NetworkPurpose::Completion, Some("m"), None),
                &route(&mock.base),
                &[],
                &[],
                |_| Ok(()),
            ),
        )
        .await
        .expect("local mock completion timed out")
    }

    #[tokio::test]
    async fn usage_real_transport_accepts_zero_normal_and_identical_snapshots_once() {
        use crate::provider_usage::{TokenUsage, UsageObservation, classify_usage};
        let normal = json!({"prompt_tokens":7,"completion_tokens":2,"total_tokens":9});
        let mut details = normal.clone();
        details["completion_tokens_details"] =
            json!({"reasoning_tokens":2,"secret":"do-not-retain"});
        for (observations, expected) in [
            (
                vec![json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0})],
                TokenUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                },
            ),
            (
                vec![normal.clone()],
                TokenUsage {
                    prompt_tokens: 7,
                    completion_tokens: 2,
                    total_tokens: 9,
                },
            ),
            (
                vec![normal, Value::Null, details],
                TokenUsage {
                    prompt_tokens: 7,
                    completion_tokens: 2,
                    total_tokens: 9,
                },
            ),
            (
                vec![
                    json!({"prompt_tokens":i64::MAX,"completion_tokens":0,"total_tokens":i64::MAX}),
                ],
                TokenUsage {
                    prompt_tokens: i64::MAX as u64,
                    completion_tokens: 0,
                    total_tokens: i64::MAX as u64,
                },
            ),
        ] {
            let body = usage_stream(&observations, "stop", true);
            // HTTP chunk boundaries include individual UTF-8 bytes and SSE delimiters.
            let mut wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/event-stream\r\n\r\n".to_vec();
            for byte in body.bytes() {
                wire.extend_from_slice(b"1\r\n");
                wire.push(byte);
                wire.extend_from_slice(b"\r\n");
            }
            wire.extend_from_slice(b"0\r\n\r\n");
            let mock = Mock::start(wire, false).await;
            let (message, usage) = usage_complete(&mock).await.unwrap();
            assert_eq!(message["content"], "桃子");
            assert_eq!(classify_usage(&usage), UsageObservation::Valid(expected));
            assert_eq!(usage.as_object().unwrap().len(), 3);
            assert!(!usage.to_string().contains("do-not-retain"));
            assert_eq!(mock.hits(), 1);
            let request = mock.request();
            let request_body: Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(request_body["stream_options"]["include_usage"], true);
            assert_eq!(request_body["max_tokens"], 128);
            mock.stop().await;
        }
    }

    #[tokio::test]
    async fn usage_real_transport_missing_and_invalid_keep_network_success_and_safe_values() {
        use crate::provider_usage::{
            UsageInvalidReason as R, UsageObservation as O, classify_usage,
        };
        let normal = json!({"prompt_tokens":7,"completion_tokens":2,"total_tokens":9});
        let cases = [
            (vec![], O::Missing),
            (vec![Value::Null], O::Missing),
            (vec![json!("do-not-retain")], O::Invalid(R::InvalidShape)),
            (
                vec![json!({"prompt_tokens":7})],
                O::Invalid(R::MissingField),
            ),
            (
                vec![json!({"prompt_tokens":7,"completion_tokens":-1,"total_tokens":6})],
                O::Invalid(R::InvalidInteger),
            ),
            (
                vec![json!({"prompt_tokens":7,"completion_tokens":2.0,"total_tokens":9})],
                O::Invalid(R::InvalidInteger),
            ),
            (
                vec![json!({"prompt_tokens":"7","completion_tokens":2,"total_tokens":9})],
                O::Invalid(R::InvalidInteger),
            ),
            (
                vec![
                    json!({"prompt_tokens":u64::MAX,"completion_tokens":0,"total_tokens":u64::MAX}),
                ],
                O::Invalid(R::OutOfRange),
            ),
            (
                vec![json!({"prompt_tokens":7,"completion_tokens":2,"total_tokens":10})],
                O::Invalid(R::InconsistentTotal),
            ),
            (
                vec![
                    normal.clone(),
                    json!({"prompt_tokens":8,"completion_tokens":1,"total_tokens":9}),
                    normal.clone(),
                ],
                O::Invalid(R::ConflictingDuplicate),
            ),
            (
                vec![
                    json!({"prompt_tokens":7}),
                    json!({"completion_tokens":2,"total_tokens":9}),
                    normal.clone(),
                ],
                O::Invalid(R::MissingField),
            ),
            (
                vec![
                    normal.clone(),
                    json!({"secret":"do-not-retain"}),
                    Value::Null,
                    normal,
                ],
                O::Invalid(R::MissingField),
            ),
        ];
        for (observations, expected) in cases {
            let body = usage_stream(&observations, "stop", true);
            let mock = Mock::start(response(200, body.as_bytes(), ""), false).await;
            let (message, usage) = usage_complete(&mock).await.unwrap();
            assert_eq!(message["content"], "桃子");
            assert_eq!(classify_usage(&usage), expected);
            assert!(!usage.to_string().contains("do-not-retain"));
            assert!(usage.get("total_tokens").is_none());
            assert_eq!(mock.hits(), 1);
            mock.stop().await;
        }
    }

    #[tokio::test]
    async fn usage_real_transport_never_salvages_usage_from_failed_completion_or_retries() {
        let usage = json!({"prompt_tokens":7,"completion_tokens":2,"total_tokens":9});
        for (status, finish, done, expected) in [
            (200, "stop", false, TransportError::Unknown),
            (200, "length", true, TransportError::Unknown),
            (200, "content_filter", true, TransportError::Unknown),
            (429, "stop", true, TransportError::Response { status: 429 }),
            (500, "stop", true, TransportError::Response { status: 500 }),
        ] {
            let body = usage_stream(std::slice::from_ref(&usage), finish, done);
            let mock = Mock::start(response(status, body.as_bytes(), ""), false).await;
            assert_eq!(usage_complete(&mock).await.unwrap_err(), expected);
            assert_eq!(mock.hits(), 1);
            mock.stop().await;
        }
    }

    #[tokio::test]
    async fn usage_real_transport_done_does_not_wait_for_connection_close() {
        use crate::provider_usage::{UsageObservation, classify_usage};
        let usage = json!({"prompt_tokens":7,"completion_tokens":2,"total_tokens":9});
        let body = usage_stream(std::slice::from_ref(&usage), "stop", true);
        // Claim an unfinished HTTP body, but send a complete SSE DONE. The protocol
        // completion is sufficient; bytes arriving later are deliberately unobserved.
        let mut wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            body.len() + 100
        )
        .into_bytes();
        wire.extend_from_slice(body.as_bytes());
        let mock = Mock::start(wire, true).await;
        let (message, observed) = usage_complete(&mock).await.unwrap();
        assert_eq!(message["content"], "桃子");
        assert!(matches!(
            classify_usage(&observed),
            UsageObservation::Valid(_)
        ));
        assert_eq!(observed, usage);
        assert_eq!(mock.hits(), 1);
        mock.stop().await;
    }

    #[tokio::test]
    async fn proxy_environment_child() {
        let Ok(mode) = std::env::var("NHC_D_PROXY_CHILD") else {
            return;
        };
        let mock = Mock::start(response(200, br#"{"data":[{"id":"m"}]}"#, ""), false).await;
        let target = format!("{}/models", mock.base);
        if mode != "explicit" {
            // Same process, target, proxy environment and request: only no_proxy differs.
            assert_eq!(list(&mock).await.unwrap(), vec!["m"]);
            assert_eq!(mock.hits(), 1);
        }
        let mut builder = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never());
        if mode == "explicit" {
            builder = builder
                .proxy(reqwest::Proxy::all(std::env::var("NHC_D_PROXY_URL").unwrap()).unwrap());
        }
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            builder
                .build()
                .unwrap()
                .get(&target)
                .bearer_auth("fixture-secret-key")
                .send(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.status(), 200);
        assert_eq!(
            result.json::<Value>().await.unwrap()["data"][0]["id"],
            "proxy-sentinel"
        );
        assert_eq!(mock.hits(), usize::from(mode != "explicit"));
        mock.stop().await;
    }

    #[tokio::test]
    async fn proxy_environment_isolated_process_has_zero_proxy_connections_with_positive_control() {
        let proxy = Mock::start(
            response(200, br#"{"data":[{"id":"proxy-sentinel"}]}"#, ""),
            false,
        )
        .await;
        for (index, mode) in ["paired-upper", "paired-lower", "explicit"]
            .into_iter()
            .enumerate()
        {
            let url = proxy.base.clone();
            let status = tokio::task::spawn_blocking(move || {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args([
                        "--exact",
                        "provider::network_tests::proxy_environment_child",
                        "--nocapture",
                    ])
                    .env("NHC_D_PROXY_CHILD", mode)
                    .env("NHC_D_PROXY_URL", &url)
                    .env_remove("REQUEST_METHOD");
                for name in [
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "ALL_PROXY",
                    "NO_PROXY",
                    "http_proxy",
                    "https_proxy",
                    "all_proxy",
                    "no_proxy",
                ] {
                    command.env_remove(name);
                }
                let names = if mode == "paired-lower" {
                    ["http_proxy", "https_proxy", "all_proxy", "no_proxy"]
                } else {
                    ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"]
                };
                for name in &names[..3] {
                    command.env(name, &url);
                }
                command.env(
                    names[3],
                    if mode == "paired-lower" {
                        "unrelated.invalid"
                    } else {
                        ""
                    },
                );
                command.output().unwrap()
            })
            .await
            .unwrap();
            assert!(
                status.status.success(),
                "{} {}",
                String::from_utf8_lossy(&status.stdout),
                String::from_utf8_lossy(&status.stderr)
            );
            assert_eq!(proxy.hits(), index + 1);
            assert!(proxy.request().starts_with("GET http://127.0.0.1:"));
        }
        proxy.stop().await;
    }
}
