# API 細部仕様

[docs/api.md](api.md) を補う、API とインデクサの細部の一覧。挙動の基準は `rust/tests/` の契約テストで、この文書と食い違ったらテストが正。

## 0. 共通ルール

| 項目 | 仕様 |
|---|---|
| 待受 | `127.0.0.1:{port}` のみ |
| 入力検証エラー | クエリの型・範囲違反と、ボディが JSON オブジェクトでない場合は **422**（`{"detail":[{"type","loc","msg"}]}`）。UIは本文を使わない。ボディの**項目**の型違反は 400（下記「ボディの解釈」） |
| 業務エラー | `{"detail": "<メッセージ>"}` + 400/403/404/409/416 |
| 想定外のエラー | DB エラー・ファイル操作の失敗・処理スレッドの panic などは **500** `{"detail": "<エラー文>"}` |
| 未定義のパス | 404 `{"detail":"Not Found"}` |
| 409の一部 | `{"status":"already_running"}` / `{"status":"root_not_configured"}`（`detail` ではなく `status`） |
| JSON | UTF-8、日本語はエスケープせずそのまま |
| Hostヘッダ | ポートを除いた名前が `localhost` / `127.0.0.1` / `[::1]` 以外は **403 `{"detail":"invalid host"}`** |
| Sec-Fetch-Site | `cross-site`/`same-site` は、`GET` かつ `Sec-Fetch-Mode: navigate` かつ `Sec-Fetch-Dest` が無いか `document` のとき以外は **403**（`cross-site request rejected`。他サイトの iframe への読み込みも拒否） |
| 埋め込み | すべての応答に `X-Frame-Options: DENY` と `Content-Security-Policy: frame-ancestors 'none'`（クリックジャッキング対策） |
| Origin | `GET/HEAD/OPTIONS` 以外で `Origin` があり、`http://{Hostヘッダ}` と不一致なら **403**。`Origin` 無しは通す |
| 静的 | `GET /` → `static/index.html`、`/static/js/*` → `static/js/` |
| HEAD | GET のルートは `HEAD` も受ける（本文なしで200/206） |
| is_dev | 設定ファイル名が `config-dev.json` のとき true（1秒以上かかったリクエストをログに出す） |
| クエリ | 同じキーが複数あれば最後の値を使う。整数は前後の空白を許すが、**空文字（`before=`）は未指定ではなく 422**。文字列の空文字の扱いはエンドポイントごとに違う（各節に記載） |

## 1. 時刻の扱い

- ファイル名の `YYYYMMDDHHMMSS` は **UTCとして解釈**して epoch 秒にする。例: `20260801000000` → `1785542400`。実在しない日時（2月30日・秒60など）と、JST（+9h）で 9999 年を超える日時のファイルは警告して取り込まない
  （SQLite の日付関数が扱えないため。以前の版が取り込んだそのような行は、カレンダーでは数えない）
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

**thumbs.db**（サムネの画像。index.db とは別ファイルで、`--rebuild` では消えない。WAL, `synchronous=NORMAL`）
- `thumbs(path TEXT PK, data BLOB, updated_at, sig)`: `path` は `messages.thumb` と同じキー（§3）。`data` は JPEG、`updated_at` は書き込んだ時刻（ns）、
  `sig` は作ったときの元ファイルの `{大きさ}:{更新日時ns}`（旧版から取り込んだものと 0.2.0 で作ったものは NULL。起動時に列が無ければ足す）
- `thumb_failures(path TEXT PK, sig)`: 作れなかったサムネと、そのときの元ファイルの `{大きさ}:{更新日時ns}`
- `meta(key, value)`: `legacy_thumbs_imported`（旧版のサムネフォルダを取り込み済み）
- 旧版（0.1.x）はサムネを `thumbs_dir` のフォルダに `{path}`（と作れなかった印 `{msg_id}.failed`）として置いていた。
  起動時と取り込みの開始時に、そのフォルダを thumbs.db へ取り込む（既にあるものは上書きしない。0バイトと `*.part.jpg` は飛ばす）。
  すべて読めたら `meta` に印を付け、以後はフォルダを見ない。**フォルダは消さない**（旧版に戻したときのため。不要なら利用者が消す）

