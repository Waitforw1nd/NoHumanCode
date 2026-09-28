param([Parameter(Mandatory=$true)][string]$Label, [Parameter(Mandatory=$true)][string[]]$CargoArgs)
$ErrorActionPreference = 'Stop'
$taskRust = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Set-Location -LiteralPath $taskRust
$taskOut = Join-Path $PSScriptRoot $Label
if (Test-Path -LiteralPath $taskOut) { throw 'Evidence label already exists' }
New-Item -ItemType Directory -Path $taskOut | Out-Null
$env:TMP = Join-Path $PSScriptRoot 'tmp'
$env:TEMP = $env:TMP
$env:CARGO_TARGET_DIR = Join-Path $PSScriptRoot 'target'
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_BUILD_JOBS = '2'
New-Item -ItemType Directory -Force -Path $env:TMP,$env:CARGO_TARGET_DIR,(Join-Path $taskRust 'target') | Out-Null
$taskMeta = [ordered]@{utc_start=[DateTime]::UtcNow.ToString('o'); cwd=$taskRust; command=('cargo ' + ($CargoArgs -join ' ')); head=(& git rev-parse HEAD); source_tree=(& git rev-parse HEAD:NoManCode); rust_tree=(& git rev-parse HEAD:NoManCode/rust-app); status=@(& git status --short); index=@(& git diff --cached --name-status); source_hashes=@(Get-ChildItem -LiteralPath (Join-Path $taskRust 'src') -File | ForEach-Object { @{path=$_.Name;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash} }); invoked=$false}
$taskCode=1
try {
    $taskSetup = [IO.File]::ReadAllText((Join-Path $taskRust 'build.ps1'))
    $taskBoundary = $taskSetup.IndexOf('switch ($Action)')
    if ($taskBoundary -lt 0) { throw 'Build setup boundary not found' }
    $taskPrefix = $taskSetup.Substring(0,$taskBoundary).Replace('Set-Location -LiteralPath $PSScriptRoot','')
    . ([scriptblock]::Create($taskPrefix)) -Action check
    $taskMeta['rustc']=(& rustc --version)
    $taskMeta['cargo']=(& cargo --version)
    $taskMeta['msvc']=$taskMsvc
    $taskMeta['sdk']=$taskSdkRoot
    $taskMeta['target']=$env:CARGO_TARGET_DIR
    $taskMeta['temp']=$env:TEMP
    $taskMeta['invoked']=$true
    & cargo @CargoArgs *> (Join-Path $taskOut 'raw.log')
    $taskCode=$LASTEXITCODE
} catch {
    $_ | Out-String | Set-Content -LiteralPath (Join-Path $taskOut 'environment-error.log') -Encoding utf8
} finally {
    $taskMeta['exit_code']=$taskCode
    $taskMeta['utc_end']=[DateTime]::UtcNow.ToString('o')
    $taskMeta | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $taskOut 'metadata.json') -Encoding utf8
    Write-Output "$Label exit=$taskCode invoked=$($taskMeta['invoked'])"
    if (Test-Path -LiteralPath (Join-Path $taskOut 'raw.log')) { Get-Content -LiteralPath (Join-Path $taskOut 'raw.log') -Tail 65 }
    if (Test-Path -LiteralPath (Join-Path $taskOut 'environment-error.log')) { Get-Content -LiteralPath (Join-Path $taskOut 'environment-error.log') }
}
exit $taskCode
