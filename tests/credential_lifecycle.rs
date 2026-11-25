//! Run with a logged-in macOS user or a Linux Secret Service session.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use coda::{
    domain::Route,
    secrets,
    store::{Store, fresh_settings},
};
use rusqlite::Connection;
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

// Every ref in these newly created test directories comes from this test. Also clean
// uncommitted items if an assertion unwinds before the normal settings deletion.
struct SmokeDirectory(tempfile::TempDir);
impl SmokeDirectory {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
}
impl Drop for SmokeDirectory {
    fn drop(&mut self) {
        for entry in std::fs::read_dir(self.path()).unwrap().flatten() {
            if entry
                .path()
                .extension()
                .is_none_or(|ext| ext != "credential-journal")
            {
                continue;
            }
            let Ok(db) = Connection::open(entry.path()) else {
                continue;
            };
            let Ok(mut stmt) = db.prepare("SELECT ref FROM owned") else {
                continue;
            };
            for blob in stmt
                .query_map([], |r| r.get::<_, Vec<u8>>(0))
                .unwrap()
                .flatten()
            {
                let _ = secrets::release(&blob);
            }
        }
    }
}

#[test]
#[ignore = "uses the real OS credential store"]
fn keyring_rotation_rollback_reopen_and_delete() {
    let temp = SmokeDirectory::new();
    let path = temp.path().join("coda.sqlite3");
    let store = Store::open(&path).unwrap();
    let mut settings = fresh_settings(&store, temp.path()).unwrap();
    settings.routes.push(Route {
        id: "smoke".into(),
        name: "Smoke".into(),
        base_url: "https://example.invalid/v1".into(),
        model: "test".into(),
        max_tokens: 128,
        parallel_limit: 1,
        key_env: None,
    });
    store.save_settings(&settings).unwrap();
    let first = uuid::Uuid::new_v4().to_string();
    let second = uuid::Uuid::new_v4().to_string();
    let db = Connection::open(&path).unwrap();
    let blob = || {
        db.query_row(
            "SELECT value FROM secrets WHERE id='route::smoke'",
            [],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .unwrap()
    };
    store.put_secret("smoke", &second).unwrap();
    let legacy: Vec<u8> = db
        .query_row("SELECT value FROM secrets WHERE id='smoke'", [], |r| {
            r.get(0)
        })
        .unwrap();
    store.put_secret("route::smoke", &first).unwrap();
    assert!(store.secret("smoke").unwrap().is_none());
    assert!(secrets::open(&legacy).is_err());
    let first_ref = blob();
    assert!(
        !first_ref
            .windows(first.len())
            .any(|v| v == first.as_bytes())
    );
    assert!(secrets::open(&first_ref).unwrap() == first.as_bytes());

    let copy_path = temp.path().join("copied.sqlite3");
    db.execute("VACUUM INTO ?1", [copy_path.to_str().unwrap()])
        .unwrap();
    // A cloned journal cannot grant the copy ownership of the original's references.
    std::fs::copy(
        path.with_added_extension("credential-journal"),
        copy_path.with_added_extension("credential-journal"),
    )
    .unwrap();
    let copied = Store::open(&copy_path).unwrap();
    copied.put_secret("route::smoke", &second).unwrap();
    assert!(store.secret("route::smoke").unwrap().as_deref() == Some(first.as_str()));
    let mut empty = settings.clone();
    empty.routes.clear();
    copied.save_settings(&empty).unwrap();
    drop(copied);

    db.execute_batch("CREATE TRIGGER fail_rotation BEFORE UPDATE ON host_config_state BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;").unwrap();
    assert!(store.put_secret("route::smoke", &second).is_err());
    assert!(blob() == first_ref);
    assert!(store.secret("route::smoke").unwrap().as_deref() == Some(first.as_str()));
    let ledger = Connection::open(path.with_added_extension("credential-journal")).unwrap();
    let active: i64 = ledger
        .query_row("SELECT COUNT(*) FROM owned", [], |r| r.get(0))
        .unwrap();
    assert_eq!(active, 1);
    db.execute_batch("DROP TRIGGER fail_rotation").unwrap();
    // An invalid first pending reference must not prevent retired real items from being removed.
    ledger
        .execute(
            "INSERT INTO owned SELECT owner,X'00' FROM owned LIMIT 1",
            [],
        )
        .unwrap();
    store.put_secret("route::smoke", &second).unwrap();
    let second_ref = blob();
    assert!(first_ref != second_ref);
    assert!(secrets::open(&first_ref).is_err());
    assert_eq!(
        ledger
            .query_row("SELECT COUNT(*) FROM owned WHERE ref=X'00'", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    ledger
        .execute("DELETE FROM owned WHERE ref=X'00'", [])
        .unwrap();
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert!(reopened.secret("route::smoke").unwrap().as_deref() == Some(second.as_str()));
    settings.routes.clear();
    reopened.save_settings(&settings).unwrap();
    assert!(reopened.secret("route::smoke").unwrap().is_none());
    assert!(secrets::open(&second_ref).is_err());
    assert_eq!(
        ledger
            .query_row("SELECT COUNT(*) FROM owned", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    for entry in std::fs::read_dir(temp.path()).unwrap().flatten() {
        if entry.path().is_file() {
            let bytes = std::fs::read(entry.path()).unwrap();
            for secret in [&first, &second] {
                assert!(
                    !bytes
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes())
                );
            }
        }
    }
}

#[test]
#[ignore = "uses the real OS credential store"]
fn cli_stdin_init_keeps_key_out_of_output_and_removes_its_item() {
    let temp = SmokeDirectory::new();
    let key = uuid::Uuid::new_v4().to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_coda"))
        .args([
            "init",
            "--provider",
            "openai",
            "--route",
            "smoke",
            "--base-url",
            "https://example.invalid/v1",
            "--model",
            "test",
            "--api-key-stdin",
            "--data-dir",
        ])
        .arg(temp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{key}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    for stream in [output.stdout, output.stderr] {
        assert!(!stream.windows(key.len()).any(|w| w == key.as_bytes()));
    }
    let path = temp.path().join("coda.sqlite3");
    let db = Connection::open(&path).unwrap();
    let blob: Vec<u8> = db
        .query_row(
            "SELECT value FROM secrets WHERE id='route::smoke'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(blob.starts_with(b"coda:keyring:v1:"));
    // Delete only this fixture's record, then reopen with the same installed executable
    // that created the Keychain item so its normal startup collector performs deletion.
    db.execute("DELETE FROM secrets WHERE id='route::smoke'", [])
        .unwrap();
    let reopened = Command::new(env!("CARGO_BIN_EXE_coda"))
        .args(["--check", "--data-dir"])
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(reopened.status.success());
    assert!(secrets::open(&blob).is_err());
}
