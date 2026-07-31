use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, signal::Signal};
use heapless::Vec;
use homescope_common::{
    measurement::Measurement,
    packet::{EncodeError, SensorPacket, cipher::PacketCipher},
};
use nrf_sdc::mpsl::FlashError;

use crate::seq_counter::SeqCounter;

pub struct PacketBuilder {
    seq_counter: SeqCounter,
    cipher: PacketCipher,
}

impl PacketBuilder {
    pub fn new(seq_counter: SeqCounter, cipher: PacketCipher) -> Self {
        Self {
            seq_counter,
            cipher,
        }
    }

    pub async fn build(
        &mut self,
        measurements: &[Measurement],
    ) -> Result<PacketBuffer, PacketBuilderError> {
        let mut buffer: PacketBuffer = Vec::from([0; SensorPacket::MAX_AIR_LEN]);

        let buffer_len = SensorPacket::encode_air(
            self.seq_counter.next().await?,
            measurements,
            &mut buffer,
            &self.cipher,
        )?;

        buffer.truncate(buffer_len);
        Ok(buffer)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum PacketBuilderError {
    Flash(FlashError),
    Encode(EncodeError),
}

impl From<FlashError> for PacketBuilderError {
    fn from(value: FlashError) -> Self {
        PacketBuilderError::Flash(value)
    }
}

impl From<EncodeError> for PacketBuilderError {
    fn from(value: EncodeError) -> Self {
        PacketBuilderError::Encode(value)
    }
}

pub type PacketBuffer = Vec<u8, { SensorPacket::MAX_AIR_LEN }>;
pub type PacketSignal = Signal<ThreadModeRawMutex, PacketBuffer>;
