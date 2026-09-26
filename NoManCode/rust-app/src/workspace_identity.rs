//! Rechecked directory identity, not a handle-based filesystem sandbox.
//! A VerifiedRoot is an observation, not a lease: check again before effects.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootIdentity {
    version: u32,
    canonical_path: PathBuf,
    directory_id: DirectoryId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", deny_unknown_fields)]
enum DirectoryId {
    Windows {
        volume_serial: u64,
        file_id: [u8; 16],
    },
    UnixFixture {
        device: u64,
        inode: u64,
    },
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
    chain: Vec<DirectoryId>,
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
        let observation = observe_stable(path)?;
        Ok(Self {
            version: 1,
            canonical_path: observation.canonical_path,
            directory_id: observation.chain.last().unwrap().clone(),
        })
    }

    pub fn verify(&self) -> Result<VerifiedRoot> {
        if self.version != 1 {
            return Err(RootIdentityError::InvalidSnapshot);
        }
        match &self.directory_id {
            DirectoryId::Windows { .. } if cfg!(windows) => {}
            DirectoryId::UnixFixture { .. } if cfg!(unix) => {}
            _ => return Err(RootIdentityError::InvalidSnapshot),
        }
        validate_path(&self.canonical_path).map_err(|_| RootIdentityError::InvalidSnapshot)?;
        let observed = observe_stable(&self.canonical_path)?;
        if observed.canonical_path != self.canonical_path {
            return Err(RootIdentityError::InvalidSnapshot);
        }
        if observed.chain.last() != Some(&self.directory_id) {
            return Err(RootIdentityError::IdentityChanged);
        }
        Ok(observed)
    }

    pub fn overlap(&self, other: &Self) -> Result<RootOverlap> {
        let left = self.verify()?;
        let right = other.verify()?;
        // A second observation rejects ordinary replacement during this operation.
        let left_after = self.verify()?;
        let right_after = other.verify()?;
        if left.chain != left_after.chain || right.chain != right_after.chain {
            return Err(RootIdentityError::IdentityChanged);
        }
        if self.directory_id == other.directory_id {
            Ok(RootOverlap::Same)
        } else if right.chain.contains(&self.directory_id) {
            Ok(RootOverlap::LeftContainsRight)
        } else if left.chain.contains(&other.directory_id) {
            Ok(RootOverlap::RightContainsLeft)
        } else {
            Ok(RootOverlap::Disjoint)
        }
    }
}

/// Checks existing directory ancestors, including the directory itself.
/// This grants no Task permission and must not replace leaf/escape checks.
pub fn verify_directory_policy(path: &Path) -> Result<()> {
    observe_stable(path).map(|_| ())
}

fn validate_path(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(RootIdentityError::InvalidPath);
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(RootIdentityError::InvalidPath);
    }
    #[cfg(windows)]
    {
        use std::path::Prefix;
        if !matches!(path.components().next(), Some(Component::Prefix(p))
            if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        {
            return Err(RootIdentityError::InvalidPath);
        }
        // Reject alternate streams and Win32-normalized trailing-dot/space aliases.
        if path.components().any(|c| {
            matches!(c, Component::Normal(s)
            if s.to_str().is_none_or(|s| s.contains(':') || s.ends_with('.') || s.ends_with(' ')))
        }) {
            return Err(RootIdentityError::InvalidPath);
        }
    }
    Ok(())
}

fn chain(path: &Path) -> Result<Vec<DirectoryId>> {
    validate_path(path)?;
    let mut paths: Vec<_> = path.ancestors().collect();
    paths.reverse();
    paths.into_iter().map(directory_identity).collect()
}

fn observe_stable(path: &Path) -> Result<VerifiedRoot> {
    let before_input = chain(path)?;
    let canonical_path = path
        .canonicalize()
        .map_err(RootIdentityError::Unavailable)?;
    let before_canonical = chain(&canonical_path)?;
    let after_input = chain(path)?;
    let after_canonical = chain(&canonical_path)?;
    let canonical_after = path
        .canonicalize()
        .map_err(RootIdentityError::Unavailable)?;
    if before_input != after_input
        || before_canonical != after_canonical
        || before_input.last() != before_canonical.last()
        || canonical_path != canonical_after
    {
        return Err(RootIdentityError::IdentityChanged);
    }
    Ok(VerifiedRoot {
        canonical_path,
        chain: after_canonical,
    })
}

#[cfg(windows)]
fn directory_identity(path: &Path) -> Result<DirectoryId> {
    use std::fs::OpenOptions;
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::*;
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(RootIdentityError::Unavailable)?;
    fn query<T>(file: &std::fs::File, class: FILE_INFO_BY_HANDLE_CLASS) -> Result<T> {
        let mut value = std::mem::MaybeUninit::<T>::zeroed();
        // SAFETY: caller uses the documented structure for each information class;
        // the file owns a valid handle and the output buffer has its exact size.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                class,
                value.as_mut_ptr().cast(),
                std::mem::size_of::<T>() as u32,
            )
        };
        if ok == 0 {
            return Err(RootIdentityError::UnsupportedIdentity);
        }
        // SAFETY: successful query initialized the fixed-size information structure.
        Ok(unsafe { value.assume_init() })
    }
    let attrs: FILE_ATTRIBUTE_TAG_INFO = query(&file, FileAttributeTagInfo)?;
    if attrs.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(RootIdentityError::ReparsePoint);
    }
    if attrs.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(RootIdentityError::InvalidPath);
    }
    // FILE_CASE_SENSITIVE_INFO is a DWORD Flags structure; windows-sys 0.59
    // exposes its information class but not this structure in Win32.
    #[repr(C)]
    struct CaseSensitiveInfo {
        flags: u32,
    }
    let case: CaseSensitiveInfo = query(&file, FileCaseSensitiveInfo)?;
    if case.flags != 0 {
        return Err(RootIdentityError::CaseSensitiveDirectory);
    }
    let id: FILE_ID_INFO = query(&file, FileIdInfo)?;
    Ok(DirectoryId::Windows {
        volume_serial: id.VolumeSerialNumber,
        file_id: id.FileId.Identifier,
    })
}

#[cfg(unix)]
fn directory_identity(path: &Path) -> Result<DirectoryId> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).map_err(RootIdentityError::Unavailable)?;
    if metadata.file_type().is_symlink() {
        return Err(RootIdentityError::ReparsePoint);
    }
    if !metadata.is_dir() {
        return Err(RootIdentityError::InvalidPath);
    }
    Ok(DirectoryId::UnixFixture {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(any(windows, unix)))]
fn directory_identity(_: &Path) -> Result<DirectoryId> {
    Err(RootIdentityError::UnsupportedPlatform)
}
