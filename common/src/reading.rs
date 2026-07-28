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

#[cfg(all(test, feature = "codec"))]
mod test {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::{
        measurement::{DecodeError, MeasurementIdUnknownError},
        wire::Truncated,
    };

    const ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    const RSSI: Dbm = Dbm(-72);

    fn envelope(packet: Vec<u8>) -> ObservationEnvelope {
        ObservationEnvelope {
            device_addr: ADDR,
            rssi: RSSI,
            received_at: DateTime::from_timestamp(1_753_000_000, 0).expect("valid timestamp"),
            packet,
        }
    }

    /// Builds the packet the way the sensor does, so these tests run against
    /// the real wire bytes rather than a hand-written approximation of them.
    fn packet_bytes(seq: u32, measurements: &[Measurement]) -> Vec<u8> {
        let mut buf = [0u8; SensorPacket::MAX_ENCODED_LEN];
        let written = SensorPacket::encode(seq, measurements, &mut buf).expect("encoding failed");

        buf[..written].to_vec()
    }

    /// Also pins where each field comes from: `seq` is read out of the packet,
    /// everything else off the envelope's cleartext header.
    #[test]
    fn full_packet_fills_every_slot() {
        let envelope = envelope(packet_bytes(
            42,
            &[
                Measurement::battery(2950),
                Measurement::temperature(2105),
                Measurement::humidity(4875),
            ],
        ));

        assert_eq!(
            SensorReading::try_from(&envelope),
            Ok(SensorReading {
                device_addr: ADDR,
                seq: 42,
                rssi: RSSI,
                received_at: envelope.received_at,

                battery: Some(Millivolts(2950)),
                temperature: Some(CentiCelsius(2105)),
                relative_humidity: Some(CentiPercent(4875)),
            })
        );
    }

    /// Partial packets are the point of the TV encoding: a node with no
    /// humidity sensor, or one whose SHT45 read failed, still reports what it
    /// does have. The absent metric becomes a NULL column, not a dropped
    /// packet — the gap in that one series is the signal.
    #[test]
    fn absent_measurements_stay_none() {
        let envelope = envelope(packet_bytes(
            7,
            &[Measurement::battery(2800), Measurement::temperature(-1250)],
        ));

        let reading = SensorReading::try_from(&envelope).expect("decoding failed");

        assert_eq!(reading.battery, Some(Millivolts(2800)));
        assert_eq!(reading.temperature, Some(CentiCelsius(-1250)));
        assert_eq!(reading.relative_humidity, None);
    }

    #[test]
    fn battery_alone_is_a_valid_reading() {
        let envelope = envelope(packet_bytes(8, &[Measurement::battery(2800)]));

        let reading = SensorReading::try_from(&envelope).expect("decoding failed");

        assert_eq!(reading.battery, Some(Millivolts(2800)));
        assert_eq!(reading.temperature, None);
        assert_eq!(reading.relative_humidity, None);
    }

    /// A packet carrying nothing but a seq is well formed on the wire and
    /// still useless, so it is rejected rather than stored as an all-NULL row.
    #[test]
    fn empty_packet_is_rejected() {
        let envelope = envelope(packet_bytes(9, &[]));

        assert_eq!(
            SensorReading::try_from(&envelope),
            Err(SensorReadingError::NoMeasurements)
        );
    }

    /// A repeated id means the packet contradicts itself, and there is no rule
    /// for picking a winner. The error carries the second value — the one that
    /// found its slot taken.
    #[test]
    fn duplicate_measurement_is_rejected() {
        let envelope = envelope(packet_bytes(
            10,
            &[
                Measurement::temperature(2100),
                Measurement::temperature(2200),
            ],
        ));

        assert_eq!(
            SensorReading::try_from(&envelope),
            Err(SensorReadingError::Duplicate(Measurement::temperature(
                2200
            )))
        );
    }

    /// An unknown id has an unknown length, and the TV encoding carries no
    /// length byte — so nothing after it can be parsed either. Dropping the
    /// whole packet is the only option, even though the measurement in front
    /// of it decoded cleanly.
    #[test]
    fn unknown_id_rejects_the_whole_packet() {
        let mut bytes = packet_bytes(11, &[Measurement::temperature(2100)]);
        bytes.extend_from_slice(&[0x7F, 0x00, 0x00]);

        assert_eq!(
            SensorReading::try_from(&envelope(bytes)),
            Err(SensorReadingError::Decode(DecodeError::UnknownId(
                MeasurementIdUnknownError(0x7F)
            )))
        );
    }

    #[test]
    fn truncated_seq_is_rejected() {
        assert_eq!(
            SensorReading::try_from(&envelope(vec![0x01, 0x02, 0x03])),
            Err(SensorReadingError::Decode(DecodeError::Truncated(
                Truncated
            )))
        );
    }

    #[test]
    fn truncated_measurement_value_is_rejected() {
        let mut bytes = 12u32.to_le_bytes().to_vec();
        bytes.push(0x02); // temperature id, value bytes missing

        assert_eq!(
            SensorReading::try_from(&envelope(bytes)),
            Err(SensorReadingError::Decode(DecodeError::Truncated(
                Truncated
            )))
        );
    }
}
