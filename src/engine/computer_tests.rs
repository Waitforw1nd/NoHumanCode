use super::*;
use axum::{Json, Router, extract::State, routing::post};
use std::time::{Duration, Instant};

#[path = "../../tests/support/network_fixture.rs"]
mod network_fixture;

#[derive(Clone, Default)]
struct Script(Arc<Mutex<Vec<(Instant, Value)>>>, Option<SaveCase>);

#[derive(Debug, Clone, Copy, PartialEq)]
enum SaveCase {
    Approve,
    Deny,
    Conflict,
    Outside,
    NoWrite,
    Mismatch,
    Stale,
}

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
            } else if let Some(case) = script.1 {
                let reply: Value = serde_json::from_str(
                    calls.last().unwrap().1["messages"]
                        .as_array()
                        .unwrap()
                        .last()
                        .unwrap()["content"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                (
                    "computer_save_as",
                    json!({"observation_ref":reply["observation_ref"],"path": if case == SaveCase::Outside {"other.txt"} else {"saved.txt"}, "expected_text":"editor text"}),
                )
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
    case: Option<SaveCase>,
) -> (Task, Vec<(Instant, Value)>) {
    let script = Script(Default::default(), case);
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
                protocol: None,
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
    if let Some(case) = case.filter(|case| *case != SaveCase::Stale) {
        // Simulate only the application's native Save action, not the verifier.
        // Its editor buffer is independent of the model's expected_text.
        fn schedule_save(path: &Path, text: &'static [u8]) -> Result<()> {
            let path = path.to_owned();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(75)).await;
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .unwrap()
                    .write_all(text)
                    .unwrap();
            });
            Ok(())
        }
        engine
            .tool_calls
            .save_fixture
            .set(match case {
                SaveCase::Mismatch => |p| schedule_save(p, b"different editor text"),
                SaveCase::Approve => |p| schedule_save(p, b"editor text"),
                _ => |_| panic!("denied or invalid save was dispatched"),
            })
            .unwrap();
    }
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
                write_scopes: case
                    .filter(|c| *c != SaveCase::NoWrite)
                    .map_or_else(Vec::new, |_| vec!["saved.txt".into()]),
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
            if let Some(case) = case
                && let Some(record) = store.pending_approvals_for_task(&task.id).unwrap().first()
            {
                assert!(!matches!(case, SaveCase::Outside | SaveCase::NoWrite));
                assert!(!workspace.join("saved.txt").exists());
                assert!(
                    record.preview.contains("absent")
                        && record.preview.contains("editor text")
                        && record.preview.contains("absolute_path")
                );
                assert_eq!(record.tool_name, "computer_save_as");
                if case == SaveCase::Conflict {
                    std::fs::write(workspace.join("saved.txt"), "external writer").unwrap();
                }
                engine
                    .decide_approval(&record.id, case != SaveCase::Deny, Some("fixture user"))
                    .unwrap();
            }
            if !matches!(task.status.as_str(), "queued" | "running") && !engine.is_busy() {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    server.abort();
    if let Some(case) = case {
        let records = store.approvals_for_task(&task.id).unwrap();
        let changes = store.workspace_changes_for_task(&task.id).unwrap();
        match case {
            SaveCase::Approve => {
                assert_eq!(task.status, "completed", "{:?}", task.error);
                assert_eq!(records.len(), 1);
                assert_eq!(
                    records[0].execution_state,
                    approval::ExecutionState::Finished
                );
                assert_eq!(
                    std::fs::read(workspace.join("saved.txt")).unwrap(),
                    b"editor text"
                );
                assert_eq!(changes.len(), 1);
                assert!(changes[0].before_digest.is_none());
                assert_eq!(
                    changes[0].state,
                    crate::workspace_changes::ChangeState::Finished
                );
                assert_eq!(engine.restore(&task.id).await.unwrap().restored, 1);
                assert!(!workspace.join("saved.txt").exists());
            }
            SaveCase::Conflict => {
                assert_eq!(
                    std::fs::read(workspace.join("saved.txt")).unwrap(),
                    b"external writer"
                );
                assert!(changes.is_empty());
            }
            SaveCase::NoWrite | SaveCase::Outside | SaveCase::Deny => {
                assert_eq!(records.len(), usize::from(case == SaveCase::Deny));
                assert!(
                    !workspace.join("saved.txt").exists() && !workspace.join("other.txt").exists()
                );
                assert!(changes.is_empty());
            }
            SaveCase::Stale => {
                // No native save fixture: the real service rejects this read-only
                // observation before any call into the platform UI writer.
                assert_eq!(task.status, "completed", "{:?}", task.error);
                assert_eq!(
                    records[0].execution_state,
                    approval::ExecutionState::Finished
                );
                assert_eq!(
                    changes[0].state,
                    crate::workspace_changes::ChangeState::Failed
                );
                assert!(!workspace.join("saved.txt").exists());
                assert!(
                    task.messages
                        .iter()
                        .any(|m| { m.text_content().contains("re-observe") })
                );
            }
            SaveCase::Mismatch => {
                assert_eq!(
                    records[0].execution_state,
                    approval::ExecutionState::Unknown
                );
                assert_eq!(changes.len(), 1);
                assert_eq!(
                    changes[0].state,
                    crate::workspace_changes::ChangeState::Unknown
                );
                assert_eq!(
                    std::fs::read(workspace.join("saved.txt")).unwrap(),
                    b"different editor text"
                );
                assert!(engine.resume(&task.id, "continue").await.is_err());
                drop(engine);
                drop(store);
                let recovered = Store::open(&db.path().join("test.db")).unwrap();
                recovered.recover().unwrap();
                assert_eq!(
                    recovered.approvals_for_task(&task.id).unwrap()[0].execution_state,
                    approval::ExecutionState::Unknown
                );
                assert_eq!(
                    recovered.workspace_changes_for_task(&task.id).unwrap()[0].state,
                    crate::workspace_changes::ChangeState::Unknown
                );
            }
        }
    }
    let requests = std::mem::take(&mut *script.0.lock().unwrap());
    (task, requests)
}

