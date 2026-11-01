param(
    [Parameter(Mandatory=$true)][string]$Probe,
    [Parameter(Mandatory=$true)][string]$Destination
)
$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $Probe).Path
if (Test-Path -LiteralPath $Destination) { throw 'Destination must not exist' }
New-Item -ItemType Directory -Path $Destination | Out-Null
$dest = (Resolve-Path -LiteralPath $Destination).Path
$hash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()
$modes = @('control','short-header','short-header-1','short-header-2','short-payload','zero-length','long-length','bad-utf8','bad-json','unknown-field','duplicate-field','trailing','duplicate-frame','id-version','id-request','id-nonce','id-generation','id-plugin','id-plugin-version','id-digest','id-fuel','id-memory','decision-phase','decision-index','decision-duplicate','decision-order','decision-no-tools','decision-votes','rejected','exit-output','exit-empty','hold-valid','hold-prefix','delayed','zero-stdout','zero-stderr','stdout-limit','stderr-limit','no-read','partial-read','partial-stdout','closed-stdout','concurrent','dual-pressure','job-cpu','job-memory','p01-memory','w06-sentinel','w06-job','valid-output','valid-stderr')
$entries = foreach ($mode in $modes) {
    $copy = Join-Path $dest "$mode.exe"
    Copy-Item -LiteralPath $source -Destination $copy
    $actual = (Get-FileHash -LiteralPath $copy -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $hash) { throw "Probe copy mismatch: $mode" }
    [pscustomobject]@{mode=$mode; bytes=(Get-Item -LiteralPath $copy).Length; sha256=$actual}
}
[pscustomobject]@{source=$source; source_sha256=$hash; copies=$entries} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $dest 'manifest.json') -Encoding utf8
