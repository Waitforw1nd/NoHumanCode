$ErrorActionPreference = 'Stop'
$taskWorkspace = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Set-Location -LiteralPath $taskWorkspace
$env:CARGO_TARGET_DIR = Join-Path $PSScriptRoot 'target'
$env:TEMP = Join-Path $PSScriptRoot 'tmp'
$env:TMP = $env:TEMP
$env:CARGO_NET_OFFLINE = 'true'
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$setup = Get-Content -Raw (Join-Path $taskWorkspace 'build.ps1')
$setup = $setup.Substring(0, $setup.IndexOf('switch ($Action)'))
$setup = $setup -replace "param\(\[ValidateSet\([^\r\n]*\r?\n", ''
$setup = $setup -replace 'Set-Location -LiteralPath \$PSScriptRoot', ''
Invoke-Expression $setup
$tasks = @(
  @{ name='01-check'; args=@('check','--offline','--locked','--lib') },
  @{ name='02-provider-lib'; args=@('test','--offline','--locked','--lib','provider::','--','--nocapture') },
  @{ name='03-provider-integration'; args=@('test','--offline','--locked','--test','provider_network','--test','provider_usage','--','--nocapture') },
  @{ name='04-clippy'; args=@('clippy','--offline','--locked','--lib','--','-D','warnings') },
  @{ name='05-fmt'; args=@('fmt','--all','--','--check') }
)
foreach ($task in $tasks) {
  $started = (Get-Date).ToString('o')
  $log = Join-Path $PSScriptRoot "logs/$($task.name).log"
  & cargo @($task.args) *> $log
  $code = $LASTEXITCODE
  @{ name=$task.name; command=('cargo ' + ($task.args -join ' ')); started=$started; ended=(Get-Date).ToString('o'); exit_code=$code; head=(& git rev-parse HEAD); product_tree=(& git rev-parse 'HEAD:NoManCode'); rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app') } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$($task.name).json")
  Write-Output "$($task.name) exit=$code"
  if ($code -ne 0) { exit $code }
}
