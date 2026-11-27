"use strict";
/* ==========================================================================
   カレンダー (日付ジャンプ)
   ========================================================================== */
import { qs, ce, escapeHtml, fetchJson, toast, makeKeyboardClickable } from "./utils.js";
import { Sidebar } from "./sidebar.js";

export const CalendarPanel = {
  // 表示の通し番号。月一覧・日表示を切り替えるたびに進め、応答が届いた時点で変わっていれば捨てる
  // (月をクリックした直後に「←」を押したとき、後から届いた方で表示が入れ替わらないように)
  _seq: 0,

  init() {
    qs("#calendarBtn").addEventListener("click", () => this.toggle());
    qs(".calendar-close").addEventListener("click", () => this.close());
    qs("#calendarBackBtn").addEventListener("click", () => this.renderMonths());
  },

  toggle() {
    const panel = qs("#calendarPanel");
    if (panel.classList.contains("hidden")) {
      this.open();
    } else {
      this.close();
    }
  },

  close() {
    qs("#calendarPanel").classList.add("hidden");
  },

  async open() {
    if (!Sidebar.selectedMember) {
      toast("先にメンバーを選択してください", true);
      return;
    }
    qs("#searchPanel").classList.add("hidden");
    qs("#favoritesPanel").classList.add("hidden");
    qs("#galleryPanel").classList.add("hidden");
    qs("#calendarPanel").classList.remove("hidden");
    await this.renderMonths();
  },

  async renderMonths() {
    const seq = ++this._seq;
    qs("#calendarBackBtn").classList.add("hidden");
    qs("#calendarTitle").textContent = "日付ジャンプ";
    const body = qs("#calendarBody");
    body.innerHTML = `<div class="hint">読み込み中...</div>`;
    try {
      const data = await fetchJson(`/api/calendar?member=${encodeURIComponent(Sidebar.selectedMember)}`);
      if (seq !== this._seq) return;
      body.innerHTML = "";
      const byYear = new Map();
      for (const c of data.calendar) {
        const [y, m] = c.yearmonth.split("-");
        if (!byYear.has(y)) byYear.set(y, []);
        byYear.get(y).push({ month: m, count: c.count });
      }
      const years = Array.from(byYear.keys()).sort().reverse();
      if (years.length === 0) {
        body.innerHTML = `<div class="hint">データがありません</div>`;
        return;
      }
      for (const y of years) {
        const heading = ce("div", "calendar-year");
        heading.textContent = `${y}年`;
        body.appendChild(heading);
        const grid = ce("div", "calendar-grid");
        const months = byYear.get(y).slice().reverse();
        for (const { month, count } of months) {
          const cell = ce("div", "calendar-month");
          cell.innerHTML = `<div class="cm-label">${Number(month)}月</div><div class="cm-count">${count}件</div>`;
          cell.addEventListener("click", () => this.renderDays(y, month));
          makeKeyboardClickable(cell, `${y}年${Number(month)}月 (${count}件)`);
          grid.appendChild(cell);
        }
        body.appendChild(grid);
      }
    } catch (e) {
      if (seq !== this._seq) return;
      console.error(e);
      body.innerHTML = `<div class="hint">読み込みに失敗しました: ${escapeHtml(e.message)}</div>`;
    }
  },

  async renderDays(year, month) {
    const seq = ++this._seq;
    const yearmonth = `${year}-${month}`;
    qs("#calendarBackBtn").classList.remove("hidden");
    qs("#calendarTitle").textContent = `${year}年${Number(month)}月`;
    const body = qs("#calendarBody");
    body.innerHTML = `<div class="hint">読み込み中...</div>`;
    try {
      const data = await fetchJson(
        `/api/calendar?member=${encodeURIComponent(Sidebar.selectedMember)}&yearmonth=${yearmonth}`
      );
      if (seq !== this._seq) return;
      const countByDay = new Map();
      for (const d of data.days) {
        const day = Number(d.date.split("-")[2]);
        countByDay.set(day, d.count);
      }

      body.innerHTML = "";
      const weekdaysRow = ce("div", "calendar-weekdays");
      for (const w of ["日", "月", "火", "水", "木", "金", "土"]) {
        const span = document.createElement("span");
        span.textContent = w;
        weekdaysRow.appendChild(span);
      }
      body.appendChild(weekdaysRow);

      const firstOfMonth = new Date(Number(year), Number(month) - 1, 1);
      const daysInMonth = new Date(Number(year), Number(month), 0).getDate();
      const startWeekday = firstOfMonth.getDay();

      const grid = ce("div", "calendar-daygrid");
      for (let i = 0; i < startWeekday; i++) {
        grid.appendChild(ce("div", "calendar-day empty"));
      }
      for (let day = 1; day <= daysInMonth; day++) {
        const count = countByDay.get(day);
        const cell = ce("div", `calendar-day ${count ? "has-messages" : "no-messages"}`);
        cell.textContent = day;
        if (count) {
          cell.title = `${count}件`;
          const dateStr = `${year}-${month}-${String(day).padStart(2, "0")}`;
          cell.addEventListener("click", async () => {
            this.close();
            const { App } = await import("./app.js");
            App.jumpToDate(Sidebar.selectedMember, dateStr);
          });
          makeKeyboardClickable(cell, `${year}年${Number(month)}月${day}日 (${count}件)`);
        }
        grid.appendChild(cell);
      }
      body.appendChild(grid);
    } catch (e) {
      if (seq !== this._seq) return;
      console.error(e);
      body.innerHTML = `<div class="hint">読み込みに失敗しました: ${escapeHtml(e.message)}</div>`;
    }
  },
};
