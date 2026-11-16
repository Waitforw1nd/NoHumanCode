# No Cargo is started until the Python runner owns the shared repository gate.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$Head,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$ProductTree,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$RustTree,
    [Parameter(Mandatory)][ValidatePattern('^[a-z0-9][a-z0-9-]{0,79}$')][string]$Attempt,
    [ValidateSet('check','targeted','full','lib','test-build','example-build')][string]$Mode = 'full',
    [ValidatePattern('^[a-zA-Z0-9_]+$')][string[]]$TestTarget = @(),
    [ValidatePattern('^[a-zA-Z0-9_]+$')][string[]]$Example = @(),
    [ValidatePattern('^[a-zA-Z0-9_:]+$')][string]$Filter = '',
    [string]$EvidenceRoot = '.local/lifecycle',
    [string]$TargetDir = '.local/lifecycle-target',
    [ValidateRange(1,7200)][int]$StepTimeoutSeconds = 1800,
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
# Discovery is performed by invoke-step.ps1 only after the runner owns the gate.
$taskArguments = @('-B', (Join-Path $PSScriptRoot 'lifecycle_gates.py'),
    '--head', $Head, '--product-tree', $ProductTree, '--rust-tree', $RustTree,
    '--attempt', $Attempt, '--mode', $Mode, '--evidence-root', $EvidenceRoot,
    '--target-dir', $TargetDir, '--step-timeout-seconds', "$StepTimeoutSeconds")
foreach ($taskTarget in $TestTarget) { $taskArguments += @('--test-target', $taskTarget) }
foreach ($taskExample in $Example) { $taskArguments += @('--example', $taskExample) }
if ($Filter) { $taskArguments += @('--filter', $Filter) }
& $Python @taskArguments
exit $LASTEXITCODE
