//! Integration tests for line-level unified diff, git diff review, and upgraded coding tools.
use coda::{
    approval::{self, PolicyDecision, ToolEffect},
    domain::{Route, Task, TaskSpec},
    git_diff::{GitDiffRequest, GitDiffStatus, GitDiffView, read_git_diff, unified_diff},
    workspace::{self, TodoItem},
};
use serde_json::{Value, json};
use std::process::Command;

fn sample_task(root: &std::path::Path) -> Task {
    Task {
        id: "tool-test-task".into(),
        run_id: "tool-test-run".into(),
        spec: TaskSpec {
            name: "Coder".into(),
            role: "Engineer".into(),
            route_id: "r1".into(),
            prompt: "edit code".into(),
            depends_on: vec![],
            tools: true,
            write_scopes: vec!["*".into()],
            allow_commands: false,
            max_rounds: 12,
        },
        route: Route {
            id: "r1".into(),
            name: "Local".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "test-model".into(),
            max_tokens: 4096,
            parallel_limit: 1,
            key_env: None,
        },
        workspace: root.to_string_lossy().into_owned(),
        status: "running".into(),
        output: String::new(),
        error: None,
        messages: vec![],
        usage: Value::Null,
        created_at: 1,
        updated_at: 1,
    }
}

#[tokio::test]
async fn edit_file_glob_search_pagination_and_git_diff_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "dev@example.com"])
        .current_dir(root)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Dev"])
        .current_dir(root)
        .status()
        .unwrap();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/auth.rs"),
        "pub struct Auth {\n    pub password: String,\n    pub cancel_token: String,\n}\n\npub fn check() -> bool {\n    false\n}\n",
    )
    .unwrap();

    Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init", "--quiet"])
        .current_dir(root)
        .status()
        .unwrap();

    let task = sample_task(root);

    // 1. glob_files discovers Rust files
    let globbed = workspace::execute(&task, "glob_files", &json!({"pattern": "**/*.rs"}))
        .await
        .unwrap();
    assert_eq!(globbed["files"], json!(["src/auth.rs"]));

    // 2. read_file supports 1-based line range pagination with line numbers
    let read = workspace::execute(
        &task,
        "read_file",
        &json!({"path": "src/auth.rs", "start_line": 6, "end_line": 8}),
    )
    .await
    .unwrap();
    assert!(
        read["numbered_content"]
            .as_str()
            .unwrap()
            .contains("6: pub fn check()")
    );

    // 3. edit_file surgically edits without mangling password/cancel_token/Bearer strings
    let edited = workspace::execute(
        &task,
        "edit_file",
        &json!({
            "path": "src/auth.rs",
            "old_string": "pub fn check() -> bool {\n    false\n}",
            "new_string": "pub fn check(header: &str) -> bool {\n    header.starts_with(\"Bearer \")\n}"
        }),
    )
    .await
    .unwrap();
    assert_eq!(edited["replacements"], 1);
    let diff_str = edited["diff"].as_str().unwrap();
    assert!(diff_str.contains("-    false"));
    assert!(diff_str.contains("+    header.starts_with(\"Bearer \")"));

    // Verify password & cancel_token & Bearer were preserved verbatim on disk!
    let saved = std::fs::read_to_string(root.join("src/auth.rs")).unwrap();
    assert!(saved.contains("pub password: String"));
    assert!(saved.contains("pub cancel_token: String"));
    assert!(saved.contains("Bearer "));

    // 4. search_files supports regex
    let searched = workspace::execute(
        &task,
        "search_files",
        &json!({"query": "starts_with\\(\"Bearer", "file_pattern": "*.rs"}),
    )
    .await
    .unwrap();
    assert_eq!(searched["matches"].as_array().unwrap().len(), 1);

    // 5. git_diff reads real line-level diff without requiring ceremony tickets
    let gdiff = read_git_diff(
        root,
        GitDiffRequest {
            path: "src/auth.rs".into(),
            view: GitDiffView::Head,
        },
    )
    .await
    .unwrap();
    assert_eq!(gdiff.status, GitDiffStatus::Text);
    let patch = gdiff.patch.unwrap();
    assert!(patch.contains("Bearer "));

    let manual = unified_diff("a.rs", "one\ntwo\nthree\n", "one\nTWO\nthree\n", 1);
    assert!(manual.contains("-two\n+TWO"));
}

