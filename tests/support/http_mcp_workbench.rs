//! Exercise remote MCP through the real in-process workbench and its approval loop.
use super::*;
use axum::{http::StatusCode, response::Response};
use coda::{approval::ApprovalStatus, store::Store};
use std::{process::Stdio, time::Duration};

const TOOL: &str = "mcp__demo__echo";
const TOKEN: &str = "test_28a718fab9f34e7eaeb64cac79ac9132";
const SESSION: &str = "sid_0159c8b6e0494a4485de60ce3df4938d";
const PROTOCOL: &str = "2025-06-18";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Approve,
    Deny,
    Disconnect,
    Neighbor,
}

#[derive(Clone)]
struct Fixture {
    scenario: Scenario,
    model_requests: Arc<Mutex<Vec<Value>>>,
    mcp_requests: Arc<Mutex<Vec<Value>>>,
}

async fn model(State(f): State<Fixture>, Json(request): Json<Value>) -> Response {
    let round = {
        let mut requests = f.model_requests.lock().unwrap();
        requests.push(request);
        requests.len()
    };
    let (delta, finish) = if round <= 2 && f.scenario != Scenario::Neighbor {
        let args = json!({"text": format!("message-{round}")});
        (
            json!({"tool_calls":[{"index":0,"id":format!("call-{round}"),"type":"function",
            "function":{"name":TOOL, "arguments":args.to_string()}}]}),
            "tool_calls",
        )
    } else if round <= 3 {
        (json!({"content":"workflow complete"}), "stop")
    } else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":finish}],
        "usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
        .into_response()
}

