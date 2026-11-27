//! 管理系: メンバー削除(DELETE /api/members/{member}) と 初期化(POST /api/reset)。
//!
//! どちらも取り込みと相互排他する。取り込み中(自プロセス、または `.index.lock` を握る別プロセス)なら409。

use crate::config::{write_root, Config};
use crate::db::{open_index_db, open_thumbs_db};
use crate::error::{blocking, ApiError, ApiResult};
use crate::indexer::thumb_member_prefix;
use crate::indexlock::{read_lock_pid, IndexLock};
use crate::state::AppState;
use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::OwnedMutexGuard;

/// 取り込みとの排他を確保する。返したガードを持っている間、自プロセスの取り込みも、
/// 別プロセス(CLI)の取り込みも始まらない(確認だけでなく `.index.lock` を実際に握る)。
fn exclusive(st: &AppState) -> ApiResult<(OwnedMutexGuard<()>, IndexLock)> {
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
    let lock = IndexLock::acquire(&lock_path).map_err(|e| {
        if e.io_error {
            // 競合ではない(権限不足など)。「実行中」と返すと原因に気付けない
            ApiError::internal(e)
        } else {
            ApiError::already_running(read_lock_pid(&lock_path).map(|pid| format!("index process pid={pid} is running")))
        }
    })?;
    Ok((guard, lock))
}

// ------------------------------------------------------------------ メンバー削除

