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

pub fn validate_persisted_id(field: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!value.is_empty() && value.len() <= 128, "{field} 长度无效");
    anyhow::ensure!(
        !value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace()),
        "{field} 包含空白或控制字符"
    );
    anyhow::ensure!(!sensitive_text(value), "{field} 不能作为标识保存敏感内容");
    Ok(())
}

/// The HTTP contract permits 1..=200 visible ASCII bytes, independently of IDs.
pub fn validate_idempotency_key(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= 200,
        "idempotency_key 长度无效"
    );
    anyhow::ensure!(
        value.bytes().all(|byte| byte.is_ascii_graphic()),
        "idempotency_key 必须为可见 ASCII 字符"
    );
    anyhow::ensure!(
        !sensitive_text(value),
        "idempotency_key 不能作为标识保存敏感内容"
    );
    Ok(())
}

pub fn safe_metadata_text(field: &str, value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(value.len() <= 500, "{field} 长度无效");
    safe_metadata_path(field, value)
}

/// Paths share the text safety checks, but not the display-title byte limit.
/// Filesystem path validity and existence are checked by the workspace layer.
pub fn safe_metadata_path(field: &str, value: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!value.trim().is_empty(), "{field} 不能为空");
    anyhow::ensure!(
        !value.chars().any(|ch| ch.is_control()),
        "{field} 包含控制字符"
    );
    anyhow::ensure!(!sensitive_text(value), "{field} 包含敏感内容，拒绝写入");
    Ok(value.to_owned())
}

fn sensitive_text(value: &str) -> bool {
    redact_text(value) != value
}

/// Defense in depth for known credential fields and recognizable token text.
/// This cannot identify an arbitrary secret embedded in unstructured prose.
/// Callers must persist this returned value and keep actual credentials in DPAPI.
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

