param(
    [string]$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../../../')).Path,
    [switch]$CheckEntry
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $RepoRoot -PathType Container)) { throw 'Repository root does not exist.' }
$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
$taskSourceRelative = 'NoManCode/rust-app/build.ps1'
$SourceScript = Join-Path $RepoRoot $taskSourceRelative
if (-not (Test-Path -LiteralPath $SourceScript -PathType Leaf)) { throw "Source script does not exist: $taskSourceRelative" }
if ($CheckEntry) {
    Write-Output "Entry OK: $taskSourceRelative"
    return
}
$FixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('nhc-build-portable-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $FixtureRoot -Force | Out-Null
$global:taskMockBuildScript = Join-Path $FixtureRoot 'build.ps1'
Copy-Item -LiteralPath $SourceScript -Destination $global:taskMockBuildScript
$global:taskMockRegistry = @{}
$global:taskMockVswhereSource = $null
$global:taskMockCargoCalls = @()
function Get-Command {
    [CmdletBinding()] param([string]$Name)
    if ($Name -ne 'vswhere.exe') { throw "Unexpected discovery command: $Name" }
    if ($global:taskMockVswhereSource) { [pscustomobject]@{Source = $global:taskMockVswhereSource} }
}
function Get-ItemProperty {
    [CmdletBinding()] param([string]$LiteralPath)
    if ($LiteralPath -notlike 'HKLM:\*') { throw "Unexpected registry path: $LiteralPath" }
    if ($global:taskMockRegistry.ContainsKey($LiteralPath)) { $global:taskMockRegistry[$LiteralPath] }
}
function cargo {
    $global:taskMockCargoCalls += [pscustomobject]@{ Arguments = ($args -join ' '); Compiler = $env:CC_x86_64_pc_windows_msvc; Archiver = $env:AR_x86_64_pc_windows_msvc; Lib = $env:LIB; Include = $env:INCLUDE }
    $global:LASTEXITCODE = 0
}
function New-FakeMsvc([string]$Root, [string]$HostName = 'Hostx64') {
    foreach ($relative in @("bin\$HostName\x64", 'lib\x64', 'include')) { New-Item -ItemType Directory -Path (Join-Path $Root $relative) -Force | Out-Null }
    foreach ($file in @('cl.exe','link.exe','lib.exe')) { Set-Content -LiteralPath (Join-Path $Root "bin\$HostName\x64\$file") -Value 'mock; never execute' }
    return $Root
}
function New-FakeSdk([string]$Root, [string]$Version) {
    foreach ($relative in @("Lib\$Version\um\x64","Lib\$Version\ucrt\x64","Include\$Version\um","Include\$Version\ucrt","Include\$Version\shared")) { New-Item -ItemType Directory -Path (Join-Path $Root $relative) -Force | Out-Null }
    Set-Content -LiteralPath (Join-Path $Root "Lib\$Version\um\x64\kernel32.Lib") -Value 'mock'
    Set-Content -LiteralPath (Join-Path $Root "Lib\$Version\ucrt\x64\ucrt.lib") -Value 'mock'
}
function Reset-Discovery {
    $env:VCToolsInstallDir = ''; $env:VSINSTALLDIR = ''; $env:WindowsSdkDir = ''; $env:UniversalCRTSdkDir = ''
    [Environment]::SetEnvironmentVariable('ProgramFiles(x86)', $FixtureRoot, 'Process')
    $env:ProgramFiles = $FixtureRoot
    $global:taskMockRegistry = @{}; $global:taskMockVswhereSource = $null; $global:taskMockCargoCalls = @()
}
function Assert-Discovery([string]$Label, [string]$Msvc, [string]$Sdk, [string]$Version, [string]$HostName = 'Hostx64') {
    & $global:taskMockBuildScript -Action check
    if ($global:taskMockCargoCalls.Count -ne 1) { throw "$Label : wrong Cargo invocation count." }
    $call = $global:taskMockCargoCalls[0]
    if ($call.Arguments -ne 'check --workspace --locked') { throw "$Label : Action changed." }
    if ($call.Compiler -ne (Join-Path $Msvc "bin\$HostName\x64\cl.exe")) { throw "$Label : wrong compiler $($call.Compiler)" }
    if ($call.Archiver -ne (Join-Path $Msvc "bin\$HostName\x64\lib.exe")) { throw "$Label : wrong archiver." }
    if ($call.Lib -ne "$Msvc\lib\x64;$Sdk\Lib\$Version\ucrt\x64;$Sdk\Lib\$Version\um\x64") { throw "$Label : wrong library paths $($call.Lib)" }
    if ($call.Include -ne "$Msvc\include;$Sdk\Include\$Version\ucrt;$Sdk\Include\$Version\shared;$Sdk\Include\$Version\um") { throw "$Label : wrong include paths." }
    "PASS: $Label"
}
function Assert-Missing([string]$Label, [string]$Expected) {
    $actual = $null
    try { & $global:taskMockBuildScript -Action check } catch { $actual = $_.Exception.Message }
    if ($actual -notlike "$Expected*") { throw "$Label : unexpected failure $actual" }
    if ($global:taskMockCargoCalls.Count) { throw "$Label : Cargo must not run on missing prerequisites." }
    "PASS: $Label"
}
$envMsvc = New-FakeMsvc (Join-Path $FixtureRoot 'explicit tool chain')
$vsRoot = Join-Path $FixtureRoot 'visual studio'
$oldMsvc = New-FakeMsvc (Join-Path $vsRoot 'VC\Tools\MSVC\14.9.1')
$newMsvc = New-FakeMsvc (Join-Path $vsRoot 'VC\Tools\MSVC\14.10.1') 'Hostx86'
$badMsvc = Join-Path $vsRoot 'VC\Tools\MSVC\14.11.1'
New-Item -ItemType Directory -Path (Join-Path $badMsvc 'bin\Hostx64\x64') -Force | Out-Null
Set-Content -LiteralPath (Join-Path $badMsvc 'bin\Hostx64\x64\link.exe') -Value 'incomplete'
$sdkRoot = Join-Path $FixtureRoot 'windows kits'
New-FakeSdk $sdkRoot '10.0.9999.0'
New-FakeSdk $sdkRoot '10.0.10000.0'
New-Item -ItemType Directory -Path (Join-Path $sdkRoot 'Lib\10.0.20000.0\um\x64') -Force | Out-Null
Set-Content -LiteralPath (Join-Path $sdkRoot 'Lib\10.0.20000.0\um\x64\kernel32.Lib') -Value 'incomplete'
Reset-Discovery
$env:VCToolsInstallDir = $envMsvc; $env:VSINSTALLDIR = $vsRoot; $env:WindowsSdkDir = $sdkRoot
Assert-Discovery 'explicit env precedence / paths with spaces / complete numeric SDK selection' $envMsvc $sdkRoot '10.0.10000.0'
Reset-Discovery
$env:VSINSTALLDIR = $vsRoot; $env:UniversalCRTSdkDir = $sdkRoot
Assert-Discovery 'VSINSTALLDIR numeric version selection / incomplete version skipped / Hostx86 fallback' $newMsvc $sdkRoot '10.0.10000.0' 'Hostx86'
Reset-Discovery
$global:taskMockVswhereSource = Join-Path $FixtureRoot 'mock-vswhere.cmd'
Set-Content -LiteralPath $global:taskMockVswhereSource -Value @('@echo off', "echo $vsRoot") -Encoding ascii
$global:taskMockRegistry['HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots'] = [pscustomobject]@{KitsRoot10 = $sdkRoot}
Assert-Discovery 'vswhere discovery / SDK Installed Roots registry' $newMsvc $sdkRoot '10.0.10000.0' 'Hostx86'
Reset-Discovery
$global:taskMockRegistry['HKLM:\SOFTWARE\WOW6432Node\Microsoft\VisualStudio\SxS\VS7'] = [pscustomobject]@{'17.0' = $vsRoot}
$global:taskMockRegistry['HKLM:\SOFTWARE\WOW6432Node\Microsoft\Microsoft SDKs\Windows\v10.0'] = [pscustomobject]@{InstallationFolder = $sdkRoot}
Assert-Discovery 'MSVC and SDK 32-bit registry fallback' $newMsvc $sdkRoot '10.0.10000.0' 'Hostx86'
Reset-Discovery
$env:VCToolsInstallDir = $badMsvc; $env:WindowsSdkDir = $sdkRoot
Assert-Missing 'missing complete MSVC rejects before Cargo' 'Missing Visual C++ x64 build tools.'
Reset-Discovery
$env:VCToolsInstallDir = $envMsvc; $env:WindowsSdkDir = Join-Path $FixtureRoot 'missing sdk'
Assert-Missing 'missing SDK rejects before Cargo' 'Missing Windows SDK x64 libraries or headers.'
'No real Cargo, compiler, SDK executable, or product process was invoked.'
