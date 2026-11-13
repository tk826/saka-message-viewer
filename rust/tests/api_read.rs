//! 読み取り系 API(threads / messages / search / jump / calendar / media-list)の契約。
//!
//! 日時: ts_raw は UTC として解釈し、カレンダー・日付の絞り込みは JST(+9h)。

mod common;

use common::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------- threads
#[test]
fn threads() {
    let srv = Server::indexed();
    let d = srv.get_json("/api/threads", &[]);
    assert_eq!(
        d,
        json!([
            { "member": "member_a", "group": null, "count": 9, "last_ts": ts(9) },
            { "member": "member_b", "group": "grpX", "count": 2, "last_ts": 1785715200 },
        ])
    ); // last_ts 降順
}

// --------------------------------------------------------------- messages
#[test]
fn messages() {
    let srv = Server::indexed();
    // 新しい3件を、時系列昇順で返す
    let d = srv.get_json("/api/messages", &[("member", "member_a"), ("limit", "3")]);
    assert_eq!(d["member"], json!("member_a"));
    assert_eq!(ids(&d["messages"]), [7, 8, 9]);

    // 形と URL
    let d = srv.get_json("/api/messages", &[("member", "member_a"), ("limit", "50")]);
    let by_id = |id: i64| d["messages"].as_array().unwrap().iter().find(|m| m["msg_id"] == json!(id)).unwrap().clone();
    let m1 = by_id(1);
    assert_eq!(keys(&m1), key_set(&MESSAGE_KEYS));
    assert_eq!(m1["body"], json!("おはようございます"));
    assert_eq!((m1["kind"].clone(), m1["flag"].clone()), (json!("text"), json!(0)));
    assert_eq!((m1["media_url"].clone(), m1["thumb_url"].clone()), (Value::Null, Value::Null));
    assert_eq!((m1["missing"].clone(), m1["comment"].clone()), (json!(false), Value::Null));
    assert_eq!((m1["ts"].clone(), m1["ts_raw"].clone()), (json!(ts(1)), json!("20260801000000")));
    let m6 = by_id(6);
    assert_eq!(m6["kind"], json!("image"));
    assert_eq!(m6["media_url"], json!("/media/member_a/2026/202608/6_1_20260807000000.jpg"));
    assert_eq!(m6["thumb_url"], json!(thumb_url("member_a", 6)));
    let m7 = by_id(7);
    assert_eq!((m7["kind"].clone(), m7["thumb_url"].clone()), (json!("video"), Value::Null)); // ffmpeg なし
    let m8 = by_id(8);
    assert_eq!((m8["body"].clone(), m8["kind"].clone()), (json!("写真のコメント"), json!("image"))); // txt+画像は1件
    assert_eq!(by_id(9)["kind"], json!("audio"));

    // グループ
    let d = srv.get_json("/api/messages", &[("member", "member_b")]);
    assert!(d["messages"].as_array().unwrap().iter().all(|m| m["group"] == json!("grpX")));
    assert_eq!(ids(&d["messages"]), [1, 2]);
}

#[test]
fn messages_before_after() {
    let srv = Server::indexed();
    let (t3, t5) = (ts(3).to_string(), ts(5).to_string());
    // ts < before の直近2件、昇順
    let d = srv.get_json("/api/messages", &[("member", "member_a"), ("before", &t3), ("limit", "2")]);
    assert_eq!(ids(&d["messages"]), [1, 2]);
    // ts > after の古い方2件、昇順
    let d = srv.get_json("/api/messages", &[("member", "member_a"), ("after", &t5), ("limit", "2")]);
    assert_eq!(ids(&d["messages"]), [6, 7]);
    let d = srv.get_json("/api/messages", &[("member", "nobody")]);
    assert_eq!(d, json!({ "member": "nobody", "messages": [] }));
}

