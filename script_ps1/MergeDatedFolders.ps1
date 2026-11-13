param(
    [string]$TargetDir,
    # MoveFiles.bat から連続実行する場合など、処理完了ごとの一時停止を
    # 省略したいときに指定する(呼び出し側が最後にまとめて一時停止する想定)。
    [switch]$NoPause
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

$TargetDir = (Resolve-Path -LiteralPath $TargetDir).Path

Write-Host "対象ディレクトリ: $TargetDir"
Write-Host "統合処理を開始します..."
Write-Host ""

# 対象パターン: {メンバー名}_{YYYYMMDDHHMMSS} という名前のディレクトリ
# 例: 佐藤優羽_20260812165421 -> 佐藤優羽 に統合する
$pattern = '^(.+)_(\d{14})$'

$datedDirs = Get-ChildItem -LiteralPath $TargetDir -Directory -Recurse |
    Where-Object { $_.Name -match $pattern }

if ($datedDirs.Count -eq 0) {
    Write-Host "日付付きフォルダは見つかりませんでした。" -ForegroundColor Yellow
    if (-not $NoPause) { Read-Host "Enterキーで終了します" }
    exit 0
}

$mergedCount = 0
$renamedCount = 0
$skipFileCount = 0
$dupRemovedCount = 0
$leftoverCount = 0

foreach ($datedDir in $datedDirs) {
    if (-not (Test-Path -LiteralPath $datedDir.FullName)) {
        # 親の統合処理で入れ子ごと移動済みのケース
        continue
    }
    if ($datedDir.Name -notmatch $pattern) { continue }
    $memberName = $matches[1]

    $parentDir = $datedDir.Parent.FullName
    $destDir = Join-Path $parentDir $memberName
    $relDatedDir = $datedDir.FullName.Substring($TargetDir.Length).TrimStart('\')

    if (-not (Test-Path -LiteralPath $destDir)) {
        # 統合先が無ければフォルダ名から日付を外すだけ
        Rename-Item -LiteralPath $datedDir.FullName -NewName $memberName
        Write-Host "[リネーム] $relDatedDir -> $memberName" -ForegroundColor Green
        $renamedCount++
        continue
    }

    # 統合先が既にある場合: 中身(ファイル・サブディレクトリ)を再帰的に移動する。
    # 同名ファイルが既に存在する場合、ファイルサイズが一致すれば同一ファイルの
    # 重複とみなし日付付きフォルダ側を削除する。サイズが異なる場合は上書きせず
    # スキップし、手動確認できるよう日付付きフォルダ側に残す。
    $entries = Get-ChildItem -LiteralPath $datedDir.FullName -Recurse -File
    foreach ($entry in $entries) {
        $relPath = $entry.FullName.Substring($datedDir.FullName.Length).TrimStart('\')
        $destPath = Join-Path $destDir $relPath
        $destEntryDir = Split-Path $destPath -Parent

        if (-not (Test-Path -LiteralPath $destEntryDir)) {
            New-Item -ItemType Directory -Path $destEntryDir -Force | Out-Null
        }

        if (Test-Path -LiteralPath $destPath) {
            $destFile = Get-Item -LiteralPath $destPath
            if ($destFile.Length -eq $entry.Length) {
                Remove-Item -LiteralPath $entry.FullName -Force
                Write-Host "[重複削除] 統合先と同サイズのため削除: $relDatedDir\$relPath" -ForegroundColor Cyan
                $dupRemovedCount++
            } else {
                Write-Host "[スキップ] 統合先に異なるサイズの同名ファイルが存在します: $relDatedDir\$relPath" -ForegroundColor Yellow
                $skipFileCount++
            }
        } else {
            Move-Item -LiteralPath $entry.FullName -Destination $destPath
        }
    }

    # 移動後に空になったサブディレクトリ・自身のフォルダを削除する。
    # スキップしたファイルが残っている場合はフォルダごと残す。
    $remaining = Get-ChildItem -LiteralPath $datedDir.FullName -Recurse -File
    if ($remaining.Count -eq 0) {
        Remove-Item -LiteralPath $datedDir.FullName -Recurse -Force
        Write-Host "[統合] $relDatedDir -> $memberName" -ForegroundColor Green
        $mergedCount++
    } else {
        Write-Host "[一部残存] $relDatedDir に $($remaining.Count) 件のファイルが残っています(スキップ分)" -ForegroundColor Yellow
        $leftoverCount++
    }
}

Write-Host ""
Write-Host "処理が完了しました。 統合: $mergedCount 件 / リネーム: $renamedCount 件 / 重複削除: $dupRemovedCount 件 / ファイルスキップ: $skipFileCount 件 / 一部残存: $leftoverCount 件"
if (-not $NoPause) { Read-Host "Enterキーで終了します" }
