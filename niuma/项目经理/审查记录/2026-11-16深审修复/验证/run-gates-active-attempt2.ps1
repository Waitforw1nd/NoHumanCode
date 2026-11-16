param([ValidatePattern('^[a-z0-9-]+$')][string]$Attempt = 'final-attempt1')
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$taskRust = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
Set-Location -LiteralPath $taskRust
$taskRepository = [IO.Path]::GetFullPath((Join-Path $taskRust '../..'))
if (@(& git -C $taskRepository status --porcelain -- NoManCode).Count) { throw 'Product inputs are not committed' }
$env:VCToolsInstallDir = Join-Path $env:ProgramFiles 'Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.43.34808'
$env:WindowsSdkDir = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10'
$taskCompiler = Join-Path $env:VCToolsInstallDir 'bin/Hostx86/x64'
$taskSdk = '10.0.22621.0'
$env:PATH = "$taskCompiler;$env:PATH"
$env:LIB = "$env:VCToolsInstallDir/lib/x64;$env:WindowsSdkDir/Lib/$taskSdk/ucrt/x64;$env:WindowsSdkDir/Lib/$taskSdk/um/x64"
$env:INCLUDE = "$env:VCToolsInstallDir/include;$env:WindowsSdkDir/Include/$taskSdk/ucrt;$env:WindowsSdkDir/Include/$taskSdk/shared;$env:WindowsSdkDir/Include/$taskSdk/um"
$env:CC_x86_64_pc_windows_msvc = Join-Path $taskCompiler 'cl.exe'
$env:AR_x86_64_pc_windows_msvc = Join-Path $taskCompiler 'lib.exe'
$env:TMP = Join-Path $PSScriptRoot 'tmp'
$env:TEMP = $env:TMP
$env:CARGO_NET_OFFLINE = 'true'
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_TARGET_DIR = Join-Path $taskRust 'target'
$env:RUST_TEST_THREADS = '1'
$taskEvidence = Join-Path $PSScriptRoot $Attempt
New-Item -ItemType Directory -Path $taskEvidence -ErrorAction Stop | Out-Null
$taskManifest = [ordered]@{
    head = (& git rev-parse HEAD)
    product_tree = (& git rev-parse HEAD:NoManCode)
    rust_tree = (& git rev-parse HEAD:NoManCode/rust-app)
    rustc = (& rustc --version)
    cargo = (& cargo --version)
    started_utc = [DateTime]::UtcNow.ToString('o')
    test_threads = 1
    worktree_timing = ($env:NHC_WORKTREE_HTTP_TIMING -eq '1')
    offline = $true
    gates = @()
}
$taskGates = @(
    @{name='check'; executable='cargo'; arguments=@('check','--workspace','--all-targets','--locked')},
    @{name='debug-bins'; executable='cargo'; arguments=@('build','--bins','--locked')},
    @{name='test'; executable='pwsh'; arguments=@('-NoProfile','-File','build.ps1','-Action','test')},
    @{name='fmt'; executable='cargo'; arguments=@('fmt','--all','--','--check')},
    @{name='clippy'; executable='cargo'; arguments=@('clippy','--workspace','--all-targets','--locked','--','-D','warnings')},
    @{name='wasm-check'; executable='cargo'; arguments=@('check','-p','peachsh-ui','--target','wasm32-unknown-unknown','--locked')}
)
foreach ($gate in $taskGates) {
    $started = [DateTime]::UtcNow.ToString('o')
    $log = Join-Path $taskEvidence ($gate.name + '.log')
    $arguments = $gate.arguments
    & $gate.executable @arguments *> $log
    $result = $LASTEXITCODE
    $taskManifest.gates += [ordered]@{
        name=$gate.name; command=($gate.executable + ' ' + ($arguments -join ' ')); exit_code=$result
        started_utc=$started; ended_utc=[DateTime]::UtcNow.ToString('o')
        sha256=(Get-FileHash -LiteralPath $log -Algorithm SHA256).Hash
        bytes=(Get-Item -LiteralPath $log).Length
    }
    $taskManifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $taskEvidence 'manifest.json') -Encoding utf8
    Write-Output "$($gate.name): exit $result"
    if ($result -ne 0) { Get-Content -LiteralPath $log -Tail 45; exit $result }
}
$taskManifest.final_product_tree = (& git rev-parse HEAD:NoManCode)
$taskManifest.ended_utc = [DateTime]::UtcNow.ToString('o')
$taskManifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $taskEvidence 'manifest.json') -Encoding utf8
if ($taskManifest.final_product_tree -ne $taskManifest.product_tree) { throw 'Product tree changed during gates' }
if (@(& git -C $taskRepository status --porcelain -- NoManCode).Count) { throw 'Product inputs changed during gates' }
Write-Output "All gates passed; evidence $taskEvidence"
