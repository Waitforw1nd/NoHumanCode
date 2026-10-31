use crate::{
    domain::*,
    store::Store,
    tool_results::{CommitDisposition, Origin},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Barrier},
};

pub(super) fn fixture(path: &Path) -> (Store, Task) {
    fixture_with_content(path, "x")
}

fn fixture_with_content(path: &Path, content: &str) -> (Store, Task) {
    fixture_call(path, "write_file", json!({"path":"a","content":content}))
}

pub(super) fn fixture_call(path: &Path, name: &str, args: Value) -> (Store, Task) {
    fixture_calls(
        path,
        vec![
            json!({"id":"call","type":"function","function":{"name":name,"arguments":args.to_string()}}),
        ],
    )
}

fn fixture_calls(path: &Path, calls: Vec<Value>) -> (Store, Task) {
    let store = Store::open(path).unwrap();
    let project = Project {
        id: "project".into(),
        name: "test".into(),
        root_path: path.parent().unwrap().to_string_lossy().into(),
        created_at: 10,
        updated_at: 10,
    };
    let session = Session {
        id: "session".into(),
        project_id: project.id.clone(),
        kind: SessionKind::Chat,
        title: "test".into(),
        legacy_run_id: "run".into(),
        created_at: 10,
        updated_at: 10,
    };
    let agent = Agent {
        id: "agent".into(),
        session_id: session.id.clone(),
        display_name: "test".into(),
        role: "test".into(),
        created_at: 10,
    };
    let turn = Turn {
        id: "turn".into(),
        session_id: session.id.clone(),
        project_id: project.id.clone(),
        status: LifecycleStatus::Queued,
        request_hash: "digest".into(),
        idempotency_key: None,
        created_at: 10,
        updated_at: 10,
    };
    let projected = TurnTask {
        id: "task-projection".into(),
        turn_id: turn.id.clone(),
        session_id: session.id.clone(),
        agent_id: agent.id.clone(),
        legacy_task_id: "task".into(),
        depends_on: vec![],
        status: LifecycleStatus::Queued,
        created_at: 10,
        updated_at: 10,
    };
    let mut task = Task {
        id: "task".into(),
        run_id: "run".into(),
        spec: TaskSpec {
            name: "test".into(),
            role: "test".into(),
            route_id: "route".into(),
            prompt: "hello".into(),
            depends_on: vec![],
            write_scopes: vec![".".into()],
            tools: true,
            allow_commands: true,
            max_rounds: 2,
        },
        route: Route {
            id: "route".into(),
            name: "test".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "test".into(),
            max_tokens: 64,
            parallel_limit: 1,
            key_env: None,
        },
        workspace: path.parent().unwrap().to_string_lossy().into(),
        status: "queued".into(),
        output: String::new(),
        error: None,
        messages: vec![
            json!({"role":"user","content":"hello"}),
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"call","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"a\",\"content\":\"x\"}"}}]}),
        ],
        usage: Value::Null,
        created_at: 10,
        updated_at: 10,
    };
    task.messages[1]["tool_calls"] = json!(calls);
    store.insert_project(&project).unwrap();
    store.insert_session(&session).unwrap();
    store.insert_agent(&agent).unwrap();
    store
        .commit_turn(&turn, &[projected], None, &[task.clone()])
        .unwrap();
    (store, task)
}

pub(super) const DENIED: &str = r#"{"error":"blocked_by_agent_policy"}"#;
pub(super) fn snapshot(store: &Store) -> (String, String, String, i64) {
    let db = store.db.lock().unwrap();
    (db.query_row("SELECT value FROM tasks WHERE id='task'", [], |r| r.get(0)).unwrap(),
     db.query_row("SELECT json_array(id,turn_id,session_id,agent_id,legacy_task_id,status,created_at,updated_at) FROM turn_tasks WHERE legacy_task_id='task'", [], |r| r.get(0)).unwrap(),
     db.query_row("SELECT json_array(id,session_id,project_id,status,request_hash,idempotency_key,created_at,updated_at) FROM turns WHERE id='turn'", [], |r| r.get(0)).unwrap(),
     db.query_row("SELECT count(*) FROM events WHERE task_id='task' AND kind='tool_result'", [], |r| r.get(0)).unwrap())
}

