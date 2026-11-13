//! 取り込み(インデクサ)。
//!
//! 流れ: ロック取得 → 走査 → (member, msg_id) ごとに行を組み立てて upsert → missing 判定 → サムネ生成。
//! 取り込み中もビューアが WAL 経由で読めるよう、500件ごとに commit する。
//!
//! - ファイル名の日時が実在しない(2月30日など)ファイルは、警告して飛ばす
//! - 検索索引の差し替えは rowid で行う。新規追加の前にも同じ rowid の索引行を消すので、孤児の索引行が残っていても
//!   rowid 衝突で失敗しない

pub mod progress;
pub mod scan;
pub mod thumbs;

use crate::config::Config;
use crate::db::open_index_db;
use crate::indexlock::{IndexAlreadyRunning, IndexLock};
use progress::{now_f64, ProgressFileWriter};
use rusqlite::{params, Connection, OptionalExtension};
use scan::FileEntry;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;
use thumbs::{ThumbJob, ThumbKind};

#[derive(Debug, Default, Clone)]
pub struct IndexSummary {
    pub new_count: u64,
    pub updated_count: u64,
    pub skipped_count: u64,
    pub missing_count: u64,
    pub thumb_count: u64,
    pub elapsed_sec: f64,
    pub total_files_scanned: u64,
    pub total_messages_scanned: u64,
}

impl IndexSummary {
    /// API・progress.json に出すサマリ。
    pub fn to_json(&self) -> Value {
        json!({
            "new_count": self.new_count,
            "updated_count": self.updated_count,
            "skipped_count": self.skipped_count,
            "missing_count": self.missing_count,
            "thumb_count": self.thumb_count,
            "elapsed_sec": self.elapsed_sec,
            "total_files_scanned": self.total_files_scanned,
            "total_messages_scanned": self.total_messages_scanned,
        })
    }
}

#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// "full" | "incremental"
    pub mode: String,
    /// DB を作り直してから全期間を取り込む(CLI の --rebuild)
    pub rebuild: bool,
    pub only_member: Option<String>,
    pub no_thumbs: bool,
}

#[derive(Debug)]
pub enum IndexError {
    AlreadyRunning(IndexAlreadyRunning),
    Failed(String),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::AlreadyRunning(e) => write!(f, "{e}"),
            IndexError::Failed(m) => f.write_str(m),
        }
    }
}

impl From<rusqlite::Error> for IndexError {
    fn from(e: rusqlite::Error) -> Self {
        IndexError::Failed(e.to_string())
    }
}

/// 進捗の通知先(サーバ内で動くときの /api/reindex/status 用)。`{"stage": ..., ...}` を受け取る。
pub type ProgressCb<'a> = &'a (dyn Fn(Value) + Sync);

// ---------------------------------------------------------------- 行の組み立て

const FLAG_TO_KIND: [&str; 4] = ["text", "image", "video", "audio"];

struct NewRow {
    member: String,
    group: Option<String>,
    msg_id: i64,
    ts: i64,
    ts_raw: String,
    body: Option<String>,
    media: Option<String>,
    kind: &'static str,
    flag: i64,
}

/// `YYYYMMDDHHMMSS`(UTC として解釈)を epoch 秒に。実在しない日時は None。
pub fn parse_ts_utc(ts_raw: &str) -> Option<i64> {
    let n = |a: usize, b: usize| ts_raw.get(a..b)?.parse::<u32>().ok();
    let date = chrono::NaiveDate::from_ymd_opt(n(0, 4)? as i32, n(4, 6)?, n(6, 8)?).filter(|d| {
        use chrono::Datelike;
        d.year() >= 1
    })?;
    let t = date.and_hms_opt(n(8, 10)?, n(10, 12)?, n(12, 14)?)?;
    Some(t.and_utc().timestamp())
}

/// テキスト本文を UTF-8 で読み、改行を `\n` に揃える(\r\n, \r → \n)。
/// UTF-8 として不正なら None(警告のみ)。
fn read_body(path: &Path) -> Option<String> {
    match std::fs::read(path).map(String::from_utf8) {
        Ok(Ok(s)) => Some(if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s }),
        Ok(Err(e)) => {
            logln!("[warn] failed to read {}: {e}", path.display());
            None
        }
        Err(e) => {
            logln!("[warn] failed to read {}: {e}", path.display());
            None
        }
    }
}

