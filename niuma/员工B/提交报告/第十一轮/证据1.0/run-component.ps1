param([string]$Log,[string[]]$CargoArguments)
$ErrorActionPreference='Stop'
Set-Location (Join-Path $PSScriptRoot '../NoManCode/rust-app')
$env:TMP=(Resolve-Path ../../.local/tmp).Path
$env:TEMP=$env:TMP
$env:CARGO_TARGET_DIR=(Resolve-Path ../../.local/target-r11).Path
$env:CARGO_PROFILE_DEV_DEBUG='0'
$env:CARGO_PROFILE_TEST_DEBUG='0'
$env:CARGO_INCREMENTAL='0'
$env:CARGO_BUILD_JOBS='2'
$env:VSINSTALLDIR='<MSVC_ROOT>'
$taskScript=[IO.File]::ReadAllText((Join-Path (Get-Location) 'build.ps1'))
$taskSetup=$taskScript.Substring($taskScript.IndexOf('$ErrorActionPreference'),$taskScript.IndexOf('switch ($Action)')-$taskScript.IndexOf('$ErrorActionPreference')).Replace('Set-Location -LiteralPath $PSScriptRoot','')
Invoke-Expression $taskSetup
$taskStart=[DateTime]::UtcNow.ToString('o')
& cargo @CargoArguments *> $Log
$taskExit=$LASTEXITCODE
@{start_utc=$taskStart;end_utc=[DateTime]::UtcNow.ToString('o');command=('cargo '+($CargoArguments -join ' '));exit=$taskExit;head=(& git rev-parse HEAD);tmp=$env:TMP;target=$env:CARGO_TARGET_DIR;msvc=$taskMsvc;status=(& git status --short)} | ConvertTo-Json | Set-Content ($Log+'.json')
Get-Content $Log -Tail 35
exit $taskExit