#[test]
fn t03_statement_and_actual_commit_failures_roll_back_every_projection() {
    for (case, sql) in [
        (
            "task",
            "CREATE TRIGGER injected BEFORE UPDATE ON tasks BEGIN SELECT RAISE(ABORT,'task failure'); END;",
        ),
        (
            "projection",
            "CREATE TRIGGER injected BEFORE UPDATE ON turn_tasks BEGIN SELECT RAISE(ABORT,'projection failure'); END;",
        ),
        (
            "event",
            "CREATE TRIGGER injected BEFORE INSERT ON events WHEN NEW.kind='tool_result' BEGIN SELECT RAISE(ABORT,'event failure'); END;",
        ),
        (
            "commit",
            "CREATE TABLE injected_parent(id INTEGER PRIMARY KEY); CREATE TABLE injected_child(id INTEGER REFERENCES injected_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER injected AFTER INSERT ON events WHEN NEW.kind='tool_result' BEGIN INSERT INTO injected_child VALUES(42); END;",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (store, task) = fixture(&dir.path().join("test.db"));
        let before = snapshot(&store);
        let original = task.messages.clone();
        store.db.lock().unwrap().execute_batch(sql).unwrap();
        let error = store
            .commit_tool_result(&task, "call", "write_file", DENIED, Origin::AgentDenied)
            .unwrap_err();
        let expected = match case {
            "task" => "task failure",
            "projection" => "projection failure",
            "event" => "event failure",
            "commit" => "FOREIGN KEY constraint failed",
            _ => unreachable!(),
        };
        assert!(format!("{error:#}").contains(expected), "{case}: {error:#}");
        let sqlite = error.downcast_ref::<rusqlite::Error>().unwrap();
        let rusqlite::Error::SqliteFailure(code, _) = sqlite else {
            panic!("{case}: {sqlite}");
        };
        assert_eq!(
            code.extended_code,
            if case == "commit" {
                rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
            } else {
                rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
            },
            "{case}"
        );
        assert_eq!(snapshot(&store), before, "{case}");
        assert_eq!(task.messages, original, "{case}");
        if case == "commit" {
            let count: i64 = store
                .db
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM injected_child", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
    }
}

#[test]
fn t04_two_stores_compete_and_replay_only_one_authoritative_result() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let (first, task) = fixture(&path);
    let second = Store::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .map(|store| {
            let barrier = barrier.clone();
            let task = task.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .commit_tool_result(&task, "call", "write_file", DENIED, Origin::AgentDenied)
                    .unwrap()
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        results
            .iter()
            .filter(|r| r.1 == CommitDisposition::Inserted)
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| r.1 == CommitDisposition::Replayed)
            .count(),
        1
    );
    assert_eq!(results[0].0.messages, results[1].0.messages);
    let store = Store::open(&path).unwrap();
    let before = snapshot(&store);
    assert_eq!(before.3, 1);
    for (name, content, origin) in [
        ("write_file", "different", Origin::AgentDenied),
        ("write_file", DENIED, Origin::InterruptedWithoutClaim),
        ("run_command", DENIED, Origin::AgentDenied),
    ] {
        assert!(
            store
                .commit_tool_result(&task, "call", name, content, origin)
                .is_err()
        );
        assert_eq!(snapshot(&store), before);
    }
    let mut wrong = task.clone();
    wrong.run_id = "other".into();
    assert!(
        store
            .commit_tool_result(&wrong, "call", "write_file", DENIED, Origin::AgentDenied)
            .is_err()
    );
    assert_eq!(snapshot(&store), before);
}

#[test]
fn t12_reserved_fields_are_rejected_even_when_null() {
    let dir = tempfile::tempdir().unwrap();
    let (store, task) = fixture(&dir.path().join("test.db"));
    let before = snapshot(&store);
    let count = store.events(&task.run_id, 0).unwrap().len();
    for (kind, value) in [
        ("tool_result", json!({"origin":null})),
        ("tool_result", json!({"origin_version":null})),
        ("approval.requested", json!({"evidence_version":null})),
        ("approval.requested", json!({"safe_call_digest":null})),
    ] {
        assert!(store.event(&task.id, kind, value).is_err());
        assert_eq!(snapshot(&store), before);
        assert_eq!(store.events(&task.run_id, 0).unwrap().len(), count);
    }
}

#[test]
fn t05_result_commit_child() {
    let Ok(root) = std::env::var("NHC_T05_CHILD_ROOT") else {
        return;
    };
    let phase = match std::env::var("NHC_T05_CHILD_PHASE").unwrap().as_str() {
        "before_commit" => "before_commit",
        "after_commit" => "after_commit",
        _ => panic!("invalid phase"),
    };
    let path = std::path::PathBuf::from(root).join("test.db");
    let store = Store::open(&path).unwrap();
    let task = store.task("task").unwrap();
    let database: String = store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    crate::tool_results::crash_probe::arm(
        database,
        task.id.clone(),
        "call".into(),
        phase,
        path.parent().unwrap().join("signal"),
    );
    store
        .commit_tool_result(&task, "call", "write_file", DENIED, Origin::AgentDenied)
        .unwrap();
    panic!("probe did not stop child");
}

#[test]
fn t05_kill_before_and_after_commit_preserves_atomic_facts() {
    use sha2::{Digest, Sha256};
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if !matches!(self.0.try_wait(), Ok(Some(_))) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    for phase in ["before_commit", "after_commit"] {
        let root = tempfile::Builder::new()
            .prefix("nhc-t05-")
            .tempdir()
            .unwrap()
            .keep();
        let (store, _) = fixture(&root.join("test.db"));
        let initial = snapshot(&store);
        drop(store);
        let stdout = std::fs::File::create(root.join("child-stdout.log")).unwrap();
        let stderr = std::fs::File::create(root.join("child-stderr.log")).unwrap();
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tool_result_tests::t05_result_commit_child",
                    "--nocapture",
                ])
                .env("NHC_T05_CHILD_ROOT", &root)
                .env("NHC_T05_CHILD_PHASE", phase)
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .spawn()
                .unwrap(),
        );
        let pid = child.0.id();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !root.join("signal").is_file() {
            if let Some(status) = child.0.try_wait().unwrap() {
                panic!("child {pid} exited early: {status}");
            }
            if Instant::now() >= deadline {
                panic!("child {pid} did not reach {phase}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(root.join("signal")).unwrap(),
            format!("task\ncall\n{phase}\n")
        );
        assert!(child.0.try_wait().unwrap().is_none());
        child.0.kill().unwrap();
        let status = child.0.wait().unwrap();
        assert!(!status.success());
        let mut manifest = vec![];
        for folder in ["raw", "query", "recovery"] {
            std::fs::create_dir(root.join(folder)).unwrap();
        }
        for name in ["test.db", "test.db-wal", "test.db-shm"] {
            let path = root.join(name);
            if path.exists() {
                let bytes = std::fs::read(&path).unwrap();
                let hash = format!("{:x}", Sha256::digest(&bytes));
                for folder in ["raw", "query", "recovery"] {
                    std::fs::write(root.join(folder).join(name), &bytes).unwrap();
                }
                assert_eq!(std::fs::read(root.join("raw").join(name)).unwrap(), bytes);
                manifest
                    .push(json!({"name":name,"present":true,"sha256":hash,"bytes":bytes.len()}));
            } else {
                manifest.push(json!({"name":name,"present":false}));
            }
        }
        std::fs::write(root.join("process.json"), serde_json::to_vec_pretty(&json!({"pid":pid,"phase":phase,"task":"task","call":"call","kill":"ok","wait":format!("{status:?}"),"reaped":true,"files":manifest})).unwrap()).unwrap();
        let query = Store::open(&root.join("query/test.db")).unwrap();
        let observed = snapshot(&query);
        assert_eq!(observed.3, if phase == "after_commit" { 1 } else { 0 });
        let messages = query.task("task").unwrap().messages;
        let mut initial_task: Value = serde_json::from_str(&initial.0).unwrap();
        let mut actual_task: Value = serde_json::from_str(&observed.0).unwrap();
        let mut expected_messages = initial_task["messages"].as_array().unwrap().clone();
        if phase == "after_commit" {
            expected_messages.push(json!({"role":"tool","tool_call_id":"call","content":DENIED}));
        }
        assert_eq!(messages, expected_messages);
        assert_eq!(actual_task["messages"], json!(expected_messages));
        initial_task.as_object_mut().unwrap().remove("messages");
        actual_task.as_object_mut().unwrap().remove("messages");
        if phase == "after_commit" {
            assert!(actual_task["updated_at"].as_i64().unwrap() >= 10);
            initial_task.as_object_mut().unwrap().remove("updated_at");
            actual_task.as_object_mut().unwrap().remove("updated_at");
        }
        assert_eq!(actual_task, initial_task);
        let projection: Value = serde_json::from_str(&observed.1).unwrap();
        let initial_projection: Value = serde_json::from_str(&initial.1).unwrap();
        assert_eq!(
            &projection.as_array().unwrap()[..7],
            &initial_projection.as_array().unwrap()[..7]
        );
        assert_eq!(
            projection[7],
            serde_json::from_str::<Value>(&observed.0).unwrap()["updated_at"]
        );
        assert_eq!(observed.2, initial.2);
        let events: Vec<_> = query
            .events("run", 0)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "tool_result")
            .collect();
        if phase == "after_commit" {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].task_id, "task");
            assert_eq!(events[0].turn_id.as_deref(), Some("turn"));
            assert_eq!(events[0].session_id.as_deref(), Some("session"));
            assert_eq!(
                events[0].data,
                json!({"name":"write_file","tool_call_id":"call",
                "result":{"error":"blocked_by_agent_policy"},"origin_version":1,"origin":"agent_denied"})
            );
        } else {
            assert!(events.is_empty());
        }
        drop(query);
        for _ in 0..2 {
            let recovery = Store::open(&root.join("recovery/test.db")).unwrap();
            recovery.recover().unwrap();
            assert_eq!(snapshot(&recovery).3, observed.3);
            assert_eq!(recovery.task("task").unwrap().messages, messages);
        }
        println!("t05 phase={phase} pid={pid} evidence={}", root.display());
    }
}

