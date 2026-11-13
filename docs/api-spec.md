# API 細部仕様

[docs/api.md](api.md) を補う、API とインデクサの細部の一覧。挙動の基準は `rust/tests/` の契約テストで、この文書と食い違ったらテストが正。

## 0. 共通ルール

| 項目 | 仕様 |
|---|---|
| 待受 | `127.0.0.1:{port}` のみ |
| 入力検証エラー | クエリ/ボディの型・範囲違反は **422**（`{"detail":[{"type","loc","msg"}]}`）。UIは本文を使わない |
| 業務エラー | `{"detail": "<メッセージ>"}` + 400/403/404/409/416 |
| 409の一部 | `{"status":"already_running"}` / `{"status":"root_not_configured"}`（`detail` ではなく `status`） |
| JSON | UTF-8、日本語はエスケープせずそのまま |
| Hostヘッダ | ポートを除いた名前が `localhost` / `127.0.0.1` / `[::1]` 以外は **403 `{"detail":"invalid host"}`** |
| Sec-Fetch-Site | `cross-site`/`same-site` は、`GET` かつ `Sec-Fetch-Mode: navigate` 以外なら **403**（`cross-site request rejected`） |
| Origin | `GET/HEAD/OPTIONS` 以外で `Origin` があり、`http://{Hostヘッダ}` と不一致なら **403**。`Origin` 無しは通す |
| 静的 | `GET /` → `static/index.html`、`/static/js/*` → `static/js/` |
| HEAD | GET のルートは `HEAD` も受ける（本文なしで200/206） |
| is_dev | 設定ファイル名が `config-dev.json` のとき true（1秒以上かかったリクエストをログに出す） |
| クエリ | 同じキーが複数あれば最後の値を使う |

## 1. 時刻の扱い

- ファイル名の `YYYYMMDDHHMMSS` は **UTCとして解釈**して epoch 秒にする。例: `20260801000000` → `1785542400`。実在しない日時（2月30日・秒60など）のファイルは警告して取り込まない
- 一方 `from`/`to`/`date`/カレンダーは **JST(+9h)** 基準。
  - `YYYY-MM-DD` → `[epoch(その日0:00 UTC) - 9h, +86399]`
  - カレンダー集計は SQL の `strftime(..., ts + 9*3600, 'unixepoch')`
- 日付形式不正は 400（`invalid date format`）、`yearmonth` 不正は 400。
- 日付の形式: 年は4桁(`0000`不可)、月日は **1〜2桁を許容**（`2026-8-2` は有効）、前後の空白・`+`符号は不可。数字は ASCII のみ

## 2. DB

**index.db**（WAL, `synchronous=NORMAL`, `busy_timeout=30000`）
- `messages(member, group_, msg_id, ts, ts_raw, body, media, kind, flag, thumb, missing, PK(member,msg_id))`
- インデックス: `idx_member_ts(member,ts)`, `idx_ts(ts)`, `idx_member_group_ts(member,group_,ts)`
- `messages_fts` = FTS5 `(body, member UNINDEXED, msg_id UNINDEXED, tokenize='trigram')`。`rowid` で `messages` と結合
- rusqlite は `bundled`（FTS5 有効。trigram トークナイザは SQLite 3.34 以降）
- 起動時に、対応する `messages` 行の無い検索索引の行（孤児）を消す

**user.db**（index.db とは別ファイル。取り込みの作り直しでは消えない）
- `favorites(member,msg_id,created_at)`, `message_comments(member,msg_id,comment,updated_at)`,
  `hidden_members(member,hidden_at)`, `member_order(member,position,updated_at)`
- 起動時マイグレーション: `favorites.note` 列が残っていれば `message_comments` へコピー（`INSERT OR IGNORE`、note列は削除しない）

**index.db と user.db は JOIN できない**ため、コメントは `(member,msg_id)` で突き合わせる（`OR` 連結のプレースホルダ）。
SQLite の変数上限（32766）に対し、最大件数は limit(≤500) なので収まる。

## 3. メッセージ共通オブジェクト

