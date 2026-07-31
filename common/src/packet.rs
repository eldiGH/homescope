//! The air payload: `[ver][seq][sealed body]`, and the `HP` magic that wraps
//! it on air.
//!
//! Split in two along the key boundary, which is also the feature boundary:
//!
//! - **Always available** — [`SensorPacket::parse`] and
//!   [`SensorPacket::strip_air_magic`]. Keyless and version-blind: enough to
//!   recognise a Homescope packet and read `seq` off it for dedup, which is
//!   all the receiver and the gateway ever need.
//! - **Behind `crypto`** — `encode` and `decode`. Both take a
//!   [`PacketCipher`], so a `Metrics` cannot be obtained without a device key.
//!
//! That split is why keyless gateways are a compile-time property rather than
//! a convention: built without `crypto`, `SensorPacket` has no `decode` method
//! at all. See `cipher` for the AEAD construction and `v1` for the body.

use crate::{
    measurement::Measurement,
    wire::{BufferTooSmall, Truncated},
};

#[cfg(feature = "crypto")]
use crate::{
    measurement,
    packet::cipher::{DecryptionError, EncryptionError, PacketCipher},
    wire::{CentiCelsius, CentiPercent, Millivolts},
};

use thiserror::Error;

#[cfg(feature = "crypto")]
pub mod cipher;
#[cfg(feature = "crypto")]
mod v1;

const AIR_MAGIC: [u8; 2] = *b"HP";
const AIR_MAGIC_SIZE: usize = AIR_MAGIC.len();

#[cfg(feature = "crypto")]
pub const VERSION: u8 = v1::VERSION;

const _: () = assert!(SensorPacket::MAX_AIR_LEN <= SensorPacket::MAX_WIRE_LEN);

/// Poly1305 tag, trailing the ciphertext.
///
/// A property of the wire format, not of the cipher — "16 of these bytes are a
/// tag" is true of a packet sitting in a keyless gateway's buffer, and the
/// packet-size constants below depend on it. `cipher` pins it against the
/// algorithm's actual tag length.
const AEAD_TAG_SIZE: usize = 16;

/// A borrowed view over one air packet.
///
/// `ver` and `seq` are cleartext by necessity — `ver` selects the decoder and
/// `seq` is what the receiver dedups on, so both must be readable before the
/// body can be. Neither is *untrusted*: both are bound as associated data, so
/// altering either costs the attacker the tag.
#[derive(Clone, Copy)]
pub struct SensorPacket<'a> {
    pub ver: u8,
    pub seq: u32,

    /// Ciphertext `||` tag. Opaque without a key, hence unread in builds
    /// without `crypto` — that is the keyless-gateway property, not dead code.
    #[cfg_attr(not(feature = "crypto"), allow(dead_code))]
    body: &'a [u8],
}

impl<'a> SensorPacket<'a> {
    const SEQ_SIZE: usize = size_of::<u32>();
    const VER_SIZE: usize = size_of::<u8>();
    const OVERHEAD_SIZE: usize = Self::SEQ_SIZE + Self::VER_SIZE;
    const MAX_BODY_LEN: usize =
        Measurement::MAX_ENCODED_LEN * Measurement::VARIANT_COUNT + AEAD_TAG_SIZE;

    pub const MAX_AIR_LEN: usize = AIR_MAGIC_SIZE + Self::MAX_ENCODED_LEN;
    pub const MAX_ENCODED_LEN: usize = Self::MAX_BODY_LEN + Self::OVERHEAD_SIZE;
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

    pub fn strip_air_magic(bytes: &[u8]) -> Result<&[u8], StripAirMagicError> {
        if bytes.len() < AIR_MAGIC_SIZE {
            return Err(StripAirMagicError::Truncated(Truncated));
        }

        bytes
            .strip_prefix(&AIR_MAGIC)
            .ok_or(StripAirMagicError::InvalidPrefix)
    }
}

