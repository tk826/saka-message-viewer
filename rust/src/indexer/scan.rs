//! 保存フォルダの走査。
//!
//! 対応する構成(`{グループ}/` と `{YYYY}/{YYYYMM}/` はそれぞれ省略可):
//!   A: {member}/{YYYY}/{YYYYMM}/{file}     B: {group}/{member}/{YYYY}/{YYYYMM}/{file}
//!   C: {member}/{file}                     D: {group}/{member}/{file}
//!
//! 走査順は取り込み順として rowid を決め、同じ秒のメッセージの検索順に効く。
//! - ディレクトリ・直下ファイルは名前順(Windows は大文字小文字を区別しない)
//! - 月ディレクトリ内のファイルは OS が返す順のまま

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FileEntry {
    /// 実ファイルの絶対パス
    pub path: PathBuf,
    /// root 相対パス(`/` 区切り)
    pub rel_path: String,
    pub member: String,
    pub group: Option<String>,
    pub msg_id: i64,
    pub flag: i64,
    pub ts_raw: String,
    /// 小文字化した拡張子
    pub ext: String,
}

/// 今回の走査で対象にした (member, yyyymm)。missing 判定に使う。None は「そのメンバーの全期間」。
#[derive(Default, Debug)]
pub struct ScanScope {
    pub members: Vec<String>,
    pub member_yearmonths: HashMap<String, Option<HashSet<String>>>,
}

impl ScanScope {
    fn add(&mut self, member: &str, yyyymm: Option<&str>) {
        if !self.member_yearmonths.contains_key(member) {
            self.members.push(member.to_string());
            self.member_yearmonths.insert(member.to_string(), Some(HashSet::new()));
        }
        let cur = self.member_yearmonths.get_mut(member).unwrap();
        match (cur.as_mut(), yyyymm) {
            (None, _) => {} // 既に全期間
            (Some(_), None) => *cur = None,
            (Some(set), Some(ym)) => {
                set.insert(ym.to_string());
            }
        }
    }
}

fn all_ascii_digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}

/// `^\d{4}$`
fn is_year_dir(name: &str) -> bool {
    all_ascii_digits(name, 4)
}

/// `^\d{6}$`
fn is_yearmonth_dir(name: &str) -> bool {
    all_ascii_digits(name, 6)
}

/// ファイル名 `^(\d+)_(\d)_(\d{14})\.(\w+)$` を分解する。(msg_id文字列, flag, ts_raw, ext)
pub fn parse_filename(name: &str) -> Option<(&str, &str, &str, &str)> {
    let (id, rest) = name.split_once('_')?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (flag, rest) = rest.split_once('_')?;
    if !all_ascii_digits(flag, 1) {
        return None;
    }
    let (ts, ext) = rest.split_once('.')?;
    if !all_ascii_digits(ts, 14) || ext.is_empty() || !ext.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((id, flag, ts, ext))
}

/// 名前順に並べる(同じ親の下での比較。Windows は大文字小文字を区別しない)。
fn sort_paths(v: &mut [PathBuf]) {
    let key = |p: &PathBuf| {
        let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if cfg!(windows) { n.to_lowercase() } else { n }
    };
    v.sort_by_key(key);
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn list_dir(d: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(d) {
        Ok(it) => it.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) => {
            logln!("[warn] failed to list {}: {e}", d.display());
            Vec::new()
        }
    }
}

/// メンバーディレクトリか(YYYY ディレクトリ、または規則に合うファイルを直下に持つか)。
/// 取り込み済みのメンバー名なら中身が空でもメンバー扱い(全ファイル削除を missing にできるように)。
fn looks_like_member_dir(d: &Path, known: &HashSet<String>) -> bool {
    if known.contains(&name_of(d)) {
        return true;
    }
    list_dir(d).iter().any(|e| {
        let n = name_of(e);
        if e.is_dir() { is_year_dir(&n) } else { parse_filename(&n).is_some() }
    })
}

/// (member_dir, member, group) を列挙する。
fn iter_member_dirs(root: &Path, known: &HashSet<String>) -> Vec<(PathBuf, String, Option<String>)> {
    let mut out = Vec::new();
    let mut tops: Vec<PathBuf> = list_dir(root).into_iter().filter(|p| p.is_dir()).collect();
    sort_paths(&mut tops);
    for top in tops {
        if looks_like_member_dir(&top, known) {
            let name = name_of(&top);
            out.push((top, name, None));
            continue;
        }
        // メンバーでなければグループ。その下をメンバーとして調べる
        let mut subs: Vec<PathBuf> = list_dir(&top).into_iter().filter(|p| p.is_dir()).collect();
        sort_paths(&mut subs);
        for sub in subs {
            if looks_like_member_dir(&sub, known) {
                out.push((sub.clone(), name_of(&sub), Some(name_of(&top))));
            }
        }
    }
    out
}

/// 直近 n ヶ月の YYYYMM(ローカル時刻基準)。
pub fn recent_yearmonths(n: u32) -> HashSet<String> {
    use chrono::Datelike;
    let now = chrono::Local::now();
    let (mut y, mut m) = (now.year(), now.month());
    let mut out = HashSet::new();
    for _ in 0..n {
        out.insert(format!("{y:04}{m:02}"));
        m -= 1;
        if m == 0 {
            m = 12;
            y -= 1;
        }
    }
    out
}

