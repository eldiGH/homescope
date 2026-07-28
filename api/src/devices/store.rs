use anyhow::Context as _;
use homescope_common::device_addr::{DeviceAddr, DeviceAddrRangeError};
use sqlx::{PgPool, query_as};

struct DeviceRow {
    id: i32,
    device_addr: i64,
    name: String,
}

#[derive(Clone)]
pub struct Device {
    pub id: i32,
    pub device_addr: DeviceAddr,
    #[allow(dead_code)] // TODO: will be used later with http management endpoints
    pub name: String,
}

impl TryFrom<DeviceRow> for Device {
    type Error = DeviceAddrRangeError;

    fn try_from(value: DeviceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            name: value.name,
            device_addr: DeviceAddr::try_from(value.device_addr as u64)?,
        })
    }
}

pub async fn get_devices(pool: &PgPool) -> anyhow::Result<Vec<Device>> {
    query_as!(
        DeviceRow,
        "
SELECT
    id, device_addr, name
FROM devices
"
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        let id = row.id;
        Device::try_from(row).with_context(|| format!("device row id={id}: invalid device_addr"))
    })
    .collect()
}
