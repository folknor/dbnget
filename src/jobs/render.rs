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
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};

use super::{SUBMITTED_COMPRESSION, SUBMITTED_SPLIT_DURATION, text_encoding_default};

/// How much of the symbol list to show before summarising the rest.
const SYMBOL_WIDTH: usize = 28;

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
        Symbols,
        dbn::{Compression, Encoding, SType},
        historical::{DateTimeRange, batch::SplitDuration},
    };
    use time::macros::datetime;

    use super::{SYMBOL_WIDTH, human_bytes, range, shape, summarize_symbols};
    use crate::jobs::{
        fixtures::{job_from, params},
        job_matches,
    };

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

    #[test]
    fn byte_counts_are_scaled() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(60_648_379_440), "56.5 GiB");
    }
}
