"use strict";
/* ==========================================================================
   サイドバー (メンバー一覧)
   ========================================================================== */
import { qs, ce, fetchJson, toast, isDevMode, formatLastTs } from "./utils.js";

const KANJI_DIGITS = { "一": 1, "二": 2, "三": 3, "四": 4, "五": 5, "六": 6, "七": 7, "八": 8, "九": 9, "十": 10 };

// グループ名から"◯期生"の期生番号を抽出する。漢数字(一〜十)・算用数字の両方に対応。
// 該当パターンが無ければnullを返す。
export function extractGeneration(groupName) {
  const arabicMatch = groupName.match(/(\d+)\s*期生/);
  if (arabicMatch) return Number(arabicMatch[1]);
  const kanjiMatch = groupName.match(/([一二三四五六七八九十])期生/);
  if (kanjiMatch) return KANJI_DIGITS[kanjiMatch[1]] ?? null;
  return null;
}

export const Sidebar = {
  threads: [],
  selectedMember: null,
  hiddenMembers: new Set(), // サイドバー一覧から隠すメンバー (user.dbに永続化)
  editMode: false,          // 「表示設定」ON中は非表示メンバーも薄く表示し、目アイコンで切替できる
  memberOrder: [],          // 手動で決めた並び順 (user.dbに永続化)。未収載は名前順で後ろに続く
  _dragMember: null,        // ドラッグ中のメンバー名
  _dropped: false,          // 一覧内でdropされたか(falseのままdragendならキャンセル)

  init() {
    qs("#memberEditBtn").addEventListener("click", () => this.toggleEditMode());

    // 一覧のどこで離しても「確定」として扱う。行の上でしかdropを受け付けないと、
    // ドラッグ中の行自身(移動済みでカーソルの真下にある)や行間で離したときに
    // キャンセル扱いになり、並び替えが戻ってしまうため。
    const list = qs("#memberList");
    list.addEventListener("dragover", (e) => {
      if (this._dragMember != null) e.preventDefault();
    });
    list.addEventListener("drop", (e) => {
      if (this._dragMember == null) return;
      e.preventDefault();
      this._dropped = true;
    });
  },

  // 非表示メンバー一覧をサーバから取得する。失敗しても一覧表示自体は
  // 続けたいので、例外は握りつぶして「非表示なし」として扱う。
  async loadHiddenMembers() {
    try {
      const data = await fetchJson("/api/hidden-members");
      this.hiddenMembers = new Set(data.members || []);
    } catch (e) {
      console.error(e);
      this.hiddenMembers = new Set();
    }
  },

  // 並び順をサーバから取得する。失敗しても一覧表示は続けたいので、
  // 例外は握りつぶして「並び順の指定なし(=名前順)」として扱う。
  async loadMemberOrder() {
    try {
      const data = await fetchJson("/api/member-order");
      this.memberOrder = data.members || [];
    } catch (e) {
      console.error(e);
      this.memberOrder = [];
    }
  },

  // 並び順の中での位置。未収載(新規メンバー等)は末尾扱いにして、
  // 既存の並びが新着で崩れないようにする。
  _orderIndex(member) {
    const i = this.memberOrder.indexOf(member);
    return i === -1 ? Number.MAX_SAFE_INTEGER : i;
  },

  toggleEditMode() {
    this.editMode = !this.editMode;
    qs("#memberEditBtn").classList.toggle("active", this.editMode);
    qs("#memberEditBtn").textContent = this.editMode ? "完了" : "表示設定";
    this.render();
  },

  async setHidden(member, hidden) {
    // 先にローカル状態を更新して即座に再描画し、通信は裏で行う(体感速度優先)。
    // 失敗した場合は元に戻して知らせる。
    if (hidden) this.hiddenMembers.add(member);
    else this.hiddenMembers.delete(member);
    this.render();
    try {
      await fetchJson("/api/hidden-members", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ member, hidden }),
      });
    } catch (e) {
      console.error(e);
      if (hidden) this.hiddenMembers.delete(member);
      else this.hiddenMembers.add(member);
      this.render();
      toast("表示設定の保存に失敗しました: " + e.message, true);
    }
  },

  async load({ silent = false } = {}) {
    if (!silent) qs("#memberListLoading").classList.remove("hidden");
    // 調査用: /api/threads の所要時間をコンソールで確認できるようにする
    // (取り込み中はSQLiteロック待ち等で遅くなりうるため)。
    const _t0 = performance.now();
    try {
      const data = await fetchJson("/api/threads");
      const _elapsed = performance.now() - _t0;
      if (isDevMode() && _elapsed > 1000) {
        console.warn(`[Sidebar.load] /api/threads took ${Math.round(_elapsed)}ms (silent=${silent})`);
      }
      const next = data || [];
      // silent時(取り込み中のバックグラウンドポーリング)は、内容が前回と同じなら
      // render()を呼ばない。renderは一覧DOMを丸ごと作り直すため、変化が無いのに
      // 毎回呼ぶとちらつきやスクロール位置のリセットにつながるため。
      if (silent && this._sameThreads(next)) {
        if (isDevMode()) {
          console.debug(`[Sidebar.load] silent poll: server returned ${next.length} threads (unchanged)`);
        }
        return;
      }
      if (silent && isDevMode()) {
        console.info(`[Sidebar.load] silent poll: threads ${this.threads.length} -> ${next.length}`);
      }
      this.threads = next;
      this.render();
    } catch (e) {
      console.error(e);
      if (silent) return; // バックグラウンド更新の失敗は静かに無視する
      qs("#memberListLoading").textContent = "読み込みに失敗しました";
      toast("メンバー一覧の取得に失敗しました: " + e.message, true);
      return;
    }
    if (!silent) qs("#memberListLoading").classList.add("hidden");
  },

  _sameThreads(next) {
    if (next.length !== this.threads.length) return false;
    for (let i = 0; i < next.length; i++) {
      const a = this.threads[i], b = next[i];
      if (a.member !== b.member || a.group !== b.group || a.count !== b.count || a.last_ts !== b.last_ts) {
        return false;
      }
    }
    return true;
  },

  render() {
    const el = qs("#memberList");
    el.innerHTML = "";

    // 編集モード中は非表示メンバーも(薄く)出して切り替えられるようにし、
    // 通常時は一覧から除外する。
    const visibleThreads = this.editMode
      ? this.threads
      : this.threads.filter((t) => !this.hiddenMembers.has(t.member));

    this.renderHiddenCount();

    const groups = new Map(); // groupName -> [thread]
    const OTHER = "その他";
    for (const t of visibleThreads) {
      const g = t.group || OTHER;
      if (!groups.has(g)) groups.set(g, []);
      groups.get(g).push(t);
    }

    // グループ名でソート(「その他」は最後)、"◯期生"を含む場合は期生番号の昇順、
    // それ以外は文字列比較にフォールバックする。
    const groupNames = Array.from(groups.keys()).sort((a, b) => {
      if (a === OTHER) return 1;
      if (b === OTHER) return -1;
      const genA = extractGeneration(a);
      const genB = extractGeneration(b);
      if (genA !== null && genB !== null) return genA - genB;
      if (genA !== null) return -1;
      if (genB !== null) return 1;
      return a.localeCompare(b, "ja");
    });

    for (const g of groupNames) {
      const heading = ce("div", "group-heading");
      heading.textContent = g;
      el.appendChild(heading);

      // 手動で並び替えた順を優先し、指定の無いメンバーは名前順で後ろに続ける
      // (last_ts降順だと新着のたびに順序が入れ替わってしまうため)。
      const members = groups.get(g).sort((a, b) => {
        const ia = this._orderIndex(a.member);
        const ib = this._orderIndex(b.member);
        if (ia !== ib) return ia - ib;
        return a.member.localeCompare(b.member, "ja");
      });
      for (const t of members) {
        el.appendChild(this.buildMemberItem(t));
      }
    }

    if (visibleThreads.length === 0) {
      const hint = ce("div", "hint");
      hint.textContent = this.threads.length === 0
        ? "メンバーがいません（インデックス未実行、または実行中）"
        : "表示中のメンバーがいません（「表示設定」から戻せます）";
      el.appendChild(hint);
    }
  },

  // 非表示にしているメンバー数をツールバーに出す。0件なら何も出さない。
  renderHiddenCount() {
    const el = qs("#sidebarHiddenCount");
    const count = this.hiddenMembers.size;
    el.textContent = count > 0 ? `${count}人を非表示中` : "";
    el.classList.toggle("hidden", count === 0);
  },

  // ドラッグ&ドロップで並び替える(「表示設定」モード中のみ)。
  // HTML5のdrag APIを使い、ドラッグ中の行を「今どの行の上にいるか」に応じて
  // DOM上で直接移動させる。確定(drop/dragend)時にDOMの並びを読み取って保存する。
  _attachDragHandlers(item, member) {
    item.draggable = true;

    item.addEventListener("dragstart", (e) => {
      this._dragMember = member;
      this._dropped = false;
      item.classList.add("dragging");
      e.dataTransfer.effectAllowed = "move";
      // Firefoxはこれを設定しないとドラッグが開始されない
      e.dataTransfer.setData("text/plain", member);
    });

    item.addEventListener("dragend", () => {
      item.classList.remove("dragging");
      this._dragMember = null;
      if (this._dropped) {
        this.saveOrderFromDom();
      } else {
        // Escや一覧の外で離した場合はキャンセル。dragover中にDOMを動かして
        // いるので、保存済みの並びで描き直して元に戻す。
        this.render();
      }
    });

    item.addEventListener("dragover", (e) => {
      if (this._dragMember == null || this._dragMember === member) return;
      const dragging = qs("#memberList").querySelector(".member-item.dragging");
      if (!dragging) return;
      // グループをまたぐ並び替えはしない(見出しの意味が壊れるため)
      if (dragging.dataset.group !== item.dataset.group) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "move";

      // カーソルが行の上半分なら前へ、下半分なら後ろへ差し込む
      const rect = item.getBoundingClientRect();
      const before = (e.clientY - rect.top) < rect.height / 2;
      const parent = item.parentElement;
      if (before) parent.insertBefore(dragging, item);
      else parent.insertBefore(dragging, item.nextSibling);
    });

    // dropの受け付けは #memberList 側(init)でまとめて行う。
    // 並びの反映はdragover時点で済んでいる。
  },

  // 現在のDOMの並びを読み取ってサーバへ保存する。
  // 表示されていないメンバー(非表示・別グループ)の並びも失わないよう、
  // 既存のmemberOrderと突き合わせて全体の順序を組み立て直す。
  async saveOrderFromDom() {
    const el = qs("#memberList");
    const shown = Array.from(el.querySelectorAll(".member-item")).map((x) => x.dataset.member);
    if (shown.length === 0) return;

    // 画面に出ている分は今の見た目の順、出ていない分は元の相対順を保つ。
    // まだ並び順に載っていないメンバー(初回の非表示メンバー等)も名前順で
    // 拾っておく。ここで漏らすと、次に表示されたとき末尾送りになるため。
    const shownSet = new Set(shown);
    const known = new Set([...shown, ...this.memberOrder]);
    const unlisted = this.threads
      .map((t) => t.member)
      .filter((m) => !known.has(m))
      .sort((a, b) => a.localeCompare(b, "ja"));
    const rest = this.memberOrder.filter((m) => !shownSet.has(m));
    const next = [...shown, ...rest, ...unlisted];

    if (next.length === this.memberOrder.length &&
        next.every((m, i) => m === this.memberOrder[i])) {
      return; // 並びが変わっていない(ドラッグせずに離した等)
    }

    const prev = this.memberOrder;
    this.memberOrder = next;
    try {
      await fetchJson("/api/member-order", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ members: next }),
      });
    } catch (e) {
      console.error(e);
      this.memberOrder = prev;
      this.render();
      toast("並び順の保存に失敗しました: " + e.message, true);
    }
  },

  buildMemberItem(t) {
    const item = ce("div", "member-item");
    item.dataset.member = t.member;
    item.dataset.group = t.group || "その他";
    if (t.member === this.selectedMember) item.classList.add("selected");

    const isHidden = this.hiddenMembers.has(t.member);
    if (isHidden) item.classList.add("is-hidden-member");

    const avatar = ce("div", "member-avatar");
    avatar.textContent = (t.member || "?").slice(0, 1);

    const meta = ce("div", "member-meta");
    const name = ce("div", "member-name");
    name.textContent = t.member;
    const sub = ce("div", "member-sub");
    const count = ce("span");
    count.textContent = `${t.count}件`;
    const last = ce("span");
    last.textContent = t.last_ts ? formatLastTs(t.last_ts) : "-";
    sub.appendChild(count);
    sub.appendChild(last);

    meta.appendChild(name);
    meta.appendChild(sub);
    item.appendChild(avatar);
    item.appendChild(meta);

    if (this.editMode) {
      // 「表示設定」中のみ並び替えできるようにする(通常時の誤ドラッグを防ぐ)
      this._attachDragHandlers(item, t.member);

      const handle = ce("span", "member-drag-handle");
      handle.textContent = "⠿";
      handle.title = "ドラッグして並び替え";
      item.insertBefore(handle, item.firstChild);

      const hideBtn = ce("button", "member-hide-btn");
      hideBtn.type = "button";
      hideBtn.textContent = isHidden ? "🚫" : "👁";
      hideBtn.title = isHidden ? "サイドバーに表示する" : "サイドバーから隠す";
      hideBtn.addEventListener("click", (e) => {
        // 行クリック(メンバー選択)を誘発させない
        e.stopPropagation();
        this.setHidden(t.member, !isHidden);
      });
      item.appendChild(hideBtn);
    }

    item.addEventListener("click", async () => {
      const { App } = await import("./app.js");
      App.selectMember(t.member);
    });
    return item;
  },

  setSelected(member) {
    this.selectedMember = member;
    qs("#memberList").querySelectorAll(".member-item").forEach((el) => {
      el.classList.toggle("selected", el.dataset.member === member);
    });
  },
};
