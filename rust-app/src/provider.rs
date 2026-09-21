use crate::{domain::*, secrets::scrub};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
fn endpoint(base: &str, tail: &str) -> String {
    format!("{}/{tail}", base.trim_end_matches('/'))
}
async fn json_response(response: reqwest::Response, key: &str) -> Result<Value> {
    let status = response.status();
    let text = response.text().await.context("读取上游响应失败")?;
    ensure!(text.len() <= 4_000_000, "上游响应过大");
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        let message = value
            .pointer("/error/message")
            .or(value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("上游未返回有效错误说明");
        bail!("上游 HTTP {status}：{}", scrub(message, key));
    }
    ensure!(!value.is_null(), "上游没有返回有效 JSON");
    Ok(value)
}
pub async fn models(client: &reqwest::Client, route: &Route, key: &str) -> Result<Vec<String>> {
    let response = client
        .get(endpoint(&route.base_url, "models"))
        .bearer_auth(key)
        .send()
        .await
        .context("模型列表连接失败")?;
    let value = json_response(response, key).await?;
    let list = value["data"].as_array().context("模型列表缺少 data")?;
    Ok(list
        .iter()
        .filter_map(|v| v["id"].as_str().or(v.as_str()).map(String::from))
        .collect())
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
}
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&str) -> Result<()>) -> Result<()> {
        self.size += bytes.len();
        ensure!(self.size <= 4_000_000, "模型输出超过安全上限");
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
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let chunk: Value = serde_json::from_str(&data).context("无效的流式响应")?;
        if !chunk["error"].is_null() {
            bail!("模型流返回错误");
        }
        if chunk["usage"].is_object() {
            self.usage = chunk["usage"].clone();
        }
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
    pub fn finish(mut self, emit: &mut impl FnMut(&str) -> Result<()>) -> Result<Value> {
        if !self.buffer.is_empty() {
            self.buffer.push(b'\n');
            self.feed(&[], emit)?;
        }
        self.flush(emit)?;
        ensure!(
            self.done || self.finish.is_some(),
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
pub async fn complete(
    client: &reqwest::Client,
    route: &Route,
    key: &str,
    messages: &[Value],
    tools: &[Value],
    mut emit: impl FnMut(&str) -> Result<()> + Send,
) -> Result<(Value, Value)> {
    let mut body = json!({"model":route.model,"messages":messages,"max_tokens":route.max_tokens,"stream":true,"stream_options":{"include_usage":true}});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
    }
    let response = client
        .post(endpoint(&route.base_url, "chat/completions"))
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .context("模型连接失败")?;
    if !response.status().is_success() {
        json_response(response, key).await?;
        bail!("上游调用失败");
    }
    let mut decoder = Decoder::default();
    let mut bytes = response.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        match chunk {
            Ok(chunk) => decoder.feed(&chunk, &mut emit)?,
            Err(_) if decoder.finish.is_some() => break,
            Err(error) => return Err(error).context("模型流连接中断"),
        }
        if decoder.done {
            break;
        }
    }
    let usage = decoder.usage.clone();
    Ok((decoder.finish(&mut emit)?, usage))
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
pub async fn balance(client: &reqwest::Client, account: &Account, token: &str) -> Result<Value> {
    let response = client
        .get(endpoint(&account.base_url, "api/user/self"))
        .bearer_auth(token)
        .header("New-Api-User", &account.user_id)
        .send()
        .await
        .context("New API 连接失败")?;
    normalize_balance(json_response(response, token).await?, account)
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
            complete(&client().unwrap(), &route, "fake", &[], &[], |_| Ok(())),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.0["content"], "OK");
        server.abort();
    }
}
