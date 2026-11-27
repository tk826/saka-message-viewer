"use strict";
/* ==========================================================================
   音量ブースト (Web Audio API)

   <video>/<audio>のネイティブcontrolsの音量は0〜100%までしか上げられない
   (ブラウザの仕様上の上限)。100%を超えて増幅するにはWeb Audio APIで
   MediaElementAudioSourceNode -> GainNode -> destination という処理グラフを
   組み、GainNodeのgain値で信号自体を増幅する必要がある。
   createMediaElementSourceは同一要素に対して一度しか呼べない(2回目は例外に
   なる)ため、要素ごとにノードをキャッシュして使い回す。
   また、その要素の音声出力は以後このグラフ経由のみになる(GainNode接続前は
   無音)ため、接続と同時にdestinationまで繋ぐ。
   AudioContextはユーザー操作なしで自動再生ポリシーによりsuspendされたまま
   になることがあるため、スライダー操作時にresume()する。

   メモリ: 音声グラフに繋がった要素は、ブラウザ(Chromium)によってはDOMから外しても
   解放されない。タイムラインの間引きやライトボックスで作っては捨てる動画が溜まらないよう、
   - ノードは作った時点ではなく、初めて再生されたときに作る(再生しない動画には作らない)
   - 一時停止・再生終了でグラフから外し、再生で繋ぎ直す
   ようにして、止まっている要素はグラフから参照されないようにする。
   ========================================================================== */
export const VolumeBoost = {
  MAX_GAIN: 2.0, // 200%
  _ctx: null,
  _nodes: new WeakMap(), // mediaEl -> { source, gain, connected }
  _watched: new WeakSet(), // 再生・停止を見張っている要素
  _enabled: false, // 設定「音量ブースター」の現在値(DisplayPrefs.apply経由で更新)

  _getContext() {
    if (!this._ctx) {
      const AC = window.AudioContext || window.webkitAudioContext;
      this._ctx = new AC();
    }
    return this._ctx;
  },

  _getEntry(mediaEl) {
    let entry = this._nodes.get(mediaEl);
    if (!entry) {
      const ctx = this._getContext();
      const source = ctx.createMediaElementSource(mediaEl);
      const gain = ctx.createGain();
      gain.connect(ctx.destination);
      entry = { source, gain, connected: false };
      this._nodes.set(mediaEl, entry);
    }
    if (!entry.connected) {
      entry.source.connect(entry.gain);
      entry.connected = true;
    }
    return entry;
  },

  // 再生・停止を見張る(要素ごとに1回だけ)。
  _watch(mediaEl) {
    if (this._watched.has(mediaEl)) return;
    this._watched.add(mediaEl);
    mediaEl.addEventListener("play", () => {
      // ブースターが一度も使われていない要素は、OFF のままなら何もしない(ネイティブの再生のまま)
      if (!this._enabled && !this._nodes.has(mediaEl)) return;
      this._apply(mediaEl, this._enabled);
    });
    const detach = () => {
      const entry = this._nodes.get(mediaEl);
      if (entry && entry.connected) {
        entry.source.disconnect();
        entry.connected = false;
      }
    };
    mediaEl.addEventListener("pause", detach);
    mediaEl.addEventListener("ended", detach);
  },

  // グラフを繋いで gain を設定する。
  _apply(mediaEl, on) {
    const ctx = this._getContext();
    if (ctx.state === "suspended") ctx.resume().catch(() => {});
    this._getEntry(mediaEl).gain.gain.value = on ? this.MAX_GAIN : 1.0;
  },

  // ブースターON/OFFをGainNodeのgain値(1.0固定 or MAX_GAIN固定)だけで
  // 切り替える。mediaEl.volumeには触れない(ネイティブcontrolsの音量
  // スライダー操作をそのまま尊重するため)。ONにする瞬間だけvolumeを
  // 0.5にリセットする(0.5×MAX_GAIN(2.0)=1.0相当を基準点にするため)。
  // ノードは再生中の要素だけ作る(止まっている要素は、次に再生したときに作る・繋ぐ)。
  setBoost(mediaEl, on) {
    this._watch(mediaEl);
    if (on) mediaEl.volume = 0.5;
    const entry = this._nodes.get(mediaEl);
    if (mediaEl.paused) {
      // 止まっている: gain だけ合わせておき、繋ぐのは再生時
      if (entry) entry.gain.gain.value = on ? this.MAX_GAIN : 1.0;
      return;
    }
    if (!on && !entry) return; // 一度もONにされていない(ネイティブの再生のまま)
    this._apply(mediaEl, on);
  },

  // 設定変更時、既に表示されている全動画/音声要素に一括適用する。
  // タイムラインとlightbox(拡大表示中の動画)の両方が対象。
  applyToAll(on) {
    this._enabled = on;
    const els = document.querySelectorAll("#timeline video, #lightboxContent video");
    for (const el of els) this.setBoost(el, on);
  },

  // 新規に生成されたmediaEl(buildMediaElで動画/音声を作るたび)に、現在の
  // 設定を反映する。設定変更前に生成された要素はapplyToAllで、設定変更後に
  // 生成される要素はこちらでカバーする。
  applyToNew(mediaEl) {
    this._watch(mediaEl);
    if (this._enabled) mediaEl.volume = 0.5;
  },
};
