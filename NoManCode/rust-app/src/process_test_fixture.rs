//! Windows-only ownership for tests of inherited process pipes.
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::c_void,
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    time::{Instant, timeout_at},
};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    fn SetInformationJobObject(job: *mut c_void, class: u32, info: *const c_void, len: u32) -> i32;
    fn TerminateJobObject(job: *mut c_void, code: u32) -> i32;
    fn QueryInformationJobObject(
        job: *mut c_void,
        class: u32,
        info: *mut c_void,
        len: u32,
        returned: *mut u32,
    ) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn DuplicateHandle(
        source_process: *mut c_void,
        source: *mut c_void,
        target_process: *mut c_void,
        target: *mut *mut c_void,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn GetProcessId(handle: *mut c_void) -> u32;
    fn GetProcessTimes(
        handle: *mut c_void,
        created: *mut u64,
        exited: *mut u64,
        kernel: *mut u64,
        user: *mut u64,
    ) -> i32;
}
fn identity(handle: &OwnedHandle) -> Result<(u32, u64)> {
    let mut created = 0;
    let mut exited = 0;
    let mut kernel = 0;
    let mut user = 0;
    let pid = unsafe { GetProcessId(handle.as_raw_handle()) };
    ensure!(
        pid != 0
            && unsafe {
                GetProcessTimes(
                    handle.as_raw_handle(),
                    &mut created,
                    &mut exited,
                    &mut kernel,
                    &mut user,
                )
            } != 0,
        "process identity: {}",
        std::io::Error::last_os_error()
    );
    Ok((pid, created))
}
#[repr(C)]
#[derive(Default)]
struct Accounting {
    total_user: i64,
    total_kernel: i64,
    period_user: i64,
    period_kernel: i64,
    page_faults: u32,
    total: u32,
    active: u32,
    terminated: u32,
}
#[repr(C)]
#[derive(Default)]
struct BasicLimit {
    process_time: i64,
    job_time: i64,
    flags: u32,
    minimum_working: usize,
    maximum_working: usize,
    active_limit: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
}
#[repr(C)]
#[derive(Default)]
struct IoCounters {
    read_ops: u64,
    write_ops: u64,
    other_ops: u64,
    read_bytes: u64,
    write_bytes: u64,
    other_bytes: u64,
}
#[repr(C)]
#[derive(Default)]
struct ExtendedLimit {
    basic: BasicLimit,
    io: IoCounters,
    process_memory: usize,
    job_memory: usize,
    peak_process: usize,
    peak_job: usize,
}
struct Job(OwnedHandle, PathBuf, String);
impl Job {
    fn new(root: &std::path::Path, case: &str) -> Result<Self> {
        let raw = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        ensure!(
            !raw.is_null(),
            "CreateJobObjectW: {}",
            std::io::Error::last_os_error()
        );
        let job = Self(
            unsafe { OwnedHandle::from_raw_handle(raw) },
            root.to_owned(),
            case.to_owned(),
        );
        let mut limits = ExtendedLimit::default();
        limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; no breakaway.
        ensure!(
            unsafe {
                SetInformationJobObject(
                    job.0.as_raw_handle(),
                    9,
                    &limits as *const _ as *const c_void,
                    std::mem::size_of::<ExtendedLimit>() as u32,
                )
            } != 0,
            "SetInformationJobObject: {}",
            std::io::Error::last_os_error()
        );
        Ok(job)
    }
    fn assign(&self, child: &Child) -> Result<()> {
        let raw = child.raw_handle().context("parent handle unavailable")?;
        ensure!(
            unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), raw) } != 0,
            "AssignProcessToJobObject: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    fn terminate_and_wait(&self) -> Result<()> {
        ensure!(
            unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } != 0,
            "TerminateJobObject: {}",
            std::io::Error::last_os_error()
        );
        let end = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let mut info = Accounting::default();
            ensure!(
                unsafe {
                    QueryInformationJobObject(
                        self.0.as_raw_handle(),
                        1,
                        &mut info as *mut _ as *mut c_void,
                        std::mem::size_of::<Accounting>() as u32,
                        std::ptr::null_mut(),
                    )
                } != 0,
                "QueryInformationJobObject: {}",
                std::io::Error::last_os_error()
            );
            if info.active == 0 {
                return Ok(());
            }
            if std::time::Instant::now() >= end {
                bail!("fixture job still has {} active processes", info.active);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        let result = self.terminate_and_wait();
        let record = format!(
            "case={} phase=job-drop cleanup_ok={} error={:?}\n",
            self.2,
            result.is_ok(),
            result.as_ref().err().map(|e| e.to_string())
        );
        eprint!("{record}");
        let _ = std::fs::write(self.1.join("job-drop.txt"), record);
    }
}

