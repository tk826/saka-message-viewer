"use strict";
/* ==========================================================================
   メッセージ描画
   ========================================================================== */
import { qs, ce, fetchJson, toast, formatTime, formatDateSep } from "./utils.js";
import { applyNicknamePlaceholder } from "./display-prefs.js";
import { LazyMedia } from "./lazy-media.js";
import { VolumeBoost } from "./volume-boost.js";
import { Favorites } from "./favorites.js";
import { openLightbox, lockAspectRatioOnLoad } from "./lightbox.js";
import { Timeline } from "./timeline.js";
import { buildCommentEditor } from "./comments.js";

export function buildMediaEl(msg) {
  const wrap = ce("div", "msg-media");

  if (msg.missing) {
    wrap.classList.add("missing-media-wrap");
    const ph = ce("div", "missing-media");
    ph.innerHTML = `&#9888;&#65039;<br>ファイルが見つかりません`;
    wrap.appendChild(ph);
    return wrap;
  }

  if (msg.kind === "image") {
    const img = ce("img", "thumb-img");
    img.alt = "画像";
    img.loading = "lazy";
    if (msg.thumb_url) {
      img.setAttribute("data-src", msg.thumb_url);
      LazyMedia.observe(img);
    } else if (msg.media_url) {
      img.setAttribute("data-src", msg.media_url);
      LazyMedia.observe(img);
    }
    img.addEventListener("click", () => openLightbox(msg));
    wrap.appendChild(img);
    return wrap;
  }

  if (msg.kind === "video") {
    const video = ce("video");
    video.controls = true;
    // preload="metadata": 実データ(映像本体)はダウンロードしないが、
    // 解像度・長さ等のメタデータだけは先に取得する。"none"だと画面内に
    // 入った時点でsourceを追加してもメタデータ取得が始まらず、再生する
    // まで実際の解像度が分からずposterのサイズのまま表示され続けていた。
    video.preload = "metadata";
    lockAspectRatioOnLoad(video, 360); // .msg-media video の max-height と合わせる
    if (msg.thumb_url) {
      video.poster = msg.thumb_url;
      // video要素はposter読み込み完了を通知しないため、同じURLをImageで
      // プリロードしてloadを検知し、末尾表示中は最下部に追従させる。
      const preloadImg = new Image();
      preloadImg.addEventListener("load", () => Timeline.stickToBottomIfNeeded(), { once: true });
      preloadImg.src = msg.thumb_url;
    }
    if (msg.media_url) {
      video.setAttribute("data-src", msg.media_url);
      video.setAttribute("data-type", "video/mp4");
      LazyMedia.observe(video);
    }
    wrap.appendChild(video);
    const badge = ce("div", "play-badge");
    badge.innerHTML = "&#9654;";
    // プレイヤーの操作を邪魔しないよう、再生開始で非表示にする
    video.addEventListener("play", () => badge.remove());
    wrap.appendChild(badge);
    // 拡大表示ボタン。ホバー時のみ表示し(CSS: .msg-media:hover)、ネイティブ
    // controls(下部バー)と重ならない右上に置くことで、再生中/再生前どちらでも
    // 干渉せずクリックできる。
    const expandBtn = ce("button", "media-expand-btn");
    expandBtn.type = "button";
    expandBtn.title = "拡大表示";
    expandBtn.innerHTML = "&#9974;";
    expandBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      openLightbox(msg, video);
    });
    wrap.appendChild(expandBtn);
    // 設定「音量ブースター」がONなら、この動画にも即座に反映する
    // (OFFの場合は何もしない=ネイティブの音量スライダーのみで操作する)。
    VolumeBoost.applyToNew(video);
    return wrap;
  }

  if (msg.kind === "audio") {
    // 拡張子が.mp4のため <video> で再生する
    const video = ce("video");
    video.controls = true;
    video.preload = "none";
    if (msg.media_url) {
      video.setAttribute("data-src", msg.media_url);
      video.setAttribute("data-type", "video/mp4");
      LazyMedia.observe(video);
    }
    wrap.appendChild(video);
    const tag = ce("div", "audio-tag");
    tag.innerHTML = "&#127925; 音声";
    wrap.appendChild(tag);
    VolumeBoost.applyToNew(video);
    return wrap;
  }

  return null; // text のみ: メディアなし
}

