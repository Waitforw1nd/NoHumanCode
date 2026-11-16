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
