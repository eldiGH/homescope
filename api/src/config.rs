use std::env::{
    VarError::{NotPresent, NotUnicode},
    var,
};

use anyhow::{Context, bail};

pub struct ApiConfig {
    pub db_url: String,
    pub db_max_connections: u32,
    pub run_migrations: bool,
    pub mqtt_host: String,
    pub mqtt_port: u16,
}

impl ApiConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            db_url: var("DATABASE_URL").context("DATABASE_URL must be set")?,
            db_max_connections: {
                match var("DATABASE_CONNECTION_POOL") {
                    Ok(connection_pool) => connection_pool
                        .parse::<u32>()
                        .context("DATABASE_CONNECTION_POOL must be valid pool size number")?,

                    Err(NotPresent) => 10,

                    Err(NotUnicode(_)) => bail!("DATABASE_CONNECTION_POOL is not valid unicode"),
                }
            },
            run_migrations: var("RUN_MIGRATIONS")
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            mqtt_host: var("MQTT_HOST").unwrap_or("127.0.0.1".to_owned()),
            mqtt_port: {
                match var("MQTT_PORT") {
                    Ok(port) => port
                        .parse::<u16>()
                        .context("MQTT_PORT must be a valid port number")?,

                    Err(NotPresent) => 1883,

                    Err(NotUnicode(_)) => bail!("MQTT_PORT is not valid unicode"),
                }
            },
        })
    }
}
