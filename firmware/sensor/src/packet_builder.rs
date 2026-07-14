use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, signal::Signal};
use homescope_common::{hardware_id::HardwareId, packet::SensorPacket};

use crate::sensors::Readings;

pub struct PacketBuilder {
    seq: u32,
    hardware_id: HardwareId,
}

impl PacketBuilder {
    pub fn new(hardware_id: HardwareId) -> Self {
        Self { seq: 0, hardware_id }
    }

    pub fn build(&mut self, readings: Readings, battery_mv: u16) -> SensorPacket {
        let packet = SensorPacket {
            hardware_id: self.hardware_id,
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
