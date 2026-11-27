//! 取り込みの多重実行防止ロック(`data/.index.lock`)。サーバ内の取り込みと CLI(`message-viewer index`)の
//! 両方がこのファイルで排他する。
//!
//! - 排他は OS のファイルロック(`File::try_lock`)で行う。ロックはファイルを開いている間だけ有効で、
//!   保持者が異常終了しても OS が解放するため、PID の再利用や書き込み途中の空ファイルで判断を誤らない。
//! - ロックファイル自体は消さない(消すと、開いたままの別プロセスと新しく作ったプロセスが別々のファイルを
//!   ロックして、両方が取得できてしまう)。
//! - 保持者の PID と取得時刻は、表示用に別ファイル `.index.pid` に書く(Windows ではロック中のファイルは
//!   他プロセスから読めないため)。ロックを持っている間だけ存在する。
//! - 取得できなければ待たずに `IndexAlreadyRunning` を返す。

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

/// ロックを取得できなかった。ふつうは既に他の取り込みが実行中。
/// `io_error` が true なら、競合ではなくロックファイルを開けない・ロック操作自体が失敗した
/// (権限不足、data フォルダが無いなど。「実行中」として扱うと原因に気付けない)。
#[derive(Debug)]
pub struct IndexAlreadyRunning {
    pub message: String,
    pub io_error: bool,
}

impl std::fmt::Display for IndexAlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// 取得済みのロック。drop で PID ファイルを消してからロックを解放する。
pub struct IndexLock {
    pid_path: PathBuf,
    /// 開いている間ロックが有効。フィールドは `drop` の本体の後に破棄されるので、解放は PID ファイルを消した後
    _file: File,
}

/// 保持者の PID を書く表示用ファイル(`.index.lock` → `.index.pid`)。
pub fn pid_path(lock_path: &Path) -> PathBuf {
    lock_path.with_extension("pid")
}

fn open_lock_file(lock_path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path)
}

impl IndexLock {
    pub fn acquire(path: &Path) -> Result<IndexLock, IndexAlreadyRunning> {
        let busy = |pid: Option<i64>| {
            let pid = pid.map(|p| p.to_string()).unwrap_or_else(|| "None".into());
            IndexAlreadyRunning {
                message: format!("index already running (pid={pid}, lock={})", path.display()),
                io_error: false,
            }
        };
        let io = |e: std::io::Error| IndexAlreadyRunning {
            message: format!("failed to lock {}: {e}", path.display()),
            io_error: true,
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = open_lock_file(path).map_err(io)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(busy(read_lock_pid(path))),
            Err(TryLockError::Error(e)) => return Err(io(e)),
        }
        // ロックを握った後に書くので、前の保持者が異常終了して残した PID ファイルもここで上書きされる
        let pid_path = pid_path(path);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let _ = std::fs::write(&pid_path, format!("{}\n{now}\n", std::process::id()));
        Ok(IndexLock { pid_path, _file: file })
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        // ロックを握っている間に消すので、消すのは必ず自分が書いた PID ファイル
        let _ = std::fs::remove_file(&self.pid_path);
    }
}

/// 保持者の PID(表示用。PID ファイルの1行目)。読めない・数値でなければ None。
pub fn read_lock_pid(lock_path: &Path) -> Option<i64> {
    let content = std::fs::read_to_string(pid_path(lock_path)).ok()?;
    content.trim().lines().next()?.trim().parse().ok()
}

/// 誰かがロックを握っているか。空いていれば一瞬だけ取って放す。
fn is_locked(lock_path: &Path) -> bool {
    match std::fs::OpenOptions::new().read(true).open(lock_path) {
        Ok(f) => matches!(f.try_lock(), Err(TryLockError::WouldBlock)),
        Err(_) => false,
    }
}

/// 取り込みが実行中なら保持者の PID を返す(UI の早期応答用。最終判断は `IndexLock::acquire`)。
/// PID ファイルの PID が生存しているときだけ、実際にロックが握られているかを確かめる
/// (異常終了で残った PID ファイルの PID が再利用されていても、実行中と誤判定しない)。
pub fn peek_running_pid(lock_path: &Path) -> Option<i64> {
    read_lock_pid(lock_path).filter(|&pid| pid_alive(pid) && is_locked(lock_path))
}

/// 指定 PID のプロセスが生存しているか。
#[cfg(windows)]
pub fn pid_alive(pid: i64) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    const STILL_ACTIVE: u32 = 259;

    if pid <= 0 || pid > u32::MAX as i64 {
        return false;
    }
    // SAFETY: Win32 API にプロセスIDを渡し、得たハンドルは必ず閉じる。
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if handle.is_null() {
            // 別ユーザ所有などで問い合わせできないだけ = 生存しているとみなす
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        // 終了コードが取れなければ安全側(死んでいる)。STILL_ACTIVE 以外は PID 再利用も含め終了済み
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(unix)]
pub fn pid_alive(pid: i64) -> bool {
    if pid <= 0 || pid > i32::MAX as i64 {
        return false;
    }
    // SAFETY: シグナル0は存在確認のみで、プロセスに影響しない。
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if r == 0 {
        return true;
    }
    // EPERM = 別ユーザ所有でシグナルを送れないだけ = 生存しているとみなす
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_alive() {
        assert!(pid_alive(std::process::id() as i64));
        assert!(!pid_alive(0));
        assert!(!pid_alive(-5));
    }

    #[test]
    fn finished_process_is_dead() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) { vec!["/C", "exit"] } else { vec![] })
            .spawn()
            .unwrap();
        let pid = child.id() as i64;
        child.wait().unwrap();
        assert!(!pid_alive(pid));
    }

    #[test]
    fn acquire_release_and_stale() {
        let dir = std::env::temp_dir().join(format!("mv_lock_acq_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(".index.lock");
        let _ = std::fs::remove_file(&p);
        {
            let _l = IndexLock::acquire(&p).unwrap();
            assert_eq!(read_lock_pid(&p), Some(std::process::id() as i64));
            // 自分自身が生きているので2回目は取れない
            let e = IndexLock::acquire(&p).err().unwrap();
            assert!(e.message.starts_with(&format!("index already running (pid={}", std::process::id())), "{e}");
            assert!(!e.io_error);
        }
        assert!(!pid_path(&p).exists(), "drop で PID ファイルを消す");
        assert!(p.exists(), "ロックファイル自体は消さない");
        assert_eq!(peek_running_pid(&p), None);
        // 異常終了で PID ファイルが残り、その PID が生きている(再利用された)場合も、ロックが空いていれば取れる
        std::fs::write(pid_path(&p), format!("{}\n0\n", std::process::id())).unwrap();
        assert_eq!(peek_running_pid(&p), None);
        let l = IndexLock::acquire(&p).unwrap();
        assert_eq!(peek_running_pid(&p), Some(std::process::id() as i64));
        drop(l);
        // 空のロックファイル(旧版が書き込み途中で止まったものなど)も問題にならない
        std::fs::write(&p, "").unwrap();
        drop(IndexLock::acquire(&p).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lock_file_parsing() {
        let dir = std::env::temp_dir().join(format!("mv_lock_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(".index.lock");
        std::fs::write(pid_path(&p), format!("{}\n1700000000.0\n", std::process::id())).unwrap();
        assert_eq!(read_lock_pid(&p), Some(std::process::id() as i64));
        std::fs::write(pid_path(&p), "garbage\n").unwrap();
        assert_eq!(read_lock_pid(&p), None);
        std::fs::remove_file(pid_path(&p)).unwrap();
        assert_eq!(read_lock_pid(&p), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
