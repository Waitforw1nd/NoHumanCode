use std::{
    future::Future,
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

tokio::task_local! { static CURRENT: Option<Arc<Case>>; }

pub(crate) struct Case {
    file: Mutex<std::fs::File>,
    started: Instant,
    id: &'static str,
    count: AtomicU64,
    command: AtomicU64,
}
impl Case {
    pub(crate) fn new(root: PathBuf, id: &'static str) -> Option<Arc<Self>> {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("worktree-diagnostics.jsonl"))
        {
            Ok(file) => Some(Arc::new(Self {
                file: Mutex::new(file),
                started: Instant::now(),
                id,
                count: AtomicU64::new(0),
                command: AtomicU64::new(0),
            })),
            Err(_) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "worktree diagnostics: cannot open event file for {id}"
                );
                None
            }
        }
    }
    fn emit(
        &self,
        phase: &'static str,
        command: u64,
        detail: &'static str,
        pid: Option<u32>,
        exit: Option<i32>,
    ) {
        let n = self.count.fetch_add(1, Ordering::Relaxed);
        if n >= 2048 {
            if n == 2048 {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "worktree diagnostics: event limit reached for {}",
                    self.id
                );
            }
            return;
        }
        let event = serde_json::json!({"case":self.id,"pid":std::process::id(),"elapsed_us":self.started.elapsed().as_micros(),"phase":phase,"command":command,"detail":detail,"child_pid":pid,"exit":exit});
        let ok = self
            .file
            .lock()
            .ok()
            .and_then(|mut f| writeln!(f, "{event}").ok())
            .is_some();
        if !ok {
            let _ = writeln!(
                std::io::stderr().lock(),
                "worktree diagnostics: event write failed for {}",
                self.id
            );
        }
    }
}
pub(crate) async fn scope<F: Future>(case: Option<Arc<Case>>, future: F) -> F::Output {
    CURRENT.scope(case, future).await
}
pub(crate) fn current() -> Option<Arc<Case>> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}
pub(crate) fn event(phase: &'static str) {
    record(phase, 0, "", None, None);
}
pub(crate) fn record(
    phase: &'static str,
    command: u64,
    detail: &'static str,
    pid: Option<u32>,
    exit: Option<i32>,
) {
    if let Some(case) = current() {
        case.emit(phase, command, detail, pid, exit);
    }
}
pub(crate) fn next_command() -> u64 {
    current().map_or(0, |c| c.command.fetch_add(1, Ordering::Relaxed) + 1)
}

pub(crate) fn case_event(case: &Option<Arc<Case>>, phase: &'static str) {
    if let Some(case) = case {
        case.emit(phase, 0, "", None, None);
    }
}

pub(crate) fn case_record(case: &Option<Arc<Case>>, phase: &'static str, sequence: u64) {
    if let Some(case) = case {
        case.emit(phase, sequence, "", None, None);
    }
}

pub(crate) fn atomic_write(path: &std::path::Path, value: &[u8]) -> anyhow::Result<()> {
    let pending = path.with_extension("pending");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)?;
    file.write_all(value)?;
    file.sync_all()?;
    std::fs::rename(pending, path)?;
    Ok(())
}

