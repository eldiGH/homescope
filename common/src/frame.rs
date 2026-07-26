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
/// frame, garbage, or several frames. Judges position 0 only; on corruption
/// the returned [`FrameParse::Corrupt`] carries how far to skip to the next
/// candidate frame start. Each outcome demands a different caller action;
/// see the variants. The returned payload borrows from `bytes`.
pub fn parse(bytes: &[u8]) -> FrameParse<'_> {
    let mut rest = bytes;

    for byte in &MAGIC_BYTES {
        let Some((bytes_byte, rest_bytes)) = rest.split_first() else {
            return FrameParse::Incomplete;
        };

        if byte != bytes_byte {
            return FrameParse::Corrupt {
                error: FrameError::BadMagic,
                discard: seek_next_magic(bytes),
            };
        }

        rest = rest_bytes;
    }

    let len_payload_crc = rest;

    let Some((payload_len, payload_crc)) = len_payload_crc.split_first_chunk() else {
        return FrameParse::Incomplete;
    };
    let payload_len = PayloadLen::from_le_bytes(*payload_len);

    if payload_len as usize > MAX_PAYLOAD_LEN {
        return FrameParse::Corrupt {
            error: FrameError::PayloadTooLarge,
            discard: seek_next_magic(bytes),
        };
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
        return FrameParse::Corrupt {
            error: FrameError::BadCrc,
            discard: seek_next_magic(bytes),
        };
    }

    let payload = &payload_crc[..payload_len as usize];
    let frame_len = OVERHEAD + payload_len as usize;

    FrameParse::Ok {
        payload,
        consumed: frame_len,
    }
}

/// Distance from position 0 to the next possible frame start: the next
/// `MAGIC_BYTES[0]` strictly after position 0 (which has already been judged
/// corrupt), or the whole buffer if there is none. Always ≥ 1.
///
/// Invariant: `bytes` is non-empty — an empty buffer returns `Incomplete`
/// before any corrupt path is reached, so the `[1..]` slice cannot panic.
fn seek_next_magic(bytes: &[u8]) -> usize {
    memchr::memchr(MAGIC_BYTES[0], &bytes[1..])
        .map(|d| d + 1)
        .unwrap_or(bytes.len())
}

/// Outcome of [`parse`].
#[derive(Debug)]
pub enum FrameParse<'a> {
    /// Not enough bytes yet — keep the buffer and read more. Not an error:
    /// a frame split across two reads is the normal case, not the edge case.
    Incomplete,
    /// A valid frame: use `payload`, then discard exactly `consumed` bytes.
    Ok { payload: &'a [u8], consumed: usize },
    /// No valid frame at position 0 — discard exactly `discard` bytes and
    /// retry. `discard` is the distance to the next possible frame start
    /// (the next magic byte strictly after position 0, or the whole buffer
    /// if there is none), so it is always ≥ 1 and never skips an unexamined
    /// magic candidate: a false match may hide a real frame's start.
    Corrupt { error: FrameError, discard: usize },
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

    const TEST_PAYLOAD: [u8; 5] = [0x12, 0x34, 0x56, 0x78, 0x9A];

    fn encoded_frame() -> ([u8; MAX_LEN], usize) {
        let mut buffer: [u8; MAX_LEN] = [0; _];
        let written = encode(&mut buffer, &TestEncodeable(TEST_PAYLOAD)).unwrap();
        (buffer, written)
    }

    #[test]
    fn every_prefix_is_incomplete() {
        let (buffer, written) = encoded_frame();

        for cut in 0..written {
            assert!(
                matches!(parse(&buffer[..cut]), FrameParse::Incomplete),
                "prefix of {cut} bytes must be Incomplete, not Corrupt"
            );
        }
    }

    #[test]
    fn bad_magic() {
        for i in 0..MAGIC_BYTES_LEN {
            let (mut buffer, _) = encoded_frame();
            buffer[i] = 0x00;

            assert!(matches!(
                parse(&buffer),
                FrameParse::Corrupt {
                    error: FrameError::BadMagic,
                    discard
                } if discard == buffer.len()
            ));
        }
    }

    #[test]
    fn oversized_len_rejected_before_waiting_for_payload() {
        let (mut buffer, _) = encoded_frame();
        buffer[2..4].copy_from_slice(&((MAX_PAYLOAD_LEN + 1) as PayloadLen).to_le_bytes());

        // only magic + len present — must already be Corrupt, not Incomplete,
        // or a corrupted length stalls the stream parser
        assert!(matches!(
            parse(&buffer[..MAGIC_BYTES_LEN + PAYLOAD_LEN_SIZE]),
            FrameParse::Corrupt {
                error: FrameError::PayloadTooLarge,
                discard
            } if discard == MAGIC_BYTES_LEN + PAYLOAD_LEN_SIZE
        ));
    }

    #[test]
    fn corrupted_byte_fails_crc() {
        let (mut buffer, written) = encoded_frame();
        buffer[MAGIC_BYTES_LEN + PAYLOAD_LEN_SIZE] ^= 0xFF;
        assert!(matches!(
            parse(&buffer[..written]),
            FrameParse::Corrupt {
                error: FrameError::BadCrc,
                discard
            } if discard == written
        ));

        let (mut buffer, written) = encoded_frame();
        buffer[written - 1] ^= 0xFF;
        assert!(matches!(
            parse(&buffer[..written]),
            FrameParse::Corrupt {
                error: FrameError::BadCrc,
                discard
            } if discard == written
        ));
    }

    #[test]
    fn trailing_bytes_stay_untouched() {
        let (buffer, written) = encoded_frame();
        let mut stream = buffer[..written].to_vec();
        stream.extend_from_slice(&[0xAA, 0xBB, 0xCC]);

        let FrameParse::Ok { payload, consumed } = parse(&stream) else {
            panic!("frame with trailing bytes must still parse");
        };

        assert_eq!(consumed, written);
        assert_eq!(payload, TEST_PAYLOAD);
    }

    #[test]
    fn discard_hint_jumps_to_frame_after_garbage() {
        let (buffer, written) = encoded_frame();
        // garbage starting with a lone magic byte — a false frame start
        let mut stream = vec![b'H', 0x42, 0x00];
        stream.extend_from_slice(&buffer[..written]);

        // 'H' then 0x42 ≠ 'S' ⇒ BadMagic; the hint must land exactly on the
        // real frame's magic — one hop, not three
        let FrameParse::Corrupt { discard, .. } = parse(&stream) else {
            panic!("garbage prefix must be Corrupt");
        };
        assert_eq!(discard, 3);

        let FrameParse::Ok { payload, consumed } = parse(&stream[discard..]) else {
            panic!("stream contains a whole frame after the garbage");
        };
        assert_eq!(discard + consumed, stream.len());
        assert_eq!(payload, TEST_PAYLOAD);
    }

    #[test]
    fn encode_checks_buffer() {
        let payload = TestEncodeable(TEST_PAYLOAD);
        let mut buffer = [0u8; OVERHEAD + TEST_PAYLOAD.len() - 1];
        assert_eq!(encode(&mut buffer, &payload), Err(BufferTooSmall));
    }
}
