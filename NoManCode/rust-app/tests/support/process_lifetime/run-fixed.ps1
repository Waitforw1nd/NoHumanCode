param(
    [Parameter(Mandatory=$true)][string]$ExpectedHead,
    [Parameter(Mandatory=$true)][string]$ExpectedProductTree,
    [Parameter(Mandatory=$true)][string]$ExpectedRustTree,
    [Parameter(Mandatory=$true)][string]$TestExe,
    [Parameter(Mandatory=$true)][string]$TestSha256,
    [Parameter(Mandatory=$true)][string]$ProbeExe,
    [Parameter(Mandatory=$true)][string]$ProbeSha256,
    [Parameter(Mandatory=$true)][string]$AttemptDir,
    [ValidateRange(30,1800)][int]$TimeoutSeconds = 180
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
foreach ($value in @($ExpectedHead,$ExpectedProductTree,$ExpectedRustTree)) {
    if ($value -notmatch '^[a-fA-F0-9]{40}$') { throw 'complete Git object IDs required' }
}
foreach ($value in @($TestSha256,$ProbeSha256)) {
    if ($value -notmatch '^[a-fA-F0-9]{64}$') { throw 'complete artifact SHA256 required' }
}
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
$attempt = [IO.Path]::GetFullPath($AttemptDir)
if (-not $attempt.StartsWith('E:\', [StringComparison]::OrdinalIgnoreCase)) { throw 'attempt must be on E:' }
if (Test-Path -LiteralPath $attempt) { throw 'attempt already exists' }
foreach ($source in @($TestExe,$ProbeExe)) {
    if (-not ([IO.Path]::GetFullPath($source)).StartsWith('E:\', [StringComparison]::OrdinalIgnoreCase)) { throw 'artifacts must be on E:' }
}
function Source-State {
    $head = (& git rev-parse HEAD).Trim(); if ($LASTEXITCODE) { throw 'HEAD unavailable' }
    $product = (& git rev-parse 'HEAD:NoManCode').Trim(); if ($LASTEXITCODE) { throw 'product tree unavailable' }
    $rust = (& git rev-parse 'HEAD:NoManCode/rust-app').Trim(); if ($LASTEXITCODE) { throw 'Rust tree unavailable' }
    $status = @(& git status --porcelain --untracked-files=all); if ($LASTEXITCODE) { throw 'status unavailable' }
    $index = @(& git diff --cached --name-only); if ($LASTEXITCODE) { throw 'index unavailable' }
    if ($head -ne $ExpectedHead -or $product -ne $ExpectedProductTree -or $rust -ne $ExpectedRustTree -or $status.Count -ne 0 -or $index.Count -ne 0) { throw 'source identity or cleanliness mismatch' }
    return @{head=$head;product=$product;rust=$rust;status=$status;index=$index}
}
function Save-Record($path, $record) { $record | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $path -Encoding UTF8 }
function Assert-Hash($path,$expected) { if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $expected) { throw "artifact mismatch: $path" } }
function Source-Files {
    $repo = (& git rev-parse --show-toplevel).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'repository root unavailable' }
    $paths = @(& git -C $repo -c core.quotepath=false ls-files --full-name -- NoManCode/rust-app)
    if ($LASTEXITCODE -ne 0 -or $paths.Count -eq 0) { throw 'tracked source list unavailable' }
    foreach ($path in ($paths | Sort-Object -CaseSensitive)) {
        $file = Join-Path $repo $path
        $item = Get-Item -LiteralPath $file -Force
        if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw "unsupported tracked source: $path" }
        [pscustomobject]@{path=$path;bytes=$item.Length;sha256=(Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()}
    }
}
function Assert-SourceFiles($expected) {
    $actual = @(Source-Files)
    if (($actual | ConvertTo-Json -Compress) -cne ($expected | ConvertTo-Json -Compress)) { throw 'tracked source bytes or paths changed' }
    return ,$actual
}
New-Item -ItemType Directory -Path $attempt | Out-Null
$temp = Join-Path $attempt 'temp'
New-Item -ItemType Directory -Path $temp | Out-Null
$env:TEMP = $temp; $env:TMP = $temp
$gatePath = Join-Path $root '.local/nhc-w2-window.lock'
New-Item -ItemType Directory -Path (Split-Path -Parent $gatePath) -Force | Out-Null
$gate = [IO.File]::Open($gatePath,[IO.FileMode]::CreateNew,[IO.FileAccess]::ReadWrite,[IO.FileShare]::Read)
$danger = $false
$active = $null
Push-Location $root
try {
    Save-Record (Join-Path $attempt 'source-before.json') (Source-State)
    $sourceFiles = @(Source-Files)
    Save-Record (Join-Path $attempt 'source-files-before.json') $sourceFiles
    Assert-Hash $TestExe $TestSha256
    Assert-Hash $ProbeExe $ProbeSha256
    $fixed = Join-Path $attempt 'fixed'; New-Item -ItemType Directory -Path $fixed | Out-Null
    $test = Join-Path $fixed 'lib-test.exe'; $probe = Join-Path $fixed 'probe.exe'
    Copy-Item -LiteralPath $TestExe -Destination $test
    Copy-Item -LiteralPath $ProbeExe -Destination $probe
    Assert-Hash $test $TestSha256; Assert-Hash $probe $ProbeSha256
    $launcher = Join-Path $PSScriptRoot '../wasm_worker_process/DirectProcess.cs'
    $launcherHash = (Get-FileHash -LiteralPath $launcher -Algorithm SHA256).Hash
    Save-Record (Join-Path $attempt 'inputs.json') @{test=$TestExe;test_sha256=$TestSha256;probe=$ProbeExe;probe_sha256=$ProbeSha256;launcher_sha256=$launcherHash}
    if ('NhcDirectProcess' -as [type]) { throw 'fresh PowerShell process required' }
    Add-Type -Path $launcher
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class NhcLifetimeWait {
 [DllImport("kernel32.dll", SetLastError=true)] public static extern uint WaitForSingleObject(SafeFileHandle h, uint ms);
 [DllImport("kernel32.dll", SetLastError=true)] public static extern bool GetExitCodeProcess(SafeFileHandle h, out uint code);
}
'@
    $cases = @(
      'process_lifetime_tests::normal_nonzero_and_prestart_rejection',
      'process_lifetime_tests::detached_multi_child_waits_for_real_tree_zero',
      'process_lifetime_tests::timeout_and_cancel_reap_original_tree_without_sealing',
      'process_lifetime_tests::dropped_caller_requests_cleanup_without_immediate_unknown',
      'process_lifetime_tests::output_limit_termination_keeps_tree_until_real_cleanup',
      'process_lifetime_tests::failed_job_observation_stays_unknown_until_late_real_cleanup',
      'process_lifetime_tests::reader_error_joins_and_cleans_but_unknown_is_sticky',
      'process_lifetime_tests::live_reader_keeps_slot_even_after_process_and_job_finish',
      'process_lifetime_tests::unknown_on_membership_and_created_hold_retains_then_cleans',
      'process_lifetime_tests::watchdog_real_exit_is_covered_by_slot',
      'process_lifetime_tests::exit_code_failure_preserves_signal_and_waits_for_io',
      'process_lifetime_tests::exit_code_failure_returns_unknown_then_releases_once',
      'process_lifetime_tests::termination_failure_retains_live_tree_then_recovers_without_reopening',
      'process_lifetime_tests::resize_races_submit_without_erasing_existing_slots',
      'process_lifetime_tests::argv_cwd_environment_roundtrip_and_invalid_prestart',
      'process_lifetime_tests::breakaway_independent_controls_and_component_denial',
      'process_lifetime::state_tests::unknown_is_sticky_and_ticket_consumed_once_after_cleanup',
      'process_lifetime::state_tests::blocking_settle_does_not_hold_decision_lock_or_release_slot',
      'process_lifetime::state_tests::resize_preserves_existing_slots_and_uses_new_limit',
      'process_lifetime::state_tests::caller_drop_only_requests_cancel_until_holder_decides',
      'process_lifetime_tests::token_cancel_waits_for_original_holder_and_reader_before_settlement',
      'process_lifetime_tests::final_read_overflow_cannot_be_adopted_as_ordinary_exit',
      'process_lifetime_tests::token_cancel_unknown_keeps_reader_and_stays_sealed_after_late_cleanup',
      'process_lifetime::state_tests::original_holder_handle_keeps_slot_until_actual_thread_exit',
      'process_lifetime_tests::known_result_allows_immediate_next_submit_at_capacity_one'
    )
    $env:NHC_PROCESS_LIFETIME_PROBE = $probe
    $env:NHC_PROCESS_LIFETIME_PROBE_SHA256 = $ProbeSha256
    $env:NHC_PROCESS_TEST_ROOT = $attempt
    $number = 0
    foreach ($case in $cases) {
        $null = Source-State; $null = Assert-SourceFiles $sourceFiles
        Assert-Hash $test $TestSha256; Assert-Hash $probe $ProbeSha256
        $number++; $site = Join-Path $attempt ('case-{0:D2}' -f $number)
        New-Item -ItemType Directory -Path $site | Out-Null
        $out = Join-Path $site 'stdout'; $err = Join-Path $site 'stderr'
        $args = "--exact $case --nocapture --test-threads=1"
        if ($case.StartsWith('process_lifetime_tests::')) { $args += ' --ignored' }
        $active = @{case=$case;state='starting';started_utc=[DateTime]::UtcNow.ToString('o')}
        Save-Record (Join-Path $site 'case.json') $active
        $danger = $true
        $native = [NhcDirectProcess]::Start($test,$args,$root,$out,$err)
        $active.pid = $native.Pid; $active.creation_filetime = $native.CreationFileTime
        if ($native.CreationFileTime -eq 0) { throw 'original process identity unavailable' }
        $active.state = 'running'; Save-Record (Join-Path $site 'case.json') $active
        $native.Resume()
        $clock = [Diagnostics.Stopwatch]::StartNew()
        while ($true) {
            $wait = [NhcLifetimeWait]::WaitForSingleObject($native.Handle,100)
            if ($wait -eq 0) { break }
            if ($wait -ne 258) { throw 'original process wait failed' }
            if (([string](Get-Content -Raw -LiteralPath $out) + [string](Get-Content -Raw -LiteralPath $err)).Contains('NHC_TERMINAL_UNKNOWN:')) { throw 'unknown marker' }
            if ($clock.Elapsed.TotalSeconds -ge $TimeoutSeconds) { throw 'deadline; original process retained' }
        }
        [uint32]$code = 0
        if (-not [NhcLifetimeWait]::GetExitCodeProcess($native.Handle,[ref]$code)) { throw 'exit code unavailable' }
        $log = [string](Get-Content -Raw -LiteralPath $out) + [string](Get-Content -Raw -LiteralPath $err)
        if ($code -ne 0 -or $log.Contains('NHC_TERMINAL_UNKNOWN:') -or $log -notmatch 'test result: ok\. 1 passed; 0 failed') { throw 'case failed or cleanup unconfirmed' }
        $active.state = 'passed'; $active.exit_code = $code; $active.elapsed_ms = $clock.ElapsedMilliseconds
        Save-Record (Join-Path $site 'case.json') $active
        $native.Thread.Dispose(); $native.Handle.Dispose(); $danger = $false
    }
    Save-Record (Join-Path $attempt 'source-after.json') (Source-State)
    $sourceFilesAfter = @(Assert-SourceFiles $sourceFiles)
    Save-Record (Join-Path $attempt 'source-files-after.json') $sourceFilesAfter
    Assert-Hash $test $TestSha256; Assert-Hash $probe $ProbeSha256
    Assert-Hash $TestExe $TestSha256; Assert-Hash $ProbeExe $ProbeSha256
    $files = @(Get-ChildItem -LiteralPath $attempt -File -Recurse | ForEach-Object {
        [pscustomobject]@{path=$_.FullName.Substring($attempt.Length + 1);bytes=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}
    })
    Save-Record (Join-Path $attempt 'evidence-manifest.json') $files
    Save-Record (Join-Path $attempt 'complete.json') @{cases=$cases.Count;state='complete';ended_utc=[DateTime]::UtcNow.ToString('o')}
} catch {
    if ($danger) {
        Save-Record (Join-Path $attempt 'incident.json') @{state='unknown';reason=$_.Exception.Message;active=$active;instruction='retain runner and original handles; do not reopen window'}
        # Keep this process, its original handles and the exclusive gate alive.
        while ($true) { Start-Sleep -Seconds 10 }
    }
    throw
} finally {
    Pop-Location
    $gate.Dispose()
    if (-not $danger) { Remove-Item -LiteralPath $gatePath }
}
