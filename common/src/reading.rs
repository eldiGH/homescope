use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::device_id::DeviceId;
#[cfg(feature = "wire")]
use crate::observation::SensorObservation;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensorReading {
    pub device_id: DeviceId,
    pub seq: u32,
    pub temp_degc: f64,
    pub rh_percent: f64,
    pub battery_mv: u16,
    pub rssi: i8,
    pub received_at: DateTime<Utc>,
}

impl SensorReading {
    #[cfg(feature = "wire")]
    pub fn from_observation(observation: SensorObservation, received_at: DateTime<Utc>) -> Self {
        Self {
            battery_mv: observation.battery_mv,
            device_id: observation.device_id,
            rh_percent: f64::from(observation.rh_cpercent) / 100.0,
            seq: observation.seq,
            temp_degc: f64::from(observation.temp_cdegc) / 100.0,
            rssi: observation.rssi,
            received_at,
        }
    }
}

impl core::fmt::Display for SensorReading {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "reading #{} from {}: {:.2} °C, {:.1} %RH, battery {} mV, rssi {} dBm",
            self.seq, self.device_id, self.temp_degc, self.rh_percent, self.battery_mv, self.rssi
        )
    }
}