#[test]
fn save_snapshot_refuses_aliases_overwrite_and_extra_arguments() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("existing.txt"), b"original").unwrap();
    let valid = json!({"observation_ref":"ref","expected_text":"test","path":"new.txt"});
    let check = |args: &Value| {
        crate::workspace::preview_file_mutation(
            workspace.path(),
            "computer_save_as",
            args,
            &["*".into()],
        )
    };
    assert!(check(&valid).is_ok());
    for path in [
        "CON.txt",
        "CON .txt",
        "LPT².txt",
        "CONOUT$.txt",
        "../other.txt",
        "a/b.txt",
        ".\\foo.txt",
        "side:ads.txt",
        "EXISTI~1.txt",
        "%TEMP%.txt",
        ".txt",
        "new.txt.",
        "new.txt ",
        "new.log",
        "existing.txt",
    ] {
        let mut args = valid.clone();
        args["path"] = json!(path);
        assert!(check(&args).is_err(), "{path}");
    }
    let mut extra = valid;
    extra["content"] = json!("not to be typed");
    assert!(check(&extra).is_err());
}

#[tokio::test]
async fn registered_save_requires_approval_and_verified_app_created_file() {
    let reader = tempfile::tempdir().unwrap();
    let fixture = crate::computer::ComputerService::fixture(reader.path());
    let (task, calls) = run_fixture(reader.path(), fixture, true, Some(SaveCase::NoWrite)).await;
    assert_eq!(task.status, "failed");
    assert!(calls.iter().all(|(_, request)| {
        request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["function"]["name"] != "computer_save_as")
    }));
    for case in [
        SaveCase::Deny,
        SaveCase::Outside,
        SaveCase::Conflict,
        SaveCase::Approve,
        SaveCase::Mismatch,
        SaveCase::Stale,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let fixture = crate::computer::ComputerService::fixture(directory.path());
        let (_, requests) = run_fixture(directory.path(), fixture, true, Some(case)).await;
        assert!(
            requests[0].1["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "computer_save_as")
        );
        if case == SaveCase::Approve {
            let last = requests[2].1["messages"]
                .as_array()
                .unwrap()
                .last()
                .unwrap();
            let result: Value = serde_json::from_str(last["content"].as_str().unwrap()).unwrap();
            assert_eq!(result["verified"], true);
        }
    }
}

#[tokio::test]
async fn registered_target_results_reach_next_model_round_after_local_wait() {
    let workspace = tempfile::tempdir().unwrap();
    let fixture = crate::computer::ComputerService::fixture(workspace.path());
    let (task, requests) = run_fixture(workspace.path(), fixture, true, None).await;
    assert_eq!(task.status, "completed", "{:?}", task.error);
    assert_eq!(requests.len(), 3);
    let names: Vec<_> = requests[0].1["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(names.contains(&"computer_observe") && names.contains(&"computer_wait"));
    assert!(!names.contains(&"computer_save_as"));
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
        let (task, requests) = run_fixture(workspace.path(), computer, tools, None).await;
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
                .any(|m| m.text_content().contains("Fixture"))
        );
    }
}
