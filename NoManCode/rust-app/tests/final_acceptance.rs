use anyhow::{Result, ensure};
use peachsh::{
    domain::*,
    store::{Store, TurnBundle},
};
use peachsh_protocol::{
    AgentId, LifecycleStatus, ProjectId, SessionId, SessionKind, TaskId, TurnId,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::NamedTempFile;

fn route() -> Route {
    Route {
        id: "main".into(),
        name: "main".into(),
        base_url: "http://127.0.0.1:1".into(),
        model: "test-model".into(),
        max_tokens: 128,
        parallel_limit: 1,
        key_env: None,
    }
}

fn task_spec(name: &str) -> TaskSpec {
    TaskSpec {
        name: name.into(),
        role: "worker".into(),
        route_id: "main".into(),
        prompt: "do the work".into(),
        depends_on: vec![],
        write_scopes: vec![],
        tools: false,
        allow_commands: false,
        max_rounds: 1,
    }
}

fn fixture() -> (
    NamedTempFile,
    Store,
    Project,
    Session,
    Agent,
    Turn,
    TurnTask,
    Task,
) {
    let file = NamedTempFile::new().unwrap();
    let store = Store::open(file.path()).unwrap();
    let project = Project {
        id: ProjectId("project-1".into()),
        name: "Project".into(),
        root_path: "C:/workspace".into(),
        created_at: 1,
        updated_at: 1,
    };
    let session = Session {
        id: SessionId("session-1".into()),
        project_id: project.id.clone(),
        kind: SessionKind::Team,
        title: "Session".into(),
        legacy_run_id: "run-1".into(),
        created_at: 1,
        updated_at: 1,
    };
    let agent = Agent {
        id: AgentId("agent-1".into()),
        session_id: session.id.clone(),
        display_name: "worker".into(),
        role: "worker".into(),
        created_at: 1,
    };
    let turn = Turn {
        id: TurnId("turn-1".into()),
        session_id: session.id.clone(),
        project_id: project.id.clone(),
        status: LifecycleStatus::Queued,
        request_hash: "hash-1".into(),
        idempotency_key: Some("key-1".into()),
        created_at: 1,
        updated_at: 1,
    };
    let task = TurnTask {
        id: TaskId("task-1".into()),
        turn_id: turn.id.clone(),
        session_id: session.id.clone(),
        agent_id: agent.id.clone(),
        legacy_task_id: "legacy-task-1".into(),
        depends_on: vec![],
        status: LifecycleStatus::Queued,
        created_at: 1,
        updated_at: 1,
    };
    let legacy = Task {
        id: "legacy-task-1".into(),
        run_id: "run-1".into(),
        spec: task_spec("worker"),
        route: route(),
        workspace: "C:/workspace".into(),
        status: "queued".into(),
        output: String::new(),
        error: None,
        messages: vec![json!({"role":"user","content":"do the work"})],
        usage: Value::Null,
        created_at: 1,
        updated_at: 1,
    };
    (file, store, project, session, agent, turn, task, legacy)
}

fn commit(
    store: &Store,
    project: &Project,
    session: &Session,
    agent: &Agent,
    turn: &Turn,
    task: &TurnTask,
    legacy: &Task,
) -> Result<Turn> {
    store.commit_turn_bundle(
        TurnBundle {
            project,
            session,
            agents: std::slice::from_ref(agent),
            turn,
            tasks: std::slice::from_ref(task),
            legacy_tasks: std::slice::from_ref(legacy),
        },
        Some("key-1"),
    )
}

#[test]
fn bundle_commits_project_session_agent_turn_and_legacy_projection() -> Result<()> {
    let (_file, store, project, session, agent, turn, task, legacy) = fixture();
    let got = commit(&store, &project, &session, &agent, &turn, &task, &legacy)?;
    ensure!(got == turn);
    ensure!(store.project(&project.id)? == project);
    ensure!(store.session(&session.id)? == session);
    ensure!(store.agent(&agent.id)? == agent);
    ensure!(store.turn(&turn.id)?.status == LifecycleStatus::Queued);
    ensure!(store.task(&legacy.id)?.id == legacy.id);
    Ok(())
}

#[test]
fn invalid_dependency_or_missing_projection_rolls_back_everything() -> Result<()> {
    let (_file, store, project, session, agent, mut turn, mut task, mut legacy) = fixture();
    task.depends_on = vec![TaskId("missing".into())];
    assert!(commit(&store, &project, &session, &agent, &turn, &task, &legacy).is_err());
    ensure!(store.projects()?.is_empty());
    turn.idempotency_key = None;
    task.depends_on.clear();
    legacy.id = "wrong-projection".into();
    assert!(
        store
            .commit_turn_bundle(
                TurnBundle {
                    project: &project,
                    session: &session,
                    agents: std::slice::from_ref(&agent),
                    turn: &turn,
                    tasks: std::slice::from_ref(&task),
                    legacy_tasks: std::slice::from_ref(&legacy)
                },
                None
            )
            .is_err()
    );
    ensure!(store.projects()?.is_empty());
    Ok(())
}

#[test]
fn same_idempotency_key_replays_original_turn_without_new_objects() -> Result<()> {
    let (_file, store, project, session, agent, turn, task, legacy) = fixture();
    commit(&store, &project, &session, &agent, &turn, &task, &legacy)?;
    let mut retry = turn.clone();
    retry.id = TurnId("turn-2".into());
    let mut retry_session = session.clone();
    retry_session.id = SessionId("session-2".into());
    retry.session_id = retry_session.id.clone();
    retry.project_id = project.id.clone();
    let mut retry_agent = agent.clone();
    retry_agent.session_id = retry_session.id.clone();
    let mut retry_task = task.clone();
    retry_task.turn_id = retry.id.clone();
    retry_task.session_id = retry_session.id.clone();
    retry_task.agent_id = retry_agent.id.clone();
    let mut retry_legacy = legacy.clone();
    retry_legacy.run_id = retry_session.legacy_run_id.clone();
    let replay = store.commit_turn_bundle(
        TurnBundle {
            project: &project,
            session: &retry_session,
            agents: std::slice::from_ref(&retry_agent),
            turn: &retry,
            tasks: std::slice::from_ref(&retry_task),
            legacy_tasks: std::slice::from_ref(&retry_legacy),
        },
        Some("key-1"),
    )?;
    ensure!(
        replay.id == turn.id
            && store.projects()?.len() == 1
            && store.turns_for_session(&session.id)?.len() == 1
    );
    Ok(())
}

#[test]
fn legacy_only_replay_returns_complete_tasks() -> Result<()> {
    let (_file, store, _project, _session, _agent, _turn, _task, legacy) = fixture();
    let mut legacy = legacy;
    legacy.run_id = "legacy-run".into();
    let run = Run {
        id: "legacy-run".into(),
        title: "legacy".into(),
        kind: SessionKind::Team,
        created_at: 1,
        tasks: vec![legacy.clone()],
    };
    store.create_run_with_idempotency(&run, "legacy-key", "legacy-hash")?;
    match store
        .idempotency_replay("legacy-key", "legacy-hash")?
        .expect("replay")
    {
        IdempotencyReplay::LegacyRun(run) => {
            ensure!(run.tasks.len() == 1 && run.tasks[0].id == legacy.id)
        }
        _ => anyhow::bail!("expected legacy replay"),
    }
    Ok(())
}

#[test]
fn state_updates_sync_new_projection_and_reject_backwards_transition() -> Result<()> {
    let (_file, store, project, session, agent, turn, task, legacy) = fixture();
    commit(&store, &project, &session, &agent, &turn, &task, &legacy)?;
    let mut running = legacy.clone();
    running.status = "running".into();
    running.updated_at = 2;
    store.save_task(&running)?;
    let mut completed = running.clone();
    completed.status = "completed".into();
    completed.updated_at = 3;
    store.save_task(&completed)?;
    ensure!(store.task(&legacy.id)?.status == "completed");
    let mut backwards = completed.clone();
    backwards.status = "running".into();
    backwards.updated_at = 4;
    assert!(store.save_task(&backwards).is_err());
    ensure!(store.turn(&turn.id)?.status == LifecycleStatus::Completed);
    Ok(())
}

#[test]
fn resume_keeps_task_identity_and_requeues_turn() -> Result<()> {
    let (_file, store, project, session, agent, turn, task, legacy) = fixture();
    commit(&store, &project, &session, &agent, &turn, &task, &legacy)?;
    let mut running = legacy.clone();
    running.status = "running".into();
    running.updated_at = 2;
    store.save_task(&running)?;
    let mut completed = legacy.clone();
    completed.status = "completed".into();
    completed.updated_at = 3;
    store.save_task(&completed)?;
    let mut resumed_input = completed.clone();
    resumed_input.status = "queued".into();
    resumed_input.updated_at = 4;
    store.resume_task(&resumed_input, "continue from the user")?;
    let resumed = store.task(&legacy.id)?;
    ensure!(resumed.id == legacy.id && resumed.route.model == legacy.route.model);
    ensure!(store.turn(&turn.id)?.status == LifecycleStatus::Queued);
    Ok(())
}

#[test]
fn sqlite_trigger_failure_is_atomic() -> Result<()> {
    let (file, store, project, session, agent, turn, task, legacy) = fixture();
    commit(&store, &project, &session, &agent, &turn, &task, &legacy)?;
    let conn = Connection::open(file.path())?;
    conn.execute("CREATE TRIGGER deny_task_events BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'denied'); END", [])?;
    let mut changed = legacy.clone();
    changed.status = "running".into();
    assert!(store.save_task(&changed).is_err());
    ensure!(store.task(&legacy.id)?.status == "queued");
    ensure!(store.turn(&turn.id)?.status == LifecycleStatus::Queued);
    ensure!(store.turn_tasks(&turn.id)?[0].status == LifecycleStatus::Queued);
    Ok(())
}

#[test]
fn inconsistent_initial_state_and_forged_status_event_are_rejected() -> Result<()> {
    let (_file, store, project, session, agent, mut turn, task, legacy) = fixture();
    turn.status = LifecycleStatus::Completed;
    assert!(commit(&store, &project, &session, &agent, &turn, &task, &legacy).is_err());
    ensure!(store.projects()?.is_empty());
    turn.status = LifecycleStatus::Queued;
    commit(&store, &project, &session, &agent, &turn, &task, &legacy)?;
    let count = store.events(&session.legacy_run_id, 0)?.len();
    assert!(
        store
            .event(&legacy.id, "status", json!({"status":"completed"}))
            .is_err()
    );
    for kind in [
        "turn.",
        "turn.unregistered",
        "unknown:",
        "unknown:Bearer private",
    ] {
        assert!(store.event(&legacy.id, kind, json!({})).is_err());
    }
    ensure!(store.events(&session.legacy_run_id, 0)?.len() == count);
    Ok(())
}

#[test]
fn recovery_and_resume_events_roll_back_with_their_states() -> Result<()> {
    let (file, store, project, session, agent, turn, task, legacy) = fixture();
    commit(&store, &project, &session, &agent, &turn, &task, &legacy)?;
    let conn = Connection::open(file.path())?;
    conn.execute_batch("CREATE TRIGGER deny_recovery BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'denied'); END;")?;
    assert!(store.recover().is_err());
    ensure!(store.task(&legacy.id)?.status == "queued");
    ensure!(store.turn(&turn.id)?.status == LifecycleStatus::Queued);
    conn.execute_batch("DROP TRIGGER deny_recovery;")?;
    ensure!(store.recover()? == 2);
    let mut resumed = store.task(&legacy.id)?;
    resumed.status = "queued".into();
    assert!(store.save_task(&resumed).is_err());
    conn.execute_batch("CREATE TRIGGER deny_resume BEFORE INSERT ON events WHEN NEW.kind='resume' BEGIN SELECT RAISE(ABORT, 'denied'); END;")?;
    let count = store.events(&session.legacy_run_id, 0)?.len();
    assert!(store.resume_task(&resumed, "continue").is_err());
    ensure!(store.task(&legacy.id)?.status == "interrupted");
    ensure!(store.turn(&turn.id)?.status == LifecycleStatus::Interrupted);
    ensure!(store.turn_tasks(&turn.id)?[0].status == LifecycleStatus::Interrupted);
    ensure!(store.events(&session.legacy_run_id, 0)?.len() == count);
    Ok(())
}
