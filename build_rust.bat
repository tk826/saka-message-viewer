@echo off
chcp 65001 >nul
setlocal

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script_ps1\build_rust.ps1"

echo.
pause

endlocal
