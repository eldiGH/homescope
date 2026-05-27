use bytemuck::{Pod, Zeroable};

use crate::device_id::DeviceId;

#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct SensorObservation {
    pub device_id: DeviceId,
    pub seq: u32,
    pub temp_cdegc: i16,
    pub humidity: u8,
    pub pressure_pa: u32,
    pub battery_mv: u16,
    pub rssi: i8,
    pub age_ms: u32,
}

impl SensorObservation {
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        bytemuck::pod_read_unaligned(bytes)
    }
}
