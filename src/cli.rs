use std::{num::NonZeroU64, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use databento::dbn::{SType, Schema};

/// Command-line downloader for Databento historical market data.
///
/// The bare command is the whole workflow: it queues a batch job for the request,
/// reports progress on a job still preparing, and downloads a job that is ready.
/// Re-running the same command is the poll loop.
#[derive(Debug, Parser)]
#[command(name = "dbnget", version, about, long_about = None)]
pub struct Cli {
    /// Databento API key, or a path to a file holding one. Falls back to the
    /// `DATABENTO_API_KEY` environment variable, which gets the same treatment.
    #[arg(long, global = true, env = "DATABENTO_API_KEY", hide_env_values = true)]
    pub key: Option<String>,

    /// Increase log verbosity. Repeat for more detail.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub fetch: FetchArgs,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the batch jobs on the account, or the datasets available to it.
    List(ListArgs),
    /// Show one dataset: its available range and schemas, or its publishers.
    Dataset(DatasetArgs),
}

/// The default command: fetch data for a set of symbols.
#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Symbols to fetch, or `ALL_SYMBOLS` for the whole dataset.
    pub symbols: Vec<String>,

    /// Dataset code, e.g. `GLBX.MDP3`.
    #[arg(short, long)]
    pub dataset: Option<String>,

    /// Record schema, e.g. `trades`, `mbo`, `ohlcv-1m`.
    #[arg(short, long)]
    pub schema: Option<Schema>,

    /// Inclusive start, as `YYYY-MM-DD` or an RFC 3339 timestamp.
    #[arg(long)]
    pub start: Option<String>,

    /// Exclusive end, as `YYYY-MM-DD` or an RFC 3339 timestamp. Defaults to exactly 24
    /// hours after `--start`, whether `--start` is a plain date or a timestamp.
    #[arg(long)]
    pub end: Option<String>,

    /// Symbology of the input symbols.
    #[arg(long, default_value = "raw_symbol")]
    pub stype_in: SType,

    /// Symbology of the symbols in the output records.
    #[arg(long, default_value = "instrument_id")]
    pub stype_out: SType,

    /// Stop after this many records.
    #[arg(long)]
    pub limit: Option<NonZeroU64>,

    /// Output file format.
    #[arg(long, default_value = "dbn")]
    pub format: CliFormat,

    /// Directory to write downloaded files into.
    #[arg(short, long, default_value = ".")]
    pub output: PathBuf,

    /// Stream the data synchronously instead of queueing a batch job. DBN only.
    #[arg(long)]
    pub immediate: bool,

    /// The most this request may cost, in US dollars. Without it, anything that
    /// would cost more than $0.00 is refused, so the default fetches only what a
    /// subscription already covers.
    #[arg(long, value_name = "USD")]
    pub spend: Option<f64>,

    /// Price the request and stop. Never queues or downloads anything.
    #[arg(long)]
    pub cost: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// What to list.
    #[arg(default_value = "jobs")]
    pub what: ListWhat,

    /// Only show jobs in these states.
    #[arg(long, value_delimiter = ',')]
    pub state: Vec<CliJobState>,

    /// Only show jobs submitted on or after this date (`YYYY-MM-DD` or RFC 3339).
    #[arg(long)]
    pub since: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ListWhat {
    /// The batch jobs on the account.
    Jobs,
    /// The datasets the account can access.
    Datasets,
}

#[derive(Debug, Args)]
pub struct DatasetArgs {
    /// Dataset code, e.g. `GLBX.MDP3`.
    pub dataset: String,

    /// Show the dataset's publishers instead of its range and schemas.
    #[arg(long)]
    pub publishers: bool,
}

/// Mirrors the upstream encoding enum so that clap can derive `ValueEnum` for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliFormat {
    Dbn,
    Csv,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliJobState {
    Queued,
    Processing,
    Done,
    Expired,
}
