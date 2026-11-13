//! 坂道メッセージビューア。
//!
//! 静的配信・外部アクセス対策・読み取りAPI・/media と /thumbs の配信・検索・お気に入りなどのユーザーデータ・
//! 取り込み(インデクサ。サーバ内のバックグラウンド実行と CLI `message-viewer index`)。仕様は docs/api-spec.md。
//!
//! 使い方:
//!   message-viewer [--config <path>]                    サーバを起動(既定。exe のダブルクリックでもよい)
//!   message-viewer [--config <path>] index [--full|--rebuild] [--only-member X] [--no-thumbs]

// logln! マクロを他のモジュールから使えるよう、最初に宣言する
#[macro_use]
mod logging;
mod config;
mod console;
mod db;
mod error;
mod guard;
mod indexer;
mod indexlock;
mod message;
mod params;
mod routes;
mod search;
mod state;
mod timeutil;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use clap::{Parser, Subcommand};
use serde_json::json;
use state::AppState;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tower_http::services::ServeDir;

#[derive(Parser, Debug)]
#[command(name = "message-viewer", about = "坂道メッセージビューア")]
struct Cli {
    /// 設定ファイルのパス(省略時は実行ファイルと同じ場所の config-dev.json → config.json)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// サーバを起動する(サブコマンド省略時の既定)
    Serve,
    /// 取り込みを実行する
    Index {
        /// 全フォルダを走査する
        #[arg(long)]
        full: bool,
        /// 直近Nヶ月のみ走査する(既定)
        #[arg(long)]
        incremental: bool,
        /// DB を作り直して全走査する
        #[arg(long)]
        rebuild: bool,
        /// 特定メンバーのみ
        #[arg(long)]
        only_member: Option<String>,
        /// サムネ生成をスキップする
        #[arg(long)]
        no_thumbs: bool,
    },
}

/// `static/` の場所。設定ファイルと同じディレクトリを優先し、無ければ実行ファイルの隣。
fn find_static_dir(cfg: &config::Config) -> PathBuf {
    let a = cfg.base_dir.join("static");
    if a.is_dir() {
        return a;
    }
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.join("static"))) {
        if d.is_dir() {
            return d;
        }
    }
    a
}

async fn index(State(st): State<AppState>) -> Response {
    match tokio::fs::read(st.static_dir.join("index.html")).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({ "detail": "Not Found" }))).into_response(),
    }
}

async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "detail": "Not Found" }))).into_response()
}

fn build_router(st: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/threads", get(routes::read::threads))
        .route("/api/messages", get(routes::read::messages))
        .route("/api/search", get(routes::search::search))
        .route("/api/jump", get(routes::read::jump))
        .route("/api/calendar", get(routes::read::calendar))
        .route("/api/media-list", get(routes::read::media_list))
        .route("/api/media-path", get(routes::media::media_path))
        .route("/media/{*path}", get(routes::media::get_media))
        .route("/thumbs/{*path}", get(routes::media::get_thumb))
        .route(
            "/api/favorites",
            get(routes::user::list_favorites)
                .post(routes::user::add_favorite)
                .delete(routes::user::remove_favorite),
        )
        .route("/api/favorites/ids", get(routes::user::favorite_ids))
        .route("/api/comments", axum::routing::put(routes::user::update_comment))
        .route(
            "/api/hidden-members",
            get(routes::user::list_hidden_members).put(routes::user::update_hidden_member),
        )
        .route(
            "/api/member-order",
            get(routes::user::get_member_order).put(routes::user::update_member_order),
        )
        .route("/api/members/{member}", delete(routes::admin::delete_member))
        .route("/api/reset", post(routes::admin::reset))
        .route("/api/reindex", post(routes::reindex::start))
        .route("/api/reindex/status", get(routes::reindex::status))
        .route("/api/settings", get(routes::settings::get_settings).post(routes::settings::set_settings))
        .nest_service("/static/js", ServeDir::new(st.static_dir.join("js")))
        .fallback(not_found)
        // 全ルート(フォールバック含む)に外部アクセス対策をかける
        .layer(from_fn_with_state(st.clone(), guard::guard))
        .with_state(st)
}

