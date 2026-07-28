use chrono::{DateTime, Utc};

#[cfg(feature = "codec")]
use thiserror::Error;

#[cfg(feature = "codec")]
use crate::observation_envelope::ObservationEnvelope;

use crate::{
    device_addr::DeviceAddr,
    wire::{CentiCelsius, CentiPercent, Dbm, Millivolts},
};

#[cfg(feature = "codec")]
use crate::{
    measurement::{self, Measurement},
    packet::SensorPacket,
};

#[derive(Clone, Debug, PartialEq)]
pub struct SensorReading {
    pub device_addr: DeviceAddr,
    pub seq: u32,
    pub rssi: Dbm,
    pub received_at: DateTime<Utc>,

    pub battery: Option<Millivolts>,
    pub temperature: Option<CentiCelsius>,
    pub relative_humidity: Option<CentiPercent>,
}

#[cfg(feature = "codec")]
fn set<T>(
    slot: &mut Option<T>,
    val: T,
    wrap: impl FnOnce(T) -> Measurement,
) -> Result<(), SensorReadingError> {
    if slot.is_some() {
        return Err(SensorReadingError::Duplicate(wrap(val)));
    }

    *slot = Some(val);

    Ok(())
}

#[cfg(feature = "codec")]
impl TryFrom<&ObservationEnvelope> for SensorReading {
    type Error = SensorReadingError;

    fn try_from(value: &ObservationEnvelope) -> Result<Self, Self::Error> {
        let packet = SensorPacket::parse(&value.packet)?;

        let mut battery = None;
        let mut temperature = None;
        let mut relative_humidity = None;

        let mut has_measurements = false;

        for measurement in packet.measurements() {
            let measurement = measurement?;
            match measurement {
                Measurement::Battery(bat) => set(&mut battery, bat, Measurement::Battery)?,
                Measurement::Temperature(temp) => {
                    set(&mut temperature, temp, Measurement::Temperature)?
                }
                Measurement::Humidity(humidity) => {
                    set(&mut relative_humidity, humidity, Measurement::Humidity)?
                }
            }

            has_measurements = true;
        }

        if !has_measurements {
            return Err(SensorReadingError::NoMeasurements);
        }

        Ok(Self {
            device_addr: value.device_addr,
            seq: packet.seq,
            rssi: value.rssi,
            received_at: value.received_at,

            battery,
            temperature,
            relative_humidity,
        })
    }
}

#[cfg(feature = "codec")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SensorReadingError {
    #[error(transparent)]
    Decode(#[from] measurement::DecodeError),

    #[error("measurement duplicated: {0}")]
    Duplicate(Measurement),

    #[error("no measurements in packet")]
    NoMeasurements,
}
