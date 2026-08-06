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
            key_updated_at: DateTime::from_timestamp(1_753_000_000, 0).expect("valid timestamp"),
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
                .expect("should have been rejected"),
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
            .expect("should have been rejected"),
            DeviceParseError::Key(keys::ParseError::InvalidLen { len: 10 })
        );
    }

    /// `device_addr` is a `BIGINT` holding a 48-bit address; anything wider is
    /// a corrupt row, not an address.
    #[test]
    fn out_of_range_device_addr_is_rejected() {
        let err = Device::try_from(DeviceRow {
            device_addr: 0x0102_0304_0506_0708,
            ..row()
        })
        .err()
        .expect("should have been rejected");

        assert!(matches!(err, DeviceParseError::DeviceAddr(_)), "{err}");
    }

    /// The row id lives on the wrapper, not the parse error, so the log line
    /// can name which row to go and look at.
    #[test]
    fn row_error_renders_the_id() {
        let err = DeviceRowError {
            id: 42,
            source: DeviceParseError::KeyMissing,
        };

        assert_eq!(err.to_string(), "device row id=42: device key is missing");
    }
}
