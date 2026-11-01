param(
    [Parameter(Mandatory=$true)][string]$ExpectedHead,
    [Parameter(Mandatory=$true)][string]$TestBinary,
    [Parameter(Mandatory=$true)][string]$BuildReceipt,
    [Parameter(Mandatory=$true)][string]$AttemptDirectory,
    [ValidateRange(15,1800)][int]$TimeoutSeconds = 45
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($ExpectedHead -notmatch '^[0-9a-fA-F]{40}$') { throw 'ExpectedHead must be a complete commit SHA' }
if (Test-Path -LiteralPath $AttemptDirectory) { throw 'Attempt directory must not exist' }
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
New-Item -ItemType Directory -Path $AttemptDirectory | Out-Null
$attempt = (Resolve-Path -LiteralPath $AttemptDirectory).Path
$env:TMP = $attempt
$env:TEMP = $attempt
$env:NHC_AUTHORITY_WAL_CASE_DIR = Join-Path $attempt 'case'
$local = Join-Path $root '.local'
New-Item -ItemType Directory -Path $local -Force | Out-Null
$gatePath = Join-Path $local 'nhc-u35-wal-window.lock'
$gate = [IO.File]::Open($gatePath,[IO.FileMode]::CreateNew,[IO.FileAccess]::ReadWrite,[IO.FileShare]::Read)
$unsafeWindow = $false
$active = $null
$native = $null
$sourceBefore = $null
function Save-Json($path,$record) { $record | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $path -Encoding utf8 }
function Write-Gate($record) {
    $bytes = [Text.Encoding]::UTF8.GetBytes(($record | ConvertTo-Json -Depth 10))
    $gate.Position=0; $gate.SetLength(0); $gate.Write($bytes,0,$bytes.Length); $gate.Flush($true)
}
function Source-State {
    $head=(& git rev-parse HEAD).Trim(); if ($LASTEXITCODE -ne 0) { throw 'HEAD unavailable' }
    $product=(& git rev-parse 'HEAD:NoManCode').Trim(); if ($LASTEXITCODE -ne 0) { throw 'product tree unavailable' }
    $rust=(& git rev-parse 'HEAD:NoManCode/rust-app').Trim(); if ($LASTEXITCODE -ne 0) { throw 'Rust tree unavailable' }
    $status=@(& git status --porcelain --untracked-files=all); if ($LASTEXITCODE -ne 0) { throw 'status unavailable' }
    $index=@(& git diff --cached --name-only); if ($LASTEXITCODE -ne 0) { throw 'index unavailable' }
    return [pscustomobject]@{head=$head;product_tree=$product;rust_tree=$rust;status=$status;index=$index}
}
function Assert-Source($state) {
    if ($state.head -ne $ExpectedHead.ToLowerInvariant() -or $state.status.Count -ne 0 -or $state.index.Count -ne 0) { throw 'source HEAD mismatch or worktree/index dirty' }
    if ($null -ne $sourceBefore -and ($state.product_tree -ne $sourceBefore.product_tree -or $state.rust_tree -ne $sourceBefore.rust_tree)) { throw 'source trees changed' }
}
Push-Location $root
try {
    $sourceBefore=Source-State
    Assert-Source $sourceBefore
    Save-Json (Join-Path $attempt 'source-before.json') $sourceBefore
    $receiptPath=(Resolve-Path -LiteralPath $BuildReceipt).Path
    $receipt=Get-Content -Raw -LiteralPath $receiptPath | ConvertFrom-Json
    if ($receipt.head -ne $sourceBefore.head -or $receipt.product_tree -ne $sourceBefore.product_tree -or $receipt.rust_tree -ne $sourceBefore.rust_tree -or $receipt.exit_code -ne 0) { throw 'build receipt does not match source and successful build' }
    $original=(Resolve-Path -LiteralPath $TestBinary).Path
    $hash=(Get-FileHash -LiteralPath $original -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -ne $receipt.test_binary_sha256.ToLowerInvariant()) { throw 'binary hash differs from build receipt' }
    $fixed=Join-Path $attempt 'lib-test.exe'
    Copy-Item -LiteralPath $original -Destination $fixed
    if ((Get-FileHash -LiteralPath $fixed -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) { throw 'fixed binary copy mismatch' }
    $name='execution_authority::tests::uncheckpointed_wal_crash_rejects_alias_and_recovers_original'
    $active=[ordered]@{state='starting';start_utc=[DateTime]::UtcNow.ToString('o');source=$sourceBefore;receipt=$receiptPath;receipt_sha256=(Get-FileHash -LiteralPath $receiptPath -Algorithm SHA256).Hash;binary_source=$original;binary_sha256=$hash;binary_bytes=(Get-Item -LiteralPath $fixed).Length;test=$name;pid=$null;creation_filetime=$null;attempt=$attempt;timeout_seconds=$TimeoutSeconds}
    Save-Json (Join-Path $attempt 'case.json') $active
    Write-Gate $active
    Add-Type -Path (Join-Path $PSScriptRoot 'DirectProcess.cs')
    $unsafeWindow=$true
    $native=[NhcWalDirectProcess]::Start($fixed,"--ignored --exact $name --nocapture --test-threads=1",$root,(Join-Path $attempt 'stdout.log'),(Join-Path $attempt 'stderr.log'))
    $active.pid=$native.Pid
    $active.creation_filetime=$native.CreationFileTime
    $active.state='created_suspended'
    Save-Json (Join-Path $attempt 'case.json') $active
    Write-Gate $active
    if ($native.CreationFileTime -eq 0) { throw 'original process creation time unavailable; process retained' }
    $native.Resume()
    $active.state='running'
    Save-Json (Join-Path $attempt 'case.json') $active
    Write-Gate $active
    $clock=[Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        if (Test-Path -LiteralPath (Join-Path $attempt 'case/unknown.txt')) { throw 'child cleanup unknown; parent retained' }
        $wait=$native.Wait(100)
        if ($wait -eq 0) { break }
        if ($wait -ne 258) { throw "parent process wait unknown: $wait" }
        if ($clock.Elapsed.TotalSeconds -ge $TimeoutSeconds) { throw 'deadline exceeded; parent retained' }
    }
    $active.end_utc=[DateTime]::UtcNow.ToString('o')
    $active['exit_code']=$native.ExitCode()
    $active.state='exited'
    Save-Json (Join-Path $attempt 'case.json') $active
    if (Test-Path -LiteralPath (Join-Path $attempt 'case/unknown.txt')) { throw 'unknown reported at exit' }
    $stdout=[string](Get-Content -Raw -LiteralPath (Join-Path $attempt 'stdout.log'))
    if ($active.exit_code -ne 0 -or $stdout -notmatch 'test result: ok\. 1 passed; 0 failed; 0 ignored;') { throw 'case failed or coverage unknown' }
    if ((Get-FileHash -LiteralPath $fixed -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash -or (Get-FileHash -LiteralPath $original -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hash) { throw 'binary changed during case' }
    $after=Source-State
    Assert-Source $after
    Save-Json (Join-Path $attempt 'source-after.json') $after
    $active.state='passed'
    Save-Json (Join-Path $attempt 'case.json') $active
    Write-Gate $active
    $unsafeWindow=$false
} catch {
    if ($unsafeWindow) {
        if ($null -eq $active) { $active=[ordered]@{} }
        if ($null -ne $native) { $active['pid']=$native.Pid; $active['creation_filetime']=$native.CreationFileTime }
        $active.state='terminal_unknown'
        $active['reason']=$_.Exception.Message
        $active['detected_utc']=[DateTime]::UtcNow.ToString('o')
        $active['instruction']='Retain the original parent and do not reopen the window until independently resolved.'
        try { Write-Gate $active; Save-Json (Join-Path $attempt 'incident.json') $active; Save-Json (Join-Path $attempt 'case.json') $active } catch { [Console]::Error.WriteLine('Unable to write complete incident; retained gate remains.') }
    }
    throw
} finally {
    $sourceFailure=$null
    if ($null -ne $sourceBefore) {
        try { $last=Source-State; Save-Json (Join-Path $attempt 'source-final.json') $last; Assert-Source $last }
        catch {
            $sourceFailure=$_.Exception.Message
            $unsafeWindow=$true
            $incident=[ordered]@{state='source_unknown';reason=$sourceFailure;detected_utc=[DateTime]::UtcNow.ToString('o');attempt=$attempt;active=$active}
            try { Write-Gate $incident; Save-Json (Join-Path $attempt 'incident.json') $incident } catch { [Console]::Error.WriteLine('Unable to write source incident; retained gate remains.') }
        }
    }
    if ($null -ne $native) { $native.Thread.Dispose(); $native.Handle.Dispose() }
    $gate.Dispose()
    if (-not $unsafeWindow) { Remove-Item -LiteralPath $gatePath }
    Pop-Location
    if ($null -ne $sourceFailure) { throw "final source validation failed: $sourceFailure" }
}
