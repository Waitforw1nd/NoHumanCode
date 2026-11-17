$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$gate = Get-Content -Raw -LiteralPath $env:NHC_LIFECYCLE_GATE_PATH | ConvertFrom-Json
if (!$env:NHC_LIFECYCLE_GATE_NONCE -or $gate.nonce -ne $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Gate mismatch' }
if ($env:NHC_PACKAGED_WORKER_SHA256) { throw 'Baseline must be unbound' }
. ./build.ps1 -Action environment
& cargo build --bins --offline --locked --message-format=json-render-diagnostics
exit $LASTEXITCODE