async fn mcp(State(f): State<Fixture>, headers: HeaderMap, Json(request): Json<Value>) -> Response {
    f.mcp_requests.lock().unwrap().push(request.clone());
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(format!("Bearer {TOKEN}").as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let method = request["method"].as_str().unwrap_or("");
    if method != "initialize"
        && !(headers.get("mcp-session-id").is_some_and(|v| v == SESSION)
            && headers
                .get("mcp-protocol-version")
                .is_some_and(|v| v == PROTOCOL))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let id = &request["id"];
    match method {
        "initialize" => (
            [("mcp-session-id", SESSION)],
            Json(json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":PROTOCOL,
                "capabilities":{"tools":{}},"serverInfo":{"name":"test-mcp","version":"1"}}})),
        ).into_response(),
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => Json(json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{
            "name":"echo", "description":format!("Echo the given text ({TOKEN}; {SESSION})"), "inputSchema":{"type":"object",
            "properties":{"text":{"type":"string"}},"required":["text"]}},
            {"name":"credential_schema","inputSchema":{"type":"object","properties":{
                (TOKEN):{"type":"string"},(SESSION):{"type":"string"}
            }}}]}})).into_response(),
        "tools/call" => {
            let body = if f.scenario == Scenario::Disconnect {
                // Valid SSE event followed by EOF without the matching JSON-RPC result.
                "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progressToken\":1,\"progress\":1}}\n\n".into()
            } else {
                let text = request.pointer("/params/arguments/text").and_then(Value::as_str).unwrap_or("");
                let reply = json!({"jsonrpc":"2.0","id":id,"result":{
                    "content":[{"type":"text","text":format!("remote-result-{text}")}],
                    "structuredContent":{(TOKEN): "bearer field value", (SESSION): "session field value"}
                }});
                format!("event: message\ndata: {reply}\n\n")
            };
            ([("content-type","text/event-stream")], body).into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn exercise(scenario: Scenario) {
    let f = Fixture {
        scenario,
        model_requests: Arc::default(),
        mcp_requests: Arc::default(),
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(model))
        .route(
            "/mcp",
            post(mcp).delete(|| async { StatusCode::NO_CONTENT }),
        )
        .with_state(f.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let url = format!("{base}/mcp");
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("project");
    let neighbor = tmp.path().join("project-next");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&neighbor).unwrap();
    let entries = json!([{"workspace": workspace, "name":"demo","url":url,"bearerTokenEnv":"DEMO_MCP_TOKEN"}]);
    let mut ask = tokio::process::Command::new(env!("CARGO_BIN_EXE_coda"));
    ask.kill_on_drop(true)
        .env("CODA_MCP_HTTP_SERVERS", entries.to_string())
        .env("DEMO_MCP_TOKEN", TOKEN)
        .env("OPENAI_API_KEY", "sk-test-model-only")
        .env("OPENAI_BASE_URL", format!("{base}/v1"))
        .env("CODA_MODEL", "gpt-local")
        .stdin(Stdio::null())
        .args(["ask", "--data-dir"])
        .arg(&data)
        .arg("--dir")
        .arg(if scenario == Scenario::Neighbor {
            &neighbor
        } else {
            &workspace
        });
    if scenario != Scenario::Deny {
        ask.arg("-y");
    }
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        ask.arg("Test remote echo").output(),
    )
    .await
    .expect("coda ask did not finish")
    .unwrap();
    server.abort();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for private in [TOKEN, SESSION] {
        assert!(!stdout.contains(private) && !stderr.contains(private));
    }
    assert_eq!(
        output.status.success(),
        scenario != Scenario::Disconnect,
        "{stderr}"
    );

    let requests = f.model_requests.lock().unwrap();
    let wire = f.mcp_requests.lock().unwrap();
    let methods: Vec<_> = wire.iter().map(|r| r["method"].as_str().unwrap()).collect();
    let advertised = |r: &Value| {
        r["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == TOOL)
    };
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|r| !r.to_string().contains(TOKEN) && !r.to_string().contains(SESSION))
    );
    if scenario == Scenario::Neighbor {
        assert_eq!(requests.len(), 1);
        assert!(!advertised(&requests[0]));
        assert!(methods.is_empty(), "{methods:?}");
        return;
    }
    assert!(requests.iter().all(advertised));
    for request in requests.iter() {
        let tools = request["tools"].as_array().unwrap();
        assert!(
            !tools
                .iter()
                .any(|tool| tool["function"]["name"] == "mcp__demo__credential_schema")
        );
        let echo = tools
            .iter()
            .find(|tool| tool["function"]["name"] == TOOL)
            .unwrap();
        assert!(
            echo["function"]["description"]
                .as_str()
                .unwrap()
                .contains("Echo the given text")
        );
    }
    assert_eq!(methods.iter().filter(|&&m| m == "initialize").count(), 1);
    assert_eq!(methods.iter().filter(|&&m| m == "tools/list").count(), 1);
    let actual_calls: Vec<_> = wire
        .iter()
        .filter(|r| r["method"] == "tools/call")
        .map(|r| {
            (
                r["params"]["name"].as_str().unwrap(),
                r["params"]["arguments"]["text"].as_str().unwrap(),
            )
        })
        .collect();

    let store = Store::open(&data.join("coda.sqlite3")).unwrap();
    let run_id = store.runs().unwrap()[0]["id"].as_str().unwrap().to_owned();
    let run = store.run(&run_id).unwrap();
    let approvals = store.approvals_for_task(&run.tasks[0].id).unwrap();
    let history = json!({"messages": run.tasks[0].messages, "events": store.events(&run_id, 0).unwrap(), "approvals": approvals}).to_string();
    for private in [TOKEN, SESSION] {
        assert!(
            !history.contains(private),
            "Remote credential appeared in persisted task history"
        );
    }
    let expected = if scenario == Scenario::Disconnect {
        1
    } else {
        2
    };
    assert_eq!(approvals.len(), expected);
    assert_eq!(stderr.matches(&url).count(), expected, "{stderr}");
    let expected_status = if scenario == Scenario::Deny {
        ApprovalStatus::Denied
    } else {
        ApprovalStatus::Approved
    };
    assert!(
        approvals
            .iter()
            .all(|a| a.status == expected_status && a.tool_name == TOOL)
    );
    match scenario {
        Scenario::Approve => {
            assert_eq!(requests.len(), 3);
            assert_eq!(actual_calls, [("echo", "message-1"), ("echo", "message-2")]);
            for round in [1, 2] {
                let tool = requests[round]["messages"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap();
                assert_eq!(tool["role"], "tool");
                let result: Value =
                    serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
                assert_eq!(
                    result["content"][0]["text"],
                    format!("remote-result-message-{round}")
                );
                let fields = result["structuredContent"].as_object().unwrap();
                assert!(fields.values().any(|v| v == "bearer field value"));
                assert!(fields.values().any(|v| v == "session field value"));
            }
        }
        Scenario::Deny => {
            assert_eq!(requests.len(), 3);
            assert!(actual_calls.is_empty());
            for request in requests.iter().skip(1) {
                let tool = request["messages"].as_array().unwrap().last().unwrap();
                assert!(tool["content"].as_str().unwrap().contains("拒绝"));
            }
        }
        Scenario::Disconnect => {
            assert_eq!(requests.len(), 1);
            assert_eq!(actual_calls, [("echo", "message-1")]);
            assert_eq!(run.tasks[0].status, "failed");
            assert!(run.tasks[0].error.as_deref().unwrap().contains("未知"));
        }
        Scenario::Neighbor => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "provider credentials require Windows DPAPI")]
async fn approved_calls_share_http_mcp_session_and_denied_calls_never_dispatch() {
    exercise(Scenario::Approve).await;
    exercise(Scenario::Deny).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "provider credentials require Windows DPAPI")]
async fn http_mcp_disconnect_is_unknown_and_neighbor_cannot_discover_endpoint() {
    exercise(Scenario::Disconnect).await;
    exercise(Scenario::Neighbor).await;
}
