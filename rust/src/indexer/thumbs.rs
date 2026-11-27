//! サムネイルの生成と保存。画像は image クレート、動画は ffmpeg(任意)。保存先は thumbs.db。
//!
//! 画像は EXIF の向き(スマホで縦に撮った写真など)を反映してから、長辺 `thumb_size` に収まるよう縮小し
//! (拡大はしない)、RGB の JPEG(品質 `thumb_quality`)にする。
//!
//! 生成は呼び出し側が並列に行い、できた JPEG は1本の書き込みスレッド(`write_outcomes`)が thumbs.db にまとめて書く。
//! SQLite は同時に1つしか書き込めないが、1枚(約10KB)の書き込みは生成(数十〜数百ms)よりずっと短いので、
//! 書き込みを1本にしても生成の並列性はほぼ落ちない。

use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbKind {
    Image,
    Video,
}

#[derive(Debug, Clone)]
pub struct ThumbJob {
    pub src: PathBuf,
    /// thumbs.db のキー(= `messages.thumb`)
    pub rel: String,
    pub kind: ThumbKind,
}

/// 長辺を `size` に収める縮小後の寸法。収まっていれば None(縮小しない)。
/// 端数は floor/ceil のうち縦横比の誤差が小さい方(同じなら floor)。
pub fn thumbnail_dims(w: u32, h: u32, size: u32) -> Option<(u32, u32)> {
    if size >= w && size >= h {
        return None;
    }
    let aspect = w as f64 / h as f64;
    let s = size as f64;
    let round_aspect = |n: f64, key: &dyn Fn(f64) -> f64| -> u32 {
        let (lo, hi) = (n.floor(), n.ceil());
        let pick = if key(hi) < key(lo) { hi } else { lo };
        (pick as u32).max(1)
    };
    if 1.0 >= aspect {
        Some((round_aspect(s * aspect, &|n| (aspect - n / s).abs()), size))
    } else {
        Some((size, round_aspect(s / aspect, &|n| if n == 0.0 { 0.0 } else { (aspect - s / n).abs() })))
    }
}

fn gen_image_thumb(src: &Path, size: u32, quality: u8) -> Result<Vec<u8>, String> {
    let reader = image::ImageReader::open(src)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    // EXIF の向きが読めなければ、そのままの向きで作る(サムネが作れないよりよい)
    let orientation = image::ImageDecoder::orientation(&mut decoder).unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let (w, h) = (img.width(), img.height());
    let (tw, th) = thumbnail_dims(w, h, size).unwrap_or((w, h));
    let rgb = if (tw, th) == (w, h) {
        img.to_rgb8()
    } else {
        // thumbnail_exact は縮小専用の面積平均で、CatmullRom の resize_exact より大きな写真でずっと速い
        img.thumbnail_exact(tw, th).to_rgb8()
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb).map_err(|e| e.to_string())?;
    Ok(out)
}

/// ffmpeg で1フレーム抜き出し、JPEG を標準出力から受け取る(一時ファイルを作らない)。
/// 1秒未満の動画では `-ss 1` が拾えないことがあるので `-ss 0` で再試行する。
fn gen_video_thumb(src: &Path, size: u32, ffmpeg: Option<&Path>) -> Result<Vec<u8>, String> {
    let ffmpeg = ffmpeg.ok_or("ffmpeg not found (PATH にも bin/ にも見つかりません)")?;
    let run = |ss: &str| {
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-ss", ss, "-i"])
            .arg(src)
            .args(["-frames:v", "1", "-vf", &format!("scale={size}:-1"), "-q:v", "3"])
            .args(["-f", "image2pipe", "-c:v", "mjpeg", "-"])
            .stdin(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: サーバから呼んでも黒い窓を出さない
        }
        run_with_timeout(cmd, std::time::Duration::from_secs(30))
    };
    let made = |out: &Result<(bool, Vec<u8>, String), String>| matches!(out, Ok((true, data, _)) if !data.is_empty());
    let mut out = run("1");
    if !made(&out) {
        out = run("0");
    }
    match out {
        Ok((true, data, _)) if !data.is_empty() => Ok(data),
        Ok((_, _, stderr)) => {
            let tail: String = stderr.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect();
            Err(format!("ffmpeg failed: {tail}"))
        }
        Err(e) => Err(e),
    }
}

