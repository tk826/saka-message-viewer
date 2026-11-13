//! 読み取り系API: threads / messages / jump / calendar / media-list。

use crate::db::fetch_comments_map;
use crate::error::{ApiError, ApiResult};
use crate::message::{MsgRow, MSG_COLS};
use crate::params::Params;
use crate::state::AppState;
use crate::timeutil::{is_yearmonth, jst_date_to_epoch_range, jst_month_epoch_range};
use axum::extract::{RawQuery, State};
use axum::Json;
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};

fn query_msgs(c: &Connection, sql: &str, params: &[&dyn ToSql]) -> rusqlite::Result<Vec<MsgRow>> {
    let mut stmt = c.prepare(sql)?;
    let rows = stmt.query_map(params, MsgRow::from_row)?;
    rows.collect()
}

/// 各行にコメントを付けてJSON化する。コメントは (member, msg_id) で user.db から引く。
pub async fn rows_with_comments(st: &AppState, rows: &[MsgRow]) -> ApiResult<Vec<Value>> {
    let pairs = rows.iter().map(|r| (r.member.clone(), r.msg_id)).collect();
    let comments = fetch_comments_map(&st.user_db, pairs).await?;
    Ok(rows
        .iter()
        .map(|r| r.to_json(comments.get(&(r.member.clone(), r.msg_id))))
        .collect())
}

