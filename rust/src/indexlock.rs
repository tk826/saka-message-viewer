//! 取り込みの多重実行防止ロック(`data/.index.lock`)。サーバ内の取り込みと CLI(`message-viewer index`)の
//! 両方がこのファイルで排他する。
//!
//! - ロックファイルの1行目が保持者の PID、2行目が取得時刻。
//! - 取得は「新規作成(既存なら失敗)」で原子的に行う。既存ロックの PID が死んでいれば stale として奪う。
//! - 取得できなければ待たずに `IndexAlreadyRunning` を返す。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 既に他の取り込みが実行中。
#[derive(Debug)]
pub struct IndexAlreadyRunning(pub String);

impl std::fmt::Display for IndexAlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 取得済みのロック。drop で解放(ファイル削除)する。
pub struct IndexLock {
    path: PathBuf,
}

impl IndexLock {
    fn try_create(path: &Path) -> std::io::Result<bool> {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut f) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                write!(f, "{}
{}
", std::process::id(), now)?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn acquire(path: &Path) -> Result<IndexLock, IndexAlreadyRunning> {
        let err = |pid: Option<i64>| {
            let pid = pid.map(|p| p.to_string()).unwrap_or_else(|| "None".into());
            IndexAlreadyRunning(format!("index already running (pid={pid}, lock={})", path.display()))
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if Self::try_create(path).map_err(|_| err(None))? {
            return Ok(IndexLock { path: path.to_path_buf() });
        }
        // 既存ロックがある。保持者が生きていれば実行中
        let existing = read_lock_pid(path);
        if let Some(pid) = existing.filter(|&p| pid_alive(p)) {
            return Err(err(Some(pid)));
        }
        // stale なロックは奪う。消した直後に他プロセスが先に作っていたら負けを認める
        let _ = std::fs::remove_file(path);
        if Self::try_create(path).map_err(|_| err(None))? {
            return Ok(IndexLock { path: path.to_path_buf() });
        }
        Err(err(read_lock_pid(path)))
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// ロックファイル1行目の PID。読めない・数値でなければ None。
pub fn read_lock_pid(lock_path: &Path) -> Option<i64> {
    let content = std::fs::read_to_string(lock_path).ok()?;
    content.trim().lines().next()?.trim().parse().ok()
}

/// ロックを取らずに、保持者が生存していればその PID を返す(UI の早期応答用。最終判断には使わない)。
pub fn peek_running_pid(lock_path: &Path) -> Option<i64> {
    read_lock_pid(lock_path).filter(|&pid| pid_alive(pid))
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
            assert!(e.0.starts_with(&format!("index already running (pid={}", std::process::id())), "{}", e.0);
        }
        assert!(!p.exists(), "drop で解放される");
        // 死んだ PID のロックは奪える
        std::fs::write(&p, "999999999
0
").unwrap();
        let l = IndexLock::acquire(&p).unwrap();
        assert_eq!(read_lock_pid(&p), Some(std::process::id() as i64));
        drop(l);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lock_file_parsing() {
        let dir = std::env::temp_dir().join(format!("mv_lock_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(".index.lock");
        std::fs::write(&p, format!("{}\n1700000000.0\n", std::process::id())).unwrap();
        assert_eq!(peek_running_pid(&p), Some(std::process::id() as i64));
        std::fs::write(&p, "garbage\n").unwrap();
        assert_eq!(read_lock_pid(&p), None);
        std::fs::remove_file(&p).unwrap();
        assert_eq!(read_lock_pid(&p), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
