//! お気に入り・コメント・非表示メンバー・並び順(user.db 系)の契約。変更系なので毎回作り直したサーバで検証する。

mod common;

use common::*;
use serde_json::{json, Value};

fn sorted_ids(srv: &Server, member: &str) -> Vec<i64> {
    let d = srv.get_json("/api/favorites/ids", &[("member", member)]);
    let mut v: Vec<i64> = d["msg_ids"].as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect();
    v.sort();
    v
}

// ------------------------------------------------------------------ お気に入り
#[test]
fn favorites_flow() {
    let srv = Server::indexed();
    assert_eq!(srv.get_json("/api/favorites", &[])["favorites"], json!([]));
    assert_eq!(srv.get_json("/api/favorites/ids", &[("member", "member_a")]), json!({ "msg_ids": [] }));

    let r = srv.post("/api/favorites", &json!({ "member": "member_a", "msg_id": 3 }));
    assert_eq!((r.status, r.json()), (200, json!({ "status": "ok", "favorited": true })));
    std::thread::sleep(std::time::Duration::from_millis(1100)); // created_at は秒精度
    srv.post("/api/favorites", &json!({ "member": "member_b", "msg_id": 1 }));
    srv.post("/api/favorites", &json!({ "member": "member_b", "msg_id": 1 })); // 重複登録は無害
    assert_eq!(sorted_ids(&srv, "member_b"), [1]);

    let favs = srv.get_json("/api/favorites", &[])["favorites"].clone();
    assert_eq!(pairs(&favs), [p("member_b", 1), p("member_a", 3)]); // created_at 降順
    assert!(favs[0].get("created_at").is_some() && favs[0].get("comment").is_some());
    assert_eq!(favs[0]["body"], json!("bさんの最初のメッセージ"));

    let only_a = srv.get_json("/api/favorites", &[("member", "member_a")]);
    assert_eq!(ids(&only_a["favorites"]), [3]);
    let created = favs[0]["created_at"].to_string();
    let older = srv.get_json("/api/favorites", &[("before", &created)]);
    assert_eq!(pairs(&older["favorites"]), [p("member_a", 3)]);

    let r = srv.delete("/api/favorites?member=member_a&msg_id=3");
    assert_eq!(r.json(), json!({ "status": "ok", "favorited": false }));
    assert_eq!(srv.delete("/api/favorites?member=member_a&msg_id=3").status, 200); // 冪等
}

/// 同じ秒に登録したお気に入りが、ページ境界で抜け落ちない(next_before / next_before_rowid)。
#[test]
fn favorites_cursor_keeps_same_second() {
    let srv = Server::indexed();
    for i in 1..=3 {
        assert_eq!(srv.post("/api/favorites", &json!({ "member": "member_a", "msg_id": i })).status, 200);
    }
    let mut seen = Vec::new();
    let mut cursor: Option<(String, String)> = None;
    for _ in 0..5 {
        let mut params = vec![("member", "member_a"), ("limit", "1")];
        if let Some((b, r)) = &cursor {
            params.push(("before", b));
            params.push(("before_rowid", r));
        }
        let d = srv.get_json("/api/favorites", &params);
        if d["favorites"].as_array().unwrap().is_empty() {
            assert_eq!((d["next_before"].clone(), d["next_before_rowid"].clone()), (Value::Null, Value::Null));
            break;
        }
        seen.extend(ids(&d["favorites"]));
        cursor = Some((d["next_before"].to_string(), d["next_before_rowid"].to_string()));
    }
    seen.sort();
    assert_eq!(seen, [1, 2, 3]);
}

#[test]
fn favorites_validation() {
    let srv = Server::indexed();
    let fav = |body: Value| srv.post("/api/favorites", &body).status;
    assert_eq!(fav(json!({ "member": "member_a", "msg_id": 999 })), 404);
    assert_eq!(fav(json!({ "member": "", "msg_id": 1 })), 400);
    assert_eq!(fav(json!({ "member": "member_a" })), 400);
    assert_eq!(fav(json!({ "member": "member_a", "msg_id": "abc" })), 400);
    assert_eq!(fav(json!({ "member": "member_a", "msg_id": 3 })), 200);
    assert_eq!(srv.delete("/api/favorites?member=member_a").status, 422);
    assert_eq!(srv.get_q("/api/favorites", &[("limit", "201")]).status, 422);
    assert_eq!(srv.get("/api/favorites/ids").status, 422);
    assert_eq!(srv.post("/api/favorites", &json!([1, 2])).status, 422); // JSON オブジェクト以外
}

