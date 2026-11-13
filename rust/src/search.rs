//! 検索の純粋関数: 語の分解・FTS式の組み立て・LIKEエスケープ・スニペット生成。
//! (docs/api-spec.md 第4章「GET /api/search」)
//!
//! 語の分解とスニペットは `rust/tests/fixtures/search_golden.json` の期待値と照合している。
//!
//! 位置はすべて文字(Unicodeコードポイント)単位で扱う。Rust の `str` のバイト位置と混同しないこと。

/// 語の区切りの空白(Unicode の空白)。
fn is_space(c: char) -> bool {
    c.is_whitespace()
}

/// 検索文字列を (含める語, 除外する語) に分解する。
///
/// 正規表現 `(-?)"([^"]*)"|(-?)(\S+)` で先頭から順に照合するのと同じ規則:
/// - 各位置でまず「(-)"…"」(閉じ引用符まで。空白を含んでよい)を試す
/// - だめなら「(-)非空白の連続」。`-` の直後が空白/末尾なら `-` 自体が1語(除外ではない)
/// - 空の語(`""` など)は捨てる
pub fn parse_search_terms(q: &str) -> (Vec<String>, Vec<String>) {
    let c: Vec<char> = q.chars().collect();
    let n = c.len();
    let (mut inc, mut exc) = (Vec::new(), Vec::new());
    let mut push = |neg: bool, term: String| {
        if !term.is_empty() {
            if neg { exc.push(term) } else { inc.push(term) }
        }
    };

    let mut i = 0;
    while i < n {
        if is_space(c[i]) {
            i += 1;
            continue;
        }
        // 第1候補: -?"[^"]*"
        let (neg, j) = if c[i] == '-' { (true, i + 1) } else { (false, i) };
        if j < n && c[j] == '"' {
            if let Some(k) = (j + 1..n).find(|&k| c[k] == '"') {
                push(neg, c[j + 1..k].iter().collect());
                i = k + 1;
                continue;
            }
        }
        // 第2候補: -?\S+ (貪欲)。`-` の後に非空白が無ければ `-` を含めて1語
        let (neg, start) = if c[i] == '-' && i + 1 < n && !is_space(c[i + 1]) { (true, i + 1) } else { (false, i) };
        let mut end = start;
        while end < n && !is_space(c[end]) {
            end += 1;
        }
        push(neg, c[start..end].iter().collect());
        i = end;
    }
    (inc, exc)
}

/// trigram 索引で引ける語か(3文字以上)。
pub fn fts_indexable(t: &str) -> bool {
    t.chars().count() >= 3
}

/// 語を「FTS で引く語」と「LIKE で絞る語」に振り分けたもの。
#[derive(Debug, PartialEq, Eq)]
pub struct TermPlan {
    pub fts_include: Vec<String>,
    pub fts_exclude: Vec<String>,
    pub like_include: Vec<String>,
    pub like_exclude: Vec<String>,
}

impl TermPlan {
    /// FTS で候補を絞れるか(3文字以上の「含める語」が1つ以上ある)。
    pub fn uses_fts(&self) -> bool {
        !self.fts_include.is_empty()
    }
}

/// trigram は3文字未満を索引できないため、3文字以上の語は FTS で、それ未満の語は LIKE で扱う。
/// 3文字以上の「含める語」が1つも無ければ FTS は使えない(全語を LIKE で扱う)。
/// (以前は2文字以下の語が1つでも混ざると全体を LIKE にしていたため、全件走査になっていた)
pub fn plan_terms(includes: &[String], excludes: &[String]) -> TermPlan {
    let split = |v: &[String]| -> (Vec<String>, Vec<String>) { v.iter().cloned().partition(|t| fts_indexable(t)) };
    let (fts_include, like_include) = split(includes);
    if fts_include.is_empty() {
        return TermPlan {
            fts_include,
            fts_exclude: Vec::new(),
            like_include,
            like_exclude: excludes.to_vec(),
        };
    }
    let (fts_exclude, like_exclude) = split(excludes);
    TermPlan { fts_include, fts_exclude, like_include, like_exclude }
}

