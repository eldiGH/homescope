use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, signal::Signal};
use homescope_common::packet::SensorPacket;

use crate::sensors::Readings;

pub struct PacketBuilder {
    seq: u32,
}

impl PacketBuilder {
    pub fn new() -> Self {
        Self { seq: 0 }
    }

    pub fn build(&mut self, readings: Readings, battery_mv: u16) -> SensorPacket {
        let packet = SensorPacket {
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
