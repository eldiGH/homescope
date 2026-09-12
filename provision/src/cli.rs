use clap::{Args, Parser, Subcommand};

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

    /// Report what is on the attached probe right now
    Info,

    /// Register a blank board with the fleet and write its key
    Provision {
        name: String,

        #[command(flatten)]
        api: ApiArgs,

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
        confirm: ConfirmArgs,

        #[arg(long)]
        unlock: bool,
    },
}
