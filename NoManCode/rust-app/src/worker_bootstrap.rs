//! Trusted worker selection from the host executable's own package.
//!
//! The digest must come from the host build, never from a request or runtime
//! environment. The returned service retains a pin on the verified image.
use crate::wasm_worker_process::{TrustedWorker, WorkerError, WorkerService};

fn parse_digest(value: &str) -> Result<[u8; 32], WorkerError> {
    let bytes = value.as_bytes();
    if bytes.len() != 64
        || !bytes
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
    {
        return Err(WorkerError::IsolationUnavailable(
            "packaged_digest_format",
            None,
        ));
    }
    let mut digest = [0u8; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        let nibble = |c: u8| if c <= b'9' { c - b'0' } else { c - b'a' + 10 };
        digest[i] = (nibble(pair[0]) << 4) | nibble(pair[1]);
    }
    Ok(digest)
}

/// Resolve only the fixed sibling of the running host. `None` disables WASM;
/// a configured but invalid or unavailable package fails closed.
pub fn bootstrap_from_packaged_digest(
    digest: Option<&str>,
) -> Result<Option<WorkerService>, WorkerError> {
    let Some(digest) = digest else {
        return Ok(None);
    };
    let digest = parse_digest(digest)?;
    let host = std::env::current_exe()
        .map_err(|_| WorkerError::IsolationUnavailable("host_executable", None))?;
    let parent = host
        .parent()
        .ok_or(WorkerError::IsolationUnavailable("host_parent", None))?;
    let path = parent.join("peachsh-wasm-worker.exe");
    let worker = TrustedWorker::pin_host_install(path, digest)?;
    Ok(Some(WorkerService::from_trusted(worker)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn digest_is_exact_lowercase_hex() {
        assert_eq!(parse_digest(&"00".repeat(32)).unwrap(), [0; 32]);
        assert!(parse_digest(&"AA".repeat(32)).is_err());
        assert!(parse_digest(&"0".repeat(63)).is_err());
        assert!(parse_digest(&"gg".repeat(32)).is_err());
        assert!(bootstrap_from_packaged_digest(None).unwrap().is_none());
    }
}
