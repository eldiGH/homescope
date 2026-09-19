use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use homescope_common::device_addr::DeviceAddr;

#[derive(Parser)]
#[command(about = "Give a board an identity in the Homescope fleet")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// How a command decides which API to talk to.
///
/// Flattened into the commands that need the network rather than made global —
/// `info` needs neither, and should not accept flags it ignores.
#[derive(Args)]
pub struct ApiArgs {
    /// Saved profile to use (see `login`)
    #[arg(short, long, env = "HOMESCOPE_PROFILE", conflicts_with = "api_url")]
    pub profile: Option<String>,

    /// Talk to this URL directly; the token must come from HOMESCOPE_TOKEN
    #[arg(long, env = "HOMESCOPE_API_URL")]
    pub api_url: Option<String>,
}

/// Consent handling for the commands that destroy something.
///
/// Flattened rather than made a global flag: `info` and `login` destroy
/// nothing and should not offer to skip a prompt they never ask.
#[derive(Args)]
pub struct ConfirmArgs {
    /// Answer every confirmation with yes
    ///
    /// ⚠️ Required when stdin is not a terminal — the prompts fail closed
    /// rather than assuming consent.
    #[arg(long)]
    pub yes: bool,
}

/// How long to read the board's own log after resetting it.
///
/// Level 0 of the verification ladder: the firmware reports whether it found a
/// key in UICR and where its seq counter resumed, which is the whole question a
/// provisioning run asks — and it needs no network, no receiver and no key.
#[derive(Args)]
pub struct LogArgs {
    /// Seconds to read the board's log after reset; 0 to skip
    #[arg(long, value_name = "SECONDS", default_value_t = 3)]
    pub logs: u64,
}

/// Which probe to talk through.
///
/// Flattened into the commands that touch a board. One probe needs no flag; the
/// ambiguity error prints the serials to choose from.
#[derive(Args)]
pub struct ProbeArgs {
    /// Serial number of the debug probe to use
    #[arg(long, value_name = "SERIAL", env = "HOMESCOPE_PROBE")]
    pub probe: Option<String>,
}

/// Which stored image to put on the board.
///
/// A **name** from the artifact store, not a path: "whatever ELF was in the
/// directory you ran from" is not an acceptable input to the one command that
/// writes flash. Omitting it where firmware is required opens the picker on a
/// terminal — see `firmware::pick`.
#[derive(Args)]
pub struct FirmwareArgs {
    /// Stored artifact to flash (see `firmware list`)
    #[arg(long, value_name = "NAME")]
    pub firmware: Option<String>,
}

#[derive(Subcommand)]
pub enum FirmwareCommand {
    /// Store a built ELF under a name
    ///
    /// Reads where the image loads and where it keeps its seq counter out of
    /// the ELF itself, so neither can be a hand-copied claim.
    Add {
        /// Path to the built ELF
        path: PathBuf,

        /// What to call it — what you will type at `--firmware`
        #[arg(long)]
        name: String,
    },

    /// List stored artifacts
    List,

    /// Forget a stored artifact
    Remove {
        /// Name to forget
        name: String,
    },
}

#[derive(Subcommand)]
pub enum Commands {
    /// Save an API token for a profile
    ///
    /// ⚠️ `--api-url` here means "the URL this profile points at", not the
    /// `ApiArgs` escape hatch — `login` creates a profile rather than
    /// bypassing one, so the two are deliberately not the same flag.
    Login {
        /// Profile to save under (default: the configured default, else "default")
        #[arg(short, long)]
        profile: Option<String>,

        /// API URL for this profile; required the first time it is used
        #[arg(long)]
        api_url: Option<String>,
    },

    /// Forget a profile's saved token, keeping the profile itself
    Logout {
        #[arg(short, long)]
        profile: Option<String>,
    },

    /// Show which API is configured and whether its token is accepted
    Whoami {
        #[command(flatten)]
        api: ApiArgs,
    },

    /// List every registered device with its key status and last reading
    List {
        #[command(flatten)]
        api: ApiArgs,
    },

    /// Report what is on the attached probe right now
    Info {
        #[command(flatten)]
        probe: ProbeArgs,
    },

    /// Manage the firmware artifact store
    Firmware {
        #[command(subcommand)]
        command: FirmwareCommand,
    },

    /// Put firmware on a board without touching its key
    ///
    /// An ordinary flash does not touch UICR, so the device keeps its key and
    /// needs no re-provisioning. Deliberately does **not** clear the seq
    /// counter: no new key is minted here, and clearing it under a live key
    /// would reuse nonces.
    Flash {
        #[command(flatten)]
        firmware: FirmwareArgs,

        #[command(flatten)]
        probe: ProbeArgs,

        #[command(flatten)]
        logs: LogArgs,
    },

    /// Register a blank board with the fleet and write its key
    Provision {
        name: String,

        #[command(flatten)]
        api: ApiArgs,

        #[command(flatten)]
        firmware: FirmwareArgs,

        #[command(flatten)]
        probe: ProbeArgs,

        #[command(flatten)]
        logs: LogArgs,

        #[command(flatten)]
        confirm: ConfirmArgs,

        /// Recover an APPROTECT-locked chip by erasing it first
        #[arg(long)]
        unlock: bool,
    },

    /// Mint a new key for a board already in the fleet
    Rotate {
        #[command(flatten)]
        api: ApiArgs,

        #[command(flatten)]
        firmware: FirmwareArgs,

        #[command(flatten)]
        probe: ProbeArgs,

        #[command(flatten)]
        logs: LogArgs,

        #[command(flatten)]
        confirm: ConfirmArgs,

        #[arg(long)]
        unlock: bool,
    },

    /// Wait until the API hears from a device under its current key
    ///
    /// Proves the whole chain at once — radio, receiver, gateway, broker and the
    /// API's decrypt. Only a reading newer than the one on file when waiting
    /// starts counts, so on a deployed board this answers "is it reporting now",
    /// not "did it ever". Never halts or resets the board.
    Verify {
        /// Device to wait for (default: the board on the attached probe)
        address: Option<DeviceAddr>,

        #[command(flatten)]
        api: ApiArgs,

        #[command(flatten)]
        probe: ProbeArgs,

        // TODO: 180 s assumes today's 60 s cadence; raise it when production moves
        // to 1–5 min between bursts.
        /// Give up after this long without a new reading
        #[arg(long, value_name = "SECONDS", default_value_t = 180)]
        timeout: u64,
    },
}
