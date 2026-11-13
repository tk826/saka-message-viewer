"use strict";
/* ==========================================================================
   設定 (メッセージ保存フォルダの指定)
   ========================================================================== */
import { qs, ce, fetchJson, toast, setDevMode } from "./utils.js";
import { Sidebar } from "./sidebar.js";
import { Reindex } from "./reindex.js";
import { Timeline } from "./timeline.js";

export const Settings = {
  configured: false,
  TABS: ["folder", "display", "reindex", "danger"],

  init() {
    qs("#settingsBtn").addEventListener("click", () => this.open());
    qs("#settingsCloseBtn").addEventListener("click", () => this.close());
    qs("#settingsSaveBtn").addEventListener("click", () => this.save());
    qs("#settingsRootInput").addEventListener("keydown", (e) => {
      if (e.key === "Enter") this.save();
    });
    qs("#settingsReindexBtn").addEventListener("click", () => this.startFullReindex());
    qs("#settingsDeleteMemberBtn").addEventListener("click", () => this.deleteMember());
    qs("#settingsResetBtn").addEventListener("click", () => this.reset());

    for (const btn of document.querySelectorAll(".settings-tab")) {
      btn.addEventListener("click", () => this.switchTab(btn.dataset.tab));
    }
  },

  switchTab(tab) {
    for (const name of this.TABS) {
      const isActive = name === tab;
      qs(`.settings-tab[data-tab="${name}"]`).classList.toggle("active", isActive);
      qs(`#settingsTab${name.charAt(0).toUpperCase()}${name.slice(1)}`).classList.toggle("hidden", !isActive);
    }
  },

  async checkOnStartup() {
    try {
      const data = await fetchJson("/api/settings");
      this.configured = !!data.configured;
      setDevMode(data.is_dev);
      if (data.root) qs("#settingsRootInput").value = data.root;
      if (!this.configured) {
        this.open(true);
      }
    } catch (e) {
      console.error(e);
    }
  },

  open(forced = false) {
    qs("#settingsError").classList.add("hidden");
    qs("#settingsModal").classList.remove("hidden");
    qs("#settingsCloseBtn").classList.toggle("hidden", forced && !this.configured);
    this.switchTab("folder");
    for (const name of ["display", "reindex", "danger"]) {
      qs(`.settings-tab[data-tab="${name}"]`).disabled = !this.configured;
    }
    if (this.configured) {
      this.fillMemberOptions();
      this.fillDeleteMemberOptions();
      // ffmpegは後から配置されうるので、開くたびに最新の状態を取り直す
      this.refreshFfmpegStatus();
    }
  },

  async refreshFfmpegStatus() {
    const statusEl = qs("#settingsFfmpegStatus");
    const helpEl = qs("#settingsFfmpegHelp");
    try {
      const data = await fetchJson("/api/settings");
      if (data.ffmpeg_available) {
        statusEl.textContent = `利用可能: ${data.ffmpeg_path}`;
        helpEl.classList.add("hidden");
      } else {
        statusEl.textContent = "未検出のため、動画のサムネイルは作成されません。";
        helpEl.classList.remove("hidden");
      }
    } catch (e) {
      console.error(e);
      statusEl.textContent = "状態を取得できませんでした";
      helpEl.classList.add("hidden");
    }
  },

  // 取り込みタブのメンバー選択肢を、サイドバーが持っている一覧から作る。
  // 選択中のメンバーがいれば初期選択にしておく。
  fillMemberOptions() {
    const sel = qs("#settingsReindexMember");
    const prev = sel.value;
    sel.innerHTML = "";
    const allOpt = ce("option");
    allOpt.value = "";
    allOpt.textContent = "全メンバー";
    sel.appendChild(allOpt);
    for (const t of Sidebar.threads) {
      const opt = ce("option");
      opt.value = t.member;
      opt.textContent = t.member;
      sel.appendChild(opt);
    }
    const preferred = prev || Sidebar.selectedMember || "";
    if ([...sel.options].some((o) => o.value === preferred)) sel.value = preferred;
  },

  // 初期化タブのメンバー削除選択肢を、サイドバーが持っている一覧から作る。
  // 取り込みタブと異なり「全メンバー」の選択肢は置かない
  // (全削除は別セクションのリセットボタンで行うため、誤操作を避ける)。
  fillDeleteMemberOptions() {
    const sel = qs("#settingsDeleteMember");
    const prev = sel.value;
    sel.innerHTML = "";
    for (const t of Sidebar.threads) {
      const opt = ce("option");
      opt.value = t.member;
      opt.textContent = t.member;
      sel.appendChild(opt);
    }
    const preferred = prev || Sidebar.selectedMember || "";
    if ([...sel.options].some((o) => o.value === preferred)) sel.value = preferred;
  },

  close() {
    if (!this.configured) {
      toast("先にメッセージ保存フォルダを設定してください", true);
      return;
    }
    qs("#settingsModal").classList.add("hidden");
  },

  async save() {
    const input = qs("#settingsRootInput");
    const root = input.value.trim();
    const errEl = qs("#settingsError");
    errEl.classList.add("hidden");
    if (!root) {
      errEl.textContent = "フォルダパスを入力してください";
      errEl.classList.remove("hidden");
      return;
    }
    const btn = qs("#settingsSaveBtn");
    btn.disabled = true;
    try {
      const res = await fetch("/api/settings", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ root }),
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(data.detail || `HTTP ${res.status}`);
      }
      this.configured = true;
      qs("#settingsModal").classList.add("hidden");
      toast("保存先フォルダを設定しました。取り込みを開始します。");
      Reindex.showRunning({ phase: "scanning", message: "取り込みを開始しています..." });
      Reindex.startActivePolling();
      Reindex.poll();
      await Sidebar.load();
    } catch (e) {
      console.error(e);
      errEl.textContent = e.message;
      errEl.classList.remove("hidden");
    } finally {
      btn.disabled = false;
    }
  },

  async startFullReindex() {
    const errEl = qs("#settingsReindexError");
    errEl.classList.add("hidden");

    const member = qs("#settingsReindexMember").value;
    const target = member || "全メンバー";
    const ok = window.confirm(
      `${target}の全期間を取り込み直します。件数によっては時間がかかります。実行しますか？`
    );
    if (!ok) return;

    const btn = qs("#settingsReindexBtn");
    btn.disabled = true;
    try {
      const params = new URLSearchParams({ mode: "full" });
      if (member) params.set("member", member);
      const res = await fetch(`/api/reindex?${params}`, { method: "POST" });
      const data = await res.json().catch(() => ({}));
      if (res.status === 409) {
        throw new Error("すでに取り込みが実行中です");
      }
      if (!res.ok) throw new Error(data.detail || `HTTP ${res.status}`);
      this.close();
      toast(`${target}の全期間取り込みを開始しました`);
      Reindex.showRunning({ phase: "scanning", message: "取り込みを開始しています..." });
      Reindex.startActivePolling();
      Reindex.poll();
    } catch (e) {
      console.error(e);
      errEl.textContent = e.message;
      errEl.classList.remove("hidden");
    } finally {
      btn.disabled = false;
    }
  },

  async deleteMember() {
    const errEl = qs("#settingsDeleteMemberError");
    errEl.classList.add("hidden");

    const member = qs("#settingsDeleteMember").value;
    if (!member) {
      errEl.textContent = "削除するメンバーを選択してください";
      errEl.classList.remove("hidden");
      return;
    }

    const ok = window.confirm(
      `${member} のメッセージ・サムネイル・お気に入り・コメントをDBから削除します。この操作は元に戻せません。実行しますか？`
    );
    if (!ok) return;

    const btn = qs("#settingsDeleteMemberBtn");
    btn.disabled = true;
    try {
      const res = await fetch(`/api/members/${encodeURIComponent(member)}`, { method: "DELETE" });
      const data = await res.json().catch(() => ({}));
      if (res.status === 409) {
        throw new Error("取り込み中は削除できません。完了後にもう一度お試しください");
      }
      if (!res.ok) throw new Error(data.detail || `HTTP ${res.status}`);

      toast(`${member} を削除しました`);
      this.close();

      // 削除したメンバーを表示中だった場合はタイムラインを未選択状態に戻す
      const wasSelected = Sidebar.selectedMember === member;
      await Sidebar.load();
      this.fillMemberOptions();
      this.fillDeleteMemberOptions();
      if (wasSelected) {
        Sidebar.selectedMember = null;
        Timeline.member = null;
        Timeline.el.innerHTML = "";
        Timeline.el.classList.add("hidden");
        qs("#emptyState").classList.remove("hidden");
      }
    } catch (e) {
      console.error(e);
      errEl.textContent = e.message;
      errEl.classList.remove("hidden");
    } finally {
      btn.disabled = false;
    }
  },

  async reset() {
    const errEl = qs("#settingsResetError");
    errEl.classList.add("hidden");

    const ok = window.confirm(
      "DB・サムネイル・保存先フォルダ設定をすべて削除します。この操作は元に戻せません。実行しますか？"
    );
    if (!ok) return;

    const btn = qs("#settingsResetBtn");
    btn.disabled = true;
    try {
      const res = await fetch("/api/reset", { method: "POST" });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(data.detail || `HTTP ${res.status}`);
      }
      toast("初期化しました。");
      setTimeout(() => location.reload(), 600);
    } catch (e) {
      console.error(e);
      errEl.textContent = e.message;
      errEl.classList.remove("hidden");
    } finally {
      btn.disabled = false;
    }
  },
};
