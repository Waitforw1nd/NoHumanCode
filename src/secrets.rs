use anyhow::Result;

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
            anyhow::bail!("Windows 凭据加密失败：{}", std::io::Error::last_os_error());
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        Ok(bytes)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
mod vault;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
pub use vault::{open, release, seal};
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
pub(crate) use vault::{reconcile, seal_snapshot, stage};

#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "android"
)))]
pub fn seal(_: &[u8]) -> Result<Vec<u8>> {
    anyhow::bail!("此平台没有系统凭据后端")
}
#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "android"
)))]
pub fn open(value: &[u8]) -> Result<Vec<u8>> {
    seal(value)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
pub(crate) fn stage(_: &rusqlite::Connection, value: &[u8]) -> Result<Vec<u8>> {
    seal(value)
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
pub(crate) use stage as seal_snapshot;

pub(crate) fn credential_transaction<T>(
    db: &mut rusqlite::Connection,
    write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let result = (|| {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
        vault::adopt_copy(&tx)?;
        let value = write(&tx)?;
        tx.commit()?;
        Ok(value)
    })();
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    reconcile(db);
    result
}

pub(crate) fn read_stored(db: &mut rusqlite::Connection, id: &str) -> Result<Option<String>> {
    read_with(db, id, open)
}

fn read_with(
    db: &mut rusqlite::Connection,
    id: &str,
    load: impl Fn(&[u8]) -> Result<Vec<u8>>,
) -> Result<Option<String>> {
    fn read_ref(db: &rusqlite::Connection, id: &str) -> Result<Option<Vec<u8>>> {
        use rusqlite::OptionalExtension;
        Ok(db
            .query_row("SELECT value FROM secrets WHERE id=?1", [id], |r| r.get(0))
            .optional()?)
    }
    let Some(blob) = read_ref(db, id)? else {
        return Ok(None);
    };
    match load(&blob) {
        Ok(bytes) => Ok(Some(String::from_utf8(bytes)?)),
        Err(error) => {
            // A writer could retire the selected item between SELECT and the OS lookup.
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let current = read_ref(&tx, id)?;
            if current.as_ref() == Some(&blob) {
                return Err(error);
            }
            current
                .map(|v| Ok(String::from_utf8(load(&v)?)?))
                .transpose()
        }
    }
}

// The authorization ledger needs only an equality check, never a second usable API key.
#[cfg(not(windows))]
fn snapshot_digest(salt: &[u8], value: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::new()
        .chain_update(b"coda:credential-check:v1")
        .chain_update(salt)
        .chain_update(value)
        .finalize()
        .into()
}

pub(crate) fn credential_snapshot(value: &[u8]) -> Result<Vec<u8>> {
    #[cfg(windows)]
    {
        seal(value)
    }
    #[cfg(not(windows))]
    {
        let salt = uuid::Uuid::new_v4();
        Ok([
            salt.as_bytes().as_slice(),
            &snapshot_digest(salt.as_bytes(), value),
        ]
        .concat())
    }
}

pub(crate) fn matches_credential_snapshot(blob: &[u8], value: &[u8]) -> bool {
    #[cfg(windows)]
    {
        open(blob).is_ok_and(|old| old == value)
    }
    #[cfg(not(windows))]
    {
        blob.split_at_checked(16)
            .is_some_and(|(salt, digest)| digest == snapshot_digest(salt, value))
    }
}

pub fn validate_persisted_id(field: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!value.is_empty() && value.len() <= 128, "{field} 长度无效");
    anyhow::ensure!(
        !value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace()),
        "{field} 包含空白或控制字符"
    );
    Ok(())
}

/// Visible ASCII bytes (1..=200), independent of persisted object IDs.
pub fn validate_idempotency_key(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= 200,
        "idempotency_key 长度无效"
    );
    anyhow::ensure!(
        value.bytes().all(|byte| byte.is_ascii_graphic()),
        "idempotency_key 必须为可见 ASCII 字符"
    );
    Ok(())
}

pub fn safe_metadata_text(field: &str, value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(value.len() <= 500, "{field} 长度无效");
    safe_metadata_path(field, value)
}

pub fn safe_metadata_path(field: &str, value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!value.trim().is_empty(), "{field} 不能为空");
    anyhow::ensure!(
        !value.chars().any(|ch| ch.is_control()),
        "{field} 包含控制字符"
    );
    Ok(value.to_owned())
}

