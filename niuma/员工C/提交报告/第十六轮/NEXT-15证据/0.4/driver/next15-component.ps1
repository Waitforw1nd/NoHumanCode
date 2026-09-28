param([Parameter(Mandatory=$true)][string]$Attempt)
$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
$taskRoot=Split-Path -Parent $PSScriptRoot
$taskRustRoot=Join-Path $taskRoot 'NoManCode/rust-app'
Set-Location -LiteralPath $taskRustRoot
$env:RUSTUP_AUTO_INSTALL='0'
$env:CARGO_NET_OFFLINE='true'
$env:TMP=Join-Path $taskRoot '.local/tmp'
$env:TEMP=$env:TMP
$env:CARGO_TARGET_DIR=Join-Path $taskRoot '.local/target'
$env:CARGO_PROFILE_DEV_DEBUG='0'
$env:CARGO_PROFILE_TEST_DEBUG='0'
$env:CARGO_INCREMENTAL='0'
$env:CARGO_BUILD_JOBS='2'
$taskLogDir=Join-Path $taskRoot ('.local/logs/next15-components-'+$Attempt)
if (Test-Path -LiteralPath $taskLogDir) { throw 'Refuse overwrite of prior component evidence' }
New-Item -ItemType Directory -Force -Path $taskLogDir,$env:TMP,$env:CARGO_TARGET_DIR,(Join-Path $taskRustRoot 'target') | Out-Null
$taskMeta=[ordered]@{attempt=$Attempt;scope='authorized components only; not final five gates';start_utc=[DateTime]::UtcNow.ToString('o');head=(git rev-parse HEAD).Trim();no_man_code_tree=(git rev-parse HEAD:NoManCode).Trim();rust_tree=(git rev-parse HEAD:NoManCode/rust-app).Trim();status_before=@(git status --short);index_before=@(git diff --cached --name-only);rustc=(& rustc --version | Out-String).Trim();cargo=(& cargo --version | Out-String).Trim();cwd=$taskRustRoot;tmp=$env:TMP;target=$env:CARGO_TARGET_DIR;steps=@()}
$taskMetaPath=Join-Path $taskLogDir 'meta.json'
$taskMeta | ConvertTo-Json -Depth 7 | Set-Content -LiteralPath $taskMetaPath -Encoding utf8
if (-not $env:VSINSTALLDIR) {
  $taskVsDirectory=Join-Path $env:ProgramFiles 'Microsoft Visual Studio/2022'
  if (Test-Path -LiteralPath $taskVsDirectory) {
    $env:VSINSTALLDIR=(Get-ChildItem -LiteralPath $taskVsDirectory -Directory | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'VC/Tools/MSVC') } | Select-Object -First 1).FullName
  }
}
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $taskRustRoot
$taskCandidates = @($env:VCToolsInstallDir)
$taskVsRoots = @($env:VSINSTALLDIR)
$taskVswhere = Get-Command 'vswhere.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
$taskVswherePath = if ($taskVswhere) { $taskVswhere.Source } else { $null }
if (-not $taskVswherePath) {
    foreach ($taskProgramFiles in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if (-not $taskProgramFiles) { continue }
        $taskProbe = Join-Path $taskProgramFiles 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (Test-Path -LiteralPath $taskProbe -PathType Leaf) { $taskVswherePath = $taskProbe; break }
    }
}
if ($taskVswherePath) {
    $taskVsRoots += @(& $taskVswherePath -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
}
foreach ($taskRegistry in @('HKLM:\SOFTWARE\Microsoft\VisualStudio\SxS\VS7', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\VisualStudio\SxS\VS7')) {
    $taskInstalledVs = Get-ItemProperty -LiteralPath $taskRegistry -ErrorAction SilentlyContinue
    if ($taskInstalledVs) {
        $taskVsRoots += @($taskInstalledVs.PSObject.Properties | Where-Object { $_.Name -match '^\d+\.\d+$' } | Sort-Object { [version]$_.Name } -Descending | ForEach-Object { $_.Value })
    }
}
foreach ($taskVsRoot in ($taskVsRoots | Where-Object { $_ } | Select-Object -Unique)) {
    $taskVersionRoot = Join-Path $taskVsRoot 'VC\Tools\MSVC'
    if (Test-Path -LiteralPath $taskVersionRoot -PathType Container) {
        $taskCandidates += @(Get-ChildItem -LiteralPath $taskVersionRoot -Directory | Sort-Object { $_.Name -as [version] } -Descending | ForEach-Object { $_.FullName })
    }
}
$taskMsvc = $null
foreach ($taskCandidate in ($taskCandidates | Where-Object { $_ } | Select-Object -Unique)) {
    foreach ($taskHost in @('Hostx64', 'Hostx86')) {
        $taskBin = Join-Path $taskCandidate "bin\$taskHost\x64"
        if ((Test-Path -LiteralPath (Join-Path $taskBin 'link.exe') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $taskBin 'cl.exe') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $taskBin 'lib.exe') -PathType Leaf) -and
            (Test-Path -LiteralPath (Join-Path $taskCandidate 'lib\x64') -PathType Container) -and
            (Test-Path -LiteralPath (Join-Path $taskCandidate 'include') -PathType Container)) {
            $taskMsvc = $taskCandidate
            break
        }
    }
    if ($taskMsvc) { break }
}
if (-not $taskMsvc) { throw 'Missing Visual C++ x64 build tools. Install the C++ build tools for Visual Studio, or set VCToolsInstallDir / VSINSTALLDIR to their installation.' }
$taskSdkRoots = @($env:WindowsSdkDir, $env:UniversalCRTSdkDir)
foreach ($taskRegistry in @('HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots')) {
    $taskInstalledSdk = Get-ItemProperty -LiteralPath $taskRegistry -ErrorAction SilentlyContinue
    if ($taskInstalledSdk.KitsRoot10) { $taskSdkRoots += $taskInstalledSdk.KitsRoot10 }
}
foreach ($taskRegistry in @('HKLM:\SOFTWARE\Microsoft\Microsoft SDKs\Windows\v10.0', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Microsoft SDKs\Windows\v10.0')) {
    $taskInstalledSdk = Get-ItemProperty -LiteralPath $taskRegistry -ErrorAction SilentlyContinue
    if ($taskInstalledSdk.InstallationFolder) { $taskSdkRoots += $taskInstalledSdk.InstallationFolder }
}
$taskSdk = $null
foreach ($taskSdkRoot in ($taskSdkRoots | Where-Object { $_ } | Select-Object -Unique)) {
    $taskSdkLib = Join-Path $taskSdkRoot 'Lib'
    if (-not (Test-Path -LiteralPath $taskSdkLib -PathType Container)) { continue }
    $taskSdk = Get-ChildItem -LiteralPath $taskSdkLib -Directory | Sort-Object { $_.Name -as [version] } -Descending | Where-Object {
        (Test-Path -LiteralPath (Join-Path $_.FullName 'um\x64\kernel32.Lib') -PathType Leaf) -and
        (Test-Path -LiteralPath (Join-Path $_.FullName 'ucrt\x64\ucrt.lib') -PathType Leaf) -and
        (Test-Path -LiteralPath (Join-Path $taskSdkRoot "Include\$($_.Name)\ucrt") -PathType Container) -and
        (Test-Path -LiteralPath (Join-Path $taskSdkRoot "Include\$($_.Name)\shared") -PathType Container) -and
        (Test-Path -LiteralPath (Join-Path $taskSdkRoot "Include\$($_.Name)\um") -PathType Container)
    } | Select-Object -First 1
    if ($taskSdk) { break }
}
if (-not $taskSdk) { throw 'Missing Windows SDK x64 libraries or headers. Install the Windows SDK, or set WindowsSdkDir to its installation.' }
$env:PATH = "$taskBin;$env:PATH"
$env:LIB = "$taskMsvc\lib\x64;$($taskSdk.FullName)\ucrt\x64;$($taskSdk.FullName)\um\x64"
$env:INCLUDE = "$taskMsvc\include;$taskSdkRoot\Include\$($taskSdk.Name)\ucrt;$taskSdkRoot\Include\$($taskSdk.Name)\shared;$taskSdkRoot\Include\$($taskSdk.Name)\um"
$env:CC_x86_64_pc_windows_msvc = Join-Path $taskBin 'cl.exe'
$env:AR_x86_64_pc_windows_msvc = Join-Path $taskBin 'lib.exe'

$taskMeta.msvc=$taskMsvc
$taskMeta.sdk=$taskSdkRoot
$taskMeta.sdk_version=$taskSdk.Name
$taskMeta.compiler=$env:CC_x86_64_pc_windows_msvc
$taskMeta | ConvertTo-Json -Depth 7 | Set-Content -LiteralPath $taskMetaPath -Encoding utf8
function Invoke-Component {
  param([string]$Name,[string[]]$CargoArguments)
  $started=[DateTime]::UtcNow.ToString('o')
  Write-Output ('START '+$Name+' '+$started)
  & cargo @CargoArguments *> (Join-Path $taskLogDir ($Name+'.log'))
  $exitCode=$LASTEXITCODE
  $taskMeta.steps += [ordered]@{name=$Name;command='cargo '+($CargoArguments -join ' ');start_utc=$started;end_utc=[DateTime]::UtcNow.ToString('o');exit_code=$exitCode}
  $taskMeta.status_after=@(git status --short)
  $taskMeta.index_after=@(git diff --cached --name-only)
  $taskMeta.head_after=(git rev-parse HEAD).Trim()
  $taskMeta.no_man_code_tree_after=(git rev-parse HEAD:NoManCode).Trim()
  $taskMeta.rust_tree_after=(git rev-parse HEAD:NoManCode/rust-app).Trim()
  $taskMeta | ConvertTo-Json -Depth 7 | Set-Content -LiteralPath $taskMetaPath -Encoding utf8
  Write-Output ('END '+$Name+' exit='+$exitCode)
  if ($exitCode -ne 0) { Get-Content -LiteralPath (Join-Path $taskLogDir ($Name+'.log')) -Tail 85; exit $exitCode }
}
Invoke-Component -Name check -CargoArguments @('check','-p','peachsh','--bin','peachsh','--locked','--offline')
Invoke-Component -Name http -CargoArguments @('test','--locked','--offline','--test','host_configuration_http','--','--nocapture')
Invoke-Component -Name cli -CargoArguments @('test','--locked','--offline','--test','host_configuration_cli','--','--nocapture')
Invoke-Component -Name workflow -CargoArguments @('test','--locked','--offline','--test','host_initialization_workflow','--','--nocapture')