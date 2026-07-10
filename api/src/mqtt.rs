use anyhow::bail;
use homescope_common::reading::SensorReading;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS::AtLeastOnce};
use tokio::sync::mpsc::{Sender, error::TrySendError};
use tracing::{debug, error, info, warn};

pub async fn run(
    host: &str,
    port: u16,
    readings_sender: Sender<SensorReading>,
) -> anyhow::Result<()> {
    let mqtt_options = MqttOptions::new("api", host, port);
    let (client, mut event_loop) = AsyncClient::new(mqtt_options, 128);

    loop {
        match event_loop.poll().await {
            Err(err) => {
                error!("mqtt err: {err}")
            }

            Ok(Event::Incoming(Packet::Publish(publish))) => {
                debug!("message from: {}", publish.topic);

                let Ok(reading) = serde_json::from_slice::<SensorReading>(&publish.payload) else {
                    error!("Couldn't deserialize publish");
                    continue;
                };

                if let Err(err) = readings_sender.try_send(reading) {
                    match err {
                        TrySendError::Full(reading) => {
                            warn!(%reading, "reading insert queue full! couldn't insert reading")
                        }

                        TrySendError::Closed(_) => {
                            bail!("readings channel closed - store_readings task is gone")
                        }
                    }
                }
            }

            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                match client
                    .subscribe("homescope/sensors/+/reading", AtLeastOnce)
                    .await
                {
                    Ok(_) => {
                        info!("Subscribed to sensors");
                    }

                    Err(err) => {
                        error!("mqtt subscribe request failed: {err}");
                    }
                }
            }

            _ => {}
        }
    }
}
