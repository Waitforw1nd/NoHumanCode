//! Local credential acquisition via Windows Console line input or redirected stdin.
use std::io::{self, IsTerminal, Read};
use std::sync::atomic::{Ordering, compiler_fence};

const MAX_BYTES: usize = 4095;
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
            CONSOLE_READCONSOLE_CONTROL, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
            ENABLE_PROCESSED_INPUT, GetConsoleMode, ReadConsoleW, SetConsoleMode,
        },
    };

    const ERROR_OPERATION_ABORTED: i32 = 995;
    static PROMPT_LOCK: Mutex<()> = Mutex::new(());

    pub(super) fn require_redirected_stdin(stdin: &io::Stdin) -> Result<(), CredentialInputError> {
        let kind = unsafe { GetFileType(stdin.as_raw_handle() as HANDLE) };
        if matches!(kind, FILE_TYPE_DISK | FILE_TYPE_PIPE) {
            Ok(())
        } else {
            Err(CredentialInputError::Unavailable)
        }
    }

    fn open_console(name: &str) -> Result<File, CredentialInputError> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(name)
            .map_err(|_| CredentialInputError::Unavailable)
    }

    fn console_mode(handle: HANDLE) -> Result<u32, CredentialInputError> {
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            Err(CredentialInputError::Unavailable)
        } else {
            Ok(mode)
        }
    }

    struct ConsoleModeGuard {
        handle: HANDLE,
        original: u32,
    }

    impl ConsoleModeGuard {
        fn acquire(input: &File) -> Result<Self, CredentialInputError> {
            let handle = input.as_raw_handle() as HANDLE;
            let original = console_mode(handle)?;
            let hidden =
                (original | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT) & !ENABLE_ECHO_INPUT;
            if unsafe { SetConsoleMode(handle, hidden) } == 0 {
                return Err(CredentialInputError::Unavailable);
            }
            Ok(Self { handle, original })
        }
    }

    impl Drop for ConsoleModeGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = SetConsoleMode(self.handle, self.original);
            }
        }
    }

    struct InputUnits(Vec<u16>);

    impl Drop for InputUnits {
        fn drop(&mut self) {
            wipe(&mut self.0);
        }
    }

    pub(super) fn read_prompt() -> Result<SecretInput, CredentialInputError> {
        let _lock = PROMPT_LOCK
            .lock()
            .map_err(|_| CredentialInputError::Unavailable)?;
        let input = open_console("CONIN$")?;
        let mut output = open_console("CONOUT$")?;
        console_mode(output.as_raw_handle() as HANDLE)?;
        let _guard = ConsoleModeGuard::acquire(&input)?;
        output
            .write_all(b"Credential (hidden): ")
            .map_err(|_| CredentialInputError::Unavailable)?;

        let mut units = InputUnits(vec![0u16; INPUT_CAPACITY]);
        let mut read = 0u32;
        let ok = unsafe {
            ReadConsoleW(
                input.as_raw_handle() as HANDLE,
                units.0.as_mut_ptr().cast(),
                units.0.len() as u32,
                &mut read,
                std::ptr::null::<CONSOLE_READCONSOLE_CONTROL>(),
            )
        };
        let os_err = io::Error::last_os_error();
        let _ = output.write_all(b"\r\n");

        if ok == 0 {
            return if os_err.raw_os_error() == Some(ERROR_OPERATION_ABORTED) {
                Err(CredentialInputError::Cancelled)
            } else {
                Err(CredentialInputError::Unavailable)
            };
        }
        let slice = &units.0[..read as usize];
        if slice.is_empty() || slice[0] == 0x1A || slice.contains(&0x03) {
            return Err(CredentialInputError::Cancelled);
        }
        if slice.len() == INPUT_CAPACITY && !matches!(slice.last(), Some(10 | 13)) {
            return Err(CredentialInputError::Invalid);
        }
        let mut text = String::from_utf16(slice).map_err(|_| CredentialInputError::Invalid)?;
        if text.ends_with('\n') {
            text.pop();
            if text.ends_with('\r') {
                text.pop();
            }
        } else if text.ends_with('\r') {
            text.pop();
        }
        validate(SecretInput(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn redirected_reads_single_line_and_handles_empty_eof() {
        for raw in [
            b"synthetic-token".as_slice(),
            b"synthetic-token\n",
            b"synthetic-token\r\n",
        ] {
            let got: String = read_redirected(&mut Cursor::new(raw)).unwrap().into();
            assert_eq!(got, "synthetic-token");
        }
        assert_eq!(
            read_redirected(&mut Cursor::new(b"")).err(),
            Some(CredentialInputError::Cancelled)
        );
    }

    #[test]
    fn validation_rejects_whitespace_controls_multiline_and_excess_length() {
        for bad in [
            b"\n".as_slice(),
            b"\r\n",
            b" leading",
            b"trailing ",
            b"has\ttab",
            b"has\0nul",
            b"line1\nline2",
            b"bad\xffutf8",
        ] {
            assert_eq!(
                read_redirected(&mut Cursor::new(bad)).err(),
                Some(CredentialInputError::Invalid)
            );
        }
        assert!(read_redirected(&mut Cursor::new(vec![b'x'; MAX_BYTES])).is_ok());
        assert_eq!(
            read_redirected(&mut Cursor::new(vec![b'x'; MAX_BYTES + 1])).err(),
            Some(CredentialInputError::Invalid)
        );
    }
}
