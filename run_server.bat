@echo off
chcp 65001 >nul
setlocal

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script_ps1\run_server.ps1"

echo.
echo サーバーが終了しました。
pause

endlocal
