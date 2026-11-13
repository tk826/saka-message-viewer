//! クエリパラメータの取り出しと検証(型・範囲違反は422)。
//!
//! 同じキーが複数あれば最後の値を採用する。

use crate::error::{ApiError, ApiResult};
use std::collections::HashMap;

pub struct Params(HashMap<String, String>);

impl Params {
    pub fn parse(raw: Option<String>) -> Self {
        let mut m = HashMap::new();
        if let Some(q) = raw {
            for (k, v) in form_urlencoded::parse(q.as_bytes()) {
                m.insert(k.into_owned(), v.into_owned());
            }
        }
        Params(m)
    }

    /// 必須の文字列。無ければ422。
    pub fn required(&self, name: &str) -> ApiResult<String> {
        self.0
            .get(name)
            .cloned()
            .ok_or_else(|| ApiError::validation("missing", name, "Field required"))
    }

    /// 任意の文字列。空文字も `Some("")` として返す(呼び出し側で falsy 扱いするか決める)。
    pub fn opt(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }

    pub fn opt_int(&self, name: &str) -> ApiResult<Option<i64>> {
        match self.0.get(name) {
            None => Ok(None),
            Some(v) => v
                .trim()
                .parse::<i64>()
                .map(Some)
                .map_err(|_| ApiError::validation("int_parsing", name, "Input should be a valid integer")),
        }
    }

    /// 必須の整数。無ければ/整数でなければ422。
    pub fn required_int(&self, name: &str) -> ApiResult<i64> {
        self.opt_int(name)?
            .ok_or_else(|| ApiError::validation("missing", name, "Field required"))
    }

    /// `limit` のように範囲付きの整数。範囲外は422。
    pub fn int_range(&self, name: &str, default: i64, min: i64, max: i64) -> ApiResult<i64> {
        let v = self.opt_int(name)?.unwrap_or(default);
        if v < min {
            return Err(ApiError::validation("greater_than_equal", name, "Input should be greater than or equal to the minimum"));
        }
        if v > max {
            return Err(ApiError::validation("less_than_equal", name, "Input should be less than or equal to the maximum"));
        }
        Ok(v)
    }
}
