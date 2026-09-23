@echo off
setlocal
if not defined NODE_EXE set "NODE_EXE=node"
if exist "%NODE_EXE%" (
  for %%I in ("%NODE_EXE%") do set "NODE_EXE=%%~fI"
  goto node_ready
)
where.exe "%NODE_EXE%" >nul 2>nul
if errorlevel 1 (
  echo [peachsh] Node runtime missing: "%NODE_EXE%"
  exit /b 1
)
:node_ready
if not defined DSH_HOME set "DSH_HOME=data"
set "DSH_HOME=%DSH_HOME:/=\%"
if "%DSH_HOME:~0,1%"=="\" if not "%DSH_HOME:~0,2%"=="\\" set "DSH_HOME=%~d0%DSH_HOME%"
if not "%DSH_HOME:~1,1%"==":" if not "%DSH_HOME:~0,2%"=="\\" set "DSH_HOME=%~dp0%DSH_HOME%"
for %%I in ("%DSH_HOME%") do set "DSH_HOME=%%~fI"
set "SKILL_ROOT=%DSH_HOME%\skills\newapi"
if /I "%~1"=="bind" goto bind
if /I "%~1"=="balance" goto balance
echo Usage: newapi.cmd bind ^| balance
exit /b 2

:bind
if not exist "%SKILL_ROOT%\scripts\bind-newapi-account.ps1" (
  echo [peachsh] New API binding script missing: "%SKILL_ROOT%\scripts\bind-newapi-account.ps1"
  exit /b 1
)
pwsh -NoProfile -ExecutionPolicy Bypass -File "%SKILL_ROOT%\scripts\bind-newapi-account.ps1"
exit /b %errorlevel%

:balance
if not exist "%SKILL_ROOT%\scripts\query-newapi-balance.mjs" (
  echo [peachsh] New API balance script missing: "%SKILL_ROOT%\scripts\query-newapi-balance.mjs"
  exit /b 1
)
"%NODE_EXE%" "%SKILL_ROOT%\scripts\query-newapi-balance.mjs"
exit /b %errorlevel%
