//! Deliberately narrow adapter for the native Notepad common file dialog.
//! No desktop-wide UIA searches, focus changes, window messages or input injection.
use super::super::SaveControls;
use super::{NativeTarget, owned_handle, window_pid};
use anyhow::{Context, Result, ensure};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, Prefix};
use windows::Win32::Foundation::HWND as UiaHwnd;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::Variant::VT_I4;
use windows::Win32::UI::Accessibility::*;
use windows::core::BSTR;
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn notepad(target: &NativeTarget) -> Result<bool> {
    // Do not add query rights to registration: read-only observation also serves
    // applications that grant SYNCHRONIZE but not PROCESS_QUERY_LIMITED_INFORMATION.
    let handle =
        owned_handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, target.pid) })?;
    let process = handle.as_raw_handle();
    let mut image = vec![0u16; 32768];
    let mut len = image.len() as u32;
    ensure!(
        unsafe { QueryFullProcessImageNameW(process, 0, image.as_mut_ptr(), &mut len) } != 0,
        "cannot identify executable"
    );
    let image = String::from_utf16(&image[..len as usize])?;
    if !Path::new(&image)
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("notepad.exe"))
    {
        return Ok(false);
    }
    let mut dir = [0u16; 1024];
    let n = unsafe { GetSystemWindowsDirectoryW(dir.as_mut_ptr(), dir.len() as u32) } as usize;
    if n > 0 && n < dir.len() {
        let dir = String::from_utf16(&dir[..n])?;
        if [
            r"\notepad.exe",
            r"\System32\notepad.exe",
            r"\SysWOW64\notepad.exe",
        ]
        .iter()
        .any(|suffix| image.eq_ignore_ascii_case(&format!("{dir}{suffix}")))
        {
            return Ok(!target.exited()?);
        }
    }
    let mut family = [0u16; 128];
    let mut len = family.len() as u32;
    Ok(
        unsafe { GetPackageFamilyName(process, &mut len, family.as_mut_ptr()) } == 0
            && (1..=family.len()).contains(&(len as usize))
            && String::from_utf16(&family[..len as usize - 1])?
                == "Microsoft.WindowsNotepad_8wekyb3d8bbwe"
            && !target.exited()?,
    )
}

fn class(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n as usize])
}

#[derive(Clone, Copy)]
enum Language {
    English,
    Chinese,
}

impl Language {
    fn dialog(title: &str) -> Option<Self> {
        match title {
            "Save As" => Some(Self::English),
            "另存为" => Some(Self::Chinese),
            _ => None,
        }
    }

    fn save(self, name: &str) -> bool {
        match self {
            Self::English => name == "Save",
            Self::Chinese => matches!(name, "保存" | "保存(S)"),
        }
    }
    fn filename(self, name: &str) -> bool {
        match self {
            Self::English => matches!(name, "File name:" | "File name"),
            Self::Chinese => matches!(name, "文件名(N):" | "文件名:" | "文件名(N)：" | "文件名："),
        }
    }
}

struct Dialog<'a> {
    target: &'a NativeTarget,
    hwnd: HWND,
    owner: HWND,
    language: Language,
    epoch: u64,
}

impl<'a> Dialog<'a> {
    fn open(target: &'a NativeTarget, raw: usize, epoch: u64) -> Result<Self> {
        let hwnd = raw as HWND;
        ensure!(
            raw != 0
                && unsafe { GetForegroundWindow() } == hwnd
                && window_pid(hwnd) == target.pid
                && !target.exited()?,
            "reobserve foreground dialog"
        );
        ensure!(
            class(hwnd) == "#32770" && notepad(target)?,
            "not a Notepad common dialog"
        );
        let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
        ensure!(
            !owner.is_null()
                && window_pid(owner) == target.pid
                && class(owner).eq_ignore_ascii_case("Notepad"),
            "not a Notepad-owned dialog"
        );
        let mut title = [0u16; 64];
        let n = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        let language = Language::dialog(&String::from_utf16(&title[..n as usize])?)
            .context("unsupported save dialog")?;
        let dialog = Self {
            target,
            hwnd,
            owner,
            language,
            epoch,
        };
        dialog.check()?;
        Ok(dialog)
    }

    fn check(&self) -> Result<()> {
        ensure!(
            unsafe { GetForegroundWindow() } == self.hwnd
                && window_pid(self.hwnd) == self.target.pid
                && unsafe { GetWindow(self.hwnd, GW_OWNER) } == self.owner
                && window_pid(self.owner) == self.target.pid
                && self.target.foreground_generation() == self.epoch
                && !self.target.exited()?,
            "target changed; reobserve save dialog"
        );
        Ok(())
    }

