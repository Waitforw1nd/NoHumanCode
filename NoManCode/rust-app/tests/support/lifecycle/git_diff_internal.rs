//! Migrated verbatim assertions from tests/git_diff.rs; private builder coverage stays internal.
use crate::git_diff;
use git_diff::{
    GitDiffError as E, GitDiffRequest, GitDiffStatus as S, GitDiffView as V, read_git_diff,
};
use std::{path::Path, process::Command};

// Setup uses the actual supervisor too, with hidden creation and original Job
// ownership. Its fixture lease is separate from test_scope's real Store ticket.
trait FixtureOutput {
    fn lifecycle_output(&mut self) -> std::process::Output;
}
struct SetupLease;
impl crate::process_lifetime::DomainLease for SetupLease {
    fn seal_unknown(&mut self) {}
    fn settle(self: Box<Self>) {}
}
impl FixtureOutput for Command {
    fn lifecycle_output(&mut self) -> std::process::Output {
        use crate::process_lifetime::{
            CompletionReason, Outcome, OverflowPolicy, ProcessSpec, Supervisor,
        };
        use std::{
            num::NonZeroUsize,
            os::windows::process::ExitStatusExt,
            path::PathBuf,
            time::{Duration, Instant},
        };
        let name = PathBuf::from(self.get_program());
        let program = if name.is_absolute() {
            name
        } else {
            let name = if name.extension().is_none() {
                name.with_extension("exe")
            } else {
                name
            };
            std::env::split_paths(&std::env::var_os("PATH").unwrap())
                .filter(|p| p.is_absolute())
                .map(|p| p.join(&name))
                .find(|p| p.is_file())
                .expect("fixture executable on PATH")
        };
        let cwd = self
            .get_current_dir()
            .map(Path::to_owned)
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        let mut environment: Vec<_> = std::env::vars_os().collect();
        for (key, value) in self.get_envs() {
            environment.retain(|(k, _)| {
                !k.to_string_lossy()
                    .eq_ignore_ascii_case(&key.to_string_lossy())
            });
            if let Some(value) = value {
                environment.push((key.to_owned(), value.to_owned()));
            }
        }
        let mut spec = ProcessSpec::new(
            program,
            cwd,
            environment,
            (1_048_576, 1_048_576),
            OverflowPolicy::Terminate,
            Instant::now() + Duration::from_secs(15),
            Duration::from_secs(5),
        );
        spec.args = self.get_args().map(ToOwned::to_owned).collect();
        std::thread::spawn(move || {
            let evidence = tempfile::Builder::new()
                .prefix("git-setup-")
                .tempdir()
                .unwrap()
                .keep();
            let supervisor = Supervisor::new(NonZeroUsize::new(1).unwrap());
            let operation = match supervisor.submit(spec, Box::new(SetupLease)) {
                Ok(operation) => operation,
                Err(rejected) => {
                    rejected.lease.settle();
                    panic!("fixture rejected: {}", rejected.reason);
                }
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            let outcome = runtime.block_on(operation.wait());
            let Outcome::Settled(result) = outcome else {
                if matches!(outcome, Outcome::Unknown) {
                    crate::fixture::retain_unknown(&evidence, "Git setup process outcome unknown");
                }
                panic!("fixture did not start: {outcome:?}");
            };
            let end = Instant::now() + Duration::from_secs(5);
            while supervisor.active_count() != 0 {
                if Instant::now() >= end {
                    crate::fixture::retain_unknown(&evidence, "Git setup holder has not exited");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                result.reason,
                CompletionReason::Exited,
                "fixture command exceeded its bound"
            );
            std::process::Output {
                status: std::process::ExitStatus::from_raw(result.exit_code),
                stdout: result.stdout.bytes,
                stderr: result.stderr.bytes,
            }
        })
        .join()
        .unwrap()
    }
}

// A nested libtest may itself retain an unknown holder. Its supervisor must not
// kill that evidence on an outer deadline: retain the original child instead.
fn retained_test_child_output(command: &mut Command) -> std::process::Output {
    use std::{
        os::windows::process::CommandExt,
        process::Stdio,
        time::{Duration, Instant},
    };
    let evidence = tempfile::Builder::new()
        .prefix("git-env-child-")
        .tempdir()
        .unwrap()
        .keep();
    let stdout = evidence.join("stdout.log");
    let stderr = evidence.join("stderr.log");
    command
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap());
    let mut child = command
        .spawn()
        .expect("create exact environment test child");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => crate::fixture::retain_unknown(
                &evidence,
                "nested environment test child exit unconfirmed; original Child retained",
            ),
        }
    };
    std::process::Output {
        status,
        stdout: std::fs::read(stdout).unwrap(),
        stderr: std::fs::read(stderr).unwrap(),
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "user.name=Diff Test",
            "-c",
            "user.email=diff@example.invalid",
        ])
        .args(args)
        .lifecycle_output();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