**index.db と user.db は JOIN できない**ため、コメントは `(member,msg_id)` で突き合わせる（`OR` 連結のプレースホルダ）。
SQLite の変数上限（32766）に対し、最大件数は limit(≤500) なので収まる。

## 3. メッセージ共通オブジェクト

```json
{"member","group","msg_id","ts","ts_raw","body","kind","flag","missing","media_url","thumb_url","comment"}
```
- `group`: `group_` 列。`missing`: bool。`media_url`: `"/media/" + media` または null。`thumb_url`: `"/thumbs/" + thumb` または null（どちらも空文字は null）。
  `media`/`thumb` は `/` で区切った各部分を**パーセントエンコード**して連結する（英数字と `- . _ ~` 以外。日本語・空白・`#`・`%` も）。
  `/media`・`/thumbs` はエンコードを戻してからファイル・サムネを探すので、`#` や `%XX` を含む名前でもそのまま使える
- `comment`: `message_comments` 由来の文字列 / null。`/api/search` は追加で `snippet`、`/api/favorites` は追加で `created_at`
- サムネのキー（`messages.thumb`・thumbs.db の `path`）: `sha1(member の UTF-8)` の16進 `h` → `{h[:2]}/{h}/{msg_id}.jpg`

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
- `q` 必須（空文字は422。NUL は取り除いてから扱い、取り除いて空なら422）, `limit` 1–500, `from`/`to`/`member`/`before`/`before_rowid`。`from`/`to`/`member` の空文字は未指定扱い
- **語の分解**: 先頭から順に「`-`（任意）+ `"…"`（閉じ引用符まで。空白を含んでよい）」、無ければ「`-`（任意）+ 空白以外の連続」を1語として取る。
  空白は Unicode の空白。空の語は捨てる。先頭が `-` の語は除外語。`-` の直後が空白/末尾なら `-` 自体が含める語。
  閉じない `"` は通常の語の一部になる（例: `"ab cd` → `"ab`, `cd`）
- **FTS と LIKE の振り分け**: trigram は3文字未満を索引できないため、3文字以上（コードポイント数）の「含める語」が1つでもあれば FTS で候補を絞る。
  3文字以上の語（含める語・除外語）は FTS 式に入れ、2文字以下の語は下記「LIKE経路」と同じ条件（ASCII だけなら `LIKE`、それ以外は `instr(mv_fold(…))`）で追加の絞り込みにする（`used_fts` は true）。
  3文字以上の「含める語」が無ければ（除外語だけの場合も含む）全語を LIKE で扱う
- **FTS式**: 各語を `"…"`（`"`は`""`にエスケープ）で囲み ` AND ` 連結、除外は ` NOT "…"` を末尾に連結
- **LIKE経路**: `includes`/`excludes` がどちらも空なら元の `q` 全体を1語として扱う。
  ASCII だけの語は `body LIKE ? ESCAPE '\'`（`\ % _` をエスケープ。SQLite の LIKE は ASCII の大小を無視する）。
  ASCII 以外を含む語は `instr(mv_fold(body), mv_fold(語)) > 0`（除外は `= 0`）。`mv_fold` はスニペットと同じ畳み方（下記）をする SQL 関数で、
  FTS(trigram) と同じく英字以外の大文字小文字も無視する（語の長さで大小の扱いが変わらないように）。除外のみのときは `body IS NOT NULL AND body <> ''` を加える
- **並び**: `ORDER BY m.ts DESC, m.rowid DESC`
- **ページング**: `LIMIT limit+1` で `has_more` 判定。`before` + `before_rowid` があれば `(ts < b OR (ts = b AND rowid < r))`、`before` のみなら `ts < b`
- **応答**: `query, used_fts, terms{include,exclude}, total, has_more, next_before(=最後の結果のts), next_before_rowid(=最後の行のrowid), results`
  - `total` は初回(`before` 無し)のみ件数、`before` 指定時は null。0件時の `next_before*` は null
  - SQLiteの構文エラー（SQLITE_ERROR）は 400 `search query error: …`。それ以外の SQLite のエラー（I/O・破損・ビジーなど）は 500
