use chrono::{DateTime, Utc};

#[cfg(feature = "crypto")]
use crate::observation_envelope::ObservationEnvelope;

#[cfg(feature = "crypto")]
use crate::packet::{DecodeError, cipher::PacketCipher};
use crate::{
    device_addr::DeviceAddr,
    wire::{CentiCelsius, CentiPercent, Dbm, Millivolts},
};

#[cfg(feature = "crypto")]
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

#[cfg(feature = "crypto")]
impl SensorReading {
    /// Opens an envelope into the API's decoded type: authenticate the packet
    /// against `cipher`, decode the body, and merge the result with the
    /// cleartext header the gateway stamped on.
    ///
    /// Not a `TryFrom` — that takes one argument, and the conversion needs the
    /// device's key. The `open_` prefix follows the AEAD seal/open convention:
    /// a `SensorReading` produced here has been authenticated, and there is no
    /// other way to produce one from an envelope.
    ///
    /// `device_addr`, `rssi` and `received_at` come off the envelope and are
    /// **not** covered by the tag — the last two are stamped downstream of the
    /// sensor, so they cannot be. Only `device_addr` is bound, indirectly:
    /// picking the wrong device's `cipher` fails authentication.
    pub fn open_envelope(
        value: &ObservationEnvelope,
        cipher: &PacketCipher,
    ) -> Result<Self, DecodeError> {
        let packet = SensorPacket::parse(&value.packet)?;
        let metrics = packet.decode(cipher)?;

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

#[cfg(all(test, feature = "crypto"))]
mod test {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::{
        device_key::DeviceKey, measurement::Measurement, packet::cipher::DecryptionError,
        wire::Truncated,
    };

    const ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    const RSSI: Dbm = Dbm(-72);

    const KEY: DeviceKey = DeviceKey::from_bytes([
        0xCE, 0x57, 0xF1, 0xC9, 0x9D, 0xA6, 0x14, 0x42, 0x14, 0x0A, 0x9F, 0x58, 0xD2, 0xC4, 0x54,
        0x7B, 0xDB, 0x68, 0x40, 0xDC, 0xCB, 0xFE, 0x41, 0x56, 0x86, 0x26, 0x3D, 0xD8, 0xAC, 0x2B,
        0x0D, 0x1B,
    ]);

    fn cipher() -> PacketCipher {
        PacketCipher::new(&KEY, ADDR)
    }

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
    fn packet_bytes(cipher: &PacketCipher, seq: u32, measurements: &[Measurement]) -> Vec<u8> {
        let mut buf = [0u8; SensorPacket::MAX_ENCODED_LEN];
        let written =
            SensorPacket::encode(seq, measurements, &mut buf, cipher).expect("encoding failed");

        buf[..written].to_vec()
    }

    /// The assembly is what this module owns, so this pins where each field
    /// comes from: `seq` is read out of the packet, the metrics come from the
    /// authenticated decode, and everything else off the envelope's cleartext
    /// header. Packet-level decoding is covered in `packet`.
    #[test]
    fn fields_come_from_the_right_side() {
        let cipher = cipher();
        let envelope = envelope(packet_bytes(
            &cipher,
            42,
            &[
                Measurement::battery(2950),
                Measurement::temperature(2105),
                Measurement::humidity(4875),
            ],
        ));

        assert_eq!(
            SensorReading::open_envelope(&envelope, &cipher),
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
        let cipher = cipher();
        let envelope = envelope(packet_bytes(
            &cipher,
            7,
            &[Measurement::battery(2800), Measurement::temperature(-1250)],
        ));

        let reading = SensorReading::open_envelope(&envelope, &cipher).expect("decoding failed");

        assert_eq!(reading.battery, Some(Millivolts(2800)));
        assert_eq!(reading.temperature, Some(CentiCelsius(-1250)));
        assert_eq!(reading.relative_humidity, None);
    }

    /// All three failure stages have to reach the caller: a header too short
    /// to parse, a body that fails authentication, and a body the version
    /// decoder rejects.
    #[test]
    fn packet_errors_propagate() {
        let cipher = cipher();

        assert_eq!(
            SensorReading::open_envelope(&envelope(vec![0x01, 0x02, 0x03]), &cipher),
            Err(DecodeError::Header(Truncated))
        );

        let mut tampered = packet_bytes(&cipher, 9, &[Measurement::battery(2800)]);
        *tampered.last_mut().unwrap() ^= 0x01;
        assert_eq!(
            SensorReading::open_envelope(&envelope(tampered), &cipher),
            Err(DecodeError::Decryption(DecryptionError::Authentication))
        );

        assert_eq!(
            SensorReading::open_envelope(&envelope(packet_bytes(&cipher, 9, &[])), &cipher),
            Err(DecodeError::Empty)
        );
    }

    /// The registry hands `open_envelope` a cipher looked up by the envelope's
    /// cleartext `deviceAddr`. If that lookup is ever wrong — or the address
    /// is spoofed to borrow another node's slot — the mismatch surfaces here
    /// rather than as a plausible reading filed under the wrong device.
    #[test]
    fn another_devices_cipher_cannot_open_the_envelope() {
        let envelope = envelope(packet_bytes(&cipher(), 42, &[Measurement::battery(2950)]));

        let other = PacketCipher::new(&KEY, DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x07]));

        assert_eq!(
            SensorReading::open_envelope(&envelope, &other),
            Err(DecodeError::Decryption(DecryptionError::Authentication))
        );
    }
}