fn repo() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--quiet"]);
    git(root.path(), &["config", "core.autocrlf", "false"]);
    git(root.path(), &["config", "user.name", "Diff Test"]);
    git(
        root.path(),
        &["config", "user.email", "diff@example.invalid"],
    );
    root
}
fn write(root: &Path, path: &str, bytes: impl AsRef<[u8]>) {
    if let Some(parent) = root.join(path).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(root.join(path), bytes).unwrap();
}
fn commit(root: &Path, path: &str) {
    git(root, &["add", "--", path]);
    git(root, &["commit", "--quiet", "-m", "fixture"]);
}
fn req(path: &str, view: V) -> GitDiffRequest {
    GitDiffRequest {
        path: path.into(),
        view,
    }
}
async fn diff(root: &Path, path: &str, view: V) -> git_diff::GitDiff {
    read_git_diff(root, req(path, view)).await.unwrap()
}

#[tokio::test]
async fn three_views_read_real_objects_and_preserve_crlf_and_eof() {
    crate::execution_authority::test_scope(async {
        let root = repo();
        let root = root.path();
        write(root, "file.txt", "head\r\nlast");
        commit(root, "file.txt");
        write(root, "file.txt", "index\r\nstaged");
        git(root, &["add", "file.txt"]);
        write(root, "file.txt", "work\r\nunstaged");
        let before_head = std::fs::read(root.join(".git/HEAD")).unwrap();
        let before_index = std::fs::read(root.join(".git/index")).unwrap();
        for (view, removed, added) in [
            (V::Staged, "head\r\n", "index\r\n"),
            (V::Unstaged, "index\r\n", "work\r\n"),
            (V::Head, "head\r\n", "work\r\n"),
        ] {
            let result = diff(root, "file.txt", view).await;
            assert_eq!(result.status, S::Text);
            assert!(!result.redacted);
            assert_eq!(
                result.base_oid.as_deref(),
                Some(git(root, &["rev-parse", "HEAD"]).as_str())
            );
            assert_eq!(
                result.index_oid.as_deref(),
                Some(git(root, &["rev-parse", ":file.txt"]).as_str())
            );
            let patch = result.patch.unwrap();
            assert!(patch.contains(&format!("-{removed}")));
            assert!(patch.contains(&format!("+{added}")));
            assert!(patch.contains("@@ -1,2 +1,2 @@"));
            assert_eq!(patch.matches("\\ No newline at end of file").count(), 2);
        }
        assert_eq!(std::fs::read(root.join(".git/HEAD")).unwrap(), before_head);
        assert_eq!(
            std::fs::read(root.join(".git/index")).unwrap(),
            before_index
        );
        assert_eq!(
            std::fs::read(root.join("file.txt")).unwrap(),
            b"work\r\nunstaged"
        );
    })
    .await;
}

