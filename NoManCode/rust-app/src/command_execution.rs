//! Bounded ownership of one approved command. This is not an OS process sandbox.
use crate::domain::Task;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{fmt, process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    time::{Instant, timeout_at},
};

/// Execution or cleanup could not be confirmed. Engine must preserve the claim.
#[derive(Debug)]
pub(crate) struct CommandOutcomeUnknown;
impl fmt::Display for CommandOutcomeUnknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("命令执行结果未知；请检查工作区后创建新任务")
    }
}
impl std::error::Error for CommandOutcomeUnknown {}

#[cfg(test)]
fn unknown_faults() -> &'static std::sync::Mutex<std::collections::HashMap<String, bool>> {
    static FAULTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> =
        std::sync::OnceLock::new();
    FAULTS.get_or_init(Default::default)
}
/// Test-only, task-scoped, single-use injection after a real process spawn.
#[cfg(test)]
pub(crate) struct UnknownAfterSpawnGuard(String);
#[cfg(test)]
impl Drop for UnknownAfterSpawnGuard {
    fn drop(&mut self) {
        unknown_faults().lock().unwrap().remove(&self.0);
    }
}
#[cfg(test)]
pub(crate) fn unknown_after_spawn_once(task_id: &str) -> UnknownAfterSpawnGuard {
    let inserted = {
        let mut faults = unknown_faults().lock().unwrap();
        if let std::collections::hash_map::Entry::Vacant(entry) = faults.entry(task_id.to_owned()) {
            entry.insert(true);
            true
        } else {
            false
        }
    };
    // Assert outside the lock: rejecting duplicate registration must not poison it.
    assert!(inserted, "duplicate task-scoped command fault registration");
    UnknownAfterSpawnGuard(task_id.to_owned())
}
#[cfg(test)]
fn take_unknown_fault(task_id: &str) -> bool {
    unknown_faults()
        .lock()
        .unwrap()
        .get_mut(task_id)
        .is_some_and(std::mem::take)
}

