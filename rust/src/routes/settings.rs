//! /api/settings。
//!
//! POST で保存フォルダ(`root`)を設定ファイルに書き戻し、差分取り込みをバックグラウンドで始める。

use crate::config::{resolve_path, write_root};
use crate::error::{ApiError, ApiResult};
use crate::routes::user::json_object;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};
use std::path::Path;

pub async fn get_settings(State(st): State<AppState>) -> Json<Value> {
    let (root, is_dev, base_dir) = {
        let c = st.cfg.read().unwrap();
        (c.root.clone(), c.is_dev(), c.base_dir.clone())
    };
    let ff = crate::indexer::thumbs::find_ffmpeg(&base_dir);
    Json(json!({
        "root": root.as_ref().map(|p| p.display().to_string()),
        "configured": root.is_some(),
        "is_dev": is_dev,
        "ffmpeg_available": ff.is_some(),
        "ffmpeg_path": ff.as_ref().map(|p| p.display().to_string()),
    }))
}

pub async fn set_settings(State(st): State<AppState>, body: Bytes) -> ApiResult<Json<Value>> {
    let payload: Map<String, Value> = json_object(&body)?;
    let new_root = match payload.get("root") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
    .trim()
    .to_string();
    if new_root.is_empty() {
        return Err(ApiError::bad_request("root は必須です"));
    }
    let p = Path::new(&new_root);
    if !p.is_absolute() {
        return Err(ApiError::bad_request("root は絶対パスで指定してください"));
    }
    if !p.is_dir() {
        return Err(ApiError::bad_request(format!("フォルダが見つかりません: {new_root}")));
    }

    let resolved = {
        let mut cfg = st.cfg.write().unwrap();
        write_root(&cfg, &new_root).map_err(ApiError::internal)?;
        let r = resolve_path(&cfg.base_dir, &new_root);
        cfg.root = Some(r.clone());
        r
    };
    // 取り込みが動いていなければ、差分取り込みをバックグラウンドで始める
    if !st.reindex.running.load(std::sync::atomic::Ordering::SeqCst) {
        crate::routes::reindex::start_background(&st, "incremental", None);
    }
    Ok(Json(json!({ "root": resolved.display().to_string(), "configured": true })))
}