fn main() {
    // panic の内容もログ(と、サーバならログファイル)に残す
    std::panic::set_hook(Box::new(|info| logln!("[panic] {info}")));
    let cli = Cli::parse();
    let cfg = config::load_config(cli.config.as_deref())
        .unwrap_or_else(|e| console::fatal_exit(&format!("設定ファイルを読み込めませんでした: {e}")));
    match cli.command {
        Some(Command::Index { full, incremental: _, rebuild, only_member, no_thumbs }) => {
            let mode = if full || rebuild { "full" } else { "incremental" };
            let opts = indexer::IndexOptions { mode: mode.into(), rebuild, only_member, no_thumbs };
            match indexer::run_index(&cfg, &opts, None) {
                Ok(summary) => indexer::print_summary(&summary),
                Err(indexer::IndexError::AlreadyRunning(e)) => {
                    logln!("[index] 既に実行中のためスキップします: {e}");
                    std::process::exit(2);
                }
                Err(e) => {
                    logln!("[index] error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some(Command::Serve) | None => {
            let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| fatal("tokio runtime", e));
            rt.block_on(serve(cfg));
            // ここに来るのはサーバが異常終了したとき(Ctrl+C やウィンドウを閉じたときはプロセスごと終わる)
            console::pause_if_standalone();
            std::process::exit(1);
        }
    }
}

/// ポートが既に使われているときの案内。二重起動の可能性が高い。
fn report_port_in_use(port: u16) {
    let url = format!("http://localhost:{port}/");
    for line in [
        "========================================".to_string(),
        format!("ポート {port} は既に使用されています。"),
        "サーバーが既に起動している可能性があります。".to_string(),
        String::new(),
        format!("  ・ブラウザで {url} を開いてください"),
        "  ・起動し直したい場合は、既に開いている message-viewer の".to_string(),
        "    黒いウィンドウを閉じるか、そのウィンドウで Ctrl+C を押してください".to_string(),
        "========================================".to_string(),
    ] {
        eprintln!("{line}");
    }
    // ダブルクリックで起動したなら、既に動いているサーバの画面を開いておく
    if console::launched_standalone() {
        console::open_browser(&url);
    }
}

async fn serve(cfg: config::Config) {
    let port = cfg.port;
    // 127.0.0.1 のみで待ち受ける(外部公開しない)。二重起動はここで分かるので、DB を開くより先に行う
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            report_port_in_use(port);
            console::pause_if_standalone();
            std::process::exit(1);
        }
        Err(e) => fatal(&format!("bind 127.0.0.1:{port}"), e),
    };
    // ウィンドウを閉じても後から追えるよう、ログをファイルにも残す
    logging::open_file(&cfg.data_dir.join("server_log.txt"));

    let db = db::Db::new(cfg.db_path.clone()).unwrap_or_else(|e| fatal("index.db", e));
    let user_db = db::UserDb::new(cfg.user_db_path()).unwrap_or_else(|e| fatal("user.db", e));
    let static_dir = find_static_dir(&cfg);
    let auto_index = cfg.auto_index_on_startup && cfg.root.is_some();

    let db_path = cfg.db_path.clone();
    let st = AppState {
        cfg: Arc::new(RwLock::new(cfg)),
        db,
        user_db,
        static_dir,
        reindex: Arc::new(state::ReindexCtl::default()),
    };
    let app = build_router(st.clone());
    logln!("listening on http://127.0.0.1:{port}");
    logln!("ブラウザで http://localhost:{port}/ を開いてください(終了するにはこのウィンドウを閉じるか Ctrl+C)");

    // 孤児の検索索引行を掃除し、設定があれば起動時の差分取り込みを始める
    // (起動を遅らせないよう裏で行う。取り込みは掃除の後)
    tokio::spawn(async move {
        let cleaned = tokio::task::spawn_blocking(move || db::clean_orphan_fts_rows(&db_path)).await;
        match cleaned {
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => logln!("[startup] removed {n} orphan rows from messages_fts"),
            Ok(Err(e)) => logln!("[startup] orphan cleanup failed: {e}"),
            Err(e) => logln!("[startup] orphan cleanup failed: {e}"),
        }
        if auto_index {
            routes::reindex::start_background(&st, "incremental", None);
        }
    });
    if let Err(e) = axum::serve(listener, app).await {
        logln!("server error: {e}");
    }
}

fn fatal<E: std::fmt::Display, T>(what: &str, e: E) -> T {
    console::fatal_exit(&format!("failed to open {what}: {e}"))
}
