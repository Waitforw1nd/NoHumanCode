use super::*;

const CALL_ID: &str = "batch-ask";
const QUESTION: &str = "private branch question";
const EXCERPT: &str = "private excerpt";
const SUMMARY: &str = "private child summary";

async fn parallel_completions(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut calls = requests.lock().unwrap();
    let (delta, finish) = if calls.is_empty() {
        let arguments = json!({"branches":[
            {"question":QUESTION,"path":".","query":"found-marker"},
            {"question":"no match","path":".","query":"missing-marker"},
            {"question":"other match","path":".","query":"other-marker"}
        ]});
        (
            json!({"content":"Before ","tool_calls":[{
                "index":0,"id":CALL_ID,"type":"function",
                "function":{"name":"explore_parallel","arguments":arguments.to_string()}
            }]}),
            "tool_calls",
        )
    } else if body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "tool")
    {
        (json!({"content":"after."}), "stop")
    } else if body["messages"].to_string().contains("other match") {
        // A child trying to call a tool fails independently of the successful branch.
        (
            json!({"tool_calls":[{"index":0,"id":"invalid-child","type":"function",
            "function":{"name":"read_file","arguments":"{}"}}]}),
            "tool_calls",
        )
    } else {
        (
            json!({"content":SUMMARY,"reasoning_content":"private child thoughts"}),
            "stop",
        )
    };
    calls.push(body);
    let frame = json!({"choices":[{"delta":delta,"finish_reason":finish}],
        "usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {frame}\n\ndata: [DONE]\n\n"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "provider credentials require Windows DPAPI")]
async fn parallel_exploration_progress_preserves_piped_parent_text() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/chat/completions", post(parallel_completions))
        .with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("sample.txt"),
        format!("found-marker {EXCERPT}\nother-marker\n"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_coda"))
        .env("OPENAI_API_KEY", "sk-parallel-test-only")
        .env("OPENAI_BASE_URL", base_url)
        .env("CODA_MODEL", "gpt-explore")
        .args(["ask", "--data-dir"])
        .arg(temp.path().join("data"))
        .arg("--dir")
        .arg(project)
        .arg("Explore the workspace")
        .output()
        .unwrap();
    server.abort();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "ask failed: {stderr}");
    assert_eq!(stdout.trim_end_matches(['\r', '\n']), "Before after.");
    for detail in [
        "Exploring 3 branches",
        "Branch 1/3: Summary ready",
        "Branch 2/3: No matches",
        "Branch 3/3: Failed",
        "Results returned",
    ] {
        let line = format!("[\"{CALL_ID}\"] {detail}");
        assert_eq!(stderr.matches(&line).count(), 1, "{stderr}");
    }
    for private in [QUESTION, EXCERPT, SUMMARY, "private child thoughts"] {
        assert!(!stderr.contains(private), "{stderr}");
    }
    let calls = requests.lock().unwrap();
    assert_eq!(calls.len(), 4);
    let parent = calls.last().unwrap()["messages"].as_array().unwrap();
    let message = parent
        .iter()
        .find(|m| m["role"] == "tool" && m["tool_call_id"] == CALL_ID)
        .unwrap();
    let result: Value = serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
    let branches = result["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 3);
    for (i, status) in ["ok", "no_matches", "error"].iter().enumerate() {
        assert_eq!(branches[i]["index"], i + 1);
        assert_eq!(branches[i]["status"], *status);
    }
    assert_eq!(branches[0]["summary"], SUMMARY);
    assert_eq!(branches[0]["evidence"][0]["path"], "sample.txt");
    assert_eq!(branches[0]["evidence"][0]["start_line"], 1);
    assert_eq!(branches[1]["evidence"], json!([]));
}
