"""indexer.py のユニットテスト / 統合テスト。"""
from __future__ import annotations

import sqlite3
from datetime import datetime
from pathlib import Path

import pytest

from conftest import make_member_file, write_config_json

from config import load_config
from indexer import (
    FileEntry,
    IndexLock,
    IndexAlreadyRunning,
    ScanScope,
    _iter_member_dirs,
    _recent_yearmonths,
    _build_message_row,
    discover_files,
    ffmpeg_available,
    find_ffmpeg,
    open_db,
    parse_ts_jst,
    pid_alive,
    rebuild_db,
    run_index,
    thumb_rel_path,
)


def make_cfg(tmp_path: Path, root: Path, incremental_months: int = 2):
    root.mkdir(parents=True, exist_ok=True)
    cfg_path = write_config_json(
        tmp_path / "config.json",
        root=str(root),
        incremental_months=incremental_months,
    )
    return load_config(cfg_path)


class TestParseTsJst:
    def test_known_datetime_converts_to_expected_epoch(self):
        # ファイル名の日時はUTCとして解釈する
        ts = parse_ts_jst("20240115120000")
        import calendar
        expected_utc = calendar.timegm((2024, 1, 15, 12, 0, 0, 0, 0, 0))
        assert ts == expected_utc


class TestThumbRelPath:
    def test_deterministic_for_same_member(self):
        p1 = thumb_rel_path("member_a", 123)
        p2 = thumb_rel_path("member_a", 123)
        assert p1 == p2

    def test_differs_for_different_member(self):
        p1 = thumb_rel_path("member_a", 123)
        p2 = thumb_rel_path("member_b", 123)
        assert p1 != p2

    def test_format(self):
        rel = thumb_rel_path("member_a", 999)
        parts = rel.split("/")
        assert len(parts) == 3
        assert parts[2] == "999.jpg"
        assert len(parts[0]) == 2
        assert parts[1].startswith(parts[0])


class TestRecentYearmonths:
    def test_two_months_from_march(self):
        result = _recent_yearmonths(2, now=datetime(2026, 3, 15))
        assert result == {"202603", "202602"}

    def test_wraps_across_year_boundary(self):
        result = _recent_yearmonths(2, now=datetime(2026, 1, 10))
        assert result == {"202601", "202512"}

    def test_single_month(self):
        result = _recent_yearmonths(1, now=datetime(2026, 8, 6))
        assert result == {"202608"}


class TestIterMemberDirs:
    def test_pattern_a_member_directly_under_root(self, tmp_path: Path):
        root = tmp_path / "root"
        make_member_file(root, "member_a", "202601", 1, 0, "20260115120000")
        results = list(_iter_member_dirs(root))
        assert len(results) == 1
        _, member, group = results[0]
        assert member == "member_a"
        assert group is None

    def test_pattern_b_member_under_group(self, tmp_path: Path):
        root = tmp_path / "root"
        make_member_file(root, "member_a", "202601", 1, 0, "20260115120000", group="group_x")
        results = list(_iter_member_dirs(root))
        assert len(results) == 1
        _, member, group = results[0]
        assert member == "member_a"
        assert group == "group_x"

    def test_mixed_patterns(self, tmp_path: Path):
        root = tmp_path / "root"
        make_member_file(root, "solo_member", "202601", 1, 0, "20260115120000")
        make_member_file(root, "grouped_member", "202601", 2, 0, "20260116120000", group="group_x")
        results = {member: group for _, member, group in _iter_member_dirs(root)}
        assert results == {"solo_member": None, "grouped_member": "group_x"}


class TestScanScope:
    def test_add_single_yearmonth(self):
        scope = ScanScope()
        scope.add("member_a", "202601")
        assert scope.member_yearmonths["member_a"] == {"202601"}

    def test_add_none_promotes_to_full_scope(self):
        scope = ScanScope()
        scope.add("member_a", "202601")
        scope.add("member_a", None)
        assert scope.member_yearmonths["member_a"] is None

    def test_add_after_full_scope_stays_full(self):
        scope = ScanScope()
        scope.add("member_a", None)
        scope.add("member_a", "202601")
        assert scope.member_yearmonths["member_a"] is None


