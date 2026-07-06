use std::{
    collections::VecDeque,
    io::ErrorKind,
    process,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{Router, extract::State, response::Html, routing::get};
use futures::StreamExt;
use homescope_common::{
    device_id::DeviceId,
    frame::{FRAME_MAGIC_BYTES, FRAME_SIZE, Frame},
    observation::SensorObservation,
    reading::SensorReading,
};
use rumqttc::{AsyncClient, EventLoop, MqttOptions, QoS};
use serial2_tokio::SerialPort;
use tokio::{
    io,
    sync::{
        mpsc::{self, Receiver, channel},
        watch,
    },
    time::{self, sleep},
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

struct ReadingRecord {
    timestamp: Instant,
    reading: SensorReading,
}

// This function serves the web page to your phone
async fn serve_ui(State(rx): State<watch::Receiver<String>>) -> Html<String> {
    // Grab the latest benchmark string
    let content = rx.borrow().clone();

    // Wrap it in a dark-mode, auto-refreshing HTML page
    let html = format!(
        r#"
        <!DOCTYPE html>
        <html>
            <head>
                <meta name="viewport" content="width=device-width, initial-scale=1.0">
                <meta http-equiv="refresh" content="1">
                <style>
                    body {{
                        background-color: #121212;
                        color: #00ff00;
                        font-family: monospace;
                        font-size: 2vw; /* Scales text to phone screen */
                        padding: 20px;
                        margin: 0;
                    }}
                    pre {{ white-space: pre-wrap; }}
                </style>
            </head>
            <body>
                <pre>{}</pre>
            </body>
        </html>
        "#,
        content
    );

    Html(html)
}

#[tokio::main]
async fn main() {
    let mqtt_options = MqttOptions::new("gateway", "127.0.0.1", 1883);
    let (client, event_loop) = AsyncClient::new(mqtt_options, 128);

    let (readings_sender, readings_receiver) = channel::<SensorReading>(1024);

    tokio::spawn(mqtt_task(event_loop));
    tokio::spawn(mqtt_readings_sender(readings_receiver, client));

    let (tx, mut rx) = mpsc::channel::<ReadingRecord>(10000);
    let (ui_tx, ui_rx) = watch::channel("Waiting for first benchmark tick...".to_string());

    let app = Router::new().route("/", get(serve_ui)).with_state(ui_rx);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("🌐 UI Server running! Open http://<YOUR_PC_IP>:3000 on your phone");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    tokio::spawn(async move {
        // --- SLIDING WINDOW BENCHMARK ---
        //
        // The window holds every packet from the last `window_duration`.
        // Within one sensor run seqs are strictly increasing, so the number of
        // packets the sensor actually sent inside the window is simply
        // `last_seq - first_seq + 1` — reliability needs no cadence config.
        let mut interval = time::interval(Duration::from_secs(1));
        let mut window: VecDeque<ReadingRecord> = VecDeque::new();
        let window_duration = Duration::from_secs(10);

        // Lock onto the first device seen; packets from other sensors are
        // counted and ignored so they cannot corrupt the numbers.
        let mut device: Option<DeviceId> = None;
        let mut other_device_packets: u64 = 0;

        // Per-run state. A "run" ends when the sensor reboots, detected by
        // seq going backwards; all counters below then start over.
        let mut first_seq: Option<u32> = None;
        let mut highest_seq: Option<u32> = None;
        let mut run_received: u64 = 0;
        let mut run_dupes: u64 = 0;
        let mut run_started = Instant::now();
        let mut restarts: u32 = 0;
        let mut last_packet_at: Option<Instant> = None;

        loop {
            tokio::select! {
                // ==========================================
                // EVENT A: 1-SECOND TICK (REPORTING)
                // ==========================================
                _ = interval.tick() => {
                    let now = Instant::now();

                    while let Some(record) = window.front() {
                        if now.duration_since(record.timestamp) > window_duration {
                            window.pop_front();
                        } else {
                            break;
                        }
                    }

                    let win_secs = window_duration.as_secs();

                    let window_part = if window.is_empty() {
                        let last_seen = match last_packet_at {
                            Some(t) => format!("last packet {:.1}s ago", now.duration_since(t).as_secs_f32()),
                            None => "no packet seen yet".to_string(),
                        };
                        format!("⛔ [LAST {win_secs}s] NO PACKETS — {last_seen}")
                    } else {
                        let win_first_seq = window.front().unwrap().reading.seq;
                        let win_last_seq = window.back().unwrap().reading.seq;
                        let last_age = now
                            .duration_since(window.back().unwrap().timestamp)
                            .as_secs_f32();

                        let expected = u64::from(win_last_seq - win_first_seq) + 1;
                        let received = window.len() as u64;
                        let reliability = received as f64 / expected as f64 * 100.0;

                        let mut rssis: Vec<i8> = window.iter().map(|r| r.reading.rssi).collect();
                        rssis.sort_unstable();
                        let rssi_min = rssis[0];
                        let rssi_max = rssis[rssis.len() - 1];
                        let rssi_med = if rssis.len().is_multiple_of(2) {
                            (rssis[rssis.len() / 2 - 1] as f32 + rssis[rssis.len() / 2] as f32) / 2.0
                        } else {
                            rssis[rssis.len() / 2] as f32
                        };

                        // Longest blackout inside the window
                        let mut worst_gap_packets: u64 = 0;
                        let mut worst_gap_secs: f32 = 0.0;
                        for pair in window.make_contiguous().windows(2) {
                            let gap = u64::from(pair[1].reading.seq - pair[0].reading.seq) - 1;
                            if gap > worst_gap_packets {
                                worst_gap_packets = gap;
                                worst_gap_secs = pair[1]
                                    .timestamp
                                    .duration_since(pair[0].timestamp)
                                    .as_secs_f32();
                            }
                        }

                        format!(
                            "🎯 [LAST {win_secs}s] RELIABILITY: {reliability:.0}%  ({received}/{expected})\n\
                             📡 RSSI (min/med/max): {rssi_min} / {rssi_med:.1} / {rssi_max} dBm\n\
                             🕳 worst gap: {worst_gap_packets} packets ({worst_gap_secs:.1}s) | last packet {last_age:.1}s ago"
                        )
                    };

                    let run_part = {
                        let expected = match (first_seq, highest_seq) {
                            (Some(first), Some(highest)) => u64::from(highest - first) + 1,
                            _ => 0,
                        };
                        let reliability = if expected > 0 {
                            run_received as f64 / expected as f64 * 100.0
                        } else {
                            100.0
                        };
                        let run_secs = now.duration_since(run_started).as_secs();

                        format!(
                            "🌍 [RUN] {reliability:.1}% ({run_received}/{expected}) over {run_secs}s | dupes: {run_dupes}\n\
                             ⚠️ [HEALTH] sensor restarts: {restarts} | foreign packets ignored: {other_device_packets}"
                        )
                    };

                    let report = format!(
                        "========== BENCHMARK TICK ==========\n\
                         {window_part}\n\
                         ------------------------------------\n\
                         {run_part}\n\
                         ====================================\n"
                    );

                    println!("{report}");
                    let _ = ui_tx.send(report);
                }

                // ==========================================
                // EVENT B: NEW PACKET RECEIVED
                // ==========================================
                Some(record) = rx.recv() => {
                    match device {
                        None => device = Some(record.reading.device_id),
                        Some(locked) if locked != record.reading.device_id => {
                            other_device_packets += 1;
                            continue;
                        }
                        _ => {}
                    }

                    let seq = record.reading.seq;
                    last_packet_at = Some(record.timestamp);

                    match highest_seq {
                        // Same seq again: a duplicate slipped past the receiver's dedup
                        Some(highest) if seq == highest => run_dupes += 1,

                        // Seq went backwards: the sensor rebooted — start a fresh run
                        Some(highest) if seq < highest => {
                            restarts += 1;
                            window.clear();
                            first_seq = Some(seq);
                            highest_seq = Some(seq);
                            run_received = 1;
                            run_dupes = 0;
                            run_started = record.timestamp;
                            window.push_back(record);
                        }

                        // Normal case: a new, higher seq
                        _ => {
                            if first_seq.is_none() {
                                first_seq = Some(seq);
                                run_started = record.timestamp;
                            }
                            highest_seq = Some(seq);
                            run_received += 1;
                            window.push_back(record);
                        }
                    }
                }
            }
        }
    });

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

                    let _ = readings_sender.send(reading).await;

                    let _ = tx.try_send(ReadingRecord {
                        timestamp: Instant::now(),
                        reading,
                    });
                }

                Err(err) => {
                    println!("Err: {err}");
                    break;
                }
            }
        }
    }
}
