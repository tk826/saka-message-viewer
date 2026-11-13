"use strict";
/* ==========================================================================
   メディア遅延ロード (IntersectionObserver)
   ========================================================================== */
import { qs, ce } from "./utils.js";
import { Timeline } from "./timeline.js";

export const LazyMedia = {
  observer: null,
  init() {
    this.observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          this.load(entry.target);
          this.observer.unobserve(entry.target);
        }
      }
    }, { root: qs("#timeline"), rootMargin: "300px 0px" });
  },
  observe(el) {
    this.observer.observe(el);
  },
  load(el) {
    const src = el.getAttribute("data-src");
    if (!src) return;
    if (el.tagName === "IMG") {
      el.addEventListener("load", () => Timeline.stickToBottomIfNeeded(), { once: true });
      el.src = src;
    } else if (el.tagName === "VIDEO") {
      const source = ce("source");
      source.src = src;
      source.type = el.getAttribute("data-type") || "video/mp4";
      el.appendChild(source);
      // preload="none"のままsourceを追加しただけでは、ブラウザが自動的に
      // メタデータ取得を始めないことがある(再生ボタンを押すまで
      // loadedmetadataが発火せず、それまでposterのサイズのまま表示され
      // 続けてしまう)。load()を明示的に呼び、メタデータだけは
      // 先読みさせる(実データの本格ダウンロードはネットワーク帯域と
      // ブラウザの実装次第だが、メタデータ取得自体は軽量)。
      el.load();
    }
    el.removeAttribute("data-src");
  },
};