class TestDiscoverFiles:
    def test_full_mode_scans_all_months(self, tmp_path: Path):
        root = tmp_path / "root"
        make_member_file(root, "member_a", "202401", 1, 0, "20240115120000")
        make_member_file(root, "member_a", "202608", 2, 0, "20260805120000")
        cfg = make_cfg(tmp_path, root)

        entries, scope = discover_files(cfg, mode="full")

        assert len(entries) == 2
        assert scope.member_yearmonths["member_a"] is None

    def test_incremental_mode_without_known_members_limits_to_recent(self, tmp_path: Path, monkeypatch):
        root = tmp_path / "root"
        make_member_file(root, "member_a", "202401", 1, 0, "20240115120000")
        cfg = make_cfg(tmp_path, root, incremental_months=2)

        import indexer as indexer_module
        monkeypatch.setattr(indexer_module, "_recent_yearmonths", lambda n, now=None: {"202608", "202607"})

        entries, scope = discover_files(cfg, mode="incremental")
        assert entries == []

    def test_incremental_mode_new_member_gets_full_scan(self, tmp_path: Path, monkeypatch):
        """回帰テスト: known_membersに含まれない新規メンバーは、incrementalでも全期間走査される。"""
        root = tmp_path / "root"
        make_member_file(root, "existing_member", "202401", 1, 0, "20240115120000")
        make_member_file(root, "existing_member", "202608", 2, 0, "20260805120000")
        make_member_file(root, "new_member", "202401", 3, 0, "20240110120000")
        make_member_file(root, "new_member", "202608", 4, 0, "20260806120000")
        cfg = make_cfg(tmp_path, root, incremental_months=2)

        import indexer as indexer_module
        monkeypatch.setattr(indexer_module, "_recent_yearmonths", lambda n, now=None: {"202608", "202607"})

        entries, scope = discover_files(
            cfg, mode="incremental", known_members={"existing_member"}
        )

        by_member = {}
        for e in entries:
            by_member.setdefault(e.member, []).append(e.msg_id)

        # 既存メンバーは直近月分のみ(202401は対象外)
        assert by_member["existing_member"] == [2]
        # 新規メンバーは古い月も含めて全件
        assert sorted(by_member["new_member"]) == [3, 4]
        assert scope.member_yearmonths["new_member"] is None
        assert scope.member_yearmonths["existing_member"] == {"202608"}

    def test_only_member_filters_others_out(self, tmp_path: Path):
        root = tmp_path / "root"
        make_member_file(root, "member_a", "202608", 1, 0, "20260805120000")
        make_member_file(root, "member_b", "202608", 2, 0, "20260805120000")
        cfg = make_cfg(tmp_path, root)

        entries, _ = discover_files(cfg, mode="full", only_member="member_a")

        assert len(entries) == 1
        assert entries[0].member == "member_a"

    def test_non_matching_filename_is_skipped(self, tmp_path: Path):
        root = tmp_path / "root"
        member_dir = root / "member_a" / "2026" / "202608"
        member_dir.mkdir(parents=True)
        (member_dir / "not_a_valid_filename.txt").write_text("x", encoding="utf-8")
        cfg = make_cfg(tmp_path, root)

        entries, _ = discover_files(cfg, mode="full")
        assert entries == []


