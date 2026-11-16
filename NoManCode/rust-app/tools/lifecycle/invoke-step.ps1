[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('check','debug-bins','targeted','test','fmt','clippy','wasm-check','lib-test','test-build','example-build')][string]$Stage,
    [ValidatePattern('^[a-zA-Z0-9_]+$')][string]$TestTarget = '',
    [ValidatePattern('^[a-zA-Z0-9_]+$')][string]$Example = '',
    [ValidatePattern('^[a-zA-Z0-9_:]+$')][string]$Filter = ''
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
# Internal child of the gate owner, not a public independent gate entry point.
if (-not $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Use run-gates.ps1 to acquire the global gate first' }
$taskGate = Get-Content -Raw -LiteralPath $env:NHC_LIFECYCLE_GATE_PATH | ConvertFrom-Json
if ($taskGate.nonce -ne $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Global gate owner mismatch' }
. (Join-Path $PSScriptRoot '../../build.ps1') -Action environment
Write-Output "MSVC=$env:CC_x86_64_pc_windows_msvc SDK=$taskSdkRoot version=$($taskSdk.Name)"
switch ($Stage) {
    'check' { & cargo check --workspace --all-targets --offline --locked }
    'debug-bins' { & cargo build --bins --offline --locked }
    'targeted' {
        if (-not $TestTarget) { throw 'TestTarget is required' }
        $taskArgs = @('test','--offline','--locked','--test',$TestTarget)
        if ($Filter) { $taskArgs += $Filter }
        $taskArgs += @('--','--test-threads=1')
        & cargo @taskArgs
    }
    'test' { & (Join-Path $PSScriptRoot '../../build.ps1') -Action test }
    'lib-test' {
        if (-not $Filter) { throw 'A lib filter is required' }
        & cargo test --offline --locked --lib $Filter -- --test-threads=1
    }
    'test-build' {
        if (-not $TestTarget) { throw 'TestTarget is required' }
        & cargo test --offline --locked --test $TestTarget --no-run
    }
    'example-build' {
        if (-not $Example) { throw 'Example is required' }
        & cargo build --offline --locked --example $Example
    }
    'fmt' { & cargo fmt --all -- --check }
    'clippy' { & cargo clippy --workspace --all-targets --offline --locked -- -D warnings }
    'wasm-check' { & cargo check -p peachsh-ui --target wasm32-unknown-unknown --offline --locked }
}
exit $LASTEXITCODE
