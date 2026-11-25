use super::*;
use std::{fs::File, io::Write, os::fd::FromRawFd, process::Stdio};

// Only stdin needs a terminal. Keep stdout/stderr separate for prompt assertions.
fn terminal_input() -> (File, File) {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty initializes both descriptors on success. Null optional
    // arguments select the default terminal settings, including canonical input.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
    // SAFETY: each fresh descriptor is transferred to exactly one File.
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}

async fn check_switch(resume: bool, process_yes: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/v1/chat/completions", post(move || {
        let step = calls.fetch_add(1, Ordering::SeqCst).checked_sub(usize::from(resume));
        async move {
            let choice = match step {
                Some(n @ (0 | 2 | 4)) => {
                    let args = json!({"path":format!("edit-{n}.txt"),"content":"approved"});
                    json!({"delta":{"tool_calls":[{"index":0,"id":format!("call-{n}"),"type":"function","function":{"name":"write_file","arguments":args.to_string()}}]},"finish_reason":"tool_calls"})
                }
                _ => json!({"delta":{"content":"done"},"finish_reason":"stop"}),
            };
            let chunk = json!({"choices":[choice]});
            ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: [DONE]\n\n"))
        }
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let command = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_coda"));
        cmd.current_dir(&project)
            .env("CODA_DATA_DIR", temp.path().join("data"))
            .env("OPENAI_BASE_URL", &base)
            .env("OPENAI_API_KEY", "local-test-key")
            .env("CODA_MODEL", "gpt-test");
        cmd
    };
    let switch = if resume {
        let seed = command().args(["--json", "ask", "seed"]).output().unwrap();
        assert!(
            seed.status.success(),
            "{}",
            String::from_utf8_lossy(&seed.stderr)
        );
        let result: Value = serde_json::from_slice(&seed.stdout).unwrap();
        format!("/resume {}", result["session_id"].as_str().unwrap())
    } else {
        "/new".to_owned()
    };
    let (mut input, stdin) = terminal_input();
    let mut chat = command();
    chat.arg("chat");
    if process_yes {
        chat.arg("-y");
    }
    let child = chat
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let first_answer = if process_yes { "" } else { "a\n" };
    let second_answer = if process_yes { "" } else { "n\n" };
    let third_answer = if process_yes { "n\n" } else { "" };
    write!(
        input,
        "first\n{first_answer}{switch}\n/status\nsecond\n{second_answer}/auto\n/status\n/clear\n/status\n/new\n/auto\nthird\n{third_answer}/exit\n\x04"
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    server.abort();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert_eq!(
        std::fs::read_to_string(project.join("edit-0.txt")).unwrap(),
        "approved"
    );
    assert_eq!(
        project.join("edit-2.txt").exists(),
        process_yes,
        "{stdout}\n{stderr}"
    );
    assert_eq!(
        project.join("edit-4.txt").exists(),
        !process_yes,
        "{stdout}\n{stderr}"
    );
    assert_eq!(
        stderr.matches("Approve?").count(),
        if process_yes { 1 } else { 2 }
    );
    if let Some(id) = switch.strip_prefix("/resume ") {
        assert!(
            stdout.contains(&format!("Session     : {id} (1 turns")),
            "{stdout}"
        );
    }
    let status = if process_yes {
        "ON (process -y)"
    } else {
        "OFF"
    };
    let toggle = if process_yes {
        "OFF (session)"
    } else {
        "ON (session)"
    };
    let statuses: Vec<_> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("Auto-Approve: "))
        .collect();
    assert_eq!(statuses, [status, toggle, status], "{stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_session_then_new_requires_fresh_approval() {
    check_switch(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_session_then_resume_other_requires_fresh_approval() {
    check_switch(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_yes_survives_new_and_auto_can_disable_it_before_first_tool() {
    check_switch(false, true).await;
}

async fn serve_route(label: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/v1/chat/completions", post(move || {
        let step = calls.fetch_add(1, Ordering::SeqCst);
        async move {
            let choice = if step % 2 == 1 {
                let path = format!("{label}-{step}.txt");
                let args = json!({"path":path,"content":"approved"});
                json!({"delta":{"tool_calls":[{"index":0,"id":path,"type":"function","function":{"name":"write_file","arguments":args.to_string()}}]},"finish_reason":"tool_calls"})
            } else {
                json!({"delta":{"content":label},"finish_reason":"stop"})
            };
            let chunk = json!({"choices":[choice]});
            ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: [DONE]\n\n"))
        }
    }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

async fn check_resumed_route(entry: &str) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let command = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_coda"));
        cmd.current_dir(&workspace)
            .env("CODA_DATA_DIR", temp.path().join("data"))
            .env("OPENAI_API_KEY", "local-test-key");
        cmd
    };
    let mut id = String::new();
    for label in ["b", "a"] {
        let seed = command()
            .env("OPENAI_BASE_URL", serve_route(label).await)
            .args([
                "--json",
                "ask",
                "--route",
                label,
                "--model",
                &format!("gpt-{label}"),
                "seed",
            ])
            .output()
            .unwrap();
        assert!(
            seed.status.success(),
            "{}",
            String::from_utf8_lossy(&seed.stderr)
        );
        let result: Value = serde_json::from_slice(&seed.stdout).unwrap();
        id = result["session_id"].as_str().unwrap().to_owned();
    }
    let mut chat = command();
    chat.args(["chat", "--route", "b"]);
    let resume = match entry {
        "/resume" => format!("/resume {id}\n"),
        "--resume" => {
            chat.args([entry, &id]);
            String::new()
        }
        "--continue" => {
            chat.arg(entry);
            String::new()
        }
        _ => unreachable!(),
    };
    let (mut input, stdin) = terminal_input();
    let child = chat
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    write!(
        input,
        "{resume}/status\nfirst\na\n/route b\n/status\nsecond\nn\n/exit\n\x04"
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert_eq!(stderr.matches("Approve?").count(), 2, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("Route/Model : a / gpt-a") && stdout.contains("Route/Model : b / gpt-b"),
        "{stdout}"
    );
    assert!(stdout.contains("Session     : (none yet)"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("a-1.txt")).unwrap(),
        "approved"
    );
    assert!(stderr.contains("b-1.txt"), "{stderr}");
    assert!(!workspace.join("b-1.txt").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_route_switch_requires_new_approval() {
    check_resumed_route("/resume").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_resumed_route_switch_requires_new_approval() {
    check_resumed_route("--resume").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_continued_route_switch_requires_new_approval() {
    check_resumed_route("--continue").await;
}
