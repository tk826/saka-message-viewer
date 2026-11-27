//! メディア配信(Range・キャッシュ)と、取り込み・メンバー削除・初期化など管理系の契約。

mod common;

use common::*;
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ /media
#[test]
fn media_full_and_range() {
    let srv = Server::indexed();
    let content = mp4_content();
    let r = srv.get(MP4);
    assert_eq!((r.status, &r.body), (200, &content));
    assert_eq!(r.header("content-type"), "video/mp4");
    assert_eq!(r.header("accept-ranges"), "bytes");
    assert_eq!(r.header("content-length"), "1024");
    assert!(!r.headers.contains_key("content-range"));

    for (hdr, s, e) in [
        ("bytes=0-4", 0, 4),
        ("bytes=100-199", 100, 199),
        ("bytes=1000-", 1000, 1023),
        ("bytes=-10", 1014, 1023),       // 末尾10バイト
        ("bytes=1000-99999", 1000, 1023), // end はファイル末尾に丸める
        ("bytes=-99999", 0, 1023),       // 長さ超過は全体
        ("bytes=0-4,10-12", 0, 4),       // 複数範囲は先頭だけ
        ("bytes=2-3xyz", 2, 3),          // 前方一致なので末尾の余計な文字は無視
        ("bytes=0-99999999999999999999999", 0, 1023), // 巨大な数値
    ] {
        let r = srv.get_h(MP4, &[("Range", hdr)]);
        assert_eq!(r.status, 206, "{hdr}");
        assert_eq!(r.body, content[s..=e], "{hdr}");
        assert_eq!(r.header("content-range"), format!("bytes {s}-{e}/1024"), "{hdr}");
        assert_eq!(r.header("content-length"), (e - s + 1).to_string(), "{hdr}");
        assert_eq!(r.header("accept-ranges"), "bytes");
    }
}

#[test]
fn media_range_errors() {
    let srv = Server::indexed();
    let r = srv.get_h(MP4, &[("Range", "bytes=1024-")]);
    assert_eq!((r.status, r.header("content-range")), (416, "bytes */1024"));
    assert!(r.json().get("detail").is_some());
    for hdr in ["bytes=5-2", "bytes=-", "items=0-1", "bytes=99999999999999999999999-", "bytes=-0", "bytes=abc", "BYTES=0-1"] {
        assert_eq!(srv.get_h(MP4, &[("Range", hdr)]).status, 416, "{hdr}");
    }
}

#[test]
fn media_files_edge_cases() {
    let srv = Server::indexed();
    // 0バイトのファイル: 全体取得は200、Range 指定は416
    std::fs::write(srv.root.join("empty.bin"), b"").unwrap();
    let r = srv.get("/media/empty.bin");
    assert_eq!((r.status, r.body.len(), r.header("content-length")), (200, 0, "0"));
    assert_eq!(srv.get_h("/media/empty.bin", &[("Range", "bytes=0-0")]).status, 416);
    // MIME: 拡張子の大文字小文字、複数ドット、パーセントエンコードされた日本語・空白
    std::fs::write(srv.root.join("A.PNG"), b"x").unwrap();
    std::fs::write(srv.root.join("a.tar.gz"), b"x").unwrap();
    std::fs::write(srv.root.join("日本語 名前.JPEG"), b"x").unwrap();
    assert_eq!(srv.get("/media/A.PNG").header("content-type"), "image/png");
    assert_eq!(srv.get("/media/a.tar.gz").header("content-type"), "application/octet-stream");
    let r = srv.get("/media/%E6%97%A5%E6%9C%AC%E8%AA%9E%20%E5%90%8D%E5%89%8D.JPEG");
    assert_eq!((r.status, r.header("content-type")), (200, "image/jpeg"));
    let a = "/media/member_a/2026/202608/";
    assert_eq!(srv.get(&format!("{a}6_1_20260807000000.jpg")).header("content-type"), "image/jpeg");
    assert_eq!(srv.get(&format!("{a}9_3_20260810000000.m4a")).header("content-type"), "application/octet-stream");
    assert_eq!(srv.get(&format!("{a}1_0_20260801000000.txt")).header("content-type"), "text/plain; charset=utf-8");
}