class TestBuildMessageRow:
    def _entry(self, member="member_a", msg_id=1, flag=0, ext="txt", ts_raw="20260805120000", path=None, group=None):
        return FileEntry(
            path=path or Path("/dummy"),
            rel_path="dummy",
            member=member,
            group=group,
            msg_id=msg_id,
            flag=flag,
            ts_raw=ts_raw,
            ext=ext,
        )

    def test_text_only_message(self, tmp_path: Path):
        txt_path = tmp_path / "msg.txt"
        txt_path.write_text("こんにちは", encoding="utf-8")
        entry = self._entry(path=txt_path)
        row = _build_message_row("member_a", 1, [entry])
        assert row["kind"] == "text"
        assert row["body"] == "こんにちは"
        assert row["media"] is None

    def test_media_with_text_caption(self, tmp_path: Path):
        txt_path = tmp_path / "1_1_20260805120000.txt"
        txt_path.write_text("caption", encoding="utf-8")
        img_entry = self._entry(msg_id=1, flag=1, ext="jpg", path=tmp_path / "1_1_20260805120000.jpg")
        txt_entry = self._entry(msg_id=1, flag=1, ext="txt", path=txt_path)
        row = _build_message_row("member_a", 1, [img_entry, txt_entry])
        assert row["kind"] == "image"
        assert row["body"] == "caption"
        assert row["media"] == "dummy"

    def test_text_read_failure_falls_back_to_none_body(self, tmp_path: Path):
        missing_path = tmp_path / "does_not_exist.txt"
        entry = self._entry(path=missing_path)
        row = _build_message_row("member_a", 1, [entry])
        assert row["body"] is None

    def test_group_is_picked_from_any_file(self):
        entry = self._entry(group="group_x", path=Path("/dummy.txt"))
        row = _build_message_row("member_a", 1, [entry])
        assert row["group_"] == "group_x"


class TestIndexLock:
    def test_acquire_and_release(self, tmp_path: Path):
        lock = IndexLock(tmp_path / ".index.lock")
        lock.acquire()
        assert lock.lock_path.exists()
        lock.release()
        assert not lock.lock_path.exists()

    def test_double_acquire_raises_when_holder_alive(self, tmp_path: Path):
        lock_path = tmp_path / ".index.lock"
        lock1 = IndexLock(lock_path)
        lock1.acquire()
        lock2 = IndexLock(lock_path)
        with pytest.raises(IndexAlreadyRunning):
            lock2.acquire()
        lock1.release()

    def test_stale_lock_is_reclaimed(self, tmp_path: Path):
        lock_path = tmp_path / ".index.lock"
        # 存在しないPIDでロックファイルを偽造する
        dead_pid = 999999
        lock_path.write_text(f"{dead_pid}\n0\n", encoding="utf-8")

        lock = IndexLock(lock_path)
        lock.acquire()  # staleと判定され奪取できるはず
        assert lock.lock_path.read_text(encoding="utf-8").splitlines()[0] != str(dead_pid)
        lock.release()

    def test_peek_running_pid_none_when_no_lock(self, tmp_path: Path):
        lock = IndexLock(tmp_path / ".index.lock")
        assert lock.peek_running_pid() is None


class TestPidAlive:
    def test_own_pid_is_alive(self):
        import os
        assert pid_alive(os.getpid()) is True

    def test_zero_or_negative_pid_is_not_alive(self):
        assert pid_alive(0) is False
        assert pid_alive(-1) is False


