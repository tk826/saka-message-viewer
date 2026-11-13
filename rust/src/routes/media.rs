//! /media, /thumbs のファイル配信(Range対応)と /api/media-path。
//! (docs/api-spec.md 第4章「GET /media/{path}」)
//!
//! `tower-http::ServeFile` は 416 の扱い・`bytes=-N` の超過・不正ヘッダの扱いが仕様と異なるため自前実装する。

use crate::config::normalize_lexical;
use crate::error::{blocking, ApiError, ApiResult};
use crate::params::Params;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path as AxPath, RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use axum::body::Bytes;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 1回に送るチャンクの大きさ。hyper は1チャンクごとに書き込みが走るため、小さいと遅い
/// (256MBの全体取得を release ビルドで計測: 1MB≈130MB/s、2MB≈190MB/s、4MB≈200MB/s)。
const CHUNK: usize = 2 * 1024 * 1024;
/// 送信待ちに積むチャンク数の上限。1ストリームのメモリ上限は CHUNK*QUEUE(=4MB)。遅いクライアントでは読み込み側が待つ。
const QUEUE: usize = 2;

// ------------------------------------------------------------------ パス解決

/// Windows の拡張パス接頭辞(`\\?\`)を外す。`canonicalize` が付けるが、利用者に見せるパスには不要。
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    p
}

/// パスを解決する。存在する最長の祖先までは実体(シンボリックリンク解決済み)にし、
/// 残りは字句的に連結する。存在しないパスでも失敗しない。
fn resolve_nonstrict(p: &Path) -> PathBuf {
    let norm = normalize_lexical(p);
    let mut existing = norm.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(&existing) {
            let mut out = c;
            for r in rest.iter().rev() {
                out.push(r);
            }
            return strip_verbatim(out);
        }
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return norm,
        }
    }
}

/// パストラバーサル対策: 解決後のパスが `base` の外に出ていれば403。実在チェックはしない。
pub fn safe_resolve_no_exist_check(base: &Path, rel: &str) -> ApiResult<PathBuf> {
    let candidate = resolve_nonstrict(&base.join(rel));
    let base_resolved = resolve_nonstrict(base);
    if !candidate.starts_with(&base_resolved) {
        return Err(ApiError::forbidden("invalid path"));
    }
    Ok(candidate)
}

/// `safe_resolve_no_exist_check` に加えて、存在しない/ファイルでなければ404。
fn safe_resolve(base: &Path, rel: &str) -> ApiResult<PathBuf> {
    let p = safe_resolve_no_exist_check(base, rel)?;
    if !p.is_file() {
        return Err(ApiError::not_found("file not found"));
    }
    Ok(p)
}

// ------------------------------------------------------------------ Range配信

fn guess_mime(p: &Path) -> &'static str {
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("mp4") => "video/mp4",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// 10進数字列を飽和させながら数値にする(巨大な値は u64::MAX 扱い。範囲外として正しく判定される)。
fn parse_sat(s: &str) -> u64 {
    s.bytes().fold(0u64, |acc, b| acc.saturating_mul(10).saturating_add((b - b'0') as u64))
}

/// Range ヘッダ `bytes=(\d*)-(\d*)` を先頭一致で解釈する(後続の文字は無視。複数範囲は先頭のみ)。
/// 一致しなければ None。
fn match_range(h: &str) -> Option<(&str, &str)> {
    let rest = h.strip_prefix("bytes=")?;
    let start_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    let after = rest[start_len..].strip_prefix('-')?;
    let end_len = after.bytes().take_while(u8::is_ascii_digit).count();
    Some((&rest[..start_len], &after[..end_len]))
}

fn range_error(size: u64, detail: &str, with_content_range: bool) -> Response {
    let mut res = (StatusCode::RANGE_NOT_SATISFIABLE, Json(json!({ "detail": detail }))).into_response();
    if with_content_range {
        res.headers_mut()
            .insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes */{size}")).unwrap());
    }
    res
}

/// ファイルの [start, start+len) を、専用のブロッキングスレッドで読みながら送るストリームにする。
/// `tokio::fs::File` はチャンクごとにブロッキングプールとの往復が入り、大きな動画で遅くなるため使わない。
fn file_body(mut file: std::fs::File, start: u64, len: u64) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(QUEUE);
    tokio::task::spawn_blocking(move || {
        if start > 0 {
            if let Err(e) = file.seek(SeekFrom::Start(start)) {
                let _ = tx.blocking_send(Err(e));
                return;
            }
        }
        let mut remaining = len;
        while remaining > 0 {
            let n = remaining.min(CHUNK as u64) as usize;
            let mut buf = vec![0u8; n];
            match file.read(&mut buf) {
                Ok(0) => break, // 送信中にファイルが縮んだ
                Ok(k) => {
                    buf.truncate(k);
                    remaining -= k as u64;
                    if tx.blocking_send(Ok(Bytes::from(buf))).is_err() {
                        break; // クライアントが切断(シーク操作など)
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    break;
                }
            }
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) });
    Body::from_stream(stream)
}

