//! 日付変換。ファイル名の時刻はUTC、日付指定(from/to/date/カレンダー)はJST(+9h)。
//! (docs/api-spec.md 第1章)

use crate::error::{ApiError, ApiResult};
use chrono::NaiveDate;

/// `YYYY-MM-DD` の日付をパースする。
/// 年は4桁(0000は不可)、月日は1〜2桁(`2026-8-2` も可)。空白・符号は不可。数字は ASCII のみ。
fn parse_ymd(s: &str) -> Option<NaiveDate> {
    let mut it = s.split('-');
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    let digits = |t: &str, min: usize, max: usize| {
        (min..=max).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit())
    };
    if !(y.len() == 4 && digits(y, 4, 4) && digits(m, 1, 2) && digits(d, 1, 2)) {
        return None;
    }
    NaiveDate::from_ymd_opt(y.parse().ok().filter(|&v| v >= 1)?, m.parse().ok()?, d.parse().ok()?)
}

/// `YYYY-MM-DD`(JST) の 00:00:00〜23:59:59 を unix epoch 範囲に変換する。
/// 不正な日付は400。
pub fn jst_date_to_epoch_range(date: &str) -> ApiResult<(i64, i64)> {
    let d = parse_ymd(date).ok_or_else(|| {
        ApiError::bad_request(format!("invalid date format: {date}, expected YYYY-MM-DD"))
    })?;
    let start_utc = d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() - 9 * 3600;
    Ok((start_utc, start_utc + 86400 - 1))
}

/// `YYYY-MM`(JST) の月初 00:00:00 〜 月末 23:59:59 を unix epoch 範囲に変換する。実在しない月は None。
pub fn jst_month_epoch_range(ym: &str) -> Option<(i64, i64)> {
    let (y, m) = ym.split_once('-')?;
    let (y, m): (i32, u32) = (y.parse().ok()?, m.parse().ok()?);
    let first = NaiveDate::from_ymd_opt(y, m, 1)?;
    let next = if m == 12 { NaiveDate::from_ymd_opt(y + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(y, m + 1, 1)? };
    let epoch = |d: NaiveDate| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() - 9 * 3600;
    Some((epoch(first), epoch(next) - 1))
}

/// `^\d{4}-\d{2}$`(ASCII数字のみ)。
pub fn is_yearmonth(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7 && b[4] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_range() {
        // 2026-08-02 JST 00:00 = 2026-08-01T15:00Z
        assert_eq!(jst_date_to_epoch_range("2026-08-02").unwrap(), (1785596400, 1785596400 + 86399));
    }

    #[test]
    fn invalid_dates() {
        assert!(jst_date_to_epoch_range("bad").is_err());
        assert!(jst_date_to_epoch_range("2026/08/01").is_err());
        assert!(jst_date_to_epoch_range("2026-02-30").is_err());
        assert!(jst_date_to_epoch_range(" 2026-08-02").is_err());
        assert!(jst_date_to_epoch_range("+2026-08-02").is_err());
        assert!(jst_date_to_epoch_range("0000-01-01").is_err());
        assert!(jst_date_to_epoch_range("2026-+8-02").is_err());
        assert!(jst_date_to_epoch_range("2026-08-02-01").is_err());
    }

    #[test]
    fn lenient_padding() {
        assert_eq!(
            jst_date_to_epoch_range("2026-8-2").unwrap(),
            jst_date_to_epoch_range("2026-08-02").unwrap()
        );
    }

    #[test]
    fn month_range() {
        // 2026-08 JST = 2026-07-31T15:00Z 〜 2026-08-31T14:59:59Z
        let (a, b) = jst_month_epoch_range("2026-08").unwrap();
        assert_eq!(a, jst_date_to_epoch_range("2026-08-01").unwrap().0);
        assert_eq!(b, jst_date_to_epoch_range("2026-08-31").unwrap().1);
        let (_, b) = jst_month_epoch_range("2026-12").unwrap();
        assert_eq!(b, jst_date_to_epoch_range("2026-12-31").unwrap().1);
        assert_eq!(jst_month_epoch_range("2026-13"), None);
        assert_eq!(jst_month_epoch_range("2026-00"), None);
    }

    #[test]
    fn yearmonth() {
        assert!(is_yearmonth("2026-08"));
        assert!(!is_yearmonth("2026-8"));
        assert!(!is_yearmonth("2026-08-01"));
        assert!(!is_yearmonth(""));
    }
}