#[test]
fn media_not_found_and_traversal() {
    let srv = Server::indexed();
    let r = srv.get("/media/nope.jpg");
    assert_eq!((r.status, r.json()), (404, json!({ "detail": "file not found" })));
    assert_eq!(srv.get("/media/member_a").status, 404); // ディレクトリ
    let r = srv.get("/media/..%2f..%2fconfig.json");
    assert_eq!((r.status, r.json()), (403, json!({ "detail": "invalid path" })));
    assert_eq!(srv.get("/media/%2e%2e/config.json").status, 403);
    assert_eq!(srv.get("/thumbs/..%2f..%2fconfig.json").status, 403);
    // 存在しない場所への脱出も、存在チェックより先に403
    assert_eq!(srv.get("/media/..%2f..%2fno_such_file_anywhere").status, 403);
}

/// UNC パス(`\\host\share`)は、外部ホストへ接続しに行く前に403にする
/// (以前は解決の途中で SMB 接続を試み、応答が無いホストだと40秒以上かかっていた)。
/// 192.0.2.1 は文書用に予約された、応答しないアドレス。
#[cfg(windows)]
#[test]
fn unc_path_is_rejected_without_network_access() {
    let srv = Server::indexed();
    for path in [
        "/media/%5C%5C192.0.2.1%5Cshare%5Cx.jpg",
        "/thumbs/%5C%5C192.0.2.1%5Cshare%5Cx.jpg",
        "/media/%2F%2F192.0.2.1%2Fshare%2Fx.jpg",
    ] {
        let t0 = Instant::now();
        assert_eq!(srv.get(path).status, 403, "{path}");
        assert!(t0.elapsed() < Duration::from_secs(5), "{path} took {:?}", t0.elapsed());
    }
}

#[test]
fn media_requires_root() {
    let srv = Server::start();
    assert_eq!(srv.get("/media/x.jpg").status, 409);
}

/// 元メディアは毎回 ETag で確かめ(no-cache)、変わっていなければ304で本文を送らない。
#[test]
fn media_cache_validators() {
    let srv = Server::indexed();
    let r = srv.get(MP4);
    let (etag, lm) = (r.header("etag").to_string(), r.header("last-modified").to_string());
    assert_eq!(r.header("cache-control"), "no-cache");
    let r = srv.get_h(MP4, &[("If-None-Match", &etag)]);
    assert_eq!((r.status, r.body.len(), r.header("etag")), (304, 0, etag.as_str()));
    assert_eq!(srv.get_h(MP4, &[("If-None-Match", &format!("W/{etag}"))]).status, 304);
    assert_eq!(srv.get_h(MP4, &[("If-None-Match", "\"other\"")]).status, 200);
    assert_eq!(srv.get_h(MP4, &[("If-Modified-Since", &lm)]).status, 304);
    // If-None-Match が外れていれば、日付が合っていても本文を返す
    assert_eq!(srv.get_h(MP4, &[("If-None-Match", "\"other\""), ("If-Modified-Since", &lm)]).status, 200);
    // 部分取得にも検証子が付く。If-Range が今のファイルと違えば全体を返す
    let r = srv.get_h(MP4, &[("Range", "bytes=0-4"), ("If-Range", &etag)]);
    assert_eq!((r.status, r.header("etag")), (206, etag.as_str()));
    let r = srv.get_h(MP4, &[("Range", "bytes=0-4"), ("If-Range", "\"stale\"")]);
    assert_eq!((r.status, r.body), (200, mp4_content()));
}

// ----------------------------------------------------------------- /thumbs
#[test]
fn thumbs_generated_and_served() {
    let srv = Server::indexed();
    let r = srv.get(&thumb_url("member_a", 6));
    assert_eq!((r.status, r.header("content-type")), (200, "image/jpeg"));
    // パスが (member, msg_id) で決まるため、一定時間は問い合わせずに使ってよい
    assert_eq!(r.header("cache-control"), "max-age=86400");
    assert!(!r.header("etag").is_empty());
    let im = image::load_from_memory(&r.body).unwrap();
    assert!(im.width().max(im.height()) <= 240); // thumb_size
    assert_eq!(srv.get(&thumb_url("member_a", 999)).status, 404);
}

/// サムネは thumbs.db に入り、旧版のようなサムネのフォルダは作らない。
#[test]
fn thumbs_are_stored_in_db() {
    let srv = Server::indexed();
    let data = srv.data_dir();
    assert!(!data.join("thumbs").exists(), "サムネのフォルダは作らない");
    let rel = thumb_url("member_a", 6).trim_start_matches("/thumbs/").to_string();
    let db = rusqlite::Connection::open(data.join("thumbs.db")).unwrap();
    let stored: Vec<u8> = db.query_row("SELECT data FROM thumbs WHERE path=?", [&rel], |r| r.get(0)).unwrap();
    assert_eq!(srv.get(&thumb_url("member_a", 6)).body, stored);
    // 部分取得・条件付き取得もできる
    let r = srv.get_h(&thumb_url("member_a", 6), &[("Range", "bytes=0-1")]);
    assert_eq!((r.status, r.body), (206, stored[..2].to_vec()));
    let etag = srv.get(&thumb_url("member_a", 6)).header("etag").to_string();
    assert_eq!(srv.get_h(&thumb_url("member_a", 6), &[("If-None-Match", &etag)]).status, 304);
}

