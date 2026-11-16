fn authorize_fixture(store: &crate::store::Store) {
    super::configure_fixture_project(store);
    for route in store.settings().unwrap().unwrap().routes {
        let subject = super::route_subject(store, &route.id);
        let p = store.network_preview(subject.clone()).unwrap();
        store
            .network_confirm(
                subject,
                crate::network_authorization::ConfirmRequest {
                    config_revision: p.config_revision,
                    credential_revision: p.credential_revision,
                    binding_digest: p.binding_digest,
                    purposes: vec![crate::network_authorization::NetworkPurpose::Completion],
                },
                &crate::domain::id(),
            )
            .unwrap();
    }
}
use super::*;
use axum::{Json, Router, extract::State, routing::post};
use std::{
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

struct ChildGuard(Option<Child>);

#[tokio::test]
async fn n11_three_reads_recheck_durable_desired_and_project_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = two_write_engine(temp.path()).await;
    finish_two_writes(&engine, &task_id, usize::MAX).await;
    let task = engine.store.task(&task_id).unwrap();
    // Force the persistent check to be the rejecting boundary while the registry is active.
    engine
        .store
        .set_plugin_desired(
            "builtin.files",
            &crate::plugin_management::SetPluginEnabled {
                enabled: false,
                expected_revision: 0,
            },
            "durable-read-deny",
        )
        .unwrap();
    assert!(
        engine
            .plugin_status("builtin.files")
            .unwrap()
            .runtime
            .admission_open
    );
    for name in ["read_file", "list_files", "search_files"] {
        let args = if name == "read_file" {
            json!({"path":"src/a.txt"})
        } else {
            json!({"path":"src","query":"after"})
        };
        let call = json!({"id":format!("denied-{name}"),"function":{"name":name,"arguments":args.to_string()}});
        let before = n10_rows(&engine.store.db.lock().unwrap());
        let error = engine
            .execute_or_wait(
                &mut task.clone(),
                &call,
                name,
                &args,
                "phase-test-key",
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<crate::plugin_management::PluginManagementError>(),
            Some(&crate::plugin_management::PluginManagementError::Unavailable)
        );
        assert_eq!(n10_rows(&engine.store.db.lock().unwrap()), before);
        assert_eq!(
            engine
                .plugin_status("builtin.files")
                .unwrap()
                .runtime
                .in_flight,
            0
        );
    }
    n10_set(&engine, true, 1, "restore-desired");
    let project = engine.store.current_project().unwrap().project_id.unwrap();
    let snapshot = engine.store.project_config(&project).unwrap();
    let mut config = snapshot.config;
    config.tools = false;
    config.write_scopes.clear();
    config.allow_commands = false;
    engine
        .store
        .configure_project(ConfigureProject {
            project_id: project,
            expected_revision: snapshot.revision,
            config,
        })
        .unwrap();
    for name in ["read_file", "list_files", "search_files"] {
        let result = engine
            .tool_runtime
            .acquire(&task, name, || engine.store.validate_files_read(&task));
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("revoked project obtained a lease"),
        };
        assert_eq!(
            error.downcast_ref::<ProjectError>(),
            Some(&ProjectError::PermissionRevoked)
        );
    }
    assert_eq!(
        engine
            .plugin_status("builtin.files")
            .unwrap()
            .runtime
            .in_flight,
        0
    );
}
#[tokio::test]
async fn n11_real_discovery_leases_finish_after_persistent_disable_and_record_results() {
    for name in ["list_files", "search_files"] {
        let temp = tempfile::tempdir().unwrap();
        let (engine, task_id) = two_write_engine(temp.path()).await;
        finish_two_writes(&engine, &task_id, usize::MAX).await;
        let mut task = engine.store.task(&task_id).unwrap();
        let args = json!({"path":"src","query":"after-1"});
        let call_id = format!("leased-{name}");
        task.messages.push(json!({"role":"assistant","tool_calls":[{"id":call_id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]}));
        let (_, operation) = engine.reserve_task_worker(&task.id).unwrap();
        operation
            .test_sync(|| engine.store.save_task(&task))
            .unwrap();
        let before_approvals = engine.store.approvals_for_task(&task_id).unwrap().len();
        let (lease, ()) = engine
            .tool_runtime
            .acquire(&task, name, || engine.store.validate_files_read(&task))
            .unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let worker_engine = engine.clone();
        let worker = tokio::spawn(operation.run(async move {
            ready_tx.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), release_rx)
                .await
                .unwrap()
                .unwrap();
            worker_engine
                .tool_calls
                .perform_tool(
                    &mut task,
                    &call_id,
                    name,
                    &args,
                    "phase-test-key",
                    None,
                    lease,
                )
                .await
                .unwrap();
        }));
        tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .unwrap()
            .unwrap();
        let result = n10_set(&engine, false, 0, "disable-discovery-lease");
        assert!(result.current.converged && result.current.runtime.draining);
        assert_eq!(result.current.runtime.in_flight, 1);
        assert_eq!(result.current.runtime.effects_count, 0);
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap();
        let final_task = engine.store.task(&task_id).unwrap();
        let content: Value = serde_json::from_str(
            final_task.messages.last().unwrap()["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        if name == "list_files" {
            assert!(
                content["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f["name"] == "a.txt")
            );
        } else {
            assert_eq!(content["matches"][0]["path"], "src/a.txt");
            assert_eq!(content["matches"][0]["line"], 1);
        }
        assert_eq!(
            engine.store.approvals_for_task(&task_id).unwrap().len(),
            before_approvals
        );
        assert_eq!(
            engine
                .plugin_status("builtin.files")
                .unwrap()
                .runtime
                .in_flight,
            0
        );
        assert!(
            !engine
                .plugin_status("builtin.files")
                .unwrap()
                .runtime
                .draining
        );
        let events = engine.store.events(&final_task.run_id, 0).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.kind == "tool_result" && e.data["name"] == name)
                .count(),
            1
        );
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            b"after-1"
        );
    }
}
#[tokio::test]
async fn n12_true_schema12_migrates_preserving_all_old_rows_and_schema13_reopens() {
    for enabled in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (old, task_id) = two_write_engine_kind(temp.path(), SessionKind::Chat).await;
        finish_two_writes(&old, &task_id, usize::MAX).await;
        old.restore(&task_id).await.unwrap();
        old.unload_builtin_files().unwrap();
        let path = temp.path().join("phase.db");
        drop(old);
        let db = rusqlite::Connection::open(&path).unwrap();
        // Rebuild the actual historical DDL; merely lowering user_version would hide migration bugs.
        db.execute_batch("PRAGMA foreign_keys=ON; DROP TABLE IF EXISTS execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; DROP TABLE IF EXISTS host_config_requests; DROP TABLE IF EXISTS host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE plugin_management_requests; DROP TABLE plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1','builtin-runtime-management-v1'); PRAGMA user_version=11;").unwrap();
        {
            let tx = db.unchecked_transaction().unwrap();
            crate::repository::apply_schema_12(&tx).unwrap();
            tx.commit().unwrap();
        }
        // The immutable NEXT-10 receipt has no version/tool-list fields.
        db.execute(
            "UPDATE plugin_desired SET rowid=37,enabled=?1,revision=1,updated_at=123 WHERE plugin_id='builtin.files'",
            [enabled],
        )
        .unwrap();
        db.execute("INSERT INTO plugin_management_requests(rowid,key,plugin_id,enabled,expected_revision,result_revision,committed_at) VALUES (71,'old-1.0-intent','builtin.files',?1,0,1,123)",[enabled]).unwrap();
        fn all_rows(
            db: &rusqlite::Connection,
        ) -> std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
            let mut rows = n10_business_rows(db);
            for name in [
                "plugin_desired",
                "plugin_management_requests",
                "schema_migrations",
            ] {
                let values = db
                    .prepare(&format!("SELECT rowid,* FROM {name} ORDER BY rowid"))
                    .unwrap()
                    .query_map([], |r| {
                        (0..r.as_ref().column_count()).map(|i| r.get(i)).collect()
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                rows.insert(name.into(), values);
            }
            rows
        }
        let before = all_rows(&db);
        let engine = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
        assert_eq!(engine.store.schema_version().unwrap(), 18);
        assert!(!engine.is_busy());
        let current = engine.plugin_status("builtin.files").unwrap();
        assert_eq!(current.version, "1.2.0");
        assert_eq!(current.desired.updated_at, 123);
        assert_eq!(current.desired.revision, 1);
        assert_eq!(current.runtime.effects_count, if enabled { 4 } else { 0 });
        let mut expected = before.clone();
        assert!(
            expected
                .insert(
                    "host_config_state".into(),
                    vec![vec![
                        rusqlite::types::Value::Integer(1),
                        rusqlite::types::Value::Integer(0)
                    ]]
                )
                .is_none()
        );
        assert!(
            expected
                .insert("host_config_requests".into(), vec![])
                .is_none()
        );
        for name in [
            "project_budgets",
            "budget_requests",
            "budget_calls",
            "worktree_operations",
            "worktree_project_reservations",
            "project_worktree_origins",
        ] {
            assert!(expected.insert(name.into(), vec![]).is_none());
        }
        let after = all_rows(&db);
        let worktree_markers: Vec<_> = after["schema_migrations"]
            .iter()
            .filter(|row| {
                row[1]
                    == rusqlite::types::Value::Text(crate::repository::WORKTREE_MIGRATION_ID.into())
                    || row[1]
                        == rusqlite::types::Value::Text(
                            crate::repository::BUDGET_MIGRATION_ID.into(),
                        )
                    || row[1]
                        == rusqlite::types::Value::Text(
                            crate::repository::HOST_CONFIG_MIGRATION_ID.into(),
                        )
                    || row[1]
                        == rusqlite::types::Value::Text(
                            crate::repository::RUNTIME_MANAGEMENT_MIGRATION_ID.into(),
                        )
                    || row[1] == rusqlite::types::Value::Text("execution-owner-v18".into())
            })
            .cloned()
            .collect();
        assert_eq!(worktree_markers.len(), 5);
        assert_eq!(n10_epoch(&db), 1);
        let shell_rows: Vec<_> = after["plugin_desired"]
            .iter()
            .filter(|row| row[1] == rusqlite::types::Value::Text("builtin.shell".into()))
            .cloned()
            .collect();
        assert_eq!(shell_rows.len(), 1);
        assert_eq!(shell_rows[0][2], rusqlite::types::Value::Integer(1));
        assert_eq!(shell_rows[0][3], rusqlite::types::Value::Integer(0));
        let marker_rows: Vec<_> = after["schema_migrations"]
            .iter()
            .filter(|row| {
                row[1]
                    == rusqlite::types::Value::Text(
                        crate::repository::SHELL_MANAGEMENT_MIGRATION_ID.into(),
                    )
            })
            .cloned()
            .collect();
        assert_eq!(marker_rows.len(), 1);
        let runtime_rows: Vec<_> = after["plugin_desired"]
            .iter()
            .filter(|row| {
                row[1] == rusqlite::types::Value::Text("builtin.chat-agent".into())
                    || row[1] == rusqlite::types::Value::Text("builtin.provider-xpeach".into())
            })
            .cloned()
            .collect();
        assert_eq!(runtime_rows.len(), 2);
        for row in &runtime_rows {
            assert_eq!(
                &row[2..],
                &[
                    rusqlite::types::Value::Integer(1),
                    rusqlite::types::Value::Integer(0),
                    rusqlite::types::Value::Integer(0)
                ]
            );
        }
        expected
            .get_mut("plugin_desired")
            .unwrap()
            .extend(shell_rows);
        expected
            .get_mut("plugin_desired")
            .unwrap()
            .extend(runtime_rows);
        expected
            .get_mut("schema_migrations")
            .unwrap()
            .extend(marker_rows);
        expected
            .get_mut("schema_migrations")
            .unwrap()
            .extend(worktree_markers);
        assert_eq!(after, expected);
        drop(Store::open(&path).unwrap());
        assert_eq!(all_rows(&db), expected);
        // If enabled, stop only in memory so a replay attempting reconciliation is observable.
        if enabled {
            engine.disable_builtin_files().unwrap();
        }
        let stopped = engine.plugin_status("builtin.files").unwrap();
        let replay = n10_set(&engine, enabled, 0, "old-1.0-intent");
        assert!(replay.replayed);
        assert_eq!(
            replay.receipt,
            crate::plugin_management::PluginMutationReceipt {
                plugin_id: "builtin.files".into(),
                enabled,
                previous_revision: 0,
                revision: 1,
                committed_at: 123
            }
        );
        assert_eq!(replay.current, stopped);
        assert_eq!(all_rows(&db), expected);
        let error = engine
            .resume(&task_id, "must stay sealed")
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<crate::store::ResumeError>(),
            Some(&crate::store::ResumeError::Sealed)
        );
        assert_eq!(all_rows(&db), expected);
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            b"before-a"
        );
    }
}

