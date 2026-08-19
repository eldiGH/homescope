use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, RwLock},
};

use homescope_common::{
    device_addr::DeviceAddr,
    device_key::{DeviceKey, KeyGenError},
    packet::cipher::PacketCipher,
};
use sqlx::PgPool;
use thiserror::Error;
use tracing::{error, info};

use crate::devices::{
    keys::{KekRing, SealedDeviceKey},
    store::{self, Device, StoreError},
};

#[derive(Clone)]
pub struct DeviceRegistry {
    cache: Arc<RwLock<HashMap<DeviceAddr, Arc<RegisteredDevice>>>>,
    pool: PgPool,
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
                            &device
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

    /// An empty registry over a pool that is never used.
    ///
    /// [`load`](Self::load) is the only production constructor and it needs a
    /// live database, so HTTP tests that only care about routing, extraction
    /// and middleware would otherwise need one too. Handlers reached in those
    /// tests are rejected before they touch the pool.
    #[cfg(test)]
    pub fn for_test(pool: PgPool) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            keyring: Arc::new(KekRing::for_test()),
            pool,
        }
    }

    pub fn get(&self, device_addr: DeviceAddr) -> Option<Arc<RegisteredDevice>> {
        self.cache
            .read()
            .expect("devices cache lock poisoned")
            .get(&device_addr)
            .cloned()
    }

    pub async fn provision(
        &self,
        addr: DeviceAddr,
        name: &str,
    ) -> Result<(Arc<RegisteredDevice>, DeviceKey), DeviceError> {
        let key = DeviceKey::generate()?;
        let sealed_key = SealedDeviceKey::seal(&self.keyring, &key, addr);

        let device = store::insert_device(
            &self.pool,
            store::InsertDevice {
                name,
                key: sealed_key,
                device_addr: addr,
            },
        )
        .await?;

        let registered_device = Arc::new(RegisteredDevice {
            device,
            cipher: PacketCipher::new(&key, addr),
        });

        self.cache
            .write()
            .expect("devices cache lock poisoned")
            .insert(addr, registered_device.clone());

        Ok((registered_device, key))
    }

    pub async fn rotate_key(
        &self,
        addr: DeviceAddr,
    ) -> Result<(Arc<RegisteredDevice>, DeviceKey), DeviceError> {
        let key = DeviceKey::generate()?;
        let sealed_key = SealedDeviceKey::seal(&self.keyring, &key, addr);

        let device = store::update_key(&self.pool, addr, sealed_key)
            .await?
            .ok_or(DeviceError::NotFound)?;

        let new_device = Arc::new(RegisteredDevice {
            device,
            cipher: PacketCipher::new(&key, addr),
        });

        self.cache
            .write()
            .expect("devices cache lock poisoned")
            .insert(addr, new_device.clone());

        Ok((new_device, key))
    }
}

pub struct RegisteredDevice {
    pub device: Device,
    pub cipher: PacketCipher,
}

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("device already provisioned")]
    AlreadyExists,
    #[error("device not found")]
    NotFound,
    #[error("key generation failed: {0}")]
    KeyGen(#[from] KeyGenError),
    #[error(transparent)]
    Store(StoreError),
}

impl From<StoreError> for DeviceError {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::DuplicateDeviceAddr => DeviceError::AlreadyExists,
            err => DeviceError::Store(err),
        }
    }
}
