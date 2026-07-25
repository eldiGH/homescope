//! Content-agnostic USB-CDC framing: `[magic "HS"][len: u16 LE][payload][crc: u16 LE]`.
//!
//! The payload is an opaque byte string — supplied via [`Encode`] on the way
//! in, returned as a borrowed slice on the way out. The CRC (CRC-16/IBM-SDLC)
//! covers the length field and the payload, not the magic. Full spec:
//! `docs/protocol.md`.

use crate::wire::BufferTooSmall;
use thiserror::Error;

type CrcType = u16;
const CRC_LEN: usize = size_of::<CrcType>();
static FRAME_CRC: crc::Crc<CrcType> = crc::Crc::<CrcType>::new(&crc::CRC_16_IBM_SDLC);

/// Frame-boundary marker; lets a stream parser resync after corruption.
pub const MAGIC_BYTES: [u8; 2] = [b'H', b'S'];
const MAGIC_BYTES_LEN: usize = MAGIC_BYTES.len();

/// Largest payload the frame protocol carries.
///
/// A transport-level cap, deliberately not derived from any payload type: it
/// bounds how much data a streaming parser will wait for on a corrupted
/// length field. Payload types prove they fit via a const assert.
pub const MAX_PAYLOAD_LEN: usize = 1024;
type PayloadLen = u16;
const PAYLOAD_LEN_SIZE: usize = size_of::<PayloadLen>();

/// Frame bytes surrounding the payload (magic + length + CRC).
/// Size encode buffers as `OVERHEAD + T::MAX_ENCODED_LEN`.
pub const OVERHEAD: usize = CRC_LEN + MAGIC_BYTES_LEN + PAYLOAD_LEN_SIZE;

/// Largest possible frame: [`OVERHEAD`] + [`MAX_PAYLOAD_LEN`].
pub const MAX_LEN: usize = OVERHEAD + MAX_PAYLOAD_LEN;

/// A payload that can write itself into a frame.
///
/// Contract: `encode` writes at most `MAX_ENCODED_LEN` bytes to the front of
/// `out` and returns exactly the count written. [`encode`](crate::frame::encode)
/// relies on this to size the length field and CRC.
pub trait Encode {
    /// Upper bound on the encoded size, for buffer sizing and the
    /// compile-time fit check against [`MAX_PAYLOAD_LEN`].
    const MAX_ENCODED_LEN: usize;

    /// Writes one complete frame containing `payload` into `out`, returning the
    /// total frame length.
    ///
    /// A payload type whose `MAX_ENCODED_LEN` exceeds [`MAX_PAYLOAD_LEN`] fails
    /// to compile at the call site.
    fn encode(&self, out: &mut [u8]) -> Result<usize, BufferTooSmall>;
}

pub fn encode<T: Encode>(out: &mut [u8], payload: &T) -> Result<usize, BufferTooSmall> {
    const { assert!(T::MAX_ENCODED_LEN <= MAX_PAYLOAD_LEN) }

    let (magic_bytes, len_payload_crc) = out
        .split_first_chunk_mut::<{ MAGIC_BYTES_LEN }>()
        .ok_or(BufferTooSmall)?;
    magic_bytes.copy_from_slice(&MAGIC_BYTES);

    let (payload_len, payload_crc) = len_payload_crc
        .split_first_chunk_mut::<PAYLOAD_LEN_SIZE>()
        .ok_or(BufferTooSmall)?;

    let payload_size = payload.encode(payload_crc)?;
    debug_assert!(payload_size <= T::MAX_ENCODED_LEN);

    payload_len.copy_from_slice(&(payload_size as PayloadLen).to_le_bytes());

    let checksummed = &len_payload_crc[..PAYLOAD_LEN_SIZE + payload_size];
    let checksum = FRAME_CRC.checksum(checksummed);

    let (crc, _) = len_payload_crc[payload_size + PAYLOAD_LEN_SIZE..]
        .split_first_chunk_mut::<CRC_LEN>()
        .ok_or(BufferTooSmall)?;
    crc.copy_from_slice(&checksum.to_le_bytes());

    Ok(OVERHEAD + payload_size)
}

