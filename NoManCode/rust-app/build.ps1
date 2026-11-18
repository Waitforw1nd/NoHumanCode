param([ValidateSet('test','web-test','live','build','check','refresh','fmt','clippy','environment')][string]$Action = 'build')
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-Location -LiteralPath $PSScriptRoot
if ($Action -eq 'web-test') {
    & node (Join-Path $PSScriptRoot 'tests/project-web.mjs')
    exit $LASTEXITCODE
}
$taskCandidates = @($env:VCToolsInstallDir)
$taskVsRoots = @($env:VSINSTALLDIR)
foreach ($pf in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
    if (-not $pf) { continue }
    foreach ($ed in @('Community', 'Professional', 'Enterprise', 'BuildTools')) {
        $probe = Join-Path $pf "Microsoft Visual Studio\2022\$ed"
        if (Test-Path -LiteralPath $probe -PathType Container) { $taskVsRoots += $probe }
    }
}
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
            (Test-Path -LiteralPath (Join-Path $taskBin 'cl.exe') -PathType Leaf)) {
            $taskMsvc = $taskCandidate
            break
        }
    }
    if ($taskMsvc) { break }
}
if (-not $taskMsvc) { throw 'Missing Visual C++ x64 build tools.' }
$taskSdkRoots = @($env:WindowsSdkDir, $env:UniversalCRTSdkDir, 'C:\Program Files (x86)\Windows Kits\10')
foreach ($taskRegistry in @('HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots')) {
    $taskInstalledSdk = Get-ItemProperty -LiteralPath $taskRegistry -ErrorAction SilentlyContinue
    if ($taskInstalledSdk.KitsRoot10) { $taskSdkRoots += $taskInstalledSdk.KitsRoot10 }
}
$taskSdk = $null
foreach ($taskSdkRoot in ($taskSdkRoots | Where-Object { $_ } | Select-Object -Unique)) {
    $taskSdkLib = Join-Path $taskSdkRoot 'Lib'
    if (-not (Test-Path -LiteralPath $taskSdkLib -PathType Container)) { continue }
    $taskSdk = Get-ChildItem -LiteralPath $taskSdkLib -Directory | Sort-Object { $_.Name -as [version] } -Descending | Where-Object {
        (Test-Path -LiteralPath (Join-Path $_.FullName 'um\x64\kernel32.Lib') -PathType Leaf) -and
        (Test-Path -LiteralPath (Join-Path $_.FullName 'ucrt\x64\ucrt.lib') -PathType Leaf)
    } | Select-Object -First 1
    if ($taskSdk) { break }
}
if (-not $taskSdk) { throw 'Missing Windows SDK x64 libraries or headers.' }
$env:PATH = "$taskBin;$env:PATH"
$env:LIB = "$taskMsvc\lib\x64;$($taskSdk.FullName)\ucrt\x64;$($taskSdk.FullName)\um\x64"
$env:INCLUDE = "$taskMsvc\include;$taskSdkRoot\Include\$($taskSdk.Name)\ucrt;$taskSdkRoot\Include\$($taskSdk.Name)\shared;$taskSdkRoot\Include\$($taskSdk.Name)\um"
$env:CC_x86_64_pc_windows_msvc = Join-Path $taskBin 'cl.exe'
$env:AR_x86_64_pc_windows_msvc = Join-Path $taskBin 'lib.exe'
if ($Action -eq 'environment') { return }
switch ($Action) {
    'build' { & cargo build --release --locked }
    'test' { & cargo test --workspace --locked }
    'live' { & cargo test --locked --test live -- --ignored --nocapture }
    'check' { & cargo check --workspace --all-targets --locked }
    'refresh' { & cargo check --workspace }
    'fmt' { & cargo fmt --all -- --check }
    'clippy' { & cargo clippy --workspace --all-targets --locked -- -D warnings }
}
exit $LASTEXITCODE
