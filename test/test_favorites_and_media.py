"""お気に入り(user.db分離)・メディアパス取得・メディアギャラリーAPIのテスト。"""
from __future__ import annotations

from pathlib import Path

import pytest

from conftest import make_member_file, wait_for_reindex_idle


def _index_one_message(client, app_module, root: Path, member="member_a", yyyymm="202608",
                        msg_id=1, flag=0, ts_raw="20260805120000", ext="txt", content="hello"):
    make_member_file(root, member, yyyymm, msg_id, flag, ts_raw, ext=ext, content=content)
    client.post("/api/settings", json={"root": str(root)})
    wait_for_reindex_idle(app_module)


class TestFavoritesEndpoint:
    def test_add_favorite_for_unknown_message_returns_404(self, client):
        res = client.post("/api/favorites", json={"member": "nobody", "msg_id": 999})
        assert res.status_code == 404

    def test_add_favorite_missing_params_rejected(self, client):
        res = client.post("/api/favorites", json={"member": "x"})
        assert res.status_code == 400

    def test_add_and_list_and_remove_favorite(self, client, app_module, tmp_path: Path):
        root = tmp_path / "messages"
        _index_one_message(client, app_module, root, member="member_a", msg_id=1, content="hello world")

        res = client.post("/api/favorites", json={"member": "member_a", "msg_id": 1})
        assert res.status_code == 200
        assert res.json() == {"status": "ok", "favorited": True}

        # 冪等: 同じものをもう一度追加してもエラーにならない (INSERT OR IGNORE)
        res2 = client.post("/api/favorites", json={"member": "member_a", "msg_id": 1})
        assert res2.status_code == 200

        listed = client.get("/api/favorites").json()
        assert len(listed["favorites"]) == 1
        fav = listed["favorites"][0]
        assert fav["member"] == "member_a"
        assert fav["msg_id"] == 1
        assert fav["body"] == "hello world"
        assert "created_at" in fav

        ids = client.get("/api/favorites/ids", params={"member": "member_a"}).json()
        assert ids["msg_ids"] == [1]

        res3 = client.delete("/api/favorites", params={"member": "member_a", "msg_id": 1})
        assert res3.status_code == 200
        assert res3.json() == {"status": "ok", "favorited": False}

        listed2 = client.get("/api/favorites").json()
        assert listed2["favorites"] == []

        ids2 = client.get("/api/favorites/ids", params={"member": "member_a"}).json()
        assert ids2["msg_ids"] == []

    def test_remove_nonexistent_favorite_is_noop(self, client):
        res = client.delete("/api/favorites", params={"member": "nobody", "msg_id": 1})
        assert res.status_code == 200
        assert res.json()["favorited"] is False

    def test_list_favorites_filtered_by_member(self, client, app_module, tmp_path: Path):
        """memberを指定すると、そのメンバーのお気に入りだけに絞り込まれる。"""
        root = tmp_path / "messages"
        make_member_file(root, "member_a", "202608", 1, 0, "20260805120000", content="from a")
        make_member_file(root, "member_b", "202608", 1, 0, "20260805130000", content="from b")
        client.post("/api/settings", json={"root": str(root)})
        wait_for_reindex_idle(app_module)

        client.post("/api/favorites", json={"member": "member_a", "msg_id": 1})
        client.post("/api/favorites", json={"member": "member_b", "msg_id": 1})

        listed_a = client.get("/api/favorites", params={"member": "member_a"}).json()
        assert len(listed_a["favorites"]) == 1
        assert listed_a["favorites"][0]["member"] == "member_a"

        listed_b = client.get("/api/favorites", params={"member": "member_b"}).json()
        assert len(listed_b["favorites"]) == 1
        assert listed_b["favorites"][0]["member"] == "member_b"

        # member未指定なら従来どおり全メンバー横断で返る(後方互換)
        listed_all = client.get("/api/favorites").json()
        assert len(listed_all["favorites"]) == 2

    def test_list_favorites_filtered_by_member_with_before(self, client, app_module, tmp_path: Path):
        """member指定時もbeforeによるページングが正しく効く(他メンバーのcreated_atに影響されない)。"""
        root = tmp_path / "messages"
        make_member_file(root, "member_a", "202608", 1, 0, "20260805120000", content="a1")
        make_member_file(root, "member_a", "202608", 2, 0, "20260805130000", content="a2")
        make_member_file(root, "member_b", "202608", 1, 0, "20260805140000", content="b1")
        client.post("/api/settings", json={"root": str(root)})
        wait_for_reindex_idle(app_module)

        client.post("/api/favorites", json={"member": "member_a", "msg_id": 1})
        client.post("/api/favorites", json={"member": "member_a", "msg_id": 2})
        client.post("/api/favorites", json={"member": "member_b", "msg_id": 1})

        # created_atはint(time.time())秒精度のため、テスト実行が速いと3件とも同秒になり
        # beforeによる順序保証を検証できない。明示的にずらして書き戻す。
        with app_module._user_db_lock:
            conn = app_module.get_user_conn()
            conn.execute("UPDATE favorites SET created_at=100 WHERE member='member_a' AND msg_id=1")
            conn.execute("UPDATE favorites SET created_at=200 WHERE member='member_a' AND msg_id=2")
            conn.execute("UPDATE favorites SET created_at=300 WHERE member='member_b' AND msg_id=1")
            conn.commit()

        first_page = client.get("/api/favorites", params={"member": "member_a", "limit": 1}).json()
        assert len(first_page["favorites"]) == 1
        assert first_page["favorites"][0]["msg_id"] == 2
        oldest_created_at = first_page["favorites"][0]["created_at"]

        second_page = client.get(
            "/api/favorites",
            params={"member": "member_a", "limit": 1, "before": oldest_created_at},
        ).json()
        assert len(second_page["favorites"]) == 1
        assert second_page["favorites"][0]["member"] == "member_a"
        assert second_page["favorites"][0]["msg_id"] != first_page["favorites"][0]["msg_id"]

    def test_favorites_list_skips_entries_missing_from_messages(self, client, app_module, tmp_path: Path):
        """rebuild等でindex.db側の行が消えた場合、user.db側のfavoritesは残っても一覧からは除外される。"""
        root = tmp_path / "messages"
        _index_one_message(client, app_module, root, member="member_a", msg_id=1, content="hello")

        res = client.post("/api/favorites", json={"member": "member_a", "msg_id": 1})
        assert res.status_code == 200

        # index.db を reset で消す(user.dbは--rebuild同様に触られないはずだが、
        # ここではresetの挙動を使ってmessages側の行を消し、favoritesが独立して残ることを確認する)
        # reset自体はuser.dbも削除するため、ここではmessagesテーブルから直接該当行を消す方が
        # 「index.db再構築でfavoritesが孤立するケース」を厳密に再現できる。
        conn = app_module.get_conn()
        conn.execute("DELETE FROM messages WHERE member=? AND msg_id=?", ("member_a", 1))
        conn.commit()

        listed = client.get("/api/favorites").json()
        assert listed["favorites"] == []

        # 一方でfavorites自体(user.db)はまだ残っている
        ids = client.get("/api/favorites/ids", params={"member": "member_a"}).json()
        assert ids["msg_ids"] == [1]

    def test_reset_clears_favorites(self, client, app_module, tmp_path: Path):
        root = tmp_path / "messages"
        _index_one_message(client, app_module, root, member="member_a", msg_id=1, content="hello")
        client.post("/api/favorites", json={"member": "member_a", "msg_id": 1})

        res = client.post("/api/reset")
        assert res.status_code == 200

        ids = client.get("/api/favorites/ids", params={"member": "member_a"}).json()
        assert ids["msg_ids"] == []