/// Tries to read one frame from the start of `bytes`.
///
/// Built for streaming: `bytes` is a growing buffer that may hold a partial
/// frame, garbage, or several frames. Judges position 0 only — hunting for
/// the next magic is the caller's job. Each [`FrameParse`] outcome demands a
/// different caller action; see its variants. The returned payload borrows
/// from `bytes`.
pub fn parse(bytes: &[u8]) -> FrameParse<'_> {
    let mut rest = bytes;

    for byte in &MAGIC_BYTES {
        let Some((bytes_byte, rest_bytes)) = rest.split_first() else {
            return FrameParse::Incomplete;
        };

        if byte != bytes_byte {
            return FrameParse::Corrupt(FrameError::BadMagic);
        }

        rest = rest_bytes;
    }

    let len_payload_crc = rest;

    let Some((payload_len, payload_crc)) = len_payload_crc.split_first_chunk() else {
        return FrameParse::Incomplete;
    };
    let payload_len = PayloadLen::from_le_bytes(*payload_len);

    if payload_len as usize > MAX_PAYLOAD_LEN {
        return FrameParse::Corrupt(FrameError::PayloadTooLarge);
    }

    if payload_crc.len() < payload_len as usize {
        return FrameParse::Incomplete;
    }

    let Some((crc, _)) = payload_crc[payload_len as usize..].split_first_chunk() else {
        return FrameParse::Incomplete;
    };
    let crc = CrcType::from_le_bytes(*crc);

    let checksummed = &len_payload_crc[..PAYLOAD_LEN_SIZE + payload_len as usize];
    let calculated_crc = FRAME_CRC.checksum(checksummed);

    if crc != calculated_crc {
        return FrameParse::Corrupt(FrameError::BadCrc);
    }

    let payload = &payload_crc[..payload_len as usize];

    FrameParse::Ok {
        payload,
        consumed: OVERHEAD + payload_len as usize,
    }
}

/// Outcome of [`parse`].
#[derive(Debug)]
pub enum FrameParse<'a> {
    /// Not enough bytes yet — keep the buffer and read more. Not an error:
    /// a frame split across two reads is the normal case, not the edge case.
    Incomplete,
    /// A valid frame: use `payload`, then discard exactly `consumed` bytes.
    Ok { payload: &'a [u8], consumed: usize },
    /// No valid frame at this position — discard exactly **one** byte and
    /// retry: a false magic match may hide a real frame one byte later.
    Corrupt(FrameError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum FrameError {
    #[error("invalid magic bytes")]
    BadMagic,
    #[error("payload len exceeds max")]
    PayloadTooLarge,
    #[error("crc check failed")]
    BadCrc,
}

#[cfg(test)]
mod test {
    use super::*;

    struct TestEncodeable([u8; 5]);
    impl Encode for TestEncodeable {
        const MAX_ENCODED_LEN: usize = 5;

        fn encode(&self, out: &mut [u8]) -> Result<usize, BufferTooSmall> {
            let (out, _) = out.split_first_chunk_mut::<5>().ok_or(BufferTooSmall)?;

            out.copy_from_slice(&self.0);

            Ok(5)
        }
    }

    #[test]
    fn round_trip() {
        let payload = TestEncodeable([0x12, 0x34, 0x56, 0x78, 0x9A]);

        let mut buffer: [u8; MAX_LEN] = [0; _];
        let written = encode(&mut buffer, &payload).unwrap();

        let FrameParse::Ok {
            payload: parsed_payload,
            consumed,
        } = parse(&buffer)
        else {
            panic!("frame not parsed");
        };

        assert_eq!(consumed, written);
        assert_eq!(payload.0, parsed_payload);
    }
}
