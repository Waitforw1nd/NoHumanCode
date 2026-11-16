//! Regressions use the real operation object and the transaction being written.
#![cfg(windows)]

use crate::{engine::Engine, store::Store};
use rusqlite::TransactionBehavior;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn runtime_security_task(name: &str) -> crate::domain::TaskSpec {
    crate::domain::TaskSpec {
        name: name.into(),
        role: format!("role-{name}"),
        route_id: "key-1".into(),
        prompt: "hello".into(),
        depends_on: vec![],
        write_scopes: vec![],
        tools: false,
        allow_commands: false,
        max_rounds: 4,
    }
}

fn runtime_security_fixture(dir: &std::path::Path) -> Arc<Engine> {
    use crate::{
        domain::{Route, Settings},
        network_authorization::{
            ConfirmRequest, NetworkPurpose, NetworkSubject, NetworkSubjectKind,
        },
        project_config::{ConfigureProject, ProjectConfig, RegisterProject, SelectProject},
    };
    let store = Arc::new(Store::open(&dir.join("test.db")).unwrap());
    let routes = (1..=3)
        .map(|n| Route {
            id: format!("key-{n}"),
            name: format!("Key {n}"),
            base_url: "http://127.0.0.1:1/v1".into(),
            model: if n == 2 { "model-b" } else { "model-a" }.into(),
            max_tokens: 128,
            parallel_limit: 1,
            key_env: None,
        })
        .collect();
    store
        .save_settings(&Settings {
            workspace: dir.to_string_lossy().into(),
            max_concurrency: 2,
            routes,
            newapi: None,
        })
        .unwrap();
    for n in 1..=3 {
        store
            .put_secret(
                &format!("key-{n}"),
                if n == 2 { "fake-key-b" } else { "fake-key-a" },
            )
            .unwrap();
    }
    let selection = store.current_project().unwrap();
    let project = store
        .register_project(RegisterProject {
            name: "fixture".into(),
            root_path: dir.to_string_lossy().into(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
        })
        .unwrap();
    store
        .select_project(SelectProject {
            project_id: project.id.clone(),
            expected_revision: selection.revision,
        })
        .unwrap();
    let before = store.project_config(&project.id).unwrap();
    store
        .configure_project(ConfigureProject {
            project_id: project.id.clone(),
            expected_revision: before.revision,
            config: ProjectConfig {
                allowed_route_ids: vec!["key-1".into(), "key-2".into(), "key-3".into()],
                default_route_id: Some("key-1".into()),
                tools: true,
                write_scopes: vec!["*".into()],
                allow_commands: true,
            },
        })
        .unwrap();
    for n in 1..=3 {
        let subject = NetworkSubject {
            kind: NetworkSubjectKind::Route,
            id: format!("key-{n}"),
            project_id: Some(project.id.clone()),
        };
        let preview = store.network_preview(subject.clone()).unwrap();
        store
            .network_confirm(
                subject,
                ConfirmRequest {
                    config_revision: preview.config_revision,
                    credential_revision: preview.credential_revision,
                    binding_digest: preview.binding_digest,
                    purposes: vec![NetworkPurpose::Models, NetworkPurpose::Completion],
                },
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
    }
    Engine::new(store, 2).unwrap()
}

#[allow(dead_code)]
#[path = "../tests/support/owned_child.rs"]
mod owned_child;

fn setup() -> (tempfile::TempDir, Arc<Store>, Arc<Engine>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("domain.db")).unwrap());
    let engine = Engine::new(store.clone(), 1).unwrap();
    (dir, store, engine)
}

#[tokio::test]
async fn standalone_project_session_agent_inserts_require_matching_active_operation() {
    use crate::domain::{Agent, Project, Session};
    use peachsh_protocol::{AgentId, ProjectId, SessionId, SessionKind};
    let (_dir, store, _engine) = setup();
    let (_foreign_dir, foreign, _foreign_engine) = setup();
    let project = Project {
        id: ProjectId("u18-project".into()),
        name: "project".into(),
        root_path: "C:/workspace".into(),
        created_at: 1,
        updated_at: 1,
    };
    let session = Session {
        id: SessionId("u18-session".into()),
        project_id: project.id.clone(),
        kind: SessionKind::Team,
        title: "session".into(),
        legacy_run_id: "u18-run".into(),
        created_at: 1,
        updated_at: 1,
    };
    let agent = Agent {
        id: AgentId("u18-agent".into()),
        session_id: session.id.clone(),
        display_name: "agent".into(),
        role: "worker".into(),
        created_at: 1,
    };
    let op = store.execution_operation(CancellationToken::new()).unwrap();
    op.run(async {
        store.insert_project(&project).unwrap();
        store.insert_session(&session).unwrap();
        store.insert_agent(&agent).unwrap();
    })
    .await;
    let mut next_project = project.clone();
    next_project.id = ProjectId("u18-project-2".into());
    let mut next_session = session.clone();
    next_session.id = SessionId("u18-session-2".into());
    next_session.legacy_run_id = "u18-run-2".into();
    let mut next_agent = agent.clone();
    next_agent.id = AgentId("u18-agent-2".into());
    let counts = || {
        let db = store.db.lock().unwrap();
        ["projects", "sessions", "agents"].map(|table| {
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        })
    };
    assert_eq!(counts(), [1, 1, 1]);
    assert!(store.insert_project(&next_project).is_err());
    assert!(store.insert_session(&next_session).is_err());
    assert!(store.insert_agent(&next_agent).is_err());
    assert_eq!(counts(), [1, 1, 1]);
    let foreign_op = foreign
        .execution_operation(CancellationToken::new())
        .unwrap();
    foreign_op
        .run(async {
            assert!(store.insert_project(&next_project).is_err());
            assert!(store.insert_session(&next_session).is_err());
            assert!(store.insert_agent(&next_agent).is_err());
        })
        .await;
    assert_eq!(counts(), [1, 1, 1]);
    let closed_op = store.execution_operation(CancellationToken::new()).unwrap();
    store.authority.close();
    closed_op
        .run(async {
            assert!(store.insert_project(&next_project).is_err());
            assert!(store.insert_session(&next_session).is_err());
            assert!(store.insert_agent(&next_agent).is_err());
        })
        .await;
    assert_eq!(counts(), [1, 1, 1]);
    assert_eq!(store.project(&project.id).unwrap(), project);
    assert_eq!(store.session(&session.id).unwrap(), session);
    assert_eq!(store.agent(&agent.id).unwrap(), agent);
}

