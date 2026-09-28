$ErrorActionPreference='Stop'
$taskRoot=Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $taskRoot
$taskLog=Join-Path $taskRoot '.local/logs/next15-final-attempt2'
$meta=Get-Content -Raw (Join-Path $taskLog 'meta.json') | ConvertFrom-Json
if($meta.status -like 'running_*'){throw 'Do not seal a running gate'}
$summaryPath=Join-Path $taskLog 'suite-summary.json'
if(Test-Path $summaryPath){throw 'Preserve existing suite summary'}
$helpers=@('credential_input::tests::redirected_process_child','credential_input::windows_console::tests::isolated_console_child','git_worktree::tests::w05_isolated_environment_helper','host_configuration::tests::transaction_child','host_configuration_crash_tests::host_process_child','provider::network_tests::proxy_environment_child')
$suites=[Collections.Generic.List[object]]::new()
$entries=[Collections.Generic.List[object]]::new()
$current='unresolved'
$lineNumber=0
foreach($line in Get-Content (Join-Path $taskLog 'test.log')){
 $lineNumber++
 if($line -match '^\s*(?:Running|Doc-tests)\s+(.+)$'){$current=$line.Trim()}
 if($line -match '^test (?<name>.+) \.\.\. (?<outcome>ok|FAILED|ignored)(?<detail>.*)$'){
  $entries.Add([ordered]@{suite=$current;line=$lineNumber;name=$Matches.name;outcome=$Matches.outcome;detail=$Matches.detail;helper=($Matches.name -in $helpers)})
 }
 if($line -match '^test result: (?<result>\w+)\. (?<passed>\d+) passed; (?<failed>\d+) failed; (?<ignored>\d+) ignored; (?<measured>\d+) measured; (?<filtered>\d+) filtered out;(?<tail>.*)$'){
  $suites.Add([ordered]@{suite=$current;line=$lineNumber;result=$Matches.result;passed=[int]$Matches.passed;failed=[int]$Matches.failed;ignored=[int]$Matches.ignored;measured=[int]$Matches.measured;filtered=[int]$Matches.filtered;tail=$Matches.tail})
 }
}
$totals=[ordered]@{passed=0;failed=0;ignored=0;measured=0;filtered=0}
foreach($s in $suites){foreach($key in @('passed','failed','ignored','measured','filtered')){$totals[$key]+=$s[$key]}}
$helperEntries=@($entries | Where-Object {$_['helper']})
$ignoredEntries=@($entries | Where-Object {$_['outcome'] -eq 'ignored'})
$summary=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');candidate=$meta.head;status=$meta.status;test_log_sha256=(Get-FileHash (Join-Path $taskLog 'test.log')).Hash;suite_count=$suites.Count;totals=$totals;suites=$suites;helper_entries=$helperEntries;ignored_entries=$ignoredEntries;all_test_entry_count=$entries.Count;entries=$entries;warning='Raw Cargo result blocks retained; helper exclusions are separately stated, not substituted for raw totals'}
$summary | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $summaryPath -Encoding utf8
$before=Get-Content -Raw (Join-Path $taskRoot '.local/next15-attempt2-before.json') | ConvertFrom-Json
$fixtureRecords=[Collections.Generic.List[object]]::new()
foreach($file in Get-ChildItem (Join-Path $taskRoot '.local/tmp') -Filter lifecycle.jsonl -Recurse -File){
 $relative=[IO.Path]::GetRelativePath($taskRoot,$file.FullName).Replace('\','/')
 if($relative -in $before.existing_lifecycle_files.relative_path){continue}
 $events=@(Get-Content -LiteralPath $file.FullName | Where-Object {$_} | ForEach-Object {ConvertFrom-Json $_})
 $start=@($events | Where-Object {$_.event -eq 'fixture_retained'})
 if($start.Count -ne 1){continue}
 $dir=$file.Directory.FullName
 $files=@(Get-ChildItem -LiteralPath $dir -Recurse -File -Force | Sort-Object FullName | ForEach-Object {[ordered]@{relative_path=[IO.Path]::GetRelativePath($taskRoot,$_.FullName).Replace('\','/');fixture_relative=[IO.Path]::GetRelativePath($dir,$_.FullName).Replace('\','/');bytes=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName).Hash}})
 $fixtureRecords.Add([ordered]@{phase=$start[0].phase;parent_pid=$start[0].parent_pid;root=[IO.Path]::GetRelativePath($taskRoot,$dir).Replace('\','/');lifecycle_sha256=(Get-FileHash -LiteralPath $file.FullName).Hash;event_count=$events.Count;events=$events;file_count=$files.Count;files=$files;preserved_original_only=$true})
}
$fixtureRecords | ConvertTo-Json -Depth 12 | Set-Content (Join-Path $taskLog 'w08-fixture-inventory.json') -Encoding utf8
$owned=@(Get-CimInstance Win32_Process | Where-Object {($_.Name -match '^(cargo|rustc|rustfmt|peachsh.*|next15.*)\.exe$') -and (($_.ExecutablePath -like "$taskRoot*") -or ($_.CommandLine -like "*$taskRoot*"))} | Select-Object ProcessId,Name,ExecutablePath)
$freeze=[ordered]@{utc=[DateTime]::UtcNow.ToString('o');head=(& git rev-parse HEAD).Trim();no_man_code_tree=(& git rev-parse HEAD:NoManCode).Trim();rust_tree=(& git rev-parse HEAD:NoManCode/rust-app).Trim();branch=(& git branch --show-current).Trim();status=@(git status --short);index=@(git diff --cached --name-only);unmerged=@(git ls-files -u);owned_running_processes=$owned;gate_status=$meta.status;statement='source/index clean/no-build after final attempt2; no further gate or diagnostic run'}
if($freeze.head -ne $meta.head -or $freeze.no_man_code_tree -ne $meta.no_man_code_tree -or $freeze.rust_tree -ne $meta.rust_tree -or $freeze.status.Count -or $freeze.index.Count -or $freeze.unmerged.Count -or $owned.Count){throw 'Final freeze invariant failed'}
$freeze | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $taskLog 'freeze-receipt.json') -Encoding utf8
[ordered]@{gate_status=$meta.status;suite_count=$suites.Count;totals=$totals;helpers=$helperEntries;ignored=$ignoredEntries;w08=@($fixtureRecords | ForEach-Object {[ordered]@{phase=$_['phase'];parent_pid=$_['parent_pid'];root=$_['root'];event_count=$_['event_count'];file_count=$_['file_count']}});freeze=$freeze} | ConvertTo-Json -Depth 8
