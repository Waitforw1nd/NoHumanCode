# D-R2-01 gate runner — runs inside the independent worktree
# ../nhc-d-plugin-host (branch codex/d/plugin-host, baseline b67fedf).
#
# Env strategy (records the contract's build.ps1 -Action check step honestly):
# PS 5.1 turns captured native stderr into a terminating NativeCommandError
# under build.ps1's own $ErrorActionPreference='Stop', so build.ps1 steps run
# as child processes whose stderr stays an OS-level text stream. This process
# establishes the same toolchain env up front by dot-sourcing build.ps1's own
# discovery block (lines 4-73), so the direct cargo steps run under identical
# variables. Env injection values follow the joint gate record
# (niuma/项目经理/审查记录/2026-09-23C-D联合门禁.md).
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$worktree = Split-Path -Parent (Split-Path -Parent $root)
$rustApp = Join-Path $worktree 'NoManCode\rust-app'
$logDir = Join-Path $root 'logs'
New-Item -ItemType Directory -Path $logDir -Force | Out-Null
Set-Location -LiteralPath $rustApp

# tests/runtime.rs spawns `pwsh.exe` by name; the only pwsh.exe on this
# machine is the Git-for-Windows embedded copy whose directory is not on
# PATH. Prepend it for this process (children inherit).
$gitPwsh = 'C:\Program Files\Git'
if (-not (Get-Command pwsh.exe -ErrorAction SilentlyContinue)) {
    if (Test-Path -LiteralPath (Join-Path $gitPwsh 'pwsh.exe')) {
        $env:PATH = "$gitPwsh;$env:PATH"
    }
}
$pwshExe = Join-Path $gitPwsh 'pwsh.exe'

# Toolchain injection per the joint gate record: VS lives at D:\vsstudio
# (unregistered with vswhere), SDK at the standard Kits\10 location.
if (-not $env:VCToolsInstallDir) {
    $env:VCToolsInstallDir = 'D:\vsstudio\VC\Tools\MSVC\14.40.33807\'
}
if (-not $env:VSINSTALLDIR) { $env:VSINSTALLDIR = 'D:\vsstudio\' }
if (-not $env:WindowsSdkDir) {
    $env:WindowsSdkDir = 'C:\Program Files (x86)\Windows Kits\10\'
}
if (-not $env:UniversalCRTSdkDir) {
    $env:UniversalCRTSdkDir = 'C:\Program Files (x86)\Windows Kits\10\'
}
# Independent target dir inside the worktree (gitignored).
$env:CARGO_TARGET_DIR = Join-Path $rustApp 'target'
$env:CARGO_TERM_COLOR = 'never'

# Establish MSVC env in THIS process via build.ps1's own discovery block
# (everything before the `switch ($Action)` dispatch), then verify it.
$buildLines = Get-Content -LiteralPath (Join-Path $rustApp 'build.ps1')
$envScript = Join-Path $root 'msvc-env.ps1'
Set-Content -LiteralPath $envScript -Value ($buildLines[3..72])
. $envScript

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

# Run build.ps1 -Action X in a child pwsh: its internal EAP=Stop cannot turn
# cargo's stderr into a thrown NativeCommandError because the child's stderr
# is an OS pipe captured as plain text here. Env vars inherit from this
# process, so the child resolves the same toolchain.
function Invoke-BuildPs1([string]$name, [string]$action) {
    Invoke-Step $name {
        $output = & $pwshExe -NoProfile -ExecutionPolicy Bypass -File .\build.ps1 -Action $action 2>&1
        $output
        $global:LASTEXITCODE = $LASTEXITCODE
    } "build.ps1 -Action $action"
}

# Contract warmup step: build.ps1 -Action check (child process, real code).
Invoke-BuildPs1 'env-check' 'check'

# Targeted gates for this module (direct cargo, env already in-process).
Invoke-Step 'test-integration' { cargo test -p peachsh --locked --test plugin_host } 'cargo test -p peachsh --locked --test plugin_host'
Invoke-Step 'test-unit' { cargo test -p peachsh --locked --lib plugin_host:: } 'cargo test -p peachsh --locked --lib plugin_host::'
Invoke-Step 'clippy-module' { cargo clippy -p peachsh --lib --test plugin_host --locked -- -D warnings } 'cargo clippy -p peachsh --lib --test plugin_host --locked -- -D warnings'

# Full gates on the branch candidate (fmt is check-only; no build, no live).
Invoke-BuildPs1 'gate-test' 'test'
Invoke-BuildPs1 'gate-fmt' 'fmt'
Invoke-BuildPs1 'gate-clippy' 'clippy'
Invoke-BuildPs1 'gate-wasm-check' 'wasm-check'

$results | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root 'gate-results.json')
Write-Output '== gate complete'
