use axum::http::StatusCode;
use serde::Serialize;

use crate::http::extract::Json;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    status: &'static str,
}

pub async fn healthcheck() -> (StatusCode, Json<Health>) {
    const HEALTH: Health = Health { status: "ok" };

    (StatusCode::OK, Json(HEALTH))
}
