"""坂道メッセージビューア バックエンド (FastAPI)。"""
from __future__ import annotations

import asyncio
import contextlib
import os
import re
import sqlite3
import sys
import threading
import time
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

from fastapi import FastAPI, HTTPException, Query, Request
from fastapi.responses import FileResponse, JSONResponse, Response, StreamingResponse
from starlette.concurrency import run_in_threadpool

import json

import shutil

from config import get_config, save_root, clear_root
from indexer import run_index, open_db, IndexSummary, IndexAlreadyRunning, IndexLock, pid_alive

cfg = get_config()

# 起動時に一度だけスキーマを保証する(_ensure_db_schema の定義は下記)。
# get_conn() 経由の各リクエストは ensure_schema=False で開くため、
# ここで先にテーブル・インデックスを作っておく必要がある。

# ---------------------------------------------------------------------------
# DB接続: スレッドごとに個別の接続を持つ(threading.local)。
# SQLiteのWALモードは「単一ライタ・複数リーダ同時実行」に対応しているため、
# app.py側の全リクエストを1本のLock+共有接続で直列化する必要はない。
# 別プロセス(indexer.py)が長時間の取り込み中でも、あるリクエストがSQLite側の
# ロック待ち(busy_timeout最大30秒)で詰まったとき、スレッドごとに接続を
# 分けておけば他の無関係なリクエストを巻き込まずに済む。
# _all_conns は /api/reset でDBファイルを削除する前に、生成済みの全接続を
# 確実に閉じる(Windowsでは開いたままのハンドルがあるとファイル削除に失敗
# しうる)ために追跡する。
# ---------------------------------------------------------------------------
_db_local = threading.local()
_all_conns_lock = threading.Lock()
_all_conns: list[sqlite3.Connection] = []
# /api/reset で全接続を閉じるたびに+1する世代カウンタ。各スレッドは自分が
# 接続を作った時点の世代を覚えておき、現在の世代とずれていたら
# (=自分の知らない間にresetが起きて接続が閉じられた)再接続する。
_db_generation = 0


def get_conn() -> sqlite3.Connection:
    conn = getattr(_db_local, "conn", None)
    gen = getattr(_db_local, "gen", -1)
    if conn is None or gen != _db_generation:
        # ensure_schema=False: スキーマ作成(CREATE TABLE/INDEX IF NOT EXISTS)は
        # プロセス起動時に _ensure_db_schema() で一度だけ済ませておく。
        # リクエスト処理のたびにここで実行すると、indexer.pyが長時間の書き込み
        # トランザクション中は毎回busy_timeoutブロックの起点になってしまうため。
        conn = open_db(cfg.db_path, ensure_schema=False)
        conn.row_factory = sqlite3.Row
        _db_local.conn = conn
        _db_local.gen = _db_generation
        with _all_conns_lock:
            _all_conns.append(conn)
    return conn


def _ensure_db_schema() -> None:
    """起動時に一度だけ、DBファイルとスキーマの存在を保証する。

    以降のリクエストは get_conn() で ensure_schema=False の接続を使うため、
    ここでテーブル・インデックスを先に作っておく必要がある
    (indexer.py側が先に作っている場合はCREATE ... IF NOT EXISTSなので無害)。
    """
    conn = open_db(cfg.db_path, ensure_schema=True)
    conn.close()


_ensure_db_schema()


def query_all(sql: str, params: tuple = ()) -> list[sqlite3.Row]:
    conn = get_conn()
    cur = conn.execute(sql, params)
    return cur.fetchall()


def _close_all_conns() -> None:
    """全スレッドの接続を閉じ、世代カウンタを進める(/api/reset専用)。

    sqlite3.Connection.close() は他スレッドから呼んでも安全(Python公式
    ドキュメントで明言されている数少ない例外的操作)なため、ここでまとめて
    閉じられる。世代カウンタを進めることで、各スレッドは次にget_conn()を
    呼んだ際「自分の接続は古い世代のものだ」と気づいて再接続する
    (閉じられた接続を掴んだまま使い続けることがない)。
    """
    global _db_generation
    with _all_conns_lock:
        for c in _all_conns:
            with contextlib.suppress(Exception):
                c.close()
        _all_conns.clear()
        _db_generation += 1


def query_one(sql: str, params: tuple = ()) -> Optional[sqlite3.Row]:
    rows = query_all(sql, params)
    return rows[0] if rows else None


# ---------------------------------------------------------------------------
# user.db 接続: お気に入り等ユーザー由来データ専用のDB。
# index.db とは完全に別ファイル・別ロックで管理し、indexer.py の --rebuild や
# /api/reset の index.db 削除処理からは触れない。
# こちらは書き込み(お気に入り登録)が主で同時アクセス数も少ないため、
# index.db 側のようなスレッドローカル化はせず、単一共有接続+ロックのままにする。
# ---------------------------------------------------------------------------
_user_db_lock = threading.Lock()
_user_conn: Optional[sqlite3.Connection] = None

