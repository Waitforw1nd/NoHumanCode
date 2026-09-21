use anyhow::{Result, bail};

// Secrets are bound to the current Windows account, never serialized in API responses.
#[cfg(windows)]
pub fn seal(value: &[u8]) -> Result<Vec<u8>> {
    crypt(value, false)
}
#[cfg(windows)]
pub fn open(value: &[u8]) -> Result<Vec<u8>> {
    crypt(value, true)
}

#[cfg(windows)]
fn crypt(value: &[u8], decrypt: bool) -> Result<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: value.len().try_into()?,
        pbData: value.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    // DPAPI owns the output allocation. Copy before LocalFree on both paths.
    unsafe {
        let ok = if decrypt {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            bail!("Windows 凭据加密失败：{}", std::io::Error::last_os_error());
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        Ok(bytes)
    }
}
#[cfg(not(windows))]
pub fn seal(_: &[u8]) -> Result<Vec<u8>> {
    bail!("此构建请通过环境变量提供 Key；持久化凭据需要 Windows DPAPI")
}
#[cfg(not(windows))]
pub fn open(_: &[u8]) -> Result<Vec<u8>> {
    bail!("此构建不能解密 Windows 凭据")
}

pub fn scrub(text: &str, secret: &str) -> String {
    let text = if secret.is_empty() {
        text.to_owned()
    } else {
        text.replace(secret, "[redacted]")
    };
    text.split_whitespace()
        .map(|word| {
            if word.starts_with("sk-") {
                "[redacted]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(400)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip() {
        let encrypted = seal(b"test-local-secret").unwrap();
        assert!(!encrypted.windows(17).any(|w| w == b"test-local-secret"));
        assert_eq!(open(&encrypted).unwrap(), b"test-local-secret");
    }
    #[test]
    fn redaction() {
        assert_eq!(
            scrub("bad sk-secret and abc", "abc"),
            "bad [redacted] and [redacted]"
        );
    }
}
