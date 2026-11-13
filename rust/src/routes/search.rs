//! GET /api/search。

use crate::error::{ApiError, ApiResult};
use crate::message::MsgRow;
use crate::params::Params;
use crate::routes::read::rows_with_comments;
use crate::search::{build_fts_query, like_escape, make_snippet, parse_search_terms, plan_terms};
use crate::state::AppState;
use crate::timeutil::jst_date_to_epoch_range;
use axum::extract::{RawQuery, State};
use axum::Json;
use rusqlite::types::Value as SqlValue;
use serde_json::{json, Value};

/// `m.` 付きの列。並びは `MsgRow::from_row` に合わせ、末尾に rowid(カーソル用)を足す。
const SEARCH_COLS: &str =
    "m.member, m.group_, m.msg_id, m.ts, m.ts_raw, m.body, m.media, m.kind, m.flag, m.thumb, m.missing, m.rowid";

/// SQLite のエラー(FTS5 の構文エラー等)は 400 `search query error: …`。
fn search_error(e: rusqlite::Error) -> ApiError {
    match e {
        rusqlite::Error::SqliteFailure(_, msg) => {
            ApiError::bad_request(format!("search query error: {}", msg.unwrap_or_default()))
        }
        other => ApiError::from(other),
    }
}

pub async fn search(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<Value>> {
    let p = Params::parse(q);
    let q = p.required("q")?;
    if q.is_empty() {
        return Err(ApiError::validation("string_too_short", "q", "String should have at least 1 character"));
    }
    let member = p.opt("member");
    let from = p.opt("from");
    let to = p.opt("to");
    let before = p.opt_int("before")?;
    let before_rowid = p.opt_int("before_rowid")?;
    let limit = p.int_range("limit", 50, 1, 500)?;

    // 空文字は未指定扱い
    let ts_min = match from.filter(|s| !s.is_empty()) {
        Some(f) => Some(jst_date_to_epoch_range(&f)?.0),
        None => None,
    };
    let ts_max = match to.filter(|s| !s.is_empty()) {
        Some(t) => Some(jst_date_to_epoch_range(&t)?.1),
        None => None,
    };

    // trigram は2文字以下を扱えないため、3文字以上の語は FTS で候補を絞り、短い語は LIKE で絞り込む。
    // 3文字以上の「含める語」が無い(除外語しか無い場合も含む)ときは、全語を LIKE で扱う。
    let (includes, excludes) = parse_search_terms(&q);
    let plan = plan_terms(&includes, &excludes);
    let fts = plan.uses_fts();
    // 含める語も除外語も無い入力(`""` のみ等)は、元の文字列をそのまま1語として扱う
    let mark_terms: Vec<String> =
        if includes.is_empty() && excludes.is_empty() { vec![q.clone()] } else { includes.clone() };

    let mut params: Vec<SqlValue> = Vec::new();
    let mut sql_from = String::new();
    let (like_include, like_exclude): (&[String], &[String]) = if fts {
        sql_from.push_str(
            " FROM messages_fts JOIN messages m ON m.rowid = messages_fts.rowid WHERE messages_fts MATCH ?",
        );
        params.push(SqlValue::Text(build_fts_query(&plan.fts_include, &plan.fts_exclude)));
        (&plan.like_include, &plan.like_exclude)
    } else {
        sql_from.push_str(" FROM messages m WHERE 1=1");
        if mark_terms.is_empty() {
            // 除外語だけの検索。本文の無いメディアだけのメッセージまでヒットさせても意味が無い
            sql_from.push_str(" AND m.body IS NOT NULL AND m.body <> ''");
        }
        (&mark_terms, &plan.like_exclude)
    };
    for t in like_include {
        sql_from.push_str(" AND m.body LIKE ? ESCAPE '\\'");
        params.push(SqlValue::Text(format!("%{}%", like_escape(t))));
    }
    for t in like_exclude {
        sql_from.push_str(" AND m.body NOT LIKE ? ESCAPE '\\'");
        params.push(SqlValue::Text(format!("%{}%", like_escape(t))));
    }

    let mut filters = String::new();
    if let Some(m) = member.filter(|s| !s.is_empty()) {
        filters.push_str(" AND m.member = ?");
        params.push(SqlValue::Text(m));
    }
    if let Some(v) = ts_min {
        filters.push_str(" AND m.ts >= ?");
        params.push(SqlValue::Integer(v));
    }
    if let Some(v) = ts_max {
        filters.push_str(" AND m.ts <= ?");
        params.push(SqlValue::Integer(v));
    }

    // 総ヒット数は絞り込み条件までで数える(カーソルは含めない)。続きのページでは数え直さない
    let count_sql = format!("SELECT COUNT(*){sql_from}{filters}");
    let count_params = params.clone();

    let mut page_params = params;
    let mut page_filters = filters;
    if let Some(b) = before {
        match before_rowid {
            // 同じ秒のメッセージがページ境界をまたいでも取りこぼさないよう (ts, rowid) をカーソルにする
            Some(r) => {
                page_filters.push_str(" AND (m.ts < ? OR (m.ts = ? AND m.rowid < ?))");
                page_params.extend([SqlValue::Integer(b), SqlValue::Integer(b), SqlValue::Integer(r)]);
            }
            None => {
                page_filters.push_str(" AND m.ts < ?");
                page_params.push(SqlValue::Integer(b));
            }
        }
    }
    // 1件多く取り、溢れた分の有無で「まだ続きがあるか」を判定する
    let page_sql = format!("SELECT {SEARCH_COLS}{sql_from}{page_filters} ORDER BY m.ts DESC, m.rowid DESC LIMIT ?");
    page_params.push(SqlValue::Integer(limit + 1));

    let need_total = before.is_none();
    // SQLのエラーは閉じ込めの内側で受け取り、500ではなく400にする(Db::run は rusqlite::Error を500にするため)
    let res = st
        .db
        .run(move |c| {
            Ok((|| -> rusqlite::Result<_> {
                let total: Option<i64> = if need_total {
                    Some(c.query_row(&count_sql, rusqlite::params_from_iter(count_params.iter()), |r| r.get(0))?)
                } else {
                    None
                };
                let mut stmt = c.prepare(&page_sql)?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(page_params.iter()), |r| {
                        Ok((MsgRow::from_row(r)?, r.get::<_, i64>(11)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok((total, rows))
            })())
        })
        .await?;
    let (total, mut rows) = res.map_err(search_error)?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_before_rowid = rows.last().map(|(_, rowid)| *rowid);
    let msgs: Vec<MsgRow> = rows.into_iter().map(|(m, _)| m).collect();

    let mut results = rows_with_comments(&st, &msgs).await?;
    for (v, m) in results.iter_mut().zip(&msgs) {
        // FTS/LIKE どちらの経路でも同じ関数で抜粋を作り、見た目を揃える
        let snippet = make_snippet(m.body.as_deref(), &mark_terms);
        let obj = v.as_object_mut().unwrap();
        // キー順はメッセージの項目 → snippet → comment
        let comment = obj.shift_remove("comment").unwrap_or(Value::Null);
        obj.insert("snippet".into(), Value::String(snippet));
        obj.insert("comment".into(), comment);
    }

    Ok(Json(json!({
        "query": q,
        "used_fts": fts,
        "terms": { "include": includes, "exclude": excludes },
        "total": total,
        "has_more": has_more,
        "next_before": msgs.last().map(|m| m.ts),
        "next_before_rowid": next_before_rowid,
        "results": results,
    })))
}