#[test]
fn t07_safe_approval_proof_corruption_and_legacy_boundaries() {
    use crate::approval::{self, ApprovalStatus};
    for case in [
        "version",
        "partial",
        "digest",
        "event_scope",
        "event_task",
        "event_approval",
        "event_call",
        "event_tool",
        "workspace",
        "scopes",
        "commands",
        "args_digest",
        "binding_digest",
        "claimed",
        "unknown",
        "finished",
        "pending",
        "workspace_change",
        "legacy_redacted",
        "legacy_raw",
        "valid",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let content = if case == "legacy_raw" {
            "ordinary"
        } else {
            "sk-abc"
        };
        let (store, task) = fixture_with_content(&dir.path().join("proof.db"), content);
        let args = json!({"path":"a","content":content});
        let (approval, created) = store
            .ensure_approval(&task, "call", "write_file", &args)
            .unwrap();
        assert!(created, "{case}");
        assert!(
            !store
                .ensure_approval(&task, "call", "write_file", &args)
                .unwrap()
                .1
        );
        if case != "pending" {
            store.decide_approval(&approval.id, false, "user").unwrap();
        }
        let durable = store.task(&task.id).unwrap();
        assert!(!serde_json::to_string(&durable).unwrap().contains("sk-abc"));
        let sql = match case {
            "version" => {
                "UPDATE events SET data=json_set(data,'$.evidence_version',2) WHERE kind='approval.requested'"
            }
            "partial" => {
                "UPDATE events SET data=json_remove(data,'$.safe_call_digest') WHERE kind='approval.requested'"
            }
            "digest" => {
                "UPDATE events SET data=json_set(data,'$.safe_call_digest','invalid') WHERE kind='approval.requested'"
            }
            "event_scope" => "UPDATE events SET turn_id=NULL WHERE kind='approval.requested'",
            "event_task" => {
                "UPDATE events SET data=json_set(data,'$.task_id','other') WHERE kind='approval.requested'"
            }
            "event_approval" => {
                "UPDATE events SET data=json_set(data,'$.approval_id','other') WHERE kind='approval.requested'"
            }
            "event_call" => {
                "UPDATE events SET data=json_set(data,'$.tool_call_id','other') WHERE kind='approval.requested'"
            }
            "event_tool" => {
                "UPDATE events SET data=json_set(data,'$.tool_name','run_command') WHERE kind='approval.requested'"
            }
            "workspace" => "UPDATE approvals SET workspace='other'",
            "scopes" => "UPDATE approvals SET write_scopes='[]'",
            "commands" => "UPDATE approvals SET allow_commands=0",
            "args_digest" => {
                "UPDATE approvals SET args_digest='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'"
            }
            "binding_digest" => {
                "UPDATE approvals SET binding_digest='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'"
            }
            "claimed" => "UPDATE approvals SET execution_state='claimed'",
            "unknown" => "UPDATE approvals SET execution_state='unknown'",
            "finished" => "UPDATE approvals SET execution_state='finished'",
            "pending" => "SELECT 1",
            "workspace_change" => {
                "INSERT INTO workspace_changes(id,task_id,tool_call_id,workspace_digest,binding_digest,write_scopes,path,path_key,kind,state,restore_state,created_at) VALUES('change','task','call','digest','binding','[]','a','a','created','finished','pending',10)"
            }
            "legacy_redacted" | "legacy_raw" => {
                "UPDATE events SET data=json_remove(data,'$.evidence_version','$.safe_call_digest') WHERE kind='approval.requested'"
            }
            "valid" => "SELECT 1",
            _ => unreachable!(),
        };
        store.db.lock().unwrap().execute_batch(sql).unwrap();
        let before = snapshot(&store);
        let count = store.events(&task.run_id, 0).unwrap().len();
        let result = store.commit_tool_result(
            &durable,
            "call",
            "write_file",
            &approval::tool_error(ApprovalStatus::Denied),
            Origin::ApprovalDenied,
        );
        if matches!(case, "valid" | "legacy_raw") {
            let (result, disposition) = result.unwrap();
            assert_eq!(disposition, CommitDisposition::Inserted);
            assert_eq!(result.messages.last().unwrap()["tool_call_id"], "call");
            assert_eq!(snapshot(&store).3, 1);
        } else {
            assert!(result.is_err(), "{case}");
            assert_eq!(snapshot(&store), before, "{case}");
            assert_eq!(
                store.events(&task.run_id, 0).unwrap().len(),
                count,
                "{case}"
            );
        }
    }
}

