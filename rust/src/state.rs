//! アプリ全体の共有状態。

use crate::config::Config;
use crate::db::{Db, UserDb};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

/// このプロセス内で動く取り込みの最新状態(/api/reindex/status 用)。
#[derive(Default, Clone)]
pub struct ReindexStatus {
    pub started_at: Option<f64>,
    pub finished_at: Option<f64>,
    /// 実行中の進捗(`{"stage": ..., ...}`)
    pub progress: Option<Value>,
    pub last_summary: Option<Value>,
    pub last_error: Option<String>,
}

/// 取り込み(reindex)の実行制御。
///
/// ロックは2種類に分ける:
/// - `run_lock`: 取り込み・削除・初期化の相互排他。取り込みの間ずっと握る長時間ロック。取れなければ待たずに409
/// - `status`: 状態の読み書きだけを守る短命ロック。状態を読むだけの API は `run_lock` を待たない
#[derive(Default)]
pub struct ReindexCtl {
    pub run_lock: Arc<tokio::sync::Mutex<()>>,
    pub running: AtomicBool,
    /// 直接ロックせず `status()` を使う
    status: Mutex<ReindexStatus>,
}

impl ReindexCtl {
    /// 状態のロック。保持中のスレッドが panic していても(poison)中身はそのまま使う
    /// (状態は単純な値の集まりで、途中で壊れて困る不変条件が無いため。ここで panic すると状態APIが500になり続ける)。
    pub fn status(&self) -> MutexGuard<'_, ReindexStatus> {
        self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `/api/reset` 後に取り込み状態(最後の結果・エラー・進捗)を消す。
    pub fn clear_status(&self) {
        *self.status() = ReindexStatus::default();
    }
}

#[derive(Clone)]
pub struct AppState {
    /// `root` は /api/settings で変わるため RwLock。
    pub cfg: Arc<RwLock<Config>>,
    pub db: Arc<Db>,
    pub user_db: Arc<UserDb>,
    pub static_dir: PathBuf,
    pub reindex: Arc<ReindexCtl>,
}

impl AppState {
    pub fn root(&self) -> Option<PathBuf> {
        self.cfg.read().unwrap().root.clone()
    }
    pub fn is_dev(&self) -> bool {
        self.cfg.read().unwrap().is_dev()
    }
}
