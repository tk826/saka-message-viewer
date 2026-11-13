//! user.db 系: お気に入り・コメント・非表示メンバー・並び順。
//!
//! ボディの値は型どおりに受け取る(member は文字列、msg_id は整数、hidden は真偽値、comment は文字列か null)。
//! 型が違えば400。文字列は前後の空白を除いてから使う。

use crate::db::{fetch_comments_map, pairs_condition};
use crate::error::{ApiError, ApiResult};
use crate::message::{MsgRow, MSG_COLS};
use crate::params::Params;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::Json;
use rusqlite::ToSql;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// コメントの長さ上限(文字数=コードポイント数)。
const COMMENT_MAX_LENGTH: usize = 1000;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// JSON オブジェクトのボディ。それ以外(不正JSON・配列・空)は422。
pub fn json_object(body: &Bytes) -> ApiResult<Map<String, Value>> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(ApiError::validation_body()),
    }
}

/// 文字列の項目を取り出す(前後の空白は除く)。無い・null なら None、文字列でなければ400。
fn opt_string(payload: &Map<String, Value>, key: &str) -> ApiResult<Option<String>> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
        Some(_) => Err(ApiError::bad_request(format!("{key} は文字列で指定してください"))),
    }
}

/// 必須のメンバー名(空白だけも不可)。
fn required_member(payload: &Map<String, Value>) -> ApiResult<String> {
    opt_string(payload, "member")?.filter(|m| !m.is_empty()).ok_or_else(|| ApiError::bad_request("member は必須です"))
}

/// `{member, msg_id}` を取り出す。msg_id は JSON の整数のみ(文字列や小数は400)。
fn member_and_msg_id(payload: &Map<String, Value>) -> ApiResult<(String, i64)> {
    let member = opt_string(payload, "member")?.filter(|m| !m.is_empty());
    let msg_id = payload.get("msg_id").filter(|v| !v.is_null());
    let (Some(member), Some(msg_id)) = (member, msg_id) else {
        return Err(ApiError::bad_request("member と msg_id は必須です"));
    };
    let msg_id = msg_id.as_i64().ok_or_else(|| ApiError::bad_request("msg_id は整数で指定してください"))?;
    Ok((member, msg_id))
}

/// index.db にメッセージが存在しなければ404。
/// (存在しないメッセージに登録できてしまうと、一覧で肉付けできない幽霊が残るため)
async fn ensure_message_exists(st: &AppState, member: &str, msg_id: i64) -> ApiResult<()> {
    let m = member.to_string();
    let exists = st
        .db
        .run(move |c| {
            let mut stmt = c.prepare("SELECT 1 FROM messages WHERE member=? AND msg_id=?")?;
            stmt.exists(rusqlite::params![m, msg_id])
        })
        .await?;
    if exists { Ok(()) } else { Err(ApiError::not_found("message not found")) }
}

// ------------------------------------------------------------------ お気に入り

pub async fn add_favorite(State(st): State<AppState>, body: Bytes) -> ApiResult<Json<Value>> {
    let payload = json_object(&body)?;
    let (member, msg_id) = member_and_msg_id(&payload)?;
    ensure_message_exists(&st, &member, msg_id).await?;
    let now = now_secs();
    st.user_db
        .run(move |c| {
            c.execute(
                "INSERT OR IGNORE INTO favorites (member, msg_id, created_at) VALUES (?, ?, ?)",
                rusqlite::params![member, msg_id, now],
            )
        })
        .await?;
    Ok(Json(json!({ "status": "ok", "favorited": true })))
}

