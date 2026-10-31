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
