use super::*;
use axum::{Json, Router, extract::State, routing::post};
use std::time::{Duration, Instant};

#[path = "../../tests/support/network_fixture.rs"]
mod network_fixture;

#[derive(Clone, Default)]
struct Script(Arc<Mutex<Vec<(Instant, Value)>>>);

async fn completion(
    State(script): State<Script>,
    Json(body): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let mut calls = script.0.lock().unwrap();
    calls.push((Instant::now(), body));
    let (delta, reason) = match calls.len() {
        round @ (1 | 2) => {
            let (name, args) = if round == 1 {
                ("computer_observe", json!({}))
            } else {
                (
                    "computer_wait",
                    json!({"condition":"dialog_foreground","timeout_ms":200}),
                )
            };
            (
                json!({"tool_calls":[{"index":0,"id":format!("call-{round}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]}),
                "tool_calls",
            )
        }
        _ => (json!({"content":"done"}), "stop"),
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":reason}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

async fn run_fixture(
    workspace: &Path,
    computer: crate::computer::ComputerService,
    tools: bool,
) -> (Task, Vec<(Instant, Value)>) {
    let script = Script::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(completion))
        .with_state(script.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let db = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&db.path().join("test.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: workspace.to_string_lossy().into_owned(),
            routes: vec![Route {
                id: "mock".into(),
                name: "mock".into(),
                base_url: base,
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 1,
                key_env: None,
            }],
            max_concurrency: 1,
            newapi: None,
        })
        .unwrap();
    store
        .put_secret("mock", "synthetic-desktop-test-key")
        .unwrap();
    network_fixture::authorize_routes(&store);
    let recovery = host_services::RecoveryService::new(store.clone()).unwrap();
    recovery.recover().unwrap();
    let mut composition = composition::HostComposition::builtin(store.clone(), recovery).unwrap();
    composition.computer = Arc::new(computer);
    let engine = Engine::assemble_composition(composition, 1).unwrap();
    let run = engine
        .start(RunRequest {
            kind: SessionKind::Chat,
            title: "desktop test".into(),
            tasks: vec![TaskSpec {
                name: "reader".into(),
                role: "reader".into(),
                route_id: "mock".into(),
                prompt: "check the registered target".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools,
                allow_commands: false,
                max_rounds: 4,
            }],
        })
        .await
        .unwrap();
    let task = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let task = store.task(&run.tasks[0].id).unwrap();
            if !matches!(task.status.as_str(), "queued" | "running") && !engine.is_busy() {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    server.abort();
    let requests = std::mem::take(&mut *script.0.lock().unwrap());
    (task, requests)
}

#[tokio::test]
async fn registered_target_results_reach_next_model_round_after_local_wait() {
    let workspace = tempfile::tempdir().unwrap();
    let fixture = crate::computer::ComputerService::fixture(workspace.path());
    let (task, requests) = run_fixture(workspace.path(), fixture, true).await;
    assert_eq!(task.status, "completed", "{:?}", task.error);
    assert_eq!(requests.len(), 3);
    let names: Vec<_> = requests[0].1["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(names.contains(&"computer_observe") && names.contains(&"computer_wait"));
    let reply = |index: usize| {
        let last = requests[index].1["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(last["role"], "tool");
        serde_json::from_str::<Value>(last["content"].as_str().unwrap()).unwrap()
    };
    assert_eq!(reply(1)["title"], "Fixture");
    assert_eq!(reply(2)["outcome"], "timeout");
    assert!(requests[2].0.duration_since(requests[1].0) >= Duration::from_millis(200));
}

#[tokio::test]
async fn forged_observation_is_rejected_without_workspace_registration_or_task_tools() {
    let workspace = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    for (computer, tools) in [
        (crate::computer::ComputerService::default(), true),
        (
            crate::computer::ComputerService::fixture(other.path()),
            true,
        ),
        (
            crate::computer::ComputerService::fixture(workspace.path()),
            false,
        ),
    ] {
        let (task, requests) = run_fixture(workspace.path(), computer, tools).await;
        assert_eq!(task.status, "failed", "{:?}", task.error);
        assert!(task.error.unwrap().contains("未授权"));
        assert_eq!(requests.len(), 1);
        assert!(
            !requests[0].1["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|t| t["function"]["name"]
                    .as_str()
                    .is_some_and(crate::tool_results::is_computer))
        );
        assert!(
            !task
                .messages
                .iter()
                .any(|m| m["content"].as_str().is_some_and(|c| c.contains("Fixture")))
        );
    }
}
