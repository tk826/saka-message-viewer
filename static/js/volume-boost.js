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
   ========================================================================== */
export const VolumeBoost = {
  MAX_GAIN: 2.0, // 200%
  _ctx: null,
  _nodes: new WeakMap(), // mediaEl -> { source, gain }
  _enabled: false, // 設定「音量ブースター」の現在値(DisplayPrefs.apply経由で更新)

  _getContext() {
    if (!this._ctx) {
      const AC = window.AudioContext || window.webkitAudioContext;
      this._ctx = new AC();
    }
    return this._ctx;
  },

  _getGainNode(mediaEl) {
    let entry = this._nodes.get(mediaEl);
    if (!entry) {
      const ctx = this._getContext();
      const source = ctx.createMediaElementSource(mediaEl);
      const gain = ctx.createGain();
      source.connect(gain);
      gain.connect(ctx.destination);
      entry = { source, gain };
      this._nodes.set(mediaEl, entry);
    }
    return entry.gain;
  },

  // ブースターON/OFFをGainNodeのgain値(1.0固定 or MAX_GAIN固定)だけで
  // 切り替える。mediaEl.volumeには触れない(ネイティブcontrolsの音量
  // スライダー操作をそのまま尊重するため)。ONにする瞬間だけvolumeを
  // 0.5にリセットする(0.5×MAX_GAIN(2.0)=1.0相当を基準点にするため)。
  // OFFにする際、まだ一度もONにされたことが無い(GainNode未生成)要素は
  // 何もしない(AudioContext初期化そのものを避ける)。
  setBoost(mediaEl, on) {
    if (!on && !this._nodes.has(mediaEl)) return;
    if (on) mediaEl.volume = 0.5;
    const ctx = this._getContext();
    if (ctx.state === "suspended") ctx.resume().catch(() => {});
    const gainNode = this._getGainNode(mediaEl);
    gainNode.gain.value = on ? this.MAX_GAIN : 1.0;
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
    if (this._enabled) this.setBoost(mediaEl, true);
  },
};
