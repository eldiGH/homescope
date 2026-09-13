//! Level-2 verification: has the fleet heard from a device under its current key?
//!
//! The strongest of the three levels in `NOTES-provisioning.md` §0, and the
//! cheapest to build. A reading only reaches `lastSeen` after crossing the
//! radio, the receiver, the gateway and the broker, and after the API has
//! decrypted it with the key `provision` or `rotate` installed — so a new
//! `lastSeen` proves the whole chain at once, with no key, no probe and no
//! firmware change needed here.
//!
//! **No clocks are compared.** "Heard from now" is judged against the API's own
//! `lastSeen` at the moment waiting began — any newer value, or any value at all
//! where there was none — never against this workstation's time, which need not
//! agree with the gateway's.

use std::{
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use homescope_api_types::devices::{DeviceKeyStatus, DeviceSummary};
use homescope_common::device_addr::DeviceAddr;
use thiserror::Error;

use crate::api_client::{ApiClient, ApiClientError};

/// How often to ask. Sensors report on a cadence of tens of seconds, so polling
/// much faster than this only adds load to the Pi.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    /// A reading newer than the baseline has arrived, stamped with this time.
    Reported(DateTime<Utc>),

    /// Nothing new yet.
    Waiting,

    /// The API cannot open this device's key, so it cannot decrypt a packet and
    /// `lastSeen` can never move. Waiting would only run out the clock.
    Undecryptable(DeviceKeyStatus),
}

/// Judges one summary against the `lastSeen` observed when waiting began.
pub fn assess(baseline: Option<DateTime<Utc>>, current: &DeviceSummary) -> Progress {
    match current.key_status {
        // `Unknown` is a status this build cannot interpret, so it does not
        // guess either way and lets `lastSeen` decide: a reading that arrives
        // is proof whatever the status is called.
        DeviceKeyStatus::Ok | DeviceKeyStatus::Unknown => {}
        status => return Progress::Undecryptable(status),
    }

    match (baseline, current.last_seen) {
        (_, None) => Progress::Waiting,
        (None, Some(seen)) => Progress::Reported(seen),
        (Some(before), Some(seen)) if seen > before => Progress::Reported(seen),
        (Some(_), Some(_)) => Progress::Waiting,
    }
}

/// The fix for a key the API cannot open — the remedies documented on
/// `DeviceKeyStatus`, worded for a terminal.
pub fn remedy(status: DeviceKeyStatus) -> &'static str {
    match status {
        DeviceKeyStatus::Missing => "it has no key on file — provision or rotate it",
        DeviceKeyStatus::Invalid => "its stored key cannot be parsed — rotate it",
        DeviceKeyStatus::KekUnavailable => {
            "its key is sealed under a KEK generation the API has not loaded — load that \
             generation rather than rotating, which would force a re-flash of a device whose \
             key was never wrong"
        }
        DeviceKeyStatus::Unopenable => "its key does not open for this device — rotate it",
        DeviceKeyStatus::Ok | DeviceKeyStatus::Unknown => "the API reports no key problem",
    }
}

#[derive(Debug, Error)]
pub enum WaitError {
    #[error("no new reading within {0}s")]
    TimedOut(u64),

    #[error("the API cannot decrypt it: {}", remedy(*.0))]
    Undecryptable(DeviceKeyStatus),

    #[error("it was removed from the registry while waiting")]
    Unregistered,

    #[error(transparent)]
    Api(#[from] ApiClientError),
}

/// Polls until a reading newer than `baseline` arrives, the key turns out to be
/// unusable, or `timeout` passes.
///
/// Sleeps *before* each request, because the caller has just fetched the
/// summary the baseline came from.
pub fn wait_for_reading(
    client: &ApiClient,
    device_addr: DeviceAddr,
    baseline: Option<DateTime<Utc>>,
    timeout: Duration,
) -> Result<DateTime<Utc>, WaitError> {
    let deadline = Instant::now() + timeout;

    loop {
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));

        let summary = client.device(device_addr)?.ok_or(WaitError::Unregistered)?;

        match assess(baseline, &summary) {
            Progress::Reported(at) => return Ok(at),
            Progress::Undecryptable(status) => return Err(WaitError::Undecryptable(status)),
            Progress::Waiting if Instant::now() >= deadline => {
                return Err(WaitError::TimedOut(timeout.as_secs()));
            }
            Progress::Waiting => {}
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn at(offset_secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_753_000_000 + offset_secs, 0).expect("valid timestamp")
    }

    fn summary(key_status: DeviceKeyStatus, last_seen: Option<DateTime<Utc>>) -> DeviceSummary {
        DeviceSummary {
            device_addr: DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
            name: "kitchen".to_owned(),
            key_status,
            key_valid_from: at(0),
            last_seen,
        }
    }

    /// A freshly provisioned board has nothing under its key yet, so the first
    /// reading at all is the proof.
    #[test]
    fn the_first_reading_under_a_new_key_is_proof() {
        assert_eq!(
            assess(None, &summary(DeviceKeyStatus::Ok, None)),
            Progress::Waiting
        );
        assert_eq!(
            assess(None, &summary(DeviceKeyStatus::Ok, Some(at(60)))),
            Progress::Reported(at(60))
        );
    }

    /// ⚠️ A board that reported yesterday proves nothing about today. Once a
    /// baseline exists only a *newer* reading counts — which is what lets
    /// `verify` answer "is it producing readings" on a board already deployed.
    #[test]
    fn an_old_reading_is_not_proof_of_a_new_one() {
        assert_eq!(
            assess(Some(at(60)), &summary(DeviceKeyStatus::Ok, Some(at(60)))),
            Progress::Waiting
        );
        assert_eq!(
            assess(Some(at(60)), &summary(DeviceKeyStatus::Ok, Some(at(120)))),
            Progress::Reported(at(120))
        );
    }

    /// A rotation mid-wait starts a new key epoch, and `lastSeen` goes back to
    /// null for it. That is still waiting, not a failure.
    #[test]
    fn a_key_rotated_mid_wait_goes_back_to_waiting() {
        assert_eq!(
            assess(Some(at(60)), &summary(DeviceKeyStatus::Ok, None)),
            Progress::Waiting
        );
    }

    /// A key the API cannot open fails immediately — no reading can arrive, so
    /// running out the timeout would only delay the real answer.
    #[test]
    fn an_unusable_key_fails_fast() {
        for status in [
            DeviceKeyStatus::Missing,
            DeviceKeyStatus::Invalid,
            DeviceKeyStatus::KekUnavailable,
            DeviceKeyStatus::Unopenable,
        ] {
            assert_eq!(
                assess(None, &summary(status, None)),
                Progress::Undecryptable(status)
            );
        }
    }

    /// A status this build does not know is not treated as a failure; the
    /// readings decide.
    #[test]
    fn an_unrecognised_status_defers_to_last_seen() {
        assert_eq!(
            assess(None, &summary(DeviceKeyStatus::Unknown, Some(at(60)))),
            Progress::Reported(at(60))
        );
        assert_eq!(
            assess(None, &summary(DeviceKeyStatus::Unknown, None)),
            Progress::Waiting
        );
    }
}
