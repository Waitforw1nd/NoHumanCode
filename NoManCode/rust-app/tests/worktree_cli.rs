//! NEXT-13 actual CLI processes, strict worktree wire receipts and no local effects.
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
const OP: &str = "12345678-1234-4234-8234-123456789abc";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
type Request = (String, String, Value, bool, Option<String>);
#[derive(Clone)]
struct Mock {
    replies: Arc<Mutex<VecDeque<(u16, String)>>>,
    requests: Arc<Mutex<Vec<Request>>>,
}
struct Host {
    port: u16,
    mock: Mock,
    task: tokio::task::JoinHandle<()>,
    cwd: tempfile::TempDir,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn reply(
    State(m): State<Mock>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    if uri.path() == "/api/bootstrap" {
        return axum::Json(json!({"token":"worktree-cli-token"})).into_response();
    }
    m.requests.lock().unwrap().push((
        method.to_string(),
        uri.to_string(),
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
        headers
            .get("x-peachsh-token")
            .is_some_and(|v| v == "worktree-cli-token"),
        headers
            .get("idempotency-key")
            .map(|v| v.to_str().unwrap().into()),
    ));
    let (status, body) = m
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("no automatic retry");
    if status == 0 {
        return Response::builder()
            .header("content-type", "application/json")
            .body(Body::from_stream(futures_util::stream::iter(vec![Err::<
                Bytes,
                _,
            >(
                std::io::Error::other("lost response"),
            )])))
            .unwrap();
    }
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}
impl Host {
    async fn new(replies: Vec<(u16, String)>) -> Self {
        let mock = Mock {
            replies: Arc::new(Mutex::new(replies.into())),
            requests: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().fallback(reply).with_state(mock.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            port,
            mock,
            task,
            cwd: tempfile::tempdir().unwrap(),
        }
    }
    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_peachsh"));
        c.args(["--port", &self.port.to_string(), "--json"])
            .args(args)
            .current_dir(self.cwd.path())
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("ALL_PROXY", "http://127.0.0.1:1")
            .env("NO_PROXY", "")
            .kill_on_drop(true);
        c
    }
    async fn cli(&self, args: &[&str]) -> Output {
        tokio::time::timeout(Duration::from_secs(10), self.command(args).output())
            .await
            .unwrap()
            .unwrap()
    }
    fn unchanged(&self, count: usize) {
        assert_eq!(self.mock.requests.lock().unwrap().len(), count);
        assert_eq!(std::fs::read_dir(self.cwd.path()).unwrap().count(), 0);
    }
}
fn parent() -> String {
    std::env::temp_dir().to_string_lossy().into_owned()
}
fn receipt(state: &str) -> Value {
    json!({"schema_version":1,"operation_id":OP,"source_project_id":"source","name":"isolated","parent_path":parent(),"target_path":std::path::Path::new(&parent()).join(format!("nhc-{OP}")),"branch":format!("codex/workspace/{OP}"),"expected_config_revision":2,"expected_head":HEAD,"request_digest":"a".repeat(64),"revision":if state=="claimed"{1}else{2},"state":state,"project_id":if state=="succeeded"{json!("result-project")}else{Value::Null},"cause":match state{"rejected"=>json!("conflict"),"unknown"=>json!("process_outcome_unknown"),_=>Value::Null},"created_at":10,"updated_at":20})
}
fn create_args(parent: &str) -> Vec<&str> {
    vec![
        "project",
        "worktree",
        "create",
        "--source-project",
        "source",
        "--name",
        "isolated",
        "--parent",
        parent,
        "--expected-config-revision",
        "2",
        "--expected-head",
        HEAD,
        "--key",
        "create-key",
    ]
}
fn stdout(o: &Output, code: i32) -> Value {
    assert_eq!(
        o.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        o.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_slice(&o.stdout).unwrap()
}
fn error(o: &Output) -> Value {
    assert!(!o.status.success());
    assert!(o.stdout.is_empty());
    serde_json::from_slice(&o.stderr).unwrap()
}
#[tokio::test]
async fn w09_cli_create_states_and_authenticated_read_only_posts() {
    for (state, http, exit) in [
        ("succeeded", 200, 0),
        ("claimed", 202, 1),
        ("unknown", 202, 1),
        ("rejected", 409, 1),
    ] {
        let r = receipt(state);
        let inspection = json!({"receipt":r,"observation":{"operation_id":OP,"observed_at":21,"state":"absent_observed","reason_codes":["absent"],"observed_head":null,"observed_branch":null}});
        let h = Host::new(vec![
            (http, r.to_string()),
            (200, r.to_string()),
            (200, r.to_string()),
            (200, inspection.to_string()),
        ])
        .await;
        assert_eq!(stdout(&h.cli(&create_args(&parent())).await, exit), r);
        assert_eq!(
            stdout(
                &h.cli(&[
                    "project",
                    "worktree",
                    "status",
                    "--source-project",
                    "source",
                    "--id",
                    OP
                ])
                .await,
                0
            ),
            r
        );
        assert_eq!(
            stdout(
                &h.cli(&[
                    "project",
                    "worktree",
                    "status",
                    "--source-project",
                    "source",
                    "--key",
                    "create-key"
                ])
                .await,
                0
            ),
            r
        );
        assert_eq!(
            stdout(
                &h.cli(&[
                    "project",
                    "worktree",
                    "inspect",
                    "--source-project",
                    "source",
                    "--id",
                    OP
                ])
                .await,
                0
            ),
            inspection
        );
        {
            let q = h.mock.requests.lock().unwrap();
            assert!(q.iter().all(|r| r.3));
            assert_eq!(q[0].4.as_deref(), Some("create-key"));
            assert_eq!(
                q[0].2,
                json!({"name":"isolated","parent_path":parent(),"expected_config_revision":2,"expected_head":HEAD})
            );
            assert_eq!(q[1].0, "GET");
            assert_eq!(q[2].0, "POST");
            assert_eq!(q[2].1, "/api/projects/source/worktrees/lookup");
            assert_eq!(q[2].2, json!({"idempotency_key":"create-key"}));
            assert_eq!(q[3].2, json!({}));
            assert!(q[1..].iter().all(|r| r.4.is_none()));
        }
        h.unchanged(4);
    }
}
#[tokio::test]
async fn w09_cli_bad_receipts_become_unknown_only_after_create() {
    let mut cases = vec![];
    for (key, value) in [
        ("schema_version", json!(2)),
        ("operation_id", json!("bad")),
        ("source_project_id", json!("other")),
        ("branch", json!("main")),
        ("target_path", json!(parent())),
        ("expected_head", json!("f".repeat(40))),
        ("expected_config_revision", json!(3)),
        ("request_digest", json!("bad")),
        ("revision", json!(0)),
        ("project_id", Value::Null),
        ("cause", json!("conflict")),
        ("updated_at", json!(1)),
        ("extra", json!(true)),
    ] {
        let mut r = receipt("succeeded");
        r[key] = value;
        cases.push((200, r.to_string()));
    }
    for field in ["cause", "project_id"] {
        let mut missing = receipt("claimed");
        missing.as_object_mut().unwrap().remove(field);
        cases.push((202, missing.to_string()));
    }
    cases.push((202, receipt("succeeded").to_string()));
    cases.push((200, receipt("unknown").to_string()));
    cases.push((0, String::new()));
    cases.push((200, "{".into()));
    cases.push((
        200,
        receipt("succeeded")
            .to_string()
            .replacen("{", "{\"schema_version\":1,", 1),
    ));
    for response in cases {
        let h = Host::new(vec![response]).await;
        let e = error(&h.cli(&create_args(&parent())).await);
        assert_eq!(e["code"], "operation_result_unknown");
        assert_eq!(e["retryable"], false);
        h.unchanged(1);
    }
    for args in [
        vec![
            "project",
            "worktree",
            "status",
            "--source-project",
            "source",
            "--key",
            "k",
        ],
        vec![
            "project",
            "worktree",
            "inspect",
            "--source-project",
            "source",
            "--id",
            OP,
        ],
    ] {
        let h = Host::new(vec![(0, String::new())]).await;
        let e = error(&h.cli(&args).await);
        assert_ne!(e["code"], "operation_result_unknown");
        h.unchanged(1);
    }
}
#[tokio::test]
async fn w09_cli_error_status_codes_are_strict_and_safe() {
    for (code, status) in [
        ("worktree_invalid", 400),
        ("worktree_not_found", 404),
        ("worktree_revision_conflict", 409),
        ("worktree_idempotency_conflict", 409),
        ("worktree_busy", 409),
        ("worktree_root_changed", 409),
        ("worktree_conflict", 409),
        ("worktree_unsupported", 422),
        ("worktree_unavailable", 503),
        ("worktree_corrupt", 500),
        ("forbidden", 403),
    ] {
        let value = json!({"code":code,"message":code,"error":code,"retryable":false});
        let h = Host::new(vec![(status, value.to_string()), (200, value.to_string())]).await;
        let e = error(&h.cli(&create_args(&parent())).await);
        assert_eq!(
            e["code"],
            if code == "worktree_corrupt" {
                "operation_result_unknown"
            } else {
                code
            }
        );
        assert_eq!(
            error(&h.cli(&create_args(&parent())).await)["code"],
            "operation_result_unknown"
        );
        h.unchanged(2);
    }
}
#[tokio::test]
async fn w09_cli_inspection_cannot_change_receipt_or_forge_matching_identity() {
    let r = receipt("unknown");
    let good = json!({"receipt":r,"observation":{"operation_id":OP,"observed_at":30,"state":"present_matching","reason_codes":[],"observed_head":HEAD,"observed_branch":format!("refs/heads/codex/workspace/{OP}")}});
    let mut bad = good.clone();
    bad["observation"]["observed_head"] = json!("f".repeat(40));
    let mut other = good.clone();
    other["observation"]["operation_id"] = json!("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    let mut raw_reason = good.clone();
    raw_reason["observation"]["reason_codes"] = json!(["stderr-secret"]);
    let h = Host::new(vec![
        (200, good.to_string()),
        (200, bad.to_string()),
        (200, other.to_string()),
        (200, raw_reason.to_string()),
    ])
    .await;
    let args = [
        "project",
        "worktree",
        "inspect",
        "--source-project",
        "source",
        "--id",
        OP,
    ];
    assert_eq!(stdout(&h.cli(&args).await, 0), good);
    for _ in 0..3 {
        assert_ne!(
            error(&h.cli(&args).await)["code"],
            "operation_result_unknown"
        );
    }
    h.unchanged(4);
}
#[tokio::test]
async fn w09_cli_invalid_selector_and_input_never_dispatch() {
    let h = Host::new(vec![]).await;
    for args in [
        vec![
            "project",
            "worktree",
            "status",
            "--source-project",
            "source",
        ],
        vec![
            "project",
            "worktree",
            "status",
            "--source-project",
            "source",
            "--id",
            OP,
            "--key",
            "k",
        ],
        vec![
            "project",
            "worktree",
            "inspect",
            "--source-project",
            "source",
            "--id",
            "bad",
        ],
    ] {
        assert!(!h.cli(&args).await.status.success());
    }
    for (field, value) in [
        ("--expected-head", "HEAD"),
        ("--expected-config-revision", "0"),
        ("--key", "bad,key"),
    ] {
        let p = parent();
        let mut args = create_args(&p);
        let index = args.iter().position(|a| *a == field).unwrap();
        args[index + 1] = value;
        assert!(!h.cli(&args).await.status.success());
    }
    h.unchanged(0);
}
#[tokio::test]
async fn w09_cli_broken_stdout_after_create_reports_unknown_without_retry() {
    let h = Host::new(vec![(200, receipt("succeeded").to_string())]).await;
    let mut child = h
        .command(&create_args(&parent()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let o = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(error(&o)["code"], "operation_result_unknown");
    h.unchanged(1);
}

#[tokio::test]
async fn w09_cli_windows_path_spellings_are_lexical_without_filesystem_access() {
    let p = parent();
    let mut r = receipt("succeeded");
    let target = r["target_path"].as_str().unwrap().replace('/', '\\');
    r["target_path"] = json!(if target.starts_with("\\\\?\\") {
        target.to_ascii_uppercase()
    } else {
        format!("\\\\?\\{}", target.to_ascii_uppercase())
    });
    let h = Host::new(vec![(200, r.to_string())]).await;
    assert_eq!(stdout(&h.cli(&create_args(&p)).await, 0), r);
    h.unchanged(1);
}
