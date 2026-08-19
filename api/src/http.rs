use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use axum::{
    Router, extract::FromRef, http::StatusCode, middleware::from_fn_with_state, routing::get,
};
use sqlx::PgPool;
use tower_http::trace::TraceLayer;

use crate::{
    devices::{self, DeviceRegistry},
    http::{auth::require_bearer, error::ApiError, health::healthcheck},
};

mod auth;
pub use auth::AdminToken;
pub mod error;
pub mod extract;
mod health;

#[derive(Clone, FromRef)]
pub struct AppState {
    devices_registry: DeviceRegistry,
    pool: PgPool,
}

pub type AppRouter = Router<AppState>;

async fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        "provided path not found",
    )
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "http method is not available for that path",
    )
}

fn router(devices_registry: DeviceRegistry, admin_token: AdminToken, pool: PgPool) -> Router {
    let protected = Router::new()
        .nest("/devices", devices::router())
        .route_layer(from_fn_with_state(Arc::new(admin_token), require_bearer));

    let public = Router::new().route("/health", get(healthcheck));

    public
        .merge(protected)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(TraceLayer::new_for_http())
        .with_state(AppState {
            devices_registry,
            pool,
        })
}

pub async fn serve(
    devices_registry: DeviceRegistry,
    http_bind: &str,
    admin_token: AdminToken,
    pool: PgPool,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(http_bind)
        .await
        .with_context(|| format!("failed to bind {http_bind}"))?;

    axum::serve(
        listener,
        router(devices_registry, admin_token, pool)
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
