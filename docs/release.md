# リリース（実行ファイルの配布）

GitHub Releases で、Windows 用の zip（`saka-message-viewer-<版>-windows-x64.zip`）を配布する。
ビルドは GitHub Actions（[.github/workflows/release.yml](../.github/workflows/release.yml)）が行う。

## 手順

1. `rust/Cargo.toml` の `version` を上げてコミットし、`main` に入れる（`rust/Cargo.lock` も更新される）
2. その版のタグを付けて push する

   ```powershell
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. Actions がテスト → release ビルド → zip 作成 → Release 作成まで行う（数分）。
   タグと `version` が一致しないと、zip を作る前に止まる

Release の本文には、ダウンロード方法の案内と、前回のタグからの変更一覧（自動生成）が入る。

## zip の中身

| ファイル | 内容 |
| --- | --- |
| `message-viewer.exe` | サーバ本体（Visual C++ ランタイム・SQLite を組み込み済み） |
| `config.json` | 設定（`root` は空。初回にブラウザの設定画面で指定する） |
| `static\` | 画面 |
| `MoveFiles.bat`, `script_ps1\MoveFiles.ps1`, `script_ps1\MergeDatedFolders.ps1` | 保存フォルダの整形（任意） |
| `README.txt` | 配布版の使い方（[docs/release/README.txt](release/README.txt)） |
| `README.md`, `LICENSE` | |

ffmpeg は同梱しない（サイズとライセンスのため。置き方は [docs/ffmpeg.md](ffmpeg.md)）。

## 手元で zip を確かめる

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\script_ps1\package_release.ps1
```

`dist\` に Actions と同じ zip ができる（`dist\` は git の対象外）。

[← READMEに戻る](../README.md)
