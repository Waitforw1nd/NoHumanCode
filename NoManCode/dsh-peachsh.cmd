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
if not defined DSH_RUNTIME_ROOT (
  echo [peachsh] Set DSH_RUNTIME_ROOT to the external DeepSeek Harness installation.
  exit /b 1
)
rem Relative configuration paths are based on this script, independent of the caller.
set "DSH_RUNTIME_ROOT=%DSH_RUNTIME_ROOT:/=\%"
if "%DSH_RUNTIME_ROOT:~0,1%"=="\" if not "%DSH_RUNTIME_ROOT:~0,2%"=="\\" set "DSH_RUNTIME_ROOT=%~d0%DSH_RUNTIME_ROOT%"
if not "%DSH_RUNTIME_ROOT:~1,1%"==":" if not "%DSH_RUNTIME_ROOT:~0,2%"=="\\" set "DSH_RUNTIME_ROOT=%~dp0%DSH_RUNTIME_ROOT%"
for %%I in ("%DSH_RUNTIME_ROOT%") do set "DSH_RUNTIME_ROOT=%%~fI"
set "DSH_BIN=%DSH_RUNTIME_ROOT%\node_modules\@deepseek-ai\dsh\lib\bin.js"
if not exist "%DSH_BIN%" (
  echo [peachsh] DeepSeek Harness runtime missing: "%DSH_BIN%"
  exit /b 1
)
pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0apply-overrides.ps1"
if errorlevel 1 exit /b 1
if not defined DSH_HOME set "DSH_HOME=data"
set "DSH_HOME=%DSH_HOME:/=\%"
if "%DSH_HOME:~0,1%"=="\" if not "%DSH_HOME:~0,2%"=="\\" set "DSH_HOME=%~d0%DSH_HOME%"
if not "%DSH_HOME:~1,1%"==":" if not "%DSH_HOME:~0,2%"=="\\" set "DSH_HOME=%~dp0%DSH_HOME%"
for %%I in ("%DSH_HOME%") do set "DSH_HOME=%%~fI"
"%NODE_EXE%" "%DSH_BIN%" %*
