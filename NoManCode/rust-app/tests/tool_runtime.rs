//! Public runtime admission/definition boundary; actual execution stays crate-private.
use peachsh::{domain::Task, tool_runtime::ToolRuntime};
use serde_json::{Value, json};
fn task() -> Task {
    serde_json::from_value(json!({"id":"task","run_id":"run",
        "spec":{"name":"worker","role":"test","route_id":"route","prompt":"test",
            "depends_on":[],"write_scopes":[],"tools":true,"allow_commands":false,"max_rounds":1},
        "route":{"id":"route","name":"route","base_url":"http://127.0.0.1","model":"mock",
            "max_tokens":16,"parallel_limit":1,"key_env":null},
        "workspace":"unused","status":"running","output":"","error":null,"messages":[],
        "usage":null,"created_at":0,"updated_at":0}))
    .unwrap()
}
fn names(definitions: Vec<Value>) -> Vec<String> {
    definitions
        .into_iter()
        .map(|d| d["function"]["name"].as_str().unwrap().to_owned())
        .collect()
}
#[test]
fn definitions_and_admission_obey_task_permission_ceiling() {
    let runtime = ToolRuntime::builtin().unwrap();
    let mut task = task();
    let mut visible = names(runtime.definitions(&task).unwrap());
    visible.sort();
    assert_eq!(
        visible,
        ["list_files", "read_file", "run_wasm", "search_files"]
    );
    assert!(
        runtime
            .acquire(&task, "write_file", || -> anyhow::Result<()> {
                panic!("unauthorized")
            })
            .is_err()
    );
    assert!(
        runtime
            .acquire(&task, "run_command", || -> anyhow::Result<()> {
                panic!("unauthorized")
            })
            .is_err()
    );
    task.spec.write_scopes = vec!["src".into()];
    task.spec.allow_commands = true;
    assert_eq!(runtime.definitions(&task).unwrap().len(), 6);
    task.spec.tools = false;
    assert!(runtime.definitions(&task).unwrap().is_empty());
    assert!(
        runtime
            .acquire(&task, "read_file", || -> anyhow::Result<()> {
                panic!("unauthorized")
            })
            .is_err()
    );
}
#[test]
fn revoked_file_tools_never_reappear_through_compatibility_dispatch() {
    let runtime = ToolRuntime::builtin().unwrap();
    let mut task = task();
    task.spec.write_scopes = vec!["*".into()];
    runtime.unload_builtin_files().unwrap();
    let mut visible = names(runtime.definitions(&task).unwrap());
    visible.sort();
    assert_eq!(visible, ["list_files", "run_wasm", "search_files"]);
    for name in ["read_file", "write_file", "unknown"] {
        assert!(
            runtime
                .acquire(&task, name, || -> anyhow::Result<()> { panic!("revoked") })
                .is_err()
        );
    }
    for name in ["list_files", "search_files", "run_wasm"] {
        let (_, admission) = runtime.acquire(&task, name, || Ok(7)).unwrap();
        assert_eq!(admission, 7);
    }
    // A new Host reconstructs builtins, not any lease from the previous Host.
    assert!(
        names(ToolRuntime::builtin().unwrap().definitions(&task).unwrap())
            .contains(&"read_file".to_owned())
    );
}
