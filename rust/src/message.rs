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
        let media_url = self.media.as_deref().filter(|s| !s.is_empty()).map(|m| format!("/media/{}", encode_path(m)));
        let thumb_url = self.thumb.as_deref().filter(|s| !s.is_empty()).map(|t| format!("/thumbs/{}", encode_path(t)));
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

/// `/` 区切りの相対パスを URL のパスとして使えるよう、各部分をパーセントエンコードする(`/` はそのまま)。
/// メンバー名・ファイル名に `#`(以降がフラグメントになる)や `%XX`(サーバ側で別の文字に戻る)が
/// 含まれていても、そのファイルを指す URL になる。サーバ(axum の Path)はエンコードを戻してからファイルを探す。
fn encode_path(rel: &str) -> String {
    let mut out = String::with_capacity(rel.len());
    for b in rel.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode_path;

    #[test]
    fn encode_path_keeps_separators_and_escapes_the_rest() {
        assert_eq!(encode_path("member_a/2026/202608/6_1_20260807000000.jpg"), "member_a/2026/202608/6_1_20260807000000.jpg");
        assert_eq!(encode_path("a#b/c?d/100%41 x.jpg"), "a%23b/c%3Fd/100%2541%20x.jpg");
        assert_eq!(encode_path("日向/a"), "%E6%97%A5%E5%90%91/a");
    }
}
