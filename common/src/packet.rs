use crate::{
    measurement::{self, Measurement},
    wire::{BufferTooSmall, Truncated},
};

pub use measurement::DecodeError;

#[derive(Clone, Copy)]
pub struct SensorPacket<'a> {
    pub seq: u32,
    pub payload: &'a [u8],
}

const _: () = assert!(SensorPacket::MAX_ENCODED_LEN <= SensorPacket::MAX_WIRE_LEN);

impl<'a> SensorPacket<'a> {
    const SEQ_SIZE: usize = size_of::<u32>();
    pub const MAX_ENCODED_LEN: usize =
        Measurement::MAX_ENCODED_LEN * Measurement::VARIANT_COUNT + Self::SEQ_SIZE;
    // BLE AD payload is 255 - type (1 byte) - company id (2 bytes) = 252
    pub const MAX_WIRE_LEN: usize = 252;

    pub fn parse(bytes: &'a [u8]) -> Result<Self, DecodeError> {
        let (seq_bytes, payload) = bytes
            .split_first_chunk::<{ SensorPacket::SEQ_SIZE }>()
            .ok_or(Truncated)?;

        Ok(Self {
            seq: u32::from_le_bytes(*seq_bytes),
            payload,
        })
    }

    pub fn encode(
        seq: u32,
        measurements: &[Measurement],
        out: &mut [u8],
    ) -> Result<usize, BufferTooSmall> {
        let (seq_buf, data_buf) = out
            .split_at_mut_checked(Self::SEQ_SIZE)
            .ok_or(BufferTooSmall)?;
        seq_buf.copy_from_slice(&seq.to_le_bytes());

        let mut written = 0;

        for measurement in measurements {
            written += measurement.encode(&mut data_buf[written..])?;
        }

        written += Self::SEQ_SIZE;

        Ok(written)
    }

    pub fn measurements(&self) -> Measurements<'a> {
        Measurements { rest: self.payload }
    }
}

pub struct Measurements<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Measurements<'a> {
    type Item = Result<Measurement, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }

        match Measurement::decode(self.rest) {
            Ok((measurement, bytes_read)) => {
                self.rest = &self.rest[bytes_read..];
                Some(Ok(measurement))
            }

            Err(err) => {
                self.rest = &[];
                Some(Err(err))
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn round_trip() {
        let measurements = [
            Measurement::temperature(2150),
            Measurement::humidity(5150),
            Measurement::battery(3123),
        ];

        let seq = 5;

        let mut buf: [u8; SensorPacket::MAX_ENCODED_LEN] = [0; _];
        let bytes =
            SensorPacket::encode(seq, &measurements, &mut buf).expect("packet encoding failed");

        assert_eq!(bytes, 13, "encoded packet len invalid");

        let packet = SensorPacket::parse(&buf[..bytes]).expect("packet parsing failed");

        assert_eq!(packet.seq, seq, "packet seq differs");

        for (i, measurement) in packet.measurements().enumerate() {
            let measurement = measurement.expect("measurement decoding failed");

            assert_eq!(measurement, measurements[i], "decoded measurement differs");
        }
    }
}
