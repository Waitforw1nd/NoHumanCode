$ErrorActionPreference='Stop'
$taskRoot=Split-Path -Parent $PSScriptRoot
$taskRustRoot=Join-Path $taskRoot 'NoManCode/rust-app'
Set-Location -LiteralPath $taskRustRoot
$taskLog=Join-Path $taskRoot '.local/logs/next15-w08-diagnostic1'
$taskTmp=Join-Path $taskRoot '.local/tmp/next15-w08-diagnostic1'
if((Test-Path $taskLog) -or (Test-Path $taskTmp)){throw 'One diagnostic only; refuse overwrite or rerun'}
New-Item -ItemType Directory -Path $taskLog,$taskTmp,(Join-Path $taskLog 'snapshots') | Out-Null
$env:TMP=$taskTmp
$env:TEMP=$taskTmp
$env:RUST_BACKTRACE='1'
$env:RUSTUP_AUTO_INSTALL='0'
$env:CARGO_NET_OFFLINE='true'
$env:CARGO_TARGET_DIR=Join-Path $taskRoot '.local/target'
$env:CARGO_PROFILE_DEV_DEBUG='0'
$env:CARGO_PROFILE_TEST_DEBUG='0'
$env:CARGO_INCREMENTAL='0'
$env:CARGO_BUILD_JOBS='2'
$taskBin=Join-Path $taskRoot '.local/target/debug/deps/peachsh-dc94de63d8d31b10.exe'
$taskTest='server::worktree_http_crash_tests::w08_real_router_process_kill_wait_at_three_durable_boundaries'
$taskMeta=[ordered]@{scope='One exact existing-binary diagnostic; not a final gate or acceptance rerun';start_utc=[DateTime]::UtcNow.ToString('o');command=$taskBin+' --exact '+$taskTest+' --nocapture';cwd=$taskRustRoot;tmp=$taskTmp;backtrace='1';head=(git rev-parse HEAD).Trim();no_man_code_tree=(git rev-parse HEAD:NoManCode).Trim();rust_tree=(git rev-parse HEAD:NoManCode/rust-app).Trim();status_before=@(git status --short);index_before=@(git diff --cached --name-only);binary_sha256=(Get-FileHash $taskBin).Hash;snapshot_limitations='External polling observes files best-effort; individual byte versions are preserved but cross-file SQLite snapshots are not asserted atomic. No source/timeout/assertion changes.';observations=@();process_samples=@()}
if($taskMeta.head -ne 'fa9623785573a8f44decd0368fa73a4e267f4ed6' -or $taskMeta.status_before.Count -or $taskMeta.index_before.Count){throw 'Frozen source mismatch'}
$taskMetaPath=Join-Path $taskLog 'meta.json'
$taskMeta | ConvertTo-Json -Depth 8 | Set-Content $taskMetaPath -Encoding utf8
$taskSeen=@{}
$taskSequence=0
$taskNextProcessSample=[DateTime]::MinValue
$taskProcess=Start-Process -FilePath $taskBin -ArgumentList @('--exact',$taskTest,'--nocapture') -WorkingDirectory $taskRustRoot -RedirectStandardOutput (Join-Path $taskLog 'parent.stdout.log') -RedirectStandardError (Join-Path $taskLog 'parent.stderr.log') -WindowStyle Hidden -PassThru
$taskMeta.parent_pid=$taskProcess.Id
$taskDeadline=[DateTime]::UtcNow.AddSeconds(120)
while(-not $taskProcess.HasExited){
    if([DateTime]::UtcNow -gt $taskDeadline){throw 'Diagnostic external bound exceeded; inspect exact owned process before stopping'}
    $taskFiles=@(Get-ChildItem -LiteralPath $taskTmp -Recurse -Force -File -ErrorAction SilentlyContinue)
    foreach($taskFile in $taskFiles){
        $taskRelative=[IO.Path]::GetRelativePath($taskTmp,$taskFile.FullName).Replace([string][char]92,'/')
        $taskSignature=$taskFile.Length.ToString()+':'+$taskFile.LastWriteTimeUtc.Ticks.ToString()
        if($taskSeen[$taskRelative] -eq $taskSignature){continue}
        try {
            $taskStream=[IO.File]::Open($taskFile.FullName,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
            $taskMemory=[IO.MemoryStream]::new()
            try {$taskStream.CopyTo($taskMemory);$taskBytes=$taskMemory.ToArray()} finally {$taskStream.Dispose();$taskMemory.Dispose()}
            $taskDigest=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($taskBytes))
            $taskSequence++
            $taskSaved=('snapshots/{0:D5}-{1}' -f $taskSequence,$taskFile.Name)
            [IO.File]::WriteAllBytes((Join-Path $taskLog $taskSaved),$taskBytes)
            $taskMeta.observations+=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');fixture_path=$taskRelative;saved=$taskSaved;bytes=$taskBytes.Length;sha256=$taskDigest}
            $taskSeen[$taskRelative]=$taskSignature
        } catch [IO.IOException] {
            $taskMeta.observations+=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');fixture_path=$taskRelative;read_error=$_.Exception.GetType().Name;note='File changed/disappeared during parent cleanup; no snapshot claimed'}
        }
    }
    if([DateTime]::UtcNow -ge $taskNextProcessSample){
        $taskProcesses=@(Get-CimInstance Win32_Process | Where-Object {$_.ExecutablePath -eq $taskBin.Replace('/',[string][char]92)} | Select-Object ProcessId,ParentProcessId,Name,CreationDate)
        $taskMeta.process_samples+=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');processes=$taskProcesses}
        $taskMeta | ConvertTo-Json -Depth 8 | Set-Content $taskMetaPath -Encoding utf8
        $taskNextProcessSample=[DateTime]::UtcNow.AddMilliseconds(250)
    }
    Start-Sleep -Milliseconds 20
    $taskProcess.Refresh()
}
$taskProcess.WaitForExit()
$taskMeta.exit_code=$taskProcess.ExitCode
$taskMeta.end_utc=[DateTime]::UtcNow.ToString('o')
$taskMeta.status_after=@(git status --short)
$taskMeta.index_after=@(git diff --cached --name-only)
$taskMeta.head_after=(git rev-parse HEAD).Trim()
$taskMeta.no_man_code_tree_after=(git rev-parse HEAD:NoManCode).Trim()
$taskMeta.rust_tree_after=(git rev-parse HEAD:NoManCode/rust-app).Trim()
$taskMeta.remaining_fixture_files=@(Get-ChildItem -LiteralPath $taskTmp -Recurse -Force -File -ErrorAction SilentlyContinue | ForEach-Object {[IO.Path]::GetRelativePath($taskTmp,$_.FullName).Replace([string][char]92,'/')})
$taskMeta.remaining_owned_processes=@(Get-CimInstance Win32_Process | Where-Object {$_.ExecutablePath -eq $taskBin.Replace('/',[string][char]92)} | Select-Object ProcessId,ParentProcessId,Name)
$taskMeta | ConvertTo-Json -Depth 8 | Set-Content $taskMetaPath -Encoding utf8
Get-Content (Join-Path $taskLog 'parent.stdout.log')
Get-Content (Join-Path $taskLog 'parent.stderr.log')
[ordered]@{exit_code=$taskMeta.exit_code;observations=$taskMeta.observations.Count;remaining_owned_processes=$taskMeta.remaining_owned_processes;head=$taskMeta.head_after} | ConvertTo-Json -Depth 4
