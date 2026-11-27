//! /api/settings。
//!
//! POST で保存フォルダ(`root`)を設定ファイルに書き戻し、差分取り込みをバックグラウンドで始める。

use crate::config::{resolve_path, write_root};
use crate::error::{blocking, ApiError, ApiResult};
use crate::indexlock::peek_running_pid;
use crate::routes::user::json_object;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::atomic::Ordering;

pub async fn get_settings(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let (root, is_dev, base_dir, root_from_env) = {
        let c = st.cfg.read().unwrap();
        (c.root.clone(), c.is_dev(), c.base_dir.clone(), c.root_from_env)
    };
    // PATH 上の全フォルダを見るため、オフラインのネットワークドライブがあると遅い。非同期のスレッドを塞がない
    let ff = blocking(move || crate::indexer::thumbs::find_ffmpeg(&base_dir)).await?;
    Ok(Json(json!({
        "root": root.as_ref().map(|p| p.display().to_string()),
        "configured": root.is_some(),
        "is_dev": is_dev,
        "ffmpeg_available": ff.is_some(),
        "ffmpeg_path": ff.as_ref().map(|p| p.display().to_string()),
        "root_env_override": root_from_env,
    })))
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
    if !Path::new(&new_root).is_absolute() {
        return Err(ApiError::bad_request("root は絶対パスで指定してください"));
    }

    // 取り込み・メンバー削除・初期化と排他する。
    // - 初期化と同時に動くと、保存した root が初期化で消えたり、設定ファイルとメモリの値が食い違ったりする
    // - 取り込み中に root を変えると、実行中の取り込みは古い root のまま最後まで走り、
    //   index.db の中身と /media の配信元が食い違う
    let Ok(guard) = st.reindex.run_lock.clone().try_lock_owned() else {
        return Err(ApiError::already_running(None));
    };
    if st.reindex.running.load(Ordering::SeqCst) {
        return Err(ApiError::already_running(None));
    }
    let cfg = st.cfg.read().unwrap().clone();
    if let Some(pid) = peek_running_pid(&cfg.index_lock_path()) {
        return Err(ApiError::already_running(Some(format!("index process pid={pid} is running"))));
    }

    // フォルダの確認(UNC パスだと SMB のタイムアウトまで待つ)と設定ファイルの書き込みは、非同期のスレッドを塞がないよう
    // 別スレッドで行う。設定のロック(全リクエストが読む)も握らない
    let root_for_write = new_root.clone();
    blocking(move || -> ApiResult<()> {
        if !Path::new(&root_for_write).is_dir() {
            return Err(ApiError::bad_request(format!("フォルダが見つかりません: {root_for_write}")));
        }
        write_root(&cfg, &root_for_write).map_err(ApiError::internal)
    })
    .await??;
    let (resolved, root_from_env) = {
        let mut c = st.cfg.write().unwrap();
        let r = resolve_path(&c.base_dir, &new_root);
        c.root = Some(r.clone());
        (r, c.root_from_env)
    };
    drop(guard);

    // 差分取り込みをバックグラウンドで始める。この間に別の取り込み(CLI を含む)・メンバー削除・初期化が始まっていたら
    // 始めず、保存自体は成功なので 200 のまま index_started=false で知らせる
    let index_started = crate::routes::reindex::try_start(&st, "incremental", None).is_ok();
    Ok(Json(json!({
        "root": resolved.display().to_string(),
        "configured": true,
        "index_started": index_started,
        "root_env_override": root_from_env,
    })))
}
