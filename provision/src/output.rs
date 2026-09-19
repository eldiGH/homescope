//! What the tool prints, and to which stream.
//!
//! **stderr** carries diagnostics, step lines and prompts. **stdout** carries
//! what a command produces — the address, `ADDR\tNAME`, or for `list` the
//! listing itself. So `homescope-provision provision kitchen >> labels.tsv`
//! does the obvious thing and the prompts still reach you.
//!
//! ⚠️ Never printed anywhere, on success or failure: the device key, the admin
//! token. `DeviceKeyResponse` deliberately does not derive `Debug`, which makes
//! `{response:?}` a compile error rather than a fleet key in a log line.

use std::io::{Write as _, stderr};

use chrono::{DateTime, SecondsFormat, Utc};
use homescope_api_types::devices::{DeviceKeyStatus, DeviceSummary};
use homescope_common::{device_addr::DeviceAddr, uicr_record::RecordHeader};

use crate::chip::ChipState;

/// The identity block — the whole answer to "is this the right board".
///
/// Printed by every command that touches a probe, destructive ones included,
/// and immediately above any confirmation, because the address on screen is
/// what actually catches a wrong board.
pub fn identity(probe: &str, target: &str, state: &ChipState) {
    let _ = writeln!(
        stderr(),
        "Probe    {probe}\nTarget   {target}\nAddress  {}\nRecord   {}",
        state.device_addr,
        record_summary(&state.record),
    );
}

/// A locked chip reports its state rather than failing: APPROTECT means the
/// address is unreadable, which is an answer, not an error.
pub fn identity_locked(probe: &str, target: &str) {
    let _ = writeln!(
        stderr(),
        "Probe    {probe}\nTarget   {target}\nStatus   locked (APPROTECT) — address unreadable",
    );
}

/// The identity block's address line on its own, for a command given an
/// address rather than a probe.
pub fn address(device_addr: DeviceAddr) {
    let _ = writeln!(stderr(), "Address  {device_addr}");
}

/// The registry half of the identity block: which API was asked, and what it
/// knows about this address. Printed beneath the chip half and above any
/// confirmation, so a prompt can be answered from a name rather than from a hex
/// string you have to recognise.
pub fn fleet(api_label: &str, summary: Option<&DeviceSummary>, now: DateTime<Utc>) {
    let _ = writeln!(
        stderr(),
        "API      {api_label}\nFleet    {}",
        fleet_summary(summary, now),
    );
}

fn fleet_summary(summary: Option<&DeviceSummary>, now: DateTime<Utc>) -> String {
    let Some(device) = summary else {
        return "not registered".to_owned();
    };

    let seen = match device.last_seen {
        Some(seen) => format!("last seen {}", seen_at(seen, now)),
        None => "never reported under this key".to_owned(),
    };

    format!(
        "{:?} · key {} · {seen}",
        device.name,
        key_status_label(device.key_status)
    )
}

fn record_summary(record: &RecordHeader) -> String {
    match record {
        RecordHeader::Blank => "blank".to_owned(),
        RecordHeader::Present => "provisioned".to_owned(),
        RecordHeader::Malformed(err) => format!("malformed — {err}"),
    }
}

/// Runs one fallible step and resolves it on screen: `Writing UICR record … ok`.
///
/// Wrapping the call rather than printing around it means a step can never be
/// reported `ok` without having run, and never left dangling on failure.
pub fn step<T, E>(label: &str, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let _ = write!(stderr(), "{label} … ");
    let _ = stderr().flush();

    match f() {
        Ok(value) => {
            let _ = writeln!(stderr(), "ok");
            Ok(value)
        }
        Err(err) => {
            let _ = writeln!(stderr(), "failed");
            Err(err)
        }
    }
}

/// The line you copy onto the enclosure.
pub fn outcome(summary: &str) {
    let _ = writeln!(stderr(), "\n{summary}");
}

/// A timestamp for a person: `2025-07-20 08:26 UTC`.
pub fn timestamp(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn seen_at(seen: DateTime<Utc>, now: DateTime<Utc>) -> String {
    format!("{} ({})", timestamp(seen), age(seen, now))
}

/// A rough age for a person, measured against this workstation's clock.
///
/// That clock need not match the gateway's, so a reading that looks slightly
/// in the future reads as "just now" rather than as a negative age. Display
/// only — nothing is ever decided on it; `verify` compares server timestamps
/// with each other instead.
pub fn age(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds();

    match secs {
        ..60 => "just now".to_owned(),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn key_status_label(status: DeviceKeyStatus) -> &'static str {
    match status {
        DeviceKeyStatus::Ok => "ok",
        DeviceKeyStatus::Missing => "missing",
        DeviceKeyStatus::Invalid => "invalid",
        DeviceKeyStatus::KekUnavailable => "kek unavailable",
        DeviceKeyStatus::Unopenable => "unopenable",
        DeviceKeyStatus::Unknown => "unknown",
    }
}

/// The key status as it travels on the wire — `KEK_UNAVAILABLE`, not
/// `kek unavailable` — so machine-readable output is stable and has no spaces.
fn wire_status(status: DeviceKeyStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .expect("DeviceKeyStatus serializes as a string")
}

/// Keeps a user-typed name on one line and inside its column: a tab or newline
/// in a name would otherwise split a row.
fn one_line(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The fleet listing: aligned columns under a header for a person, or bare
/// tab-separated rows for a pipe — so `list | cut -f1` yields addresses, and a
/// script never has to strip a header or guess at spacing.
///
/// Sorted by name, then address, because the API returns rows in whatever order
/// the table happens to hold them.
pub fn fleet_listing(devices: &[DeviceSummary], now: DateTime<Utc>, for_terminal: bool) -> String {
    let mut rows: Vec<&DeviceSummary> = devices.iter().collect();
    rows.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.device_addr.as_i64().cmp(&b.device_addr.as_i64()))
    });

    if for_terminal {
        table(&rows, now)
    } else {
        tab_separated(&rows)
    }
}

