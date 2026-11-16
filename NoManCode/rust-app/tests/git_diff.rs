#![cfg(windows)]
//! Private builder/transport/boundary assertions moved without removal to
//! support/lifecycle/git_diff_internal.rs (registered by the library).
#[allow(dead_code)]
#[path = "support/lifecycle/fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/lifecycle/host.rs"]
mod host;

use anyhow::{Result, ensure};
use peachsh::git_diff::{GitDiffError, GitDiffRequest, GitDiffStatus, GitDiffView, read_git_diff};

#[tokio::test]
async fn public_git_diff_requires_host_authority_and_host_path_still_works() -> Result<()> {
    let case = fixture::Case::new("git-public-")?;
    let host = host::Host::new(&case).await?;
    let (task, _) = host.shell("git init --quiet; if($LASTEXITCODE -ne 0){exit $LASTEXITCODE}; [IO.File]::WriteAllText((Join-Path $PWD 'a.txt'), 'host-authorized'); exit 0", 15).await?;
    host.finished(&task).await?;
    let request = || GitDiffRequest {
        path: "a.txt".into(),
        view: GitDiffView::Head,
    };
    ensure!(
        matches!(
            read_git_diff(&case.root, request()).await,
            Err(GitDiffError::Unavailable)
        ),
        "bare Git reader bypassed Host operation capability"
    );
    let diff = host.engine.git_diff(&task.id, request()).await?;
    ensure!(
        diff.status == GitDiffStatus::Text
            && diff.patch.unwrap_or_default().contains("+host-authorized"),
        "authorized Host Git path failed"
    );
    host.close().await
}
