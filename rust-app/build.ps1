param([ValidateSet('test','live','build','check','refresh','fmt','clippy','wasm-check')][string]$Action = 'build')
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
$taskCandidates = @('D:\vsstudio\VC\Tools\MSVC', 'C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC')
$taskMsvc = $null
foreach ($taskCandidate in $taskCandidates) {
    if (Test-Path -LiteralPath $taskCandidate) {
        foreach ($taskVersion in (Get-ChildItem -LiteralPath $taskCandidate -Directory | Sort-Object Name -Descending)) {
            foreach ($taskHost in @('Hostx64', 'Hostx86')) {
                $taskBin = Join-Path $taskVersion.FullName "bin\$taskHost\x64"
                if (Test-Path -LiteralPath (Join-Path $taskBin 'link.exe')) { $taskMsvc = $taskVersion.FullName; break }
            }
            if ($taskMsvc) { break }
        }
    }
    if ($taskMsvc) { break }
}
if (-not $taskMsvc) { throw 'Missing Visual C++ x64 build tools. Install the C++ build tools for Visual Studio.' }
$taskSdkRoot = 'C:\Program Files (x86)\Windows Kits\10'
$taskSdk = Get-ChildItem -LiteralPath (Join-Path $taskSdkRoot 'Lib') -Directory | Sort-Object Name -Descending | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'um\x64\kernel32.Lib') } | Select-Object -First 1
if (-not $taskSdk) { throw 'Missing Windows SDK x64 libraries.' }
$env:PATH = "$taskBin;$env:PATH"
$env:LIB = "$taskMsvc\lib\x64;$($taskSdk.FullName)\ucrt\x64;$($taskSdk.FullName)\um\x64"
$env:INCLUDE = "$taskMsvc\include;$taskSdkRoot\Include\$($taskSdk.Name)\ucrt;$taskSdkRoot\Include\$($taskSdk.Name)\shared;$taskSdkRoot\Include\$($taskSdk.Name)\um"
$env:CC_x86_64_pc_windows_msvc = Join-Path $taskBin 'cl.exe'
$env:AR_x86_64_pc_windows_msvc = Join-Path $taskBin 'lib.exe'
switch ($Action) {
    'build' { & cargo build --release --locked }
    'test' { & cargo test --workspace --locked }
    'live' { & cargo test --locked --test live -- --ignored --nocapture }
    'check' { & cargo check --workspace --locked }
    'refresh' { & cargo check --workspace }
    'fmt' { & cargo fmt --all -- --check }
    'clippy' { & cargo clippy --workspace --all-targets --locked -- -D warnings }
    'wasm-check' { & cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked }
}
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($Action -eq 'build') {
    $taskOutput = Join-Path (Split-Path -Parent $PSScriptRoot) 'bin'
    New-Item -ItemType Directory -Path $taskOutput -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\peachsh.exe') -Destination (Join-Path $taskOutput 'peachsh.exe') -Force
    Write-Output "Built: $taskOutput\peachsh.exe"
}
