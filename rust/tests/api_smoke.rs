//! 起動・設定・外部アクセス拒否の契約。

mod common;

use common::*;
use serde_json::json;

#[test]
fn index_html() {
    let srv = Server::start();
    let r = srv.get("/");
    assert_eq!(r.status, 200);
    assert!(r.header("content-type").contains("text/html"));
}

#[test]
fn static_js() {
    let srv = Server::start();
    assert_eq!(srv.get("/static/js/app.js").status, 200);
}

/// サーバのログは画面だけでなく data/server_log.txt にも追記する(起動ごとに区切り行)。
#[test]
fn server_log_file() {
    let srv = Server::start();
    srv.get("/api/threads");
    let log = std::fs::read_to_string(srv.data_dir().join("server_log.txt")).unwrap();
    assert!(log.contains("起動 =========="), "{log}");
    assert!(log.contains(&format!("listening on http://127.0.0.1:{}", srv.port)), "{log}");
}

/// 二重起動(ポートが使用中)は、案内を出して終了コード1で終わる。既に動いているサーバには影響しない。
#[test]
fn second_instance_reports_port_in_use() {
    let srv = Server::start();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_message-viewer"))
        .arg("--config")
        .arg(srv.sandbox.join("config.json"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(&format!("ポート {} は既に使用されています", srv.port)), "{stderr}");
    assert!(stderr.contains(&format!("http://localhost:{}/", srv.port)), "{stderr}");
    // 2つ目は DB もログファイルも開く前に終わる(ログに2回目の起動が記録されない)
    let log = std::fs::read_to_string(srv.data_dir().join("server_log.txt")).unwrap();
    assert_eq!(log.matches("起動 ==========").count(), 1, "{log}");
    assert_eq!(srv.get("/api/threads").status, 200);
}

#[test]
fn settings_unconfigured() {
    let srv = Server::start();
    let d = srv.get_json("/api/settings", &[]);
    assert_eq!(d["root"], json!(null));
    assert_eq!(d["configured"], json!(false));
    assert_eq!(keys(&d), key_set(&["root", "configured", "is_dev", "ffmpeg_available", "ffmpeg_path"]));
    assert_eq!(d["is_dev"], json!(false));
}

#[test]
fn settings_validation() {
    let srv = Server::start();
    assert_eq!(srv.post("/api/settings", &json!({ "root": "" })).status, 400);
    assert_eq!(srv.post("/api/settings", &json!({ "root": "relative/dir" })).status, 400);
    let nope = srv.sandbox.join("nope");
    assert_eq!(srv.post("/api/settings", &json!({ "root": nope.to_string_lossy() })).status, 400);
}

#[test]
fn settings_saves_config_and_starts_reindex() {
    let srv = Server::start();
    let root = srv.root.to_string_lossy().into_owned();
    let r = srv.post("/api/settings", &json!({ "root": root }));
    assert_eq!(r.status, 200);
    let d = r.json();
    assert_eq!(d["configured"], json!(true));
    assert!(d["root"].as_str().unwrap().replace('\\', "/").ends_with("/messages"));
    srv.wait_reindex_idle();
    assert_eq!(srv.config_json()["root"], json!(root)); // config.json に書き戻される
}

#[test]
fn foreign_host_rejected() {
    let srv = Server::start();
    let r = srv.get_h("/api/threads", &[("Host", "evil.example.com")]);
    assert_eq!(r.status, 403);
    assert_eq!(r.json(), json!({ "detail": "invalid host" }));
}

#[test]
fn allowed_hosts() {
    let srv = Server::start();
    for h in ["localhost:8000", "127.0.0.1", "[::1]:8000"] {
        assert_eq!(srv.get_h("/api/threads", &[("Host", h)]).status, 200, "{h}");
    }
}

#[test]
fn cross_site_rejected() {
    let srv = Server::start();
    let cross = [("Sec-Fetch-Site", "cross-site")];
    assert_eq!(raw_request(srv.port, "POST", "/api/reset", &cross, None).status, 403);
    assert_eq!(srv.get_h("/api/threads", &cross).status, 403);
    // トップレベルの遷移だけは許可
    let nav = [("Sec-Fetch-Site", "cross-site"), ("Sec-Fetch-Mode", "navigate")];
    assert_eq!(srv.get_h("/", &nav).status, 200);
    assert_eq!(srv.get_h("/api/threads", &nav).status, 200);
}

#[test]
fn origin_mismatch_rejected_for_writes() {
    let srv = Server::start();
    // 検証エラー(400)になる入力。外部アクセス対策を通過できたかだけを見る
    let body = br#"{"root": ""}"#;
    let post = |origin: Option<&str>| {
        let mut h = vec![("Content-Type", "application/json")];
        if let Some(o) = origin {
            h.push(("Origin", o));
        }
        raw_request(srv.port, "POST", "/api/settings", &h, Some(body)).status
    };
    assert_eq!(post(Some("http://evil.example.com")), 403);
    assert_eq!(post(Some(&srv.base_url())), 400);
    assert_eq!(post(None), 400); // Origin 無しは通す
}