class TestMediaPathEndpoint:
    def test_media_path_for_text_message_returns_404(self, client, app_module, tmp_path: Path):
        root = tmp_path / "messages"
        _index_one_message(client, app_module, root, member="member_a", msg_id=1, ext="txt", content="hi")

        res = client.get("/api/media-path", params={"member": "member_a", "msg_id": 1})
        assert res.status_code == 404

    def test_media_path_for_unknown_message_returns_404(self, client):
        res = client.get("/api/media-path", params={"member": "nobody", "msg_id": 999})
        assert res.status_code == 404

    def test_media_path_returns_absolute_path_and_exists_flag(self, client, app_module, tmp_path: Path):
        root = tmp_path / "messages"
        _index_one_message(
            client, app_module, root, member="member_a", msg_id=2, flag=1, ext="jpg",
        )

        res = client.get("/api/media-path", params={"member": "member_a", "msg_id": 2})
        assert res.status_code == 200
        body = res.json()
        assert Path(body["path"]).is_absolute()
        assert body["exists"] is True
        assert Path(body["path"]).exists()

    def test_media_path_reports_missing_file(self, client, app_module, tmp_path: Path):
        root = tmp_path / "messages"
        _index_one_message(client, app_module, root, member="member_a", msg_id=3, flag=1, ext="jpg")

        # 実ファイルを消して欠損状態を作る(missing判定の再インデックスはさせず、
        # DBのmedia列だけ見て絶対パスを組み立てる挙動を確認する)
        file_path = next(root.rglob("*.jpg"))
        file_path.unlink()

        res = client.get("/api/media-path", params={"member": "member_a", "msg_id": 3})
        assert res.status_code == 200
        assert res.json()["exists"] is False

    def test_media_path_root_not_configured_returns_409(self, client, app_module):
        # root未設定だがmessagesにmedia付き行があるという不自然な状態を直接作り、
        # root未設定時の分岐(409)を検証する。
        conn = app_module.get_conn()
        conn.execute(
            """INSERT INTO messages (member, group_, msg_id, ts, ts_raw, body, media, kind, flag, thumb, missing)
               VALUES ('member_x', NULL, 1, 0, '20260101000000', NULL, 'member_x/2026/202601/1_1_20260101000000.jpg', 'image', 1, NULL, 0)"""
        )
        conn.commit()

        res = client.get("/api/media-path", params={"member": "member_x", "msg_id": 1})
        assert res.status_code == 409