/// 旧版のサムネのフォルダ(data/thumbs)は、起動時に thumbs.db へ取り込む。フォルダは消さない。
#[test]
fn legacy_thumbs_folder_is_imported_on_startup() {
    let srv = Server::indexed();
    let data = srv.data_dir();
    let sandbox = srv.stop_keep();
    // 取り込み済みの印を消し、旧版が残したフォルダがある状態にする
    let db = rusqlite::Connection::open(data.join("thumbs.db")).unwrap();
    db.execute("DELETE FROM meta", []).unwrap();
    db.execute("DELETE FROM thumbs WHERE path GLOB 'zz/*'", []).unwrap();
    drop(db);
    let legacy = data.join("thumbs").join("zz").join("zzzz");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("7.jpg"), b"legacy-jpeg").unwrap();
    std::fs::write(legacy.join("8.failed"), b"1:2").unwrap();

    let srv = Server::restart(sandbox);
    let t0 = Instant::now();
    let r = loop {
        let r = srv.get("/thumbs/zz/zzzz/7.jpg");
        if r.status == 200 || t0.elapsed() > Duration::from_secs(10) {
            break r;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!((r.status, r.body), (200, b"legacy-jpeg".to_vec()));
    assert!(legacy.join("7.jpg").exists(), "旧フォルダは消さない");
    assert!(srv.log().contains("imported 1 thumbnails and 1 failure marks"), "{}", srv.log());
    // 既存のサムネも引き続き出る
    assert_eq!(srv.get(&thumb_url("member_a", 6)).status, 200);
}

// ------------------------------------------------------------- media-path
#[test]
fn media_path() {
    let srv = Server::indexed();
    let d = srv.get_json("/api/media-path", &[("member", "member_a"), ("msg_id", "6")]);
    assert_eq!(d["exists"], json!(true));
    let path = d["path"].as_str().unwrap().replace('\\', "/");
    assert!(path.ends_with("/messages/member_a/2026/202608/6_1_20260807000000.jpg"), "{path}");
    assert_eq!(srv.get_q("/api/media-path", &[("member", "member_a"), ("msg_id", "1")]).status, 404); // メディア無し
    assert_eq!(srv.get_q("/api/media-path", &[("member", "member_a"), ("msg_id", "999")]).status, 404);
    assert_eq!(srv.get_q("/api/media-path", &[("member", "member_a")]).status, 422);

    // 取り込み直さなくても、欠損したファイルは exists=false で返す
    std::fs::remove_file(srv.root.join("member_a/2026/202608/6_1_20260807000000.jpg")).unwrap();
    let d = srv.get_json("/api/media-path", &[("member", "member_a"), ("msg_id", "6")]);
    assert_eq!(d["exists"], json!(false));
    let path = d["path"].as_str().unwrap();
    assert!(path.replace('\\', "/").ends_with("/messages/member_a/2026/202608/6_1_20260807000000.jpg"));
    assert!(!path.starts_with(r"\\?\")); // Windows の拡張パス接頭辞が付かない
}

/// メンバー名・ファイル名に `#`・`%XX`・空白があっても、media_url でそのファイルを取得できる
/// (`#` 以降がフラグメントになったり、`%41` がサーバ側で `A` に戻ったりしない)。
#[test]
fn media_url_is_percent_encoded() {
    let srv = Server::indexed();
    let member = "m#1%41 x";
    let dir = srv.root.join(member);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("1_2_20260801000000.mp4"), b"video-bytes").unwrap();
    srv.reindex_full_ok();

    let msgs = srv.get_json("/api/messages", &[("member", member)]);
    let url = msgs["messages"][0]["media_url"].as_str().unwrap().to_string();
    assert_eq!(url, "/media/m%231%2541%20x/1_2_20260801000000.mp4");
    let r = srv.get(&url);
    assert_eq!((r.status, r.body.as_slice()), (200, b"video-bytes".as_slice()));
}

// --------------------------------------------------------------- reindex
#[test]
fn reindex_requires_root() {
    let srv = Server::start();
    let r = srv.post_q("/api/reindex", &[]);
    assert_eq!((r.status, r.json()), (409, json!({ "status": "root_not_configured" })));
}

#[test]
fn reindex_validation_and_start() {
    let srv = Server::indexed();
    assert_eq!(srv.post_q("/api/reindex", &[("mode", "bogus")]).status, 400);
    let r = srv.post_q("/api/reindex", &[("mode", "full"), ("member", "member_a")]);
    assert_eq!((r.status, r.json()), (200, json!({ "status": "started", "mode": "full", "member": "member_a" })));
    assert_eq!(srv.wait_reindex_idle()["last_error"], Value::Null);
    let r = srv.post_q("/api/reindex", &[]);
    assert_eq!(r.json(), json!({ "status": "started", "mode": "incremental", "member": null }));
    srv.wait_reindex_idle();
}

/// `member` の空文字・空白だけは未指定(全メンバー)扱い。該当0件のまま "started" にしない。
#[test]
fn reindex_empty_member_means_all() {
    let srv = Server::indexed();
    for m in ["", " "] {
        let r = srv.post_q("/api/reindex", &[("mode", "full"), ("member", m)]);
        assert_eq!((r.status, r.json()), (200, json!({ "status": "started", "mode": "full", "member": null })));
        let st = srv.wait_reindex_idle();
        assert_eq!(st["last_error"], Value::Null);
        // 全メンバーを走査した(該当0件で何もしなかったのではない)
        let total: i64 = srv.get_json("/api/threads", &[]).as_array().unwrap().iter().map(|t| t["count"].as_i64().unwrap()).sum();
        assert_eq!(st["last_summary"]["total_messages_scanned"], json!(total));
    }
}

#[test]
fn reindex_status_shape_and_idempotent() {
    let srv = Server::indexed();
    let st = srv.get_json("/api/reindex/status", &[]);
    assert_eq!(keys(&st), key_set(&["running", "started_at", "finished_at", "progress", "last_summary", "last_error"]));
    assert_eq!((st["running"].clone(), st["last_error"].clone()), (json!(false), Value::Null));
    assert_eq!(
        keys(&st["last_summary"]),
        key_set(&[
            "new_count", "updated_count", "skipped_count", "missing_count", "thumb_count", "elapsed_sec",
            "total_files_scanned", "total_messages_scanned",
        ])
    );
    // data/progress.json の内容が載る
    let pr = &st["progress"];
    assert_eq!((pr["phase"].clone(), pr["running"].clone(), pr["percent"].clone()), (json!("done"), json!(false), json!(100.0)));
    assert_eq!(pr["source"], json!("file"));
    assert!(pr["pid"].is_i64());

    // もう一度全期間を取り込むと、取り込み済みの11件(member_a 9 + member_b 2)はすべてスキップ
    let s = srv.reindex_full_ok();
    assert_eq!((s["new_count"].clone(), s["skipped_count"].clone()), (json!(0), json!(11)));
    assert_eq!(s["total_messages_scanned"], json!(11));
}

#[test]
fn reindex_detects_new_and_missing() {
    let srv = Server::indexed();
    let dir = srv.root.join("member_a/2026/202608");
    std::fs::write(dir.join("10_0_20260811000000.txt"), "追加メッセージ").unwrap();
    std::fs::remove_file(dir.join("9_3_20260810000000.m4a")).unwrap();
    let s = srv.reindex_full_ok();
    assert_eq!((s["new_count"].clone(), s["missing_count"].clone()), (json!(1), json!(1)));
    // 欠損は DB に残し missing=true
    let d = srv.get_json("/api/messages", &[("member", "member_a"), ("limit", "2")]);
    let got: Vec<(i64, bool)> = d["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["msg_id"].as_i64().unwrap(), m["missing"].as_bool().unwrap()))
        .collect();
    assert_eq!(got, [(9, true), (10, false)]);
    // missing は media-list から除く
    assert_eq!(srv.get_json("/api/media-list", &[("member", "member_a"), ("kind", "audio")])["items"], json!([]));
    // 追加した本文は検索できる(検索索引も更新)
    assert_eq!(srv.get_json("/api/search", &[("q", "追加メッセージ")])["total"], json!(1));
}

// ------------------------------------------------------------ メンバー削除
#[test]
fn delete_member() {
    let srv = Server::indexed();
    srv.post("/api/favorites", &json!({ "member": "member_a", "msg_id": 1 }));
    srv.put("/api/comments", &json!({ "member": "member_a", "msg_id": 1, "comment": "x" }));
    srv.post("/api/favorites", &json!({ "member": "member_b", "msg_id": 1 }));
    srv.put("/api/hidden-members", &json!({ "member": "member_a", "hidden": true }));
    let thumb = thumb_url("member_a", 6);
    assert_eq!(srv.get(&thumb).status, 200);

    let r = srv.delete("/api/members/member_a");
    assert_eq!((r.status, r.json()), (200, json!({ "status": "ok", "member": "member_a", "deleted_messages": 9 })));
    let threads = srv.get_json("/api/threads", &[]);
    assert_eq!(threads.as_array().unwrap().iter().map(|t| t["member"].clone()).collect::<Vec<_>>(), [json!("member_b")]);
    assert_eq!(srv.get_json("/api/messages", &[("member", "member_a")])["messages"], json!([]));
    assert_eq!(pairs(&srv.get_json("/api/favorites", &[])["favorites"]), [p("member_b", 1)]);
    assert_eq!(srv.get(&thumb).status, 404); // サムネも消す
    assert_eq!(srv.get_json("/api/search", &[("q", "おはよう")])["total"], json!(0)); // 検索にも出ない
    assert!(srv.root.join("member_a").exists()); // 保存フォルダ側は触らない
    // 表示設定(非表示・並び順)は消さない
    assert_eq!(srv.get_json("/api/hidden-members", &[]), json!({ "members": ["member_a"] }));
    // 存在しないメンバー・日本語名でも成功(0件)
    assert_eq!(srv.delete("/api/members/ghost").json()["deleted_messages"], json!(0));
    let r = srv.delete("/api/members/%E5%B9%BD%E9%9C%8A");
    assert_eq!(r.json(), json!({ "status": "ok", "member": "幽霊", "deleted_messages": 0 }));
}

fn query_db<T: rusqlite::types::FromSql>(data_dir: &Path, sql: &str) -> T {
    let db = rusqlite::Connection::open(data_dir.join("index.db")).unwrap();
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// フォルダを別グループへ移すと、取り込み直したときにグループが更新される。
#[test]
fn group_change_is_reindexed() {
    let srv = Server::indexed();
    std::fs::rename(srv.root.join("grpX"), srv.root.join("grpY")).unwrap();
    srv.reindex_full_ok();
    let g: String = query_db(&srv.data_dir(), "SELECT group_concat(DISTINCT group_) FROM messages WHERE member='member_b'");
    assert_eq!(g, "grpY");
}

/// 本文ファイルを一時的に読めない(別プロセスがロック中)ときは、保存済みの本文と検索索引を消さない。
#[cfg(windows)]
#[test]
fn unreadable_text_keeps_stored_body() {
    use std::os::windows::fs::OpenOptionsExt;
    let srv = Server::indexed();
    let txt = srv.root.join("member_a/2026/202608/1_0_20260801000000.txt");
    // 共有なしで開いて、他プロセスから読めなくする
    let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&txt).unwrap();
    srv.reindex_full_ok();
    drop(held);
    let body: String = query_db(&srv.data_dir(), "SELECT body FROM messages WHERE member='member_a' AND msg_id=1");
    assert_eq!(body, "おはようございます");
    let d = srv.get_json("/api/search", &[("q", "おはようございます")]);
    assert_eq!(ids(&d["results"]), vec![1]);
}

/// 差分取り込みで、走査範囲の一番古い月の月初 0〜9時(JST)のメッセージを、走査していないのに missing にしない
/// (ファイル名の日時は UTC なので、そのファイルは前月扱いで走査範囲の外になる)。
#[test]
fn incremental_does_not_mark_month_boundary_as_missing() {
    use chrono::Datelike;
    let srv = Server::indexed();
    // incremental_months=2 の範囲で一番古い月(先月)の1日
    let now = chrono::Local::now();
    let (mut y, mut m) = (now.year(), now.month());
    m -= 1;
    if m == 0 {
        m = 12;
        y -= 1;
    }
    let first = chrono::NaiveDate::from_ymd_opt(y, m, 1).unwrap();
    // 前月末日の 20:00 UTC = JST では first の 5:00
    let dir = srv.root.join("grpX/member_b");
    let name = format!("10_0_{}200000.txt", first.pred_opt().unwrap().format("%Y%m%d"));
    std::fs::write(dir.join(name), "月初のメッセージ").unwrap();
    // 同じメンバーの、走査範囲内のメッセージ(これが無いとメンバーごと走査範囲に入らない)
    std::fs::write(dir.join(format!("11_0_{}000000.txt", first.format("%Y%m10"))), "範囲内").unwrap();
    srv.reindex_full_ok();

    let r = srv.post_q("/api/reindex", &[("mode", "incremental")]);
    assert_eq!(r.status, 200);
    let st = srv.wait_reindex_idle();
    assert_eq!(st["last_error"], Value::Null, "{st}");
    let missing: i64 = query_db(&srv.data_dir(), "SELECT missing FROM messages WHERE member='member_b' AND msg_id=10");
    assert_eq!(missing, 0);
}

/// 壊れた画像のサムネは、作れなかったら thumb を空にし、差分取り込みのたびに作り直そうとしない。
#[test]
fn broken_image_thumbnail_is_not_retried() {
    let srv = Server::indexed();
    // 差分取り込みの走査範囲に入るよう、今月の日時にする
    let now = chrono::Utc::now().format("%Y%m%d000000");
    std::fs::write(srv.root.join(format!("grpX/member_b/40_1_{now}.jpg")), b"\xFF\xD8\xFF\xE0 broken").unwrap();
    srv.reindex_full_ok();
    let thumb: Option<String> =
        query_db(&srv.data_dir(), "SELECT thumb FROM messages WHERE member='member_b' AND msg_id=40");
    assert_eq!(thumb, None, "存在しないサムネを指さない");

    let r = srv.post_q("/api/reindex", &[("mode", "incremental")]);
    assert_eq!(r.status, 200);
    assert_eq!(srv.wait_reindex_idle()["last_error"], Value::Null);
    assert_eq!(srv.log().matches("thumb failed for").count(), 1, "差分取り込みでは作り直さない:\n{}", srv.log());
}

fn orphan_fts_rows(data_dir: &Path) -> i64 {
    let db = rusqlite::Connection::open(data_dir.join("index.db")).unwrap();
    db.query_row("SELECT COUNT(*) FROM messages_fts WHERE rowid NOT IN (SELECT rowid FROM messages)", [], |r| r.get(0))
        .unwrap()
}

/// メンバー削除で検索索引の行が孤児として残らない。サンプルでは member_a が最後に取り込まれる(rowid 最大)ため、
/// 孤児が残ると、再取り込みで rowid が衝突して失敗する。
#[test]
fn deleted_member_leaves_no_orphans_and_comes_back_on_reindex() {
    let srv = Server::indexed();
    assert_eq!(srv.delete("/api/members/member_a").status, 200);
    assert_eq!(orphan_fts_rows(&srv.data_dir()), 0);
    // 保存フォルダに残っているので、取り込み直すと復活する
    srv.reindex_full_ok();
    let t: Vec<(Value, Value)> = srv
        .get_json("/api/threads", &[])
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["member"].clone(), t["count"].clone()))
        .collect();
    assert_eq!(t, [(json!("member_a"), json!(9)), (json!("member_b"), json!(2))]);
    assert_eq!(srv.delete("/api/members/member_b").status, 200);
    assert_eq!(orphan_fts_rows(&srv.data_dir()), 0);
}

/// 古い版が残した孤児の索引行は、起動時に掃除する。
#[test]
fn orphan_fts_rows_are_cleaned_on_startup() {
    let sandbox = Server::indexed().stop_keep();
    let data = sandbox.join("data");
    let db = rusqlite::Connection::open(data.join("index.db")).unwrap();
    db.execute("DELETE FROM messages WHERE member='member_a'", []).unwrap(); // 索引行を残して消す
    drop(db);
    assert!(orphan_fts_rows(&data) > 0);

    let srv = Server::restart(sandbox);
    let deadline = Instant::now() + Duration::from_secs(10);
    while orphan_fts_rows(&data) > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(orphan_fts_rows(&data), 0);
    assert_eq!(srv.get_json("/api/threads", &[]).as_array().unwrap().len(), 1);
}

/// 別プロセスの取り込みを模して、このテストプロセスが .index.lock を握り、PID ファイルを書く。
/// 返したファイルを持っている間ロックが続く。
fn hold_index_lock(srv: &Server, pid: u32) -> std::fs::File {
    let data = srv.data_dir();
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join(".index.lock"))
        .unwrap();
    f.try_lock().unwrap();
    std::fs::write(data.join(".index.pid"), format!("{pid}\n0\n")).unwrap();
    f
}