    fn bind(&self, uia: &IUIAutomation2) -> Result<Bound> {
        self.check()?;
        let mut handles = Candidates::default();
        unsafe {
            EnumChildWindows(
                self.hwnd,
                Some(collect),
                (&mut handles as *mut Candidates) as LPARAM,
            )
        };
        ensure!(!handles.ambiguous, "ambiguous dialog controls");
        let edit = handles.edit.context("no filename edit")? as HWND;
        let button = handles.button.context("no save button")? as HWND;
        unsafe {
            let element = |hwnd, kind| -> Result<IUIAutomationElement> {
                ensure!(
                    GetAncestor(hwnd, GA_ROOT) == self.hwnd
                        && window_pid(hwnd) == self.target.pid
                        && IsWindowVisible(hwnd) != 0,
                    "control is outside dialog"
                );
                let el = uia.ElementFromHandle(UiaHwnd(hwnd))?;
                ensure!(
                    el.CurrentNativeWindowHandle()?.0 == hwnd
                        && el.CurrentProcessId()? as u32 == self.target.pid
                        && el.CurrentControlType()? == kind
                        && el.CurrentIsEnabled()?.as_bool()
                        && !el.CurrentIsOffscreen()?.as_bool(),
                    "control unavailable"
                );
                Ok(el)
            };
            let edit = element(edit, UIA_EditControlTypeId)?;
            let button = element(button, UIA_ButtonControlTypeId)?;
            ensure!(
                self.language.filename(&edit.CurrentName()?.to_string())
                    && self.language.save(&button.CurrentName()?.to_string()),
                "unsupported file dialog actions"
            );
            let value: IUIAutomationValuePattern = edit.GetCurrentPatternAs(UIA_ValuePatternId)?;
            ensure!(
                !value.CurrentIsReadOnly()?.as_bool(),
                "filename is read-only"
            );
            let invoke = button.GetCurrentPatternAs(UIA_InvokePatternId)?;
            let ids = SaveControls {
                filename_id: runtime_id(&edit)?,
                save_id: runtime_id(&button)?,
            };
            ensure!(ids.filename_id != ids.save_id, "duplicate runtime ids");
            self.check()?;
            Ok(Bound { ids, value, invoke })
        }
    }
}

#[derive(Default)]
struct Candidates {
    edit: Option<usize>,
    button: Option<usize>,
    ambiguous: bool,
}

unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
    let found = unsafe { &mut *(data as *mut Candidates) };
    let id = unsafe { GetDlgCtrlID(hwnd) };
    if unsafe { IsWindowVisible(hwnd) } == 0 {
        return 1;
    }
    // Common item dialog: filename Edit (1001) inside ComboBox, and IDOK
    // Button (1). IDOK is also used for Open: never use it without the
    // separate dialog-title, localized action-name and pattern checks.
    let slot = match id {
        1001 if class(hwnd) == "Edit" && class(unsafe { GetParent(hwnd) }) == "ComboBox" => {
            &mut found.edit
        }
        1 if class(hwnd) == "Button" => &mut found.button,
        _ => return 1,
    };
    found.ambiguous |= slot.replace(hwnd as usize).is_some();
    1
}

struct Bound {
    ids: SaveControls,
    value: IUIAutomationValuePattern,
    invoke: IUIAutomationInvokePattern,
}

struct Array(*mut SAFEARRAY);
impl Drop for Array {
    fn drop(&mut self) {
        let _ = unsafe { SafeArrayDestroy(self.0) };
    }
}
fn runtime_id(el: &IUIAutomationElement) -> Result<Vec<i32>> {
    decode_id(Array(unsafe { el.GetRuntimeId()? }))
}
fn decode_id(arr: Array) -> Result<Vec<i32>> {
    unsafe {
        ensure!(
            !arr.0.is_null() && SafeArrayGetDim(arr.0) == 1 && SafeArrayGetVartype(arr.0)? == VT_I4,
            "invalid runtime id"
        );
        let lo = SafeArrayGetLBound(arr.0, 1)?;
        let hi = SafeArrayGetUBound(arr.0, 1)?;
        ensure!(
            (0..64).contains(&(i64::from(hi) - i64::from(lo))),
            "invalid runtime id length"
        );
        (lo..=hi)
            .map(|i| {
                let mut v = 0i32;
                SafeArrayGetElement(arr.0, &i, (&mut v as *mut i32).cast())?;
                Ok(v)
            })
            .collect()
    }
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn with_uia<T: Send>(f: impl FnOnce(&IUIAutomation2) -> Result<T> + Send) -> Result<T> {
    // UIA recommends a non-window-owning MTA thread. Only owned data leaves it.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("coda-save-uia".into())
            .spawn_scoped(scope, || unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
                let _apt = Apartment;
                let uia: IUIAutomation2 =
                    CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
                uia.SetConnectionTimeout(1500)?;
                uia.SetTransactionTimeout(1500)?;
                f(&uia)
            })?
            .join()
            .map_err(|_| anyhow::anyhow!("UI Automation worker stopped"))?
    })
}

