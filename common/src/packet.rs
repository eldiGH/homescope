use crate::{
    measurement::{self, Measurement},
    wire::{BufferTooSmall, CentiCelsius, CentiPercent, Millivolts, Truncated},
};

use thiserror::Error;

mod v1;

const AIR_MAGIC: [u8; 2] = *b"HP";
const AIR_MAGIC_SIZE: usize = AIR_MAGIC.len();
const VERSION: u8 = v1::VERSION;

const _: () = assert!(SensorPacket::MAX_AIR_LEN <= SensorPacket::MAX_WIRE_LEN);

#[derive(Clone, Copy)]
pub struct SensorPacket<'a> {
    pub ver: u8,
    pub seq: u32,
    body: &'a [u8],
}

impl<'a> SensorPacket<'a> {
    const SEQ_SIZE: usize = size_of::<u32>();
    const VER_SIZE: usize = size_of::<u8>();
    const OVERHEAD_SIZE: usize = Self::SEQ_SIZE + Self::VER_SIZE;

    pub const MAX_AIR_LEN: usize = AIR_MAGIC_SIZE + Self::MAX_ENCODED_LEN;
    pub const MAX_ENCODED_LEN: usize =
        Measurement::MAX_ENCODED_LEN * Measurement::VARIANT_COUNT + Self::OVERHEAD_SIZE;
    // BLE AD payload is 255 - type (1 byte) - company id (2 bytes) = 252
    pub const MAX_WIRE_LEN: usize = 252;

    pub fn parse(bytes: &'a [u8]) -> Result<Self, Truncated> {
        let (&ver, rest) = bytes.split_first().ok_or(Truncated)?;

        let (seq_bytes, body) = rest.split_first_chunk().ok_or(Truncated)?;

        Ok(Self {
            ver,
            seq: u32::from_le_bytes(*seq_bytes),
            body,
        })
    }

    pub fn decode(&self) -> Result<Metrics, DecodeError> {
        match self.ver {
            v1::VERSION => Ok(v1::decode(self.body)?),

            ver => Err(DecodeError::UnsupportedVersion(ver)),
        }
    }

    pub fn encode(
        seq: u32,
        measurements: &[Measurement],
        out: &mut [u8],
    ) -> Result<usize, BufferTooSmall> {
        let (ver_buf, rest) = out
            .split_first_chunk_mut::<{ SensorPacket::VER_SIZE }>()
            .ok_or(BufferTooSmall)?;
        ver_buf[0] = VERSION;

        let (seq_buf, data_buf) = rest
            .split_first_chunk_mut::<{ SensorPacket::SEQ_SIZE }>()
            .ok_or(BufferTooSmall)?;
        seq_buf.copy_from_slice(&seq.to_le_bytes());

        let mut written = 0;

        for measurement in measurements {
            written += measurement.encode(&mut data_buf[written..])?;
        }

        written += Self::OVERHEAD_SIZE;

        Ok(written)
    }

    pub fn encode_air(
        seq: u32,
        measurements: &[Measurement],
        out: &mut [u8],
    ) -> Result<usize, BufferTooSmall> {
        let (magic_buf, rest) = out
            .split_first_chunk_mut::<AIR_MAGIC_SIZE>()
            .ok_or(BufferTooSmall)?;

        magic_buf.copy_from_slice(&AIR_MAGIC);

        Ok(AIR_MAGIC_SIZE + Self::encode(seq, measurements, rest)?)
    }