#[cfg(feature = "crypto")]
impl<'a> SensorPacket<'a> {
    pub fn decode(&self, cipher: &PacketCipher) -> Result<Metrics, DecodeError> {
        let mut scratch: [u8; SensorPacket::MAX_BODY_LEN] = [0; _];
        let buf = scratch
            .get_mut(..self.body.len())
            .ok_or(DecodeError::TooLong)?;
        buf.copy_from_slice(self.body);

        let plaintext = cipher.decrypt_in_place(self.ver, self.seq, buf)?;

        match self.ver {
            v1::VERSION => Ok(v1::decode(plaintext)?),

            ver => Err(DecodeError::UnsupportedVersion(ver)),
        }
    }

    pub fn encode(
        seq: u32,
        measurements: &[Measurement],
        out: &mut [u8],
        cipher: &PacketCipher,
    ) -> Result<usize, EncodeError> {
        let ver = VERSION;

        let (ver_buf, rest) = out
            .split_first_chunk_mut::<{ SensorPacket::VER_SIZE }>()
            .ok_or(BufferTooSmall)?;
        ver_buf[0] = ver;

        let (seq_buf, body_buf) = rest
            .split_first_chunk_mut::<{ SensorPacket::SEQ_SIZE }>()
            .ok_or(BufferTooSmall)?;
        seq_buf.copy_from_slice(&seq.to_le_bytes());

        let mut measurements_written = 0;

        for measurement in measurements {
            measurements_written += measurement.encode(&mut body_buf[measurements_written..])?;
        }

        let (data_buf, rest) = body_buf
            .split_at_mut_checked(measurements_written)
            .ok_or(BufferTooSmall)?;

        let (tag_buf, _) = rest
            .split_first_chunk_mut::<AEAD_TAG_SIZE>()
            .ok_or(BufferTooSmall)?;

        cipher.encrypt_in_place(ver, seq, data_buf, tag_buf)?;

        Ok(Self::OVERHEAD_SIZE + measurements_written + AEAD_TAG_SIZE)
    }