#[tokio::test]
async fn gitignore_filtering_respects_negation_dirs_rooted_and_include_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    std::fs::write(
        root.join(".gitignore"),
        "# Project ignore rules\n\n*.ignored.rs\nbuild/\n/root_only.txt\ncoverage/**\n!keep.ignored.rs\n",
    )
    .unwrap();

    std::fs::create_dir_all(root.join("src/nested")).unwrap();
    std::fs::create_dir_all(root.join("build")).unwrap();
    std::fs::create_dir_all(root.join("coverage")).unwrap();

    std::fs::write(root.join("src/main.rs"), "pub fn needle_token() {}\n").unwrap();
    std::fs::write(
        root.join("src/skip.ignored.rs"),
        "pub fn needle_token() {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/keep.ignored.rs"),
        "pub fn needle_token() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("build/out.rs"), "pub fn needle_token() {}\n").unwrap();
    std::fs::write(
        root.join("coverage/report.rs"),
        "pub fn needle_token() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("root_only.txt"), "needle_token at root\n").unwrap();
    std::fs::write(root.join("src/root_only.txt"), "needle_token in subdir\n").unwrap();
    std::fs::write(root.join("src/nested/.gitignore"), "local_gen.rs\n").unwrap();
    std::fs::write(
        root.join("src/nested/local_gen.rs"),
        "pub fn needle_token() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("src/nested/ok.rs"), "pub fn needle_token() {}\n").unwrap();
    // Security-forbidden file must NEVER appear even with include_ignored: true
    std::fs::write(root.join(".env"), "SECRET=needle_token\n").unwrap();

    let task = sample_task(root);

    // 1. glob_files filters .gitignore by default while honoring !keep.ignored.rs and nested .gitignore
    let glob_default = workspace::execute(&task, "glob_files", &json!({"pattern": "**/*"}))
        .await
        .unwrap();
    let files: Vec<String> = serde_json::from_value(glob_default["files"].clone()).unwrap();
    assert!(files.contains(&"src/main.rs".to_owned()));
    assert!(files.contains(&"src/keep.ignored.rs".to_owned()));
    assert!(files.contains(&"src/root_only.txt".to_owned()));
    assert!(files.contains(&"src/nested/ok.rs".to_owned()));
    assert!(!files.contains(&"src/skip.ignored.rs".to_owned()));
    assert!(!files.contains(&"build/out.rs".to_owned()));
    assert!(!files.contains(&"coverage/report.rs".to_owned()));
    assert!(!files.contains(&"root_only.txt".to_owned()));
    assert!(!files.contains(&"src/nested/local_gen.rs".to_owned()));
    assert!(!files.contains(&".env".to_owned()));

    // 2. glob_files with include_ignored: true reveals gitignored files, but keeps .env forbidden!
    let glob_all = workspace::execute(
        &task,
        "glob_files",
        &json!({"pattern": "**/*", "include_ignored": true}),
    )
    .await
    .unwrap();
    let all_files: Vec<String> = serde_json::from_value(glob_all["files"].clone()).unwrap();
    assert!(all_files.contains(&"src/skip.ignored.rs".to_owned()));
    assert!(all_files.contains(&"build/out.rs".to_owned()));
    assert!(all_files.contains(&"root_only.txt".to_owned()));
    assert!(all_files.contains(&"src/nested/local_gen.rs".to_owned()));
    assert!(!all_files.contains(&".env".to_owned()));

    // 3. search_files filters .gitignore by default and supports include_ignored: true
    let search_default = workspace::execute(
        &task,
        "search_files",
        &json!({"query": "needle_token", "file_pattern": "*.rs"}),
    )
    .await
    .unwrap();
    let matched_paths: Vec<String> = search_default["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        matched_paths,
        vec![
            "src/keep.ignored.rs".to_owned(),
            "src/main.rs".to_owned(),
            "src/nested/ok.rs".to_owned()
        ]
    );

    // 4. list_files hides build/ and root_only.txt at root unless include_ignored: true
    let listed = workspace::execute(&task, "list_files", &json!({"path": "."}))
        .await
        .unwrap();
    let names: Vec<String> = listed["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"src".to_owned()));
    assert!(!names.contains(&"build".to_owned()));
    assert!(!names.contains(&"root_only.txt".to_owned()));
    assert!(!names.contains(&".env".to_owned()));

    let listed_all = workspace::execute(
        &task,
        "list_files",
        &json!({"path": ".", "include_ignored": true}),
    )
    .await
    .unwrap();
    let all_names: Vec<String> = listed_all["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(all_names.contains(&"build".to_owned()));
    assert!(all_names.contains(&"root_only.txt".to_owned()));
    assert!(!all_names.contains(&".env".to_owned()));
}

#[tokio::test]
async fn edit_file_supports_unified_patch_hunks_with_offset_and_whitespace_tolerance() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/service.rs"),
        "// header 1\n// header 2\n// header 3\npub fn compute(x: i32) -> i32 {   \n    let base = x + 1;\n    base * 2\n}\n\npub fn label() -> &'static str {\n    \"v1\"\n}\n",
    )
    .unwrap();

    let task = sample_task(root);

    // Hunk 1 intentionally has shifted line numbers (-20,4 instead of -4,4) and stripped trailing spaces on context!
    // Hunk 2 updates a second section in the same unified patch!
    let patch = "\
--- a/src/service.rs
+++ b/src/service.rs
@@ -20,4 +20,5 @@
 pub fn compute(x: i32) -> i32 {
-    let base = x + 1;
-    base * 2
+    let base = x + 10;
+    let scaled = base * 3;
+    scaled
 }
@@ -45,3 +46,3 @@
 pub fn label() -> &'static str {
-    \"v1\"
+    \"v2\"
 }
";

    let preview_diff = workspace::preview_file_diff(
        root,
        "edit_file",
        &json!({"path": "src/service.rs", "patch": patch}),
        &task.spec.write_scopes,
    )
    .unwrap();
    assert!(preview_diff.contains("+    let base = x + 10;"));
    assert!(preview_diff.contains("+\"v2\"") || preview_diff.contains("+    \"v2\""));

    let edited = workspace::execute(
        &task,
        "edit_file",
        &json!({
            "path": "src/service.rs",
            "patch": patch
        }),
    )
    .await
    .unwrap();
    assert_eq!(edited["replacements"], 2);

    let updated = std::fs::read_to_string(root.join("src/service.rs")).unwrap();
    assert!(updated.contains("let base = x + 10;\n    let scaled = base * 3;\n    scaled"));
    assert!(updated.contains("\"v2\""));
}