async fn n10_pending(engine: &Arc<Engine>, task_id: &str) -> approval::ApprovalRecord {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(record) = engine
                .store
                .pending_approvals_for_task(task_id)
                .unwrap()
                .into_iter()
                .next()
            {
                break record;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
async fn n10_idle(engine: &Arc<Engine>) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn n10_set(
    engine: &Engine,
    enabled: bool,
    revision: u64,
    key: &str,
) -> crate::plugin_management::PluginMutationResult {
    engine
        .set_plugin_enabled(
            "builtin.files",
            crate::plugin_management::SetPluginEnabled {
                enabled,
                expected_revision: revision,
            },
            key,
        )
        .unwrap()
}
fn n10_rows(
    db: &rusqlite::Connection,
) -> std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
    let tables: Vec<String> = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT IN ('plugin_desired','plugin_management_requests','schema_migrations') ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    tables
        .into_iter()
        .map(|name| {
            let rows = db
                .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))
                .unwrap()
                .query_map([], |r| {
                    (0..r.as_ref().column_count()).map(|i| r.get(i)).collect()
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (name, rows)
        })
        .collect()
}
fn n10_business_rows(
    db: &rusqlite::Connection,
) -> std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
    let mut rows = n10_rows(db);
    rows.remove("execution_owner");
    rows
}
fn n10_epoch(db: &rusqlite::Connection) -> i64 {
    db.query_row("SELECT epoch FROM execution_owner WHERE id=1", [], |r| {
        r.get(0)
    })
    .unwrap()
}
#[tokio::test]
async fn n10_disable_first_preserves_decision_and_refuses_claim_without_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = two_write_engine(temp.path()).await;
    let pending = n10_pending(&engine, &task_id).await;
    let before = { n10_rows(&engine.store.db.lock().unwrap()) };
    let disabled = n10_set(&engine, false, 0, "disable-before-claim");
    assert!(disabled.current.converged);
    assert_eq!(disabled.current.runtime.in_flight, 0);
    assert_eq!(n10_rows(&engine.store.db.lock().unwrap()), before); // management did not mutate any business row
    let mut task = engine.store.task(&task_id).unwrap();
    let call = task
        .messages
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .find(|c| c["id"] == pending.tool_call_id)
        .unwrap()
        .clone();
    let args: Value =
        serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap();
    engine
        .decide_approval(&pending.id, true, Some("test"))
        .unwrap();
    // Current-thread test has not yielded to the worker: test the real Store transaction as well.
    let rejected = engine
        .store
        .claim_approval(&task, &pending.tool_call_id, "write_file", &args)
        .unwrap_err();
    assert_eq!(
        rejected.downcast_ref::<crate::plugin_management::PluginManagementError>(),
        Some(&crate::plugin_management::PluginManagementError::Unavailable)
    );
    n10_idle(&engine).await;
    let record = engine.store.approval(&pending.id).unwrap();
    assert_eq!(record.status, ApprovalStatus::Approved);
    assert_eq!(record.execution_state, approval::ExecutionState::NotStarted);
    assert!(
        engine
            .store
            .workspace_changes_for_task(&task_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before-a"
    );
    let definitions = engine.tool_runtime.definitions(&task).unwrap();
    assert!(definitions.iter().all(|d| !matches!(
        d["function"]["name"].as_str(),
        Some("read_file" | "write_file" | "list_files" | "search_files")
    )));
    let admitted = std::sync::atomic::AtomicBool::new(false);
    assert!(
        engine
            .tool_runtime
            .acquire(&task, "read_file", || {
                admitted.store(true, Ordering::SeqCst);
                Ok(())
            })
            .is_err()
    );
    assert!(!admitted.load(Ordering::SeqCst));
    // All file discovery services now share the same disabled admission.
    for name in ["list_files", "search_files"] {
        assert!(
            engine
                .tool_runtime
                .acquire(&task, name, || { engine.store.validate_files_read(&task) })
                .is_err()
        );
    }
    task.status = "running".into();
    assert!(
        engine
            .execute_or_wait(
                &mut task,
                &call,
                "write_file",
                &args,
                "phase-test-key",
                &CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(
        engine.store.approval(&pending.id).unwrap().execution_state,
        approval::ExecutionState::NotStarted
    );
}
#[tokio::test]
async fn n10_claim_first_real_write_finishes_with_durable_receipt_and_drains() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = two_write_engine(temp.path()).await;
    let pending = n10_pending(&engine, &task_id).await;
    let gate = engine.gate.lock().await; // real production write stop point
    engine
        .decide_approval(&pending.id, true, Some("test"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.store.approval(&pending.id).unwrap().execution_state
            != approval::ExecutionState::Claimed
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        engine
            .plugin_status("builtin.files")
            .unwrap()
            .runtime
            .in_flight,
        1
    );
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before-a"
    );
    let accepted = n10_set(&engine, false, 0, "claimed-disable");
    assert!(accepted.current.converged && accepted.current.runtime.draining);
    assert_eq!(accepted.current.runtime.in_flight, 1);
    assert!(!accepted.current.runtime.admission_open);
    assert_eq!(accepted.current.runtime.effects_count, 0);
    let task = engine.store.task(&task_id).unwrap();
    assert!(
        engine
            .tool_runtime
            .acquire(&task, "read_file", || Ok(()))
            .is_err()
    );
    drop(gate); // bounded second direction releases the actual write
    n10_idle(&engine).await;
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"after-0"
    );
    assert_eq!(
        std::fs::read(temp.path().join("src/b.txt")).unwrap(),
        b"before-b"
    );
    assert_eq!(
        engine.store.approval(&pending.id).unwrap().execution_state,
        approval::ExecutionState::Finished
    );
    let changes = engine.store.workspace_changes_for_task(&task_id).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].state, ChangeState::Finished);
    let current = engine.plugin_status("builtin.files").unwrap();
    assert!(current.converged && !current.runtime.draining);
    assert_eq!(current.runtime.in_flight, 0);
    let replay = n10_set(&engine, false, 0, "claimed-disable");
    assert!(replay.replayed);
    assert_eq!(replay.receipt, accepted.receipt);
    let raw = rusqlite::Connection::open(temp.path().join("phase.db")).unwrap();
    let row: (bool,u64,u64,i64) = raw.query_row("SELECT enabled,expected_revision,result_revision,committed_at FROM plugin_management_requests WHERE key='claimed-disable'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(row, (false, 0, 1, accepted.receipt.committed_at));
    let events = engine.store.events(&task.run_id, 0).unwrap();
    assert_eq!(events.iter().filter(|e| e.kind == "tool_start").count(), 1);
    assert_eq!(events.iter().filter(|e| e.kind == "tool_result").count(), 1);
}
#[tokio::test]
async fn n10_schema11_migration_preserves_nonempty_all_business_rows_and_ciphertext() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = two_write_engine_kind(temp.path(), SessionKind::Chat).await;
    finish_two_writes(&engine, &task_id, usize::MAX).await;
    let cp = engine
        .create_checkpoint(&task_id, "migration-checkpoint")
        .await
        .unwrap()
        .checkpoint;
    engine.restore_checkpoint(&cp.checkpoint_id).await.unwrap();
    engine.unload_builtin_files().unwrap();
    drop(engine);
    let path = temp.path().join("phase.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS execution_owner; DELETE FROM schema_migrations WHERE id='execution-owner-v18'; DROP TABLE IF EXISTS host_config_requests; DROP TABLE IF EXISTS host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS project_worktree_origins; DROP TABLE IF EXISTS worktree_project_reservations; DROP TABLE IF EXISTS worktree_operations; DELETE FROM schema_migrations WHERE id='git-worktree-operations-v1'; DROP TABLE plugin_management_requests; DROP TABLE plugin_desired; DELETE FROM schema_migrations WHERE id IN ('builtin-plugin-management-v1','builtin-shell-management-v1','builtin-runtime-management-v1'); PRAGMA user_version=11;").unwrap();
    {
        let tx = db.unchecked_transaction().unwrap();
        crate::repository::verify_current_schema(&tx).unwrap();
        crate::repository::verify_schema_7(&tx).unwrap();
        crate::repository::verify_schema_8(&tx).unwrap();
        crate::repository::verify_schema_9(&tx).unwrap();
        crate::repository::verify_schema_10(&tx).unwrap();
        crate::repository::verify_schema_11(&tx).unwrap();
        assert!(
            !crate::repository::migration_applied(
                &tx,
                crate::repository::PLUGIN_MANAGEMENT_MIGRATION_ID
            )
            .unwrap()
        );
        assert_eq!(tx.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('plugin_desired','plugin_management_requests')", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
        tx.commit().unwrap();
    }
    let before = n10_business_rows(&db);
    for table in [
        "tasks",
        "approvals",
        "workspace_changes",
        "workspace_restores",
        "workspace_restore_outcomes",
        "checkpoints",
        "network_calls",
        "network_subjects",
        "task_network_bindings",
        "project_configs",
        "task_project_bindings",
        "secrets",
    ] {
        assert!(!before[table].is_empty(), "empty fixture: {table}");
    }
    let ciphertext: Vec<u8> = db
        .query_row(
            "SELECT before_blob FROM workspace_changes LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!ciphertext.is_empty());
    db.execute_batch("CREATE TRIGGER fail_plugin_marker BEFORE INSERT ON schema_migrations WHEN NEW.id='builtin-plugin-management-v1' BEGIN SELECT RAISE(ABORT,'migration fault'); END;").unwrap();
    assert!(Store::open(&path).is_err());
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        11
    );
    assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('plugin_desired','plugin_management_requests')", [], |r| r.get::<_,u64>(0)).unwrap(), 0);
    assert_eq!(n10_business_rows(&db), before);
    db.execute_batch("DROP TRIGGER fail_plugin_marker").unwrap();
    let migrated = Arc::new(Store::open(&path).unwrap());
    assert_eq!(migrated.schema_version().unwrap(), 18);
    assert_eq!(migrated.plugin_desired().unwrap().revision, 0);
    assert!(migrated.plugin_desired().unwrap().enabled);
    let mut before = before;
    assert!(
        before
            .insert(
                "host_config_state".into(),
                vec![vec![
                    rusqlite::types::Value::Integer(1),
                    rusqlite::types::Value::Integer(0)
                ]]
            )
            .is_none()
    );
    assert!(
        before
            .insert("host_config_requests".into(), vec![])
            .is_none()
    );
    for name in [
        "project_budgets",
        "budget_requests",
        "budget_calls",
        "worktree_operations",
        "worktree_project_reservations",
        "project_worktree_origins",
    ] {
        assert!(before.insert(name.into(), vec![]).is_none());
    }
    assert_eq!(n10_business_rows(&db), before);
    assert_eq!(n10_epoch(&db), 0);
    let reopened = Engine::new(migrated, 2).unwrap();
    assert_eq!(n10_epoch(&db), 1);
    n10_set(&reopened, false, 0, "off");
    n10_set(&reopened, true, 1, "on");
    assert_eq!(n10_business_rows(&db), before);
    assert!(!reopened.is_busy());
    let resume = reopened
        .resume(&task_id, "must remain sealed")
        .await
        .unwrap_err();
    assert_eq!(
        resume.downcast_ref::<crate::checkpoint::CheckpointError>(),
        Some(&crate::checkpoint::CheckpointError::Conflict)
    );
    assert_eq!(n10_business_rows(&db), before);
}

