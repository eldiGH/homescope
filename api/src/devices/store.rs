use chrono::{DateTime, Utc};
use homescope_common::device_addr::DeviceAddr;
use sqlx::{PgPool, query_as};
use thiserror::Error;

use crate::devices::keys::SealedDeviceKey;

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

pub struct DeviceRecord {
    pub id: i32,
    pub device_addr: DeviceAddr,
    pub name: String,
    pub key: Option<Vec<u8>>,
    pub key_valid_from: DateTime<Utc>,
}

impl From<DeviceRow> for DeviceRecord {
    fn from(value: DeviceRow) -> Self {
        Self {
            id: value.id,
            device_addr: device_addr_of(value.device_addr),
            name: value.name,
            key: value.key,
            key_valid_from: value.key_valid_from,
        }
    }
}

fn device_addr_of(raw: i64) -> DeviceAddr {
    DeviceAddr::try_from(raw as u64)
        .expect("devices.device_addr_is_48_bits CHECK guarantees 48 bits")
}

pub async fn all_records(pool: &PgPool) -> Result<Vec<DeviceRecord>, sqlx::Error> {
    let records = query_as!(
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
    .map(DeviceRecord::from)
    .collect::<Vec<_>>();

    Ok(records)
}

pub async fn record_by_addr(
    pool: &PgPool,
    addr: DeviceAddr,
) -> Result<Option<DeviceRecord>, sqlx::Error> {
    let record = query_as!(
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
    .map(DeviceRecord::from);

    Ok(record)
}

pub struct InsertDevice<'a> {
    pub name: &'a str,
    pub device_addr: DeviceAddr,
    pub key: SealedDeviceKey,
}

pub async fn insert_device(
    pool: &PgPool,
    device: InsertDevice<'_>,
) -> Result<DeviceRecord, InsertDeviceError> {
    query_as!(
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
            InsertDeviceError::DuplicateDeviceAddr
        } else {
            InsertDeviceError::Db(err)
        }
    })
    .map(DeviceRecord::from)
}

pub async fn update_key(
    pool: &PgPool,
    device_addr: DeviceAddr,
    key: SealedDeviceKey,
) -> Result<Option<DeviceRecord>, sqlx::Error> {
    query_as!(
        DeviceRow,
        r#"
UPDATE devices SET key=$1, key_valid_from=NOW() WHERE device_addr=$2
RETURNING id, device_addr, name, key, key_valid_from
        "#,
        key.as_bytes(),
        device_addr.as_i64()
    )
    .fetch_optional(pool)
    .await
    .map(|row| row.map(DeviceRecord::from))
}

#[derive(Debug, Error)]
pub enum InsertDeviceError {
    #[error("device_addr already exists")]
    DuplicateDeviceAddr,

    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[cfg(test)]
mod test {
    use super::*;

    /// The only judgment this module makes. Postgres has no unsigned integers,
    /// so `device_addr` is stored as a signed 64-bit column and read back
    /// through a cast that would truncate a value wider than 48 bits — the
    /// `device_addr_is_48_bits` CHECK is what makes that `expect` sound, and it
    /// lives in a migration the compiler cannot see. Everything else on the way
    /// from `DeviceRow` to `DeviceRecord` is a move.
    #[test]
    fn record_decodes_the_address_and_moves_the_rest() {
        let record = DeviceRecord::from(DeviceRow {
            id: 1,
            device_addr: 0x0605_0403_0201,
            name: "kitchen".into(),
            key: Some(vec![0xAB; 4]),
            key_valid_from: DateTime::from_timestamp(1_753_000_000, 0).expect("valid timestamp"),
        });

        assert_eq!(
            record.device_addr,
            DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
            "the column is little-endian: least significant byte first"
        );
        assert_eq!(record.id, 1);
        assert_eq!(record.name, "kitchen");
        assert_eq!(record.key.as_deref(), Some(&[0xAB, 0xAB, 0xAB, 0xAB][..]));
    }

    /// The boundary the CHECK constraint permits. A 49th bit would make the
    /// conversion panic, which is the intended behaviour — a row that violates
    /// the constraint means the schema and this code disagree, and serving a
    /// silently truncated address would be worse than failing.
    #[test]
    fn record_accepts_the_widest_permitted_address() {
        let record = DeviceRecord::from(DeviceRow {
            device_addr: 0xFFFF_FFFF_FFFF,
            ..DeviceRow {
                id: 1,
                device_addr: 0,
                name: String::new(),
                key: None,
                key_valid_from: DateTime::from_timestamp(0, 0).expect("valid timestamp"),
            }
        });

        assert_eq!(record.device_addr, DeviceAddr([0xFF; 6]));
    }

    /// A null key is not this module's problem — it travels as-is and is
    /// classified by `keys::open_key_column`, which is what keeps the store
    /// free of anything needing a KEK.
    #[test]
    fn record_carries_a_null_key_through() {
        let record = DeviceRecord::from(DeviceRow {
            id: 1,
            device_addr: 0x0605_0403_0201,
            name: "kitchen".into(),
            key: None,
            key_valid_from: DateTime::from_timestamp(1_753_000_000, 0).expect("valid timestamp"),
        });

        assert!(record.key.is_none());
    }
}
