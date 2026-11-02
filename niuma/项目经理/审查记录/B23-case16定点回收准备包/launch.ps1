param(
    [Parameter(Mandatory=$true)][string]$AttemptDir,
    [Parameter(Mandatory=$true)][string]$ExpectedManifestSha256
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$work=(Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
$AttemptDir=[IO.Path]::GetFullPath($AttemptDir)
if(-not $AttemptDir.StartsWith('E:\',[StringComparison]::OrdinalIgnoreCase) -or (Split-Path -Parent $AttemptDir) -ine [IO.Path]::GetFullPath((Join-Path $work '.local'))){throw 'attempt must be a new direct E: Rust .local child'}
if(Test-Path -LiteralPath $AttemptDir){throw 'attempt already exists'}
if($ExpectedManifestSha256 -notmatch '^[a-fA-F0-9]{64}$'){throw 'complete manifest SHA required'}
$manifestPath=Join-Path $PSScriptRoot 'manifest.json'
if((Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash -ine $ExpectedManifestSha256){throw 'manifest mismatch'}
$manifest=Get-Content -Raw -LiteralPath $manifestPath|ConvertFrom-Json
if($manifest.carrier_pid -ne 89780 -or [string]$manifest.carrier_creation -cne '134381029069968888'){throw 'wrong carrier identity'}
foreach($entry in $manifest.files){
    if($entry.path -notin @('GateProcess.cs','recover.cpp','launch.ps1','README.md')){throw 'unexpected prepared file'}
    $path=Join-Path $PSScriptRoot $entry.path
    if((Get-Item -LiteralPath $path).Length -ne $entry.bytes -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.sha256){throw 'prepared input mismatch'}
}
if(@($manifest.files).Count -ne 4){throw 'incomplete manifest'}
function Save-Json($path,$value){ConvertTo-Json -InputObject $value -Depth 10|Set-Content -LiteralPath $path -Encoding UTF8}
function Monitor-Process($native,$meta,$stdout,$stderr,$limit,$diagnostic){
    $clock=[Diagnostics.Stopwatch]::StartNew();$timedOut=$false;$querySeen=$false;$releaseUnknown=$false
    while($true){
        if($diagnostic){
            $rawContent=Get-Content -Raw -LiteralPath $stdout; $content=if($null -eq $rawContent){''}else{[string]$rawContent}
            if(-not $querySeen -and $content.Contains('PREFLIGHT_READY:')){
                $querySeen=$true;$meta['query_complete_utc']=[DateTime]::UtcNow.ToString('o');$meta.state=if($timedOut){'query_complete_late'}else{'query_complete_waiting_release'};Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
            }
            if(-not $releaseUnknown -and $content.Contains('RECOVERY_UNKNOWN')){
                $releaseUnknown=$true;$meta['release_unknown_utc']=[DateTime]::UtcNow.ToString('o');$meta.state='release_unknown_retaining';Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
            }
        }
        $wait=$native.Wait(250)
        if($wait -eq 0){
            [uint32]$code=0
            if(-not $native.ExitCode([ref]$code)){ $meta.state='unknown_exit_query';$meta['reason']=$native.LastError; Save-Json (Join-Path $AttemptDir 'incident.json') $meta; while($true){Start-Sleep -Seconds 1} }
            $meta['signal_utc']=[DateTime]::UtcNow.ToString('o');$meta['exit_code']=$code;$meta['elapsed_ms']=[long]$clock.ElapsedMilliseconds
            $meta.state=if($timedOut){'late_signal'}else{'exited'}
            Save-Json (Join-Path $AttemptDir $(if($diagnostic){'diagnostic.json'}else{'compile.json'})) $meta
            return @{code=$code;timed_out=$timedOut;query_seen=$querySeen;release_unknown=$releaseUnknown}
        }
        if($wait -ne 258){$meta.state='unknown_wait';$meta['reason']=$native.LastError;Save-Json (Join-Path $AttemptDir 'incident.json') $meta;while($true){Start-Sleep -Seconds 1}}
        if(-not $timedOut -and $clock.Elapsed.TotalSeconds -ge $limit -and (-not $diagnostic -or -not $querySeen)){
            $timedOut=$true;$meta.state='observation_timeout_retaining';$meta['timeout_utc']=[DateTime]::UtcNow.ToString('o');Save-Json (Join-Path $AttemptDir 'incident.json') $meta
        }
    }
}
function Hold-ActiveAfterException($native,$meta,$diagnostic,$reason){
    $record=[ordered]@{state='monitor_exception_retaining';reason=$reason;stage=$meta.stage;pid=$meta.pid;creation_filetime=$meta.creation_filetime;detected_utc=[DateTime]::UtcNow.ToString('o')}
    try{Save-Json (Join-Path $AttemptDir 'incident-monitor-exception.json') $record}catch{}
    while($true){
        try{
            $wait=$native.Wait(1000)
            if($wait -eq 0){
                [uint32]$code=0
                if($native.ExitCode([ref]$code)){
                    $releaseConfirmed=-not $diagnostic
                    if($diagnostic){
                        $outPath=Join-Path $AttemptDir 'diagnostic.stdout'
                        $rawContent=Get-Content -Raw -LiteralPath $outPath -ErrorAction Stop; $contents=if($null -eq $rawContent){''}else{[string]$rawContent}
                        $releaseConfirmed=($code -eq 0 -and $contents.Contains('RECOVERY_LOCAL_HANDLES_RELEASED'))
                    }
                    if($releaseConfirmed){
                        $record.state='exception_path_confirmed_signal_exit';$record['signal_utc']=[DateTime]::UtcNow.ToString('o');$record['exit_code']=$code;$record['recovery_release_confirmed']=$releaseConfirmed
                        try{Save-Json (Join-Path $AttemptDir 'incident-late-signal.json') $record}catch{}
                        return
                    }
                }
            }
        }catch{}
        Start-Sleep -Milliseconds 1000
    }
}
New-Item -ItemType Directory -Path $AttemptDir -ErrorAction Stop|Out-Null
$temp=Join-Path $AttemptDir 'tmp';New-Item -ItemType Directory -Path $temp -ErrorAction Stop|Out-Null
$env:TEMP=$temp;$env:TMP=$temp
$copyDir=Join-Path $AttemptDir 'prepared';New-Item -ItemType Directory -Path $copyDir -ErrorAction Stop|Out-Null
foreach($entry in $manifest.files){$dest=Join-Path $copyDir $entry.path;Copy-Item -LiteralPath (Join-Path $PSScriptRoot $entry.path) -Destination $dest;if((Get-FileHash -LiteralPath $dest -Algorithm SHA256).Hash -ine $entry.sha256){throw 'prepared copy mismatch'}}
Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $copyDir 'manifest.json')
if((Get-FileHash -LiteralPath (Join-Path $copyDir 'manifest.json') -Algorithm SHA256).Hash -ine $ExpectedManifestSha256){throw 'manifest copy mismatch'}
$msvc=Join-Path $env:ProgramFiles 'Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.43.34808'
$bin=Join-Path $msvc 'bin/Hostx86/x64';$sdk=Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10';$sdkVer='10.0.22621.0'
$cl=Join-Path $bin 'cl.exe';$sdkLib=Join-Path $sdk ('Lib/'+$sdkVer);$sdkInc=Join-Path $sdk ('Include/'+$sdkVer)
$kernelBase=Join-Path ([Environment]::SystemDirectory) 'KernelBase.dll';$ntdll=Join-Path ([Environment]::SystemDirectory) 'ntdll.dll'
foreach($p in @($cl,(Join-Path $bin 'link.exe'),(Join-Path $msvc 'lib/x64'),(Join-Path $sdkLib 'um/x64/kernel32.lib'),(Join-Path $sdkLib 'ucrt/x64/ucrt.lib'),(Join-Path $sdkInc 'um'),(Join-Path $sdkInc 'shared'),(Join-Path $sdkInc 'ucrt'),$kernelBase,$ntdll)){if(-not (Test-Path -LiteralPath $p)){throw "fixed toolchain unavailable: $p"}}
$env:PATH="$bin;$env:PATH";$env:LIB="$(Join-Path $msvc 'lib/x64');$(Join-Path $sdkLib 'ucrt/x64');$(Join-Path $sdkLib 'um/x64')";$env:INCLUDE="$(Join-Path $msvc 'include');$(Join-Path $sdkInc 'ucrt');$(Join-Path $sdkInc 'shared');$(Join-Path $sdkInc 'um')"
Save-Json (Join-Path $AttemptDir 'environment.json') @{msvc=$msvc;bin=$bin;sdk=$sdkVer;tmp=$temp;attempt=$AttemptDir;cl_sha256=(Get-FileHash -LiteralPath $cl -Algorithm SHA256).Hash.ToLowerInvariant();link_sha256=(Get-FileHash -LiteralPath (Join-Path $bin 'link.exe') -Algorithm SHA256).Hash.ToLowerInvariant();kernel32_lib_sha256=(Get-FileHash -LiteralPath (Join-Path $sdkLib 'um/x64/kernel32.lib') -Algorithm SHA256).Hash.ToLowerInvariant();kernelbase_dll=$kernelBase;kernelbase_dll_sha256=(Get-FileHash -LiteralPath $kernelBase -Algorithm SHA256).Hash.ToLowerInvariant();ntdll=$ntdll;ntdll_sha256=(Get-FileHash -LiteralPath $ntdll -Algorithm SHA256).Hash.ToLowerInvariant()}
Add-Type -Path (Join-Path $copyDir 'GateProcess.cs')
$active=$false;$native=$null;$meta=$null;$diagnosticActive=$false
try {
$source=Join-Path $copyDir 'recover.cpp';$exe=Join-Path $AttemptDir 'recover.exe';$obj=Join-Path $AttemptDir 'recover.obj'
$clArgs="/nologo /EHsc /std:c++17 /W4 /D_WIN32_WINNT=0x0A00 /Fo`"$obj`" /Fe`"$exe`" `"$source`" /link kernel32.lib"
$out=Join-Path $AttemptDir 'compile.stdout';$err=Join-Path $AttemptDir 'compile.stderr'
$meta=[ordered]@{stage='compile';state='starting';command=('"'+$cl+'" '+$clArgs);pid=$null;creation_filetime=$null;started_utc=[DateTime]::UtcNow.ToString('o')}
Save-Json (Join-Path $AttemptDir 'compile.json') $meta
$native=[B21GateProcess]::Start($cl,$clArgs,$AttemptDir,$out,$err);$active=$true
$meta.pid=$native.Pid;$meta.creation_filetime=$native.CreationFileTime;$meta['creation_error']=$native.CreationError;$meta.state='suspended';Save-Json (Join-Path $AttemptDir 'compile.json') $meta
if($native.CreationFileTime -eq 0 -or -not $native.Resume()){throw 'compile identity or resume unavailable'}
$native.Thread.Dispose();$meta.state='running';Save-Json (Join-Path $AttemptDir 'compile.json') $meta
$result=Monitor-Process $native $meta $out $err 300 $false
$active=$false
if($result.timed_out -or $result.code -ne 0){$native.Handle.Dispose();throw 'compile failed or completed after timeout; recovery tool not started'}
$native.Handle.Dispose()
if(-not (Test-Path -LiteralPath $exe -PathType Leaf)){throw 'compiled diagnostic missing'}
$exeHash=(Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
Save-Json (Join-Path $AttemptDir 'diagnostic-artifact.json') @{path=$exe;bytes=(Get-Item -LiteralPath $exe).Length;sha256=$exeHash;source_sha256=(Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()}
$matrix=Join-Path $work '.local/b23-matrix-98cec2e4-attempt1'
$caseImage=Join-Path $matrix 'matrix/fixed/lib-test.exe';$probeImage=Join-Path $matrix 'matrix/fixed/probe.exe'
$psImage=Join-Path $PSHOME 'pwsh.exe';if(-not (Test-Path -LiteralPath $psImage)){$psImage=Join-Path $PSHOME 'powershell.exe'}
foreach($path in @($caseImage,$probeImage,$psImage)){if(-not (Test-Path -LiteralPath $path -PathType Leaf)){throw "fixed image missing: $path"}}
if((Get-FileHash -LiteralPath $caseImage -Algorithm SHA256).Hash -ine '95a58781a58981fda0fbc4e4bd9517548642800312c7c02ec448fbeccd0d2991' -or (Get-FileHash -LiteralPath $probeImage -Algorithm SHA256).Hash -ine 'bcc63ab6b6dfc982033c2e052d9b88d900fb6fb07c982435206290c9897b67c3'){throw 'matrix fixed image hash mismatch'}
$allow=Join-Path $matrix 'matrix/.tmpio5Sbs/allow';if((Get-FileHash -LiteralPath $allow -Algorithm SHA256).Hash -ine 'eeb0f78804f6805ea438d2029928899de9efeaff5832abb7e62947291309540d'){throw 'allow receipt mismatch'}
foreach($phase in @('01-case','02-runner','03-supervisor','04-launcher','05-release-diagnostic-objects')){if(Test-Path -LiteralPath (Join-Path $AttemptDir ($phase+'.request'))){throw 'unexpected preexisting stage request'}}
$out=Join-Path $AttemptDir 'diagnostic.stdout';$err=Join-Path $AttemptDir 'diagnostic.stderr'
$argsLine='"'+$AttemptDir+'" "'+$caseImage+'" "'+$psImage+'" "'+$probeImage+'" "'+$matrix+'"'
$meta=[ordered]@{stage='diagnostic';state='starting';command=('"'+$exe+'" '+$argsLine);pid=$null;creation_filetime=$null;started_utc=[DateTime]::UtcNow.ToString('o');stage_requests='01-case through 05-release-diagnostic-objects';query_timeout_seconds=120}
Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
$native=[B21GateProcess]::Start($exe,$argsLine,$AttemptDir,$out,$err);$active=$true;$diagnosticActive=$true
$meta.pid=$native.Pid;$meta.creation_filetime=$native.CreationFileTime;$meta['creation_error']=$native.CreationError;$meta.state='suspended';Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
if($native.CreationFileTime -eq 0 -or -not $native.Resume()){throw 'diagnostic identity or resume unavailable'}
$native.Thread.Dispose();$meta.state='running';Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
$result=Monitor-Process $native $meta $out $err 120 $true
$rawContent=Get-Content -Raw -LiteralPath $out; $finalOut=if($null -eq $rawContent){''}else{[string]$rawContent}
if($result.code -ne 0 -or -not $result.query_seen -or $result.release_unknown -or -not $finalOut.Contains('RECOVERY_LOCAL_HANDLES_RELEASED')){throw 'unexpected recovery exit or unresolved handles'}
$active=$false
$native.Handle.Dispose();$meta.state=if($result.timed_out){'recovered_after_preflight_timeout'}else{'recovered_local_handles_released'};Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
exit 0
} catch {
    $reason=$_|Out-String
    if($active -and $null -ne $native){
        Hold-ActiveAfterException $native $meta $diagnosticActive $reason
        $active=$false
        $native.Handle.Dispose()
        if($null -ne $native.Thread){$native.Thread.Dispose()}
    }
    try{Save-Json (Join-Path $AttemptDir 'launcher-error.json') @{state='stopped';reason=$reason}}catch{}
    exit 1
}