#[test]
fn n10_commit_before_apply_child_kill_wait_restart_keeps_unknown_and_grants() {
    const TEST: &str = "engine::workspace_phase_tests::n10_commit_before_apply_child_kill_wait_restart_keeps_unknown_and_grants";
    if let Ok(root) = std::env::var("NHC_PLUGIN_COMMIT_CHILD") {
        let root = std::path::PathBuf::from(root);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (engine, task_id) = two_write_engine(&root).await;
            let pending = n10_pending(&engine, &task_id).await;
            let _gate = engine.gate.lock().await;
            engine.decide_approval(&pending.id, true, Some("test")).unwrap();
            tokio::time::timeout(Duration::from_secs(8), async {
                while engine.store.approval(&pending.id).unwrap().execution_state != approval::ExecutionState::Claimed { tokio::task::yield_now().await; }
            }).await.unwrap();
            let original = engine.plugin_status("builtin.files").unwrap();
            assert!(original.runtime.admission_open);
            assert_eq!(original.runtime.in_flight, 1);
            let signal = root.join("committed.json");
            let payload = serde_json::to_vec(&json!({"task_id":task_id,"approval_id":pending.id,"runtime_before":original.runtime})).unwrap();
            *engine.plugin_commit_probe.lock().unwrap() = Some(Arc::new(move || {
                let staging = signal.with_extension("tmp");
                std::fs::write(&staging, &payload).unwrap();
                std::fs::rename(staging, &signal).unwrap();
                // A finite process stop point, not a simulated Store reopen.
                let deadline = Instant::now() + Duration::from_secs(20);
                while Instant::now() < deadline { std::thread::park_timeout(Duration::from_millis(20)); }
                panic!("parent did not kill management child before timeout");
            }));
            n10_set(&engine, false, 0, "post-commit-kill");
            panic!("commit stop point returned");
        });
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let output = std::fs::File::create(temp.path().join("child.log")).unwrap();
    let mut child = ChildGuard(Some(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("NHC_PLUGIN_COMMIT_CHILD", temp.path())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap(),
    ));
    let signal = temp.path().join("committed.json");
    let deadline = Instant::now() + Duration::from_secs(12);
    while !signal.exists() && Instant::now() < deadline {
        assert!(
            child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "child exited: {}",
            std::fs::read_to_string(temp.path().join("child.log")).unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(signal.exists(), "commit signal timed out");
    let payload: Value = serde_json::from_slice(&std::fs::read(&signal).unwrap()).unwrap();
    assert_eq!(payload["runtime_before"]["state"], "active");
    assert_eq!(payload["runtime_before"]["in_flight"], 1);
    let path = temp.path().join("phase.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    let committed: (bool, u64) = db
        .query_row(
            "SELECT enabled,revision FROM plugin_desired WHERE plugin_id='builtin.files'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(committed, (false, 1));
    let receipt: (u64,u64,i64) = db.query_row("SELECT expected_revision,result_revision,committed_at FROM plugin_management_requests WHERE key='post-commit-kill'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!((receipt.0, receipt.1), (0, 1));
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before-a"
    );
    let exit = child.stop(); // real OS kill followed by wait; no surviving Tokio/management thread
    assert!(!exit.success());
    eprintln!("F07 child kill+wait completed: {exit}; persisted revision=1, before apply");
    let store = Arc::new(Store::open(&path).unwrap());
    assert!(store.recover().unwrap() > 0);
    let task_id = payload["task_id"].as_str().unwrap();
    let approval_id = payload["approval_id"].as_str().unwrap();
    assert_eq!(
        store.approval(approval_id).unwrap().execution_state,
        approval::ExecutionState::Unknown
    );
    assert_eq!(store.task(task_id).unwrap().status, "interrupted");
    let after_recovery = n10_business_rows(&db);
    let epoch_after_recovery = n10_epoch(&db);
    let restarted = Engine::new(store, 2).unwrap();
    assert_eq!(n10_epoch(&db), epoch_after_recovery + 1);
    let state = restarted.plugin_status("builtin.files").unwrap();
    assert!(!state.desired.enabled && state.converged && !state.runtime.admission_open);
    assert_eq!(state.runtime.effects_count, 0);
    assert_eq!(state.runtime.in_flight, 0);
    assert!(!restarted.is_busy());
    let replay = n10_set(&restarted, false, 0, "post-commit-kill");
    assert!(replay.replayed);
    assert_eq!(replay.receipt.committed_at, receipt.2);
    assert_eq!(replay.current, state);
    assert!(
        n10_set(&restarted, true, 1, "explicit-enable")
            .current
            .converged
    );
    assert_eq!(n10_business_rows(&db), after_recovery); // no launch, unknown retry, grant replacement, or old Task writes
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before-a"
    );
    assert_eq!(
        restarted
            .store
            .approval(approval_id)
            .unwrap()
            .execution_state,
        approval::ExecutionState::Unknown
    );
    restarted.unload_builtin_files().unwrap();
    drop(restarted);
    let enabled_restart = Engine::new(Arc::new(Store::open(&path).unwrap()), 2).unwrap();
    assert_eq!(n10_epoch(&db), epoch_after_recovery + 2);
    assert!(
        enabled_restart
            .plugin_status("builtin.files")
            .unwrap()
            .converged
    );
    assert!(!enabled_restart.is_busy());
    assert_eq!(n10_business_rows(&db), after_recovery);
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before-a"
    );
}

#[tokio::test]
async fn n09_claim_wins_stop_and_duplicate_claim_never_gets_a_second_lease() {
    for unload in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (engine, task_id) = two_write_engine(temp.path()).await;
        let approval = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(record) = engine
                    .store
                    .pending_approvals_for_task(&task_id)
                    .unwrap()
                    .into_iter()
                    .next()
                {
                    break record;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Holding the real production workspace gate blocks perform_tool
        // after acquire/claim but before prepare or any filesystem write.
        let guard = engine.gate.lock().await;
        engine
            .decide_approval(&approval.id, true, Some("test"))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            while engine.store.approval(&approval.id).unwrap().execution_state
                != approval::ExecutionState::Claimed
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut task = engine.store.task(&task_id).unwrap();
        let call = task
            .messages
            .iter()
            .filter_map(|message| message["tool_calls"].as_array())
            .flatten()
            .find(|call| call["id"] == approval.tool_call_id)
            .unwrap()
            .clone();
        let args: Value =
            serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap();
        let duplicate = tokio::time::timeout(
            Duration::from_secs(2),
            engine.execute_or_wait(
                &mut task,
                &call,
                "write_file",
                &args,
                "phase-test-key",
                &CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
        assert!(duplicate.is_err());
        assert!(
            engine
                .store
                .workspace_changes_for_task(&task_id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            b"before-a"
        );
        if unload {
            engine.unload_builtin_files().unwrap();
        } else {
            engine.disable_builtin_files().unwrap();
        }
        let definitions = engine.tool_runtime.definitions(&task).unwrap();
        assert!(definitions.iter().all(|definition| !matches!(
            definition["function"]["name"].as_str(),
            Some("read_file" | "write_file" | "list_files" | "search_files")
        )));
        drop(guard);
        tokio::time::timeout(Duration::from_secs(8), async {
            while engine.is_busy() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            engine.store.approval(&approval.id).unwrap().execution_state,
            approval::ExecutionState::Finished
        );
        assert_eq!(
            engine
                .store
                .workspace_changes_for_task(&task_id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            b"after-0"
        );
        let events = engine.store.events(&task.run_id, 0).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "tool_start")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "tool_result")
                .count(),
            1
        );
    }
}

#[derive(Clone)]
struct ShellFixture {
    calls: Arc<AtomicUsize>,
    command: String,
}
async fn shell_reply(
    State(state): State<ShellFixture>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let index = state.calls.fetch_add(1, Ordering::SeqCst);
    let delta = if index == 0 {
        json!({"tool_calls":[{"index":0,"id":"shell-call","type":"function","function":{"name":"run_command","arguments":json!({"command":state.command,"timeout_seconds":10}).to_string()}}]})
    } else {
        json!({"content":"done"})
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}
async fn shell_engine(
    root: &Path,
    command: &str,
    tools: bool,
    allow_commands: bool,
) -> (Arc<Engine>, String, Arc<AtomicUsize>) {
    shell_engine_at(root, command, tools, allow_commands, &root.join("shell.db")).await
}
async fn shell_engine_at(
    root: &Path,
    command: &str,
    tools: bool,
    allow_commands: bool,
    db_path: &Path,
) -> (Arc<Engine>, String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/v1/chat/completions", post(shell_reply))
        .with_state(ShellFixture {
            calls: calls.clone(),
            command: command.into(),
        });
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = Arc::new(Store::open(db_path).unwrap());
    store
        .save_settings(&Settings {
            workspace: root.to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "route".into(),
                base_url: base,
                model: "mock".into(),
                max_tokens: 64,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "shell-fixture-key").unwrap();
    authorize_fixture(&store);
    let engine = Engine::new_with_runtime_for_test(store, ToolRuntime::builtin().unwrap()).unwrap();
    let task = engine
        .start(RunRequest {
            title: "shell".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "shell".into(),
                role: "shell".into(),
                route_id: "route".into(),
                prompt: "command".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools,
                allow_commands,
                max_rounds: 3,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    (engine, task.id, calls)
}
fn shell_set(
    engine: &Engine,
    enabled: bool,
    revision: u64,
    key: &str,
) -> crate::plugin_management::PluginMutationResult {
    engine
        .set_plugin_enabled(
            crate::plugin_management::BUILTIN_SHELL_ID,
            crate::plugin_management::SetPluginEnabled {
                enabled,
                expected_revision: revision,
            },
            key,
        )
        .unwrap()
}
fn assert_no_shell_execution(engine: &Engine, task_id: &str, calls: &AtomicUsize, root: &Path) {
    let task = engine.store.task(task_id).unwrap();
    assert!(!root.join("shell-marker.txt").exists());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!task.messages.iter().any(|m| m["role"] == "tool"));
    let events = engine.store.events(&task.run_id, 0).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind.as_str(), "tool_start" | "tool_result"))
    );
    assert_eq!(
        engine
            .plugin_status("builtin.shell")
            .unwrap()
            .runtime
            .in_flight,
        0
    );
}
#[tokio::test]
async fn n12_pending_disable_then_approval_ack_keeps_decision_without_claim() {
    for durable_only in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (engine, id, calls) = shell_engine(
            temp.path(),
            "[IO.File]::WriteAllText('shell-marker.txt','executed')",
            true,
            true,
        )
        .await;
        let pending = n10_pending(&engine, &id).await;
        let files = engine.plugin_status("builtin.files").unwrap();
        if durable_only {
            engine
                .store
                .set_plugin_desired(
                    "builtin.shell",
                    &crate::plugin_management::SetPluginEnabled {
                        enabled: false,
                        expected_revision: 0,
                    },
                    "durable-off",
                )
                .unwrap();
            assert!(
                engine
                    .plugin_status("builtin.shell")
                    .unwrap()
                    .runtime
                    .admission_open
            );
        } else {
            shell_set(&engine, false, 0, "shell-off");
        }
        engine
            .decide_approval_with_network_ack(&pending.id, true, Some("test"), true)
            .unwrap();
        if durable_only {
            let task = engine.store.task(&id).unwrap();
            let args = serde_json::from_str::<Value>(
                task.messages
                    .iter()
                    .find_map(|m| m["tool_calls"].as_array())
                    .unwrap()[0]["function"]["arguments"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            let error =
                crate::execution_authority::test_with_active_store_sync(&engine.store, || {
                    engine
                        .store
                        .claim_approval(&task, "shell-call", "run_command", &args)
                })
                .unwrap()
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<crate::plugin_management::PluginManagementError>(),
                Some(&crate::plugin_management::PluginManagementError::Unavailable)
            );
        }
        n10_idle(&engine).await;
        let approved = engine.store.approval(&pending.id).unwrap();
        assert_eq!(approved.status, ApprovalStatus::Approved);
        assert_eq!(
            approved.command_network_ack,
            approval::CommandNetworkAck::Acknowledged
        );
        assert_eq!(
            approved.execution_state,
            approval::ExecutionState::NotStarted
        );
        assert_no_shell_execution(&engine, &id, &calls, temp.path());
        assert_eq!(engine.plugin_status("builtin.files").unwrap(), files);
    }
}
#[tokio::test]
async fn n12_shell_tools_commands_project_and_ack_are_required() {
    for (tools, allow) in [(false, false), (true, false)] {
        let temp = tempfile::tempdir().unwrap();
        let (engine, id, calls) = shell_engine(
            temp.path(),
            "[IO.File]::WriteAllText('shell-marker.txt','executed')",
            tools,
            allow,
        )
        .await;
        n10_idle(&engine).await;
        assert!(engine.store.approvals_for_task(&id).unwrap().is_empty());
        assert_no_shell_execution(&engine, &id, &calls, temp.path());
    }
    for revoke in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (engine, id, calls) = shell_engine(
            temp.path(),
            "[IO.File]::WriteAllText('shell-marker.txt','executed')",
            true,
            true,
        )
        .await;
        let pending = n10_pending(&engine, &id).await;
        if revoke {
            engine
                .decide_approval_with_network_ack(&pending.id, true, Some("test"), true)
                .unwrap();
            let project = engine.store.current_project().unwrap().project_id.unwrap();
            let old = engine.store.project_config(&project).unwrap();
            let mut config = old.config;
            config.allow_commands = false;
            engine
                .store
                .configure_project(ConfigureProject {
                    project_id: project,
                    expected_revision: old.revision,
                    config,
                })
                .unwrap();
            n10_idle(&engine).await;
        } else {
            assert!(matches!(
                engine
                    .decide_approval(&pending.id, true, Some("test"))
                    .unwrap_err()
                    .downcast_ref::<approval::ApprovalError>(),
                Some(approval::ApprovalError::NetworkAcknowledgementRequired { .. })
            ));
            assert_eq!(
                engine.store.approval(&pending.id).unwrap().status,
                ApprovalStatus::Pending
            );
            engine.cancel(&id).unwrap();
            n10_idle(&engine).await;
        }
        assert_no_shell_execution(&engine, &id, &calls, temp.path());
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n12_claimed_shell_disable_preserves_work_and_cancel_prevents_unstarted_process() {
    for cancel in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (engine, id, calls) = shell_engine(
            temp.path(),
            "[IO.File]::WriteAllText('shell-marker.txt','executed'); Write-Output 'shell-complete'",
            true,
            true,
        )
        .await;
        let pending = n10_pending(&engine, &id).await;
        let files = engine.plugin_status("builtin.files").unwrap();
        let (ready_tx, ready_rx) = oneshot::channel();
        let ready = Mutex::new(Some(ready_tx));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = Mutex::new(release_rx);
        *engine.shell_claim_probe.lock().unwrap() = Some((
            id.clone(),
            Arc::new(move || {
                ready.lock().unwrap().take().unwrap().send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(8))
                    .unwrap();
            }),
        ));
        engine
            .decide_approval_with_network_ack(&pending.id, true, Some("test"), true)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(8), ready_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            engine.store.approval(&pending.id).unwrap().execution_state,
            approval::ExecutionState::Claimed
        );
        assert!(!temp.path().join("shell-marker.txt").exists());
        let disabled = shell_set(&engine, false, 0, "disable-claimed");
        assert!(disabled.current.converged && disabled.current.runtime.draining);
        assert_eq!(disabled.current.runtime.in_flight, 1);
        assert_eq!(disabled.current.runtime.effects_count, 0);
        assert_eq!(engine.plugin_status("builtin.files").unwrap(), files);
        let task = engine.store.task(&id).unwrap();
        assert!(
            engine
                .tool_runtime
                .acquire(&task, "run_command", || Ok(()))
                .is_err()
        );
        if cancel {
            engine.cancel(&id).unwrap();
        }
        release_tx.send(()).unwrap();
        n10_idle(&engine).await;
        if cancel {
            assert!(
                !temp.path().join("shell-marker.txt").exists(),
                "cancel before generation admission must not start the process"
            );
        } else {
            assert_eq!(
                std::fs::read_to_string(temp.path().join("shell-marker.txt")).unwrap(),
                "executed"
            );
        }
        assert_eq!(
            engine.store.approval(&pending.id).unwrap().execution_state,
            approval::ExecutionState::Finished
        );
        let task = engine.store.task(&id).unwrap();
        let outputs: Vec<_> = task
            .messages
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect();
        assert_eq!(outputs.len(), 1);
        let result: Value = serde_json::from_str(outputs[0]["content"].as_str().unwrap()).unwrap();
        if cancel {
            assert!(
                result["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("cancelled"))
            );
        } else {
            assert_eq!(result["exit_code"], 0);
            assert!(
                result["stdout"]
                    .as_str()
                    .unwrap()
                    .contains("shell-complete")
            );
        }
        let events = engine.store.events(&task.run_id, 0).unwrap();
        assert_eq!(events.iter().filter(|e| e.kind == "tool_start").count(), 1);
        assert_eq!(events.iter().filter(|e| e.kind == "tool_result").count(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), if cancel { 1 } else { 2 });
        assert_eq!(
            engine
                .plugin_status("builtin.shell")
                .unwrap()
                .runtime
                .in_flight,
            0
        );
        assert_eq!(engine.plugin_status("builtin.files").unwrap(), files);
    }
}

#[tokio::test]
async fn n12_spawned_typed_unknown_keeps_claim_and_stops_provider() {
    const TEST: &str =
        "engine::workspace_phase_tests::n12_spawned_typed_unknown_keeps_claim_and_stops_provider";
    if let Some(root) = std::env::var_os("NHC_TYPED_UNKNOWN_CHILD") {
        let root = std::path::PathBuf::from(root);
        let (engine, id, calls) = shell_engine(
            &root,
            "Start-Sleep -Seconds 9; [IO.File]::WriteAllText('shell-marker.txt','unexpected')",
            true,
            true,
        )
        .await;
        let pending = n10_pending(&engine, &id).await;
        let _fault = crate::command_execution::unknown_after_spawn_once(&id);
        engine
            .decide_approval_with_network_ack(&pending.id, true, Some("test"), true)
            .unwrap();
        n10_idle(&engine).await;
        let record = engine.store.approval(&pending.id).unwrap();
        assert_eq!(record.status, ApprovalStatus::Approved);
        assert_eq!(
            record.command_network_ack,
            approval::CommandNetworkAck::Acknowledged
        );
        assert_eq!(record.execution_state, approval::ExecutionState::Claimed);
        let task = engine.store.task(&id).unwrap();
        assert_eq!(task.status, "failed");
        assert!(!task.messages.iter().any(|m| m["role"] == "tool"));
        let events = engine.store.events(&task.run_id, 0).unwrap();
        assert_eq!(events.iter().filter(|e| e.kind == "tool_start").count(), 1);
        assert_eq!(events.iter().filter(|e| e.kind == "tool_result").count(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            engine
                .plugin_status("builtin.shell")
                .unwrap()
                .runtime
                .in_flight,
            0
        );
        assert!(matches!(
            engine
                .resume(&id, "must not retry")
                .await
                .unwrap_err()
                .downcast_ref::<approval::ApprovalError>(),
            Some(approval::ApprovalError::UnknownResult { .. })
        ));
        let before = n10_business_rows(&engine.store.db.lock().unwrap());
        assert!(
            engine
                .set_plugin_enabled(
                    "builtin.shell",
                    crate::plugin_management::SetPluginEnabled {
                        enabled: false,
                        expected_revision: 0
                    },
                    "unknown-off"
                )
                .is_err()
        );
        assert_eq!(n10_business_rows(&engine.store.db.lock().unwrap()), before);
        std::fs::write(
            root.join("typed-unknown.json"),
            serde_json::to_vec(&json!({
                "task_id": id, "approval_id": pending.id, "calls": calls.load(Ordering::SeqCst)
            }))
            .unwrap(),
        )
        .unwrap();
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let log = std::fs::File::create(temp.path().join("typed-unknown-child.log")).unwrap();
    let mut child = ChildGuard(Some(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("NHC_TYPED_UNKNOWN_CHILD", temp.path())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "typed unknown child did not exit"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        status.success(),
        "typed unknown child failed: {}",
        std::fs::read_to_string(temp.path().join("typed-unknown-child.log")).unwrap()
    );
    let payload: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("typed-unknown.json")).unwrap())
            .unwrap();
    assert_eq!(payload["calls"], 1);
    let id = payload["task_id"].as_str().unwrap();
    let approval_id = payload["approval_id"].as_str().unwrap();
    let observer = rusqlite::Connection::open(temp.path().join("shell.db")).unwrap();
    let before = n10_business_rows(&observer);
    drop(observer);
    let store = Arc::new(Store::open(&temp.path().join("shell.db")).unwrap());
    assert!(store.recover().unwrap() > 0);
    assert_eq!(
        store.approval(approval_id).unwrap().execution_state,
        approval::ExecutionState::Unknown
    );
    let after = n10_business_rows(&store.db.lock().unwrap());
    let epoch_after_recovery = n10_epoch(&store.db.lock().unwrap());
    for table in [
        "task_network_bindings",
        "network_authorizations",
        "task_project_bindings",
    ] {
        assert_eq!(after[table], before[table]);
    }
    let restarted = Engine::new(store, 2).unwrap();
    assert_eq!(
        n10_epoch(&restarted.store.db.lock().unwrap()),
        epoch_after_recovery + 1
    );
    shell_set(&restarted, false, 0, "unknown-off");
    shell_set(&restarted, true, 1, "unknown-on");
    assert!(matches!(
        restarted
            .resume(id, "still must not retry")
            .await
            .unwrap_err()
            .downcast_ref::<approval::ApprovalError>(),
        Some(approval::ApprovalError::UnknownResult { .. })
    ));
    assert_eq!(
        n10_business_rows(&restarted.store.db.lock().unwrap()),
        after
    );
    assert_eq!(payload["calls"], 1);
    assert!(!temp.path().join("shell-marker.txt").exists());
    eprintln!(
        "S06: task-scoped injection after real spawn; typed unknown preserved claim, not proof of actual OS cleanup failure"
    );
}
#[test]
fn n12_claim_before_spawn_child_kill_wait_recovers_unknown_without_retry() {
    const TEST: &str = "engine::workspace_phase_tests::n12_claim_before_spawn_child_kill_wait_recovers_unknown_without_retry";
    if let Ok(root) = std::env::var("NHC_SHELL_CLAIM_CHILD") {
        let root = std::path::PathBuf::from(root);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (engine, id, _) = shell_engine(
                &root,
                "[IO.File]::WriteAllText('shell-marker.txt','unexpected')",
                true,
                true,
            )
            .await;
            let pending = n10_pending(&engine, &id).await;
            let signal = root.join("claimed.json");
            let payload =
                serde_json::to_vec(&json!({"task_id":id,"approval_id":pending.id})).unwrap();
            *engine.shell_claim_probe.lock().unwrap() = Some((
                id.clone(),
                Arc::new(move || {
                    let staging = signal.with_extension("tmp");
                    std::fs::write(&staging, &payload).unwrap();
                    std::fs::rename(staging, &signal).unwrap();
                    let deadline = Instant::now() + Duration::from_secs(20);
                    while Instant::now() < deadline {
                        std::thread::park_timeout(Duration::from_millis(20));
                    }
                    panic!("parent did not stop claimed child within bound");
                }),
            ));
            engine
                .decide_approval_with_network_ack(&pending.id, true, Some("test"), true)
                .unwrap();
            n10_idle(&engine).await;
            panic!("claim stop point unexpectedly returned");
        });
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let output = std::fs::File::create(temp.path().join("child.log")).unwrap();
    let mut child = ChildGuard(Some(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("NHC_SHELL_CLAIM_CHILD", temp.path())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap(),
    ));
    let signal = temp.path().join("claimed.json");
    let deadline = Instant::now() + Duration::from_secs(12);
    while !signal.exists() && Instant::now() < deadline {
        assert!(
            child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "child exited: {}",
            std::fs::read_to_string(temp.path().join("child.log")).unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(signal.exists(), "claim signal timed out");
    let payload: Value = serde_json::from_slice(&std::fs::read(&signal).unwrap()).unwrap();
    let task_id = payload["task_id"].as_str().unwrap();
    let approval_id = payload["approval_id"].as_str().unwrap();
    let observer = rusqlite::Connection::open_with_flags(
        temp.path().join("shell.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let state: String = observer
        .query_row(
            "SELECT execution_state FROM approvals WHERE id=?1",
            [approval_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "claimed");
    let before = n10_business_rows(&observer);
    assert!(!temp.path().join("shell-marker.txt").exists());
    let exit = child.stop();
    assert!(!exit.success());
    eprintln!("S09: actual claim-before-spawn child kill+wait complete: {exit}");
    drop(observer);
    let store = Arc::new(Store::open(&temp.path().join("shell.db")).unwrap());
    assert!(store.recover().unwrap() > 0);
    assert_eq!(
        store.approval(approval_id).unwrap().execution_state,
        approval::ExecutionState::Unknown
    );
    assert_eq!(store.task(task_id).unwrap().status, "interrupted");
    let after = n10_business_rows(&store.db.lock().unwrap());
    let epoch_after_recovery = n10_epoch(&store.db.lock().unwrap());
    for table in [
        "task_network_bindings",
        "network_authorizations",
        "task_project_bindings",
    ] {
        assert_eq!(after[table], before[table]);
    }
    let engine = Engine::new(store, 2).unwrap();
    assert_eq!(
        n10_epoch(&engine.store.db.lock().unwrap()),
        epoch_after_recovery + 1
    );
    shell_set(&engine, false, 0, "crash-off");
    shell_set(&engine, true, 1, "crash-on");
    assert_eq!(n10_business_rows(&engine.store.db.lock().unwrap()), after);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert!(matches!(
            engine
                .resume(task_id, "must not retry")
                .await
                .unwrap_err()
                .downcast_ref::<approval::ApprovalError>(),
            Some(approval::ApprovalError::UnknownResult { .. })
        ));
    });
    assert_eq!(n10_business_rows(&engine.store.db.lock().unwrap()), after);
    assert!(!engine.is_busy());
    assert!(!temp.path().join("shell-marker.txt").exists());
    let task = engine.store.task(task_id).unwrap();
    assert!(!task.messages.iter().any(|m| m["role"] == "tool"));
    assert!(
        !engine
            .store
            .events(&task.run_id, 0)
            .unwrap()
            .iter()
            .any(|e| matches!(e.kind.as_str(), "tool_start" | "tool_result"))
    );
}

impl ChildGuard {
    fn stop(&mut self) -> ExitStatus {
        let mut child = self.0.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn assert_gate_pending<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
}

#[tokio::test]
async fn n07_send_and_restore_wait_on_same_production_gate() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, old_id) = two_write_engine_kind(temp.path(), SessionKind::Chat).await;
    finish_two_writes(&engine, &old_id, usize::MAX).await;
    let old = engine.store.task(&old_id).unwrap();
    assert_eq!(old.status, "completed");
    let context = engine.store.run_context(&old.run_id).unwrap();
    let cmd = SendChatTurn {
        session_id: context.session.id,
        agent_id: context.tasks[0].agent_id.clone(),
        expected_last_turn_id: context.latest_turn.id,
        message: "continue after restore".into(),
        idempotency_key: "gate-after-restore".into(),
    };
    let guard = engine.gate.lock().await;
    let mut restore = std::pin::pin!(engine.restore(&old_id));
    let mut send = std::pin::pin!(engine.send_chat_turn(cmd));
    assert_gate_pending(restore.as_mut()).await;
    assert_gate_pending(send.as_mut()).await;
    assert!(engine.latest_restore(&old_id).unwrap().is_none());
    assert_eq!(engine.store.run(&old.run_id).unwrap().tasks.len(), 1);
    drop(guard);
    let (restored, appended) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(restore, send)
    })
    .await
    .unwrap();
    assert_eq!(restored.unwrap().status, RestoreStatus::Complete);
    let appended = appended.unwrap();
    let pending = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(record) = engine
                .store
                .pending_approvals_for_task(&appended.task.legacy_task_id)
                .unwrap()
                .into_iter()
                .next()
            {
                break record;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let next = engine.store.task(&appended.task.legacy_task_id).unwrap();
    assert!(
        next.messages[old.messages.len()]["content"]
            .as_str()
            .unwrap()
            .contains("Host recovery fact")
    );
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before-a"
    );
    engine
        .decide_approval(&pending.id, true, Some("test"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.store.task(&next.id).unwrap().status, "completed");
    assert_eq!(engine.store.run(&old.run_id).unwrap().tasks.len(), 2);
    assert_eq!(
        engine
            .store
            .workspace_changes_for_task(&next.id)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn restore_probe_is_scoped_and_committed_callback_holds_no_store_lock() {
    use restore_probe::Stage;
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = two_write_engine(temp.path()).await;
    finish_two_writes(&engine, &task_id, usize::MAX).await;
    assert!(Engine::new(engine.store.clone(), 2).is_err());
    let other_root = tempfile::tempdir().unwrap();
    let other = Engine::new(
        Arc::new(Store::open(&other_root.path().join("other.db")).unwrap()),
        2,
    )
    .unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    engine.set_restore_probe(
        "different-task".into(),
        Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        }),
    );
    engine.emit_restore_probe(&task_id, "src/a.txt", Stage::Resolve, &Ok(()));
    other.emit_restore_probe("different-task", "src/a.txt", Stage::Resolve, &Ok(()));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    let count = hits.clone();
    let weak = Arc::downgrade(&engine);
    let path = temp.path().join("phase.db");
    let root = temp.path().to_owned();
    let expected_id = task_id.clone();
    engine.set_restore_probe(
        task_id.clone(),
        Arc::new(move |event| {
            assert_eq!(event.task_id, expected_id);
            if event.stage == Stage::OutcomeCommitted {
                assert!(event.error.is_none());
                assert_eq!(event.path, "src/a.txt");
                let engine = weak.upgrade().unwrap();
                // Both this Store lock and an independent writer must be available.
                let receipt = engine
                    .store
                    .latest_restore(&event.task_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(receipt.restored, 1);
                let db = rusqlite::Connection::open(&path).unwrap();
                db.busy_timeout(Duration::ZERO).unwrap();
                db.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
                let status: String = db
                    .query_row("SELECT status FROM workspace_restore_outcomes", [], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert_eq!(status, "complete");
                assert_eq!(std::fs::read(root.join("src/a.txt")).unwrap(), b"before-a");
                count.fetch_add(1, Ordering::SeqCst);
                // Reentrant clearing proves the callback is outside the probe mutex.
                engine.clear_restore_probe();
            }
        }),
    );
    assert_eq!(
        engine.restore(&task_id).await.unwrap().status,
        RestoreStatus::Complete
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    engine.emit_restore_probe(&task_id, "src/a.txt", Stage::OutcomeCommitted, &Ok(()));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // A real no-hook positive control in a separate Engine and database.
    let plain = tempfile::tempdir().unwrap();
    let (plain_engine, plain_id) = two_write_engine(plain.path()).await;
    finish_two_writes(&plain_engine, &plain_id, usize::MAX).await;
    assert_eq!(
        plain_engine.restore(&plain_id).await.unwrap().status,
        RestoreStatus::Complete
    );
}

#[cfg(windows)]
#[tokio::test]
async fn restore_probe_distinguishes_real_file_action_and_sqlite_outcome_errors() {
    use restore_probe::Stage;
    use std::os::windows::fs::OpenOptionsExt;
    for filesystem_error in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let (engine, task_id) = two_write_engine(temp.path()).await;
        finish_two_writes(&engine, &task_id, usize::MAX).await;
        let held = Arc::new(Mutex::new(None));
        let file = held.clone();
        let target = temp.path().join("src/a.txt");
        let errors = Arc::new(Mutex::new(Vec::new()));
        let captured = errors.clone();
        let weak = Arc::downgrade(&engine);
        if !filesystem_error {
            rusqlite::Connection::open(temp.path().join("phase.db")).unwrap()
                .execute_batch("CREATE TRIGGER reject_outcome BEFORE UPDATE ON workspace_restore_outcomes WHEN NEW.status='complete' BEGIN SELECT RAISE(ABORT,'private trigger detail'); END;").unwrap();
        }
        engine.set_restore_probe(
            task_id.clone(),
            Arc::new(move |event| {
                if filesystem_error
                    && event.stage == Stage::ReadCurrent
                    && event.error.is_none()
                    && weak
                        .upgrade()
                        .unwrap()
                        .store
                        .latest_restore(&event.task_id)
                        .unwrap()
                        .is_some()
                {
                    // Read/write sharing is allowed, delete/rename is not: replacement,
                    // rather than the earlier read, must fail with a real Windows code.
                    *file.lock().unwrap() = Some(
                        std::fs::OpenOptions::new()
                            .read(true)
                            .share_mode(3)
                            .open(&target)
                            .unwrap(),
                    );
                }
                if event.error.is_some() || event.stage == Stage::OutcomeCommitted {
                    captured.lock().unwrap().push(event);
                }
            }),
        );
        let error = engine.restore(&task_id).await.unwrap_err();
        let public = format!("{error:#}");
        assert!(!public.contains("private trigger detail"));
        let receipt = engine.store.latest_restore(&task_id).unwrap().unwrap();
        assert_eq!(receipt.status, RestoreStatus::Unknown);
        assert_eq!(receipt.restored, 0);
        assert_eq!(receipt.outcomes.len(), 1);
        assert_eq!(receipt.outcomes[0].status, RestoreStatus::Unknown);
        {
            let observed = errors.lock().unwrap();
            assert_eq!(observed.len(), 1);
            let event = &observed[0];
            assert_eq!(event.task_id, task_id);
            assert_eq!(event.path, "src/a.txt");
            let codes = event.error.as_ref().unwrap();
            if filesystem_error {
                assert_eq!(event.stage, Stage::FileAction);
                assert!(codes.io.is_some());
                assert_eq!(codes.sqlite, None);
            } else {
                assert_eq!(event.stage, Stage::OutcomePersist);
                assert_eq!(codes.io, None);
                assert_eq!(codes.sqlite, Some(1811));
            }
        }
        drop(held.lock().unwrap().take());
        let expected = if filesystem_error {
            b"after-1".as_slice()
        } else {
            b"before-a".as_slice()
        };
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            expected
        );
        engine.clear_restore_probe();
        assert!(engine.restore(&task_id).await.is_err());
        assert_eq!(
            engine.store.latest_restore(&task_id).unwrap().unwrap(),
            receipt
        );
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn audit_resume_pending_survives_reopen_and_consumption_rolls_back() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = two_write_engine(temp.path()).await;
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine
            .store
            .pending_approvals_for_task(&task_id)
            .unwrap()
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    engine.cancel(&task_id).unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut queued = engine.store.task(&task_id).unwrap();
    assert!(!crate::tool_call_application::unanswered_tool_calls(&queued.messages).is_empty());
    queued.status = "queued".into();
    queued.error = None;
    let (_, operation) = engine.reserve_task_worker(&task_id).unwrap();
    operation
        .test_sync(|| {
            engine
                .store
                .resume_task_with_pending_input(&queued, "accepted-first")
        })
        .unwrap();
    assert!(
        !queued
            .messages
            .iter()
            .any(|m| m["content"] == "accepted-first")
    );
    drop(operation);
    drop(engine);

    let store = Arc::new(Store::open(&temp.path().join("phase.db")).unwrap());
    store.recover().unwrap();
    assert_eq!(store.pending_resume_inputs(&task_id).unwrap().len(), 1);
    let engine = Engine::new(store.clone(), 2).unwrap();
    let mut queued = store.task(&task_id).unwrap();
    queued.status = "queued".into();
    queued.error = None;
    let (_, operation) = engine.reserve_task_worker(&task_id).unwrap();
    operation
        .test_sync(|| store.resume_task_with_pending_input(&queued, "accepted-second"))
        .unwrap();
    assert!(
        operation
            .test_sync(|| store.consume_resume_inputs(&queued))
            .unwrap_err()
            .to_string()
            .contains("工具调用组未闭合")
    );
    operation
        .hold_body()
        .run(engine.tool_calls.reconcile_open_tools(
            &mut queued,
            &CancellationToken::new(),
            "phase-test-key",
        ))
        .await
        .unwrap();
    let before = store.task(&task_id).unwrap().messages;
    store.db.lock().unwrap().execute_batch("CREATE TRIGGER audit_reject_consumption BEFORE INSERT ON events WHEN NEW.kind='unknown:resume_consumed' BEGIN SELECT RAISE(ABORT,'audit consumption fault'); END;").unwrap();
    assert!(
        operation
            .test_sync(|| store.consume_resume_inputs(&queued))
            .unwrap_err()
            .to_string()
            .contains("audit consumption fault")
    );
    assert_eq!(store.task(&task_id).unwrap().messages, before);
    assert_eq!(store.pending_resume_inputs(&task_id).unwrap().len(), 2);
    store
        .db
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER audit_reject_consumption;")
        .unwrap();
    let delivered = operation
        .test_sync(|| store.consume_resume_inputs(&queued))
        .unwrap();
    let tail: Vec<_> = delivered.messages[before.len()..]
        .iter()
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(tail, ["accepted-first", "accepted-second"]);
    assert!(store.pending_resume_inputs(&task_id).unwrap().is_empty());
    assert_eq!(
        operation
            .test_sync(|| store.consume_resume_inputs(&delivered))
            .unwrap()
            .messages,
        delivered.messages
    );
    operation
        .test_sync(|| {
            store.event(
                &task_id,
                "resume",
                json!({"message":"legacy-already-delivered"}),
            )
        })
        .unwrap();
    assert!(store.pending_resume_inputs(&task_id).unwrap().is_empty());
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before-a"
    );
    drop(operation);
    drop(engine);
    drop(store);
    let reopened = Store::open(&temp.path().join("phase.db")).unwrap();
    assert!(reopened.pending_resume_inputs(&task_id).unwrap().is_empty());
    assert_eq!(
        reopened.task(&task_id).unwrap().messages,
        delivered.messages
    );
    assert_eq!(
        reopened
            .db
            .lock()
            .unwrap()
            .pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        18
    );
}

#[tokio::test]
async fn audit_shutdown_deadline_covers_gate_and_rejects_queued_start_resume() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = two_write_engine(temp.path()).await;
    finish_two_writes(&engine, &task_id, usize::MAX).await;
    let task = engine.store.task(&task_id).unwrap();
    let before = resume_business_snapshot(&engine.store);
    let gate = engine.gate.lock().await;
    let mut start = std::pin::pin!(engine.start(RunRequest {
        title: "closing race".into(),
        kind: SessionKind::Chat,
        tasks: vec![task.spec.clone()]
    }));
    let mut resume = std::pin::pin!(engine.resume(&task_id, "must not be accepted"));
    assert_gate_pending(start.as_mut()).await;
    assert_gate_pending(resume.as_mut()).await;
    let report = tokio::time::timeout(
        Duration::from_secs(1),
        engine.shutdown(Duration::from_millis(20)),
    )
    .await
    .unwrap();
    assert!(report.timed_out);
    assert!(!report.settled());
    drop(gate);
    assert!(
        start
            .await
            .unwrap_err()
            .to_string()
            .contains("Host 正在关闭")
    );
    assert!(
        resume
            .await
            .unwrap_err()
            .to_string()
            .contains("Host 正在关闭")
    );
    assert_eq!(resume_business_snapshot(&engine.store), before);
    assert!(engine.shutdown(Duration::from_secs(1)).await.settled());
}

#[tokio::test]
async fn audit_legacy_checkpoint_order_remains_readable_without_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = two_write_engine_paths(temp.path(), SessionKind::Chat, true).await;
    finish_two_writes(&engine, &task_id, usize::MAX).await;
    let created = engine
        .create_checkpoint(&task_id, "legacy-order")
        .await
        .unwrap()
        .checkpoint;
    let mut old_sources = engine.store.workspace_changes_for_task(&task_id).unwrap();
    assert_eq!(old_sources.len(), 2);
    old_sources.reverse();
    let legacy_bytes = {
        let db = engine.store.db.lock().unwrap();
        db.execute(
            "UPDATE workspace_changes SET created_at=1000-rowid WHERE task_id=?1",
            [&task_id],
        )
        .unwrap();
        let raw: String = db
            .query_row(
                "SELECT manifest FROM checkpoints WHERE id=?1",
                [&created.checkpoint_id],
                |r| r.get(0),
            )
            .unwrap();
        let mut manifest: crate::checkpoint::Manifest = serde_json::from_str(&raw).unwrap();
        manifest.source_digest = crate::checkpoint::source_digest(&old_sources).unwrap();
        let raw = serde_json::to_string(&manifest).unwrap();
        db.execute(
            "UPDATE checkpoints SET manifest=?2,manifest_digest=?3 WHERE id=?1",
            rusqlite::params![created.checkpoint_id, raw, digest(raw.as_bytes())],
        )
        .unwrap();
        raw
    };
    drop(engine);
    let store = Arc::new(Store::open(&temp.path().join("phase.db")).unwrap());
    let engine = Engine::new(store.clone(), 2).unwrap();
    assert_eq!(
        engine.checkpoint(&created.checkpoint_id).unwrap().entries,
        created.entries
    );
    let after: String = store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT manifest FROM checkpoints WHERE id=?1",
            [&created.checkpoint_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, legacy_bytes);
    assert_eq!(
        engine
            .restore_checkpoint(&created.checkpoint_id)
            .await
            .unwrap()
            .status,
        RestoreStatus::Complete
    );
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"before-a"
    );
    assert_eq!(
        std::fs::read(temp.path().join("src/b.txt")).unwrap(),
        b"before-b"
    );
}

#[tokio::test]
async fn audit_legacy_same_path_checkpoint_reads_but_refuses_different_restore() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = two_write_engine(temp.path()).await;
    finish_two_writes(&engine, &task_id, usize::MAX).await;
    let checkpoint = engine
        .create_checkpoint(&task_id, "legacy-same-path")
        .await
        .unwrap()
        .checkpoint;
    let mut sources = engine.store.workspace_changes_for_task(&task_id).unwrap();
    assert_eq!(sources.len(), 2);
    sources.reverse();
    let legacy_bytes = {
        let db = engine.store.db.lock().unwrap();
        db.execute(
            "UPDATE workspace_changes SET created_at=1000-rowid WHERE task_id=?1",
            [&task_id],
        )
        .unwrap();
        let raw: String = db
            .query_row(
                "SELECT manifest FROM checkpoints WHERE id=?1",
                [&checkpoint.checkpoint_id],
                |r| r.get(0),
            )
            .unwrap();
        let mut manifest: crate::checkpoint::Manifest = serde_json::from_str(&raw).unwrap();
        manifest.source_digest = crate::checkpoint::source_digest(&sources).unwrap();
        manifest.entries[0].before_digest = sources[0].before_digest.clone();
        manifest.entries[0].after_digest = sources[1].after_digest.clone().unwrap();
        let raw = serde_json::to_string(&manifest).unwrap();
        db.execute(
            "UPDATE checkpoints SET manifest=?2,manifest_digest=?3 WHERE id=?1",
            rusqlite::params![checkpoint.checkpoint_id, raw, digest(raw.as_bytes())],
        )
        .unwrap();
        raw
    };
    drop(engine);
    let store = Arc::new(Store::open(&temp.path().join("phase.db")).unwrap());
    let engine = Engine::new(store.clone(), 2).unwrap();
    let loaded = engine.checkpoint(&checkpoint.checkpoint_id).unwrap();
    assert_ne!(loaded.entries, checkpoint.entries);
    let before = std::fs::read(temp.path().join("src/a.txt")).unwrap();
    let task_before = serde_json::to_value(store.task(&task_id).unwrap()).unwrap();
    assert!(store.latest_restore(&task_id).unwrap().is_none());
    for result in [
        engine.restore_checkpoint(&checkpoint.checkpoint_id).await,
        engine.restore(&task_id).await,
    ] {
        assert!(matches!(
            result.unwrap_err().downcast_ref::<WorkspaceChangeError>(),
            Some(WorkspaceChangeError::Conflict { receipt: None })
        ));
    }
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        before
    );
    assert_eq!(
        serde_json::to_value(store.task(&task_id).unwrap()).unwrap(),
        task_before
    );
    assert!(store.latest_restore(&task_id).unwrap().is_none());
    let after: String = store
        .db
        .lock()
        .unwrap()
        .query_row(
            "SELECT manifest FROM checkpoints WHERE id=?1",
            [&checkpoint.checkpoint_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, legacy_bytes);
    assert_eq!(
        engine.checkpoint(&checkpoint.checkpoint_id).unwrap(),
        loaded
    );
}

