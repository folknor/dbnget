//! `dbnget batch` - submit, poll and download Databento batch jobs.

use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use databento::{
    HistoricalClient, Symbols,
    historical::batch::{BatchJob, DownloadParams, JobState, ListJobsParams, SubmitJobParams},
};
use time::OffsetDateTime;
use tracing::{info, warn};

use crate::{
    Outcome,
    cli::{BatchCommand, BatchDownloadArgs, BatchListArgs, BatchSubmitArgs, WaitArgs},
    disk, query, spend, verify,
};

pub async fn run(client: &mut HistoricalClient, command: &BatchCommand) -> Result<Outcome> {
    match command {
        BatchCommand::Submit(args) => submit(client, args).await,
        BatchCommand::List(args) => list(client, args).await,
        BatchCommand::Status { job_id } => status(client, job_id).await,
        BatchCommand::Download(args) => download(client, args).await,
    }
}

async fn submit(client: &mut HistoricalClient, args: &BatchSubmitArgs) -> Result<Outcome> {
    let params = submit_params(args)?;

    // Ask the vendor what we already own before paying for it again. Without this, a
    // re-run of a command whose first attempt succeeded buys the same data twice.
    if let Some(existing) = find_existing(client, &params).await? {
        info!(job_id = %existing.id, state = ?existing.state, "adopting matching job");
        return settle(client, &existing.id, args).await;
    }

    let quote = spend::fetch(client, &query::metadata_params(&args.query)?).await?;
    if !args.confirm {
        println!("would submit: {} {} {quote}", params.dataset, params.schema);
        println!("pass --confirm --max-dollars USD to submit");
        return Ok(Outcome::Settled);
    }
    spend::approve(&quote, args.max_dollars)?;

    let job = client
        .batch()
        .submit_job(&params)
        .await
        .context("submitting batch job")?;
    println!("{}", job.id);
    print_job(&job);

    settle(client, &job.id, args).await
}

/// Waits for and downloads a job, when the caller asked for that.
async fn settle(
    client: &mut HistoricalClient,
    job_id: &str,
    args: &BatchSubmitArgs,
) -> Result<Outcome> {
    let Some(out) = args.wait_and_download.as_deref() else {
        return Ok(Outcome::Settled);
    };
    if wait_for_job(client, job_id, &args.wait).await? == Outcome::Nonterminal {
        return Ok(Outcome::Nonterminal);
    }
    download_files(client, job_id, out, None, args.min_free_gb).await
}

fn submit_params(args: &BatchSubmitArgs) -> Result<SubmitJobParams> {
    Ok(SubmitJobParams::builder()
        .dataset(&args.query.dataset)
        .symbols(query::symbols(&args.query.symbols)?)
        .schema(args.query.schema)
        .date_time_range(query::date_time_range(&args.query)?)
        .stype_in(args.query.stype_in)
        .stype_out(args.query.stype_out)
        .maybe_limit(args.query.limit)
        .encoding(args.encoding.into())
        .compression(args.compression.into())
        .split_duration(args.split_duration.into())
        .split_symbols(args.split_symbols)
        .pretty_px(args.pretty_px)
        .pretty_ts(args.pretty_ts)
        .build())
}

/// Finds a live job that would deliver exactly what this submission asks for.
///
/// Matching is on the request, not on any name we gave it, because the vendor is the
/// only record of what was actually bought. An expired job is not adoptable: it has no
/// downloadable files left.
async fn find_existing(
    client: &mut HistoricalClient,
    params: &SubmitJobParams,
) -> Result<Option<BatchJob>> {
    let states = vec![JobState::Done, JobState::Processing, JobState::Queued];
    let listing = ListJobsParams::builder().states(states).build();
    let jobs = client
        .batch()
        .list_jobs(&listing)
        .await
        .context("listing existing jobs before submitting")?;

    let mut matches: Vec<BatchJob> = jobs
        .into_iter()
        .filter(|job| job_matches(job, params))
        .collect();
    matches.sort_by_key(|job| (state_rank(job.state), job.ts_received));
    Ok(matches.pop())
}

/// Prefers a job that is ready over one still being prepared, and the newest of equals.
fn state_rank(state: JobState) -> u8 {
    match state {
        JobState::Done => 3,
        JobState::Processing => 2,
        JobState::Queued => 1,
        JobState::Expired => 0,
    }
}

/// Whether an existing job would deliver the same bytes this submission would buy.
///
/// Both the selection and the output format have to agree. A job covering the right
/// records in the wrong encoding is not a substitute for the one being submitted.
fn job_matches(job: &BatchJob, params: &SubmitJobParams) -> bool {
    job.dataset == params.dataset
        && job.schema == params.schema
        && job.stype_in == params.stype_in
        && job.stype_out == params.stype_out
        && job.encoding == params.encoding
        && job.compression == params.compression
        && job.split_duration == params.split_duration
        && job.split_symbols == params.split_symbols
        && job.limit == params.limit
        && same_instant(job.start, params.date_time_range.start)
        && same_instant(job.end, params.date_time_range.end)
        && same_symbols(&job.symbols, &params.symbols)
}