#[test]
fn real_old_body_blocks_new_epoch_after_controller_drop_until_settled() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("domain.db")).unwrap();
    let controller = store.authority.claim().unwrap();
    store.recover_with_controller(&controller).unwrap();
    let operation = store.execution_operation(CancellationToken::new()).unwrap();
    let body = operation.hold_body();
    let read_owner = || {
        store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT epoch,phase FROM execution_owner WHERE id=1",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .unwrap()
    };
    let before = read_owner();
    assert_eq!(before.1, "active");
    drop(controller);
    assert!(store.authority.claim().is_err());
    assert_eq!(read_owner(), before);
    drop(operation);
    assert!(
        store.authority.claim().is_err(),
        "retained original body must block a new epoch"
    );
    assert_eq!(read_owner(), before);
    drop(body);
    assert_eq!(store.execution_admission_status().unwrap().1, 0);
    let next = store.authority.claim().unwrap();
    assert_eq!(read_owner(), (before.0 + 1, "recovering".to_owned()));
    store.recover_with_controller(&next).unwrap();
    assert_eq!(read_owner(), (before.0 + 1, "active".to_owned()));
}

#[test]
fn unknown_owner_survives_dropping_every_external_reference() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sticky.db");
    let store = Arc::new(Store::open(&path).unwrap());
    let engine = Engine::new(store.clone(), 1).unwrap();
    let op = store.execution_operation(CancellationToken::new()).unwrap();
    let authority = Arc::downgrade(&store.authority);
    let old_store = Arc::downgrade(&store);
    let before: (i64, String) = store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT epoch,phase FROM execution_owner WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    // Exercise the real unknown transition on the original capability; this
    // is an injected unknown, not evidence of a particular external failure.
    op.seal_unknown();
    drop(op);
    drop(engine);
    drop(store);
    assert!(old_store.upgrade().is_none());
    assert!(
        authority.upgrade().is_some(),
        "unknown must retain the original domain owner"
    );
    let reopened = Arc::new(Store::open(&path).unwrap());
    assert!(Engine::new(reopened.clone(), 1).is_err());
    let after: (i64, String) = reopened
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT epoch,phase FROM execution_owner WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(after, before);
}

