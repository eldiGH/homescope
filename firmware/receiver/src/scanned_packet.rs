use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::Instant;
use heapless::Vec;
use homescope_common::{device_addr::DeviceAddr, packet::SensorPacket, wire::Dbm};

pub struct ScannedPacket {
    pub device_addr: DeviceAddr,
    pub captured_at: Instant,
    pub rssi: Dbm,
    pub packet: Vec<u8, { SensorPacket::MAX_WIRE_LEN }>,
}

pub type ScannedPacketChannel = Channel<NoopRawMutex, ScannedPacket, 512>;

// RAM check, as this channel may get large
const _: () = assert!(size_of::<ScannedPacketChannel>() <= 150_000);
