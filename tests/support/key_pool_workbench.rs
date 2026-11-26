//! Verify the credentials sent on the wire and the receipts persisted by a real workbench process.
use super::*;
use axum::{body::Body, http::StatusCode, response::Response};
use coda::{
    budget::{BudgetCallState, BudgetConfig, ConfigureBudget},
    network_authorization::CallResult,
    project_config::RegisterProject,
    store::Store,
};
use std::{convert::Infallible, process::Stdio, time::Duration};

const PRIMARY: &str = "qa_primary_c1f9d3734d754feb887a";
const SECOND: &str = "qa_second_fcac34f3e9a24df2b262";
const THIRD: &str = "qa_third_d67972de0d55415dba82";
const BUSINESS: &str =
    "let cancel_token = \"Bearer example\";\nlet password = \"sk-learn\"; // 业务";
const REASON: &str = "checking member ";
const MASKED_REASON: &str = "checking member [redacted] and [redacted]";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Rotate,
    Explore,
    Unauthorized,
    Forbidden,
    Interrupted,
}

#[derive(Clone)]
struct Fixture {
    scenario: Scenario,
    arrivals: Arc<Mutex<Vec<(String, Value)>>>,
}

fn delta(value: Value, finish: Option<&str>) -> String {
    let chunk = json!({"choices":[{"delta":value,"finish_reason":finish}]});
    format!("data: {chunk}\n\n")
}

