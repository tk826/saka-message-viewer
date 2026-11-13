"use strict";
/* ==========================================================================
   検索
   ========================================================================== */
import { qs, ce, escapeHtml, fetchJson, toast, formatLastTs, formatTime, sanitizeSnippet } from "./utils.js";
import { applyNicknamePlaceholderHtml } from "./display-prefs.js";
import { Sidebar } from "./sidebar.js";

export const Search = {
  PAGE_SIZE: 100,

  // 直近の検索条件。「さらに読み込む」と、ジャンプ先での語ハイライトに使う。
  lastQuery: null,       // 検索文字列そのもの
  lastTerms: [],         // サーバが解析した「含める語」(ハイライト対象)
  _params: null,         // 追加読み込み用に組み立て済みのクエリ
  _nextBefore: null,
  _nextBeforeRowid: null,  // 同じtsのメッセージを取りこぼさないためのカーソルの第2キー
  _loadingMore: false,
  _shown: 0,
  _total: 0,

  init() {
    qs("#searchBtn").addEventListener("click", () => this.run());
    qs("#searchInput").addEventListener("keydown", (e) => {
      if (e.key === "Enter") this.run();
    });
    qs(".search-close").addEventListener("click", () => this.close());
  },

  async run() {
    const q = qs("#searchInput").value.trim();
    if (!q) return;
    const scopeAll = qs("#searchScopeAll").checked;
    const member = scopeAll ? null : Sidebar.selectedMember;
    if (!scopeAll && !member) {
      toast("検索対象のメンバーが選択されていません。先にメンバーを選択するか「全メンバー」にチェックしてください。", true);
      return;
    }
    const from = qs("#searchFrom").value;
    const to = qs("#searchTo").value;

    const params = new URLSearchParams();
    params.set("q", q);
    if (member) params.set("member", member);
    if (from) params.set("from", from);
    if (to) params.set("to", to);
    params.set("limit", String(this.PAGE_SIZE));

    this.lastQuery = q;
    this._params = params;
    this._nextBefore = null;
    this._nextBeforeRowid = null;
    this._shown = 0;
    this._total = 0;

    const panel = qs("#searchPanel");
    const body = qs("#searchResultBody");
    panel.classList.remove("hidden");
    qs("#calendarPanel").classList.add("hidden");
    qs("#favoritesPanel").classList.add("hidden");
    qs("#galleryPanel").classList.add("hidden");
    body.innerHTML = `<div class="hint">検索中...</div>`;

    try {
      const data = await fetchJson(`/api/search?${params.toString()}`);
      body.innerHTML = "";
      this.lastTerms = (data.terms && data.terms.include) || [];
      this._total = data.total ?? data.results.length;

      if (data.results.length === 0) {
        this.updateTitle();
        body.innerHTML = `<div class="hint">該当する結果がありません</div>`;
        return;
      }
      this.appendResults(data);
    } catch (e) {
      console.error(e);
      body.innerHTML = `<div class="hint">検索に失敗しました: ${escapeHtml(e.message)}</div>`;
    }
  },

  // 1ページ分の結果を末尾に足し、必要なら「さらに読み込む」ボタンを置き直す。
  appendResults(data) {
    const body = qs("#searchResultBody");
    qs("#searchMoreBtn")?.parentElement?.remove();
    for (const r of data.results) {
      body.appendChild(this.buildResultItem(r));
    }
    this._shown += data.results.length;
    this._nextBefore = data.next_before;
    this._nextBeforeRowid = data.next_before_rowid ?? null;
    this.updateTitle();

    if (data.has_more) {
      const wrap = ce("div", "search-more");
      const btn = ce("button", "btn");
      btn.id = "searchMoreBtn";
      btn.textContent = `さらに読み込む (残り${Math.max(this._total - this._shown, 0)}件)`;
      btn.addEventListener("click", () => this.loadMore());
      wrap.appendChild(btn);
      body.appendChild(wrap);
    }
  },

  updateTitle() {
    const shown = this._shown;
    const total = this._total;
    const countText = total > shown ? `${shown}/${total}件` : `${total}件`;
    qs("#searchResultTitle").textContent = `検索結果 (${countText})`;
  },

  async loadMore() {
    if (this._loadingMore || this._nextBefore == null || !this._params) return;
    this._loadingMore = true;
    const btn = qs("#searchMoreBtn");
    if (btn) { btn.disabled = true; btn.textContent = "読み込み中..."; }
    try {
      const params = new URLSearchParams(this._params);
      params.set("before", String(this._nextBefore));
      if (this._nextBeforeRowid != null) params.set("before_rowid", String(this._nextBeforeRowid));
      // total は初回のみ返る(続きのページでは null)。初回の値をそのまま使う
      const data = await fetchJson(`/api/search?${params.toString()}`);
      this.appendResults(data);
    } catch (e) {
      console.error(e);
      toast("追加の読み込みに失敗しました: " + e.message, true);
      if (btn) { btn.disabled = false; btn.textContent = "さらに読み込む"; }
    } finally {
      this._loadingMore = false;
    }
  },

  buildResultItem(r) {
    const item = ce("div", "search-result-item");
    const meta = ce("div", "sr-meta");
    meta.innerHTML = `<b>${escapeHtml(r.member)}</b><span>${escapeHtml(formatLastTs(r.ts))} ${formatTime(r.ts)}</span>`;
    const snippet = ce("div", "sr-snippet");
    // snippet はサーバの make_snippet() で <mark> 以外をエスケープ済み。
    // ニックネームはエスケープしてから差し込み、念のため sanitizeSnippet で検証する。
    snippet.innerHTML = sanitizeSnippet(applyNicknamePlaceholderHtml(r.snippet || ""));
    item.appendChild(meta);
    item.appendChild(snippet);
    item.addEventListener("click", async () => {
      this.close();
      const { App } = await import("./app.js");
      // ジャンプ先のタイムラインでも検索語を強調したまま見せる
      App.jumpToMessage(r.member, r.ts, r.msg_id, this.lastTerms);
    });
    return item;
  },

  close() {
    qs("#searchPanel").classList.add("hidden");
  },
};
