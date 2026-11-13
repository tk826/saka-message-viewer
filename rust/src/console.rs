//! コンソール周り: ダブルクリックで起動されたかの判定、失敗時の一時停止、ブラウザを開く。
//!
//! exe をエクスプローラからダブルクリックすると、このプロセスのためだけのコンソールが作られ、終了と同時に閉じる。
//! そのままだと起動失敗のメッセージが一瞬で消えて読めないため、その場合だけ Enter を待ってから終了する。
//! ターミナル(cmd・PowerShell)やテストから起動した場合は待たない。

/// ダブルクリックなどで、このプロセス専用のコンソールで動いているか
/// (コンソールに付いているプロセスが自分だけ。ターミナルから起動したならシェルも付いている)。
#[cfg(windows)]
pub fn launched_standalone() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleProcessList;
    let mut pids = [0u32; 2];
    // SAFETY: 長さを正しく渡したバッファに、コンソールに付いているプロセスIDを書き込ませるだけ。
    unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) == 1 }
}

#[cfg(not(windows))]
pub fn launched_standalone() -> bool {
    false
}

/// ダブルクリックで起動していれば、Enter を押すまでウィンドウを閉じずに待つ。
pub fn pause_if_standalone() {
    if launched_standalone() {
        eprintln!();
        eprintln!("Enter キーを押すとこのウィンドウを閉じます。");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

/// 起動を続けられない失敗。ログに残し、ダブルクリックで起動していれば Enter を待ってから終了する。
pub fn fatal_exit(msg: &str) -> ! {
    logln!("{msg}");
    pause_if_standalone();
    std::process::exit(1);
}

/// 既定のブラウザで URL を開く(Windows のみ。失敗しても何もしない)。
pub fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(not(windows))]
    let _ = url;
}
