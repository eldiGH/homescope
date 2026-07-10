use homescope_common::reading::SensorReading;
use sqlx::PgPool;
use tokio::sync::mpsc::{Receiver, channel};
use tracing::{debug, error, level_filters::LevelFilter};
use tracing_subscriber::EnvFilter;

mod config;
mod db;
mod mqtt;

async fn store_readings(pool: PgPool, mut readings_receiver: Receiver<SensorReading>) {
    while let Some(reading) = readings_receiver.recv().await {
        debug!("reading to insert: {reading}");

        if let Err(err) = db::insert_reading(&pool, &reading).await {
            error!("db error: {err}");
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config = config::ApiConfig::from_env()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    let pool = db::connect(&config.db_url, config.db_max_connections).await?;

    if config.run_migrations {
        sqlx::migrate!().run(&pool).await?;
    }

    let (readings_sender, readings_receiver) = channel::<SensorReading>(256);

    tokio::spawn(store_readings(pool.clone(), readings_receiver));

    mqtt::run(&config.mqtt_host, config.mqtt_port, readings_sender).await?;

    Ok(())
}
