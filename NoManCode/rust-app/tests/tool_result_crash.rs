//! Public Host restart preserves an unanswered legacy result; Engine resume is covered separately.
#![cfg(windows)]
use peachsh::{
    domain::*,
    store::{Store, fresh_settings},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::{Child, Command};

const BIN: &str = env!("CARGO_BIN_EXE_peachsh");

async fn host(root: &Path, port: u16, label: &str) -> Child {
    let stdout = std::fs::File::create(root.join(format!("{label}.stdout.log"))).unwrap();
    let stderr = std::fs::File::create(root.join(format!("{label}.stderr.log"))).unwrap();
    let mut child = Command::new(BIN)
        .args(["--port", &port.to_string(), "--data-dir"])
        .arg(root.join("data"))
        .arg("--workspace")
        .arg(root.join("workspace"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "Host exited before readiness"
            );
            if let Ok(r) = client
                .get(format!("http://127.0.0.1:{port}/api/health"))
                .send()
                .await
                && r.status().is_success()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    child
}

#[tokio::test]
async fn host_restart_keeps_legacy_result_without_automatic_tool_execution() {
    let root = tempfile::Builder::new()
        .prefix("nhc-result-host-")
        .tempdir()
        .unwrap()
        .keep();
    std::fs::create_dir(root.join("data")).unwrap();
    std::fs::create_dir(root.join("workspace")).unwrap();
    let db = root.join("data/peachsh.sqlite3");
    let store = Store::open(&db).unwrap();
    fresh_settings(&store, &root.join("workspace")).unwrap();
    drop(store);
    let task = Task {
        id: "task".into(),
        run_id: "run".into(),
        spec: TaskSpec {
            name: "worker".into(),
            role: "worker".into(),
            route_id: "route".into(),
            prompt: "read".into(),
            depends_on: vec![],
            write_scopes: vec![],
            tools: true,
            allow_commands: false,
            max_rounds: 2,
        },
        route: Route {
            id: "route".into(),
            name: "route".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "mock".into(),
            max_tokens: 64,
            parallel_limit: 1,
            key_env: None,
        },
        workspace: root.join("workspace").to_string_lossy().into(),
        status: "running".into(),
        output: String::new(),
        error: None,
        messages: vec![
            json!({"role":"user","content":"read"}),
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"read","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}]}),
        ],
        usage: Value::Null,
        created_at: 1,
        updated_at: 1,
    };
    // Reconstruct a schema-17 historical record with no live Store or
    // controller. This seed is not a test of authorized production writes.
    let raw = rusqlite::Connection::open(&db).unwrap();
    raw.execute_batch("DROP TABLE execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; PRAGMA user_version=17;").unwrap();
    raw.execute(
        "INSERT INTO runs(id,title,kind,created_at) VALUES('run','restart','team',1)",
        [],
    )
    .unwrap();
    raw.execute(
        "INSERT INTO tasks(id,run_id,value) VALUES('task','run',?1)",
        [serde_json::to_string(&task).unwrap()],
    )
    .unwrap();
    raw.execute("INSERT INTO events(seq,task_id,kind,data,at,schema_version,cursor,session_id,turn_id) VALUES(1,'task','tool_result',?1,1,1,'1',NULL,NULL)", [json!({"name":"read_file","tool_call_id":"read","result":{"content":"legacy"}}).to_string()]).unwrap();
    drop(raw);
    let store = Store::open(&db).unwrap();
    let initial: Vec<_> = store
        .events("run", 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "tool_result")
        .map(|e| serde_json::to_value(e).unwrap())
        .collect();
    assert_eq!(initial.len(), 1);
    drop(store);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut first = host(&root, port, "first-host").await;
    let pid = first.id().unwrap();
    let cli = Command::new(BIN)
        .args([
            "--port",
            &port.to_string(),
            "--json",
            "run",
            "status",
            "run",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let body: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(body["tasks"][0]["status"], "interrupted");
    first.kill().await.unwrap();
    let status = first.wait().await.unwrap();
    assert!(!status.success());
    for folder in ["raw", "query", "recovery/data", "recovery/workspace"] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    let mut manifest = vec![];
    for suffix in ["", "-wal", "-shm"] {
        let name = format!("peachsh.sqlite3{suffix}");
        let source = root.join("data").join(&name);
        if source.exists() {
            let bytes = std::fs::read(source).unwrap();
            for folder in ["raw", "query", "recovery/data"] {
                std::fs::write(root.join(folder).join(&name), &bytes).unwrap();
            }
            manifest.push(
                json!({"name":name,"present":true,"sha256":format!("{:x}",Sha256::digest(&bytes))}),
            );
        } else {
            manifest.push(json!({"name":name,"present":false}));
        }
    }
    std::fs::write(root.join("host-process.json"),serde_json::to_vec_pretty(&json!({"pid":pid,"kill":"ok","wait":format!("{status:?}"),"reaped":true,"files":manifest.clone()})).unwrap()).unwrap();
    // Schema 18 binds the database to its actual file identity. Keep the
    // forensic copies untouched and query/recover the original file.
    let query = Store::open(&db).unwrap();
    let facts: Vec<_> = query
        .events("run", 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "tool_result")
        .map(|e| serde_json::to_value(e).unwrap())
        .collect();
    assert_eq!(facts, initial);
    assert!(
        !query
            .task("task")
            .unwrap()
            .messages
            .iter()
            .any(|m| m["role"] == "tool")
    );
    drop(query);
    let mut second = host(&root, port, "second-host").await;
    let cli = Command::new(BIN)
        .args([
            "--port",
            &port.to_string(),
            "--json",
            "run",
            "status",
            "run",
        ])
        .output()
        .await
        .unwrap();
    assert!(cli.status.success());
    let body: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(body["tasks"][0]["status"], "interrupted");
    second.kill().await.unwrap();
    assert!(!second.wait().await.unwrap().success());
    let recovered = Store::open(&db).unwrap();
    let facts: Vec<_> = recovered
        .events("run", 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "tool_result")
        .map(|e| serde_json::to_value(e).unwrap())
        .collect();
    assert_eq!(facts, initial);
    assert!(
        !recovered
            .events("run", 0)
            .unwrap()
            .iter()
            .any(|e| e.kind == "tool_start")
    );
    for file in &manifest {
        let path = root.join("raw").join(file["name"].as_str().unwrap());
        if file["present"] == true {
            let bytes = std::fs::read(path).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(&bytes)),
                file["sha256"].as_str().unwrap()
            );
        } else {
            assert!(!path.exists());
        }
    }
    println!("n19 host legacy pid={pid} evidence={}", root.display());
}