fn normalize_field(name: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (index, &ch) in chars.iter().enumerate() {
        if ch == '-' || ch == '.' || ch == ' ' || ch == '_' {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            continue;
        }
        // APIKey -> api_key; accessToken -> access_token; API_KEY -> api_key.
        let previous = index.checked_sub(1).map(|index| chars[index]);
        let next_is_lowercase = chars.get(index + 1).is_some_and(char::is_ascii_lowercase);
        let word_boundary = previous.is_some_and(|previous| {
            previous.is_ascii_lowercase()
                || previous.is_ascii_digit()
                || (previous.is_ascii_uppercase() && next_is_lowercase)
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

fn sensitive_field(name: &str) -> bool {
    let name = normalize_field(name);
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
            | "authorization_header"
            | "token_value"
    ) || name.ends_with("_token")
        || name.ends_with("_secret")
        || name.ends_with("_password")
        || name.ends_with("_credential")
        || name.ends_with("_authorization")
        || name.ends_with("_key")
}

fn sensitive_assignment_field(name: &str) -> bool {
    let name = normalize_field(name);
    if name == "key" {
        return false;
    }
    // In code/log text, generic lookup/primary keys need not be credentials.
    // Structured credential-like fields keep the more conservative policy above.
    if name.ends_with("_key") {
        return matches!(
            name.as_str(),
            "api_key"
                | "private_key"
                | "secret_key"
                | "access_key"
                | "provider_key"
                | "client_key"
                | "encryption_key"
        ) || name.ends_with("_api_key");
    }
    sensitive_field(&name)
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
        if let Some((value_start, end)) = assignment_range(input, index) {
            out.push_str(&input[index..value_start]);
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

fn assignment_range(input: &str, index: usize) -> Option<(usize, usize)> {
    if index > 0 {
        let previous = input[..index].chars().next_back()?;
        if previous.is_ascii_alphanumeric() || matches!(previous, '_' | '-' | '.') {
            return None;
        }
    }
    let bytes = input.as_bytes();
    let mut end = index;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'-' | b'.'))
    {
        end += 1;
    }
    if end == index || !sensitive_assignment_field(&input[index..end]) {
        return None;
    }
    let field = normalize_field(&input[index..end]);
    let authorization = field == "authorization"
        || field == "authorization_header"
        || field.ends_with("_authorization");
    // Quoted JSON property names are also common inside log/event strings.
    if end < bytes.len() && matches!(bytes[end], b'\'' | b'"') {
        end += 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    if end >= bytes.len() || !matches!(bytes[end], b'=' | b':') {
        return None;
    }
    end += 1;
    while end < bytes.len() && bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    let quote = bytes
        .get(end)
        .copied()
        .filter(|byte| matches!(byte, b'\'' | b'"'));
    if quote.is_some() {
        end += 1;
    }
    let value_start = end;
    if input[value_start..].starts_with("[redacted]") {
        return None;
    }
    while end < bytes.len() {
        if let Some(quote) = quote {
            if bytes[end] == quote {
                break;
            }
            if bytes[end] == b'\\' && end + 1 < bytes.len() {
                end += 2;
                continue;
            }
        } else if bytes[end].is_ascii_whitespace()
            || matches!(bytes[end], b',' | b';' | b'}' | b']' | b'\'' | b'"')
        {
            break;
        }
        end += 1;
    }
    // Unquoted HTTP Authorization values contain a scheme and a credential.
    // Redacting only the first word would leave the actual credential behind.
    if quote.is_none()
        && authorization
        && ["bearer", "basic", "token", "negotiate", "ntlm"]
            .iter()
            .any(|scheme| input[value_start..end].eq_ignore_ascii_case(scheme))
    {
        let mut credential_end = end;
        while credential_end < bytes.len() && matches!(bytes[credential_end], b' ' | b'\t') {
            credential_end += 1;
        }
        let credential_start = credential_end;
        while credential_end < bytes.len()
            && !bytes[credential_end].is_ascii_whitespace()
            && !matches!(
                bytes[credential_end],
                b',' | b';' | b'}' | b']' | b'\'' | b'"'
            )
        {
            credential_end += 1;
        }
        if credential_end > credential_start {
            end = credential_end;
        }
    }
    (end > value_start).then_some((value_start, end))
}

const TOKEN_PREFIXES: &[&str] = &[
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

fn token_end(input: &str, index: usize) -> Option<usize> {
    if index > 0 {
        let previous = input[..index].chars().next_back()?;
        if previous.is_ascii_alphanumeric() || previous == '_' || previous == '-' {
            return None;
        }
    }
    let rest = &input[index..];
    let prefix = TOKEN_PREFIXES
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

/// Incremental Host output policy, independent of the selected completion codec.
/// Only an undecided prefix (at most 11 bytes) and the last 128 ASCII field-name
/// bytes are retained. Secret values are discarded, never buffered. Dropping on
/// error/cancellation deliberately discards the undecided prefix; only a clean
/// completion may call `finish`. This is the recognizable-text policy, not a
/// detector for arbitrary unknown secrets.
#[derive(Default)]
pub(crate) struct StreamRedactor {
    pending: String,
    field: String,
    previous: Option<char>,
    mode: StreamMode,
}

#[derive(Default)]
enum StreamMode {
    #[default]
    Text,
    AssignmentSeparator {
        authorization: bool,
        quote: bool,
    },
    ValueStart {
        authorization: bool,
    },
    Token,
    BearerStart,
    Bearer,
    Quoted {
        quote: char,
        escaped: bool,
    },
    Unquoted {
        scheme: String,
        authorization: bool,
    },
    AuthorizationGap,
}

impl StreamRedactor {
    pub(crate) fn push(&mut self, input: &str) -> String {
        let mut output = String::new();
        for ch in input.chars() {
            self.pending.push(ch);
            self.drain(false, &mut output);
        }
        output
    }

    pub(crate) fn finish(mut self) -> String {
        let mut output = String::new();
        self.drain(true, &mut output);
        output
    }

    fn consume(&mut self, bytes: usize) {
        self.previous = self.pending[..bytes].chars().next_back();
        self.pending.drain(..bytes);
    }

    fn drain(&mut self, eof: bool, output: &mut String) {
        while let Some(ch) = self.pending.chars().next() {
            let mode = std::mem::take(&mut self.mode);
            match mode {
                StreamMode::Text => {
                    let token_boundary = self
                        .previous
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && !matches!(c, '_' | '-'));
                    let bearer_boundary = self.previous.is_none_or(|c| !c.is_ascii_alphanumeric());
                    if token_boundary {
                        if let Some(prefix) = TOKEN_PREFIXES.iter().find(|p| {
                            self.pending.starts_with(**p)
                                && self.pending[p.len()..].starts_with(token_char)
                        }) {
                            let len = prefix.len();
                            output.push_str("[redacted]");
                            self.field.clear();
                            self.mode = StreamMode::Token;
                            self.consume(len);
                            continue;
                        }
                        if !eof && TOKEN_PREFIXES.iter().any(|p| p.starts_with(&self.pending)) {
                            break;
                        }
                    }
                    if bearer_boundary {
                        let lower = self.pending.to_ascii_lowercase();
                        if lower.starts_with("bearer ") {
                            output.push_str("bearer ");
                            self.field.clear();
                            self.mode = StreamMode::BearerStart;
                            self.consume(7);
                            continue;
                        }
                        if !eof && "bearer ".starts_with(&lower) {
                            break;
                        }
                    }
                    if field_char(ch) {
                        self.field.push(ch);
                        if self.field.len() > 128 {
                            self.field.remove(0);
                        }
                    } else {
                        if sensitive_assignment_field(&self.field) {
                            let field = normalize_field(&self.field);
                            let authorization = field == "authorization"
                                || field == "authorization_header"
                                || field.ends_with("_authorization");
                            if matches!(ch, '=' | ':') {
                                self.mode = StreamMode::ValueStart { authorization };
                            } else if ch.is_ascii_whitespace() || matches!(ch, '\'' | '"') {
                                self.mode = StreamMode::AssignmentSeparator {
                                    authorization,
                                    quote: ch.is_ascii_whitespace(),
                                };
                            }
                        }
                        self.field.clear();
                    }
                    output.push(ch);
                    self.consume(ch.len_utf8());
                }
                StreamMode::AssignmentSeparator {
                    authorization,
                    quote,
                } => {
                    if ch.is_ascii_whitespace() || (quote && matches!(ch, '\'' | '"')) {
                        self.mode = StreamMode::AssignmentSeparator {
                            authorization,
                            quote: quote && ch.is_ascii_whitespace(),
                        };
                    } else if matches!(ch, '=' | ':') {
                        self.mode = StreamMode::ValueStart { authorization };
                    } else {
                        continue; // Reprocess in Text; no input has been consumed.
                    }
                    output.push(ch);
                    self.consume(ch.len_utf8());
                }
                StreamMode::ValueStart { authorization } => {
                    if ch.is_ascii_whitespace() {
                        output.push(ch);
                        self.consume(ch.len_utf8());
                        self.mode = StreamMode::ValueStart { authorization };
                    } else if matches!(ch, '\'' | '"') {
                        output.push(ch);
                        output.push_str("[redacted]");
                        self.consume(ch.len_utf8());
                        self.mode = StreamMode::Quoted {
                            quote: ch,
                            escaped: false,
                        };
                    } else if self.pending.starts_with("[redacted]") {
                        output.push_str("[redacted]");
                        self.consume(10);
                    } else if !eof && "[redacted]".starts_with(&self.pending) {
                        self.mode = StreamMode::ValueStart { authorization };
                        break;
                    } else if value_end(ch) {
                        continue;
                    } else {
                        output.push_str("[redacted]");
                        self.mode = StreamMode::Unquoted {
                            scheme: String::new(),
                            authorization,
                        };
                    }
                }
                StreamMode::Token => {
                    if token_char(ch) {
                        self.consume(ch.len_utf8());
                        self.mode = StreamMode::Token;
                    }
                }
                StreamMode::BearerStart => {
                    if ch.is_ascii_whitespace() {
                        self.consume(ch.len_utf8());
                        self.mode = StreamMode::BearerStart;
                    } else {
                        output.push_str("[redacted]");
                        self.mode = StreamMode::Bearer;
                    }
                }
                StreamMode::Bearer => {
                    if !ch.is_ascii_whitespace() {
                        self.consume(ch.len_utf8());
                        self.mode = StreamMode::Bearer;
                    }
                }
                StreamMode::Quoted { quote, escaped } => {
                    self.consume(ch.len_utf8());
                    if ch == quote && !escaped {
                        output.push(ch);
                    } else {
                        self.mode = StreamMode::Quoted {
                            quote,
                            escaped: ch == '\\' && !escaped,
                        };
                    }
                }
                StreamMode::Unquoted {
                    mut scheme,
                    authorization,
                } => {
                    if value_end(ch) {
                        if authorization
                            && matches!(ch, ' ' | '\t')
                            && ["bearer", "basic", "token", "negotiate", "ntlm"]
                                .iter()
                                .any(|s| scheme.eq_ignore_ascii_case(s))
                        {
                            self.mode = StreamMode::AuthorizationGap;
                            self.consume(ch.len_utf8());
                        }
                    } else {
                        if authorization && scheme.len() < 10 {
                            scheme.push(ch);
                        }
                        self.consume(ch.len_utf8());
                        self.mode = StreamMode::Unquoted {
                            scheme,
                            authorization,
                        };
                    }
                }
                StreamMode::AuthorizationGap => {
                    if matches!(ch, ' ' | '\t') {
                        self.consume(ch.len_utf8());
                        self.mode = StreamMode::AuthorizationGap;
                    } else if !value_end(ch) {
                        self.mode = StreamMode::Unquoted {
                            scheme: String::new(),
                            authorization: false,
                        };
                    }
                }
            }
        }
    }
}

fn token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}
fn field_char(ch: char) -> bool {
    token_char(ch) || ch == '.'
}
fn value_end(ch: char) -> bool {
    ch.is_ascii_whitespace() || matches!(ch, ',' | ';' | '}' | ']' | '\'' | '"')
}

#[cfg(test)]
mod stream_redaction_tests {
    use super::*;

    fn filtered(parts: &[&str]) -> String {
        let mut redactor = StreamRedactor::default();
        let mut output = String::new();
        for part in parts {
            output.push_str(&redactor.push(part));
            assert!(redactor.pending.len() <= 11);
            assert!(redactor.field.len() <= 128);
        }
        output.push_str(&redactor.finish());
        output
    }

    #[test]
    fn stream_redaction_all_splits_match_complete_policy() {
        let mut fixtures: Vec<String> = TOKEN_PREFIXES
            .iter()
            .map(|prefix| format!("前文 {prefix}synthetic123 后文🦀"))
            .collect();
        fixtures.extend(
            [
                "before Bearer synthetic123 after",
                "api_key=synthetic123; next",
                "accessToken : 'synthetic123' next",
                "{\"password\":\"synthetic\\\"123\"}",
                "service_client_secret = \"synthetic\n123\" done",
                "Authorization: Basic synthetic123\nnext",
                "authorization_header: Negotiate synthetic123\nnext",
                "Authorization: \"Bearer synthetic123\" next",
                "foo.sk-synthetic123 end",
                "password=[redacted] next",
                "password=\"[redacted]\" next",
            ]
            .map(str::to_owned),
        );
        for input in fixtures {
            let expected = redact_text(&input);
            for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
                assert_eq!(
                    filtered(&[&input[..split], &input[split..]]),
                    expected,
                    "split={split}, input={input:?}"
                );
            }
            let chars: Vec<_> = input
                .char_indices()
                .map(|(i, c)| &input[i..i + c.len_utf8()])
                .collect();
            assert_eq!(filtered(&chars), expected);
        }
        // This negative control is precisely the pre-fix event-by-event policy.
        assert_eq!(
            format!("{}{}", redact_text("s"), redact_text("k-synthetic123")),
            "sk-synthetic123"
        );
    }

    #[test]
    fn stream_redaction_long_plain_text_is_streamed_and_state_is_bounded() {
        let mut redactor = StreamRedactor::default();
        let plain = "z".repeat(4096);
        for _ in 0..100 {
            assert_eq!(redactor.push(&plain), plain);
            assert!(redactor.pending.len() <= 11);
            assert!(redactor.field.len() <= 128);
        }
        assert_eq!(
            redactor.push("_password=synthetic123;"),
            "_password=[redacted];"
        );
        assert_eq!(redactor.push("中文🦀"), "中文🦀");
        assert_eq!(redactor.finish(), "");

        let mut secret = StreamRedactor::default();
        assert_eq!(secret.push("sk-synthetic"), "[redacted]");
        for _ in 0..100 {
            assert_eq!(secret.push(&plain), "");
            assert!(secret.pending.len() <= 11);
            assert!(secret.field.is_empty());
        }
        assert_eq!(secret.push(" done"), " done");
    }

    #[test]
    fn stream_redaction_only_clean_finish_releases_undecided_text() {
        let mut redactor = StreamRedactor::default();
        assert_eq!(redactor.push("visible s"), "visible ");
        assert_eq!(redactor.finish(), "s");
        for partial in [
            "s",
            "sk-",
            "Bearer ",
            "password=\"synthetic",
            "Authorization: Basic synthetic",
        ] {
            let mut redactor = StreamRedactor::default();
            let safe = redactor.push(partial);
            assert!(!safe.contains("synthetic"));
            // Transport failure / cancellation drops state without calling a sink.
            drop(redactor);
        }
    }
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
        let camel = serde_json::json!({
            "accessToken": "camel-token",
            "modelToken": "model-token",
            "providerKey": "provider-key",
            "clientSecret": "client-secret",
            "authorizationHeader": "plain-secret",
            "APIKey": "apikey-unique-value",
            "tokenValue": "tokenvalue-unique-value",
            "databasePassword": "password-unique-value",
            "items": [{"nestedKey": "nested-key"}]
        });
        let redacted = redact_persisted(&camel).to_string();
        for secret in [
            "camel-token",
            "model-token",
            "provider-key",
            "client-secret",
            "nested-key",
            "plain-secret",
            "apikey-unique-value",
            "tokenvalue-unique-value",
            "password-unique-value",
        ] {
            assert!(!redacted.contains(secret), "{secret} leaked");
        }
    }

    #[test]
    fn credential_fields_redact_independently_without_token_shaped_values() {
        for field in [
            "APIKey",
            "API_KEY",
            "apiKey",
            "api.key",
            "api-key",
            "authorizationHeader",
            "databasePassword",
            "database_password",
            "clientSecret",
            "tokenValue",
            "accessToken",
            "refreshToken",
            "providerKey",
        ] {
            let input = serde_json::json!({field: "plain unique credential"});
            assert_eq!(
                redact_persisted(&input)[field],
                "[redacted]",
                "credential field {field} was not recognized"
            );
        }
        assert_eq!(normalize_field("APIKey"), "api_key");
        assert_eq!(normalize_field("HTTPAuthorization"), "http_authorization");
    }

    #[test]
    fn ordinary_metadata_and_code_are_preserved() {
        let code = "let max_tokens = 4096;\nlet draft_id = \"draft-1\";\nlet lookup_key = \"cache\";\nlet key = 3;\nfn token_count() -> usize { 3 }";
        let input = serde_json::json!({
            "max_tokens": 4096,
            "draft_id": "draft-1",
            "text": code,
            "title": "项目第一轮审查",
            "path": "D:\\项目\\普通 文件夹\\src\\main.rs"
        });
        assert_eq!(redact_persisted(&input), input);
        for text in [
            "项目第一轮审查",
            "draft_id=draft-1; max_tokens=4096",
            "tokenization and password reset instructions",
            "D:\\项目\\普通 文件夹\\src\\main.rs",
        ] {
            assert_eq!(safe_metadata_text("title", text).unwrap(), text);
        }
        // Unstructured arbitrary values have no reliable secret signature.
        assert_eq!(
            redact_text("opaque ordinary value"),
            "opaque ordinary value"
        );
    }

    #[test]
    fn credential_text_is_checked_throughout_metadata() {
        for text in [
            "标题 bearer plainvalue",
            "标题 APIKey = plainvalue",
            "标题 databasePassword: 'plain value with spaces'",
            "prefix {\"authorizationHeader\": \"plain header value\"}",
            "prefix xai-secretvalue suffix",
        ] {
            assert!(safe_metadata_text("title", text).is_err(), "{text}");
            assert!(safe_metadata_path("path", text).is_err(), "{text}");
            let redacted = redact_text(text);
            assert_eq!(redact_text(&redacted), redacted, "redaction must be stable");
        }
        assert_eq!(
            redact_text("databasePassword = \"plain value\"; max_tokens = 4"),
            "databasePassword = \"[redacted]\"; max_tokens = 4"
        );
    }

    #[test]
    fn authorization_assignments_redact_scheme_and_credential() {
        for (input, expected) in [
            (
                "Authorization: Bearer historical-bearer-token",
                "Authorization: [redacted]",
            ),
            (
                "Authorization: Basic aGVsbG86d29ybGQ=",
                "Authorization: [redacted]",
            ),
            (
                "authorizationHeader=basic\taGVsbG86d29ybGQ=, next=value",
                "authorizationHeader=[redacted], next=value",
            ),
            (
                "Authorization: Bearer historical-bearer-token ordinary trailing words",
                "Authorization: [redacted] ordinary trailing words",
            ),
            (
                "Authorization: Bearer\nordinary next line",
                "Authorization: [redacted]\nordinary next line",
            ),
            (
                "Authorization: \"Bearer historical-bearer-token\"",
                "Authorization: \"[redacted]\"",
            ),
        ] {
            assert_eq!(redact_text(input), expected);
            assert_eq!(redact_text(expected), expected);
            assert_eq!(
                redact_persisted(&serde_json::json!({"text": input}))["text"],
                expected
            );
        }
        assert_eq!(
            redact_text("Basic knowledge of HTTP"),
            "Basic knowledge of HTTP"
        );
    }

    #[test]
    fn identifier_key_and_path_limits_match_their_contracts() {
        for id in [
            "prj-x",
            "tsk-stable",
            "01994ede-f12a-7640-b9ca-bfdc661c0091",
        ] {
            validate_persisted_id("id", id).unwrap();
        }
        validate_persisted_id("id", &"a".repeat(128)).unwrap();
        for id in ["", " ", "a b", "a\tb", "a\nb", "a\u{2003}b", "\0id"] {
            assert!(validate_persisted_id("id", id).is_err(), "{id:?}");
        }
        assert!(validate_persisted_id("id", &"a".repeat(129)).is_err());
        assert!(validate_persisted_id("id", "xai-secretvalue").is_err());
        validate_idempotency_key(&"k".repeat(200)).unwrap();
        validate_idempotency_key("operation-1:retry/2+ok").unwrap();
        for key in ["", "a b", "中文", "a\tb", "a\nb", "\u{7f}", "api_key=plain"] {
            assert!(validate_idempotency_key(key).is_err(), "{key:?}");
        }
        assert!(validate_idempotency_key(&"k".repeat(201)).is_err());
        let long_path = format!("D:\\{}项目", "ordinary-folder\\".repeat(40));
        assert!(long_path.len() > 500);
        assert_eq!(
            safe_metadata_path("root_path", &long_path).unwrap(),
            long_path
        );
        assert!(safe_metadata_text("title", &long_path).is_err());
        for value in ["", "   ", "title\nnext", "title\0"] {
            assert!(safe_metadata_text("title", value).is_err());
            assert!(safe_metadata_path("root_path", value).is_err());
        }
    }
}
