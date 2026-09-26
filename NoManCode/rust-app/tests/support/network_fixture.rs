#[path = "project_fixture.rs"]
pub mod project_fixture;
use peachsh::{network_authorization::*, store::Store};
/// Explicit fixture authorization through the real persisted preview/confirm commands.
pub fn authorize_routes(store: &Store) {
    let project = project_fixture::configured_project(store);
    for route in store.settings().unwrap().unwrap().routes {
        let subject = NetworkSubject {
            kind: NetworkSubjectKind::Route,
            id: route.id,
            project_id: Some(project.clone()),
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
}

// Each integration target compiles this module separately; only migration tests use this helper.
#[allow(dead_code)]
pub fn drop_network_schema(db: &rusqlite::Connection) {
    project_fixture::drop_project_schema(db);
    db.execute_batch("DROP TABLE IF EXISTS command_network_ack;DROP TABLE IF EXISTS network_calls;DROP TABLE IF EXISTS task_network_bindings;DROP TABLE IF EXISTS network_confirmations;DROP TABLE IF EXISTS network_authorizations;DROP TABLE IF EXISTS network_subjects;DROP TABLE IF EXISTS network_events;DELETE FROM schema_migrations WHERE id='network-authorization-repository';").unwrap();
}
