"use strict";
/* ==========================================================================
   お気に入り
   ========================================================================== */
import { fetchJson, toast } from "./utils.js";

export const Favorites = {
  ids: new Set(),        // 現在選択中メンバーの msg_id 集合 (★の初期状態反映用)
  _loadedMember: null,   // ids がどのメンバー分か (同一メンバーへの再ジャンプで再取得を省くため)

  async loadForMember(member) {
    this.ids = new Set();
    this._loadedMember = member || null;
    if (!member) return;
    try {
      const data = await fetchJson(`/api/favorites/ids?member=${encodeURIComponent(member)}`);
      this.ids = new Set(data.msg_ids || []);
    } catch (e) {
      console.error(e);
    }
  },

  has(msgId) {
    return this.ids.has(Number(msgId));
  },

  async toggle(member, msgId, btnEl) {
    const id = Number(msgId);
    const nowFav = !this.has(id);
    try {
      if (nowFav) {
        await fetchJson("/api/favorites", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ member, msg_id: id }),
        });
        this.ids.add(id);
      } else {
        await fetchJson(`/api/favorites?member=${encodeURIComponent(member)}&msg_id=${id}`, { method: "DELETE" });
        this.ids.delete(id);
      }
      if (btnEl) this.applyBtnState(btnEl, nowFav);
    } catch (e) {
      console.error(e);
      toast("お気に入りの更新に失敗しました: " + e.message, true);
    }
  },

  applyBtnState(btnEl, isFav) {
    btnEl.classList.toggle("is-favorited", isFav);
    btnEl.textContent = isFav ? "★" : "☆"; // ★ / ☆
    btnEl.title = isFav ? "お気に入り解除" : "お気に入りに追加";
  },
};
