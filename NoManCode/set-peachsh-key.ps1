param(
  [Parameter(Mandatory = $true)]
  [ValidateSet(1, 2, 3)]
  [int]$Slot
)

$ErrorActionPreference = 'Stop'
$taskDataRoot = if ([string]::IsNullOrWhiteSpace($env:DSH_HOME)) {
  Join-Path $PSScriptRoot 'data'
} else {
  [IO.Path]::GetFullPath($env:DSH_HOME, $PSScriptRoot)
}
$path = Join-Path $taskDataRoot '.credentials.yaml'
if (-not (Test-Path -LiteralPath $path)) { throw "Credential store does not exist: $path" }

$secure = Read-Host "Enter the 🍑code API key for slot $Slot" -AsSecureString
$bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
try {
  $value = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr)
} finally {
  [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
}
if ([string]::IsNullOrWhiteSpace($value)) { throw 'The key cannot be empty.' }

# The local credentials provider accepts a strict version-1 document. Replace
# only this ref and preserve every other ref/record, without printing the key.
$ref = "PEACHSH_KEY_$Slot"
$escaped = $value.Replace('\', '\\').Replace('"', '\"').Replace("`r", '').Replace("`n", '')
$lines = [System.Collections.Generic.List[string]](Get-Content -LiteralPath $path)
$index = -1
for ($i = 0; $i -lt $lines.Count; $i++) {
  if ($lines[$i] -match "^\s+$([regex]::Escape($ref)):\s*") { $index = $i; break }
}
if ($index -ge 0) { $lines[$index] = ('  {0}: "{1}"' -f $ref, $escaped) }
else {
  $refsIndex = -1
  for ($i = 0; $i -lt $lines.Count; $i++) { if ($lines[$i] -eq 'refs:') { $refsIndex = $i; break } }
  if ($refsIndex -lt 0) { throw 'Credential store has no refs section.' }
  $insert = $refsIndex + 1
  while ($insert -lt $lines.Count -and ($lines[$insert] -match '^\s{2}\S' -or [string]::IsNullOrWhiteSpace($lines[$insert]))) { $insert++ }
  $lines.Insert($insert, ('  {0}: "{1}"' -f $ref, $escaped))
}

$temp = "$path.tmp-$([guid]::NewGuid().ToString('N'))"
try {
  Set-Content -LiteralPath $temp -Value ($lines -join [Environment]::NewLine) -Encoding UTF8
  Move-Item -LiteralPath $temp -Destination $path -Force
} finally {
  if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Force }
}
Write-Output "🍑code key slot $Slot saved. Restart 🍑sh harness to load it."
