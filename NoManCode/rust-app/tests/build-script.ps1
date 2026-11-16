param([string]$BuildScript = (Join-Path $PSScriptRoot '../build.ps1'))
$ErrorActionPreference = 'Stop'
$taskScript = (Resolve-Path -LiteralPath $BuildScript).Path
$taskRoot = Join-Path (Split-Path -Parent $PSScriptRoot) ('.local/build-script-tests/' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $taskRoot -Force | Out-Null
$taskRoot = (Resolve-Path -LiteralPath $taskRoot).Path
$taskPwsh = (Get-Process -Id $PID).Path
$taskMsvc = Join-Path $taskRoot 'mock-msvc'
$taskSdk = Join-Path $taskRoot 'mock-sdk'
foreach ($relative in @('bin/Hostx64/x64/link.exe','bin/Hostx64/x64/cl.exe','bin/Hostx64/x64/lib.exe','lib/x64/placeholder','include/placeholder')) {
    $path = Join-Path $taskMsvc $relative
    New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force | Out-Null
    [IO.File]::WriteAllText($path, '')
}
foreach ($relative in @('Lib/10.0.1/um/x64/kernel32.Lib','Lib/10.0.1/ucrt/x64/ucrt.lib','Include/10.0.1/ucrt/placeholder','Include/10.0.1/shared/placeholder','Include/10.0.1/um/placeholder')) {
    $path = Join-Path $taskSdk $relative
    New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force | Out-Null
    [IO.File]::WriteAllText($path, '')
}
$taskHarness = Join-Path $taskRoot 'harness.ps1'
@'
param([string]$Spec)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$global:taskSpec = Get-Content -Raw -LiteralPath $Spec | ConvertFrom-Json
function global:Get-ItemProperty { } # No installed SDK or registry dependency.
function global:vswhere.exe { }
function global:cargo {
    [IO.File]::AppendAllText($taskSpec.calls, ((@{tool='cargo'; arguments=@($args); cwd=(Get-Location).Path; target_dir=$env:CARGO_TARGET_DIR; build_target=$env:CARGO_BUILD_TARGET} | ConvertTo-Json -Compress) + "`n"))
    if ($args[0] -eq 'build') {
        $artifact = @{reason='compiler-artifact'; target=@{name='peachsh'; kind=@('bin'); src_path=(Join-Path $taskSpec.root 'src/main.rs')}; profile=@{test=$false}; executable=$taskSpec.artifact; fresh=$false}
        # Distractors must never be selected for publishing the Host.
        @{reason='compiler-artifact'; target=@{name='peachsh-wasm-worker'; kind=@('bin')}; profile=@{test=$false}; executable='wrong-worker.exe'} | ConvertTo-Json -Depth 5 -Compress
        if ($taskSpec.scenario -ne 'missing') {
            if ($taskSpec.scenario -ne 'no-file') {
                New-Item -ItemType Directory -Path (Split-Path -Parent $taskSpec.artifact) -Force | Out-Null
                [IO.File]::WriteAllText($taskSpec.artifact, 'fresh-host')
            }
            if ($taskSpec.scenario -eq 'wrong-package') { $artifact.target.src_path = Join-Path $taskSpec.root 'dependency/src/main.rs' }
            $artifact | ConvertTo-Json -Depth 5 -Compress
            if ($taskSpec.scenario -eq 'ambiguous') {
                $artifact.executable = $taskSpec.artifact + '.other.exe'
                $artifact | ConvertTo-Json -Depth 5 -Compress
            }
        }
        @{reason='build-finished'; success=($taskSpec.cargoExit -eq 0)} | ConvertTo-Json -Compress
    }
    if ($taskSpec.cargoExit -ne 0) {
        # Exercise native nonzero exit under the caller's Stop/true preferences.
        & $env:ComSpec /d /c 'exit 101'
        $global:LASTEXITCODE = $LASTEXITCODE
    } else { $global:LASTEXITCODE = 0 }
}
function global:node {
    [IO.File]::AppendAllText($taskSpec.calls, ((@{tool='node'; arguments=@($args)} | ConvertTo-Json -Compress) + "`n"))
    $global:LASTEXITCODE = $taskSpec.nodeExit
}
if ($taskSpec.scenario -eq 'locked-output') {
    $taskLock = [IO.File]::Open((Join-Path $taskSpec.root '../bin/peachsh.exe'), 'Open', 'Read', 'Read')
}
& $taskSpec.script -Action $taskSpec.action
exit $LASTEXITCODE
'@ | Set-Content -LiteralPath $taskHarness -Encoding utf8

$taskCases = @(
    @{name='absolute-target'; action='build'; scenario='ok'; target='absolute'; expected=0},
    @{name='relative-target'; action='build'; scenario='ok'; target='relative'; expected=0},
    @{name='target-triple'; action='build'; scenario='ok'; target='relative'; triple='x86_64-pc-windows-msvc'; expected=0},
    @{name='cargo-failure'; action='build'; scenario='ok'; target='absolute'; cargoExit=101; expected=101},
    @{name='missing-artifact'; action='build'; scenario='missing'; target='absolute'; expected=1},
    @{name='ambiguous-artifact'; action='build'; scenario='ambiguous'; target='absolute'; expected=1},
    @{name='wrong-package'; action='build'; scenario='wrong-package'; target='absolute'; expected=1},
    @{name='missing-file'; action='build'; scenario='no-file'; target='absolute'; expected=1},
    @{name='locked-output'; action='build'; scenario='locked-output'; target='absolute'; expected=1},
    @{name='cargo-test-failure'; action='test'; cargoExit=101; expected=101; tools=@('cargo')},
    @{name='web-test-failure'; action='test'; nodeExit=7; expected=7; tools=@('cargo','node')},
    @{name='daily-gate'; action='test'; expected=0; tools=@('cargo','node')},
    @{name='web-only'; action='web-test'; expected=0; tools=@('node')}
)
$taskFailures = @()
foreach ($case in $taskCases) {
    $site = Join-Path $taskRoot $case.name
    $root = Join-Path $site 'product/rust-app'
    New-Item -ItemType Directory -Path (Join-Path $root 'src') -Force | Out-Null
    Copy-Item -LiteralPath $taskScript -Destination (Join-Path $root 'build.ps1')
    [IO.File]::WriteAllText((Join-Path $root 'src/main.rs'), '// mock Host')
    $stale = Join-Path $root 'target/release/peachsh.exe'
    $published = Join-Path $site 'product/bin/peachsh.exe'
    foreach ($path in @($stale, $published)) { New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force | Out-Null }
    [IO.File]::WriteAllText($stale, 'stale-default')
    [IO.File]::WriteAllText($published, 'published-before')
    $target = if ($case.target -eq 'relative') { 'custom output' } else { Join-Path $site 'absolute output' }
    $targetRoot = [IO.Path]::GetFullPath($target, $root)
    $artifact = Join-Path $targetRoot (($case.triple + '/release/peachsh.exe').TrimStart('/'))
    $spec = @{root=$root; script=(Join-Path $root 'build.ps1'); action=$case.action; artifact=$artifact; scenario=$case.scenario; cargoExit=[int]$case.cargoExit; nodeExit=[int]$case.nodeExit; calls=(Join-Path $site 'calls.jsonl')}
    $specPath = Join-Path $site 'spec.json'
    $spec | ConvertTo-Json | Set-Content -LiteralPath $specPath -Encoding utf8
    $psi = [Diagnostics.ProcessStartInfo]::new($taskPwsh)
    foreach ($arg in @('-NoProfile','-File',$taskHarness,'-Spec',$specPath)) { $psi.ArgumentList.Add($arg) }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.WorkingDirectory = $taskRoot # Caller cwd deliberately differs from the project.
    $psi.Environment['VCToolsInstallDir'] = $taskMsvc
    $psi.Environment['WindowsSdkDir'] = $taskSdk
    $psi.Environment['VSINSTALLDIR'] = ''
    $psi.Environment['UniversalCRTSdkDir'] = ''
    $psi.Environment['ProgramFiles'] = $taskRoot
    $psi.Environment['ProgramFiles(x86)'] = $taskRoot
    $psi.Environment['CARGO_TARGET_DIR'] = $target
    $psi.Environment['CARGO_BUILD_TARGET'] = [string]$case.triple
    $process = [Diagnostics.Process]::Start($psi)
    $outTask = $process.StandardOutput.ReadToEndAsync()
    $errTask = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(15000)) { throw "Mock-only harness timed out: $($case.name); evidence: $site" }
    $stdout = $outTask.GetAwaiter().GetResult()
    $stderr = $errTask.GetAwaiter().GetResult()
    [IO.File]::WriteAllText((Join-Path $site 'stdout.txt'), $stdout)
    [IO.File]::WriteAllText((Join-Path $site 'stderr.txt'), $stderr)
    try {
        if ($process.ExitCode -ne $case.expected) { throw "exit $($process.ExitCode), expected $($case.expected)" }
        $expectedContent = if ($case.action -eq 'build' -and $case.expected -eq 0) { 'fresh-host' } else { 'published-before' }
        if ([IO.File]::ReadAllText($published) -cne $expectedContent) { throw 'wrong published bytes' }
        if (@(Get-ChildItem -LiteralPath (Split-Path -Parent $published) -Filter '.peachsh-*.exe').Count -ne 0) { throw 'staged publication file leaked' }
        $calls = @(Get-Content -LiteralPath $spec.calls | ForEach-Object { $_ | ConvertFrom-Json })
        if ($case.tools -and (($calls.tool -join ',') -cne ($case.tools -join ','))) { throw "wrong call order: $($calls.tool -join ',')" }
        if ($case.action -eq 'build') {
            if ($calls.Count -ne 1 -or $calls[0].tool -ne 'cargo') { throw 'expected one Cargo invocation' }
            if ($calls[0].arguments -notcontains '--message-format=json-render-diagnostics') { throw 'Cargo artifact output not requested' }
            if ($calls[0].cwd -ne $root -or $calls[0].target_dir -ne $target -or $calls[0].build_target -ne [string]$case.triple) { throw 'caller target configuration changed' }
        }
        if ($calls.tool -contains 'node') {
            $nodeCall = @($calls | Where-Object tool -eq 'node')[0]
            if ($nodeCall.arguments[0] -ne (Join-Path $root 'tests/project-web.mjs')) { throw 'wrong web behavior test selected' }
        }
        Write-Output "PASS $($case.name)"
    } catch {
        $taskFailures += "$($case.name): $($_.Exception.Message)"
        Write-Output "FAIL $($taskFailures[-1])"
    } finally { $process.Dispose() }
}
Write-Output "Mock-only evidence retained: $taskRoot"
if ($taskFailures.Count) { throw ($taskFailures -join "`n") }
Write-Output "Passed $($taskCases.Count) build/gate scenarios; no real Cargo, compiler or product process was invoked."