async fn provider_reply(
    State(calls): State<Arc<AtomicUsize>>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let delta = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
        json!({"tool_calls":[{"index":0,"id":"phase-write","type":"function","function":{
            "name":"write_file","arguments":json!({"path":"src/a.txt","content":"after"}).to_string()
        }}]})
    } else {
        json!({"content":"done"})
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

async fn two_write_reply(
    State(calls): State<Arc<AtomicUsize>>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let raw_index = calls.fetch_add(1, Ordering::SeqCst);
    let multiple_paths = raw_index >= 100;
    let index = raw_index % 100;
    let delta = if index < 2 || index == 3 {
        json!({"tool_calls":[{"index":0,"id":format!("phase-{index}"),"type":"function","function":{
            "name":"write_file","arguments":json!({"path":if multiple_paths && index==1 {"src/b.txt"} else {"src/a.txt"},"content":format!("after-{index}")}).to_string()
        }}]})
    } else {
        json!({"content":"done"})
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

#[derive(Clone)]
struct R01Provider {
    calls: Arc<AtomicUsize>,
    command: bool,
    first_is_write: bool,
    bearer: bool,
}

async fn r01_reply(
    State(state): State<R01Provider>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let delta = if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
        let secret = if state.bearer {
            "Bearer mock-approval-secret"
        } else {
            "sk-abc"
        };
        let (name, args) = if state.command {
            (
                "run_command",
                json!({"command":format!("[IO.File]::WriteAllText('shell-marker.txt','{secret}')"),"timeout_seconds":10}),
            )
        } else {
            (
                "write_file",
                if state.bearer {
                    json!({"path":"src/marker.txt","content":secret,"env":{"API_KEY":secret}})
                } else {
                    json!({"path":"src/marker.txt","content":secret})
                },
            )
        };
        let (first_name, first_args) = if state.first_is_write {
            (
                "write_file",
                json!({"path":"src/first.txt","content":"first"}),
            )
        } else {
            ("read_file", json!({"path":"src/input.txt"}))
        };
        json!({"tool_calls":[
            {"index":0,"id":"r01-read","type":"function","function":{"name":first_name,"arguments":first_args.to_string()}},
            {"index":1,"id":"r01-effect","type":"function","function":{"name":name,"arguments":args.to_string()}}
        ]})
    } else {
        json!({"content":"done"})
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

struct R01Strategy {
    deny_first: bool,
}

impl crate::agent_runtime::AgentStrategy for R01Strategy {
    fn decide(
        &self,
        input: crate::agent_runtime::StrategyInput<'_>,
        _messages: &[Value],
        _round: usize,
        _max_rounds: usize,
    ) -> std::result::Result<
        crate::agent_runtime::StrategyAction,
        crate::agent_runtime::StrategyError,
    > {
        use crate::agent_runtime::{CompletionKind, StrategyAction, StrategyInput, ToolVote};
        Ok(match input {
            StrategyInput::BeforeCompletion { tools } => {
                StrategyAction::UseTools((0..tools.len()).collect())
            }
            StrategyInput::AfterCompletion {
                outcome: CompletionKind::NoToolCalls,
            } => StrategyAction::Finish,
            StrategyInput::AfterCompletion {
                outcome: CompletionKind::ToolGroup(tools),
            } => StrategyAction::ToolVotes(
                tools
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        if self.deny_first && index == 0 {
                            ToolVote::Deny
                        } else {
                            ToolVote::ApplyHostPolicy
                        }
                    })
                    .collect(),
            ),
        })
    }
}

#[tokio::test]
async fn r01_same_group_safe_snapshot_keeps_sensitive_approval_binding() {
    for (deny_first, command, cancel, approve, first_is_write, bearer) in [
        (false, false, false, false, false, false),
        (false, true, true, false, false, false),
        (true, false, false, false, false, false),
        (true, true, true, false, false, false),
        (false, false, false, true, false, false),
        (false, false, false, false, true, false),
        (false, false, false, false, false, true),
        (false, true, true, false, false, true),
        (false, false, false, true, false, true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/input.txt"), "before").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/v1/chat/completions", post(r01_reply))
            .with_state(R01Provider {
                calls: calls.clone(),
                command,
                first_is_write,
                bearer,
            });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let store = Arc::new(Store::open(&temp.path().join("r01.db")).unwrap());
        store
            .save_settings(&Settings {
                workspace: temp.path().to_string_lossy().into(),
                max_concurrency: 2,
                routes: vec![Route {
                    id: "route".into(),
                    name: "route".into(),
                    base_url: base,
                    model: "mock".into(),
                    max_tokens: 128,
                    parallel_limit: 2,
                    key_env: None,
                }],
                newapi: None,
            })
            .unwrap();
        store.put_secret("route", "r01-test-key").unwrap();
        authorize_fixture(&store);
        let agent =
            crate::agent_runtime::AgentRuntime::with_strategy_for_test(Arc::new(R01Strategy {
                deny_first,
            }))
            .unwrap();
        let mut engine = Engine::new_with_agent_runtime_for_test(store.clone(), 2, agent).unwrap();
        let task = engine
            .start(RunRequest {
                title: "r01".into(),
                kind: if bearer {
                    SessionKind::Chat
                } else {
                    SessionKind::Team
                },
                tasks: vec![TaskSpec {
                    name: "worker".into(),
                    role: "worker".into(),
                    route_id: "route".into(),
                    prompt: "inspect".into(),
                    depends_on: vec![],
                    write_scopes: vec!["src".into()],
                    tools: true,
                    allow_commands: true,
                    max_rounds: 3,
                }],
            })
            .await
            .unwrap()
            .tasks
            .remove(0);
        if first_is_write {
            let first = n10_pending(&engine, &task.id).await;
            assert_eq!(first.tool_call_id, "r01-read");
            engine
                .decide_approval(&first.id, false, Some("test"))
                .unwrap();
        }
        let pending = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(record) = store
                    .pending_approvals_for_task(&task.id)
                    .unwrap()
                    .into_iter()
                    .next()
                {
                    break record;
                }
                let snapshot = store.task(&task.id).unwrap();
                assert!(
                    !matches!(
                        snapshot.status.as_str(),
                        "failed" | "cancelled" | "completed"
                    ),
                    "task ended before approval: status={} error={:?}",
                    snapshot.status,
                    snapshot.error
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("approval not created before timeout");
        assert_eq!(pending.tool_call_id, "r01-effect");
        let secret = if bearer {
            "Bearer mock-approval-secret"
        } else {
            "sk-abc"
        };
        let raw_args = if command {
            json!({"command":format!("[IO.File]::WriteAllText('shell-marker.txt','{secret}')"),"timeout_seconds":10})
        } else if bearer {
            json!({"path":"src/marker.txt","content":secret,"env":{"API_KEY":secret}})
        } else {
            json!({"path":"src/marker.txt","content":secret})
        };
        assert_eq!(
            pending.args_digest,
            approval::args_digest(&raw_args).unwrap()
        );
        let persisted = store.task(&task.id).unwrap();
        assert!(
            !serde_json::to_string(&persisted)
                .unwrap()
                .contains(if bearer {
                    "mock-approval-secret"
                } else {
                    "sk-abc"
                })
        );
        if cancel {
            store.cancel_pending_approvals(&task.id).unwrap();
        } else {
            engine
                .decide_approval(&pending.id, approve, Some("test"))
                .unwrap();
        }
        n10_idle(&engine).await;
        let finished = store.task(&task.id).unwrap();
        assert_eq!(
            finished.status,
            if cancel { "cancelled" } else { "completed" }
        );
        let events = store.events(&task.run_id, 0).unwrap();
        let results: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "tool_result")
            .collect();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].data["origin"],
            if first_is_write {
                "approval_denied"
            } else if deny_first {
                "agent_denied"
            } else {
                "read_completed"
            }
        );
        if approve {
            assert_eq!(results[1].data["approval_id"], pending.id);
            assert_eq!(
                store.approval(&pending.id).unwrap().execution_state,
                approval::ExecutionState::Finished
            );
        } else {
            assert_eq!(
                results[1].data["origin"],
                if cancel {
                    "approval_cancelled"
                } else {
                    "approval_denied"
                }
            );
        }
        assert!(
            !serde_json::to_string(&events).unwrap().contains(if bearer {
                "mock-approval-secret"
            } else {
                "sk-abc"
            })
        );
        assert!(!temp.path().join("src/marker.txt").exists());
        assert!(!temp.path().join("shell-marker.txt").exists());
        assert_eq!(calls.load(Ordering::SeqCst), if cancel { 1 } else { 2 });
        assert_eq!(
            store.approvals_for_task(&task.id).unwrap().len(),
            if first_is_write { 2 } else { 1 }
        );
        assert!(!temp.path().join("src/first.txt").exists());
        if bearer && !approve {
            if cancel {
                let before = n10_rows(&store.db.lock().unwrap());
                assert!(store.recover().is_err());
                assert_eq!(n10_rows(&store.db.lock().unwrap()), before);
                assert!(engine.shutdown(Duration::from_secs(5)).await.settled());
                drop(engine);
                store.recover().unwrap();
                let agent = crate::agent_runtime::AgentRuntime::with_strategy_for_test(Arc::new(
                    R01Strategy { deny_first },
                ))
                .unwrap();
                engine = Engine::new_with_agent_runtime_for_test(store.clone(), 2, agent).unwrap();
                engine
                    .resume(&task.id, "continue after cancellation")
                    .await
                    .unwrap();
                n10_idle(&engine).await;
            }
            let mut previous = store.task(&task.id).unwrap();
            assert_eq!(previous.status, "completed");
            let expected = approval::tool_error(if cancel {
                approval::ApprovalStatus::Cancelled
            } else {
                approval::ApprovalStatus::Denied
            });
            assert!(
                previous
                    .messages
                    .iter()
                    .any(|message| message["tool_call_id"] == "r01-effect"
                        && message["content"] == expected)
            );
            // Model a safe pre-existing snapshot whose JSON object used noncanonical whitespace.
            let legacy_args = r#" { "path" : "src/input.txt" } "#;
            let call = previous
                .messages
                .iter_mut()
                .flat_map(|message| {
                    message
                        .get_mut("tool_calls")
                        .and_then(Value::as_array_mut)
                        .into_iter()
                        .flatten()
                })
                .find(|call| call["id"] == "r01-read")
                .unwrap();
            call["function"]["arguments"] = json!(legacy_args);
            store
                .db
                .lock()
                .unwrap()
                .execute(
                    "UPDATE tasks SET value=?2 WHERE id=?1",
                    rusqlite::params![task.id, serde_json::to_string(&previous).unwrap()],
                )
                .unwrap();
            let loaded = store.task(&task.id).unwrap();
            assert_eq!(loaded.messages, previous.messages);
            let context = store.run_context(&loaded.run_id).unwrap();
            let receipt = engine
                .send_chat_turn(SendChatTurn {
                    session_id: context.session.id,
                    agent_id: context.tasks[0].agent_id.clone(),
                    expected_last_turn_id: context.latest_turn.id,
                    message: "next turn".into(),
                    idempotency_key: "r04-next-turn".into(),
                })
                .await
                .unwrap();
            n10_idle(&engine).await;
            let next = store.task(&receipt.task.legacy_task_id).unwrap();
            assert_eq!(next.status, "completed");
            assert!(next.messages.starts_with(&loaded.messages));
            assert!(!temp.path().join("src/marker.txt").exists());
            assert!(!temp.path().join("shell-marker.txt").exists());
        }
        server.abort();
    }
}

