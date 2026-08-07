@echo off
chcp 65001 >nul
setlocal

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script_ps1\run_thumbs.ps1"

echo.
echo サムネイル作成が終了しました。
pause

endlocal
