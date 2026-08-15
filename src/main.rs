mod cli;
mod dataset;
mod fetch;
mod jobs;
mod query;
mod spend;
mod verify;

use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser};
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

    // Answer "what is this" with the help, not with a complaint about a missing flag,
    // and before demanding an API key the user has no reason to have set yet.
    if args.command.is_none() && args.fetch.is_bare() {
        Cli::command().print_help().context("printing help")?;
        return Ok(Outcome::Settled);
    }

    let mut client = build_client(args.key.as_deref())?;
    match &args.command {
        Some(Command::List(list_args)) => match list_args.what {
            cli::ListWhat::Jobs => jobs::list(&mut client, list_args).await,
            cli::ListWhat::Datasets => {
                if !list_args.state.is_empty() || list_args.since.is_some() {
                    bail!("--state and --since filter jobs, not datasets");
                }
                dataset::list(&mut client).await
            }
        },
        Some(Command::Dataset(dataset_args)) => dataset::run(&mut client, dataset_args).await,
        Some(Command::Get(get_args)) => {
            jobs::download(&mut client, &get_args.job_id, &get_args.output).await
        }
        None => fetch::run(&mut client, &args.fetch).await,
    }
}

/// `-v` raises this crate to debug, `-vv` adds trace, `-vvv` turns on debug logging
/// everywhere. `RUST_LOG`, when set, wins over all of them.
///
/// The vendor crate is held back to `info` until the third `v` on purpose. Its client
/// methods are instrumented with the client itself as a span field, so every line at
/// its debug level carries the whole `BatchClient` - base URL exploded into its `Url`
/// struct, proxies, headers - repeated per span. That was 95% of the bytes at `-vv` and
/// it buried the 5% worth reading. What people actually wanted from it, that a job
/// listing was fetched and adoption attempted, dbnget now logs itself.
fn init_tracing(verbosity: u8) {
    let default = match verbosity {
        0 => "dbnget=info",
        1 => "dbnget=debug",
        2 => "dbnget=trace,databento=info",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn build_client(key: Option<&str>) -> Result<HistoricalClient> {
    let key = key
        .map(resolve_key)
        .transpose()?
        .context("no API key: pass --key or set DATABENTO_API_KEY")?;
    HistoricalClient::builder()
        .key(key)
        .context("the API key was not accepted")?
        .build()
        .context("building the Databento client")
}

/// A key looks like `db-...`. Anything else is read as a path to a file holding the
/// key and nothing else, with surrounding whitespace trimmed. The environment
/// variable gets the same treatment as the flag.
fn resolve_key(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.starts_with("db-") {
        return Ok(raw.to_owned());
    }
    // The value is never echoed. Anything that reaches here is either a path or a
    // mistyped key, and a mistyped key is still a key: printing it puts a credential
    // into a terminal, a log, or a CI transcript, which is exactly how they escape.
    let contents = std::fs::read_to_string(raw).with_context(|| {
        "the value does not start with `db-`, and reading it as a path to a key file failed"
            .to_owned()
    })?;
    let key = contents.trim();
    if !key.starts_with("db-") {
        bail!("key file `{raw}` does not contain a `db-` API key");
    }
    Ok(key.to_owned())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "assertions in tests should panic loudly"
)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_key_passes_through() {
        assert_eq!(resolve_key("db-abc123").unwrap(), "db-abc123");
        assert_eq!(resolve_key("  db-abc123\n").unwrap(), "db-abc123");
    }

    #[test]
    fn a_key_file_is_read_and_trimmed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-keys");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("databento.key");
        std::fs::write(&path, "db-fromfile\n").unwrap();
        assert_eq!(resolve_key(path.to_str().unwrap()).unwrap(), "db-fromfile");
    }

    #[test]
    fn a_missing_file_is_an_error() {
        assert!(resolve_key("/nonexistent/key/path").is_err());
    }
}
