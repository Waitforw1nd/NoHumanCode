//! Isolated process-tree fixture. Only a signed test window may run this.
#[cfg(windows)]
fn main() {
    use std::{
        fs,
        io::Write,
        os::windows::{io::AsRawHandle, process::CommandExt},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, FILETIME},
        System::Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, GetCurrentProcess, GetProcessTimes,
            OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };
    let args: Vec<_> = std::env::args_os().collect();
    let mode = args.get(1).expect("mode").to_string_lossy();
    match mode.as_ref() {
        "identity" => {
            let (mut created, mut exited, mut kernel, mut user): (
                FILETIME,
                FILETIME,
                FILETIME,
                FILETIME,
            ) = unsafe { std::mem::zeroed() };
            assert_ne!(
                unsafe {
                    GetProcessTimes(
                        GetCurrentProcess(),
                        &mut created,
                        &mut exited,
                        &mut kernel,
                        &mut user,
                    )
                },
                0
            );
            let filetime = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
            print!("{} {}", std::process::id(), filetime);
        }
        "exit" => {
            print!("stdout-marker");
            eprint!("stderr-marker");
            std::process::exit(
                args.get(2)
                    .expect("exit code")
                    .to_string_lossy()
                    .parse()
                    .unwrap(),
            );
        }
        "touch" => {
            fs::write(
                std::path::PathBuf::from(args.get(2).expect("marker").as_os_str()),
                b"ran",
            )
            .unwrap();
        }
        "roundtrip" => {
            use std::os::windows::ffi::OsStrExt;
            fn append(out: &mut Vec<u8>, value: &std::ffi::OsStr) {
                let wide: Vec<u16> = value.encode_wide().collect();
                out.extend_from_slice(&(wide.len() as u32).to_le_bytes());
                for unit in wide {
                    out.extend_from_slice(&unit.to_le_bytes());
                }
            }
            let mut receipt = Vec::new();
            for value in &args[3..] {
                append(&mut receipt, value);
            }
            append(&mut receipt, std::env::current_dir().unwrap().as_os_str());
            append(
                &mut receipt,
                &std::env::var_os("NHC_ROUNDTRIP_VALUE").expect("explicit value"),
            );
            receipt.push(u8::from(
                std::env::var_os("NHC_PARENT_SENTINEL_B23").is_some(),
            ));
            fs::write(std::path::PathBuf::from(&args[2]), receipt).unwrap();
        }
        "leaf" => {
            let release = std::path::PathBuf::from(args.get(2).expect("release").as_os_str());
            let marker = std::path::PathBuf::from(args.get(3).expect("marker").as_os_str());
            let deadline = Instant::now() + Duration::from_secs(30);
            while !release.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if release.exists() {
                fs::write(marker, b"late-effect").unwrap();
            }
        }
        "parent" => {
            let ready = std::path::PathBuf::from(args.get(2).expect("ready").as_os_str());
            let release = std::path::PathBuf::from(args.get(3).expect("release").as_os_str());
            let marker = std::path::PathBuf::from(args.get(4).expect("marker").as_os_str());
            let harness: u32 = args
                .get(5)
                .expect("harness pid")
                .to_string_lossy()
                .parse()
                .unwrap();
            let expected_harness_created: u64 = args
                .get(6)
                .expect("harness creation time")
                .to_string_lossy()
                .parse()
                .unwrap();
            let count: usize = args
                .get(7)
                .expect("count")
                .to_string_lossy()
                .parse()
                .unwrap();
            let stay = args.get(8).expect("stay").to_string_lossy() == "1";
            let noisy = args.get(9).expect("noisy").to_string_lossy() == "1";
            let mut records = String::new();
            let target = unsafe {
                OpenProcess(
                    PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    harness,
                )
            };
            assert!(!target.is_null(), "open test owner");
            let (mut harness_created, mut harness_exited, mut harness_kernel, mut harness_user): (
                FILETIME,
                FILETIME,
                FILETIME,
                FILETIME,
            ) = unsafe { std::mem::zeroed() };
            assert_ne!(
                unsafe {
                    GetProcessTimes(
                        target,
                        &mut harness_created,
                        &mut harness_exited,
                        &mut harness_kernel,
                        &mut harness_user,
                    )
                },
                0,
                "query test owner identity"
            );
            let actual_harness_created = ((harness_created.dwHighDateTime as u64) << 32)
                | harness_created.dwLowDateTime as u64;
            assert_eq!(
                actual_harness_created, expected_harness_created,
                "test owner PID was reused"
            );
            for i in 0..count {
                let child_marker = marker.with_extension(format!("{i}"));
                let child = Command::new(std::env::current_exe().unwrap())
                    .arg("leaf")
                    .arg(&release)
                    .arg(child_marker)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .expect("spawn tracked descendant");
                let mut duplicate = std::ptr::null_mut();
                assert_ne!(
                    unsafe {
                        DuplicateHandle(
                            GetCurrentProcess(),
                            child.as_raw_handle(),
                            target,
                            &mut duplicate,
                            0,
                            0,
                            DUPLICATE_SAME_ACCESS,
                        )
                    },
                    0,
                    "duplicate original child object into test owner"
                );
                let (mut created, mut exited, mut kernel, mut user): (
                    FILETIME,
                    FILETIME,
                    FILETIME,
                    FILETIME,
                ) = unsafe { std::mem::zeroed() };
                assert_ne!(
                    unsafe {
                        GetProcessTimes(
                            child.as_raw_handle(),
                            &mut created,
                            &mut exited,
                            &mut kernel,
                            &mut user,
                        )
                    },
                    0
                );
                let filetime =
                    ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
                records.push_str(&format!(
                    "{} {} {}\n",
                    duplicate as usize,
                    child.id(),
                    filetime
                ));
            }
            unsafe { CloseHandle(target) };
            let tmp = ready.with_extension("tmp");
            fs::write(&tmp, records).unwrap();
            fs::rename(&tmp, &ready).unwrap();
            if noisy {
                std::io::stdout().write_all(&vec![b'x'; 131_072]).unwrap();
            }
            if stay {
                thread::sleep(Duration::from_secs(30));
            }
        }
        "breakaway" => {
            let result = Command::new(std::env::current_exe().unwrap())
                .arg("exit")
                .arg("0")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW)
                .spawn();
            match result {
                Ok(mut child) => {
                    let _ = child.wait();
                    println!("breakaway-created");
                }
                Err(error) => println!("breakaway-denied={}", error.raw_os_error().unwrap_or(-1)),
            }
        }
        "job-breakaway-control" => {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
            use windows_sys::Win32::System::{
                JobObjects::IsProcessInJob,
                Threading::{
                    CREATE_SUSPENDED, CreateProcessW, GetExitCodeProcess, PROCESS_INFORMATION,
                    STARTUPINFOW, TerminateProcess, WaitForSingleObject,
                },
            };
            let job: usize = args
                .get(2)
                .expect("control job handle")
                .to_string_lossy()
                .parse()
                .unwrap();
            let receipt = std::path::PathBuf::from(args.get(3).expect("receipt").as_os_str());
            let exe = std::env::current_exe().unwrap();
            let app: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
            let mut command: Vec<u16> = format!("\"{}\" exit 0", exe.display())
                .encode_utf16()
                .chain([0])
                .collect();
            let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            let created = unsafe {
                CreateProcessW(
                    app.as_ptr(),
                    command.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB,
                    std::ptr::null(),
                    std::ptr::null(),
                    &startup,
                    &mut info,
                )
            };
            let text = if created == 0 {
                format!(
                    "denied={}\n",
                    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
                )
            } else {
                let mut member = 1;
                let query = unsafe { IsProcessInJob(info.hProcess, job as _, &mut member) };
                let query_error = (query == 0).then(std::io::Error::last_os_error);
                let terminated = unsafe { TerminateProcess(info.hProcess, 1) };
                let terminate_error = (terminated == 0).then(std::io::Error::last_os_error);
                let wait = unsafe { WaitForSingleObject(info.hProcess, 5000) };
                let wait_error = (wait != WAIT_OBJECT_0).then(std::io::Error::last_os_error);
                // Termination is a request. Only the original process signal
                // proves this child's cleanup; membership-query failure is a
                // separate assertion failure once that proof exists.
                if wait != WAIT_OBJECT_0 {
                    eprintln!(
                        "NHC_TERMINAL_UNKNOWN: breakaway child pid={} query={query} query_error={query_error:?} terminated={terminated} terminate_error={terminate_error:?} wait={wait} wait_error={wait_error:?}",
                        info.dwProcessId
                    );
                    loop {
                        thread::park_timeout(Duration::from_secs(1));
                    }
                }
                let mut code = 0;
                let code_ok = unsafe { GetExitCodeProcess(info.hProcess, &mut code) };
                unsafe {
                    CloseHandle(info.hThread);
                    CloseHandle(info.hProcess);
                }
                format!(
                    "created=1 query={query} member={member} wait={wait} terminated={terminated} exit_code_ok={code_ok} exit_code={code}\n"
                )
            };
            let tmp = receipt.with_extension("tmp");
            fs::write(&tmp, text).unwrap();
            fs::rename(tmp, receipt).unwrap();
        }
        _ => panic!("unknown fixture mode"),
    }
}

#[cfg(not(windows))]
fn main() {
    panic!("Windows-only fixture");
}
