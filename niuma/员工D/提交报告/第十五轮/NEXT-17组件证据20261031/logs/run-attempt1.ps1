$ErrorActionPreference='Stop'
$taskRoot=(Resolve-Path '../..').Path
$taskDir=Join-Path $taskRoot '.local/next17-d-component'
$env:TMP=Join-Path $taskDir 'tmp'; $env:TEMP=$env:TMP; $env:CARGO_TARGET_DIR=Join-Path $taskDir 'target'; $env:CARGO_NET_OFFLINE='true'; $env:CARGO_INCREMENTAL='0'; $env:CARGO_PROFILE_DEV_DEBUG='0'; $env:CARGO_PROFILE_TEST_DEBUG='0'; $env:CARGO_BUILD_JOBS='2'; $env:VCToolsInstallDir='<MSVC>/VC/Tools/MSVC/14.40.33807'; $env:WindowsSdkDir='<Windows-SDK>'
$taskBuild=[IO.File]::ReadAllText((Join-Path (Get-Location) 'build.ps1')); . ([ScriptBlock]::Create($taskBuild.Substring($taskBuild.IndexOf('$taskCandidates'),$taskBuild.IndexOf('switch ($Action)')-$taskBuild.IndexOf('$taskCandidates'))))
function Snapshot {
 $files=@(& git ls-files -- . | Where-Object {Test-Path -LiteralPath $_ -PathType Leaf} | ForEach-Object {[ordered]@{path=$_;sha256=(Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash}})
 return [ordered]@{head=(& git rev-parse HEAD);source_tree=(& git rev-parse 'HEAD:NoManCode');rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app');branch=(& git branch --show-current);status=@(& git status --short);index=@(& git diff --cached --name-status);unmerged=@(& git diff --name-only --diff-filter=U);files=$files}
}
$taskInitial=Snapshot
if($taskInitial.head -ne '7a8ba1d547b8e6c3bf20b420828117153d00caf9' -or $taskInitial.source_tree -ne '1184788b499f4993d3a69d07785c771260a58123' -or $taskInitial.rust_tree -ne '30d5e2d94ebd6256cb9aa805c0c4a0a444f63461' -or $taskInitial.status.Count -or $taskInitial.index.Count -or $taskInitial.unmerged.Count){throw 'candidate changed'}
$tasks=@(
 [ordered]@{name='check';args=@('check','--offline','--locked','--lib')},
 [ordered]@{name='test';args=@('test','--offline','--locked','--lib','agent_runtime_tests::','--','--nocapture')},
 [ordered]@{name='clippy';args=@('clippy','--offline','--locked','--all-targets','--','-D','warnings')},
 [ordered]@{name='fmt';args=@('fmt','--all','--','--check')}
)
foreach($task in $tasks){
 $name=$task.name; $log=Join-Path $taskDir "$name.log"; $meta=Join-Path $taskDir "$name.json"; if((Test-Path $log) -or (Test-Path $meta)){throw "evidence exists $name"}
 $before=Snapshot; if($before.head -ne $taskInitial.head -or $before.status.Count -or $before.index.Count -or $before.unmerged.Count){throw "candidate changed before $name"}
 $start=[DateTime]::UtcNow.ToString('o'); Write-Output "START $name $start"; $ErrorActionPreference='Continue'; & cargo @($task.args) *> $log; $code=$LASTEXITCODE; $ErrorActionPreference='Stop'; $end=[DateTime]::UtcNow.ToString('o'); $after=Snapshot
 [ordered]@{name=$name;command=('cargo '+($task.args -join ' '));cwd='NoManCode/rust-app';start_utc=$start;end_utc=$end;exit_code=$code;cargo=(& cargo --version);rustc=(& rustc --version);offline=$true;incremental=0;dev_debug=0;test_debug=0;build_jobs=2;target='.local/next17-d-component/target';tmp='.local/next17-d-component/tmp';msvc=$taskMsvc;sdk=$taskSdk.FullName;before=$before;after=$after;log_sha256=(Get-FileHash $log -Algorithm SHA256).Hash} | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $meta -Encoding utf8
 Write-Output "END $name exit=$code $end"; if($code -ne 0){Get-Content -Tail 60 $log; break}
}