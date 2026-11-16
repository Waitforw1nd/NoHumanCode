param()
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if (-not $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Use run-bootstrap-check.py to own the gate' }
$taskGate = Get-Content -Raw -LiteralPath $env:NHC_LIFECYCLE_GATE_PATH | ConvertFrom-Json
if ($taskGate.nonce -ne $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Global gate owner mismatch' }
. (Join-Path $PSScriptRoot '../../build.ps1') -Action environment
& cargo test --offline --locked --test worker_package_bootstrap -- real_pair_host_check_rejects_tampering_and_mix --exact --ignored --nocapture --test-threads=1
exit $LASTEXITCODE
