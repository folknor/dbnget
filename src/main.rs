mod batch;
mod cli;
mod get;
mod meta;
mod query;
mod session;
mod spend;
mod verify;

use anyhow::{Context, Result};
use clap::Parser;
use databento::HistoricalClient;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    init_tracing(args.verbose);

    let mut client = build_client(args.key.as_deref())?;
    match &args.command {
        Command::Get(get_args) => get::run(&mut client, get_args).await,
        Command::Cost(query_args) => meta::cost(&mut client, query_args).await,
        Command::Batch(command) => batch::run(&mut client, command).await,
        Command::Meta(command) => meta::run(&mut client, command).await,
    }
}

/// `-v` raises this crate to debug, `-vv` turns on debug logging everywhere.
/// `RUST_LOG`, when set, wins over both.
fn init_tracing(verbosity: u8) {
    let default = match verbosity {
        0 => "dbnget=info",
        1 => "dbnget=debug",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn build_client(key: Option<&str>) -> Result<HistoricalClient> {
    let builder = HistoricalClient::builder();
    let builder = match key {
        Some(key) => builder.key(key),
        None => builder.key_from_env(),
    }
    .context("no API key: pass --key or set DATABENTO_API_KEY")?;
    builder.build().context("building the Databento client")
}