fn same_instant(left: OffsetDateTime, right: OffsetDateTime) -> bool {
    left.unix_timestamp_nanos() == right.unix_timestamp_nanos()
}

/// Compares symbol selections as sets, case-insensitively.
///
/// The vendor echoes back what it parsed, so neither the ordering nor the letter case
/// of the original request survives to be compared directly.
fn same_symbols(left: &Symbols, right: &Symbols) -> bool {
    match (left, right) {
        (Symbols::All, Symbols::All) => true,
        (Symbols::Symbols(left), Symbols::Symbols(right)) => {
            let mut left: Vec<String> = left.iter().map(|s| s.to_uppercase()).collect();
            let mut right: Vec<String> = right.iter().map(|s| s.to_uppercase()).collect();
            left.sort_unstable();
            right.sort_unstable();
            left == right
        }
        (Symbols::Ids(left), Symbols::Ids(right)) => {
            let mut left = left.clone();
            let mut right = right.clone();
            left.sort_unstable();
            right.sort_unstable();
            left == right
        }
        _ => false,
    }
}

async fn list(client: &mut HistoricalClient, args: &BatchListArgs) -> Result<Outcome> {
    let states = if args.state.is_empty() {
        None
    } else {
        Some(args.state.iter().copied().map(JobState::from).collect())
    };
    let since = args
        .since
        .as_deref()
        .map(query::parse_instant)
        .transpose()?;
    let params = ListJobsParams::builder()
        .maybe_states(states)
        .maybe_since(since)
        .build();

    let jobs = client
        .batch()
        .list_jobs(&params)
        .await
        .context("listing batch jobs")?;
    if jobs.is_empty() {
        println!("no jobs");
    }
    for job in &jobs {
        print_job(job);
    }
    Ok(Outcome::Settled)
}

async fn status(client: &mut HistoricalClient, job_id: &str) -> Result<Outcome> {
    let job = client
        .batch()
        .get_job_details(job_id)
        .await
        .with_context(|| format!("fetching job {job_id}"))?;
    print_job(&job);

    let files = client
        .batch()
        .list_files(job_id)
        .await
        .with_context(|| format!("listing files for job {job_id}"))?;
    for file in &files {
        println!("  {} ({} bytes)", file.filename, file.size);
    }

    Ok(match job.state {
        JobState::Done => Outcome::Settled,
        _ => Outcome::Nonterminal,
    })
}

async fn download(client: &mut HistoricalClient, args: &BatchDownloadArgs) -> Result<Outcome> {
    if args.wait
        && wait_for_job(client, &args.job_id, &args.wait_args).await? == Outcome::Nonterminal
    {
        return Ok(Outcome::Nonterminal);
    }
    download_files(
        client,
        &args.job_id,
        &args.out,
        args.filename.as_deref(),
        args.min_free_gb,
    )
    .await
}

async fn download_files(
    client: &mut HistoricalClient,
    job_id: &str,
    out: &Path,
    filename: Option<&str>,
    min_free_gb: u64,
) -> Result<Outcome> {
    tokio::fs::create_dir_all(out)
        .await
        .with_context(|| format!("creating output directory {}", out.display()))?;
    disk::check_floor(out, min_free_gb)?;

    let manifest = client
        .batch()
        .list_files(job_id)
        .await
        .with_context(|| format!("listing files for job {job_id}"))?;
    for file in &manifest {
        verify::checked_file_name(&file.filename)?;
    }

    let params = DownloadParams::builder()
        .output_dir(out)
        .job_id(job_id)
        .maybe_filename_to_download(filename)
        .build();
    client
        .batch()
        .download(&params)
        .await
        .with_context(|| format!("downloading job {job_id}"))?;

    // The client puts a job's files in a directory named after the job.
    let job_dir = out.join(job_id);
    let wanted: Vec<_> = match filename {
        Some(name) => manifest
            .into_iter()
            .filter(|f| f.filename == name)
            .collect(),
        None => manifest,
    };
    let paths = verify::job_files(&job_dir, &wanted).await?;

    for path in &paths {
        println!("{}", path.display());
    }
    Ok(Outcome::Settled)
}

