$ErrorActionPreference='Stop'
$taskRoot=Split-Path -Parent $PSScriptRoot
$taskRust=Join-Path $taskRoot 'NoManCode/rust-app'
$taskLog=Join-Path $taskRoot '.local/logs/next18-final-attempt1'
if(Test-Path -LiteralPath $taskLog){throw 'Attempt already exists'}
New-Item -ItemType Directory -Path $taskLog -Force | Out-Null
$prior=(Get-Content -Raw -Encoding UTF8 '<PREVIOUS_C_WORKTREE>/.local/next17-c-components-attempt1/meta.json' | ConvertFrom-Json).environment
$env:RUSTUP_AUTO_INSTALL='0';$env:CARGO_NET_OFFLINE='true'
$env:TMP=Join-Path $taskRoot '.local/tmp-next18-final-attempt1';$env:TEMP=$env:TMP
$env:CARGO_TARGET_DIR=Join-Path $taskRoot '.local/target'
$env:CARGO_PROFILE_DEV_DEBUG='0';$env:CARGO_PROFILE_TEST_DEBUG='0';$env:CARGO_INCREMENTAL='0';$env:CARGO_BUILD_JOBS='2'
$bin=Split-Path -Parent $prior.compiler
if(-not (Test-Path -LiteralPath $prior.compiler -PathType Leaf)){throw 'Existing compiler missing'}
$env:PATH="$bin;$env:PATH"
$env:LIB="$($prior.msvc)\lib\x64;$($prior.sdk)\Lib\$($prior.sdk_version)\ucrt\x64;$($prior.sdk)\Lib\$($prior.sdk_version)\um\x64"
$env:INCLUDE="$($prior.msvc)\include;$($prior.sdk)\Include\$($prior.sdk_version)\ucrt;$($prior.sdk)\Include\$($prior.sdk_version)\shared;$($prior.sdk)\Include\$($prior.sdk_version)\um"
$env:CC_x86_64_pc_windows_msvc=$prior.compiler;$env:AR_x86_64_pc_windows_msvc=Join-Path $bin 'lib.exe'
New-Item -ItemType Directory -Force -Path $env:TMP,$env:CARGO_TARGET_DIR,(Join-Path $taskRust 'target') | Out-Null
Set-Location -LiteralPath $taskRoot
function Snapshot($label) {
 $paths=@(& git -c core.quotepath=false ls-files -- NoManCode)
 if($LASTEXITCODE -ne 0 -or $paths.Count -eq 0){throw 'Cannot enumerate source'}
 $hashes=@(foreach($path in $paths){$p=Join-Path $taskRoot $path;[ordered]@{path=$path;bytes=(Get-Item -LiteralPath $p).Length;sha256=(Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash}})
 $hashPath=Join-Path $taskLog ($label+'-source.json');$hashes | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 -LiteralPath $hashPath
 $snap=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');head=(& git rev-parse HEAD).Trim();tree=(& git rev-parse HEAD:NoManCode).Trim();rust_tree=(& git rev-parse HEAD:NoManCode/rust-app).Trim();branch=(& git branch --show-current).Trim();status=@(& git status --short);index=@(& git diff --cached --name-only);unmerged=@(& git ls-files -u);count=$hashes.Count;source_hash=(Get-FileHash -LiteralPath $hashPath).Hash;manifest=[IO.Path]::GetFileName($hashPath)}
 $snap.valid=($snap.head -eq '7fce5b2e187a1147f4fcc784b5d07580eb6afe2c' -and $snap.tree -eq 'da5787dfff470eda857fff8c7934432ba5fc8cc3' -and $snap.rust_tree -eq 'b5846dcd4c4a8584d2b36fe0a2c95e688c2fbf83' -and $snap.branch -eq 'codex/c/runtime-management-interface' -and $snap.status.Count -eq 0 -and $snap.index.Count -eq 0 -and $snap.unmerged.Count -eq 0)
 return $snap
}
$commands=[ordered]@{build='cargo build --offline --locked --bin peachsh';check='cargo check --offline --locked --workspace --all-targets';test='cargo test --offline --locked --workspace -- --nocapture';fmt='cargo fmt --all -- --check';clippy='cargo clippy --offline --locked --workspace --all-targets -- -D warnings';wasm='cargo check --offline --locked -p peachsh-ui --target wasm32-unknown-unknown'}
$meta=[ordered]@{execution_id='C-R19-01-20261031-E01';cwd=$taskRust;start_utc=[DateTime]::UtcNow.ToString('o');status='preparing';steps=@();not_executed=@($commands.Keys);environment=[ordered]@{RUSTUP_AUTO_INSTALL=$env:RUSTUP_AUTO_INSTALL;CARGO_NET_OFFLINE=$env:CARGO_NET_OFFLINE;TMP=$env:TMP;TEMP=$env:TEMP;CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR;CARGO_PROFILE_DEV_DEBUG=$env:CARGO_PROFILE_DEV_DEBUG;CARGO_PROFILE_TEST_DEBUG=$env:CARGO_PROFILE_TEST_DEBUG;CARGO_INCREMENTAL=$env:CARGO_INCREMENTAL;CARGO_BUILD_JOBS=$env:CARGO_BUILD_JOBS;msvc=$prior.msvc;sdk=$prior.sdk;sdk_version=$prior.sdk_version;compiler=$prior.compiler};rustc=(& rustc --version);cargo=(& cargo --version);driver_sha256=(Get-FileHash -LiteralPath $PSCommandPath).Hash}
function Save-Meta { $meta | ConvertTo-Json -Depth 12 | Set-Content -Encoding utf8 -LiteralPath (Join-Path $taskLog 'meta.json') }
Save-Meta
foreach($action in $commands.Keys){
 $before=Snapshot ($action+'-before');if(-not $before.valid){$meta.status='invariant_failed';Save-Meta;exit 97}
 $start=[DateTime]::UtcNow.ToString('o');$meta.status='running_'+$action;Save-Meta;Write-Output ('START '+$action+' '+$start)
 Set-Location -LiteralPath $taskRust
 switch($action){
  'build' { & cargo build --offline --locked --bin peachsh *> (Join-Path $taskLog 'build.log') }
  'check' { & cargo check --offline --locked --workspace --all-targets *> (Join-Path $taskLog 'check.log') }
  'test' { & cargo test --offline --locked --workspace -- --nocapture *> (Join-Path $taskLog 'test.log') }
  'fmt' { & cargo fmt --all -- --check *> (Join-Path $taskLog 'fmt.log') }
  'clippy' { & cargo clippy --offline --locked --workspace --all-targets -- -D warnings *> (Join-Path $taskLog 'clippy.log') }
  'wasm' { & cargo check --offline --locked -p peachsh-ui --target wasm32-unknown-unknown *> (Join-Path $taskLog 'wasm.log') }
 }
 $code=$LASTEXITCODE;$end=[DateTime]::UtcNow.ToString('o');Set-Location -LiteralPath $taskRoot;$after=Snapshot ($action+'-after')
 $same=($before.source_hash -eq $after.source_hash);$extra=[ordered]@{}
 if($action -eq 'build' -and $code -eq 0){$cli=Join-Path $env:CARGO_TARGET_DIR 'debug/peachsh.exe';if(-not (Test-Path -LiteralPath $cli -PathType Leaf)){$code=98}else{$extra.cli_path=$cli;$extra.cli_bytes=(Get-Item -LiteralPath $cli).Length;$extra.cli_sha256=(Get-FileHash -LiteralPath $cli).Hash}}
 $meta.steps+=[ordered]@{action=$action;command=$commands[$action];start_utc=$start;end_utc=$end;exit_code=$code;before=$before;after=$after;source_unchanged=$same;log=$action+'.log';extra=$extra}
 $meta.not_executed=@($commands.Keys | Where-Object {$_ -notin @($meta.steps | ForEach-Object {$_.action})});$meta.status=if($code -eq 0 -and $after.valid -and $same){'completed_'+$action}else{'failed_'+$action};Save-Meta
 Write-Output ('END '+$action+' exit='+$code+' unchanged='+$same)
 if($code -ne 0 -or -not $after.valid -or -not $same){Get-Content -Tail 100 (Join-Path $taskLog ($action+'.log'));if($code -ne 0){exit $code}else{exit 97}}
}
$meta.status='all_five_gates_passed';$meta.end_utc=[DateTime]::UtcNow.ToString('o');Save-Meta