fn make_entry(root: &Path, f: &Path, member: &str, group: &Option<String>) -> Option<FileEntry> {
    let name = name_of(f);
    let (id, flag, ts, ext) = parse_filename(&name)?;
    let msg_id = match id.parse::<i64>() {
        Ok(v) => v,
        Err(_) => {
            logln!("[warn] msg_id too large, skipped: {}", f.display());
            return None;
        }
    };
    let rel = f.strip_prefix(root).ok()?;
    let rel_path = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    Some(FileEntry {
        path: f.to_path_buf(),
        rel_path,
        member: member.to_string(),
        group: group.clone(),
        msg_id,
        flag: flag.parse().unwrap(),
        ts_raw: ts.to_string(),
        ext: ext.to_lowercase(),
    })
}

/// 1メンバー分を走査する。戻り値: (FileEntry 一覧, 実際に走査した yyyymm(None は全期間))
fn scan_member(
    root: &Path,
    member_dir: &Path,
    member: &str,
    group: &Option<String>,
    member_recent: Option<&HashSet<String>>,
) -> (Vec<FileEntry>, Vec<Option<String>>) {
    let mut entries = Vec::new();
    let mut touched: Vec<Option<String>> = Vec::new();

    let mut year_dirs = Vec::new();
    let mut flat_files = Vec::new();
    for e in list_dir(member_dir) {
        if e.is_dir() {
            if is_year_dir(&name_of(&e)) {
                year_dirs.push(e);
            }
        } else {
            flat_files.push(e);
        }
    }

    sort_paths(&mut year_dirs);
    for year_dir in year_dirs {
        let mut months: Vec<PathBuf> =
            list_dir(&year_dir).into_iter().filter(|d| d.is_dir() && is_yearmonth_dir(&name_of(d))).collect();
        sort_paths(&mut months);
        for month_dir in months {
            let ym = name_of(&month_dir);
            if member_recent.is_some_and(|r| !r.contains(&ym)) {
                continue;
            }
            touched.push(Some(ym));
            for f in list_dir(&month_dir) {
                if !f.is_file() {
                    continue;
                }
                if let Some(fe) = make_entry(root, &f, member, group) {
                    entries.push(fe);
                }
            }
        }
    }

    // 直下にファイルが並ぶ構成では月ディレクトリで刈り込めないため、ファイル名の日時(YYYYMM)で絞る
    sort_paths(&mut flat_files);
    for f in flat_files {
        let Some(fe) = make_entry(root, &f, member, group) else { continue };
        let ym = fe.ts_raw[..6].to_string();
        if member_recent.is_some_and(|r| !r.contains(&ym)) {
            continue;
        }
        touched.push(Some(ym));
        entries.push(fe);
    }

    if member_recent.is_none() {
        touched.push(None); // 全期間(新規メンバー含む)
    }
    (entries, touched)
}

/// 既定の並列数。CPU 数の半分(最低1)。
pub fn default_workers() -> usize {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    (n / 2).max(1)
}

/// 走査対象ファイルを列挙する。mode: "full" | "incremental"。
/// incremental でも、まだ取り込んだことの無いメンバーは全期間を走査する。
pub fn discover_files(
    root: &Path,
    mode: &str,
    incremental_months: u32,
    only_member: Option<&str>,
    known: &HashSet<String>,
) -> (Vec<FileEntry>, ScanScope) {
    let recent = (mode == "incremental").then(|| recent_yearmonths(incremental_months));
    let targets: Vec<_> = iter_member_dirs(root, known)
        .into_iter()
        .filter(|(_, m, _)| only_member.is_none_or(|o| o == m))
        .collect();

    let mut scope = ScanScope::default();
    if targets.is_empty() {
        return (Vec::new(), scope);
    }

    // メンバー単位で並列に走査する(ネットワークドライブではディレクトリ列挙の待ちが大きい)。
    // 結果は対象の並び順で結合する
    let results = super::parallel_map(&targets, default_workers(), |(dir, member, group)| {
        let is_new = mode == "incremental" && !known.contains(member);
        let member_recent = if is_new { None } else { recent.as_ref() };
        scan_member(root, dir, member, group, member_recent)
    });

    let mut entries = Vec::new();
    for ((_, member, _), (es, touched)) in targets.iter().zip(results) {
        entries.extend(es);
        for ym in touched {
            scope.add(member, ym.as_deref());
        }
    }
    (entries, scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_rule() {
        assert_eq!(parse_filename("12_1_20260801000000.jpg"), Some(("12", "1", "20260801000000", "jpg")));
        assert_eq!(parse_filename("007_0_20260801000000.TXT"), Some(("007", "0", "20260801000000", "TXT")));
        assert!(parse_filename("12_1_2026080100000.jpg").is_none()); // 13桁
        assert!(parse_filename("12_10_20260801000000.jpg").is_none()); // flag 2桁
        assert!(parse_filename("a_1_20260801000000.jpg").is_none());
        assert!(parse_filename("12_1_20260801000000").is_none()); // 拡張子なし
        assert!(parse_filename("12_1_20260801000000.tar.gz").is_none()); // \w+ に . は含まれない
        assert!(parse_filename("12_1_20260801000000.").is_none());
    }

    #[test]
    fn scope_promotion() {
        let mut s = ScanScope::default();
        s.add("a", Some("202608"));
        s.add("a", Some("202607"));
        assert_eq!(s.member_yearmonths["a"].as_ref().unwrap().len(), 2);
        s.add("a", None);
        assert!(s.member_yearmonths["a"].is_none());
        s.add("a", Some("202601"));
        assert!(s.member_yearmonths["a"].is_none());
        assert_eq!(s.members, vec!["a"]);
    }
}
