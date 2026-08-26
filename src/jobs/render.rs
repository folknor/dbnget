//! How `dbnget list` draws a batch job.
//!
//! Display only: nothing here decides whether a job is adoptable, and nothing outside
//! reads these strings back. The rule the whole module serves is that if
//! [`super::job_matches`] reads a field, the listing can show it - a row that cannot
//! explain a non-match sends people looking for bugs in the match key.

use databento::{
    Symbols,
    historical::batch::{BatchJob, Delivery},
};
use serde::Serialize;
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};

use super::{SUBMITTED_COMPRESSION, SUBMITTED_SPLIT_DURATION, symbol_names, text_encoding_default};
use crate::cli::ListFormat;

/// The whole width the selection cell is allowed, symbology prefix included.
///
/// Budgeting the CELL rather than just the symbol list is the point: the prefix is part
/// of what has to fit, and sizing only the list let `parent:` push the cell past its
/// column and shove every column after it out of alignment.
const SELECTION_WIDTH: usize = 34;

/// A job flattened for a machine to read: every field, nothing abbreviated.
///
/// Separate from the table because the two have opposite obligations. The table exists
/// to be legible in a fixed width and abbreviates to get there; this exists to be
/// complete, so the symbol list is the whole list and every timestamp is RFC 3339 rather
/// than the eye-friendly form the columns use.
///
/// Built as a struct literal from `BatchJob` for the same reason the matcher's fixtures
/// are: a field added upstream shows up as a compile error here, which is the moment to
/// decide whether machine consumers should see it.
#[derive(Debug, Serialize)]
pub(super) struct JobRow {
    id: String,
    state: String,
    dataset: String,
    schema: String,
    symbols: Vec<String>,
    stype_in: String,
    stype_out: String,
    #[serde(with = "time::serde::rfc3339")]
    start: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    end: OffsetDateTime,
    limit: Option<u64>,
    encoding: String,
    compression: String,
    split_duration: String,
    split_size: Option<u64>,
    split_symbols: bool,
    delivery: String,
    pretty_px: bool,
    pretty_ts: bool,
    map_symbols: bool,
    cost_usd: Option<f64>,
    record_count: Option<u64>,
    billed_size: Option<u64>,
    actual_size: Option<u64>,
    package_size: Option<u64>,
    progress: Option<u8>,
    #[serde(with = "time::serde::rfc3339")]
    ts_received: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    ts_queued: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    ts_process_start: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    ts_process_done: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    ts_expiration: Option<OffsetDateTime>,
}

impl JobRow {
    fn new(job: &BatchJob) -> Self {
        Self {
            id: job.id.clone(),
            state: spelled(job.state),
            dataset: job.dataset.clone(),
            schema: job.schema.to_string(),
            // The whole list, comma-joined echoes split apart. A machine format that
            // reported one 400-character pseudo-symbol would be worse than useless.
            symbols: symbol_names(&job.symbols),
            stype_in: job.stype_in.to_string(),
            stype_out: job.stype_out.to_string(),
            start: job.start,
            end: job.end,
            limit: job.limit.map(std::num::NonZeroU64::get),
            encoding: job.encoding.to_string(),
            compression: job.compression.to_string(),
            split_duration: spelled(job.split_duration),
            split_size: job.split_size.map(std::num::NonZeroU64::get),
            split_symbols: job.split_symbols,
            delivery: spelled(job.delivery),
            pretty_px: job.pretty_px,
            pretty_ts: job.pretty_ts,
            map_symbols: job.map_symbols,
            cost_usd: job.cost_usd,
            record_count: job.record_count,
            billed_size: job.billed_size,
            actual_size: job.actual_size,
            package_size: job.package_size,
            progress: job.progress,
            ts_received: job.ts_received,
            ts_queued: job.ts_queued,
            ts_process_start: job.ts_process_start,
            ts_process_done: job.ts_process_done,
            ts_expiration: job.ts_expiration,
        }
    }

