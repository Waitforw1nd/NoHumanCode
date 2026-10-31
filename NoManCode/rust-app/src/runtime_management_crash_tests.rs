use crate::{engine::crash_probe, store::Store};
#[path = "../tests/support/schema_fixture.rs"]
mod schema_fixture;
use rusqlite::Connection;
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn wait(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "child timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn spawn(root: &Path, phase: &str, suffix: &str) -> OwnedChild {
    let stdout = std::fs::File::create(root.join(format!("{suffix}.stdout.log"))).unwrap();
    let stderr = std::fs::File::create(root.join(format!("{suffix}.stderr.log"))).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "runtime_management_crash_tests::migration_kill_and_two_fresh_process_reopens",
            "--nocapture",
        ])
        .env("NHC_RUNTIME17_CHILD", phase)
        .env("NHC_RUNTIME17_ROOT", root)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    std::fs::write(root.join(format!("{suffix}.pid")), child.id().to_string()).unwrap();
    OwnedChild(child)
}
fn preserve_killed_bytes(
    root: &Path,
    signal: &str,
    phase: &str,
    prefix: &str,
    exit: &std::process::ExitStatus,
) {
    use sha2::{Digest, Sha256};
    let pid = std::fs::read_to_string(root.join(format!("{prefix}.pid"))).unwrap();
    let signal = std::fs::read_to_string(root.join(signal)).unwrap();
    let mut metadata =
        format!("phase={phase}\npid={pid}\nwait={exit:?}\nsignal_begin\n{signal}signal_end\n");
    for suffix in ["", "-wal", "-shm"] {
        let from = root.join(format!("runtime.db{suffix}"));
        let to = root.join(format!("raw-runtime.db{suffix}"));
        if from.exists() {
            let bytes = std::fs::read(&from).unwrap();
            std::fs::write(&to, &bytes).unwrap();
            metadata.push_str(&format!(
                "raw-runtime.db{suffix} length={} sha256={:x}\n",
                bytes.len(),
                Sha256::digest(&bytes)
            ));
        } else {
            metadata.push_str(&format!("raw-runtime.db{suffix} absent\n"));
        }
    }
    std::fs::write(root.join("killed-metadata.txt"), metadata).unwrap();
}
fn preserve_queryable_snapshot(db: &Connection, root: &Path, version: i64) {
    let marker: i64 = db
        .query_row(
            "SELECT count(*) FROM schema_migrations WHERE id='builtin-runtime-management-v1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    std::fs::write(
        root.join("observed-metadata.txt"),
        format!("user_version={version}\nruntime17_marker={marker}\n"),
    )
    .unwrap();
    let dest = root.join("queryable-runtime.db");
    db.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])
        .unwrap();
}
#[test]
fn migration_kill_and_two_fresh_process_reopens() {
    if let Ok(phase) = std::env::var("NHC_RUNTIME17_CHILD") {
        let root = std::path::PathBuf::from(std::env::var_os("NHC_RUNTIME17_ROOT").unwrap());
        let path = root.join("runtime.db");
        if phase != "recover" {
            let phase: &'static str = match phase.as_str() {
                "runtime17_before_rebuild" => "runtime17_before_rebuild",
                "runtime17_after_drop" => "runtime17_after_drop",
                "runtime17_after_create" => "runtime17_after_create",
                "runtime17_after_insert" => "runtime17_after_insert",
                "runtime17_after_verify" => "runtime17_after_verify",
                "runtime17_after_marker" => "runtime17_after_marker",
                "runtime17_after_restore" => "runtime17_after_restore",
                "runtime17_before_commit" => "runtime17_before_commit",
                "runtime17_after_commit" => "runtime17_after_commit",
                _ => panic!("unexpected phase"),
            };
            let db = Connection::open(&path).unwrap();
            let db_key: String = db
                .query_row(
                    "SELECT file FROM pragma_database_list WHERE name='main'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            drop(db);
            crash_probe::arm(db_key, phase, root.join("phase.signal"));
            let _ = Store::open(&path);
            panic!("migration probe did not block");
        }
        assert_eq!(Store::open(&path).unwrap().schema_version().unwrap(), 17);
        let db = Connection::open(&path).unwrap();
        assert_eq!(db.query_row("SELECT rowid FROM plugin_desired WHERE plugin_id='builtin.files' AND enabled=0 AND revision=7 AND updated_at=-12",[],|r|r.get::<_,i64>(0)).unwrap(),31);
        assert_eq!(db.query_row("SELECT rowid FROM plugin_management_requests WHERE key='old-files' AND committed_at=-12",[],|r|r.get::<_,i64>(0)).unwrap(),71);
        assert_eq!(
            db.query_row(
                "SELECT value FROM secrets WHERE id='opaque-next18'",
                [],
                |r| r.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
            vec![0, 1, 255, 254]
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM plugin_desired", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            4
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        return;
    }
    for phase in [
        "runtime17_before_rebuild",
        "runtime17_after_drop",
        "runtime17_after_create",
        "runtime17_after_restore",
        "runtime17_after_insert",
        "runtime17_after_verify",
        "runtime17_after_marker",
        "runtime17_before_commit",
        "runtime17_after_commit",
    ] {
        let root = tempfile::Builder::new()
            .prefix("nhc-runtime17-crash-")
            .tempdir()
            .unwrap()
            .keep();
        let path = root.join("runtime.db");
        drop(Store::open(&path).unwrap());
        let db = Connection::open(&path).unwrap();
        schema_fixture::restore_two_plugin_schema(&db);
        db.execute_batch("PRAGMA user_version=16; UPDATE plugin_desired SET rowid=31,enabled=0,revision=7,updated_at=-12 WHERE plugin_id='builtin.files'; INSERT INTO plugin_management_requests(rowid,key,plugin_id,enabled,expected_revision,result_revision,committed_at) VALUES(71,'old-files','builtin.files',0,6,7,-12); INSERT INTO secrets(rowid,id,value) VALUES(97,'opaque-next18',X'0001FFFE');").unwrap();
        let old_ddl: Vec<String> = db.prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name IN ('plugin_desired','plugin_management_requests') ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        drop(db);
        let mut child = spawn(&root, phase, "migration");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !root.join("phase.signal").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited before {phase}"
            );
            assert!(Instant::now() < deadline, "missing signal for {phase}");
            std::thread::sleep(Duration::from_millis(10));
        }
        child.0.kill().unwrap();
        let status = child.0.wait().unwrap();
        assert!(!status.success());
        std::fs::write(root.join("migration.wait"), format!("{status:?}\n")).unwrap();
        preserve_killed_bytes(&root, "phase.signal", phase, "migration", &status);
        let db = Connection::open(&path).unwrap();
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(
            version,
            if phase == "runtime17_after_commit" {
                17
            } else {
                16
            }
        );
        if phase != "runtime17_after_commit" {
            let ddl: Vec<String> = db.prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name IN ('plugin_desired','plugin_management_requests') ORDER BY name").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            assert_eq!(ddl, old_ddl);
            assert_eq!(db.query_row("SELECT count(*) FROM schema_migrations WHERE id='builtin-runtime-management-v1'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
            assert_eq!(
                db.query_row("SELECT count(*) FROM plugin_desired", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                2
            );
        }
        assert_eq!(db.query_row("SELECT rowid FROM plugin_desired WHERE plugin_id='builtin.files' AND enabled=0 AND revision=7 AND updated_at=-12",[],|r|r.get::<_,i64>(0)).unwrap(),31);
        assert_eq!(db.query_row("SELECT rowid FROM plugin_management_requests WHERE key='old-files' AND committed_at=-12",[],|r|r.get::<_,i64>(0)).unwrap(),71);
        preserve_queryable_snapshot(&db, &root, version);
        drop(db);
        for i in 1..=2 {
            let mut recovery = spawn(&root, "recover", &format!("recovery-{i}"));
            assert!(wait(&mut recovery.0).success());
        }
        println!("migration phase {phase}, evidence {}", root.display());
    }
}
#[test]
fn intent_kill_and_two_fresh_process_reopens() {
    use crate::{
        engine::Engine, plugin_management::SetPluginEnabled, runtime_management::RuntimeId,
    };
    use std::sync::Arc;
    if let Ok(mode) = std::env::var("NHC_INTENT18_CHILD") {
        let root = std::path::PathBuf::from(std::env::var_os("NHC_INTENT18_ROOT").unwrap());
        let path = root.join("runtime.db");
        let id = match std::env::var("NHC_INTENT18_COMPONENT").unwrap().as_str() {
            "builtin.chat-agent" => RuntimeId::ChatAgent,
            "builtin.provider-xpeach" => RuntimeId::CompletionProvider,
            _ => panic!("unexpected component"),
        };
        let store = Arc::new(Store::open(&path).unwrap());
        let engine = Engine::new(store, 2).unwrap();
        if mode != "recover" {
            let phase: &'static str = match mode.as_str() {
                "runtime_intent_before_commit" => "runtime_intent_before_commit",
                "runtime_intent_after_commit" => "runtime_intent_after_commit",
                _ => panic!("unexpected phase"),
            };
            let db = Connection::open(&path).unwrap();
            let db_key: String = db
                .query_row(
                    "SELECT file FROM pragma_database_list WHERE name='main'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            drop(db);
            crash_probe::arm(db_key, phase, root.join("intent.signal"));
            let _ = engine.set_runtime_enabled(
                id,
                SetPluginEnabled {
                    enabled: false,
                    expected_revision: 0,
                },
                "intent-key",
            );
            panic!("intent probe did not block");
        }
        let committed = std::env::var("NHC_INTENT18_COMMITTED").unwrap() == "1";
        let current = serde_json::to_value(engine.runtime_component_status(id).unwrap()).unwrap();
        assert_eq!(current["desired"]["enabled"], !committed);
        assert_eq!(
            current["desired"]["revision"],
            if committed { 1 } else { 0 }
        );
        if committed {
            assert_eq!(current["runtime"]["active"], false);
            let replay = engine
                .set_runtime_enabled(
                    id,
                    SetPluginEnabled {
                        enabled: false,
                        expected_revision: 0,
                    },
                    "intent-key",
                )
                .unwrap();
            assert!(replay.replayed);
            assert_eq!(replay.receipt.revision, 1);
            let current =
                serde_json::to_value(engine.runtime_component_status(id).unwrap()).unwrap();
            assert_eq!(current["runtime"]["active"], false);
        }
        return;
    }
    for id in ["builtin.chat-agent", "builtin.provider-xpeach"] {
        for phase in [
            "runtime_intent_before_commit",
            "runtime_intent_after_commit",
        ] {
            let root = tempfile::Builder::new()
                .prefix("nhc-runtime-intent-crash-")
                .tempdir()
                .unwrap()
                .keep();
            let path = root.join("runtime.db");
            drop(Store::open(&path).unwrap());
            let start_child = |mode: &str, suffix: &str| {
                let stdout =
                    std::fs::File::create(root.join(format!("{suffix}.stdout.log"))).unwrap();
                let stderr =
                    std::fs::File::create(root.join(format!("{suffix}.stderr.log"))).unwrap();
                let child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "runtime_management_crash_tests::intent_kill_and_two_fresh_process_reopens",
                        "--nocapture",
                    ])
                    .env("NHC_INTENT18_CHILD", mode)
                    .env("NHC_INTENT18_ROOT", &root)
                    .env("NHC_INTENT18_COMPONENT", id)
                    .env(
                        "NHC_INTENT18_COMMITTED",
                        if phase == "runtime_intent_after_commit" {
                            "1"
                        } else {
                            "0"
                        },
                    )
                    .stdout(Stdio::from(stdout))
                    .stderr(Stdio::from(stderr))
                    .spawn()
                    .unwrap();
                std::fs::write(root.join(format!("{suffix}.pid")), child.id().to_string()).unwrap();
                OwnedChild(child)
            };
            let mut child = start_child(phase, "intent");
            let deadline = Instant::now() + Duration::from_secs(20);
            while !root.join("intent.signal").exists() {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "intent child exited early"
                );
                assert!(Instant::now() < deadline, "missing signal {phase}");
                std::thread::sleep(Duration::from_millis(10));
            }
            child.0.kill().unwrap();
            let exited = child.0.wait().unwrap();
            assert!(!exited.success());
            std::fs::write(root.join("intent.wait"), format!("{exited:?}\n")).unwrap();
            preserve_killed_bytes(&root, "intent.signal", phase, "intent", &exited);
            let db = Connection::open(&path).unwrap();
            let committed = phase == "runtime_intent_after_commit";
            let persisted: (bool, i64) = db
                .query_row(
                    "SELECT enabled,revision FROM plugin_desired WHERE plugin_id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(persisted, (!committed, if committed { 1 } else { 0 }));
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM plugin_management_requests WHERE key='intent-key'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                i64::from(committed)
            );
            let version = db
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap();
            preserve_queryable_snapshot(&db, &root, version);
            drop(db);
            for i in 1..=2 {
                let mut recovery = start_child("recover", &format!("recovery-{i}"));
                assert!(wait(&mut recovery.0).success());
            }
            println!("intent {id} {phase}, evidence {}", root.display());
        }
    }
}