```json
{"member","group","msg_id","ts","ts_raw","body","kind","flag","missing","media_url","thumb_url","comment"}
```
- `group`: `group_` 列。`missing`: bool。`media_url`: `"/media/" + media` または null。`thumb_url`: `"/thumbs/" + thumb` または null（空文字も null）
- `comment`: `message_comments` 由来の文字列 / null。`/api/search` は追加で `snippet`、`/api/favorites` は追加で `created_at`
- サムネの相対パス: `sha1(member の UTF-8)` の16進 `h` → `{h[:2]}/{h}/{msg_id}.jpg`

## 4. エンドポイント別の細部

### GET /api/threads
`SELECT member, group_, COUNT(*), MAX(ts) FROM messages GROUP BY member, group_ ORDER BY last_ts DESC` → 配列 `{member,group,count,last_ts}`。

### GET /api/messages
`member`必須, `limit` 1–500（既定50）, `before`/`after`/`before_id`/`after_id` は int。**`before` と `after` の両方指定は 400**。
- 並びは `(ts, msg_id)`（同じ秒の中も msg_id 順）
- 指定なし: 新しい方から `limit` 件を取り、**昇順に並べ直して**返す
- `before`: `ts < before`（`before_id` があれば `ts < b OR (ts = b AND msg_id < id)`）の新しい方から `limit` 件、昇順で返す
- `after`: `ts > after`（`after_id` があれば `ts > a OR (ts = a AND msg_id > id)`）の古い方から `limit` 件、昇順で返す
- 続きを読むときは端のメッセージの `ts` と `msg_id` を両方渡す（`ts` だけだと、同じ秒のメッセージがページ境界で抜け落ちる）
- 応答 `{"member", "messages":[...]}`

### GET /api/search
- `q` 必須（空文字は422）, `limit` 1–500, `from`/`to`/`member`/`before`/`before_rowid`。`from`/`to`/`member` の空文字は未指定扱い
- **語の分解**: 先頭から順に「`-`（任意）+ `"…"`（閉じ引用符まで。空白を含んでよい）」、無ければ「`-`（任意）+ 空白以外の連続」を1語として取る。
  空白は Unicode の空白。空の語は捨てる。先頭が `-` の語は除外語。`-` の直後が空白/末尾なら `-` 自体が含める語。
  閉じない `"` は通常の語の一部になる（例: `"ab cd` → `"ab`, `cd`）
- **FTS と LIKE の振り分け**: trigram は3文字未満を索引できないため、3文字以上（コードポイント数）の「含める語」が1つでもあれば FTS で候補を絞る。
  3文字以上の語（含める語・除外語）は FTS 式に入れ、2文字以下の語は `body LIKE` / `NOT LIKE` で追加の絞り込みにする（`used_fts` は true）。
  3文字以上の「含める語」が無ければ（除外語だけの場合も含む）全語を LIKE で扱う
- **FTS式**: 各語を `"…"`（`"`は`""`にエスケープ）で囲み ` AND ` 連結、除外は ` NOT "…"` を末尾に連結
- **LIKE経路**: `includes`/`excludes` がどちらも空なら元の `q` 全体を1語として扱う。
  `body LIKE ? ESCAPE '\'`（`\ % _` をエスケープ）。除外のみのときは `body IS NOT NULL AND body <> ''` を加える。
  `LIKE` は SQLite 既定で **ASCIIのみ大小無視**、FTS(trigram) は Unicode の大小も無視する
- **並び**: `ORDER BY m.ts DESC, m.rowid DESC`
- **ページング**: `LIMIT limit+1` で `has_more` 判定。`before` + `before_rowid` があれば `(ts < b OR (ts = b AND rowid < r))`、`before` のみなら `ts < b`
- **応答**: `query, used_fts, terms{include,exclude}, total, has_more, next_before(=最後の結果のts), next_before_rowid(=最後の行のrowid), results`
  - `total` は初回(`before` 無し)のみ件数、`before` 指定時は null。0件時の `next_before*` は null
  - SQLiteの構文エラーは 400 `search query error: …`
