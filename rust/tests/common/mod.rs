//! HTTP 契約テストの共通基盤。
//!
//! ビルドしたサーバ(`message-viewer`)を一時ディレクトリ(サンドボックス)内で起動し、HTTP だけで検証する。
//! 本番の `config-dev.json` / `data/` には絶対に触れない(設定ファイルも保存フォルダもサンドボックス内に作る)。
//!
//! HTTP クライアントは、Host ヘッダの差し替え・不正な Range など細かい指定が要るため、標準ライブラリで最小限に書いている。

#![allow(dead_code)] // テストファイルごとに使う関数が違う

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// サンプルの member_a の msg_id → ts。ts_raw は UTC として解釈される(例: 20260801000000 → 1785542400)。
pub fn ts(msg_id: i64) -> i64 {
    match msg_id {
        1 => 1785542400, // 2026-08-01T00:00Z
        2 => 1785596400, // 2026-08-01T15:00Z = JST 8/2 00:00
        3 | 4 => 1785931200, // 3 と 4 は同じ秒
        5 => 1785974400,
        6 => 1786060800,
        7 => 1786147200,
        8 => 1786233600,
        9 => 1786320000,
        _ => panic!("no sample msg {msg_id}"),
    }
}

pub const MESSAGE_KEYS: [&str; 12] =
    ["member", "group", "msg_id", "ts", "ts_raw", "body", "kind", "flag", "missing", "media_url", "thumb_url", "comment"];

pub const MP4: &str = "/media/member_a/2026/202608/7_2_20260808000000.mp4";

/// ダミー動画の中身(1024バイト)。
pub fn mp4_content() -> Vec<u8> {
    (0..4).flat_map(|_| 0u8..=255).collect()
}

/// サムネの URL(`/thumbs/{sha1[:2]}/{sha1}/{msg_id}.jpg`)。
pub fn thumb_url(member: &str, msg_id: i64) -> String {
    use sha1::{Digest, Sha1};
    let h = format!("{:x}", Sha1::digest(member.as_bytes()));
    format!("/thumbs/{}/{h}/{msg_id}.jpg", &h[..2])
}

fn make_jpeg(path: &Path, color: [u8; 3]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(64, 48, image::Rgb(color)).save(path).unwrap();
}

fn write_text(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// 検証用の保存フォルダを作る。
///
/// - member_a: `{member}/{YYYY}/{YYYYMM}/` 構成
/// - member_b: `{group}/{member}/{file}` 構成(グループ付き・直下)
pub fn build_sample_root(root: &Path) {
    let a = root.join("member_a/2026/202608");
    write_text(&a.join("1_0_20260801000000.txt"), "おはようございます");
    write_text(&a.join("2_0_20260801150000.txt"), "ライブありがとう！ 100%楽しかった"); // JST では 8/2 0時
    write_text(&a.join("3_0_20260805120000.txt"), "ライブは中止になりました");
    write_text(&a.join("4_0_20260805120000.txt"), "同じ秒のメッセージ <b>tag</b> & ライブ");
    write_text(&a.join("5_0_20260806000000.txt"), "あ"); // 1文字(LIKE経路)
    make_jpeg(&a.join("6_1_20260807000000.jpg"), [200, 50, 50]);
    std::fs::write(a.join("7_2_20260808000000.mp4"), mp4_content()).unwrap(); // ダミー動画(サムネは作られない)
    write_text(&a.join("8_0_20260809000000.txt"), "写真のコメント");
    make_jpeg(&a.join("8_1_20260809000000.jpg"), [10, 200, 10]); // 本文付き画像(同じ msg_id)
    std::fs::write(a.join("9_3_20260810000000.m4a"), b"\x00\x01\x02").unwrap();

    let b = root.join("grpX/member_b");
    write_text(&b.join("1_0_20260802000000.txt"), "bさんの最初のメッセージ");
    write_text(&b.join("2_0_20260803000000.txt"), "ライブ楽しみ");
}

// ------------------------------------------------------------------ HTTP

pub struct Resp {
    pub status: u16,
    /// 名前は小文字
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not json ({e}): status={} {}", self.status, String::from_utf8_lossy(&self.body)))
    }
    pub fn header(&self, name: &str) -> &str {
        self.headers.get(name).map(String::as_str).unwrap_or_else(|| panic!("no header {name}: {:?}", self.headers))
    }
}