#[test]
fn t03_approval_request_and_safe_proof_share_a_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let (store, task) = fixture_with_content(&dir.path().join("approval.db"), "sk-abc");
    let before = snapshot(&store);
    let events_before = store.events(&task.run_id, 0).unwrap().len();
    store.db.lock().unwrap().execute_batch("CREATE TRIGGER reject_request BEFORE INSERT ON events WHEN NEW.kind='approval.requested' BEGIN SELECT RAISE(ABORT,'proof insert failed'); END;").unwrap();
    let args = json!({"path":"a","content":"sk-abc"});
    let error = store
        .ensure_approval(&task, "call", "write_file", &args)
        .unwrap_err();
    assert!(format!("{error:#}").contains("proof insert failed"));
    assert!(store.approvals_for_task(&task.id).unwrap().is_empty());
    assert_eq!(snapshot(&store), before);
    assert_eq!(store.events(&task.run_id, 0).unwrap().len(), events_before);
    store
        .db
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_request;")
        .unwrap();
    let (approval, created) = store
        .ensure_approval(&task, "call", "write_file", &args)
        .unwrap();
    assert!(created);
    assert_eq!(
        store
            .ensure_approval(&task, "call", "write_file", &args)
            .unwrap()
            .0
            .id,
        approval.id
    );
    let events = store.events(&task.run_id, 0).unwrap();
    let requests: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "approval.requested")
        .collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].data["evidence_version"], 1);
    assert_eq!(
        requests[0].data["safe_call_digest"].as_str().unwrap().len(),
        64
    );
    assert!(!requests[0].data.to_string().contains("sk-abc"));
}