/// FTS5 の MATCH 式。各語を `"…"`(`"` は `""`)にして AND 連結し、除外語は NOT で繋ぐ。
pub fn build_fts_query(includes: &[String], excludes: &[String]) -> String {
    let q = |t: &String| format!("\"{}\"", t.replace('"', "\"\""));
    let mut expr = includes.iter().map(q).collect::<Vec<_>>().join(" AND ");
    for t in excludes {
        expr.push_str(" NOT ");
        expr.push_str(&q(t));
    }
    expr
}

/// LIKE のワイルドカード(`%` `_`)を文字どおりに扱うためのエスケープ(`ESCAPE '\'` と併用)。
pub fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

pub const SNIPPET_CONTEXT: usize = 40;
pub const SNIPPET_MAX: usize = 300;
const ELLIPSIS: &str = "...";

/// HTML エスケープ(`& < > " '`)。
pub fn html_escape(s: &[char]) -> String {
    let mut out = String::with_capacity(s.len());
    for &ch in s {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 大文字小文字を区別しない照合用に、1文字を1文字へ畳む。
///
/// 文字数を変えないことが大事: 畳んだ文字列での位置を、そのまま元の本文の位置として使うため。
/// (`str::to_lowercase` は İ を `i̇` の2文字にするので、位置がずれてマークや抜粋の範囲を誤る)
/// 小文字化で複数文字になるものは先頭の1文字を使い(İ→i)、語末シグマ ς は σ にそろえる。
fn fold_char(c: char) -> char {
    match c.to_lowercase().next().unwrap_or(c) {
        'ς' => 'σ',
        l => l,
    }
}

fn fold_chars(s: &[char]) -> Vec<char> {
    s.iter().map(|&c| fold_char(c)).collect()
}

fn fold_str(s: &str) -> Vec<char> {
    s.chars().map(fold_char).collect()
}

/// `hay` の中で `needle` が最初に現れる文字位置。
fn find_chars(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// `s[a..b]`(範囲外は丸める)。
fn slice(s: &[char], a: usize, b: usize) -> &[char] {
    let a = a.min(s.len());
    let b = b.min(s.len()).max(a);
    &s[a..b]
}

/// 含める語のうち、本文(畳み済み)で最も早く現れる位置。
fn find_first_term(body_folded: &[char], terms: &[String]) -> Option<usize> {
    terms.iter().filter_map(|t| find_chars(body_folded, &fold_str(t))).min()
}

/// `text` の中で強調する [開始, 終了) 区間を、重なりなく前から順に返す。長い語を優先する。
fn find_mark_spans(text: &[char], includes: &[String]) -> Vec<(usize, usize)> {
    let mut terms: Vec<&String> = Vec::new();
    for t in includes {
        if !t.is_empty() && !terms.contains(&t) {
            terms.push(t);
        }
    }
    terms.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
    if terms.is_empty() {
        return Vec::new();
    }
    // 畳んでも文字数は変わらないので、畳んだ文字列での位置と長さがそのまま元の text での範囲になる
    let folded = fold_chars(text);
    let needles: Vec<Vec<char>> = terms.iter().map(|t| fold_str(t)).collect();

    let mut spans = Vec::new();
    let mut i = 0;
    while i < text.len() {
        match needles.iter().find(|n| folded[i..].starts_with(n)) {
            Some(n) => {
                spans.push((i, i + n.len()));
                i += n.len();
            }
            None => i += 1,
        }
    }
    spans
}

/// 本文からヒット箇所周辺を切り出し、該当語を `<mark>` で囲んで返す。`<mark>` 以外は HTML エスケープ済み。
pub fn make_snippet(body: Option<&str>, includes: &[String]) -> String {
    let Some(body) = body.filter(|b| !b.is_empty()) else {
        return String::new();
    };
    let body: Vec<char> = body.chars().collect();
    let Some(hit) = find_first_term(&fold_chars(&body), includes) else {
        // 語が本文中に無い(除外のみの検索など)。先頭を上限まで見せる
        let mut out = html_escape(slice(&body, 0, SNIPPET_MAX));
        if body.len() > SNIPPET_MAX {
            out.push_str(ELLIPSIS);
        }
        return out;
    };

    let start = hit.saturating_sub(SNIPPET_CONTEXT);
    let end = body.len().min(start + SNIPPET_MAX);
    let excerpt = slice(&body, start, end);

    // 素のテキスト上でマーク範囲を決めてから、エスケープしつつ組み立てる
    // (エスケープ後に置換すると、タグや実体参照 &lt; への誤マッチを招くため)
    let mut out = String::new();
    let mut pos = 0;
    for (a, b) in find_mark_spans(excerpt, includes) {
        out.push_str(&html_escape(slice(excerpt, pos, a)));
        out.push_str("<mark>");
        out.push_str(&html_escape(slice(excerpt, a, b)));
        out.push_str("</mark>");
        pos = b;
    }
    out.push_str(&html_escape(slice(excerpt, pos, usize::MAX)));

    if start > 0 {
        out.insert_str(0, ELLIPSIS);
    }
    if end < body.len() {
        out.push_str(ELLIPSIS);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn golden() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/search_golden.json")).unwrap()
    }

    fn strs(v: &Value) -> Vec<String> {
        v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
    }

    /// golden の期待値を作った後で、意図して挙動を変えた文字を含むか(そのケースは照合しない)。
    /// - İ・Σσς: 期待値は小文字化で文字数が変わる/文脈で変わることで、抜粋とマークの位置がずれていた
    /// - U+001C〜U+001F: 期待値は語の区切りの空白として扱っていた(今は Unicode の空白だけ)
    fn changed_on_purpose(s: &str) -> bool {
        s.chars().any(|c| matches!(c, 'İ' | 'Σ' | 'σ' | 'ς' | '\u{1c}'..='\u{1f}'))
    }

    #[test]
    fn parse_matches_golden() {
        let g = golden();
        let cases = g["parse"].as_array().unwrap();
        let mut bad = Vec::new();
        for c in cases {
            let q = c["q"].as_str().unwrap();
            if changed_on_purpose(q) {
                continue;
            }
            let (inc, exc) = parse_search_terms(q);
            // FTS を使うかの判定は期待値の後で変えた(短い語を LIKE に回す)ため、ここでは語の分解と FTS 式だけを照合する
            let ok = inc == strs(&c["include"])
                && exc == strs(&c["exclude"])
                && (inc.is_empty() || Some(build_fts_query(&inc, &exc).as_str()) == c["fts"].as_str());
            if !ok {
                bad.push(format!("{q:?}: got {inc:?} {exc:?}, want {} {}", c["include"], c["exclude"]));
            }
        }
        assert!(bad.is_empty(), "{} / {} mismatches:\n{}", bad.len(), cases.len(), bad.join("\n"));
    }

    #[test]
    fn snippet_matches_golden() {
        let g = golden();
        let cases = g["snippet"].as_array().unwrap();
        let mut bad = Vec::new();
        for c in cases {
            let body = c["body"].as_str().unwrap();
            let terms = strs(&c["terms"]);
            if changed_on_purpose(body) || terms.iter().any(|t| changed_on_purpose(t)) {
                continue;
            }
            let got = make_snippet(Some(body), &terms);
            let want = c["out"].as_str().unwrap();
            if got != want {
                bad.push(format!("body={body:?} terms={terms:?}\n  got  {got:?}\n  want {want:?}"));
            }
        }
        assert!(bad.is_empty(), "{} / {} mismatches:\n{}", bad.len(), cases.len(), bad.join("\n"));
    }

    #[test]
    fn like_escape_matches_golden() {
        for c in golden()["like"].as_array().unwrap() {
            assert_eq!(like_escape(c["s"].as_str().unwrap()), c["out"].as_str().unwrap());
        }
    }

    #[test]
    fn term_plan() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // 長い語と短い語の混在: 長い語は FTS、短い語は LIKE
        let p = plan_terms(&s(&["ライブ", "楽し"]), &s(&["中止です", "雨"]));
        assert_eq!(p.fts_include, s(&["ライブ"]));
        assert_eq!(p.fts_exclude, s(&["中止です"]));
        assert_eq!(p.like_include, s(&["楽し"]));
        assert_eq!(p.like_exclude, s(&["雨"]));
        assert!(p.uses_fts());
        // 長い「含める語」が無ければ、長い除外語も含めて全部 LIKE
        let p = plan_terms(&s(&["楽し"]), &s(&["中止です"]));
        assert!(!p.uses_fts());
        assert_eq!((p.like_include, p.like_exclude), (s(&["楽し"]), s(&["中止です"])));
        // 除外語だけ
        assert!(!plan_terms(&[], &s(&["中止です"])).uses_fts());
        // 全部長い語なら従来どおり全部 FTS
        let p = plan_terms(&s(&["ライブ"]), &s(&["中止です"]));
        assert!(p.uses_fts() && p.like_include.is_empty() && p.like_exclude.is_empty());
    }

    #[test]
    fn snippet_none_body() {
        assert_eq!(make_snippet(None, &["a".into()]), "");
    }

    fn html_unescape(s: &str) -> String {
        s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#x27;", "'").replace("&amp;", "&")
    }

    /// 期待値の無い入力も含め、抜粋が満たすべき性質を確かめる(golden の全ケースの本文と語で)。
    /// - 抜粋からタグと「...」を外すと、本文の連続した一部になる
    /// - `<mark>` で囲んだ部分は、どれも検索語のどれかと(大文字小文字を除いて)一致する
    /// - 語が本文にあれば、少なくとも1か所はマークする
    #[test]
    fn snippet_invariants() {
        let g = golden();
        for c in g["snippet"].as_array().unwrap() {
            let body = c["body"].as_str().unwrap();
            let terms = strs(&c["terms"]);
            let out = make_snippet(Some(body), &terms);
            let ctx = || format!("body={body:?} terms={terms:?}\n out={out:?}");

            let inner = out.strip_prefix(ELLIPSIS).unwrap_or(&out);
            let inner = inner.strip_suffix(ELLIPSIS).unwrap_or(inner);
            let plain = html_unescape(&inner.replace("<mark>", "").replace("</mark>", ""));
            assert!(body.contains(&plain), "抜粋が本文の一部でない: {}", ctx());

            let folded_terms: Vec<Vec<char>> = terms.iter().filter(|t| !t.is_empty()).map(|t| fold_str(t)).collect();
            let mut marks = 0;
            for seg in inner.split("<mark>").skip(1) {
                let marked = html_unescape(seg.split("</mark>").next().unwrap());
                assert!(folded_terms.contains(&fold_str(&marked)), "検索語でない部分をマーク({marked:?}): {}", ctx());
                marks += 1;
            }
            let body_folded = fold_str(body);
            if folded_terms.iter().any(|t| find_chars(&body_folded, t).is_some()) {
                assert!(marks > 0, "語があるのにマークが無い: {}", ctx());
            }
        }
    }

    /// 小文字化で文字数が変わる İ があっても、抜粋とマークの位置がずれない。
    #[test]
    fn snippet_positions_with_dotted_i() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(make_snippet(Some("İİİİ abc"), &s(&["abc"])), "İİİİ <mark>abc</mark>");
        assert_eq!(make_snippet(Some("İstanbul İzmir"), &s(&["istanbul"])), "<mark>İstanbul</mark> İzmir");
        assert_eq!(make_snippet(Some("xİabcİ"), &s(&["abc"])), "xİ<mark>abc</mark>İ");
        // 抜粋の開始位置も、元の本文での位置から決まる
        let body = format!("{}{}target", "İ".repeat(100), "x".repeat(10));
        let out = make_snippet(Some(&body), &s(&["target"]));
        assert!(out.starts_with("...") && out.ends_with("<mark>target</mark>"), "{out}");
        assert_eq!(out.chars().filter(|&c| c == 'İ').count(), 30); // 語の前 40 文字 = İ 30 + x 10
    }

    /// シグマは大文字・小文字・語末形(Σ σ ς)を同じ文字として照合する。
    #[test]
    fn snippet_sigma_forms() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(make_snippet(Some("ΟΔΟΣ οδος"), &s(&["οδοσ"])), "<mark>ΟΔΟΣ</mark> <mark>οδος</mark>");
        assert_eq!(make_snippet(Some("a σς b"), &s(&["ΣΣ"])), "a <mark>σς</mark> b");
    }
}
