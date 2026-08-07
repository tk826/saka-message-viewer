chcp 65001 | Out-Null
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$env:PYTHONUTF8 = "1"
$env:PYTHONIOENCODING = "utf-8"
Set-Location (Split-Path $PSScriptRoot -Parent)

# ログ出力の準備。Tee-Object -FilePath はログファイルを排他的に開き続けるため、
# ロック中は使えない。事前に書き込みテストを行い、失敗したらログ無しモードで実行する。
$logPath = '.\data\rebuild_log.txt'
$separator = "========== $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') 起動 =========="
$logAvailable = $true
try {
    Add-Content -Path $logPath -Value $separator -Encoding UTF8 -ErrorAction Stop
} catch {
    $logAvailable = $false
    Write-Output "警告: ログファイル ($logPath) に書き込めなかったため、ログ無しで実行します。"
}

if ($logAvailable) {
    & '.\venv_win\Scripts\python.exe' indexer.py --rebuild 2>&1 | ForEach-Object { "$_" } | Tee-Object -FilePath $logPath -Append
    Write-Output "EXITCODE:$LASTEXITCODE" | Tee-Object -FilePath $logPath -Append
} else {
    & '.\venv_win\Scripts\python.exe' indexer.py --rebuild 2>&1 | ForEach-Object { "$_" }
    Write-Output "EXITCODE:$LASTEXITCODE"
}
