@echo off
chcp 65001 >nul
setlocal

set "TARGET_DIR=%~1"

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script_ps1\MoveFiles.ps1" -TargetDir "%TARGET_DIR%"

endlocal
