# API

- `GET /api/threads` — メンバー一覧（グループ、件数、最新メッセージ日時）
- `GET /api/messages?member=X&before=<ts>&before_id=<msg_id>&after=<ts>&after_id=<msg_id>&limit=50` — 双方向カーソルページング。並びは `ts`、同じ秒の中は `msg_id` 順。続きを読むときは表示中の端のメッセージの `ts` と `msg_id` を両方渡す（`ts` だけだと同じ秒のメッセージがページ境界で抜け落ちる）。`before_id` / `after_id` を省くと `ts` だけで区切る
- `GET /api/search?q=...&member=X&from=YYYY-MM-DD&to=YYYY-MM-DD&before=<ts>&before_rowid=<rowid>&limit=50` — 全文検索。`q` は空白区切りで**AND検索**、`"..."` で囲むとフレーズ、先頭に `-` を付けた語は除外（例: `ライブ "あり がとう" -中止`）。3文字以上の語はFTS5（trigram）で引き、2文字以下の語はLIKEで追加の絞り込みにする。3文字以上の含める語が無い場合（除外語しか無い場合を含む）は全体をLIKEで検索する（LIKE経路でもAND・除外の意味は保たれ、`%` `_` は文字どおりに扱う）。除外語だけの場合は、その語を含まない本文ありのメッセージを返す。`snippet` はFTS/LIKEどちらの経路でも同じ形式で、ヒット箇所周辺を抜粋し該当語を `<mark>` で囲んだ文字列を返す（`<mark>` 以外はHTMLエスケープ済み）。`before` / `before_rowid` に前ページの `next_before` / `next_before_rowid` を渡すと続き（`ts` 降順、同一 `ts` 内は rowid 降順）を取得できる（同じ秒のメッセージを取りこぼさないため両方渡すこと）。レスポンスは `total`（絞り込み条件に対する総ヒット数。`limit` で切られても全件数を返す。初回のみで、`before` 指定時は `null`）、`has_more`（このページより後に残りがあるか）、`next_before` / `next_before_rowid`（次ページ用カーソル）、`terms`（解析された `include` / `exclude` の語。UI側の検索語ハイライトに使う）、`used_fts`、`results` を含む
- `GET /api/jump?member=X&date=YYYY-MM-DD` — 指定日以降の最初のメッセージのtsを取得
- `GET /api/calendar?member=X` — 年月ごとの件数
- `GET /api/media-list?member=X&kind=image|video|audio&from=YYYY-MM-DD&to=YYYY-MM-DD&before=<ts>&before_id=<msg_id>&limit=60` — メンバー別メディアギャラリー。`kind` 省略時はtext以外すべて。`from`/`to` で日付範囲を絞り込み可能。続きは前ページ最後の `ts` と `msg_id` を `before` / `before_id` に渡す
- `POST /api/favorites` — お気に入り登録（body: `{member, msg_id}`）。対象メッセージが存在しない場合は404
- `DELETE /api/favorites?member=X&msg_id=N` — お気に入り解除
- `GET /api/favorites?member=X&before=<created_at>&before_rowid=<rowid>&limit=50` — お気に入り一覧（`created_at` 降順カーソルページング）。`member` 省略時は全メンバー横断。続きは応答の `next_before` / `next_before_rowid` を `before` / `before_rowid` に渡す（同じ秒に登録したものを取りこぼさないため両方渡す）
- `GET /api/favorites/ids?member=X` — 指定メンバーのお気に入り `msg_id` 一覧（★表示の初期化用）
- `PUT /api/comments` — メッセージへのコメントを更新（body: `{member, msg_id, comment}`）。お気に入り登録の有無を問わず任意のメッセージに付けられる。`comment` を空文字にするとコメントを消去。対象メッセージが存在しない場合は404、1000文字超は400。`/api/messages`・`/api/search`・`/api/favorites`・`/api/media-list` のレスポンス各項目に `comment` フィールドとして同梱される
- `GET /api/hidden-members` — サイドバーで非表示にしているメンバー名の一覧（名前順）
- `PUT /api/hidden-members` — メンバーの表示/非表示を切り替える（body: `{member, hidden}`）。サイドバー一覧の表示上の設定であり、メッセージ自体は削除しない（検索・お気に入り一覧・メディアギャラリーには引き続き登場する）
- `GET /api/member-order` — サイドバーの並び順に指定があるメンバー名の一覧（並び順）
- `PUT /api/member-order` — サイドバーの並び順を設定（body: `{members: [name, ...]}`）。差分ではなく順序の全体を渡して置き換える。ここに無いメンバーは名前順で後ろに続くため、新規メンバーが取り込まれても既存の並びは崩れない。空配列で並び順の指定を解除（＝全員名前順に戻す）。メンバー名の重複・空文字は400。表示上の設定であり、メッセージ自体には影響しない
- `GET /media/{path}` — 実ファイル配信（Range対応、動画シーク再生用）。`ETag` / `Last-Modified` 付きで、変わっていなければ304（`Cache-Control: no-cache`）
- `GET /thumbs/{path}` — サムネ配信。`Cache-Control: max-age=86400`（1日はブラウザのキャッシュを使う）
- `POST /api/reindex?mode=incremental|full&member=X` — 取り込みをバックグラウンドで実行。`mode` 既定は `incremental`（直近Nヶ月）、`full` で全期間。`member` 指定でそのメンバーのみ。DBは作り直さないため、お気に入り・サムネイルは保持される
- `GET /api/reindex/status` — 取り込みの進行状況
- `GET /api/settings` / `POST /api/settings` — 保存フォルダ（`root`）の取得・設定（設定時に差分取り込みを自動開始）
- `POST /api/reset` — DB・サムネイル・お気に入り・コメント・保存フォルダ設定をすべて削除し初回起動相当の状態に戻す（取り消し不可）
- `DELETE /api/members/{member}` — 指定メンバーのメッセージ・サムネイル・お気に入り・コメントをDBから削除する（取り消し不可）。`root` フォルダ側のファイルには触れないため、`root` に同名メンバーがまだ存在する場合は次回の取り込みで再度取り込まれる
- `GET /` — `static/index.html`（本実装のUI）を返す

[← READMEに戻る](../README.md)
