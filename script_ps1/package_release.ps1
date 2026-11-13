chcp 65001 | Out-Null
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Set-Location (Split-Path $PSScriptRoot -Parent)

# 配布用の zip を作る(GitHub Actions のリリースと手元の確認の両方で使う)。
#   dist\saka-message-viewer-<版>-windows-x64.zip
# 版は rust\Cargo.toml の version。-Tag(例: v0.1.0)を渡したときは、版と一致しなければ止める。
# 本番の config-dev.json・data\・ffmpeg.exe は入れない。
#   使い方: powershell -File script_ps1\package_release.ps1 [v0.1.0]
$ErrorActionPreference = 'Stop'

$tag = $args | Select-Object -First 1
$cargoToml = Get-Content '.\rust\Cargo.toml' -Raw -Encoding UTF8
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    Write-Output "rust\Cargo.toml から version を読めません。"
    exit 1
}
$version = $Matches[1]
if ($tag -and $tag -ne "v$version") {
    Write-Output "タグ $tag と rust\Cargo.toml の version ($version) が一致しません。Cargo.toml の version を上げてからタグを付けてください。"
    exit 1
}

Write-Output "ビルドしています (v$version)..."
$ErrorActionPreference = 'Continue'  # cargo は進捗を標準エラーに出すため
& cargo build --release --manifest-path '.\rust\Cargo.toml' 2>&1 | ForEach-Object { "$_" }
if ($LASTEXITCODE -ne 0) {
    Write-Output "ビルドに失敗しました。"
    exit 1
}
$ErrorActionPreference = 'Stop'

$name = "saka-message-viewer-$version-windows-x64"
$stage = Join-Path (Get-Location) "dist\$name"
$zip = Join-Path (Get-Location) "dist\$name.zip"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
if (Test-Path $zip) { Remove-Item $zip -Force }
New-Item -ItemType Directory -Force (Join-Path $stage 'script_ps1') | Out-Null

Copy-Item '.\rust\target\release\message-viewer.exe' $stage
Copy-Item '.\config.json' $stage
Copy-Item '.\static' $stage -Recurse
Copy-Item '.\MoveFiles.bat' $stage
Copy-Item '.\script_ps1\MoveFiles.ps1', '.\script_ps1\MergeDatedFolders.ps1' (Join-Path $stage 'script_ps1')
Copy-Item '.\README.md', '.\LICENSE' $stage
Copy-Item '.\docs\release\README.txt' $stage

# 配布する config.json は保存フォルダ未設定であること(初回にブラウザの設定画面で指定する)
$cfg = Get-Content (Join-Path $stage 'config.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if ($cfg.root -ne '') {
    Write-Output "config.json の root が空ではありません。空にしてから配布してください。"
    exit 1
}

Compress-Archive -Path $stage -DestinationPath $zip
Write-Output "完了しました: $zip"
