use anyhow::Context as _;
use homescope_common::reading::SensorReading;

pub async fn connect(url: &str, connection_pool: u32) -> anyhow::Result<sqlx::postgres::PgPool> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(connection_pool)
        .connect(url)
        .await
        .context("failed to connect to database")
}

pub async fn insert_reading(
    pool: &sqlx::PgPool,
    reading: &SensorReading,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
                    "INSERT INTO readings (time, device_id, seq, temp_degc, rh_percent, battery_mv, rssi) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                    reading.received_at,
                    reading.device_id.0 as i64,
                    reading.seq as i64,
                    reading.temp_degc,
                    reading.rh_percent,
                    reading.battery_mv as i32,
                    reading.rssi as i16
                ).execute(pool).await?;
    Ok(())
}
