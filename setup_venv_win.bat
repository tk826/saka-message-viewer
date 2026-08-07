@echo off
chcp 65001 >nul
setlocal

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script_ps1\setup_venv_win.ps1"

echo.
pause

endlocal
