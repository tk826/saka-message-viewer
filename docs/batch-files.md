# バッチファイル一覧（Windows運用）

実体の `.ps1` は `script_ps1\` にまとめてあり、ルート直下の `.bat` からダブルクリックで実行できる。実行後もウィンドウは閉じずに待機し（`Ctrl+C`で中止可）、ログはコンソール表示と `data\*.txt` の両方に残る（ログファイルは追記式で、実行のたびに `========== 日時 起動 ==========` の区切り行が追加される。ログファイルが他プロセスにロックされている場合はログ無しで起動を継続する）。

`run_server.bat` はサーバ起動前にポート（`config-dev.json` があればそちら、無ければ `config.json` の `port`、既定8000）が既に使用中でないか確認する。使用中の場合はサーバの二重起動とみなし、日本語のメッセージを表示して終了する（既にサーバーが起動しているだけなので、`http://localhost:<port>/` をブラウザで開けばよい）。

```powershell
cd \message-viewer

.\run_server.bat          # サーバ起動（app.py）。ログは data\server_log.txt
.\run_rebuild.bat         # DBを作り直して全件走査。ログは data\rebuild_log.txt
.\run_only_member.bat     # 特定メンバーのみ取り込み。ダブルクリックでメンバー名を対話入力
.\run_only_member.bat "メンバー名"   # 引数で直接指定も可
.\run_thumbs.bat          # DBを保持したまま未生成のサムネイルのみ作成（ffmpeg配置後の動画サムネ生成用）。ログは data\thumbs_log.txt
```

実行ポリシーは `.bat` 内で `-ExecutionPolicy Bypass` 済みのため通常は問題ない。ps1を直接実行する場合は `powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\script_ps1\run_server.ps1` のように明示する。

[← READMEに戻る](../README.md)
