[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$Head,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$ProductTree,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$RustTree,
    [Parameter(Mandatory)][ValidatePattern('^[a-z0-9][a-z0-9-]{0,79}$')][string]$Pair,
    [Alias('Profile')][ValidateSet('debug','release')][string]$BuildProfile = 'debug',
    [ValidateSet('x86_64-pc-windows-msvc')][string]$Target = 'x86_64-pc-windows-msvc',
    [string]$PackageRoot = '.local/worker-packages',
    [string]$EvidenceRoot = '.local/worker-package-evidence',
    [string]$TargetDir = '.local/lifecycle-target',
    [ValidateRange(1,7200)][int]$StepTimeoutSeconds = 1800,
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$taskArguments = @('-B', (Join-Path $PSScriptRoot 'worker_package.py'),
    '--head', $Head, '--product-tree', $ProductTree, '--rust-tree', $RustTree,
    '--pair', $Pair, '--profile', $BuildProfile, '--target', $Target,
    '--package-root', $PackageRoot, '--evidence-root', $EvidenceRoot,
    '--target-dir', $TargetDir, '--step-timeout-seconds', "$StepTimeoutSeconds")
& $Python @taskArguments
exit $LASTEXITCODE