/// 同一 (member, msg_id) のファイル群から1行を組み立てる。日時が不正なら Err(警告して飛ばす)。
fn build_row(member: &str, msg_id: i64, files: &[FileEntry]) -> Result<NewRow, String> {
    let txt = files.iter().find(|f| f.ext == "txt");
    let media: Vec<&FileEntry> = files.iter().filter(|f| f.ext != "txt").collect();
    if media.len() > 1 {
        logln!(
            "[warn] multiple media files for (member={member}, msg_id={msg_id}): {:?} -> using {}",
            media.iter().map(|f| &f.rel_path).collect::<Vec<_>>(),
            media[0].rel_path
        );
    }
    // flag/kind: メディアがあればそのflag、無ければ txt の flag
    let primary = media.first().copied().or(txt).expect("files is not empty");
    let ts = parse_ts_utc(&primary.ts_raw)
        .ok_or_else(|| format!("invalid timestamp in file name: {}", primary.rel_path))?;
    Ok(NewRow {
        member: member.to_string(),
        group: files.iter().find_map(|f| f.group.clone().filter(|g| !g.is_empty())),
        msg_id,
        ts,
        ts_raw: primary.ts_raw.clone(),
        body: txt.and_then(|t| read_body(&t.path)),
        media: media.first().map(|f| f.rel_path.clone()),
        kind: FLAG_TO_KIND.get(primary.flag as usize).copied().unwrap_or("text"),
        flag: primary.flag,
    })
}

/// サムネの thumbs_dir 相対パス: `{sha1(member)[:2]}/{sha1(member)}/{msg_id}.jpg`
pub fn thumb_rel_path(member: &str, msg_id: i64) -> String {
    use sha1::{Digest, Sha1};
    let h = format!("{:x}", Sha1::digest(member.as_bytes()));
    format!("{}/{h}/{msg_id}.jpg", &h[..2])
}

/// (member, msg_id) ごとにまとめる。順序は最初に現れた順(取り込み順 = rowid の順になる)。
fn group_entries(entries: Vec<FileEntry>) -> Vec<((String, i64), Vec<FileEntry>)> {
    let mut index: HashMap<(String, i64), usize> = HashMap::new();
    let mut out: Vec<((String, i64), Vec<FileEntry>)> = Vec::new();
    for e in entries {
        let key = (e.member.clone(), e.msg_id);
        match index.get(&key) {
            Some(&i) => out[i].1.push(e),
            None => {
                index.insert(key.clone(), out.len());
                out.push((key, vec![e]));
            }
        }
    }
    // msg_id は数値化するため、ゼロ埋めの違い("007" と "7")が同じキーに衝突しうる。気づけるよう警告する
    for ((member, msg_id), files) in &out {
        let ids: HashSet<&str> = files
            .iter()
            .filter_map(|f| f.rel_path.rsplit('/').next().and_then(|n| n.split('_').next()))
            .collect();
        if ids.len() > 1 {
            logln!("[warn] msg_id representation collision for (member={member}, msg_id={msg_id}): {ids:?}");
        }
    }
    out
}

// ---------------------------------------------------------------- 実行

/// stderr への進捗表示(2秒おき)。
struct ConsoleReporter {
    label: &'static str,
    start: Instant,
    last: Option<Instant>,
}

impl ConsoleReporter {
    fn new(label: &'static str) -> Self {
        ConsoleReporter { label, start: Instant::now(), last: None }
    }
    fn maybe_report(&mut self, done: u64, total: u64) {
        let now = Instant::now();
        if self.last.is_none_or(|l| now - l >= std::time::Duration::from_secs(2)) || done == total {
            let pct = if total > 0 { done as f64 / total as f64 * 100.0 } else { 100.0 };
            logln!("[{}] {done}/{total} ({pct:.1}%) elapsed={:.1}s", self.label, self.start.elapsed().as_secs_f64());
            self.last = Some(now);
        }
    }
}

const CHECKPOINT_WAL_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;

/// WAL が一定サイズを超えていたら PASSIVE チェックポイント(コストは WAL サイズに比例するので間引く)。
fn checkpoint_if_wal_large(conn: &Connection, db_path: &Path) -> rusqlite::Result<()> {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    if std::fs::metadata(PathBuf::from(wal)).map(|m| m.len()).unwrap_or(0) >= CHECKPOINT_WAL_THRESHOLD_BYTES {
        conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))?;
    }
    Ok(())
}

