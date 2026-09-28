#![cfg(windows)]
use peachsh::{
    domain::*,
    engine::Engine,
    host_configuration::*,
    store::{Store, fresh_settings},
};
use rusqlite::{Connection, types::Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
};

fn fixture() -> (tempfile::TempDir, Arc<Store>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("host.db")).unwrap());
    fresh_settings(&store, dir.path()).unwrap();
    (dir, store)
}
fn request(revision: u64) -> ConfigureHostRoute {
    ConfigureHostRoute {
        expected_revision: revision,
        route: RouteInput {
            name: "Local mock".into(),
            base_url: "http://127.0.0.1:9/v1/".into(),
            model: "mock".into(),
            max_tokens: 128,
            parallel_limit: 2,
        },
        credential: CredentialUpdate::Replace {
            value: "fixture-value-n15".into(),
        },
    }
}
fn keep(revision: u64) -> ConfigureHostRoute {
    let mut r = request(revision);
    r.credential = CredentialUpdate::Keep;
    r
}
fn rows(db: &Connection) -> BTreeMap<String, Vec<Vec<Value>>> {
    let names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let values = db
                .prepare(&format!("SELECT rowid,* FROM \"{name}\" ORDER BY rowid"))
                .unwrap()
                .query_map([], |r| {
                    (0..r.as_ref().column_count()).map(|i| r.get(i)).collect()
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (name, values)
        })
        .collect()
}

