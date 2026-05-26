use bytemuck::{Pod, Zeroable};

#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct SensorPacket {
    pub device_id: u8,
    pub seq: u32,
    pub temp_cdegc: i16,
    pub humidity: u8,
    pub pressure_pa: u32,
    pub battery_mv: u16,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    BadMagic,
    BadCrc,
}

impl SensorPacket {
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        bytemuck::pod_read_unaligned(bytes)
    }

    pub fn checksum(&self) -> u16 {
        SENSOR_PACKET_CRC.checksum(self.as_bytes())
    }

    pub fn write_frame(&self, out: &mut [u8; SENSOR_PACKET_FRAME_LEN]) {
        out[MAGIC_BYTES_START..MAGIC_BYTES_END].copy_from_slice(&SENSOR_PACKET_FRAME_MAGIC_BYTES);
        out[DATA_START..DATA_END].copy_from_slice(self.as_bytes());
        out[CRC_START..CRC_END].copy_from_slice(&self.checksum().to_le_bytes());
    }

    pub fn parse_frame(frame: &[u8; SENSOR_PACKET_FRAME_LEN]) -> Result<Self, FrameError> {
        if frame[MAGIC_BYTES_START..MAGIC_BYTES_END] != SENSOR_PACKET_FRAME_MAGIC_BYTES {
            return Err(FrameError::BadMagic);
        }

        let packet = Self::from_bytes(&frame[DATA_START..DATA_END]);
        let crc: [u8; 2] = frame[CRC_START..CRC_END].try_into().unwrap();

        if packet.checksum() != u16::from_le_bytes(crc) {
            return Err(FrameError::BadCrc);
        }

        Ok(packet)
    }

    pub fn frame(&self) -> [u8; SENSOR_PACKET_FRAME_LEN] {
        let mut frame = [0; _];
        self.write_frame(&mut frame);

        frame
    }
}

pub const SENSOR_PACKET_FRAME_MAGIC_BYTES: [u8; 2] = [b'H', b'S'];
const SENSOR_PACKET_SIZE: usize = size_of::<SensorPacket>();

const MAGIC_BYTES_SIZE: usize = SENSOR_PACKET_FRAME_MAGIC_BYTES.len();
static SENSOR_PACKET_CRC: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_IBM_SDLC);
const SENSOR_PACKET_CRC_SIZE: usize = 2;

const MAGIC_BYTES_START: usize = 0;
const MAGIC_BYTES_END: usize = MAGIC_BYTES_START + MAGIC_BYTES_SIZE;

const DATA_START: usize = MAGIC_BYTES_END;
const DATA_END: usize = DATA_START + SENSOR_PACKET_SIZE;

const CRC_START: usize = DATA_END;
const CRC_END: usize = CRC_START + SENSOR_PACKET_CRC_SIZE;

// sizeof SensorPacket + magic bytes + crc bytes
pub const SENSOR_PACKET_FRAME_LEN: usize =
    size_of::<SensorPacket>() + SENSOR_PACKET_FRAME_MAGIC_BYTES.len() + 2;