/// 別プロセスの取り込み(.index.lock を握っている)中は、削除・初期化とも409。
#[test]
fn delete_and_reset_refuse_while_other_indexer_runs() {
    let srv = Server::indexed();
    let me = std::process::id();
    let _lock = hold_index_lock(&srv, me);
    for r in [
        srv.delete("/api/members/member_a"),
        srv.post("/api/reset", &json!({})),
        raw_request(srv.port, "POST", "/api/reindex", &[], None),
    ] {
        assert_eq!(r.status, 409);
        assert_eq!(r.json(), json!({ "status": "already_running", "detail": format!("index process pid={me} is running") }));
    }
    assert_eq!(srv.get_json("/api/threads", &[]).as_array().unwrap().len(), 2); // 何も消えていない
}

/// 別プロセスの取り込み中に保存フォルダを設定すると、保存はするが取り込みは始めず index_started=false。
#[test]
fn settings_does_not_start_reindex_while_other_indexer_runs() {
    let srv = Server::start();
    let _lock = hold_index_lock(&srv, std::process::id());
    let root = srv.root.to_string_lossy().into_owned();
    let r = srv.post("/api/settings", &json!({ "root": root }));
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["index_started"], json!(false));
    assert_eq!(srv.config_json()["root"], json!(root)); // 保存はされる
    let st = srv.get_json("/api/reindex/status", &[]);
    assert_eq!(st["last_error"], Value::Null); // バックグラウンドで already_running にもなっていない
}

