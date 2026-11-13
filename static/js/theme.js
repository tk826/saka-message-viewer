"use strict";
/* ==========================================================================
   テーマ
   ========================================================================== */
import { qs } from "./utils.js";

export const Theme = {
  KEY: "msgviewer_theme",
  init() {
    const saved = localStorage.getItem(this.KEY);
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
      localStorage.setItem(this.KEY, value);
    } else {
      document.documentElement.removeAttribute("data-theme");
      localStorage.removeItem(this.KEY);
    }
  },
};
