//! APIエラー(docs/api-spec.md 第0章)。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Value,
}

impl ApiError {
    /// `{"detail": "<msg>"}` 形式の業務エラー。
    pub fn detail(status: StatusCode, msg: impl Into<String>) -> Self {
        Self { status, body: json!({ "detail": msg.into() }) }
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::detail(StatusCode::BAD_REQUEST, msg)
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::detail(StatusCode::FORBIDDEN, msg)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::detail(StatusCode::NOT_FOUND, msg)
    }
    /// 入力検証エラー(422)。`{"detail": [{"type", "loc", "msg"}]}` の形。
    pub fn validation(kind: &str, loc: &str, msg: &str) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: json!({ "detail": [{ "type": kind, "loc": ["query", loc], "msg": msg }] }),
        }
    }
    /// JSONオブジェクトでないボディ(不正JSON・配列など)は422。
    pub fn validation_body() -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            body: json!({ "detail": [{ "type": "model_attributes_type", "loc": ["body"], "msg": "Input should be a valid dictionary" }] }),
        }
    }
    /// `{"status": "<s>"}` 形式の409(root_not_configured 等)。
    pub fn status_conflict(status: &str) -> Self {
        Self { status: StatusCode::CONFLICT, body: json!({ "status": status }) }
    }
    /// 取り込み・削除・初期化が実行中の409。`detail` があれば添える(別プロセスの取り込みの PID など)。
    pub fn already_running(detail: Option<String>) -> Self {
        let mut e = Self::status_conflict("already_running");
        if let Some(d) = detail {
            e.body["detail"] = json!(d);
        }
        e
    }
    /// 想定外の失敗(500)。
    pub fn internal(e: impl std::fmt::Display) -> Self {
        Self::detail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

/// ブロッキング処理(ファイル・DB)を専用スレッドで実行する。スレッドが panic したら500。
pub async fn blocking<T, F>(f: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(ApiError::internal)
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        logln!("[db error] {e}");
        ApiError::internal(e)
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