#[test]
fn t07_duplicate_safe_proof_is_not_accepted() {
    use crate::approval::{self, ApprovalStatus};
    let dir = tempfile::tempdir().unwrap();
    let (store, task) = fixture_with_content(&dir.path().join("duplicate.db"), "sk-abc");
    let args = json!({"path":"a","content":"sk-abc"});
    let (approval, _) = store
        .ensure_approval(&task, "call", "write_file", &args)
        .unwrap();
    store.decide_approval(&approval.id, false, "user").unwrap();
    let task = store.task(&task.id).unwrap();
    store.db.lock().unwrap().execute("INSERT INTO events(task_id,kind,data,at,turn_id,session_id) SELECT task_id,kind,data,at,turn_id,session_id FROM events WHERE kind='approval.requested'", []).unwrap();
    let before = snapshot(&store);
    let count = store.events(&task.run_id, 0).unwrap().len();
    assert!(
        store
            .commit_tool_result(
                &task,
                "call",
                "write_file",
                &approval::tool_error(ApprovalStatus::Denied),
                Origin::ApprovalDenied
            )
            .is_err()
    );
    assert_eq!(snapshot(&store), before);
    assert_eq!(store.events(&task.run_id, 0).unwrap().len(), count);
}

#[test]
fn t07_legacy_read_orphans_need_unique_matching_scope_and_no_v1_fields() {
    for case in [
        "no_id",
        "with_id",
        "wrong_name",
        "wrong_id",
        "wrong_scope",
        "duplicate",
        "v1",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (store, task) = fixture_call(
            &dir.path().join("legacy.db"),
            "read_file",
            json!({"path":"a"}),
        );
        let mut data = json!({"name":"read_file","result":{"text":"ordinary 文本🙂"}});
        match case {
            "with_id" => data["tool_call_id"] = json!("call"),
            "wrong_name" => data["name"] = json!("list_files"),
            "wrong_id" => data["tool_call_id"] = json!("other"),
            "v1" => data["origin_version"] = json!(2),
            _ => {}
        }
        if case == "v1" {
            store
                .event(
                    &task.id,
                    "tool_result",
                    json!({"name":"read_file","result":{"text":"ordinary 文本🙂"}}),
                )
                .unwrap();
            store
                .db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE events SET data=?1 WHERE kind='tool_result'",
                    [data.to_string()],
                )
                .unwrap();
        } else {
            store.event(&task.id, "tool_result", data.clone()).unwrap();
        }
        if case == "duplicate" {
            store.event(&task.id, "tool_result", data).unwrap();
        }
        if case == "wrong_scope" {
            store
                .db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE events SET turn_id=NULL WHERE kind='tool_result'",
                    [],
                )
                .unwrap();
        }
        let before = snapshot(&store);
        let original: Vec<_> = store
            .events(&task.run_id, 0)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "tool_result")
            .map(|e| e.data)
            .collect();
        let result = store.recover_legacy_read_result(&task, "call", "read_file");
        if matches!(case, "no_id" | "with_id") {
            let recovered = result.unwrap().unwrap();
            let message = recovered.messages.last().unwrap();
            assert_eq!(message["tool_call_id"], "call");
            assert_eq!(
                serde_json::from_str::<Value>(message["content"].as_str().unwrap()).unwrap(),
                json!({"text":"ordinary 文本🙂"})
            );
            assert_eq!(snapshot(&store).3, 1);
        } else {
            assert!(result.is_err(), "{case}");
            assert_eq!(snapshot(&store), before, "{case}");
        }
        let after: Vec<_> = store
            .events(&task.run_id, 0)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "tool_result")
            .map(|e| e.data)
            .collect();
        assert_eq!(after, original, "{case}");
    }
}

