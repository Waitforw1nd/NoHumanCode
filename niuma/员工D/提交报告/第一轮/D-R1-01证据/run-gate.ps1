# D-R1-01 gate runner — fixed source snapshot at ./rust-app, private target dir.
# One pwsh process: establishes the MSVC toolchain once (lines extracted
# verbatim from the frozen copy's build.ps1 env block), then runs each step
# directly so no step can leak or reset the environment for the next.
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$rustApp = Join-Path $root 'rust-app'
$logDir = Join-Path $root 'logs'
New-Item -ItemType Directory -Path $logDir -Force | Out-Null
$env:CARGO_TARGET_DIR = 'D:\code\fufu\.local\targets\d-plugin-catalog'
$env:CARGO_TERM_COLOR = 'never'
Set-Location -LiteralPath $rustApp

# tests/runtime.rs spawns `pwsh.exe` by name (workspace run_command). This
# machine has no standalone PowerShell 7 install; the only pwsh.exe is the
# copy embedded in Git for Windows, whose directory is not on PATH. Put it on
# PATH for the gate process so the pre-existing test can resolve the shell.
if (-not (Get-Command pwsh.exe -ErrorAction SilentlyContinue)) {
    $gitPwsh = 'C:\Program Files\Git'
    if (Test-Path -LiteralPath (Join-Path $gitPwsh 'pwsh.exe')) {
        $env:PATH = "$gitPwsh;$env:PATH"
    }
}

# The VS2022 instance on this machine is not registered with vswhere; point
# the standard discovery inputs at the on-disk toolchain (Hostx86-hosted x64
# tools) so the extracted build.ps1 env block resolves it normally.
if (-not $env:VCToolsInstallDir) {
    $msvcRoot = Get-ChildItem 'C:\Program Files\Microsoft Visual Studio\*\*\VC\Tools\MSVC\*' -Directory -ErrorAction SilentlyContinue |
        Where-Object {
            (Test-Path -LiteralPath (Join-Path $_.FullName 'bin\Hostx64\x64\link.exe')) -or
            (Test-Path -LiteralPath (Join-Path $_.FullName 'bin\Hostx86\x64\link.exe'))
        } |
        Sort-Object { $_.Name -as [version] } -Descending | Select-Object -First 1
    if ($msvcRoot) { $env:VCToolsInstallDir = $msvcRoot.FullName + '\' }
}
if (-not $env:WindowsSdkDir) {
    $sdkRoot = 'C:\Program Files (x86)\Windows Kits\10'
    if (Test-Path -LiteralPath (Join-Path $sdkRoot 'Lib')) { $env:WindowsSdkDir = $sdkRoot + '\' }
}

$buildLines = Get-Content -LiteralPath (Join-Path $rustApp 'build.ps1')
$envScript = Join-Path $root 'msvc-env.ps1'
Set-Content -LiteralPath $envScript -Value ($buildLines[3..72])
. $envScript

# This VS instance lacks lib\x64\msvcrt.lib; the OneCore variant is the only
# import lib on disk and satisfies the linker for build-script/test exes.
if ($env:VCToolsInstallDir) {
    $onecore = Join-Path $env:VCToolsInstallDir 'lib\onecore\x64'
    if ((Test-Path -LiteralPath $onecore) -and ($env:LIB -notlike "*onecore*")) {
        $env:LIB = "$env:LIB;$onecore"
    }
}

$results = [System.Collections.Generic.List[object]]::new()
function Invoke-Step([string]$name, [scriptblock]$cmd, [string]$display) {
    $log = Join-Path $logDir "$name.log"
    $started = Get-Date
    Write-Output "== $name : $display"
    & $cmd 2>&1 | Out-File -LiteralPath $log -Encoding utf8
    $code = $LASTEXITCODE
    $seconds = [int]((Get-Date) - $started).TotalSeconds
    $script:results.Add([pscustomobject]@{
        step = $name; command = $display; exitCode = $code; seconds = $seconds
    })
    Write-Output "== $name exit=$code (${seconds}s)"
}

Invoke-Step 'check' { cargo check --workspace --locked } 'cargo check --workspace --locked'
Invoke-Step 'test-integration' { cargo test -p peachsh --locked --test plugin_catalog } 'cargo test -p peachsh --locked --test plugin_catalog'
Invoke-Step 'test-unit' { cargo test -p peachsh --locked --lib plugin_catalog:: } 'cargo test -p peachsh --locked --lib plugin_catalog::'
Invoke-Step 'clippy-module' { cargo clippy -p peachsh --lib --test plugin_catalog --locked -- -D warnings } 'cargo clippy -p peachsh --lib --test plugin_catalog --locked -- -D warnings'
Invoke-Step 'gate-test' { cargo test --workspace --locked } 'cargo test --workspace --locked'
Invoke-Step 'gate-fmt' { cargo fmt --all -- --check } 'cargo fmt --all -- --check'
Invoke-Step 'gate-clippy' { cargo clippy --workspace --all-targets --locked -- -D warnings } 'cargo clippy --workspace --all-targets --locked -- -D warnings'
Invoke-Step 'gate-wasm-check' { cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked } 'cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked'

$results | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root 'gate-results.json')
Write-Output '== gate complete'