/// 一覧の member は「指定があるか」で判定する(空文字も指定扱い)。
#[test]
fn favorites_member_filter_empty_string() {
    let srv = Server::indexed();
    srv.post("/api/favorites", &json!({ "member": "member_a", "msg_id": 1 }));
    assert_eq!(srv.get_json("/api/favorites", &[("member", "")])["favorites"], json!([]));
    assert_eq!(srv.get_json("/api/favorites", &[])["favorites"].as_array().unwrap().len(), 1);
    assert_eq!(srv.get_q("/api/favorites", &[("before", "x")]).status, 422);
}

/// index.db 側からメッセージが消えたお気に入り(幽霊)は一覧に出さない。ids には残る。
#[test]
fn favorites_hides_ghosts() {
    let srv = Server::indexed();
    srv.post("/api/favorites", &json!({ "member": "member_b", "msg_id": 1 }));
    let db = rusqlite::Connection::open(srv.data_dir().join("user.db")).unwrap();
    db.execute("INSERT INTO favorites (member, msg_id, created_at) VALUES ('member_b', 999, 9999999999)", []).unwrap();
    drop(db);
    assert_eq!(pairs(&srv.get_json("/api/favorites", &[])["favorites"]), [p("member_b", 1)]);
    assert_eq!(sorted_ids(&srv, "member_b"), [1, 999]);
}

// ------------------------------------------------------------------ コメント
#[test]
fn comments_flow() {
    let srv = Server::indexed();
    let r = srv.put("/api/comments", &json!({ "member": "member_a", "msg_id": 1, "comment": "  メモ  " }));
    assert_eq!((r.status, r.json()), (200, json!({ "status": "ok", "comment": "メモ" }))); // 前後の空白は除く

    let comment_of_1 = |srv: &Server| {
        let d = srv.get_json("/api/messages", &[("member", "member_a")]);
        d["messages"].as_array().unwrap().iter().find(|m| m["msg_id"] == json!(1)).unwrap()["comment"].clone()
    };
    assert_eq!(comment_of_1(&srv), json!("メモ"));
    assert_eq!(srv.get_json("/api/search", &[("q", "おはよう")])["results"][0]["comment"], json!("メモ"));
    let ml = srv.get_json("/api/media-list", &[("member", "member_a")]);
    assert!(ml["items"].as_array().unwrap().iter().all(|x| x["comment"].is_null()));

    // お気に入り一覧にもコメントが載る(お気に入りでなくてもコメントは付けられる)
    srv.post("/api/favorites", &json!({ "member": "member_a", "msg_id": 1 }));
    assert_eq!(srv.get_json("/api/favorites", &[])["favorites"][0]["comment"], json!("メモ"));

    // 空文字で消去
    let r = srv.put("/api/comments", &json!({ "member": "member_a", "msg_id": 1, "comment": "" }));
    assert_eq!(r.json(), json!({ "status": "ok", "comment": null }));
    assert_eq!(comment_of_1(&srv), Value::Null);
}

#[test]
fn comments_validation() {
    let srv = Server::indexed();
    let put = |body: Value| srv.put("/api/comments", &body);
    let ok = "あ".repeat(1000);
    assert_eq!(put(json!({ "member": "member_a", "msg_id": 1, "comment": ok })).status, 200);
    assert_eq!(put(json!({ "member": "member_a", "msg_id": 1, "comment": format!("{ok}あ") })).status, 400);
    assert_eq!(put(json!({ "member": "member_a", "msg_id": 999, "comment": "x" })).status, 404);
    assert_eq!(put(json!({ "member": "", "msg_id": 1, "comment": "x" })).status, 400);
    assert_eq!(put(json!({ "member": "member_a", "comment": "x" })).status, 400);
    assert_eq!(put(json!({ "member": "member_a", "msg_id": "z", "comment": "x" })).status, 400);
    // comment の省略・null は消去扱い
    assert_eq!(put(json!({ "member": "member_a", "msg_id": 1 })).json(), json!({ "status": "ok", "comment": null }));
}

// ------------------------------------------------------------------ 非表示・並び順
#[test]
fn hidden_members() {
    let srv = Server::indexed();
    let hide = |m: &str, h: Value| srv.put("/api/hidden-members", &json!({ "member": m, "hidden": h }));
    assert_eq!(srv.get_json("/api/hidden-members", &[]), json!({ "members": [] }));
    assert_eq!(hide("member_b", json!(true)).json(), json!({ "status": "ok", "member": "member_b", "hidden": true }));
    hide("member_a", json!(true));
    assert_eq!(srv.get_json("/api/hidden-members", &[]), json!({ "members": ["member_a", "member_b"] })); // 名前順
    assert_eq!(hide("member_a", json!(false)).json()["hidden"], json!(false));
    assert_eq!(srv.get_json("/api/hidden-members", &[]), json!({ "members": ["member_b"] }));
    assert_eq!(hide(" ", json!(true)).status, 400);
    // hidden の省略は false(解除)
    assert_eq!(srv.put("/api/hidden-members", &json!({ "member": "member_b" })).json()["hidden"], json!(false));
    // 非表示でも threads には影響しない
    assert_eq!(srv.get_json("/api/threads", &[]).as_array().unwrap().len(), 2);
}

