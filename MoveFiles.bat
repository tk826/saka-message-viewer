@echo off
chcp 65001 >nul
setlocal

set "TARGET_DIR=%~1"

if "%TARGET_DIR%"=="" (
    set /p "TARGET_DIR=対象ディレクトリのパスを入力してください: "
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script_ps1\MergeDatedFolders.ps1" -TargetDir "%TARGET_DIR%" -NoPause
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script_ps1\MoveFiles.ps1" -TargetDir "%TARGET_DIR%"

endlocal
