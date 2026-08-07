"""config.py のユニットテスト。"""
from __future__ import annotations

from pathlib import Path

import pytest

from conftest import write_config_json

import config as config_module
from config import clear_root, load_config, save_root


class TestLoadConfig:
    def test_root_empty_string_becomes_none(self, tmp_path: Path):
        cfg_path = write_config_json(tmp_path / "config.json", root="")
        cfg = load_config(cfg_path)
        assert cfg.root is None

    def test_root_set_resolves_to_path(self, tmp_path: Path):
        root_dir = tmp_path / "messages"
        root_dir.mkdir()
        cfg_path = write_config_json(tmp_path / "config.json", root=str(root_dir))
        cfg = load_config(cfg_path)
        assert cfg.root == root_dir.resolve()

    def test_relative_paths_resolved_against_config_dir(self, tmp_path: Path):
        cfg_path = write_config_json(tmp_path / "config.json")
        cfg = load_config(cfg_path)
        assert cfg.data_dir == (tmp_path / "data").resolve()
        assert cfg.thumbs_dir == (tmp_path / "data" / "thumbs").resolve()
        assert cfg.db_path == (tmp_path / "data" / "index.db").resolve()

    def test_data_dirs_are_created(self, tmp_path: Path):
        cfg_path = write_config_json(tmp_path / "config.json")
        cfg = load_config(cfg_path)
        assert cfg.data_dir.is_dir()
        assert cfg.thumbs_dir.is_dir()
        assert cfg.db_path.parent.is_dir()

    def test_port_is_int(self, tmp_path: Path):
        cfg_path = write_config_json(tmp_path / "config.json", port=9001)
        cfg = load_config(cfg_path)
        assert cfg.port == 9001
        assert isinstance(cfg.port, int)

    def test_missing_required_key_raises(self, tmp_path: Path):
        cfg_path = tmp_path / "config.json"
        cfg_path.write_text('{"root": ""}', encoding="utf-8")
        with pytest.raises(KeyError):
            load_config(cfg_path)


class TestEnvOverride:
    """環境変数によるconfig.json値の上書き(空文字は無視される)を検証する。"""

    def test_env_root_overrides_config(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
        override_dir = tmp_path / "env_root"
        override_dir.mkdir()
        cfg_path = write_config_json(tmp_path / "config.json", root="")
        monkeypatch.setenv("MSGVIEWER_ROOT", str(override_dir))
        cfg = load_config(cfg_path)
        assert cfg.root == override_dir.resolve()

    def test_empty_env_root_falls_back_to_config_json(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
        """回帰テスト: 環境変数が空文字で定義されていても、config.jsonの値が使われること。"""
        preset_dir = tmp_path / "configured_root"
        preset_dir.mkdir()
        cfg_path = write_config_json(tmp_path / "config.json", root=str(preset_dir))
        monkeypatch.setenv("MSGVIEWER_ROOT", "")
        cfg = load_config(cfg_path)
        assert cfg.root == preset_dir.resolve()

    def test_empty_env_port_falls_back_to_config_json(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
        cfg_path = write_config_json(tmp_path / "config.json", port=8123)
        monkeypatch.setenv("MSGVIEWER_PORT", "")
        cfg = load_config(cfg_path)
        assert cfg.port == 8123

    def test_env_port_overrides_config(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
        cfg_path = write_config_json(tmp_path / "config.json", port=8123)
        monkeypatch.setenv("MSGVIEWER_PORT", "9999")
        cfg = load_config(cfg_path)
        assert cfg.port == 9999

    def test_no_env_var_uses_config_json(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
        monkeypatch.delenv("MSGVIEWER_ROOT", raising=False)
        preset_dir = tmp_path / "root_from_json"
        preset_dir.mkdir()
        cfg_path = write_config_json(tmp_path / "config.json", root=str(preset_dir))
        cfg = load_config(cfg_path)
        assert cfg.root == preset_dir.resolve()


class TestSaveAndClearRoot:
    def test_save_root_persists_to_config_json(self, tmp_path: Path):
        new_root = tmp_path / "new_messages"
        new_root.mkdir()
        cfg_path = write_config_json(tmp_path / "config.json", root="")
        cfg = load_config(cfg_path)

        resolved = save_root(cfg, str(new_root))

        assert resolved == new_root.resolve()
        assert cfg.root == new_root.resolve()

        # ファイルに実際に書き込まれていること(再読み込みでも同じ値になる)
        reloaded = load_config(cfg_path)
        assert reloaded.root == new_root.resolve()

    def test_save_root_does_not_touch_other_keys(self, tmp_path: Path):
        new_root = tmp_path / "new_messages"
        new_root.mkdir()
        cfg_path = write_config_json(tmp_path / "config.json", root="", port=8123)
        cfg = load_config(cfg_path)

        save_root(cfg, str(new_root))

        reloaded = load_config(cfg_path)
        assert reloaded.port == 8123

    def test_clear_root_resets_to_none(self, tmp_path: Path):
        existing_root = tmp_path / "existing"
        existing_root.mkdir()
        cfg_path = write_config_json(tmp_path / "config.json", root=str(existing_root))
        cfg = load_config(cfg_path)
        assert cfg.root is not None

        clear_root(cfg)

        assert cfg.root is None
        reloaded = load_config(cfg_path)
        assert reloaded.root is None

    def test_save_then_clear_round_trip(self, tmp_path: Path):
        new_root = tmp_path / "messages"
        new_root.mkdir()
        cfg_path = write_config_json(tmp_path / "config.json", root="")
        cfg = load_config(cfg_path)

        save_root(cfg, str(new_root))
        assert cfg.root is not None

        clear_root(cfg)
        assert cfg.root is None

        reloaded = load_config(cfg_path)
        assert reloaded.root is None


class TestGetConfig:
    def test_returns_singleton(self, monkeypatch: pytest.MonkeyPatch):
        monkeypatch.setattr(config_module, "_config_singleton", None)
        original_load = config_module.load_config

        def fake_load(config_path=None):
            return original_load(config_path)

        monkeypatch.setattr(config_module, "load_config", fake_load)
        # get_config()はプロジェクト実物のconfig.jsonを読むため、存在確認のみ行う
        cfg1 = config_module.get_config()
        cfg2 = config_module.get_config()
        assert cfg1 is cfg2