// Even an unexpected unwind/runtime teardown must not detach readers. Normal
// completion and the bounded finalizer still explicitly join each task.
struct ReaderTask(tokio::task::JoinHandle<Result<Vec<u8>>>);
impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) async fn execute(task: &Task, args: &Value) -> Result<Value> {
    ensure!(task.spec.allow_commands, "当前任务未授权运行命令");
    let command = args["command"].as_str().context("缺少 command")?;
    ensure!(command.len() <= 8192, "命令过长");
    let timeout_seconds = args["timeout_seconds"].as_u64().unwrap_or(30);
    ensure!(
        (1..=600).contains(&timeout_seconds),
        "命令超时时间必须在 1–600 秒之间"
    );
    let mut process = Command::new("pwsh.exe");
    process
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .current_dir(&task.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, _) in std::env::vars() {
        if name.contains("KEY") || name.contains("TOKEN") || name.contains("SECRET") {
            process.env_remove(name);
        }
    }
    #[cfg(windows)]
    process.creation_flags(0x08000000);
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let mut child = process.spawn().context("无法启动 PowerShell")?;
    #[cfg(test)]
    let probe = tests::PROBE.try_with(std::sync::Arc::clone).ok();
    #[cfg(test)]
    if let Some(probe) = &probe {
        probe
            .pid
            .store(child.id().unwrap(), std::sync::atomic::Ordering::SeqCst);
    }
    #[cfg(test)]
    let forced_unknown = take_unknown_fault(&task.id);
    // These are guaranteed by Stdio::piped, but still take the cleanup path if
    // an unexpected missing pipe is ever encountered after the side effect.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    #[cfg(test)]
    let out_probe = probe.clone();
    let mut out = ReaderTask(tokio::spawn(async move {
        #[cfg(test)]
        let _reader = tests::ReaderGuard::new(out_probe.clone());
        #[cfg(test)]
        if out_probe.as_ref().is_some_and(|p| p.reader_error) {
            bail!("test reader failure");
        }
        let mut bytes = Vec::new();
        stdout
            .context("missing stdout")?
            .take(65536)
            .read_to_end(&mut bytes)
            .await?;
        Ok::<_, anyhow::Error>(bytes)
    }));
    #[cfg(test)]
    let err_probe = probe.clone();
    let mut err = ReaderTask(tokio::spawn(async move {
        #[cfg(test)]
        let _reader = tests::ReaderGuard::new(err_probe);
        let mut bytes = Vec::new();
        stderr
            .context("missing stderr")?
            .take(65536)
            .read_to_end(&mut bytes)
            .await?;
        Ok::<_, anyhow::Error>(bytes)
    }));
    let mut out_joined = false;
    let mut err_joined = false;
    let completion = timeout_at(deadline, async {
        #[cfg(test)]
        if forced_unknown { bail!("test unknown outcome after real spawn"); }
        let mut status = None;
        let mut output = None;
        let mut errors = None;
        while status.is_none() || output.is_none() || errors.is_none() {
            tokio::select! {
                value = child.wait(), if status.is_none() => {
                    status = Some(value?);
                    #[cfg(test)]
                    if probe.as_ref().is_some_and(|p| p.wait_error) { bail!("test wait failure"); }
                    #[cfg(test)]
                    if let Some(probe) = &probe { probe.parent_waited.store(true, std::sync::atomic::Ordering::SeqCst); }
                }
                value = &mut out.0, if !out_joined => {
                    out_joined = true;
                    output = Some(value??);
                }
                value = &mut err.0, if !err_joined => {
                    err_joined = true;
                    errors = Some(value??);
                }
            }
        }
        Ok::<_, anyhow::Error>(json!({
            "exit_code": status.context("missing exit status")?.code(),
            "stdout": String::from_utf8_lossy(&output.context("missing stdout result")?),
            "stderr": String::from_utf8_lossy(&errors.context("missing stderr result")?)
        }))
    })
    .await;
    if let Ok(Ok(value)) = completion {
        return Ok(value);
    }
    // All failure branches converge here. There is one additional total budget,
    // including process termination, reaping, and both reader task joins.
    let cleanup_deadline = Instant::now() + Duration::from_secs(5);
    if !out_joined {
        out.0.abort();
    }
    if !err_joined {
        err.0.abort();
    }
    let cleanup = timeout_at(cleanup_deadline, async {
        let process_result: Result<()> = async {
            if child.try_wait()?.is_none() {
                child.start_kill()?;
                child.wait().await?;
            }
            Ok(())
        }
        .await;
        // Reap reader tasks even if process cleanup fails.
        if !out_joined {
            let _ = (&mut out.0).await;
        }
        if !err_joined {
            let _ = (&mut err.0).await;
        }
        process_result
    })
    .await;
    #[cfg(test)]
    if probe.as_ref().is_some_and(|p| p.cleanup_unknown) {
        return Err(CommandOutcomeUnknown.into());
    }
    if !matches!(cleanup, Ok(Ok(()))) || completion.is_ok() {
        return Err(CommandOutcomeUnknown.into());
    }
    bail!("命令运行超过 {timeout_seconds} 秒；已结束直接子进程等待与输出收集")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    };
    tokio::task_local! { pub(super) static PROBE: Arc<Probe>; }
    #[derive(Default)]
    pub(super) struct Probe {
        pub reader_error: bool,
        pub wait_error: bool,
        pub cleanup_unknown: bool,
        pub pid: AtomicU32,
        pub parent_waited: AtomicBool,
        readers: AtomicUsize,
    }
    pub(super) struct ReaderGuard(Option<Arc<Probe>>);
    impl ReaderGuard {
        pub fn new(probe: Option<Arc<Probe>>) -> Self {
            if let Some(probe) = &probe {
                probe.readers.fetch_add(1, Ordering::SeqCst);
            }
            Self(probe)
        }
    }
    impl Drop for ReaderGuard {
        fn drop(&mut self) {
            if let Some(probe) = &self.0 {
                probe.readers.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
    fn task(root: &std::path::Path) -> Task {
        serde_json::from_value(json!({"id":"command-test","run_id":"run",
            "spec":{"name":"worker","role":"test","route_id":"route","prompt":"test",
                "depends_on":[],"write_scopes":[],"tools":true,"allow_commands":true,"max_rounds":1},
            "route":{"id":"route","name":"route","base_url":"http://127.0.0.1","model":"mock",
                "max_tokens":16,"parallel_limit":1,"key_env":null},
            "workspace":root.to_str().unwrap(),"status":"running","output":"","error":null,"messages":[],
            "usage":null,"created_at":0,"updated_at":0})).unwrap()
    }
    async fn assert_pid_exited(pid: u32) {
        let mut query = Command::new("pwsh.exe");
        query.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 9 }}"),
        ]);
        query.kill_on_drop(true);
        #[cfg(windows)]
        query.creation_flags(0x08000000);
        let mut child = query.spawn().unwrap();
        let status = timeout_at(Instant::now() + Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "direct command pid remains: {pid}");
    }
    #[tokio::test]
    async fn s06_real_timeout_reaps_child_and_readers_without_false_unknown() {
        let root = tempfile::tempdir().unwrap();
        let task = task(root.path());
        let probe = Arc::new(Probe::default());
        let start = Instant::now();
        let error = PROBE.scope(probe.clone(), execute(&task, &json!({
            "command":"[IO.File]::WriteAllText((Join-Path $PWD 'ready'), 'ready'); Start-Sleep -Seconds 20",
            "timeout_seconds":1
        }))).await.unwrap_err();
        assert!(root.path().join("ready").exists());
        assert!(!error.is::<CommandOutcomeUnknown>(), "{error:#}");
        assert!(error.to_string().contains("超过 1 秒"));
        assert!(start.elapsed() < Duration::from_secs(7));
        assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
        assert_pid_exited(probe.pid.load(Ordering::SeqCst)).await;
    }
    #[tokio::test]
    async fn s06_reader_error_and_unconfirmed_cleanup_are_typed_unknown() {
        let root = tempfile::tempdir().unwrap();
        let task = task(root.path());
        for (reader_error, cleanup_unknown, wait_error) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let probe = Arc::new(Probe {
                reader_error,
                cleanup_unknown,
                wait_error,
                ..Probe::default()
            });
            let error = PROBE.scope(probe.clone(), execute(&task, &json!({
                "command": if wait_error { "exit 0" } else { "Start-Sleep -Seconds 20" }, "timeout_seconds":1
            }))).await.unwrap_err();
            assert!(error.is::<CommandOutcomeUnknown>());
            assert!(!error.to_string().contains("已停止"));
            assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
            assert_pid_exited(probe.pid.load(Ordering::SeqCst)).await;
        }
    }
    #[tokio::test]
    async fn s06_deadline_covers_inherited_pipe_after_direct_parent_exit() {
        let root = tempfile::tempdir().unwrap();
        let task = task(root.path());
        // No stdout/stderr redirect: the child inherits the parent's handles.
        // Its independent five-second lifetime is a test fallback, not product cleanup.
        let script = root.path().join("descendant.ps1");
        std::fs::write(&script, "[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'descendant-ready'), [string]$PID); Start-Sleep -Seconds 5").unwrap();
        let command = "$p=[Diagnostics.ProcessStartInfo]::new('pwsh.exe'); $p.UseShellExecute=$false; $p.CreateNoWindow=$true; $p.ArgumentList.Add('-NoProfile'); $p.ArgumentList.Add('-NonInteractive'); $p.ArgumentList.Add('-File'); $p.ArgumentList.Add((Join-Path $PWD 'descendant.ps1')); $child=[Diagnostics.Process]::Start($p); $deadline=[DateTime]::UtcNow.AddSeconds(1); while (!(Test-Path (Join-Path $PWD 'descendant-ready'))) { if ([DateTime]::UtcNow -gt $deadline) { exit 97 }; Start-Sleep -Milliseconds 10 }; [Console]::Out.Write('parent-exit')";
        let probe = Arc::new(Probe::default());
        let begin = Instant::now();
        let result = PROBE
            .scope(
                probe.clone(),
                execute(&task, &json!({"command":command,"timeout_seconds":2})),
            )
            .await;
        let parent_waited = probe.parent_waited.load(Ordering::SeqCst);
        let elapsed = begin.elapsed();
        // Always clean our own known test descendant before assertions, including failures.
        let descendant = std::fs::read_to_string(root.path().join("descendant-ready")).unwrap();
        let descendant: u32 = descendant.trim().parse().unwrap();
        let mut killer = Command::new("pwsh.exe");
        killer.args(["-NoProfile", "-NonInteractive", "-Command", &format!("$p=Get-Process -Id {descendant} -ErrorAction SilentlyContinue; if ($p) {{ $p.Kill(); $p.WaitForExit() }}")]).kill_on_drop(true);
        #[cfg(windows)]
        killer.creation_flags(0x08000000);
        let mut helper = killer.spawn().unwrap();
        assert!(
            timeout_at(Instant::now() + Duration::from_secs(6), helper.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert!(
            parent_waited,
            "fixture did not observe direct parent exit before deadline"
        );
        let error = result.unwrap_err();
        assert!(!error.is::<CommandOutcomeUnknown>(), "{error:#}");
        assert!(
            elapsed < Duration::from_secs(4),
            "pipe did not obey execution deadline: {elapsed:?}"
        );
        assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
        assert_pid_exited(probe.pid.load(Ordering::SeqCst)).await;
        assert_pid_exited(descendant).await;
    }
    #[tokio::test]
    async fn s06_task_scoped_unknown_hook_is_once_guarded_and_reaps_real_spawn() {
        let root = tempfile::tempdir().unwrap();
        let mut task = task(root.path());
        task.id = "d-s06-once-hook".into();
        let guard = unknown_after_spawn_once(&task.id);
        assert!(std::panic::catch_unwind(|| unknown_after_spawn_once(&task.id)).is_err());
        let mut other = task.clone();
        other.id = "d-s06-unaffected".into();
        assert_eq!(
            execute(&other, &json!({"command":"[Console]::Out.Write('other')"}))
                .await
                .unwrap()["stdout"],
            "other"
        );
        let probe = Arc::new(Probe::default());
        let error = PROBE
            .scope(
                probe.clone(),
                execute(&task, &json!({"command":"Start-Sleep -Seconds 20"})),
            )
            .await
            .unwrap_err();
        assert!(error.is::<CommandOutcomeUnknown>());
        assert_ne!(probe.pid.load(Ordering::SeqCst), 0);
        assert_pid_exited(probe.pid.load(Ordering::SeqCst)).await;
        assert_eq!(probe.readers.load(Ordering::SeqCst), 0);
        assert_eq!(
            execute(
                &task,
                &json!({"command":"[Console]::Out.Write('consumed')"})
            )
            .await
            .unwrap()["stdout"],
            "consumed"
        );
        assert!(std::panic::catch_unwind(|| unknown_after_spawn_once(&task.id)).is_err());
        drop(guard);
        let unused = unknown_after_spawn_once(&task.id);
        drop(unused);
        assert_eq!(
            execute(&task, &json!({"command":"[Console]::Out.Write('cleared')"}))
                .await
                .unwrap()["stdout"],
            "cleared"
        );
    }
}
