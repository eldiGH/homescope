use bytemuck::{Pod, Zeroable};

use crate::observation::SensorObservation;

#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Frame {
    pub magic_bytes: [u8; 2],
    pub payload: SensorObservation,
    pub crc: u16,
}

impl Frame {
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    fn from_bytes(bytes: &[u8; FRAME_SIZE]) -> Self {
        bytemuck::pod_read_unaligned(bytes)
    }

    pub fn try_from_bytes(bytes: &[u8; FRAME_SIZE]) -> Result<Self, FrameError> {
        let frame = Self::from_bytes(bytes);

        if frame.magic_bytes != FRAME_MAGIC_BYTES {
            return Err(FrameError::BadMagic);
        }

        let computed_crc = FRAME_CRC.checksum(frame.payload.as_bytes());
        if u16::from_le(frame.crc) != computed_crc {
            return Err(FrameError::BadCrc);
        }

        Ok(frame)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    BadMagic,
    BadCrc,
}

impl From<SensorObservation> for Frame {
    fn from(value: SensorObservation) -> Self {
        Self {
            magic_bytes: FRAME_MAGIC_BYTES,
            payload: value,
            crc: FRAME_CRC.checksum(value.as_bytes()).to_le(),
        }
    }
}

static FRAME_CRC: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_IBM_SDLC);

pub const FRAME_MAGIC_BYTES: [u8; 2] = [b'H', b'S'];
pub const FRAME_SIZE: usize = size_of::<Frame>();
