# サーバの起動とバッチファイル（Windows運用）

## サーバの起動

プロジェクト直下の `message-viewer.exe` をダブルクリックする（ターミナルから `.\message-viewer.exe` でもよい）。黒いウィンドウが開いたら、ブラウザで `http://localhost:8000/` を開く。終了するにはウィンドウを閉じるか `Ctrl+C` を押す。

- **ログ**: 画面に出すのと同時に `data\server_log.txt` に追記する。起動のたびに `========== 日時 起動 ==========` の区切り行が入る。ログファイルに書けない場合（他のプロセスがロックしているなど）は、警告を出して画面にだけ出す
- **二重起動**: ポート（`config-dev.json` があればそちら、無ければ `config.json` の `port`。既定8000）が既に使われていれば、サーバが起動済みとみなして案内を出して終了する。ダブルクリックで起動した場合は、そのページをブラウザで開く
- **起動に失敗したとき**: ダブルクリックで起動した場合は、エラーを読めるよう Enter を押すまでウィンドウを閉じない（ターミナルから起動した場合は待たない）

`message-viewer.exe` が無い場合は、先に `build_rust.bat` でビルドする。

## バッチファイル

実体の `.ps1` は `script_ps1\` にまとめてあり、ルート直下の `.bat` からダブルクリックで実行できる。

```powershell
cd \message-viewer

.\build_rust.bat                # ビルド（初回とソース更新時）。message-viewer.exe をプロジェクト直下に置く
.\MoveFiles.bat "C:\messages"   # 日付付きフォルダの統合 + {YYYY}/{YYYYMM}/ 階層への整形（任意）
```

日付付きフォルダの統合だけを単独で行いたい場合は `script_ps1\MergeDatedFolders.ps1` を直接実行する（`.bat` ラッパーは無い）。

取り込み（全期間・メンバー個別・未生成サムネイルの作成）はWeb UIの設定（&#9881;）→「取り込み」タブから実行する。CLIで行う場合は [docs/cli.md](cli.md) を参照。

実行ポリシーは `.bat` 内で `-ExecutionPolicy Bypass` 済みのため通常は問題ない。ps1を直接実行する場合は `powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\script_ps1\build_rust.ps1` のように明示する。

[← READMEに戻る](../README.md)