/// クエリ文字列(`?a=1&b=x`)。値は URL エンコードする。
pub fn qs(params: &[(&str, &str)]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let mut s = form_urlencoded::Serializer::new(String::new());
    for (k, v) in params {
        s.append_pair(k, v);
    }
    format!("?{}", s.finish())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn decode_chunked(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let eol = find(b, b"\r\n").expect("chunk size line");
        let size_str = std::str::from_utf8(&b[..eol]).unwrap();
        let size = usize::from_str_radix(size_str.split(';').next().unwrap().trim(), 16).unwrap();
        b = &b[eol + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

/// 1回のリクエスト(`Connection: close`)。`headers` に Host があれば既定の Host を付けない。
pub fn raw_request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    let body = body.unwrap_or_default();
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "DELETE") {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();

    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let head_end = find(&buf, b"\r\n\r\n").expect("response head");
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let rest = &buf[head_end + 4..];
    let body = if headers.get("transfer-encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked")) {
        decode_chunked(rest)
    } else {
        rest.to_vec()
    };
    Resp { status, headers, body }
}

// ------------------------------------------------------------------ サーバ

static SANDBOX_SEQ: AtomicUsize = AtomicUsize::new(0);

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), to).unwrap();
        }
    }
}

pub struct Server {
    pub port: u16,
    pub sandbox: PathBuf,
    /// 保存フォルダ(サンプル)
    pub root: PathBuf,
    child: Child,
    /// drop でサンドボックスを消すか(`stop_keep` で止めたときは残す)
    cleanup: bool,
}

impl Server {
    /// 素のサーバ(root 未設定・取り込みなし)。
    pub fn start() -> Server {
        Self::launch(Self::new_sandbox(), false)
    }

    /// サンプルを全期間取り込んだサーバ。
    pub fn indexed() -> Server {
        let srv = Self::launch(Self::new_sandbox(), true);
        srv.reindex_full_ok();
        srv
    }