#[tokio::test]
async fn unborn_untracked_ignored_missing_and_empty_existence() {
    crate::execution_authority::test_scope(async {
        let root = repo();
        let root = root.path();
        write(root, "new.txt", "new\n");
        let d = diff(root, "new.txt", V::Head).await;
        assert_eq!(d.base_oid, None);
        assert_eq!(d.index_oid, None);
        assert_eq!(d.before_digest, None);
        assert!(d.patch.unwrap().contains("--- /dev/null\n"));
        assert_eq!(diff(root, "new.txt", V::Staged).await.status, S::Missing);
        assert_eq!(diff(root, "absent.txt", V::Head).await.status, S::Missing);
        write(root, "empty.txt", "");
        assert_eq!(diff(root, "empty.txt", V::Head).await.status, S::Text);
        git(root, &["add", "new.txt"]);
        assert_eq!(diff(root, "new.txt", V::Staged).await.status, S::Text);
        assert_eq!(
            diff(root, "new.txt", V::Unstaged).await.status,
            S::Unchanged
        );
        write(root, ".gitignore", "ignored/\n");
        write(root, "ignored/plain.txt", "hidden\n");
        for view in [V::Staged, V::Unstaged, V::Head] {
            assert_eq!(
                read_git_diff(root, req("ignored/plain.txt", view))
                    .await
                    .unwrap_err(),
                E::Unsupported
            );
        }
    })
    .await;
}

#[tokio::test]
async fn deletion_and_rename_are_separate_paths() {
    crate::execution_authority::test_scope(async {
        let root = repo();
        let root = root.path();
        write(root, "old.txt", "original\n");
        commit(root, "old.txt");
        std::fs::rename(root.join("old.txt"), root.join("new.txt")).unwrap();
        for view in [V::Unstaged, V::Head] {
            assert!(
                diff(root, "old.txt", view)
                    .await
                    .patch
                    .unwrap()
                    .contains("+++ /dev/null\n")
            );
            assert!(
                diff(root, "new.txt", view)
                    .await
                    .patch
                    .unwrap()
                    .contains("--- /dev/null\n")
            );
        }
        git(root, &["add", "-A"]);
        assert!(
            diff(root, "old.txt", V::Staged)
                .await
                .patch
                .unwrap()
                .contains("-original\n")
        );
        assert!(
            diff(root, "new.txt", V::Staged)
                .await
                .patch
                .unwrap()
                .contains("+original\n")
        );
        assert_eq!(diff(root, "old.txt", V::Unstaged).await.status, S::Missing);
        write(root, ".gitignore", "old.txt\n");
        assert!(
            diff(root, "old.txt", V::Staged)
                .await
                .patch
                .unwrap()
                .contains("-original\n")
        );
    })
    .await;
}

