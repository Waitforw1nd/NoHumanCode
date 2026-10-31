$ErrorActionPreference='Stop'
$taskRoot=Split-Path -Parent $PSScriptRoot
$taskRust=Join-Path $taskRoot 'NoManCode/rust-app'
$taskLog=Join-Path $taskRoot '.local/next17-c-components-attempt1'
$taskPrior=(Get-Content -Raw '../nhc-c-first-user-workflow/.local/first-user-component-attempt2/meta.json' | ConvertFrom-Json).env
$env:RUSTUP_AUTO_INSTALL='0'; $env:CARGO_NET_OFFLINE='true'
$env:TMP=Join-Path $taskRoot '.local/tmp'; $env:TEMP=$env:TMP
$env:CARGO_TARGET_DIR=Join-Path $taskRoot '.local/target'
$env:CARGO_PROFILE_DEV_DEBUG='0'; $env:CARGO_PROFILE_TEST_DEBUG='0'; $env:CARGO_INCREMENTAL='0'; $env:CARGO_BUILD_JOBS='2'
$taskMsvc=$taskPrior.msvc; $taskSdk=$taskPrior.sdk; $taskSdkVersion=$taskPrior.sdk_version
$taskBin=Split-Path -Parent $taskPrior.compiler
if(-not (Test-Path -LiteralPath $taskPrior.compiler -PathType Leaf)){throw 'existing compiler missing'}
$env:PATH="$taskBin;$env:PATH"
$env:LIB="$taskMsvc\lib\x64;$taskSdk\Lib\$taskSdkVersion\ucrt\x64;$taskSdk\Lib\$taskSdkVersion\um\x64"
$env:INCLUDE="$taskMsvc\include;$taskSdk\Include\$taskSdkVersion\ucrt;$taskSdk\Include\$taskSdkVersion\shared;$taskSdk\Include\$taskSdkVersion\um"
$env:CC_x86_64_pc_windows_msvc=$taskPrior.compiler; $env:AR_x86_64_pc_windows_msvc=Join-Path $taskBin 'lib.exe'
New-Item -ItemType Directory -Force -Path $env:TMP,$env:CARGO_TARGET_DIR,(Join-Path $taskRust 'target') | Out-Null
Set-Location -LiteralPath $taskRoot
function Snapshot($label) {
 $paths=@(& git ls-files -- NoManCode)
 $hashes=@(foreach($path in $paths){$p=Join-Path $taskRoot $path; [ordered]@{path=$path;bytes=(Get-Item -LiteralPath $p).Length;sha256=(Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash}})
 $hashPath=Join-Path $taskLog ($label+'-source.json'); $hashes | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 -LiteralPath $hashPath
 $snapshot=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');head=(& git rev-parse HEAD).Trim();tree=(& git rev-parse HEAD:NoManCode).Trim();rust_tree=(& git rev-parse HEAD:NoManCode/rust-app).Trim();branch=(& git branch --show-current).Trim();status=@(& git status --short);index=@(& git diff --cached --name-only);unmerged=@(& git ls-files -u);count=$hashes.Count;source_hash=(Get-FileHash $hashPath).Hash}
 $snapshot.valid=($snapshot.head -eq '9b1fa771ec2595ef6a6829caf808d91f2ad6211c' -and $snapshot.tree -eq 'ad614815300cd122b360fa6ff56c70abfa22e878' -and $snapshot.rust_tree -eq 'fe9b4681d2778c1a3a6b57de6fb81f677db465a3' -and $snapshot.branch -eq 'codex/c/agent-runtime-interface' -and $snapshot.status.Count -eq 0 -and $snapshot.index.Count -eq 0 -and $snapshot.unmerged.Count -eq 0)
 return $snapshot
}
$commands=[ordered]@{check='cargo check --offline --locked --lib';lib='cargo test --offline --locked --lib agent_runtime_ -- --nocapture';cli='cargo test --offline --locked --test agent_runtime_cli -- --nocapture';fmt='cargo fmt --all -- --check'}
$meta=[ordered]@{execution_id='C-R18-01-20261031-E01';cwd=$taskRust;start_utc=[DateTime]::UtcNow.ToString('o');status='preparing';steps=@();not_executed=@($commands.Keys);environment=[ordered]@{RUSTUP_AUTO_INSTALL=$env:RUSTUP_AUTO_INSTALL;CARGO_NET_OFFLINE=$env:CARGO_NET_OFFLINE;TMP=$env:TMP;TEMP=$env:TEMP;CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR;CARGO_PROFILE_DEV_DEBUG=$env:CARGO_PROFILE_DEV_DEBUG;CARGO_PROFILE_TEST_DEBUG=$env:CARGO_PROFILE_TEST_DEBUG;CARGO_INCREMENTAL=$env:CARGO_INCREMENTAL;CARGO_BUILD_JOBS=$env:CARGO_BUILD_JOBS;msvc=$taskMsvc;sdk=$taskSdk;sdk_version=$taskSdkVersion;compiler=$taskPrior.compiler};rustc=(& rustc --version);cargo=(& cargo --version)}
function Save-Meta { $meta | ConvertTo-Json -Depth 10 | Set-Content -Encoding utf8 (Join-Path $taskLog 'meta.json') }
Save-Meta
foreach($action in $commands.Keys){
 $before=Snapshot ($action+'-before'); if(-not $before.valid){throw 'candidate invariant failed'}
 $start=[DateTime]::UtcNow.ToString('o');$meta.status='running_'+$action;Save-Meta;Write-Output ('START '+$action+' '+$start)
 Set-Location -LiteralPath $taskRust
 switch($action){
  'check' { & cargo check --offline --locked --lib *> (Join-Path $taskLog 'check.log') }
  'lib' { & cargo test --offline --locked --lib agent_runtime_ -- --nocapture *> (Join-Path $taskLog 'lib.log') }
  'cli' { & cargo test --offline --locked --test agent_runtime_cli -- --nocapture *> (Join-Path $taskLog 'cli.log') }
  'fmt' { & cargo fmt --all -- --check *> (Join-Path $taskLog 'fmt.log') }
 }
 $code=$LASTEXITCODE; $end=[DateTime]::UtcNow.ToString('o');Set-Location -LiteralPath $taskRoot;$after=Snapshot ($action+'-after')
 $same=($before.source_hash -eq $after.source_hash)
 $meta.steps+=[ordered]@{action=$action;command=$commands[$action];start_utc=$start;end_utc=$end;exit_code=$code;before=$before;after=$after;source_unchanged=$same;log=$action+'.log'}
 $meta.not_executed=@($commands.Keys | Where-Object {$_ -notin @($meta.steps | ForEach-Object {$_.action})})
 $meta.status=if($code -eq 0 -and $after.valid -and $same){'completed_'+$action}else{'failed_'+$action};Save-Meta
 Write-Output ('END '+$action+' exit='+$code+' unchanged='+$same)
 if($code -ne 0 -or -not $after.valid -or -not $same){Get-Content -Tail 100 (Join-Path $taskLog ($action+'.log'));if($code -ne 0){exit $code}else{exit 97}}
}
$meta.status='all_components_passed';$meta.end_utc=[DateTime]::UtcNow.ToString('o');Save-Meta
