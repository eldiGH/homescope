use tracing_subscriber::{EnvFilter, filter::LevelFilter};

pub mod env;

pub fn init() {
    dotenvy::dotenv().ok();
    dotenvy::from_filename(".env.default").ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();
}