/// ブラウザのキャッシュの扱い。
#[derive(Clone, Copy)]
enum CachePolicy {
    /// サムネ: パスは (member, msg_id) で決まり中身はまず変わらないため、一定時間は問い合わせずに使ってよい
    Thumb,
    /// 元のメディア: 保存フォルダ側で差し替えられうるため、毎回 ETag で確かめる(変わっていなければ304で本文を送らない)
    Media,
}

impl CachePolicy {
    fn header(self) -> &'static str {
        match self {
            CachePolicy::Thumb => "max-age=86400",
            CachePolicy::Media => "no-cache",
        }
    }
}

/// キャッシュの検証子。ETag は「サイズ-更新時刻(ns)」、Last-Modified は HTTP 日付。
struct Validators {
    etag: String,
    last_modified: Option<String>,
}

impl Validators {
    fn new(meta: &std::fs::Metadata) -> Self {
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
        let etag = format!("\"{:x}-{:x}\"", meta.len(), mtime.map(|d| d.as_nanos()).unwrap_or(0));
        let last_modified = mtime
            .and_then(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0))
            .map(|t| t.format("%a, %d %b %Y %H:%M:%S GMT").to_string());
        Validators { etag, last_modified }
    }

    /// 条件付きリクエスト(If-None-Match / If-Modified-Since)が「手元のもので最新」を示しているか。
    /// If-None-Match があればそれだけで判定する(RFC 9110)。日付は送り返された値との一致だけを見る。
    fn not_modified(&self, headers: &HeaderMap) -> bool {
        let get = |h| headers.get(h).and_then(|v: &HeaderValue| v.to_str().ok());
        if let Some(inm) = get(header::IF_NONE_MATCH) {
            return inm.split(',').map(str::trim).any(|t| t == "*" || t.trim_start_matches("W/") == self.etag);
        }
        matches!((get(header::IF_MODIFIED_SINCE), &self.last_modified), (Some(a), Some(b)) if a == b)
    }

    /// If-Range が付いていれば、それが今のファイルと一致するときだけ Range を使ってよい(違えば全体を返す)。
    fn range_allowed(&self, headers: &HeaderMap) -> bool {
        match headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok()) {
            None => true,
            Some(v) => v == self.etag || self.last_modified.as_deref() == Some(v),
        }
    }

    fn apply(&self, b: axum::http::response::Builder, policy: CachePolicy) -> axum::http::response::Builder {
        let b = b.header(header::ETAG, &self.etag).header(header::CACHE_CONTROL, policy.header());
        match &self.last_modified {
            Some(lm) => b.header(header::LAST_MODIFIED, lm),
            None => b,
        }
    }
}

async fn serve_file_with_range(headers: &HeaderMap, path: &Path, policy: CachePolicy) -> ApiResult<Response> {
    let p = path.to_path_buf();
    let opened = blocking(move || -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
        let f = std::fs::File::open(&p)?;
        let meta = f.metadata()?;
        Ok((f, meta))
    })
    .await?;
    let (file, meta) = opened.map_err(|_| ApiError::not_found("file not found"))?;
    let size = meta.len();
    let mime = guess_mime(path);
    let v = Validators::new(&meta);

    if v.not_modified(headers) {
        return Ok(v.apply(Response::builder().status(StatusCode::NOT_MODIFIED), policy).body(Body::empty()).unwrap());
    }

    let range = headers
        .get(header::RANGE)
        .filter(|_| v.range_allowed(headers))
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
    let Some(range) = range else {
        let body = file_body(file, 0, size);
        let b = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, size);
        return Ok(v.apply(b, policy).body(body).unwrap());
    };

    let Some((start_s, end_s)) = match_range(&range) else {
        return Ok(range_error(size, "invalid range header", false));
    };
    if start_s.is_empty() && end_s.is_empty() {
        return Ok(range_error(size, "invalid range header", false));
    }
    // 計算は i128 で行う(size==0 のとき end=-1 になるため)
    let size_i = size as i128;
    let (start, mut end): (i128, i128) = if start_s.is_empty() {
        let length = parse_sat(end_s) as i128;
        ((size_i - length).max(0), size_i - 1)
    } else {
        let s = parse_sat(start_s) as i128;
        let e = if end_s.is_empty() { size_i - 1 } else { parse_sat(end_s) as i128 };
        (s, e)
    };
    end = end.min(size_i - 1);
    if start > end || start >= size_i {
        return Ok(range_error(size, "range not satisfiable", true));
    }

    let (start, end) = (start as u64, end as u64);
    let len = end - start + 1;
    let body = file_body(file, start, len);
    let b = Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, len);
    Ok(v.apply(b, policy).body(body).unwrap())
}

