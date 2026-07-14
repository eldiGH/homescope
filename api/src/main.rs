use std::convert::Infallible;

use crate::devices::DeviceRegistry;

mod config;
mod db;
mod devices;
mod ingest;

#[tokio::main]
async fn main() -> anyhow::Result<Infallible> {
    homescope_host_util::init();

    let config = config::ApiConfig::from_env()?;

    let pool = db::connect(&config).await?;

    if config.run_migrations {
        sqlx::migrate!().run(&pool).await?;
    }

    let devices = DeviceRegistry::load(pool.clone()).await?;

    tokio::select! { r = ingest::run(&config, pool, devices) => {r} }
}
