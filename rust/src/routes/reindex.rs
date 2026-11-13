//! 取り込みの開始(POST /api/reindex)と状態(GET /api/reindex/status)、バックグラウンド実行。

use crate::error::{ApiError, ApiResult};
use crate::indexer::{self, IndexError, IndexOptions};
use crate::indexlock::{peek_running_pid, pid_alive};
use crate::params::Params;
use crate::state::{AppState, ReindexCtl};
use axum::extract::{RawQuery, State};
use axum::Json;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// 実行中フラグを、取り込みスレッドがどう終わっても(panic を含めて)必ず下ろす。
/// 下ろし損ねると、以後の取り込み・削除・初期化がサーバを再起動するまで 409 になり続ける。
struct RunningFlag(Arc<ReindexCtl>);

impl Drop for RunningFlag {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let mut s = self.0.status();
            s.last_error = Some("取り込みが内部エラーで中断されました".into());
            s.finished_at = Some(indexer::now());
        }
        self.0.running.store(false, Ordering::SeqCst);
    }
}

/// 取り込みをバックグラウンドで始める。始めたら true。既に実行中・削除や初期化の最中なら何もせず false
/// (呼び出し側は事前に確認して 409 を返している。ここでの確認は競合時の保険)。
pub fn start_background(st: &AppState, mode: &str, only_member: Option<String>) -> bool {
    let Ok(guard) = st.reindex.run_lock.clone().try_lock_owned() else { return false };
    let ctl = st.reindex.clone();
    if ctl.running.swap(true, Ordering::SeqCst) {
        return false;
    }
    {
        let mut s = ctl.status();
        s.started_at = Some(indexer::now());
        s.last_error = None;
        s.progress = None;
    }
    let cfg = st.cfg.read().unwrap().clone();
    let opts = IndexOptions { mode: mode.to_string(), rebuild: false, only_member, no_thumbs: false };
    tokio::task::spawn_blocking(move || {
        // 宣言の逆順に解放される: 実行中フラグを下ろしてから、削除・初期化との排他を解く
        let _guard = guard; // 取り込みが終わるまで削除・初期化と排他する
        let _running = RunningFlag(ctl.clone());
        let cb_ctl = ctl.clone();
        let cb = move |p: Value| cb_ctl.status().progress = Some(p);
        let res = indexer::run_index(&cfg, &opts, Some(&cb));
        let mut s = ctl.status();
        match res {
            Ok(summary) => s.last_summary = Some(summary.to_json()),
            Err(IndexError::AlreadyRunning(e)) => {
                // 別プロセス(CLI の取り込みなど)が実行中
                logln!("[reindex] skipped, already running elsewhere: {e}");
                s.last_error = Some(format!("already_running: {e}"));
            }
            Err(e) => {
                logln!("[reindex] error: {e}");
                s.last_error = Some(e.to_string());
            }
        }
        s.finished_at = Some(indexer::now());
    });
    true
}

/// 取り込みを開始する。mode: incremental(既定。直近Nヶ月)| full(全期間)。member 指定でそのメンバーのみ。
/// DB は作り直さないため、お気に入り・サムネは保持される。
pub async fn start(State(st): State<AppState>, RawQuery(q): RawQuery) -> ApiResult<Json<Value>> {
    let p = Params::parse(q);
    let mode = p.opt("mode").unwrap_or_else(|| "incremental".into());
    let member = p.opt("member");

    if st.root().is_none() {
        return Err(ApiError::status_conflict("root_not_configured"));
    }
    if mode != "incremental" && mode != "full" {
        return Err(ApiError::bad_request(format!("invalid mode: {mode}, expected 'incremental' or 'full'")));
    }
    if st.reindex.running.load(Ordering::SeqCst) {
        return Err(ApiError::already_running(None));
    }
    let lock_path = st.cfg.read().unwrap().index_lock_path();
    if let Some(pid) = peek_running_pid(&lock_path) {
        return Err(ApiError::already_running(Some(format!("index process pid={pid} is running"))));
    }
    if !start_background(&st, &mode, member.clone()) {
        // 確認の後に、別の取り込み・メンバー削除・初期化が先に始まった
        return Err(ApiError::already_running(None));
    }
    Ok(Json(json!({ "status": "started", "mode": mode, "member": member })))
}

/// 取り込みの状態。自プロセスで実行中ならメモリ上の状態、そうでなければ progress.json
/// (CLI など別プロセスの取り込みも見える)から組み立てる。
pub async fn status(State(st): State<AppState>) -> Json<Value> {
    let running = st.reindex.running.load(Ordering::SeqCst);
    let s = st.reindex.status().clone();
    let state = json!({
        "running": running,
        "started_at": s.started_at,
        "finished_at": s.finished_at,
        "progress": s.progress,
        "last_summary": s.last_summary,
        "last_error": s.last_error,
    });
    if running {
        return Json(state);
    }

    let path = st.cfg.read().unwrap().progress_json_path();
    let pf: Value = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) | Err(_) => {
                logln!("[reindex/status] failed to read progress.json");
                return Json(state);
            }
        },
        Err(_) => return Json(state),
    };

    let pf_running = pf.get("running").and_then(Value::as_bool).unwrap_or(false);
    let pf_pid = pf.get("pid").and_then(Value::as_i64);
    let get = |k: &str| pf.get(k).cloned().unwrap_or(Value::Null);

    if let Some(pid) = pf_pid.filter(|&pid| pf_running && !pid_alive(pid)) {
        // 進捗ファイルは実行中のままなのに、書いたプロセスがいない = 中断された
        return Json(json!({
            "running": false,
            "started_at": get("started_at"),
            "finished_at": get("updated_at"),
            "progress": {
                "source": "file",
                "phase": "interrupted",
                "done": get("done"),
                "total": get("total"),
                "percent": get("percent"),
                "pid": pid,
            },
            "last_summary": state["last_summary"],
            "last_error": format!("index process (pid={pid}) is not running (interrupted)"),
        }));
    }

    let mut progress = pf.clone();
    progress["source"] = json!("file");
    let phase = pf.get("phase").and_then(Value::as_str);
    Json(json!({
        "running": pf_running,
        "started_at": get("started_at"),
        "finished_at": if pf_running { Value::Null } else { get("updated_at") },
        "progress": progress,
        "last_summary": if !pf_running && phase == Some("done") { get("summary") } else { state["last_summary"].clone() },
        "last_error": if !pf_running && phase == Some("error") { get("message") } else { state["last_error"].clone() },
    }))
}
