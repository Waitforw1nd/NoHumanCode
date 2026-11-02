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
    if($entry.path -notin @('GateProcess.cs','case16_pss.cpp','launch.ps1','README.md')){throw 'unexpected prepared file'}
    $path=Join-Path $PSScriptRoot $entry.path
    if((Get-Item -LiteralPath $path).Length -ne $entry.bytes -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.sha256){throw 'prepared input mismatch'}
}
if(@($manifest.files).Count -ne 4){throw 'incomplete manifest'}
function Save-Json($path,$value){ConvertTo-Json -InputObject $value -Depth 10|Set-Content -LiteralPath $path -Encoding UTF8}
function Monitor-Process($native,$meta,$stdout,$stderr,$limit,$diagnostic){
    $clock=[Diagnostics.Stopwatch]::StartNew();$timedOut=$false;$querySeen=$false;$releaseUnknown=$false
    while($true){
        if($diagnostic){
            $content=[string](Get-Content -Raw -LiteralPath $stdout)
            if(-not $querySeen -and $content.Contains('QUERY_COMPLETE:')){
                $querySeen=$true;$meta['query_complete_utc']=[DateTime]::UtcNow.ToString('o');$meta.state=if($timedOut){'query_complete_late'}else{'query_complete_waiting_release'};Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
            }
            if(-not $releaseUnknown -and $content.Contains('RELEASE_UNKNOWN')){
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
                        $contents=[string](Get-Content -Raw -LiteralPath $outPath -ErrorAction Stop)
                        $releaseConfirmed=($code -eq 0 -and $contents.Contains('RELEASE_COMPLETE'))
                    }
                    if($releaseConfirmed){
                        $record.state='exception_path_confirmed_signal_exit';$record['signal_utc']=[DateTime]::UtcNow.ToString('o');$record['exit_code']=$code;$record['release_confirmed']=$releaseConfirmed
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
$kernelBase=Join-Path ([Environment]::SystemDirectory) 'KernelBase.dll'
foreach($p in @($cl,(Join-Path $bin 'link.exe'),(Join-Path $msvc 'lib/x64'),(Join-Path $sdkLib 'um/x64/kernel32.lib'),(Join-Path $sdkLib 'ucrt/x64/ucrt.lib'),(Join-Path $sdkInc 'um'),(Join-Path $sdkInc 'shared'),(Join-Path $sdkInc 'ucrt'),$kernelBase)){if(-not (Test-Path -LiteralPath $p)){throw "fixed toolchain unavailable: $p"}}
$env:PATH="$bin;$env:PATH";$env:LIB="$(Join-Path $msvc 'lib/x64');$(Join-Path $sdkLib 'ucrt/x64');$(Join-Path $sdkLib 'um/x64')";$env:INCLUDE="$(Join-Path $msvc 'include');$(Join-Path $sdkInc 'ucrt');$(Join-Path $sdkInc 'shared');$(Join-Path $sdkInc 'um')"
Save-Json (Join-Path $AttemptDir 'environment.json') @{msvc=$msvc;bin=$bin;sdk=$sdkVer;tmp=$temp;attempt=$AttemptDir;cl_sha256=(Get-FileHash -LiteralPath $cl -Algorithm SHA256).Hash.ToLowerInvariant();link_sha256=(Get-FileHash -LiteralPath (Join-Path $bin 'link.exe') -Algorithm SHA256).Hash.ToLowerInvariant();kernel32_lib_sha256=(Get-FileHash -LiteralPath (Join-Path $sdkLib 'um/x64/kernel32.lib') -Algorithm SHA256).Hash.ToLowerInvariant();kernelbase_dll=$kernelBase;kernelbase_dll_sha256=(Get-FileHash -LiteralPath $kernelBase -Algorithm SHA256).Hash.ToLowerInvariant()}
Add-Type -Path (Join-Path $copyDir 'GateProcess.cs')
$active=$false;$native=$null;$meta=$null;$diagnosticActive=$false
try {
$source=Join-Path $copyDir 'case16_pss.cpp';$exe=Join-Path $AttemptDir 'case16_pss.exe';$obj=Join-Path $AttemptDir 'case16_pss.obj'
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
if($result.timed_out -or $result.code -ne 0){$native.Handle.Dispose();throw 'compile failed or completed after timeout; diagnostic not started'}
$native.Handle.Dispose()
if(-not (Test-Path -LiteralPath $exe -PathType Leaf)){throw 'compiled diagnostic missing'}
$exeHash=(Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
Save-Json (Join-Path $AttemptDir 'diagnostic-artifact.json') @{path=$exe;bytes=(Get-Item -LiteralPath $exe).Length;sha256=$exeHash;source_sha256=(Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()}
$release=Join-Path $AttemptDir 'release.request';if(Test-Path -LiteralPath $release){throw 'release path already exists'}
$out=Join-Path $AttemptDir 'diagnostic.stdout';$err=Join-Path $AttemptDir 'diagnostic.stderr';$argsLine='"'+$release+'"'
$meta=[ordered]@{stage='diagnostic';state='starting';command=('"'+$exe+'" '+$argsLine);pid=$null;creation_filetime=$null;started_utc=[DateTime]::UtcNow.ToString('o');release_path=$release;query_timeout_seconds=120}
Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
$native=[B21GateProcess]::Start($exe,$argsLine,$AttemptDir,$out,$err);$active=$true;$diagnosticActive=$true
$meta.pid=$native.Pid;$meta.creation_filetime=$native.CreationFileTime;$meta['creation_error']=$native.CreationError;$meta.state='suspended';Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
if($native.CreationFileTime -eq 0 -or -not $native.Resume()){throw 'diagnostic identity or resume unavailable'}
$native.Thread.Dispose();$meta.state='running';Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
$result=Monitor-Process $native $meta $out $err 120 $true
$finalOut=[string](Get-Content -Raw -LiteralPath $out)
if($result.code -ne 0 -or -not $result.query_seen -or $result.release_unknown -or -not $finalOut.Contains('RELEASE_COMPLETE')){throw 'unexpected diagnostic exit or unresolved release'}
$active=$false
$native.Handle.Dispose();$meta.state=if($result.timed_out){'released_after_query_timeout'}else{'released'};Save-Json (Join-Path $AttemptDir 'diagnostic.json') $meta
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
