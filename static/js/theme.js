"use strict";
/* ==========================================================================
   テーマ
   ========================================================================== */
import { qs, safeStorage } from "./utils.js";

export const Theme = {
  KEY: "msgviewer_theme",
  init() {
    // サイトのデータ保存を禁止したブラウザでは localStorage が例外を投げる。
    // ここで止まると App.init 全体が止まり、アプリが起動しなくなるため safeStorage を使う
    const saved = safeStorage.get(this.KEY);
    if (saved === "light" || saved === "dark") {
      document.documentElement.setAttribute("data-theme", saved);
    }
    const select = qs("#settingsTheme");
    select.value = saved === "light" || saved === "dark" ? saved : "";
    select.addEventListener("change", () => this.set(select.value));
  },
  current() {
    const attr = document.documentElement.getAttribute("data-theme");
    if (attr) return attr;
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  },
  set(value) {
    if (value === "light" || value === "dark") {
      document.documentElement.setAttribute("data-theme", value);
      safeStorage.set(this.KEY, value);
    } else {
      document.documentElement.removeAttribute("data-theme");
      safeStorage.remove(this.KEY);
    }
  },
};
