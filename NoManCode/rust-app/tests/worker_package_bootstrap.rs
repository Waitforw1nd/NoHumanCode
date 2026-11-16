//! Production bootstrap exercised in an isolated executable directory.
//! The manager runs these process tests in the supervised heavy window.
#![cfg(windows)]

use peachsh::worker_bootstrap::bootstrap_from_packaged_digest;
use sha2::{Digest, Sha256};
use std::{fs, process::Command};

const WORKER: &str = "peachsh-wasm-worker.exe";
const TRUSTED: &[u8] = b"trusted fixture worker bytes";

#[test]
fn bootstrap_child() {
    let Ok(case) = std::env::var("NHC_BOOTSTRAP_FIXTURE_CASE") else {
        return;
    };
    let expected = format!("{:x}", Sha256::digest(TRUSTED));
    match case.as_str() {
        "disabled" => assert!(bootstrap_from_packaged_digest(None).unwrap().is_none()),
        "malformed" => assert!(bootstrap_from_packaged_digest(Some("not-a-digest")).is_err()),
        "wrong" => assert!(bootstrap_from_packaged_digest(Some(&"0".repeat(64))).is_err()),
        "mixed" | "missing" => assert!(bootstrap_from_packaged_digest(Some(&expected)).is_err()),
        "ready_and_pin" => {
            let service = bootstrap_from_packaged_digest(Some(&expected)).unwrap();
            assert!(service.is_some());
            let path = std::env::current_exe().unwrap().with_file_name(WORKER);
            let replacement = path.with_file_name("replacement.exe");
            fs::write(&replacement, b"replacement").unwrap();
            assert!(
                fs::remove_file(&path).is_err(),
                "live pin must deny deletion"
            );
            assert!(
                fs::rename(&replacement, &path).is_err(),
                "live pin must deny replacement"
            );
            drop(service);
            fs::remove_file(&path).expect("pin released with service");
            fs::rename(replacement, &path).unwrap();
            assert!(bootstrap_from_packaged_digest(Some(&expected)).is_err());
        }
        _ => panic!("unknown fixture case"),
    }
}

#[test]
fn packaged_bootstrap_rejects_mismatch_and_pins_fixed_copy() {
    let root = tempfile::tempdir().unwrap();
    let original = std::env::current_exe().unwrap();
    for case in [
        "disabled",
        "malformed",
        "wrong",
        "mixed",
        "missing",
        "ready_and_pin",
    ] {
        let dir = root.path().join(case);
        fs::create_dir(&dir).unwrap();
        let host = dir.join("fixture-host.exe");
        fs::copy(&original, &host).unwrap();
        if case != "missing" {
            fs::write(
                dir.join(WORKER),
                if case == "mixed" { b"foreign worker" } else { TRUSTED },
            )
            .unwrap();
        }
        // Wait owns the original child; no PID reopening, kill or timeout-based cleanup.
        let status = Command::new(host)
            .args(["--exact", "bootstrap_child", "--nocapture"])
            .env("NHC_BOOTSTRAP_FIXTURE_CASE", case)
            .status()
            .unwrap();
        assert!(status.success(), "bootstrap case failed: {case}");
    }
}

/// Run explicitly with two independently built pairs and an unbound Host.
/// External paths are test inputs, never passed to the product as worker paths.
#[test]
#[ignore = "requires manager-built real pairs and an unbound Host"]
fn real_pair_host_check_rejects_tampering_and_mix() {
    let pair_a = std::path::PathBuf::from(std::env::var_os("NHC_TEST_PAIR_A").expect("pair A"));
    let pair_b = std::path::PathBuf::from(std::env::var_os("NHC_TEST_PAIR_B").expect("pair B"));
    let unbound = std::path::PathBuf::from(
        std::env::var_os("NHC_TEST_UNBOUND_HOST").expect("unbound host"),
    );
    let worker_a = fs::read(pair_a.join(WORKER)).unwrap();
    let worker_b = fs::read(pair_b.join(WORKER)).unwrap();
    assert_ne!(
        worker_a, worker_b,
        "mixed-pair test needs different worker bytes"
    );
    let root = tempfile::tempdir().unwrap();
    let check = |case: &str, host: &std::path::Path, worker: Option<&[u8]>, success: bool| {
        let dir = root.path().join(case);
        fs::create_dir(&dir).unwrap();
        let executable = dir.join("peachsh.exe");
        fs::copy(host, &executable).unwrap();
        if let Some(bytes) = worker {
            fs::write(dir.join(WORKER), bytes).unwrap();
        }
        let status = Command::new(executable)
            .arg("--check")
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("--workspace")
            .arg(dir.join("workspace"))
            .current_dir(&dir)
            .status()
            .unwrap();
        assert_eq!(status.success(), success, "real Host case: {case}");
    };
    let host_a = pair_a.join("peachsh.exe");
    check("pair-a", &host_a, Some(&worker_a), true);
    check("missing", &host_a, None, false);
    check("wrong", &host_a, Some(b"tampered"), false);
    check("mixed", &host_a, Some(&worker_b), false);
    check("unbound", &unbound, None, true);
}
