#[path = "../src/workspace_identity.rs"]
#[allow(dead_code)]
mod workspace_identity;

use std::fs;
use workspace_identity::{RootIdentity, RootIdentityError, RootOverlap, verify_directory_policy};

#[test]
fn identity_roundtrip_non_ascii_and_overlap() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("项目 Ä");
    let child = parent.join("child");
    let sibling = temp.path().join("sibling");
    fs::create_dir_all(&child).unwrap();
    fs::create_dir(&sibling).unwrap();
    let a = RootIdentity::capture(&parent).unwrap();
    let b = RootIdentity::capture(&child).unwrap();
    let c = RootIdentity::capture(&sibling).unwrap();
    let json = serde_json::to_vec(&a).unwrap();
    let restored: RootIdentity = serde_json::from_slice(&json).unwrap();
    assert_eq!(restored, a);
    assert_eq!(
        restored.verify().unwrap().path(),
        parent.canonicalize().unwrap()
    );
    assert_eq!(a.canonical_path(), parent.canonicalize().unwrap());
    assert_eq!(a.overlap(&restored).unwrap(), RootOverlap::Same);
    assert_eq!(a.overlap(&b).unwrap(), RootOverlap::LeftContainsRight);
    assert_eq!(b.overlap(&a).unwrap(), RootOverlap::RightContainsLeft);
    assert_eq!(a.overlap(&c).unwrap(), RootOverlap::Disjoint);
    verify_directory_policy(&child).unwrap();
}

#[test]
fn replacement_and_missing_roots_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("original");
    fs::create_dir(&path).unwrap();
    let original = RootIdentity::capture(&path).unwrap();
    // Retain the old directory so its ID cannot immediately be reused.
    fs::rename(&path, temp.path().join("retired")).unwrap();
    assert!(matches!(
        original.verify(),
        Err(RootIdentityError::Unavailable(_))
    ));
    fs::create_dir(&path).unwrap();
    let replacement = RootIdentity::capture(&path).unwrap();
    assert_ne!(original, replacement);
    assert!(matches!(
        original.verify(),
        Err(RootIdentityError::IdentityChanged)
    ));
    assert!(original.overlap(&replacement).is_err());
    assert!(replacement.overlap(&original).is_err());
}

#[cfg(windows)]
#[test]
fn windows_deleted_then_recreated_root_does_not_inherit_authority() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    let old = RootIdentity::capture(&root).unwrap();
    fs::remove_dir(&root).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(matches!(
        old.verify(),
        Err(RootIdentityError::IdentityChanged)
    ));
}

#[test]
fn malformed_snapshot_version_platform_and_path_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let a = RootIdentity::capture(temp.path()).unwrap();
    let original = serde_json::to_value(&a).unwrap();
    let mut value = original.clone();
    value["version"] = 99.into();
    assert!(matches!(
        serde_json::from_value::<RootIdentity>(value)
            .unwrap()
            .verify(),
        Err(RootIdentityError::InvalidSnapshot)
    ));
    let mut value = original.clone();
    value["canonical_path"] = "relative".into();
    assert!(matches!(
        serde_json::from_value::<RootIdentity>(value)
            .unwrap()
            .verify(),
        Err(RootIdentityError::InvalidSnapshot)
    ));
    let mut value = original.clone();
    value["extra"] = true.into();
    assert!(serde_json::from_value::<RootIdentity>(value).is_err());
    let mut value = original.clone();
    value["directory_id"]["platform"] = "invented".into();
    assert!(serde_json::from_value::<RootIdentity>(value).is_err());
    let mut value = original.clone();
    value["directory_id"]["unexpected"] = true.into();
    assert!(serde_json::from_value::<RootIdentity>(value).is_err());
    let mut value = original.clone();
    if cfg!(windows) {
        value["directory_id"]["file_id"] = serde_json::json!(vec![0u8; 16]);
    } else {
        value["directory_id"]["inode"] = 0.into();
    }
    assert!(matches!(
        serde_json::from_value::<RootIdentity>(value)
            .unwrap()
            .verify(),
        Err(RootIdentityError::IdentityChanged)
    ));
    let mut value = original;
    value["directory_id"] = if cfg!(windows) {
        serde_json::json!({"platform":"UnixFixture","device":1,"inode":1})
    } else {
        serde_json::json!({"platform":"Windows","volume_serial":1,"file_id":vec![0u8;16]})
    };
    assert!(matches!(
        serde_json::from_value::<RootIdentity>(value)
            .unwrap()
            .verify(),
        Err(RootIdentityError::InvalidSnapshot)
    ));
}