#[derive(Clone)]
struct R01Hold {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

async fn r01_held_reply(
    State(state): State<R01Hold>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    state.entered.notify_one();
    state.release.notified().await;
    let chunk = json!({"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

#[tokio::test]
async fn r01_mismatched_live_call_cannot_create_approval_or_execute() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    let state = R01Hold {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(r01_held_reply))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = Arc::new(Store::open(&temp.path().join("r01-mismatch.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: temp.path().to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "route".into(),
                base_url: base,
                model: "mock".into(),
                max_tokens: 128,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "r01-test-key").unwrap();
    authorize_fixture(&store);
    let engine = Engine::new(store.clone(), 2).unwrap();
    let started = engine
        .start(RunRequest {
            title: "r01-mismatch".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "worker".into(),
                role: "worker".into(),
                route_id: "route".into(),
                prompt: "inspect".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 3,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    tokio::time::timeout(Duration::from_secs(8), state.entered.notified())
        .await
        .unwrap();
    let args = json!({"path":"src/right.txt","content":"sk-abc"});
    let call = json!({"id":"r01-target","type":"function","function":{
        "name":"write_file","arguments":args.to_string()
    }});
    let mut current = store.task(&started.id).unwrap();
    current
        .messages
        .push(json!({"role":"assistant","content":null,"tool_calls":[call.clone()]}));
    store.save_task(&current).unwrap();
    let mut current = store.task(&started.id).unwrap();
    let forged_args = json!({"path":"src/wrong.txt","content":"sk-abc"});
    let forged = json!({"id":"r01-target","type":"function","function":{
        "name":"write_file","arguments":forged_args.to_string()
    }});
    let before_local = current.messages.clone();
    let before_persisted = store.task(&started.id).unwrap().messages;
    let mismatched_safe = engine
        .execute_or_wait(
            &mut current,
            &forged,
            "write_file",
            &forged_args,
            "r01-test-key",
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        mismatched_safe
            .to_string()
            .contains("工具调用安全参数不匹配")
    );
    let mismatched_raw = engine
        .execute_or_wait(
            &mut current,
            &call,
            "write_file",
            &forged_args,
            "r01-test-key",
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(mismatched_raw.to_string().contains("工具调用参数不匹配"));
    assert_eq!(current.messages, before_local);
    assert_eq!(store.task(&started.id).unwrap().messages, before_persisted);
    assert!(store.approvals_for_task(&started.id).unwrap().is_empty());
    assert!(
        store
            .workspace_changes_for_task(&started.id)
            .unwrap()
            .is_empty()
    );
    let events = store.events(&started.run_id, 0).unwrap();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind.as_str(), "tool_start" | "tool_result"))
    );
    assert!(!temp.path().join("src/right.txt").exists());
    assert!(!temp.path().join("src/wrong.txt").exists());
    state.release.notify_one();
    n10_idle(&engine).await;
    server.abort();
}

fn n13_raw(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
pub(crate) async fn n13_completed_fixture(root: &Path) -> (Arc<Engine>, String) {
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    let (engine, task) =
        two_write_engine_paths_at(&source, SessionKind::Chat, false, &root.join("host.db")).await;
    finish_two_writes(&engine, &task, usize::MAX).await;
    (engine, task)
}
async fn n13_plan_for_engine(
    engine: &Engine,
    parent: &Path,
) -> (
    crate::worktree_management::CreateWorktree,
    crate::worktree_management::ProjectInventory,
    crate::git_worktree::PersistedWorktreePlan,
) {
    let project = engine.store.current_project().unwrap().project_id.unwrap();
    let root = engine.store.project_workspace(&project).unwrap();
    n13_raw(&root, &["init", "--quiet"]);
    n13_raw(&root, &["config", "user.name", "Fixture"]);
    n13_raw(&root, &["config", "user.email", "fixture@example.invalid"]);
    n13_raw(&root, &["config", "core.autocrlf", "false"]);
    n13_raw(&root, &["add", "--all"]);
    n13_raw(&root, &["commit", "--quiet", "-m", "quiescent baseline"]);
    let inventory = engine.store.worktree_inventory().unwrap();
    let roots = inventory.roots().unwrap();
    let source = roots
        .iter()
        .find(|(id, _)| id == &project)
        .unwrap()
        .1
        .clone();
    let ctx =
        crate::git_worktree::GitContext::new(roots.into_iter().map(|(_, r)| r).collect()).unwrap();
    let request = crate::worktree_management::CreateWorktree {
        source_project_id: project.clone(),
        name: "Reserved".into(),
        parent_path: parent.to_string_lossy().into(),
        expected_config_revision: engine.store.project_config(&project).unwrap().revision,
        expected_head: n13_raw(&root, &["rev-parse", "HEAD"]).trim().into(),
        idempotency_key: id(),
    };
    let operation = engine
        .store
        .execution_operation(CancellationToken::new())
        .unwrap();
    let plan = operation
        .run(crate::git_worktree::prepare(
            &ctx,
            &source,
            &crate::workspace_identity::RootIdentity::capture(parent).unwrap(),
            &id(),
            &request.expected_head,
        ))
        .await
        .unwrap()
        .into_persisted_plan();
    (request, inventory, plan)
}
#[track_caller]
fn n13_busy<T: std::fmt::Debug>(result: Result<T>) {
    let error = result.unwrap_err();
    assert_eq!(
        error.downcast_ref::<crate::worktree_management::WorktreeError>(),
        Some(&crate::worktree_management::WorktreeError::Busy),
        "actual admission error: {error:#}"
    );
}
#[tokio::test]
async fn n13_reverse_new_turn_resume_restore_checkpoint_and_registration_are_atomic() {
    use crate::worktree_management::{WorktreeCause, WorktreeState};
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = n13_completed_fixture(temp.path()).await;
    let (_, operation) = engine.reserve_task_worker(&task_id).unwrap();
    operation
        .run(async {
            let task = engine.store.task(&task_id).unwrap();
            let context = engine.store.run_context(&task.run_id).unwrap();
            let (request, inventory, plan) = n13_plan_for_engine(&engine, temp.path()).await;
            let (receipt, _) = engine
                .store
                .claim_worktree(
                    &request,
                    &inventory,
                    std::slice::from_ref(&request.source_project_id),
                    &plan,
                )
                .unwrap();
            let before = n10_rows(&engine.store.db.lock().unwrap());
            n13_busy(engine.store.resume_task(&task, "blocked resume"));
            n13_busy(
                engine
                    .start_in_project(StartInProject {
                        project_id: request.source_project_id.clone(),
                        expected_config_revision: request.expected_config_revision,
                        request: RunRequest {
                            title: "blocked new".into(),
                            kind: SessionKind::Chat,
                            tasks: vec![task.spec.clone()],
                        },
                        idempotency_key: id(),
                    })
                    .await,
            );
            n13_busy(
                engine
                    .send_chat_turn(SendChatTurn {
                        session_id: context.session.id,
                        agent_id: context.tasks[0].agent_id.clone(),
                        expected_last_turn_id: context.latest_turn.id,
                        message: "blocked append".into(),
                        idempotency_key: id(),
                    })
                    .await,
            );
            n13_busy(engine.restore(&task_id).await);
            n13_busy(
                engine
                    .create_checkpoint(&task_id, "blocked-checkpoint")
                    .await,
            );
            let newroot = temp.path().join("new-project");
            std::fs::create_dir(&newroot).unwrap();
            n13_busy(engine.store.register_project(RegisterProject {
                name: "New".into(),
                root_path: newroot.to_string_lossy().into(),
                idempotency_key: id(),
            }));
            let original = engine.store.project(&request.source_project_id).unwrap();
            let key: String = engine
                .store
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT key FROM project_registration_keys WHERE project_id=?1",
                    [&original.id.0],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                engine
                    .store
                    .register_project(RegisterProject {
                        name: original.name.clone(),
                        root_path: engine.store.settings().unwrap().unwrap().workspace,
                        idempotency_key: key
                    })
                    .unwrap()
                    .id,
                original.id
            );
            assert_eq!(n10_rows(&engine.store.db.lock().unwrap()), before);
            engine
                .store
                .finish_worktree(
                    &receipt,
                    None,
                    Some((WorktreeState::Rejected, WorktreeCause::Conflict)),
                )
                .unwrap();
            assert!(
                engine
                    .create_checkpoint(&task_id, "after-release")
                    .await
                    .is_ok()
            );
        })
        .await;
}
#[tokio::test]
async fn n13_reverse_approval_creation_approval_claim_and_deny_preserve_old_priority() {
    use crate::worktree_management::{WorktreeCause, WorktreeState};
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = n13_completed_fixture(temp.path()).await;
    let (_, operation) = engine.reserve_task_worker(&task_id).unwrap();
    operation.run(async {
    let mut task = engine.store.task(&task_id).unwrap();
    let args = json!({"path":"src/a.txt","content":"late"});
    let calls:Vec<_>=(0..4).map(|i|json!({"id":format!("late-{i}"),"type":"function","function":{"name":"write_file","arguments":args.to_string()}})).collect();
    task.messages
        .push(json!({"role":"assistant","tool_calls":calls}));
    engine.store.save_task(&task).unwrap();
    let approved = engine
        .store
        .ensure_approval(&task, "late-0", "write_file", &args)
        .unwrap()
        .0;
    engine
        .store
        .decide_approval(&approved.id, true, "fixture")
        .unwrap();
    let pending = engine
        .store
        .ensure_approval(&task, "late-1", "write_file", &args)
        .unwrap()
        .0;
    let denied = engine
        .store
        .ensure_approval(&task, "late-2", "write_file", &args)
        .unwrap()
        .0;
    let (request, inventory, plan) = n13_plan_for_engine(&engine, temp.path()).await;
    let (receipt, _) = engine
        .store
        .claim_worktree(
            &request,
            &inventory,
            std::slice::from_ref(&request.source_project_id),
            &plan,
        )
        .unwrap();
    let before = n10_rows(&engine.store.db.lock().unwrap());
    assert!(
        !engine
            .store
            .ensure_approval(&task, "late-1", "write_file", &args)
            .unwrap()
            .1
    );
    n13_busy(
        engine
            .store
            .ensure_approval(&task, "late-3", "write_file", &args),
    );
    n13_busy(engine.store.decide_approval(&pending.id, true, "fixture"));
    let mut wrong = args.clone();
    wrong["content"] = json!("wrong");
    assert!(matches!(
        engine
            .store
            .claim_approval(&task, "late-0", "write_file", &wrong)
            .unwrap_err()
            .downcast_ref::<approval::ApprovalError>(),
        Some(approval::ApprovalError::BindingConflict { .. })
    ));
    assert_eq!(n10_rows(&engine.store.db.lock().unwrap()), before);
    // Controlled persistence injection reaches the last claim guard without violating
    // the normal completed -> running prohibition through save_task/resume.
    task.status = "running".into();
    engine
        .store
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE tasks SET value=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&task).unwrap(), task.id],
        )
        .unwrap();
    n13_busy(
        engine
            .store
            .claim_approval(&task, "late-0", "write_file", &args),
    );
    assert_eq!(
        engine.store.approval(&approved.id).unwrap().execution_state,
        approval::ExecutionState::NotStarted
    );
    assert_eq!(
        engine
            .store
            .decide_approval(&denied.id, false, "fixture")
            .unwrap()
            .status,
        approval::ApprovalStatus::Denied
    );
    engine
        .store
        .finish_worktree(
            &receipt,
            None,
            Some((WorktreeState::Rejected, WorktreeCause::Conflict)),
        )
        .unwrap();
    assert_eq!(
        engine.store.approval(&denied.id).unwrap().status,
        approval::ApprovalStatus::Denied
    );
    }).await;
}
#[tokio::test]
async fn n13_reverse_checkpoint_replay_and_restore_seal_precede_reservation() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = n13_completed_fixture(temp.path()).await;
    let (_, operation) = engine.reserve_task_worker(&task_id).unwrap();
    operation
        .run(async {
            let checkpoint = engine
                .create_checkpoint(&task_id, "existing")
                .await
                .unwrap();
            let (request, inventory, plan) = n13_plan_for_engine(&engine, temp.path()).await;
            engine
                .store
                .claim_worktree(
                    &request,
                    &inventory,
                    std::slice::from_ref(&request.source_project_id),
                    &plan,
                )
                .unwrap();
            let replay = engine
                .create_checkpoint(&task_id, "existing")
                .await
                .unwrap();
            assert!(replay.replayed);
            assert_eq!(replay.checkpoint, checkpoint.checkpoint);
            assert_eq!(
                engine
                    .store
                    .resume_task(&engine.store.task(&task_id).unwrap(), "sealed")
                    .unwrap_err()
                    .downcast_ref::<crate::checkpoint::CheckpointError>(),
                Some(&crate::checkpoint::CheckpointError::Conflict)
            );
            assert_eq!(
                engine
                    .create_checkpoint(&task_id, "different")
                    .await
                    .unwrap_err()
                    .downcast_ref::<crate::checkpoint::CheckpointError>(),
                Some(&crate::checkpoint::CheckpointError::Conflict)
            );
        })
        .await;
}

#[tokio::test]
async fn n13_reverse_command_ack_replay_and_blocked_ack_are_atomic() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("baseline.txt"), "baseline").unwrap();
    let (engine, task_id, calls) = shell_engine_at(
        &source,
        "Write-Output 'unused'",
        true,
        true,
        &temp.path().join("host.db"),
    )
    .await;
    let (_, operation) = engine.reserve_task_worker(&task_id).unwrap();
    operation.run(async {
    let initial = n10_pending(&engine, &task_id).await;
    engine
        .decide_approval(&initial.id, false, Some("fixture"))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while engine.is_busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut task = engine.store.task(&task_id).unwrap();
    assert_eq!(task.status, "completed");
    let args = json!({"command":"Write-Output 'unused'","timeout_seconds":10});
    let pending:Vec<_>=(0..3).map(|i|json!({"id":format!("ack-{i}"),"type":"function","function":{"name":"run_command","arguments":args.to_string()}})).collect();
    task.messages
        .push(json!({"role":"assistant","tool_calls":pending}));
    engine.store.save_task(&task).unwrap();
    let acked = engine
        .store
        .ensure_approval(&task, "ack-0", "run_command", &args)
        .unwrap()
        .0;
    let acked = engine
        .store
        .decide_approval_with_network_ack(&acked.id, true, "fixture", true)
        .unwrap();
    let pending = engine
        .store
        .ensure_approval(&task, "ack-1", "run_command", &args)
        .unwrap()
        .0;
    let (request, inventory, plan) = n13_plan_for_engine(&engine, temp.path()).await;
    engine
        .store
        .claim_worktree(
            &request,
            &inventory,
            std::slice::from_ref(&request.source_project_id),
            &plan,
        )
        .unwrap();
    let before = n10_rows(&engine.store.db.lock().unwrap());
    let provider_calls = calls.load(Ordering::SeqCst);
    assert_eq!(
        engine
            .store
            .decide_approval_with_network_ack(&acked.id, true, "fixture", true)
            .unwrap(),
        acked
    );
    n13_busy(
        engine
            .store
            .decide_approval_with_network_ack(&pending.id, true, "fixture", true),
    );
    n13_busy(
        engine
            .store
            .ensure_approval(&task, "ack-2", "run_command", &args),
    );
    assert_eq!(n10_rows(&engine.store.db.lock().unwrap()), before);
    assert_eq!(calls.load(Ordering::SeqCst), provider_calls);
    engine
        .store
        .decide_approval(&pending.id, false, "fixture")
        .unwrap();
    }).await;
}
#[tokio::test]
async fn n13_reverse_existing_restore_replay_and_sealed_resume_precede_reservation() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task_id) = n13_completed_fixture(temp.path()).await;
    let (_, operation) = engine.reserve_task_worker(&task_id).unwrap();
    operation
        .run(async {
            let restored = engine.restore(&task_id).await.unwrap();
            let (request, inventory, plan) = n13_plan_for_engine(&engine, temp.path()).await;
            engine
                .store
                .claim_worktree(
                    &request,
                    &inventory,
                    std::slice::from_ref(&request.source_project_id),
                    &plan,
                )
                .unwrap();
            let before = n10_rows(&engine.store.db.lock().unwrap());
            assert_eq!(engine.store.claim_restore(&task_id, &[]).unwrap(), restored);
            assert_eq!(engine.restore(&task_id).await.unwrap(), restored);
            assert_eq!(
                engine
                    .store
                    .resume_task(&engine.store.task(&task_id).unwrap(), "sealed")
                    .unwrap_err()
                    .downcast_ref::<crate::store::ResumeError>(),
                Some(&crate::store::ResumeError::Sealed)
            );
            assert_eq!(n10_rows(&engine.store.db.lock().unwrap()), before);
        })
        .await;
}
#[tokio::test]
async fn n13_reverse_actual_running_task_wins_then_cancel_allows_claim() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let (engine, task_id) = two_write_engine_paths_at(
        &source,
        SessionKind::Chat,
        false,
        &temp.path().join("host.db"),
    )
    .await;
    let (_, operation) = engine.reserve_task_worker(&task_id).unwrap();
    operation
        .run(async {
            let _pending = n10_pending(&engine, &task_id).await;
            let (request, inventory, plan) = n13_plan_for_engine(&engine, temp.path()).await;
            n13_busy(engine.store.claim_worktree(
                &request,
                &inventory,
                std::slice::from_ref(&request.source_project_id),
                &plan,
            ));
            engine.cancel(&task_id).unwrap();
            tokio::time::timeout(Duration::from_secs(8), async {
                while engine.is_busy() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(engine.store.task(&task_id).unwrap().status, "cancelled");
            let receipt = engine
                .store
                .claim_worktree(
                    &request,
                    &inventory,
                    std::slice::from_ref(&request.source_project_id),
                    &plan,
                )
                .unwrap()
                .0;
            assert_eq!(
                receipt.state,
                crate::worktree_management::WorktreeState::Claimed
            );
            assert!(!plan.target_path().exists());
        })
        .await;
}
async fn two_write_engine(root: &Path) -> (Arc<Engine>, String) {
    two_write_engine_kind(root, SessionKind::Team).await
}

async fn two_write_engine_kind(root: &Path, kind: SessionKind) -> (Arc<Engine>, String) {
    two_write_engine_paths(root, kind, false).await
}

async fn two_write_engine_paths(
    root: &Path,
    kind: SessionKind,
    multiple_paths: bool,
) -> (Arc<Engine>, String) {
    two_write_engine_paths_at(root, kind, multiple_paths, &root.join("phase.db")).await
}
async fn two_write_engine_paths_at(
    root: &Path,
    kind: SessionKind,
    multiple_paths: bool,
    db_path: &Path,
) -> (Arc<Engine>, String) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.txt"), "before-a").unwrap();
    std::fs::write(root.join("src/b.txt"), "before-b").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(two_write_reply))
        .with_state(Arc::new(AtomicUsize::new(if multiple_paths {
            100
        } else {
            0
        })));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = Arc::new(Store::open(db_path).unwrap());
    store
        .save_settings(&Settings {
            workspace: root.to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "route".into(),
                base_url: base,
                model: "mock".into(),
                max_tokens: 64,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "phase-test-key").unwrap();
    authorize_fixture(&store);
    let engine = Engine::new(store, 2).unwrap();
    let task = engine
        .start(RunRequest {
            title: "two".into(),
            kind,
            tasks: vec![TaskSpec {
                name: "writer".into(),
                role: "writer".into(),
                route_id: "route".into(),
                prompt: "write".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 5,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    (engine, task.id)
}

async fn finish_two_writes(engine: &Arc<Engine>, task_id: &str, fail_at: usize) {
    for index in 0..2 {
        let approval = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(value) = engine
                    .store
                    .pending_approvals_for_task(task_id)
                    .unwrap()
                    .into_iter()
                    .next()
                {
                    break value;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if index == fail_at {
            crash_probe::fail_recheck_once(task_id.to_owned());
        }
        engine
            .decide_approval(&approval.id, true, Some("test"))
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if !engine.is_busy() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn child(phase: &'static str, root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.txt"), "before").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/chat/completions", post(provider_reply))
        .with_state(Arc::new(AtomicUsize::new(0)));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = Arc::new(Store::open(&root.join("phase.db")).unwrap());
    store
        .save_settings(&Settings {
            workspace: root.to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "route".into(),
                base_url: base,
                model: "mock".into(),
                max_tokens: 64,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })
        .unwrap();
    store.put_secret("route", "phase-test-key").unwrap();
    authorize_fixture(&store);
    let engine = Engine::new(store.clone(), 2).unwrap();
    let task = engine
        .start(RunRequest {
            title: "phase".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "writer".into(),
                role: "writer".into(),
                route_id: "route".into(),
                prompt: "write".into(),
                depends_on: vec![],
                write_scopes: vec!["src".into()],
                tools: true,
                allow_commands: false,
                max_rounds: 4,
            }],
        })
        .await
        .unwrap()
        .tasks
        .remove(0);
    let approval = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(value) = store
                .pending_approvals_for_task(&task.id)
                .unwrap()
                .into_iter()
                .next()
            {
                break value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    if phase == "write_pre_fs" {
        crash_probe::arm(task.id.clone(), phase, root.join("phase.signal"));
    } else if phase == "recheck_read_error" {
        crash_probe::fail_recheck_once(task.id.clone());
    }
    engine
        .decide_approval(&approval.id, true, Some("test"))
        .unwrap();
    if phase == "restore_pre_fs" {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if !engine.is_busy() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("src/a.txt")).unwrap(),
            "after"
        );
        crash_probe::arm(task.id.clone(), phase, root.join("phase.signal"));
        let _ = engine.restore(&task.id).await;
    } else if phase == "recheck_read_error" {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if !engine.is_busy() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let changes = store.workspace_changes_for_task(&task.id).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].state, ChangeState::Failed);
        assert_eq!(
            std::fs::read_to_string(root.join("src/a.txt")).unwrap(),
            "before"
        );
        assert!(
            engine
                .store
                .task(&task.id)
                .unwrap()
                .messages
                .iter()
                .any(|message| message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .is_some_and(|value| value.contains("error")))
        );
        drop(engine);
        store.recover().unwrap();
        assert_eq!(
            store.workspace_changes_for_task(&task.id).unwrap()[0].state,
            ChangeState::Failed
        );
    } else {
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

fn parent(phase: &'static str, test_name: &str) {
    if std::env::var("NHC_WORKSPACE_PHASE_CHILD").as_deref() == Ok(phase) {
        let root = std::path::PathBuf::from(std::env::var("NHC_WORKSPACE_PHASE_ROOT").unwrap());
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(child(phase, &root));
        panic!("phase child returned before termination");
    }
    let temp = tempfile::tempdir().unwrap();
    let mut process = ChildGuard(Some(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env("NHC_WORKSPACE_PHASE_CHILD", phase)
            .env("NHC_WORKSPACE_PHASE_ROOT", temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let signal = temp.path().join("phase.signal");
    let deadline = Instant::now() + Duration::from_secs(12);
    while !signal.exists() && Instant::now() < deadline {
        assert!(
            process.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "child exited before phase signal"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(signal.exists(), "child did not reach {phase}");
    let signal_text = std::fs::read_to_string(&signal).unwrap();
    let mut lines = signal_text.lines();
    let task_id = lines
        .next()
        .filter(|value| !value.is_empty())
        .unwrap()
        .to_owned();
    let record_id = lines
        .next()
        .filter(|value| !value.is_empty())
        .unwrap()
        .to_owned();
    let db = rusqlite::Connection::open(temp.path().join("phase.db")).unwrap();
    let (query, expected) = if phase == "write_pre_fs" {
        (
            "SELECT state FROM workspace_changes WHERE id=?1",
            "prepared",
        )
    } else {
        (
            "SELECT status FROM workspace_restores WHERE id=?1",
            "claimed",
        )
    };
    let state: String = db.query_row(query, [&record_id], |row| row.get(0)).unwrap();
    assert_eq!(state, expected);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        if phase == "write_pre_fs" {
            "before"
        } else {
            "after"
        }
    );
    let status = process.stop();
    assert!(
        !status.success(),
        "phase child exited successfully before kill"
    );
    let store = Arc::new(Store::open(&temp.path().join("phase.db")).unwrap());
    store.recover().unwrap();
    let engine = Engine::new(store.clone(), 2).unwrap();
    let bytes = std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap();
    if phase == "write_pre_fs" {
        assert_eq!(bytes, "before");
        let records = store.workspace_changes_for_task(&task_id).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, record_id);
        assert_eq!(records[0].state, ChangeState::Unknown);
        assert!(
            engine
                .changes(&task_id)
                .unwrap()
                .iter()
                .all(|item| !item.restorable)
        );
    } else {
        assert_eq!(bytes, "after");
        let receipt = engine.latest_restore(&task_id).unwrap().unwrap();
        assert_eq!(receipt.restore_id.as_deref(), Some(record_id.as_str()));
        assert_eq!(receipt.status, RestoreStatus::Unknown);
        assert_eq!(receipt.outcomes[0].status, RestoreStatus::Unknown);
        let changes = engine.changes(&task_id).unwrap();
        assert_eq!(changes[0].restore_state, "unknown");
        assert!(!changes[0].restorable);
    }
}

fn checkpoint_crash(phase: &'static str, test_name: &str) {
    if std::env::var("NHC_CHECKPOINT_CHILD").as_deref() == Ok(phase) {
        let root = std::path::PathBuf::from(std::env::var("NHC_CHECKPOINT_ROOT").unwrap());
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (engine, task) = two_write_engine_kind(&root, SessionKind::Chat).await;
            finish_two_writes(&engine, &task, usize::MAX).await;
            let cp = engine.create_checkpoint(&task, "crash-key").await.unwrap();
            crash_probe::arm(task, phase, root.join("phase.signal"));
            let _ = engine
                .restore_checkpoint(&cp.checkpoint.checkpoint_id)
                .await;
        });
        panic!("child returned before stop");
    }
    let temp = tempfile::tempdir().unwrap();
    let mut process = ChildGuard(Some(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env("NHC_CHECKPOINT_CHILD", phase)
            .env("NHC_CHECKPOINT_ROOT", temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(15);
    let signal = temp.path().join("phase.signal");
    while !signal.exists() && Instant::now() < deadline {
        assert!(
            process.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "checkpoint child exited early"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(signal.exists(), "checkpoint child never reached {phase}");
    let db = rusqlite::Connection::open(temp.path().join("phase.db")).unwrap();
    let (checkpoint_id, task_id): (String, String) = db
        .query_row("SELECT id,task_id FROM checkpoints", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM workspace_restores", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count,
        if phase == "checkpoint_pre_claim" {
            0
        } else {
            1
        }
    );
    if count == 1 {
        assert_eq!(
            db.query_row("SELECT status FROM workspace_restores", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "claimed"
        );
    }
    let before_stop = std::fs::read(temp.path().join("src/a.txt")).unwrap();
    assert_eq!(
        before_stop,
        if phase == "restore_post_fs" {
            b"before-a".as_slice()
        } else {
            b"after-1".as_slice()
        }
    );
    assert!(!process.stop().success());
    let store = Arc::new(Store::open(&temp.path().join("phase.db")).unwrap());
    let (engine, _) = Engine::new_recovered(store.clone(), 2).unwrap();
    assert_eq!(engine.checkpoint(&checkpoint_id).unwrap().task_id, task_id);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        if phase == "checkpoint_pre_claim" {
            assert!(engine.latest_restore(&task_id).unwrap().is_none());
            assert_eq!(
                engine
                    .restore_checkpoint(&checkpoint_id)
                    .await
                    .unwrap()
                    .status,
                RestoreStatus::Complete
            );
        } else {
            let receipt = engine.latest_restore(&task_id).unwrap().unwrap();
            assert_eq!(receipt.status, RestoreStatus::Unknown);
            assert!(matches!(
                engine
                    .restore_checkpoint(&checkpoint_id)
                    .await
                    .unwrap_err()
                    .downcast_ref(),
                Some(WorkspaceChangeError::Unknown { .. })
            ));
            assert_eq!(
                std::fs::read(temp.path().join("src/a.txt")).unwrap(),
                before_stop
            );
            assert!(engine.resume(&task_id, "must not retry").await.is_err());
            let task = store.task(&task_id).unwrap();
            let context = store.run_context(&task.run_id).unwrap();
            let error = engine
                .send_chat_turn(SendChatTurn {
                    session_id: context.session.id,
                    agent_id: context.tasks[0].agent_id.clone(),
                    expected_last_turn_id: context.latest_turn.id,
                    message: "blocked by unknown".into(),
                    idempotency_key: "after-checkpoint-crash".into(),
                })
                .await
                .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<ChatTurnError>(),
                Some(ChatTurnError::UnresolvedEffects)
            ));
        }
    });
}

#[test]
fn checkpoint_before_claim_real_process_stop() {
    checkpoint_crash(
        "checkpoint_pre_claim",
        "engine::workspace_phase_tests::checkpoint_before_claim_real_process_stop",
    );
}
#[test]
fn checkpoint_after_claim_real_process_stop() {
    checkpoint_crash(
        "restore_pre_fs",
        "engine::workspace_phase_tests::checkpoint_after_claim_real_process_stop",
    );
}
#[test]
fn checkpoint_after_effect_real_process_stop() {
    checkpoint_crash(
        "restore_post_fs",
        "engine::workspace_phase_tests::checkpoint_after_effect_real_process_stop",
    );
}

#[tokio::test]
async fn checkpoint_creation_and_both_restore_routes_share_real_gate() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task) = two_write_engine(temp.path()).await;
    let request = crate::git_diff::GitDiffRequest {
        path: "src/a.txt".into(),
        view: crate::git_diff::GitDiffView::Head,
    };
    assert!(matches!(
        engine
            .git_diff(&task, request.clone())
            .await
            .unwrap_err()
            .downcast_ref::<WorkspaceChangeError>(),
        Some(WorkspaceChangeError::Active)
    ));
    finish_two_writes(&engine, &task, usize::MAX).await;
    let guard = engine.gate.lock().await;
    let mut first = std::pin::pin!(engine.create_checkpoint(&task, "gate"));
    let mut second = std::pin::pin!(engine.create_checkpoint(&task, "gate"));
    assert_gate_pending(first.as_mut()).await;
    assert_gate_pending(second.as_mut()).await;
    assert!(engine.store.checkpoint_for_task(&task).unwrap().is_none());
    drop(guard);
    let a = first.await.unwrap();
    let b = second.await.unwrap();
    assert!(!a.replayed && b.replayed);
    assert_eq!(a.checkpoint, b.checkpoint);
    let guard = engine.gate.lock().await;
    let mut checkpoint = std::pin::pin!(engine.restore_checkpoint(&a.checkpoint.checkpoint_id));
    let mut compatibility = std::pin::pin!(engine.restore(&task));
    let mut diff = std::pin::pin!(engine.git_diff(&task, request));
    assert_gate_pending(checkpoint.as_mut()).await;
    assert_gate_pending(compatibility.as_mut()).await;
    assert_gate_pending(diff.as_mut()).await;
    assert!(engine.latest_restore(&task).unwrap().is_none());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "after-1"
    );
    drop(guard);
    assert_eq!(checkpoint.await.unwrap(), compatibility.await.unwrap());
    assert!(matches!(
        diff.await
            .unwrap_err()
            .downcast_ref::<crate::git_diff::GitDiffError>(),
        Some(crate::git_diff::GitDiffError::Unavailable)
    ));
}

#[tokio::test]
async fn checkpoint_partial_failure_keeps_single_receipt_and_blocks_new_turn() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, task) = two_write_engine_paths(temp.path(), SessionKind::Chat, true).await;
    finish_two_writes(&engine, &task, usize::MAX).await;
    let cp = engine
        .create_checkpoint(&task, "partial")
        .await
        .unwrap()
        .checkpoint;
    assert_eq!(cp.entries.len(), 2);
    crash_probe::fail_restore_path(task.clone(), "src/b.txt".into());
    let error = engine
        .restore_checkpoint(&cp.checkpoint_id)
        .await
        .unwrap_err();
    let Some(WorkspaceChangeError::Conflict {
        receipt: Some(receipt),
    }) = error.downcast_ref::<WorkspaceChangeError>()
    else {
        panic!("unexpected {error}");
    };
    assert_eq!(receipt.status, RestoreStatus::Partial);
    assert_eq!(receipt.restored, 1);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
        "before-a"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("src/b.txt")).unwrap(),
        "after-1"
    );
    assert_eq!(
        engine.latest_restore(&task).unwrap().as_ref(),
        Some(receipt)
    );
    let replay = engine.restore(&task).await.unwrap_err();
    assert!(
        matches!(replay.downcast_ref::<WorkspaceChangeError>(),Some(WorkspaceChangeError::Conflict{receipt:Some(other)}) if other==receipt)
    );
    let context = engine
        .store
        .run_context(&engine.store.task(&task).unwrap().run_id)
        .unwrap();
    assert!(matches!(
        engine
            .send_chat_turn(SendChatTurn {
                session_id: context.session.id,
                agent_id: context.tasks[0].agent_id.clone(),
                expected_last_turn_id: context.latest_turn.id,
                message: "no new turn".into(),
                idempotency_key: "after-partial".into()
            })
            .await
            .unwrap_err()
            .downcast_ref::<ChatTurnError>(),
        Some(ChatTurnError::UnresolvedEffects)
    ));
}
#[test]
fn write_prepare_before_fs_real_process_stop() {
    parent(
        "write_pre_fs",
        "engine::workspace_phase_tests::write_prepare_before_fs_real_process_stop",
    );
}

