param(
    [Parameter(Mandatory=$true)][string]$AttemptDir,
    [Parameter(Mandatory=$true)][string]$TargetDir
)
$ErrorActionPreference='Stop'
$PSNativeCommandUseErrorActionPreference=$false
Set-StrictMode -Version Latest
$work=(Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Set-Location -LiteralPath $work
$repo=(& git rev-parse --show-toplevel).Trim()
$gate=Join-Path $work '.local/nhc-w2-window.lock'
$env:TEMP=Join-Path $AttemptDir 'tmp';$env:TMP=$env:TEMP
$env:CARGO_NET_OFFLINE='true';$env:RUSTUP_AUTO_INSTALL='0'
$env:CARGO_INCREMENTAL='0';$env:CARGO_BUILD_JOBS='2'
$env:CARGO_PROFILE_DEV_DEBUG='0';$env:CARGO_PROFILE_TEST_DEBUG='0'
$env:CARGO_TARGET_DIR=$TargetDir
$env:VCToolsInstallDir=Join-Path $env:ProgramFiles 'Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.43.34808'
$env:WindowsSdkDir=Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10'
$expected='a4366b56cc88644336d45f961313f294b05ea5ec'
$expectedProduct='4935f1aa104de548c97a26043cb0d5e442e24dea'
$expectedRust='bfea730396003f4aa675cc25dcfaa1c6b39f1e14'
$started=[DateTime]::UtcNow.ToString('o')
function Save-Json($path,$value){$value|ConvertTo-Json -Depth 12|Set-Content -Encoding UTF8 -LiteralPath $path}
function Snapshot($name){
    $head=(& git rev-parse HEAD).Trim();if($LASTEXITCODE -ne 0){throw 'HEAD query failed'}
    $product=(& git rev-parse 'HEAD:NoManCode').Trim();if($LASTEXITCODE -ne 0){throw 'product tree query failed'}
    $rust=(& git rev-parse 'HEAD:NoManCode/rust-app').Trim();if($LASTEXITCODE -ne 0){throw 'Rust tree query failed'}
    $status=@(& git status --porcelain --untracked-files=all);if($LASTEXITCODE -ne 0){throw 'status query failed'}
    $index=@(& git diff --cached --name-only);if($LASTEXITCODE -ne 0){throw 'index query failed'}
    $state=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');head=$head;product_tree=$product;rust_tree=$rust;status=$status;index=$index}
    Save-Json (Join-Path $AttemptDir ('source-'+$name+'.json')) $state
    if($head -cne $expected -or $product -cne $expectedProduct -or $rust -cne $expectedRust -or $status.Count -ne 0 -or $index.Count -ne 0){throw 'source identity or cleanliness mismatch'}
    $paths=@(& git -C $repo ls-files -- NoManCode);if($LASTEXITCODE -ne 0){throw 'source list failed'}
    $entries=@(foreach($path in $paths){$file=Join-Path $repo $path;[pscustomobject]@{path=$path;sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant();bytes=(Get-Item -LiteralPath $file).Length}})
    Save-Json (Join-Path $AttemptDir ('source-files-'+$name+'.json')) $entries
    return ,$entries
}
function Preserve-Gate($reason){
    try {
        $f=[IO.File]::Open($gate,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
        try{$bytes=[Text.Encoding]::UTF8.GetBytes((@{state='matrix_outer_failed';reason=$reason;utc=[DateTime]::UtcNow.ToString('o')}|ConvertTo-Json));$f.Write($bytes,0,$bytes.Length);$f.Flush($true)}finally{$f.Dispose()}
        return 'created'
    } catch {if(Test-Path -LiteralPath $gate){return 'existing'};return ('failed: '+($_|Out-String))}
}
try {
    $before=Snapshot 'before'
    $setup=Get-Content -Raw -LiteralPath (Join-Path $work 'build.ps1')
    $setup=$setup.Substring(0,$setup.IndexOf('switch ($Action)'))
    $setup=$setup -replace "param\(\[ValidateSet\([^\r\n]*\r?\n", ''
    $setup=$setup -replace 'Set-Location -LiteralPath \$PSScriptRoot', ''
    Set-StrictMode -Off
    try{Invoke-Expression $setup *> (Join-Path $AttemptDir 'toolchain.log')}finally{Set-StrictMode -Version Latest}
    if($taskMsvc -ine $env:VCToolsInstallDir -or $taskSdk.FullName -ine (Join-Path $env:WindowsSdkDir 'Lib/10.0.22621.0')){throw 'detected toolchain differs from fixed versions'}
    $runner=Join-Path $work 'tests/support/wasm_worker_process/run-controlled.ps1'
    $site=Join-Path $AttemptDir 'runner'
    Save-Json (Join-Path $AttemptDir 'environment.json') ([ordered]@{started_utc=$started;target=$TargetDir;tmp=$env:TEMP;msvc=$taskMsvc;msvc_bin=$taskBin;sdk=$taskSdk.FullName;jobs=2;dev_debug=0;test_debug=0;incremental=0;offline=$true;locked=$true;case_timeout_seconds=180;runner=$runner;runner_attempt=$site;gate_owner='tracked run-controlled.ps1'})
    & $runner -ExpectedHead $expected -TargetDir $TargetDir -AttemptDir $site -CaseTimeoutSeconds 180 1> (Join-Path $AttemptDir 'runner.stdout') 2> (Join-Path $AttemptDir 'runner.stderr')
    $selection=Get-Content -Raw -LiteralPath (Join-Path $site 'selection.json')|ConvertFrom-Json
    if($selection.scope -ne 'full' -or $selection.selected_count -ne 21 -or $selection.mapped_count -ne 21){throw 'full 21 selection not established'}
    for($i=1;$i -le 21;$i++){
        $case=Get-Content -Raw -LiteralPath (Join-Path $site ('case-{0:D2}/case.json' -f $i))|ConvertFrom-Json
        if($case.state -ne 'passed' -or $case.exit_code -ne 0 -or $case.test -cne $selection.selected[$i-1]){throw "case $i not passed or selection mismatch"}
    }
    $manifest=@(Get-Content -Raw -LiteralPath (Join-Path $site 'manifest.json')|ConvertFrom-Json)
    if($manifest.Count -ne 3){throw 'three fixed artifacts not established'}
    foreach($entry in $manifest){$actual=(Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $site ('fixed/'+$entry.name))).Hash.ToLowerInvariant();if($actual -cne $entry.sha256){throw 'fixed artifact changed'}}
    if(Test-Path -LiteralPath $gate){throw 'inner gate remains after controlled runner return'}
    $after=Snapshot 'final'
    if(($before|ConvertTo-Json -Depth 5 -Compress) -cne ($after|ConvertTo-Json -Depth 5 -Compress)){throw 'tracked source hashes changed during matrix'}
    Save-Json (Join-Path $AttemptDir 'window-result.json') ([ordered]@{state='completed';exit_code=0;started_utc=$started;ended_utc=[DateTime]::UtcNow.ToString('o');cases=21})
    exit 0
} catch {
    $reason=$_|Out-String
    $preservation=Preserve-Gate $reason
    try{$null=Snapshot 'stopped'}catch{}
    Save-Json (Join-Path $AttemptDir 'window-result.json') ([ordered]@{state='failed';exit_code=1;started_utc=$started;ended_utc=[DateTime]::UtcNow.ToString('o');reason=$reason;gate_preservation=$preservation})
    exit 1
}