- **スニペット**（FTS/LIKE共通の自前実装。SQLiteの `snippet()` は使わない）
  - 定数: `SNIPPET_CONTEXT=40`, `SNIPPET_MAX=300`, 省略記号 `...`
  - 最初のヒット位置 = 含める語の本文中(大文字小文字を区別しない比較)で最小の位置。無ければ先頭 `SNIPPET_MAX` 文字
  - 切り出し `[max(0,hit-40), +300)`。マーク範囲は長い語優先・重なり無しで前から決め、**`<mark>`以外は HTML エスケープ**（`&amp; &lt; &gt; &quot; &#x27;`）
  - 先頭が切れたら `...` を前置、末尾が切れたら後置
  - **位置は文字(コードポイント)単位**。Rust では `Vec<char>` で扱い、バイト位置と混同しない
  - 大文字小文字を区別しない比較は、1文字を1文字へ畳んで行う（小文字化して複数文字になるものは先頭の1文字。`İ`→`i`。語末シグマ `ς` は `σ` にそろえる）。
    畳んでも文字数が変わらないので、畳んだ文字列での位置がそのまま元の本文の位置になる

### GET /api/jump
`date` 必須(JST日付)。`ts >= 日付開始` の最小 ts。無ければ `ts: null`。応答 `{member,date,ts}`。

### GET /api/calendar
- `yearmonth` 無し: `{member, calendar:[{yearmonth,count}]}`（昇順）
- `yearmonth=YYYY-MM`: `{member, yearmonth, days:[{date,count}]}`（昇順）。形式は数字4桁-2桁の固定（日付と違い `2026-8` は不可。空文字も400）。
  形式違反は400、実在しない月（`2026-13`）は0件。年 `0000` は日付と違い受け付ける（該当は0件）

### GET /api/media-list
`member`必須, `kind` ∈ image/video/audio（他は空文字も含め400。省略時は `kind != 'text'`）, `limit` 1–200（既定60）, `before`(ts), `before_id`, `from`/`to`（空文字は未指定扱い、不正な日付は400）。
常に `missing=0`。並びは `ts DESC, msg_id DESC`。`before_id`（前ページ最後の msg_id）を添えると `ts < b OR (ts = b AND msg_id < id)`。
`LIMIT limit+1` で `has_more`。応答 `{items, has_more}`。

### お気に入り
- `POST /api/favorites` body `{member,msg_id}`: 空/`msg_id`無しは400、型が違えば400（下記「ボディの解釈」）。
  メッセージ不在は404。`INSERT OR IGNORE`、`created_at`=現在秒。応答 `{"status":"ok","favorited":true}`
- `DELETE /api/favorites?member&msg_id` → `{"status":"ok","favorited":false}`（存在しなくても200）
- `GET /api/favorites?member?&before?&before_rowid?&limit(1–200,既定50)` → `{"favorites":[…], "next_before", "next_before_rowid", "has_more"}`。
  並びは `created_at DESC, rowid DESC`。`next_before` / `next_before_rowid` はこのページで読んだ最後の行（0件なら null）で、
  次ページの `before` / `before_rowid` に渡すと `(created_at, rowid)` を目印に続きを返す。
  `member` は「指定があるか」で判定する（空文字も指定扱い）。index.db 側にメッセージが無い行は**黙って除外**（幽霊お気に入り）。
  そのため返す件数が `limit` 未満でも続きがありうる。続きの有無は `has_more`（`limit` を超える行が残っているか。幽霊も数える）で判断する
- `GET /api/favorites/ids?member` → `{"msg_ids":[…]}`（順序は保証しない）

### PUT /api/comments
body `{member,msg_id,comment}`。`comment` の空文字/省略/null は**削除**。長さ > **1000 文字**(コードポイント)は400（存在確認より先に判定）。
メッセージ不在は404。応答 `{"status":"ok","comment": 文字列|null}`。UPSERT で `updated_at` 更新。

### ボディの解釈（お気に入り・コメント・非表示・並び順・設定）
値は型どおりに受け取る。型が違えば 400（`{"detail": "<項目> は…で指定してください"}`。`members` が配列でない・無いときは `"members は配列である必要があります"`）。
- `comment`: 文字列。前後の Unicode の空白（全角空白を含む）を除いてから使う。長さはコードポイント数
- `member`・`members` の要素: 文字列。フォルダ名そのものなので**前後の空白を削らない**（全角空白で始まるフォルダ名もある）。空白だけの `member` は未指定扱い、`members` の要素なら400
- `msg_id`: JSON の整数。数字の文字列（`"3"`）・小数・真偽値は不可
- `hidden`: 真偽値。省略・null は false
- JSON オブジェクト以外（不正JSON・空・配列）は 422

