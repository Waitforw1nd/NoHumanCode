param([Parameter(Mandatory)][ValidateSet('bins','lib')][string]$Stage)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if (-not $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Use the supervised W2 runner' }
$taskGate = Get-Content -Raw -LiteralPath $env:NHC_LIFECYCLE_GATE_PATH | ConvertFrom-Json
if ($taskGate.nonce -ne $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Global gate owner mismatch' }
. (Join-Path $PSScriptRoot '../../build.ps1') -Action environment
if ($Stage -eq 'bins') {
    & cargo build --offline --locked --target x86_64-pc-windows-msvc --bin peachsh-wasm-worker --example wasm_worker_process_probe --message-format=json-render-diagnostics
} else {
    & cargo test --offline --locked --target x86_64-pc-windows-msvc --lib --no-run --message-format=json-render-diagnostics
}
exit $LASTEXITCODE
