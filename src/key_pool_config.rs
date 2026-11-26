//! Host registrations bind backup credentials to a route and its full service URL.
use crate::{
    domain::valid_name,
    host_configuration::validate_credential,
    network_authorization::{NetworkError, NetworkErrorKind},
    provider_transport::{NetworkPurpose, validate_target},
};
use serde_json::Value;
use std::{collections::HashSet, env};

const ENV: &str = "CODA_KEY_POOLS";

fn invalid() -> NetworkError {
    NetworkError::not_sent(NetworkErrorKind::Invalid)
}

fn normalized(url: &str) -> Result<String, NetworkError> {
    validate_target(url, NetworkPurpose::Completion)
        .map(|target| target.base_url().to_owned())
        .map_err(|_| invalid())
}

fn parse(raw: &str) -> Result<Vec<Value>, NetworkError> {
    if raw.len() > 65536 {
        return Err(invalid());
    }
    let entries: Vec<Value> = serde_json::from_str(raw).map_err(|_| invalid())?;
    if entries.len() > 128 {
        return Err(invalid());
    }
    Ok(entries)
}

fn registrations() -> Result<Vec<Value>, NetworkError> {
    match env::var(ENV) {
        Ok(raw) => parse(&raw),
        Err(env::VarError::NotPresent) => Ok(Vec::new()),
        Err(_) => Err(invalid()),
    }
}

pub(crate) fn credential_env_names() -> Result<HashSet<String>, NetworkError> {
    Ok(referenced_names(&registrations()?))
}

// Even an unusable registration may name a real Host secret.
fn referenced_names(entries: &[Value]) -> HashSet<String> {
    let mut names = HashSet::new();
    for entry in entries {
        match &entry["keyEnvs"] {
            Value::Array(items) => names.extend(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_ascii_uppercase),
            ),
            Value::String(name) => {
                names.insert(name.to_ascii_uppercase());
            }
            _ => {}
        }
    }
    names
}

pub(crate) fn alternates(id: &str, base_url: &str) -> Result<Vec<String>, NetworkError> {
    resolve(registrations()?, id, base_url, |name| env::var(name))
}

fn resolve(
    entries: Vec<Value>,
    id: &str,
    base_url: &str,
    lookup: impl Fn(&str) -> Result<String, env::VarError>,
) -> Result<Vec<String>, NetworkError> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let target = normalized(base_url)?;
    let mut matches = entries.iter().filter(|entry| {
        entry["routeId"].as_str() == Some(id)
            && entry["baseUrl"]
                .as_str()
                .is_some_and(|url| normalized(url).is_ok_and(|url| url == target))
    });
    let Some(entry) = matches.next() else {
        return Ok(Vec::new());
    };
    if matches.next().is_some()
        || !valid_name(id)
        || id == "newapi-account"
        || entry.as_object().is_none_or(|obj| obj.len() != 3)
    {
        return Err(invalid());
    }
    let names = entry["keyEnvs"].as_array().ok_or_else(invalid)?;
    if !(1..=32).contains(&names.len()) {
        return Err(invalid());
    }
    let mut seen = HashSet::new();
    let mut values = Vec::new();
    for name in names {
        let name = name.as_str().ok_or_else(invalid)?;
        if name.is_empty()
            || name.len() > 500
            || !name
                .bytes()
                .enumerate()
                .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
            || !seen.insert(name.to_ascii_uppercase())
        {
            return Err(invalid());
        }
        match lookup(name) {
            Ok(value) => {
                validate_credential(&value).map_err(|_| invalid())?;
                values.push(value);
            }
            Err(env::VarError::NotPresent) => continue,
            Err(_) => return Err(invalid()),
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"[{"routeId":"default","baseUrl":"https://Example.test:443/v1/","keyEnvs":["MISSING","BACKUP_A"]}]"#;

    #[test]
    fn reads_only_matching_route_and_normalized_full_url() {
        for (id, url) in [
            ("other", "https://example.test/v1"),
            ("default", "http://example.test/v1"),
            ("default", "https://example.test/v2"),
            ("default", "https://example.test:8443/v1"),
        ] {
            assert!(
                resolve(parse(CONFIG).unwrap(), id, url, |_| panic!())
                    .unwrap()
                    .is_empty()
            );
        }
        let keys = resolve(
            parse(CONFIG).unwrap(),
            "default",
            "https://example.test/v1",
            |name| match name {
                "MISSING" => Err(env::VarError::NotPresent),
                "BACKUP_A" => Ok("synthetic".into()),
                _ => panic!(),
            },
        )
        .unwrap();
        assert_eq!(keys, ["synthetic"]);
    }

    #[test]
    fn ambiguous_or_invalid_registrations_and_keys_are_rejected() {
        let duplicate = format!(
            "{},{}",
            CONFIG.trim_end_matches(']'),
            CONFIG.trim_start_matches('[')
        );
        for bad in [
            duplicate,
            CONFIG.replace("BACKUP_A", "missing"),
            CONFIG.replace(r#"["MISSING","BACKUP_A"]"#, r#""INVALID""#),
        ] {
            assert!(
                resolve(
                    parse(&bad).unwrap(),
                    "default",
                    "https://example.test/v1",
                    |_| Err(env::VarError::NotPresent)
                )
                .is_err()
            );
        }
        let error = resolve(
            parse(CONFIG).unwrap(),
            "default",
            "https://example.test/v1",
            |_| Ok("invalid value".into()),
        )
        .unwrap_err();
        assert!(!error.to_string().contains("invalid value"));
    }

    #[test]
    fn malformed_other_registrations_only_contribute_child_environment_exclusions() {
        let mut entries = parse(CONFIG).unwrap();
        entries.push(serde_json::json!({"routeId":"default","baseUrl":"../invalid","keyEnvs":["OTHER_CREDENTIAL", 7]}));
        entries.push(serde_json::json!({"routeId":"other","baseUrl":"https://other.test/v1","keyEnvs":"REDACT_ME"}));
        let names = referenced_names(&entries);
        assert!(names.contains("OTHER_CREDENTIAL") && names.contains("REDACT_ME"));
        let values = resolve(entries, "default", "https://example.test/v1", |name| {
            Ok(name.to_owned())
        })
        .unwrap();
        assert_eq!(values, ["MISSING", "BACKUP_A"]);
    }
}
