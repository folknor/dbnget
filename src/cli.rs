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
    /// List the batch jobs on the account.
    List(ListArgs),
    /// Query dataset metadata.
    #[command(subcommand)]
    Meta(MetaCommand),
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

    /// Exclusive end, as `YYYY-MM-DD` or an RFC 3339 timestamp. Defaults to one day
    /// after `--start` when `--start` is a plain date.
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

    /// Refuse to start a download when the output filesystem has less than this many
    /// gibibytes free. Zero disables the check.
    #[arg(long, default_value_t = 1, value_name = "GIB")]
    pub min_free_gb: u64,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Only show jobs in these states.
    #[arg(long, value_delimiter = ',')]
    pub state: Vec<CliJobState>,

    /// Only show jobs submitted on or after this date (`YYYY-MM-DD` or RFC 3339).
    #[arg(long)]
    pub since: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum MetaCommand {
    /// List the datasets available to the account.
    Datasets,
    /// List the schemas offered by a dataset.
    Schemas {
        /// Dataset code, e.g. `GLBX.MDP3`.
        dataset: String,
    },
    /// Show the available date range of a dataset.
    Range {
        /// Dataset code, e.g. `GLBX.MDP3`.
        dataset: String,
    },
    /// List the publishers across all datasets.
    Publishers,
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