    pub fn encode_air(
        seq: u32,
        measurements: &[Measurement],
        out: &mut [u8],
        cipher: &PacketCipher,
    ) -> Result<usize, EncodeError> {
        let (magic_buf, rest) = out
            .split_first_chunk_mut::<AIR_MAGIC_SIZE>()
            .ok_or(BufferTooSmall)?;

        magic_buf.copy_from_slice(&AIR_MAGIC);

        Ok(AIR_MAGIC_SIZE + Self::encode(seq, measurements, rest, cipher)?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum StripAirMagicError {
    #[error(transparent)]
    Truncated(#[from] Truncated),

    #[error("invalid prefix")]
    InvalidPrefix,
}

#[cfg(feature = "crypto")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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

    #[error("body is too long")]
    TooLong,

    #[error("decryption failed: {0}")]
    Decryption(#[from] DecryptionError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum EncodeError {
    #[error("output buffer too small")]
    OutputBufferTooSmall(#[from] BufferTooSmall),

    #[cfg(feature = "crypto")]
    #[error("encryption failed: {0}")]
    Encryption(#[from] EncryptionError),
}

#[cfg(feature = "crypto")]
struct Measurements<'a> {
    rest: &'a [u8],
}

#[cfg(feature = "crypto")]
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
#[cfg(feature = "crypto")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metrics {
    pub battery: Option<Millivolts>,
    pub temperature: Option<CentiCelsius>,
    pub relative_humidity: Option<CentiPercent>,
}

#[cfg(all(test, feature = "crypto"))]
mod test {
    use std::{vec, vec::Vec};

    use super::*;
    use crate::{
        device_addr::DeviceAddr,
        measurement::MeasurementIdUnknownError,
        packet::cipher::{DecryptionError, PacketCipher},
    };

    const SEQ: u32 = 5;
    const ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);

    const KEY: [u8; 32] = [
        0xCE, 0x57, 0xF1, 0xC9, 0x9D, 0xA6, 0x14, 0x42, 0x14, 0x0A, 0x9F, 0x58, 0xD2, 0xC4, 0x54,
        0x7B, 0xDB, 0x68, 0x40, 0xDC, 0xCB, 0xFE, 0x41, 0x56, 0x86, 0x26, 0x3D, 0xD8, 0xAC, 0x2B,
        0x0D, 0x1B,
    ];

    /// The three measurements the fleet emits today, in registry order.
    const ALL_THREE: [Measurement; 3] = [
        Measurement::Battery(Millivolts(2950)),
        Measurement::Temperature(CentiCelsius(2105)),
        Measurement::Humidity(CentiPercent(4875)),
    ];

    fn cipher() -> PacketCipher {
        PacketCipher::new(&KEY, ADDR)
    }

    fn encoded(cipher: &PacketCipher, seq: u32, measurements: &[Measurement]) -> Vec<u8> {
        let mut buf = [0u8; SensorPacket::MAX_ENCODED_LEN];
        let written =
            SensorPacket::encode(seq, measurements, &mut buf, cipher).expect("encoding failed");

        buf[..written].to_vec()
    }

    fn decoded(cipher: &PacketCipher, bytes: &[u8]) -> Result<Metrics, DecodeError> {
        SensorPacket::parse(bytes)
            .expect("parse failed")
            .decode(cipher)
    }

    /// Hand-builds a packet **without** sealing the body, so tests can reach
    /// states no encoder produces — a short header, an over-long body.
    fn raw_packet(ver: u8, seq: u32, body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![ver];
        bytes.extend_from_slice(&seq.to_le_bytes());
        bytes.extend_from_slice(body);

        bytes
    }

    /// Hand-builds a packet and seals `body` properly, so tests can reach
    /// states the encoder cannot while still passing authentication — an
    /// unknown `ver`, an unknown measurement id, a body cut mid-value.
    fn sealed_packet(cipher: &PacketCipher, ver: u8, seq: u32, body: &[u8]) -> Vec<u8> {
        let mut sealed = body.to_vec();
        sealed.resize(body.len() + AEAD_TAG_SIZE, 0);

        let (data, tag) = sealed.split_at_mut(body.len());
        let tag: &mut [u8; AEAD_TAG_SIZE] = tag.try_into().expect("tag region is exactly one tag");

        cipher
            .encrypt_in_place(ver, seq, data, tag)
            .expect("sealing failed");

        raw_packet(ver, seq, &sealed)
    }

    #[test]
    fn round_trip() {
        let cipher = cipher();
        let bytes = encoded(&cipher, SEQ, &ALL_THREE);

        assert_eq!(
            bytes.len(),
            30,
            "1 B ver + 4 B seq + three 3 B measurements + 16 B tag"
        );

        let packet = SensorPacket::parse(&bytes).expect("parse failed");
        assert_eq!(packet.ver, VERSION);
        assert_eq!(packet.seq, SEQ);

        assert_eq!(
            packet.decode(&cipher).expect("decode failed"),
            Metrics {
                battery: Some(Millivolts(2950)),
                temperature: Some(CentiCelsius(2105)),
                relative_humidity: Some(CentiPercent(4875)),
            }
        );
    }

    /// The fold is order-independent by construction, so wire order must not
    /// change the outcome.
    #[test]
    fn measurement_order_does_not_matter() {
        let cipher = cipher();

        let forward = encoded(&cipher, SEQ, &ALL_THREE);

        let mut backwards = ALL_THREE;
        backwards.reverse();
        let reversed = encoded(&cipher, SEQ, &backwards);

        assert_eq!(decoded(&cipher, &forward), decoded(&cipher, &reversed));
    }

    /// Partial packets are the point of the encoding: a node whose SHT45 read
    /// failed still reports its battery, and the absent metrics stay `None`
    /// rather than failing the packet.
    #[test]
    fn absent_measurements_stay_none() {
        let cipher = cipher();
        let bytes = encoded(&cipher, SEQ, &[Measurement::battery(2800)]);

        assert_eq!(
            decoded(&cipher, &bytes),
            Ok(Metrics {
                battery: Some(Millivolts(2800)),
                ..Metrics::default()
            })
        );
    }

    // --- air layer -------------------------------------------------------

    #[test]
    fn air_round_trip_strips_the_magic() {
        let cipher = cipher();

        let mut buf = [0u8; SensorPacket::MAX_AIR_LEN];
        let written =
            SensorPacket::encode_air(SEQ, &[Measurement::battery(2950)], &mut buf, &cipher)
                .expect("air encoding failed");

        assert_eq!(&buf[..AIR_MAGIC_SIZE], &AIR_MAGIC);
        assert_eq!(
            written,
            AIR_MAGIC_SIZE + SensorPacket::OVERHEAD_SIZE + 3 + AEAD_TAG_SIZE
        );

        let stripped = SensorPacket::strip_air_magic(&buf[..written]).expect("magic check failed");
        assert_eq!(stripped.len(), written - AIR_MAGIC_SIZE);

        assert_eq!(
            decoded(&cipher, stripped).unwrap().battery,
            Some(Millivolts(2950))
        );
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
        let needed = AIR_MAGIC_SIZE + SensorPacket::OVERHEAD_SIZE + 3 + AEAD_TAG_SIZE;
        let mut buf = vec![0u8; needed - 1];

        assert_eq!(
            SensorPacket::encode_air(SEQ, &[Measurement::battery(2950)], &mut buf, &cipher()),
            Err(EncodeError::OutputBufferTooSmall(BufferTooSmall))
        );
    }

    // --- versioning ------------------------------------------------------

    /// The load-bearing test for the whole versioning design: `parse` is
    /// version-blind *and keyless*, so the receiver can still read `seq` off a
    /// node running firmware it has never heard of (and therefore keep it
    /// visible), while `decode` is what refuses to interpret the body.
    #[test]
    fn parse_is_version_blind_and_keyless() {
        let bytes = raw_packet(7, 99, &[0x01, 0x86, 0x0B]);

        let packet = SensorPacket::parse(&bytes).expect("parse must not inspect ver");
        assert_eq!(packet.ver, 7);
        assert_eq!(packet.seq, 99);
    }

    /// `decode` authenticates *before* dispatching on `ver`, so
    /// `UnsupportedVersion` is only reachable for a packet that genuinely came
    /// from the device — it means "stale firmware", never "garbage on the
    /// air". That separation is what makes the error actionable: this one says
    /// *reflash the node*, where a failed tag says *re-provision it*.
    #[test]
    fn authentic_packet_at_an_unknown_version_is_named_as_such() {
        let cipher = cipher();
        let bytes = sealed_packet(&cipher, 7, SEQ, &[0x01, 0x86, 0x0B]);

        assert_eq!(
            decoded(&cipher, &bytes),
            Err(DecodeError::UnsupportedVersion(7))
        );
    }

    /// The packet-level half of `cipher::wrong_ver_is_rejected`: `ver` travels
    /// in cleartext, so nothing stops an attacker rewriting it — but it is
    /// bound as associated data, so doing so costs them the tag.
    #[test]
    fn tampered_ver_byte_fails_authentication() {
        let cipher = cipher();
        let mut bytes = encoded(&cipher, SEQ, &ALL_THREE);

        bytes[0] = 7;

        assert_eq!(
            decoded(&cipher, &bytes),
            Err(DecodeError::Decryption(DecryptionError::Authentication))
        );
    }

    /// Same for `seq`, which the receiver dedups on and this crate derives the
    /// AEAD nonce from.
    #[test]
    fn tampered_seq_fails_authentication() {
        let cipher = cipher();
        let mut bytes = encoded(&cipher, SEQ, &ALL_THREE);

        bytes[SensorPacket::VER_SIZE] ^= 0x01;

        assert_eq!(
            decoded(&cipher, &bytes),
            Err(DecodeError::Decryption(DecryptionError::Authentication))
        );
    }

    /// Golden **plaintext** v1 bytes — the TV section as `v1` sees it, after
    /// the crypto layer has already done its job.
    ///
    /// Keeping `v1` keyless is what lets this stay a readable literal: were
    /// decryption to move into the version module, these bytes would become
    /// ciphertext and the test would pin a key instead of a wire format. The
    /// sealed counterpart is `cipher::known_answer`.
    ///
    /// If this fails the wire format has changed and every deployed node is
    /// speaking a different protocol — bump `ver`, do not "fix" the
    /// expectation.
    #[test]
    fn v1_golden_plaintext_bytes() {
        const GOLDEN: [u8; 9] = [
            0x01, 0x86, 0x0B, // battery     = 2950 mV
            0x02, 0x39, 0x08, // temperature = 21.05 °C
            0x03, 0x0B, 0x13, // humidity    = 48.75 %RH
        ];

        assert_eq!(
            v1::decode(&GOLDEN),
            Ok(Metrics {
                battery: Some(Millivolts(2950)),
                temperature: Some(CentiCelsius(2105)),
                relative_humidity: Some(CentiPercent(4875)),
            })
        );

        // …and the encoder still produces exactly those bytes. Reached through
        // the sealed packet so the assertion covers the real encode path.
        let sealed = encoded(&cipher(), 42, &ALL_THREE);
        let plaintext = SensorPacket::parse(&sealed)
            .unwrap()
            .decode(&cipher())
            .expect("decode failed");

        assert_eq!(plaintext, v1::decode(&GOLDEN).unwrap());
        assert_eq!(
            sealed.len(),
            SensorPacket::OVERHEAD_SIZE + GOLDEN.len() + AEAD_TAG_SIZE
        );
    }

    // --- decode errors ---------------------------------------------------

    /// Well formed on the wire, useless as a row — rejected rather than
    /// stored as an all-NULL reading. Note the packet is still 21 bytes: an
    /// empty TV section is 16 bytes of tag over nothing.
    #[test]
    fn empty_body_is_rejected() {
        let cipher = cipher();
        let bytes = encoded(&cipher, SEQ, &[]);

        assert_eq!(
            bytes.len(),
            SensorPacket::OVERHEAD_SIZE + AEAD_TAG_SIZE,
            "header plus a tag over an empty section"
        );
        assert_eq!(decoded(&cipher, &bytes), Err(DecodeError::Empty));
    }

    /// A repeated id means the packet contradicts itself and there is no rule
    /// for picking a winner. The error carries the *second* value — the one
    /// that found its slot taken.
    #[test]
    fn duplicate_measurement_is_rejected() {
        let cipher = cipher();
        let bytes = encoded(
            &cipher,
            SEQ,
            &[
                Measurement::temperature(2100),
                Measurement::temperature(2200),
            ],
        );

        assert_eq!(
            decoded(&cipher, &bytes),
            Err(DecodeError::Duplicate(Measurement::temperature(2200)))
        );
    }

    /// No length byte means an unknown id makes everything after it
    /// unparseable, so the whole packet goes even though the measurement in
    /// front of it decoded cleanly.
    #[test]
    fn unknown_measurement_id_rejects_the_whole_packet() {
        let cipher = cipher();
        let bytes = sealed_packet(&cipher, VERSION, SEQ, &[0x02, 0x39, 0x08, 0x7F, 0x00, 0x00]);

        assert_eq!(
            decoded(&cipher, &bytes),
            Err(DecodeError::Measurement(
                MeasurementIdUnknownError(0x7F).into()
            ))
        );
    }

    /// The three failure stages are deliberately distinct errors: a short
    /// header never reaches `DecodeError` at all, a body that cannot hold a
    /// tag is a crypto-layer rejection, and a body cut mid-value arrives
    /// wrapped as a measurement failure — only reachable once the packet has
    /// authenticated.
    #[test]
    fn truncation_is_reported_at_the_layer_that_found_it() {
        for len in 0..SensorPacket::OVERHEAD_SIZE {
            assert!(
                matches!(SensorPacket::parse(&[0u8; 8][..len]), Err(Truncated)),
                "header of {len} bytes should be truncated"
            );
        }

        let cipher = cipher();

        let no_room_for_tag = raw_packet(VERSION, SEQ, &[0x02, 0x39]);
        assert_eq!(
            decoded(&cipher, &no_room_for_tag),
            Err(DecodeError::Decryption(DecryptionError::MissingTag))
        );

        let cut_value = sealed_packet(&cipher, VERSION, SEQ, &[0x02, 0x39]);
        assert_eq!(
            decoded(&cipher, &cut_value),
            Err(DecodeError::Measurement(Truncated.into()))
        );
    }

    /// `decode` copies the body into a fixed-size stack scratch buffer, so an
    /// over-long body has to be a named rejection rather than a panic — this
    /// length arrives off MQTT and is attacker-influenced. Such a packet
    /// cannot be authentic anyway; the sensor has no way to emit one.
    #[test]
    fn over_long_body_is_rejected() {
        let cipher = cipher();
        let bytes = raw_packet(VERSION, SEQ, &[0u8; SensorPacket::MAX_BODY_LEN + 1]);

        assert_eq!(decoded(&cipher, &bytes), Err(DecodeError::TooLong));
    }
}
