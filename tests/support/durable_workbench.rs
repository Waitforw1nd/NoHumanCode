use super::*;
use rusqlite::{Connection, OpenFlags};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::Command as AsyncCommand;

const TOOL_CALL: &str = "crash-explore";
const FIRST: &str = "Inspect the committed branch";
const SLOW: &str = "Inspect the interrupted branch";
const FOLLOWUP: &str = "Check the saved evidence";
const SUMMARY: &str = "Durably saved branch A evidence.";

async fn completions(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let text = body["messages"].to_string();
    let fetch = {
        let mut calls = requests.lock().unwrap();
        calls.push(body.clone());
        calls.len()
    };
    let (delta, finish) = if fetch == 1 {
        let branches = json!({"branches":[
            {"question": FIRST,"path":".","query":"marker-a"},
            {"question": SLOW,"path":".","query":"marker-b"}
        ]});
        (
            json!({"tool_calls":[{"index":0,"id":TOOL_CALL,"type":"function",
            "function":{"name":"explore_parallel","arguments":branches.to_string()}}]}),
            "tool_calls",
        )
    } else if body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "tool")
    {
        (json!({"content":"Parent finished."}), "stop")
    } else if text.contains(FOLLOWUP) {
        (json!({"content":"Checked."}), "stop")
    } else if text.contains(SLOW) {
        // Keep B's first admitted request open while A commits and the real CLI is killed.
        if fetch <= 3 {
            std::future::pending::<()>().await;
        }
        (json!({"content":"B retried."}), "stop")
    } else {
        (json!({"content":SUMMARY}), "stop")
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":finish}],
        "usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

fn local(root: &Path) -> AsyncCommand {
    let mut cmd = AsyncCommand::new(env!("CARGO_BIN_EXE_coda"));
    cmd.arg("--json")
        .arg("--data-dir")
        .arg(root.join("data"))
        .arg("--workspace")
        .arg(root.join("workspace"))
        .kill_on_drop(true);
    cmd
}

async fn output(cmd: &mut AsyncCommand) -> Value {
    let result = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

async fn jobs(root: &Path, session: &str, args: &[&str]) -> Value {
    output(local(root).args(["jobs", "--session", session]).args(args)).await
}

fn reader(root: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        root.join("data/coda.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg_attr(not(windows), ignore = "provider credentials require Windows DPAPI")]
async fn durable_jobs_survive_real_cli_crash_and_feed_a_later_batch() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir(root.join("workspace")).unwrap();
    std::fs::write(root.join("workspace/source.txt"), "marker-a\nmarker-b\n").unwrap();
    let mut ask = local(root)
        .env("OPENAI_API_KEY", "sk-durable-example")
        .env("OPENAI_BASE_URL", base)
        .env("CODA_MODEL", "gpt-explore")
        .args(["ask", "Explore both branches"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            assert!(ask.try_wait().unwrap().is_none(), "ask exited before crash point");
            if requests.lock().unwrap().len() == 3
                && reader(root).and_then(|db| db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM exploration_jobs WHERE ordinal=0 AND status='succeeded')",
                    [], |r|r.get::<_,bool>(0))).unwrap_or(false) { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("branch A must commit while B is in flight");
    let (session, parent): (String, String) = reader(root)
        .unwrap()
        .query_row(
            "SELECT session_id,parent_task_id FROM exploration_batches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(tool_answers(root, &parent), 0);
    ask.kill().await.unwrap();

    let listed = jobs(root, &session, &["list"]).await;
    let a = listed[0]["id"].as_str().unwrap();
    let b = listed[1]["id"].as_str().unwrap();
    let batch = listed[0]["batch_id"].as_str().unwrap();
    assert_eq!(listed[1]["status"], "unknown");
    assert_eq!(listed[1]["receipts"].as_array().unwrap().len(), 1);
    let saved = jobs(root, &session, &["show", a]).await;
    assert_eq!(saved["result"]["summary"], SUMMARY);
    assert_eq!(saved["result"]["evidence"][0]["start_line"], 1);
    jobs(root, &session, &["continue", batch]).await;
    assert_eq!(
        requests.lock().unwrap().len(),
        3,
        "recovery resent a completed or uncertain branch"
    );

    let file = root.join("dependent.json");
    std::fs::write(
        &file,
        format!(
            "\u{feff}{}",
            json!({"branches":[{"question":FOLLOWUP,"path":".","query":"marker-a","depends_on":[a]}]})
        ),
    )
    .unwrap();
    let args = [
        "submit",
        "--key",
        "dependent-submit",
        "--file",
        file.to_str().unwrap(),
    ];
    let submitted = jobs(root, &session, &args).await;
    let replay = jobs(root, &session, &args).await;
    assert_eq!(submitted, replay);
    assert_eq!(requests.lock().unwrap().len(), 3);
    let dependent = submitted["jobs"][0]["id"].as_str().unwrap();
    let done = jobs(root, &session, &["continue", dependent]).await;
    assert_eq!(done["status"], "succeeded");
    {
        let calls = requests.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert!(calls[3]["messages"].to_string().contains(SUMMARY));
    }
    let retry = jobs(root, &session, &["retry", b]).await;
    assert_eq!(retry["status"], "succeeded");
    let receipts = retry["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 2);
    assert_ne!(receipts[0]["attempt"], receipts[1]["attempt"]);
    assert_ne!(receipts[0]["call_id"], receipts[1]["call_id"]);

    // Resume the original parent task through the public Host CLI.
    let reserve = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port().to_string();
    drop(reserve);
    let mut host = AsyncCommand::new(env!("CARGO_BIN_EXE_coda"))
        .args(["--port", &port, "--data-dir"])
        .arg(root.join("data"))
        .arg("--workspace")
        .arg(root.join("workspace"))
        .arg("serve")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            assert!(
                host.try_wait().unwrap().is_none(),
                "host exited during recovery"
            );
            if let Ok(reply) = client
                .get(format!("http://127.0.0.1:{port}/api/health"))
                .send()
                .await
                && reply.status().is_success()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    output(AsyncCommand::new(env!("CARGO_BIN_EXE_coda")).args([
        "--json",
        "--port",
        &port,
        "task",
        "resume",
        &parent,
        "--message",
        "Finish the recovered answer",
    ]))
    .await;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let db = reader(root).unwrap();
            let status: String = db
                .query_row(
                    "SELECT json_extract(value,'$.status') FROM tasks WHERE id=?1",
                    [&parent],
                    |row| row.get(0),
                )
                .unwrap();
            if status == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(tool_answers(root, &parent), 1);
    {
        let calls = requests.lock().unwrap();
        assert_eq!(calls.len(), 6);
        let tool = calls[5]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["tool_call_id"] == TOOL_CALL)
            .unwrap();
        assert!(tool["content"].as_str().unwrap().contains(SUMMARY));
    }
    host.kill().await.unwrap();
    server.abort();
}

fn tool_answers(root: &Path, parent: &str) -> usize {
    reader(root).unwrap().query_row(
        "SELECT COUNT(*) FROM tasks,json_each(tasks.value,'$.messages') AS message WHERE tasks.id=?1
         AND json_extract(message.value,'$.tool_call_id')=?2",[parent,TOOL_CALL],|r|r.get(0)).unwrap()
}
