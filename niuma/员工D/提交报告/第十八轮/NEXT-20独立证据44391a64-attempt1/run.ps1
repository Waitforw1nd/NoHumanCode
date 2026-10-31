$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$work = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Set-Location -LiteralPath $work
$repo = (& git rev-parse --show-toplevel).Trim()
$env:CARGO_TARGET_DIR = '../nhc-d-tool-result-tests\NoManCode\rust-app\.local\next19-d-3a1ea4f-attempt1\target'
$env:TEMP = (Resolve-Path (Join-Path $PSScriptRoot 'tmp')).Path
$env:TMP = $env:TEMP
$env:CARGO_NET_OFFLINE = 'true'
$env:RUSTUP_AUTO_INSTALL = '0'
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:VCToolsInstallDir = '<VS2022>\VC\Tools\MSVC\14.43.34808'
$env:WindowsSdkDir = '<WindowsSDK>\'
$setup = Get-Content -Raw -LiteralPath (Join-Path $work 'build.ps1')
$setup = $setup.Substring(0, $setup.IndexOf('switch ($Action)'))
$setup = $setup -replace "param\(\[ValidateSet\([^\r\n]*\r?\n", ''
$setup = $setup -replace 'Set-Location -LiteralPath \$PSScriptRoot', ''
Invoke-Expression $setup
function State($name) {
    @{at_utc=[DateTime]::UtcNow.ToString('o');head=(& git rev-parse HEAD);product_tree=(& git rev-parse 'HEAD:NoManCode');rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app');branch=(& git branch --show-current);status=@(& git status --short);staged=@(& git diff --cached --name-status);unmerged=@(& git ls-files --unmerged)} | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$name.json")
}
$head=(& git rev-parse HEAD).Trim(); $product=(& git rev-parse 'HEAD:NoManCode').Trim(); $rust=(& git rev-parse 'HEAD:NoManCode/rust-app').Trim(); $branch=(& git branch --show-current).Trim()
if ($head -ne '44391a64a7b5720546c0a2a23aab075971d7a794' -or $product -ne '2f2b70112b004654ce96b695e336ffbfd26569bc' -or $rust -ne '999c149dd7b10e6b60f25bcbc49d35c37ba81e79' -or $branch -ne 'codex/d/default-composition' -or @(& git status --short).Count -or @(& git diff --cached --name-only).Count -or @(& git ls-files --unmerged).Count) { throw 'identity or cleanliness mismatch' }
$heavy = @(Get-Process | Where-Object {$_.ProcessName -match '^(cargo|rustc|clippy-driver|peachsh|link|lld-link|msbuild|ninja|cmake)(-|\.exe|$)'})
if ($heavy.Count) { throw 'heavy process present' }
if (!(Test-Path -LiteralPath (Join-Path $work 'target') -PathType Container)) { throw 'legacy fixture target missing' }
& python (Join-Path $PSScriptRoot 'sources.py') 'source-before' | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/source-before-summary.txt')
if ($LASTEXITCODE -ne 0) { throw 'source preflight mismatch' }
State 'preflight'
@{cargo=(& cargo --version);rustc=(& rustc --version);target=$env:CARGO_TARGET_DIR;tmp=$env:TEMP;msvc=$taskMsvc;sdk=$taskSdk.FullName;jobs=2;incremental=0;dev_debug=0;test_debug=0;offline=$true;locked=$true;legacy_target_directory=$true} | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/environment.json')
$commands=@(
 @{name='01-check';args=@('check','--offline','--locked','--workspace','--all-targets')},
 @{name='02-build-cli';args=@('build','--offline','--locked','--bin','peachsh')},
 @{name='03-negative';args=@('test','--offline','--locked','--lib','process_fixture_failure_controls_reap_job','--','--nocapture')},
 @{name='04-lib';args=@('test','--offline','--locked','--lib','--','--nocapture')},
 @{name='05-integrations';args=@('test','--offline','--locked','--test','composition_http','--test','checkpoint','--test','workspace_changes','--test','project_http','--test','approval_http','--test','turn_http','--test','plugin_http','--test','runtime_management_workflow','--','--nocapture')},
 @{name='06-fmt';args=@('fmt','--all','--','--check')},
 @{name='07-clippy';args=@('clippy','--offline','--locked','--workspace','--all-targets','--','-D','warnings')}
)
$failure=0
try {
 foreach ($command in $commands) {
  $name=$command.name; $started=[DateTime]::UtcNow.ToString('o'); Write-Output "BEGIN $name $started"
  & cargo @($command.args) 1> (Join-Path $PSScriptRoot "logs/$name.stdout.log") 2> (Join-Path $PSScriptRoot "logs/$name.stderr.log")
  $code=$LASTEXITCODE; $ended=[DateTime]::UtcNow.ToString('o')
  @{command=('cargo '+($command.args -join ' '));started_utc=$started;ended_utc=$ended;exit_code=$code;head=(& git rev-parse HEAD);product_tree=(& git rev-parse 'HEAD:NoManCode');rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app')} | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$name.json")
  & python (Join-Path $PSScriptRoot 'sources.py') "source-after-$name" | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/source-summary-$name.txt")
  State "state-after-$name"
  Write-Output "END $name exit=$code $ended"
  if ($code -ne 0) { $failure=$code; break }
  if ($name -eq '02-build-cli') {
   $binary=Join-Path $env:CARGO_TARGET_DIR 'debug/peachsh.exe'
   if (!(Test-Path -LiteralPath $binary -PathType Leaf)) { $failure=1; Write-Output 'STOP CLI missing'; break }
   @{path=$binary;bytes=(Get-Item -LiteralPath $binary).Length;sha256=(Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash;checked_utc=[DateTime]::UtcNow.ToString('o')} | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/cli-binary.json')
  }
 }
} finally {
 & python (Join-Path $PSScriptRoot 'sources.py') 'source-final' | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/source-final-summary.txt')
 State 'postflight'
 & python (Join-Path $PSScriptRoot 'inventory.py') 'retained-after' | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/retained-after-summary.txt')
 $heavy=@(Get-Process | Where-Object {$_.ProcessName -match '^(cargo|rustc|clippy-driver|peachsh|link|lld-link|msbuild|ninja|cmake)(-|\.exe|$)'} | Select-Object Id,ProcessName,Path)
 @{finished_utc=[DateTime]::UtcNow.ToString('o');exit_code=$failure;heavy=$heavy} | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/final.json')
}
exit $failure





