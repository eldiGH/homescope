use crate::{
    device_addr::DeviceAddr,
    frame::{self, Encode},
    packet::SensorPacket,
    wire::{BufferTooSmall, Dbm, Truncated, Wire},
};

/// Receiver→gateway frame payload: reception metadata plus the air packet,
/// forwarded byte-for-byte and opaque at this layer (decode it with
/// [`SensorPacket::parse`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SensorObservation<'a> {
    pub device_addr: DeviceAddr,
    pub age_ms: u32,
    pub rssi: Dbm,
    pub packet: &'a [u8],
}

const _: () = assert!(SensorObservation::MAX_ENCODED_LEN <= frame::MAX_PAYLOAD_LEN);

impl<'a> SensorObservation<'a> {
    /// device_addr + age_ms + rssi
    pub const HEADER_LEN: usize = size_of::<DeviceAddr>() + size_of::<u32>() + Dbm::SIZE;

    /// Parses exactly one observation from a frame-delimited slice.
    ///
    /// Everything after the fixed header is the packet — trailing bytes are
    /// never ignored, they *are* the packet. Pass exactly one frame's payload.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Truncated> {
        let (device_addr, rest) = bytes.split_first_chunk().ok_or(Truncated)?;
        let device_addr = DeviceAddr(*device_addr);

        let (age_ms, rest) = rest.split_first_chunk().ok_or(Truncated)?;
        let age_ms = u32::from_le_bytes(*age_ms);

        let (rssi, packet) = rest.split_first_chunk::<{ Dbm::SIZE }>().ok_or(Truncated)?;
        let rssi = Dbm::decode(rssi)?;

        Ok(Self {
            device_addr,
            age_ms,
            rssi,
            packet,
        })
    }
}

impl<'a> Encode for SensorObservation<'a> {
    const MAX_ENCODED_LEN: usize = SensorObservation::HEADER_LEN + SensorPacket::MAX_WIRE_LEN;

    fn encode(&self, out: &mut [u8]) -> Result<usize, BufferTooSmall> {
        let (device_addr, rest) = out
            .split_first_chunk_mut::<{ size_of::<DeviceAddr>() }>()
            .ok_or(BufferTooSmall)?;
        device_addr.copy_from_slice(&self.device_addr.0);

        let (age_ms, rest) = rest
            .split_first_chunk_mut::<{ size_of::<u32>() }>()
            .ok_or(BufferTooSmall)?;
        age_ms.copy_from_slice(&self.age_ms.to_le_bytes());

        let (rssi, rest) = rest
            .split_first_chunk_mut::<{ Dbm::SIZE }>()
            .ok_or(BufferTooSmall)?;
        self.rssi.encode(rssi)?;

        let (rest, _) = rest
            .split_at_mut_checked(self.packet.len())
            .ok_or(BufferTooSmall)?;
        rest.copy_from_slice(self.packet);

        Ok(Self::HEADER_LEN + self.packet.len())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn round_trip() {
        let device_addr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        let age_ms = 555;
        let rssi = Dbm(-50);

        let packet = [0xca, 0xfe, 0x12, 0x34, 0x56, 0x78];

        let observation = SensorObservation {
            device_addr,
            age_ms,
            rssi,
            packet: &packet,
        };

        let mut buf: [u8; SensorObservation::MAX_ENCODED_LEN] = [0; _];
        let written = observation.encode(&mut buf).expect("fits in buffer");

        assert_eq!(written, SensorObservation::HEADER_LEN + packet.len());

        let parsed_observation = SensorObservation::parse(&buf[..written]).expect("parses");

        assert_eq!(observation, parsed_observation);
    }

    #[test]
    fn short_header_is_truncated() {
        let bytes = [0u8; SensorObservation::HEADER_LEN];

        for cut in 0..SensorObservation::HEADER_LEN {
            assert_eq!(
                SensorObservation::parse(&bytes[..cut]),
                Err(Truncated),
                "{cut} bytes must not parse"
            );
        }
    }

    #[test]
    fn empty_packet_round_trip() {
        let observation = SensorObservation {
            device_addr: DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
            age_ms: 0,
            rssi: Dbm(-90),
            packet: &[],
        };

        let mut buf: [u8; SensorObservation::MAX_ENCODED_LEN] = [0; _];
        let written = observation.encode(&mut buf).expect("fits in buffer");

        assert_eq!(written, SensorObservation::HEADER_LEN);

        let parsed_observation = SensorObservation::parse(&buf[..written]).expect("parses");

        assert_eq!(observation, parsed_observation);
        assert!(parsed_observation.packet.is_empty());
    }
}
