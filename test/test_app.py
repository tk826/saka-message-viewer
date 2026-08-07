"""app.py のAPIエンドポイントの統合テスト(TestClient使用)。"""
from __future__ import annotations

from pathlib import Path

import pytest

from conftest import make_member_file, wait_for_reindex_idle


class TestSettingsEndpoint:
    def test_get_settings_initially_unconfigured(self, client):
        res = client.get("/api/settings")
        assert res.status_code == 200
        assert res.json() == {"root": None, "configured": False}

    def test_post_settings_empty_root_rejected(self, client):
        res = client.post("/api/settings", json={"root": ""})
        assert res.status_code == 400

    def test_post_settings_relative_path_rejected(self, client, tmp_path: Path):
        res = client.post("/api/settings", json={"root": "relative/path"})
        assert res.status_code == 400

    def test_post_settings_nonexistent_dir_rejected(self, client):
        res = client.post("/api/settings", json={"root": "/definitely/does/not/exist/xyz"})
        assert res.status_code == 400

    def test_post_settings_valid_dir_persists(self, client, tmp_path: Path):
        new_root = tmp_path / "messages"
        new_root.mkdir()

        res = client.post("/api/settings", json={"root": str(new_root)})
        assert res.status_code == 200
        body = res.json()
        assert body["configured"] is True
        assert Path(body["root"]) == new_root.resolve()

        res2 = client.get("/api/settings")
        assert res2.json()["configured"] is True


class TestResetEndpoint:
    def test_reset_clears_root_and_data(self, client, app_module, tmp_path: Path):
        new_root = tmp_path / "messages"
        new_root.mkdir()
        client.post("/api/settings", json={"root": str(new_root)})
        wait_for_reindex_idle(app_module)
        assert app_module.cfg.root is not None

        res = client.post("/api/reset")
        assert res.status_code == 200
        assert res.json()["status"] == "reset"

        settings = client.get("/api/settings").json()
        assert settings["configured"] is False

    def test_reset_removes_db_file(self, client, app_module, tmp_path: Path):
        new_root = tmp_path / "messages"
        new_root.mkdir()
        client.post("/api/settings", json={"root": str(new_root)})
        wait_for_reindex_idle(app_module)

        db_path = app_module.cfg.db_path
        assert db_path.exists()

        client.post("/api/reset")
        # reset後、次回アクセスで空DBが自動再作成されるが、直後は削除されている(または空)
        threads = client.get("/api/threads").json()
        assert threads == []


class TestThreadsAndMessages:
    def test_threads_empty_when_no_data(self, client):
        res = client.get("/api/threads")
        assert res.status_code == 200
        assert res.json() == []

    def test_messages_before_and_after_together_rejected(self, client):
        res = client.get("/api/messages", params={"member": "x", "before": 1, "after": 1})
        assert res.status_code == 400

    def test_messages_for_unknown_member_returns_empty(self, client):
        res = client.get("/api/messages", params={"member": "nobody"})
        assert res.status_code == 200
        assert res.json()["messages"] == []


class TestJumpAndCalendar:
    def test_jump_no_match_returns_null_ts(self, client):
        res = client.get("/api/jump", params={"member": "x", "date": "2026-01-01"})
        assert res.status_code == 200
        assert res.json()["ts"] is None

    def test_jump_invalid_date_format_rejected(self, client):
        res = client.get("/api/jump", params={"member": "x", "date": "not-a-date"})
        assert res.status_code == 400

    def test_calendar_empty_for_unknown_member(self, client):
        res = client.get("/api/calendar", params={"member": "nobody"})
        assert res.status_code == 200
        assert res.json()["calendar"] == []

    def test_calendar_yearmonth_invalid_format_rejected(self, client):
        res = client.get("/api/calendar", params={"member": "x", "yearmonth": "2026/08"})
        assert res.status_code == 400

    def test_calendar_yearmonth_returns_daily_counts(self, client, app_module, tmp_path: Path):
        root = tmp_path / "messages"
        make_member_file(root, "member_a", "202608", 1, 0, "20260805120000", content="a")
        make_member_file(root, "member_a", "202608", 2, 0, "20260805130000", content="b")
        make_member_file(root, "member_a", "202608", 3, 0, "20260807120000", content="c")

        client.post("/api/settings", json={"root": str(root)})
        wait_for_reindex_idle(app_module)

        res = client.get("/api/calendar", params={"member": "member_a", "yearmonth": "2026-08"})
        assert res.status_code == 200
        body = res.json()
        assert body["yearmonth"] == "2026-08"
        days = {d["date"]: d["count"] for d in body["days"]}
        assert days == {"2026-08-05": 2, "2026-08-07": 1}


class TestSearch:
    def test_search_short_query_uses_like_fallback(self, client):
        res = client.get("/api/search", params={"q": "ab"})
        assert res.status_code == 200
        assert res.json()["used_fts"] is False

    def test_search_long_query_uses_fts(self, client):
        res = client.get("/api/search", params={"q": "abcdef"})
        assert res.status_code == 200
        assert res.json()["used_fts"] is True

    def test_search_empty_query_rejected(self, client):
        res = client.get("/api/search", params={"q": ""})
        assert res.status_code == 422


class TestReindexEndpoint:
    def test_reindex_without_root_configured_returns_409(self, client):
        res = client.post("/api/reindex")
        assert res.status_code == 409

    def test_reindex_status_when_idle(self, client):
        res = client.get("/api/reindex/status")
        assert res.status_code == 200
        assert res.json()["running"] is False


class TestMediaEndpoint:
    def test_media_without_root_returns_409(self, client):
        res = client.get("/media/some/path.jpg")
        assert res.status_code == 409

    def test_media_path_traversal_rejected(self, client, tmp_path: Path):
        new_root = tmp_path / "messages"
        new_root.mkdir()
        client.post("/api/settings", json={"root": str(new_root)})

        res = client.get("/media/../../../etc/passwd")
        assert res.status_code in (403, 404)

    def test_media_nonexistent_file_returns_404(self, client, tmp_path: Path):
        new_root = tmp_path / "messages"
        new_root.mkdir()
        client.post("/api/settings", json={"root": str(new_root)})

        res = client.get("/media/does_not_exist.jpg")
        assert res.status_code == 404


class TestIndexHtml:
    def test_root_serves_index_html(self, client):
        res = client.get("/")
        assert res.status_code == 200
        assert "text/html" in res.headers["content-type"]


class TestFullFlowWithIndexedData:
    """設定→取り込み→APIでの参照までの一連の流れを検証する。"""

    def test_settings_then_index_then_query(self, client, app_module, tmp_path: Path):
        root = tmp_path / "messages"
        make_member_file(root, "member_a", "202608", 1, 0, "20260805120000", content="hello world")

        res = client.post("/api/settings", json={"root": str(root)})
        assert res.status_code == 200

        # POST /api/settings はバックグラウンドスレッドで差分取り込みを自動開始するため、
        # その完了を待ってからAPI経由で結果を検証する。
        wait_for_reindex_idle(app_module)

        threads = client.get("/api/threads").json()
        assert len(threads) == 1
        assert threads[0]["member"] == "member_a"

        messages = client.get("/api/messages", params={"member": "member_a"}).json()
        assert len(messages["messages"]) == 1
        assert messages["messages"][0]["body"] == "hello world"
