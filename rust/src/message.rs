//! メッセージ行とAPI共通オブジェクト(docs/api-spec.md 第3章)。

use rusqlite::Row;
use serde_json::{json, Value};

/// `SELECT` する列の並び。`MsgRow::from_row` と対応している。
pub const MSG_COLS: &str = "member, group_, msg_id, ts, ts_raw, body, media, kind, flag, thumb, missing";

pub struct MsgRow {
    pub member: String,
    pub group: Option<String>,
    pub msg_id: i64,
    pub ts: i64,
    pub ts_raw: String,
    pub body: Option<String>,
    pub media: Option<String>,
    pub kind: String,
    pub flag: i64,
    pub thumb: Option<String>,
    pub missing: i64,
}

impl MsgRow {
    pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(MsgRow {
            member: r.get(0)?,
            group: r.get(1)?,
            msg_id: r.get(2)?,
            ts: r.get(3)?,
            ts_raw: r.get(4)?,
            body: r.get(5)?,
            media: r.get(6)?,
            kind: r.get(7)?,
            flag: r.get(8)?,
            thumb: r.get(9)?,
            missing: r.get(10)?,
        })
    }

    /// API のメッセージオブジェクト。`media`/`thumb` が空文字のときは URL を null にする。
    pub fn to_json(&self, comment: Option<&String>) -> Value {
        let media_url = self.media.as_deref().filter(|s| !s.is_empty()).map(|m| format!("/media/{m}"));
        let thumb_url = self.thumb.as_deref().filter(|s| !s.is_empty()).map(|t| format!("/thumbs/{t}"));
        json!({
            "member": self.member,
            "group": self.group,
            "msg_id": self.msg_id,
            "ts": self.ts,
            "ts_raw": self.ts_raw,
            "body": self.body,
            "kind": self.kind,
            "flag": self.flag,
            "missing": self.missing != 0,
            "media_url": media_url,
            "thumb_url": thumb_url,
            "comment": comment,
        })
    }
}
