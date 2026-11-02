param(
    [Parameter(Mandatory=$true)][string]$AttemptDir,
    [Parameter(Mandatory=$true)][string]$TargetDir
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
    $taskMsvc=$env:VCToolsInstallDir
    $taskBin=Join-Path $taskMsvc 'bin/Hostx86/x64'
    $sdkVersion='10.0.22621.0'
    $sdkLib=Join-Path $env:WindowsSdkDir ('Lib/'+$sdkVersion)
    $sdkInclude=Join-Path $env:WindowsSdkDir ('Include/'+$sdkVersion)
    foreach($file in @((Join-Path $taskBin 'cl.exe'),(Join-Path $taskBin 'link.exe'),(Join-Path $taskBin 'lib.exe'),(Join-Path $sdkLib 'um/x64/kernel32.lib'),(Join-Path $sdkLib 'ucrt/x64/ucrt.lib'))){if(-not (Test-Path -LiteralPath $file -PathType Leaf)){throw "fixed toolchain missing: $file"}}
    foreach($dir in @((Join-Path $taskMsvc 'include'),(Join-Path $taskMsvc 'lib/x64'),(Join-Path $sdkInclude 'ucrt'),(Join-Path $sdkInclude 'shared'),(Join-Path $sdkInclude 'um'))){if(-not (Test-Path -LiteralPath $dir -PathType Container)){throw "fixed toolchain directory missing: $dir"}}
    $env:PATH="$taskBin;$env:PATH"
    $env:LIB="$(Join-Path $taskMsvc 'lib/x64');$(Join-Path $sdkLib 'ucrt/x64');$(Join-Path $sdkLib 'um/x64')"
    $env:INCLUDE="$(Join-Path $taskMsvc 'include');$(Join-Path $sdkInclude 'ucrt');$(Join-Path $sdkInclude 'shared');$(Join-Path $sdkInclude 'um')"
    $env:CC_x86_64_pc_windows_msvc=Join-Path $taskBin 'cl.exe'
    $env:AR_x86_64_pc_windows_msvc=Join-Path $taskBin 'lib.exe'
    $cargo=(Get-Command cargo -ErrorAction Stop).Source
    $registry=Join-Path $env:USERPROFILE '.cargo/registry'
    if(-not (Test-Path -LiteralPath $registry -PathType Container)){throw 'offline registry cache unavailable'}
    $env:CARGO_HOME=Join-Path $base 'cargo-home'
    New-Item -ItemType Directory -Path $env:CARGO_HOME -ErrorAction Stop|Out-Null
    Copy-Item -LiteralPath $registry -Destination (Join-Path $env:CARGO_HOME 'registry') -Recurse -ErrorAction Stop
    New-Item -ItemType Directory -Path $TargetDir -ErrorAction Stop|Out-Null
    Add-Type -Path (Join-Path $PSScriptRoot 'GateProcess.cs')
    $rustc=(Get-Command rustc -ErrorAction Stop).Source
    $clippy=(Get-Command cargo-clippy -ErrorAction Stop).Source
    $tools=@(foreach($file in @($cargo,$rustc,$clippy,(Join-Path $taskBin 'cl.exe'),(Join-Path $taskBin 'link.exe'),(Join-Path $taskBin 'lib.exe'))){[pscustomobject]@{path=$file;bytes=(Get-Item -LiteralPath $file).Length;sha256=(Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()}})
    Save-Json (Join-Path $base 'tools.json') $tools
    Save-Json (Join-Path $base 'environment.json') ([ordered]@{started_utc=$started;target=$TargetDir;tmp=$env:TEMP;cargo_home=$env:CARGO_HOME;registry_copy_source=$registry;jobs=2;dev_debug=0;test_debug=0;incremental=0;offline=$true;locked=$true;msvc=$taskMsvc;msvc_bin=$taskBin;sdk=$sdkLib;cargo=$cargo;path=$env:PATH;lib=$env:LIB;include=$env:INCLUDE;rustup_home=$env:RUSTUP_HOME;rustup_toolchain=$env:RUSTUP_TOOLCHAIN;launcher_flags='CREATE_SUSPENDED | CREATE_NO_WINDOW';step_timeout_seconds=1200})
    Snapshot-Artifacts 'before'
    $commands=@(
        [pscustomobject]@{name='01-check';args="check --workspace --all-targets --offline --locked --target-dir `"$TargetDir`""},
        [pscustomobject]@{name='02-clippy';args="clippy --workspace --all-targets --offline --locked --target-dir `"$TargetDir`" -- -D warnings"}
    )
    foreach($c in $commands){
        Snapshot ('before-'+$c.name)
        foreach($tool in $tools){if((Get-FileHash -LiteralPath $tool.path -Algorithm SHA256).Hash -ine $tool.sha256 -or (Get-Item -LiteralPath $tool.path).Length -ne $tool.bytes){throw 'tool artifact changed'}}
        $stdout=Join-Path $base ($c.name+'.stdout');$stderr=Join-Path $base ($c.name+'.stderr');$meta=Join-Path $base ($c.name+'.json')
        $current=[ordered]@{name=$c.name;command=('cargo '+$c.args);start_utc=[DateTime]::UtcNow.ToString('o');end_utc=$null;state='starting';pid=$null;creation_filetime=$null;creation_error=$null;exit_code=$null;stdout=$stdout;stderr=$stderr;timeout_seconds=1200}
        Save-Json $meta $current
        $clock=[Diagnostics.Stopwatch]::StartNew()
        $child=[B21GateProcess]::Start($cargo,$c.args,$work,$stdout,$stderr)
        $childActive=$true
        $current.pid=$child.Pid;$current.creation_filetime=$child.CreationFileTime;$current.creation_error=$child.CreationError;$current.state='suspended'
        Save-Json $meta $current;Save-Json (Join-Path $base 'supervisor-state.json') $current
        if($child.CreationFileTime -eq 0){Hold-Unknown $child $current 'GetProcessTimes failed on original handle'}
        if(-not $child.Resume()){Hold-Unknown $child $current ("ResumeThread failed: "+$child.LastError)}
        $child.Thread.Dispose()
        $current.state='running';Save-Json $meta $current
        while($true){
            $out=[string](Get-Content -Raw -LiteralPath $stdout);$err=[string](Get-Content -Raw -LiteralPath $stderr)
            if([string]::Concat($out,$err).Contains('NHC_TERMINAL_UNKNOWN:')){Hold-Unknown $child $current 'terminal unknown marker'}
            if($clock.Elapsed.TotalSeconds -ge 1200){Hold-Unknown $child $current 'step deadline exceeded'}
            $wait=$child.Wait(250)
            if($wait -eq 0){break}
            if($wait -ne 258){Hold-Unknown $child $current ("original-handle wait failed: "+$child.LastError)}
        }
        $out=[string](Get-Content -Raw -LiteralPath $stdout);$err=[string](Get-Content -Raw -LiteralPath $stderr)
        if([string]::Concat($out,$err).Contains('NHC_TERMINAL_UNKNOWN:')){Hold-Unknown $child $current 'terminal unknown marker at exit'}
        $code=[uint32]0
        if(-not $child.ExitCode([ref]$code)){Hold-Unknown $child $current ("GetExitCodeProcess failed: "+$child.LastError)}
        $current.end_utc=[DateTime]::UtcNow.ToString('o');$current.exit_code=$code;$current.state='exited';$current['elapsed_ms']=[long]$clock.ElapsedMilliseconds
        Save-Json $meta $current
        $childActive=$false
        $child.Handle.Dispose();$child=$null
        Snapshot ('after-'+$c.name)
        Snapshot-Artifacts ('after-'+$c.name)
        if($code -ne 0){throw "step $($c.name) failed exit $code"}
        $current.state='passed';Save-Json $meta $current;Save-Json (Join-Path $base 'supervisor-state.json') $current
    }
    Snapshot 'final'
    foreach($tool in $tools){if((Get-FileHash -LiteralPath $tool.path -Algorithm SHA256).Hash -ine $tool.sha256 -or (Get-Item -LiteralPath $tool.path).Length -ne $tool.bytes){throw 'tool artifact changed'}}
    Save-Json (Join-Path $base 'tools-final.json') $tools
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
