param(
    [Parameter(Mandatory=$true)][string]$AttemptDir,
    [Parameter(Mandatory=$true)][string]$TargetDir,
    [Parameter(Mandatory=$true)][string]$BuildDir,
    [Parameter(Mandatory=$true)][string]$ExpectedBuildReceiptSha256
)
$ErrorActionPreference='Stop'
$PSNativeCommandUseErrorActionPreference=$false
Set-StrictMode -Version Latest
$base=$AttemptDir
$work=(Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Set-Location -LiteralPath $work
$repo=(& git rev-parse --show-toplevel).Trim()
$env:TEMP=Join-Path $base 'tmp'
$env:TMP=$env:TEMP
$env:CARGO_NET_OFFLINE='true'
$env:RUSTUP_AUTO_INSTALL='0'
$env:CARGO_INCREMENTAL='0'
$env:CARGO_BUILD_JOBS='2'
$env:CARGO_PROFILE_DEV_DEBUG='0'
$env:CARGO_PROFILE_TEST_DEBUG='0'
$env:CARGO_TARGET_DIR=$TargetDir
foreach($variable in @('RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER')){Remove-Item -LiteralPath ('Env:'+$variable) -ErrorAction SilentlyContinue}
$env:VCToolsInstallDir=Join-Path $env:ProgramFiles 'Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.43.34808'
$env:WindowsSdkDir=Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10'
$expected='98cec2e441adcb7cd0476c81db075c8f1ce4659e'
$expectedProduct='85020d9c3c34e9f498fc9f4e4b5f855b087c5f4e'
$expectedRust='951cff6854e0a8bc0b7cc48d559b735c60256c21'
$current=$null
$child=$null
$childActive=$false
$started=[DateTime]::UtcNow.ToString('o')
$sourceBaseline=$null
function Save-Json($path,$value){ConvertTo-Json -InputObject $value -Depth 12|Set-Content -Encoding UTF8 -LiteralPath $path}
function Snapshot($name){
    $head=(& git rev-parse HEAD).Trim();if($LASTEXITCODE -ne 0){throw 'HEAD query failed'}
    $product=(& git rev-parse 'HEAD:NoManCode').Trim();if($LASTEXITCODE -ne 0){throw 'product tree query failed'}
    $rust=(& git rev-parse 'HEAD:NoManCode/rust-app').Trim();if($LASTEXITCODE -ne 0){throw 'Rust tree query failed'}
    $status=@(& git status --porcelain --untracked-files=all);if($LASTEXITCODE -ne 0){throw 'status failed'}
    $index=@(& git diff --cached --name-only);if($LASTEXITCODE -ne 0){throw 'index query failed'}
    $state=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');head=$head;product_tree=$product;rust_tree=$rust;status=$status;index=$index}
    Save-Json (Join-Path $base ("source-"+$name+'.json')) $state
    if($head -ne $expected -or $product -ne $expectedProduct -or $rust -ne $expectedRust -or $status.Count -ne 0 -or $index.Count -ne 0){throw 'source identity or cleanliness mismatch'}
    $paths=@(& git -C $repo -c core.quotepath=false ls-files --full-name -- NoManCode);if($LASTEXITCODE -ne 0 -or $paths.Count -eq 0){throw 'source listing failed'}
    $entries=@(foreach($path in ($paths|Sort-Object -CaseSensitive)){$f=Join-Path $repo $path;$item=Get-Item -LiteralPath $f -Force;if($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)){throw 'unsupported source entry'};[pscustomobject]@{path=$path;sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $f).Hash.ToLowerInvariant();bytes=$item.Length}})
    Save-Json (Join-Path $base ("source-files-"+$name+'.json')) $entries
    $serialized=$entries|ConvertTo-Json -Depth 12 -Compress
    if($null -eq $script:sourceBaseline){$script:sourceBaseline=$serialized}elseif($serialized -cne $script:sourceBaseline){throw 'tracked source bytes changed'}
}
function Snapshot-Artifacts($name){
    $entries=@()
    if(Test-Path -LiteralPath $TargetDir){
        $entries=@(Get-ChildItem -LiteralPath $TargetDir -File -Recurse -Force | Sort-Object FullName | ForEach-Object {
            if($_.Attributes -band [IO.FileAttributes]::ReparsePoint){throw 'unsupported target reparse point'}
            [pscustomobject]@{path=$_.FullName.Substring($TargetDir.Length).TrimStart('\');bytes=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}
        })
    }
    Save-Json (Join-Path $base ("artifacts-"+$name+'.json')) $entries
}
function Hold-Unknown($process,$record,$reason){
    $record.state='retained_unknown'
    $record['reason']=$reason
    $record['detected_utc']=[DateTime]::UtcNow.ToString('o')
    try { $marker=[IO.File]::Open((Join-Path $base 'terminal-unknown.marker'),[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read); $marker.Dispose() } catch { }
    try { Save-Json (Join-Path $base 'incident.json') $record; Save-Json (Join-Path $base 'supervisor-state.json') $record } catch { }
    try { Snapshot 'unknown' } catch { }
    # Keep the original handle, including a suspended thread when resume failed.
    # Even a later signal is recorded but does not clear unknown or the gate.
    $recorded=$false
    while($true){
        $wait=$process.Wait(1000)
        if($wait -eq 0 -and -not $recorded){
            $code=[uint32]0
            $ok=$process.ExitCode([ref]$code)
            $record['late_signal_utc']=[DateTime]::UtcNow.ToString('o')
            $record['late_exit_query_ok']=$ok
            $record['late_exit_code']=if($ok){$code}else{$null}
            $record['late_exit_error']=if($ok){$null}else{$process.LastError}
            try { Save-Json (Join-Path $base 'incident-late-signal.json') $record } catch { }
            $recorded=$true
        }
        if($wait -eq 0 -or $wait -eq [uint32]::MaxValue){Start-Sleep -Milliseconds 1000}
    }
}
try {
    Snapshot 'before'
    $receiptPath=Join-Path $BuildDir 'build-receipt.json'
    if((Get-FileHash -LiteralPath $receiptPath -Algorithm SHA256).Hash -ine $ExpectedBuildReceiptSha256){throw 'build receipt mismatch'}
    $receipt=Get-Content -Raw -LiteralPath $receiptPath|ConvertFrom-Json
    $buildResult=Get-Content -Raw -LiteralPath (Join-Path $BuildDir 'window-result.json')|ConvertFrom-Json
    $buildLauncher=Get-Content -Raw -LiteralPath (Join-Path $BuildDir 'launcher.json')|ConvertFrom-Json
    if($receipt.head -cne $expected -or $receipt.product_tree -cne $expectedProduct -or $receipt.rust_tree -cne $expectedRust -or $receipt.state -cne 'built-not-executed' -or $buildResult.state -cne 'completed' -or $buildResult.exit_code -ne 0 -or $buildLauncher.state -cne 'completed' -or $buildLauncher.exit_code -ne 0){throw 'build provenance incomplete or wrong candidate'}
    $artifacts=@($receipt.artifacts);if($artifacts.Count -ne 2){throw 'build artifact count mismatch'}
    $probeRecord=@($artifacts|Where-Object {$_.name -ceq 'probe.exe'});$testRecord=@($artifacts|Where-Object {$_.name -ceq 'lib-test.exe'})
    if($probeRecord.Count -ne 1 -or $testRecord.Count -ne 1){throw 'build artifacts missing or ambiguous'}
    $probe=(Resolve-Path -LiteralPath (Join-Path $BuildDir 'fixed/probe.exe')).Path
    $test=(Resolve-Path -LiteralPath (Join-Path $BuildDir 'fixed/lib-test.exe')).Path
    foreach($pair in @(@($probe,$probeRecord[0]),@($test,$testRecord[0]))){$path=$pair[0];$record=$pair[1];if($record.copy -ine $path -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $record.sha256 -or (Get-Item -LiteralPath $path).Length -ne $record.bytes){throw 'fixed build artifact mismatch'}}
    $runner=Join-Path $work 'tests/support/process_lifetime/run-fixed.ps1'
    $runnerAttempt=Join-Path $base 'matrix'
    $exe=Join-Path $PSHOME 'powershell.exe';if(-not (Test-Path -LiteralPath $exe)){$exe=Join-Path $PSHOME 'pwsh.exe'}
    if(-not (Test-Path -LiteralPath $exe)){throw 'PowerShell executable unavailable'}
    Add-Type -Path (Join-Path $PSScriptRoot 'GateProcess.cs')
    $argsLine="-NoProfile -File `"$runner`" -ExpectedHead $expected -ExpectedProductTree $expectedProduct -ExpectedRustTree $expectedRust -TestExe `"$test`" -TestSha256 $($testRecord[0].sha256) -ProbeExe `"$probe`" -ProbeSha256 $($probeRecord[0].sha256) -AttemptDir `"$runnerAttempt`""
    $stdout=Join-Path $base 'matrix.stdout';$stderr=Join-Path $base 'matrix.stderr';$meta=Join-Path $base 'matrix-step.json'
    $current=[ordered]@{name='20-exact';command=('"'+$exe+'" '+$argsLine);start_utc=[DateTime]::UtcNow.ToString('o');end_utc=$null;state='starting';pid=$null;creation_filetime=$null;creation_error=$null;exit_code=$null;stdout=$stdout;stderr=$stderr;timeout_seconds=4200;build_dir=$BuildDir;build_receipt_sha256=$ExpectedBuildReceiptSha256}
    Save-Json (Join-Path $base 'build-provenance.json') ([ordered]@{receipt_sha256=$ExpectedBuildReceiptSha256;receipt=$receipt;window_result=$buildResult;launcher=$buildLauncher})
    Save-Json $meta $current;Snapshot 'before-matrix'
    $clock=[Diagnostics.Stopwatch]::StartNew();$child=[B21GateProcess]::Start($exe,$argsLine,$work,$stdout,$stderr);$childActive=$true
    $current.pid=$child.Pid;$current.creation_filetime=$child.CreationFileTime;$current.creation_error=$child.CreationError;$current.state='suspended';Save-Json $meta $current;Save-Json (Join-Path $base 'supervisor-state.json') $current
    if($child.CreationFileTime -eq 0){Hold-Unknown $child $current 'runner creation identity unavailable'}
    if(-not $child.Resume()){Hold-Unknown $child $current ('runner resume failed: '+$child.LastError)}
    $child.Thread.Dispose();$current.state='running';Save-Json $meta $current
    while($true){
        if(Test-Path -LiteralPath (Join-Path $runnerAttempt 'incident.json')){Hold-Unknown $child $current 'inner runner reports unknown'}
        if($clock.Elapsed.TotalSeconds -ge 4200){Hold-Unknown $child $current 'matrix runner deadline exceeded'}
        $wait=$child.Wait(250);if($wait -eq 0){break};if($wait -ne 258){Hold-Unknown $child $current ('runner wait failed: '+$child.LastError)}
    }
    if(Test-Path -LiteralPath (Join-Path $runnerAttempt 'incident.json')){Hold-Unknown $child $current 'inner runner incident at exit'}
    $code=[uint32]0;if(-not $child.ExitCode([ref]$code)){Hold-Unknown $child $current ('runner exit query failed: '+$child.LastError)}
    $current.end_utc=[DateTime]::UtcNow.ToString('o');$current.exit_code=$code;$current.state='exited';$current['elapsed_ms']=$clock.ElapsedMilliseconds;Save-Json $meta $current
    $childActive=$false;$child.Handle.Dispose();$child=$null
    if($code -ne 0){throw "matrix runner exited $code"}
    $complete=Get-Content -Raw -LiteralPath (Join-Path $runnerAttempt 'complete.json')|ConvertFrom-Json
    if($complete.state -cne 'complete' -or $complete.cases -ne 20){throw 'matrix completion count mismatch'}
    foreach($pair in @(@($probe,$probeRecord[0]),@($test,$testRecord[0]))){if((Get-FileHash -LiteralPath $pair[0] -Algorithm SHA256).Hash -ine $pair[1].sha256){throw 'build artifact changed'}}
    if((Get-FileHash -LiteralPath $receiptPath -Algorithm SHA256).Hash -ine $ExpectedBuildReceiptSha256){throw 'build receipt changed'}
    Snapshot 'final';$current.state='passed';Save-Json $meta $current
    Save-Json (Join-Path $base 'window-result.json') ([ordered]@{state='completed';exit_code=0;started_utc=$started;ended_utc=[DateTime]::UtcNow.ToString('o')})
    exit 0
} catch {
    $errorText=$_|Out-String
    if($childActive -and $null -ne $child){Hold-Unknown $child $current $errorText}
    $record=[ordered]@{state='stopped';reason=$errorText;at_utc=[DateTime]::UtcNow.ToString('o');attempt=$base;current=$current;instruction='Gate remains; manager must resolve before restart.'}
    Save-Json (Join-Path $base 'incident.json') $record
    try{Snapshot 'stopped'}catch{[IO.File]::WriteAllText((Join-Path $base 'source-stop-error.txt'),($_|Out-String))}
    Save-Json (Join-Path $base 'window-result.json') ([ordered]@{state='failed';exit_code=1;started_utc=$started;ended_utc=[DateTime]::UtcNow.ToString('o');reason=$errorText})
    exit 1
}
