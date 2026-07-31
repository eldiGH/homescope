use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use homescope_common::{device_addr::DeviceAddr, packet::cipher::PacketCipher};
use sqlx::PgPool;

use crate::devices::store::{self, Device};

#[derive(Clone)]
pub struct DeviceRegistry {
    cache: Arc<RwLock<HashMap<DeviceAddr, Arc<RegisteredDevice>>>>,
    #[allow(dead_code)] // TODO: will be used when we do device management http endpoints
    pool: PgPool,
}

impl DeviceRegistry {
    pub async fn load(pool: PgPool) -> anyhow::Result<Self> {
        let map = store::get_devices(&pool)
            .await?
            .into_iter()
            .map(|device| {
                let device_addr = device.device_addr;

                (
                    device_addr,
                    Arc::new(RegisteredDevice {
                        device,
                        cipher: PacketCipher::new(
                            // TODO: Dev only, will be replaced by KEK + provisioning later on
                            &[
                                0x5F, 0xE3, 0x41, 0x54, 0x0A, 0x81, 0x66, 0x60, 0xF0, 0x65, 0x69,
                                0x14, 0x5B, 0xC0, 0x22, 0x7E, 0x46, 0xAF, 0x9E, 0xCC, 0x36, 0x16,
                                0xA9, 0x8C, 0xFC, 0x35, 0xC6, 0x32, 0xAD, 0xD4, 0xAC, 0x5D,
                            ],
                            device_addr,
                        ),
                    }),
                )
            })
            .collect();

        Ok(Self {
            cache: Arc::new(RwLock::new(map)),
            pool,
        })
    }

    pub fn get(&self, device_addr: DeviceAddr) -> Option<Arc<RegisteredDevice>> {
        self.cache
            .read()
            .expect("devices cache lock poisoned")
            .get(&device_addr)
            .cloned()
    }
}

pub struct RegisteredDevice {
    pub device: Device,
    pub cipher: PacketCipher,
}