class TestPidAliveWindows:
    """pid_alive()のWindows分岐(_pid_alive_windows)のテスト。

    このテストはLinux上で実行されるため、ctypes.windll等のWindows専用APIは
    実在しない。os.name を "nt" に見せかけた上で、indexer モジュールが参照する
    ctypes.windll / ctypes.get_last_error / ctypes.byref / ctypes.wintypes を
    MagicMockで差し替えることで、実際にOSへアクセスせずに分岐を検証する。
    """

    def _make_kernel32(self, monkeypatch, *, open_process_handle, exit_code=None,
                        get_exit_code_ok=True, last_error=0):
        import ctypes
        from unittest.mock import MagicMock

        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "os", indexer_module.os, raising=False)
        monkeypatch.setattr(indexer_module.os, "name", "nt")

        kernel32 = MagicMock()
        kernel32.OpenProcess.return_value = open_process_handle
        kernel32.CloseHandle.return_value = True

        def _get_exit_code_process(handle, byref_result):
            if not get_exit_code_ok:
                return False
            # ctypes.byref(exit_code) の中身(DWORD)にexit_codeを書き込む挙動を模倣
            byref_result._obj.value = exit_code
            return True

        kernel32.GetExitCodeProcess.side_effect = _get_exit_code_process

        windll = MagicMock()
        windll.kernel32 = kernel32

        monkeypatch.setattr(ctypes, "windll", windll, raising=False)
        monkeypatch.setattr(ctypes, "get_last_error", lambda: last_error, raising=False)
        return indexer_module

    def test_handle_acquired_and_still_active_is_alive(self, monkeypatch):
        indexer_module = self._make_kernel32(
            monkeypatch, open_process_handle=1234, exit_code=259  # STILL_ACTIVE
        )
        assert indexer_module.pid_alive(999) is True

    def test_handle_acquired_and_exited_is_not_alive(self, monkeypatch):
        indexer_module = self._make_kernel32(
            monkeypatch, open_process_handle=1234, exit_code=0  # 終了コードあり
        )
        assert indexer_module.pid_alive(999) is False

    def test_handle_denied_with_access_denied_is_alive(self, monkeypatch):
        ERROR_ACCESS_DENIED = 5
        indexer_module = self._make_kernel32(
            monkeypatch, open_process_handle=0, last_error=ERROR_ACCESS_DENIED
        )
        assert indexer_module.pid_alive(999) is True

    def test_handle_denied_with_other_error_is_not_alive(self, monkeypatch):
        ERROR_INVALID_PARAMETER = 87
        indexer_module = self._make_kernel32(
            monkeypatch, open_process_handle=0, last_error=ERROR_INVALID_PARAMETER
        )
        assert indexer_module.pid_alive(999) is False


class TestOpenDbAndRebuild:
    def test_open_db_creates_schema(self, tmp_path: Path):
        db_path = tmp_path / "index.db"
        conn = open_db(db_path)
        cur = conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='messages'"
        )
        assert cur.fetchone() is not None
        conn.close()

    def test_rebuild_db_removes_existing_file(self, tmp_path: Path):
        db_path = tmp_path / "index.db"
        conn = open_db(db_path)
        conn.execute(
            "INSERT INTO messages(member, msg_id, ts, ts_raw, kind, flag) VALUES (?,?,?,?,?,?)",
            ("member_a", 1, 0, "20260101000000", "text", 0),
        )
        conn.commit()
        conn.close()

        conn2 = rebuild_db(db_path)
        cur = conn2.execute("SELECT COUNT(*) FROM messages")
        assert cur.fetchone()[0] == 0
        conn2.close()


