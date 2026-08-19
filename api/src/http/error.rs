use axum::{
    Json,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use tracing::error;

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    headers: Vec<(HeaderName, HeaderValue)>,
}

/// The wire shape. Separate from `ApiError` because `StatusCode` isn't
/// `Serialize` and because the body is a contract — it should change only
/// deliberately, not as a side effect of adding a field to the struct.
#[derive(Serialize)]
struct ApiErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            headers: Default::default(),
        }
    }

    pub fn add_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.push((name, value));
        self
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal server error",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            self.headers.into_iter().collect::<HeaderMap>(),
            Json(ApiErrorBody {
                code: self.code,
                message: &self.message,
            }),
        )
            .into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        error!(%err, "db error");
        ApiError::internal()
    }
}
