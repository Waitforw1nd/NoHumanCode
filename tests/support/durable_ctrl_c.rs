use super::*;
use tokio::io::AsyncWriteExt;
use windows_sys::Win32::System::{
    Console::{
        CTRL_C_EVENT, GenerateConsoleCtrlEvent, GetConsoleProcessList, SetConsoleCtrlHandler,
    },
    Threading::CREATE_NEW_CONSOLE,
};

// Windows cannot target CTRL_C_EVENT to a single process group. Re-exec just this
// test in a NEW console. Check membership immediately before every delivery and
// fail closed. Never detach, attach to another console, or signal the user's shell.
fn check_console(child: Option<u32>) {
    let mut pids = [0; 2];
    let count = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), 2) };
    assert_eq!(
        count,
        1 + u32::from(child.is_some()),
        "not a private console"
    );
    assert!(pids.contains(&std::process::id()));
    if let Some(child) = child {
        assert!(pids.contains(&child));
    }
}

unsafe extern "system" fn ignore_own_ctrl_c(event: u32) -> i32 {
    i32::from(event == CTRL_C_EVENT)
}

async fn in_private_console(repl: bool, name: &str) {
    let test = format!("durable_workbench::ctrl_c::{name}");
    if std::env::var("CODA_TEST_PRIVATE_CONSOLE").as_deref() != Ok(test.as_str()) {
        let result = tokio::time::timeout(
            Duration::from_secs(120),
            AsyncCommand::new(std::env::current_exe().unwrap())
                .args(["--exact", &test, "--nocapture"])
                .env("CODA_TEST_PRIVATE_CONSOLE", &test)
                .creation_flags(CREATE_NEW_CONSOLE)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            result.status.success() && String::from_utf8_lossy(&result.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    check_console(None);
    // Unlike SetConsoleCtrlHandler(NULL, TRUE), named handlers are not inherited.
    assert_ne!(
        unsafe { SetConsoleCtrlHandler(Some(ignore_own_ctrl_c), 1) },
        0
    );
    scenario(repl).await;
}

#[tokio::test]
async fn jobs_ctrl_c_cli() {
    in_private_console(false, "jobs_ctrl_c_cli").await;
}

#[tokio::test]
async fn jobs_ctrl_c_repl() {
    in_private_console(true, "jobs_ctrl_c_repl").await;
}

async fn scenario(repl: bool) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/chat/completions", post(reply))
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir(root.join("workspace")).unwrap();
    std::fs::write(root.join("workspace/source.txt"), "marker").unwrap();
    let ask = output(
        local(root)
            .env("OPENAI_API_KEY", "sk-test-durable")
            .env("OPENAI_BASE_URL", base)
            .env("CODA_MODEL", "gpt-explore")
            .args(["ask", "Prepare a session"]),
    )
    .await;
    let session = ask["session_id"].as_str().unwrap();
    let file = root.join("branches.json");
    let branches =
        [FIRST, SLOW, FOLLOWUP].map(|q| json!({"question":q,"path":".","query":"marker"}));
    std::fs::write(&file, json!({"branches":branches}).to_string()).unwrap();
    let batch = jobs(
        root,
        session,
        &[
            "submit",
            "--key",
            "interrupt",
            "--file",
            file.to_str().unwrap(),
        ],
    )
    .await;
    let batch_id = batch["id"].as_str().unwrap();
    let slow_id = batch["jobs"][1]["id"].as_str().unwrap();
    for (index, (verb, target)) in [("continue", batch_id), ("retry", slow_id)]
        .into_iter()
        .enumerate()
    {
        let mut cmd = local(root);
        if repl {
            cmd.args(["chat", "--resume", session])
                .stdin(Stdio::piped());
        } else {
            cmd.args(["jobs", "--session", session, verb, target]);
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(format!("/jobs {verb} {target}\n").as_bytes())
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            while calls.lock().unwrap().iter().filter(|q| **q == SLOW).count() <= index {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "CLI exited before dispatch"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("mock must receive the in-flight attempt");
        check_console(Some(child.id().unwrap()));
        assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) }, 0);
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(format!("/jobs show {slow_id}\n/exit\n").as_bytes())
                .await
                .unwrap();
        }
        let out = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .expect("Ctrl+C must stop waiting for the unresponsive provider")
            .unwrap();
        if repl {
            assert!(out.status.success(), "{:?}", out.status);
            assert!(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .any(|v| v["id"] == slow_id && v["status"] == "unknown"),
                "REPL must execute the next command"
            );
        }
        // Before opening another CLI (which runs crash recovery), distinguish
        // cooperative interruption from termination by Windows' default handler.
        let state: String = reader(root)
            .unwrap()
            .query_row(
                "SELECT status FROM exploration_jobs WHERE id=?1",
                [slow_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "unknown",
            "attempt must be reconciled before process exit"
        );
        let list = jobs(root, session, &["list"]).await;
        assert_eq!(list[0]["result"]["summary"], SUMMARY);
        let receipts = list[1]["receipts"].as_array().unwrap();
        assert_eq!(receipts.len(), index + 1);
        if index == 1 {
            assert_ne!(receipts[0]["attempt"], receipts[1]["attempt"]);
            assert_ne!(receipts[0]["call_id"], receipts[1]["call_id"]);
        }
        assert_eq!(list[2]["status"], "queued");
        assert!(list[2]["receipts"].as_array().unwrap().is_empty());
    }
    let done = jobs(root, session, &["continue", batch_id]).await;
    assert_eq!(done["jobs"][1]["status"], "unknown");
    assert_eq!(done["jobs"][2]["status"], "succeeded");
    assert_eq!(
        *calls.lock().unwrap(),
        ["parent", FIRST, SLOW, SLOW, FOLLOWUP]
    );
    server.abort();
}

async fn reply(
    State(calls): State<Arc<Mutex<Vec<&'static str>>>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let messages = body["messages"].to_string();
    let branch = [FIRST, SLOW, FOLLOWUP]
        .into_iter()
        .find(|q| messages.contains(q))
        .unwrap_or("parent");
    calls.lock().unwrap().push(branch);
    if branch == SLOW {
        std::future::pending::<()>().await;
    }
    let chunk = json!({"choices":[{"delta":{"content":SUMMARY},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}
