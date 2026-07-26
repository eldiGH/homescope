use homescope_common::frame::{self, FrameParse};
use tokio_util::{
    bytes::{Buf as _, BytesMut},
    codec::Decoder,
};
use tracing::warn;

pub struct FrameDecoder;
impl Decoder for FrameDecoder {
    type Item = Vec<u8>;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            match frame::parse(src) {
                FrameParse::Ok { payload, consumed } => {
                    let payload = payload.to_vec();

                    src.advance(consumed);
                    return Ok(Some(payload));
                }

                FrameParse::Incomplete => return Ok(None),

                FrameParse::Corrupt { discard, error } => {
                    warn!(%error, discard, "corrupt frame data");
                    src.advance(discard);
                }
            }
        }
    }
}
