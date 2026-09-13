use chrono::{DateTime, Utc};
use homescope_common::device_addr::DeviceAddr;
use sqlx::{PgPool, query_as, query_scalar};
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

/// One `devices` row with its address decoded — columns and nothing else.
///
/// What the writes return and what the registry loads at startup. Neither can
/// know anything about `readings`, so neither should be handed a type with a
/// slot for it; see [`DeviceActivity`] for the read side.
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

struct DeviceActivityRow {
    id: i32,
    device_addr: i64,
    name: String,
    key: Option<Vec<u8>>,
    key_valid_from: DateTime<Utc>,
    last_seen: Option<DateTime<Utc>>,
}

/// A device plus what the fleet has heard from it under its current key.
///
/// Kept apart from [`DeviceRecord`] because `last_seen` is not a column — it is
/// derived from `readings`. Folded into the record, the writes had to invent a
/// `None` that meant "did not look" beside a read's `None` meaning "never
/// reported under this key", and every startup ran the readings join only to
/// throw the result away.
pub struct DeviceActivity {
    pub record: DeviceRecord,

    /// The newest reading since `key_valid_from`. `None` is registered but
    /// never reported under the current key — the state `verify` polls out of.
    pub last_seen: Option<DateTime<Utc>>,
}

impl From<DeviceActivityRow> for DeviceActivity {
    fn from(value: DeviceActivityRow) -> Self {
        Self {
            // Through `DeviceRow`, so the address decode — the one judgment
            // this module makes — exists once and cannot drift between the two.
            record: DeviceRecord::from(DeviceRow {
                id: value.id,
                device_addr: value.device_addr,
                name: value.name,
                key: value.key,
                key_valid_from: value.key_valid_from,
            }),
            last_seen: value.last_seen,
        }
    }
}

fn device_addr_of(raw: i64) -> DeviceAddr {
    DeviceAddr::try_from(raw as u64)
        .expect("devices.device_addr_is_48_bits CHECK guarantees 48 bits")
}

/// Every device, columns only — the registry's startup load, which builds
/// ciphers and has no use for reading activity.
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

// The two activity queries share their join and cannot share its text: the
// sqlx macros take a string literal, so a fix to one must be made to both.
//
// `ORDER BY time DESC LIMIT 1` rather than `MAX(time)`: it walks
// `readings (device_id, time DESC)` from the newest end and stops at the first
// row, which is the shape TimescaleDB's ordered append is built for.
//
// `"last_seen?"` states the nullability instead of leaving sqlx to infer it
// through a lateral join — a device with no readings under its key produces a
// NULL there, and an inference that ever guessed NOT NULL would turn that into
// a decode error at runtime.

pub async fn all_activity(pool: &PgPool) -> Result<Vec<DeviceActivity>, sqlx::Error> {
    let activity = query_as!(
        DeviceActivityRow,
        r#"
SELECT
  d.id,
  d.device_addr,
  d.name,
  d.key,
  d.key_valid_from,
  r.last_seen AS "last_seen?"
FROM
  devices AS d
  LEFT JOIN LATERAL (
    SELECT
      time AS last_seen
    FROM
      readings
    WHERE
      device_id = d.id
      AND time > d.key_valid_from
    ORDER BY
      time DESC
    LIMIT
      1
  ) r ON true
"#
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(DeviceActivity::from)
    .collect::<Vec<_>>();

    Ok(activity)
}

pub async fn activity_by_addr(
    pool: &PgPool,
    addr: DeviceAddr,
) -> Result<Option<DeviceActivity>, sqlx::Error> {
    let activity = query_as!(
        DeviceActivityRow,
        r#"
SELECT
  d.id,
  d.device_addr,
  d.name,
  d.key,
  d.key_valid_from,
  r.last_seen AS "last_seen?"
FROM
  devices AS d
  LEFT JOIN LATERAL (
    SELECT
      time AS last_seen
    FROM
      readings
    WHERE
      device_id = d.id
      AND time > d.key_valid_from
    ORDER BY
      time DESC
    LIMIT
      1
  ) r ON true
WHERE
  d.device_addr = $1
"#,
        addr.as_i64()
    )
    .fetch_optional(pool)
    .await?
    .map(DeviceActivity::from);

    Ok(activity)
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
    let inserted = query_as!(
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
    .await;

    match inserted {
        Ok(row) => Ok(DeviceRecord::from(row)),

        Err(err) if is_duplicate_device_addr(&err) => {
            // ⚠️ `fetch_one`, not `fetch_optional`. The row this insert just
            // collided with should still be there; if a delete landed in
            // between, there is no honest name to report, because the conflict
            // itself no longer exists. `RowNotFound` then leaves through `Db`
            // as a 500 — which is the truthful answer, since a retry would
            // succeed — rather than a 409 carrying a made-up name in a typed
            // field a client may display or match on.
            let name = query_scalar!(
                "SELECT name FROM devices WHERE device_addr = $1",
                device.device_addr.as_i64()
            )
            .fetch_one(pool)
            .await?;

            Err(InsertDeviceError::DuplicateDeviceAddr { name })
        }

        Err(err) => Err(InsertDeviceError::Db(err)),
    }
}

fn is_duplicate_device_addr(err: &sqlx::Error) -> bool {
    err.as_database_error().and_then(|d| d.constraint()) == Some(DEVICE_ADDR_UNIQUE)
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
    /// Carries the name of the row already holding this address, so the 409
    /// can say which device it collided with.
    #[error("device_addr already exists")]
    DuplicateDeviceAddr { name: String },

    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

// TODO: the SQL itself is untested — the activity join and the name-lookup race
// need #[sqlx::test] database tests. See docs/design/provisioning.md § Postponed.
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

    fn activity_row(last_seen: Option<DateTime<Utc>>) -> DeviceActivityRow {
        DeviceActivityRow {
            id: 7,
            device_addr: 0x0605_0403_0201,
            name: "kitchen".into(),
            key: Some(vec![0xCD; 4]),
            key_valid_from: DateTime::from_timestamp(1_753_000_000, 0).expect("valid timestamp"),
            last_seen,
        }
    }

    /// The activity row decodes through the record's own conversion, so the
    /// address comes out exactly as a plain record's would — and `last_seen`
    /// rides alongside rather than being folded into the record.
    #[test]
    fn activity_decodes_its_record_and_carries_last_seen() {
        let seen = DateTime::from_timestamp(1_753_003_600, 0).expect("valid timestamp");

        let activity = DeviceActivity::from(activity_row(Some(seen)));

        assert_eq!(
            activity.record.device_addr,
            DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06])
        );
        assert_eq!(activity.record.id, 7);
        assert_eq!(activity.record.name, "kitchen");
        assert_eq!(activity.record.key.as_deref(), Some(&[0xCD; 4][..]));
        assert_eq!(activity.last_seen, Some(seen));
    }

    /// The LEFT JOIN's NULL — no readings under the current key — must reach
    /// the summary as `None`, not be defaulted into a timestamp.
    #[test]
    fn activity_with_no_readings_under_its_key_has_no_last_seen() {
        assert_eq!(DeviceActivity::from(activity_row(None)).last_seen, None);
    }
}