/// msg 3 と 4 は同じ秒。ts だけの目印では境界で抜け落ちるが、(ts, msg_id) なら取りこぼさない。
#[test]
fn messages_cursor_with_id_keeps_same_second() {
    let srv = Server::indexed();
    let (t3, t4, t5) = (ts(3).to_string(), ts(4).to_string(), ts(5).to_string());
    // 新しい側から: [4,5] を表示中に、先頭(4)より前を読む
    let d = srv.get_json("/api/messages", &[("member", "member_a"), ("before", &t4), ("before_id", "4"), ("limit", "2")]);
    assert_eq!(ids(&d["messages"]), [2, 3]);
    // 古い側から: [2,3] を表示中に、末尾(3)より後を読む
    let d = srv.get_json("/api/messages", &[("member", "member_a"), ("after", &t3), ("after_id", "3"), ("limit", "2")]);
    assert_eq!(ids(&d["messages"]), [4, 5]);
    // 同じ秒の中も msg_id 順で並ぶ
    let d = srv.get_json("/api/messages", &[("member", "member_a"), ("before", &t5), ("limit", "2")]);
    assert_eq!(ids(&d["messages"]), [3, 4]);
    let r = srv.get_q("/api/messages", &[("member", "member_a"), ("before", "1"), ("before_id", "x")]);
    assert_eq!(r.status, 422);
}

#[test]
fn messages_validation() {
    let srv = Server::indexed();
    assert_eq!(srv.get("/api/messages").status, 422); // member 必須
    assert_eq!(srv.get_q("/api/messages", &[("member", "member_a"), ("limit", "0")]).status, 422);
    assert_eq!(srv.get_q("/api/messages", &[("member", "member_a"), ("limit", "501")]).status, 422);
    assert_eq!(srv.get_q("/api/messages", &[("member", "member_a"), ("before", "x")]).status, 422);
    assert_eq!(srv.get_q("/api/messages", &[("member", "member_a"), ("before", "1"), ("after", "2")]).status, 400);
}

// ----------------------------------------------------------------- search
fn search(srv: &Server, q: &str, extra: &[(&str, &str)]) -> Value {
    let mut params = vec![("q", q)];
    params.extend_from_slice(extra);
    srv.get_json("/api/search", &params)
}

#[test]
fn search_fts_and_snippets() {
    let srv = Server::indexed();
    let d = search(&srv, "ライブ", &[]);
    assert_eq!(d["used_fts"], json!(true));
    assert_eq!(d["terms"], json!({ "include": ["ライブ"], "exclude": [] }));
    assert_eq!((d["total"].clone(), d["has_more"].clone()), (json!(4), json!(false)));
    // ts 降順、同じ ts は rowid 降順(msg_id 4 が 3 より先)
    assert_eq!(
        pairs(&d["results"]),
        [p("member_a", 4), p("member_a", 3), p("member_b", 2), p("member_a", 2)]
    );
    let mut want = key_set(&MESSAGE_KEYS);
    want.insert("snippet".into());
    assert_eq!(keys(&d["results"][0]), want);
    assert_eq!(d["query"], json!("ライブ"));

    // スニペットは HTML エスケープし、該当語を <mark> で囲む
    let d = search(&srv, "tag", &[]);
    assert_eq!(d["results"][0]["snippet"], json!("同じ秒のメッセージ &lt;b&gt;<mark>tag</mark>&lt;/b&gt; &amp; ライブ"));
    let d = search(&srv, "ライブ", &[("member", "member_b")]);
    assert_eq!(d["results"][0]["snippet"], json!("<mark>ライブ</mark>楽しみ"));
}

#[test]
fn search_mixed_long_and_short_terms() {
    let srv = Server::indexed();
    // 3文字以上の「ライブ」は FTS で引き、2文字の「中止」は LIKE で除く
    let d = search(&srv, "ライブ -中止", &[]);
    assert_eq!(d["used_fts"], json!(true));
    assert_eq!(d["terms"], json!({ "include": ["ライブ"], "exclude": ["中止"] }));
    assert_eq!(pairs(&d["results"]), [p("member_a", 4), p("member_b", 2), p("member_a", 2)]);
    assert_eq!(d["total"], json!(3));

    // 除外語も3文字以上なら FTS 式に入る
    let d = search(&srv, "ライブ -同じ秒", &[]);
    assert_eq!(d["used_fts"], json!(true));
    assert!(!pairs(&d["results"]).contains(&p("member_a", 4)));
    assert_eq!(d["total"], json!(3));

    // AND
    let d = search(&srv, "ライブ 楽しみ", &[]);
    assert_eq!((d["used_fts"].clone(), d["total"].clone()), (json!(true), json!(1)));
    assert_eq!(d["results"][0]["member"], json!("member_b"));
}