/// パス解決(ファイルシステムを触る)をブロッキングスレッドで行う。
async fn resolve_blocking(base: PathBuf, rel: String) -> ApiResult<PathBuf> {
    blocking(move || safe_resolve(&base, &rel)).await?
}

// ------------------------------------------------------------------ ハンドラ

pub async fn get_media(
    State(st): State<AppState>,
    AxPath(path): AxPath<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let Some(root) = st.root() else {
        return Err(ApiError::detail(StatusCode::CONFLICT, "root is not configured"));
    };
    let file = resolve_blocking(root, path).await?;
    serve_file_with_range(&headers, &file, CachePolicy::Media).await
}

pub async fn get_thumb(
    State(st): State<AppState>,
    AxPath(path): AxPath<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let thumbs_dir = st.cfg.read().unwrap().thumbs_dir.clone();
    let file = resolve_blocking(thumbs_dir, path).await?;
    serve_file_with_range(&headers, &file, CachePolicy::Thumb).await
}

/// 「パスをコピー」用。欠損(missing)していても絶対パスは返し、`exists` で有無を知らせる。
pub async fn media_path(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<serde_json::Value>> {
    let p = Params::parse(q);
    let member = p.required("member")?;
    let msg_id = p.required_int("msg_id")?;

    let media: Option<String> = st
        .db
        .run(move |c| {
            let mut stmt = c.prepare("SELECT media FROM messages WHERE member=? AND msg_id=?")?;
            let mut rows = stmt.query(rusqlite::params![member, msg_id])?;
            match rows.next()? {
                Some(r) => r.get::<_, Option<String>>(0),
                None => Ok(None),
            }
        })
        .await?;
    let Some(media) = media.filter(|m| !m.is_empty()) else {
        return Err(ApiError::not_found("message not found or has no media"));
    };
    let Some(root) = st.root() else {
        return Err(ApiError::detail(StatusCode::CONFLICT, "root is not configured"));
    };

    let (path, exists) = blocking(move || -> ApiResult<(PathBuf, bool)> {
        let p = safe_resolve_no_exist_check(&root, &media)?;
        let exists = p.is_file();
        Ok((p, exists))
    })
    .await??;
    Ok(Json(json!({ "path": path.display().to_string(), "exists": exists })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_matching() {
        assert_eq!(match_range("bytes=0-4"), Some(("0", "4")));
        assert_eq!(match_range("bytes=-10"), Some(("", "10")));
        assert_eq!(match_range("bytes=5-"), Some(("5", "")));
        assert_eq!(match_range("bytes=-"), Some(("", "")));
        assert_eq!(match_range("bytes=0-4,10-12"), Some(("0", "4")));
        assert_eq!(match_range("bytes=2-3xyz"), Some(("2", "3")));
        assert_eq!(match_range("bytes=abc"), None);
        assert_eq!(match_range("BYTES=0-1"), None);
        assert_eq!(match_range("items=0-1"), None);
    }

    #[test]
    fn saturating_parse() {
        assert_eq!(parse_sat("123"), 123);
        assert_eq!(parse_sat("99999999999999999999999"), u64::MAX);
    }

    #[test]
    fn mime() {
        assert_eq!(guess_mime(Path::new("a.JPG")), "image/jpeg");
        assert_eq!(guess_mime(Path::new("a.tar.gz")), "application/octet-stream");
        assert_eq!(guess_mime(Path::new("a.m4a")), "application/octet-stream");
        assert_eq!(guess_mime(Path::new(".jpg")), "application/octet-stream");
    }

    #[test]
    fn verbatim() {
        assert_eq!(strip_verbatim(PathBuf::from(r"\\?\C:\a\b")), PathBuf::from(r"C:\a\b"));
        assert_eq!(strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share\x")), PathBuf::from(r"\\srv\share\x"));
        assert_eq!(strip_verbatim(PathBuf::from(r"C:\a")), PathBuf::from(r"C:\a"));
    }

    #[test]
    fn traversal_is_rejected_even_if_missing() {
        let base = std::env::temp_dir().join(format!("mv_traversal_test_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        assert!(safe_resolve_no_exist_check(&base, "../x").is_err());
        assert!(safe_resolve_no_exist_check(&base, "a/../../x").is_err());
        assert!(safe_resolve_no_exist_check(&base, "a/../b.jpg").is_ok());
        assert!(safe_resolve_no_exist_check(&base, "no/such/file.jpg").is_ok());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