pub(crate) fn wait_start(path: &std::path::Path) -> anyhow::Result<()> {
    let deadline = Instant::now() + std::time::Duration::from_secs(CONTROL_SECS);
    loop {
        match std::fs::read(path) {
            Ok(value) if value == b"start" => return Ok(()),
            Ok(_) => anyhow::bail!("invalid fixture start gate"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        anyhow::ensure!(Instant::now() < deadline, "fixture start gate deadline");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

// The single-project fixture reaches claim after 27 read commands and the
// created probe after another 58 reads plus one mutating command. A control
// allowance is separate from each production command and its cleanup limit.
pub(crate) const CONTROL_SECS: u64 = 10;
pub(crate) const READ_SECS: u64 = 10;
pub(crate) const MUTATE_SECS: u64 = 30;
pub(crate) const CLEANUP_SECS: u64 = 5;
pub(crate) const READ_LIMIT: u64 = 85;
pub(crate) const MUTATE_LIMIT: u64 = 1;
pub(crate) const CONTROL_LIMIT: u64 = 12;
pub(crate) const FIXTURE_COMMANDS: u64 = 7;
pub(crate) const TOTAL_SECS: u64 = FIXTURE_COMMANDS * (CONTROL_SECS + CLEANUP_SECS)
    + READ_LIMIT * (READ_SECS + CLEANUP_SECS + CONTROL_SECS)
    + MUTATE_LIMIT * (MUTATE_SECS + CLEANUP_SECS + CONTROL_SECS)
    + CONTROL_LIMIT * CONTROL_SECS;

/// A bounded observer for the fixed single-project fixture, driven only by
/// complete diagnostic records and valid, monotonic state transitions.
pub(crate) struct Supervisor {
    path: PathBuf,
    offset: usize,
    started: Instant,
    last: Instant,
    stage: usize,
    command: u64,
    active: Option<(u64, bool)>,
    replay: bool,
    fixture_expected: bool,
    fixture_command: u64,
    fixture_active: bool,
    fixture_done: bool,
}
impl Supervisor {
    pub(crate) fn new(path: PathBuf) -> Self {
        let now = Instant::now();
        Self {
            path,
            offset: 0,
            started: now,
            last: now,
            stage: 0,
            command: 0,
            active: None,
            replay: false,
            fixture_expected: false,
            fixture_command: 0,
            fixture_active: false,
            fixture_done: false,
        }
    }
    pub(crate) fn with_fixture(path: PathBuf) -> Self {
        let mut result = Self::new(path);
        result.fixture_expected = true;
        result
    }
    fn apply(&mut self, value: &serde_json::Value, now: Instant) -> anyhow::Result<()> {
        use anyhow::{bail, ensure};
        let phase = value["phase"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing phase"))?;
        let sequence = value["command"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("missing sequence"))?;
        let stages = [
            "engine_entry",
            "prepare_begin",
            "prepare_ok",
            "project_probe_begin",
            "project_probe_ok",
            "claim_begin",
            "claim_ok",
            "claim_probe_enter",
            "created_probe_enter",
            "registered_probe_enter",
        ];
        if phase == "fixture_git_begin" {
            ensure!(
                self.fixture_expected
                    && self.stage == 0
                    && !self.fixture_active
                    && sequence == self.fixture_command + 1
                    && sequence <= FIXTURE_COMMANDS,
                "invalid fixture command start"
            );
            self.fixture_active = true;
            self.last = now;
        } else if phase == "fixture_git_end" {
            ensure!(
                self.fixture_active && sequence == self.fixture_command + 1,
                "invalid fixture command end"
            );
            self.fixture_command = sequence;
            self.fixture_active = false;
            self.last = now;
        } else if phase == "fixture_ready" {
            ensure!(
                self.fixture_expected
                    && !self.fixture_done
                    && !self.fixture_active
                    && self.fixture_command == FIXTURE_COMMANDS,
                "invalid fixture ready"
            );
            self.fixture_done = true;
            self.last = now;
        } else if phase == "engine_entry" && self.stage == stages.len() && !self.replay {
            self.replay = true;
            self.last = now;
        } else if matches!(
            phase,
            "prepare_error"
                | "project_probe_error"
                | "claim_error"
                | "git_construct_error"
                | "git_spawn_error"
                | "git_return_error"
                | "git_stdout_error"
                | "git_stderr_error"
                | "git_child_wait_error"
        ) {
            bail!("fixture operation failed");
        } else if let Some(index) = stages.iter().position(|name| *name == phase) {
            ensure!(
                index == self.stage && self.active.is_none(),
                "invalid or duplicate control phase"
            );
            ensure!(
                !self.fixture_expected || self.fixture_done,
                "engine started before fixture ready"
            );
            let expected = match phase {
                "prepare_ok" => Some(15),
                "project_probe_ok" | "claim_begin" | "claim_ok" | "claim_probe_enter" => Some(27),
                "created_probe_enter" | "registered_probe_enter" => Some(86),
                _ => None,
            };
            if let Some(expected) = expected {
                ensure!(
                    self.command == expected,
                    "unexpected command count at control phase"
                );
            }
            self.stage += 1;
            self.last = now;
        } else if phase == "git_spawn_begin" {
            ensure!(
                self.active.is_none()
                    && sequence == self.command + 1
                    && sequence <= READ_LIMIT + MUTATE_LIMIT,
                "invalid command sequence"
            );
            ensure!(
                matches!(
                    (self.stage, sequence),
                    (2, 1..=15) | (4, 16..=27) | (8, 28..=86)
                ),
                "command outside its legal stage"
            );
            let mutating = match value["detail"].as_str() {
                Some("read") => false,
                Some("mutating") => true,
                _ => bail!("invalid command category"),
            };
            ensure!(
                mutating == (sequence == 57),
                "unexpected mutating command position"
            );
            self.active = Some((sequence, mutating));
            self.last = now;
        } else if phase == "git_wait_end" {
            ensure!(
                self.active.map(|v| v.0) == Some(sequence),
                "unexpected command completion"
            );
            ensure!(value["detail"] == "ok", "git execution failed");
            let exit = value["exit"]
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("git exit missing"))?;
            ensure!(
                exit == if sequence == 15 || sequence == 42 {
                    1
                } else {
                    0
                },
                "unexpected git exit"
            );
            self.command = sequence;
            self.active = None;
            self.last = now;
        }
        Ok(())
    }
    fn check_at(&self, now: Instant) -> anyhow::Result<()> {
        anyhow::ensure!(
            now.duration_since(self.started) <= std::time::Duration::from_secs(TOTAL_SECS),
            "fixture total budget exceeded"
        );
        let quiet = match self.active {
            Some((_, true)) => MUTATE_SECS + CLEANUP_SECS + CONTROL_SECS,
            Some((_, false)) => READ_SECS + CLEANUP_SECS + CONTROL_SECS,
            None => CONTROL_SECS,
        };
        anyhow::ensure!(
            now.duration_since(self.last) <= std::time::Duration::from_secs(quiet),
            "fixture progress stalled"
        );
        Ok(())
    }
    pub(crate) fn poll(&mut self) -> anyhow::Result<()> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            bytes.len() >= self.offset && bytes.len() <= 4 * 1024 * 1024,
            "invalid diagnostic stream length"
        );
        let now = Instant::now();
        self.check_at(now)?;
        while let Some(end) = bytes[self.offset..].iter().position(|b| *b == b'\n') {
            let end = self.offset + end;
            let value: serde_json::Value = serde_json::from_slice(&bytes[self.offset..end])?;
            self.apply(&value, now)?;
            self.offset = end + 1;
        }
        Ok(())
    }
    pub(crate) fn stage(&self) -> usize {
        self.stage
    }
}

#[cfg(test)]
mod supervisor_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn duplicate_progress_cannot_extend_stall_and_partial_lines_are_not_ready() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events");
        let mut monitor = Supervisor::new(path.clone());
        let start = monitor.started;
        monitor
            .apply(&json!({"phase":"engine_entry","command":0}), start)
            .unwrap();
        assert!(
            monitor
                .apply(
                    &json!({"phase":"engine_entry","command":0}),
                    start + std::time::Duration::from_secs(9)
                )
                .is_err()
        );
        assert!(
            monitor
                .check_at(start + std::time::Duration::from_secs(11))
                .is_err()
        );
        std::fs::write(&path, b"{\"phase\":\"prepare_begin\"").unwrap();
        assert!(monitor.poll().is_ok());
        assert_eq!(monitor.stage(), 1);
    }

    #[test]
    fn legal_preparation_can_exceed_old_windows_but_not_total_or_command_limits() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Supervisor::new(dir.path().join("events"));
        let start = monitor.started;
        monitor
            .apply(&json!({"phase":"engine_entry","command":0}), start)
            .unwrap();
        monitor
            .apply(&json!({"phase":"prepare_begin","command":0}), start)
            .unwrap();
        for number in 1..=27 {
            if number == 16 {
                let at = start + std::time::Duration::from_secs(30);
                monitor
                    .apply(&json!({"phase":"prepare_ok","command":0}), at)
                    .unwrap();
                monitor
                    .apply(&json!({"phase":"project_probe_begin","command":0}), at)
                    .unwrap();
            }
            let begin = start + std::time::Duration::from_secs((number - 1) * 2);
            monitor
                .apply(
                    &json!({"phase":"git_spawn_begin","command":number,"detail":"read"}),
                    begin,
                )
                .unwrap();
            monitor
                .check_at(begin + std::time::Duration::from_secs(1))
                .unwrap();
            monitor.apply(&json!({"phase":"git_wait_end","command":number,"detail":"ok","exit":if number == 15 { 1 } else { 0 }}), begin + std::time::Duration::from_secs(1)).unwrap();
        }
        let at = start + std::time::Duration::from_secs(54);
        monitor
            .apply(&json!({"phase":"project_probe_ok","command":0}), at)
            .unwrap();
        monitor.check_at(at).unwrap();
        assert!(
            monitor
                .check_at(start + std::time::Duration::from_secs(TOTAL_SECS + 1))
                .is_err()
        );
    }

    #[test]
    fn command_outside_stage_or_failed_operation_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Supervisor::new(dir.path().join("events"));
        let start = monitor.started;
        assert!(
            monitor
                .apply(
                    &json!({"phase":"git_spawn_begin","command":1,"detail":"read"}),
                    start
                )
                .is_err()
        );
        assert!(
            monitor
                .apply(&json!({"phase":"prepare_error","command":0}), start)
                .is_err()
        );
    }

    #[test]
    fn fixture_and_full_created_path_obey_stage_and_mutation_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let mut monitor = Supervisor::with_fixture(dir.path().join("events"));
        let start = monitor.started;
        for number in 1..=FIXTURE_COMMANDS {
            let at = start + std::time::Duration::from_secs(number * 2);
            monitor
                .apply(&json!({"phase":"fixture_git_begin","command":number}), at)
                .unwrap();
            monitor
                .apply(
                    &json!({"phase":"fixture_git_end","command":number}),
                    at + std::time::Duration::from_secs(1),
                )
                .unwrap();
        }
        let mut at = start + std::time::Duration::from_secs(15);
        for phase in ["fixture_ready", "engine_entry", "prepare_begin"] {
            monitor
                .apply(&json!({"phase":phase,"command":0}), at)
                .unwrap();
        }
        for number in 1..=86 {
            if number == 16 {
                for phase in ["prepare_ok", "project_probe_begin"] {
                    monitor
                        .apply(&json!({"phase":phase,"command":0}), at)
                        .unwrap();
                }
            } else if number == 28 {
                for phase in [
                    "project_probe_ok",
                    "claim_begin",
                    "claim_ok",
                    "claim_probe_enter",
                ] {
                    monitor
                        .apply(&json!({"phase":phase,"command":0}), at)
                        .unwrap();
                }
            }
            monitor.apply(&json!({"phase":"git_spawn_begin","command":number,"detail":if number == 57 {"mutating"} else {"read"}}), at).unwrap();
            at += std::time::Duration::from_secs(1);
            monitor.check_at(at).unwrap();
            monitor.apply(&json!({"phase":"git_wait_end","command":number,"detail":"ok","exit":if number == 15 || number == 42 {1} else {0}}), at).unwrap();
        }
        monitor
            .apply(&json!({"phase":"created_probe_enter","command":0}), at)
            .unwrap();
        assert_eq!(monitor.stage(), 9);
        assert!(
            monitor
                .apply(&json!({"phase":"created_probe_enter","command":0}), at)
                .is_err()
        );
    }
}
