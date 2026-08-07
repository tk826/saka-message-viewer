param(
    [string]$TargetDir
)

try {
    [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
} catch {}

if ([string]::IsNullOrWhiteSpace($TargetDir)) {
    $TargetDir = Read-Host "対象ディレクトリのパスを入力してください"
}

if (-not (Test-Path -LiteralPath $TargetDir)) {
    Write-Host "指定されたディレクトリが見つかりません: $TargetDir" -ForegroundColor Red
    Read-Host "Enterキーで終了します"
    exit 1
}

Write-Host "対象ディレクトリ: $TargetDir"
Write-Host "移動処理を開始します..."
Write-Host ""

# 対象パターン: 数値5～6桁_数値1桁_YYYYMMDDHHMMSS.jpg/mp4/txt
# 例: (対象ディレクトリ配下の)サブフォルダ\12345_1_20240115123045.jpg
#     -> 同じサブフォルダ\2024\202401\12345_1_20240115123045.jpg
# (ファイルが今存在しているフォルダを基準にYYYY\YYYYMMを作成する)
$pattern = '^(\d{5,6})_(\d)_(\d{14})\.(jpg|mp4|txt)$'

# 移動先(TargetDir配下のYYYY\YYYYMMフォルダ)自体は検索対象から除外するため、
# 事前に対象ファイル一覧を取得してから処理する
$targetFiles = Get-ChildItem -LiteralPath $TargetDir -File -Recurse |
    Where-Object { $_.Name -match $pattern }

if ($targetFiles.Count -eq 0) {
    Write-Host "該当するファイルは見つかりませんでした。" -ForegroundColor Yellow
    Read-Host "Enterキーで終了します"
    exit 0
}

$successCount = 0
$skipCount = 0

foreach ($file in $targetFiles) {
    if ($file.Name -match $pattern) {
        $datetime = $matches[3]  # YYYYMMDDHHMMSS

        $yyyy = $datetime.Substring(0, 4)
        $yyyymm = $datetime.Substring(0, 6)

        # 既に「...\YYYY\YYYYMM\」の下にあるファイルは移動済みとみなしてスキップ
        # (再実行時にYYYY\YYYYMM\YYYY\YYYYMM...と二重に入れ子になるのを防ぐ)
        $parentDir = $file.Directory
        $grandParentDir = $parentDir.Parent
        if ($parentDir.Name -eq $yyyymm -and $grandParentDir -and $grandParentDir.Name -eq $yyyy) {
            continue
        }

        $destDir = Join-Path $file.DirectoryName (Join-Path $yyyy $yyyymm)
        $destPath = Join-Path $destDir $file.Name

        if (-not (Test-Path -LiteralPath $destDir)) {
            New-Item -ItemType Directory -Path $destDir -Force | Out-Null
        }

        if (Test-Path -LiteralPath $destPath) {
            Write-Host "[スキップ] 移動先に同名ファイルが既に存在します: $($file.Name)" -ForegroundColor Yellow
            $skipCount++
        } else {
            Move-Item -LiteralPath $file.FullName -Destination $destPath
            Write-Host "[移動] $($file.Name) -> $yyyy\$yyyymm\" -ForegroundColor Green
            $successCount++
        }
    }
}

Write-Host ""
Write-Host "処理が完了しました。 移動: $successCount 件 / スキップ: $skipCount 件"
Read-Host "Enterキーで終了します"
