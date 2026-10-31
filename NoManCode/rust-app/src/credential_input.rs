//! Local credential acquisition; no logging or fallback to echoed input.
//! Memory clearing is best effort. The caller owns the consumed String.
use std::io::{self, IsTerminal, Read};
use std::sync::atomic::{Ordering, compiler_fence};

const MAX_BYTES: usize = 4095;
// Maximum credential, optional CRLF, and one byte proving excess input.
const INPUT_CAPACITY: usize = MAX_BYTES + 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialInputMode {
    Prompt,
    Stdin,
}

/// Deliberately has no Debug, Display, Clone, or serialization implementation.
pub struct SecretInput(String);

impl SecretInput {
    pub fn into_string(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl From<SecretInput> for String {
    fn from(value: SecretInput) -> Self {
        value.into_string()
    }
}

impl Drop for SecretInput {
    fn drop(&mut self) {
        // SAFETY: zero bytes are valid UTF-8 and the String is not used again.
        wipe(unsafe { self.0.as_bytes_mut() });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialInputError {
    Cancelled,
    Unavailable,
    Invalid,
}

impl std::fmt::Display for CredentialInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "credential input cancelled",
            Self::Unavailable => "secure credential input unavailable",
            Self::Invalid => "invalid credential input",
        })
    }
}

impl std::error::Error for CredentialInputError {}

/// Prompt uses the real Windows console independently of redirected stdio.
/// Stdin requires a redirected stream and EOF, detecting data after the first
/// line. Empty EOF is cancellation; a nonempty final line need not end with LF.
/// No environment variable or credential store is read.
pub fn read_credential(mode: CredentialInputMode) -> Result<SecretInput, CredentialInputError> {
    match mode {
        CredentialInputMode::Prompt => read_prompt(),
        CredentialInputMode::Stdin => {
            let stdin = io::stdin();
            if stdin.is_terminal() {
                return Err(CredentialInputError::Unavailable);
            }
            #[cfg(windows)]
            windows_console::require_redirected_stdin(&stdin)?;
            read_redirected(&mut stdin.lock())
        }
    }
}

fn wipe<T: Copy + Default>(values: &mut [T]) {
    for value in values {
        // SAFETY: each pointer is a live exclusively borrowed element.
        unsafe { std::ptr::write_volatile(value, T::default()) };
    }
    compiler_fence(Ordering::SeqCst);
}

struct InputBytes([u8; INPUT_CAPACITY]);

impl Drop for InputBytes {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

fn validate(secret: SecretInput) -> Result<SecretInput, CredentialInputError> {
    // These exclusions reject all bytes forbidden in an HTTP HeaderValue.
    // Preserve valid non-ASCII UTF-8, as the existing Bearer transport does;
    // additionally reject Unicode whitespace and control characters.
    if secret.0.is_empty()
        || secret.0.len() > MAX_BYTES
        || secret
            .0
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(CredentialInputError::Invalid);
    }
    Ok(secret)
}

fn read_redirected(reader: &mut impl Read) -> Result<SecretInput, CredentialInputError> {
    let mut bytes = InputBytes([0; INPUT_CAPACITY]);
    let mut used = 0;
    loop {
        if used == INPUT_CAPACITY {
            return Err(CredentialInputError::Invalid);
        }
        match reader.read(&mut bytes.0[used..]) {
            Ok(0) => break,
            Ok(count) => used += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                return Err(CredentialInputError::Cancelled);
            }
            Err(_) => return Err(CredentialInputError::Unavailable),
        }
    }
    if used == 0 {
        return Err(CredentialInputError::Cancelled);
    }
    if bytes.0[used - 1] == b'\n' {
        used -= 1;
        if used > 0 && bytes.0[used - 1] == b'\r' {
            used -= 1;
        }
    }
    let text = std::str::from_utf8(&bytes.0[..used]).map_err(|_| CredentialInputError::Invalid)?;
    validate(SecretInput(text.to_owned()))
}

#[cfg(not(windows))]
fn read_prompt() -> Result<SecretInput, CredentialInputError> {
    Err(CredentialInputError::Unavailable)
}

#[cfg(windows)]
fn read_prompt() -> Result<SecretInput, CredentialInputError> {
    windows_console::read_prompt()
}

