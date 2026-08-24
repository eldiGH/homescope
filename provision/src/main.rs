use clap::Parser as _;

use crate::{
    api_client::ApiClient,
    cli::{Cli, Commands},
};

mod api_client;
mod chip;
mod cli;
mod commands;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info => commands::info()?,
        Commands::Provision { name, token, unlock } => {
            let api_client = ApiClient::new(token, cli.url);
            commands::provision(&api_client, unlock, name)?
        }
        Commands::Rotate { token, unlock } => {
            let api_client = ApiClient::new(token, cli.url);
            commands::rotate_key(&api_client, unlock)?
        }
    };

    Ok(())
}
