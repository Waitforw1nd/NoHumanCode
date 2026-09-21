@echo off
setlocal
set "PEACHSH_EXE=%~dp0bin\peachsh.exe"
if not exist "%PEACHSH_EXE%" (
  echo [peachsh] Native executable missing: %PEACHSH_EXE%
  echo Build it with: pwsh -File "%~dp0rust-app\build.ps1" -Action build
  echo Legacy version: "%~dp0start-peachsh-legacy.cmd"
  exit /b 1
)
"%PEACHSH_EXE%" --data-dir "%~dp0data-rust" --legacy-data "%~dp0data" --workspace "%~dp0workspace" %*
