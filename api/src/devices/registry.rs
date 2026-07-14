use std::sync::Arc;

use homescope_common::hardware_id::HardwareId;
use sqlx::PgPool;

use crate::devices::{
    cache::Cache,
    store::{self, Device},
};

#[derive(Clone)]
pub struct DeviceRegistry {
    cache: Arc<Cache>,
    pool: PgPool,
}

impl DeviceRegistry {
    pub async fn load(pool: PgPool) -> anyhow::Result<Self> {
        let devices = store::get_devices(&pool).await?;

        Ok(Self {
            cache: Arc::new(Cache::new(devices)),
            pool,
        })
    }

    pub fn get(&self, hardware_id: HardwareId) -> Option<Device> {
        self.cache.get(hardware_id)
    }
}
