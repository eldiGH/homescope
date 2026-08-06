use std::{env::var, path::PathBuf};

use anyhow::Context;
use homescope_host_util::env::env_var_or;

pub struct ApiConfig {
    pub db_user: String,
    pub db_password: String,
    pub db_database: String,
    pub db_host: String,
    pub db_port: u16,
    pub db_pool_max_connections: u32,
    pub run_migrations: bool,
    pub mqtt_host: String,
    pub mqtt_port: u16,
    pub kek_path: PathBuf,
}

impl ApiConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            db_user: var("DB_USER").context("DB_USER must be set")?,
            db_password: var("DB_PASSWORD").context("DB_PASSWORD must be set")?,
            db_database: var("DB_DATABASE").context("DB_DATABASE must be set")?,
            db_host: var("DB_HOST").context("DB_HOST must be set")?,
            db_port: env_var_or("DB_PORT", 5432)?,
            db_pool_max_connections: env_var_or("DB_POOL_MAX_CONNECTIONS", 10)?,
            run_migrations: env_var_or("RUN_MIGRATIONS", false)?,
            mqtt_host: var("MQTT_HOST").context("MQTT_HOST must be set")?,
            mqtt_port: env_var_or("MQTT_PORT", 1883)?,
            kek_path: var("KEK_PATH").context("KEK_PATH must be set")?.into(),
        })
    }
}
