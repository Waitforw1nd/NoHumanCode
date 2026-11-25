//! Canonical directory identity verification supporting NTFS, ReFS (Dev Drive), junctions, and WSL dirs.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootIdentity {
    version: u32,
    canonical_path: PathBuf,
    identity_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootOverlap {
    Disjoint,
    Same,
    LeftContainsRight,
    RightContainsLeft,
}

#[derive(Debug)]
pub struct VerifiedRoot {
    canonical_path: PathBuf,
}

impl VerifiedRoot {
    pub fn path(&self) -> &Path {
        &self.canonical_path
    }
}

#[derive(Debug)]
pub enum RootIdentityError {
    InvalidPath,
    UnsupportedPlatform,
    UnsupportedIdentity,
    ReparsePoint,
    CaseSensitiveDirectory,
    Unavailable(std::io::Error),
    IdentityChanged,
    InvalidSnapshot,
}

impl std::fmt::Display for RootIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidPath => "invalid workspace root path",
            Self::UnsupportedPlatform => "unsupported workspace platform",
            Self::UnsupportedIdentity => "unsupported directory identity or policy query",
            Self::ReparsePoint => "workspace path contains a reparse point or symlink",
            Self::CaseSensitiveDirectory => "case-sensitive workspace directories are unsupported",
            Self::Unavailable(_) => "workspace root is unavailable",
            Self::IdentityChanged => "workspace root identity changed",
            Self::InvalidSnapshot => "invalid workspace root identity snapshot",
        })
    }
}

impl std::error::Error for RootIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unavailable(error) => Some(error),
            _ => None,
        }
    }
}

type Result<T> = std::result::Result<T, RootIdentityError>;

impl RootIdentity {
    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub fn capture(path: &Path) -> Result<Self> {
        validate_input_path(path)?;
        let canonical_path = resolve_directory(path)?;
        let identity_key = canonical_identity_key(&canonical_path)?;
        Ok(Self {
            version: 1,
            canonical_path,
            identity_key,
        })
    }

    pub fn validate_snapshot(&self) -> Result<()> {
        if self.version != 1 || self.identity_key.is_empty() {
            return Err(RootIdentityError::InvalidSnapshot);
        }
        validate_input_path(&self.canonical_path)
            .map_err(|_| RootIdentityError::InvalidSnapshot)?;
        if canonical_identity_key(&self.canonical_path)
            .map_err(|_| RootIdentityError::InvalidSnapshot)?
            != self.identity_key
        {
            return Err(RootIdentityError::InvalidSnapshot);
        }
        Ok(())
    }

    pub fn verify(&self) -> Result<VerifiedRoot> {
        self.validate_snapshot()?;
        let observed = resolve_directory(&self.canonical_path)?;
        let observed_key = canonical_identity_key(&observed)?;
        if observed_key != self.identity_key || (cfg!(unix) && observed != self.canonical_path) {
            return Err(RootIdentityError::IdentityChanged);
        }
        Ok(VerifiedRoot {
            canonical_path: observed,
        })
    }

    pub fn overlap(&self, other: &Self) -> Result<RootOverlap> {
        self.verify()?;
        other.verify()?;
        let (left, right) = if cfg!(windows) {
            (self.identity_key.as_str(), other.identity_key.as_str())
        } else {
            (
                self.canonical_path
                    .to_str()
                    .ok_or(RootIdentityError::InvalidPath)?,
                other
                    .canonical_path
                    .to_str()
                    .ok_or(RootIdentityError::InvalidPath)?,
            )
        };
        if left == right {
            Ok(RootOverlap::Same)
        } else if is_ancestor_key(left, right) {
            Ok(RootOverlap::LeftContainsRight)
        } else if is_ancestor_key(right, left) {
            Ok(RootOverlap::RightContainsLeft)
        } else {
            Ok(RootOverlap::Disjoint)
        }
    }
}

pub fn verify_directory_policy(path: &Path) -> Result<()> {
    validate_input_path(path)?;
    resolve_directory(path).map(|_| ())
}

fn validate_input_path(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(RootIdentityError::InvalidPath);
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(RootIdentityError::InvalidPath);
    }
    Ok(())
}

fn resolve_directory(path: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(path).map_err(RootIdentityError::Unavailable)?;
    let meta = std::fs::metadata(&canonical).map_err(RootIdentityError::Unavailable)?;
    if !meta.is_dir() {
        return Err(RootIdentityError::InvalidPath);
    }
    Ok(canonical)
}

fn canonical_identity_key(canonical: &Path) -> Result<String> {
    let raw = canonical.to_str().ok_or(RootIdentityError::InvalidPath)?;
    // Keep the v1 persisted key readable even when a Unix name contains a literal backslash.
    Ok(snapshot_key(raw))
}

pub(crate) fn path_comparison_key(raw: &str) -> String {
    if cfg!(windows) {
        snapshot_key(raw)
    } else {
        raw.to_owned()
    }
}

fn snapshot_key(raw: &str) -> String {
    let normalized = raw.replace('\\', "/");
    let stripped = if let Some(unc) = normalized.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else if let Some(rest) = normalized.strip_prefix("//?/") {
        rest.to_owned()
    } else {
        normalized
    };
    let trimmed = if stripped == "/" {
        stripped
    } else {
        stripped.trim_end_matches('/').to_owned()
    };
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed
    }
}

fn is_ancestor_key(ancestor: &str, descendant: &str) -> bool {
    if ancestor == "/" {
        return descendant.starts_with('/') && descendant.len() > 1;
    }
    descendant
        .strip_prefix(ancestor)
        .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_verifies_and_detects_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let child = parent.join("child");
        let sibling = temp.path().join("sibling");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let p_id = RootIdentity::capture(&parent).unwrap();
        let c_id = RootIdentity::capture(&child).unwrap();
        let s_id = RootIdentity::capture(&sibling).unwrap();

        assert!(p_id.verify().is_ok());
        assert_eq!(p_id.overlap(&p_id).unwrap(), RootOverlap::Same);
        assert_eq!(p_id.overlap(&c_id).unwrap(), RootOverlap::LeftContainsRight);
        assert_eq!(c_id.overlap(&p_id).unwrap(), RootOverlap::RightContainsLeft);
        assert_eq!(p_id.overlap(&s_id).unwrap(), RootOverlap::Disjoint);
    }

    #[cfg(unix)]
    #[test]
    fn backslash_in_root_name_is_not_a_directory_separator() {
        let temp = tempfile::tempdir().unwrap();
        let literal = temp.path().join(r"parent\child");
        let nested = temp.path().join("parent/child");
        std::fs::create_dir(&literal).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        let left = RootIdentity::capture(&literal).unwrap();
        let right = RootIdentity::capture(&nested).unwrap();
        assert_eq!(left.overlap(&right).unwrap(), RootOverlap::Disjoint);
    }

    #[test]
    fn rejects_relative_or_file_paths() {
        assert!(matches!(
            RootIdentity::capture(Path::new("relative/dir")),
            Err(RootIdentityError::InvalidPath)
        ));
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file.txt");
        std::fs::write(&file, "hi").unwrap();
        assert!(matches!(
            RootIdentity::capture(&file),
            Err(RootIdentityError::InvalidPath)
        ));
    }
}
