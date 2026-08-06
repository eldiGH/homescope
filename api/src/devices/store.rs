use chrono::{DateTime, Utc};
use homescope_common::device_addr::{DeviceAddr, DeviceAddrRangeError};
use sqlx::{PgPool, query_as};
use thiserror::Error;

use crate::devices::keys::{self, SealedDeviceKey};

struct DeviceRow {
    id: i32,
    device_addr: i64,
    name: String,
    key: Option<Vec<u8>>,
    key_updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct Device {
    pub id: i32,
    pub device_addr: DeviceAddr,
    #[allow(dead_code)] // TODO: will be used later with http management endpoints
    pub name: String,
    pub key: SealedDeviceKey,
    #[allow(dead_code)] // TODO: will be used
    pub key_updated_at: DateTime<Utc>,
}

impl TryFrom<DeviceRow> for Device {
    type Error = DeviceParseError;

    fn try_from(value: DeviceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            name: value.name,
            device_addr: DeviceAddr::try_from(value.device_addr as u64)?,
            key_updated_at: value.key_updated_at,
            key: value.key.ok_or(DeviceParseError::KeyMissing)?[..].try_into()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeviceParseError {
    #[error("device has invalid key: {0}")]
    Key(#[from] keys::ParseError),

    #[error("device has invalid device_addr: {0}")]
    DeviceAddr(#[from] DeviceAddrRangeError),

    #[error("device key is missing")]
    KeyMissing,
}

#[derive(Debug, Error)]
#[error("device row id={id}: {source}")]
pub struct DeviceRowError {
    pub id: i32,
    pub source: DeviceParseError,
}

pub async fn get_devices(pool: &PgPool) -> anyhow::Result<Vec<Result<Device, DeviceRowError>>> {
    Ok(query_as!(
        DeviceRow,
        "
SELECT
    id, device_addr, name, key, key_updated_at
FROM devices
"
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        let id = row.id;

        Device::try_from(row).map_err(|err| DeviceRowError { id, source: err })
    })
    .collect())
}
