param([ValidateSet('check','test','fmt','clippy','wasm-check','approval','approval-lib','approval-clippy','regression')][string]$Action)
$ErrorActionPreference = 'Continue'
$root = (Resolve-Path (Join-Path $PSScriptRoot '../../../../..')).Path
$source = (Resolve-Path (Join-Path $root '../nhc-b-approval/NoManCode/rust-app')).Path
Set-Location -LiteralPath $source
$started = [DateTimeOffset]::Now.ToString('o')
$lines = @()
if ($Action -in @('check','test','fmt','clippy','wasm-check')) {
    $lines = @(& ./build.ps1 -Action $Action 2>&1)
    $code = $LASTEXITCODE
} else {
    # build.ps1 initializes compiler variables in this process. Log this check
    # as part of the supplemental invocation, not as a separate passed gate.
    $lines = @(& ./build.ps1 -Action check 2>&1)
    $code = $LASTEXITCODE
    if ($code -eq 0) {
        switch ($Action) {
            'approval' { $lines += @(& cargo test -p peachsh --locked --test approval_gate 2>&1) }
            'approval-lib' { $lines += @(& cargo test -p peachsh --locked --lib approval:: 2>&1) }
            'approval-clippy' { $lines += @(& cargo clippy -p peachsh --lib --test approval_gate --locked -- -D warnings 2>&1) }
            'regression' { $lines += @(& cargo test --workspace --locked --no-fail-fast 2>&1) }
        }
        $code = $LASTEXITCODE
    }
}
$safe = @($lines | ForEach-Object {
    $_.ToString().Replace($source, '../nhc-b-approval/NoManCode/rust-app').Replace($root, '.').Replace($env:USERPROFILE, './.local/user-profile')
})
foreach ($extension in @('log','json')) {
    $existing = Join-Path $PSScriptRoot "$Action.$extension"
    if (Test-Path -LiteralPath $existing) {
        $stamp = [DateTimeOffset]::Now.ToString('yyyyMMdd-HHmmssfff')
        Copy-Item -LiteralPath $existing -Destination (Join-Path $PSScriptRoot "$Action.previous-$stamp.$extension")
    }
}
$safe | Set-Content -LiteralPath (Join-Path $PSScriptRoot "$Action.log") -Encoding utf8
[pscustomobject]@{ action=$Action; started=$started; finished=[DateTimeOffset]::Now.ToString('o'); exit_code=$code; powershell=$PSVersionTable.PSVersion.ToString() } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $PSScriptRoot "$Action.json") -Encoding utf8
$safe | Select-Object -Last 100
Write-Output "ACTION=$Action EXIT=$code"
exit $code