#[test]
fn unknown_owner_child() {
    let Some(root) = std::env::var_os("NHC_U18_UNKNOWN_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let store = Arc::new(Store::open(&root.join("unknown.db")).unwrap());
    let engine = Engine::new(store.clone(), 1).unwrap();
    let op = store.execution_operation(CancellationToken::new()).unwrap();
    op.seal_unknown();
    drop(op);
    drop(engine);
    drop(store);
    std::fs::write(root.join("unknown-ready"), b"ready").unwrap();
    loop {
        std::thread::park_timeout(std::time::Duration::from_secs(60));
    }
}

#[tokio::test]
async fn unknown_terminal_child() {
    use crate::domain::{Route, Run, SessionKind, Task, TaskSpec};
    let Ok(case) = std::env::var("NHC_U18_TERMINAL_CASE") else {
        return;
    };
    let (dir, store, _engine) = setup();
    let task = Task {
        id: "u18-terminal".into(),
        run_id: "u18-terminal-run".into(),
        spec: TaskSpec {
            name: "worker".into(),
            role: "worker".into(),
            route_id: "route".into(),
            prompt: "prompt".into(),
            depends_on: vec![],
            write_scopes: vec![],
            tools: false,
            allow_commands: false,
            max_rounds: 1,
        },
        route: Route {
            id: "route".into(),
            name: "route".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "fixture".into(),
            max_tokens: 64,
            parallel_limit: 1,
            key_env: None,
        },
        workspace: dir.path().to_string_lossy().into_owned(),
        status: "running".into(),
        output: "durable output".into(),
        error: None,
        messages: vec![serde_json::json!({"role":"user","content":"durable message"})],
        usage: serde_json::Value::Null,
        created_at: 1,
        updated_at: 1,
    };
    let mut other_task = task.clone();
    other_task.id = "u18-other-task".into();
    let seed = store.execution_operation(CancellationToken::new()).unwrap();
    seed.run(async {
        store
            .create_run(&Run {
                id: task.run_id.clone(),
                title: "fixture".into(),
                kind: SessionKind::Team,
                created_at: 1,
                tasks: vec![task.clone(), other_task.clone()],
            })
            .unwrap();
    })
    .await;
    let other = store.execution_operation(CancellationToken::new()).unwrap();
    let local = store.execution_operation(CancellationToken::new()).unwrap();
    let (_other_dir, other_store, _other_engine) = setup();
    let foreign = other_store
        .execution_operation(CancellationToken::new())
        .unwrap();
    local.bind_task(&task.id).unwrap();
    foreign.bind_task(&task.id).unwrap();
    if case == "cross" {
        foreign.seal_unknown();
    } else {
        local.seal_unknown();
    }
    if case == "closed" {
        store.authority.close();
    }
    if case == "epoch" || case == "phase" {
        let change = if case == "epoch" {
            "UPDATE execution_owner SET epoch=epoch+1 WHERE id=1"
        } else {
            "UPDATE execution_owner SET phase='recovering' WHERE id=1"
        };
        store.db.lock().unwrap().execute(change, []).unwrap();
    }
    if case == "normal" || case == "closed" {
        // An independently admitted operation may still settle its own result.
        other
            .run(async {
                let mut db = store.db.lock().unwrap();
                let tx = db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                store.execution_settlement_fence(&tx).unwrap();
                tx.execute(
                    "INSERT INTO config(id,value) VALUES('other-settled','yes')",
                    [],
                )
                .unwrap();
                tx.commit().unwrap();
            })
            .await;
        let mut forged = task.clone();
        let mut wrong_task = other_task.clone();
        wrong_task.status = "failed".into();
        local
            .hold_body()
            .run(async {
                assert!(store.save_unknown_terminal(&wrong_task).is_err());
            })
            .await;
        assert_eq!(
            serde_json::to_value(store.task(&other_task.id).unwrap()).unwrap(),
            serde_json::to_value(&other_task).unwrap()
        );
        forged.status = "completed".into();
        forged.output = "untrusted success".into();
        let scope = local.hold_body();
        scope
            .run(async {
                assert!(store.save_unknown_terminal(&forged).is_err());
            })
            .await;
        assert_eq!(
            serde_json::to_value(store.task(&task.id).unwrap()).unwrap(),
            serde_json::to_value(&task).unwrap()
        );
        forged.status = "failed".into();
        forged
            .messages
            .push(serde_json::json!({"role":"tool","content":"untrusted result"}));
        local
            .run(async {
                store.save_unknown_terminal(&forged).unwrap();
            })
            .await;
        let saved = store.task(&task.id).unwrap();
        assert_eq!(saved.status, "failed");
        assert_eq!(saved.output, task.output);
        assert_eq!(saved.messages, task.messages);
        assert!(saved.error.is_some());
        assert!(!store.execution_admission_status().unwrap().0);
    } else {
        let mut rejected = task.clone();
        rejected.status = "failed".into();
        let op = if case == "cross" { foreign } else { local };
        op.run(async {
            assert!(store.save_unknown_terminal(&rejected).is_err());
        })
        .await;
        assert_eq!(
            serde_json::to_value(store.task(&task.id).unwrap()).unwrap(),
            serde_json::to_value(&task).unwrap()
        );
    }
}

#[test]
fn unknown_terminal_uses_original_scope_and_preserves_durable_content() {
    use std::{process::Command, time::Duration};
    for case in ["normal", "cross", "closed", "epoch", "phase"] {
        let root = tempfile::Builder::new()
            .prefix("u18-terminal-")
            .tempdir()
            .unwrap()
            .keep();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "execution_authority_regression_tests::unknown_terminal_child",
                "--nocapture",
            ])
            .env("NHC_U18_TERMINAL_CASE", case);
        let output =
            owned_child::OwnedChild::spawn(&mut command, &root).output(Duration::from_secs(30));
        assert!(
            output.status.success(),
            "{case} child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn durable_epoch_and_phase_are_checked_before_process_or_generation() {
    use crate::process_lifetime::Outcome;
    use std::time::{Duration, Instant};
    let case_override = std::env::var("NHC_U18_GENERATION_CASE").ok();
    let cases: &[&str] = match case_override.as_deref() {
        None => &["active", "cancelled"],
        Some("epoch") => &["epoch"],
        Some("phase") => &["phase"],
        Some(_) => panic!("unknown generation test case"),
    };
    for &case in cases {
        let (dir, store, engine) = setup();
        let cancel = CancellationToken::new();
        let op = store.execution_operation(cancel.clone()).unwrap();
        let retained = op.hold_body();
        match case {
            "cancelled" => cancel.cancel(),
            "epoch" => {
                store
                    .db
                    .lock()
                    .unwrap()
                    .execute("UPDATE execution_owner SET epoch=epoch+1 WHERE id=1", [])
                    .unwrap();
            }
            "phase" => {
                store
                    .db
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE execution_owner SET phase='recovering' WHERE id=1",
                        [],
                    )
                    .unwrap();
            }
            _ => {}
        }
        let root = std::env::var_os("SystemRoot").expect("Windows SystemRoot");
        let cmd = std::path::PathBuf::from(root).join("System32/cmd.exe");
        let mut command = tokio::process::Command::new(cmd);
        command
            .current_dir(dir.path())
            .args(["/D", "/C", "echo marker>generation-marker.txt"]);
        let spec = crate::execution_authority::command_spec(
            &command,
            4096,
            Instant::now() + Duration::from_secs(15),
        )
        .unwrap();
        if case == "active" {
            let lease = store.execution_generation(&retained).unwrap();
            lease.settle();
        }
        let outcome = op.run(crate::execution_authority::run_process(spec)).await;
        if case == "active" {
            match outcome.unwrap() {
                Outcome::Settled(_) => {}
                Outcome::Unknown => loop {
                    std::thread::park_timeout(Duration::from_secs(60));
                },
                Outcome::NotStarted { .. } => panic!("positive control did not start"),
            }
            assert!(dir.path().join("generation-marker.txt").exists());
            drop(retained);
            assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
        } else {
            assert!(
                outcome.is_err(),
                "{case} must reject before supervisor submission"
            );
            assert!(
                !dir.path().join("generation-marker.txt").exists(),
                "{case} launched the process"
            );
            assert!(
                store.execution_generation(&retained).is_err(),
                "{case} must reject the shared file/worker generation entry"
            );
        }
    }
}

