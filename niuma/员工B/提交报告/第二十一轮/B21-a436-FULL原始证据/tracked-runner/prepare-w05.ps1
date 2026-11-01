param(
    [Parameter(Mandatory=$true)][string]$Probe,
    [Parameter(Mandatory=$true)][string]$Destination
)
$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $Probe).Path
if (Test-Path -LiteralPath $Destination) { throw 'W05 destination must not exist' }
New-Item -ItemType Directory -Path $Destination | Out-Null
$dest = (Resolve-Path -LiteralPath $Destination).Path
$hash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()
$entries = foreach ($scenario in @('resumed', 'pending')) {
    $dir = Join-Path $dest $scenario
    New-Item -ItemType Directory -Path $dir | Out-Null
    $copy = Join-Path $dir 'no-read.exe'
    Copy-Item -LiteralPath $source -Destination $copy
    $actual = (Get-FileHash -LiteralPath $copy -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $hash) { throw "W05 probe copy mismatch: $scenario" }
    [pscustomobject]@{scenario=$scenario; bytes=(Get-Item -LiteralPath $copy).Length; sha256=$actual}
}
[pscustomobject]@{source=$source; source_sha256=$hash; copies=$entries} |
    ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $dest 'manifest.json') -Encoding utf8
