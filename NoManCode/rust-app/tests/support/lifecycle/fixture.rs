//! Retained, bounded fixtures. Never reopen or terminate a target by PID/name.
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    cell::Cell,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

static SERIAL_CASE: Mutex<()> = Mutex::new(());
static UNKNOWN: AtomicBool = AtomicBool::new(false);

pub fn retain_unknown(root: &std::path::Path, reason: &str) -> ! {
    UNKNOWN.store(true, Ordering::Release);
    let _ = std::fs::write(
        root.join("unknown-holder.json"),
        json!({"state":"unknown","reason":reason,"holder":"same test process","stop_window":true})
            .to_string(),
    );
    eprintln!(
        "NHC_TERMINAL_UNKNOWN: {}; {reason}; retaining original objects",
        root.display()
    );
    // Stop the window while the original observing process keeps its handles.
    loop {
        std::thread::park_timeout(Duration::from_secs(1));
    }
}
use windows_sys::Win32::{
    Foundation::{FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        GetCurrentProcess, GetProcessId, GetProcessTimes, TerminateProcess, WaitForSingleObject,
    },
};

pub async fn until(mut probe: impl FnMut() -> Result<bool>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !probe()? {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    })
    .await
    .context("bounded lifecycle observation expired")?
}

fn identity(handle: HANDLE) -> Result<(u32, u64)> {
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exit = created;
    let mut kernel = created;
    let mut user = created;
    ensure!(
        unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) } != 0,
        "query original process identity: {}",
        std::io::Error::last_os_error()
    );
    let pid = unsafe { GetProcessId(handle) };
    ensure!(pid != 0, "query original process ID");
    Ok((
        pid,
        ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64,
    ))
}

fn signaled(handle: &OwnedHandle) -> Result<bool> {
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        other => anyhow::bail!("original process wait unknown: {other}"),
    }
}

pub struct Case {
    pub root: PathBuf,
    parent: Option<OwnedHandle>,
    descendant: Option<OwnedHandle>,
    complete: bool,
    prepared: Cell<bool>,
    _serial: MutexGuard<'static, ()>,
}

