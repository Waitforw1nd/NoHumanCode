//! Fixed-basename fault worker, used only by the W02/W03 test harness.
#[cfg(windows)]
fn main() {
    use peachsh::wasm_worker_protocol::{
        self as protocol, Decision, Error, Outcome, Response, Vote,
    };
    use std::{
        fs::OpenOptions,
        io::{self, Read, Write},
        thread,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Storage::FileSystem::WriteFile,
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };

    let path = std::env::current_exe().expect("probe path");
    let mode = path
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("probe basename");
    let modes = [
        "control",
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
        "rejected",
        "exit-output",
        "exit-empty",
        "hold-valid",
        "hold-prefix",
        "delayed",
        "zero-stdout",
        "zero-stderr",
        "stdout-limit",
        "stderr-limit",
        "no-read",
        "partial-read",
        "partial-stdout",
        "closed-stdout",
        "concurrent",
        "dual-pressure",
        "job-cpu",
        "job-memory",
        "p01-memory",
        "valid-output",
        "valid-stderr",
    ];
    assert!(modes.contains(&mode), "unknown probe mode");
    let stage_path = path.with_extension("stage");
    let mark = |stage: &str| {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stage_path)
            .expect("probe stage");
        writeln!(f, "pid={} phase={stage}", std::process::id()).expect("write probe stage");
        f.flush().expect("flush probe stage");
    };
    let hold = || thread::sleep(Duration::from_secs(20));
    mark("started");
    if mode == "no-read" {
        mark("holding_without_read");
        hold();
        return;
    }
    if mode == "partial-read" {
        let mut b = [0u8; 32];
        io::stdin().read_exact(&mut b).expect("partial stdin");
        mark("read_32");
        let release = path.with_extension("release");
        let started = std::time::Instant::now();
        while !release.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "partial read barrier timed out"
            );
            thread::sleep(Duration::from_millis(2));
        }
        mark("pending_confirmed");
        assert_ne!(unsafe { CloseHandle(GetStdHandle(STD_INPUT_HANDLE)) }, 0);
        mark("stdin_closed");
        hold();
        return;
    }
    if mode == "concurrent" {
        mark("concurrent_start");
        for _ in 0..8 {
            io::stdout()
                .write_all(&[b'o'; 4096])
                .expect("concurrent stdout");
            io::stderr()
                .write_all(&[b'e'; 4096])
                .expect("concurrent stderr");
        }
        mark("concurrent_written");
        hold();
        return;
    }
    if mode == "dual-pressure" {
        let marker_lock = std::sync::Mutex::new(());
        thread::scope(|scope| {
            scope.spawn(|| {
                {
                    let _guard = marker_lock.lock().unwrap();
                    mark("pressure_stdout_attempt");
                }
                let _ = io::stdout().write_all(&vec![b'o'; 128 * 1024]);
            });
            scope.spawn(|| {
                {
                    let _guard = marker_lock.lock().unwrap();
                    mark("pressure_stderr_attempt");
                }
                let _ = io::stderr().write_all(&vec![b'e'; 128 * 1024]);
            });
        });
        mark("pressure_writes_finished");
        hold();
        return;
    }
    let request =
        protocol::read_request(&mut io::stdin().lock()).expect("complete request and EOF");
    mark("request_eof");
    if mode == "job-cpu" {
        mark("cpu_loop_start");
        let marker_lock = std::sync::Mutex::new(());
        thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    {
                        let _guard = marker_lock.lock().unwrap();
                        mark("cpu_thread_started");
                    }
                    loop {
                        std::hint::black_box(1usize.wrapping_add(std::hint::black_box(1)));
                    }
                });
            }
            {
                let _guard = marker_lock.lock().unwrap();
                mark("cpu_threads_started");
            }
        });
    }
    if mode == "p01-memory" {
        mark("p01_wait");
        let release = path.with_extension("release");
        let started = std::time::Instant::now();
        while !release.exists() && started.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(2));
        }
        if !release.exists() {
            mark("p01_wait_timeout");
            return;
        }
        mark("p01_released");
    }
    if matches!(mode, "job-memory" | "p01-memory") {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
        };
        mark("memory_allocation_start");
        let mut failure_log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stage_path)
            .expect("open memory failure log");
        let mut blocks = Vec::new();
        loop {
            let allocation = unsafe {
                VirtualAlloc(
                    std::ptr::null(),
                    4 * 1024 * 1024,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                )
            };
            if allocation.is_null() {
                let error = io::Error::last_os_error().raw_os_error().unwrap_or(0);
                writeln!(
                    failure_log,
                    "pid={} phase=memory_allocation_failed count={} error={error}",
                    std::process::id(),
                    blocks.len() * 4 * 1024 * 1024
                )
                .unwrap();
                writeln!(
                    failure_log,
                    "pid={} phase=memory_allocation_failed",
                    std::process::id()
                )
                .unwrap();
                failure_log.flush().unwrap();
                std::process::exit(43);
            }
            blocks.push(allocation);
            assert!(
                blocks.len() <= 64,
                "memory limit failed to constrain allocation"
            );
        }
    }
    if mode == "exit-empty" {
        mark("exit_42");
        std::process::exit(42);
    }
    let mut response = Response {
        manifest: request.manifest,
        outcome: Outcome::Ok {
            decision: Decision::Stop {},
        },
    };
    match mode {
        "id-version" => response.manifest.version += 1,
        "id-request" => response.manifest.request_id = "different".into(),
        "id-nonce" => {
            response.manifest.nonce = if response.manifest.nonce == "0".repeat(32) {
                "1".repeat(32)
            } else {
                "0".repeat(32)
            }
        }
        "id-generation" => response.manifest.binding_generation += 1,
        "id-plugin" => response.manifest.plugin_id = "other".into(),
        "id-plugin-version" => response.manifest.plugin_version = "2.0.0".into(),
        "id-digest" => {
            response.manifest.digest = if response.manifest.digest == "0".repeat(64) {
                "1".repeat(64)
            } else {
                "0".repeat(64)
            }
        }
        "id-fuel" => response.manifest.fuel += 1,
        "id-memory" => response.manifest.memory_pages += 1,
        "decision-phase" => {
            response.outcome = Outcome::Ok {
                decision: Decision::Finish {},
            }
        }
        "decision-index" => {
            response.outcome = Outcome::Ok {
                decision: Decision::UseTools { indices: vec![99] },
            }
        }
        "decision-duplicate" => {
            response.outcome = Outcome::Ok {
                decision: Decision::UseTools {
                    indices: vec![0, 0],
                },
            }
        }
        "decision-order" => {
            response.outcome = Outcome::Ok {
                decision: Decision::UseTools {
                    indices: vec![1, 0],
                },
            }
        }
        "decision-no-tools" | "decision-votes" => {
            response.outcome = Outcome::Ok {
                decision: Decision::ToolVotes {
                    votes: vec![Vote::Deny],
                },
            }
        }
        "rejected" => {
            response.outcome = Outcome::Error {
                error: Error::Module,
            }
        }
        _ => {}
    }
    let mut frame = Vec::new();
    protocol::write_response(&mut frame, &response).expect("encode response");
    let raw_frame = |data: &[u8]| {
        let mut v = (data.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(data);
        v
    };
    let mut bytes = match mode {
        "short-header" => vec![0, 0, 1],
        "short-header-1" => vec![0],
        "short-header-2" => vec![0, 0],
        "short-payload" => {
            let mut v = ((frame.len() - 4 + 1) as u32).to_be_bytes().to_vec();
            v.extend_from_slice(&frame[4..]);
            v
        }
        "zero-length" => 0u32.to_be_bytes().to_vec(),
        "long-length" | "hold-prefix" => {
            ((protocol::MAX_RESPONSE + 1) as u32).to_be_bytes().to_vec()
        }
        "bad-utf8" => raw_frame(&[0xff]),
        "bad-json" => raw_frame(b"{"),
        "unknown-field" => {
            let mut json = serde_json::to_vec(&response).unwrap();
            json.pop();
            json.extend_from_slice(b",\"unknown\":1}");
            raw_frame(&json)
        }
        "duplicate-field" => {
            let mut json = serde_json::to_vec(&response).unwrap();
            json.pop();
            json.extend_from_slice(b",\"manifest\":");
            json.extend_from_slice(&serde_json::to_vec(&response.manifest).unwrap());
            json.push(b'}');
            raw_frame(&json)
        }
        "partial-stdout" => frame[..2].to_vec(),
        "closed-stdout" | "stderr-limit" => Vec::new(),
        "stdout-limit" => vec![b'x'; protocol::MAX_RESPONSE + 5],
        _ => frame.clone(),
    };
    if mode == "duplicate-frame" {
        bytes.extend_from_slice(&frame);
    }
    if mode == "trailing" {
        bytes.push(b'x');
    }
    if mode == "closed-stdout" {
        assert_ne!(unsafe { CloseHandle(GetStdHandle(STD_OUTPUT_HANDLE)) }, 0);
        mark("stdout_closed");
        hold();
        return;
    }
    if mode == "delayed" {
        mark("delayed_wait");
        thread::sleep(Duration::from_secs(8));
        mark("delayed_send_attempt");
    }
    if mode == "stderr-limit" {
        mark("stderr_limit_write");
        let _ = io::stderr().write_all(&vec![b'e'; 64 * 1024 + 1]);
        hold();
        return;
    }
    if mode == "stdout-limit" {
        mark("stdout_limit_write");
    }
    let mut out = io::stdout().lock();
    out.write_all(&bytes).expect("write response bytes");
    out.flush().expect("flush response bytes");
    mark("output_written");
    if mode == "valid-stderr" {
        io::stderr()
            .write_all(b"stderr-after-injection")
            .expect("stderr data");
        mark("stderr_written");
    }
    if mode == "zero-stdout" || mode == "zero-stderr" {
        let handle = unsafe {
            GetStdHandle(if mode == "zero-stdout" {
                STD_OUTPUT_HANDLE
            } else {
                STD_ERROR_HANDLE
            })
        };
        let mut written = 1;
        assert_ne!(
            unsafe {
                WriteFile(
                    handle,
                    std::ptr::null(),
                    0,
                    &mut written,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(written, 0);
        mark("zero_write");
        if mode == "zero-stdout" {
            out.write_all(b"x").expect("trailing byte");
            out.flush().unwrap();
        } else {
            io::stderr().write_all(b"after-zero").expect("stderr tail");
        }
        mark("zero_tail_written");
    }
    if ["hold-valid", "hold-prefix", "partial-stdout"].contains(&mode) {
        mark("holding_open");
        hold();
    }
    if mode == "exit-output" {
        mark("exit_42");
        std::process::exit(42);
    }
    mark("done");
}

#[cfg(not(windows))]
fn main() {
    panic!("W02/W03 fault probe requires Windows");
}
