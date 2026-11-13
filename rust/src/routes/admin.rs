//! 管理系: メンバー削除(DELETE /api/members/{member}) と 初期化(POST /api/reset)。
//!
//! どちらも取り込みと相互排他する。取り込み中(自プロセス、または `.index.lock` の PID が生存する別プロセス)なら409。

use crate::config::write_root;
use crate::db::open_index_db;
use crate::error::{blocking, ApiError, ApiResult};
use crate::indexlock::peek_running_pid;
use crate::routes::media::safe_resolve_no_exist_check;
use crate::state::AppState;
use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::OwnedMutexGuard;

/// 取り込みとの排他を確保する。返したガードを持っている間、自プロセスの取り込みは始まらない。
fn exclusive(st: &AppState) -> ApiResult<OwnedMutexGuard<()>> {
    let guard = st
        .reindex
        .run_lock
        .clone()
        .try_lock_owned()
        .map_err(|_| ApiError::already_running(None))?;
    if st.reindex.running.load(Ordering::SeqCst) {
        return Err(ApiError::already_running(None));
    }
    let lock_path = st.cfg.read().unwrap().index_lock_path();
    if let Some(pid) = peek_running_pid(&lock_path) {
        return Err(ApiError::already_running(Some(format!("index process pid={pid} is running"))));
    }
    Ok(guard)
}

// ------------------------------------------------------------------ メンバー削除

/// 指定メンバーのメッセージ・サムネ・お気に入り・コメントを削除する。root 側のファイルには触れない
/// (root に同名メンバーが残っていれば次回の取り込みで復活する。表示設定 hidden/order は消さない)。
///
/// 検索索引 messages_fts の行も同時に消す。索引行が孤児として残ると、最後に取り込まれたメンバーを削除して
/// 再取り込みしたときに rowid が衝突して取り込みが失敗する。
pub async fn delete_member(State(st): State<AppState>, AxPath(member): AxPath<String>) -> ApiResult<Json<Value>> {
    if member.is_empty() {
        return Err(ApiError::bad_request("member は必須です"));
    }
    let _guard = exclusive(&st)?;

    let m = member.clone();
    let (deleted, thumbs): (usize, Vec<String>) = st
        .db
        .run(move |c| {
            let thumbs = {
                let mut stmt = c.prepare("SELECT thumb FROM messages WHERE member=? AND thumb IS NOT NULL")?;
                let rows = stmt.query_map([&m], |r| r.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let tx = c.unchecked_transaction()?;
            tx.execute("DELETE FROM messages_fts WHERE rowid IN (SELECT rowid FROM messages WHERE member=?)", [&m])?;
            let n = tx.execute("DELETE FROM messages WHERE member=?", [&m])?;
            tx.commit()?;
            Ok((n, thumbs))
        })
        .await?;

    let thumbs_dir = st.cfg.read().unwrap().thumbs_dir.clone();
    blocking(move || {
        for rel in thumbs {
            if let Ok(p) = safe_resolve_no_exist_check(&thumbs_dir, &rel) {
                let _ = std::fs::remove_file(p);
            }
        }
    })
    .await?;

    let m = member.clone();
    st.user_db
        .run(move |c| {
            c.execute("DELETE FROM favorites WHERE member=?", [&m])?;
            c.execute("DELETE FROM message_comments WHERE member=?", [&m])
        })
        .await?;

    Ok(Json(json!({ "status": "ok", "member": member, "deleted_messages": deleted })))
}

// ------------------------------------------------------------------ 初期化

/// ファイルを消す。Windows では閉じ切っていないハンドル(別プロセスの取り込み直後など)があると
/// 失敗しうるため、少しの間だけ再試行する。
fn remove_file_retry(p: &Path) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match std::fs::remove_file(p) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// DB・サムネ・お気に入り等・保存フォルダ設定をすべて消し、初回起動相当に戻す(取り消し不可)。
pub async fn reset(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let _guard = exclusive(&st)?;
    let cfg = st.cfg.read().unwrap().clone();


    let st2 = st.clone();
    blocking(move || -> std::io::Result<()> {
        // index.db は全接続を閉じ、新しい接続を作らせない状態で消して作り直す
        st2.db.while_closed(|| -> std::io::Result<()> {
            for p in [cfg.db_path.clone(), with_suffix(&cfg.db_path, "-wal"), with_suffix(&cfg.db_path, "-shm")] {
                remove_file_retry(&p)?;
            }
            open_index_db(&cfg.db_path, true).map_err(std::io::Error::other)?;
            Ok(())
        })?;

        if cfg.thumbs_dir.exists() {
            std::fs::remove_dir_all(&cfg.thumbs_dir)?;
        }
        std::fs::create_dir_all(&cfg.thumbs_dir)?;

        for p in [cfg.progress_json_path(), cfg.index_lock_path()] {
            remove_file_retry(&p)?;
        }

        // user.db は接続を閉じたままロックを握って消す(途中で他のリクエストが開き直さないように)。
        // 次の利用時に空のスキーマで開き直される
        let udb = cfg.user_db_path();
        st2.user_db.while_closed(|| {
            for p in [udb.clone(), with_suffix(&udb, "-wal"), with_suffix(&udb, "-shm")] {
                remove_file_retry(&p)?;
            }
            Ok(())
        })
    })
    .await?
    .map_err(ApiError::internal)?;

    {
        let mut c = st.cfg.write().unwrap();
        write_root(&c, "").map_err(ApiError::internal)?;
        c.root = None;
    }
    st.reindex.clear_status();
    Ok(Json(json!({ "status": "reset" })))
}
