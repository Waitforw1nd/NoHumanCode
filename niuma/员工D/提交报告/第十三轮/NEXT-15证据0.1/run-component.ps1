param([string]$Label='attempt1',[ValidateSet('test','clippy','format')][string]$Action='test')
$ErrorActionPreference='Stop'
$taskLocal=$PSScriptRoot
$taskRoot=(Resolve-Path (Join-Path $taskLocal '../..')).Path
Set-Location -LiteralPath (Join-Path $taskRoot 'NoManCode/rust-app')
$env:TMP=Join-Path $taskLocal 'tmp'
$env:TEMP=$env:TMP
$env:CARGO_TARGET_DIR=Join-Path $taskLocal 'target'
$env:CARGO_NET_OFFLINE='true'
$env:CARGO_INCREMENTAL='0'
$env:CARGO_PROFILE_DEV_DEBUG='0'
$env:CARGO_PROFILE_TEST_DEBUG='0'
$env:CARGO_BUILD_JOBS='2'
$taskBuild=[IO.File]::ReadAllText((Join-Path (Get-Location) 'build.ps1'))
$taskStart=$taskBuild.IndexOf('$taskCandidates')
$taskEnd=$taskBuild.IndexOf('switch ($Action)')
. ([ScriptBlock]::Create($taskBuild.Substring($taskStart,$taskEnd-$taskStart)))
$taskLog=Join-Path $taskLocal ('logs/'+$Label+'.log')
$taskMeta=Join-Path $taskLocal ('logs/'+$Label+'.json')
if(Test-Path $taskLog){throw 'Evidence label already exists'}
$taskArgs = if($Action -eq 'test'){@('test','--manifest-path',(Join-Path $taskLocal 'Cargo.toml'),'--offline','--','--test-threads=1')} elseif($Action -eq 'clippy'){@('clippy','--manifest-path',(Join-Path $taskLocal 'Cargo.toml'),'--offline','--all-targets','--','-D','warnings')} else {@('--edition','2024','--check','src/credential_input.rs')}
$taskTool=if($Action -eq 'format'){'rustfmt'}else{'cargo'}
$taskBefore=[ordered]@{head=(& git rev-parse HEAD);source_tree=(& git rev-parse HEAD:NoManCode);rust_tree=(& git rev-parse HEAD:NoManCode/rust-app);status=@(& git status --short);files=@(Get-FileHash src/credential_input.rs,Cargo.toml,Cargo.lock -Algorithm SHA256 | Select-Object Path,Hash);utc=[DateTime]::UtcNow.ToString('o')}
$ErrorActionPreference='Continue'
& $taskTool @taskArgs *> $taskLog
$taskExit=$LASTEXITCODE
$ErrorActionPreference='Stop'
$taskRecord=[ordered]@{label=$Label;action=$Action;command=(@($taskTool)+$taskArgs);cwd=(Get-Location).Path;before=$taskBefore;end_utc=[DateTime]::UtcNow.ToString('o');exit_code=$taskExit;after_head=(& git rev-parse HEAD);after_status=@(& git status --short);after_files=@(Get-FileHash src/credential_input.rs,Cargo.toml,Cargo.lock -Algorithm SHA256 | Select-Object Path,Hash);environment=@{TMP=$env:TMP;TEMP=$env:TEMP;CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR;CARGO_NET_OFFLINE=$env:CARGO_NET_OFFLINE;MSVC=$taskMsvc;SDK=$taskSdkRoot};log_sha256=(Get-FileHash $taskLog -Algorithm SHA256).Hash}
[IO.File]::WriteAllText($taskMeta,($taskRecord|ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
Get-Content $taskLog -Tail 55
Write-Output ('COMPONENT_EXIT='+$taskExit)
exit $taskExit