//! 取り込み(インデクサ)。
//!
//! 流れ: ロック取得 → 走査 → (member, msg_id) ごとに行を組み立てて upsert → missing 判定 → サムネ生成(thumbs.db)。
//! 取り込み中もビューアが WAL 経由で読めるよう、500件ごとに commit する。
//!
//! - ファイル名の日時が実在しない(2月30日など)ファイルは、警告して飛ばす
//! - 検索索引の差し替えは rowid で行う。新規追加の前にも同じ rowid の索引行を消すので、孤児の索引行が残っていても
//!   rowid 衝突で失敗しない

pub mod progress;
pub mod scan;
pub mod thumbs;

use crate::config::Config;
use crate::db::{open_index_db, open_thumbs_db};
use crate::indexlock::{IndexAlreadyRunning, IndexLock};
use progress::{now_f64, ProgressFileWriter};
use rusqlite::{params, Connection, OptionalExtension};
use scan::FileEntry;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// 本文ファイルを一時的に読めなかった(ロック中など)。このときは保存済みの本文を消さずに残す
    body_unreadable: bool,
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
    // 表示は JST(+9h)。JST で 9999 年を超える日時は SQLite の日付関数が扱えない(カレンダーが壊れる)ため不正とする
    Some(t.and_utc().timestamp()).filter(|&ts| ts + 9 * 3600 <= crate::timeutil::MAX_TS_JST)
}

/// テキスト本文を UTF-8 で読み、改行を `\n` に揃える(\r\n, \r → \n)。
/// UTF-8 として不正なら Ok(None)(警告のみ)。読み込み自体の失敗(別プロセスがロック中など)は Err。
fn read_body(path: &Path) -> Result<Option<String>, ()> {
    match std::fs::read(path).map(String::from_utf8) {
        Ok(Ok(s)) => Ok(Some(if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s })),
        Ok(Err(e)) => {
            logln!("[warn] failed to read {}: {e}", path.display());
            Ok(None)
        }
        Err(e) => {
            logln!("[warn] failed to read {} (keeping the stored body): {e}", path.display());
            Err(())
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
    let body = txt.map(|t| read_body(&t.path));
    Ok(NewRow {
        member: member.to_string(),
        group: files.iter().find_map(|f| f.group.clone().filter(|g| !g.is_empty())),
        msg_id,
        ts,
        ts_raw: primary.ts_raw.clone(),
        body: body.clone().and_then(|b| b.ok().flatten()),
        body_unreadable: body.is_some_and(|b| b.is_err()),
        media: media.first().map(|f| f.rel_path.clone()),
        kind: FLAG_TO_KIND.get(primary.flag as usize).copied().unwrap_or("text"),
        flag: primary.flag,
    })
}

/// サムネのキー(thumbs.db の `path`、`messages.thumb`、`/thumbs/{path}`): `{sha1(member)[:2]}/{sha1(member)}/{msg_id}.jpg`
/// (旧版がサムネのフォルダに置いていたときの相対パスと同じ形)
pub fn thumb_rel_path(member: &str, msg_id: i64) -> String {
    format!("{}{msg_id}.jpg", thumb_member_prefix(member))
}

/// メンバーのサムネのキーに共通する先頭部分 `{sha1(member)[:2]}/{sha1(member)}/`(メンバー削除で使う)。
/// 16進数字と `/` だけなので、GLOB のパターンにそのまま使える。
pub fn thumb_member_prefix(member: &str) -> String {
    use sha1::{Digest, Sha1};
    let h = format!("{:x}", Sha1::digest(member.as_bytes()));
    format!("{}/{h}/", &h[..2])
}

/// 旧版のサムネフォルダを thumbs.db に取り込む(済んでいれば何もしない)。
/// 失敗は警告だけにする(取り込めなかったサムネは、取り込みで作り直される)。
pub fn import_legacy_thumbs(conn: &Connection, cfg: &Config) {
    let dir = &cfg.legacy_thumbs_dir;
    match thumbs::import_legacy_dir(conn, dir) {
        Ok(None) => {}
        Ok(Some(r)) if r == thumbs::LegacyImport::default() => {}
        Ok(Some(r)) => {
            logln!(
                "[thumbs] imported {} thumbnails and {} failure marks from the old folder {}",
                r.thumbs,
                r.failures,
                dir.display()
            );
            if r.errors > 0 {
                logln!("[thumbs] warning: {} 件を読めませんでした。次回の起動・取り込みでもう一度試します", r.errors);
            } else {
                logln!("[thumbs] 旧サムネフォルダ {} は使わなくなりました。旧版に戻さないなら削除してかまいません", dir.display());
            }
        }
        Err(e) => logln!("[thumbs] warning: failed to import the old thumbnail folder {}: {e}", dir.display()),
    }
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
        let p = PathBuf::from(p);
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(IndexError::Failed(format!(
                    "{} を削除できませんでした(他のプロセスが開いている可能性があります): {e}",
                    p.display()
                )));
            }
        }
    }
    Ok(())
}

