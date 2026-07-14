use std::{convert::Infallible, time::Duration};

use anyhow::bail;
use homescope_common::reading::SensorReading;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS::AtLeastOnce};
use sqlx::PgPool;
use tokio::{
    sync::mpsc::{Receiver, Sender, channel, error::TrySendError},
    time::sleep,
};
use tracing::{debug, error, info, warn};

use crate::{config::ApiConfig, db, devices::DeviceRegistry, ingest::unknown::Report};

mod unknown;

async fn store_readings(
    pool: PgPool,
    mut readings_receiver: Receiver<SensorReading>,
    devices: DeviceRegistry,
) -> anyhow::Result<Infallible> {
    let mut unknown = unknown::UnknownDevices::default();

    while let Some(reading) = readings_receiver.recv().await {
        debug!("reading to insert: {reading}");

        let Some(device) = devices.get(reading.hardware_id) else {
            match unknown.record(reading.hardware_id) {
                Some(Report::New) => {
                    warn!(hardware_id = %reading.hardware_id, "unknown device, dropping its readings")
                }
                Some(Report::Repeat { count, since }) => {
                    warn!(hardware_id = %reading.hardware_id, count, ?since,
                  "unknown device still transmitting")
                }
                Some(Report::Overflow { packets }) => {
                    warn!(
                        packets,
                        "packets from unknown devices beyond tracking capacity"
                    )
                }
                None => {}
            }
            continue;
        };

        if let Err(err) = db::insert_reading(&pool, &reading, device.id).await {
            error!("db error: {err}");
        }
    }

    bail!("ingestion channel closed");
}

async fn subscribe_mqtt(
    host: &str,
    port: u16,
    readings_sender: Sender<SensorReading>,
) -> anyhow::Result<Infallible> {
    let mut mqtt_options = MqttOptions::new("api", host, port);
    mqtt_options.set_clean_session(false);

    let (client, mut event_loop) = AsyncClient::new(mqtt_options, 128);

    loop {
        match event_loop.poll().await {
            Err(err) => {
                error!("mqtt err: {err}");
                sleep(Duration::from_secs(1)).await;
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

pub async fn run(
    config: &ApiConfig,
    pool: PgPool,
    devices: DeviceRegistry,
) -> anyhow::Result<Infallible> {
    let (readings_sender, readings_receiver) = channel::<SensorReading>(256);

    tokio::select! {
        r = subscribe_mqtt(&config.mqtt_host, config.mqtt_port, readings_sender) => r,
        r = store_readings(pool, readings_receiver, devices) => r
    }
}
