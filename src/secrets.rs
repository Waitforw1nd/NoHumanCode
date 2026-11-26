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

/// The exact credentials authorized for one provider request. Never formatted or serialized.
#[derive(Clone)]
pub(crate) struct CredentialSet(std::sync::Arc<[String]>);

impl CredentialSet {
    pub(crate) fn new(mut keys: Vec<String>) -> Self {
        keys.retain(|key| !key.trim().is_empty());
        keys.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        keys.dedup();
        Self(keys.into())
    }

    pub(crate) fn scrub(&self, text: &str) -> String {
        self.scrub_prefix(text, true).0
    }

    // Prefer the longest member when configured keys overlap. A suffix that might
    // become a longer match stays buffered until the next chunk or end of stream.
    fn scrub_prefix(&self, text: &str, at_end: bool) -> (String, usize) {
        let mut out = String::with_capacity(text.len());
        let mut offset = 0;
        while offset < text.len() {
            let rest = &text[offset..];
            if !at_end
                && self
                    .0
                    .iter()
                    .any(|key| key.len() > rest.len() && key.starts_with(rest))
            {
                break;
            }
            if let Some(key) = self.0.iter().find(|key| rest.starts_with(key.as_str())) {
                out.push_str("[redacted]");
                offset += key.len();
            } else {
                let c = rest.chars().next().expect("nonempty suffix");
                out.push(c);
                offset += c.len_utf8();
            }
        }
        (out, offset)
    }

    pub(crate) fn stream(&self) -> StreamRedactor {
        StreamRedactor {
            keys: self.clone(),
            pending: String::new(),
        }
    }
}

pub(crate) struct StreamRedactor {
    keys: CredentialSet,
    pending: String,
}

impl StreamRedactor {
    pub(crate) fn push(&mut self, input: &str) -> String {
        self.pending.push_str(input);
        let (safe, consumed) = self.keys.scrub_prefix(&self.pending, false);
        self.pending.drain(..consumed);
        safe
    }

    pub(crate) fn finish(self) -> String {
        self.keys.scrub(&self.pending)
    }
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
    fn pooled_stream_preserves_text_across_splits_and_overlapping_keys() {
        let keys = CredentialSet::new(vec!["token-α".into(), "token-αβ".into(), "other".into()]);
        let raw = "函数 token-αβ:token-α/other; tail token-";
        let expected = "函数 [redacted]:[redacted]/[redacted]; tail token-";
        for (split, _) in raw.char_indices() {
            let mut stream = keys.stream();
            let mut actual = stream.push(&raw[..split]);
            actual.push_str(&stream.push(&raw[split..]));
            actual.push_str(&stream.finish());
            assert_eq!(actual, expected);
        }
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
