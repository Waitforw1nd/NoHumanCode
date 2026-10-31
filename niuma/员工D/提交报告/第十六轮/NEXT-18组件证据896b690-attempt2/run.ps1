$ErrorActionPreference = 'Stop'
$taskRepo = (Resolve-Path (Join-Path $PSScriptRoot '../../../..')).Path
$taskWorkspace = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Set-Location -LiteralPath $taskWorkspace
$env:CARGO_TARGET_DIR = (Resolve-Path (Join-Path $PSScriptRoot '../next18-d-67841d-attempt1/target')).Path
$env:TEMP = Join-Path $PSScriptRoot 'tmp'
$env:TMP = $env:TEMP
$env:CARGO_NET_OFFLINE = 'true'
$env:RUSTUP_AUTO_INSTALL = '0'
$env:VCToolsInstallDir = '[msvc]'
$env:WindowsSdkDir = '[windows-sdk]\'
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$setup = Get-Content -Raw -LiteralPath (Join-Path $taskWorkspace 'build.ps1')
$setup = $setup.Substring(0, $setup.IndexOf('switch ($Action)'))
$setup = $setup -replace "param\(\[ValidateSet\([^\r\n]*\r?\n", ''
$setup = $setup -replace 'Set-Location -LiteralPath \$PSScriptRoot', ''
Invoke-Expression $setup
$state = @{ started_utc=[DateTime]::UtcNow.ToString('o'); head=(& git rev-parse HEAD); product_tree=(& git rev-parse 'HEAD:NoManCode'); rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app'); branch=(& git branch --show-current); status=@(& git status --short); staged=@(& git diff --cached --name-status); unmerged=@(& git ls-files --unmerged); cargo=(& cargo --version); rustc=(& rustc --version); target=$env:CARGO_TARGET_DIR; temp=$env:TEMP; msvc=$taskMsvc; sdk=$taskSdk.FullName; jobs=2; incremental=0; dev_debug=0; test_debug=0; offline=$true; locked=$true }
if($state.head -ne '896b690938cb31946acb2c4030de76f58eb13d43' -or $state.product_tree -ne 'f687e5f5babcded3585cf56fabc216976434c4f0' -or $state.rust_tree -ne '5e1282deb5df607959c620c25ce440d4cf26e7fd' -or $state.status.Count -or $state.staged.Count -or $state.unmerged.Count) { throw 'Candidate identity or cleanliness mismatch.' }
$heavy = @(Get-Process | Where-Object { $_.ProcessName -match '^(cargo|rustc|clippy-driver|peachsh)(-|\.exe|$)' } | Select-Object Id,ProcessName,Path)
if($heavy.Count) { $heavy | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/heavy-processes.json'); throw 'Heavy process present; no build started.' }
$state | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/preflight.json')
function Save-SourceManifest($name) {
 $files = @(& git -C $taskRepo ls-files -- NoManCode/rust-app)
 $out = foreach($file in $files) { $p=Join-Path $taskRepo $file; $h=Get-FileHash -LiteralPath $p -Algorithm SHA256; [pscustomobject]@{ path=$file; bytes=(Get-Item -LiteralPath $p).Length; sha256=$h.Hash } }
 $out | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$name.json")
}
$tasks = @(
 @{ name='01-check'; args=@('check','--offline','--locked','--all-targets') },
 @{ name='02-build-cli'; args=@('build','--offline','--locked','--bin','peachsh') },
 @{ name='03-lib'; args=@('test','--offline','--locked','--lib','--','--nocapture') },
 @{ name='04-schema'; args=@('test','--offline','--locked','--test','runtime_management_schema','--','--nocapture') },
 @{ name='05-historical'; args=@('test','--offline','--locked','--test','approval_gate','--test','checkpoint','--test','project_migration','--test','workspace_changes','--test','host_configuration','--test','host_configuration_schema','--test','project_context','--','--nocapture') },
 @{ name='06-clippy'; args=@('clippy','--offline','--locked','--all-targets','--','-D','warnings') },
 @{ name='07-fmt'; args=@('fmt','--all','--','--check') }
)
$failure = 0
foreach($task in $tasks) {
 Save-SourceManifest "$($task.name)-before"
 $start=[DateTime]::UtcNow.ToString('o')
 & cargo @($task.args) *> (Join-Path $PSScriptRoot "logs/$($task.name).log")
 $code=$LASTEXITCODE
 $ended=[DateTime]::UtcNow.ToString('o')
 Save-SourceManifest "$($task.name)-after"
 @{ command=('cargo '+($task.args -join ' ')); started_utc=$start; ended_utc=$ended; exit_code=$code; head=(& git rev-parse HEAD); product_tree=(& git rev-parse 'HEAD:NoManCode'); rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app') } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$($task.name).json")
 Write-Output "$($task.name) exit=$code"
 if($code -ne 0) { $failure=$code; break }
 if($task.name -eq '02-build-cli') {
  $binary = Join-Path $env:CARGO_TARGET_DIR 'debug/peachsh.exe'
  $present = Test-Path -LiteralPath $binary -PathType Leaf
  $details = @{ present=$present; checked_utc=[DateTime]::UtcNow.ToString('o'); path=$binary }
  if($present) { $details.bytes=(Get-Item -LiteralPath $binary).Length; $details.sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash }
  $details | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/cli-binary.json')
  if(!$present) { $failure=1; Write-Output 'CLI binary missing after build; stopping'; break }
 }
}
@{ finished_utc=[DateTime]::UtcNow.ToString('o'); head=(& git rev-parse HEAD); product_tree=(& git rev-parse 'HEAD:NoManCode'); rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app'); status=@(& git status --short); staged=@(& git diff --cached --name-status); unmerged=@(& git ls-files --unmerged); exit_code=$failure } | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/postflight.json')
exit $failure

