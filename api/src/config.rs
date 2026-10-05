use std::path::PathBuf;

use homescope_host_util::{
    env::{env_var, env_var_or},
    mqtt::MqttConfig,
};

pub struct ApiConfig {
    pub db_user: String,
    pub db_password: String,
    pub db_database: String,
    pub db_host: String,
    pub db_port: u16,
    pub db_pool_max_connections: u32,
    pub run_migrations: bool,
    pub mqtt: MqttConfig,
    pub kek_path: PathBuf,
    pub http_bind: String,
    pub admin_token_path: PathBuf,
}

impl ApiConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            db_user: env_var("DB_USER")?,
            db_password: env_var("DB_PASSWORD")?,
            db_database: env_var("DB_DATABASE")?,
            db_host: env_var("DB_HOST")?,
            db_port: env_var_or("DB_PORT", 5432)?,
            db_pool_max_connections: env_var_or("DB_POOL_MAX_CONNECTIONS", 10)?,
            run_migrations: env_var_or("RUN_MIGRATIONS", false)?,
            // `homescope-api`, not the `api` used before 2026-10: the client id
            // keys the durable session, so a rename orphans whatever the broker
            // queued under the old one. It was free while no broker held one.
            mqtt: MqttConfig::from_env("homescope-api".to_owned())?,
            kek_path: env_var("KEK_PATH")?,
            http_bind: env_var("HTTP_BIND")?,
            admin_token_path: env_var("ADMIN_TOKEN_PATH")?,
        })
    }
}
