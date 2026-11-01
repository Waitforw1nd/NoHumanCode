$ErrorActionPreference='Stop'
$attempt='E:/code/nhc-c-domain-authority/.local/u35-pre-b-attempt1'
$env:TMP="$attempt/tmp"
$env:TEMP="$attempt/tmp"
$binary="$attempt/peachsh-libtest.exe"
$names=@('binding_missing_corrupt_and_truncated_are_closed','observed_binding_corruption_permanently_seals_owner','initialized_missing_db_and_bootstrap_interrupt_refuse_recreation','single_link_move_and_whole_directory_move_are_rejected','same_path_with_replaced_parent_rejects_bound_database_and_marker','legacy_proof_cannot_be_reused_after_same_path_parent_replacement','sole_surviving_hard_link_is_not_silently_adopted','legacy_requires_proof_and_adoption_preserves_live_wal','same_file_and_hard_link_contend_for_one_mutex','pinned_original_and_transient_alias_cannot_form_a_second_product_domain','fresh_database_competitors_have_one_owner','directory_reparse_is_rejected','connections_keep_the_domain_pinned_until_the_last_one_drops')
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