export function buildMessageRow(msg) {
  // .msg-row自体を「メッセージ本体(左, 従来のmax-width)」と
  // 「コメント欄(右, 残り幅)」の横並びコンテナにする。仮想スクロールの
  // トリミング(Timeline.trimIfNeeded等)は.msg-row/.date-sepが#timelineの
  // 直接の子であることを前提にしているため、その構造自体は変えず、
  // .msg-row内部だけを分割する。
  const row = ce("div", "msg-row");
  row.dataset.msgId = msg.msg_id;
  row.dataset.ts = msg.ts;

  const main = ce("div", "msg-row-main");

  const bubble = ce("div", "msg-bubble");

  if (msg.missing) {
    const flag = ce("div", "missing-flag");
    flag.textContent = "⚠️ 欠損（実ファイルなし）";
    bubble.appendChild(flag);
  }

  if (msg.kind !== "text") {
    const mediaEl = buildMediaEl(msg);
    if (mediaEl) bubble.appendChild(mediaEl);
  }

  if (msg.body) {
    const bodyEl = ce("div", "msg-body");
    bodyEl.dataset.rawBody = msg.body;
    bodyEl.textContent = applyNicknamePlaceholder(msg.body);
    bubble.appendChild(bodyEl);
  } else if (msg.kind === "text" && !msg.missing) {
    const bodyEl = ce("div", "msg-body");
    bodyEl.textContent = "(本文なし)";
    bodyEl.style.opacity = "0.5";
    bubble.appendChild(bodyEl);
  }

  // バブルとアクション群(★/コピー)を横並びにするラッパー。
  // バブルのすぐ右にボタンが来るようにし、かつバブルの折り返し幅は崩さない。
  const bubbleRow = ce("div", "msg-bubble-row");
  bubbleRow.appendChild(bubble);
  bubbleRow.appendChild(buildMsgRowActions(msg));
  main.appendChild(bubbleRow);

  const timeEl = ce("div", "msg-time");
  timeEl.textContent = formatTime(msg.ts);
  main.appendChild(timeEl);

  row.appendChild(main);
  row.appendChild(buildCommentEditor(msg, "msg-row-comment"));

  return row;
}

// ★お気に入りボタン・パスコピーボタンをまとめたアクション群。
// .msg-bubble-row内でバブルの隣に置かれ、ホバー時のみ表示される。
export function buildMsgRowActions(msg) {
  const actions = ce("div", "msg-row-actions");

  const favBtn = ce("button", "msg-action-btn fav-btn");
  favBtn.type = "button";
  const isFav = Favorites.has(msg.msg_id);
  Favorites.applyBtnState(favBtn, isFav);
  if (isFav) actions.classList.add("has-favorite"); // お気に入り済みは常時点灯させる
  favBtn.addEventListener("click", async (e) => {
    e.stopPropagation();
    await Favorites.toggle(msg.member, msg.msg_id, favBtn);
    actions.classList.toggle("has-favorite", Favorites.has(msg.msg_id));
  });
  actions.appendChild(favBtn);

  if (msg.kind !== "text") {
    const copyBtn = ce("button", "msg-action-btn copy-path-btn");
    copyBtn.type = "button";
    copyBtn.title = "メディアのパスをコピー";
    copyBtn.textContent = "⧉";
    copyBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      copyMediaPath(msg.member, msg.msg_id);
    });
    actions.appendChild(copyBtn);
  }

  return actions;
}

// クリップボードへのコピー。navigator.clipboardはHTTPS/localhost限定のため、
// LAN IP等の平文http経由アクセス時はexecCommandへフォールバックする。
export async function copyTextToClipboard(text) {
  if (navigator.clipboard && window.isSecureContext) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (e) {
      console.error(e);
    }
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.top = "-9999px";
    ta.style.left = "-9999px";
    document.body.appendChild(ta);
    ta.focus();
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    if (ok) return true;
  } catch (e) {
    console.error(e);
  }
  return false;
}

export async function copyMediaPath(member, msgId) {
  try {
    const data = await fetchJson(`/api/media-path?member=${encodeURIComponent(member)}&msg_id=${msgId}`);
    const ok = await copyTextToClipboard(data.path);
    if (ok) {
      toast(data.exists ? "パスをコピーしました" : "パスをコピーしました（ファイルは見つかりません）", !data.exists);
    } else {
      // コピーにも失敗した場合は手動コピーできるようパスを直接見せる
      window.prompt("コピーに失敗しました。以下のパスを手動でコピーしてください:", data.path);
    }
  } catch (e) {
    console.error(e);
    toast("パスの取得に失敗しました: " + e.message, true);
  }
}

export function buildDateSep(ts) {
  const sep = ce("div", "date-sep");
  const span = ce("span");
  span.textContent = formatDateSep(ts);
  sep.appendChild(span);
  return sep;
}
