//! The account's batch jobs: listing them, matching a request against them, and
//! downloading a finished one.

use std::path::Path;

use anyhow::{Context, Result, bail};
use databento::{
    HistoricalClient, Symbols,
    dbn::{Compression, Encoding},
    historical::batch::{
        BatchJob, DownloadParams, JobState, ListJobsParams, SplitDuration, SubmitJobParams,
    },
};
use time::OffsetDateTime;
use tracing::{debug, info, warn};

use crate::{Outcome, cli::ListArgs, lock, query, verify};

#[cfg(test)]
mod fixtures;
mod render;

use render::print_header;
pub use render::print_job;

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

/// The output-shaping values dbnget always submits, and never offers a flag for.
///
/// They live here, next to the matching and listing that read them, rather than at the
/// submission that sets them. The listing marks a job that differs from these, so if the
/// submission changed and these did not, every job dbnget made would be flagged as
/// unusual - the drift would be silent and in the direction of noise.
pub const SUBMITTED_COMPRESSION: Compression = Compression::Zstd;
/// See [`SUBMITTED_COMPRESSION`].
pub const SUBMITTED_SPLIT_DURATION: SplitDuration = SplitDuration::Day;

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
        .unwrap_or_else(|| text_encoding_default(params.encoding))
}

/// The `map_symbols` the vendor applies when a submission does not ask: text encodings
/// get the symbol column, DBN does not.
///
/// Shared with the listing, which needs the same fact to decide whether a job's
/// `map_symbols` is worth marking as unusual.
fn text_encoding_default(encoding: Encoding) -> bool {
    matches!(encoding, Encoding::Csv | Encoding::Json)
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
/// Every variant reduces to STRINGS, including `Symbols::Ids`. The client's `Symbols` is
/// an untagged enum, so a symbols echo of JSON numbers deserializes as `Ids` while this
/// tool's request side only ever builds `Symbols` of strings - `query::symbols` has no
/// path that produces `Ids`, whatever `--stype-in` says. Keeping ids in a separate,
/// never-equal representation therefore meant that if the vendor echoed an
/// instrument-id job numerically, the job could never match the command that bought it,
/// and every re-run would buy it again. Numbers and their decimal spellings select the
/// same instruments, and a job whose symbology differs is already excluded by
/// `job_matches` comparing `stype_in`, so folding them together cannot create a false
/// match either.
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
/// only way that holds. The ids split broke that too: `Ids([42])` and `Symbols(["42"])`
/// were different selections to matching and the same file name to the key.
///
/// Sorting is lexical rather than numeric, which is fine because both sides of every
/// comparison are sorted the same way; the order is a canonical form, not a ranking.
pub fn canonical_symbols(symbols: &Symbols) -> Vec<String> {
    let mut out: Vec<String> = match symbols {
        Symbols::All => vec![ALL_SYMBOLS.to_owned()],
        Symbols::Symbols(list) => list
            .iter()
            .flat_map(|s| s.split(','))
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect(),
        Symbols::Ids(list) => list.iter().map(u32::to_string).collect(),
    };
    out.sort_unstable();
    out.dedup();
    out
}

/// What state the account says a job is in, or `None` if it cannot be found out.
///
/// Advisory, and deliberately so: it exists to improve a message, never to decide
/// whether to download. A listing that fails must not turn a working `dbnget get` into
/// an error, so an unanswerable question leaves the caller's original diagnosis intact.
async fn state_of(client: &mut HistoricalClient, job_id: &str) -> Option<JobState> {
    match all(client).await {
        Ok(jobs) => jobs
            .into_iter()
            .find(|job| job.id == job_id)
            .map(|job| job.state),
        Err(err) => {
            debug!(%err, "could not check the job's state");
            None
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
        // The adoption path reaches here only for a job it has already seen is Done, but
        // `dbnget get` takes an id straight from the user and checks nothing. Asserting
        // "is done" about a job that is still queued sends someone looking for a
        // symbology mistake when the answer is to wait, so the state decides the message.
        //
        // Which state maps to which outcome matters more than the wording. Exit 3 is a
        // promise that re-running will eventually settle, and a shell loop is expected to
        // spin on it until it does. Only a job still being prepared can honour that. An
        // EXPIRED job never becomes ready - its files are gone for good - so returning
        // nonterminal for one would loop a script forever on a job that cannot arrive,
        // while telling the user it is "not ready" as though waiting were the answer.
        match state_of(client, job_id).await {
            Some(JobState::Queued | JobState::Processing) => {
                println!("{job_id} is still being prepared; re-run once it is done");
                return Ok(Outcome::Nonterminal);
            }
            Some(JobState::Expired) => bail!(
                "job {job_id} has expired; the vendor deletes a job's prepared files about 30 days \
                 after completion, so this data can only be obtained by buying it again - re-run \
                 the original fetch command to be quoted for it"
            ),
            // Done, or a state the listing could not tell us. Either way the job claims
            // to be finished and delivered nothing, which is the original diagnosis.
            Some(JobState::Done) | None => {}
        }
        bail!("job {job_id} delivered no files; it bought nothing, so there is nothing to verify");
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use databento::{
        dbn::{Compression, SType, Schema},
        historical::batch::SplitDuration,
    };

    use super::{
        fixtures::{job_from, params},
        *,
    };

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

    /// The request side never builds `Symbols::Ids`, but the vendor's untagged enum
    /// produces one from a numeric echo. Held in a separate representation, such a job
    /// could never match the command that bought it - and every re-run would buy it
    /// again.
    #[test]
    fn numeric_ids_match_the_symbols_that_bought_them() {
        let echoed = Symbols::Ids(vec![12_345]);
        let requested = Symbols::Symbols(vec!["12345".to_owned()]);
        assert!(same_symbols(&echoed, &requested));
        assert!(same_symbols(&requested, &echoed));

        let several = Symbols::Ids(vec![7, 42, 7]);
        let spelled = Symbols::Symbols(vec!["42".to_owned(), "7".to_owned()]);
        assert!(same_symbols(&several, &spelled));

        assert!(!same_symbols(
            &echoed,
            &Symbols::Symbols(vec!["999".to_owned()])
        ));
    }

    #[test]
    fn different_symbol_sets_do_not_match() {
        let left = Symbols::Symbols(vec!["ESM4".to_owned()]);
        let right = Symbols::Symbols(vec!["ESM4".to_owned(), "NQM4".to_owned()]);
        assert!(!same_symbols(&left, &right));
        assert!(!same_symbols(&Symbols::All, &right));
    }

    #[test]
    fn a_ready_job_outranks_one_still_preparing() {
        assert!(state_rank(JobState::Done) > state_rank(JobState::Processing));
        assert!(state_rank(JobState::Processing) > state_rank(JobState::Queued));
        assert!(state_rank(JobState::Queued) > state_rank(JobState::Expired));
    }
}
