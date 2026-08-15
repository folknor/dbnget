//! The default command: fetch data, with the command itself as the state machine.
//!
//! The whole request is one batch job on the vendor's account (Databento keeps a
//! multi-symbol submit as a single job). Each run reconciles the request against the
//! account: a finished matching job is downloaded, a pending one is reported, a
//! missing one is queued - subject to the spend gate. Re-running the same command is
//! the poll loop.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use databento::{
    HistoricalClient,
    dbn::{Compression, Encoding, SType, Schema},
    historical::{
        DateRange, DateTimeRange,
        batch::{BatchJob, JobState, SplitDuration, SubmitJobParams},
        metadata::GetQueryParams,
        symbology::ResolveParams,
        timeseries::GetRangeParams,
    },
};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use tracing::{debug, info, warn};

use crate::{Outcome, cli::FetchArgs, jobs, query, spend, verify};

/// The stamp format for `--immediate` file names: sortable, and free of separators
/// that would collide with the dotted fields around it.
const STAMP: &[FormatItem<'_>] = format_description!("[year][month][day]T[hour][minute][second]");

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
        return cost(client, args, &request).await;
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
        stype_in: args.stype_in.into(),
        stype_out: args.stype_out.into(),
        limit: args.limit,
        encoding: args.format.into(),
    })
}

/// `--cost`: price the request and stop. Never queues.
async fn cost(
    client: &mut HistoricalClient,
    args: &FetchArgs,
    request: &Request,
) -> Result<Outcome> {
    let quote = spend::fetch(client, &request.metadata_params()).await?;
    println!("records:       {}", quote.records);
    println!("billable size: {} bytes", quote.billable_bytes);
    println!("cost:          {}", spend::money(quote.usd));

    // The price alone is half an answer. Anyone running `--cost` is deciding whether to
    // run the command for real, and the gate's verdict is that decision - a quote read
    // without it says "free" at a price the very next command refuses.
    println!("would fetch:   {}", spend::verdict(&quote, args.spend));

    if let Some(summary) = resolve_summary(client, request).await {
        println!("symbols:       {summary}");
    }

    // Only on the streaming path is there a name to print. A batch job lands in
    // `OUTPUT/JOB_ID/`, and the job id does not exist until the job is submitted, so
    // anything printed here would be an invention.
    if args.immediate {
        println!(
            "would write:   {}",
            args.output.join(request.file_name()?).display()
        );
    }

    if quote.is_empty() {
        println!();
        println!("This query matches no records. A $0.00 quote here means empty, not free.");
    }
    Ok(Outcome::Settled)
}

/// How the request's symbols resolve, for `--cost` to report alongside the price.
///
/// Symbology resolution is a free metadata call, and it answers what a price cannot: a
/// $0.00 quote reads the same whether a subscription covers the request or the symbols
/// select nothing at all, and `ES.FUT` under the wrong symbology is the ordinary way to
/// reach the second case.
///
/// Strictly advisory. The endpoint refuses symbology combinations that fetch perfectly
/// well - `parent` to `raw_symbol` among them - so a failure here says nothing about
/// whether the request is valid, and must not change what `--cost` reports or exits.
async fn resolve_summary(client: &mut HistoricalClient, request: &Request) -> Option<String> {
    let params = ResolveParams::builder()
        .dataset(&request.dataset)
        .symbols(request.symbols.clone())
        .stype_in(request.stype_in)
        .stype_out(request.stype_out)
        .date_range(DateRange::from(
            request.range.start.date()..request.range.end.date(),
        ))
        .build();

    match client.symbology().resolve(&params).await {
        Ok(resolution) => {
            let mut summary = format!(
                "{} resolved, {} partial, {} not found",
                resolution.mappings.len(),
                resolution.partial.len(),
                resolution.not_found.len()
            );
            if !resolution.not_found.is_empty() {
                summary.push_str(&format!(" ({})", resolution.not_found.join(", ")));
            }
            Some(summary)
        }
        Err(err) => {
            debug!(%err, "symbology resolution unavailable for this request");
            None
        }
    }
}

