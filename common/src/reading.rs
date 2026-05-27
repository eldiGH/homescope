use serde::{Deserialize, Serialize};

use crate::{device_id::DeviceId, observation::SensorObservation};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensorReading {
    pub device_id: DeviceId,
    pub seq: u32,
    pub temp_degc: f64,
    pub humidity: u8,
    pub pressure_pa: u32,
    pub battery_mv: u16,
    pub rssi: i8,
    pub received_at_ms: i64,
}

impl SensorReading {
    #[cfg(feature = "wire")]
    pub fn from_observation(observation: SensorObservation, received_at_ms: i64) -> Self {
        Self {
            battery_mv: observation.battery_mv,
            device_id: observation.device_id,
            humidity: observation.humidity,
            pressure_pa: observation.pressure_pa,
            seq: observation.seq,
            temp_degc: f64::from(observation.temp_cdegc) / 100.0,
            rssi: observation.rssi,
            received_at_ms,
        }
    }
}