/// 子プロセスを実行し、制限時間を過ぎたら止める。(成功したか, 標準出力, 標準エラー)
fn run_with_timeout(mut cmd: Command, limit: std::time::Duration) -> Result<(bool, Vec<u8>, String), String> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    // 両方を別スレッドで読み切る(片方だけ読むと、もう片方のパイプが詰まって子プロセスが止まる)
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err("ffmpeg timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    Ok((status.success(), out_reader.join().unwrap_or_default(), err_reader.join().unwrap_or_default()))
}

/// 元ファイルの大きさと更新日時(`{len}:{mtime_ns}`)。作れなかったサムネの印に使う。
pub fn source_signature(src: &Path) -> Option<String> {
    let m = std::fs::metadata(src).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some(format!("{}:{mtime}", m.len()))
}

/// 1件生成し、JPEG のバイト列を返す。失敗しても取り込み全体は止めない(呼び出し側で警告を出す)。
pub fn generate(job: &ThumbJob, size: u32, quality: u8, ffmpeg: Option<&Path>) -> Result<Vec<u8>, String> {
    let data = match job.kind {
        ThumbKind::Image => gen_image_thumb(&job.src, size, quality),
        ThumbKind::Video => gen_video_thumb(&job.src, size, ffmpeg),
    }?;
    if data.is_empty() {
        return Err("empty thumbnail".into());
    }
    Ok(data)
}

// ---------------------------------------------------------------- thumbs.db

/// 使えるサムネが thumbs.db に既にあるか。
pub fn is_stored(conn: &Connection, rel: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM thumbs WHERE path=? AND length(data) > 0")?
        .query_row([rel], |_| Ok(()))
        .optional()?
        .is_some())
}

/// 同じ元ファイル(大きさ・更新日時が同じ)から前回作れなかったか。
/// 壊れた画像や読めない動画を、取り込みのたびに作り直そうとしない(動画は1件で最大60秒かかる)。
/// 元ファイルが差し替えられれば、印と食い違うので作り直す。
pub fn failed_before(conn: &Connection, rel: &str, src: &Path) -> rusqlite::Result<bool> {
    let mark: Option<String> = conn
        .prepare_cached("SELECT sig FROM thumb_failures WHERE path=?")?
        .query_row([rel], |r| r.get(0))
        .optional()?;
    Ok(matches!((mark, source_signature(src)), (Some(m), Some(s)) if m == s))
}

/// 生成の結果。書き込みスレッドへ送る。
pub enum Outcome {
    Made { rel: String, data: Vec<u8> },
    /// 作れなかった。`sig` は元ファイルの印(読めなければ None で、印を残さない)
    Failed { rel: String, sig: Option<String> },
}

fn to_ns(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos().min(i64::MAX as u128) as i64)
}

fn store(conn: &Connection, o: &Outcome) -> rusqlite::Result<()> {
    match o {
        Outcome::Made { rel, data } => {
            conn.prepare_cached(
                "INSERT INTO thumbs (path, data, updated_at) VALUES (?, ?, ?)
                 ON CONFLICT(path) DO UPDATE SET data=excluded.data, updated_at=excluded.updated_at",
            )?
            .execute(params![rel, data, to_ns(std::time::SystemTime::now())])?;
            conn.prepare_cached("DELETE FROM thumb_failures WHERE path=?")?.execute([rel])?;
        }
        Outcome::Failed { rel, sig: Some(sig) } => {
            conn.prepare_cached("INSERT OR REPLACE INTO thumb_failures (path, sig) VALUES (?, ?)")?
                .execute(params![rel, sig])?;
        }
        Outcome::Failed { sig: None, .. } => {}
    }
    Ok(())
}

/// 1つのトランザクションにまとめる最大件数。
const WRITE_BATCH: usize = 200;

