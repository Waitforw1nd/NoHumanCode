param([Parameter(Mandatory=$true)][string]$Label,[ValidateSet('i09','security','inputs','clippy','format','cli')][string]$Action='i09')
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
$taskPrior=Get-Content -Raw -LiteralPath (Join-Path $taskRoot '.local/credential-input-component/logs/attempt3.json') | ConvertFrom-Json
$env:VCToolsInstallDir=$taskPrior.environment.MSVC
$env:WindowsSdkDir=$taskPrior.environment.SDK
$taskBuild=[IO.File]::ReadAllText((Join-Path (Get-Location) 'build.ps1'))
$taskStart=$taskBuild.IndexOf('$taskCandidates')
$taskEnd=$taskBuild.IndexOf('switch ($Action)')
. ([ScriptBlock]::Create($taskBuild.Substring($taskStart,$taskEnd-$taskStart)))
$taskLog=Join-Path $taskLocal ('logs/'+$Label+'.log')
$taskMeta=Join-Path $taskLocal ('logs/'+$Label+'.json')
if(Test-Path -LiteralPath $taskLog){throw 'Evidence label already exists'}
$taskArgs=switch($Action){
'i09' {@('test','--locked','--lib','host_configuration_crash_tests','--','--test-threads=1','--nocapture')}
'security' {@('test','--locked','--test','host_configuration_security','--','--test-threads=1','--nocapture')}
'inputs' {@('test','--locked','--lib','credential_input::','--','--test-threads=1')}
'cli' {@('test','--locked','--test','host_configuration_cli','--','--test-threads=1')}
'clippy' {@('clippy','--locked','--lib','--test','host_configuration_security','--','-D','warnings')}
'format' {@('--edition','2024','--config','style_edition=2024,skip_children=true','--check','src/credential_input.rs','src/host_configuration_crash_tests.rs','tests/host_configuration_security.rs')}
}
$taskTool=if($Action -eq 'format'){'<existing-rustup>/toolchains/stable-x86_64-pc-windows-msvc/bin/rustfmt.exe'}else{'cargo'}
$taskFiles=@('src/credential_input.rs','src/host_configuration_crash_tests.rs','tests/host_configuration_security.rs','Cargo.toml','Cargo.lock','src/lib.rs') | Where-Object {Test-Path -LiteralPath $_}
$taskBefore=[ordered]@{head=(& git rev-parse HEAD);source_tree=(& git rev-parse HEAD:NoManCode);rust_tree=(& git rev-parse HEAD:NoManCode/rust-app);status=@(& git status --short);files=@(Get-FileHash -LiteralPath $taskFiles -Algorithm SHA256 | Select-Object Path,Hash);utc=[DateTime]::UtcNow.ToString('o')}
$ErrorActionPreference='Continue'
& $taskTool @taskArgs *> $taskLog
$taskExit=$LASTEXITCODE
$ErrorActionPreference='Stop'
$taskRecord=[ordered]@{label=$Label;action=$Action;command=(@($taskTool)+$taskArgs);cwd=(Get-Location).Path;before=$taskBefore;end_utc=[DateTime]::UtcNow.ToString('o');exit_code=$taskExit;after_head=(& git rev-parse HEAD);after_status=@(& git status --short);after_files=@(Get-FileHash -LiteralPath $taskFiles -Algorithm SHA256 | Select-Object Path,Hash);environment=@{TMP=$env:TMP;TEMP=$env:TEMP;CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR;CARGO_NET_OFFLINE=$env:CARGO_NET_OFFLINE;MSVC=$taskMsvc;SDK=$taskSdkRoot};log_sha256=(Get-FileHash -LiteralPath $taskLog -Algorithm SHA256).Hash}
[IO.File]::WriteAllText($taskMeta,($taskRecord|ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
Get-Content -LiteralPath $taskLog -Tail 80
Write-Output ('COMPONENT_EXIT='+$taskExit)
exit $taskExit