fn table(rows: &[&DeviceSummary], now: DateTime<Utc>) -> String {
    const HEADER: [&str; 4] = ["ADDRESS", "NAME", "KEY", "LAST SEEN"];

    let cells: Vec<[String; 4]> = rows
        .iter()
        .map(|device| {
            [
                device.device_addr.to_string(),
                one_line(&device.name),
                key_status_label(device.key_status).to_owned(),
                device
                    .last_seen
                    .map_or_else(|| "never".to_owned(), |seen| seen_at(seen, now)),
            ]
        })
        .collect();

    let mut widths = HEADER.map(|title| title.chars().count());
    for row in &cells {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    std::iter::once(HEADER.map(str::to_owned))
        .chain(cells)
        .map(|row| {
            let line = row
                .iter()
                .zip(widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect::<Vec<_>>()
                .join("  ");

            format!("{}\n", line.trim_end())
        })
        .collect()
}

fn tab_separated(rows: &[&DeviceSummary]) -> String {
    rows.iter()
        .map(|device| {
            format!(
                "{}\t{}\t{}\t{}\n",
                device.device_addr,
                one_line(&device.name),
                wire_status(device.key_status),
                device
                    .last_seen
                    .map(|seen| seen.to_rfc3339_opts(SecondsFormat::Secs, true))
                    .unwrap_or_default(),
            )
        })
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;

    /// 2025-07-20T08:26:40Z.
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_753_000_000, 0).expect("valid timestamp")
    }

    fn device(
        last_byte: u8,
        name: &str,
        key_status: DeviceKeyStatus,
        last_seen: Option<DateTime<Utc>>,
    ) -> DeviceSummary {
        DeviceSummary {
            device_addr: DeviceAddr([last_byte, 0x02, 0x03, 0x04, 0x05, 0xC6]),
            name: name.to_owned(),
            key_status,
            key_valid_from: now(),
            last_seen,
        }
    }

    fn three_minutes_age() -> Option<DateTime<Utc>> {
        Some(now() - chrono::Duration::seconds(180))
    }

    #[test]
    fn ages_round_down_into_the_largest_whole_unit() {
        for (secs, expected) in [
            (0, "just now"),
            (59, "just now"),
            (60, "1m ago"),
            (3_599, "59m ago"),
            (3_600, "1h ago"),
            (86_399, "23h ago"),
            (86_400, "1d ago"),
        ] {
            assert_eq!(
                age(now() - chrono::Duration::seconds(secs), now()),
                expected
            );
        }
    }

    /// The gateway's clock running ahead of this workstation's is not a
    /// negative age.
    #[test]
    fn a_reading_from_the_future_is_just_now() {
        assert_eq!(
            age(now() + chrono::Duration::seconds(90), now()),
            "just now"
        );
    }

    #[test]
    fn the_fleet_line_names_the_device() {
        assert_eq!(
            fleet_summary(
                Some(&device(
                    1,
                    "kitchen",
                    DeviceKeyStatus::Ok,
                    three_minutes_age()
                )),
                now()
            ),
            "\"kitchen\" · key ok · last seen 2025-07-20 08:23 UTC (3m ago)"
        );
        assert_eq!(
            fleet_summary(
                Some(&device(1, "kitchen", DeviceKeyStatus::Missing, None)),
                now()
            ),
            "\"kitchen\" · key missing · never reported under this key"
        );
        assert_eq!(fleet_summary(None, now()), "not registered");
    }

    /// Every cell of a column starts where its header does, whatever the width
    /// of the longest value — and rows come out sorted by name.
    #[test]
    fn the_terminal_table_is_aligned_and_sorted() {
        let devices = [
            device(1, "kitchen", DeviceKeyStatus::Ok, three_minutes_age()),
            device(2, "garage", DeviceKeyStatus::KekUnavailable, None),
        ];

        let listing = fleet_listing(&devices, now(), true);
        let lines: Vec<&str> = listing.lines().collect();

        assert_eq!(lines.len(), 3, "{listing}");

        let column = |line: &str, needle: &str| line.find(needle).expect(needle);
        let header = lines[0];

        assert!(header.starts_with("ADDRESS"), "{listing}");
        assert!(
            lines[1].contains("garage") && lines[2].contains("kitchen"),
            "{listing}"
        );

        for (row, name, key, seen) in [
            (lines[1], "garage", "kek unavailable", "never"),
            (lines[2], "kitchen", "ok", "2025-07-20 08:23 UTC (3m ago)"),
        ] {
            assert_eq!(column(row, name), column(header, "NAME"), "{listing}");
            assert_eq!(column(row, key), column(header, "KEY"), "{listing}");
            assert_eq!(column(row, seen), column(header, "LAST SEEN"), "{listing}");
        }
    }

    /// Machine output: no header, wire status names, RFC 3339 timestamps, an
    /// empty field for never, and a name that cannot break its row.
    #[test]
    fn piped_output_is_bare_tab_separated_rows() {
        let devices = [
            device(1, "kitchen", DeviceKeyStatus::Ok, three_minutes_age()),
            device(2, "gar\tage", DeviceKeyStatus::KekUnavailable, None),
        ];

        assert_eq!(
            fleet_listing(&devices, now(), false),
            "C60504030202\tgar age\tKEK_UNAVAILABLE\t\n\
             C60504030201\tkitchen\tOK\t2025-07-20T08:23:40Z\n"
        );
    }
}