#[tokio::test]
async fn git_status_and_session_todo_tools_work_without_approval_friction() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "dev@example.com"])
        .current_dir(root)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Dev"])
        .current_dir(root)
        .status()
        .unwrap();

    std::fs::write(root.join("README.md"), "# Demo\n").unwrap();
    std::fs::write(root.join("tracked.rs"), "pub fn v() -> u32 {\n    1\n}\n").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "initial", "--quiet"])
        .current_dir(root)
        .status()
        .unwrap();

    let mut task = sample_task(root);
    task.run_id = format!("todo-git-status-run-{}", uuid::Uuid::new_v4());

    // Policy checks: git_status, todo_write, todo_read are classified as ToolEffect::None (Allow)
    assert_eq!(approval::classify("git_status"), ToolEffect::None);
    assert_eq!(approval::classify("todo_write"), ToolEffect::None);
    assert_eq!(approval::classify("todo_read"), ToolEffect::None);
    assert_eq!(
        approval::evaluate(&task, "git_status"),
        PolicyDecision::Allow
    );
    assert_eq!(
        approval::evaluate(&task, "todo_write"),
        PolicyDecision::Allow
    );
    assert_eq!(
        approval::evaluate(&task, "todo_read"),
        PolicyDecision::Allow
    );

    let defs = workspace::definitions(&task);
    for expected_tool in ["git_status", "todo_write", "todo_read"] {
        assert!(
            defs.iter().any(|d| d["function"]["name"] == expected_tool),
            "expected {expected_tool} in workspace::definitions"
        );
    }

    // 1. Clean git_status
    let clean_status = workspace::execute(&task, "git_status", &json!({}))
        .await
        .unwrap();
    assert_eq!(clean_status["clean"], true);
    assert!(!clean_status["branch"].as_str().unwrap().is_empty());

    // 2. Stage a file, modify another, and add an untracked file
    std::fs::write(root.join("README.md"), "# Demo Updated\n").unwrap();
    Command::new("git")
        .args(["add", "README.md"])
        .current_dir(root)
        .status()
        .unwrap();
    std::fs::write(root.join("tracked.rs"), "pub fn v() -> u32 {\n    2\n}\n").unwrap();
    std::fs::write(root.join("notes.txt"), "draft\n").unwrap();

    let dirty_status = workspace::execute(&task, "git_status", &json!({}))
        .await
        .unwrap();
    assert_eq!(dirty_status["clean"], false);
    assert_eq!(dirty_status["staged"], json!(["README.md"]));
    assert_eq!(dirty_status["modified"], json!(["tracked.rs"]));
    assert_eq!(dirty_status["untracked"], json!(["notes.txt"]));
    assert!(
        dirty_status["diff_stat"]
            .as_str()
            .unwrap()
            .contains("tracked.rs")
    );

    // 3. todo_write & todo_read & workspace::todos_for_run
    let written = workspace::execute(
        &task,
        "todo_write",
        &json!({
            "todos": [
                {"id": "1", "content": "Inspect git_status", "status": "completed"},
                {"id": "2", "content": "Apply unified patch", "status": "in_progress"},
                {"id": "3", "content": "Verify tests", "status": "pending"}
            ]
        }),
    )
    .await
    .unwrap();
    assert_eq!(written["summary"]["total"], 3);
    assert_eq!(written["summary"]["completed"], 1);
    assert_eq!(written["summary"]["in_progress"], 1);
    assert_eq!(written["summary"]["pending"], 1);

    let read_back = workspace::execute(&task, "todo_read", &json!({}))
        .await
        .unwrap();
    assert_eq!(read_back["todos"].as_array().unwrap().len(), 3);

    let cli_items = workspace::todos_for_run(&task.run_id);
    let expected_items = vec![
        TodoItem {
            id: "1".into(),
            content: "Inspect git_status".into(),
            status: "completed".into(),
        },
        TodoItem {
            id: "2".into(),
            content: "Apply unified patch".into(),
            status: "in_progress".into(),
        },
        TodoItem {
            id: "3".into(),
            content: "Verify tests".into(),
            status: "pending".into(),
        },
    ];
    assert_eq!(cli_items, expected_items);

    // 4. Simulate cross-process restart: clear in-memory RUN_TODOS cache and recover from task.messages!
    task.messages = vec![
        json!({"role": "user", "content": "track tasks"}),
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call-todo-old",
                "type": "function",
                "function": {
                    "name": "todo_write",
                    "arguments": json!({
                        "todos": [{"id": "1", "content": "Old item", "status": "pending"}]
                    }).to_string()
                }
            }]
        }),
        json!({"role": "tool", "tool_call_id": "call-todo-old", "content": "{}"}),
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call-todo-latest",
                "type": "function",
                "function": {
                    "name": "todo_write",
                    "arguments": json!({
                        "todos": [
                            {"id": "1", "content": "Inspect git_status", "status": "completed"},
                            {"id": "2", "content": "Apply unified patch", "status": "in_progress"},
                            {"id": "3", "content": "Verify tests", "status": "pending"}
                        ]
                    }).to_string()
                }
            }]
        }),
        json!({"role": "tool", "tool_call_id": "call-todo-latest", "content": "{}"}),
    ];
    workspace::clear_todo_cache_for_run(&task.run_id);
    assert!(workspace::todos_for_run(&task.run_id).is_empty());

    let recovered = workspace::todos_for_task(&task);
    assert_eq!(recovered, expected_items);
    assert_eq!(workspace::todos_for_run(&task.run_id), expected_items);

    workspace::clear_todo_cache_for_run(&task.run_id);
    let read_after_clear = workspace::execute(&task, "todo_read", &json!({}))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_value::<Vec<TodoItem>>(read_after_clear["todos"].clone()).unwrap(),
        expected_items
    );
}
