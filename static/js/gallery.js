"use strict";
/* ==========================================================================
   メディアギャラリーパネル
   ========================================================================== */
import { qs, ce, escapeHtml, fetchJson, toast, formatLastTs, formatTime, isDevMode } from "./utils.js";
import { applyNicknamePlaceholder } from "./display-prefs.js";
import { Sidebar } from "./sidebar.js";
import { openLightbox } from "./lightbox.js";

export const GalleryPanel = {
  loading: false,
  hasMore: true,
  oldestTs: null,
  oldestId: null,  // oldestTs と組で続きの目印にする(同じ秒のメディアの取りこぼし防止)
  kind: "",   // "" = すべて
  from: "",   // "" = 下限なし (YYYY-MM-DD)
  to: "",     // "" = 上限なし (YYYY-MM-DD)
  PAGE_SIZE: 60,

  init() {
    qs("#galleryBtn").addEventListener("click", () => this.toggle());
    qs(".gallery-close").addEventListener("click", () => this.close());
    qs("#galleryBody").addEventListener("scroll", () => this.onScroll());
    for (const btn of document.querySelectorAll(".gallery-filter-btn")) {
      btn.addEventListener("click", () => this.setKind(btn.dataset.kind, btn));
    }
    qs("#galleryFrom").addEventListener("change", () => this.setDateRange());
    qs("#galleryTo").addEventListener("change", () => this.setDateRange());
    qs("#galleryDateClearBtn").addEventListener("click", () => this.clearDateRange());
  },

  toggle() {
    const panel = qs("#galleryPanel");
    if (panel.classList.contains("hidden")) {
      this.open();
    } else {
      this.close();
    }
  },

  async open() {
    if (!Sidebar.selectedMember) {
      toast("先にメンバーを選択してください", true);
      return;
    }
    qs("#searchPanel").classList.add("hidden");
    qs("#calendarPanel").classList.add("hidden");
    qs("#favoritesPanel").classList.add("hidden");
    qs("#galleryPanel").classList.remove("hidden");
    await this.reload();
  },

  close() {
    qs("#galleryPanel").classList.add("hidden");
  },

  setKind(kind, btnEl) {
    if (this.kind === kind) return;
    this.kind = kind;
    document.querySelectorAll(".gallery-filter-btn").forEach((b) => b.classList.toggle("active", b === btnEl));
    this.reload();
  },

  setDateRange() {
    const from = qs("#galleryFrom").value;
    const to = qs("#galleryTo").value;
    if (from === this.from && to === this.to) return;
    this.from = from;
    this.to = to;
    qs("#galleryDateClearBtn").classList.toggle("hidden", !from && !to);
    this.reload();
  },

  clearDateRange() {
    if (!this.from && !this.to) return;
    qs("#galleryFrom").value = "";
    qs("#galleryTo").value = "";
    this.from = "";
    this.to = "";
    qs("#galleryDateClearBtn").classList.add("hidden");
    this.reload();
  },

  async reload() {
    this.hasMore = true;
    this.oldestTs = null;
    this.oldestId = null;
    const grid = qs("#galleryGrid");
    grid.innerHTML = `<div class="hint">読み込み中...</div>`;
    qs("#galleryLoadSentinel").classList.add("hidden");
    try {
      const items = await this.fetchPage();
      grid.innerHTML = "";
      if (items.length === 0) {
        grid.innerHTML = `<div class="hint">メディアがありません</div>`;
      } else {
        for (const item of items) grid.appendChild(this.buildItem(item));
      }
    } catch (e) {
      console.error(e);
      grid.innerHTML = `<div class="hint">読み込みに失敗しました: ${escapeHtml(e.message)}</div>`;
    }
  },

  async fetchPage() {
    const params = new URLSearchParams();
    params.set("member", Sidebar.selectedMember);
    if (this.kind) params.set("kind", this.kind);
    if (this.from) params.set("from", this.from);
    if (this.to) params.set("to", this.to);
    params.set("limit", String(this.PAGE_SIZE));
    if (this.oldestTs != null) {
      params.set("before", String(this.oldestTs));
      params.set("before_id", String(this.oldestId));
    }
    const data = await fetchJson(`/api/media-list?${params.toString()}`);
    const items = data.items || [];
    this.hasMore = !!data.has_more;
    if (items.length > 0) {
      this.oldestTs = items[items.length - 1].ts;
      this.oldestId = items[items.length - 1].msg_id;
    }
    return items;
  },

  async loadMore() {
    if (this.loading || !this.hasMore) return;
    this.loading = true;
    const sentinel = qs("#galleryLoadSentinel");
    sentinel.textContent = "読み込み中...";
    sentinel.classList.remove("hidden");
    try {
      const items = await this.fetchPage();
      const grid = qs("#galleryGrid");
      for (const item of items) grid.appendChild(this.buildItem(item));
    } catch (e) {
      console.error(e);
    } finally {
      this.loading = false;
      sentinel.classList.add("hidden");
    }
  },

  onScroll() {
    const body = qs("#galleryBody");
    if (body.scrollHeight - body.scrollTop - body.clientHeight < 200) {
      this.loadMore();
    }
  },

  buildItem(msg) {
    const cell = ce("div", "gallery-item");
    const media = ce("div", "gallery-item-media");

    // サムネイルが無い(動画/音声はindexerがサムネを作らない)場合、<img>にmedia_urlをそのまま
    // 突っ込むと何も表示されない空タイルになってしまうため、種別に応じたプレースホルダを出す。
    const thumbSrc = msg.thumb_url || (msg.kind === "image" ? msg.media_url : null);
    const hasThumb = !!thumbSrc;
    if (thumbSrc) {
      const img = ce("img");
      img.loading = "lazy";
      img.src = thumbSrc;
      img.alt = "";
      media.appendChild(img);
    } else if (msg.kind === "video" || msg.kind === "audio") {
      const placeholder = ce("div", "gallery-item-placeholder");
      const icon = ce("div", "gallery-placeholder-icon");
      icon.textContent = msg.kind === "video" ? "▶" : "🎵";
      const label = ce("div", "gallery-placeholder-label");
      label.textContent = msg.kind === "video" ? "動画" : "音声";
      placeholder.appendChild(icon);
      placeholder.appendChild(label);
      media.appendChild(placeholder);
    }

    // プレースホルダに既に種別ラベルがあるため、バッジは重複させない
    // (サムネイルがある場合のみ、サムネ上に種別を示すバッジを重ねる)
    if (hasThumb && (msg.kind === "video" || msg.kind === "audio")) {
      const badge = ce("div", "gallery-kind-badge");
      badge.textContent = msg.kind === "video" ? "動画" : "音声";
      media.appendChild(badge);
    }

    const jumpBtn = ce("button", "gallery-jump-btn");
    jumpBtn.type = "button";
    jumpBtn.title = "このメッセージへジャンプ";
    jumpBtn.textContent = "→";
    jumpBtn.addEventListener("click", async (e) => {
      e.stopPropagation();
      this.close();
      const { App } = await import("./app.js");
      App.jumpToMessage(msg.member, msg.ts, msg.msg_id);
    });
    media.appendChild(jumpBtn);

    cell.appendChild(media);

    const meta = ce("div", "gallery-item-meta");
    const timeEl = ce("div", "gallery-item-time");
    timeEl.textContent = `${formatLastTs(msg.ts)} ${formatTime(msg.ts)}`;
    meta.appendChild(timeEl);
    if (msg.body) {
      const snippet = ce("div", "gallery-item-snippet");
      snippet.textContent = applyNicknamePlaceholder(msg.body);
      meta.appendChild(snippet);
    }
    cell.appendChild(meta);

    // クリック挙動: 画像はライトボックスで開く。動画/音声・ライトボックス非対応時は
    // タイムラインへジャンプする。どの場合も右下の小さな「→」ボタンから
    // 必ずタイムラインへジャンプできるようにしておく。
    cell.addEventListener("click", async () => {
      if (msg.kind === "image") {
        openLightbox(msg);
      } else {
        if (isDevMode()) {
          console.info("[GalleryPanel] clicked item", { msg_id: msg.msg_id, ts: msg.ts, kind: msg.kind, member: msg.member });
        }
        this.close();
        const { App } = await import("./app.js");
        App.jumpToMessage(msg.member, msg.ts, msg.msg_id);
      }
    });

    return cell;
  },
};
