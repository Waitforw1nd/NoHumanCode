param([Parameter(Mandatory=$true)][string]$Attempt)
$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
$taskRoot=Split-Path -Parent $PSScriptRoot
$taskRustRoot=Join-Path $taskRoot 'NoManCode/rust-app'
Set-Location -LiteralPath $taskRustRoot
$env:RUSTUP_AUTO_INSTALL='0'
$env:CARGO_NET_OFFLINE='true'
$env:TMP=Join-Path $taskRoot '.local/tmp'
$env:TEMP=$env:TMP
$env:CARGO_TARGET_DIR=Join-Path $taskRoot '.local/target'
$env:CARGO_PROFILE_DEV_DEBUG='0'
$env:CARGO_PROFILE_TEST_DEBUG='0'
$env:CARGO_INCREMENTAL='0'
$env:CARGO_BUILD_JOBS='2'
$taskPrior=(Get-Content (Join-Path $taskRoot '.local/next17-c-components-attempt1/meta.json') -Raw | ConvertFrom-Json).environment
$env:VCToolsInstallDir=$taskPrior.msvc
$env:WindowsSdkDir=$taskPrior.sdk
if(-not (Test-Path -LiteralPath $taskPrior.compiler -PathType Leaf)){throw 'Previously used installed compiler missing; no installation permitted'}
$taskLogDir=Join-Path $taskRoot ('.local/logs/next17-final-'+$Attempt)
if(Test-Path -LiteralPath $taskLogDir){throw 'Refuse overwrite of final gate evidence'}
New-Item -ItemType Directory -Path $taskLogDir | Out-Null
New-Item -ItemType Directory -Force -Path $env:TMP,$env:CARGO_TARGET_DIR,(Join-Path $taskRustRoot 'target') | Out-Null
$taskExpectedHead='a95e9923e7ba293dba964670c3fc4d98fbd9357a'
$taskExpectedNoMan='906328dea19bc45b0a29c8279b931144b08144cb'
$taskExpectedRust='7e9f77907c5df4712d2689dd22532579d3e01e15'
$taskBuildPath=Join-Path $taskRustRoot 'build.ps1'
$taskCommands=[ordered]@{check='cargo check --workspace --locked';test='cargo test --workspace --locked';fmt='cargo fmt --all -- --check';clippy='cargo clippy --workspace --all-targets --locked -- -D warnings';'wasm-check'='cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked'}
$taskMeta=[ordered]@{attempt=$Attempt;execution_id='C-R18-01-20261031-E01';scope='Manager authorized final gates; source/index frozen';start_utc=[DateTime]::UtcNow.ToString('o');status='preparing';head=$taskExpectedHead;no_man_code_tree=$taskExpectedNoMan;rust_tree=$taskExpectedRust;cwd=$taskRustRoot;environment=[ordered]@{RUSTUP_AUTO_INSTALL=$env:RUSTUP_AUTO_INSTALL;CARGO_NET_OFFLINE=$env:CARGO_NET_OFFLINE;TMP=$env:TMP;TEMP=$env:TEMP;CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR;CARGO_PROFILE_DEV_DEBUG=$env:CARGO_PROFILE_DEV_DEBUG;CARGO_PROFILE_TEST_DEBUG=$env:CARGO_PROFILE_TEST_DEBUG;CARGO_INCREMENTAL=$env:CARGO_INCREMENTAL;CARGO_BUILD_JOBS=$env:CARGO_BUILD_JOBS;VCToolsInstallDir=$env:VCToolsInstallDir;WindowsSdkDir=$env:WindowsSdkDir};rustc=(& rustc --version | Out-String).Trim();cargo=(& cargo --version | Out-String).Trim();rustfmt=(& rustfmt --version | Out-String).Trim();installed_targets=@(& rustup target list --installed);msvc=$taskPrior.msvc;sdk_version=$taskPrior.sdk_version;compiler=$taskPrior.compiler;build_ps1_sha256=(Get-FileHash -LiteralPath $taskBuildPath).Hash;driver_sha256=(Get-FileHash -LiteralPath $PSCommandPath).Hash;steps=@();not_executed=@($taskCommands.Keys)}
$taskMetaPath=Join-Path $taskLogDir 'meta.json'
function Save-Metadata { $taskMeta | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $taskMetaPath -Encoding utf8 }
function Capture-Snapshot([string]$Label) {
    $sourcePaths=@(& git -C $taskRoot -c core.quotepath=false ls-files -- NoManCode)
    if($LASTEXITCODE -ne 0 -or $sourcePaths.Count -eq 0){throw 'Cannot enumerate tracked source'}
    $sourceHashes=@(foreach($path in $sourcePaths){$file=Join-Path $taskRoot $path;[ordered]@{path=$path;bytes=(Get-Item -LiteralPath $file).Length;sha256=(Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash}})
    $hashName=$Label+'-source-hashes.json'
    $hashPath=Join-Path $taskLogDir $hashName
    $sourceHashes | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $hashPath -Encoding utf8
    $snapshot=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');head=(& git rev-parse HEAD).Trim();no_man_code_tree=(& git rev-parse HEAD:NoManCode).Trim();rust_tree=(& git rev-parse HEAD:NoManCode/rust-app).Trim();branch=(& git branch --show-current).Trim();status=@(& git status --short);index=@(& git diff --cached --name-only);unmerged=@(& git ls-files -u);source_file_count=$sourceHashes.Count;source_hash_file=$hashName;source_hash_manifest_sha256=(Get-FileHash -LiteralPath $hashPath).Hash}
    $snapshot.invariant_ok=($snapshot.head -eq $taskExpectedHead -and $snapshot.no_man_code_tree -eq $taskExpectedNoMan -and $snapshot.rust_tree -eq $taskExpectedRust -and $snapshot.branch -eq 'codex/c/agent-runtime-interface' -and $snapshot.status.Count -eq 0 -and $snapshot.index.Count -eq 0 -and $snapshot.unmerged.Count -eq 0)
    return $snapshot
}
Save-Metadata
foreach($action in $taskCommands.Keys){
    $before=Capture-Snapshot ($action+'-before')
    if(-not $before.invariant_ok){$taskMeta.status='blocked_candidate_changed';Save-Metadata;throw 'Candidate/source/index invariant failed before gate'}
    $started=[DateTime]::UtcNow.ToString('o')
    $taskMeta.status='running_'+$action
    $taskMeta.active_gate=[ordered]@{action=$action;command='pwsh -NoProfile -File ./build.ps1 -Action '+$action;cargo_command=$taskCommands[$action];start_utc=$started;before=$before}
    Save-Metadata
    Write-Output ('START '+$action+' '+$started)
    & pwsh -NoProfile -File $taskBuildPath -Action $action *> (Join-Path $taskLogDir ($action+'.log'))
    $exitCode=$LASTEXITCODE
    $ended=[DateTime]::UtcNow.ToString('o')
    $after=Capture-Snapshot ($action+'-after')
    $sameSource=($before.source_hash_manifest_sha256 -eq $after.source_hash_manifest_sha256)
    $taskMeta.steps+=[ordered]@{action=$action;command='pwsh -NoProfile -File ./build.ps1 -Action '+$action;cargo_command=$taskCommands[$action];start_utc=$started;end_utc=$ended;exit_code=$exitCode;log=$action+'.log';before=$before;after=$after;source_hashes_unchanged=$sameSource}
    $taskMeta.not_executed=@($taskCommands.Keys | Where-Object {$_ -notin @($taskMeta.steps | ForEach-Object {$_.action})})
    $taskMeta.Remove('active_gate')
    $taskMeta.status=if($exitCode -eq 0 -and $after.invariant_ok -and $sameSource){'completed_'+$action}else{'failed_'+$action}
    Save-Metadata
    Write-Output ('END '+$action+' exit='+$exitCode+' source_unchanged='+$sameSource)
    if($exitCode -ne 0 -or -not $after.invariant_ok -or -not $sameSource){
        Get-Content -LiteralPath (Join-Path $taskLogDir ($action+'.log')) -Tail 100
        if($exitCode -ne 0){exit $exitCode}else{exit 97}
    }
}
$taskMeta.status='all_five_passed'
$taskMeta.end_utc=[DateTime]::UtcNow.ToString('o')
Save-Metadata
Write-Output 'ALL FIVE GATES PASSED; stop builds and record final freeze before handoff'