pub(super) fn observe(target: &NativeTarget, hwnd: usize) -> Result<Option<SaveControls>> {
    let epoch = target.foreground_generation();
    with_uia(|uia| {
        Ok(Dialog::open(target, hwnd, epoch)
            .and_then(|d| d.bind(uia))
            .ok()
            .map(|b| b.ids))
    })
}

fn dialog_path(path: &Path) -> Result<BSTR> {
    let prefix = path.components().next().and_then(|c| {
        if let std::path::Component::Prefix(p) = c {
            Some(p.kind())
        } else {
            None
        }
    });
    ensure!(
        matches!(prefix, Some(Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
            && path.is_absolute()
            && path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("txt")),
        "expected absolute local .txt path"
    );
    let s = path.to_str().context("unsupported filename encoding")?;
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    // The common file dialog interprets environment substitutions and parses
    // ordinary DOS paths (not verbatim paths). Never approve one spelling and
    // silently submit a path with different semantics.
    ensure!(
        !s.contains(['\0', '%'])
            && s[2..].split(['\\', '/']).all(|part| {
                !part.ends_with([' ', '.']) && !part.contains([':', '"', '*', '?', '<', '>', '|'])
            }),
        "path is not literal in the Windows save dialog"
    );
    Ok(BSTR::from(s))
}

pub(super) fn submit(
    target: &NativeTarget,
    hwnd: usize,
    expected: &SaveControls,
    path: &Path,
) -> Result<()> {
    let epoch = target.foreground_generation();
    with_uia(|uia| {
        let requested = dialog_path(path)?;
        let dialog = Dialog::open(target, hwnd, epoch)?;
        let bind = || -> Result<Bound> {
            let bound = dialog.bind(uia)?;
            ensure!(&bound.ids == expected, "save controls changed; reobserve");
            Ok(bound)
        };
        let bound = bind()?;
        dialog.check()?;
        unsafe {
            bound.value.SetValue(&requested)?;
        }
        let bound = bind()?;
        ensure!(
            unsafe { bound.value.CurrentValue()? } == requested,
            "filename readback mismatch; not invoking Save"
        );
        ensure!(
            matches!(path.symlink_metadata(), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
            "destination no longer known to be new"
        );
        dialog.check()?;
        unsafe { bound.invoke.Invoke() }
            .context("Save dispatch outcome unknown; verify without retry")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_button_is_not_a_save_action() {
        assert!(Language::dialog("Open").is_none());
        assert!(Language::dialog("Confirm Save As").is_none());
        let lang = Language::dialog("Save As").unwrap();
        assert!(lang.save("Save") && lang.filename("File name:"));
        assert!(!lang.save("Open") && !lang.save("OK") && !lang.filename("Search"));
        assert!(!Language::dialog("另存为").unwrap().save("打开(O)"));
    }
    #[test]
    fn filename_requires_local_absolute_txt() {
        for path in [
            r"a.txt",
            r"C:a.txt",
            r"\\server\x\a.txt",
            r"C:\tmp\a.exe",
            r"C:\%TEMP%\a.txt",
            r"\\?\C:\trailing. \a.txt",
            r"C:\tmp\f:stream.txt",
        ] {
            assert!(dialog_path(Path::new(path)).is_err());
        }
        assert_eq!(
            dialog_path(Path::new(r"\\?\C:\tmp\样例.txt"))
                .unwrap()
                .to_string(),
            r"C:\tmp\样例.txt"
        );
    }

    #[test]
    fn runtime_ids_preserve_signed_values_and_nonzero_lower_bound() -> Result<()> {
        use windows::Win32::System::Variant::VT_I2;
        unsafe {
            let arr = Array(SafeArrayCreateVector(VT_I4, 3, 2));
            ensure!(!arr.0.is_null());
            for (index, value) in [(3i32, 42i32), (4, -77)] {
                SafeArrayPutElement(arr.0, &index, (&value as *const i32).cast())?;
            }
            assert_eq!(decode_id(arr)?, vec![42, -77]);
            assert!(decode_id(Array(SafeArrayCreateVector(VT_I2, 0, 2))).is_err());
            assert!(decode_id(Array(SafeArrayCreateVector(VT_I4, 0, 65))).is_err());
        }
        Ok(())
    }
}
