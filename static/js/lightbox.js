"use strict";
/* ==========================================================================
   ライトボックス
   ========================================================================== */
import { qs, ce } from "./utils.js";
import { VolumeBoost } from "./volume-boost.js";

// 拡大表示中の動画がタイムライン側のどの<video>から開かれたかを覚えておき、
// 閉じるときに再生位置・再生状態を書き戻す(closeLightbox参照)。
let _lightboxSourceVideo = null;

function lockAspectRatioOnLoad(video, maxHeightPx) {
  video.addEventListener("loadedmetadata", () => {
    const vw = video.videoWidth;
    const vh = video.videoHeight;
    if (vw > 0 && vh > 0) {
      const h = maxHeightPx ? Math.min(vh, maxHeightPx) : vh;
      const w = Math.round((h / vh) * vw);
      video.style.width = `${w}px`;
      video.style.height = `${h}px`;
    }
  }, { once: true });
}

// sourceVideo: タイムライン側の<video>から拡大表示を開いた場合、その要素。
// 裏で二重に再生・音声が鳴り続けないよう一時停止し、再生位置を引き継ぐ。
export function openLightbox(msg, sourceVideo = null) {
  _lightboxSourceVideo = sourceVideo;
  const lb = qs("#lightbox");
  const content = qs("#lightboxContent");
  content.innerHTML = "";
  if (msg.kind === "image" && msg.media_url) {
    const img = ce("img");
    img.src = msg.media_url;
    content.appendChild(img);
  } else if (msg.kind === "video" && msg.media_url) {
    const video = ce("video");
    video.controls = true;
    if (msg.thumb_url) video.poster = msg.thumb_url;
    // lightboxはビューポート依存(max-height: 85vh)のため、その時点の
    // 実際の高さ上限をpxで計算して渡す。
    lockAspectRatioOnLoad(video, window.innerHeight * 0.85);

    let seekTo = 0;
    if (sourceVideo) {
      video.autoplay = !sourceVideo.paused;
      sourceVideo.pause();
      seekTo = sourceVideo.currentTime;
    } else {
      video.autoplay = true;
    }
    video.addEventListener("loadedmetadata", () => { video.currentTime = seekTo; }, { once: true });

    video.src = msg.media_url;
    content.appendChild(video);
    // タイムライン側と同じ音量ブースト設定(ON/OFF)をここにも適用する。
    VolumeBoost.applyToNew(video);
  }
  lb.classList.remove("hidden");
}

export function closeLightbox() {
  const content = qs("#lightboxContent");
  const video = content.querySelector("video");
  if (video) {
    // タイムライン側の動画から開いた場合、拡大表示中の再生位置・再生状態を
    // 元の動画へ書き戻し、そのまま再生を続けられるようにする
    // (lightbox内のvideo要素自体はここで破棄するため、音声を途切れさせない
    // ためには先に元の動画へ引き継いでおく必要がある)。
    if (_lightboxSourceVideo) {
      const src = _lightboxSourceVideo;
      src.currentTime = video.currentTime;
      if (!video.paused) src.play().catch(() => {});
    } else {
      // タイムライン外(ギャラリー等)から直接開いた場合は引き継ぎ先が
      // 無いため、従来通り音声が流れ続けないよう停止する。
      video.pause();
    }
    video.removeAttribute("src");
    video.load();
  }
  _lightboxSourceVideo = null;
  qs("#lightbox").classList.add("hidden");
  content.innerHTML = "";
}

export { lockAspectRatioOnLoad };
