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

    /// Increase log verbosity: `-v` debug, `-vv` trace, `-vvv` adds the vendor client's
    /// own logging, which is voluminous.
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
    /// Download a finished batch job by its id.
    Get(GetArgs),
}

/// `dbnget get JOB_ID` - download a job the account already owns.
///
/// Reconciliation adopts a finished job only when the command reproduces the original
/// request exactly, field for field. That is the right rule for deciding whether to
/// spend money, and a poor one for retrieving data already bought: a job whose exact
/// command has been lost is otherwise unreachable, and `dbnget list` shows plenty of
/// them. Naming the job directly skips the reconstruction entirely.
///
/// Nothing here can charge. The job is already paid for, the files already prepared,
/// and downloading them again costs nothing - which is why this path has no spend gate,
/// and why it is not a hole in one.
#[derive(Debug, Args)]
pub struct GetArgs {
    /// The job id, as shown by `dbnget list jobs`.
    pub job_id: String,

    /// Directory to write the job's files into. They land in `OUTPUT/JOB_ID/`.
    #[arg(short, long, default_value = ".")]
    pub output: PathBuf,
}

/// The default command: fetch data for a set of symbols.
#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Symbols to fetch, or `ALL_SYMBOLS` for the whole dataset.
    ///
    /// Read according to `--stype-in`, so what a symbol selects depends on that flag.
    /// Repeated symbols select once. Symbols are compared case-insensitively as a set
    /// when deciding whether a job already bought this request.
    pub symbols: Vec<String>,

    /// Dataset code, e.g. `GLBX.MDP3`.
    ///
    /// `dbnget list datasets` shows the ones this account can access.
    #[arg(short, long)]
    pub dataset: Option<String>,

    /// Record schema, e.g. `trades`, `mbo`, `ohlcv-1m`.
    ///
    /// `dbnget dataset CODE` lists the schemas a dataset carries. `definition` is the
    /// instrument catalogue rather than a tick stream, which is how you enumerate what
    /// exists in a dataset.
    #[arg(short, long)]
    pub schema: Option<Schema>,

    /// Inclusive start, as `YYYY-MM-DD` or an RFC 3339 timestamp.
    ///
    /// `dbnget dataset CODE` shows the range a dataset actually covers.
    #[arg(long)]
    pub start: Option<String>,

    /// Exclusive end, as `YYYY-MM-DD` or an RFC 3339 timestamp. Defaults to exactly 24
    /// hours after `--start`, whether `--start` is a plain date or a timestamp.
    #[arg(long)]
    pub end: Option<String>,

    /// How to read the symbols you passed.
    ///
    /// The same string means different things in different symbologies, and the
    /// difference is what you are billed for: `ES.FUT` is one instrument as a
    /// `raw_symbol` and every ES future as a `parent`. This flag is part of what
    /// identifies a job, so changing it makes a different request, not the same one
    /// spelled differently.
    ///
    /// The historical API currently accepts four of these as input: `raw_symbol`,
    /// `instrument_id`, `parent` and `continuous`. The rest are the vendor's full
    /// symbology set, available elsewhere in their API and not on the historical
    /// endpoints dbnget uses; passing one is refused by the server, which names the set
    /// it will take. They are listed rather than removed because the accepted set is
    /// the vendor's to change, and refusing them here would outlast the restriction.
    #[arg(long, default_value = "raw_symbol", value_name = "STYPE")]
    pub stype_in: CliSType,

    /// How symbols appear in the delivered records.
    ///
    /// Defaults to the numeric `instrument_id`. DBN files carry the id-to-symbol
    /// mappings in their metadata header, and CSV and JSON get a symbol column by
    /// default, so the names are recoverable either way. Leave this alone unless you
    /// know you need something else: symbol maps and `--cost` symbol resolution both
    /// require `instrument_id` on one side.
    #[arg(long, default_value = "instrument_id", value_name = "STYPE")]
    pub stype_out: CliSType,

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

    /// The most this request may cost, in US dollars. Without it, the cap is $0.00 and
    /// anything with a price is refused.
    ///
    /// The cap is checked against the vendor's list price, which is quoted per request
    /// with no reference to your account. A subscription that will bill you $0.00 for
    /// this data does not change the quote, and prices run to fractions of a cent, so
    /// the default cap refuses nearly everything that holds records. Use `--cost` to see
    /// the price, then pass a cap at or above it. There is no "only if free" setting:
    /// nothing the API offers before a submit can tell "covered by a subscription" from
    /// "cheap", so such a flag would be a guess about your money.
    ///
    /// You are only charged for a request the account has not already bought. Before
    /// pricing anything, dbnget checks the vendor's job listing for a job matching this
    /// exact request - same dataset, schema, symbols, bounds, symbology, format and
    /// limit - and downloads that instead if it finds one. So re-running a command is
    /// how you poll it, not how you buy it twice. `dbnget list` shows what is already
    /// there, and `dbnget get JOB_ID` fetches one by name.
    #[arg(long, value_name = "USD")]
    pub spend: Option<f64>,

    /// Price the request and stop. Never queues or downloads anything.
    #[arg(long)]
    pub cost: bool,
}