/// The default path: reconcile the request against the vendor's job listing.
async fn reconcile(
    client: &mut HistoricalClient,
    args: &FetchArgs,
    request: &Request,
) -> Result<Outcome> {
    // Which path a run took is a billing and latency difference, and nothing else in
    // the output distinguishes them - a user who left `--immediate` off could not tell
    // whether a job had been queued on the account.
    info!("batch mode: reconciling against the jobs on the account");

    let params = request.submit_params();
    let listing = jobs::all(client).await?;

    // The single most useful thing verbosity had to say was that adoption was attempted
    // at all, and it was only visible as a vendor span buried in the client's own debug
    // output. dbnget says it itself.
    debug!(
        jobs = listing.len(),
        "checking whether a job already bought this request"
    );

    if let Some(job) = jobs::find_live(&listing, &params) {
        return match job.state {
            JobState::Done => {
                // The zero-record rule is a property of the request, not of the path
                // that reaches it. Checking it only before submitting would let an
                // empty job already sitting on the account be adopted and exit 0,
                // which is the same "symbology or date-range mistake delivered as an
                // empty file" the spend gate exists to refuse. Refusing rather than
                // ignoring the job matters: ignoring it would fall through and submit
                // a duplicate.
                if job.record_count == Some(0) {
                    bail!(
                        "job {} matches this request but holds no records - check the symbols, dataset and date range",
                        job.id
                    );
                }
                info!(job_id = %job.id, "adopting finished job");
                jobs::download(client, &job.id, &args.output).await
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

    info!("no job on the account matches this request; pricing a new one");
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

    info!("immediate mode: streaming directly, nothing is queued on the account");

    tokio::fs::create_dir_all(&args.output)
        .await
        .with_context(|| format!("creating output directory {}", args.output.display()))?;
    let path = args.output.join(request.file_name()?);
    let partial = path.with_extension("part");

    // Both names are claimed BEFORE the request is priced or issued, and claimed by
    // creating them exclusively rather than by asking whether they exist. A check
    // followed by a write is a race: two runs of the same command both see nothing
    // there, both pay, both stream into one partial file, and the loser's data is
    // replaced by the winner's. An exclusive create is the only form of this that two
    // processes cannot both win.
    //
    // Doing it before `spend::fetch` matters as much as doing it exclusively. Every
    // failure reachable here - the file exists, the directory is unwritable, a path
    // component is inaccessible - is a failure that must happen while the request is
    // still free.
    let claimed = claim(&path)?;
    // The second claim is the one that can fail with the first already made. Letting `?`
    // return here would leave the final name reserved by a run that never priced
    // anything, so the next attempt is refused over an empty file it did not create -
    // the same stranded-claim failure as a refusal by the spend gate, one line earlier.
    let claimed_partial = match claim(&partial) {
        Ok(file) => file,
        Err(err) => {
            drop(claimed);
            release(&path).await;
            return Err(err);
        }
    };

    // Pricing and the gate come after the claims and before anything is billed, so a
    // refusal here has spent nothing and written nothing - and the two empty files it
    // would otherwise leave behind are pure damage. The next run finds the names taken
    // and reports that the data was already paid for, which is the opposite of what
    // happened: a free refusal would have permanently blocked the command it refused.
    // Only claims this run made are released, and only while they are still empty.
    let quote = match gate(client, args, request).await {
        Ok(quote) => quote,
        Err(err) => {
            drop(claimed);
            drop(claimed_partial);
            release(&partial).await;
            release(&path).await;
            return Err(err);
        }
    };

    // The cost endpoint prices the default feed mode, which is the batch one, and it
    // takes no mode parameter to ask otherwise. Streaming is a different, dearer feed
    // mode, so this quote is a floor rather than the bill. Said out loud because the
    // gate's whole promise is that nothing is charged beyond what was approved, and on
    // this path that promise is weaker than on the batch path.
    warn!(
        quoted = spend::money(quote.usd),
        "--immediate is gated on the batch price; streaming is billed at the streaming rate, which is higher"
    );

    // The handles have done their job: the names are taken, and holding an open file is
    // not what reserves them. Releasing them before the write keeps the rename from
    // being a rename onto a file this process still has open, which the platforms this
    // crate does not exclude do not allow.
    drop(claimed);
    drop(claimed_partial);

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
    let outcome = client
        .timeseries()
        .get_range_to_file(&params.with_path(&partial))
        .await;

    if let Err(err) = outcome {
        // The claims are this run's own, and the request failed, so releasing them lets
        // a retry proceed. Leaving them would turn one network error into a permanently
        // blocked command.
        release(&partial).await;
        release(&path).await;
        return Err(err).with_context(|| format!("downloading {}", partial.display()));
    }

    // Replaces this run's own placeholder, which is what makes the rename safe: no
    // other process could have claimed that name, so nothing else can be under it.
    tokio::fs::rename(&partial, &path)
        .await
        .with_context(|| format!("renaming {} to {}", partial.display(), path.display()))?;

    println!("{}", path.display());
    Ok(Outcome::Settled)
}

/// Prices the request and puts it through the spend gate, as one fallible step.
///
/// Kept together so a caller holding output claims has a single error to unwind from,
/// rather than two places where an early return could leak them.
async fn gate(
    client: &mut HistoricalClient,
    args: &FetchArgs,
    request: &Request,
) -> Result<spend::Quote> {
    let quote = spend::fetch(client, &request.metadata_params()).await?;
    spend::approve(&quote, args.spend)?;
    Ok(quote)
}

/// Claims an output name by creating it exclusively.
///
/// Returns the handle so the caller holds the claim for as long as it needs it. The
/// error distinguishes the ordinary case - the file is already there - from a genuine
/// filesystem problem, because the two call for different things from the user, and
/// because treating the second as "does not exist" is how a permission error turns into
/// a charge for data that cannot be written.
fn claim(path: &Path) -> Result<std::fs::File> {
    verify::no_symlink(path)?;
    std::fs::File::create_new(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            // An empty file here is not data. Telling someone they already paid for a
            // zero-byte husk sends them hunting for a completed job to reuse, which is
            // a long walk from a file they can simply delete. Only a file with bytes in
            // it earns the warning about paying twice.
            let empty = std::fs::metadata(path).is_ok_and(|meta| meta.len() == 0);
            if empty {
                anyhow::anyhow!(
                    "{} already exists and is empty - an abandoned claim from a run that was refused or interrupted, not downloaded data. Delete it and try again.",
                    path.display()
                )
            } else {
                anyhow::anyhow!(
                    "{} already exists and holds data; move it aside or choose another --output rather than paying for it again",
                    path.display()
                )
            }
        } else {
            anyhow::Error::new(err).context(format!("claiming {}", path.display()))
        }
    })
}

/// Drops a claim this run made and could not use.
async fn release(path: &Path) {
    if let Err(err) = tokio::fs::remove_file(path).await {
        warn!(path = %path.display(), %err, "could not clean up after a failed download");
    }
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

    /// Names an `--immediate` file after the query it holds, so a directory of them
    /// stays self-describing and no two different requests can land on one path.
    ///
    /// The name carries dataset, schema, a symbol hint, both bounds and an identity
    /// key. The stamps carry seconds, and sub-second bounds carry their nanoseconds
    /// too: minute precision let two differently-priced RFC 3339 requests land on one
    /// name, and the second one to run would silently replace the first.
    ///
    /// The symbol hint is a convenience and the KEY is the identity. The hint is
    /// sanitized rather than validated, because a symbol is not required to be a legal
    /// filename - a colon is illegal on Windows and perfectly ordinary in a symbol -
    /// and refusing to fetch `ES:FUT` because of how its file would be named would be
    /// absurd. Anything the hint cannot represent is still separated by the key.
    ///
    /// The result is validated as a plain file name rather than trusted. `dataset` is
    /// a free-form string from the command line, and it is being joined onto
    /// `--output`: a leading separator would make the join absolute and ignore the
    /// output directory entirely.
    fn file_name(&self) -> Result<PathBuf> {
        let dataset = self.dataset.replace('.', "_");
        let start = stamp(self.range.start);
        let end = stamp(self.range.end);
        let name = format!(
            "{dataset}.{schema}.{hint}.{start}-{end}.{key}.dbn.zst",
            schema = self.schema,
            hint = self.symbol_hint(),
            key = self.identity_key(),
        );
        verify::checked_file_name(&name)
            .with_context(|| format!("`{}` does not make a usable file name", self.dataset))?;
        Ok(PathBuf::from(name))
    }

    /// A short digest of everything that makes this request the request it is.
    ///
    /// Without it the name was dataset, schema and range - so AAPL and MSFT over one
    /// window shared a path. The second request found the first one's file sitting
    /// there and reported it as data already paid for: follow that advice and you
    /// discard the AAPL data, ignore it and `-o` never yields MSFT, and script it and
    /// you get a nonzero exit over a file holding the wrong instrument entirely.
    ///
    /// The fields are the ones adoption matches on, because those are by definition the
    /// ones that change the bytes delivered - symbols, symbology in and out, bounds and
    /// limit, alongside the dataset and schema already in the name. Encoding is fixed:
    /// `--immediate` refuses anything but DBN. It is hashed rather than spelled out
    /// because a hundred-symbol request has no short readable spelling, and a name that
    /// is sometimes too long for the filesystem is a worse failure than an opaque one.
    ///
    /// Truncated to 16 hex characters. This separates requests; it does not defend
    /// against anyone constructing a collision, and nothing downstream trusts it.
    fn identity_key(&self) -> String {
        let symbols = match jobs::canonical_symbols(&self.symbols) {
            Ok(names) => names.join(","),
            Err(ids) => ids.iter().map(u32::to_string).collect::<Vec<_>>().join(","),
        };
        // Newline-separated with the field count fixed, so no field's contents can be
        // read as another field's - a symbol containing the separator would otherwise
        // let two different requests hash identically.
        let identity = format!(
            "{dataset}\n{schema}\n{symbols}\n{start}\n{end}\n{stype_in}\n{stype_out}\n{limit}\n{encoding}",
            dataset = self.dataset,
            schema = self.schema,
            start = self.range.start.unix_timestamp_nanos(),
            end = self.range.end.unix_timestamp_nanos(),
            stype_in = self.stype_in,
            stype_out = self.stype_out,
            limit = self.limit.map_or(0, std::num::NonZeroU64::get),
            encoding = self.encoding,
        );
        let digest = Sha256::digest(identity.as_bytes());
        digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// A readable gesture at the symbols, for the human reading the directory.
    ///
    /// Reduced to the same canonical selection adoption compares, so the hint does not
    /// change when the same request is spelled differently. Characters that cannot
    /// safely sit in a file name are replaced rather than refused, and a long list is
    /// truncated with a count - the identity key is what actually distinguishes the
    /// file, so the hint is free to be lossy.
    fn symbol_hint(&self) -> String {
        let names: Vec<String> = match jobs::canonical_symbols(&self.symbols) {
            Ok(names) => names,
            Err(ids) => ids.iter().map(u32::to_string).collect(),
        };
        if names.is_empty() {
            return "none".to_owned();
        }

        let safe = |name: &str| -> String {
            name.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect()
        };

        let mut out = String::new();
        let mut shown = 0;
        for name in &names {
            let cleaned = safe(name);
            if !out.is_empty() && out.len() + cleaned.len() + 1 > HINT_WIDTH {
                break;
            }
            if !out.is_empty() {
                out.push('_');
            }
            out.push_str(&cleaned);
            shown += 1;
        }
        if shown < names.len() {
            out.push_str(&format!("+{}", names.len() - shown));
        }
        // A selection whose every character was unrepresentable still needs a stem that
        // is not empty and not a leading dot.
        if out.trim_matches('-').is_empty() {
            return format!("symbols{out}");
        }
        out
    }
}

/// How much of the symbol list a file name carries before it summarises the rest. Long
/// enough to recognise a handful of tickers, short enough to leave room for the rest of
/// the name inside a filesystem's limit.
const HINT_WIDTH: usize = 24;

/// A bound as a sortable stamp, to whatever precision it actually carries.
fn stamp(instant: OffsetDateTime) -> String {
    let base = instant
        .format(STAMP)
        .unwrap_or_else(|_| instant.unix_timestamp().to_string());
    match instant.nanosecond() {
        0 => base,
        nanos => format!("{base}_{nanos:09}"),
    }
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
            request().file_name().unwrap().to_str().unwrap(),
            "GLBX_MDP3.trades.ESM4.20240430T220000-20240501T210000.8813bcf0dd200097.dbn.zst"
        );
    }

    /// The reported bug: the name held dataset, schema and range only, so two symbols
    /// over one window shared a path. The second request was then told the first's file
    /// was data it had already paid for - advice that destroys the first instrument's
    /// data if followed and never yields the second if ignored.
    #[test]
    fn different_symbols_do_not_share_a_file_name() {
        let mut aapl = request();
        aapl.symbols = databento::Symbols::Symbols(vec!["AAPL".to_owned()]);
        let mut msft = request();
        msft.symbols = databento::Symbols::Symbols(vec!["MSFT".to_owned()]);
        assert_ne!(aapl.file_name().unwrap(), msft.file_name().unwrap());

        // And the whole-dataset request is not any one symbol's file either.
        let mut all = request();
        all.symbols = databento::Symbols::All;
        assert_ne!(all.file_name().unwrap(), aapl.file_name().unwrap());
    }

    /// Every field adoption matches on changes the bytes delivered, so every one of
    /// them has to change the name. `--stype-in` especially: `ES.FUT` is one instrument
    /// as a raw symbol and every ES future as a parent.
    #[test]
    fn every_request_defining_field_changes_the_file_name() {
        let baseline = request().file_name().unwrap();

        let mut stype_in = request();
        stype_in.stype_in = SType::Parent;
        assert_ne!(stype_in.file_name().unwrap(), baseline, "stype_in");

        let mut stype_out = request();
        stype_out.stype_out = SType::RawSymbol;
        assert_ne!(stype_out.file_name().unwrap(), baseline, "stype_out");

        let mut limit = request();
        limit.limit = std::num::NonZeroU64::new(1_000);
        assert_ne!(limit.file_name().unwrap(), baseline, "limit");

        let mut dataset = request();
        "XNAS.ITCH".clone_into(&mut dataset.dataset);
        assert_ne!(dataset.file_name().unwrap(), baseline, "dataset");

        let mut schema = request();
        schema.schema = Schema::Mbo;
        assert_ne!(schema.file_name().unwrap(), baseline, "schema");
    }

    /// The same request spelled differently is the same request - adoption says so, and
    /// the file name has to agree or a re-run writes a second copy beside the first.
    #[test]
    fn the_same_request_spelled_differently_names_one_file() {
        let mut once = request();
        once.symbols = databento::Symbols::Symbols(vec!["esm4".to_owned(), "NQM4".to_owned()]);
        let mut again = request();
        again.symbols = databento::Symbols::Symbols(vec![
            "NQM4".to_owned(),
            "ESM4".to_owned(),
            "nqm4".to_owned(),
        ]);
        assert_eq!(once.file_name().unwrap(), again.file_name().unwrap());
    }

    /// A symbol is not obliged to be a legal file name. Refusing to fetch one because
    /// of how its file would be named would be absurd, so the hint is sanitized - and
    /// two symbols that sanitize alike are still separated by the identity key.
    #[test]
    fn symbols_that_are_not_legal_file_names_are_still_fetchable() {
        let mut colons = request();
        colons.symbols = databento::Symbols::Symbols(vec!["ES:FUT".to_owned()]);
        let name = colons.file_name().unwrap();
        assert!(name.to_str().unwrap().contains("ES-FUT"), "{name:?}");

        let mut slashes = request();
        slashes.symbols = databento::Symbols::Symbols(vec!["ES/FUT".to_owned()]);
        assert_ne!(slashes.file_name().unwrap(), name);
    }

    /// A long list cannot go in the name in full, so it is truncated with a count and
    /// the identity key carries the difference.
    #[test]
    fn a_long_symbol_list_is_summarised_but_still_distinct() {
        let many: Vec<String> = (0..40).map(|i| format!("SYM{i:02}")).collect();
        let mut first = request();
        first.symbols = databento::Symbols::Symbols(many.clone());
        let mut second = request();
        second.symbols = databento::Symbols::Symbols(many[1..].to_vec());

        let name = first.file_name().unwrap();
        let rendered = name.to_str().unwrap();
        assert!(rendered.contains('+'), "{rendered} should count the rest");
        assert!(rendered.len() < 120, "{rendered} is too long");
        assert_ne!(name, second.file_name().unwrap());
    }

    /// Two requests that differ only below the minute must not name the same file: the
    /// second one billed would otherwise truncate the first one's data.
    #[test]
    fn sub_minute_bounds_do_not_collide() {
        let mut coarse = request();
        coarse.range = DateTimeRange::from(
            datetime!(2024-05-01 12:00:00 UTC)..datetime!(2024-05-01 13:00 UTC),
        );
        let mut fine = request();
        fine.range = DateTimeRange::from(
            datetime!(2024-05-01 12:00:30 UTC)..datetime!(2024-05-01 13:00 UTC),
        );
        assert_ne!(coarse.file_name().unwrap(), fine.file_name().unwrap());

        let mut nanos = request();
        nanos.range = DateTimeRange::from(
            datetime!(2024-05-01 12:00:00.5 UTC)..datetime!(2024-05-01 13:00 UTC),
        );
        assert_ne!(coarse.file_name().unwrap(), nanos.file_name().unwrap());
    }

    /// A dataset carrying a separator would otherwise escape `--output`, and a leading
    /// one would make the join absolute.
    #[test]
    fn a_dataset_that_would_escape_the_output_directory_is_refused() {
        for bad in ["/etc/passwd", "../../evil", "a/b"] {
            let mut req = request();
            bad.clone_into(&mut req.dataset);
            assert!(req.file_name().is_err(), "should have refused `{bad}`");
        }
    }

    #[test]
    fn missing_required_flags_are_reported_by_name() {
        let args = FetchArgs {
            symbols: vec!["ESM4".to_owned()],
            dataset: None,
            schema: None,
            start: None,
            end: None,
            stype_in: crate::cli::CliSType::RawSymbol,
            stype_out: crate::cli::CliSType::InstrumentId,
            limit: None,
            format: crate::cli::CliFormat::Dbn,
            output: PathBuf::from("."),
            immediate: false,
            spend: None,
            cost: false,
        };
        let err = validate(&args).unwrap_err().to_string();
        assert!(err.contains("--dataset"), "{err}");
    }
}
