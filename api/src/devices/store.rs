use homescope_common::hardware_id::HardwareId;
use sqlx::{PgPool, query_as};

struct DeviceRow {
    id: i32,
    hardware_id: i64,
    name: String,
}

#[derive(Clone)]
pub struct Device {
    pub id: i32,
    pub hardware_id: HardwareId,
    pub name: String,
}

impl From<DeviceRow> for Device {
    fn from(value: DeviceRow) -> Self {
        Self {
            id: value.id,
            hardware_id: HardwareId(value.hardware_id as u64),
            name: value.name,
        }
    }
}

pub async fn get_devices(pool: &PgPool) -> anyhow::Result<Vec<Device>> {
    Ok(query_as!(
        DeviceRow,
        "
SELECT
    id, hardware_id, name
FROM devices
"
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(Device::from)
    .collect())
}
