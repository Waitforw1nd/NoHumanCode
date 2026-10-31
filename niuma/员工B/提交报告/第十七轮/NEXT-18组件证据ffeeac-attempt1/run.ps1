$ErrorActionPreference = 'Stop'
$taskRepo = (Resolve-Path (Join-Path $PSScriptRoot '../../../..')).Path
$taskWorkspace = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Set-Location -LiteralPath $taskWorkspace
$env:CARGO_TARGET_DIR = Join-Path $PSScriptRoot 'target'
$env:TEMP = Join-Path $PSScriptRoot 'tmp'
$env:TMP = $env:TEMP
$env:CARGO_NET_OFFLINE = 'true'
$env:RUSTUP_AUTO_INSTALL = '0'
$env:VCToolsInstallDir = '[msvc]\2022\Community\VC\Tools\MSVC\14.43.34808'
$env:WindowsSdkDir = '[windows-sdk]\10\'
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
if($state.head -ne 'ffeeac762643c168a8ec5e1570c7e1d63bede230' -or $state.product_tree -ne '7e429546b4abe26c3fc6d8543fc53f0093931790' -or $state.rust_tree -ne '831168a516069c2949bb627c662d86986ef868bf' -or $state.status.Count -or $state.staged.Count -or $state.unmerged.Count) { throw 'Candidate identity or cleanliness mismatch.' }
$heavy = @(Get-Process | Where-Object { $_.ProcessName -match '^(cargo|rustc|clippy-driver|peachsh)(\.exe)?$' } | Select-Object Id,ProcessName,Path)
if($heavy.Count) { $heavy | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/heavy-processes.json'); throw 'Heavy process present; no build started.' }
$state | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/preflight.json')
function Save-SourceManifest($name) {
  $files = @(& git -C $taskRepo ls-files -- NoManCode/rust-app)
  $out = foreach($file in $files) { $p=Join-Path $taskRepo $file; $h=Get-FileHash -LiteralPath $p -Algorithm SHA256; [pscustomobject]@{ path=$file; bytes=(Get-Item -LiteralPath $p).Length; sha256=$h.Hash } }
  $out | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$name.json")
}
Save-SourceManifest 'source-before'
$tasks = @(
 @{ name='01-check'; args=@('check','--offline','--locked','--lib') },
 @{ name='02-test'; args=@('test','--offline','--locked','--lib','--','--nocapture') },
 @{ name='03-clippy'; args=@('clippy','--offline','--locked','--lib','--','-D','warnings') },
 @{ name='04-fmt'; args=@('fmt','--all','--','--check') }
)
$failure = 0
foreach($task in $tasks) {
 $start=[DateTime]::UtcNow.ToString('o')
 & cargo @($task.args) *> (Join-Path $PSScriptRoot "logs/$($task.name).log")
 $code=$LASTEXITCODE
 @{ command=('cargo '+($task.args -join ' ')); started_utc=$start; ended_utc=[DateTime]::UtcNow.ToString('o'); exit_code=$code; head=(& git rev-parse HEAD); product_tree=(& git rev-parse 'HEAD:NoManCode'); rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app') } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$($task.name).json")
 Write-Output "$($task.name) exit=$code"
 if($code -ne 0) { $failure=$code; break }
}
Save-SourceManifest 'source-after'
@{ finished_utc=[DateTime]::UtcNow.ToString('o'); head=(& git rev-parse HEAD); product_tree=(& git rev-parse 'HEAD:NoManCode'); rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app'); status=@(& git status --short); staged=@(& git diff --cached --name-status); unmerged=@(& git ls-files --unmerged); exit_code=$failure } | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/postflight.json')
exit $failure
