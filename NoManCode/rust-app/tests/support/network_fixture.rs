use peachsh::{network_authorization::*, store::Store};
/// Explicit fixture authorization through the real persisted preview/confirm commands.
pub fn authorize_routes(store: &Store) {
    for route in store.settings().unwrap().unwrap().routes {
        let subject = NetworkSubject {
            kind: NetworkSubjectKind::Route,
            id: route.id,
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
