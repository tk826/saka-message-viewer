"use strict";
/* ==========================================================================
   再インデックス
   ========================================================================== */
import { qs, fetchJson, toast } from "./utils.js";
import { Sidebar } from "./sidebar.js";
import { Timeline } from "./timeline.js";

const PHASE_LABELS = {
  scanning: "走査中...",
  upsert: "DB投入中",
  thumbs: "サムネ生成中",
  done: "完了",
  error: "エラー",
  interrupted: "中断",
};

function formatEta(sec) {
  if (sec === null || sec === undefined || !isFinite(sec) || sec < 0) return "";
  if (sec < 60) return `残り約${Math.ceil(sec)}秒`;
  const min = Math.round(sec / 60);
  if (min < 60) return `残り約${min}分`;
  const h = Math.floor(min / 60);
  const m = min % 60;
  return `残り約${h}時間${m}分`;
}

export const Reindex = {
  activePolling: null,   // 実行中: 2秒間隔
  idlePolling: null,     // 非実行中: 30秒間隔 (CLI起動を検知するため完全停止はしない)
  lastRunning: false,

  init() {
    qs("#reindexBtn").addEventListener("click", () => this.start());
    this.checkStatusOnLoad();
    this.startIdlePolling();
  },

  async checkStatusOnLoad() {
    try {
      const status = await fetchJson("/api/reindex/status");
      // ページ読み込み時点で中断/エラー状態だった場合も一目で分かるように表示する
      // (completed の場合はトースト通知やデータ再読み込みまではしない = 誤発火防止)。
      // progress.json 由来 (source: "file") の phase のみを見る。
      // プロセス内 _reindex_state.last_error は "already_running" のような
      // 内部的な早期リターン理由も含みうるため、ここでは参照しない
      // (誤ってユーザーにエラー扱いで見せてしまうのを防ぐ)。
      const p = status.progress;
      if (!status.running && p && p.source === "file") {
        if (p.phase === "interrupted") {
          this.showFinished("interrupted", this.interruptedDetail(p));
          return;
        }
        if (p.phase === "error") {
          this.showFinished("error", p.message || "エラーが発生しました");
          return;
        }
      }
      this.applyStatus(status);
    } catch (e) {
      console.error(e);
    }
  },

  async start() {
    const btn = qs("#reindexBtn");
    btn.disabled = true;
    this.showRunning({ phase: "scanning", message: "更新を開始しています..." });
    try {
      const res = await fetch("/api/reindex", { method: "POST" });
      if (res.status === 409) {
        toast("すでに実行中です");
        this.startActivePolling();
        this.poll();
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      this.startActivePolling();
      this.poll();
    } catch (e) {
      console.error(e);
      toast("更新の開始に失敗しました: " + e.message, true);
      btn.disabled = false;
      this.hideBar();
    }
  },

  // --- ポーリング制御 ---------------------------------------------------
  // 注意: ここでは即時 poll() を呼ばない。applyStatus() から呼ばれる経路があり、
  // (取得済みのstatusを処理中に、さらに新しいstatusをfetchする)二重ポーリングに
  // なってしまうため。即時取得したい呼び出し元は自分で poll() を呼ぶこと。
  startActivePolling() {
    this.stopIdlePolling();
    qs("#reindexBtn").disabled = true;
    if (this.activePolling) return;
    this.activePolling = setInterval(() => this.poll(), 2500);
  },

  stopActivePolling() {
    if (this.activePolling) {
      clearInterval(this.activePolling);
      this.activePolling = null;
    }
  },

  startIdlePolling() {
    if (this.idlePolling) return;
    this.idlePolling = setInterval(() => this.poll(), 30000);
  },

  stopIdlePolling() {
    if (this.idlePolling) {
      clearInterval(this.idlePolling);
      this.idlePolling = null;
    }
  },

  async poll() {
    try {
      const status = await fetchJson("/api/reindex/status");
      this.applyStatus(status);
    } catch (e) {
      console.error(e);
    }
  },

  // --- ステータス反映 -----------------------------------------------------
  applyStatus(status) {
    if (status.running) {
      // CLIで実行中のインデクサをアイドルポーリングで検知したケースも含め、
      // 実行中と分かったらアクティブポーリングに切り替える。
      this.startActivePolling();
      this.showRunning(this.normalizeProgress(status.progress));
      this.lastRunning = true;
      // 取り込み中でも、ユーザーが最新メッセージ位置を見ている場合は
      // 随時反映する(indexer側がこまめにcommit+checkpointするようになったため、
      // 完了を待たずに新着が読めるようになった分、体感速度を上げる)。
      Timeline.refreshIfAtBottom().catch((e) => console.error(e));
      // 取り込み中は継続してサイドバーも更新する(新規メンバーの追加・件数の
      // 増加を随時反映するため)。silent指定により、内容に変化が無ければ
      // render()自体はスキップされるのでちらつきは起きない。
      Sidebar.load({ silent: true }).catch((e) => console.error(e));
      return;
    }

    // ここに来るのは非実行中。直前まで実行中を追っていた場合のみ完了処理をする
    // (アイドルポーリングでたまたま非実行中を見ただけの場合は何もしない)。
    const wasTracking = this.lastRunning || this.activePolling !== null;
    this.stopActivePolling();
    this.startIdlePolling();
    qs("#reindexBtn").disabled = false;
    this.lastRunning = false;

    if (!wasTracking) return;

    const p = status.progress || {};
    // "already_running" は多重起動を防いだだけの内部的な早期リターンであり、
    // ユーザーに見せるべき失敗ではないため、エラー表示からは除外する。
    const isBenignAlreadyRunning = status.last_error && String(status.last_error).startsWith("already_running");
    if (p.phase === "interrupted" || (status.last_error && String(status.last_error).includes("interrupted"))) {
      this.showFinished("interrupted", this.interruptedDetail(p));
      toast("インデックス処理が中断されました", true);
    } else if (status.last_error && !isBenignAlreadyRunning) {
      this.showFinished("error", `エラー: ${status.last_error}`);
      toast("更新中にエラーが発生しました: " + status.last_error, true);
    } else if (isBenignAlreadyRunning) {
      // 何もしない: 自分が開始しようとしたが既に他プロセスが実行中だっただけ。
      this.hideBar();
    } else {
      this.hideBar();
      toast("更新が完了しました");
      // メンバー一覧・現在のタイムラインを再読み込み (新しいデータが入っているため)
      Sidebar.load().then(() => {
        if (Sidebar.selectedMember) {
          return Timeline.loadInitial(Sidebar.selectedMember, null);
        }
      }).catch((e) => console.error(e));
    }
  },

  // status.progress は2種類の形がありうる:
  //  - サーバ内で取り込み中: {stage, done, total, ...}
  //  - progress.json由来 (CLI で取り込み中、またはサーバ内の取り込みの終了後): {phase, done, total, percent, eta_sec, message, ...}
  // ここで表示用の共通形に正規化する。
  normalizeProgress(p) {
    if (!p) return { phase: "scanning", done: null, total: null, percent: null, eta_sec: null, message: null };
    if (p.phase) {
      // すでにファイル由来の形式
      return p;
    }
    // stageベース(プロセス内)を phase ベースに変換
    const stageToPhase = {
      scanning: "scanning",
      scanned: "scanning",
      upserting: "upsert",
      missing_check: "upsert",
      thumbnails: "thumbs",
      thumbnailing: "thumbs",
    };
    const phase = stageToPhase[p.stage] || p.stage || "scanning";
    let message = null;
    if (p.stage === "scanned") message = `${p.files ?? 0}件のファイル / ${p.messages ?? 0}件のメッセージを検出`;
    if (p.stage === "missing_check") message = "欠損チェック中...";
    const done = p.done ?? null;
    const total = p.total ?? null;
    const percent = (done !== null && total) ? Math.round((done / total) * 1000) / 10 : null;
    return { phase, done, total, percent, eta_sec: null, message };
  },

  // --- 表示 ---------------------------------------------------------------
  showRunning(p) {
    const bar = qs("#reindexStatusBar");
    bar.classList.remove("hidden", "is-error", "is-interrupted");

    const label = PHASE_LABELS[p.phase] || p.phase || "取り込み中";
    qs("#reindexPhaseLabel").textContent = label;

    let detail = p.message || "";
    if (p.phase === "upsert" || p.phase === "thumbs") {
      if (p.total) {
        detail = `${p.done ?? 0} / ${p.total} 件 (${p.percent != null ? p.percent : "?"}%)`;
      } else if (p.done !== null && p.done !== undefined) {
        detail = `${p.done} 件`;
      }
    } else if (p.phase === "scanning" && !detail) {
      detail = "ファイル総数を確認中...";
    }
    qs("#reindexDetailText").textContent = detail;

    const etaText = p.eta_sec != null ? formatEta(p.eta_sec) : "";
    qs("#reindexEtaText").textContent = etaText;

    const track = qs("#reindexProgressTrack");
    const fill = qs("#reindexProgressFill");
    if (p.total && p.done !== null && p.done !== undefined) {
      track.classList.remove("indeterminate");
      const pct = p.percent != null ? p.percent : Math.min(100, (p.done / p.total) * 100);
      fill.style.width = `${Math.max(0, Math.min(100, pct))}%`;
    } else {
      // total不明 (走査中など): 不定形プログレスバー
      track.classList.add("indeterminate");
      fill.style.width = "";
    }

    qs("#reindexBtn").disabled = true;
  },

  interruptedDetail(p) {
    const parts = [];
    if (p.pid) parts.push(`pid=${p.pid} が停止`);
    if (p.total && p.done !== null && p.done !== undefined) {
      parts.push(`${p.done} / ${p.total} 件 (${p.percent != null ? p.percent : "?"}%) まで処理済み`);
    }
    return parts.join(" / ") || "インデックス処理プロセスが見つかりません";
  },

  showFinished(kind, text) {
    // kind: "error" | "interrupted"
    const bar = qs("#reindexStatusBar");
    bar.classList.remove("hidden");
    bar.classList.toggle("is-error", kind === "error");
    bar.classList.toggle("is-interrupted", kind === "interrupted");
    qs("#reindexPhaseLabel").textContent = kind === "interrupted" ? "中断されました" : "エラー";
    qs("#reindexDetailText").textContent = text;
    qs("#reindexEtaText").textContent = "";
    const track = qs("#reindexProgressTrack");
    track.classList.remove("indeterminate");
    qs("#reindexProgressFill").style.width = "100%";
    setTimeout(() => this.hideBar(), 8000);
  },

  hideBar() {
    const bar = qs("#reindexStatusBar");
    bar.classList.add("hidden");
    bar.classList.remove("is-error", "is-interrupted");
  },
};
