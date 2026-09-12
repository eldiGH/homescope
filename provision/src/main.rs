use clap::Parser as _;

use crate::cli::{Cli, Commands};

mod api_client;
mod chip;
mod cli;
mod commands;
mod confirm;
mod output;
mod store;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Login { profile, api_url } => commands::login(profile, api_url)?,
        Commands::Logout { profile } => commands::logout(profile)?,
        Commands::Whoami { api } => commands::whoami(&api)?,
        Commands::Info => commands::info()?,
        Commands::Provision {
            name,
            api,
            confirm,
            unlock,
        } => commands::provision(&api, &confirm, unlock, name)?,
        Commands::Rotate {
            api,
            confirm,
            unlock,
        } => commands::rotate_key(&api, &confirm, unlock)?,
    };

    Ok(())
}