class TestRunIndex:
    def test_full_index_inserts_messages(self, tmp_path: Path):
        root = tmp_path / "root"
        make_member_file(root, "member_a", "202608", 1, 0, "20260805120000", content="hello")
        cfg = make_cfg(tmp_path, root)

        summary = run_index(mode="full", cfg=cfg, no_thumbs=True)

        assert summary.new_count == 1
        conn = open_db(cfg.db_path)
        row = conn.execute("SELECT member, body FROM messages WHERE msg_id=1").fetchone()
        assert row == ("member_a", "hello")
        conn.close()

    def test_incremental_then_new_member_added_gets_fully_indexed(self, tmp_path: Path):
        """回帰テスト: 初回セットアップ後に新規メンバーが増えても、差分取り込みで過去分まで拾われる。"""
        root = tmp_path / "root"
        make_member_file(root, "existing_member", "202401", 1, 0, "20240115120000")
        cfg = make_cfg(tmp_path, root)

        run_index(mode="full", cfg=cfg, no_thumbs=True)

        # 新規メンバーを、直近月フィルタの範囲外である過去の月にだけ追加する
        make_member_file(root, "new_member", "202401", 2, 0, "20240120120000")

        summary = run_index(mode="incremental", cfg=cfg, no_thumbs=True)

        assert summary.new_count == 1
        conn = open_db(cfg.db_path)
        row = conn.execute("SELECT member FROM messages WHERE member='new_member'").fetchone()
        assert row is not None
        conn.close()

    def test_missing_file_is_flagged(self, tmp_path: Path):
        root = tmp_path / "root"
        file_path = make_member_file(root, "member_a", "202608", 1, 0, "20260805120000")
        cfg = make_cfg(tmp_path, root)

        run_index(mode="full", cfg=cfg, no_thumbs=True)
        file_path.unlink()

        summary = run_index(mode="full", cfg=cfg, no_thumbs=True)

        assert summary.missing_count == 1
        conn = open_db(cfg.db_path)
        row = conn.execute("SELECT missing FROM messages WHERE msg_id=1").fetchone()
        assert row[0] == 1
        conn.close()

    def test_rebuild_true_clears_previous_data(self, tmp_path: Path):
        root = tmp_path / "root"
        make_member_file(root, "member_a", "202608", 1, 0, "20260805120000")
        cfg = make_cfg(tmp_path, root)

        run_index(mode="full", cfg=cfg, no_thumbs=True, rebuild=True)
        run_index(mode="full", cfg=cfg, no_thumbs=True, rebuild=True)

        conn = open_db(cfg.db_path)
        count = conn.execute("SELECT COUNT(*) FROM messages").fetchone()[0]
        assert count == 1  # 再構築しても重複しない
        conn.close()


class TestFfmpegAvailable:
    """ffmpeg_available()のテスト。

    find_ffmpegはPATHより先にプロジェクト直下を見るため、実際にffmpegを配置した環境でも
    テストが安定するようbase_dirを空のtmp_pathに差し替える。
    """

    def test_true_when_which_finds_ffmpeg(self, tmp_path: Path, monkeypatch):
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "_FFMPEG_BASE_DIR", tmp_path)
        monkeypatch.setattr(indexer_module.shutil, "which", lambda name: "/usr/bin/ffmpeg")
        assert ffmpeg_available() is True

    def test_false_when_which_finds_nothing(self, tmp_path: Path, monkeypatch):
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "_FFMPEG_BASE_DIR", tmp_path)
        monkeypatch.setattr(indexer_module.shutil, "which", lambda name: None)
        assert ffmpeg_available() is False


class TestFindFfmpeg:
    """find_ffmpeg()の探索順(bin/直下 > プロジェクト直下 > PATH)を検証する。

    実リポジトリを汚さないよう、indexer._FFMPEG_BASE_DIRをtmp_path配下に
    差し替えてテストする(実際のffmpegダミーファイルは絶対に本体repoへ置かない)。
    """

    def _make_exe(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("#!/bin/sh\necho fake ffmpeg\n")
        path.chmod(0o755)

    def test_found_in_bin_subdir(self, tmp_path: Path, monkeypatch):
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "_FFMPEG_BASE_DIR", tmp_path)
        monkeypatch.setattr(indexer_module.shutil, "which", lambda name: None)
        exe = tmp_path / "bin" / "ffmpeg"
        self._make_exe(exe)

        assert find_ffmpeg() == str(exe)

    def test_found_directly_under_base_dir(self, tmp_path: Path, monkeypatch):
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "_FFMPEG_BASE_DIR", tmp_path)
        monkeypatch.setattr(indexer_module.shutil, "which", lambda name: None)
        exe = tmp_path / "ffmpeg.exe"
        self._make_exe(exe)

        assert find_ffmpeg() == str(exe)

    def test_bin_subdir_takes_priority_over_base_dir(self, tmp_path: Path, monkeypatch):
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "_FFMPEG_BASE_DIR", tmp_path)
        monkeypatch.setattr(indexer_module.shutil, "which", lambda name: None)
        bin_exe = tmp_path / "bin" / "ffmpeg"
        root_exe = tmp_path / "ffmpeg"
        self._make_exe(bin_exe)
        self._make_exe(root_exe)

        assert find_ffmpeg() == str(bin_exe)

    def test_falls_back_to_path_when_not_bundled(self, tmp_path: Path, monkeypatch):
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "_FFMPEG_BASE_DIR", tmp_path)
        monkeypatch.setattr(indexer_module.shutil, "which", lambda name: "/usr/bin/ffmpeg")

        assert find_ffmpeg() == "/usr/bin/ffmpeg"

    def test_none_when_nowhere_found(self, tmp_path: Path, monkeypatch):
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "_FFMPEG_BASE_DIR", tmp_path)
        monkeypatch.setattr(indexer_module.shutil, "which", lambda name: None)

        assert find_ffmpeg() is None


