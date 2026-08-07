# 動画サムネイルとffmpeg（任意）

ffmpegは不要（画像サムネイルはPillowで生成し、動画・音声はブラウザの `<video>`/`<audio>` でネイティブ再生する設計）。
ffmpegを追加で用意しておくと、動画サムネイル（一覧表示用のプレビュー画像）も自動生成されるようになる（任意）。
未インストール時は動画は▶プレースホルダ表示のままで、機能上の問題はない。

- **置き場所**: `message-viewer\bin\ffmpeg.exe` または `message-viewer\ffmpeg.exe`（プロジェクト直下）に置くだけで自動認識される。**PATHを通す必要はない。**
  PATHが通っている環境ではそちらのffmpegも（同梱が無ければ）使われる。
- Windows版ffmpegは [https://www.gyan.dev/ffmpeg/builds/](https://www.gyan.dev/ffmpeg/builds/) から入手できる（`essentials` ビルドで十分）。
  ダウンロードしたzipを展開し、中にある `bin\ffmpeg.exe` を取り出して上記のいずれかに置く。
- 後からffmpegを配置した場合は再取り込みを行えば、既に画像サムネイルがある分は再生成されず、
  サムネイルが未生成の動画分だけがまとめて生成される（`run_server.bat` 起動時の自動取り込み、または `python indexer.py --incremental` でよい）。
  > **注意**: 差分取り込み（既定 `--incremental`）は直近 `incremental_months`（既定2ヶ月）分しか走査しない。
  > **それより古い動画のサムネイルもまとめて作りたい場合は、`run_rebuild.bat` または `python indexer.py --full` で全期間を走査する必要がある。**
  > ただし `run_rebuild.bat` はDBを作り直す（`--rebuild`）ため取り込み済みデータの再走査に時間がかかる。
  > **DBを削除せず未生成のサムネイル（動画）だけを作りたい場合は `run_thumbs.bat` を使う**とよい。
  > 内部で `python indexer.py --full`（`--rebuild` なし）を実行するため、既存DBを保持したまま
  > 全期間を対象にサムネ未生成分だけを生成できる（上記の「`--full` が必要」という条件も自動的に満たされる）。

[← READMEに戻る](../README.md)