#[test]
fn generation_rejects_changed_durable_state_in_isolated_original_children() {
    use std::{process::Command, time::Duration};
    for case in ["epoch", "phase"] {
        let root = tempfile::Builder::new()
            .prefix("u18-generation-")
            .tempdir()
            .unwrap()
            .keep();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "execution_authority_regression_tests::durable_epoch_and_phase_are_checked_before_process_or_generation", "--nocapture"])
            .env("NHC_U18_GENERATION_CASE", case);
        let output =
            owned_child::OwnedChild::spawn(&mut command, &root).output(Duration::from_secs(30));
        assert!(
            output.status.success(),
            "{case} child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn unknown_owner_retains_os_exclusion_until_original_child_is_waited() {
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    let root = tempfile::Builder::new()
        .prefix("u18-unknown-")
        .tempdir()
        .unwrap()
        .keep();
    let path = root.join("unknown.db");
    drop(Store::open(&path).unwrap());
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "execution_authority_regression_tests::unknown_owner_child",
            "--nocapture",
        ])
        .env("NHC_U18_UNKNOWN_ROOT", &root);
    let mut child = owned_child::OwnedChild::spawn(&mut command, &root.join("evidence"));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !root.join("unknown-ready").exists() {
        assert!(
            Instant::now() < deadline,
            "original child never reached the unknown barrier"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        Store::open(&path).is_err(),
        "live unknown owner must retain the original domain lock"
    );
    assert!(!child.kill_wait().success());
    drop(child);
    // The injected case owns no real external effect. Successful recovery here
    // says nothing about unresolved effects in a genuine host crash.
    let store = Arc::new(Store::open(&path).unwrap());
    let engine = Engine::new_recovered(store, 1).unwrap().0;
    assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
}

fn value(store: &Store) -> Option<String> {
    use rusqlite::OptionalExtension;
    store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT value FROM config WHERE id='u18-fence-test'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
}

