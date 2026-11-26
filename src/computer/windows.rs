mod save;

use super::WindowState;
use anyhow::{Context, Result, ensure};
use std::cell::OnceCell;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use tokio::sync::watch;
use windows_sys::Win32::Foundation::{HANDLE, HWND, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, SetEvent, WaitForSingleObject,
};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_OBJECT_NAMECHANGE, EVENT_SYSTEM_DIALOGEND, EVENT_SYSTEM_DIALOGSTART,
    EVENT_SYSTEM_FOREGROUND, GetClassNameW, GetForegroundWindow, GetWindowTextW,
    GetWindowThreadProcessId, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
    PeekMessageW, QS_ALLINPUT, WINEVENT_OUTOFCONTEXT,
};

thread_local! {
    // Out-of-context WinEvents run on the thread that registered the hooks.
    static CHANGES: OnceCell<watch::Sender<u64>> = const { OnceCell::new() };
    static FOCUS_EPOCH: OnceCell<Arc<AtomicU64>> = const { OnceCell::new() };
}

unsafe extern "system" fn window_event(
    _: HWINEVENTHOOK,
    event: u32,
    _: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND {
        FOCUS_EPOCH.with(|epoch| {
            if let Some(epoch) = epoch.get() {
                epoch.fetch_add(1, Ordering::Release);
            }
        });
    }
    notify();
}

fn notify() {
    CHANGES.with(|changes| {
        if let Some(sender) = changes.get() {
            sender.send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    });
}

struct Hook(HWINEVENTHOOK);

impl Hook {
    fn register(first: u32, last: u32, pid: u32) -> Result<Self> {
        let hook = unsafe {
            SetWinEventHook(
                first,
                last,
                ptr::null_mut(),
                Some(window_event),
                pid,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        if hook.is_null() {
            return Err(io::Error::last_os_error()).context("register Windows foreground observer");
        }
        Ok(Self(hook))
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe { UnhookWinEvent(self.0) };
    }
}

pub(super) struct NativeTarget {
    pid: u32,
    process: Arc<OwnedHandle>,
    stop: Arc<OwnedHandle>,
    worker: Option<thread::JoinHandle<()>>,
    changes: watch::Receiver<u64>,
    focus_epoch: Arc<AtomicU64>,
}

fn owned_handle(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

impl NativeTarget {
    pub(super) fn foreground_generation(&self) -> u64 {
        self.focus_epoch.load(Ordering::Acquire)
    }

    pub(super) fn save_controls(&self, hwnd: usize) -> Result<Option<super::SaveControls>> {
        save::observe(self, hwnd)
    }

    pub(super) fn save_as(
        &self,
        hwnd: usize,
        controls: &super::SaveControls,
        path: &std::path::Path,
    ) -> Result<()> {
        save::submit(self, hwnd, controls, path)
    }

    pub(super) fn open(pid: u32) -> Result<Self> {
        // GetWindowTextW can synchronously dispatch WM_GETTEXT within our own process.
        ensure!(
            pid != std::process::id(),
            "cannot target the Coda host itself"
        );
        let process = Arc::new(
            owned_handle(unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) })
                .context("open configured computer target process")?,
        );
        let stop = Arc::new(
            owned_handle(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) })
                .context("create computer observer shutdown event")?,
        );
        let (sender, changes) = watch::channel(0);
        let (ready, installed) = mpsc::sync_channel(1);
        let focus_epoch = Arc::new(AtomicU64::default());
        let worker_epoch = focus_epoch.clone();
        let worker_stop = stop.clone();
        let worker_process = process.clone();
        let worker = thread::Builder::new()
            .name("coda-window-observer".into())
            .spawn(move || {
                let hooks = (|| -> Result<_> {
                    Ok((
                        Hook::register(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, 0)?,
                        Hook::register(EVENT_SYSTEM_DIALOGSTART, EVENT_SYSTEM_DIALOGEND, pid)?,
                        Hook::register(EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE, pid)?,
                    ))
                })();
                let _hooks = match hooks {
                    Ok(hooks) => hooks,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                CHANGES.with(|changes| {
                    let _ = changes.set(sender);
                });
                FOCUS_EPOCH.with(|epoch| {
                    let _ = epoch.set(worker_epoch);
                });
                if ready.send(Ok(())).is_ok() {
                    pump_events(worker_stop.as_raw_handle(), worker_process.as_raw_handle());
                }
            })?;
        installed
            .recv()
            .context("computer observer stopped at startup")??;
        Ok(Self {
            pid,
            process,
            stop,
            worker: Some(worker),
            changes,
            focus_epoch,
        })
    }

    fn exited(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()).context("check target process lifetime"),
        }
    }

    pub(super) fn snapshot(&self) -> Result<WindowState> {
        if self.exited()? {
            return Ok(WindowState::Exited);
        }
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_null() || window_pid(hwnd) != self.pid {
            return Ok(WindowState::Outside);
        }
        let mut class = [0u16; 256];
        let mut title = [0u16; 1024];
        let class_len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        let title_len = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        if self.exited()? {
            return Ok(WindowState::Exited);
        }
        if unsafe { GetForegroundWindow() } != hwnd || window_pid(hwnd) != self.pid {
            return Ok(WindowState::Outside);
        }
        ensure!(class_len > 0, "cannot read foreground window class");
        let class = String::from_utf16_lossy(&class[..class_len as usize]);
        Ok(WindowState::Target {
            hwnd: hwnd as usize,
            dialog: class == "#32770",
            class,
            title: String::from_utf16_lossy(&title[..title_len as usize]),
        })
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.clone()
    }
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

fn pump_events(stop: HANDLE, process: HANDLE) {
    let handles = [stop, process];
    loop {
        let reason = unsafe {
            MsgWaitForMultipleObjectsEx(
                2,
                handles.as_ptr(),
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        if reason == WAIT_OBJECT_0 + 1 {
            notify();
            return;
        }
        if reason != WAIT_OBJECT_0 + 2 {
            return;
        }
        // Bound each batch so a flood of messages cannot starve shutdown.
        for _ in 0..32 {
            let mut msg: MSG = unsafe { std::mem::zeroed() };
            if unsafe { PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) } == 0 {
                break;
            }
            unsafe { DispatchMessageW(&msg) };
        }
    }
}

impl Drop for NativeTarget {
    fn drop(&mut self) {
        unsafe { SetEvent(self.stop.as_raw_handle()) };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    #[test]
    fn host_cannot_register_its_own_windows() {
        assert!(NativeTarget::open(std::process::id()).is_err());
    }

    #[tokio::test]
    async fn target_exit_wakes_waiters_without_a_foreground_window() -> Result<()> {
        let mut child = Command::new("cmd.exe")
            .args(["/D", "/Q"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let target = NativeTarget::open(child.id())?;
        let mut changes = target.subscribe();
        assert!(matches!(target.snapshot()?, WindowState::Outside));
        child.stdin.take().unwrap().write_all(b"exit\r\n")?;
        ensure!(child.wait()?.success(), "target fixture failed");
        // The worker sends a final generation then closes the channel. Both must
        // happen even on a desktop that generates no foreground WinEvents.
        tokio::time::timeout(Duration::from_secs(5), changes.changed()).await??;
        assert!(matches!(target.snapshot()?, WindowState::Exited));
        tokio::time::timeout(Duration::from_secs(5), async {
            while changes.changed().await.is_ok() {}
        })
        .await?;
        Ok(())
    }
}
