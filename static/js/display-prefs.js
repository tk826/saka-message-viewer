"use strict";
/* ==========================================================================
   表示設定 (フォント・文字サイズ。ブラウザのlocalStorageに保存)
   ========================================================================== */
import { qs, escapeHtml } from "./utils.js";
import { VolumeBoost } from "./volume-boost.js";
import { Timeline } from "./timeline.js";

export const DisplayPrefs = {
  STORAGE_KEY: "msgviewer_display_prefs",
  DEFAULT_FONT_SIZE: 100,
  DEFAULT_NICKNAME: "ユーザー",

  load() {
    let prefs = {
      fontFamily: "",
      fontSize: this.DEFAULT_FONT_SIZE,
      nickname: this.DEFAULT_NICKNAME,
      volumeBoostEnabled: false,
    };
    try {
      const raw = localStorage.getItem(this.STORAGE_KEY);
      if (raw) prefs = { ...prefs, ...JSON.parse(raw) };
      if (!prefs.nickname) prefs.nickname = this.DEFAULT_NICKNAME;
    } catch (e) {
      console.error(e);
    }
    return prefs;
  },

  save(prefs) {
    try {
      localStorage.setItem(this.STORAGE_KEY, JSON.stringify(prefs));
    } catch (e) {
      console.error(e);
    }
  },

  apply(prefs) {
    const root = document.documentElement;
    if (prefs.fontFamily) {
      root.style.setProperty("--font-family", prefs.fontFamily);
    } else {
      root.style.removeProperty("--font-family");
    }
    root.style.fontSize = `${prefs.fontSize}%`;
    VolumeBoost.applyToAll(prefs.volumeBoostEnabled);
  },

  init() {
    const prefs = this.load();
    this.apply(prefs);

    const nicknameInput = qs("#settingsNickname");
    const familySelect = qs("#settingsFontFamily");
    const sizeInput = qs("#settingsFontSize");
    const sizeValue = qs("#settingsFontSizeValue");
    const volumeBoostCheckbox = qs("#settingsVolumeBoost");

    nicknameInput.value = prefs.nickname;
    familySelect.value = prefs.fontFamily || "";
    sizeInput.value = prefs.fontSize;
    sizeValue.textContent = `${prefs.fontSize}%`;
    volumeBoostCheckbox.checked = prefs.volumeBoostEnabled;

    nicknameInput.addEventListener("input", () => {
      const current = this.load();
      current.nickname = nicknameInput.value.trim() || this.DEFAULT_NICKNAME;
      this.save(current);
      Timeline.reapplyNicknamePlaceholder();
    });

    familySelect.addEventListener("change", () => {
      const current = this.load();
      current.fontFamily = familySelect.value;
      this.save(current);
      this.apply(current);
    });

    volumeBoostCheckbox.addEventListener("change", () => {
      const current = this.load();
      current.volumeBoostEnabled = volumeBoostCheckbox.checked;
      this.save(current);
      this.apply(current);
    });

    sizeInput.addEventListener("input", () => {
      sizeValue.textContent = `${sizeInput.value}%`;
      const current = this.load();
      current.fontSize = Number(sizeInput.value);
      this.save(current);
      this.apply(current);
    });
  },
};

export function applyNicknamePlaceholder(s) {
  if (s == null) return s;
  const nickname = DisplayPrefs.load().nickname;
  return s.replaceAll("%%%", nickname).replaceAll("％％％", nickname);
}

// HTMLエスケープ済みの文字列(検索スニペット等)用。ニックネーム側もエスケープして差し込む。
export function applyNicknamePlaceholderHtml(s) {
  if (s == null) return s;
  const nickname = escapeHtml(DisplayPrefs.load().nickname);
  return s.replaceAll("%%%", nickname).replaceAll("％％％", nickname);
}