/// 取り込みを実行する。多重実行は `IndexError::AlreadyRunning`(待たない)。
pub fn run_index(cfg: &Config, opts: &IndexOptions, progress_cb: Option<ProgressCb>) -> Result<IndexSummary, IndexError> {
    let start = Instant::now();
    let lock = IndexLock::acquire(&cfg.index_lock_path()).map_err(|e| {
        if e.io_error { IndexError::Failed(e.to_string()) } else { IndexError::AlreadyRunning(e) }
    })?;
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
    group: Option<String>,
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
    let tconn = open_thumbs_db(&cfg.thumbs_db_path(), true)?;
    import_legacy_thumbs(&tconn, cfg);
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

    // IMMEDIATE で始める(DEFERRED だと、読んでから書くときに他の書き手と競合して、待たずに SQLITE_BUSY で失敗しうる)
    conn.execute_batch("BEGIN IMMEDIATE")?;
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
                upsert_one(&conn, &tconn, opts, &row, files, ffmpeg.is_some(), &mut thumb_jobs, summary)?;
            }

            if done.is_multiple_of(100) || done == total {
                console.maybe_report(done, total);
                report("upserting", kw(&[("done", json!(done)), ("total", json!(total))]));
            }
            // 取り込み中もビューアが WAL 経由で読めるよう 500 件ごとに commit する
            if done.is_multiple_of(500) || done == total {
                conn.execute_batch("COMMIT")?;
                checkpoint_if_wal_large(&conn, &cfg.db_path)?;
                conn.execute_batch("BEGIN IMMEDIATE")?;
            }
        }
    }
    conn.execute_batch("COMMIT")?;
    conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))?;

    // missing 判定: 走査範囲内の既存行のうち、今回見つからなかったものを missing=1(見つかれば 0 に戻す)。
    // 走査範囲(差分取り込みの年月)は、月フォルダ名またはファイル名の日時(UTC)の年月で決まる。
    // 一方、月フォルダが UTC と JST(ts+9h)のどちらで分かれているかは分からないため、両方の年月が範囲内の行だけを
    // 判定する。片方だけで判定すると、月初 0〜9 時(JST)のメッセージが走査されていないのに missing=1 になる。
    // 見つかった行は取り込み時に missing=0 に戻しているので、範囲外として飛ばしても見落としは起きない
    report("missing_check", Map::new());
    let mut missing_updates = 0u64;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    for member in &scope.members {
        let months = &scope.member_yearmonths[member];
        let rows: Vec<(i64, i64, String, i64)> = {
            let mut stmt = conn.prepare_cached("SELECT msg_id, ts, ts_raw, missing FROM messages WHERE member=?")?;
            let it = stmt.query_map([member], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            it.collect::<rusqlite::Result<_>>()?
        };
        for (msg_id, ts, ts_raw, missing) in rows {
            if let Some(months) = months {
                let jst = chrono::DateTime::from_timestamp(ts + 9 * 3600, 0).map(|d| d.format("%Y%m").to_string());
                let utc = ts_raw.get(..6);
                if !(jst.is_some_and(|ym| months.contains(&ym)) && utc.is_some_and(|ym| months.contains(ym))) {
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

    // サムネ生成(失敗しても取り込み全体は止めない)。生成は並列に、thumbs.db への書き込みは1本のスレッドで行う
    if thumb_jobs.is_empty() {
        logln!("[index] no new thumbnails needed");
    } else {
        let n = thumb_jobs.len() as u64;
        report("thumbnails", kw(&[("done", json!(0)), ("total", json!(n))]));
        logln!("[index] generating {n} thumbnails");
        let console = Mutex::new(ConsoleReporter::new("thumbs"));
        let state = Mutex::new((0u64, 0u64)); // (done, ok)
        let all = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        // 送り待ちの上限(メモリを抑える。書き込みが詰まったら生成側が待つ)
        let (tx, rx) = std::sync::mpsc::sync_channel::<thumbs::Outcome>(256);
        // 書き込みが失敗したら、残りは作っても保存できないので作らない
        let writer_failed = AtomicBool::new(false);
        let db_path = &cfg.db_path;
        let written = std::thread::scope(|s| {
            // thumbs.db に保存できた分だけ、続けて messages.thumb を設定する
            // (index.db の接続はスレッドをまたげないので、書き込みスレッドで開く)
            let writer = s.spawn(move || -> rusqlite::Result<()> {
                let iconn = open_index_db(db_path, false)?;
                thumbs::write_outcomes(&tconn, &rx, |made| {
                    thumbs::in_transaction(&iconn, || {
                        let mut stmt = iconn.prepare_cached("UPDATE messages SET thumb=? WHERE member=? AND msg_id=?")?;
                        for (member, msg_id, rel) in made {
                            stmt.execute(params![rel, member, msg_id])?;
                        }
                        Ok(())
                    })
                })
            });
            parallel_for(&thumb_jobs, all, |job| {
                if writer_failed.load(Ordering::Relaxed) {
                    return;
                }
                // 1枚の失敗(デコーダの panic を含む)で残りのサムネ生成や取り込み全体を止めない
                let res =
                    std::panic::catch_unwind(|| thumbs::generate(job, cfg.thumb_size, cfg.thumb_quality, ffmpeg.as_deref()))
                        .unwrap_or_else(|payload| Err(format!("internal error: {}", panic_message(&*payload))));
                let ok = res.is_ok();
                let outcome = match res {
                    Ok(data) => thumbs::Outcome::Made {
                        rel: job.rel.clone(),
                        data,
                        sig: thumbs::source_signature(&job.src),
                        member: job.member.clone(),
                        msg_id: job.msg_id,
                    },
                    Err(e) => {
                        // thumb は空のまま(画面は元のメディアを表示する)
                        logln!("[warn] thumb failed for {}: {e}", job.src.display());
                        thumbs::Outcome::Failed { rel: job.rel.clone(), sig: thumbs::source_signature(&job.src) }
                    }
                };
                if tx.send(outcome).is_err() {
                    writer_failed.store(true, Ordering::Relaxed);
                    return;
                }
                let (d, _ok) = {
                    let mut s = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    s.0 += 1;
                    if ok {
                        s.1 += 1;
                    }
                    *s
                };
                console.lock().unwrap().maybe_report(d, n);
                if d % 200 == 0 || d == n {
                    report("thumbnailing", kw(&[("done", json!(d)), ("total", json!(n))]));
                }
            });
            // 送り手を閉じると、書き込みスレッドは残りを書いて終わる
            drop(tx);
            match writer.join() {
                Ok(res) => res.map_err(|e| e.to_string()),
                Err(payload) => Err(format!("internal error: {}", panic_message(&*payload))),
            }
        });
        summary.thumb_count = state.into_inner().unwrap().1;
        written.map_err(|e| IndexError::Failed(format!("サムネを thumbs.db に保存できませんでした: {e}")))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upsert_one(
    conn: &Connection,
    tconn: &Connection,
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
            "SELECT body, media, kind, flag, thumb, missing, ts_raw, group_ FROM messages WHERE member=? AND msg_id=?",
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
                group: r.get(7)?,
            })
        })
        .optional()?;
    let mut thumb_rel: Option<String> = prev.as_ref().and_then(|p| p.thumb.clone());
    // サムネの有無をこの取り込みで決めたか(決めたら、保存済みの thumb と食い違えば書き直す)
    let mut thumb_decided = false;

    // サムネ元: jpg(画像)。無ければ ffmpeg があるときだけ動画(flag=2 の mp4。音声も mp4 を使うため flag で絞る)
    let jpg = files.iter().find(|f| f.ext == "jpg");
    let video = if jpg.is_none() && has_ffmpeg { files.iter().find(|f| f.ext == "mp4" && f.flag == 2) } else { None };
    if let Some(src) = jpg.or(video).filter(|_| !opts.no_thumbs) {
        let rel = thumb_rel_path(&row.member, row.msg_id);
        thumb_decided = true;
        // 同じ元ファイルから作ったサムネが既にあれば作り直さない(後から ffmpeg を入れても、未生成の動画サムネだけが積まれる)
        if thumbs::is_stored_for(tconn, &rel, &src.path)? {
            thumb_rel = Some(rel);
        } else if opts.mode != "full" && thumbs::failed_before(tconn, &rel, &src.path)? {
            // 同じ元ファイルから前回作れなかった。差分取り込みでは作り直さない(全期間の取り込みでは作り直しを試す)
            thumb_rel = None;
        } else {
            // thumb は、サムネを thumbs.db に保存できてから設定する(サムネ生成の段階)。
            // それまでは空にしておき、画面には元のメディアを出す。先に設定すると、途中で止まったときに
            // 存在しないサムネを指したまま残る
            let kind = if jpg.is_some() { ThumbKind::Image } else { ThumbKind::Video };
            thumb_jobs.push(ThumbJob {
                src: src.path.clone(),
                rel,
                kind,
                member: row.member.clone(),
                msg_id: row.msg_id,
            });
            thumb_rel = None;
        }
    }

    // 本文ファイルを読めなかったときは保存済みの本文を使う(一時的なロックで本文と検索索引を消さない)
    let body = if row.body_unreadable { prev.as_ref().and_then(|p| p.body.clone()) } else { row.body.clone() };
    let has_body = body.as_deref().is_some_and(|b| !b.is_empty());
    let replace_fts = |conn: &Connection| -> rusqlite::Result<()> {
        let rowid: Option<i64> = conn
            .prepare_cached("SELECT rowid FROM messages WHERE member=? AND msg_id=?")?
            .query_row(params![row.member, row.msg_id], |r| r.get(0))
            .optional()?;
        if let Some(rowid) = rowid {
            conn.prepare_cached("DELETE FROM messages_fts WHERE rowid=?")?.execute([rowid])?;
            if has_body {
                conn.prepare_cached("INSERT INTO messages_fts(rowid, body, member, msg_id) VALUES (?, ?, ?, ?)")?
                    .execute(params![rowid, body, row.member, row.msg_id])?;
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
                row.member, row.group, row.msg_id, row.ts, row.ts_raw, body, row.media, row.kind, row.flag, thumb_rel
            ])?;
            replace_fts(conn)?;
            summary.new_count += 1;
        }
        Some(p) => {
            let changed = p.body != body
                || p.group != row.group
                || p.media != row.media
                || p.kind != row.kind
                || p.flag != row.flag
                || p.missing != 0
                || p.ts_raw != row.ts_raw
                || (thumb_decided && p.thumb != thumb_rel);
            if changed {
                conn.prepare_cached(
                    "UPDATE messages SET group_=?, ts=?, ts_raw=?, body=?, media=?, kind=?, flag=?, thumb=?, missing=0
                     WHERE member=? AND msg_id=?",
                )?
                .execute(params![
                    row.group, row.ts, row.ts_raw, body, row.media, row.kind, row.flag, thumb_rel, row.member,
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
