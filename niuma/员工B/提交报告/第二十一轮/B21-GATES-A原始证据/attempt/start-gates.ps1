$ErrorActionPreference='Stop'
$PSNativeCommandUseErrorActionPreference=$false
Set-StrictMode -Version Latest
$base=$PSScriptRoot
$work=(Resolve-Path (Join-Path $base '../..')).Path
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
$env:VCToolsInstallDir='C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.43.34808'
$env:WindowsSdkDir='C:\Program Files (x86)\Windows Kits\10\'
$target='E:\code\nhc-b-tool-call-application\NoManCode\rust-app\.local\next19-b-a7fe2626-attempt4\target'
$env:CARGO_TARGET_DIR=$target
$expected='f898f0b372bada37e2b962890c3dca23935c2f40'
$expectedProduct='77d0ecdf81b4ad90029bbc85db06271cc4478276'
$expectedRust='43a9b604fb5e21b111cc831d98d2a92794d0962f'
$gatePath=Join-Path $work '.local/nhc-w2-window.lock'
$gate=[IO.File]::Open($gatePath,[IO.FileMode]::CreateNew,[IO.FileAccess]::ReadWrite,[IO.FileShare]::Read)
$retain=$true
$current=$null
$started=[DateTime]::UtcNow.ToString('o')
function Save-Json($path,$value){$value|ConvertTo-Json -Depth 10|Set-Content -Encoding UTF8 -LiteralPath $path}
function Gate($value){$b=[Text.Encoding]::UTF8.GetBytes(($value|ConvertTo-Json -Depth 10));$gate.Position=0;$gate.SetLength(0);$gate.Write($b,0,$b.Length);$gate.Flush($true)}
function Snapshot($name){
 $head=(& git rev-parse HEAD).Trim();if($LASTEXITCODE -ne 0){throw 'HEAD query failed'}
 $product=(& git rev-parse 'HEAD:NoManCode').Trim();if($LASTEXITCODE -ne 0){throw 'product tree query failed'}
 $rust=(& git rev-parse 'HEAD:NoManCode/rust-app').Trim();if($LASTEXITCODE -ne 0){throw 'Rust tree query failed'}
 $status=@(& git status --porcelain --untracked-files=all);if($LASTEXITCODE -ne 0){throw 'status failed'}
 $index=@(& git diff --cached --name-only);if($LASTEXITCODE -ne 0){throw 'index query failed'}
 $state=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');head=$head;product_tree=$product;rust_tree=$rust;status=$status;index=$index}
 Save-Json (Join-Path $base ("source-"+$name+'.json')) $state
 if($head -ne $expected -or $product -ne $expectedProduct -or $rust -ne $expectedRust -or $status.Count -ne 0 -or $index.Count -ne 0){throw 'source identity or cleanliness mismatch'}
 $paths=@(& git -C $repo ls-files -- NoManCode);if($LASTEXITCODE -ne 0){throw 'source listing failed'}
 $entries=@(foreach($path in $paths){$f=Join-Path $repo $path;[pscustomobject]@{path=$path;sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $f).Hash.ToLowerInvariant();bytes=(Get-Item -LiteralPath $f).Length}})
 Save-Json (Join-Path $base ("source-files-"+$name+'.json')) $entries
}
function Fail($message){throw $message}
try {
 Gate ([ordered]@{state='preflight';attempt=$base;started_utc=$started})
 Snapshot 'before'
 $setup=Get-Content -Raw -LiteralPath (Join-Path $work 'build.ps1')
 $setup=$setup.Substring(0,$setup.IndexOf('switch ($Action)'))
 $setup=$setup -replace "param\(\[ValidateSet\([^\r\n]*\r?\n", ''
 $setup=$setup -replace 'Set-Location -LiteralPath \$PSScriptRoot', ''
 Invoke-Expression $setup *> (Join-Path $base 'toolchain.log')
 Add-Type -Path (Join-Path $base 'GateDirectProcess.cs')
 if(-not [NhcDirectProcess]::HasHiddenConsole()){throw 'hidden inherited console not established'}
 $cargo=(Get-Command cargo -ErrorAction Stop).Source
 Save-Json (Join-Path $base 'environment.json') ([ordered]@{started_utc=$started;target=$target;tmp=$env:TEMP;jobs=2;dev_debug=0;test_debug=0;incremental=0;offline=$true;locked=$true;test_threads=1;msvc=$taskMsvc;sdk=$taskSdk.FullName;cargo=$cargo;hidden_console_verified=$true;launcher_flags='CREATE_SUSPENDED, inherited hidden console';runner_pid=$PID;case_timeout_seconds=1200})
 $commands=@(
  [pscustomobject]@{name='01-check';args="check --workspace --all-targets --offline --locked --target-dir `"$target`""},
  [pscustomobject]@{name='02-build-host';args="build --offline --locked --target-dir `"$target`" --bin peachsh"},
  [pscustomobject]@{name='03-test-serial';args="test --workspace --offline --locked --target-dir `"$target`" -- --test-threads=1"},
  [pscustomobject]@{name='04-fmt';args='fmt --all -- --check'},
  [pscustomobject]@{name='05-clippy';args="clippy --workspace --all-targets --offline --locked --target-dir `"$target`" -- -D warnings"},
  [pscustomobject]@{name='06-wasm';args="check -p peachsh-ui --target wasm32-unknown-unknown --offline --locked --target-dir `"$target`""}
 )
 foreach($c in $commands){
  Snapshot ('before-'+$c.name)
  $stdout=Join-Path $base ($c.name+'.stdout');$stderr=Join-Path $base ($c.name+'.stderr');$meta=Join-Path $base ($c.name+'.json')
  $current=[ordered]@{name=$c.name;command=('cargo '+$c.args);start_utc=[DateTime]::UtcNow.ToString('o');end_utc=$null;state='starting';pid=$null;creation_filetime=$null;exit_code=$null;stdout=$stdout;stderr=$stderr;timeout_seconds=1200}
  Save-Json $meta $current;Gate $current
  $clock=[Diagnostics.Stopwatch]::StartNew()
  $native=[NhcDirectProcess]::Start($cargo,$c.args,$work,$stdout,$stderr)
  $current.pid=$native.Pid;$current.creation_filetime=$native.CreationFileTime;$current.state='running';Save-Json $meta $current;Gate $current
  if($native.CreationFileTime -eq 0){throw 'process creation identity unavailable'}
  $process=[Diagnostics.Process]::GetProcessById($native.Pid);$null=$process.Handle
  if($process.StartTime.ToUniversalTime().ToFileTimeUtc() -ne $native.CreationFileTime){throw 'process identity mismatch'}
  $native.Resume()
  while($true){
   $out=[string](Get-Content -Raw -LiteralPath $stdout);$err=[string](Get-Content -Raw -LiteralPath $stderr)
   if([string]::Concat($out,$err).Contains('NHC_TERMINAL_UNKNOWN:')){throw 'terminal unknown marker; process retained'}
   if($clock.Elapsed.TotalSeconds -ge 1200){throw 'step deadline exceeded; process retained'}
   if($process.HasExited){break}
   Start-Sleep -Milliseconds 250
  }
  $current.end_utc=[DateTime]::UtcNow.ToString('o');$current.exit_code=$process.ExitCode;$current.state='exited';$current['elapsed_ms']=[long]$clock.ElapsedMilliseconds
  Save-Json $meta $current;Gate $current
  if($process.ExitCode -ne 0){throw "step $($c.name) failed exit $($process.ExitCode)"}
  $native.Thread.Dispose();$native.Handle.Dispose();$process.Dispose()
  if($c.name -eq '02-build-host'){
   $artifactDir=Join-Path $base 'artifact';New-Item -ItemType Directory -Path $artifactDir|Out-Null
   $src=Join-Path $target 'debug/peachsh.exe';$copy=Join-Path $artifactDir 'peachsh.exe'
   Copy-Item -LiteralPath $src -Destination $copy
   $srcHash=(Get-FileHash -Algorithm SHA256 -LiteralPath $src).Hash.ToLowerInvariant();$copyHash=(Get-FileHash -Algorithm SHA256 -LiteralPath $copy).Hash.ToLowerInvariant()
   Save-Json (Join-Path $base 'host-artifact.json') ([ordered]@{source=$src;snapshot=$copy;source_sha256=$srcHash;snapshot_sha256=$copyHash;bytes=(Get-Item -LiteralPath $copy).Length})
   if($srcHash -ne $copyHash){throw 'host artifact copy mismatch'}
  }
  Snapshot ('after-'+$c.name)
  $current.state='passed';Save-Json $meta $current;Gate $current
 }
 $host=Get-Content -Raw (Join-Path $base 'host-artifact.json')|ConvertFrom-Json
 $finalHash=(Get-FileHash -Algorithm SHA256 -LiteralPath $host.snapshot).Hash.ToLowerInvariant()
 Save-Json (Join-Path $base 'host-artifact-final.json') ([ordered]@{snapshot_sha256=$finalHash;expected=$host.snapshot_sha256;source_sha256=(Get-FileHash -Algorithm SHA256 -LiteralPath $host.source).Hash.ToLowerInvariant()})
 if($finalHash -ne $host.snapshot_sha256){throw 'host artifact snapshot changed'}
 Snapshot 'final'
 $retain=$false
 Save-Json (Join-Path $base 'window-result.json') ([ordered]@{state='completed';exit_code=0;started_utc=$started;ended_utc=[DateTime]::UtcNow.ToString('o')})
} catch {
 $errorText=$_|Out-String
 $record=[ordered]@{state='stopped';reason=$errorText;at_utc=[DateTime]::UtcNow.ToString('o');attempt=$base;current=$current;instruction='Do not restart until manager resolves incident and any retained process objects.'}
 Gate $record
 Save-Json (Join-Path $base 'incident.json') $record
 try{Snapshot 'stopped'}catch{[IO.File]::WriteAllText((Join-Path $base 'source-stop-error.txt'),($_|Out-String))}
 Save-Json (Join-Path $base 'window-result.json') ([ordered]@{state='failed';exit_code=1;started_utc=$started;ended_utc=[DateTime]::UtcNow.ToString('o');reason=$errorText})
} finally {
 $gate.Dispose()
 if(-not $retain){Remove-Item -LiteralPath $gatePath}
}
if($retain){exit 1}