/// Redact exact JSON object credential keys while leaving normal string content untouched.
pub fn redact_persisted(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut redacted = serde_json::Map::with_capacity(map.len());
            for (key, child) in map {
                if is_credential_field(key) {
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
        other => other.clone(),
    }
}

fn normalize_field(name: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (index, &ch) in chars.iter().enumerate() {
        if matches!(ch, '-' | '.' | ' ' | '_') {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            continue;
        }
        let previous = index.checked_sub(1).map(|i| chars[i]);
        let next_is_lowercase = chars.get(index + 1).is_some_and(char::is_ascii_lowercase);
        let word_boundary = previous.is_some_and(|prev| {
            prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_is_lowercase)
        });
        if ch.is_ascii_uppercase() && word_boundary && !out.ends_with('_') {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

fn is_credential_field(name: &str) -> bool {
    matches!(
        normalize_field(name).as_str(),
        "api_key"
            | "apikey"
            | "secret"
            | "password"
            | "authorization"
            | "access_token"
            | "refresh_token"
            | "client_secret"
            | "credential"
    )
}

pub fn scrub(text: &str, secret: &str) -> String {
    if secret.trim().is_empty() {
        text.to_owned()
    } else {
        text.replace(secret, "[redacted]")
    }
}

/// Pass-through compatibility stub for callers that thread completion streams;
/// performs zero heuristic redaction so generated code is never corrupted.
#[allow(dead_code)]
#[derive(Default)]
pub(crate) struct StreamRedactor;

#[allow(dead_code)]
impl StreamRedactor {
    pub(crate) fn push(&mut self, input: &str) -> String {
        input.to_owned()
    }

    pub(crate) fn finish(self) -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_committed_replacement_if_a_writer_retires_the_old_reference() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("race.db");
        let mut db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE secrets(id TEXT,value BLOB); INSERT INTO secrets VALUES('r',X'01')",
        )
        .unwrap();
        let writer = rusqlite::Connection::open(path).unwrap();
        let got = read_with(&mut db, "r", |reference| {
            if reference == [1] {
                writer.execute("UPDATE secrets SET value=X'02'", [])?;
                anyhow::bail!("old item retired");
            }
            Ok(b"replacement".to_vec())
        })
        .unwrap();
        assert!(got.as_deref() == Some("replacement"));
        let count = std::cell::Cell::new(0);
        assert!(
            read_with(&mut db, "r", |_| {
                count.set(count.get() + 1);
                anyhow::bail!("locked")
            })
            .is_err()
        );
        assert_eq!(count.get(), 1);
    }

    #[cfg(not(windows))]
    #[test]
    fn credential_snapshot_distinguishes_rotations_without_a_recoverable_copy() {
        let bytes = uuid::Uuid::new_v4().to_string();
        let a = credential_snapshot(bytes.as_bytes()).unwrap();
        let b = credential_snapshot(bytes.as_bytes()).unwrap();
        assert!(a != b);
        assert!(matches_credential_snapshot(&a, bytes.as_bytes()));
        assert!(!matches_credential_snapshot(&a, b"changed"));
        assert!(!matches_credential_snapshot(&a[..15], bytes.as_bytes()));
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_roundtrip() {
        let encrypted = seal(b"test-local-secret").unwrap();
        assert!(!encrypted.windows(17).any(|w| w == b"test-local-secret"));
        assert_eq!(open(&encrypted).unwrap(), b"test-local-secret");
    }

    #[test]
    fn exact_scrub_preserves_ordinary_code_and_whitespace() {
        let sample = "use sk-learn;\nBearer auth failed with secret-xyz";
        assert_eq!(
            scrub(sample, "secret-xyz"),
            "use sk-learn;\nBearer auth failed with [redacted]"
        );
        assert_eq!(scrub(sample, ""), sample);
        assert_eq!(scrub(sample, "   "), sample);
    }

    #[test]
    fn redact_persisted_redacts_credential_keys_and_preserves_code_strings() {
        let code = "let cancel_token = 1;\nstruct Auth { password: String }\nmacro_rules! m { ($x:pat_param) => {}; }\n// sk-learn Bearer auth";
        let input = serde_json::json!({
            "content": code,
            "arguments": "{\"patch\":\"password = \\\"unchanged\\\"\"}",
            "nested": {
                "api_key": "k1",
                "APIKey": "k2",
                "apikey": "k3",
                "accessToken": "k4",
                "refresh_token": "k5",
                "clientSecret": "k6",
                "password": "k7",
                "secret": "k8",
                "authorization": "Bearer k9",
                "credential": "k10",
                "max_tokens": 4096
            }
        });
        let out = redact_persisted(&input);
        assert_eq!(out["content"], code);
        assert_eq!(out["arguments"], input["arguments"]);
        assert_eq!(out["nested"]["max_tokens"], 4096);
        for key in [
            "api_key",
            "APIKey",
            "apikey",
            "accessToken",
            "refresh_token",
            "clientSecret",
            "password",
            "secret",
            "authorization",
            "credential",
        ] {
            assert_eq!(out["nested"][key], "[redacted]", "{key}");
        }
    }

    #[test]
    fn metadata_and_id_validators_check_bounds_and_controls_only() {
        assert_eq!(
            safe_metadata_path("root_path", r"E:\code\pat_demo").unwrap(),
            r"E:\code\pat_demo"
        );
        assert_eq!(
            safe_metadata_text("title", "Fix Bearer auth and sk-learn import").unwrap(),
            "Fix Bearer auth and sk-learn import"
        );
        validate_persisted_id("id", "pat_demo-1").unwrap();
        validate_idempotency_key("retry:api_key=1").unwrap();
        assert!(validate_persisted_id("id", "").is_err());
        assert!(validate_persisted_id("id", "bad id").is_err());
        assert!(safe_metadata_text("title", "bad\ntitle").is_err());
        assert!(safe_metadata_path("path", "   ").is_err());
    }
}
