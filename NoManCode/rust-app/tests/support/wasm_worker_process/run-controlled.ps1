param(
    [Parameter(Mandatory=$true)][string]$ExpectedHead,
    [Parameter(Mandatory=$true)][string]$TargetDir,
    [Parameter(Mandatory=$true)][string]$AttemptDir,
    [ValidateRange(30,1800)][int]$CaseTimeoutSeconds = 180
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($ExpectedHead -notmatch '^[0-9a-fA-F]{40}$') { throw 'ExpectedHead must be a complete commit SHA' }
if (Test-Path -LiteralPath $AttemptDir) { throw 'Attempt directory must not exist' }
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
$target = [System.IO.Path]::GetFullPath($TargetDir)
New-Item -ItemType Directory -Path $AttemptDir | Out-Null
$attempt = (Resolve-Path -LiteralPath $AttemptDir).Path
$local = Join-Path $root '.local'
New-Item -ItemType Directory -Path $local -Force | Out-Null
$gatePath = Join-Path $local 'nhc-w2-window.lock'
# Retained after unknown or runner crash; no later attempt can reopen the window.
$gate = [System.IO.File]::Open($gatePath, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::Read)
$unsafeWindow = $false
$active = $null
$manifest = @()
$sourceBefore = $null
$sourceValidated = $false
function Save-Json($path, $value) { $value | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $path -Encoding UTF8 }
function Write-Gate($record) {
    $bytes = [System.Text.Encoding]::UTF8.GetBytes(($record | ConvertTo-Json -Depth 12))
    $gate.Position = 0
    $gate.SetLength(0)
    $gate.Write($bytes, 0, $bytes.Length)
    $gate.Flush($true)
}
function Get-SourceState {
    $head = (& git rev-parse --verify HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'cannot read HEAD' }
    $product = (& git rev-parse 'HEAD:NoManCode').Trim()
    if ($LASTEXITCODE -ne 0) { throw 'cannot read product tree' }
    $rust = (& git rev-parse 'HEAD:NoManCode/rust-app').Trim()
    if ($LASTEXITCODE -ne 0) { throw 'cannot read Rust tree' }
    $status = @(& git status --porcelain --untracked-files=all)
    if ($LASTEXITCODE -ne 0) { throw 'cannot read source status' }
    $index = @(& git diff --cached --name-only)
    if ($LASTEXITCODE -ne 0) { throw 'cannot read index' }
    [pscustomobject]@{head=$head; product_tree=$product; rust_tree=$rust; status=$status; index=$index}
}
function Assert-Source($state) {
    if ($state.head -ne $ExpectedHead.ToLowerInvariant() -or $state.status.Count -ne 0 -or $state.index.Count -ne 0) { throw 'source HEAD mismatch or tree/index dirty' }
    if ($null -ne $sourceBefore -and ($state.product_tree -ne $sourceBefore.product_tree -or $state.rust_tree -ne $sourceBefore.rust_tree)) { throw 'source tree changed' }
}
function Run-Controlled($test, $arguments, $site, $timeoutSeconds) {
    $nonce = [guid]::NewGuid().ToString('N')
    $env:NHC_W2_RUN_NONCE = $nonce
    $log = Join-Path $site 'test.stdout'
    $err = Join-Path $site 'test.stderr'
    $metadata = Join-Path $site 'case.json'
    $script:active = [ordered]@{test=$test; nonce=$nonce; state='starting'; start_utc=[DateTime]::UtcNow.ToString('o'); timeout_seconds=$timeoutSeconds; pid=$null; creation_filetime=$null; stdout=$log; stderr=$err; fixed_hashes=$manifest; source=$sourceBefore; attempt=$attempt}
    Save-Json $metadata $script:active
    Write-Gate $script:active
    $script:unsafeWindow = $true
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $native = [NhcDirectProcess]::Start((Join-Path $fixed 'lib-test.exe'), $arguments, $root, $log, $err)
    $script:active.pid = $native.Pid
    $script:active.creation_filetime = $native.CreationFileTime
    $script:active.state = 'running'
    Save-Json $metadata $script:active
    Write-Gate $script:active
    if ($native.CreationFileTime -eq 0) { throw 'process creation identity unavailable' }
    $process = [System.Diagnostics.Process]::GetProcessById($native.Pid)
    $null = $process.Handle
    if ($process.StartTime.ToUniversalTime().ToFileTimeUtc() -ne $native.CreationFileTime) { throw 'process identity mismatch' }
    $native.Resume()
    while ($true) {
        $stdout = [string](Get-Content -Raw -LiteralPath $log)
        $stderr = [string](Get-Content -Raw -LiteralPath $err)
        if ([string]::Concat($stdout, $stderr).Contains('NHC_TERMINAL_UNKNOWN:')) { throw 'terminal unknown marker' }
        if (Test-Path -LiteralPath (Join-Path $site 'host.stage')) {
            $stage = [string](Get-Content -Raw -LiteralPath (Join-Path $site 'host.stage'))
            if ([string]::Concat($stage, '').Contains('phase=created_process_handle_unknown ')) { throw 'terminal creation handle unknown' }
        }
        if ($clock.Elapsed.TotalSeconds -ge $timeoutSeconds) { throw 'case deadline exceeded; process retained' }
        if ($process.HasExited) { break }
        Start-Sleep -Milliseconds 100
    }
    $script:active['end_utc'] = [DateTime]::UtcNow.ToString('o')
    $script:active['elapsed_ms'] = [long]$clock.ElapsedMilliseconds
    $script:active['exit_code'] = $process.ExitCode
    $script:active.state = 'exited'
    Save-Json $metadata $script:active
    $contents = [string]::Concat((Get-Content -Raw -LiteralPath $log), (Get-Content -Raw -LiteralPath $err))
    if ($contents.Contains('NHC_TERMINAL_UNKNOWN:')) { throw 'terminal unknown marker at exit' }
    if ($process.ExitCode -ne 0) { throw 'case failed; cleanup not independently confirmed' }
    if ($test -ne '__list__' -and ($contents -notmatch 'test result: ok\. 1 passed; 0 failed' -or $contents -match '(?i)not established')) { throw 'missing single passing case or attack not established' }
    $script:unsafeWindow = $false
    $script:active.state = 'passed'
    Save-Json $metadata $script:active
    Write-Gate $script:active
    $native.Thread.Dispose()
    $native.Handle.Dispose()
    $process.Dispose()
    return $log
}
Push-Location $root
try {
    $sourceBefore = Get-SourceState
    Assert-Source $sourceBefore
    $sourceValidated = $true
    Save-Json (Join-Path $attempt 'source-before.json') $sourceBefore
    Write-Gate ([ordered]@{state='building'; expected_head=$ExpectedHead; attempt=$attempt; source=$sourceBefore})
    if ('NhcDirectProcess' -as [type]) { throw 'run from a fresh PowerShell process so the launcher type matches this source' }
    Add-Type -Path (Join-Path $PSScriptRoot 'DirectProcess.cs')
    & cargo build --offline --locked --target-dir $target --bin peachsh-wasm-worker --example wasm_worker_process_probe 1> (Join-Path $attempt 'build.stdout') 2> (Join-Path $attempt 'build.stderr')
    if ($LASTEXITCODE -ne 0) { throw 'build failed' }
    $json = Join-Path $attempt 'test-build.jsonl'
    & cargo test --offline --locked --target-dir $target --lib --no-run --message-format=json 1> $json 2> (Join-Path $attempt 'test-build.stderr')
    if ($LASTEXITCODE -ne 0) { throw 'test build failed' }
    $afterBuild = Get-SourceState
    Save-Json (Join-Path $attempt 'source-after-build.json') $afterBuild
    Assert-Source $afterBuild
    $bins = @(Get-Content -LiteralPath $json | ForEach-Object { try { $_ | ConvertFrom-Json } catch { $null } } | Where-Object { $_ -and $_.PSObject.Properties['reason'] -and $_.reason -eq 'compiler-artifact' -and $_.profile.test -eq $true -and $_.target.kind -contains 'lib' -and $_.executable } | ForEach-Object { $_.executable } | Select-Object -Unique)
    if ($bins.Count -ne 1) { throw "expected exactly one lib test binary; found $($bins.Count)" }
    $fixed = Join-Path $attempt 'fixed'
    New-Item -ItemType Directory -Path $fixed | Out-Null
    $sources = @((Join-Path $target 'debug/peachsh-wasm-worker.exe'), (Join-Path $target 'debug/examples/wasm_worker_process_probe.exe'), $bins[0])
    $names = @('worker.exe','probe.exe','lib-test.exe')
    $manifest = for ($i=0; $i -lt $sources.Count; $i++) {
        $source = (Resolve-Path -LiteralPath $sources[$i]).Path
        $dest = Join-Path $fixed $names[$i]
        Copy-Item -LiteralPath $source -Destination $dest
        $hash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
        if ((Get-FileHash -LiteralPath $dest -Algorithm SHA256).Hash -ne $hash) { throw 'fixed copy mismatch' }
        [pscustomobject]@{ name=$names[$i]; source=$source; sha256=$hash.ToLowerInvariant(); bytes=(Get-Item -LiteralPath $dest).Length }
    }
    $manifest | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $attempt 'manifest.json') -Encoding UTF8
    $tests = @(
        'wasm_worker_process_tests::w01_real_worker_before_after_and_near_8mib',
        'wasm_worker_process_tests::fault_matrix::w02_protocol_fault_matrix',
        'wasm_worker_process_tests::fault_matrix::w03_pipe_fault_matrix',
        'wasm_worker_process_tests::fault_matrix::w04_resource_limits',
        'wasm_worker_process_tests::fault_matrix::w04_injected_zero_read_sensitivity',
        'wasm_worker_process_tests::fault_matrix::w04_unknown_cleanup_and_slot_reuse',
        'wasm_worker_process_tests::fault_matrix::w04_cancellation_and_acceptance_boundaries',
        'wasm_worker_process_tests::fault_matrix::w04_injected_membership_failure_reaps',
        'wasm_worker_process_tests::fault_matrix::w04_persistent_observation_errors_retain_io_and_slot',
        'wasm_worker_process_tests::fault_matrix::w05_host_kill_boundaries',
        'wasm_worker_process_tests::fault_matrix::w06_p01_live_job_observation',
        'wasm_worker_process::platform::w06_artifact_tests::pin_file_counterexamples',
        'wasm_worker_process::platform::w06_artifact_tests::pin_existing_writable_mapping',
        'wasm_worker_process::platform::w06_artifact_tests::process_image_identity_counterexample',
        'wasm_worker_process::platform::w06_handle_tests::sentinel_explicit_inheritance_control',
        'wasm_worker_process_tests::fault_matrix::w06_non_whitelisted_event',
        'wasm_worker_process_tests::fault_matrix::w06_job_handle_not_inherited',
        'wasm_worker_process::platform::w06_limit_tests::active_limit_single_factor',
        'wasm_worker_process::platform::w06_limit_tests::breakaway_single_factor',
        'wasm_worker_process::platform::r04_tests::suspended_process_in_real_job_requires_process_fallback'
    )
    $helpers = @(
        'wasm_worker_process_tests::fault_matrix::w05_host_helper',
        'wasm_worker_process::platform::w06_handle_tests::positive_helper',
        'wasm_worker_process::platform::w06_limit_tests::limit_helper'
    )
    $listingSite = Join-Path $attempt 'listing'
    New-Item -ItemType Directory -Path $listingSite | Out-Null
    $listingFile = Run-Controlled '__list__' '--list --ignored --format terse' $listingSite $CaseTimeoutSeconds
    $listing = @(Get-Content -LiteralPath $listingFile)
    $discovered = @($listing | ForEach-Object { if ($_ -match '^(.*): test$') { $Matches[1] } } | Where-Object { $_ -like 'wasm_worker_process_tests::*' -or $_ -like 'wasm_worker_process::platform::w06_*' -or $_ -like 'wasm_worker_process::platform::r04_tests::*' })
    foreach ($name in $discovered) { if ($name -notin $tests -and $name -notin $helpers) { throw "unmapped controlled test: $name" } }
    foreach ($name in @($tests + $helpers)) { if ($name -notin $discovered) { throw "missing ignored controlled test: $name" } }
    $index = 0
    foreach ($test in $tests) {
        Assert-Source (Get-SourceState)
        foreach ($entry in $manifest) {
            if ((Get-FileHash -LiteralPath (Join-Path $fixed $entry.name) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $entry.sha256) { throw 'fixed artifact changed' }
        }
        $index++
        $site = Join-Path $attempt ('case-{0:D2}' -f $index)
        New-Item -ItemType Directory -Path $site | Out-Null
        $worker = Join-Path $site 'worker.exe'
        Copy-Item -LiteralPath (Join-Path $fixed 'worker.exe') -Destination $worker
        if ((Get-FileHash -LiteralPath $worker -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest[0].sha256) { throw 'worker case copy mismatch' }
        $env:NHC_W2_WORKER_SNAPSHOT = $worker
        $env:NHC_W2_WORKER_SHA256 = $manifest[0].sha256
        $env:NHC_W2_FAULT_SHA256 = $manifest[1].sha256
        $env:NHC_W2_STAGE_LOG = Join-Path $site 'host.stage'
        $env:NHC_W2_FAULT_DIR = Join-Path $site 'probes'
        $env:NHC_W05_ROOT = Join-Path $site 'w05'
        $env:NHC_W06_ROOT = Join-Path $site 'w06'
        New-Item -ItemType Directory -Path $env:NHC_W06_ROOT | Out-Null
        & (Join-Path $PSScriptRoot 'prepare-probes.ps1') -Probe (Join-Path $fixed 'probe.exe') -Destination $env:NHC_W2_FAULT_DIR
        & (Join-Path $PSScriptRoot 'prepare-w05.ps1') -Probe (Join-Path $fixed 'probe.exe') -Destination $env:NHC_W05_ROOT
        $null = Run-Controlled $test "--exact $test --ignored --nocapture --test-threads=1" $site $CaseTimeoutSeconds
        if ((Get-FileHash -LiteralPath $worker -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest[0].sha256) { throw 'worker case copy changed' }
        $probes = @(Get-ChildItem -LiteralPath $env:NHC_W2_FAULT_DIR -Filter '*.exe' -File) + @(Get-ChildItem -LiteralPath $env:NHC_W05_ROOT -Filter '*.exe' -File -Recurse)
        foreach ($probe in $probes) {
            if ((Get-FileHash -LiteralPath $probe.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest[1].sha256) { throw "probe case copy changed: $($probe.FullName)" }
        }
        foreach ($entry in $manifest) {
            if ((Get-FileHash -LiteralPath (Join-Path $fixed $entry.name) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $entry.sha256) { throw 'fixed artifact changed during test' }
        }
    }
    $sourceAfter = Get-SourceState
    Save-Json (Join-Path $attempt 'source-after.json') $sourceAfter
    Assert-Source $sourceAfter
    Write-Output 'All listed exact cases completed; inspect logs for scoped evidence.'
} catch {
    if ($unsafeWindow) {
        $active.state = 'terminal_unknown'
        $active['reason'] = $_.Exception.Message
        $active['detected_utc'] = [DateTime]::UtcNow.ToString('o')
        $active['end_utc'] = $active['detected_utc']
        $active['elapsed_ms'] = [long](([DateTime]::UtcNow - [DateTime]::Parse($active.start_utc).ToUniversalTime()).TotalMilliseconds)
        $active['instruction'] = 'Do not reopen the window until retained objects and unknown cleanup are independently resolved.'
        Write-Gate $active
        Save-Json (Join-Path $attempt 'incident.json') $active
        Save-Json (Join-Path (Split-Path -Parent $active.stdout) 'case.json') $active
    }
    throw
} finally {
    $sourceError = $null
    if ($sourceValidated) {
        try {
            $finalSource = Get-SourceState
            Save-Json (Join-Path $attempt 'source-final.json') $finalSource
            Assert-Source $finalSource
        } catch {
            $sourceError = $_.Exception.Message
            $unsafeWindow = $true
            Write-Gate ([ordered]@{state='source_unknown'; reason=$sourceError; attempt=$attempt; source=$sourceBefore; active=$active; fixed_hashes=$manifest})
        }
    }
    $gate.Dispose()
    if (-not $unsafeWindow) { Remove-Item -LiteralPath $gatePath }
    Pop-Location
    if ($sourceError) { throw "final source verification failed: $sourceError" }
}