USER_DB_SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS favorites (
  member     TEXT NOT NULL,
  msg_id     INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (member, msg_id)
);
CREATE INDEX IF NOT EXISTS idx_fav_created ON favorites(created_at DESC);
"""


def _open_user_db(db_path: Path) -> sqlite3.Connection:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(db_path), check_same_thread=False)
    conn.execute("PRAGMA journal_mode=WAL;")
    conn.execute("PRAGMA busy_timeout=30000;")
    conn.executescript(USER_DB_SCHEMA_SQL)
    conn.commit()
    conn.row_factory = sqlite3.Row
    return conn


def get_user_conn() -> sqlite3.Connection:
    global _user_conn
    if _user_conn is None:
        _user_conn = _open_user_db(cfg.user_db_path)
    return _user_conn


def user_query_all(sql: str, params: tuple = ()) -> list[sqlite3.Row]:
    with _user_db_lock:
        conn = get_user_conn()
        cur = conn.execute(sql, params)
        return cur.fetchall()


def user_query_one(sql: str, params: tuple = ()) -> Optional[sqlite3.Row]:
    rows = user_query_all(sql, params)
    return rows[0] if rows else None


def user_execute(sql: str, params: tuple = ()) -> None:
    with _user_db_lock:
        conn = get_user_conn()
        conn.execute(sql, params)
        conn.commit()


# ---------------------------------------------------------------------------
# 再インデックス状態管理
#
# ロックを2種類に分ける:
# - _reindex_run_lock: 「reindexが実行中か」の相互排他そのもの。
#   _run_reindex_background が run_index() 実行中(取り込み全体、数十秒〜数分)
#   ずっと保持し続ける長時間ロック。api_reset() がこれを使い、reindex実行中の
#   resetを防ぐ(取得できなければ即409、ブロッキング待ちはしない)。
# - _reindex_state_lock: _reindex_state 辞書そのものの読み書きだけを保護する
#   短命ロック。進捗コールバックや状態参照のたびに一瞬だけ取る。
#
# api_reindex_status のような「状態を読むだけ」のエンドポイントは、必ず
# _reindex_state_lock の方を使うこと。_reindex_run_lock をブロッキング取得
# すると、reindex実行中はそのAPIが取り込み完了までずっと応答を返せなくなり、
# イベントループを長時間占有して他の無関係なリクエストまで巻き込んでしまう。
# ---------------------------------------------------------------------------
_reindex_state = {
    "running": False,
    "started_at": None,
    "finished_at": None,
    "last_summary": None,
    "last_error": None,
    "progress": None,
}
_reindex_run_lock = threading.RLock()
_reindex_state_lock = threading.Lock()


def _reindex_progress_cb(payload: dict):
    with _reindex_state_lock:
        _reindex_state["progress"] = payload


def _run_reindex_background(mode: str = "incremental"):
    # _reindex_run_lock は「実行中フラグの排他」だけでなく、api_reset() のDB削除処理
    # との相互排他も兼ねる。ここで確保したロックは reindex 完了まで解放しないため、
    # reset実行中はreindexの開始がブロックされ、reindex実行中はresetがブロックされる
    # （api_reset側は非ブロッキングでこのロックを試み、取れなければ409を返す）。
    # 真の排他制御（他プロセス・CLI実行との競合防止）は
    # run_index() 内の IndexLock（data/.index.lock, PID+stale判定）が担う。
    # _reindex_state 辞書自体への読み書きは、この長時間ロックとは別の
    # _reindex_state_lock(短命)で保護する。他スレッド(APIハンドラ)は
    # _reindex_run_lock を待たずに状態を読めるようにするため。
    if not _reindex_run_lock.acquire(blocking=False):
        return
    try:
        with _reindex_state_lock:
            if _reindex_state["running"]:
                return
            _reindex_state["running"] = True
            _reindex_state["started_at"] = time.time()
            _reindex_state["last_error"] = None
            _reindex_state["progress"] = None

        try:
            summary = run_index(mode=mode, cfg=cfg, progress_cb=_reindex_progress_cb)
            with _reindex_state_lock:
                _reindex_state["last_summary"] = summary.__dict__
        except IndexAlreadyRunning as ex:
            # 他プロセス（別のindexer.py実行など）が既にロックを保持している。
            with _reindex_state_lock:
                _reindex_state["last_error"] = f"already_running: {ex}"
            print(f"[reindex] skipped, already running elsewhere: {ex}")
        except Exception as ex:
            with _reindex_state_lock:
                _reindex_state["last_error"] = str(ex)
            print(f"[reindex] error: {ex}")
        finally:
            with _reindex_state_lock:
                _reindex_state["running"] = False
                _reindex_state["finished_at"] = time.time()
            # 再インデックス後、次回クエリで最新DB内容を見えるようにするため接続はそのまま
            # (同一プロセス内の同一コネクションなので自動的に見える)
    finally:
        _reindex_run_lock.release()


@asynccontextmanager
async def lifespan(app: FastAPI):
    if cfg.auto_index_on_startup and cfg.root is not None:
        def _bg():
            _run_reindex_background(mode="incremental")

        t = threading.Thread(target=_bg, daemon=True)
        t.start()
    yield


app = FastAPI(title="坂道メッセージビューア", lifespan=lifespan)


# 調査用: SQLiteロック待ち等で一部リクエストだけ極端に遅くなる場合に備え、
# 閾値を超えたリクエストのみサーバ側ログにも出す。フロント側(index.html の
# fetchJson)の計測と突き合わせて、ブラウザ〜サーバ間なのかサーバ内部
# (DB待ち等)なのかを切り分けられるようにする。
# config-dev.json 使用時(=開発環境)のみ有効にし、本番運用(config.json)では
# 出力しない。
_SLOW_REQUEST_THRESHOLD_SEC = 1.0


@app.middleware("http")
async def _log_slow_requests(request: Request, call_next):
    if not cfg.is_dev:
        return await call_next(request)
    t0 = time.time()
    response = await call_next(request)
    elapsed = time.time() - t0
    if elapsed >= _SLOW_REQUEST_THRESHOLD_SEC:
        print(f"[slow] {request.method} {request.url.path} took {elapsed:.1f}s", file=sys.stderr)
    return response


# ---------------------------------------------------------------------------
# ヘルパー
# ---------------------------------------------------------------------------

def row_to_message(row: sqlite3.Row) -> dict:
    d = {
        "member": row["member"],
        "group": row["group_"],
        "msg_id": row["msg_id"],
        "ts": row["ts"],
        "ts_raw": row["ts_raw"],
        "body": row["body"],
        "kind": row["kind"],
        "flag": row["flag"],
        "missing": bool(row["missing"]),
        "media_url": f"/media/{row['media']}" if row["media"] else None,
        "thumb_url": f"/thumbs/{row['thumb']}" if row["thumb"] else None,
    }
    return d


def _validate_date(s: str) -> datetime:
    try:
        return datetime.strptime(s, "%Y-%m-%d")
    except ValueError:
        raise HTTPException(status_code=400, detail=f"invalid date format: {s}, expected YYYY-MM-DD")


def jst_date_to_epoch_range(date_str: str) -> tuple[int, int]:
    """YYYY-MM-DD (JST) の 00:00:00〜23:59:59 を unix epoch範囲に変換。"""
    dt = _validate_date(date_str)
    start_utc = int(datetime(dt.year, dt.month, dt.day, 0, 0, 0, tzinfo=timezone.utc).timestamp()) - 9 * 3600
    end_utc = start_utc + 86400 - 1
    return start_utc, end_utc


# ---------------------------------------------------------------------------
# API: /api/threads
# ---------------------------------------------------------------------------
@app.get("/api/threads")
async def api_threads():
    rows = await run_in_threadpool(
        query_all,
        """
        SELECT member, group_ AS grp, COUNT(*) AS cnt, MAX(ts) AS last_ts
        FROM messages
        GROUP BY member, group_
        ORDER BY last_ts DESC
        """,
    )
    result = []
    for r in rows:
        result.append(
            {
                "member": r["member"],
                "group": r["grp"],
                "count": r["cnt"],
                "last_ts": r["last_ts"],
            }
        )
    return result


# ---------------------------------------------------------------------------
# API: /api/messages
# ---------------------------------------------------------------------------
@app.get("/api/messages")
async def api_messages(
    member: str = Query(...),
    before: Optional[int] = Query(None),
    after: Optional[int] = Query(None),
    limit: int = Query(50, ge=1, le=500),
):
    if before is not None and after is not None:
        raise HTTPException(status_code=400, detail="before と after は同時指定不可")

    if after is not None:
        rows = await run_in_threadpool(
            query_all,
            """SELECT * FROM messages WHERE member=? AND ts > ?
               ORDER BY ts ASC LIMIT ?""",
            (member, after, limit),
        )
        rows = list(rows)
        rows.sort(key=lambda r: r["ts"])
    elif before is not None:
        rows = await run_in_threadpool(
            query_all,
            """SELECT * FROM messages WHERE member=? AND ts < ?
               ORDER BY ts DESC LIMIT ?""",
            (member, before, limit),
        )
        rows = list(rows)
        rows.reverse()
    else:
        rows = await run_in_threadpool(
            query_all,
            """SELECT * FROM messages WHERE member=?
               ORDER BY ts DESC LIMIT ?""",
            (member, limit),
        )
        rows = list(rows)
        rows.reverse()

    return {
        "member": member,
        "messages": [row_to_message(r) for r in rows],
    }


# ---------------------------------------------------------------------------
# API: /api/search
# ---------------------------------------------------------------------------
def _fts_escape(q: str) -> str:
    # ダブルクォートをエスケープしてフレーズクエリとして扱う（trigramトークナイザ向け）
    return q.replace('"', '""')


@app.get("/api/search")
async def api_search(
    q: str = Query(..., min_length=1),
    member: Optional[str] = Query(None),
    from_: Optional[str] = Query(None, alias="from"),
    to: Optional[str] = Query(None),
    limit: int = Query(50, ge=1, le=500),
):
    ts_min = None
    ts_max = None
    if from_:
        ts_min, _ = jst_date_to_epoch_range(from_)
    if to:
        _, ts_max = jst_date_to_epoch_range(to)

    use_fts = len(q) >= 3  # trigramは2文字以下を扱えないため、3文字未満はLIKEにフォールバック

    conditions = []
    params: list = []

    if use_fts:
        fts_query = f'"{_fts_escape(q)}"'
        sql = """
            SELECT m.*, snippet(messages_fts, 0, '<mark>', '</mark>', '...', 10) AS snippet
            FROM messages_fts
            JOIN messages m ON m.rowid = messages_fts.rowid
            WHERE messages_fts MATCH ?
        """
        params.append(fts_query)
    else:
        sql = """
            SELECT m.*, m.body AS snippet
            FROM messages m
            WHERE m.body LIKE ?
        """
        params.append(f"%{q}%")

    if member:
        sql += " AND m.member = ?"
        params.append(member)
    if ts_min is not None:
        sql += " AND m.ts >= ?"
        params.append(ts_min)
    if ts_max is not None:
        sql += " AND m.ts <= ?"
        params.append(ts_max)

    sql += " ORDER BY m.ts DESC LIMIT ?"
    params.append(limit)

    try:
        rows = await run_in_threadpool(query_all, sql, tuple(params))
    except sqlite3.OperationalError as ex:
        raise HTTPException(status_code=400, detail=f"search query error: {ex}")

    results = []
    for r in rows:
        msg = row_to_message(r)
        msg["snippet"] = r["snippet"]
        results.append(msg)

    return {"query": q, "used_fts": use_fts, "results": results}


# ---------------------------------------------------------------------------
# API: /api/jump
# ---------------------------------------------------------------------------
@app.get("/api/jump")
async def api_jump(member: str = Query(...), date: str = Query(...)):
    start_ts, _ = jst_date_to_epoch_range(date)
    row = await run_in_threadpool(
        query_one,
        """SELECT ts FROM messages WHERE member=? AND ts >= ?
           ORDER BY ts ASC LIMIT 1""",
        (member, start_ts),
    )
    if row is None:
        return {"member": member, "date": date, "ts": None}
    return {"member": member, "date": date, "ts": row["ts"]}


# ---------------------------------------------------------------------------
# API: /api/calendar
# ---------------------------------------------------------------------------
@app.get("/api/calendar")
async def api_calendar(member: str = Query(...), yearmonth: Optional[str] = Query(None)):
    # JST変換とYYYY-MM(-DD)集計をSQL側で行い、Pythonループでの全件走査を避ける
    # （メンバーによっては1万件超のメッセージがあり、体感できるレベルで重い）。
    if yearmonth is not None:
        if not re.match(r"^\d{4}-\d{2}$", yearmonth):
            raise HTTPException(status_code=400, detail=f"invalid yearmonth format: {yearmonth}, expected YYYY-MM")
        rows = await run_in_threadpool(
            query_all,
            """
            SELECT strftime('%Y-%m-%d', ts + 9 * 3600, 'unixepoch') AS date, COUNT(*) AS cnt
            FROM messages
            WHERE member=? AND strftime('%Y-%m', ts + 9 * 3600, 'unixepoch')=?
            GROUP BY date
            ORDER BY date
            """,
            (member, yearmonth),
        )
        result = [{"date": r["date"], "count": r["cnt"]} for r in rows]
        return {"member": member, "yearmonth": yearmonth, "days": result}

    rows = await run_in_threadpool(
        query_all,
        """
        SELECT strftime('%Y-%m', ts + 9 * 3600, 'unixepoch') AS yearmonth, COUNT(*) AS cnt
        FROM messages
        WHERE member=?
        GROUP BY yearmonth
        ORDER BY yearmonth
        """,
        (member,),
    )
    result = [{"yearmonth": r["yearmonth"], "count": r["cnt"]} for r in rows]
    return {"member": member, "calendar": result}


# ---------------------------------------------------------------------------
# API: /api/favorites
# ---------------------------------------------------------------------------
@app.post("/api/favorites")
async def api_add_favorite(payload: dict):
    member = str(payload.get("member", "")).strip()
    msg_id_raw = payload.get("msg_id")
    if not member or msg_id_raw is None:
        raise HTTPException(status_code=400, detail="member と msg_id は必須です")
    try:
        msg_id = int(msg_id_raw)
    except (TypeError, ValueError):
        raise HTTPException(status_code=400, detail="msg_id は整数で指定してください")

    # messages側(index.db)に存在しないメッセージをお気に入り登録できてしまうと、
    # 一覧表示時に肉付けできない幽霊お気に入りが残るため、事前に存在確認する。
    exists = await run_in_threadpool(
        query_one, "SELECT 1 FROM messages WHERE member=? AND msg_id=?", (member, msg_id)
    )
    if exists is None:
        raise HTTPException(status_code=404, detail="message not found")

    await run_in_threadpool(
        user_execute,
        "INSERT OR IGNORE INTO favorites (member, msg_id, created_at) VALUES (?, ?, ?)",
        (member, msg_id, int(time.time())),
    )
    return {"status": "ok", "favorited": True}


@app.delete("/api/favorites")
async def api_remove_favorite(member: str = Query(...), msg_id: int = Query(...)):
    await run_in_threadpool(
        user_execute,
        "DELETE FROM favorites WHERE member=? AND msg_id=?",
        (member, msg_id),
    )
    return {"status": "ok", "favorited": False}


@app.get("/api/favorites")
async def api_list_favorites(
    limit: int = Query(50, ge=1, le=200),
    before: Optional[int] = Query(None),
    member: Optional[str] = Query(None),
):
    # user.db (favorites) と index.db (messages) は別ファイルなのでSQL側でJOINできない。
    # そのためuser.dbからcreated_at順の(member, msg_id)を取り、その組でmessagesを引いて
    # row_to_messageで肉付けする。件数はfavorites側のlimitで確定させ、messages側に
    # 該当が無い(--rebuild直後など)行はスキップする。
    # member未指定時は従来どおり全メンバー横断（後方互換）。
    if member is not None:
        if before is not None:
            fav_rows = await run_in_threadpool(
                user_query_all,
                "SELECT member, msg_id, created_at FROM favorites WHERE member=? AND created_at < ? ORDER BY created_at DESC LIMIT ?",
                (member, before, limit),
            )
        else:
            fav_rows = await run_in_threadpool(
                user_query_all,
                "SELECT member, msg_id, created_at FROM favorites WHERE member=? ORDER BY created_at DESC LIMIT ?",
                (member, limit),
            )
    elif before is not None:
        fav_rows = await run_in_threadpool(
            user_query_all,
            "SELECT member, msg_id, created_at FROM favorites WHERE created_at < ? ORDER BY created_at DESC LIMIT ?",
            (before, limit),
        )
    else:
        fav_rows = await run_in_threadpool(
            user_query_all,
            "SELECT member, msg_id, created_at FROM favorites ORDER BY created_at DESC LIMIT ?",
            (limit,),
        )

    if not fav_rows:
        return {"favorites": []}

    fav_list = [(r["member"], r["msg_id"], r["created_at"]) for r in fav_rows]
    pairs = [(m, i) for (m, i, _) in fav_list]

    # IN句のパラメータ数を抑えるため (member, msg_id) の組ごとにOR条件で引く。
    # 件数はlimit(最大200)なので、SQLite既定のパラメータ上限に対しても十分余裕がある。
    placeholders = " OR ".join(["(member=? AND msg_id=?)"] * len(pairs))
    params: list = []
    for m, i in pairs:
        params.extend([m, i])
    msg_rows = await run_in_threadpool(
        query_all, f"SELECT * FROM messages WHERE {placeholders}", tuple(params)
    )
    msg_by_key = {(r["member"], r["msg_id"]): r for r in msg_rows}

    result = []
    for member, msg_id, created_at in fav_list:
        row = msg_by_key.get((member, msg_id))
        if row is None:
            continue
        msg = row_to_message(row)
        msg["created_at"] = created_at
        result.append(msg)

    return {"favorites": result}


@app.get("/api/favorites/ids")
async def api_favorite_ids(member: str = Query(...)):
    rows = await run_in_threadpool(
        user_query_all,
        "SELECT msg_id FROM favorites WHERE member=?",
        (member,),
    )
    return {"msg_ids": [r["msg_id"] for r in rows]}


# ---------------------------------------------------------------------------
# メディア配信 (Range対応)
# ---------------------------------------------------------------------------
RANGE_RE = re.compile(r"bytes=(\d*)-(\d*)")

MIME_TYPES = {
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".png": "image/png",
    ".mp4": "video/mp4",
    ".txt": "text/plain; charset=utf-8",
}


def _guess_mime(path: Path) -> str:
    return MIME_TYPES.get(path.suffix.lower(), "application/octet-stream")


def _safe_resolve(base: Path, rel_path: str) -> Path:
    """パストラバーサル対策: realpathで解決後、baseの外に出ていないか検証する。"""
    candidate = (base / rel_path).resolve()
    base_resolved = base.resolve()
    try:
        candidate.relative_to(base_resolved)
    except ValueError:
        raise HTTPException(status_code=403, detail="invalid path")
    if not candidate.exists() or not candidate.is_file():
        raise HTTPException(status_code=404, detail="file not found")
    return candidate


def _serve_file_with_range(request: Request, file_path: Path) -> Response:
    file_size = file_path.stat().st_size
    mime = _guess_mime(file_path)
    range_header = request.headers.get("range")

    if range_header:
        m = RANGE_RE.match(range_header)
        if not m:
            raise HTTPException(status_code=416, detail="invalid range header")
        start_s, end_s = m.groups()
        if start_s == "" and end_s == "":
            raise HTTPException(status_code=416, detail="invalid range header")
        if start_s == "":
            # suffix range: last N bytes
            length = int(end_s)
            start = max(0, file_size - length)
            end = file_size - 1
        else:
            start = int(start_s)
            end = int(end_s) if end_s != "" else file_size - 1
        end = min(end, file_size - 1)
        if start > end or start >= file_size:
            headers = {"Content-Range": f"bytes */{file_size}"}
            raise HTTPException(status_code=416, detail="range not satisfiable", headers=headers)

        chunk_size = end - start + 1

        def iter_chunk():
            with open(file_path, "rb") as f:
                f.seek(start)
                remaining = chunk_size
                block = 1024 * 1024
                while remaining > 0:
                    read_size = min(block, remaining)
                    data = f.read(read_size)
                    if not data:
                        break
                    remaining -= len(data)
                    yield data

        headers = {
            "Content-Range": f"bytes {start}-{end}/{file_size}",
            "Accept-Ranges": "bytes",
            "Content-Length": str(chunk_size),
        }
        return StreamingResponse(iter_chunk(), status_code=206, media_type=mime, headers=headers)

    # Rangeなし: 全体を返す
    def iter_full():
        with open(file_path, "rb") as f:
            block = 1024 * 1024
            while True:
                data = f.read(block)
                if not data:
                    break
                yield data

    headers = {
        "Accept-Ranges": "bytes",
        "Content-Length": str(file_size),
    }
    return StreamingResponse(iter_full(), status_code=200, media_type=mime, headers=headers)


@app.get("/media/{path:path}")
async def get_media(path: str, request: Request):
    if cfg.root is None:
        raise HTTPException(status_code=409, detail="root is not configured")
    file_path = _safe_resolve(cfg.root, path)
    return _serve_file_with_range(request, file_path)


@app.get("/thumbs/{path:path}")
async def get_thumb(path: str, request: Request):
    file_path = _safe_resolve(cfg.thumbs_dir, path)
    return _serve_file_with_range(request, file_path)


def _safe_resolve_no_exist_check(base: Path, rel_path: str) -> Path:
    """_safe_resolveのパストラバーサル検証のみを行い、実在チェックはしない版。

    「パスをコピー」機能では、実ファイルが欠損(missing)していても絶対パス文字列
    自体は返したい（existsフィールドで欠損有無を別途知らせる）ため、
    _safe_resolveのように未存在で404にしてしまうものは使えない。
    """
    candidate = (base / rel_path).resolve()
    base_resolved = base.resolve()
    try:
        candidate.relative_to(base_resolved)
    except ValueError:
        raise HTTPException(status_code=403, detail="invalid path")
    return candidate


@app.get("/api/media-path")
async def api_media_path(member: str = Query(...), msg_id: int = Query(...)):
    row = await run_in_threadpool(
        query_one, "SELECT media FROM messages WHERE member=? AND msg_id=?", (member, msg_id)
    )
    if row is None or not row["media"]:
        raise HTTPException(status_code=404, detail="message not found or has no media")

    if cfg.root is None:
        raise HTTPException(status_code=409, detail="root is not configured")

    file_path = _safe_resolve_no_exist_check(cfg.root, row["media"])
    return {"path": str(file_path), "exists": file_path.exists() and file_path.is_file()}


# ---------------------------------------------------------------------------
# API: /api/media-list (メンバー別メディアギャラリー)
# ---------------------------------------------------------------------------
MEDIA_KINDS = {"image", "video", "audio"}


@app.get("/api/media-list")
async def api_media_list(
    member: str = Query(...),
    kind: Optional[str] = Query(None),
    before: Optional[int] = Query(None),
    limit: int = Query(60, ge=1, le=200),
):
    if kind is not None and kind not in MEDIA_KINDS:
        raise HTTPException(status_code=400, detail=f"invalid kind: {kind}, expected one of {sorted(MEDIA_KINDS)}")

    conditions = ["member=?", "missing=0"]
    params: list = [member]
    if kind is not None:
        conditions.append("kind=?")
        params.append(kind)
    else:
        # kind省略時はtext以外すべて(image/video/audio)を対象にする。
        conditions.append("kind != 'text'")
    if before is not None:
        conditions.append("ts < ?")
        params.append(before)

    where = " AND ".join(conditions)
    # has_moreを判定するため limit+1 件取得し、余分の1件を切り落として返す。
    rows = await run_in_threadpool(
        query_all,
        f"SELECT * FROM messages WHERE {where} ORDER BY ts DESC LIMIT ?",
        (*params, limit + 1),
    )
    rows = list(rows)
    has_more = len(rows) > limit
    rows = rows[:limit]

    return {"items": [row_to_message(r) for r in rows], "has_more": has_more}


# ---------------------------------------------------------------------------
# 設定API
# ---------------------------------------------------------------------------
@app.get("/api/settings")
async def api_get_settings():
    return {
        "root": str(cfg.root) if cfg.root is not None else None,
        "configured": cfg.root is not None,
        # フロント側の調査用コンソールログ(Sidebar.load等)を開発環境限定に
        # するための判定材料。config-dev.json使用時のみ true。
        "is_dev": cfg.is_dev,
    }


@app.post("/api/settings")
async def api_set_settings(payload: dict):
    new_root = str(payload.get("root", "")).strip()
    if not new_root:
        raise HTTPException(status_code=400, detail="root は必須です")

    p = Path(new_root)
    if not p.is_absolute():
        raise HTTPException(status_code=400, detail="root は絶対パスで指定してください")
    if not p.exists() or not p.is_dir():
        raise HTTPException(status_code=400, detail=f"フォルダが見つかりません: {new_root}")

    resolved = save_root(cfg, new_root)

    with _reindex_state_lock:
        already_running = _reindex_state["running"]
    if not already_running:
        t = threading.Thread(target=_run_reindex_background, kwargs={"mode": "incremental"}, daemon=True)
        t.start()

    return {"root": str(resolved), "configured": True}


@app.post("/api/reset")
async def api_reset():
    """DB・サムネイル・保存フォルダ設定を初期化する（初回起動相当の状態に戻す）。

    _reindex_run_lock を削除処理が終わるまで保持し続けることで、reindexスレッド
    （_run_reindex_background）と相互排他する。ロックが取れない場合は
    reindexが実行中ということなので即409を返す（TOCTOUレース防止）。
    ここは元々ブロッキングせずに即409を返す設計なので、長時間ロックを
    そのまま使っても他のリクエストを巻き込む問題は起きない。
    """
    global _user_conn

    if not _reindex_run_lock.acquire(blocking=False):
        return JSONResponse({"status": "already_running"}, status_code=409)

    try:
        with _reindex_state_lock:
            already_running = _reindex_state["running"]
        if already_running:
            return JSONResponse({"status": "already_running"}, status_code=409)

        other_pid = IndexLock(cfg.index_lock_path).peek_running_pid()
        if other_pid is not None:
            return JSONResponse(
                {"status": "already_running", "detail": f"index process pid={other_pid} is running"},
                status_code=409,
            )

        _close_all_conns()

        if cfg.db_path.exists():
            cfg.db_path.unlink()
        for suffix in ("-wal", "-shm"):
            p = Path(str(cfg.db_path) + suffix)
            if p.exists():
                p.unlink()

        # get_conn() は ensure_schema=False で開くため、削除した空DBに対して
        # 次のリクエストがテーブル未作成のままSELECTしてしまわないよう、
        # ここで即座にスキーマを作り直しておく。
        _ensure_db_schema()

        if cfg.thumbs_dir.exists():
            shutil.rmtree(cfg.thumbs_dir)
        cfg.thumbs_dir.mkdir(parents=True, exist_ok=True)

        for name in ("progress.json", ".index.lock"):
            p = cfg.data_dir / name
            if p.exists():
                p.unlink()

        # /api/reset は「初回起動時の状態に戻す」処理であり、お気に入り(user.db)も
        # 対象に含める。index.dbとは別ファイル・別ロック(_user_db_lock)なので、
        # 同様に接続を閉じてから削除する。
        with _user_db_lock:
            if _user_conn is not None:
                _user_conn.close()
                _user_conn = None

            if cfg.user_db_path.exists():
                cfg.user_db_path.unlink()
            for suffix in ("-wal", "-shm"):
                p = Path(str(cfg.user_db_path) + suffix)
                if p.exists():
                    p.unlink()

        clear_root(cfg)

        with _reindex_state_lock:
            _reindex_state["running"] = False
            _reindex_state["started_at"] = None
            _reindex_state["finished_at"] = None
            _reindex_state["last_summary"] = None
            _reindex_state["last_error"] = None
            _reindex_state["progress"] = None
    finally:
        _reindex_run_lock.release()

    return {"status": "reset"}


# ---------------------------------------------------------------------------
# 再インデックスAPI
# ---------------------------------------------------------------------------
@app.post("/api/reindex")
async def api_reindex():
    if cfg.root is None:
        return JSONResponse({"status": "root_not_configured"}, status_code=409)

    with _reindex_state_lock:
        if _reindex_state["running"]:
            return JSONResponse({"status": "already_running"}, status_code=409)

    # プロセス内フラグだけでなく、他プロセス（CLIでのindexer.py実行など）が
    # ロックファイルを保持していないかも事前確認する。最終的な排他判定は
    # run_index() 内の IndexLock.acquire() が行うため、ここはあくまで早期応答用。
    other_pid = IndexLock(cfg.index_lock_path).peek_running_pid()
    if other_pid is not None:
        return JSONResponse(
            {"status": "already_running", "detail": f"index process pid={other_pid} is running"},
            status_code=409,
        )

    t = threading.Thread(target=_run_reindex_background, kwargs={"mode": "incremental"}, daemon=True)
    t.start()
    return {"status": "started"}


def _read_progress_file() -> Optional[dict]:
    """progress.json を読む。存在しない/壊れている場合は None を返す。

    書き込み側 (indexer.ProgressFileWriter) は一時ファイル+os.replaceで
    アトミックに書くため、読みかけの壊れたJSONを掴む心配は基本的にないが、
    念のため例外はここで握りつぶし、呼び出し側は「情報なし」として扱う。
    """
    path = cfg.progress_json_path
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except FileNotFoundError:
        return None
    except (json.JSONDecodeError, OSError) as ex:
        print(f"[reindex/status] failed to read progress.json: {ex}")
        return None


@app.get("/api/reindex/status")
async def api_reindex_status():
    with _reindex_state_lock:
        state = {
            "running": _reindex_state["running"],
            "started_at": _reindex_state["started_at"],
            "finished_at": _reindex_state["finished_at"],
            "progress": _reindex_state["progress"],
            "last_summary": _reindex_state["last_summary"],
            "last_error": _reindex_state["last_error"],
        }

    # app.py 経由 (このプロセス内スレッド) で実行中なら、それが最も新鮮な情報源。
    # progress.json は見ずにそのまま返す。
    if state["running"]:
        return state

    # プロセス内状態が非実行中の場合、CLIで直接起動された indexer.py の進捗を
    # progress.json 経由で拾う。ファイルが無ければ従来通り (running: False) を返す。
    pf = _read_progress_file()
    if pf is None:
        return state

    pf_running = bool(pf.get("running"))
    pf_pid = pf.get("pid")

    if pf_running and pf_pid is not None and not pid_alive(pf_pid):
        # progress.json 上は running=true だが、書き込んだプロセスが既に死んでいる
        # = 異常終了などで中断された状態 (stale)。中断扱いにして返す。
        return {
            "running": False,
            "started_at": pf.get("started_at"),
            "finished_at": pf.get("updated_at"),
            "progress": {
                "source": "file",
                "phase": "interrupted",
                "done": pf.get("done"),
                "total": pf.get("total"),
                "percent": pf.get("percent"),
                "pid": pf_pid,
            },
            "last_summary": state["last_summary"],
            "last_error": f"index process (pid={pf_pid}) is not running (interrupted)",
        }

    progress_payload = dict(pf)
    progress_payload["source"] = "file"

    return {
        "running": pf_running,
        "started_at": pf.get("started_at"),
        "finished_at": None if pf_running else pf.get("updated_at"),
        "progress": progress_payload,
        "last_summary": pf.get("summary") if not pf_running and pf.get("phase") == "done" else state["last_summary"],
        "last_error": pf.get("message") if not pf_running and pf.get("phase") == "error" else state["last_error"],
    }


# ---------------------------------------------------------------------------
# 静的ファイル (index.html)
# ---------------------------------------------------------------------------
STATIC_DIR = Path(__file__).resolve().parent / "static"


@app.get("/")
async def index():
    return FileResponse(str(STATIC_DIR / "index.html"))


if __name__ == "__main__":
    import sys

    import uvicorn

    if sys.platform == "win32":
        # WindowsのデフォルトProactorEventLoopは、クライアントが接続を確立途中
        # (TCPハンドシェイク後、ブラウザのタブを閉じる等)で切断した際に
        # OSErrorがaccept_coro自体を異常終了させ、以降そのイベントループが
        # 二度とacceptしなくなる(サーバプロセスは生きているのに新規接続だけ
        # 全て拒否される)既知の弱点がある。SelectorEventLoopはaccept処理の
        # 実装が異なりこの種のクラッシュを起こさないため、Windows限定で
        # 明示的に切り替える(このアプリはasyncio経由のsubprocessを使わないため
        # SelectorEventLoopの制限の影響を受けない)。
        asyncio.set_event_loop_policy(asyncio.WindowsSelectorEventLoopPolicy())

    uvicorn.run("app:app", host="127.0.0.1", port=cfg.port, reload=False, log_level="info", access_log=False)