#[test]
fn search_like_paths() {
    let srv = Server::indexed();
    // 短い語だけなら LIKE
    let d = search(&srv, "あ", &[]);
    assert_eq!((d["used_fts"].clone(), d["total"].clone()), (json!(false), json!(2)));
    assert_eq!(d["results"][0]["snippet"], json!("<mark>あ</mark>"));
    assert_eq!(ids(&d["results"]), [5, 2]);

    // フレーズと % の扱い
    let d = search(&srv, "\"同じ秒\"", &[]);
    assert_eq!((d["terms"]["include"].clone(), d["total"].clone()), (json!(["同じ秒"]), json!(1)));
    let d = search(&srv, "100%", &[]);
    assert_eq!((d["total"].clone(), d["results"][0]["msg_id"].clone()), (json!(1), json!(2)));
    assert!(d["results"][0]["snippet"].as_str().unwrap().ends_with("<mark>100%</mark>楽しかった"));
    assert_eq!(search(&srv, "あ%", &[])["total"], json!(0)); // LIKE でも % は文字どおり

    // 除外語だけ: 本文ありのうち「中止」を含まないもの(本文なしのメディアは除く)
    let d = search(&srv, "-中止", &[]);
    assert_eq!(d["used_fts"], json!(false));
    assert_eq!(d["terms"], json!({ "include": [], "exclude": ["中止"] }));
    assert_eq!(d["total"], json!(7));
    assert!(!pairs(&d["results"]).contains(&p("member_a", 3)));
    assert!(d["results"].as_array().unwrap().iter().all(|m| m["body"].as_str().is_some_and(|b| !b.is_empty())));

    // 引用符だけの入力は語なし
    let d = search(&srv, "\"\"", &[]);
    assert_eq!((d["terms"].clone(), d["used_fts"].clone()), (json!({ "include": [], "exclude": [] }), json!(false)));
}

#[test]
fn search_filters() {
    let srv = Server::indexed();
    assert_eq!(search(&srv, "ライブ", &[("member", "member_a")])["total"], json!(3));
    let d = search(&srv, "ライブ", &[("from", "2026-08-05"), ("to", "2026-08-05")]);
    assert_eq!(d["total"], json!(2)); // 3,4 (JST 8/5 21:00)
    let d = search(&srv, "ライブ", &[("from", "2026-08-02"), ("to", "2026-08-03")]);
    let mut got = pairs(&d["results"]);
    got.sort();
    assert_eq!(got, [p("member_a", 2), p("member_b", 2)]);
}

#[test]
fn search_paging_cursor() {
    let srv = Server::indexed();
    let p1 = search(&srv, "ライブ", &[("limit", "2")]);
    assert_eq!((p1["total"].clone(), p1["has_more"].clone()), (json!(4), json!(true)));
    assert_eq!(pairs(&p1["results"]), [p("member_a", 4), p("member_a", 3)]);
    assert_eq!(p1["next_before"], json!(ts(3)));
    assert!(!p1["next_before_rowid"].is_null());
    let (nb, nr) = (p1["next_before"].to_string(), p1["next_before_rowid"].to_string());
    let p2 = search(&srv, "ライブ", &[("limit", "2"), ("before", &nb), ("before_rowid", &nr)]);
    assert_eq!(p2["total"], Value::Null); // 続きのページでは数え直さない
    assert_eq!(pairs(&p2["results"]), [p("member_b", 2), p("member_a", 2)]);
    assert_eq!(p2["has_more"], json!(false));
}

#[test]
fn search_no_hit_and_validation() {
    let srv = Server::indexed();
    let d = search(&srv, "存在しない語句ですよ", &[]);
    assert_eq!((d["total"].clone(), d["results"].clone(), d["has_more"].clone()), (json!(0), json!([]), json!(false)));
    assert_eq!((d["next_before"].clone(), d["next_before_rowid"].clone()), (Value::Null, Value::Null));
    assert_eq!(srv.get("/api/search").status, 422);
    assert_eq!(srv.get_q("/api/search", &[("q", "")]).status, 422);
    assert_eq!(srv.get_q("/api/search", &[("q", "a"), ("limit", "0")]).status, 422);
    assert_eq!(srv.get_q("/api/search", &[("q", "a"), ("from", "2026/08/01")]).status, 400);
}