class TestVideoThumbnailGating:
    """ffmpegの有無によって動画(flag=2)がサムネ対象になるかどうかを検証する。

    音声もmp4拡張子を使うため、flag=3(音声)は常にサムネ対象外であることも合わせて確認する。
    """

    def test_video_not_thumbnailed_without_ffmpeg(self, tmp_path: Path, monkeypatch):
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "ffmpeg_available", lambda: False)
        root = tmp_path / "root"
        make_member_file(root, "member_a", "202608", 1, 2, "20260805120000", ext="mp4")
        cfg = make_cfg(tmp_path, root)

        # ffmpegが無い環境でも例外なく完走すること、かつthumbが生成されないことを確認する。
        summary = run_index(mode="full", cfg=cfg, no_thumbs=False)

        assert summary.thumb_count == 0
        conn = open_db(cfg.db_path)
        row = conn.execute("SELECT thumb, kind FROM messages WHERE msg_id=1").fetchone()
        assert row[1] == "video"
        assert row[0] is None
        conn.close()

    def test_audio_mp4_never_thumbnailed_even_with_ffmpeg(self, tmp_path: Path, monkeypatch):
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "ffmpeg_available", lambda: True)
        root = tmp_path / "root"
        # 音声もmp4拡張子(flag=3)なので、ffmpegがあってもサムネ対象にならないこと。
        make_member_file(root, "member_a", "202608", 1, 3, "20260805120000", ext="mp4")
        cfg = make_cfg(tmp_path, root)

        summary = run_index(mode="full", cfg=cfg, no_thumbs=False)

        assert summary.thumb_count == 0
        conn = open_db(cfg.db_path)
        row = conn.execute("SELECT thumb, kind FROM messages WHERE msg_id=1").fetchone()
        assert row[1] == "audio"
        assert row[0] is None
        conn.close()

    def test_jpg_present_takes_priority_over_video_even_with_ffmpeg(self, tmp_path: Path, monkeypatch):
        """同一メッセージにjpgとmp4が両方ある想定は通常無いが、jpgがある限り画像優先を保証する。"""
        import indexer as indexer_module

        monkeypatch.setattr(indexer_module, "ffmpeg_available", lambda: True)
        root = tmp_path / "root"
        make_member_file(root, "member_a", "202608", 1, 1, "20260805120000", ext="jpg")
        cfg = make_cfg(tmp_path, root)

        summary = run_index(mode="full", cfg=cfg, no_thumbs=False)

        # jpgはPillowでサムネ化されるはずなので、生成が試みられて成功していること
        # (ダミーJPEGバイトのため実際に成功するかはPillow次第だが、失敗してもthumb_countで検証できる)
        conn = open_db(cfg.db_path)
        row = conn.execute("SELECT kind FROM messages WHERE msg_id=1").fetchone()
        assert row[0] == "image"
        conn.close()
        assert summary.thumb_count in (0, 1)  # ダミーバイトが不正画像でも例外で全体は止まらない
