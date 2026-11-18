//! Explicit fixture setup using production project commands; never adopts an existing Task.
use coda::{domain::*, project_config::*, store::Store};

pub fn configured_project(store: &Store) -> ProjectId {
    let settings = store.settings().unwrap().unwrap();
    let current = store.current_project().unwrap();
    let project = if let Some(id) = current.project_id {
        id
    } else {
        let project = store
            .register_project(RegisterProject {
                name: "fixture".into(),
                root_path: settings.workspace.clone(),
                idempotency_key: uuid::Uuid::new_v4().to_string(),
            })
            .unwrap();
        store
            .select_project(SelectProject {
                project_id: project.id.clone(),
                expected_revision: current.revision,
            })
            .unwrap();
        project.id
    };
    let before = store.project_config(&project).unwrap();
    let config = ProjectConfig {
        allowed_route_ids: settings.routes.iter().map(|r| r.id.clone()).collect(),
        default_route_id: settings.routes.first().map(|r| r.id.clone()),
        tools: true,
        write_scopes: vec!["*".into()],
        allow_commands: true,
    };
    if before.config != config {
        store
            .configure_project(ConfigureProject {
                project_id: project.clone(),
                expected_revision: before.revision,
                config,
            })
            .unwrap();
    }
    project
}

// Migration targets remove the new schema explicitly before exercising historical versions.
#[allow(dead_code)]
pub fn drop_project_schema(db: &rusqlite::Connection) {
    db.execute_batch("DROP TABLE IF EXISTS host_config_requests; DROP TABLE IF EXISTS host_config_state; DELETE FROM schema_migrations WHERE id='host-route-configuration-v1'; DROP TABLE IF EXISTS budget_calls; DROP TABLE IF EXISTS budget_requests; DROP TABLE IF EXISTS project_budgets; DELETE FROM schema_migrations WHERE id='completion-budget-v1'; DROP TABLE IF EXISTS task_project_bindings; DROP TABLE IF EXISTS project_registration_keys; DROP TABLE IF EXISTS project_selection; DROP TABLE IF EXISTS project_root_bindings; DROP TABLE IF EXISTS project_configs; DELETE FROM schema_migrations WHERE id='project-config-root-binding-v1';").unwrap();
}
