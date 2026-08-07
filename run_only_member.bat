@echo off
chcp 65001 >nul
setlocal

set "MEMBER=%~1"
if "%MEMBER%"=="" (
    set /p "MEMBER=対象メンバー名を入力してください: "
)
if "%MEMBER%"=="" (
    echo メンバー名が入力されませんでした。
    pause
    goto :eof
)

set "FULL_OPT="
choice /C YN /N /M "--full を付けて実行しますか？ (Y/N): "
if errorlevel 2 set "FULL_OPT="
if errorlevel 1 if not errorlevel 2 set "FULL_OPT=-Full"

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script_ps1\run_only_member.ps1" -Member "%MEMBER%" %FULL_OPT%

echo.
echo 処理が終了しました。
pause

endlocal