- **スニペット**（FTS/LIKE共通の自前実装。SQLiteの `snippet()` は使わない）
  - 定数: `SNIPPET_CONTEXT=40`, `SNIPPET_MAX=300`, 省略記号 `...`
  - 最初のヒット位置 = 含める語の本文中(大文字小文字を区別しない比較)で最小の位置。無ければ先頭 `SNIPPET_MAX` 文字
  - 切り出し `[max(0,hit-40), +300)`。マーク範囲は長い語優先・重なり無しで前から決め、**`<mark>`以外は HTML エスケープ**（`& < > " '` を実体参照）
  - 先頭が切れたら `...` を前置、末尾が切れたら後置
  - **位置は文字(コードポイント)単位**。Rust では `Vec<char>` で扱い、バイト位置と混同しない
  - 大文字小文字を区別しない比較は、1文字を1文字へ畳んで行う（小文字化して複数文字になるものは先頭の1文字。`İ`→`i`。語末シグマ `ς` は `σ` にそろえる）。
    畳んでも文字数が変わらないので、畳んだ文字列での位置がそのまま元の本文の位置になる

### GET /api/jump
`date` 必須(JST日付)。`ts >= 日付開始` の最小 ts。無ければ `ts: null`。応答 `{member,date,ts}`。

### GET /api/calendar
- `yearmonth` 無し: `{member, calendar:[{yearmonth,count}]}`（昇順）
- `yearmonth=YYYY-MM`: `{member, yearmonth, days:[{date,count}]}`（昇順）。形式違反は400、実在しない月（`2026-13`）は0件

### GET /api/media-list
`member`必須, `kind` ∈ image/video/audio（他は400。省略時は `kind != 'text'`）, `limit` 1–200（既定60）, `before`(ts), `before_id`, `from`/`to`。
常に `missing=0`。並びは `ts DESC, msg_id DESC`。`before_id`（前ページ最後の msg_id）を添えると `ts < b OR (ts = b AND msg_id < id)`。
`LIMIT limit+1` で `has_more`。応答 `{items, has_more}`。

### お気に入り
- `POST /api/favorites` body `{member,msg_id}`: 空/`msg_id`無しは400、型が違えば400（下記「ボディの解釈」）。
  メッセージ不在は404。`INSERT OR IGNORE`、`created_at`=現在秒。応答 `{"status":"ok","favorited":true}`
- `DELETE /api/favorites?member&msg_id` → `{"status":"ok","favorited":false}`（存在しなくても200）
- `GET /api/favorites?member?&before?&before_rowid?&limit(1–200,既定50)` → `{"favorites":[…], "next_before", "next_before_rowid"}`。
  並びは `created_at DESC, rowid DESC`。`next_before` / `next_before_rowid` はこのページで読んだ最後の行（0件なら null）で、
  次ページの `before` / `before_rowid` に渡すと `(created_at, rowid)` を目印に続きを返す。
  `member` は「指定があるか」で判定する（空文字も指定扱い）。index.db 側にメッセージが無い行は**黙って除外**（幽霊お気に入り）
- `GET /api/favorites/ids?member` → `{"msg_ids":[…]}`（順序は保証しない）

### PUT /api/comments
body `{member,msg_id,comment}`。`comment` の空文字/省略/null は**削除**。長さ > **1000 文字**(コードポイント)は400（存在確認より先に判定）。
メッセージ不在は404。応答 `{"status":"ok","comment": 文字列|null}`。UPSERT で `updated_at` 更新。

### ボディの解釈（お気に入り・コメント・非表示・並び順・設定）
値は型どおりに受け取る。型が違えば 400（`{"detail": "<項目> は…で指定してください"}`）。
- `member`・`comment`・`members` の要素: 文字列。前後の Unicode の空白（全角空白を含む）を除いてから使う。長さはコードポイント数
- `msg_id`: JSON の整数。数字の文字列（`"3"`）・小数・真偽値は不可
- `hidden`: 真偽値。省略・null は false
- JSON オブジェクト以外（不正JSON・空・配列）は 422

