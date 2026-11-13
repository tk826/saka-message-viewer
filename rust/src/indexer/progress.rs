//! 進捗の永続化(`data/progress.json`)。
//!
//! CLI で動いている取り込みの進捗も、サーバの /api/reindex/status がこのファイルを読んで UI に出す。
//! 一時ファイルに書いてから rename で置き換え、読み手が書きかけの JSON を掴まないようにする。

use super::IndexSummary;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 小数第1位に丸める。
pub fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

pub struct ProgressFileWriter {
    path: PathBuf,
    mode: String,
    started_at: f64,
    pid: u32,
}

impl ProgressFileWriter {
    pub fn new(path: &Path, mode: &str) -> Self {
        ProgressFileWriter { path: path.to_path_buf(), mode: mode.into(), started_at: now_f64(), pid: std::process::id() }
    }

    fn write(&self, data: Map<String, Value>) {
        let mut payload = json!({
            "running": true,
            "phase": null,
            "done": null,
            "total": null,
            "percent": null,
            "elapsed_sec": round1(now_f64() - self.started_at),
            "eta_sec": null,
            "started_at": self.started_at,
            "updated_at": now_f64(),
            "pid": self.pid,
            "mode": self.mode,
            "message": null,
        });
        let obj = payload.as_object_mut().unwrap();
        for (k, v) in data {
            obj.insert(k, v);
        }
        let res = (|| -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut tmp = self.path.as_os_str().to_owned();
            tmp.push(format!(".tmp{}", self.pid));
            let tmp = PathBuf::from(tmp);
            std::fs::write(&tmp, serde_json::to_vec(&payload)?)?;
            std::fs::rename(&tmp, &self.path)
        })();
        if let Err(e) = res {
            // 進捗の永続化に失敗しても、取り込み本体は止めない
            logln!("[warn] failed to write progress file: {e}");
        }
    }

    fn eta(done: Option<u64>, total: Option<u64>, elapsed: f64) -> Option<f64> {
        let (done, total) = (done?, total?);
        if done == 0 || total == 0 || done > total {
            return None;
        }
        let rate = if elapsed > 0.0 { done as f64 / elapsed } else { 0.0 };
        if rate <= 0.0 {
            return None;
        }
        Some(round1((total - done) as f64 / rate))
    }

    pub fn update(&self, phase: &str, done: Option<u64>, total: Option<u64>, message: Option<String>) {
        let elapsed = now_f64() - self.started_at;
        let percent = match (done, total) {
            (Some(d), Some(t)) if t > 0 => Some(round1(d as f64 / t as f64 * 100.0)),
            _ => None,
        };
        let mut m = Map::new();
        m.insert("running".into(), json!(true));
        m.insert("phase".into(), json!(phase));
        m.insert("done".into(), json!(done));
        m.insert("total".into(), json!(total));
        m.insert("percent".into(), json!(percent));
        m.insert("elapsed_sec".into(), json!(round1(elapsed)));
        m.insert("eta_sec".into(), json!(Self::eta(done, total, elapsed)));
        m.insert("message".into(), json!(message));
        self.write(m);
    }

    pub fn finish_done(&self, summary: &IndexSummary) {
        let mut m = Map::new();
        m.insert("running".into(), json!(false));
        m.insert("phase".into(), json!("done"));
        m.insert("done".into(), Value::Null);
        m.insert("total".into(), Value::Null);
        m.insert("percent".into(), json!(100.0));
        m.insert("eta_sec".into(), json!(0.0));
        m.insert("message".into(), Value::Null);
        m.insert("summary".into(), summary.to_json());
        self.write(m);
    }

    pub fn finish_error(&self, message: &str) {
        let mut m = Map::new();
        m.insert("running".into(), json!(false));
        m.insert("phase".into(), json!("error"));
        m.insert("eta_sec".into(), Value::Null);
        m.insert("message".into(), json!(message));
        self.write(m);
    }
}