impl Case {
    pub fn new(label: &str) -> Result<Self> {
        ensure!(
            !UNKNOWN.load(Ordering::Acquire),
            "unknown stopped this process window"
        );
        let serial = SERIAL_CASE.lock().unwrap_or_else(|p| p.into_inner());
        ensure!(
            !UNKNOWN.load(Ordering::Acquire),
            "unknown stopped this process window"
        );
        let base = std::env::var_os("NHC_LIFECYCLE_ARTIFACTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/lifecycle-fixtures")
            });
        std::fs::create_dir_all(&base)?;
        let base = base.canonicalize()?;
        // Keep an ordinary drive path for PowerShell and Git's sh alias parser.
        let base = PathBuf::from(
            base.to_string_lossy()
                .trim_start_matches("\\\\?\\")
                .to_string(),
        );
        ensure!(
            base.to_string_lossy()
                .replace("\\\\?\\", "")
                .to_ascii_lowercase()
                .starts_with("e:\\"),
            "lifecycle evidence must remain on E"
        );
        let root = tempfile::Builder::new()
            .prefix(label)
            .tempdir_in(base)?
            .keep();
        eprintln!("LIFECYCLE_CASE={}", root.display());
        std::fs::write(
            root.join("case.json"),
            json!({"case":label,"state":"preparing"}).to_string(),
        )?;
        Ok(Self {
            root,
            parent: None,
            descendant: None,
            complete: false,
            prepared: Cell::new(false),
            _serial: serial,
        })
    }

    pub fn command(&self, wait_parent: bool, flood: bool) -> Result<String> {
        std::fs::write(self.root.join("parent.ps1"), include_str!("descendant.ps1"))?;
        let (observer, created) = identity(unsafe { GetCurrentProcess() })?;
        let quote = |path: PathBuf| format!("'{}'", path.to_string_lossy().replace('\'', "''"));
        Ok(format!(
            "& {} -Root {} -Observer {observer} -ObserverCreated {created} {} {}",
            quote(self.root.join("parent.ps1")),
            quote(self.root.clone()),
            if wait_parent { "-WaitParent" } else { "" },
            if flood { "-Flood" } else { "" }
        ))
    }

    pub fn git_command(&self) -> Result<String> {
        let _ = self.command(false, false)?;
        let (observer, created) = identity(unsafe { GetCurrentProcess() })?;
        let sh_quote = |path: PathBuf| {
            format!(
                "'{}'",
                path.to_string_lossy()
                    .replace('\\', "/")
                    .replace('\'', "'\\''")
            )
        };
        let alias = format!(
            "alias.lifecycle=!pwsh.exe -NoProfile -NonInteractive -File {} -Root {} -Observer {observer} -ObserverCreated {created}",
            sh_quote(self.root.join("parent.ps1")),
            sh_quote(self.root.clone())
        );
        Ok(format!(
            "git -c '{}' lifecycle; exit $LASTEXITCODE",
            alias.replace('\'', "''")
        ))
    }

    pub async fn observe(&mut self) -> Result<()> {
        self.prepared.set(true);
        until(|| Ok(self.root.join("handles.json").exists())).await?;
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.root.join("handles.json"))?)?;
        let take = |name: &str| -> Result<OwnedHandle> {
            let raw = value[name].as_u64().context("missing duplicated handle")? as usize as HANDLE;
            ensure!(
                !raw.is_null() && raw as isize != -1,
                "invalid duplicated handle"
            );
            // The creator transferred this exact handle into this process once.
            Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
        };
        self.parent = Some(take("parent")?);
        self.descendant = Some(take("descendant")?);
        let parent = identity(self.parent.as_ref().unwrap().as_raw_handle())?;
        let descendant = identity(self.descendant.as_ref().unwrap().as_raw_handle())?;
        std::fs::write(
            self.root.join("identity.json"),
            json!({"parent":parent,"descendant":descendant,"source":"creator DuplicateHandle"})
                .to_string(),
        )?;
        until(|| Ok(self.root.join("descendant-ready").exists())).await?;
        ensure!(
            !self.parent_exited()? && !self.descendant_exited()?,
            "fixture exited before acknowledgment"
        );
        std::fs::write(self.root.join("ack"), "ack")?;
        Ok(())
    }

    pub fn parent_exited(&self) -> Result<bool> {
        signaled(self.parent.as_ref().context("parent not observed")?)
    }
    pub fn descendant_exited(&self) -> Result<bool> {
        signaled(
            self.descendant
                .as_ref()
                .context("descendant not observed")?,
        )
    }
    pub fn release(&self) -> Result<()> {
        std::fs::write(self.root.join("release"), "release")?;
        Ok(())
    }
    pub async fn prove_exited(&mut self) -> Result<()> {
        until(|| Ok(self.parent_exited()? && self.descendant_exited()?)).await?;
        std::fs::write(self.root.join("cleanup.json"), json!({"state":"settled","parent_signal":true,"descendant_signal":true,"forced_by_fixture":false}).to_string())?;
        self.complete = true;
        Ok(())
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if self.complete || !self.prepared.get() {
            return;
        }
        // Failure cleanup is distinguishable from the product's cleanup. It
        // cannot turn a failed product assertion into passing evidence.
        let mut confirmed = self.parent.is_some() && self.descendant.is_some();
        for handle in [&self.descendant, &self.parent].into_iter().flatten() {
            match signaled(handle) {
                Ok(false) => {
                    if unsafe { TerminateProcess(handle.as_raw_handle(), 99) } == 0
                        || unsafe { WaitForSingleObject(handle.as_raw_handle(), 5000) }
                            != WAIT_OBJECT_0
                    {
                        confirmed = false;
                    }
                }
                Ok(true) => {}
                Err(_) => confirmed = false,
            }
        }
        let _ = std::fs::write(
            self.root.join("cleanup.json"),
            json!({
                "state":if confirmed {"failure-cleanup-confirmed"} else {"unknown"},
                "forced_by_fixture":true,"retain_scene":true
            })
            .to_string(),
        );
        if !confirmed {
            // Retain the exact observer handles on uncertainty. They are not
            // replaced by a PID record, and this process must remain available
            // to the manager's unknown-preserving supervisor.
            for handle in [self.parent.take(), self.descendant.take()]
                .into_iter()
                .flatten()
            {
                std::mem::forget(handle);
            }
            retain_unknown(&self.root, "original fixture cleanup is unconfirmed");
        }
    }
}
