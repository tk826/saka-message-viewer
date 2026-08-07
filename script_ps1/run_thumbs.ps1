chcp 65001 | Out-Null
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$env:PYTHONUTF8 = "1"
$env:PYTHONIOENCODING = "utf-8"
Set-Location (Split-Path $PSScriptRoot -Parent)

# ffmpeg探索。indexer.py の find_ffmpeg() と同じ探索順(bin\ffmpeg.exe → ffmpeg.exe → PATH)で
# 事前チェックし、無い場合は日本語で案内する(動画サムネが作られないまま無言で終わると
# 分かりにくいため)。
$ffmpegPath = $null
foreach ($candidate in @('.\bin\ffmpeg.exe', '.\bin\ffmpeg', '.\ffmpeg.exe', '.\ffmpeg')) {
    if (Test-Path -Path $candidate -PathType Leaf) {
        $ffmpegPath = $candidate
        break
    }
}
if (-not $ffmpegPath) {
    $cmd = Get-Command ffmpeg -ErrorAction SilentlyContinue
    if ($cmd) {
        $ffmpegPath = $cmd.Source
    }
}

if ($ffmpegPath) {
    Write-Output "ffmpeg を検出しました: $ffmpegPath"
} else {
    Write-Output "========================================"
    Write-Output "ffmpeg が見つかりませんでした。"
    Write-Output "動画のサムネイルは作成されません。"
    Write-Output "（画像のサムネイルは ffmpeg なしで作成されます）"
    Write-Output ""
    Write-Output "動画サムネイルも作りたい場合:"
    Write-Output "  1. https://www.gyan.dev/ffmpeg/builds/ から"
    Write-Output "     essentials ビルドをダウンロード"
    Write-Output "  2. zip内の bin\ffmpeg.exe を取り出して"
    Write-Output "     このフォルダか bin\ フォルダに置く"
    Write-Output "  3. このバッチをもう一度実行"
    Write-Output "========================================"

    $choice = $Host.UI.PromptForChoice(
        '確認',
        'このまま画像サムネイルのみ作成を続行しますか？',
        @(
            (New-Object System.Management.Automation.Host.ChoiceDescription '&Y', '続行する'),
            (New-Object System.Management.Automation.Host.ChoiceDescription '&N', '中止する')
        ),
        0
    )
    if ($choice -ne 0) {
        Write-Output "中止しました。"
        exit 0
    }
}

# ログ出力の準備。Tee-Object -FilePath はログファイルを排他的に開き続けるため、
# ロック中は使えない。事前に書き込みテストを行い、失敗したらログ無しモードで実行する。
$logPath = '.\data\thumbs_log.txt'
$separator = "========== $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') 起動 =========="
$logAvailable = $true
try {
    Add-Content -Path $logPath -Value $separator -Encoding UTF8 -ErrorAction Stop
} catch {
    $logAvailable = $false
    Write-Output "警告: ログファイル ($logPath) に書き込めなかったため、ログ無しで実行します。"
}

if ($logAvailable) {
    & '.\venv_win\Scripts\python.exe' indexer.py --full 2>&1 | ForEach-Object { "$_" } | Tee-Object -FilePath $logPath -Append
    Write-Output "EXITCODE:$LASTEXITCODE" | Tee-Object -FilePath $logPath -Append
} else {
    & '.\venv_win\Scripts\python.exe' indexer.py --full 2>&1 | ForEach-Object { "$_" }
    Write-Output "EXITCODE:$LASTEXITCODE"
}