#[test]
fn real_operation_fences_the_same_transaction_before_business_write() {
    let (_dir, store, _engine) = setup();
    let op = store.execution_operation(CancellationToken::new()).unwrap();
    let mut db = store.db.lock().unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    store.execution_fence_for(&tx, &op).unwrap();
    tx.execute(
        "INSERT INTO config(id,value) VALUES('u18-fence-test','positive')",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    assert_eq!(value(&store).as_deref(), Some("positive"));
}

#[test]
fn stale_real_operation_and_wrong_phase_reject_without_business_mutation() {
    for change in [
        "UPDATE execution_owner SET epoch=epoch+1 WHERE id=1",
        "UPDATE execution_owner SET phase='recovering' WHERE id=1",
    ] {
        let (_dir, store, _engine) = setup();
        let op = store.execution_operation(CancellationToken::new()).unwrap();
        {
            let mut db = store.db.lock().unwrap();
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            store.execution_fence_for(&tx, &op).unwrap();
            tx.execute(
                "INSERT INTO config(id,value) VALUES('u18-fence-test','before')",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
            db.execute(change, []).unwrap();
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(store.execution_fence_for(&tx, &op).is_err());
            tx.rollback().unwrap();
        }
        assert_eq!(value(&store).as_deref(), Some("before"));
        assert!(!store.execution_admission_status().unwrap().0);
    }
}

#[tokio::test]
async fn operation_from_another_domain_cannot_authorize_transaction() {
    let (_first_dir, first, first_engine) = setup();
    let (_second_dir, second, second_engine) = setup();
    let foreign = first.execution_operation(CancellationToken::new()).unwrap();
    let local = second
        .execution_operation(CancellationToken::new())
        .unwrap();
    let mut db = second.db.lock().unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    store_positive_and_insert(&second, &tx, &local);
    tx.commit().unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(second.execution_fence_for(&tx, &foreign).is_err());
    tx.rollback().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT value FROM config WHERE id='u18-fence-test'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "before"
    );
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    second.execution_fence_for(&tx, &local).unwrap();
    tx.execute(
        "UPDATE config SET value='after' WHERE id='u18-fence-test'",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    assert_eq!(value(&second).as_deref(), Some("after"));
    assert_eq!(second.execution_admission_status().unwrap().2, 0);
    drop(foreign);
    drop(local);
    assert!(
        first_engine
            .shutdown(std::time::Duration::from_secs(5))
            .await
            .settled()
    );
    assert!(
        second_engine
            .shutdown(std::time::Duration::from_secs(5))
            .await
            .settled()
    );
}

#[test]
fn copied_owner_row_cannot_make_foreign_transaction_belong_to_original_store() {
    let (_first_dir, first, _first_engine) = setup();
    let (_second_dir, second, _second_engine) = setup();
    let op = first.execution_operation(CancellationToken::new()).unwrap();
    let owner: (String, i64, String) = {
        let db = first.db.lock().unwrap();
        db.query_row(
            "SELECT domain_identity,epoch,phase FROM execution_owner WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    {
        let mut db = first.db.lock().unwrap();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        store_positive_and_insert(&first, &tx, &op);
        tx.commit().unwrap();
    }
    let mut db = second.db.lock().unwrap();
    db.execute(
        "UPDATE execution_owner SET domain_identity=?1,epoch=?2,phase=?3 WHERE id=1",
        rusqlite::params![owner.0, owner.1, owner.2],
    )
    .unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert!(
        first.execution_fence_for(&tx, &op).is_err(),
        "an authentic operation must not authorize another database's transaction"
    );
    tx.rollback().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM config WHERE id='u18-fence-test'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    let mut db = first.db.lock().unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    first.execution_fence_for(&tx, &op).unwrap();
    tx.execute(
        "UPDATE config SET value='after' WHERE id='u18-fence-test'",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    assert_eq!(value(&first).as_deref(), Some("after"));
}

#[test]
fn cancelled_before_start_does_not_poison_next_operation() {
    let (_dir, store, _engine) = setup();
    let cancel = CancellationToken::new();
    let cancelled = store.execution_operation(cancel.clone()).unwrap();
    cancel.cancel();
    {
        let mut db = store.db.lock().unwrap();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(store.execution_fence_for(&tx, &cancelled).is_err());
        tx.rollback().unwrap();
    }
    drop(cancelled);
    assert_eq!(value(&store), None);
    assert_eq!(store.execution_admission_status().unwrap().1, 0);
    assert_eq!(store.execution_admission_status().unwrap().2, 0);
    let next = store.execution_operation(CancellationToken::new()).unwrap();
    let mut db = store.db.lock().unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    store_positive_and_insert(&store, &tx, &next);
    tx.commit().unwrap();
    drop(db);
    assert_eq!(value(&store).as_deref(), Some("before"));
}

#[tokio::test]
async fn claimed_body_can_settle_after_close_or_cancel() {
    use std::time::Duration;
    for close in 0..3 {
        let (_dir, store, engine) = setup();
        let cancel = CancellationToken::new();
        let op = store.execution_operation(cancel.clone()).unwrap();
        let body = op.hold_body();
        {
            let mut db = store.db.lock().unwrap();
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            store_positive_and_insert(&store, &tx, &op);
            tx.commit().unwrap();
        }
        match close {
            0 => cancel.cancel(),
            1 => engine.request_shutdown(),
            _ => store.authority.close(),
        }
        {
            let mut db = store.db.lock().unwrap();
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            assert!(store.execution_fence_for(&tx, &body).is_err());
            tx.rollback().unwrap();
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            store.execution_settlement_fence_for(&tx, &body).unwrap();
            tx.execute(
                "UPDATE config SET value='settled' WHERE id='u18-fence-test'",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        drop(op);
        drop(body);
        assert_eq!(value(&store).as_deref(), Some("settled"));
        assert_eq!(store.execution_admission_status().unwrap().2, 0);
        assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
        let (_, in_flight, unknown) = store.execution_admission_status().unwrap();
        assert_eq!((in_flight, unknown), (0, 0));
    }
}

#[tokio::test]
async fn checkpoint_waiting_at_gate_cannot_claim_after_shutdown() {
    use std::{task::Poll, time::Duration};
    let (_dir, store, engine) = setup();
    let gate = engine.workspace_gate_for_test();
    let held = gate.lock().await;
    let mut create = Box::pin(engine.create_checkpoint("not-a-task", "u18-gate"));
    assert!(matches!(
        futures_util::poll!(create.as_mut()),
        Poll::Pending
    ));
    assert_eq!(
        store.execution_admission_status().unwrap().1,
        1,
        "pending request must retain its original operation"
    );
    let before: i64 = store
        .db
        .lock()
        .unwrap()
        .query_row("SELECT count(*) FROM checkpoints", [], |r| r.get(0))
        .unwrap();
    engine.request_shutdown();
    drop(held);
    assert!(create.as_mut().await.is_err());
    drop(create);
    assert_eq!(
        store
            .db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM checkpoints", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
}

#[test]
fn confirmed_commit_rollback_keeps_owner_open_for_new_operation() {
    let (_dir, store, _engine) = setup();
    {
        let db = store.db.lock().unwrap();
        db.execute_batch("CREATE TABLE u18_parent(id INTEGER PRIMARY KEY); CREATE TABLE u18_child(parent_id INTEGER REFERENCES u18_parent(id) DEFERRABLE INITIALLY DEFERRED);").unwrap();
    }
    let op = store.execution_operation(CancellationToken::new()).unwrap();
    {
        let mut db = store.db.lock().unwrap();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        store.execution_fence_for(&tx, &op).unwrap();
        tx.execute("INSERT INTO u18_child VALUES(42)", []).unwrap();
        tx.execute(
            "INSERT INTO config(id,value) VALUES('u18-rollback','must-rollback')",
            [],
        )
        .unwrap();
        assert!(store.commit_execution_tx(tx).is_err());
        assert!(
            db.is_autocommit(),
            "failed COMMIT must have a confirmed rollback"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM u18_child", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM config WHERE id='u18-rollback'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    drop(op);
    let (open, _, unknown) = store.execution_admission_status().unwrap();
    assert!(open);
    assert_eq!(unknown, 0);
    let next = store.execution_operation(CancellationToken::new()).unwrap();
    let mut db = store.db.lock().unwrap();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    store.execution_fence_for(&tx, &next).unwrap();
    tx.execute(
        "INSERT INTO config(id,value) VALUES('u18-rollback','next-operation')",
        [],
    )
    .unwrap();
    store.commit_execution_tx(tx).unwrap();
    drop(db);
    assert_eq!(
        store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM config WHERE id='u18-rollback'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "next-operation"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claimed_restore_finishes_after_shutdown_cutoff() {
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let (engine, task) =
        crate::engine::workspace_phase_tests::n13_completed_fixture(root.path()).await;
    let file = root.path().join("source/src/a.txt");
    assert_ne!(std::fs::read(&file).unwrap(), b"before-a");
    let barrier = crate::engine::runtime_barrier::arm(task.clone(), "restore_claim_committed");
    struct ReleaseBarrier(Arc<crate::engine::runtime_barrier::Barrier>);
    impl Drop for ReleaseBarrier {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    let _release_on_failure = ReleaseBarrier(barrier.clone());
    let worker = {
        let engine = engine.clone();
        let task = task.clone();
        tokio::spawn(async move { engine.restore(&task).await })
    };
    let arrived = barrier.wait_arrived(Duration::from_secs(10));
    if !arrived {
        barrier.release();
        panic!("restore did not reach the committed-claim barrier");
    }
    assert_eq!(
        engine.latest_restore(&task).unwrap().unwrap().status,
        crate::workspace_changes::RestoreStatus::Claimed
    );
    assert!(engine.store.execution_admission_status().unwrap().1 > 0);
    engine.request_shutdown();
    barrier.release();
    let receipt = tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.status,
        crate::workspace_changes::RestoreStatus::Complete
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"before-a");
    assert_eq!(engine.latest_restore(&task).unwrap().unwrap(), receipt);
    assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_content_change_before_first_action_settles_without_unknown_owner() {
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let (engine, task) =
        crate::engine::workspace_phase_tests::n13_completed_fixture(root.path()).await;
    let file = root.path().join("source/src/a.txt");
    let barrier = crate::engine::runtime_barrier::arm(task.clone(), "restore_claim_committed");
    struct ReleaseBarrier(Arc<crate::engine::runtime_barrier::Barrier>);
    impl Drop for ReleaseBarrier {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    let _release_on_failure = ReleaseBarrier(barrier.clone());
    let worker = {
        let engine = engine.clone();
        let task = task.clone();
        tokio::spawn(async move { engine.restore(&task).await })
    };
    assert!(
        barrier.wait_arrived(Duration::from_secs(10)),
        "actual restore claim was not committed"
    );
    std::fs::write(&file, b"changed after claim").unwrap();
    barrier.release();
    let result = tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"changed after claim");
    let receipt = engine.latest_restore(&task).unwrap().unwrap();
    assert_eq!(
        receipt.status,
        crate::workspace_changes::RestoreStatus::Unknown
    );
    let (_, in_flight, unknown) = engine.store.execution_admission_status().unwrap();
    assert_eq!((in_flight, unknown), (0, 0));
    assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_epoch_or_phase_child() {
    use std::time::Duration;
    let Ok(case) = std::env::var("NHC_U18_RESTORE_CASE") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let (engine, task) =
        crate::engine::workspace_phase_tests::n13_completed_fixture(root.path()).await;
    let file = root.path().join("source/src/a.txt");
    let before = std::fs::read(&file).unwrap();
    let barrier = crate::engine::runtime_barrier::arm(task.clone(), "restore_claim_committed");
    struct ReleaseBarrier(Arc<crate::engine::runtime_barrier::Barrier>);
    impl Drop for ReleaseBarrier {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    let _release_on_failure = ReleaseBarrier(barrier.clone());
    let worker = {
        let engine = engine.clone();
        let task = task.clone();
        tokio::spawn(async move { engine.restore(&task).await })
    };
    assert!(
        barrier.wait_arrived(Duration::from_secs(10)),
        "actual restore claim was not committed"
    );
    let sql = match case.as_str() {
        "epoch" => "UPDATE execution_owner SET epoch=epoch+1 WHERE id=1",
        "phase" => "UPDATE execution_owner SET phase='recovering' WHERE id=1",
        _ => panic!("invalid case"),
    };
    engine.store.db.lock().unwrap().execute(sql, []).unwrap();
    barrier.release();
    assert!(
        tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert!(!engine.store.execution_admission_status().unwrap().0);
    assert!(engine.store.execution_admission_status().unwrap().2 > 0);
    assert_eq!(
        engine.latest_restore(&task).unwrap().unwrap().status,
        crate::workspace_changes::RestoreStatus::Claimed
    );
}

#[tokio::test]
async fn network_cross_operation_receipt_child() {
    use crate::network_authorization::{
        CallState, NetworkPurpose, NetworkSubject, NetworkSubjectKind,
    };
    use crate::provider_transport::TransportError;
    let Ok(_) = std::env::var("NHC_U18_RECEIPT_CHILD") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let (engine, _) =
        crate::engine::workspace_phase_tests::n13_completed_fixture(root.path()).await;
    let subject = NetworkSubject {
        kind: NetworkSubjectKind::Route,
        id: "route".into(),
        project_id: engine.current_project().unwrap().project_id,
    };
    let first = engine
        .store
        .execution_operation(CancellationToken::new())
        .unwrap();
    let second = engine
        .store
        .execution_operation(CancellationToken::new())
        .unwrap();
    let (first_permit, first_receipt) = first
        .hold_body()
        .run(async {
            engine
                .store
                .network_admit(&subject, NetworkPurpose::Completion, None)
                .unwrap()
        })
        .await;
    let (second_permit, second_receipt) = second
        .hold_body()
        .run(async {
            engine
                .store
                .network_admit(&subject, NetworkPurpose::Completion, None)
                .unwrap()
        })
        .await;
    let before = engine.store.network_call(&second_receipt.call_id).unwrap();
    assert_eq!(before.state, CallState::Claimed);
    first.seal_unknown();
    let result = first
        .hold_body()
        .run(async {
            engine
                .store
                .finish_network::<()>(second_receipt.clone(), Err(TransportError::Unknown))
        })
        .await;
    assert!(result.is_err());
    assert_eq!(
        engine.store.network_call(&second_receipt.call_id).unwrap(),
        before
    );
    let result = first
        .hold_body()
        .run(async {
            engine
                .store
                .finish_network::<()>(first_receipt.clone(), Err(TransportError::Unknown))
        })
        .await;
    assert!(result.is_err());
    assert_eq!(
        engine
            .store
            .network_call(&first_receipt.call_id)
            .unwrap()
            .state,
        CallState::Unknown
    );
    let result = second
        .hold_body()
        .run(async {
            engine
                .store
                .finish_network::<()>(second_receipt.clone(), Err(TransportError::Invalid))
        })
        .await;
    assert!(result.is_err());
    assert_eq!(
        engine
            .store
            .network_call(&second_receipt.call_id)
            .unwrap()
            .state,
        CallState::NotSent
    );
    drop((first_permit, second_permit));
}

#[test]
fn network_receipt_is_bound_to_its_original_operation() {
    use std::{process::Command, time::Duration};
    let root = tempfile::Builder::new()
        .prefix("u18-receipt-")
        .tempdir()
        .unwrap()
        .keep();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "execution_authority_regression_tests::network_cross_operation_receipt_child",
            "--nocapture",
        ])
        .env("NHC_U18_RECEIPT_CHILD", "1");
    let output =
        owned_child::OwnedChild::spawn(&mut command, &root).output(Duration::from_secs(40));
    assert!(
        output.status.success(),
        "receipt child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn restore_rechecks_durable_authority_before_file_action() {
    use std::{process::Command, time::Duration};
    for case in ["epoch", "phase"] {
        let root = tempfile::Builder::new()
            .prefix("u18-restore-")
            .tempdir()
            .unwrap()
            .keep();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "execution_authority_regression_tests::restore_epoch_or_phase_child",
                "--nocapture",
            ])
            .env("NHC_U18_RESTORE_CASE", case);
        let output =
            owned_child::OwnedChild::spawn(&mut command, &root).output(Duration::from_secs(40));
        assert!(
            output.status.success(),
            "{case} child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn store_positive_and_insert(
    store: &Store,
    tx: &rusqlite::Transaction<'_>,
    op: &crate::execution_authority::ExecutionOperation,
) {
    store.execution_fence_for(tx, op).unwrap();
    tx.execute(
        "INSERT INTO config(id,value) VALUES('u18-fence-test','before')",
        [],
    )
    .unwrap();
}

#[test]
fn u18_crash_child() {
    let Some(root) = std::env::var_os("NHC_U18_PHASE_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let phase = match std::env::var("NHC_U18_PHASE").unwrap().as_str() {
        "epoch_after_commit" => "epoch_after_commit",
        "recovery_before_commit" => "recovery_before_commit",
        "recovery_after_commit" => "recovery_after_commit",
        _ => panic!("unknown test phase"),
    };
    crate::engine::crash_probe::arm("u18-owner".into(), phase, root.join("phase.signal"));
    let store = Arc::new(Store::open(&root.join("phase.db")).unwrap());
    let _engine = Engine::new_recovered(store, 1).unwrap().0;
    panic!("armed phase was not reached");
}

#[tokio::test]
async fn killed_at_real_epoch_and_recovery_commit_barriers_recovers() {
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    for (phase, expected_phase) in [
        ("epoch_after_commit", "recovering"),
        ("recovery_before_commit", "recovering"),
        ("recovery_after_commit", "active"),
    ] {
        let root = tempfile::Builder::new()
            .prefix("u18-phase-")
            .tempdir()
            .unwrap()
            .keep();
        let path = root.join("phase.db");
        let store = Store::open(&path).unwrap();
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO config(id,value) VALUES('u18-phase-sentinel','unchanged')",
                [],
            )
            .unwrap();
        drop(store);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "execution_authority_regression_tests::u18_crash_child",
                "--nocapture",
            ])
            .env("NHC_U18_PHASE_ROOT", &root)
            .env("NHC_U18_PHASE", phase);
        let mut child = owned_child::OwnedChild::spawn(&mut command, &root.join("evidence"));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !root.join("phase.signal").exists() {
            assert!(
                Instant::now() < deadline,
                "actual {phase} barrier not reached"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!child.kill_wait().success());
        drop(child);
        let db = rusqlite::Connection::open(&path).unwrap();
        let state: (i64, String) = db
            .query_row(
                "SELECT epoch,phase FROM execution_owner WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (1, expected_phase.to_owned()), "{phase}");
        assert_eq!(
            db.query_row(
                "SELECT value FROM config WHERE id='u18-phase-sentinel'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "unchanged"
        );
        drop(db);
        let store = Arc::new(Store::open(&path).unwrap());
        let engine = Engine::new_recovered(store.clone(), 1).unwrap().0;
        let state: (i64, String) = store
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT epoch,phase FROM execution_owner WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (2, "active".to_owned()));
        assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
    }
}

#[tokio::test]
async fn restart_and_http_security() {
    use crate::{
        domain::*,
        server::{self, App},
    };
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use std::time::Duration;
    use tower::ServiceExt;
    let temp = tempfile::tempdir().unwrap();
    let e = runtime_security_fixture(temp.path());
    let route = e.store.settings().unwrap().unwrap().routes[0].clone();
    let durable_task = Task {
        id: "durable-id".into(),
        run_id: "durable-run".into(),
        spec: runtime_security_task("worker"),
        route,
        workspace: temp.path().to_string_lossy().into(),
        status: "running".into(),
        output: "partial".into(),
        error: None,
        messages: vec![],
        usage: Value::Null,
        created_at: now(),
        updated_at: now(),
    };
    e.store
        .execution_operation(CancellationToken::new())
        .unwrap()
        .run(async {
            e.store
                .create_run(&Run {
                    id: "durable-run".into(),
                    title: "recovery".into(),
                    kind: SessionKind::Team,
                    created_at: now(),
                    tasks: vec![durable_task],
                })
                .unwrap();
            e.store
                .event(
                    "durable-id",
                    "delta",
                    json!({"text":"partial recovered from event log"}),
                )
                .unwrap();
        })
        .await;
    let store = Arc::new(Store::open(&temp.path().join("test.db")).unwrap());
    let before = serde_json::to_value(store.events("durable-run", 0).unwrap()).unwrap();
    assert!(
        store.recover().is_err(),
        "live controller admitted recovery"
    );
    assert_eq!(store.task("durable-id").unwrap().status, "running");
    assert_eq!(
        serde_json::to_value(store.events("durable-run", 0).unwrap()).unwrap(),
        before
    );
    assert!(e.shutdown(Duration::from_secs(5)).await.settled());
    let old_store = Arc::downgrade(&e.store);
    drop(e);
    assert!(
        old_store.upgrade().is_none(),
        "old controller still holds its Store"
    );
    let (e, recovered) = Engine::new_recovered(store.clone(), 2).unwrap();
    assert_eq!(recovered, 1);
    assert_eq!(
        store.events("durable-run", 0).unwrap().last().unwrap().data["status"],
        "interrupted"
    );
    assert_eq!(store.task("durable-id").unwrap().status, "interrupted");
    assert_eq!(
        store.task("durable-id").unwrap().output,
        "partial recovered from event log"
    );
    let app = server::router(App {
        engine: e,
        token: "csrf-test".into(),
        origin: "http://127.0.0.1:3090".into(),
    });
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .header("host", "evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .header("host", "127.0.0.1:3090")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("host", "127.0.0.1:3090")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let payload = serde_json::to_vec(&RunRequest {
        kind: SessionKind::Team,
        title: "idempotent".into(),
        tasks: vec![runtime_security_task("idempotent-worker")],
    })
    .unwrap();
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("host", "127.0.0.1:3090")
                .header("x-peachsh-token", "csrf-test")
                .header("idempotency-key", "runtime-idempotency")
                .header("content-type", "application/json")
                .body(Body::from(payload.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let first: Run =
        serde_json::from_slice(&to_bytes(r.into_body(), 1_000_000).await.unwrap()).unwrap();
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("host", "127.0.0.1:3090")
                .header("x-peachsh-token", "csrf-test")
                .header("idempotency-key", "runtime-idempotency")
                .header("content-type", "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let second: Run =
        serde_json::from_slice(&to_bytes(r.into_body(), 1_000_000).await.unwrap()).unwrap();
    assert_eq!(first.id, second.id);
    let r = app
        .oneshot(
            Request::builder()
                .uri("/api/settings")
                .header("host", "127.0.0.1:3090")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let b = to_bytes(r.into_body(), 1_000_000).await.unwrap();
    let s = String::from_utf8(b.to_vec()).unwrap();
    assert!(!s.contains("fake-key-a"));
    assert!(!s.contains("fake-key-b"));
}
