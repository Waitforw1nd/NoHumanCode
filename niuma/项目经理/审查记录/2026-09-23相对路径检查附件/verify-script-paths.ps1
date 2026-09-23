param(
    [string]$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../../..')).Path,
    [switch]$CheckEntry
)
$ErrorActionPreference = 'Stop'
$taskRepositoryRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
foreach ($taskRequired in @('NoManCode/apply-overrides.ps1','NoManCode/dsh-peachsh.cmd','NoManCode/start-peachsh-legacy.cmd','NoManCode/newapi.cmd','NoManCode/set-peachsh-key.ps1','NoManCode/tests/test-skill-discovery.mjs','NoManCode/overrides')) {
    if (-not (Test-Path -LiteralPath (Join-Path $taskRepositoryRoot $taskRequired))) {
        throw "Missing repository entry: $taskRequired"
    }
}
if ($CheckEntry) {
    Write-Output 'Entry OK: NoManCode scripts, tests/test-skill-discovery.mjs and overrides'
    return
}
$taskTempRoot = Join-Path ([IO.Path]::GetTempPath()) ('nhc portable (mock) ' + [guid]::NewGuid().ToString('N'))
$taskSourceRoot = Join-Path $taskTempRoot 'source root'
$taskRuntimeRoot = Join-Path $taskTempRoot 'external runtime'
$taskCallerRoot = Join-Path $taskTempRoot 'unrelated caller'
[void](New-Item -ItemType Directory -Path $taskSourceRoot,$taskRuntimeRoot,$taskCallerRoot)
foreach ($taskFile in @('apply-overrides.ps1','dsh-peachsh.cmd','start-peachsh-legacy.cmd','newapi.cmd','set-peachsh-key.ps1','tests/test-skill-discovery.mjs')) {
    $taskCopyPath = Join-Path $taskSourceRoot $taskFile
    [void](New-Item -ItemType Directory -Path (Split-Path -Parent $taskCopyPath) -Force)
    Copy-Item -LiteralPath (Join-Path $taskRepositoryRoot ('NoManCode/' + $taskFile)) -Destination $taskCopyPath
}
$taskOriginalOverrides = Join-Path $taskRepositoryRoot 'NoManCode/overrides'
foreach ($taskFile in (Get-ChildItem -LiteralPath $taskOriginalOverrides -File -Recurse)) {
    $taskRelative = [IO.Path]::GetRelativePath($taskOriginalOverrides,$taskFile.FullName)
    $taskSource = Join-Path (Join-Path $taskSourceRoot 'overrides') $taskRelative
    $taskTarget = if ($taskRelative.StartsWith('root\')) { Join-Path (Join-Path $taskRuntimeRoot 'node_modules') $taskRelative.Substring(5) } else { Join-Path (Join-Path $taskRuntimeRoot 'data/profiles/web/node_modules') $taskRelative.Substring(4) }
    [void](New-Item -ItemType Directory -Path (Split-Path -Parent $taskSource),(Split-Path -Parent $taskTarget) -Force)
    Set-Content -LiteralPath $taskSource -Value ('mock override ' + $taskRelative)
    Set-Content -LiteralPath $taskTarget -Value 'before override'
}
$taskFakeDsh = Join-Path $taskRuntimeRoot 'node_modules/@deepseek-ai/dsh/lib/bin.js'
[void](New-Item -ItemType Directory -Path (Split-Path -Parent $taskFakeDsh) -Force)
Set-Content -LiteralPath $taskFakeDsh -Value 'console.log("MOCK_JSON:" + JSON.stringify({cwd:process.cwd(), home:process.env.DSH_HOME, runtime:process.env.DSH_RUNTIME_ROOT, args:process.argv.slice(2)}));'
$taskFakeSkillDir = Join-Path $taskSourceRoot 'custom data/skills/newapi/scripts'
[void](New-Item -ItemType Directory -Path $taskFakeSkillDir -Force)
Set-Content -LiteralPath (Join-Path $taskFakeSkillDir 'query-newapi-balance.mjs') -Value 'console.log("MOCK_JSON:" + JSON.stringify({home:process.env.DSH_HOME})); process.exit(7);'
Set-Content -LiteralPath (Join-Path $taskFakeSkillDir 'bind-newapi-account.ps1') -Value 'Write-Output "MOCK_BIND:$env:DSH_HOME"; exit 7'
$taskProviderDir = Join-Path $taskSourceRoot 'node_modules/@deepseek-ai/dsh-skill-filesystem/lib'
[void](New-Item -ItemType Directory -Path $taskProviderDir -Force)
Set-Content -LiteralPath (Join-Path (Split-Path -Parent $taskProviderDir) 'package.json') -Value '{"type":"module"}'
Set-Content -LiteralPath (Join-Path $taskProviderDir 'index.js') -Value 'export class FileSystemSkillProvider { constructor(a,b,options) { this.options=options; } async list(root) { console.log("MOCK_JSON:"+JSON.stringify({root,...this.options})); return [{name:"newapi"},{name:"swarm"}]; } async get(item) { return {...item, invocation:{userInvocable:true}}; } async dispose() {} }'
$taskNode = (Get-Command node.exe).Source
$taskPwsh = (Get-Command pwsh.exe).Source
function Run-TaskProcess([string]$Program,[string[]]$Arguments,[hashtable]$Environment=@{}) {
    $taskStart = [Diagnostics.ProcessStartInfo]::new()
    $taskStart.FileName = $Program
    if ($Program -eq $env:ComSpec) { $taskStart.Arguments = '/d /s /c ' + $Arguments[3] } else { foreach ($taskArgument in $Arguments) { $taskStart.ArgumentList.Add($taskArgument) } }
    $taskStart.WorkingDirectory = $taskCallerRoot
    $taskStart.UseShellExecute = $false
    $taskStart.CreateNoWindow = $true
    $taskStart.RedirectStandardOutput = $true
    $taskStart.RedirectStandardError = $true
    foreach ($taskName in @('DSH_RUNTIME_ROOT','DSH_HOME','NODE_EXE')) { [void]$taskStart.Environment.Remove($taskName) }
    foreach ($taskName in $Environment.Keys) { $taskStart.Environment[$taskName] = $Environment[$taskName] }
    $taskProcess = [Diagnostics.Process]::Start($taskStart)
    $taskOut = $taskProcess.StandardOutput.ReadToEnd()
    $taskErr = $taskProcess.StandardError.ReadToEnd()
    $taskProcess.WaitForExit()
    [pscustomobject]@{Code=$taskProcess.ExitCode;Out=$taskOut;Err=$taskErr}
}
function Run-TaskCmd([string]$Name,[string]$Tail='', [hashtable]$Environment=@{}) {
    $taskCommand = '""' + (Join-Path $taskSourceRoot $Name) + '" ' + $Tail + '"'
    Run-TaskProcess $env:ComSpec @('/d','/s','/c',$taskCommand) $Environment
}
function Assert-Task([bool]$Condition,[string]$Label,$Result) {
    if (-not $Condition) { throw ($Label + ': ' + ($Result | ConvertTo-Json -Compress -Depth 4)) }
    Write-Output ('PASS ' + $Label)
}
function Get-TaskJson($Result) {
    $taskLine = @($Result.Out -split '\r?\n' | Where-Object { $_.StartsWith('MOCK_JSON:') }) | Select-Object -Last 1
    if (-not $taskLine) { throw ('Missing mock output: ' + ($Result | ConvertTo-Json -Compress)) }
    $taskLine.Substring(10) | ConvertFrom-Json
}
$taskResult = Run-TaskProcess $taskNode @('--check',(Join-Path $taskSourceRoot 'tests/test-skill-discovery.mjs'))
Assert-Task ($taskResult.Code -eq 0) 'node syntax' $taskResult
$taskResult = Run-TaskCmd 'dsh-peachsh.cmd'
Assert-Task ($taskResult.Code -eq 1 -and $taskResult.Out.Contains('Set DSH_RUNTIME_ROOT')) 'required runtime env' $taskResult
$taskResult = Run-TaskCmd 'dsh-peachsh.cmd' '' @{DSH_RUNTIME_ROOT='../external runtime';NODE_EXE='missing-nhc-node.exe'}
Assert-Task ($taskResult.Code -eq 1 -and $taskResult.Out.Contains('Node runtime missing')) 'missing Node clear error' $taskResult
$taskResult = Run-TaskCmd 'dsh-peachsh.cmd' '--mock-arg' @{DSH_RUNTIME_ROOT='../external runtime'}
$taskJson = Get-TaskJson $taskResult
Assert-Task ($taskResult.Code -eq 0 -and $taskJson.runtime -eq $taskRuntimeRoot -and $taskJson.home -eq (Join-Path $taskSourceRoot 'data') -and $taskJson.cwd -eq $taskCallerRoot -and $taskJson.args[0] -eq '--mock-arg') 'relative runtime, default data, Node PATH and caller cwd' $taskResult
$taskResult = Run-TaskCmd 'start-peachsh-legacy.cmd' '--mock-arg' @{DSH_RUNTIME_ROOT='../external runtime';DSH_HOME='custom data';NODE_EXE=$taskNode}
$taskJson = Get-TaskJson $taskResult
Assert-Task ($taskResult.Code -eq 0 -and $taskJson.home -eq (Join-Path $taskSourceRoot 'custom data') -and $taskJson.cwd -eq $taskSourceRoot -and ($taskJson.args -join '|') -eq 'web|--no-open|--mock-arg') 'legacy relative data, explicit Node and script cwd' $taskResult
$taskResult = Run-TaskCmd 'dsh-peachsh.cmd' '' @{DSH_RUNTIME_ROOT=$taskRuntimeRoot;DSH_HOME=(Join-Path $taskSourceRoot 'absolute data')}
$taskJson = Get-TaskJson $taskResult
Assert-Task ($taskResult.Code -eq 0 -and $taskJson.runtime -eq $taskRuntimeRoot -and $taskJson.home -eq (Join-Path $taskSourceRoot 'absolute data')) 'absolute runtime/data config' $taskResult
$taskResult = Run-TaskCmd 'newapi.cmd' 'balance' @{DSH_HOME='custom data'}
$taskJson = Get-TaskJson $taskResult
Assert-Task ($taskResult.Code -eq 7 -and $taskJson.home -eq (Join-Path $taskSourceRoot 'custom data')) 'newapi data base and balance exit propagation' $taskResult
$taskResult = Run-TaskCmd 'newapi.cmd' 'bind' @{DSH_HOME='custom data'}
Assert-Task ($taskResult.Code -eq 7 -and $taskResult.Out.Contains('MOCK_BIND:' + (Join-Path $taskSourceRoot 'custom data'))) 'newapi mock bind exit propagation' $taskResult
$taskResult = Run-TaskCmd 'newapi.cmd' 'balance'
Assert-Task ($taskResult.Code -eq 1 -and $taskResult.Out.Contains('New API balance script missing:')) 'missing skill clear error' $taskResult
$taskResult = Run-TaskProcess $taskPwsh @('-NoProfile','-File',(Join-Path $taskSourceRoot 'set-peachsh-key.ps1'),'-Slot','1') @{DSH_HOME='custom data'}
Assert-Task ($taskResult.Code -ne 0 -and $taskResult.Err.Contains('source root\custom data\.credentials.yaml')) 'key script relative data missing-store guard, no credential read' $taskResult
$taskResult = Run-TaskProcess $taskPwsh @('-NoProfile','-File',(Join-Path $taskSourceRoot 'set-peachsh-key.ps1'),'-Slot','1')
Assert-Task ($taskResult.Code -ne 0 -and $taskResult.Err.Contains('source root\data\.credentials.yaml')) 'key script default data missing-store guard, no credential read' $taskResult
$taskResult = Run-TaskProcess $taskPwsh @('-NoProfile','-File',(Join-Path $taskSourceRoot 'apply-overrides.ps1'))
Assert-Task ($taskResult.Code -ne 0 -and $taskResult.Err.Contains('Set DSH_RUNTIME_ROOT')) 'PowerShell required runtime env' $taskResult
$taskResult = Run-TaskProcess $taskPwsh @('-NoProfile','-File',(Join-Path $taskSourceRoot 'apply-overrides.ps1')) @{DSH_RUNTIME_ROOT='../external runtime'}
Assert-Task ($taskResult.Code -eq 0) 'PowerShell relative runtime' $taskResult
$taskResult = Run-TaskProcess $taskNode @((Join-Path $taskSourceRoot 'tests/test-skill-discovery.mjs')) @{DSH_HOME='custom data'}
$taskJson = Get-TaskJson $taskResult
Assert-Task ($taskResult.Code -eq 0 -and $taskJson.dshHome -eq (Join-Path $taskSourceRoot 'custom data') -and $taskJson.agentsHome -eq (Join-Path $taskSourceRoot 'custom data/agents') -and $taskJson.root.TrimEnd('\') -eq $taskSourceRoot) 'discovery module root, relative data and agents' $taskResult
$taskResult = Run-TaskProcess $taskNode @((Join-Path $taskSourceRoot 'tests/test-skill-discovery.mjs'))
$taskJson = Get-TaskJson $taskResult
Assert-Task ($taskResult.Code -eq 0 -and $taskJson.dshHome -eq (Join-Path $taskSourceRoot 'data')) 'discovery default data' $taskResult
Write-Output ('Mock fixtures: ' + $taskTempRoot)



