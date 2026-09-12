use clap::Parser as _;

use crate::cli::{Cli, Commands};

mod api_client;
mod chip;
mod cli;
mod commands;
mod store;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Login { profile, api_url } => commands::login(profile, api_url)?,
        Commands::Logout { profile } => commands::logout(profile)?,
        Commands::Whoami { api } => commands::whoami(&api)?,
        Commands::Info => commands::info()?,
        Commands::Provision { name, api, unlock } => commands::provision(&api, unlock, name)?,
        Commands::Rotate { api, unlock } => commands::rotate_key(&api, unlock)?,
    };

    Ok(())
}
