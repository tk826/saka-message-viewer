//! SQLite接続。index.db(メッセージ)・thumbs.db(サムネ)・user.db(お気に入り等)を別ファイルで扱う。
//!
//! - index.db / thumbs.db: 接続プール。WAL では複数リーダ同時実行できるため、1本の共有接続で直列化しない。
//!   取り込み(別プロセス)が長い書き込みトランザクション中でも、無関係なリクエストを巻き込まない。
//!   `/api/reset` が DB ファイルを消す間は、ゲート(RwLock)の書き込み側を握って新しい接続を作らせない。
//! - user.db: 書き込みが主で同時アクセスが少ないため、単一接続+Mutex。

use crate::error::{blocking, ApiResult};
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OpenFlags, ToSql};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

pub const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS messages (
  member   TEXT NOT NULL,
  group_   TEXT,
  msg_id   INTEGER NOT NULL,
  ts       INTEGER NOT NULL,
  ts_raw   TEXT NOT NULL,
  body     TEXT,
  media    TEXT,
  kind     TEXT NOT NULL,
  flag     INTEGER NOT NULL,
  thumb    TEXT,
  missing  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (member, msg_id)
);
CREATE INDEX IF NOT EXISTS idx_member_ts ON messages(member, ts);
CREATE INDEX IF NOT EXISTS idx_ts ON messages(ts);
CREATE INDEX IF NOT EXISTS idx_member_group_ts ON messages(member, group_, ts);
";

/// 検索索引。`messages_fts.rowid` は `messages` の暗黙の rowid と同じ値にしてつなぐ。
///
/// 注意: `messages` には `INTEGER PRIMARY KEY` が無いため、VACUUM すると rowid が振り直されることがあり、
/// 索引が別のメッセージを指すようになる。アプリは VACUUM しない。手動で VACUUM したら、
/// `message-viewer index --rebuild` で作り直すこと。
pub const FTS_SQL: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  body, member UNINDEXED, msg_id UNINDEXED, tokenize='trigram'
);
";

