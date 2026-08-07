"""pytest共通フィクスチャ。

プロジェクトルート(app.py/config.py/indexer.pyのある場所)をsys.pathに追加し、
テストごとに隔離されたconfig.json/data_dir/rootディレクトリを用意する。
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

import config as config_module


@pytest.fixture(autouse=True)
def _reset_config_singleton():
    """config.get_config()のモジュールレベルシングルトンをテスト間で汚染しないようにする。"""
    config_module._config_singleton = None
    yield
    config_module._config_singleton = None


def write_config_json(path: Path, **overrides) -> Path:
    """テスト用config.jsonを書き出す。overridesでキーを上書きできる。"""
    base = {
        "root": "",
        "data_dir": "./data",
        "thumbs_dir": "./data/thumbs",
        "db_path": "./data/index.db",
        "port": 8000,
        "thumb_size": 240,
        "thumb_quality": 80,
        "incremental_months": 2,
        "auto_index_on_startup": False,
    }
    base.update(overrides)
    path.write_text(json.dumps(base, ensure_ascii=False, indent=2), encoding="utf-8")
    return path


@pytest.fixture
def config_path(tmp_path: Path) -> Path:
    """rootが未設定の素のconfig.jsonを配置したパスを返す。"""
    return write_config_json(tmp_path / "config.json")


def make_member_file(
    root: Path,
    member: str,
    yyyymm: str,
    msg_id: int,
    flag: int,
    ts_raw: str,
    ext: str = "txt",
    group: str | None = None,
    content: str | None = None,
) -> Path:
    """root配下にmessage-viewerのファイル名規則に従うファイルを1件作成する。

    パターンA: root/{member}/{YYYY}/{YYYYMM}/{file}
    パターンB: root/{group}/{member}/{YYYY}/{YYYYMM}/{file}
    """
    yyyy = yyyymm[:4]
    base = root / group / member if group else root / member
    month_dir = base / yyyy / yyyymm
    month_dir.mkdir(parents=True, exist_ok=True)
    filename = f"{msg_id}_{flag}_{ts_raw}.{ext}"
    file_path = month_dir / filename
    if ext == "txt":
        file_path.write_text(content if content is not None else "", encoding="utf-8")
    else:
        file_path.write_bytes(b"\xff\xd8\xff")  # 最小限のJPEGマジックバイト風ダミー
    return file_path


@pytest.fixture
def app_module(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """app.py を隔離されたconfig/data_dir/root環境でロードし直したモジュールを返す。

    app.py はモジュールロード時に `cfg = get_config()` をグローバルスコープで
    実行するため、テストごとに環境変数でパスを上書きしたうえで importlib.reload する。
    """
    import importlib

    data_dir = tmp_path / "data"
    thumbs_dir = data_dir / "thumbs"
    db_path = data_dir / "index.db"
    cfg_path = write_config_json(tmp_path / "config.json")

    monkeypatch.setenv("MSGVIEWER_ROOT", "")
    monkeypatch.setenv("MSGVIEWER_DATA_DIR", str(data_dir))
    monkeypatch.setenv("MSGVIEWER_THUMBS_DIR", str(thumbs_dir))
    monkeypatch.setenv("MSGVIEWER_DB_PATH", str(db_path))
    monkeypatch.setenv("MSGVIEWER_PORT", "8000")

    config_module._config_singleton = None

    # config.pyのload_configはconfig_path引数が無い場合 _BASE_DIR/config.json を見るため、
    # _BASE_DIR自体をテスト用ディレクトリに向ける。
    monkeypatch.setattr(config_module, "_BASE_DIR", tmp_path)
    (tmp_path / "config.json").write_text(cfg_path.read_text(encoding="utf-8"), encoding="utf-8")

    import app as app_module_ref

    importlib.reload(app_module_ref)
    yield app_module_ref

    if app_module_ref._conn is not None:
        app_module_ref._conn.close()
        app_module_ref._conn = None
    if app_module_ref._user_conn is not None:
        app_module_ref._user_conn.close()
        app_module_ref._user_conn = None


@pytest.fixture
def client(app_module):
    from fastapi.testclient import TestClient

    with TestClient(app_module.app) as c:
        yield c


def wait_for_reindex_idle(app_module, timeout: float = 5.0) -> None:
    """POST /api/settings や /api/reindex が起動するバックグラウンド取り込みスレッドの
    完了を待つ。テストが直後にDB/ロックへ触れて競合するのを防ぐ。
    """
    import time

    deadline = time.time() + timeout
    while time.time() < deadline:
        with app_module._reindex_lock:
            if not app_module._reindex_state["running"]:
                return
        time.sleep(0.02)
    raise TimeoutError("background reindex did not finish in time")
