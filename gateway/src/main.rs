use std::{io::ErrorKind, process, time::Duration};

use futures::StreamExt;
use homescope_common::{
    frame::{FRAME_MAGIC_BYTES, FRAME_SIZE, Frame},
    observation::SensorObservation,
    reading::SensorReading,
};
use serial2_tokio::SerialPort;
use tokio::{io, time::sleep};
use tokio_util::{
    bytes::Buf,
    codec::{Decoder, FramedRead},
};

const PATH: &str = "/dev/homescope-receiver";

struct SensorObservationDecoder;
impl Decoder for SensorObservationDecoder {
    type Item = SensorObservation;
    type Error = io::Error;

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

#[tokio::main]
async fn main() {
    loop {
        let port = match SerialPort::open(PATH, 115200) {
            Ok(port) => port,

            Err(err) => match err.kind() {
                ErrorKind::PermissionDenied => {
                    println!("permission denied to port: {PATH} ");
                    process::exit(1);
                }

                err => {
                    println!("error: {err} - retrying");
                    sleep(Duration::from_secs(1)).await;
                    continue;
                }
            },
        };

        let mut frames = FramedRead::new(port, SensorObservationDecoder);

        while let Some(result) = frames.next().await {
            match result {
                Ok(packet) => {
                    let reading: SensorReading = packet.into();
                    println!("Got seq: {}", reading.seq);
                }

                Err(err) => {
                    println!("Err: {err}");
                    break;
                }
            }
        }
    }
}
