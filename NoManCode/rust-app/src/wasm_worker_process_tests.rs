//! Component tests for the trusted worker supervisor.
use super::wasm_worker_process::{TrustedWorker, WorkerService};
use super::wasm_worker_protocol::{
    self as protocol, Decision, Input, Manifest, Phase, ToolClass, Vote,
};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};
use tokio_util::sync::CancellationToken;

fn worker() -> WorkerService {
    // The outer runner builds the real W1 binary last, copies it to a fixed
    // snapshot and supplies both path and hash. This entry exists only in tests.
    let path =
        PathBuf::from(std::env::var_os("NHC_W2_WORKER_SNAPSHOT").expect("fixed worker snapshot"));
    let expected = std::env::var("NHC_W2_WORKER_SHA256").expect("fixed worker digest");
    let digest: [u8; 32] = Sha256::digest(fs::read(&path).expect("read worker snapshot")).into();
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&path).unwrap())),
        expected
    );
    WorkerService::from_trusted(TrustedWorker::from_host_install(path, digest))
}

fn wasm() -> Vec<u8> {
    let before = r#"{"action":"use_tools","indices":[1]}"#;
    let after = r#"{"action":"tool_votes","votes":["deny","apply_host_policy"]}"#;
    let encode = |text: &str| {
        text.bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let before_bytes = encode(before);
    let after_bytes = encode(after);
    wat::parse_str(format!(
        r#"(module
          (memory (export "memory") 1 1)
          (global $started (mut i32) (i32.const 0))
          (func $start i32.const 7 global.set $started)
          (start $start)
          (data (i32.const 1024) "{before_bytes}")
          (data (i32.const 1200) "{after_bytes}")
          (func (export "alloc") (param i32) (result i32) i32.const 0)
          (func (export "run_json") (param $p i32) (param i32) (result i64)
            global.get $started i32.const 7 i32.ne if unreachable end
            local.get $p i32.const 9 i32.add i32.load8_u i32.const 48 i32.eq
            if (result i64) i64.const {} else i64.const {} end))"#,
        (1024u64 << 32) | before.len() as u64,
        (1200u64 << 32) | after.len() as u64
    ))
    .unwrap()
}

fn leb(mut value: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            return bytes;
        }
    }
}
fn near_limit(mut module: Vec<u8>) -> Vec<u8> {
    let target = protocol::MAX_MODULE - 64;
    let mut payload = target - module.len() - 6;
    while module.len() + 1 + leb(payload).len() + payload < target {
        payload += 1;
    }
    assert_eq!(module.len() + 1 + leb(payload).len() + payload, target);
    module.push(0); // WebAssembly custom section.
    module.extend(leb(payload));
    module.extend([1, b'x']);
    module.resize(target, 0);
    module
}

fn manifest() -> Manifest {
    Manifest {
        version: protocol::VERSION,
        request_id: "replaced-by-host".into(),
        nonce: "00000000000000000000000000000000".into(),
        binding_generation: 19,
        plugin_id: "w2-test".into(),
        plugin_version: "1.0.0".into(),
        digest: "0".repeat(64),
        fuel: 1_000_000,
        memory_pages: 1,
    }
}

#[tokio::test]
async fn w01_real_worker_before_after_and_near_8mib() {
    let stage_log = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").expect("attempt stage log"));
    assert!(
        !stage_log.exists(),
        "stage log must belong to this attempt only"
    );
    let service = worker();
    let before = Input {
        round: 0,
        max_rounds: 3,
        decision: Phase::Before {
            tools: vec![ToolClass::Read, ToolClass::Write],
        },
    };
    let module = near_limit(wasm());
    assert!(module.len() > 8 * 1024 * 1024 - 128);
    let first = service
        .call(manifest(), before, module, CancellationToken::new())
        .await
        .expect("real before policy");
    println!(
        "W01 before pid={} exit={} signaled={} job_active={} io_completed={} module_bytes={}",
        first.process_id,
        first.exit_code,
        first.process_signaled,
        first.job_active_processes,
        first.io_completed,
        protocol::MAX_MODULE - 64
    );
    assert_eq!(first.decision, Decision::UseTools { indices: vec![1] });
    assert!(first.process_id != 0 && first.process_signaled && first.io_completed);
    assert_eq!((first.exit_code, first.job_active_processes), (0, 0));

    let after = Input {
        round: 1,
        max_rounds: 3,
        decision: Phase::AfterTools {
            tools: vec![ToolClass::Command, ToolClass::Read],
        },
    };
    let module = wasm();
    let second = service
        .call(manifest(), after, module, CancellationToken::new())
        .await
        .expect("real after policy");
    println!(
        "W01 after pid={} exit={} signaled={} job_active={} io_completed={}",
        second.process_id,
        second.exit_code,
        second.process_signaled,
        second.job_active_processes,
        second.io_completed
    );
    assert_eq!(
        second.decision,
        Decision::ToolVotes {
            votes: vec![Vote::Deny, Vote::ApplyHostPolicy]
        }
    );
    assert!(second.process_id != 0 && second.process_signaled && second.io_completed);
    assert_eq!((second.exit_code, second.job_active_processes), (0, 0));
    let stages = fs::read_to_string(stage_log).expect("read supervisor stages");
    for pid in [first.process_id, second.process_id] {
        for phase in ["created_suspended", "resumed", "io_complete_reaped"] {
            assert!(
                stages.contains(&format!("phase={phase} pid=Some({pid})")),
                "missing {phase} for worker {pid}"
            );
        }
    }
}

