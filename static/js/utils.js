"use strict";
/* ==========================================================================
   共通ユーティリティ
   ========================================================================== */

export function qs(sel, root) { return (root || document).querySelector(sel); }
export function ce(tag, cls) {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  return el;
}
export function escapeHtml(s) {
  if (s == null) return "";
  return s.replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

// サーバの make_snippet() は <mark> 以外をエスケープ済みの文字列を返す。
// ここで再度escapeHtmlすると &lt; が &amp;lt; になって本文に化けるため、
// 素通しせず「<mark>と</mark>以外のタグが混ざっていないか」だけを検証し、
// 想定外のタグがあった場合のみ安全側に倒して全体をエスケープする。
export function sanitizeSnippet(raw) {
  const withoutMarks = raw.split("<mark>").join("").split("</mark>").join("");
  if (withoutMarks.includes("<") || withoutMarks.includes(">")) {
    // 想定外の生タグ。<mark>ごとエスケープして表示する(強調は諦める)。
    return escapeHtml(raw);
  }
  return raw;
}

// 本文中の検索語を <mark> で囲んで強調する。innerHTMLを使わずDOMノードを
// 組み立てるため、本文にHTMLが含まれていても注入にならない。
// terms: 強調したい文字列の配列(除外語は渡さないこと)。
export function highlightTerms(container, text, terms) {
  container.textContent = "";
  const valid = (terms || []).filter((t) => t && t.length > 0);
  if (valid.length === 0) {
    container.textContent = text;
    return;
  }
  // 長い語を先に見て、短い語が長い語の一部を先取りしないようにする
  const sorted = [...valid].sort((a, b) => b.length - a.length);
  const lower = text.toLowerCase();
  const lowerTerms = sorted.map((t) => t.toLowerCase());

  let i = 0;
  let plain = "";
  const flushPlain = () => {
    if (plain) {
      container.appendChild(document.createTextNode(plain));
      plain = "";
    }
  };
  while (i < text.length) {
    let matched = -1;
    for (let k = 0; k < lowerTerms.length; k++) {
      if (lower.startsWith(lowerTerms[k], i)) { matched = k; break; }
    }
    if (matched >= 0) {
      flushPlain();
      const len = sorted[matched].length;
      const mark = document.createElement("mark");
      mark.textContent = text.slice(i, i + len);
      container.appendChild(mark);
      i += len;
    } else {
      plain += text[i];
      i++;
    }
  }
  flushPlain();
}

export function jstDateParts(ts) {
  // ts は unix epoch 秒 (UTC)。JST = UTC+9。
  const d = new Date((ts + 9 * 3600) * 1000);
  return {
    y: d.getUTCFullYear(),
    m: d.getUTCMonth() + 1,
    day: d.getUTCDate(),
    hh: d.getUTCHours(),
    mm: d.getUTCMinutes(),
    dow: d.getUTCDay(),
  };
}

export function formatDateSep(ts) {
  const p = jstDateParts(ts);
  const dowNames = ["日", "月", "火", "水", "木", "金", "土"];
  return `${p.y}年${p.m}月${p.day}日 (${dowNames[p.dow]})`;
}

export function formatTime(ts) {
  const p = jstDateParts(ts);
  return `${String(p.hh).padStart(2, "0")}:${String(p.mm).padStart(2, "0")}`;
}

export function dateKey(ts) {
  const p = jstDateParts(ts);
  return `${p.y}-${p.m}-${p.day}`;
}

export function formatLastTs(ts) {
  const p = jstDateParts(ts);
  return `${p.y}/${String(p.m).padStart(2, "0")}/${String(p.day).padStart(2, "0")}`;
}

export function toast(msg, isError) {
  const el = qs("#toast");
  el.textContent = msg;
  el.classList.remove("hidden");
  el.classList.toggle("error", !!isError);
  clearTimeout(toast._t);
  toast._t = setTimeout(() => el.classList.add("hidden"), 3500);
}

// 調査用コンソールログ(Sidebar.loadの所要時間計測等)の出力可否。
// /api/settings のレスポンス(is_dev, config-dev.json使用時のみtrue)を
// Settings.checkOnStartup() が受け取ってここに反映する。
// App.init() は checkOnStartup() の完了を待ってから他の初期化を行うため、
// それ以降のコードからはこの値を参照できる。
// モジュール分割によりファイルをまたいで参照するため、let直接exportではなく
// getter/setter経由にする(ESMのlive bindingでもimport先はconstとしてしか
// 束縛できず、代入は元モジュール内からしかできないため)。
let _isDevMode = false;
export function isDevMode() { return _isDevMode; }
export function setDevMode(v) { _isDevMode = !!v; }

// サーバ側のSQLite busy_timeout(30秒)に合わせた既定タイムアウト。
// 取り込み中に読み取りリクエストがロック待ちで詰まった場合、fetch自体は
// ブラウザ側では無期限に待ち続けてしまう(=画面が「読み込み中...」のまま
// 固まって見える)ため、ここで打ち切ってユーザーに状況を伝えられるようにする。
const FETCH_TIMEOUT_MS = 35000;

export async function fetchJson(url, opts) {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
  let res;
  try {
    res = await fetch(url, { ...opts, signal: controller.signal });
  } catch (e) {
    if (e.name === "AbortError") {
      throw new Error("サーバの応答がありません(タイムアウト)。取り込み処理が重い可能性があります。");
    }
    throw e;
  } finally {
    clearTimeout(timeoutId);
  }
  if (!res.ok) {
    let detail = "";
    try {
      const j = await res.json();
      detail = j.detail || JSON.stringify(j);
    } catch (e) { /* ignore */ }
    throw new Error(`HTTP ${res.status} ${detail}`);
  }
  return res.json();
}
