"use strict";
/* ==========================================================================
   お気に入り一覧パネル
   ========================================================================== */
import { qs, ce, escapeHtml, fetchJson, toast, formatLastTs, formatTime } from "./utils.js";
import { applyNicknamePlaceholder } from "./display-prefs.js";
import { Sidebar } from "./sidebar.js";
import { Favorites } from "./favorites.js";
import { Timeline } from "./timeline.js";
import { buildCommentEditor } from "./comments.js";

export const FavoritesPanel = {
  loading: false,
  hasMore: true,
  oldestCreatedAt: null,
  oldestRowid: null,  // oldestCreatedAt と組で続きの目印にする(同じ秒に登録したものの取りこぼし防止)
  PAGE_SIZE: 50,

  init() {
    qs("#favoritesBtn").addEventListener("click", () => this.toggle());
    qs(".favorites-close").addEventListener("click", () => this.close());
    qs("#favoritesBody").addEventListener("scroll", () => this.onScroll());
  },

  toggle() {
    const panel = qs("#favoritesPanel");
    if (panel.classList.contains("hidden")) {
      this.open();
    } else {
      this.close();
    }
  },

  async open() {
    // ギャラリーと同様、選択中メンバーが無いと何を表示すべきか決まらないため開かない
    if (!Sidebar.selectedMember) {
      toast("先にメンバーを選択してください", true);
      return;
    }
    // 他パネルと同時に開かない
    qs("#searchPanel").classList.add("hidden");
    qs("#calendarPanel").classList.add("hidden");
    qs("#galleryPanel").classList.add("hidden");
    qs("#favoritesPanel").classList.remove("hidden");
    qs("#favoritesTitle").textContent = `お気に入り — ${Sidebar.selectedMember}`;
    await this.reload();
  },

  close() {
    qs("#favoritesPanel").classList.add("hidden");
  },

  async reload() {
    this.hasMore = true;
    this.oldestCreatedAt = null;
    this.oldestRowid = null;
    const body = qs("#favoritesBody");
    body.innerHTML = `<div class="hint">読み込み中...</div>`;
    try {
      const data = await fetchJson(`/api/favorites?limit=${this.PAGE_SIZE}&member=${encodeURIComponent(Sidebar.selectedMember)}`);
      body.innerHTML = "";
      const favs = data.favorites || [];
      if (favs.length === 0) {
        body.innerHTML = `<div class="hint">このメンバーのお気に入りはまだありません</div>`;
        this.hasMore = false;
        return;
      }
      for (const f of favs) body.appendChild(this.buildItem(f));
      this.oldestCreatedAt = data.next_before;
      this.oldestRowid = data.next_before_rowid;
      if (favs.length < this.PAGE_SIZE) this.hasMore = false;
    } catch (e) {
      console.error(e);
      body.innerHTML = `<div class="hint">読み込みに失敗しました: ${escapeHtml(e.message)}</div>`;
    }
  },

  async loadMore() {
    if (this.loading || !this.hasMore || this.oldestCreatedAt == null) return;
    this.loading = true;
    try {
      const data = await fetchJson(`/api/favorites?limit=${this.PAGE_SIZE}&before=${this.oldestCreatedAt}&before_rowid=${this.oldestRowid}&member=${encodeURIComponent(Sidebar.selectedMember)}`);
      const favs = data.favorites || [];
      if (favs.length === 0) {
        this.hasMore = false;
        return;
      }
      const body = qs("#favoritesBody");
      for (const f of favs) body.appendChild(this.buildItem(f));
      this.oldestCreatedAt = data.next_before;
      this.oldestRowid = data.next_before_rowid;
      if (favs.length < this.PAGE_SIZE) this.hasMore = false;
    } catch (e) {
      console.error(e);
    } finally {
      this.loading = false;
    }
  },

  onScroll() {
    const body = qs("#favoritesBody");
    if (body.scrollHeight - body.scrollTop - body.clientHeight < 200) {
      this.loadMore();
    }
  },

  buildItem(f) {
    const item = ce("div", "favorite-item");

    const thumbSrc = f.thumb_url || f.media_url;
    if (thumbSrc) {
      const img = ce("img", "favorite-thumb");
      img.loading = "lazy";
      img.src = thumbSrc;
      img.alt = "";
      item.appendChild(img);
    }

    const main = ce("div", "favorite-main");
    const meta = ce("div", "fav-meta");
    const memberEl = ce("b");
    memberEl.textContent = f.member;
    const timeEl = ce("span");
    timeEl.textContent = `${formatLastTs(f.ts)} ${formatTime(f.ts)}`;
    meta.appendChild(memberEl);
    meta.appendChild(timeEl);

    const snippet = ce("div", "fav-snippet");
    snippet.textContent = f.body ? applyNicknamePlaceholder(f.body) : (f.kind !== "text" ? `[${f.kind}]` : "");

    main.appendChild(meta);
    main.appendChild(snippet);
    main.appendChild(buildCommentEditor(f));
    item.appendChild(main);

    const unfavBtn = ce("button", "favorite-unfav-btn");
    unfavBtn.type = "button";
    unfavBtn.title = "お気に入り解除";
    unfavBtn.textContent = "★";
    unfavBtn.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        await fetchJson(`/api/favorites?member=${encodeURIComponent(f.member)}&msg_id=${f.msg_id}`, { method: "DELETE" });
        Favorites.ids.delete(Number(f.msg_id));
        item.remove();
        // 現在表示中タイムラインに同じメッセージがあれば★表示も同期する
        const row = Timeline.el.querySelector(`.msg-row[data-msg-id="${CSS.escape(String(f.msg_id))}"]`);
        if (row) {
          const btn = row.querySelector(".fav-btn");
          if (btn) Favorites.applyBtnState(btn, false);
          const actions = row.querySelector(".msg-row-actions");
          if (actions) actions.classList.remove("has-favorite");
        }
      } catch (e2) {
        console.error(e2);
        toast("お気に入り解除に失敗しました: " + e2.message, true);
      }
    });
    item.appendChild(unfavBtn);

    item.addEventListener("click", async () => {
      this.close();
      const { App } = await import("./app.js");
      App.jumpToMessage(f.member, f.ts, f.msg_id);
    });

    return item;
  },
};
