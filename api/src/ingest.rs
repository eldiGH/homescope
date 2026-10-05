use std::{convert::Infallible, time::Duration};

use anyhow::bail;
use homescope_common::{
    device_addr::DeviceAddr,
    envelope_topic::{EnvelopeTopic, EnvelopeTopicParseError},
    observation_envelope::ObservationEnvelope,
    reading::SensorReading,
    site::Site,
};
use homescope_host_util::mqtt::MqttConfig;
use rumqttc::{AsyncClient, Event, Packet, QoS::AtLeastOnce, SubscribeReasonCode};
use sqlx::PgPool;
use thiserror::Error;
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

/// An envelope plus the site its topic named — the publishing gateway's, which
/// under per-site broker ACLs is the one piece of site information the broker
/// vouches for. Provenance only: where the *device* is comes from its
/// placement in the database, and the two may legitimately differ.
struct ReceivedEnvelope {
    site: Site,
    envelope: ObservationEnvelope,
}

#[derive(Debug, Error)]
enum RejectedPublish {
    #[error("unexpected topic: {0}")]
    Topic(#[from] EnvelopeTopicParseError),

    #[error("envelope deserialization failed: {0}")]
    Payload(#[from] serde_json::Error),

    /// The gateway builds both from the same observation, so they cannot
    /// differ honestly: this is a bug or a forged publish.
    #[error("topic names device {topic} but the envelope carries {envelope}")]
    DeviceAddrMismatch {
        topic: DeviceAddr,
        envelope: DeviceAddr,
    },
}

/// One MQTT publish into an envelope, or the reason it is dropped. Kept free of
/// the event loop so it can be tested without a broker.
fn decode_publish(topic: &str, payload: &[u8]) -> Result<ReceivedEnvelope, RejectedPublish> {
    let topic: EnvelopeTopic = topic.parse()?;
    let envelope: ObservationEnvelope = serde_json::from_slice(payload)?;

    if topic.device_addr != envelope.device_addr {
        return Err(RejectedPublish::DeviceAddrMismatch {
            topic: topic.device_addr,
            envelope: envelope.device_addr,
        });
    }

    Ok(ReceivedEnvelope {
        site: topic.site,
        envelope,
    })
}

async fn store_envelopes(
    pool: PgPool,
    mut envelope_receiver: Receiver<ReceivedEnvelope>,
    devices: DeviceRegistry,
) -> anyhow::Result<Infallible> {
    let mut unknown_devices = UnknownDevices::default();

    while let Some(received) = envelope_receiver.recv().await {
        handle_envelope(&received, &pool, &devices, &mut unknown_devices).await;
    }

    bail!("ingestion channel closed");
}

#[instrument(skip_all, fields(site = %received.site, device_addr = %received.envelope.device_addr))]
async fn handle_envelope(
    received: &ReceivedEnvelope,
    pool: &PgPool,
    devices: &DeviceRegistry,
    unknown_devices: &mut UnknownDevices,
) {
    let envelope = &received.envelope;

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

    // TODO: log-and-continue loses every reading for the length of a database
    // outage, because rumqttc has already acked them. Restore `bail!`, then move
    // to manual acks. See docs/design/ingest-db-error-handling.md.
    if let Err(err) = db::insert_reading(pool, &reading, device.device_addr).await {
        error!(%err, "db error");
    }
}

async fn subscribe_mqtt(
    mqtt: &MqttConfig,
    envelope_sender: Sender<ReceivedEnvelope>,
) -> anyhow::Result<Infallible> {
    let mut mqtt_options = mqtt.options();
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

                let received = match decode_publish(&publish.topic, &publish.payload) {
                    Ok(received) => received,
                    Err(err @ RejectedPublish::DeviceAddrMismatch { .. }) => {
                        warn!(%err, topic = %publish.topic, "dropping publish");
                        continue;
                    }
                    Err(err) => {
                        error!(%err, topic = %publish.topic, "dropping publish");
                        continue;
                    }
                };

                if let Err(err) = envelope_sender.try_send(received) {
                    match err {
                        TrySendError::Full(received) => {
                            warn!(
                                site = %received.site,
                                device_addr = %received.envelope.device_addr,
                                received_at = %received.envelope.received_at,
                                "envelope insert queue full! couldn't insert reading"
                            )
                        }

                        TrySendError::Closed(_) => {
                            bail!("envelope channel closed - store_envelopes task is gone")
                        }
                    }
                }
            }

            // `session_present` says whether the broker kept this client's
            // durable session — and with it whatever it queued while the API
            // was away. `false` on a reconnect means that queue is gone.
            Ok(Event::Incoming(Packet::ConnAck(connack))) => {
                info!(
                    session_present = connack.session_present,
                    "connected to the MQTT broker"
                );

                match client
                    .subscribe(EnvelopeTopic::SUBSCRIPTION, AtLeastOnce)
                    .await
                {
                    Ok(_) => debug!("subscription requested"),
                    Err(err) => error!(%err, "mqtt subscribe request failed"),
                }
            }

            // The broker's answer, which the request above only asked for.
            // ⚠️ It cannot catch a missing ACL read rule on Mosquitto: 2.0's
            // acl_file *grants* such a subscription and then silently filters
            // every delivery (verified 2026-10-05 against 2.0.22). That case
            // shows up only as silence; the dev broker's copy of the
            // production ACL is what catches it before a deploy. What this
            // does catch is a QoS downgrade, and refusals from brokers or auth
            // plugins that do refuse. Logged rather than fatal: the next
            // reconnect resubscribes, so a broker-side fix heals it without
            // restarting the API.
            Ok(Event::Incoming(Packet::SubAck(suback))) => match suback.return_codes.as_slice() {
                [SubscribeReasonCode::Success(AtLeastOnce)] => {
                    info!(subscription = EnvelopeTopic::SUBSCRIPTION, "subscribed")
                }
                [SubscribeReasonCode::Success(qos)] => warn!(
                    subscription = EnvelopeTopic::SUBSCRIPTION,
                    ?qos,
                    "subscribed below QoS 1: envelopes sent while the API is down can be lost"
                ),
                [SubscribeReasonCode::Failure] => error!(
                    subscription = EnvelopeTopic::SUBSCRIPTION,
                    "broker refused the subscription — check this MQTT user's ACL; nothing will be ingested"
                ),
                codes => warn!(?codes, "unexpected SUBACK"),
            },

            _ => {}
        }
    }
}

