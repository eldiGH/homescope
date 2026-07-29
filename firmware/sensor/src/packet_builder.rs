use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, signal::Signal};
use heapless::Vec;
use homescope_common::{measurement::Measurement, packet::SensorPacket, wire::BufferTooSmall};
use nrf_sdc::mpsl::FlashError;

use crate::seq_counter::SeqCounter;

pub struct PacketBuilder {
    seq_counter: SeqCounter,
}

impl PacketBuilder {
    pub fn new(seq_counter: SeqCounter) -> Self {
        Self { seq_counter }
    }

    pub async fn build(
        &mut self,
        measurements: &[Measurement],
    ) -> Result<PacketBuffer, PacketBuilderError> {
        let mut buffer: PacketBuffer = Vec::from([0; SensorPacket::MAX_AIR_LEN]);

        let buffer_len =
            SensorPacket::encode_air(self.seq_counter.next().await?, measurements, &mut buffer)?;

        buffer.truncate(buffer_len);
        Ok(buffer)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum PacketBuilderError {
    FlashError(FlashError),
    EncodeError(BufferTooSmall),
}

impl From<FlashError> for PacketBuilderError {
    fn from(value: FlashError) -> Self {
        PacketBuilderError::FlashError(value)
    }
}

impl From<BufferTooSmall> for PacketBuilderError {
    fn from(value: BufferTooSmall) -> Self {
        PacketBuilderError::EncodeError(value)
    }
}

pub type PacketBuffer = Vec<u8, { SensorPacket::MAX_AIR_LEN }>;
pub type PacketSignal = Signal<ThreadModeRawMutex, PacketBuffer>;
