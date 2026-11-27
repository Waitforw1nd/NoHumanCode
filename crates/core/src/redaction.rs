//! Pure credential redaction and persisted identifier validation without OS/DB dependencies.

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
            | "xi_api_key"
            | "x_api_key"
            | "x_goog_api_key"
            | "ocp_apim_subscription_key"
            | "x_api_app_key"
            | "x_api_access_key"
            | "secret_key"
            | "access_key"
            | "api_token"
            | "bearer_token"
            | "oauth_token"
            | "auth_token"
            | "private_key"
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
pub struct CredentialSet(std::sync::Arc<[String]>);

impl CredentialSet {
    pub fn new(mut keys: Vec<String>) -> Self {
        keys.retain(|key| !key.trim().is_empty());
        keys.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        keys.dedup();
        Self(keys.into())
    }

    pub fn scrub(&self, text: &str) -> String {
        self.scrub_prefix(text, true).0
    }

    pub fn merged_with(&self, mut extra: Vec<String>) -> Self {
        extra.extend(self.0.iter().cloned());
        Self::new(extra)
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

    pub fn stream(&self) -> StreamRedactor {
        StreamRedactor {
            keys: self.clone(),
            pending: String::new(),
        }
    }
}

pub struct StreamRedactor {
    keys: CredentialSet,
    pending: String,
}

impl StreamRedactor {
    pub fn push(&mut self, input: &str) -> String {
        self.pending.push_str(input);
        let (safe, consumed) = self.keys.scrub_prefix(&self.pending, false);
        self.pending.drain(..consumed);
        safe
    }

    pub fn finish(self) -> String {
        self.keys.scrub(&self.pending)
    }
}
