@echo off
chcp 65001 >nul
setlocal

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script_ps1\run_rebuild.ps1"

echo.
echo 処理が終了しました。
pause

endlocal
