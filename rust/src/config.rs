//! 設定読み込み(docs/api-spec.md 第6章)。
//!
//! - `--config` 指定が無ければ、実行ファイルのあるディレクトリの `config-dev.json` → `config.json`
//! - 相対パスは設定ファイルのディレクトリ基準
//! - `MSGVIEWER_*` 環境変数は「空でなければ」優先
//! - `config-dev.json` を使っているときだけ `is_dev`

use serde::Deserialize;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const CONFIG_FILENAME: &str = "config.json";
const CONFIG_DEV_FILENAME: &str = "config-dev.json";

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    root: String,
    data_dir: String,
    thumbs_dir: String,
    db_path: String,
    port: u16,
    thumb_size: u32,
    thumb_quality: u8,
    incremental_months: u32,
    auto_index_on_startup: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub root: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub thumbs_dir: PathBuf,
    pub db_path: PathBuf,
    pub port: u16,
    pub thumb_size: u32,
    pub thumb_quality: u8,
    pub incremental_months: u32,
    pub auto_index_on_startup: bool,
    pub base_dir: PathBuf,
    pub config_path: PathBuf,
}

impl Config {
    pub fn index_lock_path(&self) -> PathBuf {
        self.data_dir.join(".index.lock")
    }
    pub fn progress_json_path(&self) -> PathBuf {
        self.data_dir.join("progress.json")
    }
    /// お気に入り等ユーザー由来データ。index.db とは別ファイル(reset/rebuild の対象外にするため)。
    pub fn user_db_path(&self) -> PathBuf {
        self.data_dir.join("user.db")
    }
    pub fn is_dev(&self) -> bool {
        self.config_path.file_name().and_then(|n| n.to_str()) == Some(CONFIG_DEV_FILENAME)
    }
}

fn env_or(name: &str, default: String) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => default,
    }
}

/// 相対パスは `base` 基準。存在しないパスでも失敗しないよう
/// 正規化は字句的に行う(Windows の `\\?\` 付きパスを避けるため `canonicalize` は使わない)。
pub fn resolve_path(base: &Path, value: &str) -> PathBuf {
    let p = Path::new(value);
    let joined = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    normalize_lexical(&joined)
}

/// `..` と `.` を字句的に畳む(ファイルシステムは見ない)。
pub fn normalize_lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn default_config_path() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let dev = base.join(CONFIG_DEV_FILENAME);
    if dev.exists() {
        dev
    } else {
        base.join(CONFIG_FILENAME)
    }
}

pub fn load_config(config_path: Option<&Path>) -> Result<Config, String> {
    let path = config_path.map(Path::to_path_buf).unwrap_or_else(default_config_path);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw: RawConfig = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;

    let abs = if path.is_absolute() { path.clone() } else { std::env::current_dir().map_err(|e| e.to_string())?.join(&path) };
    let base = normalize_lexical(&abs).parent().map(Path::to_path_buf).unwrap_or_default();

    let root = env_or("MSGVIEWER_ROOT", raw.root);
    let data_dir = env_or("MSGVIEWER_DATA_DIR", raw.data_dir);
    let db_path = env_or("MSGVIEWER_DB_PATH", raw.db_path);
    let thumbs_dir = env_or("MSGVIEWER_THUMBS_DIR", raw.thumbs_dir);
    let port: u16 = env_or("MSGVIEWER_PORT", raw.port.to_string())
        .parse()
        .map_err(|e| format!("MSGVIEWER_PORT: {e}"))?;

    let cfg = Config {
        root: if root.is_empty() { None } else { Some(resolve_path(&base, &root)) },
        data_dir: resolve_path(&base, &data_dir),
        thumbs_dir: resolve_path(&base, &thumbs_dir),
        db_path: resolve_path(&base, &db_path),
        port,
        thumb_size: raw.thumb_size,
        thumb_quality: raw.thumb_quality,
        incremental_months: raw.incremental_months,
        auto_index_on_startup: raw.auto_index_on_startup,
        base_dir: base,
        config_path: abs,
    };

    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&cfg.thumbs_dir).map_err(|e| e.to_string())?;
    if let Some(parent) = cfg.db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    Ok(cfg)
}

/// 設定ファイルの `root` だけを書き換える(他のキーと順序は保つ)。
/// 2スペースのインデント + 末尾改行で書く。キー順を保つため serde_json の `preserve_order` feature を有効にしている。
pub fn write_root(cfg: &Config, new_root: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(&cfg.config_path).map_err(|e| e.to_string())?;
    let mut v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let obj: &mut Map<String, Value> = v.as_object_mut().ok_or("config is not an object")?;
    obj.insert("root".into(), Value::String(new_root.into()));
    let mut out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    out.push('\n');
    std::fs::write(&cfg.config_path, out).map_err(|e| e.to_string())
}
