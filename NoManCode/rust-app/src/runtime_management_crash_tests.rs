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
        drop(db);
        for i in 1..=2 {
            let mut recovery = spawn(&root, "recover", &format!("recovery-{i}"));
            assert!(wait(&mut recovery.0).success());
        }
        println!("migration phase {phase}, evidence {}", root.display());
    }
}