pub const USER_DB_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS favorites (
  member     TEXT NOT NULL,
  msg_id     INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (member, msg_id)
);
CREATE INDEX IF NOT EXISTS idx_fav_created ON favorites(created_at DESC);
CREATE TABLE IF NOT EXISTS message_comments (
  member     TEXT NOT NULL,
  msg_id     INTEGER NOT NULL,
  comment    TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (member, msg_id)
);
CREATE TABLE IF NOT EXISTS hidden_members (
  member     TEXT PRIMARY KEY,
  hidden_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS member_order (
  member     TEXT PRIMARY KEY,
  position   INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
";

/// サムネの画像(JPEG)を入れる DB。index.db と分けるのは、`--rebuild` で index.db を作り直しても
/// サムネを作り直さずに済むようにするため(画像の分だけ大きく、作り直しに時間もかかる)。
///
/// - `thumbs.path`: `messages.thumb` と同じ相対パス(`{sha1(member)[:2]}/{sha1(member)}/{msg_id}.jpg`)。
///   `/thumbs/{path}` の `{path}` もこれ。`updated_at` は書き込んだ時刻(ns)で、ETag・Last-Modified に使う。
///   `sig` は作ったときの元ファイルの大きさ・更新日時(`{len}:{mtime_ns}`)で、元ファイルが差し替えられたら作り直す
///   (旧版から取り込んだサムネや 0.2.0 で作ったサムネは NULL)
/// - `thumb_failures`: 作れなかったサムネと、そのときの元ファイルの大きさ・更新日時(`{len}:{mtime_ns}`)。
///   同じ元ファイルから差分取り込みのたびに作り直そうとしないため
/// - `meta`: 旧版のサムネフォルダを取り込み済みかなど
pub const THUMBS_DB_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS thumbs (
  path       TEXT PRIMARY KEY,
  data       BLOB NOT NULL,
  updated_at INTEGER NOT NULL,
  sig        TEXT
);
CREATE TABLE IF NOT EXISTS thumb_failures (
  path TEXT PRIMARY KEY,
  sig  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
";

/// 共通のPRAGMA。ロック競合時はすぐ失敗せず30秒までリトライ待ちする(database is locked 対策)。
/// busy_timeout は最初に設定する(journal_mode の切り替え・WAL の復旧と重なったときも待つように)。
fn apply_pragmas(conn: &Connection, synchronous_normal: bool) -> rusqlite::Result<()> {
    conn.busy_timeout(std::time::Duration::from_millis(30_000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    if synchronous_normal {
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    Ok(())
}

/// テーブルに列があるか。
fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for n in names {
        if n? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 0.2.0 の thumbs.db(`thumbs.sig` 列なし)に列を足す。
/// 別のプロセス(CLI の取り込みとサーバ)が同時に足そうとして失敗しても、列ができていればよい。
fn migrate_thumbs_db(conn: &Connection) -> rusqlite::Result<()> {
    if has_column(conn, "thumbs", "sig")? {
        return Ok(());
    }
    match conn.execute_batch("ALTER TABLE thumbs ADD COLUMN sig TEXT") {
        Ok(()) => Ok(()),
        Err(e) => {
            if has_column(conn, "thumbs", "sig")? {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// index.db を開く。`ensure_schema` ならテーブル・インデックス・FTSを作る(起動時に1回だけ行う)。
pub fn open_index_db(path: &Path, ensure_schema: bool) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    apply_pragmas(&conn, true)?;
    // 検索の短い語(LIKE 経路)で、英字以外も大文字小文字を区別せずに照合するための関数。
    // 畳み方はスニペットの強調と同じ(`search::fold_text`)
    conn.create_scalar_function(
        "mv_fold",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<Option<String>>(0)?.map(|s| crate::search::fold_text(&s))),
    )?;
    if ensure_schema {
        conn.execute_batch(SCHEMA_SQL)?;
        conn.execute_batch(FTS_SQL)?;
    }
    Ok(conn)
}

/// thumbs.db を開く。`ensure_schema` ならテーブルを作る。
pub fn open_thumbs_db(path: &Path, ensure_schema: bool) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    apply_pragmas(&conn, true)?;
    if ensure_schema {
        conn.execute_batch(THUMBS_DB_SCHEMA_SQL)?;
        migrate_thumbs_db(&conn)?;
    }
    Ok(conn)
}

/// 旧バージョンの user.db(favorites.note 列あり)から、note を message_comments へ移す。
/// note 列自体は残す(以後どのコードも参照しない)。
fn migrate_user_db(conn: &Connection) -> rusqlite::Result<()> {
    let has_note = {
        let mut stmt = conn.prepare("PRAGMA table_info(favorites)")?;
        let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
        let mut found = false;
        for n in names {
            if n? == "note" {
                found = true;
            }
        }
        found
    };
    if has_note {
        conn.execute_batch(
            "INSERT OR IGNORE INTO message_comments (member, msg_id, comment, updated_at)
             SELECT member, msg_id, note, created_at FROM favorites
             WHERE note IS NOT NULL AND note != ''",
        )?;
    }
    Ok(())
}

pub fn open_user_db(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    apply_pragmas(&conn, false)?;
    conn.execute_batch(USER_DB_SCHEMA_SQL)?;
    migrate_user_db(&conn)?;
    Ok(conn)
}

/// DB を開く関数(`open_index_db` / `open_thumbs_db`)。第2引数はスキーマを作るか。
type Opener = fn(&Path, bool) -> rusqlite::Result<Connection>;

pub struct Db {
    path: PathBuf,
    open: Opener,
    pool: Mutex<Vec<Connection>>,
    /// 通常の利用は読み取り側、`/api/reset` のファイル削除は書き込み側で握る。
    /// 書き込み側を握っている間は、使用中の接続が無く、新しい接続も作られない。
    gate: RwLock<()>,
}

impl Db {
    /// index.db。
    pub fn new(path: PathBuf) -> rusqlite::Result<Arc<Self>> {
        Self::with_opener(path, open_index_db)
    }

    /// thumbs.db。
    pub fn new_thumbs(path: PathBuf) -> rusqlite::Result<Arc<Self>> {
        Self::with_opener(path, open_thumbs_db)
    }

    fn with_opener(path: PathBuf, open: Opener) -> rusqlite::Result<Arc<Self>> {
        // 起動時に一度だけスキーマを保証する。以降の接続は作成処理を省く
        // (CREATE ... IF NOT EXISTS でも書き込みロックを一瞬取るため、取り込み中のブロック起点になる)。
        open(&path, true)?;
        Ok(Arc::new(Db { path, open, pool: Mutex::new(Vec::new()), gate: RwLock::new(()) }))
    }

    /// プールから接続を借りて `f` をブロッキングスレッドで実行する。
    pub async fn run<T, F>(self: &Arc<Self>, f: F) -> ApiResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let this = Arc::clone(self);
        let res = blocking(move || -> rusqlite::Result<T> {
            let _gate = this.gate.read().unwrap();
            let conn = match this.pool.lock().unwrap().pop() {
                Some(c) => c,
                None => (this.open)(&this.path, false)?,
            };
            let out = f(&conn);
            this.pool.lock().unwrap().push(conn);
            out
        })
        .await?;
        Ok(res?)
    }

    /// 使用中の接続が無くなるのを待ってから全接続を閉じ、その状態のまま `f` を実行する
    /// (`/api/reset` で DB ファイルを消して作り直すため。開いたままだと Windows では削除できない)。
    pub fn while_closed<T>(&self, f: impl FnOnce() -> T) -> T {
        let _gate = self.gate.write().unwrap();
        self.pool.lock().unwrap().clear();
        f()
    }
}

pub struct UserDb {
    conn: Mutex<Option<Connection>>,
    path: PathBuf,
}

impl UserDb {
    pub fn new(path: PathBuf) -> rusqlite::Result<Arc<Self>> {
        let conn = open_user_db(&path)?;
        Ok(Arc::new(UserDb { conn: Mutex::new(Some(conn)), path }))
    }

    pub async fn run<T, F>(self: &Arc<Self>, f: F) -> ApiResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let this = Arc::clone(self);
        let res = blocking(move || -> rusqlite::Result<T> {
            let mut guard = this.conn.lock().unwrap();
            if guard.is_none() {
                *guard = Some(open_user_db(&this.path)?);
            }
            f(guard.as_ref().unwrap())
        })
        .await?;
        Ok(res?)
    }

    /// 接続を閉じ、ロックを握ったまま `f` を実行する(`/api/reset` で user.db のファイルを消すため)。
    /// 次の `run` で開き直す。
    pub fn while_closed<T>(&self, f: impl FnOnce() -> T) -> T {
        let mut guard = self.conn.lock().unwrap();
        *guard = None;
        f()
    }
}

/// (member, msg_id) の組のどれかに一致する条件 `(member=? AND msg_id=?) OR ...` と、そのパラメータ。
/// index.db と user.db は別ファイルで JOIN できないため、片方で得た組でもう片方を引くときに使う。
pub fn pairs_condition(pairs: &[(String, i64)]) -> (String, Vec<&dyn ToSql>) {
    let cond = vec!["(member=? AND msg_id=?)"; pairs.len()].join(" OR ");
    let params = pairs.iter().flat_map(|(m, i)| [m as &dyn ToSql, i as &dyn ToSql]).collect();
    (cond, params)
}

/// (member, msg_id) の組に対応するコメントをまとめて引く。
/// index.db と user.db は別ファイルでSQLのJOINができないため、ここで突き合わせる。
pub async fn fetch_comments_map(
    user_db: &Arc<UserDb>,
    pairs: Vec<(String, i64)>,
) -> ApiResult<HashMap<(String, i64), String>> {
    if pairs.is_empty() {
        return Ok(HashMap::new());
    }
    user_db
        .run(move |c| {
            let (cond, params) = pairs_condition(&pairs);
            let sql = format!("SELECT member, msg_id, comment FROM message_comments WHERE {cond}");
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), |r| {
                Ok(((r.get::<_, String>(0)?, r.get::<_, i64>(1)?), r.get::<_, String>(2)?))
            })?;
            rows.collect::<rusqlite::Result<HashMap<_, _>>>()
        })
        .await
}

/// 検索索引 messages_fts のうち、対応する messages 行が無い(孤児)ものを消す。消した件数を返す。
///
/// 孤児の rowid が
/// 後で再利用されると、取り込みが rowid 衝突で失敗したり、削除済みの本文で無関係なメッセージが
/// ヒットしたりするため、起動時に掃除する(古い版のメンバー削除が索引行を残していた)。
pub fn clean_orphan_fts_rows(path: &Path) -> rusqlite::Result<usize> {
    let conn = open_index_db(path, false)?;
    conn.execute("DELETE FROM messages_fts WHERE rowid NOT IN (SELECT rowid FROM messages)", [])
}