fn rebuild_db(db_path: &Path) -> Result<(), IndexError> {
    for suffix in ["", "-wal", "-shm"] {
        let mut p = db_path.as_os_str().to_owned();
        p.push(suffix);
        match std::fs::remove_file(PathBuf::from(p)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(IndexError::Failed(e.to_string())),
        }
    }
    Ok(())
}

/// 取り込みを実行する。多重実行は `IndexError::AlreadyRunning`(待たない)。
pub fn run_index(cfg: &Config, opts: &IndexOptions, progress_cb: Option<ProgressCb>) -> Result<IndexSummary, IndexError> {
    let start = Instant::now();
    let lock = IndexLock::acquire(&cfg.index_lock_path()).map_err(IndexError::AlreadyRunning)?;
    // 進捗ファイルはロックを取れてから作る(実行中の他プロセスの進捗を上書きしないため)
    let writer = ProgressFileWriter::new(&cfg.progress_json_path(), &opts.mode);

    let report = |stage: &str, kw: Map<String, Value>| {
        if let Some(cb) = progress_cb {
            let mut payload = Map::new();
            payload.insert("stage".into(), json!(stage));
            payload.extend(kw.clone());
            cb(Value::Object(payload));
        }
        if stage == "done" {
            return;
        }
        let phase = match stage {
            "scanning" | "scanned" => "scanning",
            "upserting" | "missing_check" => "upsert",
            "thumbnails" | "thumbnailing" => "thumbs",
            other => other,
        };
        let num = |k: &str| kw.get(k).and_then(Value::as_u64);
        let message = match stage {
            "scanned" => Some(format!(
                "{}件のファイル / {}件のメッセージを検出",
                num("files").unwrap_or(0),
                num("messages").unwrap_or(0)
            )),
            "missing_check" => Some("欠損チェック中...".to_string()),
            _ => None,
        };
        writer.update(phase, num("done"), num("total"), message);
    };

    let mut summary = IndexSummary::default();
    // 想定外の panic(壊れたファイルのデコードなど)も、ふつうの失敗として扱う。
    // そのまま巻き戻すと progress.json が「実行中」のまま残り、サーバ内で動いていた場合は
    // 書いたプロセス(=サーバ自身)が生きているため、状態APIが実行中を返し続ける
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_locked(cfg, opts, &report, &mut summary)))
        .unwrap_or_else(|payload| Err(IndexError::Failed(format!("internal error: {}", panic_message(&*payload)))));
    // 進捗ファイルの書き込みを終えてからロックを解放する
    // (先に解放すると、その間に始まった別の取り込みの進捗を「完了」で上書きしうる)
    let result = match result {
        Err(e) => {
            writer.finish_error(&e.to_string());
            Err(e)
        }
        Ok(()) => {
            summary.elapsed_sec = start.elapsed().as_secs_f64();
            let mut kw = Map::new();
            kw.insert("summary".into(), summary.to_json());
            report("done", kw);
            writer.finish_done(&summary);
            Ok(summary)
        }
    };
    drop(lock);
    result
}

/// panic の内容(`panic!("...")` の文字列)。取り出せなければ固定の文言。
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}