class TestMediaListEndpoint:
    def test_media_list_invalid_kind_rejected(self, client):
        res = client.get("/api/media-list", params={"member": "x", "kind": "bogus"})
        assert res.status_code == 400

    def test_media_list_filters_by_kind_and_excludes_missing(self, client, app_module, tmp_path: Path):
        root = tmp_path / "messages"
        make_member_file(root, "member_a", "202608", 1, 1, "20260805120000", ext="jpg")  # image
        make_member_file(root, "member_a", "202608", 2, 2, "20260805130000", ext="mp4")  # video
        make_member_file(root, "member_a", "202608", 3, 0, "20260805140000", ext="txt", content="text msg")

        client.post("/api/settings", json={"root": str(root)})
        wait_for_reindex_idle(app_module)

        res_image = client.get("/api/media-list", params={"member": "member_a", "kind": "image"})
        assert res_image.status_code == 200
        body = res_image.json()
        assert len(body["items"]) == 1
        assert body["items"][0]["kind"] == "image"

        res_all_media = client.get("/api/media-list", params={"member": "member_a"})
        kinds = {item["kind"] for item in res_all_media.json()["items"]}
        assert kinds == {"image", "video"}  # text は除外される

        # missing=1の行はギャラリーから除外される
        conn = app_module.get_conn()
        conn.execute("UPDATE messages SET missing=1 WHERE member='member_a' AND msg_id=1")
        conn.commit()

        res_after_missing = client.get("/api/media-list", params={"member": "member_a", "kind": "image"})
        assert res_after_missing.json()["items"] == []

    def test_media_list_pagination_has_more(self, client, app_module, tmp_path: Path):
        root = tmp_path / "messages"
        for i in range(3):
            make_member_file(root, "member_a", "202608", i + 1, 1, f"2026080512000{i}", ext="jpg")
        client.post("/api/settings", json={"root": str(root)})
        wait_for_reindex_idle(app_module)

        res = client.get("/api/media-list", params={"member": "member_a", "kind": "image", "limit": 2})
        body = res.json()
        assert len(body["items"]) == 2
        assert body["has_more"] is True

        res2 = client.get("/api/media-list", params={"member": "member_a", "kind": "image", "limit": 10})
        assert res2.json()["has_more"] is False