impl FetchArgs {
    /// Whether the user asked for nothing at all.
    ///
    /// `dbnget` on its own is someone finding out what the tool is, and answering that
    /// with `--dataset is required` teaches them nothing. A bare invocation that still
    /// carries `--key` or `-v` is the same question.
    /// Only the request-defining arguments count. `--format`, `--stype-in/out` and
    /// `--output` deliberately do NOT, even though a run carrying one is arguably
    /// part-way through composing a command rather than asking what the tool is.
    /// Counting them was tried and reverted: the API key is resolved before the request
    /// is validated, so `dbnget --format csv` stopped printing the help and started
    /// demanding a key instead of naming the flag it still needs - a lateral move, not
    /// a fix. Making it name the flag means validating the request before building the
    /// client, which is the change to make if this is revisited.
    pub fn is_bare(&self) -> bool {
        self.symbols.is_empty()
            && self.dataset.is_none()
            && self.schema.is_none()
            && self.start.is_none()
            && self.end.is_none()
            && self.limit.is_none()
            && !self.immediate
            && !self.cost
            && self.spend.is_none()
    }
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

/// Mirrors the upstream symbology enum so that clap can derive `ValueEnum` for it.
///
/// The upstream `SType` only implements `FromStr`, which leaves `--help` printing a
/// default and nothing else - the legal values are undiscoverable from the tool, which
/// is how you end up guessing at a flag that decides what you are billed for. The names
/// are the vendor's own, so they line up with the symbology documentation.
///
/// The deprecated `smart` symbology is deliberately absent: it was split into
/// `continuous` and `parent`.
///
/// Snake case, not clap's default kebab case, because these are the vendor's spellings:
/// `raw_symbol` is what the symbology documentation says and what the API accepts, and a
/// tool that demanded `raw-symbol` instead would be gratuitously incompatible with every
/// example a user has open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum CliSType {
    /// One exact symbol as the publisher writes it, e.g. `ESM4`. The default for input.
    RawSymbol,
    /// Databento's numeric instrument ids. The default for output, and the form
    /// downloaded records carry.
    InstrumentId,
    /// A whole product family under one symbol, e.g. `ES.FUT` is every ES future.
    Parent,
    /// A rolling series that points at different instruments over time, e.g. `ES.v.0`
    /// is front-month ES by volume.
    Continuous,
    /// US equities, NASDAQ Integrated suffix conventions.
    NasdaqSymbol,
    /// US equities, CMS suffix conventions.
    CmsSymbol,
    /// International Security Identification Numbers, ISO 6166.
    Isin,
    /// US domestic CUSIP codes.
    UsCode,
    /// Bloomberg composite global ids.
    BbgCompId,
    /// Bloomberg composite tickers.
    BbgCompTicker,
    /// Bloomberg FIGI exchange level ids.
    Figi,
    /// Bloomberg exchange level tickers.
    FigiTicker,
}

impl From<CliSType> for SType {
    fn from(stype: CliSType) -> Self {
        match stype {
            CliSType::RawSymbol => Self::RawSymbol,
            CliSType::InstrumentId => Self::InstrumentId,
            CliSType::Parent => Self::Parent,
            CliSType::Continuous => Self::Continuous,
            CliSType::NasdaqSymbol => Self::NasdaqSymbol,
            CliSType::CmsSymbol => Self::CmsSymbol,
            CliSType::Isin => Self::Isin,
            CliSType::UsCode => Self::UsCode,
            CliSType::BbgCompId => Self::BbgCompId,
            CliSType::BbgCompTicker => Self::BbgCompTicker,
            CliSType::Figi => Self::Figi,
            CliSType::FigiTicker => Self::FigiTicker,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliJobState {
    Queued,
    Processing,
    Done,
    Expired,
}
