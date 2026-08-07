# API

- `GET /api/threads` — メンバー一覧（グループ、件数、最新メッセージ日時）
- `GET /api/messages?member=X&before=<ts>&after=<ts>&limit=50` — 双方向カーソルページング
- `GET /api/search?q=...&member=X&from=YYYY-MM-DD&to=YYYY-MM-DD&limit=50` — FTS5全文検索（2文字以下はLIKEにフォールバック）
- `GET /api/jump?member=X&date=YYYY-MM-DD` — 指定日以降の最初のメッセージのtsを取得
- `GET /api/calendar?member=X` — 年月ごとの件数
- `GET /media/{path}` — 実ファイル配信（Range対応、動画シーク再生用）
- `GET /thumbs/{path}` — サムネ配信
- `POST /api/reindex` — 差分取り込みをバックグラウンドで実行
- `GET /api/reindex/status` — 取り込みの進行状況
- `GET /api/settings` / `POST /api/settings` — 保存フォルダ（`root`）の取得・設定（設定時に差分取り込みを自動開始）
- `GET /` — `static/index.html`（本実装のUI）を返す

[← READMEに戻る](../README.md)
