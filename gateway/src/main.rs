use std::time::Duration;

use anyhow::{Context as _, bail};
use chrono::Utc;
use futures::StreamExt;
use homescope_common::{observation::SensorObservation, observation_envelope::ObservationEnvelope};
use rumqttc::{AsyncClient, EventLoop, MqttOptions, QoS};
use serial2_tokio::SerialPort;
use tokio::{
    sync::mpsc::{Receiver, channel},
    time::sleep,
};
use tokio_util::codec::FramedRead;
use tracing::{debug, error, warn};

use crate::{config::GatewayConfig, decoder::FrameDecoder};

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

async fn mqtt_envelope_sender(
    mut envelope_receiver: Receiver<ObservationEnvelope>,
    mqtt_client: AsyncClient,
) {
    while let Some(envelope) = envelope_receiver.recv().await {
        match serde_json::to_vec(&envelope) {
            Ok(bytes) => {
                if let Err(err) = mqtt_client
                    .publish(
                        format!("homescope/sensors/{}/envelope", envelope.device_addr),
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

    let (envelope_sender, envelope_receiver) = channel::<ObservationEnvelope>(1024);

    tokio::spawn(mqtt_task(event_loop));
    tokio::spawn(mqtt_envelope_sender(envelope_receiver, client));

    let port = SerialPort::open(&config.receiver_path, 115200)
        .with_context(|| format!("opening {}", &config.receiver_path))?;

    let mut frames = FramedRead::new(port, FrameDecoder);

    while let Some(result) = frames.next().await {
        let observation_bytes = match result {
            Ok(bytes) => bytes,
            Err(err) => bail!("serial read error: {err}"),
        };

        let observation = match SensorObservation::parse(&observation_bytes) {
            Ok(observation) => observation,
            Err(err) => {
                warn!("invalid observation packet: {err}");
                continue;
            }
        };

        let envelope = ObservationEnvelope::from_observation(observation, Utc::now());

        debug!(
            device_addr = %envelope.device_addr,
            rssi = envelope.rssi.0,
            received_at = %envelope.received_at,
            packet_len = envelope.packet.len(),
            "envelope decoded"
        );

        if envelope_sender.send(envelope).await.is_err() {
            bail!("envelope channel closed. fatal error")
        }
    }

    bail!("serial stream ended");
}
