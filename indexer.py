"""メッセージ保存フォルダを走査し SQLite インデックスを構築するモジュール。

階層は2パターン:
  パターンA: {メンバー}/{YYYY}/{YYYYMM}/{file}
  パターンB: {グループ}/{メンバー}/{YYYY}/{YYYYMM}/{file}
メンバー名は「YYYYフォルダの1つ上」として抽出する（階層数に依存しない）。

ファイル名: {msg_id}_{flag}_{YYYYMMDDHHMMSS}.{ext}
flag: 0=text 1=image 2=video 3=audio (拡張子でなく flag を信頼する)

主キー: (member, msg_id) の複合キー。
"""
from __future__ import annotations

import calendar
import contextlib
import ctypes
import errno
import hashlib
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import time
from concurrent.futures import ProcessPoolExecutor, as_completed
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable, Optional

from config import Config, get_config

FILENAME_RE = re.compile(r"^(\d+)_(\d)_(\d{14})\.(\w+)$")
YEAR_DIR_RE = re.compile(r"^\d{4}$")
YEARMONTH_DIR_RE = re.compile(r"^\d{6}$")

FLAG_TO_KIND = {
    0: "text",
    1: "image",
    2: "video",
    3: "audio",
}

SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS messages (
  member   TEXT NOT NULL,
  group_   TEXT,
  msg_id   INTEGER NOT NULL,
  ts       INTEGER NOT NULL,
  ts_raw   TEXT NOT NULL,
  body     TEXT,
  media    TEXT,
  kind     TEXT NOT NULL,
  flag     INTEGER NOT NULL,
  thumb    TEXT,
  missing  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (member, msg_id)
);
CREATE INDEX IF NOT EXISTS idx_member_ts ON messages(member, ts);
CREATE INDEX IF NOT EXISTS idx_ts ON messages(ts);
"""

FTS_SQL = """
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  body, member UNINDEXED, msg_id UNINDEXED, tokenize='trigram'
);
"""


def open_db(db_path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(str(db_path), check_same_thread=False)
    conn.execute("PRAGMA journal_mode=WAL;")
    conn.execute("PRAGMA synchronous=NORMAL;")
    # 書き込み側・読み取り側どちらの接続でも、ロック競合時にすぐ例外化せず
    # 一定時間はリトライ待ちさせる（database is locked 対策）。
    conn.execute("PRAGMA busy_timeout=30000;")
    conn.executescript(SCHEMA_SQL)
    conn.executescript(FTS_SQL)
    conn.commit()
    return conn


def _pid_alive_windows(pid: int) -> bool:
    """Windows向けのPID生存確認(ctypes経由でWin32 APIを呼ぶ)。

    os.kill(pid, 0) はWindowsでは「生存確認だけしたい」用途に対応しておらず、
    実行中のプロセスに対してもOSError(EINVAL, WinError 87)を送出することがある。
    そのため OpenProcess + GetExitCodeProcess で確実に判定する。
    PROCESS_QUERY_LIMITED_INFORMATION(0x1000) はVista以降で、対象プロセスの
    所有者が別ユーザでも生存確認程度の権限があれば取得できる。
    """
    import ctypes.wintypes  # Windows専用モジュールのためここでインポートする

    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    STILL_ACTIVE = 259
    ERROR_ACCESS_DENIED = 5

    kernel32 = ctypes.windll.kernel32
    handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        err = ctypes.get_last_error()
        if err == ERROR_ACCESS_DENIED:
            # 別ユーザ所有などで問い合わせできないだけ = 生存しているとみなす
            # (Unix版のEPERM扱いと同じ哲学)
            return True
        return False

    try:
        exit_code = ctypes.wintypes.DWORD()
        if not kernel32.GetExitCodeProcess(handle, ctypes.byref(exit_code)):
            # 終了コードが取得できない場合は安全側(死んでいる)に倒す
            return False
        # STILL_ACTIVE以外はPIDの再利用も含めて終了済みとみなす
        return exit_code.value == STILL_ACTIVE
    finally:
        kernel32.CloseHandle(handle)


def pid_alive(pid: int) -> bool:
    """指定PIDのプロセスが生存しているか確認する。

    Unix系ではos.kill(pid, 0)による生存確認、Windowsでは os.kill が
    signal=0 の生存確認用途に対応していないため ctypes 経由でWin32 API
    (OpenProcess/GetExitCodeProcess) を使う。
    IndexLock と app.py の status API (stale判定) の両方から使う共通実装。
    """
    if pid <= 0:
        return False
    if os.name == "nt":
        return _pid_alive_windows(pid)
    try:
        os.kill(pid, 0)
    except OSError as ex:
        if ex.errno == errno.ESRCH:
            return False
        if ex.errno == errno.EPERM:
            # 別ユーザ所有などで signal を送れないだけ = 生存しているとみなす
            return True
        return False
    return True


class IndexAlreadyRunning(RuntimeError):
    """既に他のインデックス処理が実行中であることを示す例外。"""


class IndexLock:
    """インデックス処理の多重実行を防止するPIDロックファイル。

    - 取得できなければ即座に IndexAlreadyRunning を送出する（ブロックしない）。
    - ロックファイルには保持者のPIDを記録する。取得を試みる際、既存ロックの
      PIDが生きていなければ stale とみなして奪取する（os.kill(pid, 0)で生存確認）。
    - os.O_CREAT | os.O_EXCL による原子的なファイル作成でTOCTOUを避ける。
      stale奪取時は unlink 後に再度 O_EXCL 作成を試み、他プロセスとの競合が
      あれば素直に諦める（そちらが勝った、ということなので）。
    """

    def __init__(self, lock_path: Path):
        self.lock_path = lock_path
        self._acquired = False

    def _pid_alive(self, pid: int) -> bool:
        return pid_alive(pid)

    def _read_lock_pid(self) -> Optional[int]:
        try:
            content = self.lock_path.read_text(encoding="utf-8").strip()
            return int(content.splitlines()[0])
        except Exception:
            return None

    def _try_create(self) -> bool:
        try:
            fd = os.open(str(self.lock_path), os.O_CREAT | os.O_EXCL | os.O_WRONLY)
        except FileExistsError:
            return False
        try:
            os.write(fd, f"{os.getpid()}\n{time.time()}\n".encode("utf-8"))
        finally:
            os.close(fd)
        return True

    def acquire(self):
        self.lock_path.parent.mkdir(parents=True, exist_ok=True)
        if self._try_create():
            self._acquired = True
            return
        # 既存ロックがある。staleかどうか判定する。
        existing_pid = self._read_lock_pid()
        if existing_pid is not None and self._pid_alive(existing_pid):
            raise IndexAlreadyRunning(
                f"index already running (pid={existing_pid}, lock={self.lock_path})"
            )
        # staleなロック: 奪取を試みる。奪取競合時は負けを認める。
        try:
            self.lock_path.unlink()
        except FileNotFoundError:
            pass
        if self._try_create():
            self._acquired = True
            return
        # unlink後に別プロセスが先にlockを取った
        winner_pid = self._read_lock_pid()
        raise IndexAlreadyRunning(
            f"index already running (pid={winner_pid}, lock={self.lock_path})"
        )

    def release(self):
        if self._acquired:
            with contextlib.suppress(FileNotFoundError):
                self.lock_path.unlink()
            self._acquired = False

    def peek_running_pid(self) -> Optional[int]:
        """ロックを取得せずに、現在保持者が生存しているか覗き見る（UI早期応答用）。

        あくまで参考情報。実際の排他判定は acquire() 時に行われる
        （TOCTOUの余地があるため、ここでの結果を最終判断に使わない）。
        """
        pid = self._read_lock_pid()
        if pid is not None and self._pid_alive(pid):
            return pid
        return None

    def __enter__(self):
        self.acquire()
        return self

    def __exit__(self, exc_type, exc, tb):
        self.release()
        return False


class ProgressFileWriter:
    """進捗を {data_dir}/progress.json にアトミックに永続化する。

    CLIで直接起動された indexer.py の進捗も、app.py 側の /api/reindex/status が
    このファイルを読むことでUIから見えるようにするためのもの。
    書き込みは一時ファイル + os.replace によるアトミック置換で行い、
    UIが読みかけの壊れたJSONを掴まないようにする。

    ロックを取得できなかった（他プロセスが実行中）場合はインスタンス化・書き込み
    どちらも行わないこと（呼び出し側で制御する）。
    """

    def __init__(self, path: Path, mode: str):
        self.path = path
        self.mode = mode
        self.started_at = time.time()
        self.pid = os.getpid()

    def _write(self, data: dict):
        payload = {
            "running": True,
            "phase": None,
            "done": None,
            "total": None,
            "percent": None,
            "elapsed_sec": round(time.time() - self.started_at, 1),
            "eta_sec": None,
            "started_at": self.started_at,
            "updated_at": time.time(),
            "pid": self.pid,
            "mode": self.mode,
            "message": None,
        }
        payload.update(data)
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            tmp_path = self.path.with_suffix(self.path.suffix + f".tmp{self.pid}")
            with open(tmp_path, "w", encoding="utf-8") as f:
                json.dump(payload, f, ensure_ascii=False)
            os.replace(str(tmp_path), str(self.path))
        except Exception as ex:
            # 進捗永続化の失敗はインデックス処理本体を止める理由にはしない。
            print(f"[warn] failed to write progress file: {ex}", file=sys.stderr)

    @staticmethod
    def _eta(done: Optional[int], total: Optional[int], elapsed: float) -> Optional[float]:
        if not done or not total or done <= 0 or total <= 0 or done > total:
            return None
        rate = done / elapsed if elapsed > 0 else 0
        if rate <= 0:
            return None
        remaining = total - done
        return round(remaining / rate, 1)

    def update(
        self,
        phase: str,
        done: Optional[int] = None,
        total: Optional[int] = None,
        message: Optional[str] = None,
    ):
        elapsed = time.time() - self.started_at
        percent = None
        if done is not None and total:
            percent = round(done / total * 100, 1)
        self._write(
            {
                "running": True,
                "phase": phase,
                "done": done,
                "total": total,
                "percent": percent,
                "elapsed_sec": round(elapsed, 1),
                "eta_sec": self._eta(done, total, elapsed),
                "message": message,
            }
        )

    def finish_done(self, summary: "IndexSummary"):
        self._write(
            {
                "running": False,
                "phase": "done",
                "done": None,
                "total": None,
                "percent": 100.0,
                "eta_sec": 0.0,
                "message": None,
                "summary": summary.__dict__,
            }
        )

    def finish_error(self, message: str):
        self._write(
            {
                "running": False,
                "phase": "error",
                "eta_sec": None,
                "message": message,
            }
        )


def rebuild_db(db_path: Path) -> sqlite3.Connection:
    """既存DBファイルを削除して作り直す。"""
    if db_path.exists():
        db_path.unlink()
    for suffix in ("-wal", "-shm"):
        p = Path(str(db_path) + suffix)
        if p.exists():
            p.unlink()
    return open_db(db_path)


@dataclass
class FileEntry:
    path: Path              # 実ファイルの絶対パス
    rel_path: str           # root相対パス (posix形式)
    member: str
    group: Optional[str]
    msg_id: int
    flag: int
    ts_raw: str
    ext: str


@dataclass
class ScanScope:
    """今回の走査で対象とした (member, yyyymm) の集合。missing判定に使う。"""
    member_yearmonths: dict = field(default_factory=dict)  # member -> set(yyyymm) or None(全部)

    def add(self, member: str, yyyymm: Optional[str]):
        if member not in self.member_yearmonths:
            self.member_yearmonths[member] = set()
        cur = self.member_yearmonths[member]
        if cur is None:
            return  # 既に全体スコープ
        if yyyymm is None:
            self.member_yearmonths[member] = None  # 全体スコープに昇格
        else:
            cur.add(yyyymm)


def _iter_member_dirs(root: Path) -> Iterable[tuple[Path, str, Optional[str]]]:
    """root配下を走査し、(member_dir, member_name, group_name) を列挙する。

    パターンA: root/{member}/{YYYY}/...
    パターンB: root/{group}/{member}/{YYYY}/...
    YYYY形式のディレクトリを含むかどうかで判定する。
    """
    for top in sorted(p for p in root.iterdir() if p.is_dir()):
        # topの直下にYYYYディレクトリがあればパターンA (topがmember)
        has_year_direct = any(
            d.is_dir() and YEAR_DIR_RE.match(d.name) for d in top.iterdir()
        )
        if has_year_direct:
            yield top, top.name, None
            continue
        # そうでなければ、topはグループ。その下のディレクトリを member として調べる
        for sub in sorted(p for p in top.iterdir() if p.is_dir()):
            has_year = any(
                d.is_dir() and YEAR_DIR_RE.match(d.name) for d in sub.iterdir()
            )
            if has_year:
                yield sub, sub.name, top.name
            # YYYYも持たないサブディレクトリは仕様上存在しない想定（無視）


def _recent_yearmonths(n_months: int, now: Optional[datetime] = None) -> set[str]:
    now = now or datetime.now()
    result = set()
    y, m = now.year, now.month
    for _ in range(n_months):
        result.add(f"{y:04d}{m:02d}")
        m -= 1
        if m == 0:
            m = 12
            y -= 1
    return result


def _member_dir_for(root: Path, member: str, only_member: Optional[str]) -> bool:
    if only_member is None:
        return True
    return member == only_member


def discover_files(
    cfg: Config,
    mode: str,
    only_member: Optional[str] = None,
    known_members: Optional[set[str]] = None,
) -> tuple[list[FileEntry], ScanScope]:
    """走査対象ファイルを列挙する。mode: 'full' | 'incremental'.

    known_members: incrementalモード時、DBに既に取り込み済みのメンバー名の集合。
    ここに含まれない(＝まだ一度も取り込んだことがない)メンバーは、新規追加とみなし
    直近Nヶ月の範囲に関係なく全期間を走査する。
    """
    root = cfg.root
    scope = ScanScope()
    entries: list[FileEntry] = []

    recent = _recent_yearmonths(cfg.incremental_months) if mode == "incremental" else None

    for member_dir, member, group in _iter_member_dirs(root):
        if not _member_dir_for(root, member, only_member):
            continue

        is_new_member = mode == "incremental" and known_members is not None and member not in known_members
        member_recent = None if is_new_member else recent

        year_dirs = [d for d in member_dir.iterdir() if d.is_dir() and YEAR_DIR_RE.match(d.name)]
        for year_dir in sorted(year_dirs):
            month_dirs = [
                d for d in year_dir.iterdir() if d.is_dir() and YEARMONTH_DIR_RE.match(d.name)
            ]
            for month_dir in sorted(month_dirs):
                yyyymm = month_dir.name
                if member_recent is not None and yyyymm not in member_recent:
                    continue
                scope.add(member, yyyymm)
                for f in month_dir.iterdir():
                    if not f.is_file():
                        continue
                    m = FILENAME_RE.match(f.name)
                    if not m:
                        continue
                    msg_id_s, flag_s, ts_raw, ext = m.groups()
                    rel_path = f.relative_to(root).as_posix()
                    entries.append(
                        FileEntry(
                            path=f,
                            rel_path=rel_path,
                            member=member,
                            group=group,
                            msg_id=int(msg_id_s),
                            flag=int(flag_s),
                            ts_raw=ts_raw,
                            ext=ext.lower(),
                        )
                    )

        if member_recent is None:
            scope.add(member, None)  # full scan (新規メンバー含む) -> 全体スコープ

    return entries, scope


def parse_ts_jst(ts_raw: str) -> int:
    """YYYYMMDDHHMMSS 文字列(JSTとして解釈)を unix epoch秒に変換する。"""
    dt = datetime.strptime(ts_raw, "%Y%m%d%H%M%S")
    # dtをJSTのnaive時刻とみなし、UTC epochに変換する: epoch = calendar.timegm(dt) - 9時間
    utc_epoch = calendar.timegm(dt.timetuple())
    return utc_epoch - 9 * 3600


def thumb_rel_path(member: str, msg_id: int) -> str:
    """サムネの thumbs_dir 相対パス。member名はハッシュ化してディレクトリ分割する。

    形式: {member_hash(2桁)}/{member_hash}/{msg_id}.jpg
    1ディレクトリあたりのファイル数を抑えるため、ハッシュ先頭2文字でサブディレクトリを切る。
    """
    h = hashlib.sha1(member.encode("utf-8")).hexdigest()
    return f"{h[:2]}/{h}/{msg_id}.jpg"


@dataclass
class IndexSummary:
    new_count: int = 0
    updated_count: int = 0
    skipped_count: int = 0
    missing_count: int = 0
    thumb_count: int = 0
    elapsed_sec: float = 0.0
    total_files_scanned: int = 0
    total_messages_scanned: int = 0


class ProgressReporter:
    def __init__(self, label: str = "index", interval_sec: float = 2.0):
        self.label = label
        self.interval = interval_sec
        self.last_report = 0.0
        self.start = time.time()

    def maybe_report(self, done: int, total: int, extra: str = ""):
        now = time.time()
        if now - self.last_report >= self.interval or done == total:
            elapsed = now - self.start
            pct = (done / total * 100) if total else 100
            print(
                f"[{self.label}] {done}/{total} ({pct:.1f}%) elapsed={elapsed:.1f}s {extra}",
                file=sys.stderr,
                flush=True,
            )
            self.last_report = now


def _msg_id_str_from_rel_path(rel_path: str) -> str:
    """rel_pathのファイル名部分からmsg_idの元表記(ゼロ埋め等を保持した文字列)を取り出す。"""
    m = FILENAME_RE.match(Path(rel_path).name)
    return m.group(1) if m else ""


def _group_entries_by_message(entries: list[FileEntry]) -> dict[tuple[str, int], list[FileEntry]]:
    grouped: dict[tuple[str, int], list[FileEntry]] = {}
    for e in entries:
        key = (e.member, e.msg_id)
        grouped.setdefault(key, []).append(e)

    # msg_idはint()でパースするため、ゼロ埋めの有無("007"と"7"等)が異なる
    # ファイル名同士は同じキーに衝突しうる。無警告での上書きに気づけるよう検知する。
    for key, files in grouped.items():
        member, msg_id = key
        id_strs = {_msg_id_str_from_rel_path(f.rel_path) for f in files}
        if len(id_strs) > 1:
            print(
                f"[warn] msg_id representation collision for (member={member}, msg_id={msg_id}): "
                f"filenames={[Path(f.rel_path).name for f in files]}",
                file=sys.stderr,
            )
    return grouped


def _build_message_row(member: str, msg_id: int, files: list[FileEntry]) -> dict:
    """同一(member,msg_id)のファイル群から1メッセージ行を構築する。"""
    txt_file = next((f for f in files if f.ext == "txt"), None)
    media_files = [f for f in files if f.ext != "txt"]
    media_file = media_files[0] if media_files else None
    if len(media_files) > 1:
        print(
            f"[warn] multiple media files for (member={member}, msg_id={msg_id}): "
            f"{[f.rel_path for f in media_files]} -> using {media_file.rel_path}",
            file=sys.stderr,
        )

    # flag/kind の決定: メディアファイルがあればそのflag、なければtxtのflag(=0のはず)
    primary = media_file or txt_file
    flag = primary.flag
    kind = FLAG_TO_KIND.get(flag, "text")

    group = next((f.group for f in files if f.group), None)
    ts_raw = primary.ts_raw
    ts = parse_ts_jst(ts_raw)

    body = None
    if txt_file is not None:
        try:
            body = txt_file.path.read_text(encoding="utf-8")
        except Exception as ex:
            print(f"[warn] failed to read {txt_file.path}: {ex}", file=sys.stderr)
            body = None

    media_rel = media_file.rel_path if media_file else None

    return {
        "member": member,
        "group_": group,
        "msg_id": msg_id,
        "ts": ts,
        "ts_raw": ts_raw,
        "body": body,
        "media": media_rel,
        "kind": kind,
        "flag": flag,
        "files": files,
    }


# find_ffmpeg()はProcessPoolExecutorのワーカープロセスからも呼ばれるため、
# config.Configには依存せず、config.py の _BASE_DIR と同じ考え方でモジュール自身の
# 場所を基準にする(cfgオブジェクトを別プロセスへ渡す必要をなくすため)。
_FFMPEG_BASE_DIR = Path(__file__).resolve().parent


def find_ffmpeg() -> Optional[str]:
    """ffmpeg実行ファイルの絶対パスを探して返す(無ければNone)。

    「プロジェクトフォルダに置くだけで使える」ようにするため、PATHだけでなく
    同梱バイナリの置き場所(bin/直下・ルート直下)も探す。Windows(ffmpeg.exe)と
    WSL/Linux(ffmpeg)の両方の名前をどちらの環境でも試すことで、OS判定を
    誤っても取りこぼさないようにする。
    探索順: <base_dir>/bin/ffmpeg(.exe) → <base_dir>/ffmpeg(.exe) → PATH。
    """
    names = ("ffmpeg.exe", "ffmpeg")
    candidate_dirs = (_FFMPEG_BASE_DIR / "bin", _FFMPEG_BASE_DIR)
    for d in candidate_dirs:
        for name in names:
            p = d / name
            # Windowsではos.access(X_OK)が常にTrueになりがちで実行可否の判定に
            # ならないため、存在チェックを主とし、実行可能ならなお良しとする程度に扱う。
            if p.is_file():
                return str(p)
    which = shutil.which("ffmpeg")
    if which:
        return which
    return None


def ffmpeg_available() -> bool:
    """ffmpegが利用可能か判定する。

    ffmpegは必須ではなく任意の追加機能(動画サムネイル生成)のためだけに使う。
    後からffmpegを導入(PATHまたはプロジェクトフォルダへの同梱)して再取り込みすれば、
    この判定がTrueになった時点で(既にjpgサムネがある動画は対象外のまま)
    未生成の動画サムネだけが作られる。
    """
    return find_ffmpeg() is not None


def _gen_image_thumb(src: Path, dst: Path, thumb_size: int, thumb_quality: int) -> None:
    from PIL import Image

    with Image.open(src) as im:
        im = im.convert("RGB")
        im.thumbnail((thumb_size, thumb_size))
        im.save(dst, "JPEG", quality=thumb_quality)


def _gen_video_thumb(src: Path, dst: Path, thumb_size: int, thumb_quality: int, ffmpeg_path: Optional[str]) -> None:
    """ffmpegで動画の1フレームを抜き出してJPEGサムネを作る。

    1秒未満の短い動画では -ss 1 がフレームを拾えず失敗することがあるため、
    その場合は -ss 0 (先頭フレーム)で再試行する。
    ffmpeg_pathは呼び出し元(ワーカープロセス)で解決済みの絶対パス。ここでは
    「見つからなかった」場合の扱いだけ行い、探索ロジック自体は持たない
    (探索はfind_ffmpeg()に一本化し、ここでは重複させない)。
    """
    if not ffmpeg_path:
        raise RuntimeError("ffmpeg not found (PATH にも bin/ にも見つかりません)")

    def _extract(ss: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [
                ffmpeg_path, "-y", "-ss", ss, "-i", str(src),
                "-frames:v", "1", "-vf", f"scale={thumb_size}:-1",
                "-q:v", "3", str(dst),
            ],
            capture_output=True,
            timeout=30,
        )

    proc = _extract("1")
    if proc.returncode != 0 or not dst.exists():
        proc = _extract("0")
    if proc.returncode != 0 or not dst.exists():
        stderr = proc.stderr.decode("utf-8", errors="replace") if proc.stderr else ""
        raise RuntimeError(f"ffmpeg failed (rc={proc.returncode}): {stderr[-500:]}")
    _ = thumb_quality  # ffmpeg側は-q:vで指定済み。Pillow版と引数形状を揃えるためだけに受け取る。


def _gen_thumb_worker(args):
    """ProcessPoolExecutor用ワーカー: 1件のサムネ(画像 or 動画)を生成する。

    args: (src_path_str, dst_path_str, thumb_size, thumb_quality, kind, ffmpeg_path)
    kind: "image" (Pillow) または "video" (ffmpeg)。
    ffmpeg_path: メインプロセス側でfind_ffmpeg()により解決済みの絶対パス(無ければNone)。
    ワーカーは別プロセスなのでmain側のPATHやプロジェクト配置を都度探すより、
    解決済みパスをpicklableな文字列として渡す方が安全・高速。
    失敗しても例外を外へ投げず、既存の(src, False, err)失敗パスに乗せて
    全体の取り込み処理を止めないようにする。
    """
    src_path_str, dst_path_str, thumb_size, thumb_quality, kind, ffmpeg_path = args
    try:
        src = Path(src_path_str)
        dst = Path(dst_path_str)
        dst.parent.mkdir(parents=True, exist_ok=True)
        if kind == "video":
            _gen_video_thumb(src, dst, thumb_size, thumb_quality, ffmpeg_path)
        else:
            _gen_image_thumb(src, dst, thumb_size, thumb_quality)
        return (src_path_str, True, None)
    except Exception as ex:
        return (src_path_str, False, str(ex))


def run_index(
    mode: str = "incremental",
    cfg: Optional[Config] = None,
    rebuild: bool = False,
    only_member: Optional[str] = None,
    no_thumbs: bool = False,
    progress_cb=None,
) -> IndexSummary:
    """インデックス取り込みを実行する。app.py からも呼べる再利用可能な関数。

    mode: 'full' | 'incremental'
    progress_cb: Optional[Callable[[dict], None]] 進捗コールバック(APIステータス用)
    """
    cfg = cfg or get_config()
    start_time = time.time()
    summary = IndexSummary()

    # stage(app.py向けの旧来のprogress_cbペイロード)を progress.json 用の
    # phase(scanning/upsert/thumbs/done/error) にマッピングする。
    _STAGE_TO_PHASE = {
        "scanning": "scanning",
        "scanned": "scanning",
        "upserting": "upsert",
        "missing_check": "upsert",
        "thumbnails": "thumbs",
        "thumbnailing": "thumbs",
    }

    def report(stage: str, **kw):
        if progress_cb:
            payload = {"stage": stage}
            payload.update(kw)
            progress_cb(payload)
        if progress_writer is not None and stage != "done":
            phase = _STAGE_TO_PHASE.get(stage, stage)
            done = kw.get("done")
            total = kw.get("total")
            message = None
            if stage == "scanned":
                message = f"{kw.get('files', 0)}件のファイル / {kw.get('messages', 0)}件のメッセージを検出"
            elif stage == "missing_check":
                message = "欠損チェック中..."
            elif stage == "thumbnails":
                total = kw.get("total")
            progress_writer.update(phase, done=done, total=total, message=message)

    # ロック取得前に progress.json を書いてしまうと、既に他プロセスが実行中の
    # ケース(IndexAlreadyRunning)で相手の進捗ファイルを上書きしてしまう恐れがある。
    # そのため progress_writer はロック取得成功後にのみ生成する。
    progress_writer: Optional[ProgressFileWriter] = None

    lock = IndexLock(cfg.index_lock_path)
    lock.acquire()  # 多重実行時は IndexAlreadyRunning を送出してここで終了する
    progress_writer = ProgressFileWriter(cfg.progress_json_path, mode=mode)
    try:
        _run_index_locked(
            mode=mode,
            cfg=cfg,
            rebuild=rebuild,
            only_member=only_member,
            no_thumbs=no_thumbs,
            report=report,
            summary=summary,
        )
    except Exception as ex:
        progress_writer.finish_error(str(ex))
        raise
    finally:
        lock.release()

    summary.elapsed_sec = time.time() - start_time
    report("done", summary=summary.__dict__)
    progress_writer.finish_done(summary)
    return summary


def _run_index_locked(
    *,
    mode: str,
    cfg: Config,
    rebuild: bool,
    only_member: Optional[str],
    no_thumbs: bool,
    report,
    summary: "IndexSummary",
) -> None:
    """ロック取得後の実処理本体。run_index からのみ呼ばれる。"""
    # ffmpegは必須ではない任意機能。あれば動画サムネも生成し、無ければ従来通り
    # jpg(画像)のみサムネ化する。ユーザーが後からffmpegを導入したことに気づけるよう
    # 有無(見つかった場合はどこのffmpegを使っているか)を必ず1行ログに出す。
    # has_ffmpegの判定はffmpeg_available()(テストでモックしやすいよう分離)を使い、
    # 実際にワーカーへ渡すパスはfind_ffmpeg()で別途解決する。
    has_ffmpeg = ffmpeg_available()
    ffmpeg_path = find_ffmpeg() if has_ffmpeg else None
    print(
        f"[index] ffmpeg found at {ffmpeg_path}: video thumbnails enabled" if has_ffmpeg
        else "[index] ffmpeg not found: video thumbnails skipped",
        file=sys.stderr,
    )

    if rebuild:
        conn = rebuild_db(cfg.db_path)
    else:
        conn = open_db(cfg.db_path)

    try:
        report("scanning")
        print(f"[index] scanning root={cfg.root} mode={mode} only_member={only_member}", file=sys.stderr)
        known_members = {row[0] for row in conn.execute("SELECT DISTINCT member FROM messages")}
        entries, scope = discover_files(
            cfg,
            mode="full" if rebuild else mode,
            only_member=only_member,
            known_members=known_members,
        )
        summary.total_files_scanned = len(entries)
        print(f"[index] discovered {len(entries)} files", file=sys.stderr)

        grouped = _group_entries_by_message(entries)
        summary.total_messages_scanned = len(grouped)
        report("scanned", files=len(entries), messages=len(grouped))

        cur = conn.cursor()

        # 既存行をロード（このスコープに関係するmemberのみで十分だが、シンプルに全件ロード）
        existing = {}
        cur.execute("SELECT member, msg_id, body, media, kind, flag, thumb, missing, ts_raw FROM messages")
        for row in cur.fetchall():
            existing[(row[0], row[1])] = row

        seen_keys: set[tuple[str, int]] = set()
        thumb_jobs = []  # (src_path_str, dst_path_str, member, msg_id, thumb_rel, kind)

        reporter = ProgressReporter(label="upsert")
        done = 0
        total = len(grouped)

        for (member, msg_id), files in grouped.items():
            done += 1
            seen_keys.add((member, msg_id))
            row = _build_message_row(member, msg_id, files)
            key = (member, msg_id)
            prev = existing.get(key)

            thumb_rel = None
            if prev is not None:
                thumb_rel = prev[6]  # thumb column

            need_thumb = False
            jpg_file = next((f for f in files if f.ext == "jpg"), None)
            # jpgサムネ(画像メッセージ)が無いときだけ、ffmpegがあれば動画(flag=2)のmp4を対象にする。
            # 音声(flag=3)もmp4拡張子を使うため、flagでvideoのみに絞る点に注意。
            video_file = None
            if jpg_file is None and has_ffmpeg:
                video_file = next((f for f in files if f.ext == "mp4" and f.flag == 2), None)
            thumb_src_file = jpg_file or video_file
            thumb_kind = "image" if jpg_file is not None else "video"

            if thumb_src_file is not None and not no_thumbs:
                thumb_rel_candidate = thumb_rel_path(member, msg_id)
                thumb_abs = cfg.thumbs_dir / thumb_rel_candidate
                if not thumb_abs.exists():
                    # 既にサムネがあるものは再生成しない差分方式のため、後からffmpegを
                    # 導入して再取り込みしても、未生成の動画サムネだけが積まれる。
                    need_thumb = True
                    thumb_jobs.append(
                        (str(thumb_src_file.path), str(thumb_abs), member, msg_id, thumb_rel_candidate, thumb_kind)
                    )
                thumb_rel = thumb_rel_candidate

            if prev is None:
                # 通常は INSERT だが、(member, msg_id) がスナップショット取得後に
                # 他所から挿入されていた場合に備えて ON CONFLICT DO UPDATE で
                # 冪等化する（多重実行防止ロックがあっても、同一プロセス内の
                # 再走査や将来の防御崩れに対する保険として残す）。
                # thumbは新規時、既存のthumb値を壊さないよう COALESCE で保護する。
                cur.execute(
                    """INSERT INTO messages
                       (member, group_, msg_id, ts, ts_raw, body, media, kind, flag, thumb, missing)
                       VALUES (?,?,?,?,?,?,?,?,?,?,0)
                       ON CONFLICT(member, msg_id) DO UPDATE SET
                         group_=excluded.group_,
                         ts=excluded.ts,
                         ts_raw=excluded.ts_raw,
                         body=excluded.body,
                         media=excluded.media,
                         kind=excluded.kind,
                         flag=excluded.flag,
                         thumb=COALESCE(excluded.thumb, messages.thumb),
                         missing=0""",
                    (
                        row["member"], row["group_"], row["msg_id"], row["ts"], row["ts_raw"],
                        row["body"], row["media"], row["kind"], row["flag"], thumb_rel,
                    ),
                )
                # FTS整合: conflict/insertいずれでも既存FTS行を消してから入れ直す
                # （挿入かconflict-updateかをPython側から判別できないため一律この手順にする）。
                cur.execute("SELECT rowid FROM messages_fts WHERE member=? AND msg_id=?", (member, msg_id))
                fr = cur.fetchone()
                if fr:
                    cur.execute("DELETE FROM messages_fts WHERE rowid=?", (fr[0],))
                if row["body"]:
                    cur.execute(
                        "INSERT INTO messages_fts(rowid, body, member, msg_id) VALUES ((SELECT rowid FROM messages WHERE member=? AND msg_id=?), ?, ?, ?)",
                        (member, msg_id, row["body"], member, msg_id),
                    )
                summary.new_count += 1
            else:
                prev_body, prev_media, prev_kind, prev_flag, prev_thumb, prev_missing, prev_ts_raw = prev[2:]
                changed = (
                    prev_body != row["body"]
                    or prev_media != row["media"]
                    or prev_kind != row["kind"]
                    or prev_flag != row["flag"]
                    or prev_missing != 0
                    or prev_ts_raw != row["ts_raw"]
                    or (thumb_rel is not None and prev_thumb != thumb_rel)
                )
                if changed:
                    cur.execute(
                        """UPDATE messages SET group_=?, ts=?, ts_raw=?, body=?, media=?, kind=?, flag=?, thumb=?, missing=0
                           WHERE member=? AND msg_id=?""",
                        (
                            row["group_"], row["ts"], row["ts_raw"], row["body"], row["media"],
                            row["kind"], row["flag"], thumb_rel, member, msg_id,
                        ),
                    )
                    # FTS更新: 既存rowidを探して置き換え
                    cur.execute("SELECT rowid FROM messages_fts WHERE member=? AND msg_id=?", (member, msg_id))
                    fr = cur.fetchone()
                    if fr:
                        cur.execute("DELETE FROM messages_fts WHERE rowid=?", (fr[0],))
                    if row["body"]:
                        # rowidをmessagesと合わせる必要はないが、結合のためrowidを引けるようにする
                        cur.execute(
                            "INSERT INTO messages_fts(rowid, body, member, msg_id) VALUES ((SELECT rowid FROM messages WHERE member=? AND msg_id=?), ?, ?, ?)",
                            (member, msg_id, row["body"], member, msg_id),
                        )
                    summary.updated_count += 1
                else:
                    summary.skipped_count += 1

            if done % 500 == 0 or done == total:
                reporter.maybe_report(done, total)
                report("upserting", done=done, total=total)
                conn.commit()

        conn.commit()

        # missing判定: スコープ内の (member, yearmonth) に該当する既存行のうち、
        # 今回見つからなかった (member,msg_id) を missing=1 にする。
        # yearmonthはts(JST)から求める。scope.member_yearmonths[member] が None なら全月対象。
        report("missing_check")
        missing_updates = 0
        cur.execute("SELECT member, msg_id, ts, missing FROM messages")
        all_rows = cur.fetchall()
        for member, msg_id, ts, missing in all_rows:
            if member not in scope.member_yearmonths:
                continue
            months = scope.member_yearmonths[member]
            # tsはUTC epoch(JSTから9h引いたもの)。yyyymmはJST基準で求める必要がある。
            jst_dt = datetime.fromtimestamp(ts + 9 * 3600, tz=timezone.utc)
            yyyymm = f"{jst_dt.year:04d}{jst_dt.month:02d}"
            in_scope = (months is None) or (yyyymm in months)
            if not in_scope:
                continue
            key = (member, msg_id)
            should_be_missing = 1 if key not in seen_keys else 0
            if should_be_missing != missing:
                cur.execute(
                    "UPDATE messages SET missing=? WHERE member=? AND msg_id=?",
                    (should_be_missing, member, msg_id),
                )
                missing_updates += 1
                if should_be_missing:
                    summary.missing_count += 1
        conn.commit()
        print(f"[index] missing flag updates: {missing_updates}", file=sys.stderr)

        # サムネ生成
        if thumb_jobs:
            report("thumbnails", done=0, total=len(thumb_jobs))
            print(f"[index] generating {len(thumb_jobs)} thumbnails", file=sys.stderr)
            thumb_reporter = ProgressReporter(label="thumbs")
            thumb_done = 0
            # ffmpeg_pathはメインプロセスで解決済みのものをそのままワーカーへ渡す
            # (picklableな文字列/None なので安全。ワーカー側で毎回探索し直す必要がない)。
            job_args = [
                (src, dst, cfg.thumb_size, cfg.thumb_quality, kind, ffmpeg_path)
                for (src, dst, member, msg_id, rel, kind) in thumb_jobs
            ]
            with ProcessPoolExecutor() as executor:
                futures = {executor.submit(_gen_thumb_worker, a): a for a in job_args}
                for fut in as_completed(futures):
                    src_path_str, ok, err = fut.result()
                    thumb_done += 1
                    if ok:
                        summary.thumb_count += 1
                    else:
                        print(f"[warn] thumb failed for {src_path_str}: {err}", file=sys.stderr)
                    thumb_reporter.maybe_report(thumb_done, len(thumb_jobs))
                    if thumb_done % 200 == 0:
                        report("thumbnailing", done=thumb_done, total=len(thumb_jobs))
        else:
            print("[index] no new thumbnails needed", file=sys.stderr)

    finally:
        conn.commit()
        conn.close()


def print_summary(summary: IndexSummary):
    print("=" * 60)
    print("インデックス取り込み サマリ")
    print("=" * 60)
    print(f"新規:         {summary.new_count}")
    print(f"更新:         {summary.updated_count}")
    print(f"スキップ:     {summary.skipped_count}")
    print(f"missing化:    {summary.missing_count}")
    print(f"サムネ生成:   {summary.thumb_count}")
    print(f"走査ファイル数: {summary.total_files_scanned}")
    print(f"走査メッセージ数: {summary.total_messages_scanned}")
    print(f"所要時間:     {summary.elapsed_sec:.2f}秒")
    print("=" * 60)


def main():
    import argparse

    parser = argparse.ArgumentParser(description="メッセージ インデクサ")
    parser.add_argument("--full", action="store_true", help="全フォルダ走査")
    parser.add_argument("--incremental", action="store_true", help="直近Nヶ月のみ走査（既定）")
    parser.add_argument("--rebuild", action="store_true", help="DBを作り直して全走査")
    parser.add_argument("--only-member", type=str, default=None, help="特定メンバーのみ")
    parser.add_argument("--no-thumbs", action="store_true", help="サムネ生成をスキップ")
    args = parser.parse_args()

    cfg = get_config()

    if args.rebuild:
        mode = "full"
    elif args.full:
        mode = "full"
    else:
        mode = "incremental"

    try:
        summary = run_index(
            mode=mode,
            cfg=cfg,
            rebuild=args.rebuild,
            only_member=args.only_member,
            no_thumbs=args.no_thumbs,
        )
    except IndexAlreadyRunning as ex:
        print(f"[index] 既に実行中のためスキップします: {ex}", file=sys.stderr)
        sys.exit(2)
    print_summary(summary)


if __name__ == "__main__":
    main()