#[test]
fn member_order() {
    let srv = Server::indexed();
    let set = |body: Value| srv.put("/api/member-order", &body);
    let get = || srv.get_json("/api/member-order", &[]);
    assert_eq!(get(), json!({ "members": [] }));
    assert_eq!(set(json!({ "members": ["member_b", " member_a "] })).json(), json!({ "status": "ok", "count": 2 }));
    assert_eq!(get(), json!({ "members": ["member_b", "member_a"] })); // 空白除去・指定順
    set(json!({ "members": ["member_a"] })); // 全体を置き換える
    assert_eq!(get(), json!({ "members": ["member_a"] }));
    assert_eq!(set(json!({ "members": ["x", "x"] })).status, 400);
    assert_eq!(set(json!({ "members": ["x", " "] })).status, 400);
    assert_eq!(set(json!({ "members": "x" })).status, 400);
    assert_eq!(set(json!({})).status, 400);
    assert_eq!(set(json!({ "members": [] })).json(), json!({ "status": "ok", "count": 0 }));
    assert_eq!(get(), json!({ "members": [] }));
}

/// ボディの値は型どおりに受け取る(member は文字列、msg_id は整数、hidden は真偽値、comment は文字列か null)。
#[test]
fn payload_types() {
    let srv = Server::indexed();
    let fav = |body: Value| srv.post("/api/favorites", &body);
    let a = |id: Value| fav(json!({ "member": "member_a", "msg_id": id })).status;
    assert_eq!(a(json!(3)), 200);
    assert_eq!(a(json!(-1)), 404);
    // 整数以外は、数字の文字列や小数・真偽値でも受けない
    for bad in [json!("3"), json!(" 3 "), json!(3.7), json!(true), json!([3]), json!(""), json!(null)] {
        assert_eq!(a(bad.clone()), 400, "{bad}");
    }
    let r = fav(json!({ "member": "member_a", "msg_id": "3" }));
    assert_eq!(r.json(), json!({ "detail": "msg_id は整数で指定してください" }));
    assert_eq!(fav(json!({ "member": "  member_a  ", "msg_id": 2 })).status, 200); // member は前後の空白を除く
    let r = fav(json!({ "member": 123, "msg_id": 1 }));
    assert_eq!((r.status, r.json()), (400, json!({ "detail": "member は文字列で指定してください" })));
    assert_eq!(srv.send_raw("POST", "/api/favorites", b"not json").status, 422);
    assert_eq!(srv.send_raw("POST", "/api/favorites", b"").status, 422);
    assert_eq!(sorted_ids(&srv, "member_a"), [2, 3]);

    let hid = |v: Value| srv.put("/api/hidden-members", &json!({ "member": "m", "hidden": v }));
    assert_eq!(hid(json!(true)).json()["hidden"], json!(true));
    assert_eq!(hid(json!(false)).json()["hidden"], json!(false));
    assert_eq!(hid(json!(null)).json()["hidden"], json!(false)); // null は省略と同じ(解除)
    for bad in [json!(1), json!("true"), json!(0), json!([])] {
        assert_eq!(hid(bad.clone()).status, 400, "{bad}");
    }
    assert_eq!(srv.put("/api/hidden-members", &json!({ "member": 1, "hidden": true })).status, 400);

    let c = |comment: Value, msg_id: i64| srv.put("/api/comments", &json!({ "member": "member_a", "msg_id": msg_id, "comment": comment }));
    assert_eq!(c(json!(12345), 2).status, 400);
    assert_eq!(c(json!("\u{3000}全角空白\u{3000}\n"), 2).json()["comment"], json!("全角空白")); // Unicode の空白も除く
    assert_eq!(c(json!("😀".repeat(1000)), 2).status, 200); // 文字数(コードポイント)で数える
    assert_eq!(c(json!("a".repeat(1001)), 2).status, 400);
    assert_eq!(c(json!("a".repeat(1001)), 999).status, 400); // 長さの検証(400)は存在確認(404)より先

    assert_eq!(srv.put("/api/member-order", &json!({ "members": [1, "member_a"] })).status, 400);
    assert_eq!(srv.get_json("/api/member-order", &[]), json!({ "members": [] })); // 失敗したら何も変えない
}
