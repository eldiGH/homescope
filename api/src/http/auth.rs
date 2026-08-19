use std::{net::SocketAddr, path::Path, sync::Arc};

use anyhow::{Context as _, bail};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use subtle::ConstantTimeEq as _;
use tokio::fs;
use tracing::{instrument, warn};
use zeroize::Zeroizing;

use crate::http::error::ApiError;

const MIN_ADMIN_TOKEN_LEN: usize = 32;

pub struct AdminToken {
    token: Zeroizing<String>,
}

impl AdminToken {
    pub async fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read_to_string(path)
            .await
            .with_context(|| format!("couldn't read admin token file: {}", path.display()))?;
        let token = contents
            .lines()
            .next()
            .with_context(|| format!("admin token file `{}` is empty", path.display()))?
            .trim();

        if token.len() < MIN_ADMIN_TOKEN_LEN {
            bail!("admin token is too short: {}", path.display());
        }

        Ok(Self {
            token: Zeroizing::new(token.to_owned()),
        })
    }

    pub fn verify(&self, other: &str) -> bool {
        self.token.as_bytes().ct_eq(other.as_bytes()).into()
    }
}

impl std::fmt::Debug for AdminToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AdminToken(<redacted>)")
    }
}

fn unauthorized() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "you are not authorized to perform that operation",
    )
    .add_header(
        axum::http::header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer"),
    )
}

#[instrument(skip_all, fields(client_addr = %connection))]
pub async fn require_bearer(
    State(admin_token): State<Arc<AdminToken>>,
    ConnectInfo(connection): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let Some(bearer) = request.headers().get("Authorization") else {
        warn!("no authorization header");
        return unauthorized().into_response();
    };

    let Ok(bearer) = bearer.to_str() else {
        warn!("couldn't convert authorization header's value to string");
        return unauthorized().into_response();
    };

    let Some(token) = bearer.strip_prefix("Bearer ") else {
        warn!("token has invalid prefix (Bearer)");
        return unauthorized().into_response();
    };

    if !admin_token.verify(token.trim_end()) {
        warn!("invalid token");
        return unauthorized().into_response();
    }

    next.run(request).await
}
