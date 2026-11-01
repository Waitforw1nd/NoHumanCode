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
#[ignore = "requires controlled worker snapshots and isolated process observation"]
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
    use crate::wasm_worker_process::test_hooks;
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
        Exit43,
        CpuLimit,
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
    fn assert_reaped(log: &str, pid: u32) {
        assert!(
            log.lines()
                .any(|line| line == format!("phase=process_wait pid=Some({pid}) count=0"))
        );
        assert!(
            log.lines()
                .any(|line| line == format!("phase=job_active pid=Some({pid}) count=0"))
        );
        assert!(max_count(log, "job_query", pid) > 0);
        assert!(contains_pid(log, "run_handles_released", pid));
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
        if matches!(mode, "job-cpu" | "job-memory") {
            test_hooks::arm_job_events();
        }
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
        if matches!(result, Err(WorkerError::CleanupUnknown)) {
            eprintln!("NHC_TERMINAL_UNKNOWN: {mode} cleanup unconfirmed");
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
            Expected::Exit43 => assert!(
                matches!(
                    result,
                    Err(WorkerError::IsolationUnavailable("worker_exit", Some(43)))
                ),
                "{mode}: {result:?}"
            ),
            Expected::CpuLimit => assert!(
                matches!(result, Err(WorkerError::IsolationUnavailable("worker_exit", Some(code))) if code != 0),
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
        if mode == "job-cpu" || mode == "job-memory" {
            assert_eq!(max_count(log, "job_memory_limit", pid), 128 * 1024 * 1024);
            assert_eq!(max_count(log, "job_cpu_limit_100ns", pid), 30_000_000);
            let notification = if mode == "job-cpu" { 2 } else { 9 };
            assert!(
                log.lines().any(|line| line
                    == format!("phase=job_notification pid=Some({pid}) count={notification}")),
                "{mode}: required Job notification absent: {log}"
            );
            assert!(max_count(log, "job_extended_query", pid) > 0);
            if mode == "job-cpu" {
                let thread_marker = format!("pid={pid} phase=cpu_thread_started");
                assert_eq!(
                    probe
                        .lines()
                        .filter(|line| *line == thread_marker.as_str())
                        .count(),
                    8,
                    "all finite busy threads must actually start"
                );
                assert!(max_count(log, "job_user_100ns", pid) >= 30_000_000);
                assert!(
                    elapsed < Duration::from_secs(5),
                    "job CPU limit did not precede host deadline"
                );
            } else {
                assert!(max_count(log, "job_peak_process_memory", pid) > 100 * 1024 * 1024);
            }
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
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
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
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
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

    fn limit_module(mode: &str, max_pages: u32) -> Vec<u8> {
        let decision = r#"{"action":"stop"}"#;
        let encoded = decision
            .bytes()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>();
        let start = if mode == "start-fuel" {
            "(func $start (loop $again br $again)) (start $start)"
        } else {
            ""
        };
        let body = match mode {
            "run-fuel" => "(loop $again br $again) i64.const 0".to_string(),
            "memory" | "memory-declaration" => format!(
                "i32.const 1 memory.grow i32.const -1 i32.eq if unreachable end i64.const {}",
                (1024u64 << 32) | decision.len() as u64
            ),
            _ => format!("i64.const {}", (1024u64 << 32) | decision.len() as u64),
        };
        wat::parse_str(format!(r#"(module (memory (export "memory") 1 {max_pages}) (data (i32.const 1024) "{encoded}") {start} (func (export "alloc") (param i32) (result i32) i32.const 0) (func (export "run_json") (param i32 i32) (result i64) {body}))"#)).unwrap()
    }
    async fn real_limit(mode: &str, pages: u32, expected_success: bool) {
        let stage = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").unwrap());
        let offset = read_log(&stage).len();
        let mut m = manifest();
        m.fuel = 100_000;
        m.memory_pages = pages as u64;
        test_observer::arm(false);
        let module_pages = if mode == "memory-declaration" {
            2
        } else {
            pages
        };
        let result = worker()
            .call(
                m,
                before(),
                limit_module(mode, module_pages),
                CancellationToken::new(),
            )
            .await;
        let (pid, observed) = test_observer::take().expect("real worker observer");
        assert_eq!(
            unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
        let text = read_log(&stage);
        let log = &text[offset..];
        for phase in [
            "created_suspended",
            "resumed",
            "io_complete_reaped",
            "stdin_io_settled",
            "stdout_io_settled",
            "stderr_io_settled",
            "run_handles_released",
        ] {
            assert!(
                contains_pid(log, phase, pid),
                "{mode}: missing {phase}: {log}"
            );
        }
        if expected_success {
            assert!(
                matches!(result, Ok(ref s) if matches!(s.decision, Decision::Stop {})),
                "{mode}: {result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(WorkerError::WorkerRejected)),
                "{mode}: {result:?}"
            );
        }
        println!("resource {mode} pages={pages} pid={pid} result={result:?}");
        control().await;
    }
    fn p01_boundary_check(records: &[serde_json::Value], pid: u32) -> Result<(), String> {
        for v in records {
            let kind = v["kind"].as_str().ok_or("missing record kind")?;
            if matches!(kind, "open_failed" | "list_failed") {
                return Err(format!("observation failed: {v}"));
            }
            if kind == "configuration"
                && (v["host_in_any_job_query_ok"] != true || v["active_limit"] != 1)
            {
                return Err(format!("job configuration unknown: {v}"));
            }
            if kind == "list_before_open" {
                if v["complete"] != true {
                    return Err(format!("incomplete job list: {v}"));
                }
                let ids = v["ids"].as_array().ok_or("missing first job list")?;
                if ids.iter().any(|id| id.as_u64() != Some(pid as u64)) {
                    return Err(format!("extra list object: {v}"));
                }
            }
            if matches!(kind, "job_list" | "notification" | "host_suspended") {
                if v["pid"].as_u64() != Some(pid as u64) {
                    return Err(format!("extra observed object: {v}"));
                }
                if v["list_complete"] != true || v["accounting_ok"] != true {
                    return Err(format!("job query incomplete: {v}"));
                }
                let ids = v["list_ids"].as_array().ok_or("missing second job list")?;
                if ids.iter().any(|id| id.as_u64() != Some(pid as u64)) {
                    return Err(format!("extra list object: {v}"));
                }
                if v["active"].as_u64().ok_or("missing active count")? > 1
                    || v["total"].as_u64().ok_or("missing total count")? > 1
                {
                    return Err(format!("job count exceeds one: {v}"));
                }
                if v["retained_handle"] != true
                    || v["handle_pid"].as_u64() != Some(pid as u64)
                    || v["member_ok"] != true
                    || v["member"] != 1
                    || v["any_job_ok"] != true
                    || v["any_job"] != 1
                    || v["times_ok"] != true
                    || !v["creation_filetime"].as_u64().is_some_and(|t| t != 0)
                {
                    return Err(format!("process identity or membership unknown: {v}"));
                }
                for field in ["process_before", "process_after"] {
                    if v[field]["member_ok"] != true
                        || v[field]["member"] != 1
                        || !matches!(v[field]["wait"].as_u64(), Some(0 | 258))
                    {
                        return Err(format!("process status unknown: {v}"));
                    }
                }
            }
            for field in ["target_before", "target_after"] {
                if !v[field].is_null()
                    && (v[field]["member_ok"] != true
                        || v[field]["member"] != 1
                        || !matches!(v[field]["wait"].as_u64(), Some(0 | 258)))
                {
                    return Err(format!("target status unknown: {v}"));
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w06_p01_live_job_observation() {
        // attempt8 seq 14/17 observed this extra object and accounting state.
        let extra = serde_json::json!({"kind":"notification","pid":9276});
        let active_two = serde_json::json!({"kind":"notification","pid":64492,
            "list_complete":true,"accounting_ok":true,"list_ids":[64492],
            "active":2,"total":2});
        assert!(
            p01_boundary_check(&[extra], 64492)
                .expect_err("attempt8 extra object must fail")
                .starts_with("extra observed object:")
        );
        assert!(
            p01_boundary_check(&[active_two], 64492)
                .expect_err("attempt8 active count must fail")
                .starts_with("job count exceeds one:")
        );
        let log = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").expect("stage log"));
        let (service, probe_log) = fault_service("p01-memory");
        let release = probe_log.with_extension("release");
        assert!(!log.exists() && !probe_log.exists() && !release.exists());
        test_observer::arm(false);
        test_hooks::arm_p01_events();
        let barrier_log = log.clone();
        let barrier_probe = probe_log.clone();
        let barrier = std::thread::spawn(move || {
            let started = Instant::now();
            let mut observed = None;
            let mut ready = false;
            while started.elapsed() < Duration::from_millis(1500) {
                if observed.is_none() {
                    observed = test_observer::take();
                }
                let text = read_log(&barrier_log);
                if let Some((pid, handle)) = observed.as_ref() {
                    let records: Vec<serde_json::Value> = text
                        .lines()
                        .filter_map(|line| {
                            serde_json::from_str::<serde_json::Value>(line.strip_prefix("p01=")?)
                                .ok()
                        })
                        .collect();
                    let snapshot = records.iter().any(|v| {
                        v["kind"] == "job_list"
                            && v["pid"].as_u64() == Some(*pid as u64)
                            && v["list_complete"] == true
                            && v["target_before"]["wait"].as_u64() == Some(258)
                            && v["target_after"]["wait"].as_u64() == Some(258)
                            && v["target_before"]["member_ok"] == true
                            && v["target_after"]["member_ok"] == true
                            && v["target_before"]["member"] == 1
                            && v["target_after"]["member"] == 1
                            && v["process_before"]["wait"].as_u64() == Some(258)
                            && v["process_after"]["wait"].as_u64() == Some(258)
                            && v["process_before"]["member_ok"] == true
                            && v["process_after"]["member_ok"] == true
                            && v["process_before"]["member"] == 1
                            && v["process_after"]["member"] == 1
                            && v["list_ids"].as_array().is_some_and(|ids| {
                                ids.iter().any(|id| id.as_u64() == Some(*pid as u64))
                            })
                            && v["handle_pid"].as_u64() == Some(*pid as u64)
                            && v["times_ok"] == true
                            && v["creation_filetime"].as_u64().is_some_and(|t| t != 0)
                            && v["image"].as_str().is_some()
                            && records.iter().any(|end| {
                                end["kind"] == "scan_complete"
                                    && end["scan"] == v["scan"]
                                    && end["target_before"]["wait"].as_u64() == Some(258)
                                    && end["target_after"]["wait"].as_u64() == Some(258)
                                    && end["target_before"]["member_ok"] == true
                                    && end["target_after"]["member_ok"] == true
                                    && end["target_before"]["member"] == 1
                                    && end["target_after"]["member"] == 1
                            })
                    });
                    if snapshot
                        && text.contains("phase=p01_collector_ready")
                        && text.contains("\"kind\":\"host_suspended\"")
                        && probe_pid(&read_log(&barrier_probe), "p01_wait", *pid)
                        && unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } == 258
                    {
                        ready = true;
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let released = fs::write(release, b"release").is_ok();
            (observed, ready, released)
        });
        let result = service
            .call(manifest(), before(), wasm(), CancellationToken::new())
            .await;
        let (observed, ready, released) = barrier.join().expect("P01 barrier");
        let observed = observed.or_else(test_observer::take);
        if let Some((pid, handle)) = observed.as_ref() {
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
                WAIT_OBJECT_0,
                "P01 worker cleanup unknown for {pid}"
            );
        }
        if matches!(result, Err(WorkerError::CleanupUnknown)) {
            eprintln!("NHC_TERMINAL_UNKNOWN: P01 cleanup unconfirmed");
        }
        assert!(
            !matches!(result, Err(WorkerError::CleanupUnknown)),
            "P01 cleanup unknown"
        );
        assert!(
            released && ready,
            "P01 handshake incomplete; preserve evidence as unknown"
        );
        let (pid, _) = observed.expect("P01 worker observer");
        let text = read_log(&log);
        let probe = read_log(&probe_log);
        assert!(probe_pid(&probe, "p01_released", pid));
        assert!(probe_pid(&probe, "memory_allocation_failed", pid));
        assert!(
            matches!(
                result,
                Err(WorkerError::IsolationUnavailable("worker_exit", Some(43)))
            ),
            "P01 resource behavior not observed: {result:?}"
        );
        assert_reaped(&text, pid);
        assert!(text.contains("phase=p01_collector_stopped"));
        assert!(!text.contains("phase=p01_collector_shutdown_unknown"));
        assert!(!text.contains("phase=p01_collector_failed"));
        assert!(!text.contains("phase=p01_dequeue_failed"));
        assert!(!text.contains("phase=p01_bad_completion_key"));
        use windows_sys::Win32::System::Threading::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
            EXTENDED_STARTUPINFO_PRESENT,
        };
        let flags = CREATE_SUSPENDED
            | DETACHED_PROCESS
            | CREATE_UNICODE_ENVIRONMENT
            | EXTENDED_STARTUPINFO_PRESENT;
        assert!(
            text.lines()
                .any(|line| line == format!("phase=create_flags pid=Some(0) count={flags}"))
        );
        let records = text
            .lines()
            .filter_map(|line| line.strip_prefix("p01="))
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<Result<Vec<_>, _>>()
            .expect("valid P01 records");
        assert!(
            records
                .iter()
                .enumerate()
                .all(|(i, record)| record["sequence"].as_u64() == Some(i as u64)),
            "P01 records missing or out of order"
        );
        p01_boundary_check(&records, pid)
            .expect("P01 job observation violates boundary or is unknown");
        println!("P01 pid={pid} result={result:?}; observed records meet this case's boundary");
    }

    #[tokio::test]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w04_resource_limits() {
        fresh_group();
        control().await;
        real_limit("start-fuel", 1, false).await;
        real_limit("run-fuel", 1, false).await;
        real_limit("memory", 1, false).await;
        real_limit("memory", 2, true).await;
        real_limit("memory-declaration", 1, false).await;
        real_limit("memory-declaration", 2, true).await;
        case(
            "job-cpu",
            Expected::CpuLimit,
            "cpu_threads_started",
            None,
            false,
        )
        .await;
        case(
            "job-memory",
            Expected::Exit43,
            "memory_allocation_failed",
            None,
            false,
        )
        .await;
    }
    #[tokio::test]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w04_injected_zero_read_sensitivity() {
        fresh_group();
        control().await;
        test_hooks::arm_zero(true);
        case(
            "valid-output",
            Expected::Success,
            "output_written",
            Some("injected_stdout_zero"),
            false,
        )
        .await;
        test_hooks::arm_zero(false);
        case(
            "valid-stderr",
            Expected::Success,
            "stderr_written",
            Some("injected_stderr_zero"),
            false,
        )
        .await;
        let log = read_log(&PathBuf::from(
            std::env::var_os("NHC_W2_STAGE_LOG").unwrap(),
        ));
        let probe = read_log(
            &PathBuf::from(std::env::var_os("NHC_W2_FAULT_DIR").unwrap())
                .join("valid-stderr.stage"),
        );
        let pid: u32 = probe
            .lines()
            .find_map(|line| line.strip_prefix("pid=")?.split(' ').next()?.parse().ok())
            .unwrap();
        assert!(
            contains_pid(&log, "stderr_data", pid),
            "stderr after injected zero not read"
        );
    }

    async fn wait_until(mut predicate: impl FnMut() -> bool, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            if predicate() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        predicate()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w06_non_whitelisted_event() {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::{
                CompareObjectHandles, DUPLICATE_SAME_ACCESS, DuplicateHandle, GetHandleInformation,
            },
            Security::SECURITY_ATTRIBUTES,
            System::Threading::{
                CreateEventW, GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
        };
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let event_raw = unsafe { CreateEventW(&mut attributes, 1, 0, std::ptr::null()) };
        assert!(
            !event_raw.is_null(),
            "create sentinel: {:?}",
            std::io::Error::last_os_error()
        );
        let event = unsafe { OwnedHandle::from_raw_handle(event_raw) };
        let mut flags = 0u32;
        assert_ne!(
            unsafe { GetHandleInformation(event.as_raw_handle(), &mut flags) },
            0
        );
        assert_ne!(flags & 1, 0, "sentinel must actually be inheritable");
        assert_eq!(
            unsafe { WaitForSingleObject(event.as_raw_handle(), 0) },
            258
        );
        let log_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").expect("W06 stage"));
        assert!(!log_path.exists());
        let (service, probe_log) = fault_service("w06-sentinel");
        assert!(!probe_log.exists());
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let payload = format!("{nonce}:{}", event.as_raw_handle() as usize).into_bytes();
        let cancel = CancellationToken::new();
        let child_cancel = cancel.clone();
        test_observer::arm(false);
        let mut call = tokio::spawn(async move {
            service
                .call(manifest(), before(), payload, child_cancel)
                .await
        });
        let reached = wait_until(
            || read_log(&probe_log).contains(&format!("phase=sentinel_wait nonce={nonce}")),
            Duration::from_secs(3),
        )
        .await;
        let mut observed = test_observer::take();
        let mut evidence = Err("probe not ready or worker not observed".to_string());
        if reached && let Some((pid, process)) = observed.as_ref() {
            let original_identity = w05_identity(process);
            let duplicate_process = unsafe {
                OpenProcess(
                    PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    *pid,
                )
            };
            if duplicate_process.is_null() {
                evidence = Err(format!(
                    "worker duplicate access failed: {:?}",
                    std::io::Error::last_os_error().raw_os_error()
                ));
            } else {
                let duplicate_process = unsafe { OwnedHandle::from_raw_handle(duplicate_process) };
                let copied_identity = w05_identity(&duplicate_process);
                if !matches!((&original_identity, &copied_identity),
                    (Ok(a), Ok(b)) if a == b && a.0 == *pid)
                    || unsafe { WaitForSingleObject(duplicate_process.as_raw_handle(), 0) } != 258
                {
                    evidence = Err(format!(
                        "worker identity or liveness unknown: original={original_identity:?} copied={copied_identity:?}"
                    ));
                } else {
                    let mut copy = std::ptr::null_mut();
                    let ok = unsafe {
                        DuplicateHandle(
                            duplicate_process.as_raw_handle(),
                            event.as_raw_handle(),
                            GetCurrentProcess(),
                            &mut copy,
                            0,
                            0,
                            DUPLICATE_SAME_ACCESS,
                        )
                    };
                    let error = if ok == 0 {
                        std::io::Error::last_os_error().raw_os_error()
                    } else {
                        None
                    };
                    let same = if ok != 0 {
                        let copy = unsafe { OwnedHandle::from_raw_handle(copy) };
                        let mut copy_flags = 0;
                        if unsafe { GetHandleInformation(copy.as_raw_handle(), &mut copy_flags) }
                            == 0
                        {
                            None
                        } else {
                            Some(
                                unsafe {
                                    CompareObjectHandles(
                                        event.as_raw_handle(),
                                        copy.as_raw_handle(),
                                    )
                                } != 0,
                            )
                        }
                    } else {
                        None
                    };
                    let sentinel_wait = unsafe { WaitForSingleObject(event.as_raw_handle(), 0) };
                    evidence = if sentinel_wait == 258 && (error == Some(6) || same == Some(false))
                    {
                        Ok((error, same))
                    } else {
                        Err(format!(
                            "sentinel identity unknown or leaked: duplicate_error={error:?} same={same:?} event_wait={sentinel_wait}"
                        ))
                    };
                }
            }
        }
        cancel.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(8), &mut call).await;
        if observed.is_none() {
            observed = test_observer::take();
        }
        let process_signaled = observed.as_ref().is_some_and(|(_, h)| {
            (unsafe { WaitForSingleObject(h.as_raw_handle(), 0) }) == WAIT_OBJECT_0
        });
        if !process_signaled
            || !test_hooks::slot_free()
            || !matches!(outcome, Ok(Ok(ref result)) if !matches!(result, Err(WorkerError::CleanupUnknown)))
        {
            eprintln!(
                "NHC_TERMINAL_UNKNOWN: W06 sentinel cleanup unknown; retain original object and stop"
            );
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
        let (pid, _) = observed.expect("W06 worker observer");
        assert!(reached, "sentinel probe did not reach hold");
        assert!(read_log(&probe_log).contains(&format!("phase=sentinel_result nonce={nonce}")));
        assert_reaped(&read_log(&log_path), pid);
        println!("W06 non-whitelisted event pid={pid} evidence={evidence:?}");
        assert!(evidence.is_ok(), "{evidence:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w06_job_handle_not_inherited() {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::{
                CompareObjectHandles, DUPLICATE_SAME_ACCESS, DuplicateHandle, GetHandleInformation,
            },
            System::Threading::{
                GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            },
        };
        let log_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").expect("W06 stage"));
        assert!(!log_path.exists());
        let (service, probe_log) = fault_service("w06-job");
        let control = probe_log.with_extension("job");
        let temporary_control = probe_log.with_extension("job-tmp");
        assert!(!probe_log.exists() && !control.exists() && !temporary_control.exists());
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let cancel = CancellationToken::new();
        let child_cancel = cancel.clone();
        test_observer::arm(false);
        test_observer::arm_job_probe();
        let mut call = tokio::spawn(async move {
            service
                .call(manifest(), before(), wasm(), child_cancel)
                .await
        });
        let ready = wait_until(
            || read_log(&probe_log).contains("phase=job_control_wait"),
            Duration::from_secs(3),
        )
        .await;
        let job = test_observer::take_job();
        let mut observed = test_observer::take();
        let mut evidence = Err("job/probe observation not ready".to_string());
        if ready && let (Some(job), Some((pid, process))) = (job.as_ref(), observed.as_ref()) {
            let mut flags = 0u32;
            let flag_ok = unsafe { GetHandleInformation(job.as_raw_handle(), &mut flags) };
            if flag_ok == 0 || flags & 1 != 0 {
                evidence = Err(format!(
                    "job inherit flag unknown or enabled: ok={flag_ok} flags={flags}"
                ));
            } else if let Err(error) = fs::write(
                &temporary_control,
                format!("{nonce}:{}", job.as_raw_handle() as usize),
            )
            .and_then(|()| fs::rename(&temporary_control, &control))
            {
                evidence = Err(format!("job control write failed: {error}"));
            } else if !wait_until(
                || read_log(&probe_log).contains(&format!("phase=job_wait nonce={nonce}")),
                Duration::from_secs(1),
            )
            .await
            {
                evidence = Err("job probe did not enter hold".into());
            } else {
                let handle = unsafe {
                    OpenProcess(
                        PROCESS_DUP_HANDLE
                            | PROCESS_QUERY_LIMITED_INFORMATION
                            | PROCESS_SYNCHRONIZE,
                        0,
                        *pid,
                    )
                };
                if handle.is_null() {
                    evidence = Err(format!(
                        "worker duplicate access failed: {:?}",
                        std::io::Error::last_os_error().raw_os_error()
                    ));
                } else {
                    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
                    let original_identity = w05_identity(process);
                    let copied_identity = w05_identity(&handle);
                    if !matches!((&original_identity, &copied_identity), (Ok(a), Ok(b)) if a == b && a.0 == *pid)
                        || unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } != 258
                    {
                        evidence = Err(format!(
                            "worker identity unknown: original={original_identity:?} copied={copied_identity:?}"
                        ));
                    } else {
                        let mut copy = std::ptr::null_mut();
                        let ok = unsafe {
                            DuplicateHandle(
                                handle.as_raw_handle(),
                                job.as_raw_handle(),
                                GetCurrentProcess(),
                                &mut copy,
                                0,
                                0,
                                DUPLICATE_SAME_ACCESS,
                            )
                        };
                        let error = if ok == 0 {
                            std::io::Error::last_os_error().raw_os_error()
                        } else {
                            None
                        };
                        let same = if ok != 0 {
                            let copy = unsafe { OwnedHandle::from_raw_handle(copy) };
                            let mut copy_flags = 0;
                            if unsafe {
                                GetHandleInformation(copy.as_raw_handle(), &mut copy_flags)
                            } == 0
                            {
                                None
                            } else {
                                Some(
                                    unsafe {
                                        CompareObjectHandles(
                                            job.as_raw_handle(),
                                            copy.as_raw_handle(),
                                        )
                                    } != 0,
                                )
                            }
                        } else {
                            None
                        };
                        evidence = if error == Some(6) || same == Some(false) {
                            Ok((error, same))
                        } else {
                            Err(format!(
                                "job identity unknown or leaked: duplicate_error={error:?} same={same:?}"
                            ))
                        };
                    }
                }
            }
        }
        drop(job);
        cancel.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(8), &mut call).await;
        if observed.is_none() {
            observed = test_observer::take();
        }
        let process_signaled = observed.as_ref().is_some_and(|(_, h)| {
            (unsafe { WaitForSingleObject(h.as_raw_handle(), 0) }) == WAIT_OBJECT_0
        });
        if !process_signaled
            || !test_hooks::slot_free()
            || !matches!(outcome, Ok(Ok(ref result)) if !matches!(result, Err(WorkerError::CleanupUnknown)))
        {
            eprintln!(
                "NHC_TERMINAL_UNKNOWN: W06 job handle cleanup unknown; retain original objects and stop"
            );
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
        let (pid, _) = observed.expect("W06 worker observer");
        assert_reaped(&read_log(&log_path), pid);
        println!("W06 job handle pid={pid} evidence={evidence:?}");
        assert!(evidence.is_ok(), "{evidence:?}");
    }

    fn w05_identity(handle: &std::os::windows::io::OwnedHandle) -> Result<(u32, u64), String> {
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::Threading::{GetProcessId, GetProcessTimes},
        };
        let pid = unsafe { GetProcessId(handle.as_raw_handle()) };
        if pid == 0 {
            return Err(format!(
                "GetProcessId: {:?}",
                std::io::Error::last_os_error().raw_os_error()
            ));
        }
        let mut creation: FILETIME = unsafe { std::mem::zeroed() };
        let mut exit: FILETIME = unsafe { std::mem::zeroed() };
        let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
        let mut user: FILETIME = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            GetProcessTimes(
                handle.as_raw_handle(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        if ok == 0 {
            return Err(format!(
                "GetProcessTimes: {:?}",
                std::io::Error::last_os_error().raw_os_error()
            ));
        }
        Ok((
            pid,
            (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime),
        ))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w05_host_helper() {
        use std::io::{BufRead, Write};
        let scenario = std::env::var_os("NHC_W05_SCENARIO").expect("W05 helper scenario");
        let scenario = scenario.to_string_lossy().into_owned();
        let nonce = std::env::var("NHC_W05_NONCE").expect("W05 nonce");
        let helper_pid = std::process::id();
        let first = if scenario == "before" {
            "w05_before_create"
        } else {
            "w05_suspended"
        };
        let later = match scenario.as_str() {
            "resumed" => Some("w05_resumed"),
            "pending" => Some("w05_stdin_pending"),
            "before" | "suspended" => None,
            _ => panic!("unknown W05 scenario"),
        };
        let (service, probe) = match scenario.as_str() {
            "resumed" | "pending" => {
                let (service, stage) = fault_service("no-read");
                (service, Some(stage))
            }
            _ => (worker(), None),
        };
        test_hooks::w05_reset_stopper();
        if scenario != "before" {
            test_observer::arm(false);
        }
        let guard = test_hooks::arm(first);
        let cancellation = CancellationToken::new();
        let cancel_task = cancellation.clone();
        let start = Instant::now();
        let module = if scenario == "pending" {
            near_limit(wasm())
        } else {
            wasm()
        };
        let mut call = tokio::spawn(async move {
            service
                .call(manifest(), before(), module, cancel_task)
                .await
        });
        let mut observed = None;
        let mut status = "setup_failed";
        if wait_until(test_hooks::reached, Duration::from_secs(3)).await {
            if scenario != "before" {
                observed = test_observer::take();
            }
            let identity = observed.as_ref().and_then(|(_, h)| match w05_identity(h) {
                Ok(value) => Some(value),
                Err(error) => {
                    eprintln!("W05 identity failed: {error}");
                    None
                }
            });
            let worker_live = observed
                .as_ref()
                .is_none_or(|(_, h)| (unsafe { WaitForSingleObject(h.as_raw_handle(), 0) }) == 258);
            if (scenario == "before" || identity.is_some())
                && worker_live
                && !test_hooks::w05_stopper_started()
                && start.elapsed() < Duration::from_millis(3500)
            {
                let event = serde_json::json!({
                    "kind":"first", "nonce":nonce, "scenario":scenario,
                    "helper_pid":helper_pid, "phase":first, "elapsed_ms":start.elapsed().as_millis(),
                    "pid":identity.map(|v| v.0), "creation_filetime":identity.map(|v| v.1),
                    "stopper_started":false
                });
                println!("W05={event}");
                let _ = std::io::stdout().flush();
                let mut line = String::new();
                let received = std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
                let ack: Option<serde_json::Value> = serde_json::from_str(&line).ok();
                if received > 0
                    && ack.as_ref().is_some_and(|v| {
                        v["nonce"] == nonce
                            && v["scenario"] == scenario
                            && v["kind"] == "advance"
                            && v["helper_pid"].as_u64() == Some(helper_pid as u64)
                    })
                    && later.is_some()
                {
                    let phase = later.unwrap();
                    test_hooks::release_with_next(phase);
                    let reached =
                        wait_until(|| test_hooks::phase_reached(phase), Duration::from_secs(3))
                            .await;
                    let probe_started = wait_until(
                        || {
                            probe.as_ref().is_some_and(|path| {
                                identity.is_some_and(|(pid, _)| {
                                    probe_pid(&read_log(path), "holding_without_read", pid)
                                })
                            })
                        },
                        Duration::from_secs(1),
                    )
                    .await;
                    if reached
                        && probe_started
                        && !test_hooks::w05_stopper_started()
                        && start.elapsed() < Duration::from_millis(3500)
                        && observed.as_ref().is_some_and(|(_, h)| {
                            (unsafe { WaitForSingleObject(h.as_raw_handle(), 0) }) == 258
                        })
                    {
                        let event = serde_json::json!({
                            "kind":"final", "nonce":nonce, "scenario":scenario,
                            "helper_pid":helper_pid, "phase":phase,
                            "elapsed_ms":start.elapsed().as_millis(), "stopper_started":false,
                            "pid":identity.map(|v| v.0), "creation_filetime":identity.map(|v| v.1)
                        });
                        println!("W05={event}");
                        let _ = std::io::stdout().flush();
                        line.clear();
                        let _ = std::io::stdin().lock().read_line(&mut line);
                    }
                }
                status = "channel_closed_or_invalid";
            }
        }
        cancellation.cancel();
        test_hooks::release();
        let outcome = tokio::time::timeout(Duration::from_secs(8), &mut call).await;
        let worker_signaled = observed.as_ref().map(|(_, handle)| {
            (unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) }) == WAIT_OBJECT_0
        });
        let known_outcome = matches!(outcome, Ok(Ok(ref result)) if !matches!(result, Err(WorkerError::CleanupUnknown)));
        println!(
            "W05={}",
            serde_json::json!({"kind":"cleanup","nonce":nonce,"scenario":scenario,
                "helper_pid":helper_pid,"status":status,
                "call_returned":known_outcome,"worker_signaled":worker_signaled,
                "slot_free":test_hooks::slot_free()})
        );
        drop(guard);
    }

    struct W05Parent {
        child: std::process::Child,
        worker: Option<std::os::windows::io::OwnedHandle>,
        events: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        reader: Option<std::thread::JoinHandle<()>>,
        reader_failed: bool,
        nonce: String,
        scenario: String,
        helper_pid: u32,
        before_proven: bool,
        complete: bool,
    }
    impl W05Parent {
        fn finish_reader(&mut self) -> bool {
            if self.reader_failed {
                return false;
            }
            let Some(reader) = self.reader.take() else {
                return true;
            };
            let started = Instant::now();
            while !reader.is_finished() && started.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(5));
            }
            if !reader.is_finished() {
                self.reader = Some(reader);
                return false;
            }
            if reader.join().is_err() {
                self.reader_failed = true;
                return false;
            }
            true
        }
    }
    impl Drop for W05Parent {
        fn drop(&mut self) {
            if self.complete {
                return;
            }
            drop(self.child.stdin.take());
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(8) {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if !matches!(self.child.try_wait(), Ok(Some(_))) {
                let _ = self.child.kill();
            }
            let helper_wait = unsafe { WaitForSingleObject(self.child.as_raw_handle(), 2000) };
            let worker_wait = self
                .worker
                .as_ref()
                .map(|h| unsafe { WaitForSingleObject(h.as_raw_handle(), 5000) });
            let reader_finished = self.finish_reader();
            let cleanup = self
                .events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|v| {
                    v["kind"] == "cleanup"
                        && v["nonce"] == self.nonce
                        && v["scenario"] == self.scenario
                        && v["helper_pid"].as_u64() == Some(self.helper_pid as u64)
                        && v["call_returned"] == true
                        && v["slot_free"] == true
                        && (v["worker_signaled"] == true
                            || (self.scenario == "before" && v["worker_signaled"].is_null()))
                });
            let worker_known = worker_wait == Some(WAIT_OBJECT_0)
                || (self.worker.is_none() && (self.before_proven || cleanup));
            if helper_wait != WAIT_OBJECT_0 || !reader_finished || !worker_known {
                eprintln!(
                    "NHC_TERMINAL_UNKNOWN: W05 cleanup_unknown: retain observed objects and stop"
                );
                loop {
                    std::thread::park_timeout(Duration::from_secs(1));
                }
            }
        }
    }

    #[test]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    fn w05_host_kill_boundaries() {
        use std::{
            io::{BufRead, BufReader, Write},
            os::windows::io::FromRawHandle,
            process::{Command, Stdio},
            sync::{Arc, Mutex, mpsc},
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, TerminateProcess,
        };
        let root = PathBuf::from(std::env::var_os("NHC_W05_ROOT").expect("W05 fresh root"));
        for scenario in ["before", "suspended", "resumed", "pending"] {
            let stage = root.join(format!("{scenario}-host.stage"));
            assert!(!stage.exists(), "W05 stage must be fresh");
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            let mut command = Command::new(std::env::current_exe().expect("test executable"));
            command
                .args([
                    "--exact",
                    "wasm_worker_process_tests::fault_matrix::w05_host_helper",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("NHC_W05_SCENARIO", scenario)
                .env("NHC_W05_NONCE", &nonce)
                .env("NHC_W2_STAGE_LOG", &stage)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            if matches!(scenario, "resumed" | "pending") {
                command.env("NHC_W2_FAULT_DIR", root.join(scenario));
                assert!(!root.join(scenario).join("no-read.stage").exists());
            }
            let started = Instant::now();
            let mut child = command.spawn().expect("start W05 helper");
            let helper_pid = child.id();
            let stdout = child.stdout.take().expect("helper stdout");
            let events = Arc::new(Mutex::new(Vec::new()));
            let events_thread = events.clone();
            let (sender, receiver) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let line = line.expect("W05 stdout reader failed");
                    if let Some((_, json)) = line.split_once("W05=")
                        && let Ok(value) = serde_json::from_str::<serde_json::Value>(json)
                    {
                        events_thread
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(value.clone());
                        let _ = sender.send(value);
                    }
                }
            });
            let mut parent = W05Parent {
                child,
                worker: None,
                events,
                reader: Some(reader),
                reader_failed: false,
                nonce: nonce.clone(),
                scenario: scenario.to_string(),
                helper_pid,
                before_proven: false,
                complete: false,
            };
            let first = receiver
                .recv_timeout(Duration::from_secs(4))
                .expect("W05 first event");
            assert!(
                first["kind"] == "first"
                    && first["nonce"] == nonce
                    && first["scenario"] == scenario
                    && first["helper_pid"].as_u64() == Some(helper_pid as u64)
                    && first["stopper_started"] == false
            );
            assert_eq!(
                first["phase"],
                if scenario == "before" {
                    "w05_before_create"
                } else {
                    "w05_suspended"
                }
            );
            let identity = if scenario == "before" {
                None
            } else {
                let pid = first["pid"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .expect("worker PID");
                let creation = first["creation_filetime"]
                    .as_u64()
                    .expect("worker creation");
                let raw = unsafe {
                    OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                        0,
                        pid,
                    )
                };
                assert!(
                    !raw.is_null(),
                    "worker observation open failed: {:?}",
                    std::io::Error::last_os_error().raw_os_error()
                );
                let handle = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) };
                assert_eq!(
                    w05_identity(&handle),
                    Ok((pid, creation)),
                    "worker identity mismatch"
                );
                assert_eq!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
                    258
                );
                parent.worker = Some(handle);
                Some((pid, creation))
            };
            if matches!(scenario, "resumed" | "pending") {
                let ack = serde_json::json!({"kind":"advance","scenario":scenario,"nonce":nonce,"helper_pid":helper_pid});
                writeln!(parent.child.stdin.as_mut().expect("helper input"), "{ack}")
                    .expect("advance");
                parent
                    .child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .flush()
                    .expect("flush advance");
                let final_event = receiver
                    .recv_timeout(Duration::from_secs(4))
                    .expect("W05 final event");
                let (pid, creation) = identity.unwrap();
                assert!(
                    final_event["kind"] == "final"
                        && final_event["nonce"] == nonce
                        && final_event["scenario"] == scenario
                        && final_event["helper_pid"].as_u64() == Some(helper_pid as u64)
                        && final_event["pid"].as_u64() == Some(pid as u64)
                        && final_event["creation_filetime"].as_u64() == Some(creation)
                        && final_event["stopper_started"] == false
                );
                assert_eq!(
                    final_event["phase"],
                    if scenario == "resumed" {
                        "w05_resumed"
                    } else {
                        "w05_stdin_pending"
                    }
                );
            }
            assert!(
                started.elapsed() < Duration::from_millis(3500),
                "W05 deadline margin lost"
            );
            let host_log = read_log(&stage);
            assert!(!host_log.contains("phase=w05_gate_or_stopper_closed"));
            assert!(host_log.contains("phase=w05_job_active_limit pid=Some(0) count=1"));
            assert!(host_log.contains("phase=w05_job_handle_flags pid=Some(0) count="));
            assert_eq!(max_count(&host_log, "w05_job_handle_flags", 0) & 1, 0);
            assert!(host_log.contains("phase=w05_host_in_job pid=Some(0) count="));
            use windows_sys::Win32::System::JobObjects::{
                JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
            };
            let job_flags = max_count(&host_log, "w05_job_limit_flags", 0) as u32;
            assert_eq!(
                job_flags & (JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE),
                JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            );
            assert_eq!(
                job_flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK),
                0
            );
            let expected_phase = match scenario {
                "before" => "w05_before_create",
                "suspended" => "w05_suspended",
                "resumed" => "w05_resumed",
                _ => "w05_stdin_pending",
            };
            assert!(host_log.contains(&format!("phase={expected_phase} ")));
            assert_eq!(
                unsafe { WaitForSingleObject(parent.child.as_raw_handle(), 0) },
                258
            );
            if let Some(handle) = &parent.worker {
                assert_eq!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
                    258
                );
            }
            let kill_started_ms = started.elapsed().as_millis();
            let killed = unsafe { TerminateProcess(parent.child.as_raw_handle(), 1) };
            let kill_returned_ms = started.elapsed().as_millis();
            let kill_error = if killed == 0 {
                std::io::Error::last_os_error().raw_os_error()
            } else {
                None
            };
            println!(
                "W05 scenario={scenario} helper={helper_pid} worker={identity:?} terminate={killed} error={kill_error:?} kill_started_ms={kill_started_ms} kill_returned_ms={kill_returned_ms}"
            );
            assert_ne!(killed, 0, "helper termination failed: {kill_error:?}");
            if scenario == "before" && kill_returned_ms < 3500 {
                parent.before_proven = true;
            }
            assert!(
                kill_returned_ms < 3500,
                "W05 kill completed after deadline margin"
            );
            let helper_wait_start_ms = started.elapsed().as_millis();
            assert_eq!(
                unsafe { WaitForSingleObject(parent.child.as_raw_handle(), 5000) },
                WAIT_OBJECT_0
            );
            let helper_wait_end_ms = started.elapsed().as_millis();
            let worker_wait_start_ms = started.elapsed().as_millis();
            if let Some(handle) = &parent.worker {
                assert_eq!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) },
                    WAIT_OBJECT_0,
                    "worker cleanup unknown"
                );
            }
            let worker_wait_end_ms = started.elapsed().as_millis();
            println!(
                "W05 scenario={scenario} helper_wait=0 helper_wait_start_ms={helper_wait_start_ms} helper_wait_end_ms={helper_wait_end_ms} worker_wait={:?} worker_wait_start_ms={worker_wait_start_ms} worker_wait_end_ms={worker_wait_end_ms}",
                parent.worker.as_ref().map(|_| WAIT_OBJECT_0),
            );
            assert!(
                parent.finish_reader(),
                "helper stdout reader cleanup unknown"
            );
            parent.complete = true;
        }
    }
    fn cancelled_creation_failure_evidence(log: &str) -> Result<(usize, usize), String> {
        fn count(log: &str, phase: &str) -> Result<usize, String> {
            let prefix = format!("phase={phase} pid=Some(0) count=");
            let values: Vec<_> = log
                .lines()
                .filter_map(|line| line.strip_prefix(&prefix))
                .collect();
            if values.len() != 1 {
                return Err(format!("{phase}: expected exactly one observation"));
            }
            values[0]
                .parse()
                .map_err(|_| format!("{phase}: invalid count"))
        }
        let ordered = [
            "phase=create_ticket_granted ",
            "phase=create_after_ticket ",
            "phase=cleanup_unknown ",
            "phase=injection_released ",
            "phase=create_flags ",
            "phase=create_failed ",
            "phase=stopper_joined ",
            "phase=create_failed_job_query ",
            "phase=supervisor_platform_returned ",
        ];
        let mut previous = 0;
        for phase in ordered {
            let position = log.find(phase).ok_or_else(|| format!("missing {phase}"))?;
            if position < previous {
                return Err(format!("out of order: {phase}"));
            }
            previous = position;
        }
        for forbidden in [
            "phase=created_suspended ",
            "phase=created_process_handle_unknown ",
            "phase=resume_ticket_granted ",
            "phase=resume_api_return ",
            "phase=resumed ",
            "phase=submit_ticket_granted ",
            "phase=stdin_pending ",
            "phase=cancel_io_return ",
            "phase=stdin_io_settled ",
            "phase=stdout_io_settled ",
            "phase=stderr_io_settled ",
            "phase=stopper_join_failed ",
            "phase=injection_timeout ",
        ] {
            if log.contains(forbidden) {
                return Err(format!("unexpected {forbidden}"));
            }
        }
        let error = count(log, "create_failed_win32_error")?;
        if error == 0
            || count(log, "stopper_terminate_job_return")? != 1
            || count(log, "create_failed_job_query")? != 1
            || count(log, "create_failed_job_active")? != 0
            || count(log, "create_failed_job_total")? > 1
        {
            return Err("create error, Stopper, or Job evidence incomplete".into());
        }
        Ok((error, count(log, "create_flags")?))
    }

    async fn unknown_case(phase: &'static str, force_create_failure: bool) {
        let log_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").unwrap());
        let offset = read_log(&log_path).len();
        let (service, _) = fault_service("no-read");
        let token = CancellationToken::new();
        let gate = test_hooks::arm(phase);
        test_observer::arm(false);
        let invalid_flags = force_create_failure.then(test_hooks::arm_invalid_create_flags);
        let service_task = service.clone();
        let token_task = token.clone();
        let mut task = tokio::spawn(async move {
            service_task
                .call(manifest(), before(), near_limit(wasm()), token_task)
                .await
        });
        let mut pending_before_cancel = false;
        if phase == "cancel_after_api" {
            let pending = wait_until(
                || read_log(&log_path)[offset..].contains("phase=stdin_pending "),
                Duration::from_secs(4),
            )
            .await;
            if pending {
                pending_before_cancel = true;
                token.cancel();
            }
        }
        let reached = wait_until(test_hooks::reached, Duration::from_secs(4)).await;
        let busy_concurrent = service
            .call(manifest(), before(), Vec::new(), CancellationToken::new())
            .await;
        token.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(4), &mut task).await;
        let busy_a = service
            .call(manifest(), before(), Vec::new(), CancellationToken::new())
            .await;
        drop(service);
        let (rebuilt, _) = fault_service("no-read");
        let busy_b = rebuilt
            .call(manifest(), before(), Vec::new(), CancellationToken::new())
            .await;
        let before_release = read_log(&log_path);
        let created_before = before_release[offset..]
            .matches("phase=created_suspended ")
            .count();
        let observed_early = if phase == "observation_injected" {
            test_observer::take()
        } else {
            None
        };
        let event = if phase == "observation_injected" {
            test_observer::take_event()
        } else {
            None
        };
        let process_wait_before = observed_early
            .as_ref()
            .map(|(_, h)| unsafe { WaitForSingleObject(h.as_raw_handle(), 0) });
        let event_wait_before = event
            .as_ref()
            .map(|h| unsafe { WaitForSingleObject(h.as_raw_handle(), 0) });
        test_hooks::release();
        let free = wait_until(test_hooks::slot_free, Duration::from_secs(8)).await;
        let hook_ok = test_hooks::completed_without_timeout();
        drop(gate);
        let observed = observed_early.or_else(test_observer::take);
        if !free {
            eprintln!("NHC_TERMINAL_UNKNOWN: supervisor did not release its slot");
            loop {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
        assert!(
            reached && free && hook_ok,
            "{phase}: reached={reached} free={free} hook_ok={hook_ok}"
        );
        assert!(
            matches!(outcome, Ok(Ok(Err(WorkerError::CleanupUnknown)))),
            "{phase}: {outcome:?}"
        );
        assert!(
            matches!(busy_concurrent, Err(WorkerError::Busy))
                && matches!(busy_a, Err(WorkerError::Busy))
                && matches!(busy_b, Err(WorkerError::Busy)),
            "{phase}: concurrent={busy_concurrent:?}, busy A={busy_a:?}, B={busy_b:?}"
        );
        assert!(created_before <= 1, "{phase}: second worker was created");
        if phase == "create_after_ticket" && observed.is_none() {
            let text = read_log(&log_path);
            let log = &text[offset..];
            let (error, flags) = match cancelled_creation_failure_evidence(log) {
                Ok(evidence) => evidence,
                Err(reason) => {
                    eprintln!("NHC_TERMINAL_UNKNOWN: creation failure evidence: {reason}");
                    loop {
                        std::thread::park_timeout(Duration::from_secs(1));
                    }
                }
            };
            if !test_observer::disarm_after_return_without_capture() {
                eprintln!("NHC_TERMINAL_UNKNOWN: creation observer was consumed or captured");
                loop {
                    std::thread::park_timeout(Duration::from_secs(1));
                }
            }
            if force_create_failure {
                assert!(log.contains("phase=fixture_invalid_create_flags "));
                use windows_sys::Win32::System::Threading::{CREATE_NEW_CONSOLE, DETACHED_PROCESS};
                assert_eq!(
                    flags & (CREATE_NEW_CONSOLE | DETACHED_PROCESS) as usize,
                    (CREATE_NEW_CONSOLE | DETACHED_PROCESS) as usize,
                    "fixture must pass the actual incompatible flags"
                );
                assert_eq!(
                    error,
                    windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER as usize,
                    "real invalid flag combination must return ERROR_INVALID_PARAMETER"
                );
            } else {
                assert!(!log.contains("phase=fixture_invalid_create_flags "));
            }
            println!(
                "unknown {phase} cancelled_create_rejected=true win32_error={error} flags={flags} job_active=0"
            );
            drop(invalid_flags);
            control().await;
            return;
        }
        let (pid, process) = observed.expect("created worker was observed");
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
            WAIT_OBJECT_0,
            "{phase}: process not signaled"
        );
        let text = read_log(&log_path);
        let log = &text[offset..];
        assert_reaped(log, pid);
        assert!(
            !force_create_failure,
            "invalid creation flags unexpectedly created a process; held object was reaped"
        );
        for item in [
            "created_suspended",
            "failure_reaped",
            "run_handles_released",
        ] {
            assert!(
                contains_pid(log, item, pid),
                "{phase}: missing {item}: {log}"
            );
        }
        assert!(!log.contains("phase=injection_timeout "));
        if matches!(phase, "create_after_ticket" | "create_returned_suspended") {
            assert!(
                !contains_pid(log, "resumed", pid),
                "late created worker was resumed"
            );
            for forbidden in [
                "resume_ticket_granted",
                "resume_api_return",
                "submit_ticket_granted",
                "stdin_pending",
                "stdin_io_settled",
                "stdout_io_settled",
                "stderr_io_settled",
            ] {
                assert!(
                    !log.lines().any(|line| line.starts_with(&format!("phase={forbidden} pid=Some({pid})"))),
                    "late suspended worker reached {forbidden}"
                );
            }
        }
        if phase == "observation_injected" {
            assert!(contains_pid(log, "kernel_write_event_unsignaled", pid));
            assert_eq!(
                process_wait_before,
                Some(windows_sys::Win32::Foundation::WAIT_TIMEOUT)
            );
            assert_eq!(
                event_wait_before,
                Some(windows_sys::Win32::Foundation::WAIT_TIMEOUT)
            );
            assert!(before_release[offset..].contains("phase=injected_stopper_held"));
            for item in ["stdin_io_settled", "stdout_io_settled", "stderr_io_settled"] {
                assert!(contains_pid(log, item, pid), "missing {item}");
            }
        }
        if phase == "cancel_after_api" {
            assert!(
                pending_before_cancel,
                "stdin write never observed pending before cancellation"
            );
            assert!(
                before_release[offset..]
                    .contains(&format!("phase=cancel_io_return pid=Some({pid}) count="))
            );
            assert!(
                !contains_pid(&before_release[offset..], "stdin_io_settled", pid),
                "return was mistaken for a harvested completion"
            );
            assert!(contains_pid(log, "stdin_io_settled", pid));
        }
        if phase == "result_observation_injected" {
            assert!(!contains_pid(
                &before_release[offset..],
                "stdin_io_settled",
                pid
            ));
            assert!(contains_pid(log, "stdin_io_settled", pid));
        }
        println!("unknown {phase} pid={pid} busy_before_release=2 actual_reaped=true");
        drop(invalid_flags);
        control().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w04_unknown_cleanup_and_slot_reuse() {
        fresh_group();
        let real = worker()
            .call(manifest(), before(), wasm(), CancellationToken::new())
            .await
            .expect("real worker positive control");
        assert_eq!(real.decision, Decision::UseTools { indices: vec![1] });
        assert!(real.process_id != 0 && real.process_signaled && real.io_completed);
        assert_eq!((real.exit_code, real.job_active_processes), (0, 0));
        assert_reaped(
            &read_log(&PathBuf::from(
                std::env::var_os("NHC_W2_STAGE_LOG").expect("stage log"),
            )),
            real.process_id,
        );
        println!("W04 real worker positive pid={}", real.process_id);
        control().await;
        unknown_case("create_after_ticket", true).await;
        for phase in [
            "create_after_ticket",
            "create_returned_suspended",
            "submit_after_ticket",
            "submit_returned_pending",
            "observation_injected",
            "result_observation_injected",
            "cancel_after_api",
        ] {
            unknown_case(phase, false).await;
        }
    }

    async fn boundary_case(phase: &'static str, drop_future: bool) {
        let log_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").unwrap());
        let offset = read_log(&log_path).len();
        let service = worker();
        let cancel = CancellationToken::new();
        let gate = test_hooks::arm(phase);
        if phase != "create_before_ticket" {
            test_observer::arm(false);
        }
        let call_service = service.clone();
        let call_cancel = cancel.clone();
        let mut task = tokio::spawn(async move {
            call_service
                .call(manifest(), before(), wasm(), call_cancel)
                .await
        });
        let reached = wait_until(test_hooks::reached, Duration::from_secs(4)).await;
        let mut dropped = false;
        let mut drop_busy = None;
        if drop_future {
            task.abort();
            dropped = matches!((&mut task).await, Err(ref e) if e.is_cancelled());
            test_hooks::mark("future_dropped");
            drop_busy = Some(
                service
                    .call(manifest(), before(), Vec::new(), CancellationToken::new())
                    .await,
            );
        } else {
            cancel.cancel();
            test_hooks::mark("cancellation_sent");
        }
        test_hooks::release();
        let outcome = if drop_future {
            None
        } else {
            Some(tokio::time::timeout(Duration::from_secs(8), &mut task).await)
        };
        let free = wait_until(test_hooks::slot_free, Duration::from_secs(8)).await;
        let hook_ok = test_hooks::completed_without_timeout();
        drop(gate);
        let observed = if phase == "create_before_ticket" {
            None
        } else {
            test_observer::take()
        };
        assert!(
            reached && free && hook_ok,
            "{phase}: reached={reached} free={free} hook_ok={hook_ok}"
        );
        if drop_future {
            assert!(dropped, "abort requested but future not dropped");
            assert!(
                matches!(drop_busy, Some(Err(WorkerError::Busy))),
                "future drop released occupied slot: {drop_busy:?}"
            );
        } else {
            assert!(
                matches!(outcome, Some(Ok(Ok(Err(WorkerError::Cancelled))))),
                "{phase}: {outcome:?}"
            );
        }
        let text = read_log(&log_path);
        let log = &text[offset..];
        let marker = if drop_future {
            "phase=future_dropped"
        } else {
            "phase=cancellation_sent"
        };
        let after = log.split_once(marker).expect("cancellation marker").1;
        let forbidden = match phase {
            "create_before_ticket" => Some("phase=create_ticket_granted"),
            "resume_before_ticket" => Some("phase=resume_ticket_granted"),
            "submit_before_ticket" => Some("phase=submit_ticket_granted"),
            _ => None,
        };
        if let Some(forbidden) = forbidden {
            assert!(
                !after.contains(forbidden),
                "{phase}: ticket after cancellation: {log}"
            );
        }
        if phase == "create_before_ticket" {
            assert!(!log.contains("phase=created_suspended"));
        }
        if let Some((pid, process)) = observed {
            assert_eq!(
                unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
                WAIT_OBJECT_0
            );
            assert!(
                contains_pid(log, "failure_reaped", pid)
                    && contains_pid(log, "run_handles_released", pid),
                "{phase}: {log}"
            );
            assert_reaped(log, pid);
        } else if phase != "create_before_ticket" {
            panic!("{phase}: missing observer");
        }
        control().await;
    }
    async fn acceptance_case(phase: &'static str) {
        let log_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").unwrap());
        let offset = read_log(&log_path).len();
        let service = worker();
        let token = CancellationToken::new();
        let gate = test_hooks::arm(phase);
        test_observer::arm(false);
        let service_task = service.clone();
        let token_task = token.clone();
        let mut task = tokio::spawn(async move {
            service_task
                .call(manifest(), before(), wasm(), token_task)
                .await
        });
        let reached = wait_until(test_hooks::reached, Duration::from_secs(4)).await;
        token.cancel();
        test_hooks::release();
        let outcome = tokio::time::timeout(Duration::from_secs(8), &mut task).await;
        let free = wait_until(test_hooks::slot_free, Duration::from_secs(8)).await;
        let hook_ok = test_hooks::completed_without_timeout();
        drop(gate);
        assert!(
            reached && free && hook_ok,
            "{phase}: reached={reached} free={free} hook_ok={hook_ok}"
        );
        if phase == "accept_before" {
            assert!(
                matches!(outcome, Ok(Ok(Err(WorkerError::Cancelled)))),
                "{phase}: {outcome:?}"
            );
        } else {
            assert!(matches!(outcome, Ok(Ok(Ok(_)))), "{phase}: {outcome:?}");
        }
        assert_accepted_process(&log_path, offset, phase);
        control().await;
    }
    fn assert_accepted_process(log_path: &std::path::Path, offset: usize, phase: &str) {
        let (pid, process) = test_observer::take().expect("accepted worker observer");
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
        let text = read_log(log_path);
        let log = &text[offset..];
        assert_reaped(log, pid);
        for item in [
            "created_suspended",
            "resumed",
            "io_complete_reaped",
            "stdin_io_settled",
            "stdout_io_settled",
            "stderr_io_settled",
            phase,
        ] {
            assert!(contains_pid(log, item, pid), "missing {item}: {log}");
        }
    }
    async fn acceptance_deadline_case() {
        let log_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").unwrap());
        let offset = read_log(&log_path).len();
        let service = worker();
        let gate = test_hooks::arm("accept_before");
        test_observer::arm(false);
        let mut task = tokio::spawn(async move {
            service
                .call(manifest(), before(), wasm(), CancellationToken::new())
                .await
        });
        let reached = wait_until(test_hooks::reached, Duration::from_secs(4)).await;
        tokio::time::sleep(Duration::from_millis(5200)).await;
        test_hooks::release();
        let outcome = tokio::time::timeout(Duration::from_secs(8), &mut task).await;
        let free = wait_until(test_hooks::slot_free, Duration::from_secs(8)).await;
        let hook_ok = test_hooks::completed_without_timeout();
        drop(gate);
        assert!(reached && free && hook_ok);
        assert!(
            matches!(outcome, Ok(Ok(Err(WorkerError::Deadline)))),
            "late result: {outcome:?}"
        );
        assert_accepted_process(&log_path, offset, "accept_before");
        control().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w04_cancellation_and_acceptance_boundaries() {
        fresh_group();
        control().await;
        let stage_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").unwrap());
        let before_pre_cancel = read_log(&stage_path);
        let pre_cancel = CancellationToken::new();
        pre_cancel.cancel();
        assert!(matches!(
            worker()
                .call(manifest(), before(), wasm(), pre_cancel)
                .await,
            Err(WorkerError::Cancelled)
        ));
        assert_eq!(
            before_pre_cancel,
            read_log(&stage_path),
            "pre-cancel started supervision"
        );
        for phase in [
            "create_before_ticket",
            "resume_before_ticket",
            "submit_before_ticket",
        ] {
            boundary_case(phase, false).await;
        }
        boundary_case("resume_after_ticket", false).await;
        boundary_case("submit_after_ticket", true).await;
        acceptance_case("accept_before").await;
        acceptance_case("accept_after").await;
        acceptance_deadline_case().await;
    }

    #[tokio::test]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w04_injected_membership_failure_reaps() {
        fresh_group();
        control().await;
        let log_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").unwrap());
        let offset = read_log(&log_path).len();
        test_hooks::arm_membership_failure();
        test_observer::arm(false);
        let result = worker()
            .call(manifest(), before(), wasm(), CancellationToken::new())
            .await;
        let (pid, observed) = test_observer::take().expect("worker observation handle");
        let text = read_log(&log_path);
        let log = &text[offset..];
        assert_reaped(log, pid);
        assert!(
            matches!(
                result,
                Err(WorkerError::IsolationUnavailable("worker_not_in_job", None))
            ),
            "{result:?}"
        );
        assert_eq!(
            unsafe { WaitForSingleObject(observed.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
        assert!(log.contains(&format!("phase=membership_actual pid=Some({pid}) count=1")));
        for phase in [
            "created_suspended",
            "membership_failure_injected",
            "failure_reaped",
            "run_handles_released",
        ] {
            assert!(contains_pid(log, phase, pid), "missing {phase}: {log}");
        }
        assert!(!contains_pid(log, "resumed", pid));
        assert!(log.contains(&format!(
            "phase=terminate_process_return pid=Some({pid}) count="
        )));
        control().await;
    }

    async fn persistent_error_case(result_error: bool) {
        let log_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").unwrap());
        let offset = read_log(&log_path).len();
        let (service, _) = fault_service("no-read");
        let token = CancellationToken::new();
        let injection = test_hooks::arm_persistent_error(result_error);
        test_observer::arm(false);
        let service_task = service.clone();
        let token_task = token.clone();
        let mut task = tokio::spawn(async move {
            service_task
                .call(manifest(), before(), near_limit(wasm()), token_task)
                .await
        });
        let reached = wait_until(test_hooks::persistent_reached, Duration::from_secs(4)).await;
        let busy_concurrent = service
            .call(manifest(), before(), Vec::new(), CancellationToken::new())
            .await;
        token.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(4), &mut task).await;
        let busy_a = service
            .call(manifest(), before(), Vec::new(), CancellationToken::new())
            .await;
        drop(service);
        let (rebuilt, _) = fault_service("no-read");
        let busy_b = rebuilt
            .call(manifest(), before(), Vec::new(), CancellationToken::new())
            .await;
        let prior = read_log(&log_path);
        test_hooks::release_persistent_error();
        let free = wait_until(test_hooks::slot_free, Duration::from_secs(8)).await;
        drop(injection);
        let observed = test_observer::take();
        assert!(
            reached && free,
            "injection reached={reached}, recovered={free}"
        );
        assert!(
            matches!(outcome, Ok(Ok(Err(WorkerError::CleanupUnknown)))),
            "{outcome:?}"
        );
        assert!(
            matches!(busy_concurrent, Err(WorkerError::Busy))
                && matches!(busy_a, Err(WorkerError::Busy))
                && matches!(busy_b, Err(WorkerError::Busy))
        );
        let (pid, process) = observed.expect("actual worker observed");
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
            WAIT_OBJECT_0
        );
        let before_release = &prior[offset..];
        let marker = if result_error {
            "persistent_injected_result_error"
        } else {
            "persistent_injected_event_error"
        };
        assert!(contains_pid(before_release, marker, pid));
        assert!(before_release.contains(&format!("phase=cancel_io_return pid=Some({pid}) count=")));
        assert!(contains_pid(before_release, "stdin_cancel_returned", pid));
        assert!(
            !contains_pid(before_release, "stdin_io_settled", pid),
            "unknown observation must retain OVERLAPPED"
        );
        assert_eq!(
            before_release.matches("phase=created_suspended ").count(),
            1
        );
        let text = read_log(&log_path);
        let log = &text[offset..];
        assert_reaped(log, pid);
        for phase in [
            "stdin_io_settled",
            "stdout_io_settled",
            "stderr_io_settled",
            "failure_reaped",
            "run_handles_released",
        ] {
            assert!(contains_pid(log, phase, pid), "missing {phase}: {log}");
        }
        println!("persistent {marker} pid={pid} retained_until_release=true");
        control().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires controlled worker snapshots and isolated process observation"]
    async fn w04_persistent_observation_errors_retain_io_and_slot() {
        fresh_group();
        control().await;
        persistent_error_case(false).await;
        persistent_error_case(true).await;
    }
}
