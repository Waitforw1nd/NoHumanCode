param(
    [Parameter(Mandatory=$true)][string]$AttemptDir,
    [Parameter(Mandatory=$true)][string]$TargetDir,
    [Parameter(Mandatory=$true)][string]$ExpectedHead,
    [Parameter(Mandatory=$true)][string]$ExpectedManifestSha256,
    [Parameter(Mandatory=$true)][string]$BuildDir,
    [Parameter(Mandatory=$true)][string]$ExpectedBuildReceiptSha256
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$expected='98cec2e441adcb7cd0476c81db075c8f1ce4659e'
if($ExpectedHead -cne $expected){throw 'ExpectedHead does not match fixed candidate'}
$work=(Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$common=(& git -C $work rev-parse --git-common-dir).Trim()
if($LASTEXITCODE -ne 0){throw 'cannot locate shared Git directory'}
$common=if([IO.Path]::IsPathRooted($common)){[IO.Path]::GetFullPath($common)}else{[IO.Path]::GetFullPath((Join-Path $work $common))}
if(-not $common.StartsWith('E:\',[StringComparison]::OrdinalIgnoreCase)){throw 'shared gate must be on E:'}
$gate=Join-Path $common 'nhc-heavy-window.lock'
if($ExpectedManifestSha256 -notmatch '^[a-fA-F0-9]{64}$' -or $ExpectedBuildReceiptSha256 -notmatch '^[a-fA-F0-9]{64}$'){throw 'complete manifest and receipt SHAs required'}
$BuildDir=[IO.Path]::GetFullPath($BuildDir)
if(-not $BuildDir.StartsWith('E:\',[StringComparison]::OrdinalIgnoreCase)){throw 'build must be on E:'}
$manifestPath=Join-Path $PSScriptRoot 'manifest.json'
if((Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash -ine $ExpectedManifestSha256){throw 'prepared manifest mismatch'}
$manifest=Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
if($manifest.candidate_head -cne $expected){throw 'prepared manifest candidate mismatch'}
foreach($entry in $manifest.files){
    if($entry.path -notin @('GateProcess.cs','launch.ps1','supervisor.ps1')){throw 'unexpected prepared file'}
    $file=Join-Path $PSScriptRoot $entry.path
    if((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ine $entry.sha256 -or (Get-Item -LiteralPath $file).Length -ne $entry.bytes){throw 'prepared input changed'}
}
if(@($manifest.files).Count -ne 3){throw 'incomplete prepared manifest'}
function Save-Json($path,$value){ConvertTo-Json -InputObject $value -Depth 12|Set-Content -Encoding UTF8 -LiteralPath $path}
function Hold-Original($process,$record,$reason){
    $record['state']='retained_unknown';$record['reason']=$reason;$record['detected_utc']=[DateTime]::UtcNow.ToString('o')
    try{Save-Json (Join-Path $AttemptDir 'launcher-incident.json') $record}catch{}
    $recorded=$false
    while($true){
        $wait=$process.Wait(1000)
        if($wait -eq 0 -and -not $recorded){
            $code=[uint32]0;$ok=$process.ExitCode([ref]$code)
            $record['late_signal_utc']=[DateTime]::UtcNow.ToString('o');$record['late_exit_query_ok']=$ok
            $record['late_exit_code']=if($ok){$code}else{$null};$record['late_exit_error']=if($ok){$null}else{$process.LastError}
            try{Save-Json (Join-Path $AttemptDir 'launcher-late-signal.json') $record}catch{}
            $recorded=$true
        }
        if($wait -eq 0 -or $wait -eq [uint32]::MaxValue){Start-Sleep -Milliseconds 1000}
    }
}
$AttemptDir=[IO.Path]::GetFullPath($AttemptDir);$TargetDir=[IO.Path]::GetFullPath($TargetDir)
if(-not $AttemptDir.StartsWith('E:\',[StringComparison]::OrdinalIgnoreCase) -or -not $TargetDir.StartsWith('E:\',[StringComparison]::OrdinalIgnoreCase)){throw 'Attempt and target must be on E:'}
if((Split-Path -Parent $AttemptDir) -ine [IO.Path]::GetFullPath((Join-Path $work '.local')) -or $TargetDir -ine [IO.Path]::GetFullPath((Join-Path $AttemptDir 'target'))){throw 'attempt must be a new direct .local child and target must be its target subdirectory'}
if(Test-Path -LiteralPath $AttemptDir){throw 'attempt already exists'}
if(Test-Path -LiteralPath $TargetDir){throw 'target already exists'}
if(Test-Path -LiteralPath $gate){throw 'existing gate: stop, do not overwrite'}
$lock=$null;$child=$null;$active=$false;$record=[ordered]@{state='preparing';expected_head=$ExpectedHead;attempt=$AttemptDir;target=$TargetDir;pid=$null;creation_filetime=$null;stdout=$null;stderr=$null;started_utc=[DateTime]::UtcNow.ToString('o')}
try{
    $lock=[IO.File]::Open($gate,[IO.FileMode]::CreateNew,[IO.FileAccess]::ReadWrite,[IO.FileShare]::Read)
    New-Item -ItemType Directory -Path $AttemptDir -ErrorAction Stop|Out-Null
    New-Item -ItemType Directory -Path (Join-Path $AttemptDir 'tmp') -ErrorAction Stop|Out-Null
    $preparedCopy=Join-Path $AttemptDir 'prepared';New-Item -ItemType Directory -Path $preparedCopy -ErrorAction Stop|Out-Null
    foreach($entry in $manifest.files){Copy-Item -LiteralPath (Join-Path $PSScriptRoot $entry.path) -Destination (Join-Path $preparedCopy $entry.path);if((Get-FileHash -LiteralPath (Join-Path $preparedCopy $entry.path) -Algorithm SHA256).Hash -ine $entry.sha256){throw 'prepared copy mismatch'}}
    Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $preparedCopy 'manifest.json')
    if((Get-FileHash -LiteralPath (Join-Path $preparedCopy 'manifest.json') -Algorithm SHA256).Hash -ine $ExpectedManifestSha256){throw 'manifest copy mismatch'}
    $env:TEMP=Join-Path $AttemptDir 'tmp';$env:TMP=$env:TEMP
    $bytes=[Text.Encoding]::UTF8.GetBytes(($record|ConvertTo-Json -Depth 12));$lock.Write($bytes,0,$bytes.Length);$lock.Flush($true)
    Add-Type -Path (Join-Path $PSScriptRoot 'GateProcess.cs')
    $exe=Join-Path $PSHOME 'powershell.exe';if(-not (Test-Path -LiteralPath $exe)){$exe=Join-Path $PSHOME 'pwsh.exe'}
    if(-not (Test-Path -LiteralPath $exe)){throw 'current PowerShell executable not found'}
    $script=Join-Path $PSScriptRoot 'supervisor.ps1'
    $argsLine="-NoProfile -File `"$script`" -AttemptDir `"$AttemptDir`" -TargetDir `"$TargetDir`" -BuildDir `"$BuildDir`" -ExpectedBuildReceiptSha256 $ExpectedBuildReceiptSha256"
    $record.stdout=Join-Path $AttemptDir 'supervisor.stdout';$record.stderr=Join-Path $AttemptDir 'supervisor.stderr'
    $record['command']='"'+$exe+'" '+$argsLine;$record['flags']='CREATE_SUSPENDED | CREATE_NO_WINDOW';$record['timeout_seconds']=5400
    Save-Json (Join-Path $AttemptDir 'launcher.json') $record
    $clock=[Diagnostics.Stopwatch]::StartNew()
    $child=[B21GateProcess]::Start($exe,$argsLine,$work,$record.stdout,$record.stderr);$active=$true
    $record.pid=$child.Pid;$record.creation_filetime=$child.CreationFileTime;$record['creation_error']=$child.CreationError;$record.state='suspended'
    Save-Json (Join-Path $AttemptDir 'launcher.json') $record
    if($child.CreationFileTime -eq 0){Hold-Original $child $record 'GetProcessTimes failed on original supervisor handle'}
    if(-not $child.Resume()){Hold-Original $child $record ('ResumeThread failed: '+$child.LastError)}
    $child.Thread.Dispose();$record.state='running';Save-Json (Join-Path $AttemptDir 'launcher.json') $record
    while($true){
        $out=[string](Get-Content -Raw -LiteralPath $record.stdout);$err=[string](Get-Content -Raw -LiteralPath $record.stderr)
        if([string]::Concat($out,$err).Contains('NHC_TERMINAL_UNKNOWN:')){Hold-Original $child $record 'terminal unknown marker in supervisor output'}
        if(Test-Path -LiteralPath (Join-Path $AttemptDir 'terminal-unknown.marker')){Hold-Original $child $record 'supervisor reports retained unknown'}
        if($clock.Elapsed.TotalSeconds -ge 5400){Hold-Original $child $record 'supervisor deadline exceeded'}
        $wait=$child.Wait(250);if($wait -eq 0){break};if($wait -ne 258){Hold-Original $child $record ('supervisor wait failed: '+$child.LastError)}
    }
    if(Test-Path -LiteralPath (Join-Path $AttemptDir 'terminal-unknown.marker')){Hold-Original $child $record 'supervisor reports retained unknown at exit'}
    $statePath=Join-Path $AttemptDir 'supervisor-state.json'
    if(Test-Path -LiteralPath $statePath){$state=Get-Content -Raw -LiteralPath $statePath|ConvertFrom-Json;if($state.state -eq 'retained_unknown'){Hold-Original $child $record 'supervisor reports retained unknown at exit'}}
    $out=[string](Get-Content -Raw -LiteralPath $record.stdout);$err=[string](Get-Content -Raw -LiteralPath $record.stderr)
    if([string]::Concat($out,$err).Contains('NHC_TERMINAL_UNKNOWN:')){Hold-Original $child $record 'terminal unknown marker at supervisor exit'}
    $code=[uint32]0;if(-not $child.ExitCode([ref]$code)){Hold-Original $child $record ('GetExitCodeProcess failed: '+$child.LastError)}
    $record['exit_code']=$code;$record['signal_utc']=[DateTime]::UtcNow.ToString('o');$record.state='exited';Save-Json (Join-Path $AttemptDir 'launcher.json') $record
    $active=$false;$child.Handle.Dispose();$child=$null
    if($code -ne 0){throw "supervisor exit $code"}
    $result=Get-Content -Raw -LiteralPath (Join-Path $AttemptDir 'window-result.json')|ConvertFrom-Json
    $final=Get-Content -Raw -LiteralPath (Join-Path $AttemptDir 'source-final.json')|ConvertFrom-Json
    if($result.state -ne 'completed' -or $result.exit_code -ne 0 -or $final.head -cne $expected -or $final.product_tree -cne '85020d9c3c34e9f498fc9f4e4b5f855b087c5f4e' -or $final.rust_tree -cne '951cff6854e0a8bc0b7cc48d559b735c60256c21' -or @($final.status).Count -ne 0 -or @($final.index).Count -ne 0){throw 'completion or final source verification failed'}
    foreach($entry in $manifest.files){if((Get-FileHash -LiteralPath (Join-Path $PSScriptRoot $entry.path) -Algorithm SHA256).Hash -ine $entry.sha256 -or (Get-FileHash -LiteralPath (Join-Path $preparedCopy $entry.path) -Algorithm SHA256).Hash -ine $entry.sha256){throw 'prepared source or copy changed during window'}}
    if((Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash -ine $ExpectedManifestSha256 -or (Get-FileHash -LiteralPath (Join-Path $preparedCopy 'manifest.json') -Algorithm SHA256).Hash -ine $ExpectedManifestSha256){throw 'prepared manifest changed during window'}
    $record.state='completed';Save-Json (Join-Path $AttemptDir 'launcher.json') $record
    $lock.Dispose();$lock=$null;Remove-Item -LiteralPath $gate
    exit 0
}catch{
    $reason=$_|Out-String
    if($active -and $null -ne $child){Hold-Original $child $record $reason}
    $record.state='stopped';$record['reason']=$reason;$record['ended_utc']=[DateTime]::UtcNow.ToString('o')
    try{Save-Json (Join-Path $AttemptDir 'launcher-incident.json') $record}catch{}
    if($null -ne $lock){$lock.Dispose()}
    exit 1
}
