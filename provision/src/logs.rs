//! Level 0 verification: what the board says about itself at boot.
//!
//! The cheapest rung of the ladder in `docs/design/provisioning.md`, and the
//! only one that works with no network, no receiver and no key: reset the
//! board, read its defmt log over RTT, and show what came out. The firmware
//! already reports the things a provisioning run can get wrong — whether it
//! found a key in UICR, where its seq counter resumed — so this needs nothing
//! from the firmware side.
//!
//! The window is short on purpose. Everything that answers "did this run work"
//! lands within milliseconds of reset; the 60 s sensor cycle is what `verify`
//! is for.
//!
//! ⚠️ Decoding couples this to the firmware's defmt version. `defmt-decoder`
//! must track the `defmt` in `firmware/sensor/Cargo.toml` across a major bump,
//! or frames stop decoding and this reports silence.

use std::{
    io::Write,
    time::{Duration, Instant},
};

use defmt_decoder::{DecodeError, Table};
use probe_rs::{Core, rtt::Rtt};
use thiserror::Error;

/// How often to drain the target's RTT buffer. The buffer is small and the
/// board writes its whole boot in one burst, so this only has to be faster
/// than the buffer fills.
const POLL: Duration = Duration::from_millis(20);

/// Finding the control block means scanning the target's RAM over SWD, which
/// takes seconds on its own. ⚠️ It gets its own budget so that `--logs 3` means
/// three seconds of *reading*: counting the scan against the window is how a
/// short window ends up reading nothing and calling the board silent.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// How many log frames decoded.
    pub frames: usize,

    /// Channels the board published, and raw bytes read from them. Reported
    /// when nothing decodes, because "no channel", "channel but no bytes" and
    /// "bytes that did not decode" have three different causes.
    pub channels: usize,
    pub bytes: usize,

    /// Lines the firmware logged at ERROR. ⚠️ These are what L0 exists to
    /// catch: `device is empty - not provisioned` and its siblings are the
    /// firmware saying the key did not land.
    pub errors: Vec<String>,
}

impl Summary {
    /// ⚠️ Silence is ambiguous and must not read as success. Either the board
    /// never booted, or the image was built without `DEFMT_LOG` — which
    /// compiles every statement out and is indistinguishable from a hung board
    /// from here. (The second is not hypothetical: building firmware with
    /// `--manifest-path` from the repo root skips `firmware/.cargo/config.toml`
    /// and does exactly that.)
    pub fn is_silent(&self) -> bool {
        self.frames == 0
    }
}

/// Streams the board's log for `window`, writing each line to `out`.
///
/// Takes the ELF bytes rather than a path because the caller already holds the
/// artifact it just flashed — and decoding against a *different* build than the
/// one running produces plausible nonsense rather than an error.
pub fn stream(
    core: &mut Core<'_>,
    elf: &[u8],
    window: Duration,
    out: &mut dyn Write,
) -> Result<Summary, LogError> {
    let Some(table) = Table::parse(elf)? else {
        return Err(LogError::NoDefmtData);
    };

    let attach_by = Instant::now() + ATTACH_TIMEOUT;

    // ⚠️ Called straight after a reset, so the control block may not exist yet —
    // and worse, it may exist *empty*: probe-rs finds the block by its magic,
    // which the firmware writes before it publishes any channel. Accepting that
    // first attach reads zero bytes forever and reports the board as silent.
    // So retry until a channel actually appears.
    let mut rtt = loop {
        match Rtt::attach(core) {
            Ok(rtt) if !rtt.up_channels.is_empty() => break rtt,
            Ok(_) if Instant::now() >= attach_by => return Err(LogError::NoChannels),
            Err(err) if Instant::now() >= attach_by => return Err(err.into()),
            _ => std::thread::sleep(POLL),
        }
    };

    let mut decoder = table.new_stream_decoder();
    let mut summary = Summary {
        channels: rtt.up_channels.len(),
        ..Summary::default()
    };
    let mut buf = [0u8; 1024];

    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        let mut read_anything = false;

        for channel in rtt.up_channels.iter_mut() {
            let count = channel.read(core, &mut buf)?;
            if count > 0 {
                summary.bytes += count;
                decoder.received(&buf[..count]);
                read_anything = true;
            }
        }

        loop {
            match decoder.decode() {
                Ok(frame) => {
                    summary.frames += 1;

                    let message = frame.display_message().to_string();

                    // Compared as a string because `defmt_decoder` does not
                    // re-export the level enum, and naming it would mean taking
                    // a dependency on `defmt-parser` for one variant.
                    let level = frame.level().map_or("print", |level| level.as_str());

                    if level == "error" {
                        summary.errors.push(message.clone());
                    }

                    let _ = writeln!(out, "  [{:<5}] {message}", level.to_uppercase());
                }

                // A partial frame: wait for the rest rather than dropping it.
                Err(DecodeError::UnexpectedEof) => break,

                // ⚠️ Malformed, which usually means this ELF is not the image
                // that is running. Reported rather than retried: silently
                // resyncing would decode the *next* frames against the wrong
                // table and print plausible nonsense.
                Err(DecodeError::Malformed) => return Err(LogError::Malformed),
            }
        }

        if !read_anything {
            std::thread::sleep(POLL);
        }
    }

    Ok(summary)
}

#[derive(Debug, Error)]
pub enum LogError {
    #[error("could not read the image's defmt data: {0}")]
    Table(#[from] anyhow::Error),

    #[error(
        "this image carries no defmt data\n\n  \
         Firmware built with `--manifest-path` from the repository root misses\n  \
         `firmware/.cargo/config.toml`, and with it DEFMT_LOG — which compiles every\n  \
         log statement out. Rebuild with `just firmware-store <crate> <board>`."
    )]
    NoDefmtData,

    #[error("the log did not decode against this image; it is probably running a different build")]
    Malformed,

    #[error("the board published an RTT control block but no channel")]
    NoChannels,

    #[error(transparent)]
    Rtt(#[from] probe_rs::rtt::Error),

    #[error(transparent)]
    Probe(#[from] probe_rs::Error),
}

#[cfg(test)]
mod test {
    use super::*;

    /// ⚠️ The distinction the whole module turns on: nothing decoded is not the
    /// same as nothing wrong, and must never be reported as a pass.
    #[test]
    fn silence_is_not_success() {
        assert!(Summary::default().is_silent());

        let spoke = Summary {
            frames: 3,
            ..Summary::default()
        };
        assert!(!spoke.is_silent(), "frames decoded, so the board spoke");
    }

    #[test]
    fn errors_are_collected_separately_from_the_frame_count() {
        let summary = Summary {
            frames: 9,
            errors: vec!["device is empty - not provisioned".to_owned()],
            ..Summary::default()
        };

        assert!(!summary.is_silent());
        assert_eq!(summary.errors.len(), 1);
    }
}