/// `f` を1つのトランザクションで実行する(失敗したら巻き戻す)。
fn in_transaction(conn: &Connection, f: impl FnOnce() -> rusqlite::Result<()>) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN")?;
    match f() {
        Ok(()) => conn.execute_batch("COMMIT"),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// 書き込みスレッドの本体。送り手がすべて閉じるまで、届いた結果を thumbs.db に書く。
/// 届いている分をまとめて1つのトランザクションにする(1件ずつ commit すると遅い)。届くのを待たずに commit するので、
/// 取り込み中でもできたサムネから画面に出る。
pub fn write_outcomes(conn: &Connection, rx: &Receiver<Outcome>) -> rusqlite::Result<()> {
    while let Ok(first) = rx.recv() {
        in_transaction(conn, || {
            store(conn, &first)?;
            for _ in 1..WRITE_BATCH {
                match rx.try_recv() {
                    Ok(o) => store(conn, &o)?,
                    Err(_) => break,
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

// ---------------------------------------------------------------- 旧版のサムネフォルダの取り込み

const LEGACY_IMPORTED_KEY: &str = "legacy_thumbs_imported";

/// 旧版のサムネフォルダから取り込んだ件数。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LegacyImport {
    pub thumbs: u64,
    pub failures: u64,
    /// 読めなかったファイル・フォルダの数。1つでもあれば「取り込み済み」にせず、次回また試す
    pub errors: u64,
}

/// フォルダの下のファイルを再帰的に集める(読めないものは数えて飛ばす)。
fn walk_files(dir: &Path, out: &mut Vec<PathBuf>, errors: &mut u64) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        *errors += 1;
        return;
    };
    for e in rd {
        let Ok(e) = e else {
            *errors += 1;
            continue;
        };
        match e.file_type() {
            Ok(t) if t.is_dir() => walk_files(&e.path(), out, errors),
            Ok(t) if t.is_file() => out.push(e.path()),
            Ok(_) => {}
            Err(_) => *errors += 1,
        }
    }
}

/// `base` からの相対パスを `/` 区切りで(thumbs.db のキーの形)。
fn rel_key(base: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(base).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

/// 旧版のフォルダのファイル1つを取り込む。
fn import_legacy_file(conn: &Connection, dir: &Path, p: &Path, res: &mut LegacyImport) -> rusqlite::Result<()> {
    let Some(rel) = rel_key(dir, p) else { return Ok(()) };
    if rel.ends_with(".part.jpg") {
        return Ok(()); // 旧版が書きかけで残したもの
    }
    if let Some(stem) = rel.strip_suffix(".failed") {
        match std::fs::read_to_string(p) {
            Ok(sig) => {
                res.failures += conn
                    .prepare_cached("INSERT OR IGNORE INTO thumb_failures (path, sig) VALUES (?, ?)")?
                    .execute(params![format!("{stem}.jpg"), sig.trim()])? as u64;
            }
            Err(_) => res.errors += 1,
        }
    } else if rel.ends_with(".jpg") {
        match std::fs::read(p).and_then(|d| Ok((d, std::fs::metadata(p)?.modified()?))) {
            Ok((data, _)) if data.is_empty() => {} // 旧版が失敗して残した0バイト(取り込みで作り直す)
            Ok((data, mtime)) => {
                res.thumbs += conn
                    .prepare_cached("INSERT OR IGNORE INTO thumbs (path, data, updated_at) VALUES (?, ?, ?)")?
                    .execute(params![rel, data, to_ns(mtime)])? as u64;
            }
            Err(_) => res.errors += 1,
        }
    }
    Ok(())
}

/// 旧版(0.1.x)のサムネフォルダ(`{sha1[:2]}/{sha1}/{msg_id}.jpg` と、作れなかった印 `{msg_id}.failed`)を
/// thumbs.db に取り込む。取り込み済みなら何もせず None。
///
/// - 既に thumbs.db にあるものは上書きしない(取り込みと同時に動いても、新しく作ったサムネを古いもので消さない)
/// - 旧フォルダは消さない(旧版に戻したときに使えるように。不要になったら利用者が消す)
/// - すべて読めたら `meta` に印を付け、次からはフォルダを見ない
pub fn import_legacy_dir(conn: &Connection, dir: &Path) -> rusqlite::Result<Option<LegacyImport>> {
    let done: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key=?", [LEGACY_IMPORTED_KEY], |r| r.get(0))
        .optional()?;
    if done.is_some() {
        return Ok(None);
    }
    let mut res = LegacyImport::default();
    let mut files = Vec::new();
    if dir.is_dir() {
        walk_files(dir, &mut files, &mut res.errors);
    }
    for chunk in files.chunks(500) {
        in_transaction(conn, || {
            for p in chunk {
                import_legacy_file(conn, dir, p, &mut res)?;
            }
            Ok(())
        })?;
    }
    if res.errors == 0 {
        conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?, '1')", [LEGACY_IMPORTED_KEY])?;
    }
    Ok(Some(res))
}

/// ffmpeg の場所。探索順: 設定ファイルのディレクトリ → 実行ファイルのディレクトリ(それぞれ `bin/` → 直下) → PATH。
/// (実行ファイルは設定ファイルと別の場所に置かれうるため両方を見る)
pub fn find_ffmpeg(base_dir: &Path) -> Option<PathBuf> {
    let names = ["ffmpeg.exe", "ffmpeg"];
    let mut dirs = vec![base_dir.join("bin"), base_dir.to_path_buf()];
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir.join("bin"));
        dirs.push(exe_dir);
    }
    for d in &dirs {
        for n in names {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for d in std::env::split_paths(&path) {
        for n in names {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(thumbnail_dims(64, 48, 240), None); // 拡大しない
        assert_eq!(thumbnail_dims(1920, 1080, 240), Some((240, 135)));
        assert_eq!(thumbnail_dims(1080, 1920, 240), Some((135, 240)));
        assert_eq!(thumbnail_dims(1000, 1000, 240), Some((240, 240)));
        assert_eq!(thumbnail_dims(5000, 3, 240), Some((240, 1)));
        assert_eq!(thumbnail_dims(333, 777, 240), Some((103, 240)));
        assert_eq!(thumbnail_dims(241, 240, 240), Some((240, 239)));
        assert_eq!(thumbnail_dims(799, 601, 240), Some((240, 181)));
    }

    /// JPEG の SOI の直後に、向き(Orientation)だけを持つ EXIF(APP1)を差し込む。
    fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut tiff = b"MM\0\x2a\0\0\0\x08".to_vec(); // ビッグエンディアン、IFD0 はオフセット8
        tiff.extend([0, 1]); // 項目数 1
        tiff.extend([0x01, 0x12, 0, 3, 0, 0, 0, 1]); // Orientation, SHORT, 1個
        tiff.extend(orientation.to_be_bytes());
        tiff.extend([0, 0]);
        tiff.extend([0, 0, 0, 0]); // 次の IFD なし
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff);
        let mut out = jpeg[..2].to_vec(); // SOI
        out.extend([0xFF, 0xE1]);
        out.extend(((app1.len() + 2) as u16).to_be_bytes());
        out.extend(app1);
        out.extend(&jpeg[2..]);
        out
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mv_thumb_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mem_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(crate::db::THUMBS_DB_SCHEMA_SQL).unwrap();
        c
    }

    fn job(src: PathBuf) -> ThumbJob {
        ThumbJob { src, rel: "ab/abcd/1.jpg".into(), kind: ThumbKind::Image }
    }

    /// EXIF の向きを反映する(縦に撮った写真が横倒しのサムネにならない)。
    #[test]
    fn applies_exif_orientation() {
        let dir = temp_dir("exif");
        // 横長 400x200。左半分が赤、右半分が青
        let img = image::RgbImage::from_fn(400, 200, |x, _| if x < 200 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95).encode_image(&img).unwrap();
        // 6 = 時計回りに90度回して表示する
        let src = dir.join("rot.jpg");
        std::fs::write(&src, with_exif_orientation(&jpeg, 6)).unwrap();
        let data = generate(&job(src), 240, 90, None).unwrap();
        let t = image::load_from_memory(&data).unwrap().to_rgb8();
        assert_eq!((t.width(), t.height()), (120, 240)); // 縦長になる
        // 回した後は上半分が赤、下半分が青
        let (top, bottom) = (t.get_pixel(60, 30), t.get_pixel(60, 210));
        assert!(top[0] > 200 && top[2] < 60, "top={top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "bottom={bottom:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn image_roundtrip() {
        let dir = temp_dir("roundtrip");
        let src = dir.join("src.jpg");
        image::RgbImage::from_pixel(800, 600, image::Rgb([200, 50, 50])).save(&src).unwrap();
        let data = generate(&job(src), 240, 80, None).unwrap();
        let t = image::load_from_memory(&data).unwrap();
        assert_eq!((t.width(), t.height()), (240, 180));
        assert!(generate(&job(dir.join("missing.jpg")), 240, 80, None).is_err());
        std::fs::write(dir.join("broken.jpg"), b"\xFF\xD8\xFF\xE0 not really a jpeg").unwrap();
        assert!(generate(&job(dir.join("broken.jpg")), 240, 80, None).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 書き込みスレッド: 作れたものは保存し、作れなかったものは印を残す。同じ元ファイルなら作り直さない。
    /// 元ファイルが変われば作り直し、作れたら印は消える。
    #[test]
    fn writer_stores_thumbs_and_failure_marks() {
        let dir = temp_dir("writer");
        let src = dir.join("broken.jpg");
        std::fs::write(&src, b"broken").unwrap();
        let conn = mem_db();
        let rel = "ab/abcd/1.jpg".to_string();
        assert!(!is_stored(&conn, &rel).unwrap());
        assert!(!failed_before(&conn, &rel, &src).unwrap());

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Outcome::Failed { rel: rel.clone(), sig: source_signature(&src) }).unwrap();
        tx.send(Outcome::Made { rel: "ab/abcd/2.jpg".into(), data: vec![1, 2, 3] }).unwrap();
        tx.send(Outcome::Failed { rel: "ab/abcd/3.jpg".into(), sig: None }).unwrap();
        drop(tx);
        write_outcomes(&conn, &rx).unwrap();
        assert!(failed_before(&conn, &rel, &src).unwrap());
        assert!(is_stored(&conn, "ab/abcd/2.jpg").unwrap());
        assert!(!failed_before(&conn, "ab/abcd/3.jpg", &src).unwrap(), "元ファイルが読めなければ印を残さない");

        // 元ファイルを差し替えると作り直しの対象になる
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&src, b"replaced, longer than before").unwrap();
        assert!(!failed_before(&conn, &rel, &src).unwrap(), "差し替えられた元ファイルは作り直す");

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Outcome::Made { rel: rel.clone(), data: vec![9] }).unwrap();
        drop(tx);
        write_outcomes(&conn, &rx).unwrap();
        assert!(is_stored(&conn, &rel).unwrap());
        let marks: i64 = conn.query_row("SELECT count(*) FROM thumb_failures", [], |r| r.get(0)).unwrap();
        assert_eq!(marks, 0, "作れたら印は消える");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 旧版のサムネフォルダを取り込む。既にあるものは上書きせず、取り込み済みなら2回目は何もしない。
    #[test]
    fn imports_legacy_dir_once() {
        let dir = temp_dir("legacy");
        let member_dir = dir.join("ab").join("abcd");
        std::fs::create_dir_all(&member_dir).unwrap();
        std::fs::write(member_dir.join("1.jpg"), b"old-1").unwrap();
        std::fs::write(member_dir.join("2.jpg"), b"old-2").unwrap();
        std::fs::write(member_dir.join("3.failed"), b"10:123\n").unwrap();
        std::fs::write(member_dir.join("4.jpg"), b"").unwrap(); // 0バイトは取り込まない
        std::fs::write(member_dir.join("5.part.jpg"), b"partial").unwrap(); // 書きかけも取り込まない
        let conn = mem_db();
        conn.execute("INSERT INTO thumbs (path, data, updated_at) VALUES ('ab/abcd/2.jpg', x'01', 1)", []).unwrap();

        let r = import_legacy_dir(&conn, &dir).unwrap();
        assert_eq!(r, Some(LegacyImport { thumbs: 1, failures: 1, errors: 0 }));
        let data = |p: &str| -> Option<Vec<u8>> {
            conn.query_row("SELECT data FROM thumbs WHERE path=?", [p], |r| r.get(0)).optional().unwrap()
        };
        assert_eq!(data("ab/abcd/1.jpg"), Some(b"old-1".to_vec()));
        assert_eq!(data("ab/abcd/2.jpg"), Some(vec![1]), "新しく作ったものを古いもので上書きしない");
        assert_eq!(data("ab/abcd/4.jpg"), None);
        assert_eq!(data("ab/abcd/5.part.jpg"), None);
        let sig: String = conn.query_row("SELECT sig FROM thumb_failures WHERE path='ab/abcd/3.jpg'", [], |r| r.get(0)).unwrap();
        assert_eq!(sig, "10:123");
        assert!(member_dir.join("1.jpg").exists(), "旧フォルダは消さない");

        assert_eq!(import_legacy_dir(&conn, &dir).unwrap(), None, "取り込み済み");
        // フォルダが無ければ0件で済ませる
        let conn = mem_db();
        assert_eq!(import_legacy_dir(&conn, &dir.join("none")).unwrap(), Some(LegacyImport::default()));
        assert_eq!(import_legacy_dir(&conn, &dir).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