#[test]
fn fresh_initialization_is_empty_revision_zero_and_cannot_overwrite_history() {
    let (dir, store) = fixture();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    assert_eq!(
        store.host_config().unwrap(),
        HostConfigSnapshot {
            revision: 0,
            routes: vec![]
        }
    );
    assert_eq!(store.settings().unwrap().unwrap().max_concurrency, 3);
    let before = rows(&db);
    fresh_settings(&store, &dir.path().join("another")).unwrap();
    assert_eq!(rows(&db), before);
    assert!(!dir.path().join("another").exists());
    store
        .configure_host_route("route", request(0), "first")
        .unwrap();
    db.execute("DELETE FROM config WHERE id='settings'", [])
        .unwrap();
    let before = rows(&db);
    assert!(fresh_settings(&store, dir.path()).is_err());
    assert_eq!(rows(&db), before);
}
#[test]
fn durable_replay_lookup_and_noop_preserve_original_receipt() {
    let (dir, store) = fixture();
    let first = store
        .configure_host_route("route", request(0), "operation-a")
        .unwrap();
    assert_eq!(first.receipt.revision, 1);
    assert!(first.receipt.changed);
    assert!(!first.replayed);
    assert_eq!(
        first.receipt.route.credential_source,
        CredentialSource::Stored
    );
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let before = rows(&db);
    let again = store
        .configure_host_route("route", request(0), "operation-a")
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.receipt, first.receipt);
    assert_eq!(rows(&db), before);
    let noop = store
        .configure_host_route("route", request(1), "operation-b")
        .unwrap();
    assert!(!noop.receipt.changed);
    assert_eq!(noop.receipt.revision, 1);
    let mut change = keep(1);
    change.route.name = "Updated display".into();
    store
        .configure_host_route("route", change, "operation-c")
        .unwrap();
    assert_eq!(store.host_config().unwrap().revision, 2);
    drop(store);
    for _ in 0..2 {
        let reopened = Store::open(&dir.path().join("host.db")).unwrap();
        assert_eq!(
            reopened
                .host_config_operation("operation-a")
                .unwrap()
                .receipt,
            first.receipt
        );
        assert!(
            reopened
                .host_config_operation("operation-b")
                .unwrap()
                .replayed
        );
        assert_eq!(reopened.host_config().unwrap().revision, 2);
    }
}
#[test]
fn idempotency_conflicts_cover_secret_route_expected_and_mode_without_writes() {
    let (dir, store) = fixture();
    store
        .configure_host_route("route", request(0), "same-key")
        .unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let before = rows(&db);
    let mut secret = request(0);
    secret.credential = CredentialUpdate::Replace {
        value: "different-fixture".into(),
    };
    let mut model = request(0);
    model.route.model = "other".into();
    for (id, r) in [
        ("route", secret),
        ("other-route", request(0)),
        ("route", request(1)),
        ("route", keep(0)),
        ("route", model),
    ] {
        assert_eq!(
            store.configure_host_route(id, r, "same-key").unwrap_err(),
            HostConfigError::IdempotencyConflict
        );
        assert_eq!(rows(&db), before);
    }
    assert_eq!(
        store.host_config_operation("not-seen").unwrap_err(),
        HostConfigError::NotFound
    );
    assert_eq!(rows(&db), before);
}
#[test]
fn two_stores_same_key_converge_and_different_keys_cas_atomically() {
    for same in [true, false] {
        let (dir, a) = fixture();
        let b = Arc::new(Store::open(&dir.path().join("host.db")).unwrap());
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = [a.clone(), b]
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    s.configure_host_route(
                        "route",
                        request(0),
                        if same || i == 0 { "a" } else { "b" },
                    )
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(a.host_config().unwrap().revision, 1);
        if same {
            assert_eq!(
                results[0].as_ref().unwrap().receipt,
                results[1].as_ref().unwrap().receipt
            );
            assert_eq!(
                results
                    .iter()
                    .filter(|r| r.as_ref().unwrap().replayed)
                    .count(),
                1
            );
        } else {
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
            assert_eq!(
                *results.iter().find_map(|r| r.as_ref().err()).unwrap(),
                HostConfigError::RevisionConflict
            );
        }
        let db = Connection::open(dir.path().join("host.db")).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM host_config_requests", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
    }
}
#[test]
fn same_key_different_secret_race_has_one_winner_and_two_noops_can_coexist() {
    let (dir, a) = fixture();
    let b = Arc::new(Store::open(&dir.path().join("host.db")).unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = [a.clone(), b.clone()]
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut r = request(0);
                r.credential = CredentialUpdate::Replace {
                    value: format!("fixture-{i}"),
                };
                barrier.wait();
                s.configure_host_route("route", r, "race")
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        *results.iter().find_map(|r| r.as_ref().err()).unwrap(),
        HostConfigError::IdempotencyConflict
    );
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = [a.clone(), b]
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                s.configure_host_route("route", keep(1), &format!("noop-{i}"))
            })
        })
        .collect();
    barrier.wait();
    for h in handles {
        let result = h.join().unwrap().unwrap();
        assert!(!result.receipt.changed);
        assert_eq!(result.receipt.revision, 1);
    }
}
#[test]
fn strict_request_and_target_validation_is_shared_by_store() {
    let (_dir, store) = fixture();
    for url in [
        "http://localhost/v1",
        "http://example.invalid/v1",
        "http://127.1/v1",
        "https://user:pass@example.invalid",
        "https://example.invalid/a/%2e%2e/x",
        "https://example.invalid/a%2fb",
        "https://example.invalid?q=1",
        "https://example.invalid#fragment",
    ] {
        let mut r = request(0);
        r.route.base_url = url.into();
        assert_eq!(
            store
                .configure_host_route("route", r, "invalid")
                .unwrap_err(),
            HostConfigError::Invalid
        );
    }
    for value in ["", " space", "line\nline", "tab\tvalue"] {
        let mut r = request(0);
        r.credential = CredentialUpdate::Replace {
            value: value.into(),
        };
        assert_eq!(
            store
                .configure_host_route("route", r, "invalid")
                .unwrap_err(),
            HostConfigError::Invalid
        );
    }
    for raw in [
        r#"{"mode":"keep","value":null}"#,
        r#"{"mode":"keep","value":"x"}"#,
        r#"{"mode":"replace"}"#,
        r#"{"mode":"replace","value":"x","extra":1}"#,
    ] {
        assert!(
            serde_json::from_str::<CredentialUpdate>(raw).is_err(),
            "strict tagged union"
        );
    }
    assert_eq!(
        store
            .configure_host_route("newapi-account", request(0), "invalid")
            .unwrap_err(),
        HostConfigError::Invalid
    );
    assert_eq!(store.host_config().unwrap().revision, 0);
    let mut r = request(0);
    r.route.base_url = "http://[::1]:123/v1/".into();
    assert_eq!(
        r.validate("route", "valid").unwrap().base_url,
        "http://[::1]:123/v1"
    );
}
#[test]
fn keep_requires_existing_available_credential_and_same_api_base() {
    let (_dir, store) = fixture();
    assert_eq!(
        store
            .configure_host_route("route", keep(0), "keep-new")
            .unwrap_err(),
        HostConfigError::CredentialRequired
    );
    store
        .configure_host_route("route", request(0), "create")
        .unwrap();
    let mut r = keep(1);
    r.route.base_url = "https://different.invalid/v1".into();
    assert_eq!(
        store
            .configure_host_route("route", r, "keep-moved")
            .unwrap_err(),
        HostConfigError::CredentialRequired
    );
    let mut r = request(1);
    r.route.base_url = "https://different.invalid/v1".into();
    assert_eq!(
        store
            .configure_host_route("route", r, "replace-moved")
            .unwrap()
            .receipt
            .revision,
        2
    );
}
#[test]
fn replacement_clears_env_and_keeps_legacy_ciphertext() {
    let (dir, store) = fixture();
    let name = format!("NHC_N15_{}", id().replace('-', "_"));
    // Unique name belongs solely to this test; no other task reads or writes it.
    unsafe {
        std::env::set_var(&name, "synthetic-environment-value");
    }
    let mut settings = store.settings().unwrap().unwrap();
    settings.routes.push(Route {
        id: "route".into(),
        name: "Local mock".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "mock".into(),
        max_tokens: 128,
        parallel_limit: 2,
        key_env: Some(name.clone()),
    });
    store.save_settings(&settings).unwrap();
    store.put_secret("route", "legacy-fixture").unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let blob: Vec<u8> = db
        .query_row("SELECT value FROM secrets WHERE id='route'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        store.host_config().unwrap().routes[0].credential_source,
        CredentialSource::Environment
    );
    let revision = store.host_config().unwrap().revision;
    assert_eq!(
        store
            .configure_host_route("route", keep(revision), "keep-env")
            .unwrap()
            .receipt
            .route
            .key_env,
        Some(name.clone())
    );
    let revision = store.host_config().unwrap().revision;
    let result = store
        .configure_host_route("route", request(revision), "replace-env")
        .unwrap();
    assert_eq!(result.receipt.route.key_env, None);
    assert_eq!(
        result.receipt.route.credential_source,
        CredentialSource::Stored
    );
    assert_eq!(
        Engine::new(store.clone(), 3)
            .unwrap()
            .key(&store.settings().unwrap().unwrap().routes[0])
            .unwrap(),
        "fixture-value-n15"
    );
    assert_eq!(
        db.query_row("SELECT value FROM secrets WHERE id='route'", [], |r| r
            .get::<_, Vec<u8>>(0))
            .unwrap(),
        blob
    );
    unsafe {
        std::env::remove_var(&name);
    }
}
#[test]
fn unavailable_secret_and_damaged_receipt_fail_closed() {
    let (dir, store) = fixture();
    store
        .configure_host_route("route", request(0), "first")
        .unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    db.execute(
        "UPDATE secrets SET value=X'000102' WHERE id='route::route'",
        [],
    )
    .unwrap();
    assert_eq!(
        store.host_config().unwrap().routes[0].credential_source,
        CredentialSource::Unavailable
    );
    let before = rows(&db);
    assert_eq!(
        store
            .configure_host_route("route", keep(1), "keep-bad")
            .unwrap_err(),
        HostConfigError::Unavailable
    );
    assert_eq!(
        store
            .configure_host_route("route", request(1), "replace-bad")
            .unwrap_err(),
        HostConfigError::Unavailable
    );
    assert_eq!(rows(&db), before);
    db.execute(
        "UPDATE host_config_requests SET request_fingerprint_blob=X'010203'",
        [],
    )
    .unwrap();
    assert_eq!(
        store.host_config_operation("first").unwrap_err(),
        HostConfigError::Unavailable
    );
    assert_eq!(
        store
            .configure_host_route("route", request(0), "first")
            .unwrap_err(),
        HostConfigError::Unavailable
    );
    assert!(Store::open(&dir.path().join("host.db")).is_err());
}
#[test]
fn compatibility_writes_invalidate_cas_without_receipts_and_noop_keeps_revision() {
    let (_dir, store) = fixture();
    let mut settings = store.settings().unwrap().unwrap();
    settings.max_concurrency = 2;
    store.save_settings(&settings).unwrap();
    assert_eq!(store.host_config().unwrap().revision, 1);
    store.save_settings(&settings).unwrap();
    assert_eq!(store.host_config().unwrap().revision, 1);
    assert_eq!(
        store
            .configure_host_route("route", request(0), "stale")
            .unwrap_err(),
        HostConfigError::RevisionConflict
    );
    store
        .put_secret("newapi-account", "account-fixture")
        .unwrap();
    assert_eq!(store.host_config().unwrap().revision, 2);
    store
        .put_secret("newapi-account", "account-fixture")
        .unwrap();
    assert_eq!(store.host_config().unwrap().revision, 2);
    assert_eq!(
        store.host_config_operation("stale").unwrap_err(),
        HostConfigError::NotFound
    );
}
#[test]
fn receipt_insert_failure_and_revision_overflow_roll_back_every_row() {
    let (dir, store) = fixture();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON host_config_requests BEGIN SELECT RAISE(ABORT,'synthetic failure'); END").unwrap();
    let before = rows(&db);
    assert_eq!(
        store
            .configure_host_route("route", request(0), "fail")
            .unwrap_err(),
        HostConfigError::Internal
    );
    assert_eq!(rows(&db), before);
    db.execute_batch(
        "DROP TRIGGER fail_receipt; UPDATE host_config_state SET revision=9223372036854775807",
    )
    .unwrap();
    let before = rows(&db);
    assert_eq!(
        store
            .configure_host_route("route", request(i64::MAX as u64), "overflow")
            .unwrap_err(),
        HostConfigError::Corrupt
    );
    assert_eq!(rows(&db), before);
}
#[test]
fn stored_request_contains_only_sealed_digest_and_public_receipt() {
    use sha2::{Digest, Sha256};
    let (dir, store) = fixture();
    let result = store
        .configure_host_route("route", request(0), "safe-operation")
        .unwrap();
    let db = Connection::open(dir.path().join("host.db")).unwrap();
    let (sealed, receipt): (Vec<u8>, String) = db
        .query_row(
            "SELECT request_fingerprint_blob,receipt FROM host_config_requests",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_ne!(sealed.len(), 32);
    assert_eq!(peachsh::secrets::open(&sealed).unwrap().len(), 32);
    let output = serde_json::to_string(&result).unwrap();
    for text in [&receipt, &output] {
        assert!(!text.contains("fixture-value-n15"));
        assert!(!text.contains("safe-operation"));
        assert!(!text.contains("fingerprint"));
    }
    let raw_digest = Sha256::digest(b"fixture-value-n15");
    drop(db);
    drop(store);
    for suffix in ["host.db", "host.db-wal"] {
        if let Ok(bytes) = std::fs::read(dir.path().join(suffix)) {
            assert!(
                !bytes
                    .windows(b"fixture-value-n15".len())
                    .any(|w| w == b"fixture-value-n15")
            );
            assert!(!bytes.windows(32).any(|w| w == raw_digest.as_slice()));
        }
    }
}
