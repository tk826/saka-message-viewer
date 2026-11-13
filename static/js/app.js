"use strict";
/* ==========================================================================
   坂道メッセージビューア フロントエンド (バニラJS / 外部依存なし, ES Modules)
   アプリ本体
   ========================================================================== */
import { qs, fetchJson, toast } from "./utils.js";
import { Theme } from "./theme.js";
import { DisplayPrefs } from "./display-prefs.js";
import { LazyMedia } from "./lazy-media.js";
import { Timeline } from "./timeline.js";
import { Sidebar } from "./sidebar.js";
import { Search } from "./search.js";
import { CalendarPanel } from "./calendar.js";
import { FavoritesPanel } from "./favorites-panel.js";
import { GalleryPanel } from "./gallery.js";
import { Settings } from "./settings.js";
import { Reindex } from "./reindex.js";
import { Favorites } from "./favorites.js";
import { closeLightbox } from "./lightbox.js";

export const App = {
  async init() {
    Theme.init();
    DisplayPrefs.init();
    LazyMedia.init();
    Timeline.init();
    Search.init();
    CalendarPanel.init();
    FavoritesPanel.init();
    GalleryPanel.init();
    Settings.init();
    Reindex.init();
    Sidebar.init();

    qs("#lightboxClose").addEventListener("click", closeLightbox);
    qs("#lightbox").addEventListener("click", (e) => {
      if (e.target.id === "lightbox") closeLightbox();
    });
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        closeLightbox();
        Search.close();
        CalendarPanel.close();
        FavoritesPanel.close();
        GalleryPanel.close();
      }
    });

    await Settings.checkOnStartup();
    // 非表示メンバー設定と並び順はSidebar.load()のrender()で参照するため、先に読む
    await Promise.all([Sidebar.loadHiddenMembers(), Sidebar.loadMemberOrder()]);
    await Sidebar.load();
  },

  async selectMember(member) {
    Sidebar.setSelected(member);
    Timeline.setHighlightTerms([]);
    Search.close();
    CalendarPanel.close();
    FavoritesPanel.close();
    GalleryPanel.close();
    await Favorites.loadForMember(member);
    await Timeline.loadInitial(member, null);
  },

  async jumpToDate(member, dateStr) {
    try {
      const data = await fetchJson(`/api/jump?member=${encodeURIComponent(member)}&date=${dateStr}`);
      if (data.ts == null) {
        toast("その日付付近にメッセージが見つかりませんでした", true);
        return;
      }
      Sidebar.setSelected(member);
      if (member !== Favorites._loadedMember) await Favorites.loadForMember(member);
      Timeline.setHighlightTerms([]);
      await Timeline.loadInitial(member, data.ts);
    } catch (e) {
      console.error(e);
      toast("ジャンプに失敗しました: " + e.message, true);
    }
  },

  // highlightTerms: 検索結果から飛んできた場合の強調表示対象の語
  async jumpToMessage(member, ts, msgId = null, highlightTerms = []) {
    Sidebar.setSelected(member);
    if (member !== Favorites._loadedMember) await Favorites.loadForMember(member);
    Timeline.setHighlightTerms(highlightTerms);
    await Timeline.loadInitial(member, ts, msgId);
  },
};

window.addEventListener("DOMContentLoaded", () => App.init());
