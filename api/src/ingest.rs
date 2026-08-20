use std::{convert::Infallible, time::Duration};

use anyhow::bail;
use homescope_common::{observation_envelope::ObservationEnvelope, reading::SensorReading};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS::AtLeastOnce};
use sqlx::PgPool;
use tokio::{
    sync::mpsc::{Receiver, Sender, channel, error::TrySendError},
    time::sleep,
};
use tracing::{debug, error, info, instrument, warn};

use crate::{
    config::ApiConfig,
    db,
    devices::DeviceRegistry,
    ingest::unknown_devices::{Report, UnknownDevices},
};

mod unknown_devices;

async fn store_envelopes(
    pool: PgPool,
    mut envelope_receiver: Receiver<ObservationEnvelope>,
    devices: DeviceRegistry,
) -> anyhow::Result<Infallible> {
    let mut unknown_devices = UnknownDevices::default();

    while let Some(envelope) = envelope_receiver.recv().await {
        handle_envelope(&envelope, &pool, &devices, &mut unknown_devices).await;
    }

    bail!("ingestion channel closed");
}

#[instrument(skip_all, fields(device_addr = %envelope.device_addr))]
async fn handle_envelope(
    envelope: &ObservationEnvelope,
    pool: &PgPool,
    devices: &DeviceRegistry,
    unknown_devices: &mut UnknownDevices,
) {
    let Some(device) = devices.get(envelope.device_addr) else {
        match unknown_devices.record(envelope.device_addr) {
            Some(Report::New) => {
                warn!("unknown device, dropping its envelopes")
            }
            Some(Report::Repeat { count, since }) => {
                warn!(count, ?since, "unknown device still transmitting")
            }
            Some(Report::Overflow { packets }) => {
                warn!(
                    packets,
                    "packets from unknown devices beyond tracking capacity"
                )
            }
            None => {}
        }
        return;
    };

    let reading = match SensorReading::open_envelope(envelope, &device.cipher) {
        Ok(reading) => reading,
        Err(err) => {
            error!(%err, "invalid packet, dropping");
            return;
        }
    };

    if let Err(err) = db::insert_reading(pool, &reading, device.id).await {
        error!(%err, "db error");
    }
}

async fn subscribe_mqtt(
    host: &str,
    port: u16,
    envelope_sender: Sender<ObservationEnvelope>,
) -> anyhow::Result<Infallible> {
    let mut mqtt_options = MqttOptions::new("api", host, port);
    mqtt_options.set_clean_session(false);

    let (client, mut event_loop) = AsyncClient::new(mqtt_options, 128);

    loop {
        match event_loop.poll().await {
            Err(err) => {
                error!(%err, "mqtt error");
                sleep(Duration::from_secs(1)).await;
            }

            Ok(Event::Incoming(Packet::Publish(publish))) => {
                debug!(topic = %publish.topic, bytes = publish.payload.len(), "envelope received");

                let envelope = match serde_json::from_slice::<ObservationEnvelope>(&publish.payload)
                {
                    Ok(e) => e,
                    Err(err) => {
                        error!(%err, topic = %publish.topic, "envelope deserialization failed");
                        continue;
                    }
                };

                if let Err(err) = envelope_sender.try_send(envelope) {
                    match err {
                        TrySendError::Full(envelope) => {
                            warn!(
                                device_addr = %envelope.device_addr,
                                received_at = %envelope.received_at,
                                "envelope insert queue full! couldn't insert reading"
                            )
                        }

                        TrySendError::Closed(_) => {
                            bail!("envelope channel closed - store_envelopes task is gone")
                        }
                    }
                }
            }

            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                match client
                    .subscribe("homescope/sensors/+/envelope", AtLeastOnce)
                    .await
                {
                    Ok(_) => {
                        info!("Subscribed to sensors");
                    }

                    Err(err) => {
                        error!(%err, "mqtt subscribe request failed");
                    }
                }
            }

            _ => {}
        }
    }
}

pub async fn run(
    config: &ApiConfig,
    pool: PgPool,
    devices: DeviceRegistry,
) -> anyhow::Result<Infallible> {
    let (envelope_sender, envelope_receiver) = channel::<ObservationEnvelope>(256);

    tokio::select! {
        r = subscribe_mqtt(&config.mqtt_host, config.mqtt_port, envelope_sender) => r,
        r = store_envelopes(pool, envelope_receiver, devices) => r
    }
}