    /// The CSV column order, matching [`CSV_HEADER`]. Sizes and counts stay raw rather
    /// than scaled - a spreadsheet can divide, and `1.3 GiB` cannot be summed.
    fn csv_fields(&self) -> Vec<String> {
        fn opt<T: ToString>(v: Option<T>) -> String {
            v.map(|x| x.to_string()).unwrap_or_default()
        }
        vec![
            self.id.clone(),
            self.state.clone(),
            self.dataset.clone(),
            self.schema.clone(),
            self.symbols.join(","),
            self.stype_in.clone(),
            self.stype_out.clone(),
            rfc3339(self.start),
            rfc3339(self.end),
            opt(self.limit),
            self.encoding.clone(),
            self.compression.clone(),
            self.split_duration.clone(),
            opt(self.split_size),
            self.split_symbols.to_string(),
            self.delivery.clone(),
            self.pretty_px.to_string(),
            self.pretty_ts.to_string(),
            self.map_symbols.to_string(),
            opt(self.cost_usd),
            opt(self.record_count),
            opt(self.billed_size),
            opt(self.actual_size),
            opt(self.package_size),
            opt(self.progress),
            rfc3339(self.ts_received),
            self.ts_queued.map(rfc3339).unwrap_or_default(),
            self.ts_process_start.map(rfc3339).unwrap_or_default(),
            self.ts_process_done.map(rfc3339).unwrap_or_default(),
            self.ts_expiration.map(rfc3339).unwrap_or_default(),
        ]
    }
}

const CSV_HEADER: [&str; 30] = [
    "id",
    "state",
    "dataset",
    "schema",
    "symbols",
    "stype_in",
    "stype_out",
    "start",
    "end",
    "limit",
    "encoding",
    "compression",
    "split_duration",
    "split_size",
    "split_symbols",
    "delivery",
    "pretty_px",
    "pretty_ts",
    "map_symbols",
    "cost_usd",
    "record_count",
    "billed_size",
    "actual_size",
    "package_size",
    "progress",
    "ts_received",
    "ts_queued",
    "ts_process_start",
    "ts_process_done",
    "ts_expiration",
];

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| t.unix_timestamp().to_string())
}

