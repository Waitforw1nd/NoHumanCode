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