#[test]
fn t04_group_order_and_stale_prefix_reject_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("group.db");
    let calls = ["first", "second"].into_iter().map(|id| json!({"id":id,"type":"function","function":{"name":"write_file","arguments":"{\"path\":\"a\",\"content\":\"x\"}"}})).collect();
    let (store, task) = fixture_calls(&path, calls);
    let before = snapshot(&store);
    assert!(
        store
            .commit_tool_result(&task, "second", "write_file", DENIED, Origin::AgentDenied)
            .is_err()
    );
    assert_eq!(snapshot(&store), before);
    let (first, _) = store
        .commit_tool_result(&task, "first", "write_file", DENIED, Origin::AgentDenied)
        .unwrap();
    let after_first = snapshot(&store);
    assert!(
        store
            .commit_tool_result(&task, "second", "write_file", DENIED, Origin::AgentDenied)
            .is_err()
    );
    assert_eq!(snapshot(&store), after_first);
    let (second, _) = store
        .commit_tool_result(&first, "second", "write_file", DENIED, Origin::AgentDenied)
        .unwrap();
    assert_eq!(snapshot(&store).3, 2);
    assert_eq!(second.messages[2]["tool_call_id"], "first");
    assert_eq!(second.messages[3]["tool_call_id"], "second");
}

#[test]
fn t09_cancelled_state_survives_late_result_and_other_store_stale_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel.db");
    let (store, task) = fixture(&path);
    let other = Store::open(&path).unwrap();
    let mut cancelled = task.clone();
    cancelled.status = "cancelled".into();
    cancelled.error = Some("cancelled by user".into());
    store.save_task(&cancelled).unwrap();
    let (result, _) = other
        .commit_tool_result(&task, "call", "write_file", DENIED, Origin::AgentDenied)
        .unwrap();
    assert_eq!(result.status, "cancelled");
    assert_eq!(result.error, cancelled.error);
    assert_eq!(result.messages.len(), task.messages.len() + 1);
    store.save_task(&cancelled).unwrap();
    let authoritative = other.task(&task.id).unwrap();
    assert_eq!(authoritative.status, "cancelled");
    assert_eq!(authoritative.error, cancelled.error);
    assert_eq!(authoritative.messages, result.messages);
    assert_eq!(snapshot(&other).3, 1);
}

