param(
    [Parameter(Mandatory=$true)][string]$TargetDir,
    [Parameter(Mandatory=$true)][string]$AttemptDir
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (Test-Path -LiteralPath $AttemptDir) { throw 'Attempt directory must not exist' }
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
$target = [System.IO.Path]::GetFullPath($TargetDir)
New-Item -ItemType Directory -Path $AttemptDir | Out-Null
$attempt = (Resolve-Path -LiteralPath $AttemptDir).Path
Push-Location $root
try {
    & cargo build --offline --locked --target-dir $target --bin peachsh-wasm-worker --example wasm_worker_process_probe 1> (Join-Path $attempt 'build.stdout') 2> (Join-Path $attempt 'build.stderr')
    if ($LASTEXITCODE -ne 0) { throw 'build failed' }
    $json = Join-Path $attempt 'test-build.jsonl'
    & cargo test --offline --locked --target-dir $target --lib --no-run --message-format=json 1> $json 2> (Join-Path $attempt 'test-build.stderr')
    if ($LASTEXITCODE -ne 0) { throw 'test build failed' }
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
    $listingFile = Join-Path $attempt 'ignored-tests.txt'
    & (Join-Path $fixed 'lib-test.exe') --list --ignored --format terse 1> $listingFile 2> (Join-Path $attempt 'ignored-tests.stderr')
    if ($LASTEXITCODE -ne 0) { throw 'cannot enumerate ignored tests' }
    $listing = @(Get-Content -LiteralPath $listingFile)
    $discovered = @($listing | ForEach-Object { if ($_ -match '^(.*): test$') { $Matches[1] } } | Where-Object { $_ -like 'wasm_worker_process_tests::*' -or $_ -like 'wasm_worker_process::platform::w06_*' -or $_ -like 'wasm_worker_process::platform::r04_tests::*' })
    foreach ($name in $discovered) { if ($name -notin $tests -and $name -notin $helpers) { throw "unmapped controlled test: $name" } }
    foreach ($name in @($tests + $helpers)) { if ($name -notin $discovered) { throw "missing ignored controlled test: $name" } }
    $index = 0
    foreach ($test in $tests) {
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
        $log = Join-Path $site 'test.stdout'
        $err = Join-Path $site 'test.stderr'
        & (Join-Path $fixed 'lib-test.exe') --exact $test --ignored --nocapture --test-threads=1 1> $log 2> $err
        $exitCode = $LASTEXITCODE
        $contents = [string](Get-Content -Raw -LiteralPath $log) + [string](Get-Content -Raw -LiteralPath $err)
        if ($exitCode -ne 0 -or $contents -notmatch 'test result: ok\. 1 passed; 0 failed' -or $contents -match '(?i)cleanup.?unknown|not established') { throw "failed, unknown, uncovered, or missing exact test: $test; see $log and $err" }
        if ((Get-FileHash -LiteralPath $worker -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest[0].sha256) { throw 'worker case copy changed' }
        $probes = @(Get-ChildItem -LiteralPath $env:NHC_W2_FAULT_DIR -Filter '*.exe' -File) + @(Get-ChildItem -LiteralPath $env:NHC_W05_ROOT -Filter '*.exe' -File -Recurse)
        foreach ($probe in $probes) {
            if ((Get-FileHash -LiteralPath $probe.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest[1].sha256) { throw "probe case copy changed: $($probe.FullName)" }
        }
        foreach ($entry in $manifest) {
            if ((Get-FileHash -LiteralPath (Join-Path $fixed $entry.name) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $entry.sha256) { throw 'fixed artifact changed during test' }
        }
    }
    Write-Output 'All listed exact cases completed; inspect logs for scoped evidence.'
} finally { Pop-Location }