pub(crate) struct ReadyFixture {
    pub child: Option<Child>,
    pub parent: OwnedHandle,
    pub descendant: OwnedHandle,
    pub root: PathBuf,
    job: Job,
}
impl ReadyFixture {
    pub fn cleanup(&self, case: &str) -> Result<()> {
        let release = std::fs::write(self.root.join("release"), "release");
        let descendant_wait = if release.is_ok() {
            unsafe { WaitForSingleObject(self.descendant.as_raw_handle(), 5000) }
        } else {
            u32::MAX
        };
        let result = self.job.terminate_and_wait();
        let parent = unsafe { WaitForSingleObject(self.parent.as_raw_handle(), 0) };
        let descendant = unsafe { WaitForSingleObject(self.descendant.as_raw_handle(), 0) };
        let record = format!(
            "case={case} phase=cleanup release_ok={} release_wait={descendant_wait} job_ok={} parent_signal={parent} descendant_signal={descendant}\n",
            release.is_ok(),
            result.is_ok()
        );
        eprint!("{record}");
        let _ = std::fs::write(self.root.join("cleanup.txt"), record);
        result?;
        release.context("fixture release failed")?;
        ensure!(
            descendant_wait == 0,
            "descendant did not acknowledge release by exiting"
        );
        ensure!(
            parent == 0 && descendant == 0,
            "original fixture process exit unconfirmed"
        );
        Ok(())
    }
}
async fn wait_file(path: &std::path::Path) -> Result<String> {
    loop {
        match tokio::fs::read_to_string(path).await {
            Ok(value) if !value.is_empty() => return Ok(value),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn wait_parent_file(path: &std::path::Path, child: &mut Child) -> Result<String> {
    loop {
        if let Some(status) = child.try_wait()? {
            bail!(
                "parent exited before handle publication: code={:?}",
                status.code()
            );
        }
        match tokio::fs::read_to_string(path).await {
            Ok(value) if !value.is_empty() => return Ok(value),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
/// Parent blocks until assigned to a test-only job. Its Process object owns the
/// descendant handle until the observer duplicates it and acknowledges receipt.
pub(crate) async fn prepare(case: &str) -> Result<ReadyFixture> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let root = tempfile::tempdir()?.keep();
    prepare_in(case, root, "normal", deadline).await
}
async fn prepare_in(
    case: &str,
    root: PathBuf,
    mode: &str,
    deadline: Instant,
) -> Result<ReadyFixture> {
    eprintln!("fixture case={case} phase=retained root={}", root.display());
    std::fs::write(
        root.join("case.txt"),
        format!("case={case} phase=preparation\n"),
    )?;
    let job = Job::new(&root, case)?;
    let descendant_script = r#"[Console]::Out.Write('sentinel'); [Console]::Out.Flush(); [IO.File]::WriteAllText('descendant-ready','ready'); $end=[DateTime]::UtcNow.AddSeconds(60); while(!(Test-Path 'release')) { if([DateTime]::UtcNow -gt $end){exit 98}; Start-Sleep -Milliseconds 10 }"#;
    let descendant_script = if mode == "pipe-eof" {
        "[Console]::Out.Write('sentinel'); [Console]::Out.Flush(); [IO.File]::WriteAllText('descendant-ready','ready'); exit 0"
    } else {
        descendant_script
    };
    std::fs::write(root.join("descendant.ps1"), descendant_script)?;
    let parent_script = r#"$end=[DateTime]::UtcNow.AddSeconds(60); while(!(Test-Path 'allow')) { if([DateTime]::UtcNow -gt $end){exit 96}; Start-Sleep -Milliseconds 10 }; $p=[Diagnostics.ProcessStartInfo]::new('pwsh.exe'); $p.UseShellExecute=$false; $p.CreateNoWindow=$true; $p.RedirectStandardInput=$true; $p.ArgumentList.Add('-NoProfile'); $p.ArgumentList.Add('-NonInteractive'); $p.ArgumentList.Add('-File'); $p.ArgumentList.Add((Join-Path $PWD 'descendant.ps1')); $c=[Diagnostics.Process]::Start($p); $c.StandardInput.Close(); [IO.File]::WriteAllText('handle-pending',[string]$c.Handle.ToInt64()); Move-Item 'handle-pending' 'handle-ready'; while(!(Test-Path 'ack')) { if([DateTime]::UtcNow -gt $end){exit 97}; Start-Sleep -Milliseconds 10 }; exit 0"#;
    let parent_script = match mode {
        "parent-exit" => "$end=[DateTime]::UtcNow.AddSeconds(60); while(!(Test-Path 'allow')) { if([DateTime]::UtcNow -gt $end){exit 96}; Start-Sleep -Milliseconds 10 }; exit 42".to_owned(),
        "no-marker" => parent_script.replace("Move-Item 'handle-pending' 'handle-ready';", ""),
        _ => parent_script.to_owned(),
    };
    let mut command = Command::new("pwsh.exe");
    command
        .current_dir(&root)
        .args(["-NoProfile", "-NonInteractive", "-Command", &parent_script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .creation_flags(0x08000000);
    let mut child = command.spawn()?;
    if let Err(error) = job.assign(&child) {
        let killed = child.start_kill();
        let waited = timeout_at(Instant::now() + Duration::from_secs(5), child.wait()).await;
        let record = format!(
            "case={case} phase=assign-failed kill_ok={} wait_ok={}\n",
            killed.is_ok(),
            matches!(waited, Ok(Ok(_)))
        );
        let _ = std::fs::write(root.join("cleanup.txt"), record);
        return Err(error.context("job assignment failed; direct child cleanup attempted"));
    }
    let parent =
        unsafe { BorrowedHandle::borrow_raw(child.raw_handle().context("parent handle")?) }
            .try_clone_to_owned()?;
    std::fs::write(root.join("allow"), "allow")?;
    let ready_result = timeout_at(deadline, async {
        let value = wait_parent_file(&root.join("handle-ready"), &mut child).await?;
        let source = value.trim().parse::<usize>()? as *mut c_void;
        ensure!(unsafe { WaitForSingleObject(parent.as_raw_handle(), 0) } == 258, "parent exited before handle pull");
        let mut target = std::ptr::null_mut();
        ensure!(unsafe { DuplicateHandle(parent.as_raw_handle(), source, GetCurrentProcess(), &mut target, 0, 0, 2) } != 0, "DuplicateHandle original descendant: {}", std::io::Error::last_os_error());
        let descendant = unsafe { OwnedHandle::from_raw_handle(target) };
        let (parent_pid, parent_created) = identity(&parent)?;
        let (descendant_pid, descendant_created) = identity(&descendant)?;
        let identity_record = format!("case={case} phase=descendant-owned parent_pid={parent_pid} parent_created={parent_created} descendant_pid={descendant_pid} descendant_created={descendant_created}\n");
        eprint!("{identity_record}");
        std::fs::write(root.join("identity.txt"), identity_record)?;
        if mode == "ack-fail" { bail!("injected ack failure"); }
        std::fs::write(root.join("ack"), "ack")?;
        wait_file(&root.join("descendant-ready")).await?;
        let status = child.wait().await?;
        std::fs::write(root.join("parent-exit.txt"), format!("exit_code={:?}\n", status.code()))?;
        ensure!(status.code() == Some(0), "original parent exit code {:?}", status.code());
        let mut stdout = child.stdout.take().context("stdout unavailable")?;
        let mut sentinel = [0; 8];
        let mut count = 0;
        loop {
            let bytes = sentinel[..count].iter().map(|b| format!("{b:02x}")).collect::<String>();
            std::fs::write(root.join("sentinel-read.txt"), format!("phase=waiting count={count} bytes_hex={bytes}\n"))?;
            match stdout.read(&mut sentinel[count..]).await {
                Ok(0) => {
                    std::fs::write(root.join("sentinel-read.txt"), format!("phase=early-eof count={count} bytes_hex={bytes}\n"))?;
                    bail!("sentinel early EOF after {count} bytes");
                }
                Ok(n) => {
                    count += n;
                    let bytes = sentinel[..count].iter().map(|b| format!("{b:02x}")).collect::<String>();
                    let phase = if count == sentinel.len() { "complete" } else { "partial" };
                    std::fs::write(root.join("sentinel-read.txt"), format!("phase={phase} count={count} bytes_hex={bytes}\n"))?;
                    if count == sentinel.len() { break; }
                }
                Err(error) => {
                    std::fs::write(root.join("sentinel-read.txt"), format!("phase=read-error count={count} bytes_hex={bytes} kind={:?}\n", error.kind()))?;
                    return Err(error.into());
                }
            }
        }
        ensure!(&sentinel == b"sentinel", "sentinel mismatch");
        std::fs::write(root.join("sentinel.txt"), "sentinel=observed\n")?;
        let mut byte = [0; 1];
        match tokio::time::timeout(Duration::from_millis(100), stdout.read(&mut byte)).await {
            Err(_) => {},
            Ok(Ok(0)) => bail!("stdout EOF after parent exit and sentinel"),
            Ok(Ok(_)) => bail!("unexpected stdout after sentinel"),
            Ok(Err(error)) => return Err(error.into()),
        }
        child.stdout = Some(stdout);
        let mut stderr = child.stderr.take().context("stderr unavailable")?;
        match tokio::time::timeout(Duration::from_millis(100), stderr.read(&mut byte)).await {
            Err(_) => {},
            Ok(Ok(0)) => bail!("stderr EOF after parent exit"),
            Ok(Ok(_)) => bail!("unexpected stderr"),
            Ok(Err(error)) => return Err(error.into()),
        }
        child.stderr = Some(stderr);
        ensure!(unsafe { WaitForSingleObject(descendant.as_raw_handle(), 0) } == 258, "original descendant not alive");
        let parent_signal = unsafe { WaitForSingleObject(parent.as_raw_handle(), 0) };
        let record = format!("case={case} phase=ready parent_exit={:?} parent_signal={parent_signal} descendant_signal=258 sentinel=true stdout_pending=true stderr_pending=true\n", status.code());
        eprint!("{record}");
        std::fs::write(root.join("ready-evidence.txt"), record)?;
        Ok::<_, anyhow::Error>(descendant)
    }).await.context("fixture preparation exceeded 15 seconds").and_then(|result| result);
    let ready = match ready_result {
        Ok(ready) => ready,
        Err(error) => {
            let record = format!("case={case} phase=preparation-failed error={error:#}\n");
            eprint!("{record}");
            let _ = std::fs::write(root.join("failure.txt"), record);
            return Err(error);
        }
    };
    Ok(ReadyFixture {
        child: Some(child),
        parent,
        descendant: ready,
        root,
        job,
    })
}

#[tokio::test]
async fn process_fixture_failure_controls_reap_job() {
    for mode in ["no-marker", "parent-exit", "pipe-eof", "ack-fail"] {
        let root = tempfile::tempdir().unwrap().keep();
        let deadline = Instant::now() + Duration::from_secs(15);
        let result = prepare_in(mode, root.clone(), mode, deadline).await;
        assert!(
            result.is_err(),
            "fixture negative control {mode} unexpectedly ready"
        );
        let failure = std::fs::read_to_string(root.join("failure.txt")).expect("failure evidence");
        assert!(failure.contains("preparation-failed"));
        match mode {
            "no-marker" => {
                assert!(
                    failure.contains("fixture preparation exceeded 15 seconds"),
                    "{failure}"
                );
                assert!(root.join("handle-pending").exists());
                assert!(!root.join("handle-ready").exists());
            }
            "parent-exit" => assert!(failure.contains("code=Some(42)"), "{failure}"),
            "pipe-eof" => {
                assert!(
                    failure.contains("stdout EOF after parent exit and sentinel"),
                    "{failure}"
                );
                assert!(
                    std::fs::read_to_string(root.join("parent-exit.txt"))
                        .unwrap()
                        .contains("Some(0)")
                );
                assert!(root.join("sentinel.txt").exists());
            }
            "ack-fail" => {
                assert!(failure.contains("injected ack failure"), "{failure}");
                assert!(root.join("identity.txt").exists());
                assert!(!root.join("ack").exists());
            }
            _ => unreachable!(),
        }
        let cleanup =
            std::fs::read_to_string(root.join("job-drop.txt")).expect("job cleanup evidence");
        assert!(cleanup.contains("cleanup_ok=true"), "{mode}: {cleanup}");
    }
}
