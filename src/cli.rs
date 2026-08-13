use std::{num::NonZeroU64, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use databento::dbn::{SType, Schema};

/// Command-line downloader for Databento historical market data.
#[derive(Debug, Parser)]
#[command(name = "dbnget", version, about, long_about = None)]
pub struct Cli {
    /// Databento API key. Falls back to the `DATABENTO_API_KEY` environment variable.
    #[arg(long, global = true, env = "DATABENTO_API_KEY", hide_env_values = true)]
    pub key: Option<String>,

    /// Increase log verbosity. Repeat for more detail.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Stream a date range straight to DBN files on disk.
    Get(GetArgs),
    /// Estimate the record count, billable size and cost of a query.
    Cost(QueryArgs),
    /// Submit, inspect and download batch jobs.
    #[command(subcommand)]
    Batch(BatchCommand),
    /// Query dataset metadata.
    #[command(subcommand)]
    Meta(MetaCommand),
}

/// The query parameters shared by every request that reads market data.
#[derive(Debug, Args)]
pub struct QueryArgs {
    /// Dataset code, e.g. `GLBX.MDP3`.
    #[arg(short, long)]
    pub dataset: String,

    /// Record schema, e.g. `trades`, `mbo`, `ohlcv-1m`.
    #[arg(short, long)]
    pub schema: Schema,

    /// Comma-separated symbols, or `ALL_SYMBOLS` for the whole dataset.
    #[arg(short = 'S', long, value_delimiter = ',')]
    pub symbols: Vec<String>,

    /// Inclusive start, as `YYYY-MM-DD` or an RFC 3339 timestamp.
    #[arg(long)]
    pub start: String,

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
}

#[derive(Debug, Args)]
pub struct GetArgs {
    #[command(flatten)]
    pub query: QueryArgs,

    /// Directory to write DBN files into.
    #[arg(short, long, default_value = ".")]
    pub out: PathBuf,

    /// Split the range into one request and one file per UTC day, so an
    /// interrupted download resumes at the first missing day.
    #[arg(long)]
    pub daily: bool,

    /// Re-download files that already exist instead of skipping them.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum BatchCommand {
    /// Submit a new batch job.
    Submit(BatchSubmitArgs),
    /// List batch jobs on the account.
    List(BatchListArgs),
    /// Show the details of a single job.
    Status {
        /// The job ID.
        job_id: String,
    },
    /// Download the files of a completed job.
    Download(BatchDownloadArgs),
}

#[derive(Debug, Args)]
pub struct BatchSubmitArgs {
    #[command(flatten)]
    pub query: QueryArgs,

    /// Output encoding of the batched files.
    #[arg(long, default_value = "dbn")]
    pub encoding: CliEncoding,

    /// Compression applied to the batched files.
    #[arg(long, default_value = "zstd")]
    pub compression: CliCompression,

    /// Time interval at which the output is split into separate files.
    #[arg(long, default_value = "day")]
    pub split_duration: CliSplitDuration,

    /// Write one set of files per raw symbol.
    #[arg(long)]
    pub split_symbols: bool,

    /// Format prices with their fixed-precision scale. Text encodings only.
    #[arg(long)]
    pub pretty_px: bool,

    /// Format timestamps as ISO 8601 strings. Text encodings only.
    #[arg(long)]
    pub pretty_ts: bool,

    /// Wait until the job finishes, then download it to this directory.
    #[arg(long, value_name = "DIR")]
    pub wait_and_download: Option<PathBuf>,

    /// Seconds between polls while waiting for a job.
    #[arg(long, default_value_t = 15, value_name = "SECS")]
    pub poll_interval: u64,
}

#[derive(Debug, Args)]
pub struct BatchListArgs {
    /// Only show jobs in these states.
    #[arg(long, value_delimiter = ',')]
    pub state: Vec<CliJobState>,

    /// Only show jobs submitted on or after this date (`YYYY-MM-DD` or RFC 3339).
    #[arg(long)]
    pub since: Option<String>,
}

#[derive(Debug, Args)]
pub struct BatchDownloadArgs {
    /// The job ID.
    pub job_id: String,

    /// Directory to download into.
    #[arg(short, long, default_value = ".")]
    pub out: PathBuf,

    /// Download only this file rather than the whole job.
    #[arg(long)]
    pub filename: Option<String>,

    /// Block until the job reaches the `done` state before downloading.
    #[arg(long)]
    pub wait: bool,

    /// Seconds between polls while waiting for a job.
    #[arg(long, default_value_t = 15, value_name = "SECS")]
    pub poll_interval: u64,
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

/// Mirrors [`databento::historical::batch::Delivery`]-adjacent enums so that clap can
/// derive `ValueEnum` for them without touching the upstream types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliEncoding {
    Dbn,
    Csv,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliCompression {
    None,
    Zstd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliSplitDuration {
    Day,
    Week,
    Month,
    Year,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliJobState {
    Queued,
    Processing,
    Done,
    Expired,
}
