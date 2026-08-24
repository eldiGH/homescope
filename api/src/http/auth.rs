use std::{net::SocketAddr, path::Path, sync::Arc};

use anyhow::{Context as _, bail};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use homescope_api_types::error::ApiErrorCode;
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

    /// A token for routing tests, which need the guard in place but never
    /// present a credential. [`load`](Self::load) is the only production
    /// constructor and it reads a file; this keeps tests that are not about
    /// loading from having to write one.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self {
            token: Zeroizing::new("x".repeat(MIN_ADMIN_TOKEN_LEN)),
        }
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
        ApiErrorCode::Unauthorized,
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

#[cfg(test)]
mod test {
    use axum::{Router, body::Body, middleware::from_fn_with_state, routing::get};
    use tower::ServiceExt as _;

    use super::*;

    const TOKEN: &str = "8f14e45fceea167a5a36dedd4bea2543f14e45fceea167a5a36dedd4bea25431";

    /// The `TempDir` is returned, not dropped, because dropping it deletes the
    /// directory — callers must hold it for as long as the path is used.
    async fn token_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("admin.token");
        fs::write(&path, contents).await.expect("write token file");
        (dir, path)
    }

    // ---- loading ---------------------------------------------------------

    #[tokio::test]
    async fn load_reads_the_token() {
        let (_dir, path) = token_file(&format!("{TOKEN}\n")).await;

        assert!(
            AdminToken::load(&path)
                .await
                .expect("valid token file")
                .verify(TOKEN)
        );
    }

    /// A file holding nothing but a newline yields `Some("")` from `lines()`,
    /// not `None` — so the emptiness guard alone does not catch it, and an
    /// empty stored token would authenticate `Authorization: Bearer `.
    /// A truncated secret write must fail startup, not disable auth.
    #[tokio::test]
    async fn load_rejects_a_blank_first_line() {
        let (_dir, path) = token_file("\n").await;

        assert!(AdminToken::load(&path).await.is_err());
    }

    #[tokio::test]
    async fn load_rejects_a_short_token() {
        let (_dir, path) = token_file("tooshort\n").await;

        assert!(AdminToken::load(&path).await.is_err());
    }

    #[tokio::test]
    async fn load_rejects_a_missing_file() {
        let dir = tempfile::tempdir().expect("temp dir");

        assert!(AdminToken::load(&dir.path().join("absent")).await.is_err());
    }

    /// Trimming happens on load as well as on the presented value; a secret
    /// file with stray whitespace would otherwise reject the correct token
    /// forever.
    #[tokio::test]
    async fn load_trims_and_takes_only_the_first_line() {
        let (_dir, path) = token_file(&format!("  {TOKEN}  \nignored\n")).await;

        assert!(
            AdminToken::load(&path)
                .await
                .expect("valid token file")
                .verify(TOKEN)
        );
    }

    #[tokio::test]
    async fn debug_does_not_render_the_token() {
        let (_dir, path) = token_file(&format!("{TOKEN}\n")).await;
        let token = AdminToken::load(&path).await.expect("valid token file");

        let rendered = format!("{token:?}");
        assert!(!rendered.contains(TOKEN), "leaked token: {rendered}");
    }

    #[tokio::test]
    async fn verify_rejects_a_near_miss() {
        let (_dir, path) = token_file(&format!("{TOKEN}\n")).await;
        let token = AdminToken::load(&path).await.expect("valid token file");

        let mut wrong = TOKEN.to_owned();
        wrong.replace_range(0..1, "0");

        assert!(!token.verify(&wrong));
        assert!(!token.verify(&TOKEN[..TOKEN.len() - 1]));
        assert!(!token.verify(""));
    }

    // ---- middleware ------------------------------------------------------

    /// The middleware over a trivial handler: 200 means the request got
    /// through, anything else means it was stopped here.
    async fn guarded(authorization: Option<&str>) -> Response {
        let (_dir, path) = token_file(&format!("{TOKEN}\n")).await;
        let admin_token = Arc::new(AdminToken::load(&path).await.expect("valid token file"));

        let app = Router::new()
            .route("/", get(async || StatusCode::OK))
            .route_layer(from_fn_with_state(admin_token, require_bearer));

        let mut request = Request::builder().uri("/");
        if let Some(value) = authorization {
            request = request.header("Authorization", value);
        }
        let mut request = request.body(Body::empty()).expect("valid request");

        // `ConnectInfo` is normally supplied by
        // `into_make_service_with_connect_info`; `oneshot` bypasses that, so
        // the extension has to be inserted by hand or extraction fails.
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));

        app.oneshot(request).await.expect("infallible")
    }

    #[tokio::test]
    async fn a_valid_token_passes_through() {
        assert_eq!(
            guarded(Some(&format!("Bearer {TOKEN}"))).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_missing_header_is_rejected() {
        assert_eq!(guarded(None).await.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_wrong_token_is_rejected() {
        assert_eq!(
            guarded(Some(&format!("Bearer {}", "0".repeat(TOKEN.len()))))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// The token alone, or under another scheme, must not be accepted — the
    /// prefix check is what stops `Basic <base64>` from being compared raw.
    #[tokio::test]
    async fn a_wrong_scheme_is_rejected() {
        assert_eq!(
            guarded(Some(TOKEN)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            guarded(Some(&format!("Basic {TOKEN}"))).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// RFC 7235 requires it on a 401, and it is served through `ApiError`, so
    /// this also covers `ApiError` actually emitting the headers it carries.
    #[tokio::test]
    async fn a_rejection_carries_www_authenticate() {
        assert_eq!(
            guarded(None)
                .await
                .headers()
                .get(axum::http::header::WWW_AUTHENTICATE)
                .expect("WWW-Authenticate must be set on a 401"),
            "Bearer"
        );
    }
}
