use homescope_common::reading::SensorReading;
use sqlx::PgPool;
use tokio::sync::mpsc::{Receiver, channel};
use tracing::{debug, error};

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
    homescope_host_util::init();

    let config = config::ApiConfig::from_env()?;

    let pool = db::connect(&config).await?;

    if config.run_migrations {
        sqlx::migrate!().run(&pool).await?;
    }

    let (readings_sender, readings_receiver) = channel::<SensorReading>(256);

    tokio::spawn(store_readings(pool.clone(), readings_receiver));

    mqtt::run(&config.mqtt_host, config.mqtt_port, readings_sender).await?;

    Ok(())
}
