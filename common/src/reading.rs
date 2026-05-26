use serde::{Deserialize, Serialize};

#[cfg(feature = "packet")]
use crate::packet::SensorPacket;

#[cfg(feature = "packet")]
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SensorReading {
    device_id: u8,
    pub seq: u32,
    temp_cdegc: i16,
    humidity: u8,
    pressure_pa: u32,
    battery_mv: u16,
}

#[cfg(feature = "packet")]
impl From<SensorPacket> for SensorReading {
    fn from(value: SensorPacket) -> Self {
        Self {
            battery_mv: value.battery_mv,
            device_id: value.device_id,
            humidity: value.humidity,
            pressure_pa: value.pressure_pa,
            seq: value.seq,
            temp_cdegc: value.temp_cdegc,
        }
    }
}
