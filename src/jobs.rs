//! The account's batch jobs: listing them, matching a request against them, and
//! downloading a finished one.

use std::path::Path;

use anyhow::{Context, Result};
use databento::{
    HistoricalClient, Symbols,
    historical::batch::{BatchJob, DownloadParams, JobState, ListJobsParams, SubmitJobParams},
};
use time::OffsetDateTime;

use crate::{Outcome, cli::ListArgs, disk, query, verify};

/// `dbnget list` - the vendor's job listing is the only account of what was bought,
/// and this is the user's tool for checking it before submitting.
pub async fn list(client: &mut HistoricalClient, args: &ListArgs) -> Result<Outcome> {
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

/// Fetches every job on the account, in every state. Expired jobs are included so the
/// caller can warn that a re-submit is a re-purchase.
pub async fn all(client: &mut HistoricalClient) -> Result<Vec<BatchJob>> {
    let params = ListJobsParams::builder().build();
    client
        .batch()
        .list_jobs(&params)
        .await
        .context("listing existing jobs")
}

/// Picks the best live job that would deliver exactly what `params` asks for.
///
/// Matching is on the request, not on any name we gave it, because the vendor is the
/// only record of what was actually bought. An expired job is not adoptable: it has no
/// downloadable files left.
pub fn find_live(jobs: &[BatchJob], params: &SubmitJobParams) -> Option<BatchJob> {
    let mut matches: Vec<&BatchJob> = jobs
        .iter()
        .filter(|job| job.state != JobState::Expired && job_matches(job, params))
        .collect();
    matches.sort_by_key(|job| (state_rank(job.state), job.ts_received));
    matches.pop().cloned()
}

/// Finds an expired job matching `params`, whose data the account already paid for.
pub fn find_expired(jobs: &[BatchJob], params: &SubmitJobParams) -> Option<BatchJob> {
    jobs.iter()
        .find(|job| job.state == JobState::Expired && job_matches(job, params))
        .cloned()
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
/// of the original request survives to be compared directly. A multi-symbol job can
/// come back as one comma-joined string rather than a list, so elements are split on
/// commas before comparing.
fn same_symbols(left: &Symbols, right: &Symbols) -> bool {
    fn normalize(list: &[String]) -> Vec<String> {
        let mut out: Vec<String> = list
            .iter()
            .flat_map(|s| s.split(','))
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        out.sort_unstable();
        out
    }

    match (left, right) {
        (Symbols::All, Symbols::All) => true,
        (Symbols::Symbols(left), Symbols::Symbols(right)) => normalize(left) == normalize(right),
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

/// Downloads a finished job into `out/JOB_ID/` and verifies every file against the
/// manifest. Prints the verified paths.
pub async fn download(
    client: &mut HistoricalClient,
    job_id: &str,
    out: &Path,
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
        .build();
    client
        .batch()
        .download(&params)
        .await
        .with_context(|| format!("downloading job {job_id}"))?;

    // The client puts a job's files in a directory named after the job.
    let job_dir = out.join(job_id);
    let paths = verify::job_files(&job_dir, &manifest).await?;

    for path in &paths {
        println!("{}", path.display());
    }
    Ok(Outcome::Settled)
}

/// How much of the symbol list to show before summarising the rest.
const SYMBOL_WIDTH: usize = 28;

pub fn print_job(job: &BatchJob) {
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
    fn a_comma_joined_echo_matches_the_list_it_came_from() {
        let echoed = Symbols::Symbols(vec!["CLZ4,GCZ4".to_owned()]);
        let requested = Symbols::Symbols(vec!["GCZ4".to_owned(), "clz4".to_owned()]);
        assert!(same_symbols(&echoed, &requested));
        let other = Symbols::Symbols(vec!["CLZ4".to_owned()]);
        assert!(!same_symbols(&echoed, &other));
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