### 非表示メンバー / 並び順
- `GET /api/hidden-members` → `{"members":[…]}`（名前昇順）。`PUT` body `{member,hidden}`: `member` 空は400。
  応答 `{"status":"ok","member","hidden"}`
- `GET /api/member-order` → `{"members":[…]}`（`position, member` 順）。`PUT` body `{members:[…]}`: 配列以外/文字列以外の要素/空要素/重複は400。
  **全置換**（DELETE→INSERT）。応答 `{"status":"ok","count"}`

### GET /media/{path}, /thumbs/{path}
- `/media` は `root` 未設定で **409** `root is not configured`。
  `/thumbs` は thumbs.db の `path` で引く（ファイルシステムには触れない）。下記の403になる形と、`..` を含む `path` は **403 `invalid path`**、
  無ければ **404 `file not found`**。`Content-Type` は常に `image/jpeg`。以下のパス解決は `/media` だけ
- パス解決: `path` が絶対パス・ドライブ指定・UNC（`\\host\share`）、または（Windows で）`:` を含むなら、ファイルシステムに触れずに **403 `invalid path`**
  （`base.join` で `base` ごと置き換わり、確認の前に外部ホストへ SMB 接続してしまうため）。それ以外は `base.join(path)` を正規化し、`base` の外に出れば **403 `invalid path`**（`%2e%2e`/`..%2f` も正規化後に判定）。
  存在する最長の祖先まではシンボリックリンクを解決し、残りは字句的に連結する。存在しない場所への `..` 脱出も、存在チェックより先に403。
  存在しない/ディレクトリは **404 `file not found`**。
  検証してから開くまでの間にリンクへ差し替えられた場合に備え、開いた後でもう一度、開いたファイルの実際のパス
  （Windows は `GetFinalPathNameByHandleW`、Linux は `/proc/self/fd`）が `base` の下にあるかを確かめ、外なら **403**
- MIME（拡張子、小文字化）: `.jpg/.jpeg`→`image/jpeg`, `.png`→`image/png`, `.mp4`→`video/mp4`, `.txt`→`text/plain; charset=utf-8`, **それ以外は `application/octet-stream`**（`.m4a` 等も）
- 200/206 には常に `Accept-Ranges: bytes`, `Content-Length`, `Content-Type`。`Range` 無し→200全体
- **キャッシュ**: 200/206/304 に `ETag`（`"サイズ16進-更新時刻ns16進"`。`/thumbs` の更新時刻は `updated_at`）と `Last-Modified`、`Cache-Control` を付ける（304 はこの3つだけで本文なし）。
  更新時刻が取れないファイルは `Last-Modified` を付けず、`ETag` の時刻部は `0`。
  `Cache-Control` は `/thumbs` が `max-age=86400`（パスが (member, msg_id) で決まり中身はまず変わらない）、`/media` が `no-cache`（毎回検証）。
  `If-None-Match` が一致（`*`・弱い比較可）すれば **304**。`If-None-Match` が無いときだけ `If-Modified-Since` を見る（送り返された日付との一致のみ）。
  `If-Range` が今の `ETag` / `Last-Modified` と違えば `Range` を無視して200全体
- **Range**（`bytes=(\d*)-(\d*)` を前方一致、**単一範囲のみ**）:
  - 416 の本文は `{"detail":"invalid range header"}`（形式違反）/ `{"detail":"range not satisfiable"}`（範囲外）。`Accept-Ranges` は付かない
  - 不一致 / 両方空 → 416
  - `-N`（start空）→ 末尾N（`N`がサイズ超なら全体 `0..size-1`。`-0` は416）
  - `N-` → `N..size-1`。`end` は `size-1` に丸める
  - `start > end` または `start >= size` → **416** + `Content-Range: bytes */{size}`
  - 成功は **206** + `Content-Range: bytes s-e/size`。ストリーミングで返す（動画を丸ごとメモリに載せない）
  - 前方一致なので `bytes=0-4,10-12`（複数範囲）は先頭のみ、`bytes=2-3xyz` も 2-3 として扱う。
    `bytes=` は大文字小文字を区別（`BYTES=` は416）。巨大な数値は飽和させて同じ判定にする。0バイトのファイルは全体取得200、Range指定は416
  - `tower-http::ServeFile` は 416 の本文・`-N` の超過・不正ヘッダの扱いがこの仕様と異なるため、自前で実装している

