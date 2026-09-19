use std::time::Duration;

use clap::Parser as _;

use crate::cli::{Cli, Commands};

mod api_client;
mod chip;
mod cli;
mod commands;
mod confirm;
mod elf;
mod firmware;
mod output;
mod store;
mod verify;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Login { profile, api_url } => commands::login(profile, api_url)?,
        Commands::Logout { profile } => commands::logout(profile)?,
        Commands::Whoami { api } => commands::whoami(&api)?,
        Commands::List { api } => commands::list(&api)?,
        Commands::Info => commands::info()?,
        Commands::Firmware { command } => commands::firmware(command)?,
        Commands::Flash { firmware } => commands::flash(&firmware)?,
        Commands::Provision {
            name,
            api,
            firmware,
            confirm,
            unlock,
        } => commands::provision(&api, &firmware, &confirm, unlock, name)?,
        Commands::Rotate {
            api,
            firmware,
            confirm,
            unlock,
        } => commands::rotate_key(&api, &firmware, &confirm, unlock)?,
        Commands::Verify {
            address,
            api,
            timeout,
        } => commands::verify(&api, address, Duration::from_secs(timeout))?,
    };

    Ok(())
}
