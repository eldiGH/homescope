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

#[cfg(test)]
mod test {
    use axum::body::to_bytes;

    use super::*;

    async fn body_of(err: ApiError) -> serde_json::Value {
        let body = err.into_response().into_body();
        let bytes = to_bytes(body, usize::MAX).await.expect("body");

        serde_json::from_slice(&bytes).expect("body is json")
    }

    /// `code` is the contract — homescope-provision branches on it, so it is
    /// asserted literally rather than through a helper.
    #[tokio::test]
    async fn renders_code_and_message() {
        let err = ApiError::new(StatusCode::CONFLICT, "device_already_exists", "and so on");

        assert_eq!(
            body_of(err).await,
            serde_json::json!({ "code": "device_already_exists", "message": "and so on" })
        );
    }

    #[test]
    fn keeps_its_status() {
        let response = ApiError::new(StatusCode::NOT_FOUND, "nope", "nope").into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// Regression: `headers` was collected into the struct and then dropped on
    /// the floor by `into_response`, so `WWW-Authenticate` never reached a
    /// client. Nothing about that is visible without inspecting a response.
    #[test]
    fn emits_the_headers_it_carries() {
        let response = ApiError::internal()
            .add_header(
                HeaderName::from_static("x-one"),
                HeaderValue::from_static("1"),
            )
            .add_header(
                HeaderName::from_static("x-two"),
                HeaderValue::from_static("2"),
            )
            .into_response();

        assert_eq!(response.headers().get("x-one").expect("x-one"), "1");
        assert_eq!(response.headers().get("x-two").expect("x-two"), "2");
    }

    /// A 500 must describe nothing. `sqlx::Error`'s `Display` names tables and
    /// constraints, and this is the conversion every db failure funnels
    /// through — the detail belongs in the log, not the body.
    #[tokio::test]
    async fn a_db_error_is_not_echoed_to_the_client() {
        let err = ApiError::from(sqlx::Error::RowNotFound);

        assert_eq!(
            body_of(err).await,
            serde_json::json!({ "code": "internal_error", "message": "internal server error" })
        );
    }
}
