param([Parameter(Mandatory)][ValidateSet('lib','probe')][string]$Stage)
$ErrorActionPreference='Stop'
$PSNativeCommandUseErrorActionPreference=$false
$taskRepo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$taskRust=Join-Path $taskRepo 'NoManCode/rust-app'
if (-not $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Missing manager gate' }
$taskGate=Get-Content -LiteralPath $env:NHC_LIFECYCLE_GATE_PATH -Raw | ConvertFrom-Json
if ($taskGate.nonce -ne $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Gate mismatch' }
. (Join-Path $taskRust 'build.ps1') -Action environment
if($Stage -eq 'lib') {
    & cargo test --offline --locked --lib --no-run --message-format=json
} else {
    & cargo build --offline --locked --example process_lifetime_probe --message-format=json
}
exit $LASTEXITCODE