#[tokio::test]
async fn absent_promisor_object_is_unavailable_without_network_or_writes() {
    crate::execution_authority::test_scope(async {
        let root = repo();
        let root = root.path();
        write(root, "a.txt", "missing object\n");
        commit(root, "a.txt");
        let oid = git(root, &["rev-parse", "HEAD:a.txt"]);
        let object = root.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
        // This fixture owns this newly-created loose object; it is not a repository
        // cleanup or an operation exposed by the production reader.
        std::fs::remove_file(object).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        git(
            root,
            &[
                "config",
                "remote.origin.url",
                &format!("http://{}/objects", listener.local_addr().unwrap()),
            ],
        );
        git(root, &["config", "remote.origin.promisor", "true"]);
        git(root, &["config", "extensions.partialClone", "origin"]);
        git(root, &["config", "protocol.http.allow", "always"]);
        let index = std::fs::read(root.join(".git/index")).unwrap();
        assert_eq!(
            read_git_diff(root, req("a.txt", V::Head))
                .await
                .unwrap_err(),
            E::Unavailable
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
        assert_eq!(
            std::fs::read(root.join("a.txt")).unwrap(),
            b"missing object\n"
        );
        // Prove the empty allowlist denies a custom helper even when local config
        // explicitly allows it. A permissive fixture control first proves this
        // local-only helper is executable; it merely writes a sentinel and exits.
        let helper_dir = tempfile::tempdir().unwrap();
        let marker = helper_dir.path().join("invoked");
        let helper = helper_dir.path().join("git-remote-nhcfixture");
        let escaped = marker
            .to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\\''");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\nprintf invoked > '{escaped}'\nexit 1\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        git(root, &["config", "protocol.nhcfixture.allow", "always"]);
        let path = std::env::join_paths(
            std::iter::once(helper_dir.path().to_path_buf())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut control = git_diff::transport_fixture_command(root).unwrap();
        control
            .env("PATH", &path)
            .env("GIT_ALLOW_PROTOCOL", "nhcfixture")
            .args(["ls-remote", "nhcfixture://local-fixture"]);
        let _ = git_diff::run_command(control, 8192, std::time::Duration::from_secs(3))
            .await
            .unwrap();
        assert!(
            marker.exists(),
            "local helper control must actually execute"
        );
        std::fs::remove_file(&marker).unwrap();
        let mut denied = git_diff::transport_fixture_command(root).unwrap();
        assert!(
            denied
                .as_std()
                .get_envs()
                .any(|(key, value)| key == "GIT_ALLOW_PROTOCOL"
                    && value.is_some_and(|v| v.is_empty()))
        );
        denied
            .env("PATH", &path)
            .args(["ls-remote", "nhcfixture://local-fixture"]);
        let denied = git_diff::run_command(denied, 8192, std::time::Duration::from_secs(3))
            .await
            .unwrap();
        assert_ne!(denied.code, Some(0));
        assert!(
            !marker.exists(),
            "custom transport bypassed the empty allowlist"
        );
    })
    .await;
}

#[tokio::test]
async fn root_boundaries_and_linked_worktree() {
    crate::execution_authority::test_scope(async {
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(
            read_git_diff(plain.path(), req("a.txt", V::Head))
                .await
                .unwrap_err(),
            E::Unavailable
        );
        let root = repo();
        write(root.path(), "a.txt", "a\n");
        commit(root.path(), "a.txt");
        std::fs::create_dir(root.path().join("sub")).unwrap();
        assert_eq!(
            read_git_diff(&root.path().join("sub"), req("a.txt", V::Head))
                .await
                .unwrap_err(),
            E::Unavailable
        );
        git(&root.path().join("sub"), &["init", "--quiet"]);
        write(root.path(), "sub/a.txt", "sub\n");
        assert_eq!(
            read_git_diff(root.path(), req("sub/a.txt", V::Head))
                .await
                .unwrap_err(),
            E::Unavailable
        );
        let linked = tempfile::tempdir().unwrap();
        let linked_path = linked.path().join("linked");
        git(
            root.path(),
            &["worktree", "add", "--detach", linked_path.to_str().unwrap()],
        );
        write(&linked_path, "a.txt", "linked\n");
        assert!(
            diff(&linked_path, "a.txt", V::Head)
                .await
                .patch
                .unwrap()
                .contains("+linked\n")
        );
        let bare = tempfile::tempdir().unwrap();
        git(bare.path(), &["init", "--bare", "--quiet"]);
        assert_eq!(
            read_git_diff(bare.path(), req("a.txt", V::Head))
                .await
                .unwrap_err(),
            E::Unavailable
        );
    })
    .await;
}

#[tokio::test]
async fn path_validation_literal_names_and_link_sentinel() {
    crate::execution_authority::test_scope(async {
        let root = repo();
        let root = root.path();
        for name in ["中文 空格.txt", "-leading.txt", "[ab].txt"] {
            write(root, name, "literal\n");
            let result = diff(root, name, V::Head).await;
            assert_eq!(result.path, name);
            assert!(result.patch.unwrap().contains("+literal\n"));
            commit(root, name);
            write(root, name, "literal changed\n");
            assert!(
                diff(root, name, V::Head)
                    .await
                    .patch
                    .unwrap()
                    .contains("-literal\n")
            );
        }
        for path in [
            "../outside",
            "/absolute",
            "C:/outside",
            ".git/config",
            ".env",
            "dir/secret.key",
            ".ssh/id",
            "token=secret",
            "a\nfile",
            ":(glob)*",
            "*",
        ] {
            assert_eq!(
                read_git_diff(root, req(path, V::Head)).await.unwrap_err(),
                E::InvalidPath,
                "{path}"
            );
        }
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "sentinel.txt", "sentinel-private\n");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(
            outside.path().join("sentinel.txt"),
            root.join("link.txt"),
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path().join("sentinel.txt"), root.join("link.txt"))
            .unwrap();
        assert_eq!(
            read_git_diff(root, req("link.txt", V::Head))
                .await
                .unwrap_err(),
            E::InvalidPath
        );
        assert_eq!(
            std::fs::read(outside.path().join("sentinel.txt")).unwrap(),
            b"sentinel-private\n"
        );
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(outside.path(), root.join("linked-dir")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.join("linked-dir")).unwrap();
        assert_eq!(
            read_git_diff(root, req("linked-dir/sentinel.txt", V::Head))
                .await
                .unwrap_err(),
            E::InvalidPath
        );
    })
    .await;
}

#[tokio::test]
async fn unmerged_gitlink_and_git_symlink_are_unsupported() {
    crate::execution_authority::test_scope(async {
        let root = repo();
        let root = root.path();
        write(root, "a.txt", "a\n");
        commit(root, "a.txt");
        let oid = git(root, &["rev-parse", "HEAD:a.txt"]);
        let commit_oid = git(root, &["rev-parse", "HEAD"]);
        git(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{commit_oid},module"),
            ],
        );
        assert_eq!(
            read_git_diff(root, req("module", V::Staged))
                .await
                .unwrap_err(),
            E::Unsupported
        );
        git(
            root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{oid},link"),
            ],
        );
        assert_eq!(
            read_git_diff(root, req("link", V::Staged))
                .await
                .unwrap_err(),
            E::Unsupported
        );
        git(root, &["checkout", "-b", "other"]);
        write(root, "a.txt", "other\n");
        commit(root, "a.txt");
        git(root, &["checkout", "-b", "diverge", "HEAD~1"]);
        write(root, "a.txt", "diverge\n");
        commit(root, "a.txt");
        let merge = Command::new("git")
            .current_dir(root)
            .args(["merge", "other"])
            .lifecycle_output();
        assert!(!merge.status.success());
        assert_eq!(
            read_git_diff(root, req("a.txt", V::Head))
                .await
                .unwrap_err(),
            E::Unsupported
        );
    })
    .await;
}

