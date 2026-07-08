use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, signal::Signal};
use homescope_common::{device_id::DeviceId, packet::SensorPacket};

use crate::sensors::Readings;

pub struct PacketBuilder {
    seq: u32,
    device_id: DeviceId,
}

impl PacketBuilder {
    pub fn new(device_id: DeviceId) -> Self {
        Self { seq: 0, device_id }
    }

    pub fn build(&mut self, readings: Readings, battery_mv: u16) -> SensorPacket {
        let packet = SensorPacket {
            device_id: self.device_id,
            seq: self.seq,
            temp_cdegc: readings.temp_cdegc,
            rh_cpercent: readings.rh_cpercent,
            battery_mv,
        };

        self.seq += 1;

        packet
    }
}

pub type PacketSignal = Signal<ThreadModeRawMutex, SensorPacket>;
