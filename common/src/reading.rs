use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::device_addr::DeviceAddr;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensorReading {
    pub device_addr: DeviceAddr,
    pub seq: u32,
    pub temp_degc: f64,
    pub rh_percent: f64,
    pub battery_mv: u16,
    pub rssi: i8,
    pub received_at: DateTime<Utc>,
}
