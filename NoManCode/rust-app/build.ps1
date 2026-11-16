param([ValidateSet('test','web-test','live','build','check','refresh','fmt','clippy','wasm-check','environment')][string]$Action = 'build')
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-Location -LiteralPath $PSScriptRoot
if ($Action -eq 'web-test') {
    & node (Join-Path $PSScriptRoot 'tests/project-web.mjs')
    exit $LASTEXITCODE
}
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
if ($Action -eq 'environment') { return } # Dot-source to prepare the same discovered toolchain without invoking Cargo.
switch ($Action) {
    'build' {
        # Cargo resolves CARGO_TARGET_DIR and build.target (including target triples).
        # Select from this invocation, never from an assumed or previous target path.
        $taskBuildMessages = @(& cargo build --release --locked --message-format=json-render-diagnostics)
    }
    'test' {
        & cargo test --workspace --locked
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        & node (Join-Path $PSScriptRoot 'tests/project-web.mjs')
    }
    'live' { & cargo test --locked --test live -- --ignored --nocapture }
    'check' { & cargo check --workspace --locked }
    'refresh' { & cargo check --workspace }
    'fmt' { & cargo fmt --all -- --check }
    'clippy' { & cargo clippy --workspace --all-targets --locked -- -D warnings }
    'wasm-check' { & cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked }
}
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($Action -eq 'build') {
    $taskHostSource = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot 'src/main.rs'))
    $taskExecutables = @($taskBuildMessages | ForEach-Object {
        $taskBuildLine = $_
        try { $taskMessage = $taskBuildLine | ConvertFrom-Json -ErrorAction Stop } catch { Write-Host $taskBuildLine; return }
        if ($taskMessage.reason -eq 'compiler-artifact' -and
            $taskMessage.target.name -eq 'peachsh' -and
            $taskMessage.target.kind -contains 'bin' -and
            $taskMessage.profile.test -ne $true -and
            $taskMessage.target.src_path -and
            [IO.Path]::GetFullPath($taskMessage.target.src_path) -eq $taskHostSource -and
            $taskMessage.executable) {
            [IO.Path]::GetFullPath($taskMessage.executable, $PSScriptRoot)
        }
    } | Select-Object -Unique)
    if ($taskExecutables.Count -ne 1) { throw "Expected one peachsh Host executable from Cargo; found $($taskExecutables.Count). Nothing published." }
    $taskExecutable = $taskExecutables[0]
    if (-not (Test-Path -LiteralPath $taskExecutable -PathType Leaf)) { throw "Cargo Host executable is missing: $taskExecutable. Nothing published." }
    $taskOutput = Join-Path (Split-Path -Parent $PSScriptRoot) 'bin'
    New-Item -ItemType Directory -Path $taskOutput -Force | Out-Null
    $taskStaged = Join-Path $taskOutput ('.peachsh-' + [guid]::NewGuid().ToString('N') + '.exe')
    try {
        Copy-Item -LiteralPath $taskExecutable -Destination $taskStaged
        [IO.File]::Move($taskStaged, (Join-Path $taskOutput 'peachsh.exe'), $true)
    } finally {
        if (Test-Path -LiteralPath $taskStaged) { Remove-Item -LiteralPath $taskStaged }
    }
    Write-Output "Built: $taskOutput\peachsh.exe"
}
