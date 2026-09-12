//! What the tool prints, and to which stream.
//!
//! **stderr** carries diagnostics, step lines and prompts. **stdout** carries
//! the one durable fact a command produces — the address, or `ADDR\tNAME`. So
//! `homescope-provision provision kitchen >> labels.tsv` does the obvious
//! thing and the prompts still reach you.
//!
//! ⚠️ Never printed anywhere, on success or failure: the device key, the admin
//! token. `DeviceKeyResponse` deliberately does not derive `Debug`, which makes
//! `{response:?}` a compile error rather than a fleet key in a log line.

use std::io::{Write as _, stderr};

use homescope_common::uicr_record::RecordHeader;

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
