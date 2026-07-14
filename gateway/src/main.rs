use std::time::Duration;

use anyhow::{Context as _, bail};
use chrono::Utc;
use futures::StreamExt;
use homescope_common::reading::SensorReading;
use rumqttc::{AsyncClient, EventLoop, MqttOptions, QoS};
use serial2_tokio::SerialPort;
use tokio::{
    sync::mpsc::{Receiver, channel},
    time::sleep,
};
use tokio_util::codec::FramedRead;
use tracing::{debug, error};

use crate::{config::GatewayConfig, decoder::SensorObservationDecoder};

mod config;
mod decoder;

async fn mqtt_task(mut event_loop: EventLoop) {
    loop {
        if let Err(err) = event_loop.poll().await {
            error!("mqtt err: {err}");
            sleep(Duration::from_secs(1)).await;
        }
    }
}

async fn mqtt_readings_sender(
    mut reading_receiver: Receiver<SensorReading>,
    mqtt_client: AsyncClient,
) {
    while let Some(reading) = reading_receiver.recv().await {
        match serde_json::to_vec(&reading) {
            Ok(bytes) => {
                if let Err(err) = mqtt_client
                    .publish(
                        format!("homescope/sensors/{}/reading", reading.hardware_id),
                        QoS::AtLeastOnce,
                        false,
                        bytes,
                    )
                    .await
                {
                    error!("mqtt publish error: {err}")
                }
            }

            Err(err) => {
                error!("serialization error: {err}");
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    homescope_host_util::init();
    let config = GatewayConfig::from_env()?;

    let mqtt_options = MqttOptions::new("gateway", &config.mqtt_host, config.mqtt_port);
    let (client, event_loop) = AsyncClient::new(mqtt_options, 128);

    let (readings_sender, readings_receiver) = channel::<SensorReading>(1024);

    tokio::spawn(mqtt_task(event_loop));
    tokio::spawn(mqtt_readings_sender(readings_receiver, client));

    let port = SerialPort::open(&config.receiver_path, 115200)
        .with_context(|| format!("opening {}", &config.receiver_path))?;

    let mut frames = FramedRead::new(port, SensorObservationDecoder);

    while let Some(result) = frames.next().await {
        match result {
            Ok(observation) => {
                let received_at =
                    Utc::now() - chrono::TimeDelta::milliseconds(observation.age_ms.into());
                let reading: SensorReading =
                    SensorReading::from_observation(observation, received_at);
                debug!("packet: {}", reading);

                if readings_sender.send(reading).await.is_err() {
                    bail!("readings channel closed. fatal error")
                }
            }

            Err(err) => bail!("serial read error: {err}"),
        }
    }

    bail!("serial stream ended");
}
