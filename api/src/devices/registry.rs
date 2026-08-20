use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, RwLock},
};

use chrono::{DateTime, Utc};
use homescope_common::{
    device_addr::DeviceAddr,
    device_key::{DeviceKey, KeyGenError},
    packet::cipher::PacketCipher,
};
use sqlx::PgPool;
use thiserror::Error;
use tracing::{error, info};

use crate::devices::{
    keys::{KekRing, SealedDeviceKey, open_key_column},
    store::{self, InsertDeviceError},
    summary::DeviceSummary,
};

#[derive(Clone)]
pub struct DeviceRegistry {
    cache: Arc<RwLock<HashMap<DeviceAddr, Arc<Device>>>>,
    pool: PgPool,
    keyring: Arc<KekRing>,
}

impl DeviceRegistry {
    pub async fn load(pool: PgPool, kek_path: &Path) -> anyhow::Result<Self> {
        let keyring = KekRing::load(kek_path)?;

        let mut key_failures = 0;

        let map: HashMap<DeviceAddr, Arc<Device>> = store::all_records(&pool)
            .await?
            .into_iter()
            .filter_map(|device| {
                let key = open_key_column(device.key, device.device_addr, &keyring)
                    .inspect_err(|err| {
                        key_failures += 1;
                        error!("device {}: can't open key: {err}", device.device_addr);
                    })
                    .ok()?;

                Some((
                    device.device_addr,
                    Arc::new(Device {
                        id: device.id,
                        name: device.name,
                        device_addr: device.device_addr,
                        key_valid_from: device.key_valid_from,
                        cipher: PacketCipher::new(&key, device.device_addr),
                    }),
                ))
            })
            .collect();

        info!(
            "device registry loaded: {} devices ({key_failures} keys could not be opened)",
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

    pub fn get(&self, device_addr: DeviceAddr) -> Option<Arc<Device>> {
        self.cache
            .read()
            .expect("devices cache lock poisoned")
            .get(&device_addr)
            .cloned()
    }

    pub async fn summary(
        &self,
        device_addr: DeviceAddr,
    ) -> Result<Option<DeviceSummary>, sqlx::Error> {
        let summary = store::record_by_addr(&self.pool, device_addr)
            .await?
            .map(|record| DeviceSummary::classify(record, &self.keyring));

        Ok(summary)
    }

    pub async fn summaries(&self) -> Result<Vec<DeviceSummary>, sqlx::Error> {
        let summaries = store::all_records(&self.pool)
            .await?
            .into_iter()
            .map(|record| DeviceSummary::classify(record, &self.keyring))
            .collect();

        Ok(summaries)
    }

    pub async fn provision(
        &self,
        addr: DeviceAddr,
        name: &str,
    ) -> Result<(Arc<Device>, DeviceKey), DeviceError> {
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

        let registered_device = Arc::new(Device {
            id: device.id,
            device_addr: device.device_addr,
            name: device.name,
            key_valid_from: device.key_valid_from,

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
    ) -> Result<(Arc<Device>, DeviceKey), DeviceError> {
        let key = DeviceKey::generate()?;
        let sealed_key = SealedDeviceKey::seal(&self.keyring, &key, addr);

        let device = store::update_key(&self.pool, addr, sealed_key)
            .await?
            .ok_or(DeviceError::NotFound)?;

        let new_device = Arc::new(Device {
            id: device.id,
            device_addr: device.device_addr,
            name: device.name,
            key_valid_from: device.key_valid_from,
            cipher: PacketCipher::new(&key, addr),
        });

        self.cache
            .write()
            .expect("devices cache lock poisoned")
            .insert(addr, new_device.clone());

        Ok((new_device, key))
    }
}

pub struct Device {
    pub id: i32,
    pub device_addr: DeviceAddr,
    pub name: String,
    pub key_valid_from: DateTime<Utc>,
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
    Db(#[from] sqlx::Error),
}

impl From<InsertDeviceError> for DeviceError {
    fn from(value: InsertDeviceError) -> Self {
        match value {
            InsertDeviceError::DuplicateDeviceAddr => DeviceError::AlreadyExists,
            InsertDeviceError::Db(err) => DeviceError::Db(err),
        }
    }
}