#[test]
fn t12_read_result_json_unicode_and_secret_redaction_are_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let (store, task) = fixture_call(
        &dir.path().join("unicode.db"),
        "read_file",
        json!({"path":"a"}),
    );
    let raw = json!({"z":"普通🙂","a":"Bearer sk-abc"}).to_string();
    let (result, disposition) = store
        .commit_tool_result(&task, "call", "read_file", &raw, Origin::ReadCompleted)
        .unwrap();
    assert_eq!(disposition, CommitDisposition::Inserted);
    let message: Value =
        serde_json::from_str(result.messages.last().unwrap()["content"].as_str().unwrap()).unwrap();
    let events = store.events(&task.run_id, 0).unwrap();
    let event = events.iter().find(|e| e.kind == "tool_result").unwrap();
    assert_eq!(event.data["result"], message);
    assert_eq!(event.data["origin"], "read_completed");
    assert_eq!(message["z"], "普通🙂");
    assert!(!message.to_string().contains("sk-abc"));
    let before = snapshot(&store);
    let alternate = r#"{ "z": "普通🙂", "a": "Bearer sk-abc" }"#;
    let (replay, disposition) = store
        .commit_tool_result(&task, "call", "read_file", alternate, Origin::ReadCompleted)
        .unwrap();
    assert_eq!(disposition, CommitDisposition::Replayed);
    assert_eq!(replay.messages, result.messages);
    assert_eq!(snapshot(&store), before);
}

fn stored_task_json(store: &Store) -> (String, Value) {
    let db = store.db.lock().unwrap();
    let raw: String = db
        .query_row("SELECT value FROM tasks WHERE id='task'", [], |r| r.get(0))
        .unwrap();
    let parsed = serde_json::from_str(&raw).unwrap();
    (raw, parsed)
}

fn task_arguments(task: &Task) -> &str {
    task.messages[1]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap()
}

#[test]
fn p01_sensitive_argument_object_survives_real_task_persistence_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parameter-object.db");
    // This is a persistence fixture, not a claim about executable write_file schema.
    let args = json!({"text":"Bearer n19fixturetoken", "nested":{"API_KEY":"n19nestedfixture", "label":"普通🙂 \"quoted\" \\ path"}, "items":[true, 17, null, {"enabled":false}], "ratio":1.5});
    let (store, _) = fixture_call(&path, "persistence_fixture", args);
    let task = store.task("task").unwrap();
    let first = task_arguments(&task).to_owned();
    let safe: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(
        safe,
        json!({"text":"bearer [redacted]", "nested":{"API_KEY":"[redacted]", "label":"普通🙂 \"quoted\" \\ path"}, "items":[true, 17, null, {"enabled":false}], "ratio":1.5})
    );
    let (raw, persisted): (String, Value) = stored_task_json(&store);
    assert_eq!(
        persisted["messages"][1]["tool_calls"][0]["function"]["arguments"].as_str(),
        Some(first.as_str())
    );
    for secret in ["n19fixturetoken", "n19nestedfixture"] {
        assert!(!raw.contains(secret));
        assert!(!serde_json::to_string(&task).unwrap().contains(secret));
    }
    store.save_task(&task).unwrap();
    drop(store);
    let reopened = Store::open(&path).unwrap();
    let saved = reopened.task("task").unwrap();
    assert_eq!(task_arguments(&saved), first);
    assert_eq!(
        serde_json::from_str::<Value>(task_arguments(&saved)).unwrap(),
        safe
    );
    let (raw_again, persisted_again) = stored_task_json(&reopened);
    assert_eq!(
        persisted_again["messages"][1]["tool_calls"][0]["function"]["arguments"].as_str(),
        Some(first.as_str())
    );
    for secret in ["n19fixturetoken", "n19nestedfixture"] {
        assert!(!raw_again.contains(secret));
    }
    reopened.save_task(&saved).unwrap();
    drop(reopened);
    let again = Store::open(&path).unwrap();
    assert_eq!(task_arguments(&again.task("task").unwrap()), first);
}