fn kw(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

struct Existing {
    body: Option<String>,
    media: Option<String>,
    kind: String,
    flag: i64,
    thumb: Option<String>,
    missing: i64,
    ts_raw: String,
}

fn run_locked(
    cfg: &Config,
    opts: &IndexOptions,
    report: &(dyn Fn(&str, Map<String, Value>) + Sync),
    summary: &mut IndexSummary,
) -> Result<(), IndexError> {
    let root = cfg.root.clone().ok_or_else(|| IndexError::Failed("root is not configured".into()))?;
    let ffmpeg = thumbs::find_ffmpeg(&cfg.base_dir);
    match &ffmpeg {
        Some(p) => logln!("[index] ffmpeg found at {}: video thumbnails enabled", p.display()),
        None => logln!("[index] ffmpeg not found: video thumbnails skipped"),
    }

    if opts.rebuild {
        rebuild_db(&cfg.db_path)?;
    }
    let conn = open_index_db(&cfg.db_path, true)?;
    let mode = if opts.rebuild { "full" } else { opts.mode.as_str() };

    report("scanning", Map::new());
    logln!("[index] scanning root={} mode={mode} only_member={:?}", root.display(), opts.only_member);
    let known: HashSet<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT member FROM messages")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let (entries, scope) =
        scan::discover_files(&root, mode, cfg.incremental_months, opts.only_member.as_deref(), &known);
    summary.total_files_scanned = entries.len() as u64;
    logln!("[index] discovered {} files", entries.len());

    let grouped = group_entries(entries);
    summary.total_messages_scanned = grouped.len() as u64;
    report("scanned", kw(&[("files", json!(summary.total_files_scanned)), ("messages", json!(grouped.len()))]));

    let total = grouped.len() as u64;
    let mut seen: HashSet<(String, i64)> = HashSet::with_capacity(grouped.len());
    let mut thumb_jobs: Vec<ThumbJob> = Vec::new();
    let mut console = ConsoleReporter::new("upsert");
    let mut done: u64 = 0;
    let workers = scan::default_workers();

    conn.execute_batch("BEGIN")?;
    // 本文の読み込み(I/O 待ち)は並列に、DB 書き込みは順に行う。順序は保つ(走査順が rowid の順になる)
    for chunk in grouped.chunks(512) {
        let built: Vec<Result<NewRow, String>> = parallel_map(chunk, workers, |((m, id), files)| build_row(m, *id, files));
        for (((member, msg_id), files), row) in chunk.iter().zip(built) {
            done += 1;
            let row = match row {
                Ok(r) => Some(r),
                Err(e) => {
                    logln!("[warn] skipped: {e}");
                    None
                }
            };
            if let Some(row) = row {
                seen.insert((member.clone(), *msg_id));
                upsert_one(&conn, cfg, opts, &row, files, ffmpeg.is_some(), &mut thumb_jobs, summary)?;
            }

            if done.is_multiple_of(100) || done == total {
                console.maybe_report(done, total);
                report("upserting", kw(&[("done", json!(done)), ("total", json!(total))]));
            }
            // 取り込み中もビューアが WAL 経由で読めるよう 500 件ごとに commit する
            if done.is_multiple_of(500) || done == total {
                conn.execute_batch("COMMIT")?;
                checkpoint_if_wal_large(&conn, &cfg.db_path)?;
                conn.execute_batch("BEGIN")?;
            }
        }
    }
    conn.execute_batch("COMMIT")?;
    conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))?;

    // missing 判定: 走査範囲内の既存行のうち、今回見つからなかったものを missing=1(見つかれば 0 に戻す)。
    // 範囲の年月は ts(UTC epoch)+9h の JST で求める
    report("missing_check", Map::new());
    let mut missing_updates = 0u64;
    conn.execute_batch("BEGIN")?;
    for member in &scope.members {
        let months = &scope.member_yearmonths[member];
        let rows: Vec<(i64, i64, i64)> = {
            let mut stmt = conn.prepare_cached("SELECT msg_id, ts, missing FROM messages WHERE member=?")?;
            let it = stmt.query_map([member], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            it.collect::<rusqlite::Result<_>>()?
        };
        for (msg_id, ts, missing) in rows {
            if let Some(months) = months {
                let jst = chrono::DateTime::from_timestamp(ts + 9 * 3600, 0).map(|d| d.format("%Y%m").to_string());
                if !jst.is_some_and(|ym| months.contains(&ym)) {
                    continue;
                }
            }
            let should = if seen.contains(&(member.clone(), msg_id)) { 0 } else { 1 };
            if should != missing {
                conn.execute("UPDATE messages SET missing=? WHERE member=? AND msg_id=?", params![should, member, msg_id])?;
                missing_updates += 1;
                if should == 1 {
                    summary.missing_count += 1;
                }
            }
        }
    }
    conn.execute_batch("COMMIT")?;
    logln!("[index] missing flag updates: {missing_updates}");

    // サムネ生成(失敗しても取り込み全体は止めない)
    if thumb_jobs.is_empty() {
        logln!("[index] no new thumbnails needed");
    } else {
        let n = thumb_jobs.len() as u64;
        report("thumbnails", kw(&[("done", json!(0)), ("total", json!(n))]));
        logln!("[index] generating {n} thumbnails");
        let console = Mutex::new(ConsoleReporter::new("thumbs"));
        let state = Mutex::new((0u64, 0u64)); // (done, ok)
        let all = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        parallel_for(&thumb_jobs, all, |job| {
            // 1枚の失敗(デコーダの panic を含む)で残りのサムネ生成や取り込み全体を止めない
            let res = std::panic::catch_unwind(|| thumbs::generate(job, cfg.thumb_size, cfg.thumb_quality, ffmpeg.as_deref()))
                .unwrap_or_else(|payload| Err(format!("internal error: {}", panic_message(&*payload))));
            if let Err(e) = &res {
                logln!("[warn] thumb failed for {}: {e}", job.src.display());
            }
            let (d, _ok) = {
                let mut s = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                s.0 += 1;
                if res.is_ok() {
                    s.1 += 1;
                }
                *s
            };
            console.lock().unwrap().maybe_report(d, n);
            if d % 200 == 0 {
                report("thumbnailing", kw(&[("done", json!(d)), ("total", json!(n))]));
            }
        });
        summary.thumb_count = state.into_inner().unwrap().1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upsert_one(
    conn: &Connection,
    cfg: &Config,
    opts: &IndexOptions,
    row: &NewRow,
    files: &[FileEntry],
    has_ffmpeg: bool,
    thumb_jobs: &mut Vec<ThumbJob>,
    summary: &mut IndexSummary,
) -> Result<(), IndexError> {
    // 差分判定用の既存行。主キーで1件ずつ引く(以前は走査したメンバーの全行を本文ごと先読みしていたため、
    // 差分取り込みでも DB の本文をほぼ全部読んでいた)
    let prev: Option<Existing> = conn
        .prepare_cached(
            "SELECT body, media, kind, flag, thumb, missing, ts_raw FROM messages WHERE member=? AND msg_id=?",
        )?
        .query_row(params![row.member, row.msg_id], |r| {
            Ok(Existing {
                body: r.get(0)?,
                media: r.get(1)?,
                kind: r.get(2)?,
                flag: r.get(3)?,
                thumb: r.get(4)?,
                missing: r.get(5)?,
                ts_raw: r.get(6)?,
            })
        })
        .optional()?;
    let mut thumb_rel: Option<String> = prev.as_ref().and_then(|p| p.thumb.clone());

    // サムネ元: jpg(画像)。無ければ ffmpeg があるときだけ動画(flag=2 の mp4。音声も mp4 を使うため flag で絞る)
    let jpg = files.iter().find(|f| f.ext == "jpg");
    let video = if jpg.is_none() && has_ffmpeg { files.iter().find(|f| f.ext == "mp4" && f.flag == 2) } else { None };
    if let Some(src) = jpg.or(video).filter(|_| !opts.no_thumbs) {
        let rel = thumb_rel_path(&row.member, row.msg_id);
        let abs = cfg.thumbs_dir.join(&rel);
        // 既にあるサムネは作り直さない(後から ffmpeg を入れても、未生成の動画サムネだけが積まれる)
        if !thumbs::thumb_exists(&abs) {
            let kind = if jpg.is_some() { ThumbKind::Image } else { ThumbKind::Video };
            thumb_jobs.push(ThumbJob { src: src.path.clone(), dst: abs, kind });
        }
        thumb_rel = Some(rel);
    }

    let has_body = row.body.as_deref().is_some_and(|b| !b.is_empty());
    let replace_fts = |conn: &Connection| -> rusqlite::Result<()> {
        let rowid: Option<i64> = conn
            .prepare_cached("SELECT rowid FROM messages WHERE member=? AND msg_id=?")?
            .query_row(params![row.member, row.msg_id], |r| r.get(0))
            .optional()?;
        if let Some(rowid) = rowid {
            conn.prepare_cached("DELETE FROM messages_fts WHERE rowid=?")?.execute([rowid])?;
            if has_body {
                conn.prepare_cached("INSERT INTO messages_fts(rowid, body, member, msg_id) VALUES (?, ?, ?, ?)")?
                    .execute(params![rowid, row.body, row.member, row.msg_id])?;
            }
        }
        Ok(())
    };

    match prev {
        None => {
            conn.prepare_cached(
                "INSERT INTO messages
                   (member, group_, msg_id, ts, ts_raw, body, media, kind, flag, thumb, missing)
                 VALUES (?,?,?,?,?,?,?,?,?,?,0)
                 ON CONFLICT(member, msg_id) DO UPDATE SET
                   group_=excluded.group_, ts=excluded.ts, ts_raw=excluded.ts_raw, body=excluded.body,
                   media=excluded.media, kind=excluded.kind, flag=excluded.flag,
                   thumb=COALESCE(excluded.thumb, messages.thumb), missing=0",
            )?
            .execute(params![
                row.member, row.group, row.msg_id, row.ts, row.ts_raw, row.body, row.media, row.kind, row.flag, thumb_rel
            ])?;
            replace_fts(conn)?;
            summary.new_count += 1;
        }
        Some(p) => {
            let changed = p.body != row.body
                || p.media != row.media
                || p.kind != row.kind
                || p.flag != row.flag
                || p.missing != 0
                || p.ts_raw != row.ts_raw
                || (thumb_rel.is_some() && p.thumb != thumb_rel);
            if changed {
                conn.prepare_cached(
                    "UPDATE messages SET group_=?, ts=?, ts_raw=?, body=?, media=?, kind=?, flag=?, thumb=?, missing=0
                     WHERE member=? AND msg_id=?",
                )?
                .execute(params![
                    row.group, row.ts, row.ts_raw, row.body, row.media, row.kind, row.flag, thumb_rel, row.member,
                    row.msg_id
                ])?;
                replace_fts(conn)?;
                summary.updated_count += 1;
            } else {
                summary.skipped_count += 1;
            }
        }
    }
    Ok(())
}

/// 入力順を保ったまま並列に map する。
pub(crate) fn parallel_map<T: Sync, R: Send>(items: &[T], workers: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let slots: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    parallel_for_indexed(items, workers, |i, item| *slots[i].lock().unwrap() = Some(f(item)));
    slots.into_iter().map(|s| s.into_inner().unwrap().unwrap()).collect()
}

fn parallel_for<T: Sync>(items: &[T], workers: usize, f: impl Fn(&T) + Sync) {
    parallel_for_indexed(items, workers, |_, item| f(item));
}

fn parallel_for_indexed<T: Sync>(items: &[T], workers: usize, f: impl Fn(usize, &T) + Sync) {
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, items.len().max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(item) = items.get(i) else { break };
                f(i, item);
            });
        }
    });
}

