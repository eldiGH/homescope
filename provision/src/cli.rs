use clap::{Parser, Subcommand};

#[derive(Parser)]
pub struct Cli {
    #[arg(long, default_value_t = "http://127.0.0.1:7890".to_owned())]
    pub url: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    Info,
    Provision {
        name: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        unlock: bool,
    },
    Rotate {
        #[arg(long)]
        token: String,
        #[arg(long)]
        unlock: bool
    },
}
