use std::{
    io::ErrorKind,
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use homescope_common::{
    frame::{FRAME_MAGIC_BYTES, FRAME_SIZE, Frame},
    observation::SensorObservation,
    reading::SensorReading,
};
use rumqttc::{AsyncClient, EventLoop, MqttOptions, QoS};
use serial2_tokio::SerialPort;
use tokio::{
    io,
    sync::mpsc::{Receiver, channel},
    time::sleep,
};
use tokio_util::{
    bytes::Buf,
    codec::{Decoder, FramedRead},
};

const PATH: &str = "/dev/homescope-receiver";

async fn mqtt_task(mut event_loop: EventLoop) {
    loop {
        if let Err(err) = event_loop.poll().await {
            println!("mqtt err: {err}")
        }
    }
}

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

async fn mqtt_readings_sender(
    mut reading_receiver: Receiver<SensorReading>,
    mqtt_client: AsyncClient,
) {
    while let Some(reading) = reading_receiver.recv().await {
        let serialized_reading = serde_json::to_vec(&reading);

        match serialized_reading {
            Ok(bytes) => {
                if let Err(err) = mqtt_client
                    .publish(
                        format!("homescope/sensors/{}/reading", reading.device_id),
                        QoS::AtLeastOnce,
                        false,
                        bytes,
                    )
                    .await
                {
                    println!("mqtt publish error: {err}")
                }
            }

            Err(err) => {
                println!("serialization error: {err}");
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let mqtt_options = MqttOptions::new("gateway", "127.0.0.1", 1883);
    let (client, event_loop) = AsyncClient::new(mqtt_options, 128);

    let (readings_sender, readings_receiver) = channel::<SensorReading>(1024);

    tokio::spawn(mqtt_task(event_loop));
    tokio::spawn(mqtt_readings_sender(readings_receiver, client));

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
                Ok(observation) => {
                    let received_at_ms = i64::try_from(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .expect("clock before UNIX epoch")
                            .as_millis()
                            .saturating_sub(u128::from(observation.age_ms)),
                    )
                    .expect("ts overflow");

                    let reading: SensorReading =
                        SensorReading::from_observation(observation, received_at_ms);
                    println!("Got seq: {}", reading.seq);

                    let _ = readings_sender.send(reading).await;
                }

                Err(err) => {
                    println!("Err: {err}");
                    break;
                }
            }
        }
    }
}