### 非表示メンバー / 並び順
- `GET /api/hidden-members` → `{"members":[…]}`（名前昇順）。`PUT` body `{member,hidden}`: `member` 空は400。
  応答 `{"status":"ok","member","hidden"}`
- `GET /api/member-order` → `{"members":[…]}`（`position, member` 順）。`PUT` body `{members:[…]}`: 配列以外/文字列以外の要素/空要素/重複は400。
  **全置換**（DELETE→INSERT）。応答 `{"status":"ok","count"}`

### GET /media/{path}, /thumbs/{path}
- `/media` は `root` 未設定で **409** `root is not configured`。`/thumbs` は `thumbs_dir` 基準
- パス解決: `base.join(path)` を正規化し、`base` の外に出れば **403 `invalid path`**（`%2e%2e`/`..%2f` も正規化後に判定）。
  存在する最長の祖先まではシンボリックリンクを解決し、残りは字句的に連結する。存在しない場所への `..` 脱出も、存在チェックより先に403。
  存在しない/ディレクトリは **404 `file not found`**
- MIME（拡張子、小文字化）: `.jpg/.jpeg`→`image/jpeg`, `.png`→`image/png`, `.mp4`→`video/mp4`, `.txt`→`text/plain; charset=utf-8`, **それ以外は `application/octet-stream`**（`.m4a` 等も）
- 常に `Accept-Ranges: bytes`, `Content-Length`。`Range` 無し→200全体
- **キャッシュ**: 200/206/304 に `ETag`（`"サイズ16進-更新時刻ns16進"`）と `Last-Modified`、`Cache-Control` を付ける。
  `Cache-Control` は `/thumbs` が `max-age=86400`（パスが (member, msg_id) で決まり中身はまず変わらない）、`/media` が `no-cache`（毎回検証）。
  `If-None-Match` が一致（`*`・弱い比較可）すれば **304**。`If-None-Match` が無いときだけ `If-Modified-Since` を見る（送り返された日付との一致のみ）。
  `If-Range` が今の `ETag` / `Last-Modified` と違えば `Range` を無視して200全体
- **Range**（`bytes=(\d*)-(\d*)` を前方一致、**単一範囲のみ**）:
  - 不一致 / 両方空 → 416
  - `-N`（start空）→ 末尾N（`N`がサイズ超なら全体 `0..size-1`）
  - `N-` → `N..size-1`。`end` は `size-1` に丸める
  - `start > end` または `start >= size` → **416** + `Content-Range: bytes */{size}`
  - 成功は **206** + `Content-Range: bytes s-e/size`。ストリーミングで返す（動画を丸ごとメモリに載せない）
  - 前方一致なので `bytes=0-4,10-12`（複数範囲）は先頭のみ、`bytes=2-3xyz` も 2-3 として扱う。
    `bytes=` は大文字小文字を区別（`BYTES=` は416）。巨大な数値は飽和させて同じ判定にする。0バイトのファイルは全体取得200、Range指定は416
  - `tower-http::ServeFile` は 416 の本文・`-N` の超過・不正ヘッダの扱いがこの仕様と異なるため、自前で実装している

