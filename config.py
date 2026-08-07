"""設定読み込みモジュール。

config.json を読み込み、相対パスは config.json のあるディレクトリ基準で解決する。
config-dev.json が存在する場合はそちらを優先して読み込む(gitignore対象、個人環境用の
root等をコミット対象のconfig.jsonから分離するため)。
環境変数での上書きに対応する:
  MSGVIEWER_ROOT, MSGVIEWER_DATA_DIR, MSGVIEWER_DB_PATH, MSGVIEWER_THUMBS_DIR, MSGVIEWER_PORT
"""
from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

_CONFIG_FILENAME = "config.json"
_CONFIG_DEV_FILENAME = "config-dev.json"
_BASE_DIR = Path(__file__).resolve().parent


@dataclass
class Config:
    root: Optional[Path]
    data_dir: Path
    thumbs_dir: Path
    db_path: Path
    port: int
    thumb_size: int
    thumb_quality: int
    incremental_months: int
    auto_index_on_startup: bool
    base_dir: Path
    config_path: Path

    @property
    def index_lock_path(self) -> Path:
        return self.data_dir / ".index.lock"

    @property
    def progress_json_path(self) -> Path:
        return self.data_dir / "progress.json"

    @property
    def user_db_path(self) -> Path:
        # お気に入り等ユーザー由来のデータはindex.dbと別ファイルにする。
        # /api/reset や --rebuild で index.db が消えても、ユーザーが作ったデータは
        # 残したい（残ってしまう、ではなく意図的に分離している）ため。
        return self.data_dir / "user.db"


def _resolve(base: Path, value: str) -> Path:
    p = Path(value)
    if not p.is_absolute():
        p = (base / p).resolve()
    else:
        p = p.resolve()
    return p


def load_config(config_path: str | os.PathLike | None = None) -> Config:
    if config_path:
        path = Path(config_path)
    else:
        dev_path = _BASE_DIR / _CONFIG_DEV_FILENAME
        path = dev_path if dev_path.exists() else (_BASE_DIR / _CONFIG_FILENAME)
    with open(path, "r", encoding="utf-8") as f:
        raw = json.load(f)

    base = path.resolve().parent

    def _env_or(name: str, default):
        # 環境変数が定義されていても空文字の場合は「未指定」として扱い、
        # config.json 側の値にフォールバックする(空文字での意図しない上書き防止)。
        v = os.environ.get(name)
        return v if v else default

    root = _env_or("MSGVIEWER_ROOT", raw.get("root", ""))
    data_dir = _env_or("MSGVIEWER_DATA_DIR", raw["data_dir"])
    db_path = _env_or("MSGVIEWER_DB_PATH", raw["db_path"])
    thumbs_dir = _env_or("MSGVIEWER_THUMBS_DIR", raw["thumbs_dir"])
    port = int(_env_or("MSGVIEWER_PORT", raw["port"]))

    cfg = Config(
        root=_resolve(base, root) if root else None,
        data_dir=_resolve(base, data_dir),
        thumbs_dir=_resolve(base, thumbs_dir),
        db_path=_resolve(base, db_path),
        port=port,
        thumb_size=int(raw["thumb_size"]),
        thumb_quality=int(raw["thumb_quality"]),
        incremental_months=int(raw["incremental_months"]),
        auto_index_on_startup=bool(raw["auto_index_on_startup"]),
        base_dir=base,
        config_path=path,
    )

    cfg.data_dir.mkdir(parents=True, exist_ok=True)
    cfg.thumbs_dir.mkdir(parents=True, exist_ok=True)
    cfg.db_path.parent.mkdir(parents=True, exist_ok=True)

    return cfg


def save_root(cfg: Config, new_root: str) -> Path:
    """config.json の root を更新し、Config.root を差し替える。"""
    with open(cfg.config_path, "r", encoding="utf-8") as f:
        raw = json.load(f)
    raw["root"] = new_root
    with open(cfg.config_path, "w", encoding="utf-8") as f:
        json.dump(raw, f, ensure_ascii=False, indent=2)
        f.write("\n")

    resolved = _resolve(cfg.base_dir, new_root)
    cfg.root = resolved
    return resolved


def clear_root(cfg: Config) -> None:
    """config.json の root を未設定(空文字)に戻し、Config.root を None にする。"""
    with open(cfg.config_path, "r", encoding="utf-8") as f:
        raw = json.load(f)
    raw["root"] = ""
    with open(cfg.config_path, "w", encoding="utf-8") as f:
        json.dump(raw, f, ensure_ascii=False, indent=2)
        f.write("\n")

    cfg.root = None


# モジュールレベルのシングルトン（app.py / indexer.py から共有）
_config_singleton: Config | None = None


def get_config() -> Config:
    global _config_singleton
    if _config_singleton is None:
        _config_singleton = load_config()
    return _config_singleton
