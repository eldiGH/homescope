use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, signal::Signal};
use homescope_common::packet::SensorPacket;
use nrf_sdc::mpsl::FlashError;

use crate::{sensors::Readings, seq_counter::SeqCounter};

pub struct PacketBuilder {
    seq_counter: SeqCounter,
}

impl PacketBuilder {
    pub fn new(seq_counter: SeqCounter) -> Self {
        Self { seq_counter }
    }

    pub async fn build(&mut self, readings: Readings, battery_mv: u16) -> Result<SensorPacket, FlashError> {
        Ok(SensorPacket {
            seq: self.seq_counter.next().await?,
            temp_cdegc: readings.temp_cdegc,
            rh_cpercent: readings.rh_cpercent,
            battery_mv,
        })
    }
}

pub type PacketSignal = Signal<ThreadModeRawMutex, SensorPacket>;