#[tokio::test]
async fn binary_large_patch_and_serialized_response_are_bounded() {
    crate::execution_authority::test_scope(async {
        let root = repo();
        let root = root.path();
        for bytes in [vec![0, b'a'], vec![0xff, b'a']] {
            write(root, "a.txt", bytes);
            let d = diff(root, "a.txt", V::Head).await;
            assert_eq!(d.status, S::Binary);
            assert_eq!(d.patch, None);
            assert!(d.after_digest.is_some());
        }
        write(root, "a.txt", vec![b'x'; 262145]);
        assert_eq!(diff(root, "a.txt", V::Head).await.status, S::TooLarge);
        git(root, &["add", "a.txt"]);
        assert_eq!(diff(root, "a.txt", V::Staged).await.status, S::TooLarge);
        write(root, "a.txt", "a\n".repeat(100000));
        commit(root, "a.txt");
        write(root, "a.txt", "b\n".repeat(100000));
        let d = diff(root, "a.txt", V::Head).await;
        assert_eq!(d.status, S::TooLarge);
        assert_eq!(d.patch, None);
        // JSON control escapes may exceed the CLI cap even with a bounded patch.
        write(root, "a.txt", vec![1; 200000]);
        commit(root, "a.txt");
        write(root, "a.txt", vec![2; 200000]);
        let d = diff(root, "a.txt", V::Head).await;
        assert_eq!(d.status, S::TooLarge);
        assert_eq!(d.patch, None);
    })
    .await;
}

#[tokio::test]
async fn secrets_are_redacted_before_diff_prefixes_and_digests_are_raw() {
    crate::execution_authority::test_scope(async {
        use sha2::{Digest, Sha256};
        let root = repo();
        let root = root.path();
        let before = "sk-oldcredential\napi_key=plainold\n";
        let after = "sk-newcredential\napi_key=plainnew\nbearer plainbearer\n";
        write(root, "a.txt", before);
        commit(root, "a.txt");
        write(root, "a.txt", after);
        let d = diff(root, "a.txt", V::Head).await;
        assert!(d.redacted);
        assert_eq!(
            d.before_digest,
            Some(format!("{:x}", Sha256::digest(before.as_bytes())))
        );
        assert_eq!(
            d.after_digest,
            Some(format!("{:x}", Sha256::digest(after.as_bytes())))
        );
        let body = serde_json::to_string(&d).unwrap();
        for secret in [
            "sk-oldcredential",
            "sk-newcredential",
            "plainold",
            "plainnew",
            "plainbearer",
        ] {
            assert!(!body.contains(secret));
        }
        assert!(d.patch.unwrap().contains("-[redacted]\n"));
        for error in [
            E::InvalidPath,
            E::Unavailable,
            E::Unsupported,
            E::Conflict,
            E::Internal,
        ] {
            assert!(!error.to_string().contains(root.to_str().unwrap()));
        }
    })
    .await;
}