// ------------------------------------------------------------- jump / calendar
#[test]
fn jump_and_date_parsing() {
    let srv = Server::indexed();
    let jump = |date: &str| srv.get_q("/api/jump", &[("member", "member_a"), ("date", date)]);
    // JST 8/2 00:00 ちょうど
    assert_eq!(jump("2026-08-02").json(), json!({ "member": "member_a", "date": "2026-08-02", "ts": ts(2) }));
    assert_eq!(jump("2026-08-03").json()["ts"], json!(ts(3)));
    assert_eq!(jump("2027-01-01").json()["ts"], Value::Null);
    assert_eq!(jump("bad").status, 400);
    assert_eq!(srv.get_q("/api/jump", &[("member", "member_a")]).status, 422);

    for ok in ["2026-08-02", "2026-8-2", "2026-08-2", "2026-8-02", "9999-12-31"] {
        assert_eq!(jump(ok).status, 200, "{ok}");
    }
    for bad in [
        " 2026-08-02", "2026-08-02 ", "2026-08-02x", "02-08-2026", "20260802", "0000-01-01", "+2026-08-02",
        "2026-+8-02", "2026-13-01", "2026-02-29", "2026-08-02-01",
    ] {
        assert_eq!(jump(bad).status, 400, "{bad:?}");
    }
}

#[test]
fn calendar() {
    let srv = Server::indexed();
    let d = srv.get_json("/api/calendar", &[("member", "member_a")]);
    assert_eq!(d, json!({ "member": "member_a", "calendar": [{ "yearmonth": "2026-08", "count": 9 }] }));
    let d = srv.get_json("/api/calendar", &[("member", "member_a"), ("yearmonth", "2026-08")]);
    assert_eq!(d["yearmonth"], json!("2026-08"));
    assert_eq!(
        d["days"],
        json!([
            { "date": "2026-08-01", "count": 1 },
            { "date": "2026-08-02", "count": 1 }, // msg 2 は JST で翌日
            { "date": "2026-08-05", "count": 2 }, // msg 3,4 は JST 8/5 21:00
            { "date": "2026-08-06", "count": 1 },
            { "date": "2026-08-07", "count": 1 },
            { "date": "2026-08-08", "count": 1 },
            { "date": "2026-08-09", "count": 1 },
            { "date": "2026-08-10", "count": 1 },
        ])
    );
    assert_eq!(srv.get_q("/api/calendar", &[("member", "member_a"), ("yearmonth", "2026-8")]).status, 400);
    // 実在しない月は0件
    let d = srv.get_json("/api/calendar", &[("member", "member_a"), ("yearmonth", "2026-13")]);
    assert_eq!(d, json!({ "member": "member_a", "yearmonth": "2026-13", "days": [] }));
}

// ------------------------------------------------------------------ media-list
#[test]
fn media_list() {
    let srv = Server::indexed();
    let list = |params: &[(&str, &str)]| {
        let mut all = vec![("member", "member_a")];
        all.extend_from_slice(params);
        srv.get_json("/api/media-list", &all)
    };
    let d = list(&[]);
    assert_eq!(ids(&d["items"]), [9, 8, 7, 6]); // ts 降順、text は除く
    assert_eq!(d["has_more"], json!(false));
    assert_eq!(keys(&d["items"][0]), key_set(&MESSAGE_KEYS));
    assert_eq!(ids(&list(&[("kind", "image")])["items"]), [8, 6]);
    let d = list(&[("limit", "2")]);
    assert_eq!((ids(&d["items"]), d["has_more"].clone()), (vec![9, 8], json!(true)));
    let t8 = ts(8).to_string();
    assert_eq!(ids(&list(&[("before", &t8)])["items"]), [7, 6]);
    // 同じ ts でも msg_id が目印より小さければ続きに含む
    assert_eq!(ids(&list(&[("before", &t8), ("before_id", "99")])["items"]), [8, 7, 6]);
    assert_eq!(ids(&list(&[("before", &t8), ("before_id", "8")])["items"]), [7, 6]);
    // JST 8/8 09:00 〜 8/9 09:00
    assert_eq!(ids(&list(&[("from", "2026-08-08"), ("to", "2026-08-09")])["items"]), [8, 7]);

    assert_eq!(srv.get_q("/api/media-list", &[("member", "member_a"), ("kind", "text")]).status, 400);
    assert_eq!(srv.get_q("/api/media-list", &[("member", "member_a"), ("limit", "201")]).status, 422);
    assert_eq!(srv.get("/api/media-list").status, 422);
}