#[cfg(windows)]
mod windows_console {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use std::sync::Mutex;
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType,
        },
        System::Console::{
            ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
            ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT, FlushConsoleInputBuffer,
            GetConsoleMode, GetNumberOfConsoleInputEvents, INPUT_RECORD, KEY_EVENT,
            ReadConsoleInputW, SetConsoleMode,
        },
    };

    static PROMPT_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(test)]
    static PROMPT_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub(super) fn require_redirected_stdin(stdin: &io::Stdin) -> Result<(), CredentialInputError> {
        // Failed terminal probing must not admit an unknown console/device.
        let kind = unsafe { GetFileType(stdin.as_raw_handle() as HANDLE) };
        if matches!(kind, FILE_TYPE_DISK | FILE_TYPE_PIPE) {
            Ok(())
        } else {
            Err(CredentialInputError::Unavailable)
        }
    }

    fn console(name: &str) -> Result<File, CredentialInputError> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(name)
            .map_err(|_| CredentialInputError::Unavailable)
    }

    fn mode(handle: HANDLE) -> Result<u32, CredentialInputError> {
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            Err(CredentialInputError::Unavailable)
        } else {
            Ok(mode)
        }
    }

    struct ConsoleMode<'a> {
        input: &'a File,
        original: u32,
        active: bool,
    }

    impl<'a> ConsoleMode<'a> {
        fn acquire(input: &'a File) -> Result<Self, CredentialInputError> {
            let handle = input.as_raw_handle() as HANDLE;
            let original = mode(handle)?;
            let guard = Self {
                input,
                original,
                active: true,
            };
            // Deliver Ctrl+C as input, allowing ordinary unwinding/restoration.
            let hidden = (original | ENABLE_EXTENDED_FLAGS)
                & !(ENABLE_ECHO_INPUT
                    | ENABLE_LINE_INPUT
                    | ENABLE_PROCESSED_INPUT
                    | ENABLE_QUICK_EDIT_MODE
                    | ENABLE_VIRTUAL_TERMINAL_INPUT);
            if unsafe { SetConsoleMode(handle, hidden) } == 0 || mode(handle)? != hidden {
                return Err(CredentialInputError::Unavailable);
            }
            // Discard typeahead before showing the prompt, with echo disabled.
            if unsafe { FlushConsoleInputBuffer(handle) } == 0 {
                return Err(CredentialInputError::Unavailable);
            }
            Ok(guard)
        }

        fn restore(&mut self) -> Result<(), CredentialInputError> {
            let handle = self.input.as_raw_handle() as HANDLE;
            // Rejected/pasted suffixes must not reach the next shell read.
            let flushed = unsafe { FlushConsoleInputBuffer(handle) } != 0;
            let restored = unsafe { SetConsoleMode(handle, self.original) } != 0;
            if restored && mode(handle)? == self.original {
                self.active = false;
                if flushed {
                    return Ok(());
                }
            }
            Err(CredentialInputError::Unavailable)
        }
    }

    impl Drop for ConsoleMode<'_> {
        fn drop(&mut self) {
            if self.active {
                let _ = self.restore();
            }
        }
    }

    struct InputUnits(Vec<u16>);

    impl Drop for InputUnits {
        fn drop(&mut self) {
            wipe(&mut self.0);
        }
    }

    fn next_key(handle: HANDLE) -> Result<Option<(u16, u16)>, CredentialInputError> {
        // Zero is valid for every field/union in INPUT_RECORD.
        let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
        let mut count = 0;
        if unsafe { ReadConsoleInputW(handle, &mut record, 1, &mut count) } == 0 {
            return Err(CredentialInputError::Unavailable);
        }
        if count != 1 {
            return Err(CredentialInputError::Cancelled);
        }
        if record.EventType != KEY_EVENT as u16 {
            return Ok(None);
        }
        let key = unsafe { &mut record.Event.KeyEvent };
        let ch = unsafe { key.uChar.UnicodeChar };
        key.uChar.UnicodeChar = 0;
        if key.bKeyDown == 0 || key.wRepeatCount == 0 {
            return Ok(None);
        }
        // Ignore only non-text modifier keys. NUL from Ctrl+Space/Ctrl+2 is invalid.
        if ch == 0 && matches!(key.wVirtualKeyCode, 16..=18 | 20 | 91 | 92 | 144 | 145) {
            return Ok(None);
        }
        Ok(Some((ch, key.wRepeatCount)))
    }

    fn reject_queued_suffix(handle: HANDLE, after_cr: bool) -> Result<(), CredentialInputError> {
        let mut count = 0;
        if unsafe { GetNumberOfConsoleInputEvents(handle, &mut count) } == 0 {
            return Err(CredentialInputError::Unavailable);
        }
        if count > 16384 {
            return Err(CredentialInputError::Invalid);
        }
        let mut allow_lf = after_cr;
        for _ in 0..count {
            if let Some((ch, repeats)) = next_key(handle)? {
                if ch == 10 && repeats == 1 && allow_lf {
                    allow_lf = false;
                } else {
                    return Err(CredentialInputError::Invalid);
                }
            }
        }
        Ok(())
    }

    fn read_hidden(handle: HANDLE) -> Result<SecretInput, CredentialInputError> {
        let mut units = InputUnits(Vec::with_capacity(MAX_BYTES));
        loop {
            let Some((ch, repeats)) = next_key(handle)? else {
                continue;
            };
            match ch {
                3 | 4 | 26 | 27 => return Err(CredentialInputError::Cancelled),
                10 | 13 => {
                    if repeats != 1 {
                        return Err(CredentialInputError::Invalid);
                    }
                    reject_queued_suffix(handle, ch == 13)?;
                    let text =
                        String::from_utf16(&units.0).map_err(|_| CredentialInputError::Invalid)?;
                    return validate(SecretInput(text));
                }
                0..=31 | 127 => return Err(CredentialInputError::Invalid),
                _ => {
                    if units.0.len() + usize::from(repeats) > MAX_BYTES {
                        return Err(CredentialInputError::Invalid);
                    }
                    units
                        .0
                        .extend(std::iter::repeat_n(ch, usize::from(repeats)));
                }
            }
        }
    }

    pub(super) fn read_prompt() -> Result<SecretInput, CredentialInputError> {
        let _lock = PROMPT_LOCK
            .lock()
            .map_err(|_| CredentialInputError::Unavailable)?;
        let input = console("CONIN$")?;
        let mut output = console("CONOUT$")?;
        mode(output.as_raw_handle() as HANDLE)?;
        let mut guard = ConsoleMode::acquire(&input)?;
        let result = (|| {
            output
                .write_all(b"Credential (hidden): ")
                .map_err(|_| CredentialInputError::Unavailable)?;
            #[cfg(test)]
            PROMPT_READY.store(true, Ordering::SeqCst);
            let result = read_hidden(input.as_raw_handle() as HANDLE);
            output
                .write_all(b"\r\n")
                .map_err(|_| CredentialInputError::Unavailable)?;
            result
        })();
        // Restoration failure overrides successful credential acquisition.
        guard.restore()?;
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        use windows_sys::Win32::System::Console::{
            CONSOLE_SCREEN_BUFFER_INFO, COORD, GetConsoleProcessList, GetConsoleScreenBufferInfo,
            ReadConsoleOutputCharacterW, WriteConsoleInputW,
        };

        fn exercise_prompt(units: Vec<u16>, expected: Result<usize, CredentialInputError>) {
            let input = console("CONIN$").unwrap();
            let handle = input.as_raw_handle() as HANDLE;
            let original = mode(handle).unwrap();
            PROMPT_READY.store(false, Ordering::SeqCst);
            let feeder = std::thread::spawn(move || {
                let input = console("CONIN$").unwrap();
                let handle = input.as_raw_handle() as HANDLE;
                let deadline = Instant::now() + Duration::from_secs(5);
                while !PROMPT_READY.load(Ordering::SeqCst) {
                    assert!(Instant::now() < deadline, "prompt did not become ready");
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(mode(handle).unwrap() & ENABLE_ECHO_INPUT, 0);
                let mut records: Vec<INPUT_RECORD> = units
                    .into_iter()
                    .map(|ch| {
                        let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
                        record.EventType = KEY_EVENT as u16;
                        let key = unsafe { &mut record.Event.KeyEvent };
                        key.bKeyDown = 1;
                        key.wRepeatCount = 1;
                        key.uChar.UnicodeChar = ch;
                        record
                    })
                    .collect();
                let mut written = 0;
                let ok = unsafe {
                    WriteConsoleInputW(handle, records.as_ptr(), records.len() as u32, &mut written)
                };
                for record in &mut records {
                    record.Event.KeyEvent.uChar.UnicodeChar = 0;
                }
                assert_ne!(ok, 0);
                assert_eq!(written as usize, records.len());
            });
            let result = read_credential(CredentialInputMode::Prompt)
                .map(|secret| secret.into_string().len());
            feeder.join().unwrap();
            assert_eq!(result, expected);
            assert_eq!(
                mode(handle).unwrap(),
                original,
                "console mode must be restored exactly"
            );
            let mut pending = 0;
            assert_ne!(
                unsafe { GetNumberOfConsoleInputEvents(handle, &mut pending) },
                0
            );
            assert_eq!(
                pending, 0,
                "no rejected secret suffix may reach the next reader"
            );
        }

        // This helper only operates in an explicitly created private console.
        #[test]
        fn isolated_console_child() {
            if std::env::var("NHC_INPUT_PRIVATE_CONSOLE").as_deref() != Ok("1") {
                return;
            }
            let mut processes = [0_u32; 8];
            assert_eq!(
                unsafe { GetConsoleProcessList(processes.as_mut_ptr(), 8) },
                1,
                "refuse to operate on a shared/user console"
            );
            let input = console("CONIN$").unwrap();
            let handle = input.as_raw_handle() as HANDLE;
            let original = mode(handle).unwrap();
            assert!(matches!(
                read_credential(CredentialInputMode::Stdin),
                Err(CredentialInputError::Unavailable)
            ));
            for baseline in [
                original | ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT,
                original & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT),
            ] {
                assert_ne!(unsafe { SetConsoleMode(handle, baseline) }, 0);
                exercise_prompt("synthetic-console-value\r".encode_utf16().collect(), Ok(23));
                exercise_prompt("界\r\n".encode_utf16().collect(), Ok(3));
                exercise_prompt(
                    "x".repeat(MAX_BYTES).encode_utf16().chain([13]).collect(),
                    Ok(MAX_BYTES),
                );
                exercise_prompt(
                    "x".repeat(MAX_BYTES + 1)
                        .encode_utf16()
                        .chain([13])
                        .collect(),
                    Err(CredentialInputError::Invalid),
                );
                for units in [
                    vec![13],
                    vec![b'x' as u16, 9, 13],
                    vec![0, 13],
                    vec![0xd800, 13],
                    "a\rb\r".encode_utf16().collect(),
                    "a\r\n\r\n".encode_utf16().collect(),
                    " token\r".encode_utf16().collect(),
                ] {
                    exercise_prompt(units, Err(CredentialInputError::Invalid));
                }
                for cancel in [3, 4, 26, 27] {
                    exercise_prompt(
                        vec![b'x' as u16, cancel, b'y' as u16],
                        Err(CredentialInputError::Cancelled),
                    );
                }
                let unwound = std::panic::catch_unwind(|| {
                    let _guard = ConsoleMode::acquire(&input).unwrap();
                    panic!("intentional restoration probe");
                });
                assert!(unwound.is_err());
                assert_eq!(mode(handle).unwrap(), baseline);
            }
            assert_ne!(unsafe { SetConsoleMode(handle, original) }, 0);
            let output = console("CONOUT$").unwrap();
            let output_handle = output.as_raw_handle() as HANDLE;
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
            assert_ne!(
                unsafe { GetConsoleScreenBufferInfo(output_handle, &mut info) },
                0
            );
            let size =
                usize::try_from(info.dwSize.X).unwrap() * usize::try_from(info.dwSize.Y).unwrap();
            assert!(size < 4_000_000);
            let mut screen = vec![0_u16; size];
            let mut read = 0;
            assert_ne!(
                unsafe {
                    ReadConsoleOutputCharacterW(
                        output_handle,
                        screen.as_mut_ptr(),
                        size as u32,
                        COORD { X: 0, Y: 0 },
                        &mut read,
                    )
                },
                0
            );
            let screen = String::from_utf16_lossy(&screen[..read as usize]);
            assert!(!screen.contains("synthetic-console-value"));
            assert!(!screen.contains(&"x".repeat(32)));
        }

        #[test]
        fn private_console_prompt_has_no_echo_and_restores_on_all_normal_exits() {
            let root = std::env::temp_dir().join(format!(
                "nhc-console-probe-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let quote =
                |s: &std::path::Path| format!("'{}'", s.to_string_lossy().replace('\'', "''"));
            let exe = std::env::current_exe().unwrap();
            let stdout = root.join("child.stdout.log");
            let stderr = root.join("child.stderr.log");
            let child_name = format!(
                "{}::isolated_console_child",
                module_path!().split_once("::").unwrap().1
            );
            let script = format!(
                "$ErrorActionPreference='Stop'; $p=Start-Process -FilePath {} -ArgumentList '--exact','{}','--nocapture','--test-threads=1' -WindowStyle Hidden -RedirectStandardOutput {} -RedirectStandardError {} -PassThru; if(-not $p.WaitForExit(30000)){{$p.Kill();$p.WaitForExit();exit 124}}; exit $p.ExitCode",
                quote(&exe),
                child_name,
                quote(&stdout),
                quote(&stderr)
            );
            let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let result = Command::new(powershell)
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .env("NHC_INPUT_PRIVATE_CONSOLE", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "private console child failed; retained probe logs under TMP"
            );
            let stdout = std::fs::read(stdout).unwrap();
            let stderr = std::fs::read(stderr).unwrap();
            for output in [&stdout, &stderr, &result.stdout, &result.stderr] {
                assert!(
                    !output
                        .windows(23)
                        .any(|window| window == b"synthetic-console-value")
                );
            }
            assert!(String::from_utf8_lossy(&stdout).contains("1 passed"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[cfg(windows)]
    #[test]
    fn redirected_process_child() {
        let Ok(expected) = std::env::var("NHC_INPUT_CHILD_EXPECTED") else {
            return;
        };
        let mode = if expected == "no-console" {
            let mut processes = [0_u32; 2];
            assert_eq!(
                unsafe {
                    windows_sys::Win32::System::Console::GetConsoleProcessList(
                        processes.as_mut_ptr(),
                        2,
                    )
                },
                0
            );
            CredentialInputMode::Prompt
        } else {
            CredentialInputMode::Stdin
        };
        let result = read_credential(mode).map(|secret| secret.into_string().len());
        match expected.as_str() {
            "valid" => assert_eq!(result, Ok(MAX_BYTES)),
            "cancelled" => assert_eq!(result, Err(CredentialInputError::Cancelled)),
            "invalid" => assert_eq!(result, Err(CredentialInputError::Invalid)),
            "no-console" => assert_eq!(result, Err(CredentialInputError::Unavailable)),
            _ => panic!("unknown fixture case"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn public_stdin_uses_real_bounded_pipes_and_prompt_never_falls_back() {
        use std::io::Write;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let name = format!(
            "{}::redirected_process_child",
            module_path!().split_once("::").unwrap().1
        );
        for (bytes, expected) in [
            (vec![b'x'; MAX_BYTES], "valid"),
            ([vec![b'x'; MAX_BYTES], b"\r\n".to_vec()].concat(), "valid"),
            (Vec::new(), "cancelled"),
            (b"x\nnext".to_vec(), "invalid"),
            (b"x\xff".to_vec(), "invalid"),
            (b" x".to_vec(), "invalid"),
            (vec![b'x'; MAX_BYTES + 1], "invalid"),
            (
                b"synthetic-pipe-fallback-must-not-be-read".to_vec(),
                "no-console",
            ),
        ] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture", "--test-threads=1"])
                .env("NHC_INPUT_CHILD_EXPECTED", expected)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(if expected == "no-console" {
                    0x00000008
                } else {
                    0x08000000
                })
                .spawn()
                .unwrap();
            let mut input = child.stdin.take().unwrap();
            let _ = input.write_all(&bytes);
            drop(input);
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("input fixture exceeded its deadline: {expected}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "public input subprocess failed: {expected}"
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            for stream in [&output.stdout, &output.stderr] {
                assert!(!stream.windows(32).any(|window| window == vec![b'x'; 32]));
            }
        }
    }

    fn error(bytes: &[u8], expected: CredentialInputError) {
        let result = read_redirected(&mut Cursor::new(bytes));
        assert!(matches!(result, Err(actual) if actual == expected));
    }

    #[test]
    fn redirected_preserves_single_line_and_only_removes_one_ending() {
        for bytes in [
            b"synthetic-token".as_slice(),
            b"synthetic-token\n",
            b"synthetic-token\r\n",
        ] {
            let result = read_redirected(&mut Cursor::new(bytes))
                .unwrap()
                .into_string();
            assert!(result == "synthetic-token");
        }
        error(b"", CredentialInputError::Cancelled);
        for bytes in [
            b"\n".as_slice(),
            b"\r\n",
            b"s\r",
            b"s\n\n",
            b"s\r\nnext",
            b"s\nnext",
        ] {
            error(bytes, CredentialInputError::Invalid);
        }
    }

    #[test]
    fn whitespace_controls_and_bad_encoding_fail_without_trimming() {
        for text in [
            " token",
            "token ",
            "t\t",
            "t\0",
            "t\u{7f}",
            "t\u{85}",
            "t\u{a0}",
            "t\u{2003}",
            "t\u{2028}",
            "t\u{3000}",
        ] {
            error(text.as_bytes(), CredentialInputError::Invalid);
        }
        for bytes in [
            b"s\xff".as_slice(),
            b"\xc0\xaf",
            b"\xed\xa0\x80",
            b"s\xe2\x82",
        ] {
            error(bytes, CredentialInputError::Invalid);
        }
    }

    #[test]
    fn byte_limit_is_exact_with_each_line_ending() {
        for ending in ["", "\n", "\r\n"] {
            let mut bytes = vec![b'x'; MAX_BYTES];
            bytes.extend_from_slice(ending.as_bytes());
            assert!(read_redirected(&mut Cursor::new(bytes)).is_ok());
            let mut bytes = vec![b'x'; MAX_BYTES + 1];
            bytes.extend_from_slice(ending.as_bytes());
            error(&bytes, CredentialInputError::Invalid);
        }
        assert!(read_redirected(&mut Cursor::new("界".repeat(1365))).is_ok());
        error("界".repeat(1366).as_bytes(), CredentialInputError::Invalid);
    }

    #[test]
    fn unending_stream_is_read_only_to_the_fixed_bound() {
        struct Endless(usize);
        impl Read for Endless {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                bytes.fill(b'x');
                self.0 += bytes.len();
                Ok(bytes.len())
            }
        }
        let mut reader = Endless(0);
        assert!(matches!(
            read_redirected(&mut reader),
            Err(CredentialInputError::Invalid)
        ));
        assert_eq!(reader.0, INPUT_CAPACITY);
    }

    #[test]
    fn partial_reads_and_read_failures_never_return_partial_secret() {
        struct Failing {
            interrupted: bool,
            first: bool,
        }
        impl Read for Failing {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.first {
                    self.first = false;
                    bytes[0] = b'x';
                    return Ok(1);
                }
                Err(io::Error::new(
                    if self.interrupted {
                        io::ErrorKind::Interrupted
                    } else {
                        io::ErrorKind::Other
                    },
                    "synthetic sensitive error payload",
                ))
            }
        }
        for interrupted in [false, true] {
            let expected = if interrupted {
                CredentialInputError::Cancelled
            } else {
                CredentialInputError::Unavailable
            };
            let result = read_redirected(&mut Failing {
                interrupted,
                first: true,
            });
            assert!(matches!(result, Err(actual) if actual == expected));
        }
    }

    #[test]
    fn error_output_is_fixed_and_has_no_underlying_payload() {
        for (error, text) in [
            (
                CredentialInputError::Cancelled,
                "credential input cancelled",
            ),
            (
                CredentialInputError::Unavailable,
                "secure credential input unavailable",
            ),
            (CredentialInputError::Invalid, "invalid credential input"),
        ] {
            assert_eq!(error.to_string(), text);
            assert!(std::error::Error::source(&error).is_none());
        }
    }
}