#[test]
fn p02_safe_legacy_raw_and_invalid_arguments_remain_unmodified() {
    for (case, raw) in [
        ("object", r#"{  "z" : "普通🙂", "a" : [3, true] }"#),
        ("malformed", "{not-json"),
        ("array", "[1,2]"),
        ("scalar", "false"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{case}.db"));
        let call = json!({"id":"call","type":"function","function":{"name":"legacy_fixture","arguments":raw}});
        let (store, _) = fixture_calls(&path, vec![call]);
        let task = store.task("task").unwrap();
        assert_eq!(task_arguments(&task), raw, "{case}");
        let (_, persisted) = stored_task_json(&store);
        assert_eq!(
            persisted["messages"][1]["tool_calls"][0]["function"]["arguments"].as_str(),
            Some(raw)
        );
        let parsed = serde_json::from_str::<Value>(task_arguments(&task));
        if case == "object" {
            assert!(parsed.unwrap().is_object());
        } else {
            assert!(
                parsed.is_err() || !parsed.unwrap().is_object(),
                "{case} must not become an object"
            );
        }
        store.save_task(&task).unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        let saved = reopened.task("task").unwrap();
        assert_eq!(task_arguments(&saved), raw, "{case}");
        let (_, persisted) = stored_task_json(&reopened);
        assert_eq!(
            persisted["messages"][1]["tool_calls"][0]["function"]["arguments"].as_str(),
            Some(raw)
        );
    }
}

#[test]
fn t07_no_id_duplicate_result_cannot_be_attributed_to_the_next_call() {
    let dir = tempfile::tempdir().unwrap();
    let calls = ["first", "second"].into_iter().map(|id| json!({"id":id,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}})).collect();
    let (store, mut task) = fixture_calls(&dir.path().join("ambiguous.db"), calls);
    let data = json!({"name":"read_file","result":{"content":"same"}});
    store.event(&task.id, "tool_result", data.clone()).unwrap();
    task.messages
        .push(json!({"role":"tool","tool_call_id":"first","content":"{\"content\":\"same\"}"}));
    store.save_task(&task).unwrap();
    store.event(&task.id, "tool_result", data).unwrap();
    let before = snapshot(&store);
    let original: Vec<_> = store
        .events(&task.run_id, 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "tool_result")
        .map(|e| (e.seq, e.data))
        .collect();
    assert!(
        store
            .recover_legacy_read_result(&task, "second", "read_file")
            .is_err()
    );
    assert_eq!(snapshot(&store), before);
    let after: Vec<_> = store
        .events(&task.run_id, 0)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "tool_result")
        .map(|e| (e.seq, e.data))
        .collect();
    assert_eq!(after, original);
}

#[test]
fn t04_read_conflict_and_authoritative_replay_after_other_store_append() {
    use crate::tool_results::ToolResultError;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("read-race.db");
    let calls = ["read-a", "read-b"].into_iter().map(|id| json!({"id":id,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}})).collect();
    let (store, original) = fixture_calls(&path, calls);
    let other = Store::open(&path).unwrap();
    let (first, inserted) = store
        .commit_tool_result(
            &original,
            "read-a",
            "read_file",
            r#"{"text":"A"}"#,
            Origin::ReadCompleted,
        )
        .unwrap();
    assert_eq!(inserted, CommitDisposition::Inserted);
    let after_a = snapshot(&store);
    let events_a = serde_json::to_value(store.events(&original.run_id, 0).unwrap()).unwrap();
    let error = other
        .commit_tool_result(
            &original,
            "read-a",
            "read_file",
            r#"{"text":"B"}"#,
            Origin::ReadCompleted,
        )
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ToolResultError>(),
        Some(ToolResultError::Conflict | ToolResultError::Corrupt)
    ));
    assert_eq!(snapshot(&store), after_a);
    assert_eq!(
        serde_json::to_value(store.events(&original.run_id, 0).unwrap()).unwrap(),
        events_a
    );

    let error = other
        .commit_tool_result(
            &original,
            "read-b",
            "read_file",
            r#"{"text":"second"}"#,
            Origin::ReadCompleted,
        )
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ToolResultError>(),
        Some(ToolResultError::Conflict | ToolResultError::Corrupt)
    ));
    assert_eq!(snapshot(&store), after_a);
    assert_eq!(
        serde_json::to_value(store.events(&original.run_id, 0).unwrap()).unwrap(),
        events_a
    );

    let (current, inserted) = other
        .commit_tool_result(
            &first,
            "read-b",
            "read_file",
            r#"{"text":"second"}"#,
            Origin::ReadCompleted,
        )
        .unwrap();
    assert_eq!(inserted, CommitDisposition::Inserted);
    let after_b = snapshot(&store);
    let events_b = serde_json::to_value(store.events(&original.run_id, 0).unwrap()).unwrap();
    assert_eq!(after_b.3, 2);
    let (replayed, disposition) = store
        .commit_tool_result(
            &original,
            "read-a",
            "read_file",
            r#"{"text":"A"}"#,
            Origin::ReadCompleted,
        )
        .unwrap();
    assert_eq!(disposition, CommitDisposition::Replayed);
    assert_eq!(
        serde_json::to_value(&replayed).unwrap(),
        serde_json::to_value(&current).unwrap()
    );
    assert_eq!(replayed.messages.len(), original.messages.len() + 2);
    assert_eq!(replayed.messages.last().unwrap()["tool_call_id"], "read-b");
    assert_eq!(snapshot(&store), after_b);
    assert_eq!(
        serde_json::to_value(store.events(&original.run_id, 0).unwrap()).unwrap(),
        events_b
    );
}
