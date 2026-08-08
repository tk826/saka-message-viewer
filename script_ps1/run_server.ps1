chcp 65001 | Out-Null
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$env:PYTHONUTF8 = "1"
$env:PYTHONIOENCODING = "utf-8"
Set-Location (Split-Path $PSScriptRoot -Parent)

# --- ポート番号の決定 -------------------------------------------------
# config-dev.json があればそちらを優先（config.py の挙動に合わせる）。
# 読めない場合は既定の 8000 にフォールバックする。
$port = 8000
$configPath = if (Test-Path '.\config-dev.json') { '.\config-dev.json' } else { '.\config.json' }
try {
    if (Test-Path $configPath) {
        $configJson = Get-Content -Path $configPath -Raw -ErrorAction Stop | ConvertFrom-Json -ErrorAction Stop
        if ($configJson.port) {
            $port = [int]$configJson.port
        }
    }
} catch {
    $port = 8000
}

# --- 二重起動の検知 -----------------------------------------------------
# Get-NetTCPConnection が使えない環境では、チェックできないことを理由に
# 起動できなくなるのを避けるため、チェック自体をスキップして従来どおり起動する。
if (Get-Command Get-NetTCPConnection -ErrorAction SilentlyContinue) {
    try {
        $existing = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue
    } catch {
        $existing = $null
    }
    if ($existing) {
        Write-Output "========================================"
        Write-Output "ポート $port は既に使用されています。"
        Write-Output "サーバーが既に起動している可能性があります。"
        Write-Output ""
        Write-Output "  ・ブラウザで http://localhost:$port/ を開いてください"
        Write-Output "  ・起動し直したい場合は、既に開いている"
        Write-Output "    run_server の黒いウィンドウを閉じるか、"
        Write-Output "    そのウィンドウで Ctrl+C を押してください"
        Write-Output "========================================"
        exit 1
    }
}

# --- ログ出力の準備 -------------------------------------------------
# Add-Content/Tee-Object の -Encoding UTF8 は Windows PowerShell 5.1 では
# BOM付きUTF-8固定になり、かつ Tee-Object 経由だと2バイト単位の空白が
# 混入する文字化けが起きるため、.NET の StreamWriter (BOMなしUTF-8) に
# 直接書き込む。ログファイルを排他的に開き続けるため、ロック中は使えない。
# 事前に書き込みテストを行い、失敗したらログ無しモードで起動する。
$logPath = '.\data\server_log.txt'
$separator = "========== $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') 起動 =========="
$logAvailable = $true
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
try {
    $writer = New-Object System.IO.StreamWriter($logPath, $true, $utf8NoBom)
    $writer.WriteLine($separator)
} catch {
    $logAvailable = $false
    Write-Output "警告: ログファイル ($logPath) に書き込めなかったため、ログ無しで起動します。"
}

if ($logAvailable) {
    try {
        & '.\venv_win\Scripts\python.exe' app.py 2>&1 | ForEach-Object {
            $line = "$_"
            Write-Output $line
            $writer.WriteLine($line)
            $writer.Flush()
        }
    } finally {
        $writer.Close()
    }
} else {
    & '.\venv_win\Scripts\python.exe' app.py 2>&1 | ForEach-Object { "$_" }
}
