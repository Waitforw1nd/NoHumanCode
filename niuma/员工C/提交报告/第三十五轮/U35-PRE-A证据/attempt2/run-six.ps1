$ErrorActionPreference='Stop'
$attempt='E:/code/nhc-c-domain-authority/.local/u35-pre-a-attempt2'
$env:TMP="$attempt/tmp"
$env:TEMP="$attempt/tmp"
$binary="$attempt/peachsh-libtest.exe"
$names=@('same_file_and_hard_link_contend_for_one_mutex','delete_sharing_and_disposition_controls','domain_lock_covers_second_directory_and_late_hard_link','fresh_database_competitors_have_one_owner','directory_reparse_is_rejected','connections_keep_the_domain_pinned_until_the_last_one_drops')
$records=@()
foreach ($name in $names) {
  $full="execution_authority::tests::$name"
  $start=(Get-Date).ToUniversalTime().ToString('o')
  $ErrorActionPreference='Continue'
  & $binary $full --exact --nocapture --test-threads=1 *> "$attempt/$name.log"
  $rc=$LASTEXITCODE
  $ErrorActionPreference='Stop'
  $end=(Get-Date).ToUniversalTime().ToString('o')
  $output=Get-Content -Raw "$attempt/$name.log"
  $covered=$output -match 'test result: ok\. 1 passed; 0 failed; 0 ignored;'
  $records+= [pscustomobject]@{name=$full;command="$binary $full --exact --nocapture --test-threads=1";start_utc=$start;end_utc=$end;exit_code=$rc;one_pass=$covered}
  $records | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 "$attempt/test-meta.json"
  Write-Output "$name exit=$rc one_pass=$covered"
  if ($rc -ne 0 -or -not $covered) { exit 1 }
}
