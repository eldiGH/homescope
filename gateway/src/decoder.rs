use homescope_common::{
    frame::{FRAME_MAGIC_BYTES, FRAME_SIZE, Frame},
    observation::SensorObservation,
};
use tokio_util::{bytes::Buf as _, codec::Decoder};

pub struct SensorObservationDecoder;
impl Decoder for SensorObservationDecoder {
    type Item = SensorObservation;
    type Error = std::io::Error;

    fn decode(
        &mut self,
        src: &mut tokio_util::bytes::BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            let Some(magic_index) = memchr::memchr(FRAME_MAGIC_BYTES[0], src) else {
                return Ok(None);
            };

            src.advance(magic_index);

            if src.len() < FRAME_SIZE {
                return Ok(None);
            }

            if src[1] != FRAME_MAGIC_BYTES[1] {
                src.advance(1);
                continue;
            }

            match Frame::try_from_bytes(&src[..FRAME_SIZE].try_into().unwrap()) {
                Ok(frame) => {
                    src.advance(FRAME_SIZE);
                    return Ok(Some(frame.payload));
                }

                Err(_) => {
                    src.advance(1);
                    continue;
                }
            }
        }
    }
}
