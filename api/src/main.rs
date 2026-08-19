use crate::{devices::DeviceRegistry, http::AdminToken};

mod config;
mod db;
mod devices;
mod http;
mod ingest;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    homescope_host_util::init();

    let config = config::ApiConfig::from_env()?;

    let pool = db::connect(&config).await?;

    if config.run_migrations {
        sqlx::migrate!().run(&pool).await?;
    }

    let devices = DeviceRegistry::load(pool.clone(), &config.kek_path).await?;
    let admin_token = AdminToken::load(&config.admin_token_path).await?;

    tokio::select! {
        r = ingest::run(&config, pool.clone(), devices.clone()) => r.map(|never| match never {}),
        r = http::serve(devices, &config.http_bind, admin_token, pool) => r
    }
}