#[test]
fn invalid_input_and_two_missing_roots_do_not_compare_equal() {
    assert!(RootIdentity::capture(std::path::Path::new("relative")).is_err());
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    let ra = RootIdentity::capture(&a).unwrap();
    let rb = RootIdentity::capture(&b).unwrap();
    fs::remove_dir(&a).unwrap();
    fs::remove_dir(&b).unwrap();
    assert!(ra.overlap(&rb).is_err());
    assert!(rb.overlap(&ra).is_err());
    let file = temp.path().join("file");
    fs::write(&file, "sentinel").unwrap();
    assert!(RootIdentity::capture(&file).is_err());
}

#[cfg(windows)]
fn junction(link: &std::path::Path, target: &std::path::Path) {
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction fixture unavailable: {:?}",
        output
    );
}

#[cfg(windows)]
#[test]
fn windows_root_and_ancestor_junctions_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    fs::create_dir_all(real.join("child")).unwrap();
    let link = temp.path().join("alias");
    junction(&link, &real);
    assert!(matches!(
        RootIdentity::capture(&link),
        Err(RootIdentityError::ReparsePoint)
    ));
    assert!(matches!(
        RootIdentity::capture(&link.join("child")),
        Err(RootIdentityError::ReparsePoint)
    ));
    assert!(verify_directory_policy(&link.join("child")).is_err());
    fs::remove_dir(&link).unwrap();
    assert!(real.join("child").is_dir());
}

#[cfg(windows)]
#[test]
fn windows_replacement_with_junction_cannot_reuse_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let other = temp.path().join("other");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&other).unwrap();
    let identity = RootIdentity::capture(&root).unwrap();
    fs::rename(&root, temp.path().join("retired")).unwrap();
    junction(&root, &other);
    assert!(matches!(
        identity.verify(),
        Err(RootIdentityError::ReparsePoint)
    ));
    fs::remove_dir(&root).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_case_sensitive_root_and_descendant_policy_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("case");
    fs::create_dir(&child).unwrap();
    let before = RootIdentity::capture(&child).unwrap();
    let output = std::process::Command::new("fsutil.exe")
        .args(["file", "setCaseSensitiveInfo"])
        .arg(&child)
        .arg("enable")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "case-sensitive fixture unavailable (not a pass): {:?}",
        output
    );
    assert!(matches!(
        RootIdentity::capture(&child),
        Err(RootIdentityError::CaseSensitiveDirectory)
    ));
    assert!(matches!(
        before.verify(),
        Err(RootIdentityError::CaseSensitiveDirectory)
    ));
    assert!(matches!(
        verify_directory_policy(&child),
        Err(RootIdentityError::CaseSensitiveDirectory)
    ));
    fs::create_dir(child.join("nested")).unwrap();
    assert!(RootIdentity::capture(&child.join("nested")).is_err());
    fs::remove_dir(child.join("nested")).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_case_alias_same_root_and_device_paths_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("MiXeD");
    fs::create_dir(&root).unwrap();
    let upper = std::path::PathBuf::from(root.to_str().unwrap().to_ascii_uppercase());
    let a = RootIdentity::capture(&root).unwrap();
    let b = RootIdentity::capture(&upper).unwrap();
    assert_eq!(a.overlap(&b).unwrap(), RootOverlap::Same);
    assert_eq!(
        a.overlap(&RootIdentity::capture(a.canonical_path()).unwrap())
            .unwrap(),
        RootOverlap::Same
    );
    for path in [
        r"\\server\share\root",
        r"\\.\C:\",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1\",
    ] {
        assert!(matches!(
            RootIdentity::capture(std::path::Path::new(path)),
            Err(RootIdentityError::InvalidPath)
        ));
    }
}

#[cfg(unix)]
#[test]
fn unix_fixture_symlink_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    fs::create_dir(&real).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(
        RootIdentity::capture(&link),
        Err(RootIdentityError::ReparsePoint)
    ));
}
