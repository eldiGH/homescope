use chrono::{DateTime, Utc};
use homescope_common::device_addr::DeviceAddr;
use sqlx::{PgPool, query_as};
use thiserror::Error;

use crate::devices::keys::{self, SealedDeviceKey};

/// Named in migration 20260716201805. Nothing type-checks this against the
/// schema — a rename there silently downgrades 409 to 500 here.
const DEVICE_ADDR_UNIQUE: &str = "devices_device_addr_key";

struct DeviceRow {
    id: i32,
    device_addr: i64,
    name: String,
    key: Option<Vec<u8>>,
    key_valid_from: DateTime<Utc>,
}

#[derive(Clone)]
pub struct Device {
    pub id: i32,
    pub device_addr: DeviceAddr,
    pub name: String,
    pub key: SealedDeviceKey,
    pub key_valid_from: DateTime<Utc>,
}

impl TryFrom<DeviceRow> for Device {
    type Error = DeviceRowError;

    fn try_from(value: DeviceRow) -> Result<Self, Self::Error> {
        let device_addr = DeviceAddr::try_from(value.device_addr as u64)
            .expect("devices.device_addr_is_48_bits CHECK guarantees 48 bits");

        let key = value
            .key
            .ok_or(DeviceParseError::KeyMissing)
            .and_then(|bytes| bytes[..].try_into().map_err(Into::into))
            .map_err(|source| DeviceRowError {
                device_addr,
                source,
            })?;

        Ok(Self {
            id: value.id,
            name: value.name,
            device_addr,
            key_valid_from: value.key_valid_from,
            key,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeviceParseError {
    #[error("device has invalid key: {0}")]
    Key(#[from] keys::ParseError),

    #[error("device key is missing")]
    KeyMissing,
}

#[derive(Debug, Error)]
#[error("device {device_addr}: {source}")]
pub struct DeviceRowError {
    pub device_addr: DeviceAddr,
    pub source: DeviceParseError,
}

pub async fn get_devices(
    pool: &PgPool,
) -> Result<Vec<Result<Device, DeviceRowError>>, sqlx::Error> {
    Ok(query_as!(
        DeviceRow,
        "
SELECT
    id, device_addr, name, key, key_valid_from
FROM devices
"
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(Device::try_from)
    .collect())
}

pub async fn get_device(
    pool: &PgPool,
    addr: DeviceAddr,
) -> Result<Option<Result<Device, DeviceRowError>>, sqlx::Error> {
    let Some(row) = query_as!(
        DeviceRow,
        r#"
SELECT
    id, device_addr, name, key, key_valid_from
FROM devices
WHERE device_addr = $1
    "#,
        addr.as_i64()
    )
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };

    let device = Device::try_from(row);
    Ok(Some(device))
}

pub struct InsertDevice<'a> {
    pub name: &'a str,
    pub device_addr: DeviceAddr,
    pub key: SealedDeviceKey,
}

pub async fn insert_device(pool: &PgPool, device: InsertDevice<'_>) -> Result<Device, StoreError> {
    Ok(query_as!(
        DeviceRow,
        r#"
INSERT INTO devices (name, device_addr, key)
VALUES ($1, $2, $3)
RETURNING id, device_addr, name, key, key_valid_from
        "#,
        device.name,
        device.device_addr.as_i64(),
        device.key.as_bytes()
    )
    .fetch_one(pool)
    .await
    .map_err(|err| {
        if err.as_database_error().and_then(|d| d.constraint()) == Some(DEVICE_ADDR_UNIQUE) {
            StoreError::DuplicateDeviceAddr
        } else {
            StoreError::Db(err)
        }
    })?
    .try_into()?)
}

pub async fn update_key(
    pool: &PgPool,
    device_addr: DeviceAddr,
    key: SealedDeviceKey,
) -> Result<Option<Device>, StoreError> {
    Ok(query_as!(
        DeviceRow,
        r#"
UPDATE devices SET key=$1, key_valid_from=NOW() WHERE device_addr=$2
RETURNING id, device_addr, name, key, key_valid_from
        "#,
        key.as_bytes(),
        device_addr.as_i64()
    )
    .fetch_optional(pool)
    .await?
    .map(Device::try_from)
    .transpose()?)
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("device_addr already exists")]
    DuplicateDeviceAddr,

    #[error(transparent)]
    Db(#[from] sqlx::Error),

    #[error(transparent)]
    Row(#[from] DeviceRowError),
}

#[cfg(test)]
mod test {
    use super::*;

    /// A structurally valid sealed key: `TryFrom` checks length and version
    /// only — opening it is the registry's job and needs a KEK.
    fn sealed_key_bytes() -> Vec<u8> {
        let mut bytes = vec![0u8; SealedDeviceKey::SIZE];
        bytes[0] = SealedDeviceKey::VERSION;
        bytes[1] = 1;
        bytes
    }

    fn row() -> DeviceRow {
        DeviceRow {
            id: 1,
            device_addr: 0x0605_0403_0201,
            name: "kitchen".into(),
            key: Some(sealed_key_bytes()),
            key_valid_from: DateTime::from_timestamp(1_753_000_000, 0).expect("valid timestamp"),
        }
    }

    #[test]
    fn valid_row_converts() {
        let device = Device::try_from(row()).expect("valid row");

        assert_eq!(device.id, 1);
        assert_eq!(
            device.device_addr,
            DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06])
        );
        assert_eq!(device.name, "kitchen");
        assert_eq!(device.key.ver(), SealedDeviceKey::VERSION);
    }

    /// Phase 1 of the key migration leaves the column nullable, so an
    /// unprovisioned device is representable in the database but must not be
    /// representable as a `Device` — that is what keeps `Device.key`
    /// non-optional and makes phase 3 a two-line deletion.
    #[test]
    fn null_key_is_rejected() {
        assert_eq!(
            Device::try_from(DeviceRow { key: None, ..row() })
                .err()
                .expect("should have been rejected")
                .source,
            DeviceParseError::KeyMissing
        );
    }

    #[test]
    fn malformed_key_is_rejected() {
        assert_eq!(
            Device::try_from(DeviceRow {
                key: Some(vec![0u8; 10]),
                ..row()
            })
            .err()
            .expect("should have been rejected")
            .source,
            DeviceParseError::Key(keys::ParseError::InvalidLen { len: 10 })
        );
    }

    /// The row id lives on the wrapper, not the parse error, so the log line
    /// can name which row to go and look at.
    #[test]
    fn row_error_renders_the_id() {
        let err = DeviceRowError {
            device_addr: DeviceAddr([0x12, 0x34, 0x56, 0x78, 0x90, 0xAB]),
            source: DeviceParseError::KeyMissing,
        };

        assert_eq!(
            err.to_string(),
            "device AB9078563412: device key is missing"
        );
    }
}