#[tokio::test]
async fn external_diff_textconv_filters_and_hooks_are_never_invoked() {
    crate::execution_authority::test_scope(async {
        let root = repo();
        let root = root.path();
        write(root, "a.txt", "old\n");
        commit(root, "a.txt");
        write(root, ".gitattributes", "*.txt diff=evil filter=evil\n");
        // Any execution creates a sentinel; Git plumbing must not invoke these.
        let command = "echo invoked > external-sentinel";
        for key in [
            "diff.external",
            "diff.evil.textconv",
            "filter.evil.smudge",
            "filter.evil.clean",
            "core.fsmonitor",
        ] {
            git(root, &["config", key, command]);
        }
        write(root, "a.txt", "new\n");
        let index = std::fs::read(root.join(".git/index")).unwrap();
        assert!(
            diff(root, "a.txt", V::Head)
                .await
                .patch
                .unwrap()
                .contains("+new\n")
        );
        assert!(!root.join("external-sentinel").exists());
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
    })
    .await;
}

#[tokio::test]
async fn deterministic_head_index_and_work_changes_return_conflict() {
    crate::execution_authority::test_scope(async {
        for change in ["head", "index", "work"] {
            let root = repo();
            let root = root.path();
            write(root, "a.txt", "old\n");
            commit(root, "a.txt");
            let result = git_diff::read_with_barrier(root, req("a.txt", V::Head), || async {
                match change {
                    "head" => {
                        git(
                            root,
                            &["commit", "--allow-empty", "--quiet", "-m", "external"],
                        );
                    }
                    "index" => {
                        write(root, "a.txt", "new\n");
                        git(root, &["add", "a.txt"]);
                        write(root, "a.txt", "old\n");
                    }
                    _ => write(root, "a.txt", "new\n"),
                }
            })
            .await;
            assert_eq!(result.unwrap_err(), E::Conflict, "{change}");
        }
    })
    .await;
}

#[tokio::test]
async fn bounded_runner_terminates_overflow_and_timeout() {
    crate::execution_authority::test_scope(async {
        use std::{
            process::Stdio,
            time::{Duration, Instant},
        };
        let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .filter(|p| p.is_absolute())
            .map(|p| p.join("pwsh.exe"))
            .find(|p| p.is_file())
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let environment: Vec<_> = ["SystemRoot", "WINDIR", "PATH", "TEMP", "TMP"]
            .into_iter().filter_map(|key| std::env::var_os(key).map(|value| (key, value))).collect();
        let mut output = tokio::process::Command::new(&executable);
        output
            .current_dir(root.path())
            .env_clear()
            .envs(environment.clone());
        if cfg!(windows) {
            output.args([
                "-NoProfile",
                "-Command",
                "[IO.File]::WriteAllText((Join-Path $PWD 'overflow-started'),'started'); [Console]::Out.Write('x' * 100000); Start-Sleep -Seconds 30",
            ]);
        } else {
            output.args(["-c", "printf '%100000s' x; sleep 30"]);
        }
        output
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let start = Instant::now();
        let output_hooks = std::sync::Arc::new(crate::process_lifetime::TestHooks::default());
        let mut output_spec = crate::execution_authority::command_spec(&output, 128, start + Duration::from_secs(3)).unwrap();
        output_spec.test_hooks = Some(output_hooks.clone());
        let _output_override = crate::execution_authority::test_process_once(root.path(), output_spec);
        assert!(matches!(
            git_diff::run_command(output, 128, Duration::from_secs(3)).await,
            Err(E::Unavailable)
        ));
        assert!(start.elapsed() < Duration::from_secs(6));
        assert!(root.path().join("overflow-started").exists(), "overflow fixture did not actually execute");
        assert_original_process_reaped(&output_hooks);
        let mut sleeper = tokio::process::Command::new(&executable);
        sleeper
            .current_dir(root.path())
            .env_clear()
            .envs(environment);
        if cfg!(windows) {
            sleeper.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]);
        } else {
            sleeper.args(["-c", "sleep 30"]);
        }
        sleeper
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let start = Instant::now();
        let sleeper_hooks = std::sync::Arc::new(crate::process_lifetime::TestHooks::default());
        let mut sleeper_spec = crate::execution_authority::command_spec(&sleeper, 128, start + Duration::from_millis(100)).unwrap();
        sleeper_spec.test_hooks = Some(sleeper_hooks.clone());
        let _sleeper_override = crate::execution_authority::test_process_once(root.path(), sleeper_spec);
        assert!(matches!(
            git_diff::run_command(sleeper, 128, Duration::from_millis(100)).await,
            Err(E::Unavailable)
        ));
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_original_process_reaped(&sleeper_hooks);
    })
    .await;
}

