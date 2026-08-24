use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use axum::{
    Router, extract::FromRef, http::StatusCode, middleware::from_fn_with_state, routing::get,
};
use homescope_api_types::error::ApiErrorCode;
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
}

pub type AppRouter = Router<AppState>;

async fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        ApiErrorCode::NotFound,
        "provided path not found",
    )
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        ApiErrorCode::MethodNotAllowed,
        "http method is not available for that path",
    )
}

fn router(devices_registry: DeviceRegistry, admin_token: AdminToken) -> Router {
    let protected = Router::new()
        .nest("/devices", devices::router())
        .route_layer(from_fn_with_state(Arc::new(admin_token), require_bearer));

    let public = Router::new().route("/health", get(healthcheck));

    public
        .merge(protected)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(TraceLayer::new_for_http())
        .with_state(AppState { devices_registry })
}

pub async fn serve(
    devices_registry: DeviceRegistry,
    http_bind: &str,
    admin_token: AdminToken,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(http_bind)
        .await
        .with_context(|| format!("failed to bind {http_bind}"))?;

    axum::serve(
        listener,
        router(devices_registry, admin_token).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// Routing, wiring and the public/protected split.
///
/// Route paths are stringly typed and validated at runtime — `Router::route`
/// *panics* on a path that does not start with `/`, and axum performs no
/// trailing-slash redirection, so `…/rotate-key/` and `…/rotate-key` are
/// unrelated routes. Neither mistake is visible to `cargo check` or clippy,
/// and both have happened here. These tests are the only thing that sees them.
///
/// The lever is that `route_layer` applies **only after a route matches**: an
/// unauthenticated request to a registered path is a 401, and to anything else
/// a 404. So the status code alone reports whether a route exists, without a
/// database behind it.
#[cfg(test)]
mod test {
    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{Method, Request},
        response::Response,
    };
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt as _;

    use super::*;

    /// Nothing here reaches a handler, so the pool is never connected —
    /// `connect_lazy` dials on first use and there is no first use.
    fn app() -> Router {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1/unused")
            .expect("lazy pool");

        router(
            DeviceRegistry::for_test(pool.clone()),
            AdminToken::for_test(),
        )
    }

    async fn send(method: Method, uri: &str) -> Response {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .expect("valid request");

        // `serve` supplies this via `into_make_service_with_connect_info`;
        // `oneshot` bypasses that, and without it `require_bearer`'s
        // `ConnectInfo` extractor fails and every response is a 500.
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));

        app().oneshot(request).await.expect("infallible")
    }

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");

        serde_json::from_slice(&bytes).expect("body is json")
    }

    async fn status(method: Method, uri: &str) -> StatusCode {
        send(method, uri).await.status()
    }

    /// Constructing the router at all is half the test: an invalid path panics
    /// inside `Router::route` before any request is made.
    #[tokio::test]
    async fn the_device_routes_are_registered() {
        for (method, uri) in [
            (Method::POST, "/devices"),
            (Method::GET, "/devices"),
            (Method::GET, "/devices/AABBCCDDEEFF"),
            (Method::POST, "/devices/AABBCCDDEEFF/rotate-key"),
        ] {
            assert_eq!(
                status(method.clone(), uri).await,
                StatusCode::UNAUTHORIZED,
                "{method} {uri} did not resolve to a route"
            );
        }
    }

    /// The trailing-slash variants are *not* the same routes, so asserting the
    /// registered ones exist is not enough — this pins which spelling is the
    /// contract.
    #[tokio::test]
    async fn trailing_slash_variants_are_not_routes() {
        for uri in ["/devices/AABBCCDDEEFF/rotate-key/", "/health/"] {
            assert_eq!(
                status(Method::POST, uri).await,
                StatusCode::NOT_FOUND,
                "{uri} unexpectedly resolved"
            );
        }
    }

    #[tokio::test]
    async fn health_is_public() {
        assert_eq!(status(Method::GET, "/health").await, StatusCode::OK);
    }

    #[tokio::test]
    async fn an_unknown_path_is_a_json_404() {
        let response = send(Method::GET, "/nope").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["code"], "not_found");
    }

    /// Without `method_not_allowed_fallback` this is an empty-bodied 405 from
    /// axum, breaking the `{code, message}` contract on the one status a
    /// client is most likely to hit while learning the API.
    #[tokio::test]
    async fn a_wrong_method_is_a_json_405() {
        let response = send(Method::DELETE, "/health").await;

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(json_body(response).await["code"], "method_not_allowed");
    }
}