fn sse(frames: Vec<String>) -> Response {
    let stream = async_stream::stream! {
        for frame in frames {
            yield Ok::<_,Infallible>(frame);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    (
        [("content-type", "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn model(State(f): State<Fixture>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let attempt = {
        let mut arrivals = f.arrivals.lock().unwrap();
        arrivals.push((authorization.to_owned(), body));
        arrivals.len()
    };
    match f.scenario {
        Scenario::Unauthorized => return StatusCode::UNAUTHORIZED.into_response(),
        Scenario::Forbidden => return StatusCode::FORBIDDEN.into_response(),
        Scenario::Explore if attempt == 2 => return StatusCode::TOO_MANY_REQUESTS.into_response(),
        Scenario::Rotate if attempt == 1 => {
            return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "120")]).into_response();
        }
        _ => {}
    }
    if attempt > 4 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut frames = Vec::new();
    if f.scenario == Scenario::Explore {
        let (value, finish) = match attempt {
            1 => {
                let args = json!({"question":"What does the probe do?", "paths":["probe.txt"]});
                (
                    json!({"tool_calls":[{"index":0,"id":"probe","type":"function","function":{
                        "name":"explore_files", "arguments":args.to_string()
                    }}]}),
                    "tool_calls",
                )
            }
            3 => (
                json!({"content":format!("checked the probe: {SECOND}")}),
                "stop",
            ),
            _ => return StatusCode::BAD_REQUEST.into_response(),
        };
        frames.push(delta(value, Some(finish)));
    } else if f.scenario == Scenario::Rotate && attempt < 4 {
        frames.push(delta(
            json!({"tool_calls":[{"index":0,"id":format!("read-{attempt}"),"type":"function",
            "function":{"name":"todo_read","arguments":"{}"}}]}),
            Some("tool_calls"),
        ));
    } else {
        let (r1, r2) = THIRD.split_at(16);
        let (p1, p2) = PRIMARY.split_at(17);
        let (b1, b2) = SECOND.split_at(15);
        frames.extend([
            delta(json!({"reasoning_content":format!("{REASON}{r1}")}), None),
            delta(json!({"reasoning_content":format!("{r2} and {p1}")}), None),
            delta(json!({"reasoning_content":p2}), None),
            delta(json!({"content":format!("{BUSINESS}\nmember={b1}")}), None),
            delta(json!({"content":format!("{b2}.")}), None),
        ]);
        if f.scenario == Scenario::Interrupted {
            return sse(frames); // EOF after real deltas, before any completion marker.
        }
        frames.push(delta(json!({}), Some("stop")));
    }
    frames.push("data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\ndata: [DONE]\n\n".into());
    sse(frames)
}

async fn exercise(scenario: Scenario) {
    let f = Fixture {
        scenario,
        arrivals: Arc::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(model))
        .with_state(f.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let workspace = tmp.path().join("project");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    if scenario == Scenario::Explore {
        std::fs::write(
            workspace.join("probe.txt"),
            format!("probe marker {SECOND}"),
        )
        .unwrap();
    }
    let db_path = data.join("coda.sqlite3");
    let project = {
        let store = Store::open(&db_path).unwrap();
        coda::store::fresh_settings(&store, &workspace).unwrap();
        let project = store
            .register_project(RegisterProject {
                name: "project".into(),
                root_path: workspace.to_string_lossy().into_owned(),
                idempotency_key: "key-pool-fixture".into(),
            })
            .unwrap();
        store
            .configure_budget(ConfigureBudget {
                project_id: project.id.clone(),
                expected_revision: 0,
                idempotency_key: "key-pool-budget".into(),
                config: BudgetConfig {
                    limit_tokens: if scenario == Scenario::Explore {
                        6
                    } else {
                        100
                    },
                    per_call_reservation_tokens: 4,
                },
            })
            .unwrap();
        project
    };
    let pools = json!([{"routeId":"default","baseUrl":base,"keyEnvs":["QA_ALT_ONE","QA_ALT_TWO"]}]);
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_coda"));
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .env("CODA_KEY_POOLS", pools.to_string())
        .env("QA_ALT_ONE", SECOND)
        .env("QA_ALT_TWO", THIRD)
        .env("OPENAI_API_KEY", PRIMARY)
        .env("OPENAI_BASE_URL", &base)
        .env("CODA_MODEL", "gpt-test-key-pool")
        .args(["ask", "--data-dir"])
        .arg(&data)
        .arg("--dir")
        .arg(&workspace)
        .arg("Read the current todo list.");
    let output = tokio::time::timeout(Duration::from_secs(25), command.output())
        .await
        .expect("coda ask timed out")
        .unwrap();
    server.abort();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.success(),
        scenario == Scenario::Rotate,
        "{stderr}"
    );
    let store = Store::open(&db_path).unwrap();
    let id = store.runs().unwrap()[0]["id"].as_str().unwrap().to_owned();
    let run = store.run(&id).unwrap();
    let task = &run.tasks[0];
    let events = store.events(&id, 0).unwrap();
    let calls = store.network_calls_for_task(&task.id).unwrap();
    let arrivals = f.arrivals.lock().unwrap();
    let bodies: Vec<_> = arrivals.iter().map(|(_, body)| body).collect();
    let durable = json!({"task":task,"events":events,"calls":calls}).to_string();
    for secret in [PRIMARY, SECOND, THIRD] {
        assert!(!durable.contains(secret) && !stdout.contains(secret) && !stderr.contains(secret));
        assert!(!json!(bodies).to_string().contains(secret));
    }
    let expected = match scenario {
        Scenario::Rotate => vec![PRIMARY, SECOND, THIRD, SECOND],
        Scenario::Explore => vec![PRIMARY, SECOND, THIRD],
        _ => vec![PRIMARY],
    };
    assert_eq!(arrivals.len(), expected.len());
    assert_eq!(calls.len(), arrivals.len());
    for (i, ((auth, _), call)) in arrivals.iter().zip(&calls).enumerate() {
        assert_eq!(auth, &format!("Bearer {}", expected[i]));
        let slot = [PRIMARY, SECOND, THIRD]
            .iter()
            .position(|&key| key == expected[i])
            .unwrap();
        assert_eq!(call.credential_slot, Some(slot));
        assert_eq!(call.subject.id, "default");
        assert_eq!(call.subject.project_id.as_ref(), Some(&project.id));
        assert_eq!(call.task_id.as_ref(), Some(&task.id));
        assert_eq!(call.authorization_id, calls[0].authorization_id);
        let budget = store.budget_call(&project.id, &call.call_id).unwrap();
        assert_eq!(budget.call, *call);
        assert_eq!(budget.reserved_tokens, Some(4));
        let state = if (scenario == Scenario::Rotate && i > 0)
            || (scenario == Scenario::Explore && i != 1)
        {
            BudgetCallState::Settled
        } else {
            BudgetCallState::Unknown
        };
        assert_eq!(budget.state, Some(state));
    }
    let expected_http = match scenario {
        Scenario::Explore => {
            assert_eq!(calls[0].result, Some(CallResult::Success));
            assert_eq!(calls[1].http_status, Some(429));
            assert_eq!(calls[1].result, Some(CallResult::HttpFailure));
            assert_eq!(calls[2].result, Some(CallResult::Success));
            for body in &bodies[1..] {
                assert!(body.get("tools").is_none() || body["tools"] == json!([]));
                let messages = body["messages"].to_string();
                assert!(
                    messages.contains("What does the probe do?")
                        && messages.contains("probe marker [redacted]")
                );
                assert!(!messages.contains("Read the current todo list."));
            }
            assert_eq!(task.status, "failed");
            let code = coda::budget::BudgetErrorKind::Exceeded.code();
            assert!(
                task.error
                    .as_deref()
                    .is_some_and(|error| error.contains(code))
            );
            assert_eq!(task.usage["total_tokens"], 6);
            assert!(task.messages.iter().any(|m| {
                m["role"] == "tool"
                    && m["tool_call_id"] == "probe"
                    && m["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("checked the probe: [redacted]"))
            }));
            let status = store.budget_status(&project.id).unwrap();
            assert_eq!(
                (
                    status.spent_tokens,
                    status.held_tokens,
                    status.available_tokens
                ),
                (6, 0, Some(0))
            );
            return;
        }
        Scenario::Rotate => Some(429),
        Scenario::Unauthorized => Some(401),
        Scenario::Forbidden => Some(403),
        Scenario::Interrupted => None,
    };
    assert_eq!(calls[0].http_status, expected_http);
    assert_eq!(
        calls[0].result,
        Some(if expected_http.is_some() {
            CallResult::HttpFailure
        } else {
            CallResult::TransportUnknown
        })
    );
    assert_eq!(
        store.budget_status(&project.id).unwrap().spent_tokens,
        if scenario == Scenario::Rotate { 9 } else { 0 }
    );
    if matches!(scenario, Scenario::Rotate | Scenario::Interrupted) {
        let joined = |kind: &str| -> String {
            events
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.data["text"].as_str().unwrap())
                .collect()
        };
        assert_eq!(joined("reasoning_delta"), MASKED_REASON);
        assert!(!joined("delta").contains(REASON));
        assert!(joined("delta").contains(BUSINESS));
        if scenario == Scenario::Rotate {
            assert_eq!(task.output, format!("{BUSINESS}\nmember=[redacted]."));
            assert_eq!(task.usage["total_tokens"], 9);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "provider credentials require Windows DPAPI")]
async fn key_pool_rotates_after_429_and_protects_streams_and_receipts() {
    exercise(Scenario::Rotate).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "provider credentials require Windows DPAPI")]
async fn key_pool_stops_after_permanent_http_failures_and_partial_stream() {
    for scenario in [
        Scenario::Unauthorized,
        Scenario::Forbidden,
        Scenario::Interrupted,
    ] {
        exercise(scenario).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "provider credentials require Windows DPAPI")]
async fn exploration_rotates_members_and_consumes_the_parent_budget() {
    exercise(Scenario::Explore).await;
}
