$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$base = $PSScriptRoot
$work = (Resolve-Path (Join-Path $base '../..')).Path
Set-Location -LiteralPath $work
$env:TEMP = Join-Path $base 'tmp'
$env:TMP = $env:TEMP
$env:CARGO_NET_OFFLINE = 'true'
$env:RUSTUP_AUTO_INSTALL = '0'
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:VCToolsInstallDir = 'C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.43.34808'
$env:WindowsSdkDir = 'C:\Program Files (x86)\Windows Kits\10\'
$target='E:\code\nhc-b-tool-call-application\NoManCode\rust-app\.local\next19-b-a7fe2626-attempt4\target'
$attempt=Join-Path $base 'runner'
$gate=Join-Path $work '.local/nhc-w2-window.lock'
$started=[DateTime]::UtcNow.ToString('o')
$command='.\tests\support\wasm_worker_process\run-controlled.ps1 -ExpectedHead 796f8847eaf32bc2494cc2ad962f781f9ba19bfb -TargetDir <target> -AttemptDir <runner> -ExactTest wasm_worker_process_tests::fault_matrix::w04_unknown_cleanup_and_slot_reuse -CaseTimeoutSeconds 180'
try {
    $setup=Get-Content -Raw -LiteralPath (Join-Path $work 'build.ps1')
    $setup=$setup.Substring(0,$setup.IndexOf('switch ($Action)'))
    $setup=$setup -replace "param\(\[ValidateSet\([^\r\n]*\r?\n", ''
    $setup=$setup -replace 'Set-Location -LiteralPath \$PSScriptRoot', ''
    Invoke-Expression $setup *> (Join-Path $base 'toolchain.log')
    if (-not (Test-Path -LiteralPath (Join-Path $taskBin 'cl.exe'))) { throw 'MSVC setup missing cl' }
    $environment=@{started_utc=$started;command=$command;target=$target;attempt=$attempt;tmp=$env:TEMP;jobs=2;dev_debug=0;test_debug=0;incremental=0;offline=$true;locked=$true;msvc=$taskMsvc;sdk=$taskSdk.FullName;launcher='hidden PowerShell; DirectProcess CreateProcessW flags 0x08000004 CREATE_SUSPENDED | CREATE_NO_WINDOW';runner_pid=$PID}
    $environment | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $base 'environment.json')
    & (Join-Path $work 'tests/support/wasm_worker_process/run-controlled.ps1') -ExpectedHead '796f8847eaf32bc2494cc2ad962f781f9ba19bfb' -TargetDir $target -AttemptDir $attempt -ExactTest wasm_worker_process_tests::fault_matrix::w04_unknown_cleanup_and_slot_reuse -CaseTimeoutSeconds 180 1> (Join-Path $base 'runner.stdout') 2> (Join-Path $base 'runner.stderr')
    $status=@{state='completed';exit_code=0;started_utc=$started;ended_utc=[DateTime]::UtcNow.ToString('o')}
} catch {
    $errorText=$_ | Out-String
    if (-not (Test-Path -LiteralPath $gate)) {
        try {
            $f=[System.IO.File]::Open($gate,[System.IO.FileMode]::CreateNew,[System.IO.FileAccess]::Write,[System.IO.FileShare]::Read)
            try { $b=[System.Text.Encoding]::UTF8.GetBytes((@{state='window_failure';attempt=$attempt;reason=$errorText;at_utc=[DateTime]::UtcNow.ToString('o')} | ConvertTo-Json)); $f.Write($b,0,$b.Length); $f.Flush($true) } finally { $f.Dispose() }
        } catch { $errorText += "`nfailed to preserve gate: $($_ | Out-String)" }
    }
    $status=@{state='failed';exit_code=1;started_utc=$started;ended_utc=[DateTime]::UtcNow.ToString('o');error=$errorText;gate=$gate}
}
$status | ConvertTo-Json -Depth 6 | Set-Content -Encoding UTF8 (Join-Path $base 'window-result.json')
if ($status.exit_code -ne 0) { exit 1 }