### GET /api/media-path
`member`,`msg_id` 必須。行が無い/`media`が空は404。`root` 未設定は409。`{"path": 絶対パス文字列, "exists": bool}`。
パス検証は「外に出ないか」のみ（実在チェック無し）。Windows では `\` 区切りのまま返り、`\\?\` 接頭辞は付けない。

### GET/POST /api/settings
- GET: `{"root": 文字列|null, "configured": bool, "is_dev": bool, "ffmpeg_available": bool, "ffmpeg_path": 文字列|null}`
- POST body `{root}`: 前後の空白を除いて 空→400、**絶対パスでない→400**、存在しない/ディレクトリでない→400。
  成功時 設定ファイル（`config-dev.json` 使用中はそちら）の `root` を書き換え（他のキーと順序は保つ。2スペースのインデント + 末尾改行）、
  取り込み未実行なら差分取り込みを**バックグラウンドで開始**。応答 `{"root": 解決済み絶対パス, "configured": true}`

### POST /api/reindex?mode&member
- `root` 未設定→409 `{"status":"root_not_configured"}`。`mode` ∈ incremental(既定)/full 以外→400
- 実行中(自プロセス or `.index.lock` の他PID生存)、またはメンバー削除・初期化の最中→409 `{"status":"already_running"[, "detail":"index process pid=N is running"]}`
- 成功: `{"status":"started","mode","member"}`（`member` 未指定は null）

### GET /api/reindex/status
`{running, started_at, finished_at, progress, last_summary, last_error}`
- 自プロセスが実行中→メモリ上の状態をそのまま返す
- そうでなければ `data/progress.json` を読む（無い/壊れていればメモリ上の状態を返す）。
  `progress.json` が `running:true` なのに `pid` が死んでいれば `phase:"interrupted"` と
  `last_error:"index process (pid=N) is not running (interrupted)"` を返す
- それ以外は `progress = progress.json + {"source":"file"}`、`finished_at = running ? null : updated_at`、
  `last_summary` は `phase=="done"` のとき `progress.json` の `summary`、`last_error` は `phase=="error"` のとき `message`
- `last_summary` のキー: `new_count, updated_count, skipped_count, missing_count, thumb_count, elapsed_sec, total_files_scanned, total_messages_scanned`
- `progress.json` のキー: `running, phase, done, total, percent, elapsed_sec, eta_sec, started_at, updated_at, pid, mode, message, summary`
- 取り込みが想定外のエラー（panic）で止まった場合も、`last_error` に記録して実行中の状態を解く

### POST /api/reset
実行中(自/他プロセス)→409 `{"status":"already_running"}`。成功時の副作用（順序どおり）:
index.db(+`-wal`/`-shm`)削除→スキーマ再作成→`thumbs_dir` を空に→`progress.json`,`.index.lock` 削除→
user.db(+`-wal`/`-shm`)削除→設定ファイルの `root` を `""`→メモリ上の取り込み状態をクリア。応答 `{"status":"reset"}`。
**Windowsでは開いたままのハンドルがあると削除に失敗する**ため、index.db の利用をゲート（RwLock）で囲み、
削除〜再作成の間は書き込み側を握って新しい接続を作らせない。user.db も同様にロックを握ったまま消す。
`.index.lock` の PID が生存していれば 409 `{"status":"already_running","detail":"index process pid=N is running"}`、
終了済み PID のロック（stale）は無視する

### DELETE /api/members/{member}
reset同様に実行中は409。`messages` と検索索引の行、該当サムネを削除し、`favorites`/`message_comments` から該当メンバーを削除。
応答 `{"status":"ok","member","deleted_messages": 件数}`。存在しないメンバーでも 200（0件）。**root側のファイルは触らない**。
表示設定（`hidden_members`/`member_order`）は消さない。
検索索引の行を残すと、SQLite が削除後に最大 rowid を再利用したときに、再取り込みで rowid が衝突したり、
削除済みの本文で無関係なメッセージがヒットしたりするため、同じトランザクションで消す。

## 5. インデクサ

- ファイル名 `^(\d+)_(\d)_(\d{14})(_.+)?\.(\w+)$`（`(_.+)?` は colmsg v5 以降の投稿者名で、読み捨てる）。flag 0=text 1=image 2=video 3=audio。`msg_id` は int 化（ゼロ埋めの衝突は警告のみ）
- 同一 `(member,msg_id)` の `.txt` が本文、非txtがメディア（複数あれば先頭を採用）。メディアがあればそのflag、無ければtxtのflag
- **本文**: 改行を `\n` に揃える（`\r\n`・`\r` → `\n`）。UTF-8 として不正なら本文なし（警告のみ）。BOM は残る
- 取り込み結果の判定:
  - 既存行(`member,msg_id`)が無ければ `new`（INSERT。`ON CONFLICT DO UPDATE` で冪等化、`thumb` は `COALESCE` で既存値を保護）
  - あれば `body/media/kind/flag/ts_raw/missing(≠0)/thumb` のいずれかが変わったときだけ `UPDATE`+検索索引の差し替えで `updated`、変化なしは `skipped`
  - 検索索引の行は `body` が空でないときだけ作る。rowid は `messages` の rowid に合わせ（検索の JOIN が `rowid` 結合のため）、差し替えは rowid で消して入れ直す
- `missing` 判定: 今回の走査範囲（`incremental` なら直近月のみ）に含まれる既存行のうち、今回見つからなかったものを `missing=1`、
  再発見されたら `0` に戻す。範囲判定の年月は **`ts + 9h` の JST** から求める（ファイル名のUTC解釈とずれる点に注意）
- 走査パターン4種（グループ有無 × `{YYYY}/{YYYYMM}/`有無）。`incremental` は直近 `incremental_months` ヶ月（ローカル時刻基準）のフォルダのみ。
  まだ取り込んだことの無いメンバーは全期間を走査する
- **走査順**: 取り込み順が rowid を決め、同じ秒のメッセージの検索順に効く。ディレクトリと直下ファイルは名前順
  （Windows は大文字小文字を区別しない）、月ディレクトリ内のファイルは OS が返す順のまま
- **サムネ**: 対象は拡張子 `jpg` のファイル（`png` 等は対象外）。無ければ ffmpeg があるときだけ `flag==2 && ext==mp4` の動画。
  - 画像は EXIF の向きを反映してから、長辺 `thumb_size` に収める（拡大しない。端数は縦横比の誤差が小さい方）。JPEG品質 `thumb_quality`
  - 動画は ffmpeg で1フレーム抜き出す（1秒地点、取れなければ先頭。30秒でタイムアウト）
  - 一時ファイル（`{msg_id}.part.jpg`）に書いてから置き換える。サムネファイルが既にあれば作り直さない（0バイトは無いものとして作り直す）
  - 1枚の失敗で取り込み全体は止めない
- **ffmpeg の探索順**: 設定ファイルのディレクトリ → 実行ファイルのディレクトリ（それぞれ `bin/` → 直下）→ PATH
- 排他: `data/.index.lock`（1行目が PID、2行目が取得時刻。PID が死んでいれば stale として奪う。Windows は `OpenProcess` 系で生死確認）。
  進捗は `data/progress.json` を一時ファイル+rename で原子的に書く。進捗ファイルを書き終えてからロックを解放する
- CLI（[docs/cli.md](cli.md)）: `message-viewer [--config <path>] index [--full|--incremental|--rebuild] [--only-member X] [--no-thumbs]`。ロック競合は終了コード 2

## 6. 設定

- 読み込み: `--config <path>`。無ければ実行ファイルと同じディレクトリの `config-dev.json` → `config.json`
- 環境変数 `MSGVIEWER_ROOT/DATA_DIR/DB_PATH/THUMBS_DIR/PORT` が**空でなければ**優先
- 相対パスは設定ファイルのディレクトリ基準（字句的に正規化。存在しないパスでもよい）。`data_dir`/`thumbs_dir`/`db_path` の親は起動時に作成
- キー: `root, data_dir, thumbs_dir, db_path, port, thumb_size, thumb_quality, incremental_months, auto_index_on_startup`
- `auto_index_on_startup` が true かつ `root` 設定済みなら、起動時に差分取り込みをバックグラウンドで開始
- `static/` は設定ファイルのディレクトリを優先し、無ければ実行ファイルの隣を使う

## 7. サーバの起動

- 最初にポートを確保する。使用中なら二重起動とみなし、案内を出して終了コード1で終わる（DB もログファイルも開かない）。
  ダブルクリックで起動した場合は `http://localhost:{port}/` をブラウザで開く
- ログは stderr と `data_dir/server_log.txt`（追記。起動ごとに `========== 日時 起動 ==========`）の両方に出す。各行の先頭に日時（ローカル時刻）。
  ファイルに書けなければ警告して stderr だけに出す
- ダブルクリックで起動した場合（コンソールに付いているプロセスが自分だけ）、起動失敗・異常終了のときは Enter を押すまでウィンドウを閉じない
