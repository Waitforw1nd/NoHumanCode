@echo off
setlocal
set "NODE_EXE=C:\Users\Administrator\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe"
set "DSH_BIN=D:\DeepSeekHarness\node_modules\@deepseek-ai\dsh\lib\bin.js"
if not exist "%NODE_EXE%" (
  echo [peachsh] Node runtime missing: %NODE_EXE%
  exit /b 1
)
if not exist "%DSH_BIN%" (
  echo [peachsh] DeepSeek Harness runtime missing: %DSH_BIN%
  exit /b 1
)
pwsh -NoProfile -ExecutionPolicy Bypass -File "D:\peachsh-harness\apply-overrides.ps1"
if errorlevel 1 exit /b 1
set "DSH_HOME=D:\peachsh-harness\data"
"%NODE_EXE%" "%DSH_BIN%" %*