#[test]
fn restore_claim_before_fs_real_process_stop() {
    parent(
        "restore_pre_fs",
        "engine::workspace_phase_tests::restore_claim_before_fs_real_process_stop",
    );
}

#[test]
fn recheck_io_failure_after_prepare_is_failed_without_file_effect() {
    let temp = tempfile::tempdir().unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(child("recheck_read_error", temp.path()));
}

#[test]
fn failed_prepare_does_not_pollute_later_or_earlier_success() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for (fail_at, expected) in [(0usize, "after-1"), (1usize, "after-0")] {
            let temp = tempfile::tempdir().unwrap();
            let (engine, task_id) = two_write_engine(temp.path()).await;
            finish_two_writes(&engine, &task_id, fail_at).await;
            let records = engine.store.workspace_changes_for_task(&task_id).unwrap();
            assert_eq!(records.len(), 2);
            assert_eq!(records[fail_at].state, ChangeState::Failed);
            assert_eq!(records[1 - fail_at].state, ChangeState::Finished);
            assert_eq!(
                std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
                expected
            );
            let changes = engine.changes(&task_id).unwrap();
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].state, ChangeState::Finished);
            assert!(changes[0].restorable);
            assert_eq!(
                engine.restore(&task_id).await.unwrap().status,
                RestoreStatus::Complete
            );
            assert_eq!(
                std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
                "before-a"
            );
        }
    });
}

