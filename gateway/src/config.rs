use homescope_common::site::Site;
use homescope_host_util::{
    env::{env_var, env_var_or},
    mqtt::MqttConfig,
};

pub struct GatewayConfig {
    /// The site this gateway publishes for — the `<site>` in every topic.
    /// Required, with no default: a wrong site is exactly what a per-site
    /// broker ACL drops *silently*, so it has to be stated, and checked here.
    pub site: Site,
    pub mqtt: MqttConfig,
    pub receiver_path: String,
}

impl GatewayConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let site: Site = env_var("SITE")?;

        Ok(Self {
            mqtt: MqttConfig::from_env(format!("homescope-gateway-{site}"))?,
            site,
            receiver_path: env_var_or("RECEIVER_PATH", "/dev/homescope-receiver".to_owned())?,
        })
    }
}
