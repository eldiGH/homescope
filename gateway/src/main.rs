use std::{
    collections::{HashSet, VecDeque},
    io::ErrorKind,
    process,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{Router, extract::State, response::Html, routing::get};
use futures::StreamExt;
use homescope_common::{
    frame::{FRAME_MAGIC_BYTES, FRAME_SIZE, Frame},
    observation::SensorObservation,
    reading::SensorReading,
};
use rumqttc::{AsyncClient, EventLoop, MqttOptions, QoS};
use serial2_tokio::SerialPort;
use tokio::{
    io, mpsc,
    time::{self, sleep},
    watch,
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
        // --- 1. SLIDING WINDOW SETUP ---
        let mut interval = time::interval(Duration::from_secs(1));
        let mut window_buffer: VecDeque<ReadingRecord> = VecDeque::new();
        let window_duration = Duration::from_secs(5);

        // --- 2. GLOBAL BENCHMARK STATE ---
        let mut global_count: u64 = 0;
        let mut global_rssi_min: i8 = i8::MAX;
        let mut global_rssi_max: i8 = i8::MIN;
        let mut global_rssi_sum: i64 = 0;
        let mut global_rssi_histogram = [0u64; 256];

        // Sequence & Reliability Tracking
        let mut first_seq: Option<u32> = None;
        let mut highest_seq: Option<u32> = None;
        let mut seen_seqs: HashSet<u32> = HashSet::new();

        let mut total_dupes: u64 = 0;
        let mut max_consecutive_drops: u64 = 0;
        let mut current_consecutive_drops: u64 = 0;

        loop {
            tokio::select! {
                            // ==========================================
                            // EVENT A: 1-SECOND TICK (REPORTING)
                            // ==========================================
                            _ = interval.tick() => {
                                let now = Instant::now();

                                // 1. Sliding Window: Pop records older than 5 seconds
                                while let Some(record) = window_buffer.front() {
                                    if now.duration_since(record.timestamp) > window_duration {
                                        window_buffer.pop_front();
                                    } else {
                                        break; // Front is within 5s, so the rest are too
                                    }
                                }

                                if window_buffer.is_empty() {
                                    println!("--- [Sliding 5s Window: NO DATA] ---");
                                    continue;
                                }

                                let window_count = window_buffer.len();

                                // 2. LOCAL RSSI STATS (Min, Max, Avg, Median)
                                let mut window_rssis: Vec<i8> = window_buffer.iter().map(|r| r.reading.rssi).collect();
                                window_rssis.sort_unstable();

                                let window_rssi_min = window_rssis.first().copied().unwrap_or(0);
                                let window_rssi_max = window_rssis.last().copied().unwrap_or(0);
                                let window_rssi_sum: i32 = window_rssis.iter().map(|&r| r as i32).sum();
                                let window_rssi_avg = window_rssi_sum as f32 / window_count as f32;

                                let window_rssi_med = if window_count == 0 {
                                    0.0
                                } else if window_count.is_multiple_of(2){
                                    let mid = window_count / 2;
                                    (window_rssis[mid - 1] as f32 + window_rssis[mid] as f32) / 2.0
                                } else {
                                    window_rssis[window_count / 2] as f32
                                };

                                // 3. LOCAL TIMING STATS (Intervals & Jitter)
                                let mut intervals_ms = Vec::with_capacity(window_count.saturating_sub(1));

                                // .make_contiguous() safely gives us a flat slice to run .windows() on
                                for pair in window_buffer.make_contiguous().windows(2) {
                                    let diff = pair[1].timestamp.duration_since(pair[0].timestamp).as_secs_f32() * 1000.0;
                                    intervals_ms.push(diff);
                                }
                                intervals_ms.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());

                                let (int_min, int_max, int_avg, int_med) = if intervals_ms.is_empty() {
                                    (0.0, 0.0, 0.0, 0.0)
                                } else {
                                    let min = *intervals_ms.first().unwrap();
                                    let max = *intervals_ms.last().unwrap();
                                    let sum: f32 = intervals_ms.iter().sum();
                                    let avg = sum / intervals_ms.len() as f32;

                                    let mid = intervals_ms.len() / 2;
                                    let med = if intervals_ms.len() % 2 == 0 {
                                        (intervals_ms[mid - 1] + intervals_ms[mid]) / 2.0
                                    } else {
                                        intervals_ms[mid]
                                    };
                                    (min, max, avg, med)
                                };

            // 4. GLOBAL RELIABILITY & RSSI STATS
                                let highest = highest_seq.unwrap_or(0);
                                let first = first_seq.unwrap_or(0);

                                let expected_packets = (highest + 1).saturating_sub(first) as u64;
                                let unique_received = global_count.saturating_sub(total_dupes);
                                let total_dropped = expected_packets.saturating_sub(unique_received);

                                let reliability_pct = if expected_packets > 0 {
                                    (unique_received as f64 / expected_packets as f64) * 100.0
                                } else {
                                    100.0
                                };

                                // -> NEW: Calculate Global Average
                                let global_rssi_avg = if global_count > 0 {
                                    global_rssi_sum as f64 / global_count as f64
                                } else {
                                    0.0
                                };

                                // -> NEW: Calculate Global Median from the Histogram
                                let mut global_rssi_med = 0.0;
                                if global_count > 0 {
                                    let mut running_count = 0;
                                    let target = global_count / 2;
                                    for (idx, &count) in global_rssi_histogram.iter().enumerate() {
                                        running_count += count;
                                        if running_count > target {
                                            // Map the index (0..255) back to the RSSI value (-128..127)
                                            global_rssi_med = (idx as i16 - 128) as f64;
                                            break;
                                        }
                                    }
                                }

                                // 5. PRINT BENCHMARK REPORT
                                let latest_info = if let Some(latest) = window_buffer.back() {
                                    format!(
                                        "📡 RSSI: {}dBm | 🔢 Seq: {} | ⏱️ Interval: {:.1}ms",
                                        latest.reading.rssi,
                                        latest.reading.seq,
                                        intervals_ms.last().unwrap_or(&0.0)
                                    )
                                } else {
                                    "No current data".to_string()
                                };

                                let report = format!(
                                    "========== BENCHMARK TICK ==========\n\
                                    📊 [5s SLIDING] Packets: {}  Total Packets: {}\n\
                                    📡   RSSI     (Min/Med/Avg/Max): {} / {:.1} / {:.1} / {}\n\
                                    ⏱️   Interval (Min/Med/Avg/Max): {:.1}ms / {:.1}ms / {:.1}ms / {:.1}ms\n\
                                    🌍 [GLOBAL]     Reliability: {:.3}% | Total Dropped: {}\n\
                                                    RSSI (Min/Med/Avg/Max): {} / {:.1} / {:.1} / {}\n\
                                    ⚠️ [HEALTH]     Dupes: {} | Max Consecutive Drops: {}\n\
                                    ====================================\n\
                                    📥 [LATEST]     {}\n\
                                    ====================================\n",
                                    window_count, unique_received,
                                    window_rssi_min, window_rssi_med, window_rssi_avg, window_rssi_max,
                                    int_min, int_med, int_avg, int_max,
                                    reliability_pct, total_dropped,
                                    global_rssi_min, global_rssi_med, global_rssi_avg, global_rssi_max,
                                    total_dupes, max_consecutive_drops,
                                    latest_info
                                );

                                println!("{}", report);

                                let _ = ui_tx.send(report);

                                // Prevent memory leak: Prune the HashSet of old sequences (keep last 1000)
                                seen_seqs.retain(|&s| s > highest.saturating_sub(1000));
                            }

                            // ==========================================
                            // EVENT B: NEW PACKET RECEIVED
                            // ==========================================
                            Some(record) = rx.recv() => {
                                let seq = record.reading.seq;
                                let rssi = record.reading.rssi;

                                // 1. Global RSSI Tracking
                                global_count += 1;
                                global_rssi_sum += rssi as i64;
                                global_rssi_min = global_rssi_min.min(rssi);
                                global_rssi_max = global_rssi_max.max(rssi);

                                // Safely map -128..127 to index 0..255 for the zero-overhead histogram
                                let hist_idx = (rssi as i16 + 128) as usize;
                                global_rssi_histogram[hist_idx] += 1;

                                // 2. Sequence and Dupe Tracking
                                if first_seq.is_none() {
                                    first_seq = Some(seq);
                                }

                                // .insert() returns false if the item was already in the set
                                if !seen_seqs.insert(seq) {
                                    total_dupes += 1;
                                }

                                // 3. Consecutive Drop Tracking
                                if let Some(highest) = highest_seq {
                                    if seq > highest + 1 {
                                        // A gap is detected! (e.g. expected 5, got 8 -> gap of 2)
                                        let gap = (seq - highest - 1) as u64;
                                        current_consecutive_drops += gap;
                                        max_consecutive_drops = max_consecutive_drops.max(current_consecutive_drops);
                                    } else if seq == highest + 1 {
                                        // Perfect continuity, reset consecutive drop counter
                                        current_consecutive_drops = 0;
                                    }

                                    highest_seq = Some(highest.max(seq));
                                } else {
                                    highest_seq = Some(seq);
                                }

                                // 4. Add to the back of the Sliding Window
                                window_buffer.push_back(record);
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
                    let reading: SensorReading = observation.into();

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
