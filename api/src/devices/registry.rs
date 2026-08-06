use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, RwLock},
};

use homescope_common::{device_addr::DeviceAddr, packet::cipher::PacketCipher};
use sqlx::PgPool;
use tracing::{error, info};

use crate::devices::{
    keys::KekRing,
    store::{self, Device},
};

#[derive(Clone)]
pub struct DeviceRegistry {
    cache: Arc<RwLock<HashMap<DeviceAddr, Arc<RegisteredDevice>>>>,
    #[allow(dead_code)] // TODO: will be used when we do device management http endpoints
    pool: PgPool,
    #[allow(dead_code)] // TODO: will be used when adding new devices will be implemented
    keyring: Arc<KekRing>,
}

impl DeviceRegistry {
    pub async fn load(pool: PgPool, kek_path: &Path) -> anyhow::Result<Self> {
        let keyring = KekRing::load(kek_path)?;

        let mut key_failures = 0;
        let mut invalid_rows = 0;

        let map: HashMap<DeviceAddr, Arc<RegisteredDevice>> = store::get_devices(&pool)
            .await?
            .into_iter()
            .filter_map(|device| {
                let device = device
                    .inspect_err(|err| {
                        invalid_rows += 1;
                        error!(%err);
                    })
                    .ok()?;

                let device_addr = device.device_addr;

                Some((
                    device_addr,
                    Arc::new(RegisteredDevice {
                        cipher: PacketCipher::new(
                            device
                                .key
                                .open(&keyring, device_addr)
                                .inspect_err(|err| {
                                    key_failures += 1;
                                    error!("device row id={}; can't open key: {err}", device.id);
                                })
                                .ok()?,
                            device_addr,
                        ),
                        device,
                    }),
                ))
            })
            .collect();

        info!(
            "device registry loaded: {} devices ({invalid_rows} invalid rows, {key_failures} keys could not be opened)",
            map.len()
        );

        Ok(Self {
            cache: Arc::new(RwLock::new(map)),
            keyring: Arc::new(keyring),
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
