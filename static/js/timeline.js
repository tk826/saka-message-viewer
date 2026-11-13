"use strict";
/* ==========================================================================
   タイムライン (仮想スクロール)
   ========================================================================== */
import { qs, ce, escapeHtml, fetchJson, dateKey, formatDateSep, isDevMode, highlightTerms as applyHighlight } from "./utils.js";
import { applyNicknamePlaceholder } from "./display-prefs.js";

export const Timeline = {
  member: null,
  loadingOlder: false,
  loadingNewer: false,
  hasMoreOlder: true,
  hasMoreNewer: true,
  PAGE_SIZE: 50,
  MAX_DOM_ROWS: 400,   // これを超えたら反対側を間引く
  TRIM_TO: 250,
  highlightTerms: [],  // 検索結果から飛んできたときに本文中で強調する語

  el: null,

  init() {
    this.el = qs("#timeline");
    this.el.addEventListener("scroll", () => this.onScroll(), { passive: true });
    qs("#scrollBottomBtn").addEventListener("click", () => this.scrollToBottomAndResync());
    // 末尾行の高さ確定(画像/動画の読み込み完了)を検知して最下部に追従する。
    // 個別のloadイベント待ちは発火タイミングを取りこぼすことがあるため、
    // サイズ変化そのものを監視する。
    this._bottomRo = new ResizeObserver(() => this.stickToBottomIfNeeded());
  },

  // ResizeObserverの監視対象を現在の末尾行に切り替える(appendBatchから呼ぶ)。
  _updateBottomResizeObserver() {
    this._bottomRo.disconnect();
    const last = this.el.lastElementChild;
    if (last) this._bottomRo.observe(last);
  },

  // フローティング日付見出し(#stickyDateSep)の更新。
  // 全.date-sepを毎回走査すると行数が多い場合に重くなるため、
  // スクロールコンテナ上端直下の要素をelementFromPointで1件だけ拾い、
  // そこから遡って直近の.msg-row(の日付)ないし.date-sepを求める軽量な方法にする。
  updateStickyDateSep() {
    const el = this.el;
    const stickyEl = qs("#stickyDateSep");
    if (!this.member || el.children.length === 0) {
      stickyEl.classList.add("hidden");
      return;
    }
    const rect = el.getBoundingClientRect();
    // コンテナ上端よりわずかに内側の点で判定(境界ちょうどだと取得できないブラウザ対策)
    const probeY = rect.top + 2;
    const probeX = rect.left + rect.width / 2;
    let target = document.elementFromPoint(probeX, probeY);
    // ヒットした要素から.msg-rowまたは.date-sepまで祖先を辿る
    let node = target;
    while (node && node !== el && !node.classList?.contains("msg-row") && !node.classList?.contains("date-sep")) {
      node = node.parentElement;
    }
    if (!node || node === el) {
      // 取得できなかった場合(スクロール最上部など)は先頭要素から求める
      node = el.firstElementChild;
    }
    let dateText = null;
    let sepNode = null;
    while (node) {
      if (node.classList && node.classList.contains("date-sep")) {
        dateText = node.textContent;
        sepNode = node;
        break;
      }
      if (node.classList && node.classList.contains("msg-row") && node.dataset.ts) {
        dateText = formatDateSep(Number(node.dataset.ts));
        // この.msg-row自体は.date-sepではないため、対応する実.date-sepを
        // さらに遡って探す(表示中の日付を示している要素そのもの)。
        let seekNode = node.previousElementSibling;
        while (seekNode) {
          if (seekNode.classList && seekNode.classList.contains("date-sep")) {
            sepNode = seekNode;
            break;
          }
          seekNode = seekNode.previousElementSibling;
        }
        break;
      }
      node = node.previousElementSibling;
    }
    // 対応する.date-sep要素自体が既にビューポート内に見えているなら、
    // 通常のセパレータと重複表示されてしまうためsticky側は非表示にする。
    // 見つからなかった場合は誤って重複表示するより非表示側に倒す。
    if (dateText && sepNode) {
      const sepRect = sepNode.getBoundingClientRect();
      const alreadyVisible = sepRect.bottom > rect.top;
      if (alreadyVisible) {
        stickyEl.classList.add("hidden");
      } else {
        stickyEl.textContent = dateText;
        stickyEl.classList.remove("hidden");
      }
    } else {
      stickyEl.classList.add("hidden");
    }
  },

  // 検索語の強調表示を設定する。空配列を渡すと解除される。
  // loadInitial前に呼ぶことで、描画時から強調された状態で出せる。
  setHighlightTerms(terms) {
    this.highlightTerms = Array.isArray(terms) ? terms.filter(Boolean) : [];
  },

  reapplyNicknamePlaceholder() {
    this.el.querySelectorAll(".msg-body[data-raw-body]").forEach((el) => {
      const text = applyNicknamePlaceholder(el.dataset.rawBody);
      // 強調中はニックネーム置換後の本文に対して貼り直す
      if (this.highlightTerms.length > 0) {
        applyHighlight(el, text, this.highlightTerms);
      } else {
        el.textContent = text;
      }
    });
  },

  reset(member) {
    this.member = member;
    this.loadingOlder = false;
    this.loadingNewer = false;
    this.hasMoreOlder = true;
    this.hasMoreNewer = true;
    this.el.innerHTML = "";
    this._globalLastDateKey = null;
    this._stickToBottom = false;
    this._bottomRo.disconnect();
    qs("#emptyState").classList.add("hidden");
    this.el.classList.remove("hidden");
    qs("#stickyDateSep").classList.add("hidden");
  },

  // 末尾表示中に遅延読み込み画像が展開されても最下部に追従させる
  stickToBottomIfNeeded() {
    if (!this._stickToBottom) return;
    this.el.scrollTop = this.el.scrollHeight;
  },

  // jumpMsgId: ジャンプ先を一意に特定するmsg_id(あれば優先して使う)。
  // tsは「同じ秒」に複数メッセージが存在すると重複しうるため、ts一致だけで
  // ジャンプ先を探すと別のメッセージにマッチすることがある
  // (呼び出し元がmsg_idを持たない場合=カレンダーからのジャンプ等のみnull)。
  async loadInitial(member, jumpTs, jumpMsgId = null) {
    // 遅延importで循環参照(message-render.js -> timeline.js)を避けつつ利用する
    const { buildDateSep, buildMessageRow } = await import("./message-render.js");
    this.reset(member);
    this.el.innerHTML = `<div class="load-sentinel">読み込み中...</div>`;
    try {
      let data;
      if (jumpTs != null) {
        // ジャンプ先の少し前から読み込む
        data = await fetchJson(`/api/messages?member=${encodeURIComponent(member)}&before=${jumpTs + 1}&limit=${this.PAGE_SIZE}`);
      } else {
        data = await fetchJson(`/api/messages?member=${encodeURIComponent(member)}&limit=${this.PAGE_SIZE}`);
      }
      this.el.innerHTML = "";
      const msgs = data.messages || [];
      if (msgs.length === 0) {
        this.el.innerHTML = `<div class="load-sentinel">メッセージがありません</div>`;
        this.hasMoreOlder = false;
        this.hasMoreNewer = false;
        return;
      }
      if (msgs.length < this.PAGE_SIZE) {
        this.hasMoreOlder = false;
      }
      this.appendBatch(msgs, "bottom", buildDateSep, buildMessageRow);

      if (jumpTs != null) {
        // ジャンプ指定がある場合、以降のメッセージも少し読み込んでおく
        const newer = await fetchJson(`/api/messages?member=${encodeURIComponent(member)}&after=${msgs[msgs.length - 1].ts}&after_id=${msgs[msgs.length - 1].msg_id}&limit=${this.PAGE_SIZE}`);
        if (newer.messages && newer.messages.length > 0) {
          this.appendBatch(newer.messages, "bottom", buildDateSep, buildMessageRow);
          if (newer.messages.length < this.PAGE_SIZE) this.hasMoreNewer = false;
        } else {
          this.hasMoreNewer = false;
        }
        // ジャンプ先の行までスクロール。msg_idが分かっていればそれで一意に
        // 特定する(before=jumpTs+1で取得したバッチの末尾がjumpTsのメッセージ
        // だが、同じ秒に複数メッセージがある場合はts一致だけでは別の
        // メッセージにマッチしうるため)。
        const byId = jumpMsgId != null ? msgs.find((m) => m.msg_id === jumpMsgId) : null;
        const byTs = !byId ? msgs.find((m) => m.ts === jumpTs) : null;
        const jumpMsg = byId || byTs || msgs[msgs.length - 1];
        if (isDevMode()) {
          console.info("[Timeline.loadInitial] jump debug", {
            requestedJumpTs: jumpTs,
            requestedJumpMsgId: jumpMsgId,
            matchedBy: byId ? "msg_id" : (byTs ? "ts" : "fallback-last"),
            resolvedMsgId: jumpMsg.msg_id,
            resolvedTs: jumpMsg.ts,
            msgsCount: msgs.length,
            msgsTsRange: [msgs[0]?.ts, msgs[msgs.length - 1]?.ts],
          });
        }
        requestAnimationFrame(() => this.scrollToMsgId(jumpMsg.msg_id));
      } else {
        this.hasMoreNewer = false; // 最新から読んでいるので「新しい方向」はもうない
        this._stickToBottom = true;
        requestAnimationFrame(() => { this.el.scrollTop = this.el.scrollHeight; });
      }
      requestAnimationFrame(() => this.updateStickyDateSep());
    } catch (e) {
      console.error(e);
      this.el.innerHTML = `<div class="load-sentinel">読み込みに失敗しました: ${escapeHtml(e.message)}</div>`;
    }
  },

  scrollToMsgId(msgId) {
    const row = this.el.querySelector(`.msg-row[data-msg-id="${CSS.escape(String(msgId))}"]`);
    if (!row) return;
    row.scrollIntoView({ block: "center" });
    const bubble = row.querySelector(".msg-bubble");
    if (bubble) {
      bubble.style.transition = "box-shadow 0.6s";
      const orig = bubble.style.boxShadow;
      bubble.style.boxShadow = "0 0 0 2px var(--selected-border)";
      setTimeout(() => { bubble.style.boxShadow = orig; }, 1200);
    }
    // 画像サムネやvideoのposter画像は非同期で読み込まれるため、
    // scrollIntoView実行時点ではまだ高さが確定しておらず、その後の
    // 読み込み完了でレイアウトシフトが起き、対象行が画面外に押し出される
    // ことがある。読み込み完了(またはタイムアウト)を待って再度centerへ
    // スクロールし直すことで、この分の位置ズレを補正する。
    const resync = () => row.scrollIntoView({ block: "center" });
    const img = row.querySelector("img");
    if (img && !img.complete) {
      img.addEventListener("load", resync, { once: true });
    }
    const video = row.querySelector("video");
    if (video) {
      const posterUrl = video.getAttribute("poster");
      if (posterUrl) {
        // video要素自体はposter画像の読み込み完了を通知するイベントを
        // 持たないため、同じURLをImageでプリロードしてloadを検知する。
        const preloadImg = new Image();
        preloadImg.addEventListener("load", resync, { once: true });
        preloadImg.src = posterUrl;
      }
      // preload="none"のためLazyMediaが実データを読み込むまでmetadataも
      // 取得できない。画面内に入って読み込みが進んだ場合の保険。
      video.addEventListener("loadedmetadata", resync, { once: true });
    }
    // 上記いずれのイベントも発火しなかった場合の最終保険。
    setTimeout(resync, 300);
  },

  // batch: 昇順(古い→新しい)の配列を想定
  appendBatch(msgs, where, buildDateSep, buildMessageRow) {
    const frag = document.createDocumentFragment();
    for (const msg of msgs) {
      const dk = dateKey(msg.ts);
      if (dk !== this._globalLastDateKey) {
        frag.appendChild(buildDateSep(msg.ts));
      }
      const row = buildMessageRow(msg);
      // 検索結果から飛んできた場合は、追加読み込み分の行にも同じ強調をかける
      if (this.highlightTerms.length > 0) {
        const bodyEl = row.querySelector(".msg-body[data-raw-body]");
        if (bodyEl) {
          applyHighlight(bodyEl, applyNicknamePlaceholder(bodyEl.dataset.rawBody), this.highlightTerms);
        }
      }
      frag.appendChild(row);
      this._globalLastDateKey = dk;
    }
    if (where === "bottom") {
      this.el.appendChild(frag);
      this._updateBottomResizeObserver();
    } else {
      this.el.insertBefore(frag, this.el.firstChild);
    }
  },

  onScroll() {
    const el = this.el;
    if (el.scrollTop < 400 && this.hasMoreOlder && !this.loadingOlder) {
      this.loadOlder();
    }
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 400 && this.hasMoreNewer && !this.loadingNewer) {
      this.loadNewer();
    }
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 200;
    qs("#scrollBottomBtn").classList.toggle("hidden", nearBottom);
    this._stickToBottom = nearBottom;

    // スクロールが止まっている間は更新不要なので、rAFで1フレームあたり1回に間引く
    if (!this._stickyRafPending) {
      this._stickyRafPending = true;
      requestAnimationFrame(() => {
        this._stickyRafPending = false;
        this.updateStickyDateSep();
      });
    }
  },

  async loadOlder() {
    const { buildDateSep, buildMessageRow } = await import("./message-render.js");
    const firstRow = this.el.querySelector(".msg-row");
    if (!firstRow) return;
    const oldestTs = Number(firstRow.dataset.ts);
    // (ts, msg_id) を目印にして、同じ秒のメッセージがページ境界で抜け落ちないようにする
    const oldestId = Number(firstRow.dataset.msgId);
    this.loadingOlder = true;
    const sentinel = ce("div", "load-sentinel");
    sentinel.textContent = "過去のメッセージを読み込み中...";
    this.el.insertBefore(sentinel, this.el.firstChild);
    try {
      const data = await fetchJson(`/api/messages?member=${encodeURIComponent(this.member)}&before=${oldestTs}&before_id=${oldestId}&limit=${this.PAGE_SIZE}`);
      const msgs = data.messages || [];
      sentinel.remove();
      if (msgs.length === 0) {
        this.hasMoreOlder = false;
        return;
      }
      if (msgs.length < this.PAGE_SIZE) this.hasMoreOlder = false;

      // スクロール位置補正: 挿入前の scrollHeight を記録
      const prevScrollHeight = this.el.scrollHeight;
      const prevScrollTop = this.el.scrollTop;

      // 日付セパレータ再計算のため、先頭要素の日付を一旦リセットして挿入
      this._globalLastDateKey = null;
      this.appendBatch(msgs, "top", buildDateSep, buildMessageRow);
      // 直後に続く要素との重複日付セパレータを除去
      this._dedupeDateSeps();

      const newScrollHeight = this.el.scrollHeight;
      this.el.scrollTop = prevScrollTop + (newScrollHeight - prevScrollHeight);

      this.trimIfNeeded("bottom");
    } catch (e) {
      console.error(e);
      sentinel.textContent = "読み込みに失敗しました";
      setTimeout(() => sentinel.remove(), 2000);
    } finally {
      this.loadingOlder = false;
    }
  },

  async loadNewer() {
    const { buildDateSep, buildMessageRow } = await import("./message-render.js");
    const rows = this.el.querySelectorAll(".msg-row");
    if (rows.length === 0) return;
    const lastRow = rows[rows.length - 1];
    const newestTs = Number(lastRow.dataset.ts);
    const newestId = Number(lastRow.dataset.msgId);
    this.loadingNewer = true;
    try {
      const data = await fetchJson(`/api/messages?member=${encodeURIComponent(this.member)}&after=${newestTs}&after_id=${newestId}&limit=${this.PAGE_SIZE}`);
      const msgs = data.messages || [];
      if (msgs.length === 0) {
        this.hasMoreNewer = false;
        return;
      }
      if (msgs.length < this.PAGE_SIZE) this.hasMoreNewer = false;
      // appendBatch は this._globalLastDateKey (タイムライン末尾の日付) を使うので、
      // 末尾に追記する場合はリセット不要(そのまま続きとして比較される)
      const lastKey = dateKey(Number(lastRow.dataset.ts));
      this._globalLastDateKey = lastKey;
      this.appendBatch(msgs, "bottom", buildDateSep, buildMessageRow);
      this.trimIfNeeded("top");
    } catch (e) {
      console.error(e);
    } finally {
      this.loadingNewer = false;
    }
  },

  // 取り込み中の自動追従用: ユーザーが末尾付近を見ていて、かつ既に最新まで
  // 読み込み済み(hasMoreNewer=false)の場合だけ新着メッセージを取り込む。
  // 過去ログを閲覧中(末尾から離れている)のユーザーの位置を勝手に動かさないため、
  // 条件を満たさない場合は何もしない(通常のloadNewerページングと競合しないよう
  // loadingNewer中もスキップする)。
  async refreshIfAtBottom() {
    if (!this.member || !this._stickToBottom || this.hasMoreNewer || this.loadingNewer) return;
    const rows = this.el.querySelectorAll(".msg-row");
    if (rows.length === 0) return;
    await this.loadNewer();
    requestAnimationFrame(() => { this.el.scrollTop = this.el.scrollHeight; });
  },

  _dedupeDateSeps() {
    // 連続する date-sep のうち2つ目以降を削除
    const children = Array.from(this.el.children);
    let prevWasSep = false;
    for (const child of children) {
      const isSep = child.classList.contains("date-sep");
      if (isSep && prevWasSep) {
        child.remove();
      }
      prevWasSep = isSep;
    }
    // 同一日付が連続するセパレータも統合(念のため二重チェック)
    const seps = Array.from(this.el.querySelectorAll(".date-sep"));
    for (let i = 1; i < seps.length; i++) {
      if (seps[i].textContent === seps[i - 1].textContent) {
        let node = seps[i - 1].nextSibling;
        let hasMsgBetween = false;
        while (node && node !== seps[i]) {
          if (node.classList && node.classList.contains("msg-row")) hasMsgBetween = true;
          node = node.nextSibling;
        }
        if (!hasMsgBetween) seps[i].remove();
      }
    }
  },

  trimIfNeeded(side) {
    const rows = this.el.querySelectorAll(".msg-row");
    if (rows.length <= this.MAX_DOM_ROWS) return;
    const excess = rows.length - this.TRIM_TO;
    if (excess <= 0) return;

    if (side === "bottom") {
      // 下端(新しい方)を間引く。ユーザーが下端付近を見ている場合は間引かない。
      const el = this.el;
      const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
      if (distFromBottom < 800) return;
      let node = el.lastElementChild;
      const toRemove = [];
      let rowCount = 0;
      while (node && rowCount < excess) {
        toRemove.push(node);
        if (node.classList.contains("msg-row")) rowCount++;
        node = node.previousElementSibling;
      }
      for (const n of toRemove) n.remove();
      this.hasMoreNewer = true;
    } else {
      // 上端(古い方)を間引く。ユーザーが上端付近を見ている場合は間引かない。
      const el = this.el;
      if (el.scrollTop < 800) return;
      const prevScrollHeight = el.scrollHeight;
      const prevScrollTop = el.scrollTop;

      let node = el.firstElementChild;
      const toRemove = [];
      let rowCount = 0;
      while (node && rowCount < excess) {
        toRemove.push(node);
        if (node.classList.contains("msg-row")) rowCount++;
        node = node.nextElementSibling;
      }
      for (const n of toRemove) n.remove();
      this.hasMoreOlder = true;

      const newScrollHeight = el.scrollHeight;
      el.scrollTop = prevScrollTop - (prevScrollHeight - newScrollHeight);
    }
  },

  scrollToBottomAndResync() {
    if (this.member) {
      this.loadInitial(this.member, null);
    }
  },
};