// ---------------------------------------------------------------- /api/threads
pub async fn threads(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let rows = st
        .db
        .run(|c| {
            let mut stmt = c.prepare(
                "SELECT member, group_ AS grp, COUNT(*) AS cnt, MAX(ts) AS last_ts
                 FROM messages GROUP BY member, group_ ORDER BY last_ts DESC",
            )?;
            let it = stmt.query_map([], |r| {
                Ok(json!({
                    "member": r.get::<_, String>(0)?,
                    "group": r.get::<_, Option<String>>(1)?,
                    "count": r.get::<_, i64>(2)?,
                    "last_ts": r.get::<_, i64>(3)?,
                }))
            })?;
            it.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await?;
    Ok(Json(Value::Array(rows)))
}

// -------------------------------------------------------------- /api/messages
pub async fn messages(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<Value>> {
    let p = Params::parse(q);
    let member = p.required("member")?;
    let before = p.opt_int("before")?;
    let after = p.opt_int("after")?;
    // 同じ秒のメッセージがページ境界をまたいでも取りこぼさないよう、(ts, msg_id) を続きの目印にできる。
    // before_id/after_id が無ければ従来どおり ts だけで区切る(ジャンプの `before=ts+1` など)
    let before_id = p.opt_int("before_id")?;
    let after_id = p.opt_int("after_id")?;
    let limit = p.int_range("limit", 50, 1, 500)?;
    if before.is_some() && after.is_some() {
        return Err(ApiError::bad_request("before と after は同時指定不可"));
    }

    let m = member.clone();
    let rows = st
        .db
        .run(move |c| {
            // 並びは (ts, msg_id)。同じ秒の中も msg_id 順で決まった並びにする
            if let Some(a) = after {
                // 古い方から limit 件。すでに昇順
                let (cond, params): (&str, Vec<&dyn ToSql>) = match &after_id {
                    Some(id) => ("ts >= ? AND (ts > ? OR msg_id > ?)", vec![&m, &a, &a, id, &limit]),
                    None => ("ts > ?", vec![&m, &a, &limit]),
                };
                let sql =
                    format!("SELECT {MSG_COLS} FROM messages WHERE member=? AND {cond} ORDER BY ts ASC, msg_id ASC LIMIT ?");
                query_msgs(c, &sql, &params)
            } else if let Some(b) = before {
                let (cond, params): (&str, Vec<&dyn ToSql>) = match &before_id {
                    Some(id) => ("ts <= ? AND (ts < ? OR msg_id < ?)", vec![&m, &b, &b, id, &limit]),
                    None => ("ts < ?", vec![&m, &b, &limit]),
                };
                let sql =
                    format!("SELECT {MSG_COLS} FROM messages WHERE member=? AND {cond} ORDER BY ts DESC, msg_id DESC LIMIT ?");
                let mut v = query_msgs(c, &sql, &params)?;
                v.reverse(); // 新しい方から取り、時系列昇順に並べ直す
                Ok(v)
            } else {
                let sql = format!("SELECT {MSG_COLS} FROM messages WHERE member=? ORDER BY ts DESC, msg_id DESC LIMIT ?");
                let mut v = query_msgs(c, &sql, &[&m, &limit])?;
                v.reverse();
                Ok(v)
            }
        })
        .await?;

    let messages = rows_with_comments(&st, &rows).await?;
    Ok(Json(json!({ "member": member, "messages": messages })))
}

// ----------------------------------------------------------------- /api/jump
pub async fn jump(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<Value>> {
    let p = Params::parse(q);
    let member = p.required("member")?;
    let date = p.required("date")?;
    let (start_ts, _) = jst_date_to_epoch_range(&date)?;

    let m = member.clone();
    let ts: Option<i64> = st
        .db
        .run(move |c| {
            let mut stmt = c.prepare("SELECT ts FROM messages WHERE member=? AND ts >= ? ORDER BY ts ASC LIMIT 1")?;
            let mut rows = stmt.query(rusqlite::params![m, start_ts])?;
            match rows.next()? {
                Some(r) => Ok(Some(r.get(0)?)),
                None => Ok(None),
            }
        })
        .await?;
    Ok(Json(json!({ "member": member, "date": date, "ts": ts })))
}

// ------------------------------------------------------------- /api/calendar
pub async fn calendar(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<Value>> {
    let p = Params::parse(q);
    let member = p.required("member")?;
    let yearmonth = p.opt("yearmonth");

    if let Some(ym) = yearmonth {
        if !is_yearmonth(&ym) {
            return Err(ApiError::bad_request(format!("invalid yearmonth format: {ym}, expected YYYY-MM")));
        }
        // 実在しない月(2026-13 など)は、従来の文字列比較と同じく0件
        let Some((ts_min, ts_max)) = jst_month_epoch_range(&ym) else {
            return Ok(Json(json!({ "member": member, "yearmonth": ym, "days": [] })));
        };
        let m = member.clone();
        let days = st
            .db
            .run(move |c| {
                // 月の範囲は ts で絞る(strftime で比べると索引が使えず、メンバーの全行を見ることになる)
                let mut stmt = c.prepare(
                    "SELECT strftime('%Y-%m-%d', ts + 9 * 3600, 'unixepoch') AS date, COUNT(*) AS cnt
                     FROM messages
                     WHERE member=? AND ts >= ? AND ts <= ?
                     GROUP BY date ORDER BY date",
                )?;
                let it = stmt.query_map(rusqlite::params![m, ts_min, ts_max], |r| {
                    Ok(json!({ "date": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)? }))
                })?;
                it.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await?;
        return Ok(Json(json!({ "member": member, "yearmonth": ym, "days": days })));
    }

    let m = member.clone();
    let cal = st
        .db
        .run(move |c| {
            let mut stmt = c.prepare(
                "SELECT strftime('%Y-%m', ts + 9 * 3600, 'unixepoch') AS yearmonth, COUNT(*) AS cnt
                 FROM messages WHERE member=? GROUP BY yearmonth ORDER BY yearmonth",
            )?;
            let it = stmt.query_map(rusqlite::params![m], |r| {
                Ok(json!({ "yearmonth": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)? }))
            })?;
            it.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await?;
    Ok(Json(json!({ "member": member, "calendar": cal })))
}

// ----------------------------------------------------------- /api/media-list
pub async fn media_list(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<Value>> {
    let p = Params::parse(q);
    let member = p.required("member")?;
    let kind = p.opt("kind");
    let before = p.opt_int("before")?;
    // 同じ秒のメディアがページ境界をまたいでも取りこぼさないための目印(前ページ最後の msg_id)
    let before_id = p.opt_int("before_id")?;
    let from = p.opt("from");
    let to = p.opt("to");
    let limit = p.int_range("limit", 60, 1, 200)?;

    if let Some(k) = &kind {
        if !matches!(k.as_str(), "image" | "video" | "audio") {
            return Err(ApiError::bad_request(format!(
                "invalid kind: {k}, expected one of ['audio', 'image', 'video']"
            )));
        }
    }

    let mut conds: Vec<&str> = vec!["member=?", "missing=0"];
    let mut params: Vec<Box<dyn ToSql + Send>> = vec![Box::new(member.clone())];
    match &kind {
        Some(k) => {
            conds.push("kind=?");
            params.push(Box::new(k.clone()));
        }
        None => conds.push("kind != 'text'"),
    }
    // 空文字は未指定扱い
    if let Some(f) = from.filter(|s| !s.is_empty()) {
        let (ts_min, _) = jst_date_to_epoch_range(&f)?;
        conds.push("ts >= ?");
        params.push(Box::new(ts_min));
    }
    if let Some(t) = to.filter(|s| !s.is_empty()) {
        let (_, ts_max) = jst_date_to_epoch_range(&t)?;
        conds.push("ts <= ?");
        params.push(Box::new(ts_max));
    }
    match (before, before_id) {
        (Some(b), Some(id)) => {
            conds.push("ts <= ? AND (ts < ? OR msg_id < ?)");
            params.extend([Box::new(b) as Box<dyn ToSql + Send>, Box::new(b), Box::new(id)]);
        }
        (Some(b), None) => {
            conds.push("ts < ?");
            params.push(Box::new(b));
        }
        (None, _) => {}
    }
    params.push(Box::new(limit + 1)); // 1件多く取り、溢れで has_more を判定する

    let sql = format!(
        "SELECT {MSG_COLS} FROM messages WHERE {} ORDER BY ts DESC, msg_id DESC LIMIT ?",
        conds.join(" AND ")
    );
    let mut rows = st
        .db
        .run(move |c| {
            let refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref() as &dyn ToSql).collect();
            query_msgs(c, &sql, &refs)
        })
        .await?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);

    let items = rows_with_comments(&st, &rows).await?;
    Ok(Json(json!({ "items": items, "has_more": has_more })))
}
