//! SQLite接続。index.db(メッセージ)と user.db(お気に入り等)を別ファイルで扱う。
//!
//! - index.db: 接続プール。WAL では複数リーダ同時実行できるため、1本の共有接続で直列化しない。
//!   取り込み(別プロセス)が長い書き込みトランザクション中でも、無関係なリクエストを巻き込まない。
//!   `/api/reset` が DB ファイルを消す間は、ゲート(RwLock)の書き込み側を握って新しい接続を作らせない。
//! - user.db: 書き込みが主で同時アクセスが少ないため、単一接続+Mutex。

use crate::error::{blocking, ApiResult};
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

/// 共通のPRAGMA。ロック競合時はすぐ失敗せず30秒までリトライ待ちする(database is locked 対策)。
fn apply_pragmas(conn: &Connection, synchronous_normal: bool) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    if synchronous_normal {
        conn.pragma_update(None, "synchronous", "NORMAL")?;
    }
    conn.busy_timeout(std::time::Duration::from_millis(30_000))?;
    Ok(())
}

/// index.db を開く。`ensure_schema` ならテーブル・インデックス・FTSを作る(起動時に1回だけ行う)。
pub fn open_index_db(path: &Path, ensure_schema: bool) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    apply_pragmas(&conn, true)?;
    if ensure_schema {
        conn.execute_batch(SCHEMA_SQL)?;
        conn.execute_batch(FTS_SQL)?;
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

pub struct Db {
    path: PathBuf,
    pool: Mutex<Vec<Connection>>,
    /// 通常の利用は読み取り側、`/api/reset` のファイル削除は書き込み側で握る。
    /// 書き込み側を握っている間は、使用中の接続が無く、新しい接続も作られない。
    gate: RwLock<()>,
}

impl Db {
    pub fn new(path: PathBuf) -> rusqlite::Result<Arc<Self>> {
        // 起動時に一度だけスキーマを保証する。以降の接続は作成処理を省く
        // (CREATE ... IF NOT EXISTS でも書き込みロックを一瞬取るため、取り込み中のブロック起点になる)。
        open_index_db(&path, true)?;
        Ok(Arc::new(Db { path, pool: Mutex::new(Vec::new()), gate: RwLock::new(()) }))
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
                None => open_index_db(&this.path, false)?,
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
