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

/// ファイル名 `^(\d+)_(\d)_(\d{14})(_.+)?\.(\w+)$` を分解する。(msg_id文字列, flag, ts_raw, ext)
/// `(_.+)?` は colmsg v5 以降が付ける投稿者名(例: `485173_0_20261110042549_金村 美玖.txt`)。
/// 投稿者名は使わないので捨てる。
pub fn parse_filename(name: &str) -> Option<(&str, &str, &str, &str)> {
    let (id, rest) = name.split_once('_')?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (flag, rest) = rest.split_once('_')?;
    if !all_ascii_digits(flag, 1) {
        return None;
    }
    // 投稿者名に '.' が含まれうるので、拡張子は最後の '.' で切る
    let (stem, ext) = rest.rsplit_once('.')?;
    let ts = stem.get(..14)?;
    let poster = &stem[14..];
    if !all_ascii_digits(ts, 14) || !(poster.is_empty() || (poster.starts_with('_') && poster.len() > 1)) {
        return None;
    }
    if ext.is_empty() || !ext.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((id, flag, ts, ext))
}

/// 名前順に並べる(同じ親の下での比較。Windows は大文字小文字を区別しない)。
fn sort_paths(v: &mut [PathBuf]) {
    let key = |p: &PathBuf| {
        let n = p.file_name().map_or_default(|n| n.to_string_lossy().into_owned());
        if cfg!(windows) { n.to_lowercase() } else { n }
    };
    v.sort_by_key(key);
}

fn name_of(p: &Path) -> String {
    p.file_name().map_or_default(|n| n.to_string_lossy().into_owned())
}

/// ディレクトリの中身。読めなければ警告して None。
/// 個々の項目が読めない場合も None にする(一部だけ読めた結果を「全部」として扱うと、残りを missing にしてしまう)。
fn try_list_dir(d: &Path) -> Option<Vec<PathBuf>> {
    let res = std::fs::read_dir(d).and_then(|it| it.map(|e| e.map(|e| e.path())).collect::<std::io::Result<Vec<_>>>());
    match res {
        Ok(v) => Some(v),
        Err(e) => {
            logln!("[warn] failed to list {}: {e}", d.display());
            None
        }
    }
}

/// ディレクトリの中身(読めなければ空)。メンバー探しなど、読めなくても missing 判定に響かないところで使う。
fn list_dir(d: &Path) -> Vec<PathBuf> {
    try_list_dir(d).unwrap_or_default()
}

/// ディレクトリか・ファイルか。メタデータを読めなければ None。
fn entry_kind(p: &Path) -> Option<bool> {
    match std::fs::metadata(p) {
        Ok(m) => Some(m.is_dir()),
        Err(e) => {
            logln!("[warn] failed to read metadata of {}: {e}", p.display());
            None
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
///
/// 読めなかったフォルダ(ネットワークドライブの瞬断、権限、同期ソフトのロックなど)の範囲は、走査した範囲に含めない。
/// 含めると、その範囲の取り込み済みの行がすべて missing=1 になる。
/// - メンバーフォルダ自体が読めない: 何も走査しなかった扱い
/// - 年・月フォルダや項目の一部が読めない: 読めた月だけを走査した扱い(全期間でも「全期間」にしない)
fn scan_member(
    root: &Path,
    member_dir: &Path,
    member: &str,
    group: &Option<String>,
    member_recent: Option<&HashSet<String>>,
) -> (Vec<FileEntry>, Vec<Option<String>>) {
    let mut entries = Vec::new();
    let mut touched: Vec<Option<String>> = Vec::new();
    // 走査できなかった範囲があるか
    let mut incomplete = false;

    let Some(top) = try_list_dir(member_dir) else {
        return (entries, touched);
    };
    let mut year_dirs = Vec::new();
    let mut flat_files = Vec::new();
    for e in top {
        match entry_kind(&e) {
            Some(true) => {
                if is_year_dir(&name_of(&e)) {
                    year_dirs.push(e);
                }
            }
            Some(false) => flat_files.push(e),
            None => incomplete = true,
        }
    }

    sort_paths(&mut year_dirs);
    for year_dir in year_dirs {
        let Some(items) = try_list_dir(&year_dir) else {
            incomplete = true;
            continue;
        };
        let mut months = Vec::new();
        for d in items {
            if !is_yearmonth_dir(&name_of(&d)) {
                continue;
            }
            match entry_kind(&d) {
                Some(true) => months.push(d),
                Some(false) => {}
                None => incomplete = true,
            }
        }
        sort_paths(&mut months);
        for month_dir in months {
            let ym = name_of(&month_dir);
            if member_recent.is_some_and(|r| !r.contains(&ym)) {
                continue;
            }
            let Some(files) = try_list_dir(&month_dir) else {
                incomplete = true;
                continue;
            };
            let mut month_entries = Vec::new();
            let mut month_ok = true;
            for f in files {
                match entry_kind(&f) {
                    Some(false) => {
                        if let Some(fe) = make_entry(root, &f, member, group) {
                            month_entries.push(fe);
                        }
                    }
                    Some(true) => {}
                    None => month_ok = false,
                }
            }
            // 読めた分は取り込む(missing=0 に戻すだけで害はない)。月の一部が読めなければ、その月は判定しない
            entries.extend(month_entries);
            if month_ok {
                touched.push(Some(ym));
            } else {
                incomplete = true;
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

    if member_recent.is_none() && !incomplete {
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
        // colmsg v5 の投稿者名付き
        assert_eq!(
            parse_filename("485173_0_20261110042549_金村 美玖.txt"),
            Some(("485173", "0", "20261110042549", "txt"))
        );
        assert_eq!(parse_filename("12_1_20260801000000_A.B.jpg"), Some(("12", "1", "20260801000000", "jpg")));
        assert_eq!(parse_filename("12_3_20260801000000_unknown.mp4"), Some(("12", "3", "20260801000000", "mp4")));
        assert!(parse_filename("12_1_20260801000000_.jpg").is_none()); // 投稿者名が空
        assert!(parse_filename("12_1_20260801000000x.jpg").is_none()); // 区切りが '_' でない
        assert!(parse_filename("12_1_2026080100000_金村.jpg").is_none()); // 13桁
        assert!(parse_filename("12_1_20260801000000_金村").is_none()); // 拡張子なし
    }

    /// 読めなかったフォルダの範囲は走査範囲に入れない(入れると、その範囲の取り込み済みの行がすべて missing になる)。
    #[test]
    fn unreadable_dirs_are_not_in_scope() {
        let dir = std::env::temp_dir().join(format!("mv_scan_unreadable_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let member = dir.join("m");
        std::fs::create_dir_all(member.join("2026").join("202608")).unwrap();
        std::fs::write(member.join("2026").join("202608").join("1_0_20260801000000.txt"), "a").unwrap();

        // 読めたメンバーは全期間
        let (es, touched) = scan_member(&dir, &member, "m", &None, None);
        assert_eq!(es.len(), 1);
        assert!(touched.contains(&None));

        // メンバーフォルダが読めない(ここでは消えた)なら、何も走査しなかった扱い
        let (es, touched) = scan_member(&dir, &dir.join("gone"), "gone", &None, None);
        assert!(es.is_empty() && touched.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
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
