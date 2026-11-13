//! サムネイル生成。画像は image クレート、動画は ffmpeg(任意)。
//!
//! 画像は EXIF の向き(スマホで縦に撮った写真など)を反映してから、長辺 `thumb_size` に収まるよう縮小し
//! (拡大はしない)、RGB の JPEG(品質 `thumb_quality`)で保存する。

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbKind {
    Image,
    Video,
}

#[derive(Debug, Clone)]
pub struct ThumbJob {
    pub src: PathBuf,
    pub dst: PathBuf,
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

fn gen_image_thumb(src: &Path, dst: &Path, size: u32, quality: u8) -> Result<(), String> {
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
    let file = std::fs::File::create(dst).map_err(|e| e.to_string())?;
    let mut w = std::io::BufWriter::new(file);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, quality).encode_image(&rgb).map_err(|e| e.to_string())?;
    // BufWriter の drop は書き出しの失敗を握りつぶすため、明示的に書き切る
    std::io::Write::flush(&mut w).map_err(|e| e.to_string())
}

/// ffmpeg で1フレーム抜き出す。1秒未満の動画では `-ss 1` が拾えないことがあるので `-ss 0` で再試行する。
fn gen_video_thumb(src: &Path, dst: &Path, size: u32, ffmpeg: Option<&Path>) -> Result<(), String> {
    let ffmpeg = ffmpeg.ok_or("ffmpeg not found (PATH にも bin/ にも見つかりません)")?;
    let run = |ss: &str| {
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-y", "-ss", ss, "-i"])
            .arg(src)
            .args(["-frames:v", "1", "-vf", &format!("scale={size}:-1"), "-q:v", "3"])
            .arg(dst)
            .stdin(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: サーバから呼んでも黒い窓を出さない
        }
        run_with_timeout(cmd, std::time::Duration::from_secs(30))
    };
    let mut out = run("1");
    if !matches!(&out, Ok((true, _))) || !thumb_exists(dst) {
        out = run("0");
    }
    match out {
        Ok((true, _)) if thumb_exists(dst) => Ok(()),
        Ok((_, stderr)) => {
            let tail: String = stderr.chars().rev().take(500).collect::<Vec<_>>().into_iter().rev().collect();
            Err(format!("ffmpeg failed: {tail}"))
        }
        Err(e) => Err(e),
    }
}

/// 子プロセスを実行し、制限時間を過ぎたら止める。(成功したか, 標準エラー)
fn run_with_timeout(mut cmd: Command, limit: std::time::Duration) -> Result<(bool, String), String> {
    let mut child = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
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
    Ok((status.success(), reader.join().unwrap_or_default()))
}

/// 書きかけのサムネの置き場所(`6.jpg` → `6.part.jpg`)。ffmpeg は拡張子で出力形式を決めるため `.jpg` で終える。
fn temp_path(dst: &Path) -> PathBuf {
    let stem = dst.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    dst.with_file_name(format!("{stem}.part.jpg"))
}

/// 使えるサムネが既にあるか。0バイト(以前の版が書き込み途中で失敗して残したもの)は無いものとして作り直す。
pub fn thumb_exists(dst: &Path) -> bool {
    std::fs::metadata(dst).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// 1件生成する。失敗しても取り込み全体は止めない(呼び出し側で警告を出す)。
///
/// 一時ファイルに書き上げてから置き換える。直接書くと、失敗やタイムアウトで書きかけのファイルが残り、
/// 「既にあるサムネは作り直さない」ため壊れたサムネが表示され続ける。
pub fn generate(job: &ThumbJob, size: u32, quality: u8, ffmpeg: Option<&Path>) -> Result<(), String> {
    if let Some(parent) = job.dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = temp_path(&job.dst);
    // 前回の中断で残った書きかけを、今回の出力と取り違えないよう先に消す
    let _ = std::fs::remove_file(&tmp);
    let res = match job.kind {
        ThumbKind::Image => gen_image_thumb(&job.src, &tmp, size, quality),
        ThumbKind::Video => gen_video_thumb(&job.src, &tmp, size, ffmpeg),
    }
    .and_then(|()| std::fs::rename(&tmp, &job.dst).map_err(|e| e.to_string()));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
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

    /// EXIF の向きを反映する(縦に撮った写真が横倒しのサムネにならない)。
    #[test]
    fn applies_exif_orientation() {
        let dir = std::env::temp_dir().join(format!("mv_thumb_exif_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 横長 400x200。左半分が赤、右半分が青
        let img = image::RgbImage::from_fn(400, 200, |x, _| if x < 200 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95).encode_image(&img).unwrap();
        // 6 = 時計回りに90度回して表示する
        let src = dir.join("rot.jpg");
        std::fs::write(&src, with_exif_orientation(&jpeg, 6)).unwrap();
        let job = ThumbJob { src, dst: dir.join("rot_t.jpg"), kind: ThumbKind::Image };
        generate(&job, 240, 90, None).unwrap();
        let t = image::open(&job.dst).unwrap().to_rgb8();
        assert_eq!((t.width(), t.height()), (120, 240)); // 縦長になる
        // 回した後は上半分が赤、下半分が青
        let (top, bottom) = (t.get_pixel(60, 30), t.get_pixel(60, 210));
        assert!(top[0] > 200 && top[2] < 60, "top={top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "bottom={bottom:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn image_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mv_thumb_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.jpg");
        image::RgbImage::from_pixel(800, 600, image::Rgb([200, 50, 50])).save(&src).unwrap();
        let job = ThumbJob { src, dst: dir.join("out/t.jpg"), kind: ThumbKind::Image };
        generate(&job, 240, 80, None).unwrap();
        let t = image::open(&job.dst).unwrap();
        assert_eq!((t.width(), t.height()), (240, 180));
        assert!(thumb_exists(&job.dst));
        assert!(!temp_path(&job.dst).exists(), "一時ファイルは置き換えで消える");
        let bad = ThumbJob { src: dir.join("missing.jpg"), dst: dir.join("x.jpg"), kind: ThumbKind::Image };
        assert!(generate(&bad, 240, 80, None).is_err());
        // 壊れた元画像: 失敗しても書きかけ(本体・一時ファイルとも)を残さない
        std::fs::write(dir.join("broken.jpg"), b"\xFF\xD8\xFF\xE0 not really a jpeg").unwrap();
        let broken = ThumbJob { src: dir.join("broken.jpg"), dst: dir.join("b.jpg"), kind: ThumbKind::Image };
        assert!(generate(&broken, 240, 80, None).is_err());
        assert!(!broken.dst.exists() && !temp_path(&broken.dst).exists());
        // 0バイトのサムネは「無い」扱い(作り直しの対象)
        std::fs::write(dir.join("empty.jpg"), b"").unwrap();
        assert!(!thumb_exists(&dir.join("empty.jpg")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
