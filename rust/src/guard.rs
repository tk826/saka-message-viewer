//! 外部サイトからのアクセス対策と、スロークエリログ。
//!
//! - DNSリバインディング: Host が localhost 系以外なら拒否。
//! - CSRF: Sec-Fetch-Site が cross-site/same-site のものは、トップレベルのGET遷移以外を拒否。
//!   古いブラウザ向けに、変更系メソッドは Origin が `http://{Host}` と一致しなければ拒否。
//! - クリックジャッキング: すべての応答に `X-Frame-Options: DENY` と `frame-ancestors 'none'` を付ける。

use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::time::Instant;

const ALLOWED_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

/// Host ヘッダからポートを除いたホスト名を返す(`[::1]:8000` にも対応)。
pub fn host_name(host_header: &str) -> String {
    let h = host_header.trim().to_lowercase();
    if h.starts_with('[') {
        return match h.find(']') {
            Some(i) => h[..=i].to_string(),
            None => h,
        };
    }
    h.split(':').next().unwrap_or("").to_string()
}

fn reject(msg: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({ "detail": msg }))).into_response()
}

pub async fn guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let mut res = check_and_run(st, req, next).await;
    // クリックジャッキング対策: 他サイトの iframe に埋め込ませない(埋め込まれた画面上で
    // 初期化・メンバー削除などのボタンを押させる攻撃を防ぐ)
    let h = res.headers_mut();
    h.insert(header::X_FRAME_OPTIONS, header::HeaderValue::from_static("DENY"));
    h.insert(header::CONTENT_SECURITY_POLICY, header::HeaderValue::from_static("frame-ancestors 'none'"));
    res
}

async fn check_and_run(st: AppState, req: Request, next: Next) -> Response {
    let headers = req.headers();
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if !ALLOWED_HOSTS.contains(&host_name(&host).as_str()) {
        return reject("invalid host");
    }

    let method = req.method().clone();
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    if matches!(site, Some("cross-site") | Some("same-site")) {
        // iframe への読み込みも mode=navigate になるため、行き先がトップレベルの文書のときだけ許す
        let dest = headers.get("sec-fetch-dest").and_then(|v| v.to_str().ok());
        let is_navigation = method == Method::GET
            && headers.get("sec-fetch-mode").and_then(|v| v.to_str().ok()) == Some("navigate")
            && dest.is_none_or(|d| d == "document");
        if !is_navigation {
            return reject("cross-site request rejected");
        }
    }

    let safe = matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe
        && let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && origin != format!("http://{host}")
    {
        return reject("cross-origin request rejected");
    }

    if !st.is_dev() {
        return next.run(req).await;
    }
    // 開発環境(config-dev.json)のみ、1秒以上かかったリクエストを stderr に出す
    let path = req.uri().path().to_string();
    let t0 = Instant::now();
    let res = next.run(req).await;
    let elapsed = t0.elapsed().as_secs_f64();
    if elapsed >= 1.0 {
        logln!("[slow] {method} {path} took {elapsed:.1}s");
    }
    res
}

#[cfg(test)]
mod tests {
    use super::host_name;

    #[test]
    fn host_names() {
        assert_eq!(host_name("localhost:8000"), "localhost");
        assert_eq!(host_name("127.0.0.1"), "127.0.0.1");
        assert_eq!(host_name("[::1]:8000"), "[::1]");
        assert_eq!(host_name("[::1]"), "[::1]");
        assert_eq!(host_name(" LocalHost "), "localhost");
        assert_eq!(host_name("evil.example.com"), "evil.example.com");
        assert_eq!(host_name(""), "");
    }
}