#[cfg(windows)]
mod fault_matrix {
    use super::*;
    use crate::wasm_worker_process::{WorkerError, test_observer};
    use std::{
        os::windows::io::AsRawHandle,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_BROKEN_PIPE, WAIT_OBJECT_0},
        System::Threading::WaitForSingleObject,
    };

    #[derive(Clone, Copy)]
    enum Expected {
        Protocol,
        Rejected,
        Exit42,
        Deadline,
        BrokenPipe,
        StderrLimit,
        Success,
    }

    fn fault_service(mode: &str) -> (WorkerService, PathBuf) {
        let dir =
            PathBuf::from(std::env::var_os("NHC_W2_FAULT_DIR").expect("fault snapshot directory"));
        let path = dir.join(format!("{mode}.exe"));
        let hash = Sha256::digest(fs::read(&path).expect("read fixed fault snapshot"));
        assert_eq!(
            format!("{hash:x}"),
            std::env::var("NHC_W2_FAULT_SHA256").expect("fault digest")
        );
        (
            WorkerService::from_trusted(TrustedWorker::from_host_install(
                path.clone(),
                hash.into(),
            )),
            path.with_extension("stage"),
        )
    }

    fn read_log(path: &std::path::Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }
    fn contains_pid(log: &str, phase: &str, pid: u32) -> bool {
        log.lines()
            .any(|line| line == format!("phase={phase} pid=Some({pid})"))
    }
    fn probe_pid(log: &str, phase: &str, pid: u32) -> bool {
        log.lines()
            .any(|line| line == format!("pid={pid} phase={phase}"))
    }
    fn max_count(log: &str, phase: &str, pid: u32) -> usize {
        let prefix = format!("phase={phase} pid=Some({pid}) count=");
        log.lines()
            .filter_map(|line| line.strip_prefix(&prefix)?.parse().ok())
            .max()
            .unwrap_or(0)
    }
    fn before() -> Input {
        Input {
            round: 0,
            max_rounds: 3,
            decision: Phase::Before {
                tools: vec![ToolClass::Read, ToolClass::Write],
            },
        }
    }
    fn input_for(mode: &str) -> Input {
        match mode {
            "decision-no-tools" => Input {
                round: 1,
                max_rounds: 3,
                decision: Phase::AfterNoTools {},
            },
            "decision-votes" => Input {
                round: 1,
                max_rounds: 3,
                decision: Phase::AfterTools {
                    tools: vec![ToolClass::Read, ToolClass::Write],
                },
            },
            _ => before(),
        }
    }
    async fn control() {
        let (service, _) = fault_service("control");
        let result = service
            .call(manifest(), before(), wasm(), CancellationToken::new())
            .await;
        assert!(
            matches!(result, Ok(ref s) if matches!(s.decision, Decision::Stop {}) && s.process_signaled && s.job_active_processes == 0 && s.io_completed),
            "control/slot reuse: {result:?}"
        );
    }

    fn fresh_group() {
        let log =
            PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").expect("supervisor stage log"));
        assert!(!log.exists(), "group stage log must be new");
        let (_, control_stage) = fault_service("control");
        assert!(
            !control_stage.exists(),
            "group probe snapshots must be fresh"
        );
    }

    async fn case(
        mode: &str,
        expected: Expected,
        probe_phase: &str,
        host_phase: Option<&str>,
        large: bool,
    ) {
        let log_path =
            PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").expect("supervisor stage log"));
        let prior_log = read_log(&log_path).len();
        let (service, probe_log) = fault_service(mode);
        let module = if large { near_limit(wasm()) } else { wasm() };
        assert!(
            !probe_log.exists(),
            "each mode needs a fresh snapshot and stage file"
        );
        let barrier = if mode == "partial-read" {
            let host_log = log_path.clone();
            let stage = probe_log.clone();
            let release = stage.with_extension("release");
            assert!(!release.exists());
            Some(std::thread::spawn(move || {
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(3) {
                    let host = read_log(&host_log);
                    if host.len() >= prior_log
                        && host[prior_log..].contains("phase=stdin_pending ")
                        && read_log(&stage).contains("phase=read_32")
                    {
                        fs::write(release, b"release").expect("release partial-read barrier");
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                false
            }))
        } else {
            None
        };
        test_observer::arm(mode == "dual-pressure");
        let started = Instant::now();
        let result = service
            .call(
                manifest(),
                input_for(mode),
                module,
                CancellationToken::new(),
            )
            .await;
        let elapsed = started.elapsed();
        if let Some(barrier) = barrier {
            assert!(barrier.join().expect("partial barrier thread"));
        }
        assert!(
            !matches!(result, Err(WorkerError::CleanupUnknown)),
            "{mode}: cleanup unknown; stop matrix: {result:?}"
        );
        match expected {
            Expected::Protocol => assert!(
                matches!(result, Err(WorkerError::Protocol)),
                "{mode}: {result:?}"
            ),
            Expected::Rejected => assert!(
                matches!(result, Err(WorkerError::WorkerRejected)),
                "{mode}: {result:?}"
            ),
            Expected::Exit42 => assert!(
                matches!(
                    result,
                    Err(WorkerError::IsolationUnavailable("worker_exit", Some(42)))
                ),
                "{mode}: {result:?}"
            ),
            Expected::Deadline => {
                assert!(
                    matches!(result, Err(WorkerError::Deadline)),
                    "{mode}: {result:?}"
                );
                assert!(
                    elapsed >= Duration::from_millis(4500) && elapsed < Duration::from_secs(9),
                    "{mode}: elapsed={elapsed:?}"
                );
            }
            Expected::BrokenPipe => assert!(
                matches!(result, Err(WorkerError::IsolationUnavailable("io_submit" | "io_completion", Some(code))) if code == ERROR_BROKEN_PIPE as i32),
                "{mode}: {result:?}"
            ),
            Expected::StderrLimit => assert!(
                matches!(
                    result,
                    Err(WorkerError::IsolationUnavailable("stderr_limit", None))
                ),
                "{mode}: {result:?}"
            ),
            Expected::Success => assert!(
                matches!(result, Ok(ref s) if matches!(s.decision, Decision::Stop {})),
                "{mode}: {result:?}"
            ),
        }
        let (pid, handle) = test_observer::take().expect("live-process observation handle");
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
            WAIT_OBJECT_0,
            "{mode}: observed process not signaled"
        );
        let log = read_log(&log_path);
        let log = &log[prior_log..];
        for phase in [
            "created_suspended",
            "resumed",
            "stdin_io_settled",
            "stdout_io_settled",
            "stderr_io_settled",
            "run_handles_released",
        ] {
            assert!(
                contains_pid(log, phase, pid),
                "{mode}: missing {phase} for {pid}; {log}"
            );
        }
        assert!(
            contains_pid(log, "io_complete_reaped", pid)
                || contains_pid(log, "failure_reaped", pid),
            "{mode}: missing process and Job proof: {log}"
        );
        for (phase, count) in [("process_wait", 0), ("job_active", 0)] {
            assert!(
                log.lines()
                    .any(|line| line == format!("phase={phase} pid=Some({pid}) count={count}")),
                "{mode}: missing {phase} syscall result: {log}"
            );
        }
        assert!(
            max_count(log, "job_query", pid) > 0,
            "{mode}: job query failed or not observed"
        );
        if let Some(phase) = host_phase {
            assert!(
                contains_pid(log, phase, pid),
                "{mode}: missing host {phase}: {log}"
            );
        }
        let probe = read_log(&probe_log);
        assert!(
            probe_pid(&probe, probe_phase, pid),
            "{mode}: missing probe {probe_phase} for {pid}: {probe}"
        );
        if !matches!(
            mode,
            "no-read" | "partial-read" | "concurrent" | "dual-pressure"
        ) {
            assert!(
                probe_pid(&probe, "request_eof", pid),
                "{mode}: complete request/EOF not reached"
            );
        }
        if mode == "concurrent" {
            for phase in ["stdin_pending", "stdout_data", "stderr_data"] {
                assert!(
                    contains_pid(log, phase, pid),
                    "{mode}: missing {phase}: {log}"
                );
            }
            assert!(
                max_count(log, "stdout_bytes", pid) > 0 && max_count(log, "stderr_bytes", pid) > 0
            );
        }
        if mode == "dual-pressure" {
            for phase in ["pressure_stdout_attempt", "pressure_stderr_attempt"] {
                assert!(probe_pid(&probe, phase, pid), "{mode}: missing {phase}");
            }
            for stream in ["stdout", "stderr"] {
                let capacity = max_count(log, &format!("pressure_{stream}_capacity"), pid);
                let available = max_count(log, &format!("pressure_{stream}_available"), pid);
                assert!(
                    capacity > 0 && available >= capacity,
                    "{mode}: {stream} capacity={capacity} available={available}"
                );
            }
            assert!(contains_pid(log, "stdin_pending", pid));
            assert!(contains_pid(log, "stderr_limit", pid));
            assert!(max_count(log, "stderr_bytes", pid) > 64 * 1024);
        }
        if mode == "stdout-limit" {
            assert!(max_count(log, "stdout_bytes", pid) > protocol::MAX_RESPONSE + 4);
        }
        if mode == "stderr-limit" {
            assert!(max_count(log, "stderr_bytes", pid) > 64 * 1024);
        }
        if mode == "zero-stdout" || mode == "zero-stderr" {
            assert!(
                probe_pid(&probe, "zero_write", pid) && probe_pid(&probe, "zero_tail_written", pid)
            );
            let phase = if mode == "zero-stdout" {
                "stdout_zero_read"
            } else {
                "stderr_zero_read"
            };
            println!(
                "{mode} actual_zero_read_observed={}",
                contains_pid(log, phase, pid)
            );
        }
        println!(
            "{mode} pid={pid} elapsed_ms={} result={result:?}",
            elapsed.as_millis()
        );
        drop(handle);
        control().await;
    }

    #[tokio::test]
    async fn w02_protocol_fault_matrix() {
        fresh_group();
        control().await;
        for mode in [
            "short-header",
            "short-header-1",
            "short-header-2",
            "short-payload",
            "zero-length",
            "long-length",
            "bad-utf8",
            "bad-json",
            "unknown-field",
            "duplicate-field",
            "trailing",
            "duplicate-frame",
            "id-version",
            "id-request",
            "id-nonce",
            "id-generation",
            "id-plugin",
            "id-plugin-version",
            "id-digest",
            "id-fuel",
            "id-memory",
            "decision-phase",
            "decision-index",
            "decision-duplicate",
            "decision-order",
            "decision-no-tools",
            "decision-votes",
        ] {
            case(mode, Expected::Protocol, "output_written", None, false).await;
        }
        case(
            "rejected",
            Expected::Rejected,
            "output_written",
            None,
            false,
        )
        .await;
        for mode in ["exit-output", "exit-empty"] {
            case(mode, Expected::Exit42, "exit_42", None, false).await;
        }
        for mode in ["hold-valid", "hold-prefix"] {
            case(mode, Expected::Deadline, "holding_open", None, false).await;
        }
        case("delayed", Expected::Deadline, "delayed_wait", None, false).await;
    }

    #[tokio::test]
    async fn w03_pipe_fault_matrix() {
        fresh_group();
        control().await;
        case(
            "zero-stdout",
            Expected::Protocol,
            "zero_tail_written",
            None,
            false,
        )
        .await;
        case(
            "zero-stderr",
            Expected::Success,
            "zero_tail_written",
            Some("stderr_data"),
            false,
        )
        .await;
        case(
            "stdout-limit",
            Expected::Protocol,
            "stdout_limit_write",
            Some("stdout_limit"),
            false,
        )
        .await;
        case(
            "stderr-limit",
            Expected::StderrLimit,
            "stderr_limit_write",
            Some("stderr_limit"),
            false,
        )
        .await;
        case(
            "no-read",
            Expected::Deadline,
            "holding_without_read",
            Some("stdin_pending"),
            true,
        )
        .await;
        case(
            "partial-read",
            Expected::BrokenPipe,
            "stdin_closed",
            Some("stdin_pending"),
            true,
        )
        .await;
        case(
            "partial-stdout",
            Expected::Deadline,
            "holding_open",
            Some("stdout_data"),
            false,
        )
        .await;
        case(
            "closed-stdout",
            Expected::Deadline,
            "stdout_closed",
            Some("stdout_eof_process_alive_stderr_open"),
            false,
        )
        .await;
        case(
            "concurrent",
            Expected::Deadline,
            "concurrent_written",
            Some("stdin_pending"),
            true,
        )
        .await;
        case(
            "dual-pressure",
            Expected::StderrLimit,
            "pressure_stdout_attempt",
            Some("dual_pressure_release"),
            true,
        )
        .await;
    }
}