### GET /api/media-path
`member`,`msg_id` 必須。行が無い/`media`が空は404。`root` 未設定は409（404 の判定が先）。`{"path": 絶対パス文字列, "exists": bool}`。
パス検証は `/media` と同じ（絶対パス・UNC・`:` を含む・`root` の外は 403 `invalid path`）だが、実在チェックは無い。
返すパスはシンボリックリンク・ジャンクションを解決したもの（ネットワークドライブなら UNC 形式になりうる）。Windows では `\` 区切りのまま返り、`\\?\` 接頭辞は付けない。

### GET/POST /api/settings
- GET: `{"root": 文字列|null, "configured": bool, "is_dev": bool, "ffmpeg_available": bool, "ffmpeg_path": 文字列|null, "root_env_override": bool}`
- POST body `{root}`: 前後の空白を除いて 空→400、**絶対パスでない→400**、存在しない/ディレクトリでない→400。
  成功時 設定ファイル（`config-dev.json` 使用中はそちら）の `root` を、前後の空白を除いた入力のまま書き換え（他のキーと順序は保つ。2スペースのインデント + 末尾改行。書けなければ500）、
  差分取り込みを**バックグラウンドで開始**する。
  自/他プロセスの取り込み・メンバー削除・初期化の最中は**保存せずに** 409 `{"status":"already_running"[, "detail":"index process pid=N is running"]}`
  （初期化と同時に保存すると root が消えたり設定ファイルとメモリが食い違ったりし、取り込み中に変えると実行中の取り込みが古い root のまま走るため）。
  保存の後で取り込みを始められなかったとき（その間に別の取り込みが始まった）は 200 のまま `index_started:false`。
  応答 `{"root": 字句的に正規化した絶対パス, "configured": true, "index_started": bool, "root_env_override": bool}`
- 環境変数 `MSGVIEWER_ROOT` が設定されていると、次の起動では書き戻した値より環境変数が優先される（6章）。このとき GET・POST・初期化の応答の
  `root_env_override` が true になり、画面で知らせる

### POST /api/reindex?mode&member
- `root` 未設定→409 `{"status":"root_not_configured"}`。`mode` ∈ incremental(既定)/full 以外→400。`member` の空文字・空白だけは未指定（全メンバー）扱い
- 実行中(自プロセス or 別プロセスが `.index.lock` を握っている)、またはメンバー削除・初期化の最中→409 `{"status":"already_running"[, "detail":"index process pid=N is running"]}`
- 成功: `{"status":"started","mode","member"}`（`member` 未指定は null）

### GET /api/reindex/status
`{running, started_at, finished_at, progress, last_summary, last_error}`
- 自プロセスが実行中→メモリ上の状態をそのまま返す。このときの `progress` は取り込み処理が渡す
  `{"stage": "scanning"|"scanned"|"upserting"|"missing_check"|"thumbnails"|"thumbnailing", done?, total?, files?, messages?}`（`phase` 形式ではない）
- そうでなければ `data/progress.json` を読む（無い/壊れていればメモリ上の状態を返す）。
  `progress.json` が `running:true` なのに `pid` が死んでいれば（PID の生存だけで判定し、ロックは見ない）
  `progress = {source:"file", phase:"interrupted", done, total, percent, pid}` と
  `last_error:"index process (pid=N) is not running (interrupted)"` を返す（`last_summary` はメモリ上の値）
- それ以外は `progress = progress.json + {"source":"file"}`、`finished_at = running ? null : updated_at`、
  `last_summary` は `phase=="done"` のとき `progress.json` の `summary`、`last_error` は `phase=="error"` のとき `message`。
  該当しなければ null ではなくメモリ上の値（自プロセスの前回の結果）を返す
- `last_summary` のキー: `new_count, updated_count, skipped_count, missing_count, thumb_count, elapsed_sec, total_files_scanned, total_messages_scanned`
- `progress.json` のキー: `running, phase, done, total, percent, elapsed_sec, eta_sec, started_at, updated_at, pid, mode, message, summary`
- 取り込みが想定外のエラー（panic）で止まった場合も、`last_error` に記録して実行中の状態を解く
  （通常は `"internal error: <panic内容>"`。それもすり抜けた場合は `"取り込みが内部エラーで中断されました"`）

### POST /api/reset
実行中(自/他プロセス)→409 `{"status":"already_running"}`。
対象: index.db・user.db・thumbs.db（各 `-wal`/`-shm` も）・`progress.json`・旧版のサムネフォルダ（`thumbs_dir`。`data_dir` の下にあるときだけ。
外を指していると無関係なファイルを消しうるため）、設定ファイルの `root`、メモリ上の取り込み状態。応答 `{"status":"reset", "root_env_override": bool}`。
**全部消えるか、何も変わらないかのどちらか**になるよう、2段階で行う。
1. 対象のうち実在するものを、すべて `<名前>.resetting` に名前を変える（Windows で一時的に開かれている場合に備えて各最大3秒再試行）。
   続けて空の index.db・thumbs.db のスキーマを作り、設定ファイルの `root` を `""` にする（一時ファイル経由で置き換え）。
   どこかで失敗したら、作りかけを消して名前を元に戻し、500 `{"detail":"初期化できませんでした(何も変更していません): …"}`。
   Windows では開かれているファイル（を含むフォルダ）は名前も変えられないので、何かを消す前に失敗が分かる
   （例: エクスプローラーのプレビューが旧版のサムネフォルダの画像を開いている）。閉じてからやり直せばよい
2. `*.resetting` を消す。消し損ねても初期化は済んでいて、残りは次の初期化の前と、サーバの起動時に消す。user.db は次の利用時に空のスキーマで開き直される

**Windowsでは開いたままのハンドルがあると名前の変更・削除に失敗する**ため、index.db の利用をゲート（RwLock）で囲み、
段階1の間は書き込み側を握って新しい接続を作らせない。thumbs.db も同じ。user.db も同様にロックを握ったまま行う。
メンバー削除と同じく、処理の間は `.index.lock` を握る（その間に CLI の取り込みも始まらない。`.index.lock` 自体は対象外）。
別プロセスが握っていれば 409 `{"status":"already_running","detail":"index process pid=N is running"}`（PID が分からなければ `detail` なし）。
ロックファイルを開けない・ロック操作自体が失敗したなど、競合以外でロックを取れなかった場合は 500（409 にすると原因に気付けない）。

### DELETE /api/members/{member}
reset同様に実行中は409、処理の間は `.index.lock` を握る。`messages` と検索索引の行、該当サムネを削除し、`favorites`/`message_comments` から該当メンバーを削除。
サムネは thumbs.db から、キーがメンバーの `{h[:2]}/{h}/` で始まるものを作れなかった印ごと消す（`GLOB`。消した領域はファイルを縮めずに再利用される）。
3つの DB は1つのトランザクションにできないため、user.db（お気に入り・コメントを1トランザクション）→ index.db（メッセージ・検索索引を1トランザクション）→
thumbs.db（サムネ・印を1トランザクション）の順に消す。途中で失敗すると500だが、メッセージが残っている限りメンバーは一覧に出たままなので、もう一度削除すれば最後まで消せる。
応答 `{"status":"ok","member","deleted_messages": 件数}`。存在しないメンバーでも 200（0件）。**root側のファイルは触らない**。
表示設定（`hidden_members`/`member_order`）は消さない。
検索索引の行を残すと、SQLite が削除後に最大 rowid を再利用したときに、再取り込みで rowid が衝突したり、
削除済みの本文で無関係なメッセージがヒットしたりするため、同じトランザクションで消す。

## 5. インデクサ

- ファイル名 `^(\d+)_(\d)_(\d{14})(_.+)?\.(\w+)$`（`(_.+)?` は colmsg v5 以降の投稿者名で、読み捨てる）。flag 0=text 1=image 2=video 3=audio（4〜9 も受け付け、`kind` は text）。`msg_id` は int 化（ゼロ埋めの衝突は警告のみ。i64 に収まらないものは警告して飛ばす）
- 同一 `(member,msg_id)` の `.txt` が本文、非txtがメディア（複数あれば先頭を採用）。メディアがあればそのflag、無ければtxtのflag
- **本文**: 改行を `\n` に揃える（`\r\n`・`\r` → `\n`）。UTF-8 として不正なら本文なし（警告のみ）。BOM は残る
- 取り込み結果の判定:
  - 既存行(`member,msg_id`)が無ければ `new`（INSERT。`ON CONFLICT DO UPDATE` で冪等化、`thumb` は `COALESCE` で既存値を保護）
  - あれば `group_/body/media/kind/flag/ts_raw/missing(≠0)/thumb` のいずれかが変わったときだけ `UPDATE`+検索索引の差し替えで `updated`、変化なしは `skipped`
  - 本文ファイルを読めなかった（別プロセスがロック中など）ときは、保存済みの `body` をそのまま使う（一時的な失敗で本文と検索索引を消さない）。UTF-8 として不正なときは `null`
  - 検索索引の行は `body` が空でないときだけ作る。rowid は `messages` の rowid に合わせ（検索の JOIN が `rowid` 結合のため）、差し替えは rowid で消して入れ直す
- `missing` 判定: 今回の走査範囲に含まれる既存行のうち、今回見つからなかったものを `missing=1`、
  再発見されたら `0` に戻す。範囲判定では、**`ts + 9h` の JST の年月**と**ファイル名の日時（UTC）の年月**の両方が範囲内の行だけを対象にする
  （月フォルダが UTC と JST のどちらで分かれているか分からないため。片方だけだと、月初 0〜9 時（JST）の行を走査していないのに `missing=1` にしてしまう）。
  見つかった行は取り込み時に `missing=0` に戻すので、範囲外として飛ばしても戻し漏れは起きない。
  走査範囲は、走査したメンバーごとに、`full`（とまだ取り込んだことの無いメンバー）なら全期間、`incremental` なら直近月のうち
  **実在した月フォルダ、または直下ファイルがあった年月だけ**。そのため差分取り込みでは、月フォルダごと消えた月は判定されない。
  また、フォルダが無くなったメンバーは走査対象にならないので、全期間の取り込みでも `missing=1` にならない。
  **読めなかったフォルダ**（ネットワークドライブの瞬断・権限・同期ソフトのロックなど）の範囲も判定しない: メンバーフォルダが読めなければ
  そのメンバーは判定せず、年・月フォルダや項目の一部が読めなければ、読めた月だけを判定する（`full` でも全期間扱いにしない）
- 走査パターン4種（グループ有無 × `{YYYY}/{YYYYMM}/`有無）。`incremental` は直近 `incremental_months` ヶ月（ローカル時刻基準）のフォルダのみ。
  まだ取り込んだことの無いメンバーは全期間を走査する
- **走査順**: 取り込み順が rowid を決め、同じ秒のメッセージの検索順に効く。ディレクトリと直下ファイルは名前順
  （Windows は大文字小文字を区別しない）、月ディレクトリ内のファイルは OS が返す順のまま
- **サムネ**: 対象は拡張子 `jpg` のファイル（`png` 等は対象外）。無ければ ffmpeg があるときだけ `flag==2 && ext==mp4` の動画。
  - 画像は EXIF の向きを反映してから、長辺 `thumb_size` に収める（拡大しない。端数は縦横比の誤差が小さい方）。JPEG品質 `thumb_quality`
  - 動画は ffmpeg で1フレーム抜き出す（1秒地点、取れなければ先頭。1回30秒でタイムアウトするので1件で最大60秒）。
    画像と同じく長辺を `thumb_size` に収め、拡大はしない（`scale=w='min(iw,S)':h='min(ih,S)':force_original_aspect_ratio=decrease`）。
    品質は `thumb_quality` を `-q:v`（`2 + (100 - thumb_quality) / 5`、2〜31）に換算する
  - 動画の JPEG は一時ファイルを作らず、ffmpeg の標準出力（`-f image2pipe -c:v mjpeg -`）から受け取る
  - 生成は CPU のスレッド数ぶん並列に行い、thumbs.db への書き込みは1本のスレッドで、届いている分（最大200件）を1トランザクションにまとめて書く。
    thumbs.db に既にあれば作り直さない。ただし `sig` が今の元ファイルと食い違えば（元ファイルが差し替えられた）作り直す
  - `messages.thumb` は、サムネを thumbs.db に保存できてから設定する（書き込みスレッドが thumbs.db を commit するたびに、
    その分の `thumb` を index.db に書く）。作る前は空にしておくので、途中で止まっても存在しないサムネを指したまま残らない
  - 作れなかったら `thumb_failures` に元ファイルの大きさと更新日時を残す（`thumb` は空のまま）。差分取り込みでは、元ファイルが
    変わっていなければ作り直さない（壊れた画像・動画を毎回試さない）。全期間の取り込みでは作り直しを試す。作れたら印は消す
  - thumbs.db に書き込めなくなったら（ディスクがいっぱいなど）、残りは作らずに取り込みをエラーで終える
  - 1枚の失敗で取り込み全体は止めない
- **ffmpeg の探索順**: 設定ファイルのディレクトリ → 実行ファイルのディレクトリ（それぞれ `bin/` → 直下）→ PATH
- 排他: `data/.index.lock` の OS ファイルロック（`File::try_lock`）。保持者が異常終了しても OS が解放するので、PID の再利用や空ファイルで誤判定しない。
  ロックファイル自体は消さない。保持者の PID と取得時刻は表示用に `data/.index.pid`（1行目 PID、2行目 取得時刻）へ書き、解放時に消す
  （Windows ではロック中のファイルを他プロセスが読めないため別ファイル）。UI の事前確認は PID が生存しているときだけ実際のロックで確かめる。
  進捗は `data/progress.json` を一時ファイル+rename で原子的に書く。進捗ファイルを書き終えてからロックを解放する
- CLI（[docs/cli.md](cli.md)）: `message-viewer [--config <path>] index [--full|--incremental|--rebuild] [--only-member X] [--no-thumbs]`。
  終了コード: 0 成功 / 1 失敗 / 2 ロックを取れなかった（実行中、またはロックファイルを開けない）。
  `--rebuild` は index.db（+`-wal`/`-shm`）だけを消す（user.db・thumbs.db は残る）。`--only-member` との併用と、サーバ起動中
  （設定のポートが使用中。サーバが index.db を開いたままのため）は、何もせずに終了コード 1

## 6. 設定

- 読み込み: `--config <path>`。無ければ実行ファイルと同じディレクトリの `config-dev.json` → `config.json`。先頭の BOM は無視する
- `root` の書き戻しは一時ファイル（`<設定ファイル>.tmp`）に書いてから置き換える（途中で落ちても設定ファイルが壊れない）
- 環境変数 `MSGVIEWER_ROOT/DATA_DIR/DB_PATH/THUMBS_DIR/PORT` が**空でなければ**優先
- 相対パスは設定ファイルのディレクトリ基準（字句的に正規化。存在しないパスでもよい）。`data_dir`/`db_path` の親は起動時に作成。
  user.db・thumbs.db は `data_dir` の下
- キー: `root, data_dir, thumbs_dir, db_path, port, thumb_size, thumb_quality, incremental_months, auto_index_on_startup`。
  `root` と `thumbs_dir` 以外は**必須**（1つでも欠けると起動できない）。`thumbs_dir` は旧版のサムネフォルダ（取り込みの移行元。既定 `./data/thumbs`）で、作成はしない。設定ファイル自体が無い場合も起動できない（終了コード1）
- `auto_index_on_startup` が true かつ `root` 設定済みなら、起動時に差分取り込みをバックグラウンドで開始
- `static/` は設定ファイルのディレクトリを優先し、無ければ実行ファイルの隣を使う

## 7. サーバの起動

- 設定を読み（`data_dir` などの親ディレクトリはここで作る）、次にポートを確保する。使用中なら二重起動とみなし、案内を出して終了コード1で終わる（DB もログファイルも開かない）。
  このとき、ダブルクリックで起動していれば、既に動いているサーバの `http://localhost:{port}/` をブラウザで開く（通常の起動ではブラウザを開かない）
- ログは stderr と `data_dir/server_log.txt`（追記。起動ごとに `========== 日時 起動 ==========`）の両方に出す。各行の先頭に日時（ローカル時刻）。
  ファイルに書けなければ警告して stderr だけに出す
- ダブルクリックで起動した場合（コンソールに付いているプロセスが自分だけ）、起動失敗・異常終了のときは Enter を押すまでウィンドウを閉じない
