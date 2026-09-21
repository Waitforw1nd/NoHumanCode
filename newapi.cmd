@echo off
setlocal
set "NODE_EXE=C:\Users\Administrator\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe"
set "SKILL_ROOT=D:\peachsh-harness\data\skills\newapi"
if not exist "%NODE_EXE%" (
  echo [peachsh] Node runtime missing: %NODE_EXE%
  exit /b 1
)
if /I "%~1"=="bind" (
  pwsh -NoProfile -ExecutionPolicy Bypass -File "%SKILL_ROOT%\scripts\bind-newapi-account.ps1"
  exit /b %errorlevel%
)
if /I "%~1"=="balance" (
  "%NODE_EXE%" "%SKILL_ROOT%\scripts\query-newapi-balance.mjs"
  exit /b %errorlevel%
)
echo Usage: newapi.cmd bind ^| balance
exit /b 2
