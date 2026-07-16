use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use homescope_common::device_addr::DeviceAddr;

/// Distinct unknown ids we keep individual state for. Anything beyond
/// this folds into one overflow counter, so a flood of random ids
/// can't grow memory without bound.
const MAX_TRACKED: usize = 256;

/// How often a still-transmitting unknown id is re-reported.
const REPORT_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// What the caller should tell a human, if anything.
pub enum Report {
    /// First packet from this id.
    New,
    /// Still transmitting; emitted at most once per REPORT_INTERVAL.
    Repeat { count: u64, since: Duration },
    /// Aggregate for ids beyond MAX_TRACKED.
    Overflow { packets: u64 },
}

struct Entry {
    count: u64,
    first_seen: Instant,
    last_reported: Instant,
}

#[derive(Default)]
pub struct UnknownDevices {
    tracked: HashMap<DeviceAddr, Entry>,
    overflow_packets: u64,
    overflow_reported: Option<Instant>,
}

impl UnknownDevices {
    /// Note a dropped packet from an unregistered device.
    /// Returns Some when this occurrence deserves a log line.
    pub fn record(&mut self, device_addr: DeviceAddr) -> Option<Report> {
        let now = Instant::now();

        if let Some(entry) = self.tracked.get_mut(&device_addr) {
            entry.count += 1;

            if now.duration_since(entry.last_reported) < REPORT_INTERVAL {
                return None;
            }

            entry.last_reported = now;
            return Some(Report::Repeat {
                count: entry.count,
                since: now.duration_since(entry.first_seen),
            });
        }

        if self.tracked.len() < MAX_TRACKED {
            let entry = Entry {
                count: 1,
                first_seen: now,
                last_reported: now,
            };
            self.tracked.insert(device_addr, entry);
            return Some(Report::New);
        }

        self.overflow_packets += 1;
        match self.overflow_reported {
            Some(at) if now.duration_since(at) < REPORT_INTERVAL => None,
            _ => {
                self.overflow_reported = Some(now);
                Some(Report::Overflow {
                    packets: self.overflow_packets,
                })
            }
        }
    }
}
