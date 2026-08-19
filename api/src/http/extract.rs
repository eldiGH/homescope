use axum::{
    extract::{FromRequest, FromRequestParts},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Serialize, de::DeserializeOwned};

use crate::http::error::ApiError;

pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Json::<T>::from_request(req, state)
            .await
            .map(|axum::extract::Json(v)| Self(v))
            .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_body", r.body_text()))
    }
}

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> axum::response::Response {
        axum::extract::Json::<T>(self.0).into_response()
    }
}

pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send + Sync,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Path(v)| Self(v))
            .map_err(|r| ApiError::new(StatusCode::BAD_REQUEST, "invalid_params", r.body_text()))
    }
}
