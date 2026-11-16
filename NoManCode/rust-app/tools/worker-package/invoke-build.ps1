[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('worker','host')][string]$Stage,
    [Parameter(Mandatory)][Alias('Profile')][ValidateSet('debug','release')][string]$BuildProfile,
    [Parameter(Mandatory)][ValidateSet('x86_64-pc-windows-msvc')][string]$Target
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if (-not $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Use package-worker.ps1 to own the global gate' }
$taskGate = Get-Content -Raw -LiteralPath $env:NHC_LIFECYCLE_GATE_PATH | ConvertFrom-Json
if ($taskGate.nonce -ne $env:NHC_LIFECYCLE_GATE_NONCE) { throw 'Global gate owner mismatch' }
if ($Stage -eq 'worker' -and $env:NHC_PACKAGED_WORKER_SHA256) { throw 'Worker build must not consume its own digest' }
if ($Stage -eq 'host' -and $env:NHC_PACKAGED_WORKER_SHA256 -cnotmatch '^[0-9a-f]{64}$') { throw 'Missing Host compile-time worker digest' }
. (Join-Path $PSScriptRoot '../../build.ps1') -Action environment
# Capture the tools selected after MSVC/SDK discovery, without executing probes.
# Rustup shims and wrappers can select other executables later; this is evidence,
# not a claim of the effective compiler version or a reproducible environment.
if (-not $env:NHC_BUILD_PROVENANCE_PATH) { throw 'Missing provenance output path' }
$taskTools = @{}
foreach ($taskName in @('cargo', 'rustc', 'cl.exe', 'link.exe', 'lib.exe')) {
    $taskCommand = Get-Command $taskName -CommandType Application -ErrorAction Stop | Select-Object -First 1
    $taskTools[$taskName] = @{ path = $taskCommand.Source; sha256 = (Get-FileHash -LiteralPath $taskCommand.Source -Algorithm SHA256).Hash.ToLowerInvariant() }
}
$taskRecord = @{ tools = $taskTools; msvc = $taskMsvc; sdk = $taskSdk.FullName;
    scope = 'discovered command files; rustup, wrappers, linker inputs and effective versions are not attested' }
$taskJson = $taskRecord | ConvertTo-Json -Depth 5
$taskStream = [IO.File]::Open($env:NHC_BUILD_PROVENANCE_PATH, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
try {
    $taskBytes = [Text.UTF8Encoding]::new($false).GetBytes($taskJson)
    $taskStream.Write($taskBytes, 0, $taskBytes.Length)
    $taskStream.Flush($true)
} finally { $taskStream.Dispose() }
$taskBin = if ($Stage -eq 'worker') { 'peachsh-wasm-worker' } else { 'peachsh' }
$taskArgs = @('build','--offline','--locked','--target',$Target,'--bin',$taskBin,'--message-format=json-render-diagnostics')
if ($BuildProfile -eq 'release') { $taskArgs += '--release' }
& ($taskTools['cargo'].path) @taskArgs
exit $LASTEXITCODE