/// Writes the whole listing in `format`.
pub(super) fn print_listing(jobs: &[BatchJob], format: ListFormat) -> anyhow::Result<()> {
    match format {
        ListFormat::Table => {
            if jobs.is_empty() {
                println!("no jobs");
                return Ok(());
            }
            print_header();
            for job in jobs {
                print_job(job);
            }
        }
        // No "no jobs" line for the machine formats: an empty array, no lines, or a
        // header alone are all correct and parseable, and a prose sentence on stdout
        // would break whatever is reading them.
        ListFormat::Json => {
            let rows: Vec<JobRow> = jobs.iter().map(JobRow::new).collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        ListFormat::Ndjson => {
            for job in jobs {
                println!("{}", serde_json::to_string(&JobRow::new(job))?);
            }
        }
        ListFormat::Csv => {
            println!("{}", csv_record(&CSV_HEADER.map(ToOwned::to_owned)));
            for job in jobs {
                println!("{}", csv_record(&JobRow::new(job).csv_fields()));
            }
        }
        ListFormat::Markdown => print_markdown(jobs),
        ListFormat::Ids => {
            for job in jobs {
                println!("{}", job.id);
            }
        }
    }
    Ok(())
}

/// One RFC 4180 record.
///
/// A field is quoted when it holds a comma, a quote, a newline or a carriage return, and
/// inner quotes are doubled. The symbols column is comma-joined and so is always quoted,
/// which is precisely why this follows the rule rather than eyeballing it - a bare
/// comma-joined symbol list would silently become 63 extra columns.
fn csv_record(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| {
            if field.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                field.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A pipe table. Pipes inside a cell are escaped, since a raw one would split it.
fn print_markdown(jobs: &[BatchJob]) {
    const COLUMNS: [&str; 8] = [
        "Job ID",
        "State",
        "Dataset",
        "Schema",
        "Symbols",
        "Range (UTC)",
        "Output",
        "Download",
    ];
    println!("| {} |", COLUMNS.join(" | "));
    println!(
        "|{}|",
        COLUMNS.iter().map(|_| "---").collect::<Vec<_>>().join("|")
    );
    for job in jobs {
        let cells = [
            job.id.clone(),
            spelled(job.state),
            job.dataset.clone(),
            job.schema.to_string(),
            // Full list: markdown is read by people but stored as text, and a reader can
            // scroll a wide cell where they cannot recover a truncated one.
            format!("{}:{}", job.stype_in, symbol_names(&job.symbols).join(",")),
            range(job),
            shape(job),
            job.package_size.map_or_else(|| "-".to_owned(), human_bytes),
        ];
        let escaped: Vec<String> = cells.iter().map(|c| c.replace('|', "\\|")).collect();
        println!("| {} |", escaped.join(" | "));
    }
}

/// Names the columns, because two of them are byte counts that mean different things
/// and one of them is the one people size a download by.
///
/// Printed only for the listing. A single job echoed back after a submit has nothing to
/// line up with, and a header over one row is noise.
pub(super) fn print_header() {
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

    // Everything below is shown ONLY when it differs from what dbnget submits. These
    // are the remaining fields adoption compares, and dbnget cannot produce a job that
    // varies in any of them - but the vendor's web UI can, and such a job was
    // indistinguishable from an adoptable one while quietly refusing to be adopted.
    // Printing them unconditionally would add eight columns that read identically on
    // every row a user of this tool ever created, so the unusual is what earns the ink.
    if job.compression != SUBMITTED_COMPRESSION {
        out.push_str(&format!(" compression:{}", job.compression));
    }
    if job.split_duration != SUBMITTED_SPLIT_DURATION {
        out.push_str(&format!(" split:{}", spelled(job.split_duration)));
    }
    if let Some(size) = job.split_size {
        out.push_str(&format!(" splitsize:{}", human_bytes(size.get())));
    }
    if job.split_symbols {
        out.push_str(" split_symbols");
    }
    if job.delivery != Delivery::Download {
        out.push_str(&format!(" delivery:{}", spelled(job.delivery)));
    }
    if job.pretty_px {
        out.push_str(" pretty_px");
    }
    if job.pretty_ts {
        out.push_str(" pretty_ts");
    }
    // `map_symbols` has no fixed value to compare against: the vendor's default depends
    // on the encoding, so what counts as unusual does too.
    if job.map_symbols != text_encoding_default(job.encoding) {
        out.push_str(&format!(" map_symbols:{}", job.map_symbols));
    }
    out
}

/// A `Debug`-only vendor enum as a lower-case word. `Compression` and `Encoding`
/// implement `Display`; `SplitDuration` and `Delivery` do not.
fn spelled(value: impl std::fmt::Debug) -> String {
    format!("{value:?}").to_lowercase()
}

/// The symbols a job covers, qualified by the symbology they are written in.
///
/// `ES.FUT` means nothing on its own: as a raw symbol it is one instrument that may not
/// exist, and as a parent symbol it is every ES future. The stype belongs next to it.
fn selection(job: &BatchJob) -> String {
    let prefix = format!("{}:", job.stype_in);
    let budget = SELECTION_WIDTH.saturating_sub(prefix.len());
    format!("{prefix}{}", summarize_symbols(&job.symbols, budget))
}

/// Renders a symbol list inside `budget` characters, without hiding how many were left
/// out.
///
/// Works down from showing everything, because the remainder note takes room of its own
/// and how much depends on how many are omitted - so the two cannot be decided
/// independently. The first rendering that fits wins.
fn summarize_symbols(symbols: &Symbols, budget: usize) -> String {
    let names = symbol_names(symbols);
    if names.is_empty() {
        return "-".to_owned();
    }

    for take in (1..=names.len()).rev() {
        let head = names[..take].join(",");
        let rendered = if take == names.len() {
            head
        } else {
            format!("{head} +{} more", names.len() - take)
        };
        if rendered.len() <= budget {
            return rendered;
        }
    }
    // Even one symbol does not fit, so a single name is longer than the whole cell.
    // Cut it rather than let it overflow, and mark the cut.
    ellipsize(&names[0], budget)
}

/// Shortens to `budget` characters, ending in a marker so a cut is never mistaken for
/// the real value. Cuts on a character boundary: symbols are not obliged to be ASCII.
fn ellipsize(value: &str, budget: usize) -> String {
    const MARK: &str = "...";
    if value.chars().count() <= budget {
        return value.to_owned();
    }
    let keep = budget.saturating_sub(MARK.len());
    let head: String = value.chars().take(keep).collect();
    format!("{head}{MARK}")
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
        Symbols,
        dbn::{Compression, Encoding, SType},
        historical::{DateTimeRange, batch::SplitDuration},
    };
    use time::macros::datetime;

    use super::{
        CSV_HEADER, JobRow, SELECTION_WIDTH, csv_record, human_bytes, range, selection, shape,
        summarize_symbols, symbol_names,
    };
    use crate::jobs::{
        fixtures::{job_from, params},
        job_matches,
    };

    /// The budget every one of these tests measures against, standing in for the
    /// symbology prefix the real cell also has to fit.
    const BUDGET: usize = SELECTION_WIDTH - "parent:".len();

    #[test]
    fn short_symbol_lists_are_shown_whole() {
        let symbols = Symbols::Symbols(vec!["ES.FUT".to_owned(), "NQ.FUT".to_owned()]);
        assert_eq!(summarize_symbols(&symbols, BUDGET), "ES.FUT,NQ.FUT");
        assert_eq!(summarize_symbols(&Symbols::All, BUDGET), "ALL_SYMBOLS");
    }

    #[test]
    fn long_symbol_lists_say_how_many_were_omitted() {
        let names: Vec<String> = (0..20).map(|i| format!("SYM{i:02}")).collect();
        let summary = summarize_symbols(&Symbols::Symbols(names), BUDGET);
        assert!(
            summary.contains("more"),
            "{summary} should count the remainder"
        );
        assert!(summary.len() <= BUDGET, "{summary} is too wide");
    }

    /// The defect that made a real listing unreadable. The vendor echoes a multi-symbol
    /// selection as ONE comma-joined string, so the summariser saw a list of length one,
    /// concluded there was nothing to omit, and printed all 63 symbols - blowing the
    /// column apart and dragging every column after it off the screen.
    #[test]
    fn a_comma_joined_echo_is_split_before_it_is_summarised() {
        let joined = "ES.FUT,MES.FUT,NQ.FUT,MNQ.FUT,RTY.FUT,M2K.FUT,YM.FUT,MYM.FUT,EMD.FUT";
        let symbols = Symbols::Symbols(vec![joined.to_owned()]);

        assert_eq!(symbol_names(&symbols).len(), 9, "the echo has to be split");

        let summary = summarize_symbols(&symbols, BUDGET);
        assert!(summary.len() <= BUDGET, "{summary} is too wide");
        assert!(
            summary.contains("more"),
            "{summary} should count the remainder"
        );
        assert!(
            !summary.contains("EMD.FUT"),
            "{summary} should have been cut"
        );
    }

    /// The whole cell has to fit its column, prefix included - sizing only the symbol
    /// list let `parent:` push the row out of alignment.
    #[test]
    fn the_selection_cell_fits_its_column() {
        let sixty_three: Vec<String> = (0..63).map(|i| format!("SYM{i:02}.FUT")).collect();
        let job = job_from(&params(|p| {
            p.symbols = Symbols::Symbols(vec![sixty_three.join(",")]);
            p.stype_in = SType::Parent;
        }));
        let cell = selection(&job);
        assert!(cell.len() <= SELECTION_WIDTH, "{cell} overflows its column");
        assert!(cell.starts_with("parent:"), "{cell}");
    }

    /// A single symbol wider than the entire cell still must not overflow, and a cut
    /// value must never be mistakable for the real one.
    #[test]
    fn one_oversized_symbol_is_cut_rather_than_allowed_to_overflow() {
        let huge = "A".repeat(100);
        let summary = summarize_symbols(&Symbols::Symbols(vec![huge]), BUDGET);
        assert!(summary.len() <= BUDGET, "{summary} is too wide");
        assert!(summary.ends_with("..."), "{summary} should mark the cut");
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

    /// A job dbnget submitted cannot vary in the remaining matched fields, so its row
    /// stays quiet. Marking them unconditionally would put eight identical columns on
    /// every row a user of this tool ever produced.
    #[test]
    fn a_job_dbnget_submitted_carries_no_unusual_markers() {
        let shown = shape(&job_from(&params(|_| {})));
        for noise in [
            "compression:",
            "split:",
            "splitsize:",
            "split_symbols",
            "delivery:",
            "pretty_px",
            "pretty_ts",
            "map_symbols:",
        ] {
            assert!(!shown.contains(noise), "{shown} should not mention {noise}");
        }
    }

    /// The case the markers exist for: a job made in the vendor's web UI, which dbnget
    /// will refuse to adopt for reasons that were previously invisible on the row.
    #[test]
    fn a_job_dbnget_could_not_have_made_says_so() {
        let job = job_from(&params(|p| {
            p.compression = Compression::None;
            p.split_duration = SplitDuration::Week;
            p.pretty_px = true;
            p.pretty_ts = true;
            p.split_symbols = true;
            p.split_size = NonZeroU64::new(1_073_741_824);
        }));
        let shown = shape(&job);
        assert!(shown.contains("compression:none"), "{shown}");
        assert!(shown.contains("split:week"), "{shown}");
        assert!(shown.contains("splitsize:1.0 GiB"), "{shown}");
        assert!(shown.contains("split_symbols"), "{shown}");
        assert!(shown.contains("pretty_px"), "{shown}");
        assert!(shown.contains("pretty_ts"), "{shown}");

        // And it genuinely is not adoptable, which is what the row is explaining.
        assert!(!job_matches(&job, &params(|_| {})));
    }

    /// `map_symbols` has no fixed value to compare against, so "unusual" is relative to
    /// the encoding: a CSV job with the symbol column is ordinary, a DBN one is not.
    #[test]
    fn map_symbols_is_marked_against_its_encoding_default() {
        let ordinary_csv = job_from(&params(|p| p.encoding = Encoding::Csv));
        assert!(!shape(&ordinary_csv).contains("map_symbols"));

        let mut odd_dbn = job_from(&params(|_| {}));
        odd_dbn.map_symbols = true;
        assert!(shape(&odd_dbn).contains("map_symbols:true"));

        let mut odd_csv = job_from(&params(|p| p.encoding = Encoding::Csv));
        odd_csv.map_symbols = false;
        assert!(shape(&odd_csv).contains("map_symbols:false"));
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

    /// The rule that keeps a symbol list from becoming 63 extra columns.
    #[test]
    fn csv_quotes_fields_that_would_otherwise_break_the_record() {
        let record = csv_record(&[
            "plain".to_owned(),
            "ES.FUT,MES.FUT".to_owned(),
            "say \"hi\"".to_owned(),
            "two\nlines".to_owned(),
        ]);
        assert_eq!(
            record,
            "plain,\"ES.FUT,MES.FUT\",\"say \"\"hi\"\"\",\"two\nlines\""
        );
    }

    /// Every column has a header, or a consumer lines the wrong data up under the wrong
    /// name - which is worse than failing, because it parses.
    #[test]
    fn the_csv_header_matches_the_row_width() {
        let job = job_from(&params(|_| {}));
        assert_eq!(JobRow::new(&job).csv_fields().len(), CSV_HEADER.len());
    }

    /// A machine format must never abbreviate. The table cuts a 63-symbol list down to
    /// fit a column; JSON and CSV have to carry every one of them.
    #[test]
    fn machine_formats_carry_the_whole_symbol_list() {
        let sixty_three: Vec<String> = (0..63).map(|i| format!("SYM{i:02}.FUT")).collect();
        let job = job_from(&params(|p| {
            // The vendor's comma-joined echo, which is what actually arrives.
            p.symbols = Symbols::Symbols(vec![sixty_three.join(",")]);
        }));

        let row = JobRow::new(&job);
        assert_eq!(
            row.symbols.len(),
            63,
            "the echo must be split, not truncated"
        );

        let fields = row.csv_fields();
        let symbols_column = &fields[4];
        assert!(
            symbols_column.contains("SYM62.FUT"),
            "the last symbol is missing"
        );
        assert!(
            !symbols_column.contains("more"),
            "a machine format must not abbreviate"
        );

        // And the table, for contrast, still abbreviates.
        assert!(selection(&job).contains("more"));
    }

    /// Markdown is read by people but stored as text, so a raw pipe inside a cell would
    /// silently split the row.
    #[test]
    fn markdown_escapes_pipes_in_cells() {
        let job = job_from(&params(|p| {
            p.symbols = Symbols::Symbols(vec!["WEIRD|SYMBOL".to_owned()]);
        }));
        let cells = format!("{}:{}", job.stype_in, symbol_names(&job.symbols).join(","));
        assert!(cells.replace('|', "\\|").contains("WEIRD\\|SYMBOL"));
    }

    #[test]
    fn byte_counts_are_scaled() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(60_648_379_440), "56.5 GiB");
    }
}
