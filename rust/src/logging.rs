//! ログ。exe として常駐させても後から追えるよう、各行の先頭に日時(ローカル時刻)を付ける。
//!
//! 出力先は stderr。サーバとして起動したときは `open_file` で `data\server_log.txt` にも追記する
//! (ウィンドウを閉じるとコンソールの表示は消えるため)。
//!
//! `main.rs` で他のモジュールより先に `#[macro_use] mod logging;` と宣言しているため、
//! クレート内のどこからでも `logln!(...)` を `eprintln!` と同じ書式で使える。

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

macro_rules! logln {
    ($($arg:tt)*) => {
        $crate::logging::write(format_args!($($arg)*))
    };
}

/// 追記先のログファイル(開いていなければ None)。
static FILE: Mutex<Option<File>> = Mutex::new(None);

fn now() -> impl std::fmt::Display {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
}

/// 1行を `2026-11-13 12:34:56 <本文>` の形で書く(1回の書き込みで出すので、スレッド間で行が混ざらない)。
pub fn write(args: std::fmt::Arguments) {
    let line = format!("{} {args}", now());
    eprintln!("{line}");
    let mut file = FILE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(f) = file.as_mut() {
        // ファイルに書けなくてもサーバは止めない(画面には出ている)
        let _ = writeln!(f, "{line}");
    }
}

/// 以後のログを `path` にも追記する。開けたら起動の区切り行を書く。
/// 開けなければ(他のプロセスがロックしているなど)警告だけ出し、画面への出力だけで続ける。
pub fn open_file(path: &Path) {
    let opened = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| File::options().create(true).append(true).open(path))
        .and_then(|mut f| writeln!(f, "========== {} 起動 ==========", now()).map(|()| f));
    match opened {
        Ok(f) => *FILE.lock().unwrap_or_else(PoisonError::into_inner) = Some(f),
        Err(e) => logln!("警告: ログファイル ({}) に書き込めないため、ログを画面にだけ出します: {e}", path.display()),
    }
}