/// 指定メンバーのメッセージ・サムネ・お気に入り・コメントを削除する。root 側のファイルには触れない
/// (root に同名メンバーが残っていれば次回の取り込みで復活する。表示設定 hidden/order は消さない)。
///
/// 検索索引 messages_fts の行も同時に消す。索引行が孤児として残ると、最後に取り込まれたメンバーを削除して
/// 再取り込みしたときに rowid が衝突して取り込みが失敗する。
///
/// index.db・user.db・thumbs.db は別ファイルで1つのトランザクションにできないため、user.db(お気に入り・コメント)→
/// index.db(メッセージ・検索索引)→ thumbs.db(サムネ)の順に消す。途中で失敗してもメッセージが残っていれば
/// メンバーは一覧に出たままなので、画面からもう一度削除すれば最後まで消せる
/// (逆順だと、メッセージだけ消えて一覧から見えなくなり、お気に入り・コメントが見えないまま残る)。
pub async fn delete_member(State(st): State<AppState>, AxPath(member): AxPath<String>) -> ApiResult<Json<Value>> {
    if member.is_empty() {
        return Err(ApiError::bad_request("member は必須です"));
    }
    let _guard = exclusive(&st)?;

    let m = member.clone();
    st.user_db
        .run(move |c| {
            let tx = c.unchecked_transaction()?;
            tx.execute("DELETE FROM favorites WHERE member=?", [&m])?;
            tx.execute("DELETE FROM message_comments WHERE member=?", [&m])?;
            tx.commit()
        })
        .await?;

    let m = member.clone();
    let deleted: usize = st
        .db
        .run(move |c| {
            let tx = c.unchecked_transaction()?;
            tx.execute("DELETE FROM messages_fts WHERE rowid IN (SELECT rowid FROM messages WHERE member=?)", [&m])?;
            let n = tx.execute("DELETE FROM messages WHERE member=?", [&m])?;
            tx.commit()?;
            Ok(n)
        })
        .await?;

    // サムネのキーはメンバー名の sha1 で始まるので、先頭一致で消す(作れなかった印も)。
    // 消した分の領域はファイルを縮めずに残り、以後のサムネの書き込みに使われる
    let pattern = format!("{}*", thumb_member_prefix(&member));
    st.thumbs_db
        .run(move |c| {
            let tx = c.unchecked_transaction()?;
            tx.execute("DELETE FROM thumbs WHERE path GLOB ?", [&pattern])?;
            tx.execute("DELETE FROM thumb_failures WHERE path GLOB ?", [&pattern])?;
            tx.commit()
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

/// 初期化で、消す前に一時的に付ける名前の接尾辞。
const RESET_TRASH_SUFFIX: &str = ".resetting";

/// 初期化で消すもの(無いものは飛ばす)。.index.lock はこの処理自身が握っているので含めない(PID ファイルは解放時に消える)。
/// 旧版のサムネのフォルダは、開かれているファイルがあると名前を変えられないことが多いので最後に置く(戻す数が増えないように)。
/// 旧版のサムネのフォルダは、data フォルダの下にあるときだけ消す(設定で別のフォルダを指していると、
/// 中身を確かめずに丸ごと消すと無関係なファイルを消しうる。data フォルダの外なら利用者が消す)。
fn reset_targets(cfg: &Config) -> Vec<PathBuf> {
    let udb = cfg.user_db_path();
    let tdb = cfg.thumbs_db_path();
    let legacy = &cfg.legacy_thumbs_dir;
    let legacy_in_data = legacy != &cfg.data_dir && legacy.starts_with(&cfg.data_dir);
    let mut v = vec![
        cfg.db_path.clone(),
        with_suffix(&cfg.db_path, "-wal"),
        with_suffix(&cfg.db_path, "-shm"),
        udb.clone(),
        with_suffix(&udb, "-wal"),
        with_suffix(&udb, "-shm"),
        tdb.clone(),
        with_suffix(&tdb, "-wal"),
        with_suffix(&tdb, "-shm"),
        cfg.progress_json_path(),
    ];
    if legacy_in_data {
        v.push(legacy.clone());
    }
    v
}

fn trash_path(p: &Path) -> PathBuf {
    with_suffix(p, RESET_TRASH_SUFFIX)
}

fn exists_no_follow(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// ファイルでもフォルダでも消す。無ければ成功扱い。
fn remove_any(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => remove_file_retry(p),
    }
}

/// 名前を変える。Windows では閉じ切っていないハンドルがあると失敗しうるため、少しの間だけ再試行する。
fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// 前回の初期化で消し損ねたもの(`*.resetting`)を消す。起動時と初期化の前に呼ぶ。失敗は警告だけ。
pub fn cleanup_reset_leftovers(cfg: &Config) {
    for t in reset_targets(cfg) {
        let tp = trash_path(&t);
        if exists_no_follow(&tp)
            && let Err(e) = remove_any(&tp)
        {
            logln!("[reset] warning: 前回の初期化の残り {} を削除できませんでした: {e}", tp.display());
        }
    }
}

/// 段階1: 対象をすべて一時的な名前に変える。1つでも変えられなければ、変えた分を戻して Err(何も変わっていない)。
/// Windows では開かれているファイル(やそれを含むフォルダ)は名前も変えられないので、何かを消す前に失敗を検知できる。
fn move_aside(targets: &[PathBuf]) -> std::io::Result<Vec<PathBuf>> {
    let mut moved = Vec::new();
    for t in targets {
        if !exists_no_follow(t) {
            continue;
        }
        if let Err(e) = rename_retry(t, &trash_path(t)) {
            restore(&moved);
            return Err(std::io::Error::new(e.kind(), format!("{} を移動できませんでした: {e}", t.display())));
        }
        moved.push(t.clone());
    }
    Ok(moved)
}

/// `move_aside` で名前を変えたものを元に戻す。
fn restore(moved: &[PathBuf]) {
    for t in moved.iter().rev() {
        if let Err(e) = std::fs::rename(trash_path(t), t) {
            logln!("[reset] error: {} を元に戻せませんでした: {e}", t.display());
        }
    }
}

/// 空の状態を作る(index.db・thumbs.db のスキーマ、設定ファイルの root を空に)。
/// user.db は次の利用時に空のスキーマで開き直される。
fn create_fresh(cfg: &Config) -> std::io::Result<()> {
    open_index_db(&cfg.db_path, true).map_err(std::io::Error::other)?;
    open_thumbs_db(&cfg.thumbs_db_path(), true).map_err(std::io::Error::other)?;
    // 一時ファイルに書いてから置き換えるので、失敗しても設定ファイルは元のまま
    write_root(cfg, "").map_err(std::io::Error::other)?;
    Ok(())
}

/// DB・サムネ(thumbs.db と旧版のサムネのフォルダ)・お気に入り等・保存フォルダ設定をすべて消し、初回起動相当に戻す(取り消し不可)。
///
/// 途中で失敗して中途半端に消えた状態を残さないよう、2段階で行う。
/// 1. 消す対象をすべて一時的な名前(`*.resetting`)に変え、空の状態を作って設定ファイルの root を空にする。
///    どこかで失敗したら、作りかけを消して名前を元に戻す(何も変わっていない状態で 500)。
/// 2. 一時的な名前のものを消す。消し損ねても初期化は済んでいて、残りは次の初期化・起動時に消す。
pub async fn reset(State(st): State<AppState>) -> ApiResult<Json<Value>> {
    let _guard = exclusive(&st)?;
    let cfg = st.cfg.read().unwrap().clone();

    let st2 = st.clone();
    blocking(move || -> Result<(), String> {
        cleanup_reset_leftovers(&cfg);
        let targets = reset_targets(&cfg);
        // すべての DB の接続を閉じ、新しい接続を作らせない状態で行う(Windows では開いたままだと名前を変えられない)
        st2.db.while_closed(|| {
            st2.thumbs_db.while_closed(|| {
                st2.user_db.while_closed(|| -> Result<(), String> {
                    let moved = move_aside(&targets).map_err(|e| format!("初期化できませんでした(何も変更していません): {e}"))?;
                    if let Err(e) = create_fresh(&cfg) {
                        // 作りかけ(新しい index.db・thumbs.db)を消して元に戻す。
                        // 対象のうち実在したものはすべて名前を変えてあるので、今ある対象は作りかけだけ
                        for t in &targets {
                            if let Err(e) = remove_any(t) {
                                logln!("[reset] error: 作りかけの {} を削除できませんでした: {e}", t.display());
                            }
                        }
                        restore(&moved);
                        return Err(format!("初期化できませんでした(何も変更していません): {e}"));
                    }
                    Ok(())
                })
            })
        })?;
        for t in &targets {
            let tp = trash_path(t);
            if let Err(e) = remove_any(&tp) {
                logln!("[reset] warning: {} を削除できませんでした(次の初期化・起動時に再度消します): {e}", tp.display());
            }
        }
        Ok(())
    })
    .await?
    .map_err(ApiError::internal)?;

    let root_from_env = {
        let mut c = st.cfg.write().unwrap();
        c.root = None;
        c.root_from_env
    };
    st.reindex.clear_status();
    // 環境変数 MSGVIEWER_ROOT で root を上書きしていると、次の起動でその値に戻る(画面で知らせる)
    Ok(Json(json!({ "status": "reset", "root_env_override": root_from_env })))
}