pub async fn remove_favorite(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<Value>> {
    let p = Params::parse(q);
    let member = p.required("member")?;
    let msg_id = p.required_int("msg_id")?;
    st.user_db
        .run(move |c| c.execute("DELETE FROM favorites WHERE member=? AND msg_id=?", rusqlite::params![member, msg_id]))
        .await?;
    Ok(Json(json!({ "status": "ok", "favorited": false })))
}

/// お気に入り一覧(created_at 降順のカーソルページング)。member は「指定があるか」で判定する(空文字も指定扱い)。
///
/// 同じ秒に登録したものがページ境界をまたいでも取りこぼさないよう、並びは (created_at, rowid) の降順にし、
/// 応答の `next_before` / `next_before_rowid` を次ページの `before` / `before_rowid` に渡せるようにする。
pub async fn list_favorites(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<Value>> {
    let p = Params::parse(q);
    let limit = p.int_range("limit", 50, 1, 200)?;
    let before = p.opt_int("before")?;
    let before_rowid = p.opt_int("before_rowid")?;
    let member = p.opt("member");

    let favs: Vec<(String, i64, i64, i64)> = st
        .user_db
        .run(move |c| {
            let mut sql = String::from("SELECT member, msg_id, created_at, rowid FROM favorites WHERE 1=1");
            let mut params: Vec<Box<dyn ToSql>> = Vec::new();
            if let Some(m) = member {
                sql.push_str(" AND member=?");
                params.push(Box::new(m));
            }
            match (before, before_rowid) {
                (Some(b), Some(r)) => {
                    sql.push_str(" AND (created_at < ? OR (created_at = ? AND rowid < ?))");
                    params.extend([Box::new(b) as Box<dyn ToSql>, Box::new(b), Box::new(r)]);
                }
                (Some(b), None) => {
                    sql.push_str(" AND created_at < ?");
                    params.push(Box::new(b));
                }
                (None, _) => {}
            }
            sql.push_str(" ORDER BY created_at DESC, rowid DESC LIMIT ?");
            params.push(Box::new(limit));
            let mut stmt = c.prepare(&sql)?;
            let refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
            let rows = stmt.query_map(refs.as_slice(), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            rows.collect()
        })
        .await?;
    // 次ページの目印は、肉付けで除外される幽霊も含めた「このページで読んだ最後の行」
    let next_before = favs.last().map(|f| f.2);
    let next_before_rowid = favs.last().map(|f| f.3);
    if favs.is_empty() {
        return Ok(Json(json!({ "favorites": [], "next_before": null, "next_before_rowid": null })));
    }

    // index.db と user.db は JOIN できないため、(member, msg_id) で突き合わせる
    let pairs: Vec<(String, i64)> = favs.iter().map(|(m, i, _, _)| (m.clone(), *i)).collect();
    let pairs_q = pairs.clone();
    let msgs: Vec<MsgRow> = st
        .db
        .run(move |c| {
            let (cond, params) = pairs_condition(&pairs_q);
            let sql = format!("SELECT {MSG_COLS} FROM messages WHERE {cond}");
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params.as_slice(), MsgRow::from_row)?;
            rows.collect()
        })
        .await?;
    let by_key: HashMap<(String, i64), MsgRow> =
        msgs.into_iter().map(|m| ((m.member.clone(), m.msg_id), m)).collect();
    let comments = fetch_comments_map(&st.user_db, pairs).await?;

    let mut result = Vec::new();
    for (member, msg_id, created_at, _) in favs {
        let key = (member, msg_id);
        // index.db 側に無い(幽霊)お気に入りは黙って除外する
        let Some(row) = by_key.get(&key) else { continue };
        let mut v = row.to_json(None);
        let obj = v.as_object_mut().unwrap();
        obj.shift_remove("comment"); // キー順はメッセージの項目 → created_at → comment
        obj.insert("created_at".into(), json!(created_at));
        obj.insert("comment".into(), json!(comments.get(&key)));
        result.push(v);
    }
    Ok(Json(json!({ "favorites": result, "next_before": next_before, "next_before_rowid": next_before_rowid })))
}

pub async fn favorite_ids(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<Value>> {
    let member = Params::parse(q).required("member")?;
    let ids: Vec<i64> = st
        .user_db
        .run(move |c| {
            let mut stmt = c.prepare("SELECT msg_id FROM favorites WHERE member=?")?;
            let rows = stmt.query_map([member], |r| r.get(0))?;
            rows.collect()
        })
        .await?;
    Ok(Json(json!({ "msg_ids": ids })))
}

// ------------------------------------------------------------------ コメント

pub async fn update_comment(State(st): State<AppState>, body: Bytes) -> ApiResult<Json<Value>> {
    let payload = json_object(&body)?;
    let (member, msg_id) = member_and_msg_id(&payload)?;
    // 省略・null・空文字(前後の空白を除いた後)での更新は「コメントを消す」
    let comment = opt_string(&payload, "comment")?.filter(|s| !s.is_empty());
    if let Some(c) = &comment {
        if c.chars().count() > COMMENT_MAX_LENGTH {
            return Err(ApiError::bad_request(format!("コメントは{COMMENT_MAX_LENGTH}文字以内で入力してください")));
        }
    }
    ensure_message_exists(&st, &member, msg_id).await?;

    let stored = comment.clone();
    let now = now_secs();
    st.user_db
        .run(move |c| match stored {
            None => c.execute("DELETE FROM message_comments WHERE member=? AND msg_id=?", rusqlite::params![member, msg_id]),
            Some(text) => c.execute(
                "INSERT INTO message_comments (member, msg_id, comment, updated_at) VALUES (?, ?, ?, ?)
                 ON CONFLICT (member, msg_id) DO UPDATE SET comment=excluded.comment, updated_at=excluded.updated_at",
                rusqlite::params![member, msg_id, text, now],
            ),
        })
        .await?;
    Ok(Json(json!({ "status": "ok", "comment": comment })))
}

// ------------------------------------------------------------------ 非表示メンバー

pub async fn list_hidden_members(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let members: Vec<String> = st
        .user_db
        .run(|c| {
            let mut stmt = c.prepare("SELECT member FROM hidden_members ORDER BY member")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect()
        })
        .await?;
    Ok(Json(json!({ "members": members })))
}

/// サイドバーの表示上の設定。メッセージ自体は削除しない。
pub async fn update_hidden_member(State(st): State<AppState>, body: Bytes) -> ApiResult<Json<Value>> {
    let payload = json_object(&body)?;
    let member = required_member(&payload)?;
    // 省略・null は false(非表示の解除)
    let hidden = match payload.get("hidden") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(ApiError::bad_request("hidden は true / false で指定してください")),
    };
    let (m, now) = (member.clone(), now_secs());
    st.user_db
        .run(move |c| {
            if hidden {
                c.execute("INSERT OR REPLACE INTO hidden_members (member, hidden_at) VALUES (?, ?)", rusqlite::params![m, now])
            } else {
                c.execute("DELETE FROM hidden_members WHERE member=?", [m])
            }
        })
        .await?;
    Ok(Json(json!({ "status": "ok", "member": member, "hidden": hidden })))
}

// ------------------------------------------------------------------ 並び順

pub async fn get_member_order(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let members: Vec<String> = st
        .user_db
        .run(|c| {
            let mut stmt = c.prepare("SELECT member FROM member_order ORDER BY position, member")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect()
        })
        .await?;
    Ok(Json(json!({ "members": members })))
}

/// 並び順を丸ごと置き換える(差分ではなく順序の全体を受け取る)。
pub async fn update_member_order(State(st): State<AppState>, body: Bytes) -> ApiResult<Json<Value>> {
    let payload = json_object(&body)?;
    let Some(Value::Array(members)) = payload.get("members") else {
        return Err(ApiError::bad_request("members は配列である必要があります"));
    };
    let mut names: Vec<String> = Vec::new();
    for m in members {
        let Some(name) = m.as_str().map(|s| s.trim().to_string()) else {
            return Err(ApiError::bad_request("members はメンバー名(文字列)の配列で指定してください"));
        };
        if name.is_empty() {
            return Err(ApiError::bad_request("空のメンバー名は指定できません"));
        }
        if names.contains(&name) {
            return Err(ApiError::bad_request(format!("メンバー名が重複しています: {name}")));
        }
        names.push(name);
    }
    let count = names.len();
    let now = now_secs();
    st.user_db
        .run(move |c| {
            let tx = c.unchecked_transaction()?;
            tx.execute("DELETE FROM member_order", [])?;
            {
                let mut stmt = tx.prepare("INSERT INTO member_order (member, position, updated_at) VALUES (?, ?, ?)")?;
                for (i, name) in names.iter().enumerate() {
                    stmt.execute(rusqlite::params![name, i as i64, now])?;
                }
            }
            tx.commit()
        })
        .await?;
    Ok(Json(json!({ "status": "ok", "count": count })))
}
