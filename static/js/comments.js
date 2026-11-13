"use strict";
/* ==========================================================================
   メッセージへのコメント (お気に入り登録の有無を問わない)
   ========================================================================== */
import { ce, fetchJson, toast } from "./utils.js";

export const Comments = {
  // comment: 空文字/nullでコメントを消去。成功時は正規化後のcomment(nullまたは文字列)を返す。
  async update(member, msgId, comment) {
    const data = await fetchJson("/api/comments", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ member, msg_id: Number(msgId), comment }),
    });
    return data.comment;
  },
};

// コメントの表示/編集UI。タイムライン(常時表示)とお気に入り一覧パネルの
// 両方から共通で使う。msg.comment を直接書き換えて状態を保持する。
// クリックしても親要素側のジャンプ動作等を誘発しないよう、この要素内の
// クリックは常に stopPropagation する。
export function buildCommentEditor(msg, extraClass) {
  const wrap = ce("div", extraClass ? `comment-editor ${extraClass}` : "comment-editor");
  wrap.addEventListener("click", (e) => e.stopPropagation());

  const renderView = () => {
    wrap.innerHTML = "";
    if (msg.comment) {
      const text = ce("div", "comment-text");
      text.textContent = msg.comment;
      text.title = "クリックして編集";
      text.addEventListener("click", renderEdit);
      wrap.appendChild(text);
    } else {
      const addBtn = ce("button", "comment-add-btn");
      addBtn.type = "button";
      addBtn.textContent = "+ コメント";
      addBtn.addEventListener("click", renderEdit);
      wrap.appendChild(addBtn);
    }
  };

  const renderEdit = () => {
    wrap.innerHTML = "";
    const textarea = ce("textarea", "comment-input");
    textarea.value = msg.comment || "";
    textarea.placeholder = "コメントを入力...";
    textarea.rows = 2;
    wrap.appendChild(textarea);

    const actions = ce("div", "comment-actions");
    const saveBtn = ce("button", "comment-save-btn");
    saveBtn.type = "button";
    saveBtn.textContent = "保存";
    const cancelBtn = ce("button", "comment-cancel-btn");
    cancelBtn.type = "button";
    cancelBtn.textContent = "キャンセル";
    actions.appendChild(saveBtn);
    actions.appendChild(cancelBtn);
    wrap.appendChild(actions);

    textarea.focus();
    cancelBtn.addEventListener("click", renderView);
    saveBtn.addEventListener("click", async () => {
      saveBtn.disabled = true;
      try {
        const saved = await Comments.update(msg.member, msg.msg_id, textarea.value);
        msg.comment = saved;
        renderView();
      } catch (e) {
        console.error(e);
        toast("コメントの保存に失敗しました: " + e.message, true);
        saveBtn.disabled = false;
      }
    });
  };

  renderView();
  return wrap;
}