/// Polls until the job is downloadable, the wait budget runs out, or it can no longer
/// become downloadable.
///
/// Running out of budget is not an error. A month of MBP-1 legitimately takes hours to
/// prepare, so the answer is "not yet", and the job keeps preparing whether or not
/// anything is watching.
async fn wait_for_job(
    client: &mut HistoricalClient,
    job_id: &str,
    wait: &WaitArgs,
) -> Result<Outcome> {
    let deadline = OffsetDateTime::now_utc() + time::Duration::seconds(wait.max_wait_secs());
    let mut interval = Duration::from_secs(wait.poll_interval);
    let ceiling = Duration::from_secs(wait.max_poll_interval);

    loop {
        let job = client
            .batch()
            .get_job_details(job_id)
            .await
            .with_context(|| format!("polling job {job_id}"))?;
        match job.state {
            JobState::Done => return Ok(Outcome::Settled),
            JobState::Expired => bail!("job {job_id} expired before it could be downloaded"),
            state => info!(
                job_id,
                ?state,
                progress = job.progress.unwrap_or(0),
                "waiting"
            ),
        }

        if OffsetDateTime::now_utc() + interval >= deadline {
            warn!(job_id, "still preparing when the wait budget ran out");
            return Ok(Outcome::Nonterminal);
        }
        tokio::time::sleep(interval).await;
        // Back off so a job that takes hours does not make hundreds of requests.
        interval = (interval * 2).min(ceiling);
    }
}

/// How much of the symbol list to show before summarising the rest.
const SYMBOL_WIDTH: usize = 28;

fn print_job(job: &BatchJob) {
    let cost = job
        .cost_usd
        .map_or_else(|| "-".to_owned(), |c| format!("${c:.2}"));
    let size = job.actual_size.map_or_else(|| "-".to_owned(), human_bytes);
    println!(
        "{id}  {state:<10}  {dataset:<10} {schema:<10} {selection:<34}  {start}..{end}  {cost:>8}  {size:>9}",
        id = job.id,
        state = format!("{:?}", job.state),
        dataset = job.dataset,
        schema = job.schema.to_string(),
        selection = selection(job),
        start = job.start.date(),
        end = job.end.date(),
    );
}

/// The symbols a job covers, qualified by the symbology they are written in.
///
/// `ES.FUT` means nothing on its own: as a raw symbol it is one instrument that may not
/// exist, and as a parent symbol it is every ES future. The stype belongs next to it.
fn selection(job: &BatchJob) -> String {
    format!("{}:{}", job.stype_in, summarize_symbols(&job.symbols))
}

/// Renders a symbol list short enough to sit in a column, without hiding how many were
/// left out.
fn summarize_symbols(symbols: &Symbols) -> String {
    let names: Vec<String> = match symbols {
        Symbols::All => return "ALL_SYMBOLS".to_owned(),
        Symbols::Symbols(list) => list.clone(),
        Symbols::Ids(list) => list.iter().map(u32::to_string).collect(),
    };
    if names.is_empty() {
        return "-".to_owned();
    }

    let mut out = String::new();
    let mut shown = 0;
    for name in &names {
        if !out.is_empty() && out.len() + name.len() + 1 > SYMBOL_WIDTH {
            break;
        }
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(name);
        shown += 1;
    }
    if shown < names.len() {
        out.push_str(&format!("+{}", names.len() - shown));
    }
    out
}

/// Byte counts as humans read them. Batch jobs run to tens of gigabytes, where a raw
/// count is just a wall of digits.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    #[expect(
        clippy::cast_precision_loss,
        reason = "the value is formatted for humans, not compared"
    )]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_sets_ignore_order_and_case() {
        let left = Symbols::Symbols(vec!["esm4".to_owned(), "NQM4".to_owned()]);
        let right = Symbols::Symbols(vec!["nqm4".to_owned(), "ESM4".to_owned()]);
        assert!(same_symbols(&left, &right));
    }

    #[test]
    fn different_symbol_sets_do_not_match() {
        let left = Symbols::Symbols(vec!["ESM4".to_owned()]);
        let right = Symbols::Symbols(vec!["ESM4".to_owned(), "NQM4".to_owned()]);
        assert!(!same_symbols(&left, &right));
        assert!(!same_symbols(&Symbols::All, &right));
    }

    #[test]
    fn short_symbol_lists_are_shown_whole() {
        let symbols = Symbols::Symbols(vec!["ES.FUT".to_owned(), "NQ.FUT".to_owned()]);
        assert_eq!(summarize_symbols(&symbols), "ES.FUT,NQ.FUT");
        assert_eq!(summarize_symbols(&Symbols::All), "ALL_SYMBOLS");
    }

    #[test]
    fn long_symbol_lists_say_how_many_were_omitted() {
        let names: Vec<String> = (0..20).map(|i| format!("SYM{i:02}")).collect();
        let summary = summarize_symbols(&Symbols::Symbols(names));
        assert!(
            summary.contains('+'),
            "{summary} should count the remainder"
        );
        assert!(summary.len() <= SYMBOL_WIDTH + 4, "{summary} is too wide");
    }

    #[test]
    fn byte_counts_are_scaled() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(60_648_379_440), "56.5 GiB");
    }

    #[test]
    fn a_ready_job_outranks_one_still_preparing() {
        assert!(state_rank(JobState::Done) > state_rank(JobState::Processing));
        assert!(state_rank(JobState::Processing) > state_rank(JobState::Queued));
        assert!(state_rank(JobState::Queued) > state_rank(JobState::Expired));
    }
}