pub async fn run(
    config: &ApiConfig,
    pool: PgPool,
    devices: DeviceRegistry,
) -> anyhow::Result<Infallible> {
    let (envelope_sender, envelope_receiver) = channel::<ReceivedEnvelope>(256);

    tokio::select! {
        r = subscribe_mqtt(&config.mqtt, envelope_sender) => r,
        r = store_envelopes(pool, envelope_receiver, devices) => r
    }
}

#[cfg(test)]
mod test {
    use chrono::DateTime;
    use homescope_common::wire::Dbm;

    use super::*;

    const ADDR: &str = "CEA99627BD3F";

    fn payload_for(device_addr: &str) -> Vec<u8> {
        serde_json::to_vec(&ObservationEnvelope {
            device_addr: device_addr.parse().expect("valid address"),
            rssi: Dbm(-70),
            received_at: DateTime::from_timestamp(1_760_000_000, 0).expect("valid timestamp"),
            packet: vec![0x01, 0x02, 0x03],
        })
        .expect("envelope serializes")
    }

    #[test]
    fn takes_the_site_from_the_topic() {
        let received = decode_publish(
            "homescope/odin/sensors/CEA99627BD3F/envelope",
            &payload_for(ADDR),
        )
        .expect("accepted");

        assert_eq!(received.site.as_str(), "odin");
        assert_eq!(received.envelope.device_addr, ADDR.parse().unwrap());
    }

    /// The topic and the envelope must name the same device; the gateway
    /// builds both from one observation.
    #[test]
    fn rejects_a_topic_naming_another_device() {
        let rejected = decode_publish(
            "homescope/odin/sensors/C0FFEE000001/envelope",
            &payload_for(ADDR),
        );

        assert!(
            matches!(rejected, Err(RejectedPublish::DeviceAddrMismatch { .. })),
            "{:?}",
            rejected.err()
        );
    }

    /// The subscription filter already excludes it on a real broker; the
    /// parser must not accept it either.
    #[test]
    fn rejects_the_pre_site_topic() {
        let rejected = decode_publish(
            "homescope/sensors/CEA99627BD3F/envelope",
            &payload_for(ADDR),
        );

        assert!(
            matches!(
                rejected,
                Err(RejectedPublish::Topic(EnvelopeTopicParseError::Shape))
            ),
            "{:?}",
            rejected.err()
        );
    }

    #[test]
    fn rejects_a_payload_that_is_not_an_envelope() {
        let rejected = decode_publish("homescope/odin/sensors/CEA99627BD3F/envelope", b"{}");

        assert!(
            matches!(rejected, Err(RejectedPublish::Payload(_))),
            "{:?}",
            rejected.err()
        );
    }
}
