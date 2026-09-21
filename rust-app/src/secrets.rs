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

/// Persistence boundary for events, task JSON, and later Engine/HTTP writes.
///
/// Field names and token-shaped text are both rewritten.  Callers must store
/// the returned value instead of the original; checking only `sk-` is not
/// enough for provider tokens.
pub fn redact_persisted(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (key, child) in map {
                if sensitive_field(key) {
                    redacted.insert(key.clone(), serde_json::Value::String("[redacted]".into()));
                } else {
                    redacted.insert(key.clone(), redact_persisted(child));
                }
            }
            serde_json::Value::Object(redacted)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_persisted).collect())
        }
        serde_json::Value::String(text) => serde_json::Value::String(redact_text(text)),
        other => other.clone(),
    }
}

fn sensitive_field(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace('-', "_");
    matches!(
        name.as_str(),
        "token"
            | "access_token"
            | "refresh_token"
            | "id_token"
            | "api_key"
            | "apikey"
            | "secret"
            | "password"
            | "authorization"
            | "bearer"
            | "credential"
            | "credentials"
            | "private_key"
            | "key"
    ) || name.ends_with("_token")
        || name.ends_with("_secret")
        || name.ends_with("_password")
        || name.ends_with("_key")
        || name.ends_with("_authorization")
}

fn redact_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if let Some(end) = bearer_end(input, index) {
            out.push_str("bearer [redacted]");
            index = end;
            continue;
        }
        if let Some(end) = assignment_end(input, index) {
            let key_end = input[index..end]
                .find(['=', ':'])
                .map(|offset| index + offset + 1)
                .unwrap_or(end);
            out.push_str(&input[index..key_end]);
            out.push_str("[redacted]");
            index = end;
            continue;
        }
        if let Some(end) = token_end(input, index) {
            out.push_str("[redacted]");
            index = end;
            continue;
        }
        let rest = input.get(index..).unwrap_or("");
        let ch = rest.chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        index = index.saturating_add(ch.len_utf8()).min(input.len());
    }
    out
}

fn bearer_end(input: &str, index: usize) -> Option<usize> {
    let rest = input.get(index..)?;
    let prefix = "bearer ";
    if rest.len() < prefix.len()
        || !rest.is_char_boundary(prefix.len())
        || !rest[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        return None;
    }
    if index > 0 {
        let previous = input[..index].chars().next_back()?;
        if previous.is_ascii_alphanumeric() {
            return None;
        }
    }
    let mut end = index + prefix.len();
    let bytes = input.as_bytes();
    while end < bytes.len() && bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    let token = end;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    (end > token).then_some(end)
}

fn assignment_end(input: &str, index: usize) -> Option<usize> {
    const NAMES: &[&str] = &[
        "api_key",
        "apikey",
        "api-key",
        "access_token",
        "token",
        "secret",
        "password",
        "authorization",
    ];
    let rest = input.get(index..)?;
    let name = NAMES.iter().find(|name| {
        rest.len() > name.len()
            && rest.is_char_boundary(name.len())
            && rest[..name.len()].eq_ignore_ascii_case(name)
    })?;
    if index > 0 {
        let previous = input[..index].chars().next_back()?;
        if previous.is_ascii_alphanumeric() || previous == '_' {
            return None;
        }
    }
    let mut end = index + name.len();
    let bytes = input.as_bytes();
    if end >= bytes.len() || !matches!(bytes[end], b'=' | b':') {
        return None;
    }
    end += 1;
    while end < bytes.len() && bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    let token = end;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b',' {
        end += 1;
    }
    (end > token).then_some(end)
}

fn token_end(input: &str, index: usize) -> Option<usize> {
    const PREFIXES: &[&str] = &[
        "github_pat_",
        "sk-",
        "pk-",
        "rk-",
        "xai-",
        "ghp_",
        "gho_",
        "pat_",
        "ntn_",
        "AKIA",
    ];
    if index > 0 {
        let previous = input[..index].chars().next_back()?;
        if previous.is_ascii_alphanumeric() || previous == '_' || previous == '-' {
            return None;
        }
    }
    let rest = &input[index..];
    let prefix = PREFIXES
        .iter()
        .find(|prefix| rest.len() > prefix.len() && rest.starts_with(*prefix))?;
    let mut end = index + prefix.len();
    let bytes = input.as_bytes();
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'-'))
    {
        end += 1;
    }
    (end > index + prefix.len()).then_some(end)
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

    #[test]
    fn persisted_values_redact_nested_and_non_sk_tokens() {
        let value = serde_json::json!({
            "text": "delta bearer peach-token and xai-notaskey",
            "tool": {"api_key": "plain-provider-token", "path": "src/main.rs"},
            "error": "failed token=ntn_accountsecret"
        });
        let redacted = redact_persisted(&value);
        let blob = redacted.to_string();
        assert!(!blob.contains("peach-token"));
        assert!(!blob.contains("xai-notaskey"));
        assert!(!blob.contains("plain-provider-token"));
        assert!(!blob.contains("ntn_accountsecret"));
        assert!(blob.contains("src/main.rs"));
        assert_eq!(redacted["tool"]["api_key"], "[redacted]");
    }
}
