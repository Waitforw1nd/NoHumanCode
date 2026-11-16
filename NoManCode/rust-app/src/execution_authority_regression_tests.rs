//! Regressions use the real operation object and the transaction being written.
#![cfg(windows)]

use crate::{engine::Engine, store::Store};
use rusqlite::TransactionBehavior;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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
    for close in [false, true] {
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
        if close {
            store.authority.close();
        } else {
            cancel.cancel();
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
        let (_, in_flight, unknown) = store.execution_admission_status().unwrap();
        assert_eq!((in_flight, unknown), (0, 0));
        assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
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
