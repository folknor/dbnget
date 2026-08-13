mod batch;
mod cli;
mod disk;
mod get;
mod meta;
mod query;
mod session;
mod spend;
mod verify;

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Parser;
use databento::HistoricalClient;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};

/// How a command finished.
///
/// A batch job that is still queued is not a failure and not a success: nothing is
/// wrong, the data just is not ready. It gets its own exit code so a shell loop can
/// tell "come back later" from "give up", which is what makes re-invoking the tool the
/// poll loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The work is done.
    Settled,
    /// The work is unfinished but healthy; run the same command again later.
    Nonterminal,
}

impl From<Outcome> for ExitCode {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Settled => Self::SUCCESS,
            Outcome::Nonterminal => Self::from(3),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(outcome) => outcome.into(),
        Err(err) => {
            eprintln!("Error: {err:?}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<Outcome> {
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
