use chrono::{DateTime, Utc};

#[cfg(feature = "codec")]
use crate::{observation_envelope::ObservationEnvelope, packet};

use crate::{
    device_addr::DeviceAddr,
    wire::{CentiCelsius, CentiPercent, Dbm, Millivolts},
};

#[cfg(feature = "codec")]
use crate::packet::SensorPacket;

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
impl TryFrom<&ObservationEnvelope> for SensorReading {
    type Error = packet::DecodeError;

    fn try_from(value: &ObservationEnvelope) -> Result<Self, Self::Error> {
        let packet = SensorPacket::parse(&value.packet)?;
        let metrics = packet.decode()?;

        Ok(Self {
            device_addr: value.device_addr,
            seq: packet.seq,
            rssi: value.rssi,
            received_at: value.received_at,

            battery: metrics.battery,
            temperature: metrics.temperature,
            relative_humidity: metrics.relative_humidity,
        })
    }
}

#[cfg(all(test, feature = "codec"))]
mod test {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::{measurement::Measurement, packet::DecodeError, wire::Truncated};

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

    /// The assembly is what this module owns, so this pins where each field
    /// comes from: `seq` is read out of the packet, the metrics come from the
    /// version-scoped decode, and everything else off the envelope's cleartext
    /// header. Packet-level decoding is covered in `packet`.
    #[test]
    fn fields_come_from_the_right_side() {
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

    /// An absent metric survives the assembly as `None` — that is what becomes
    /// a NULL column rather than a rejected packet.
    #[test]
    fn absent_metrics_survive_as_none() {
        let envelope = envelope(packet_bytes(
            7,
            &[Measurement::battery(2800), Measurement::temperature(-1250)],
        ));

        let reading = SensorReading::try_from(&envelope).expect("decoding failed");

        assert_eq!(reading.battery, Some(Millivolts(2800)));
        assert_eq!(reading.temperature, Some(CentiCelsius(-1250)));
        assert_eq!(reading.relative_humidity, None);
    }

    /// Both failure stages have to reach the caller: a header too short to
    /// parse, and a body the version decoder rejects.
    #[test]
    fn packet_errors_propagate() {
        assert_eq!(
            SensorReading::try_from(&envelope(vec![0x01, 0x02, 0x03])),
            Err(DecodeError::Header(Truncated))
        );

        assert_eq!(
            SensorReading::try_from(&envelope(packet_bytes(9, &[]))),
            Err(DecodeError::Empty)
        );
    }
}
