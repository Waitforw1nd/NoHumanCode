$ErrorActionPreference = 'Stop'

$sourceRoot = 'D:\peachsh-harness\overrides'
$runtimeRoot = 'D:\DeepSeekHarness\node_modules'
$webRoot = 'D:\DeepSeekHarness\data\profiles\web\node_modules'

$files = @(
  @("$sourceRoot\root\@deepseek-ai\dsh-experimental-agent-team\lib\index.js", "$runtimeRoot\@deepseek-ai\dsh-experimental-agent-team\lib\index.js"),
  @("$sourceRoot\root\@deepseek-ai\dsh-experimental-agent-team\lib\types\types.d.ts", "$runtimeRoot\@deepseek-ai\dsh-experimental-agent-team\lib\types\types.d.ts"),
  @("$sourceRoot\root\@deepseek-ai\dsh-experimental-agent-team\lib\typert.host.js", "$runtimeRoot\@deepseek-ai\dsh-experimental-agent-team\lib\typert.host.js"),
  @("$sourceRoot\root\@deepseek-ai\dsh-experimental-agent-team\lib\typert.remote-client.js", "$runtimeRoot\@deepseek-ai\dsh-experimental-agent-team\lib\typert.remote-client.js"),
  @("$sourceRoot\root\@deepseek-ai\dsh-experimental-tool-agent-team\lib\index.js", "$runtimeRoot\@deepseek-ai\dsh-experimental-tool-agent-team\lib\index.js"),
  @("$sourceRoot\web\@deepseek-ai\dsh-experimental-agent-team\lib\index.js", "$webRoot\@deepseek-ai\dsh-experimental-agent-team\lib\index.js"),
  @("$sourceRoot\web\@deepseek-ai\dsh-experimental-agent-team\lib\types\types.d.ts", "$webRoot\@deepseek-ai\dsh-experimental-agent-team\lib\types\types.d.ts"),
  @("$sourceRoot\web\@deepseek-ai\dsh-experimental-agent-team\lib\typert.host.js", "$webRoot\@deepseek-ai\dsh-experimental-agent-team\lib\typert.host.js"),
  @("$sourceRoot\web\@deepseek-ai\dsh-experimental-agent-team\lib\typert.remote-client.js", "$webRoot\@deepseek-ai\dsh-experimental-agent-team\lib\typert.remote-client.js"),
  @("$sourceRoot\root\@deepseek-ai\dsh-experimental-client-ui-agent-team\lib\client.js", "$runtimeRoot\@deepseek-ai\dsh-experimental-client-ui-agent-team\lib\client.js"),
  @("$sourceRoot\web\@deepseek-ai\dsh-experimental-tool-agent-team\lib\index.js", "$webRoot\@deepseek-ai\dsh-experimental-tool-agent-team\lib\index.js")
)

foreach ($pair in $files) {
  if (-not (Test-Path -LiteralPath $pair[0])) { throw "Missing override: $($pair[0])" }
  if (-not (Test-Path -LiteralPath $pair[1])) { throw "Missing runtime target: $($pair[1])" }
  Copy-Item -LiteralPath $pair[0] -Destination $pair[1] -Force
}

Write-Output '🍑sh harness overrides applied.'
