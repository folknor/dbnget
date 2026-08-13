//! The default command: fetch data, with the command itself as the state machine.
//!
//! The whole request is one batch job on the vendor's account (Databento keeps a
//! multi-symbol submit as a single job). Each run reconciles the request against the
//! account: a finished matching job is downloaded, a pending one is reported, a
//! missing one is queued - subject to the spend gate. Re-running the same command is
//! the poll loop.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use databento::{
    HistoricalClient,
    dbn::{Compression, Encoding, SType, Schema},
    historical::{
        DateTimeRange,
        batch::{BatchJob, JobState, SplitDuration, SubmitJobParams},
        metadata::GetQueryParams,
        timeseries::GetRangeParams,
    },
};
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use tracing::{info, warn};

use crate::{Outcome, cli::FetchArgs, jobs, query, spend};

/// The stamp format for `--immediate` file names: sortable, and free of separators
/// that would collide with the dotted fields around it.
const STAMP: &[FormatItem<'_>] = format_description!("[year][month][day]T[hour][minute]");

/// A fully validated, normalized request. Normalization matters because adoption keys
/// on the request: a near-miss (unsorted symbols, un-normalized dates) re-buys.
#[derive(Debug)]
struct Request {
    dataset: String,
    schema: Schema,
    symbols: databento::Symbols,
    range: DateTimeRange,
    stype_in: SType,
    stype_out: SType,
    limit: Option<std::num::NonZeroU64>,
    encoding: Encoding,
}

pub async fn run(client: &mut HistoricalClient, args: &FetchArgs) -> Result<Outcome> {
    let request = validate(args)?;

    if args.cost {
        return cost(client, &request).await;
    }
    if args.immediate {
        return immediate(client, args, &request).await;
    }
    reconcile(client, args, &request).await
}

fn validate(args: &FetchArgs) -> Result<Request> {
    let Some(dataset) = args.dataset.clone() else {
        bail!("--dataset is required");
    };
    let Some(schema) = args.schema else {
        bail!("--schema is required");
    };
    let Some(start) = args.start.as_deref() else {
        bail!("--start is required");
    };
    Ok(Request {
        dataset,
        schema,
        symbols: query::symbols(&args.symbols)?,
        range: query::date_time_range(start, args.end.as_deref())?,
        stype_in: args.stype_in,
        stype_out: args.stype_out,
        limit: args.limit,
        encoding: args.format.into(),
    })
}

/// `--cost`: price the request and stop. Never queues.
async fn cost(client: &mut HistoricalClient, request: &Request) -> Result<Outcome> {
    let quote = spend::fetch(client, &request.metadata_params()).await?;
    println!("records:       {}", quote.records);
    println!("billable size: {} bytes", quote.billable_bytes);
    println!("cost:          ${:.2}", quote.usd);
    if quote.is_empty() {
        println!();
        println!("This query matches no records. A $0.00 quote here means empty, not free.");
    }
    Ok(Outcome::Settled)
}

/// The default path: reconcile the request against the vendor's job listing.
async fn reconcile(
    client: &mut HistoricalClient,
    args: &FetchArgs,
    request: &Request,
) -> Result<Outcome> {
    let params = request.submit_params();
    let listing = jobs::all(client).await?;

    if let Some(job) = jobs::find_live(&listing, &params) {
        return match job.state {
            JobState::Done => {
                info!(job_id = %job.id, "adopting finished job");
                jobs::download(client, &job.id, &args.output, args.min_free_gb).await
            }
            _ => {
                report_pending(&job);
                Ok(Outcome::Nonterminal)
            }
        };
    }

    // A matching expired job means the account already paid for this exact data once,
    // and the prepared files are gone. Submitting again is a re-purchase at full
    // price, which must be said out loud rather than surfacing as a generic refusal.
    if let Some(expired) = jobs::find_expired(&listing, &params) {
        warn!(
            job_id = %expired.id,
            expired_on = %expired
                .ts_expiration
                .map_or_else(|| "unknown".to_owned(), |t| t.date().to_string()),
            "a job for this exact request expired on the vendor side; proceeding re-purchases it at full price"
        );
    }

    let quote = spend::fetch(client, &request.metadata_params()).await?;
    spend::approve(&quote, args.spend)?;

    let job = client
        .batch()
        .submit_job(&params)
        .await
        .context("submitting batch job")?;
    jobs::print_job(&job);
    println!("queued; re-run the same command to poll and, once done, download");
    Ok(Outcome::Nonterminal)
}

/// One line about a job that is still preparing: state, how long it has been in that
/// state, and the vendor's percent completion.
fn report_pending(job: &BatchJob) {
    let (verb, since) = match job.state {
        JobState::Processing => ("processing", job.ts_process_start),
        _ => ("queued", job.ts_queued.or(Some(job.ts_received))),
    };
    let for_how_long = since.map_or_else(String::new, |t| format!(" for {}", ago(t)));
    let progress = job
        .progress
        .map_or_else(String::new, |p| format!(", {p}% complete"));
    println!("{}  {verb}{for_how_long}{progress}", job.id);
    println!("not ready; re-run the same command to poll");
}

/// A duration since `instant`, in the largest sensible unit.
fn ago(instant: OffsetDateTime) -> String {
    let secs = (OffsetDateTime::now_utc() - instant).whole_seconds().max(0);
    match secs {
        0..=119 => format!("{secs}s"),
        120..=7199 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

/// `--immediate`: one plain streaming request, written straight to disk. No session
/// splitting, no resume - with batch as the default path, the vendor does the
/// splitting. Streaming bills the moment the request is issued, so the spend gate
/// applies exactly as on the batch path.
async fn immediate(
    client: &mut HistoricalClient,
    args: &FetchArgs,
    request: &Request,
) -> Result<Outcome> {
    if request.encoding != Encoding::Dbn {
        bail!("--immediate delivers DBN only; drop --format or use the batch path");
    }

    let quote = spend::fetch(client, &request.metadata_params()).await?;
    spend::approve(&quote, args.spend)?;

    tokio::fs::create_dir_all(&args.output)
        .await
        .with_context(|| format!("creating output directory {}", args.output.display()))?;
    let path = args.output.join(request.file_name());

    info!(path = %path.display(), "streaming");
    let params = GetRangeParams::builder()
        .dataset(&request.dataset)
        .symbols(request.symbols.clone())
        .schema(request.schema)
        .date_time_range(request.range.clone())
        .stype_in(request.stype_in)
        .stype_out(request.stype_out)
        .maybe_limit(request.limit)
        .build();
    let decoder = client
        .timeseries()
        .get_range_to_file(&params.with_path(&path))
        .await
        .with_context(|| format!("downloading {}", path.display()))?;
    drop(decoder);

    println!("{}", path.display());
    Ok(Outcome::Settled)
}

impl Request {
    fn metadata_params(&self) -> GetQueryParams {
        GetQueryParams::builder()
            .dataset(&self.dataset)
            .symbols(self.symbols.clone())
            .schema(self.schema)
            .date_time_range(self.range.clone())
            .stype_in(self.stype_in)
            .maybe_limit(self.limit)
            .build()
    }

    /// The batch submission this request corresponds to. The non-negotiated fields
    /// (compression, split duration) are fixed so that every run of the same command
    /// builds the identical request, which is what adoption matches on.
    fn submit_params(&self) -> SubmitJobParams {
        SubmitJobParams::builder()
            .dataset(&self.dataset)
            .symbols(self.symbols.clone())
            .schema(self.schema)
            .date_time_range(self.range.clone())
            .stype_in(self.stype_in)
            .stype_out(self.stype_out)
            .maybe_limit(self.limit)
            .encoding(self.encoding)
            .compression(Compression::Zstd)
            .split_duration(SplitDuration::Day)
            .build()
    }

    /// Names an `--immediate` file after the query it holds, both bounds included, so
    /// a directory of them stays self-describing.
    fn file_name(&self) -> PathBuf {
        let dataset = self.dataset.replace('.', "_");
        let start = stamp(self.range.start);
        let end = stamp(self.range.end);
        PathBuf::from(format!("{dataset}.{}.{start}-{end}.dbn.zst", self.schema))
    }
}

fn stamp(instant: OffsetDateTime) -> String {
    instant
        .format(STAMP)
        .unwrap_or_else(|_| instant.unix_timestamp().to_string())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "assertions in tests should panic loudly"
)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn request() -> Request {
        Request {
            dataset: "GLBX.MDP3".to_owned(),
            schema: Schema::Trades,
            symbols: databento::Symbols::Symbols(vec!["ESM4".to_owned()]),
            range: DateTimeRange::from(
                datetime!(2024-04-30 22:00 UTC)..datetime!(2024-05-01 21:00 UTC),
            ),
            stype_in: SType::RawSymbol,
            stype_out: SType::InstrumentId,
            limit: None,
            encoding: Encoding::Dbn,
        }
    }

    #[test]
    fn immediate_file_names_carry_both_bounds() {
        assert_eq!(
            request().file_name().to_str().unwrap(),
            "GLBX_MDP3.trades.20240430T2200-20240501T2100.dbn.zst"
        );
    }

    #[test]
    fn missing_required_flags_are_reported_by_name() {
        let args = FetchArgs {
            symbols: vec!["ESM4".to_owned()],
            dataset: None,
            schema: None,
            start: None,
            end: None,
            stype_in: SType::RawSymbol,
            stype_out: SType::InstrumentId,
            limit: None,
            format: crate::cli::CliFormat::Dbn,
            output: PathBuf::from("."),
            immediate: false,
            spend: None,
            cost: false,
            min_free_gb: 1,
        };
        let err = validate(&args).unwrap_err().to_string();
        assert!(err.contains("--dataset"), "{err}");
    }
}
