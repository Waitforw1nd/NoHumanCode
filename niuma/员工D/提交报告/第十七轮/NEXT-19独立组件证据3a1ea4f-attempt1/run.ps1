$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$work = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Set-Location -LiteralPath $work
$repo = (& git rev-parse --show-toplevel).Trim()
$env:CARGO_TARGET_DIR = (Resolve-Path (Join-Path $PSScriptRoot 'target')).Path
$env:TEMP = (Resolve-Path (Join-Path $PSScriptRoot 'tmp')).Path
$env:TMP = $env:TEMP
$env:CARGO_NET_OFFLINE = 'true'
$env:RUSTUP_AUTO_INSTALL = '0'
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_BUILD_JOBS = '2'
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:VCToolsInstallDir = '[msvc]'
$env:WindowsSdkDir = '[windows-sdk]\'
$setup = Get-Content -Raw -LiteralPath (Join-Path $work 'build.ps1')
$setup = $setup.Substring(0, $setup.IndexOf('switch ($Action)'))
$setup = $setup -replace "param\(\[ValidateSet\([^\r\n]*\r?\n", ''
$setup = $setup -replace 'Set-Location -LiteralPath \$PSScriptRoot', ''
Invoke-Expression $setup
function Save-SourceManifest($name) {
    $paths = @(& git -C $repo ls-files -- NoManCode/rust-app)
    $entries = foreach ($path in $paths) {
        $full = Join-Path $repo $path
        [pscustomobject]@{path=$path; bytes=(Get-Item -LiteralPath $full).Length; sha256=(Get-FileHash -LiteralPath $full -Algorithm SHA256).Hash}
    }
    $entries | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$name.json")
}
function State($name) {
    @{at_utc=[DateTime]::UtcNow.ToString('o');head=(& git rev-parse HEAD);product_tree=(& git rev-parse 'HEAD:NoManCode');rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app');branch=(& git branch --show-current);status=@(& git status --short);staged=@(& git diff --cached --name-status);unmerged=@(& git ls-files --unmerged)} | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$name.json")
}
$head = (& git rev-parse HEAD).Trim()
$product = (& git rev-parse 'HEAD:NoManCode').Trim()
$rust = (& git rev-parse 'HEAD:NoManCode/rust-app').Trim()
$branch = (& git branch --show-current).Trim()
if ($head -ne '3a1ea4f82b21413b98e5f8f0236e04e2b72067ef' -or $product -ne 'fd662d0a0202612a69f6242fcdf0aa8750f2c525' -or $rust -ne '7c71e6614d6cffa178b6565fde4617a48b21a02b' -or $branch -ne 'codex/d/tool-result-tests' -or @(& git status --short).Count -or @(& git diff --cached --name-only).Count -or @(& git ls-files --unmerged).Count) { throw 'identity or cleanliness mismatch' }
$heavy = @(Get-Process | Where-Object {$_.ProcessName -match '^(cargo|rustc|clippy-driver|peachsh|link|lld-link|msbuild|ninja|cmake)(-|\.exe|$)'})
if ($heavy.Count) { throw 'heavy process present' }
State 'preflight'
@{cargo=(& cargo --version);rustc=(& rustc --version);target=$env:CARGO_TARGET_DIR;tmp=$env:TEMP;msvc=$taskMsvc;sdk=$taskSdk.FullName;jobs=2;incremental=0;dev_debug=0;test_debug=0;offline=$true;locked=$true;legacy_target_directory=(Test-Path -LiteralPath (Join-Path $work 'target') -PathType Container)} | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/environment.json')
Save-SourceManifest 'source-before'
& python (Join-Path $PSScriptRoot 'inventory.py') 'retained-before' | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/retained-before-summary.txt')
$commands = @(
  @{name='01-check';args=@('check','--offline','--locked','--all-targets')},
  @{name='02-build-cli';args=@('build','--offline','--locked','--bin','peachsh')},
  @{name='03-lib';args=@('test','--offline','--locked','--lib','--','--nocapture')},
  @{name='04-integrations';args=@('test','--offline','--locked','--test','approval_gate','--test','approval_http','--test','tool_session_turns','--test','tool_result_crash','--test','workspace_changes','--test','workspace_crash','--test','checkpoint','--test','checkpoint_http','--test','network_authorization','--test','network_http','--test','plugin_management','--test','plugin_http','--test','runtime_management_workflow','--test','tool_runtime','--','--nocapture')},
  @{name='05-clippy';args=@('clippy','--offline','--locked','--all-targets','--','-D','warnings')},
  @{name='06-fmt';args=@('fmt','--all','--','--check')}
)
$failure = 0
try {
    foreach ($command in $commands) {
        $name=$command.name
        $started=[DateTime]::UtcNow.ToString('o')
        Write-Output "BEGIN $name $started"
        & cargo @($command.args) 1> (Join-Path $PSScriptRoot "logs/$name.stdout.log") 2> (Join-Path $PSScriptRoot "logs/$name.stderr.log")
        $code=$LASTEXITCODE
        @{command=('cargo '+($command.args -join ' '));started_utc=$started;ended_utc=[DateTime]::UtcNow.ToString('o');exit_code=$code;head=(& git rev-parse HEAD);product_tree=(& git rev-parse 'HEAD:NoManCode');rust_tree=(& git rev-parse 'HEAD:NoManCode/rust-app')} | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot "logs/$name.json")
        Save-SourceManifest "source-after-$name"
        State "state-after-$name"
        Write-Output "END $name exit=$code"
        if ($code -ne 0) { $failure=$code; break }
        if ($name -eq '02-build-cli') {
            $binary=Join-Path $env:CARGO_TARGET_DIR 'debug/peachsh.exe'
            if (!(Test-Path -LiteralPath $binary -PathType Leaf)) { $failure=1; Write-Output 'CLI binary missing'; break }
            @{path=$binary;bytes=(Get-Item -LiteralPath $binary).Length;sha256=(Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash;checked_utc=[DateTime]::UtcNow.ToString('o')} | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/cli-binary.json')
        }
    }
} finally {
    Save-SourceManifest 'source-after'
    State 'postflight'
    & python (Join-Path $PSScriptRoot 'inventory.py') 'retained-after' | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/retained-after-summary.txt')
    $heavy = @(Get-Process | Where-Object {$_.ProcessName -match '^(cargo|rustc|clippy-driver|peachsh|link|lld-link|msbuild|ninja|cmake)(-|\.exe|$)'} | Select-Object Id,ProcessName,Path)
    @{finished_utc=[DateTime]::UtcNow.ToString('o');exit_code=$failure;heavy=$heavy} | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'logs/final.json')
}
exit $failure
