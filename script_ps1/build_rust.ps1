chcp 65001 | Out-Null
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Set-Location (Split-Path $PSScriptRoot -Parent)

# サーバ(message-viewer.exe)をビルドし、プロジェクト直下にコピーする。
# 事前に Rust(rustup)と Visual Studio Build Tools(C++)のインストールが必要(docs/setup.md 参照)。

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($cargo) {
    $cargo = $cargo.Source
} else {
    # rustup 直後はこのウィンドウの PATH にまだ入っていないことがあるため、既定の場所も探す
    $candidate = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (Test-Path $candidate) {
        $cargo = $candidate
    } else {
        Write-Output "cargo が見つかりません。Rust をインストールしてください(winget install Rustlang.Rustup)。"
        exit 1
    }
}

Write-Output "ビルドしています(初回は数分かかります)..."
# cargo は進捗を標準エラーに出すため、PowerShell のエラー扱いにせず文字列として流す
$ErrorActionPreference = 'Continue'
& $cargo build --release --manifest-path '.\rust\Cargo.toml' 2>&1 | ForEach-Object { "$_" }
if ($LASTEXITCODE -ne 0) {
    Write-Output "ビルドに失敗しました。"
    exit 1
}

try {
    Copy-Item '.\rust\target\release\message-viewer.exe' '.\message-viewer.exe' -Force -ErrorAction Stop
} catch {
    Write-Output "message-viewer.exe をコピーできませんでした。サーバが起動中なら止めてから再実行してください。"
    Write-Output "  $($_.Exception.Message)"
    exit 1
}
Write-Output "完了しました: $(Resolve-Path '.\message-viewer.exe')"