    fn new_sandbox() -> PathBuf {
        let n = SANDBOX_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("mv_apitest_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        build_sample_root(&dir.join("messages"));
        copy_dir(&project_root().join("static"), &dir.join("static"));
        dir
    }

    fn config_path(sandbox: &Path) -> PathBuf {
        sandbox.join("config.json")
    }

    /// 設定ファイルを書いて起動する。`with_root` なら保存フォルダを設定済みにする(取り込みはしない)。
    fn launch(sandbox: PathBuf, with_root: bool) -> Server {
        let port = free_port();
        let root = sandbox.join("messages");
        let cfg_path = Self::config_path(&sandbox);
        let mut cfg = match std::fs::read_to_string(&cfg_path) {
            Ok(t) => serde_json::from_str::<Value>(&t).unwrap(),
            Err(_) => json!({
                "root": "",
                "data_dir": "./data",
                "thumbs_dir": "./data/thumbs",
                "db_path": "./data/index.db",
                "port": 0,
                "thumb_size": 240,
                "thumb_quality": 80,
                "incremental_months": 2,
                "auto_index_on_startup": false,
            }),
        };
        cfg["port"] = json!(port);
        if with_root {
            cfg["root"] = json!(root.to_string_lossy());
        }
        std::fs::write(&cfg_path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
        assert!(!sandbox.join("config-dev.json").exists());

        let log = std::fs::File::create(sandbox.join("server.log")).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_message-viewer"));
        cmd.arg("--config").arg(&cfg_path).current_dir(&sandbox).stdout(log.try_clone().unwrap()).stderr(log);
        // 利用者環境の設定を拾わない。PATH 上の ffmpeg で動画サムネの有無が変わらないよう PATH も絞る
        for (k, _) in std::env::vars() {
            if k.starts_with("MSGVIEWER_") {
                cmd.env_remove(k);
            }
        }
        let system_dir = std::env::var("SystemRoot").map(|r| format!(r"{r}\System32")).unwrap_or_default();
        cmd.env("PATH", system_dir);
        let child = cmd.spawn().unwrap();

        let mut srv = Server { port, sandbox, root, child, cleanup: true };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(st) = srv.child.try_wait().unwrap() {
                panic!("server exited early ({st}):\n{}", srv.log());
            }
            if TcpStream::connect(("127.0.0.1", port)).is_ok() && srv.get("/").status == 200 {
                return srv;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server did not start:\n{}", srv.log());
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.sandbox.join("server.log")).unwrap_or_default()
    }

    pub fn config_json(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(Self::config_path(&self.sandbox)).unwrap()).unwrap()
    }

    pub fn data_dir(&self) -> PathBuf {
        self.sandbox.join("data")
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    // ---------------------------------------------------------------- リクエスト

    pub fn get(&self, path: &str) -> Resp {
        raw_request(self.port, "GET", path, &[], None)
    }
    pub fn get_h(&self, path: &str, headers: &[(&str, &str)]) -> Resp {
        raw_request(self.port, "GET", path, headers, None)
    }
    pub fn get_q(&self, path: &str, params: &[(&str, &str)]) -> Resp {
        self.get(&format!("{path}{}", qs(params)))
    }
    /// GET して 200 を確かめ、JSON を返す。
    pub fn get_json(&self, path: &str, params: &[(&str, &str)]) -> Value {
        let r = self.get_q(path, params);
        assert_eq!(r.status, 200, "GET {path} {params:?}: {}", String::from_utf8_lossy(&r.body));
        r.json()
    }
    pub fn send_json(&self, method: &str, path: &str, body: &Value) -> Resp {
        self.send_raw(method, path, &serde_json::to_vec(body).unwrap())
    }
    pub fn send_raw(&self, method: &str, path: &str, body: &[u8]) -> Resp {
        raw_request(self.port, method, path, &[("Content-Type", "application/json")], Some(body))
    }
    pub fn post(&self, path: &str, body: &Value) -> Resp {
        self.send_json("POST", path, body)
    }
    pub fn put(&self, path: &str, body: &Value) -> Resp {
        self.send_json("PUT", path, body)
    }
    pub fn delete(&self, path: &str) -> Resp {
        raw_request(self.port, "DELETE", path, &[], None)
    }
    pub fn post_q(&self, path: &str, params: &[(&str, &str)]) -> Resp {
        raw_request(self.port, "POST", &format!("{path}{}", qs(params)), &[], None)
    }

    // ---------------------------------------------------------------- 取り込み

    pub fn wait_reindex_idle(&self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let st = self.get_json("/api/reindex/status", &[]);
            if st["running"] == json!(false) {
                return st;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("reindex did not finish:\n{}", self.log());
    }

    /// 全期間を取り込み、エラーなく終わったことを確かめる。サマリを返す。
    pub fn reindex_full_ok(&self) -> Value {
        let r = self.post_q("/api/reindex", &[("mode", "full")]);
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let st = self.wait_reindex_idle();
        assert_eq!(st["last_error"], Value::Null, "{st}");
        st["last_summary"].clone()
    }

    /// 保存フォルダを /api/settings で設定してから全期間を取り込む(初期化後の再設定など)。
    pub fn configure_and_index(&self) {
        let r = self.post("/api/settings", &json!({ "root": self.root.to_string_lossy() }));
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        self.wait_reindex_idle();
        self.reindex_full_ok();
    }

    /// サーバを止める。サンドボックス(data/ と config.json)は残し、`restart` で起動し直せる。
    pub fn stop_keep(mut self) -> PathBuf {
        self.kill();
        self.cleanup = false;
        self.sandbox.clone()
    }

    /// `stop_keep` で止めたサンドボックスのまま起動し直す。
    pub fn restart(sandbox: PathBuf) -> Server {
        Self::launch(sandbox, false)
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
        if self.cleanup && !std::thread::panicking() {
            // 失敗したテストは調査できるようサンドボックスを残す
            let _ = std::fs::remove_dir_all(&self.sandbox);
        }
    }
}

/// msg_id の並び。
pub fn ids(items: &Value) -> Vec<i64> {
    items.as_array().unwrap().iter().map(|m| m["msg_id"].as_i64().unwrap()).collect()
}

/// (member, msg_id) の並び。
pub fn pairs(items: &Value) -> Vec<(String, i64)> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["member"].as_str().unwrap().to_string(), m["msg_id"].as_i64().unwrap()))
        .collect()
}

pub fn p(member: &str, msg_id: i64) -> (String, i64) {
    (member.to_string(), msg_id)
}

/// オブジェクトのキー集合。
pub fn keys(v: &Value) -> std::collections::BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

pub fn key_set(names: &[&str]) -> std::collections::BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}