fn assert_original_process_reaped(hooks: &crate::process_lifetime::TestHooks) {
    use std::{os::windows::io::AsRawHandle, sync::atomic::Ordering};
    use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject};
    assert!(
        hooks.process_handle_error.lock().unwrap().is_none(),
        "original handle duplication failed"
    );
    assert!(
        hooks.process_identity.lock().unwrap().is_some(),
        "process never acquired creation identity"
    );
    let original = hooks.process_handle.lock().unwrap();
    let original = original
        .as_ref()
        .expect("real CreateProcess object must exist; prestart failure is not coverage");
    assert_eq!(
        unsafe { WaitForSingleObject(original.as_raw_handle(), 0) },
        WAIT_OBJECT_0,
        "original process still live"
    );
    assert!(
        hooks.root_signaled.load(Ordering::Acquire) && hooks.job_empty.load(Ordering::Acquire),
        "original root or Job cleanup unproved"
    );
    assert!(
        hooks.stdout_eof_joined.load(Ordering::Acquire)
            && hooks.stderr_eof_joined.load(Ordering::Acquire),
        "both original readers must prove EOF and join"
    );
}

#[test]
fn strict_request_and_safe_error_contract() {
    assert!(
        serde_json::from_str::<GitDiffRequest>(r#"{"path":"a","view":"head","extra":1}"#).is_err()
    );
    assert!(serde_json::from_str::<GitDiffRequest>(r#"{"path":"a","view":"other"}"#).is_err());
    assert_eq!(
        serde_json::to_string(&S::TooLarge).unwrap(),
        "\"too_large\""
    );
}

#[tokio::test]
async fn malicious_environment_child() {
    crate::execution_authority::test_scope(async {
        let Some(root) = std::env::var_os("NHC_DIFF_ENV_FIXTURE") else {
            return;
        };
        let result = diff(Path::new(&root), "a.txt", V::Head).await;
        assert!(result.patch.unwrap().contains("+real-work\n"));
    })
    .await;
}

#[test]
fn inherited_git_redirection_and_config_are_cleared_in_child_process() {
    let root = repo();
    write(root.path(), "a.txt", "real-head\n");
    commit(root.path(), "a.txt");
    write(root.path(), "a.txt", "real-work\n");
    let alien = repo();
    write(alien.path(), "a.txt", "alien\n");
    commit(alien.path(), "a.txt");
    let output = retained_test_child_output(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "git_diff_contract_tests::malicious_environment_child",
                "--nocapture",
            ])
            .env("NHC_DIFF_ENV_FIXTURE", root.path())
            .env("GIT_DIR", alien.path().join(".git"))
            .env("GIT_WORK_TREE", alien.path())
            .env("GIT_INDEX_FILE", alien.path().join(".git/index"))
            .env("GIT_OBJECT_DIRECTORY", alien.path().join(".git/objects"))
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.bare")
            .env("GIT_CONFIG_VALUE_0", "true")
            .env("GIT_CONFIG_PARAMETERS", "invalid-injected-config")
            .env("GIT_EXTERNAL_DIFF", "nonexistent-external-command")
            .env("GIT_CONFIG_SYSTEM", alien.path().join("bad-config"))
            .env("GIT_CONFIG_GLOBAL", alien.path().join("bad-config")),
    );
    assert!(
        output.status.success(),
        "child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed"),
        "exact child selector did not execute exactly one passing test: {stdout}"
    );
}
