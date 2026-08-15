//! The account's batch jobs: listing them, matching a request against them, and
//! downloading a finished one.

use std::path::Path;

use anyhow::{Context, Result, bail};
use databento::{
    HistoricalClient, Symbols,
    dbn::Encoding,
    historical::batch::{BatchJob, DownloadParams, JobState, ListJobsParams, SubmitJobParams},
};
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use tracing::{info, warn};

use crate::{Outcome, cli::ListArgs, lock, query, verify};

/// `dbnget list` - the vendor's job listing is the only account of what was bought,
/// and this is the user's tool for checking it before submitting.
pub async fn list(client: &mut HistoricalClient, args: &ListArgs) -> Result<Outcome> {
    let states = if args.state.is_empty() {
        // Naming every state, rather than omitting the filter, for the reasons on
        // `LISTED_STATES`: an omitted filter hides expired jobs and admits states this
        // client cannot deserialize.
        LISTED_STATES.to_vec()
    } else {
        args.state.iter().copied().map(JobState::from).collect()
    };
    let since = args
        .since
        .as_deref()
        .map(query::parse_instant)
        .transpose()?;
    let params = ListJobsParams::builder()
        .states(states)
        .maybe_since(since)
        .build();

    let jobs = client
        .batch()
        .list_jobs(&params)
        .await
        .context("listing batch jobs")?;
    if jobs.is_empty() {
        println!("no jobs");
    } else {
        print_header();
    }
    for job in &jobs {
        print_job(job);
    }
    Ok(Outcome::Settled)
}

/// Every state the client can represent. Naming them is not the same as omitting the
/// filter, and the difference bites twice.
///
/// An omitted filter means "all except expired" server-side, so `find_expired` could
/// never find anything and the re-purchase warning was unreachable. And the vendor has
/// at least one state this client's enum cannot represent - a job sits in `received`
/// before it is queued - which an omitted filter would include and the client's
/// deserializer rejects outright, failing the whole listing and taking every command
/// with it.
///
/// The cost of naming them is that a job still in `received` is invisible here, so a
/// re-run inside that window submits again. That is the same submit-to-listing window
/// the README already discloses, and it is bounded by seconds; a listing that cannot be
/// parsed at all is not.
const LISTED_STATES: [JobState; 4] = [
    JobState::Queued,
    JobState::Processing,
    JobState::Done,
    JobState::Expired,
];

/// Fetches every job on the account, in every state this client understands. Expired
/// jobs are included so the caller can warn that a re-submit is a re-purchase.
pub async fn all(client: &mut HistoricalClient) -> Result<Vec<BatchJob>> {
    let params = ListJobsParams::builder()
        .states(LISTED_STATES.to_vec())
        .build();
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
        && job.split_size == params.split_size
        && job.delivery == params.delivery
        && job.pretty_px == params.pretty_px
        && job.pretty_ts == params.pretty_ts
        && job.map_symbols == effective_map_symbols(params)
        && job.limit == params.limit
        && same_instant(job.start, params.date_time_range.start)
        && same_instant(job.end, params.date_time_range.end)
        && same_symbols(&job.symbols, &params.symbols)
}

