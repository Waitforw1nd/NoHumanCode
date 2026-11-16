[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('worker','host')][string]$Stage,
    [Parameter(Mandatory)][Alias('Profile')][ValidateSet('debug','release')][string]$BuildProfile,
    [Parameter(Mandatory)][ValidateSet('x86_64-pc-windows-msvc')][string]$Target
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if (-not $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Use package-worker.ps1 to own the global gate' }
$taskGate = Get-Content -Raw -LiteralPath $env:NHC_LIFECYCLE_GATE_PATH | ConvertFrom-Json
if ($taskGate.nonce -ne $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Global gate owner mismatch' }
if ($Stage -eq 'worker' -and $env:NHC_PACKAGED_WORKER_SHA256) { throw 'Worker build must not consume its own digest' }
if ($Stage -eq 'host' -and $env:NHC_PACKAGED_WORKER_SHA256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Missing Host compile-time worker digest' }
. (Join-Path $PSScriptRoot '../../build.ps1') -Action environment
$taskBin = if ($Stage -eq 'worker') { 'peachsh-wasm-worker' } else { 'peachsh' }
$taskArgs = @('build','--offline','--locked','--target',$Target,'--bin',$taskBin,'--message-format=json-render-diagnostics')
if ($BuildProfile -eq 'release') { $taskArgs += '--release' }
& cargo @taskArgs
exit $LASTEXITCODE
