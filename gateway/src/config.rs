use std::env::var;

use anyhow::Context as _;
use homescope_host_util::env::env_var_or;

pub struct GatewayConfig {
    pub mqtt_host: String,
    pub mqtt_port: u16,
    pub receiver_path: String,
}

impl GatewayConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            mqtt_host: var("MQTT_HOST").context("MQTT_HOST must be set")?,
            mqtt_port: env_var_or("MQTT_PORT", 1883)?,
            receiver_path: env_var_or("RECEIVER_PATH", "/dev/homescope-receiver".to_owned())?,
        })
    }
}
