use anyhow::Context as _;
use homescope_common::{
    reading::SensorReading,
    wire::{CentiCelsius, CentiPercent},
};

use crate::config::ApiConfig;

pub async fn connect(config: &ApiConfig) -> anyhow::Result<sqlx::postgres::PgPool> {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    let options = PgConnectOptions::new()
        .host(&config.db_host)
        .port(config.db_port)
        .username(&config.db_user)
        .password(&config.db_password)
        .database(&config.db_database);

    PgPoolOptions::new()
        .max_connections(config.db_pool_max_connections)
        .connect_with(options)
        .await
        .context("failed to connect to database")
}

pub async fn insert_reading(
    pool: &sqlx::PgPool,
    reading: &SensorReading,
    device_id: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO readings 
    (time, device_id, seq, temp_degc, rh_percent, battery_mv, rssi)
VALUES 
    ($1, $2, $3, $4, $5, $6, $7)
ON CONFLICT DO NOTHING",
        reading.received_at,
        device_id,
        reading.seq as i64,
        reading.temperature.map(CentiCelsius::as_f64),
        reading.relative_humidity.map(CentiPercent::as_f64),
        reading.battery.map(|v| v.0 as i32),
        reading.rssi.0 as i16
    )
    .execute(pool)
    .await?;
    Ok(())
}
