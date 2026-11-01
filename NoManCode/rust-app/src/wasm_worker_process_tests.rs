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
    #[tokio::test]
    async fn w04_resource_limits() {
        fresh_group();
        control().await;
        real_limit("start-fuel", 1, false).await;
        real_limit("run-fuel", 1, false).await;
        real_limit("memory", 1, false).await;
        real_limit("memory", 2, true).await;
        real_limit("memory-declaration", 1, false).await;
        real_limit("memory-declaration", 2, true).await;
        case("job-cpu", Expected::CpuLimit, "cpu_loop_start", None, false).await;
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
    async fn unknown_case(phase: &'static str) {
        let log_path = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").unwrap());
        let offset = read_log(&log_path).len();
        let (service, _) = fault_service("no-read");
        let token = CancellationToken::new();
        let gate = test_hooks::arm(phase);
        test_observer::arm(false);
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
        let (pid, process) = observed.expect("created worker was observed");
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
            WAIT_OBJECT_0,
            "{phase}: process not signaled"
        );
        let text = read_log(&log_path);
        let log = &text[offset..];
        assert_reaped(log, pid);
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
        if phase == "create_returned_suspended" {
            assert!(
                !contains_pid(log, "resumed", pid),
                "late created worker was resumed"
            );
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
        control().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn w04_unknown_cleanup_and_slot_reuse() {
        fresh_group();
        control().await;
        for phase in [
            "create_after_ticket",
            "create_returned_suspended",
            "submit_after_ticket",
            "submit_returned_pending",
            "observation_injected",
            "result_observation_injected",
            "cancel_after_api",
        ] {
            unknown_case(phase).await;
        }
    }

    fn test_mark(path: &std::path::Path, marker: &str) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "phase={marker} pid=None").unwrap();
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
            test_mark(&log_path, "future_dropped");
            drop_busy = Some(
                service
                    .call(manifest(), before(), Vec::new(), CancellationToken::new())
                    .await,
            );
        } else {
            cancel.cancel();
            test_mark(&log_path, "cancellation_sent");
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
    async fn w04_persistent_observation_errors_retain_io_and_slot() {
        fresh_group();
        control().await;
        persistent_error_case(false).await;
        persistent_error_case(true).await;
    }
}
