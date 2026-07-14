use bytemuck::{Pod, Zeroable};

use crate::hardware_id::HardwareId;

#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct SensorPacket {
    pub hardware_id: HardwareId,
    pub seq: u32,
    pub temp_cdegc: i16,
    pub rh_cpercent: u16,
    pub battery_mv: u16,
}

impl SensorPacket {
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        bytemuck::pod_read_unaligned(bytes)
    }
}