/// What `map_symbols` will actually be once the vendor applies its default.
///
/// The submission carries an `Option<bool>` and the job echoes back a concrete `bool`,
/// so comparing them directly would never match on the default path. The default is
/// encoding-dependent: text encodings get the symbol column, DBN does not.
fn effective_map_symbols(params: &SubmitJobParams) -> bool {
    params
        .map_symbols
        .unwrap_or(matches!(params.encoding, Encoding::Csv | Encoding::Json))
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
///
/// Duplicates are collapsed, which is what makes this a set comparison rather than a
/// sorted-list comparison. A request written `ESM4 ESM4 NQM4` selects exactly what
/// `ESM4 NQM4` selects, and the vendor is free to echo back either form; leaving the
/// repeat in would fail the match and re-buy data the account already owns.
fn same_symbols(left: &Symbols, right: &Symbols) -> bool {
    canonical_symbols(left) == canonical_symbols(right)
}

/// The sentinel meaning "every symbol in the dataset".
pub const ALL_SYMBOLS: &str = "ALL_SYMBOLS";

/// One symbol selection reduced to a comparable form: sorted, uppercased, deduplicated,
/// and split on the commas the vendor may have joined it with.
///
/// `Symbols::All` becomes the sentinel spelled out, because that is how it comes back.
/// The vendor echoes symbols as a JSON array, and the client only maps a SCALAR
/// `"ALL_SYMBOLS"` string to `Symbols::All` - an array containing it deserializes as an
/// ordinary one-element list. Comparing the variants directly therefore never matched a
/// whole-dataset job against the request that bought it, which is the single most
/// expensive thing this comparison could get wrong.
///
/// Shared with the `--immediate` filename key rather than kept private to matching. Two
/// requests that adoption calls the same request must produce the same file name, and
/// two it calls different must not collide - one definition of "same selection" is the
/// only way that holds.
pub fn canonical_symbols(symbols: &Symbols) -> Result<Vec<String>, Vec<u32>> {
    match symbols {
        Symbols::All => Ok(vec![ALL_SYMBOLS.to_owned()]),
        Symbols::Symbols(list) => {
            let mut out: Vec<String> = list
                .iter()
                .flat_map(|s| s.split(','))
                .map(|s| s.trim().to_uppercase())
                .filter(|s| !s.is_empty())
                .collect();
            out.sort_unstable();
            out.dedup();
            Ok(out)
        }
        Symbols::Ids(list) => {
            let mut out = list.clone();
            out.sort_unstable();
            out.dedup();
            Err(out)
        }
    }
}

/// Downloads a finished job into `out/JOB_ID/` and verifies every file against the
/// manifest. Prints the verified paths.
pub async fn download(client: &mut HistoricalClient, job_id: &str, out: &Path) -> Result<Outcome> {
    // The job id becomes a path component, and it arrives from the same response the
    // filenames do. It gets the same treatment.
    let job_id = verify::checked_file_name(job_id)
        .context("the vendor's job id is not usable as a directory name")?;

    tokio::fs::create_dir_all(out)
        .await
        .with_context(|| format!("creating output directory {}", out.display()))?;

    let manifest = client
        .batch()
        .list_files(job_id)
        .await
        .with_context(|| format!("listing files for job {job_id}"))?;
    if manifest.is_empty() {
        bail!(
            "job {job_id} is done but delivered no files; it bought nothing, so there is nothing to verify"
        );
    }
    for file in &manifest {
        verify::checked_file_name(&file.filename)?;
        // The download claims this directory with a lock file of its own, and manifest
        // names are joined into that same directory. A job delivering a file by that
        // name would be downloaded straight onto the lock.
        if lock::is_lock_file(&file.filename) {
            bail!(
                "job {job_id} delivers a file named `{}`, which is the name dbnget uses to lock an output directory; refusing rather than writing data onto it",
                file.filename
            );
        }
    }

    // The client puts a job's files in a directory named after the job.
    let job_dir = out.join(job_id);
    verify::no_symlink(&job_dir)?;
    tokio::fs::create_dir_all(&job_dir)
        .await
        .with_context(|| format!("creating job directory {}", job_dir.display()))?;

    // Held for the whole download. Two runs writing one file is not hypothetical here:
    // the documented way to use this tool is to re-run the same command until it stops
    // exiting 3, and a long download overlapping the next poll is exactly what that
    // produces. The claim is what lets an incomplete file below be read as "interrupted"
    // rather than "in progress elsewhere".
    let Some(_claim) = lock::try_claim(&job_dir)? else {
        println!(
            "{job_id} is being downloaded by another dbnget right now; re-run once it finishes"
        );
        return Ok(Outcome::Nonterminal);
    };

    // One file at a time, from the manifest validated above. Handing the whole job to
    // the client's download-all instead would re-fetch the manifest inside it and build
    // every path from that second response, so the names checked here would not be the
    // names written.
    let mut paths = Vec::with_capacity(manifest.len());
    for desc in &manifest {
        let name = verify::checked_file_name(&desc.filename)?;
        let path = job_dir.join(name);
        verify::no_symlink(&path)?;

        // An already-good file is done: say so and move on without asking the vendor
        // for bytes the disk already holds.
        //
        // A file that is present and bad is either incomplete or corrupt, and the two
        // call for opposite treatment. The client compares a file's length against the
        // manifest: shorter means resume from that offset with a Range request, equal
        // means skip it without reading, longer is an error.
        //
        // So a SHORT file is left exactly where it is. Deleting it threw away every byte
        // of an interrupted transfer and started again from nothing, which on a
        // multi-gigabyte job can mean never finishing at all. The claim taken above is
        // what makes "short" safe to read as "interrupted": nothing else can be writing
        // here, so it cannot mean "in progress elsewhere".
        //
        // A file whose length already MATCHES has to be removed, because the client
        // skips it without reading and the corruption would survive every retry: dbnget
        // rejects it, the next run asks for it again, the client declines to fetch it,
        // and the same failure repeats forever with no way out but deleting the file by
        // hand - for data that has already been paid for. A longer-than-expected file
        // goes for the same reason; the client refuses to touch it at all.
        //
        // A short file whose existing bytes are corrupt is not a trap either: the resume
        // completes it, verification fails at full length, and the next run deletes it
        // under the rule above. Two runs, no manual intervention.
        if tokio::fs::try_exists(&path)
            .await
            .with_context(|| format!("checking for {}", path.display()))?
        {
            match verify::file(&path, desc).await {
                Ok(path) => {
                    paths.push(path);
                    continue;
                }
                Err(err) => {
                    let have = tokio::fs::metadata(&path)
                        .await
                        .with_context(|| format!("measuring {}", path.display()))?
                        .len();
                    if have < desc.size {
                        info!(
                            path = %path.display(),
                            have,
                            want = desc.size,
                            "resuming an interrupted download"
                        );
                    } else {
                        warn!(path = %path.display(), %err, "discarding a bad file and fetching it again");
                        tokio::fs::remove_file(&path)
                            .await
                            .with_context(|| format!("removing the corrupt {}", path.display()))?;
                    }
                }
            }
        }

        let params = DownloadParams::builder()
            .output_dir(out)
            .job_id(job_id)
            .filename_to_download(name)
            .build();
        client
            .batch()
            .download(&params)
            .await
            .with_context(|| format!("downloading {name} from job {job_id}"))?;

        paths.push(verify::file(&path, desc).await?);
    }

    for path in &paths {
        println!("{}", path.display());
    }
    Ok(Outcome::Settled)
}

/// How much of the symbol list to show before summarising the rest.
const SYMBOL_WIDTH: usize = 28;

/// Names the columns, because two of them are byte counts that mean different things
/// and one of them is the one people size a download by.
///
/// Printed only for the listing. A single job echoed back after a submit has nothing to
/// line up with, and a header over one row is noise.
fn print_header() {
    println!(
        "{id:<24}  {state:<10}  {dataset:<10} {schema:<10} {selection:<34}  {range:<49}  {shape:<38}  {cost:>10}  {size:>9}  {delivered:>9}",
        id = "JOB ID",
        state = "STATE",
        dataset = "DATASET",
        schema = "SCHEMA",
        selection = "SYMBOLS",
        range = "RANGE (UTC)",
        shape = "OUTPUT",
        cost = "COST",
        size = "DATA",
        delivered = "DOWNLOAD",
    );
}

pub fn print_job(job: &BatchJob) {
    let cost = job
        .cost_usd
        .map_or_else(|| "-".to_owned(), crate::spend::money);
    // Two sizes, because one unlabelled number is read as the download and is not it.
    // `actual_size` is the uncompressed data; `package_size` is what actually comes
    // down the wire, and for zstd-compressed DBN that is several times smaller - sizing
    // a download off the first number overstates it badly. The relationship is not
    // fixed, though: a job of four small CSV and JSON files packages LARGER than its
    // contents, so neither number can be derived from the other.
    let size = job.actual_size.map_or_else(|| "-".to_owned(), human_bytes);
    let delivered = job.package_size.map_or_else(|| "-".to_owned(), human_bytes);
    println!(
        "{id:<24}  {state:<10}  {dataset:<10} {schema:<10} {selection:<34}  {range:<49}  {shape:<38}  {cost:>10}  {size:>9}  {delivered:>9}",
        id = job.id,
        state = format!("{:?}", job.state),
        dataset = job.dataset,
        schema = job.schema.to_string(),
        selection = selection(job),
        range = range(job),
        shape = shape(job),
    );
}

/// Whole-date stamps hide as much as they show, so the times come out when they matter.
const RANGE_STAMP: &[FormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");

/// A bound at whatever precision it carries, down to the nanosecond.
///
/// Adoption compares nanosecond instants, so a listing that stops at whole seconds can
/// print two genuinely different - and differently priced - jobs identically. That is
/// the bare-dates defect again, one unit further down: `12:30:00.1` and `12:30:00.9`
/// are not the same request, and a user comparing them against their own has no way to
/// see it. `--immediate` file names already carry nanoseconds for the same reason.
fn instant(t: OffsetDateTime) -> String {
    let base = t
        .format(RANGE_STAMP)
        .unwrap_or_else(|_| t.unix_timestamp().to_string());
    match t.nanosecond() {
        0 => base,
        nanos => format!("{base}.{}", format!("{nanos:09}").trim_end_matches('0')),
    }
}

/// A job's range, at the precision the job actually has.
///
/// Rendering bounds as bare dates is what makes this listing lie about the thing it
/// exists to answer. A job covering 12:30 to 14:00 on one day printed as
/// `2022-06-10..2022-06-10`, which reads as a whole-day job whose end is inclusive - so
/// a user comparing it against a whole-day request sees a match, submits, and is told
/// the request is new. The match key is on exact instants; the display has to be too.
fn range(job: &BatchJob) -> String {
    let midnight_aligned = |t: OffsetDateTime| t.time() == time::Time::MIDNIGHT;
    if midnight_aligned(job.start) && midnight_aligned(job.end) {
        // Bare dates are honest here, and the end is genuinely exclusive: a one-day job
        // runs to the following midnight, so showing the day before it would claim a
        // range the job does not cover.
        return format!("{}..{}", job.start.date(), job.end.date());
    }
    format!("{}..{}", instant(job.start), instant(job.end))
}

/// The output-shaping fields a request has to agree on to adopt this job.
///
/// Encoding, output symbology and `limit` are all part of the match key and were all
/// invisible here, which is the other half of why a job could look adoptable and not be:
/// a CSV job capped at 1000 records is not a substitute for an uncapped DBN request over
/// the same records, and nothing on the line said so.
///
/// `stype_out` belongs with them rather than beside the symbols. The symbol column
/// qualifies the selection with the symbology it is WRITTEN in, which is `stype_in`;
/// how symbols come back in the delivered records is a property of the output, and
/// putting it here keeps the line from growing another column.
fn shape(job: &BatchJob) -> String {
    let mut out = format!("{} out:{}", job.encoding, job.stype_out);
    if let Some(limit) = job.limit {
        out.push_str(&format!(" limit:{limit}"));
    }
    out
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
    use std::num::NonZeroU64;

    use databento::{
        dbn::{Compression, SType, Schema},
        historical::{
            DateTimeRange,
            batch::{Delivery, SplitDuration},
        },
    };
    use time::macros::datetime;

    use super::*;

    /// A submission with every field at the default the fetch path builds, mutated by
    /// the caller. Built as a literal rather than through the builder so a field added
    /// upstream fails this file to compile - which is the point, since an unmatched
    /// field is a double charge.
    fn params(edit: impl FnOnce(&mut SubmitJobParams)) -> SubmitJobParams {
        let mut params = SubmitJobParams {
            dataset: "GLBX.MDP3".to_owned(),
            symbols: Symbols::Symbols(vec!["ESM4".to_owned()]),
            schema: Schema::Trades,
            date_time_range: DateTimeRange::from(
                datetime!(2024-05-01 0:00 UTC)..datetime!(2024-05-02 0:00 UTC),
            ),
            encoding: Encoding::Dbn,
            compression: Compression::Zstd,
            pretty_px: false,
            pretty_ts: false,
            map_symbols: None,
            split_symbols: false,
            split_duration: SplitDuration::Day,
            split_size: None,
            delivery: Delivery::Download,
            stype_in: SType::RawSymbol,
            stype_out: SType::InstrumentId,
            limit: None,
        };
        edit(&mut params);
        params
    }

    /// The job the vendor would echo back for `params`, with its defaults resolved the
    /// way the vendor resolves them.
    fn job_from(params: &SubmitJobParams) -> BatchJob {
        BatchJob {
            id: "GLBX-20260813-TESTJOB".to_owned(),
            user_id: None,
            cost_usd: Some(0.0),
            dataset: params.dataset.clone(),
            symbols: params.symbols.clone(),
            stype_in: params.stype_in,
            stype_out: params.stype_out,
            schema: params.schema,
            start: params.date_time_range.start,
            end: params.date_time_range.end,
            limit: params.limit,
            encoding: params.encoding,
            compression: params.compression,
            pretty_px: params.pretty_px,
            pretty_ts: params.pretty_ts,
            map_symbols: effective_map_symbols(params),
            split_symbols: params.split_symbols,
            split_duration: params.split_duration,
            split_size: params.split_size,
            delivery: params.delivery,
            record_count: Some(1_000),
            billed_size: Some(1_000),
            actual_size: Some(1_000),
            package_size: Some(1_000),
            state: JobState::Done,
            ts_received: datetime!(2024-05-02 1:00 UTC),
            ts_queued: None,
            ts_process_start: None,
            ts_process_done: None,
            ts_expiration: None,
            progress: Some(100),
        }
    }

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

    /// The match key is the only thing standing between a re-run and a second charge,
    /// and the vendor is free to echo a deduplicated list back.
    #[test]
    fn repeated_symbols_are_one_selection() {
        let requested = Symbols::Symbols(vec![
            "ESM4".to_owned(),
            "ESM4".to_owned(),
            "NQM4".to_owned(),
        ]);
        let echoed = Symbols::Symbols(vec!["ESM4".to_owned(), "NQM4".to_owned()]);
        assert!(same_symbols(&requested, &echoed));

        let ids = Symbols::Ids(vec![42, 42, 7]);
        assert!(same_symbols(&ids, &Symbols::Ids(vec![7, 42])));
    }

    #[test]
    fn map_symbols_defaults_follow_the_encoding() {
        let dbn = params(|_| {});
        assert!(!effective_map_symbols(&dbn));

        let csv = params(|p| p.encoding = Encoding::Csv);
        assert!(effective_map_symbols(&csv));

        let explicit = params(|p| {
            p.encoding = Encoding::Csv;
            p.map_symbols = Some(false);
        });
        assert!(!effective_map_symbols(&explicit));
    }

    /// Every field the match key reads changes the bytes the job delivers, so a job
    /// differing in any ONE of them is not a substitute for the submission being
    /// matched. All of them are exercised, not a sample: the literal-struct fixtures
    /// catch a field ADDED upstream and left out of `job_matches`, but nothing else
    /// catches a field that is compared against the wrong operand or dropped from the
    /// chain, and either one is a silent double charge.
    #[test]
    fn output_affecting_fields_prevent_a_match() {
        let baseline = params(|_| {});
        let job = job_from(&baseline);
        assert!(job_matches(&job, &baseline));

        /// One named way an otherwise matching job can differ.
        type Case = (&'static str, Box<dyn Fn(&mut BatchJob)>);

        let cases: Vec<Case> = vec![
            (
                "dataset",
                Box::new(|j| "XNAS.ITCH".clone_into(&mut j.dataset)),
            ),
            ("schema", Box::new(|j| j.schema = Schema::Mbo)),
            ("stype_in", Box::new(|j| j.stype_in = SType::Parent)),
            ("stype_out", Box::new(|j| j.stype_out = SType::RawSymbol)),
            ("encoding", Box::new(|j| j.encoding = Encoding::Csv)),
            (
                "compression",
                Box::new(|j| j.compression = Compression::None),
            ),
            (
                "split_duration",
                Box::new(|j| j.split_duration = SplitDuration::Week),
            ),
            (
                "split_symbols",
                Box::new(|j| j.split_symbols = !j.split_symbols),
            ),
            (
                "split_size",
                Box::new(|j| j.split_size = NonZeroU64::new(2_000_000_000)),
            ),
            // `delivery` is compared by `job_matches` but cannot be exercised here:
            // upstream `Delivery` has exactly one variant, so no two values exist to
            // differ. Add a case here the day it gains a second.
            ("pretty_px", Box::new(|j| j.pretty_px = !j.pretty_px)),
            ("pretty_ts", Box::new(|j| j.pretty_ts = !j.pretty_ts)),
            ("map_symbols", Box::new(|j| j.map_symbols = !j.map_symbols)),
            ("limit", Box::new(|j| j.limit = NonZeroU64::new(1_000))),
            ("start", Box::new(|j| j.start -= time::Duration::seconds(1))),
            ("end", Box::new(|j| j.end += time::Duration::seconds(1))),
            (
                "symbols",
                Box::new(|j| j.symbols = Symbols::Symbols(vec!["NQM4".to_owned()])),
            ),
        ];

        for (field, mutate) in cases {
            let mut differs = job.clone();
            mutate(&mut differs);
            assert!(
                !job_matches(&differs, &baseline),
                "a job differing only in {field} was treated as this request"
            );
        }
    }

    /// A one-nanosecond difference in either bound is a different request at a
    /// different price, and the bounds are the field most likely to be compared
    /// loosely by accident.
    #[test]
    fn bounds_are_matched_to_the_nanosecond() {
        let baseline = params(|_| {});
        let job = job_from(&baseline);

        let mut differs = job.clone();
        differs.start += time::Duration::nanoseconds(1);
        assert!(!job_matches(&differs, &baseline), "start");

        let mut differs = job.clone();
        differs.end -= time::Duration::nanoseconds(1);
        assert!(!job_matches(&differs, &baseline), "end");
    }

    /// A CSV job gets `map_symbols` by default, and the submission carries `None`. The
    /// two have to compare equal or every CSV re-run buys the data again.
    #[test]
    fn a_csv_job_matches_its_own_defaults() {
        let submitted = params(|p| p.encoding = Encoding::Csv);
        let job = job_from(&submitted);
        assert!(
            job.map_symbols,
            "the vendor applies the text-encoding default"
        );
        assert!(job_matches(&job, &submitted));
    }

    /// The vendor echoes symbols as an array, and an array holding the sentinel
    /// deserializes as an ordinary one-element list rather than as `Symbols::All`. A
    /// whole-dataset job is the most expensive thing on an account, so this is the
    /// worst possible place for a mismatch.
    #[test]
    fn all_symbols_matches_however_it_is_spelled() {
        let echoed = Symbols::Symbols(vec!["ALL_SYMBOLS".to_owned()]);
        assert!(same_symbols(&Symbols::All, &echoed));
        assert!(same_symbols(&echoed, &Symbols::All));
        assert!(same_symbols(&Symbols::All, &Symbols::All));

        let lowercase = Symbols::Symbols(vec!["all_symbols".to_owned()]);
        assert!(same_symbols(&Symbols::All, &lowercase));

        // Still not the same as naming one instrument.
        let one = Symbols::Symbols(vec!["ESM4".to_owned()]);
        assert!(!same_symbols(&Symbols::All, &one));
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

    /// An intraday job rendered as bare dates is indistinguishable from a whole-day one,
    /// and that is exactly the reading that makes a correct non-match look like a bug in
    /// the matcher.
    #[test]
    fn an_intraday_range_shows_its_times() {
        let job = job_from(&params(|p| {
            p.date_time_range = DateTimeRange::from(
                datetime!(2022-06-10 12:30 UTC)..datetime!(2022-06-10 14:00 UTC),
            );
        }));
        assert_eq!(range(&job), "2022-06-10T12:30:00..2022-06-10T14:00:00");
    }

    #[test]
    fn a_whole_day_range_stays_as_dates() {
        let job = job_from(&params(|_| {}));
        assert_eq!(range(&job), "2024-05-01..2024-05-02");
    }

    /// Every one of these is part of the match key, so a job differing in any of them
    /// is not adoptable - the listing has to show them or the user cannot tell why.
    #[test]
    fn the_shape_column_shows_encoding_output_symbology_and_limit() {
        let plain = job_from(&params(|_| {}));
        assert_eq!(shape(&plain), "dbn out:instrument_id");

        let capped = job_from(&params(|p| {
            p.encoding = Encoding::Csv;
            p.limit = NonZeroU64::new(1_000);
        }));
        assert_eq!(shape(&capped), "csv out:instrument_id limit:1000");

        // Two jobs identical everywhere the listing used to look, differing only in the
        // field that decides whether either can be adopted.
        let raw = job_from(&params(|p| p.stype_out = SType::RawSymbol));
        assert_ne!(shape(&raw), shape(&plain));
    }

    /// Adoption compares nanosecond instants, so a listing that stops at whole seconds
    /// prints two different - and differently priced - requests identically.
    #[test]
    fn sub_second_bounds_are_visible_in_the_range() {
        let early = job_from(&params(|p| {
            p.date_time_range = DateTimeRange::from(
                datetime!(2022-06-10 12:30:00.1 UTC)..datetime!(2022-06-10 14:00 UTC),
            );
        }));
        let late = job_from(&params(|p| {
            p.date_time_range = DateTimeRange::from(
                datetime!(2022-06-10 12:30:00.9 UTC)..datetime!(2022-06-10 14:00 UTC),
            );
        }));
        assert_eq!(range(&early), "2022-06-10T12:30:00.1..2022-06-10T14:00:00");
        assert_ne!(range(&early), range(&late));
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