// --------------------------------------------------------------- CLI
/// `index --rebuild` は `--only-member` と併用できない(他のメンバーまで消えるため)。何も消さずに終了コード1。
#[test]
fn cli_rebuild_rejects_only_member() {
    let sandbox = Server::indexed().stop_keep(); // サーバ起動中の拒否と区別するため止めておく
    let (code, out) = run_cli(&sandbox, &["index", "--rebuild", "--only-member", "member_a"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("--only-member"), "{out}");
    let srv = Server::restart(sandbox);
    assert_eq!(srv.get_json("/api/threads", &[]).as_array().unwrap().len(), 2); // 何も消えていない
}

/// サーバ起動中は `index --rebuild` を拒否する(サーバが開いている index.db を消せない/消すと食い違う)。
/// 止めていれば作り直せる。
#[test]
fn cli_rebuild_refuses_while_server_runs() {
    let srv = Server::indexed();
    let (code, out) = srv.cli(&["index", "--rebuild", "--no-thumbs"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("サーバが起動中"), "{out}");
    assert_eq!(srv.get_json("/api/threads", &[]).as_array().unwrap().len(), 2);

    let sandbox = srv.stop_keep();
    let (code, out) = run_cli(&sandbox, &["index", "--rebuild", "--no-thumbs"]);
    assert_eq!(code, 0, "{out}");
    let srv = Server::restart(sandbox);
    assert_eq!(srv.get_json("/api/threads", &[]).as_array().unwrap().len(), 2);
}

/// 取り込みが異常終了して PID ファイルだけが残った(ロックは OS が解放済み)場合は無視して実行する。
/// その PID が別のプロセスに再利用されて生きていても同じ。
#[test]
fn stale_index_lock_is_ignored() {
    let srv = Server::indexed();
    let mut child = if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", "exit"]).spawn().unwrap()
    } else {
        std::process::Command::new("true").spawn().unwrap()
    };
    let dead = child.id();
    child.wait().unwrap();
    drop(hold_index_lock(&srv, dead)); // ロックは解放し、PID ファイルだけ残す
    assert_eq!(srv.delete("/api/members/ghost").status, 200);
    // PID が再利用された場合(生きている PID = このテストプロセス)
    drop(hold_index_lock(&srv, std::process::id()));
    assert_eq!(srv.delete("/api/members/ghost").status, 200);
    assert_eq!(srv.post("/api/reset", &json!({})).status, 200);
    assert!(!srv.data_dir().join(".index.pid").exists()); // 残っていた PID ファイルも片付く
}

// -------------------------------------------------------------------- 初期化
#[test]
fn reset() {
    let srv = Server::indexed();
    srv.post("/api/favorites", &json!({ "member": "member_a", "msg_id": 1 }));
    srv.put("/api/comments", &json!({ "member": "member_a", "msg_id": 1, "comment": "x" }));
    srv.put("/api/hidden-members", &json!({ "member": "member_a", "hidden": true }));
    srv.put("/api/member-order", &json!({ "members": ["member_b", "member_a"] }));
    let thumb = thumb_url("member_a", 6);
    assert_eq!(srv.get(&thumb).status, 200);

    let r = srv.post("/api/reset", &json!({}));
    assert_eq!((r.status, r.json()), (200, json!({ "status": "reset" })));
    assert_eq!(srv.get_json("/api/threads", &[]), json!([]));
    assert_eq!(srv.get_json("/api/favorites", &[])["favorites"], json!([]));
    assert_eq!(srv.get_json("/api/hidden-members", &[]), json!({ "members": [] }));
    assert_eq!(srv.get_json("/api/member-order", &[]), json!({ "members": [] }));
    assert_eq!(srv.get(&thumb).status, 404);
    let s = srv.get_json("/api/settings", &[]);
    assert_eq!((s["root"].clone(), s["configured"].clone()), (Value::Null, json!(false)));
    assert_eq!(srv.config_json()["root"], json!(""));
    let data = srv.data_dir();
    assert!(!data.join("progress.json").exists());
    assert!(data.join("index.db").exists() && data.join("thumbs.db").exists());
    assert_eq!(srv.get("/media/member_a/2026/202608/6_1_20260807000000.jpg").status, 409); // root 未設定
    // 初期化の直後も各 API は普通に使える(接続の張り直し)
    assert_eq!(srv.get_json("/api/search", &[("q", "ライブ")])["total"], json!(0));
    assert_eq!(srv.put("/api/hidden-members", &json!({ "member": "x", "hidden": true })).status, 200);
    // 2回続けても成功する
    assert_eq!(srv.post("/api/reset", &json!({})).status, 200);
    assert_eq!(resetting_leftovers(&data), Vec::<String>::new()); // 一時的な名前のものは残らない
}

/// data/ に残っている、初期化の一時的な名前(`*.resetting`)のもの。
fn resetting_leftovers(data: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(data)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".resetting"))
        .collect();
    v.sort();
    v
}