#[test]
fn sealed_pending_resume_and_failed_commit_leave_no_deferred_input() {
    for restore_first in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (old_engine, task_id) = runtime.block_on(async {
            let (engine, id) = two_write_engine(temp.path()).await;
            let mut seen = 0;
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    if let Some(approval) = engine
                        .store
                        .pending_approvals_for_task(&id)
                        .unwrap()
                        .first()
                    {
                        if seen == 0 {
                            engine.decide_approval(&approval.id, true, None).unwrap();
                            seen = 1;
                        } else if approval.tool_call_id == "phase-1" {
                            break;
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            (engine, id)
        });
        runtime.shutdown_timeout(Duration::from_secs(2));
        let store = old_engine.store.clone();
        assert!(store.recover().is_err());
        drop(old_engine);
        let (engine, _) = Engine::new_recovered(store, 2).unwrap();
        let task = engine.store.task(&task_id).unwrap();
        assert!(!crate::tool_call_application::unanswered_tool_calls(&task.messages).is_empty());
        assert_eq!(
            engine
                .store
                .pending_approvals_for_task(&task_id)
                .unwrap()
                .len(),
            1
        );
        if restore_first {
            let changes = engine.store.workspace_changes_for_task(&task_id).unwrap();
            assert_eq!(changes.len(), 1);
            engine.store.claim_restore(&task_id, &changes).unwrap();
        } else {
            engine.store.db.lock().unwrap().execute_batch("CREATE TRIGGER reject_resume BEFORE INSERT ON events WHEN NEW.kind='resume' BEGIN SELECT RAISE(ABORT,'resume fixture fault'); END;").unwrap();
        }
        let before = resume_business_snapshot(&engine.store);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(engine.resume(&task_id, "deferred input must not remain"))
            .unwrap_err();
        if restore_first {
            assert_eq!(
                error.downcast_ref::<crate::store::ResumeError>(),
                Some(&crate::store::ResumeError::Sealed)
            );
        } else {
            assert!(error.to_string().contains("resume fixture fault"));
        }
        assert_eq!(resume_business_snapshot(&engine.store), before);
        assert!(
            engine
                .store
                .pending_resume_inputs(&task_id)
                .unwrap()
                .is_empty()
        );
        assert!(!engine.is_busy());
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            b"after-0"
        );
        runtime.shutdown_timeout(Duration::from_secs(2));
    }
}

