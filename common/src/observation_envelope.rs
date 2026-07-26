use alloc::vec::Vec;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{device_addr::DeviceAddr, wire::Dbm};

#[cfg(feature = "wire")]
use crate::observation::SensorObservation;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ObservationEnvelope {
    pub device_addr: DeviceAddr,
    pub rssi: Dbm,
    pub received_at: DateTime<Utc>,

    #[serde(with = "base64_bytes")]
    pub packet: Vec<u8>,
}

#[cfg(feature = "wire")]
impl ObservationEnvelope {
    pub fn from_observation(observation: SensorObservation, now: DateTime<Utc>) -> Self {
        Self {
            device_addr: observation.device_addr,
            rssi: observation.rssi,
            received_at: now - chrono::TimeDelta::milliseconds(observation.age_ms.into()),
            packet: Vec::from(observation.packet),
        }
    }
}

mod base64_bytes {
    use alloc::{string::String, vec::Vec};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(s).map_err(Error::custom)
    }
}