/// CLI の結果表示。
pub fn print_summary(s: &IndexSummary) {
    println!("{}", "=".repeat(60));
    println!("インデックス取り込み サマリ");
    println!("{}", "=".repeat(60));
    println!("新規:         {}", s.new_count);
    println!("更新:         {}", s.updated_count);
    println!("スキップ:     {}", s.skipped_count);
    println!("missing化:    {}", s.missing_count);
    println!("サムネ生成:   {}", s.thumb_count);
    println!("走査ファイル数: {}", s.total_files_scanned);
    println!("走査メッセージ数: {}", s.total_messages_scanned);
    println!("所要時間:     {:.2}秒", s.elapsed_sec);
    println!("{}", "=".repeat(60));
}

/// 現在時刻(進捗・状態用)。
pub fn now() -> f64 {
    now_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_parsing() {
        assert_eq!(parse_ts_utc("20260801000000"), Some(1785542400));
        assert_eq!(parse_ts_utc("20260801150000"), Some(1785596400));
        assert_eq!(parse_ts_utc("20260230000000"), None); // 2月30日
        assert_eq!(parse_ts_utc("20261301000000"), None);
        assert_eq!(parse_ts_utc("20260801240000"), None);
        assert_eq!(parse_ts_utc("20260801235960"), None); // 秒60
        assert_eq!(parse_ts_utc("00000101000000"), None); // 0年
    }

    #[test]
    fn thumb_path() {
        // sha1("member_a") の16進
        let p = thumb_rel_path("member_a", 6);
        assert!(p.starts_with("f0/f01efa24a214b59efb5c1f58e247792aabfefca3/"), "{p}");
        assert!(p.ends_with("/6.jpg"));
    }

    #[test]
    fn order_preserving_parallel_map() {
        let v: Vec<u32> = (0..1000).collect();
        assert_eq!(parallel_map(&v, 8, |x| x * 2), (0..1000).map(|x| x * 2).collect::<Vec<_>>());
        assert!(parallel_map(&Vec::<u32>::new(), 8, |x| *x).is_empty());
    }
}