#[tokio::test]
async fn no_restore_resume_executes_and_checkpoint_keeps_original_error() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, id) = two_write_engine(temp.path()).await;
    finish_two_writes(&engine, &id, usize::MAX).await;
    assert!(engine.store.latest_restore(&id).unwrap().is_none());
    assert_eq!(engine.resume(&id, "write once more").await.unwrap().id, id);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            for approval in engine.store.pending_approvals_for_task(&id).unwrap() {
                engine.decide_approval(&approval.id, true, None).unwrap();
            }
            if !engine.is_busy() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(engine.store.task(&id).unwrap().status, "completed");
    assert_eq!(
        std::fs::read(temp.path().join("src/a.txt")).unwrap(),
        b"after-3"
    );
    let checkpoint = engine.create_checkpoint(&id, "seal").await.unwrap();
    engine
        .restore_checkpoint(&checkpoint.checkpoint.checkpoint_id)
        .await
        .unwrap();
    let before = resume_business_snapshot(&engine.store);
    let mut task = engine.store.task(&id).unwrap();
    task.status = "queued".into();
    assert_eq!(
        engine
            .store
            .resume_task(&task, "blocked")
            .unwrap_err()
            .downcast_ref::<crate::checkpoint::CheckpointError>(),
        Some(&crate::checkpoint::CheckpointError::Conflict)
    );
    assert_eq!(
        engine
            .resume(&id, "blocked")
            .await
            .unwrap_err()
            .downcast_ref::<crate::checkpoint::CheckpointError>(),
        Some(&crate::checkpoint::CheckpointError::Conflict)
    );
    assert_eq!(resume_business_snapshot(&engine.store), before);
}

fn resume_business_snapshot(store: &Store) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let db = store.db.lock().unwrap();
    [
        "tasks",
        "events",
        "turns",
        "turn_tasks",
        "approvals",
        "workspace_changes",
        "workspace_restores",
        "workspace_restore_outcomes",
        "network_calls",
    ]
    .iter()
    .map(|table| {
        let mut query = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = query.column_count();
        query
            .query_map([], |row| {
                (0..columns)
                    .map(|i| row.get(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
    .collect()
}

async fn assert_resume_sealed_without_changes(engine: &Arc<Engine>, id: &str) {
    let before = resume_business_snapshot(&engine.store);
    let mut proposed = engine.store.task(id).unwrap();
    proposed.status = "queued".into();
    proposed
        .messages
        .push(json!({"role":"user","content":"must not persist"}));
    assert_eq!(
        engine
            .store
            .resume_task(&proposed, "must not persist")
            .unwrap_err()
            .downcast_ref::<crate::store::ResumeError>(),
        Some(&crate::store::ResumeError::Sealed)
    );
    assert_eq!(
        engine
            .resume(id, "must not launch")
            .await
            .unwrap_err()
            .downcast_ref::<crate::store::ResumeError>(),
        Some(&crate::store::ResumeError::Sealed)
    );
    assert_eq!(resume_business_snapshot(&engine.store), before);
    assert!(!engine.is_busy());
    assert!(engine.store.pending_resume_inputs(id).unwrap().is_empty());
}

#[test]
fn restore_gate_serializes_competitors_and_seals_resumed_writes() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let (engine, task_id) = two_write_engine(temp.path()).await;
        finish_two_writes(&engine, &task_id, usize::MAX).await;
        let guard = engine.gate.lock().await;
        let mut first = std::pin::pin!(engine.restore(&task_id));
        let mut second = std::pin::pin!(engine.restore(&task_id));
        // Both production futures have reached the held gate and returned Pending.
        assert_gate_pending(first.as_mut()).await;
        assert_gate_pending(second.as_mut()).await;
        assert!(engine.latest_restore(&task_id).unwrap().is_none());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
            "after-1"
        );
        drop(guard);
        let (receipt_a, receipt_b) = tokio::time::timeout(Duration::from_secs(8), async {
            tokio::join!(first, second)
        })
        .await
        .unwrap();
        let receipt_a = receipt_a.unwrap();
        let receipt_b = receipt_b.unwrap();
        assert_eq!(receipt_a, receipt_b);
        assert_eq!(receipt_a.status, RestoreStatus::Complete);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
            "before-a"
        );
        let count: i64 = rusqlite::Connection::open(temp.path().join("phase.db"))
            .unwrap()
            .query_row(
                "SELECT count(*) FROM workspace_restores WHERE task_id=?1",
                [&task_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        assert_resume_sealed_without_changes(&engine, &task_id).await;
        assert_eq!(
            std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
            "before-a"
        );
        assert_eq!(engine.latest_restore(&task_id).unwrap().unwrap(), receipt_a);
        assert_eq!(
            engine
                .store
                .workspace_changes_for_task(&task_id)
                .unwrap()
                .len(),
            2
        );
    });
}

#[test]
fn partial_and_unknown_restore_histories_seal_resumed_writes() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for status in [
            RestoreStatus::Claimed,
            RestoreStatus::Partial,
            RestoreStatus::Unknown,
            RestoreStatus::Conflict,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (engine, task_id) = two_write_engine(temp.path()).await;
            finish_two_writes(&engine, &task_id, usize::MAX).await;
            let records = engine.store.workspace_changes_for_task(&task_id).unwrap();
            let original_ids: Vec<_> = records.iter().map(|record| record.id.clone()).collect();
            let claimed = engine.store.claim_restore(&task_id, &records[..1]).unwrap();
            let restore_id = claimed.restore_id.clone().unwrap();
            let receipt = if status == RestoreStatus::Claimed {
                claimed
            } else {
                engine.store.finish_restore(&restore_id, status).unwrap()
            };
            assert_eq!(receipt.status, status);
            assert_eq!(
                receipt.outcomes[0].status,
                if status == RestoreStatus::Claimed {
                    RestoreStatus::Claimed
                } else {
                    RestoreStatus::Unknown
                }
            );
            assert!(!engine.changes(&task_id).unwrap()[0].restorable);

            assert_resume_sealed_without_changes(&engine, &task_id).await;
            assert_eq!(
                std::fs::read_to_string(temp.path().join("src/a.txt")).unwrap(),
                "after-1"
            );
            let records_after = engine.store.workspace_changes_for_task(&task_id).unwrap();
            assert_eq!(
                records_after
                    .iter()
                    .map(|record| record.id.clone())
                    .collect::<Vec<_>>(),
                original_ids
            );
            assert_eq!(engine.latest_restore(&task_id).unwrap().unwrap(), receipt);
            let operations: i64 = rusqlite::Connection::open(temp.path().join("phase.db"))
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM workspace_restores WHERE task_id=?1",
                    [&task_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(operations, 1);
        }
    });
}

#[test]
fn start_and_resume_wait_for_production_workspace_gate() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let (engine, task_id) = two_write_engine(temp.path()).await;
        finish_two_writes(&engine, &task_id, usize::MAX).await;
        let runs_before = engine.store.runs().unwrap();
        let task_before = serde_json::to_value(engine.store.task(&task_id).unwrap()).unwrap();
        let guard = engine.gate.lock().await;
        let mut restore = std::pin::pin!(engine.restore(&task_id));
        let mut start = std::pin::pin!(engine.start(RunRequest {
            title: "next".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "reader".into(),
                role: "reader".into(),
                route_id: "route".into(),
                prompt: "done".into(),
                depends_on: vec![],
                write_scopes: vec![],
                tools: false,
                allow_commands: false,
                max_rounds: 1
            }]
        }));
        let mut resume = std::pin::pin!(engine.resume(&task_id, "continue"));
        assert_gate_pending(restore.as_mut()).await;
        assert_gate_pending(start.as_mut()).await;
        assert_gate_pending(resume.as_mut()).await;
        assert_eq!(engine.store.runs().unwrap(), runs_before);
        assert_eq!(
            serde_json::to_value(engine.store.task(&task_id).unwrap()).unwrap(),
            task_before
        );
        assert!(!engine.is_busy());
        assert!(engine.latest_restore(&task_id).unwrap().is_none());
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            b"after-1"
        );
        drop(guard);
        let (receipt, new_run, resumed) = tokio::time::timeout(Duration::from_secs(8), async {
            tokio::join!(restore, start, resume)
        })
        .await
        .unwrap();
        assert_eq!(receipt.unwrap().status, RestoreStatus::Complete);
        assert_eq!(new_run.unwrap().tasks.len(), 1);
        assert_eq!(
            resumed
                .unwrap_err()
                .downcast_ref::<crate::store::ResumeError>(),
            Some(&crate::store::ResumeError::Sealed)
        );
        assert_eq!(engine.store.runs().unwrap().len(), runs_before.len() + 1);
        assert_eq!(
            std::fs::read(temp.path().join("src/a.txt")).unwrap(),
            b"before-a"
        );
        engine.cancel_all();
        tokio::time::timeout(Duration::from_secs(8), async {
            while engine.is_busy() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
}