    pub fn strip_air_magic(bytes: &[u8]) -> Result<&[u8], StripAirMagicError> {
        if bytes.len() < AIR_MAGIC_SIZE {
            return Err(StripAirMagicError::Truncated(Truncated));
        }

        bytes
            .strip_prefix(&AIR_MAGIC)
            .ok_or(StripAirMagicError::InvalidPrefix)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum StripAirMagicError {
    #[error(transparent)]
    Truncated(#[from] Truncated),

    #[error("invalid prefix")]
    InvalidPrefix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DecodeError {
    #[error("packet header truncated")]
    Header(#[from] Truncated),

    #[error("unsupported version: {0}")]
    UnsupportedVersion(u8),

    #[error("measurement: {0}")]
    Measurement(#[from] measurement::DecodeError),

    #[error("duplicate measurement: {0}")]
    Duplicate(Measurement),

    #[error("body has no measurements")]
    Empty,
}

struct Measurements<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Measurements<'a> {
    type Item = Result<Measurement, measurement::DecodeError>;

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

/// One packet's measurements folded into named slots — the canonical,
/// version-independent shape every version's decoder produces.
///
/// `Default` is the fold accumulator: every field is `Option`, so `None` is
/// always the right answer for a version that did not carry that metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metrics {
    pub battery: Option<Millivolts>,
    pub temperature: Option<CentiCelsius>,
    pub relative_humidity: Option<CentiPercent>,
}

#[cfg(test)]
mod test {
    use std::{vec, vec::Vec};

    use super::*;
    use crate::measurement::MeasurementIdUnknownError;

    const SEQ: u32 = 5;

    fn encoded(seq: u32, measurements: &[Measurement]) -> Vec<u8> {
        let mut buf = [0u8; SensorPacket::MAX_ENCODED_LEN];
        let written = SensorPacket::encode(seq, measurements, &mut buf).expect("encoding failed");

        buf[..written].to_vec()
    }

    /// Hand-builds a packet so tests can reach states the encoder cannot
    /// produce — an unsupported version, a body cut mid-measurement.
    fn packet_with(ver: u8, seq: u32, body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![ver];
        bytes.extend_from_slice(&seq.to_le_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn round_trip() {
        let measurements = [
            Measurement::temperature(2150),
            Measurement::humidity(5150),
            Measurement::battery(3123),
        ];

        let bytes = encoded(SEQ, &measurements);
        assert_eq!(
            bytes.len(),
            14,
            "1 B ver + 4 B seq + three 3 B measurements"
        );

        let packet = SensorPacket::parse(&bytes).expect("parse failed");
        assert_eq!(packet.ver, VERSION);
        assert_eq!(packet.seq, SEQ);

        assert_eq!(
            packet.decode().expect("decode failed"),
            Metrics {
                battery: Some(Millivolts(3123)),
                temperature: Some(CentiCelsius(2150)),
                relative_humidity: Some(CentiPercent(5150)),
            }
        );
    }

    /// The fold is order-independent by construction, so wire order must not
    /// change the outcome.
    #[test]
    fn measurement_order_does_not_matter() {
        let forward = encoded(
            SEQ,
            &[
                Measurement::battery(2950),
                Measurement::temperature(2105),
                Measurement::humidity(4875),
            ],
        );
        let reversed = encoded(
            SEQ,
            &[
                Measurement::humidity(4875),
                Measurement::temperature(2105),
                Measurement::battery(2950),
            ],
        );

        assert_eq!(
            SensorPacket::parse(&forward).unwrap().decode(),
            SensorPacket::parse(&reversed).unwrap().decode()
        );
    }

    /// Partial packets are the point of the encoding: a node whose SHT45 read
    /// failed still reports its battery, and the absent metrics stay `None`
    /// rather than failing the packet.
    #[test]
    fn absent_measurements_stay_none() {
        let bytes = encoded(SEQ, &[Measurement::battery(2800)]);

        assert_eq!(
            SensorPacket::parse(&bytes).unwrap().decode(),
            Ok(Metrics {
                battery: Some(Millivolts(2800)),
                ..Metrics::default()
            })
        );
    }

    // --- air layer -------------------------------------------------------

    #[test]
    fn air_round_trip_strips_the_magic() {
        let mut buf = [0u8; SensorPacket::MAX_AIR_LEN];
        let written = SensorPacket::encode_air(SEQ, &[Measurement::battery(2950)], &mut buf)
            .expect("air encoding failed");

        assert_eq!(&buf[..AIR_MAGIC_SIZE], &AIR_MAGIC);
        assert_eq!(written, AIR_MAGIC_SIZE + SensorPacket::OVERHEAD_SIZE + 3);

        let stripped = SensorPacket::strip_air_magic(&buf[..written]).expect("magic check failed");
        assert_eq!(stripped.len(), written - AIR_MAGIC_SIZE);

        let packet = SensorPacket::parse(stripped).expect("parse failed");
        assert_eq!(packet.seq, SEQ);
        assert_eq!(packet.decode().unwrap().battery, Some(Millivolts(2950)));
    }

    /// The dongle's only filter against shared `0xFFFF` airspace. `b"HS"` is
    /// the *frame* magic — checked here so the two layers can't quietly
    /// converge on one constant.
    #[test]
    fn strip_air_magic_rejects_foreign_and_short_input() {
        assert_eq!(
            SensorPacket::strip_air_magic(&[]),
            Err(StripAirMagicError::Truncated(Truncated))
        );
        assert_eq!(
            SensorPacket::strip_air_magic(b"H"),
            Err(StripAirMagicError::Truncated(Truncated))
        );
        assert_eq!(
            SensorPacket::strip_air_magic(&[b'H', b'S', 0x01]),
            Err(StripAirMagicError::InvalidPrefix)
        );
        assert_eq!(SensorPacket::strip_air_magic(&AIR_MAGIC), Ok(&[][..]));
    }

    #[test]
    fn encode_air_rejects_a_buffer_one_byte_short() {
        let needed = AIR_MAGIC_SIZE + SensorPacket::OVERHEAD_SIZE + 3;
        let mut buf = vec![0u8; needed - 1];

        assert_eq!(
            SensorPacket::encode_air(SEQ, &[Measurement::battery(2950)], &mut buf),
            Err(BufferTooSmall)
        );
    }

    // --- versioning ------------------------------------------------------

    /// The load-bearing test for the whole versioning design: `parse` is
    /// version-blind so the receiver can still read `seq` off a node running
    /// firmware it has never heard of (and therefore keep it *visible*),
    /// while `decode` is what refuses to interpret the body.
    #[test]
    fn parse_is_version_blind_and_decode_is_not() {
        let bytes = packet_with(7, 99, &[0x01, 0x86, 0x0B]);

        let packet = SensorPacket::parse(&bytes).expect("parse must not inspect ver");
        assert_eq!(packet.ver, 7);
        assert_eq!(packet.seq, 99);

        assert_eq!(packet.decode(), Err(DecodeError::UnsupportedVersion(7)));
    }

    /// Golden v1 bytes. If this fails the wire format has changed and every
    /// deployed node is speaking a different protocol — bump `ver`, do not
    /// "fix" the expectation.
    #[test]
    fn v1_golden_bytes() {
        const GOLDEN: [u8; 14] = [
            0x01, // ver = 1
            0x2A, 0x00, 0x00, 0x00, // seq = 42
            0x01, 0x86, 0x0B, // battery     = 2950 mV
            0x02, 0x39, 0x08, // temperature = 21.05 °C
            0x03, 0x0B, 0x13, // humidity    = 48.75 %RH
        ];

        let packet = SensorPacket::parse(&GOLDEN).expect("parse failed");
        assert_eq!(packet.ver, 1);
        assert_eq!(packet.seq, 42);
        assert_eq!(
            packet.decode(),
            Ok(Metrics {
                battery: Some(Millivolts(2950)),
                temperature: Some(CentiCelsius(2105)),
                relative_humidity: Some(CentiPercent(4875)),
            })
        );

        assert_eq!(
            encoded(
                42,
                &[
                    Measurement::battery(2950),
                    Measurement::temperature(2105),
                    Measurement::humidity(4875),
                ]
            ),
            GOLDEN,
            "encoder drifted from the v1 wire format"
        );
    }

    // --- decode errors ---------------------------------------------------

    /// Well formed on the wire, useless as a row — rejected rather than
    /// stored as an all-NULL reading.
    #[test]
    fn empty_body_is_rejected() {
        let bytes = encoded(SEQ, &[]);
        assert_eq!(bytes.len(), SensorPacket::OVERHEAD_SIZE);

        assert_eq!(
            SensorPacket::parse(&bytes).unwrap().decode(),
            Err(DecodeError::Empty)
        );
    }

    /// A repeated id means the packet contradicts itself and there is no rule
    /// for picking a winner. The error carries the *second* value — the one
    /// that found its slot taken.
    #[test]
    fn duplicate_measurement_is_rejected() {
        let bytes = encoded(
            SEQ,
            &[
                Measurement::temperature(2100),
                Measurement::temperature(2200),
            ],
        );

        assert_eq!(
            SensorPacket::parse(&bytes).unwrap().decode(),
            Err(DecodeError::Duplicate(Measurement::temperature(2200)))
        );
    }

    /// No length byte means an unknown id makes everything after it
    /// unparseable, so the whole packet goes even though the measurement in
    /// front of it decoded cleanly.
    #[test]
    fn unknown_measurement_id_rejects_the_whole_packet() {
        let bytes = packet_with(VERSION, SEQ, &[0x02, 0x39, 0x08, 0x7F, 0x00, 0x00]);

        assert_eq!(
            SensorPacket::parse(&bytes).unwrap().decode(),
            Err(DecodeError::Measurement(
                MeasurementIdUnknownError(0x7F).into()
            ))
        );
    }

    /// The two truncations are deliberately different errors: a short header
    /// never reaches `DecodeError` at all, while a body cut mid-value arrives
    /// wrapped as a measurement failure. Both share `wire::Truncated`; only
    /// the variant says where it happened.
    #[test]
    fn header_and_body_truncation_are_distinct() {
        for len in 0..SensorPacket::OVERHEAD_SIZE {
            assert!(
                matches!(SensorPacket::parse(&[0u8; 8][..len]), Err(Truncated)),
                "header of {len} bytes should be truncated"
            );
        }

        let cut_body = packet_with(VERSION, SEQ, &[0x02, 0x39]);
        assert_eq!(
            SensorPacket::parse(&cut_body).unwrap().decode(),
            Err(DecodeError::Measurement(Truncated.into()))
        );
    }
}