/// 初期化の途中で失敗したら(旧版のサムネのフォルダのファイルを他のプロセスが開いていて動かせない)、何も変えずに 500。
/// 閉じてからやり直せば初期化できる。Windows では開いているファイルを含むフォルダの名前を変えられないことを使う。
#[cfg(windows)]
#[test]
fn reset_is_all_or_nothing() {
    use std::os::windows::fs::OpenOptionsExt;
    let srv = Server::indexed();
    srv.post("/api/favorites", &json!({ "member": "member_a", "msg_id": 1 }));
    srv.put("/api/comments", &json!({ "member": "member_a", "msg_id": 1, "comment": "x" }));
    let root_before = srv.config_json()["root"].clone();
    let data = srv.data_dir();
    let legacy = data.join("thumbs").join("ab").join("x.jpg");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, b"old").unwrap();
    // エクスプローラーのプレビューなどと同じく、削除・名前の変更を許さずに開く(FILE_SHARE_READ のみ)
    let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&legacy).unwrap();

    let r = srv.post("/api/reset", &json!({}));
    assert_eq!(r.status, 500);
    assert!(r.json()["detail"].as_str().unwrap().contains("何も変更していません"), "{:?}", r.json());
    // 何も消えていない
    assert_eq!(srv.get_json("/api/threads", &[]).as_array().unwrap().len(), 2);
    assert_eq!(srv.get_json("/api/favorites", &[])["favorites"].as_array().unwrap().len(), 1);
    assert_eq!(srv.get_json("/api/messages", &[("member", "member_a")])["messages"][0]["comment"], json!("x"));
    assert_eq!(srv.get(&thumb_url("member_a", 6)).status, 200);
    assert_eq!(srv.config_json()["root"], root_before);
    assert_eq!(srv.get_json("/api/settings", &[])["configured"], json!(true));
    assert_eq!(resetting_leftovers(&data), Vec::<String>::new());

    drop(held);
    assert_eq!(srv.post("/api/reset", &json!({})).status, 200);
    assert_eq!(srv.get_json("/api/threads", &[]), json!([]));
    assert_eq!(srv.get_json("/api/favorites", &[])["favorites"], json!([]));
    assert_eq!(resetting_leftovers(&data), Vec::<String>::new());
}

/// 前回の初期化で消し損ねた一時的な名前のものは、起動時に片付ける。
#[test]
fn reset_leftovers_are_cleaned_on_startup() {
    let srv = Server::indexed();
    let data = srv.data_dir();
    let sandbox = srv.stop_keep();
    std::fs::write(data.join("index.db.resetting"), b"old").unwrap();
    std::fs::create_dir_all(data.join("thumbs.resetting").join("ab")).unwrap();
    let srv = Server::restart(sandbox);
    assert_eq!(resetting_leftovers(&srv.data_dir()), Vec::<String>::new());
    assert_eq!(srv.get_json("/api/threads", &[]).as_array().unwrap().len(), 2); // 本物には触れない
}

#[test]
fn reset_then_reindex() {
    let srv = Server::indexed();
    srv.post("/api/reset", &json!({}));
    assert_eq!(srv.get_json("/api/reindex/status", &[])["last_summary"], Value::Null);
    srv.configure_and_index(); // 初期化後に設定し直せば、再び取り込める
    assert_eq!(srv.get_json("/api/threads", &[]).as_array().unwrap().len(), 2);
}
